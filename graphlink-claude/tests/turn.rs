//! The turn over shared memory: the seat writes messages, tools and a request; the peer runs a `claude` child (tests/fake_claude.py: NOT the real CLI) and
//! publishes Reply and Chunk nodes. A round that ends in tool calls is answered by the seat's next request, whose last message is the tool's result.
mod common;
use common::{cell, logged, Rig, KEY};
use graphlink_claude::handler::Auth;
use serde_json::{json, Value as J};

fn text(r: &graphlink_shm::cells::CellsReader, rid: &str) -> String {
    let n = cell(r, &format!("claude::Reply:{rid}"), "chunks").and_then(|v| v.as_u64()).unwrap_or(0);
    (0..n).map(|k| cell(r, &format!("claude::Chunk:{rid}:{k}"), "text").and_then(|v| v.as_str().map(String::from)).unwrap_or_default()).collect()
}
fn alive(pid: i64) -> bool { unsafe { libc::kill(pid as i32, 0) == 0 } }

#[test]
fn a_plain_answer_is_one_round_with_text_finish_stop_and_usage() {
    let mut rig = Rig::new("p1");
    rig.seat.round1("c/p1", "plain p1", &["echo"]);
    rig.until_state("c/p1-1", "done");
    let r = rig.seat.reader();
    assert_eq!(text(&r, "c/p1-1"), "hello from claude");
    assert_eq!(cell(&r, "claude::Reply:c/p1-1", "finish"), Some(json!("stop")));
    assert_eq!(cell(&r, "claude::Reply:c/p1-1", "usage"), Some(json!({"read": 5, "generated": 7})));
    assert_eq!(cell(&r, "claude::Reply:c/p1-1", "tool_calls"), None);
}

#[test]
fn usage_maps_the_token_counts_onto_read_loaded_generated_and_leaves_out_what_is_not_there() {
    let mut rig = Rig::new("u1");
    rig.seat.round1("c/u1", "cache u1", &["echo"]);
    rig.until_state("c/u1-1", "done");
    // read: tokens read fresh (input + written to the prompt cache); loaded: tokens served from the prompt cache; generated: output tokens
    assert_eq!(cell(&rig.seat.reader(), "claude::Reply:c/u1-1", "usage"), Some(json!({"read": 7, "loaded": 50, "generated": 2})));
}

#[test]
fn the_child_is_started_with_the_flags_of_the_seats_claude_rung_and_nothing_else() {
    let mut rig = Rig::new("p2");
    rig.seat.round1("c/p2", "plain p2", &["echo"]);
    rig.until_state("c/p2-1", "done");
    let l = logged("p2");
    let argv: Vec<&str> = l["argv"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    let flag = |f: &str| argv.iter().position(|a| *a == f).unwrap_or_else(|| panic!("missing {f}: {argv:?}"));
    assert_eq!(argv[0], "-p");
    for (f, v) in [("--input-format", "stream-json"), ("--output-format", "stream-json"), ("--tools", ""), ("--allowedTools", "mcp__seat__*"), ("--setting-sources", ""), ("--model", "sonnet"), ("--system-prompt", "You are the graph.")] {
        assert_eq!(argv[flag(f) + 1], v, "{f}");
    }
    for f in ["--verbose", "--strict-mcp-config", "--no-session-persistence"] { flag(f); }
    assert!(!argv.contains(&"--max-turns") && !argv.contains(&"--include-partial-messages"), "{argv:?}");
    assert!(!argv.contains(&"--bare") && !argv.contains(&"--dangerously-skip-permissions"));
    assert_eq!(l["user"], json!("plain p2"), "the user message is on stdin");
    let cfg: J = serde_json::from_str(argv[flag("--mcp-config") + 1]).unwrap();
    assert_eq!(cfg["mcpServers"]["seat"]["args"][0], json!("mcp"));
}

#[test]
fn a_tool_call_ends_the_round_and_the_next_request_carries_the_result_into_the_same_run() {
    let mut rig = Rig::new("t1");
    rig.seat.round1("c/t1", "tool t1", &["echo"]);
    rig.until_state("c/t1-1", "done");
    let r = rig.seat.reader();
    assert_eq!(cell(&r, "claude::Reply:c/t1-1", "finish"), Some(json!("tool_calls")));
    assert_eq!(cell(&r, "claude::Reply:c/t1-1", "tool_calls"), Some(json!([{"name": "echo", "arguments": {"text": "ping"}}])));
    assert_eq!(text(&r, "c/t1-1"), "checking");
    // the seat runs the tool and writes round 2: the same messages, the assistant's calls, the tool's result last
    rig.seat.assistant_calls("c/t1", 2, json!([{"type": "function", "function": {"name": "echo", "arguments": {"text": "ping"}}}]));
    rig.seat.msg("c/t1", 3, "tool", "PONG");
    rig.seat.request("c/t1", 2, 4, 1);
    rig.until_state("c/t1-2", "done");
    let r = rig.seat.reader();
    assert_eq!(text(&r, "c/t1-2"), "got: PONG [tools: echo]", "the child continued with the result as the tool's result");
    assert_eq!(cell(&r, "claude::Reply:c/t1-2", "finish"), Some(json!("stop")));
    assert_eq!(cell(&r, "claude::Reply:c/t1-2", "usage"), Some(json!({"read": 11, "generated": 13})));
}

#[test]
fn a_tool_result_over_the_cap_reaches_the_child_as_its_head_and_tail() {
    // the CLI refuses a result over its own limit and the model then gets nothing: the child is handed the first and last 8192 characters and a line that says what was left out (src/cap.rs)
    let mut rig = Rig::new("tcap");
    rig.seat.round1("c/tcap", "toolsize tcap", &["echo"]);
    rig.until_state("c/tcap-1", "done");
    let big = format!("{}{}{}", "A".repeat(8192), "m".repeat(30_000), "Z".repeat(8192));
    rig.seat.assistant_calls("c/tcap", 2, json!([{"type": "function", "function": {"name": "echo", "arguments": {"text": "ping"}}}]));
    rig.seat.msg("c/tcap", 3, "tool", &big);
    rig.seat.request("c/tcap", 2, 4, 1);
    rig.until_state("c/tcap-2", "done");
    let r = rig.seat.reader();
    let got = text(&r, "c/tcap-2");
    let len: usize = got.strip_prefix("len=").and_then(|x| x.split(' ').next()).and_then(|x| x.parse().ok()).expect("the length the child saw");
    assert!(len < 16_700 && len > 16_384, "the child saw the two ends and a line, not the whole {}: {got}", big.len());
    assert!(got.contains("head=AAA") && got.contains("tail=ZZZ") && got.contains("cut=True"), "{got}");
}

#[test]
fn several_tool_uses_of_one_step_are_each_their_own_round_because_the_cli_calls_one_tool_at_a_time() {
    // measured on the real CLI: with two tool_use blocks in one message the second call comes only after the first was answered. A peer that waited for both before ending the round
    // waited for ever (turns that took 30 minutes on a seat): the round ends on the first held call, the seat runs it, and the second call is the next round's
    let mut rig = Rig::new("t2");
    rig.seat.round1("c/t2", "two t2", &["echo", "other"]);
    rig.until_state("c/t2-1", "done");
    let r = rig.seat.reader();
    let calls = cell(&r, "claude::Reply:c/t2-1", "tool_calls").unwrap();
    assert_eq!(calls.as_array().unwrap().len(), 1, "one call in round 1: {calls}");
    assert_eq!(calls[0]["name"], "echo");
    rig.seat.assistant_calls("c/t2", 2, json!([]));
    rig.seat.msg("c/t2", 3, "tool", "result-of-echo");
    rig.seat.request("c/t2", 2, 4, 1);
    rig.until_state("c/t2-2", "done");
    let r = rig.seat.reader();
    let calls = cell(&r, "claude::Reply:c/t2-2", "tool_calls").unwrap();
    assert_eq!(calls.as_array().unwrap().len(), 1, "one call in round 2: {calls}");
    assert_eq!(calls[0]["name"], "other");
    rig.seat.assistant_calls("c/t2", 4, json!([]));
    rig.seat.msg("c/t2", 5, "tool", "result-of-other");
    rig.seat.request("c/t2", 3, 6, 1);
    rig.until_state("c/t2-3", "done");
    assert_eq!(text(&rig.seat.reader(), "c/t2-3"), "got: result-of-echo|result-of-other");
}

#[test]
fn a_cli_that_calls_two_tools_at_once_gets_an_error_for_the_second_and_can_call_it_again() {
    // not the measured CLI, but a later one might: the call that comes while the round is over is answered at once with an error that says what to do, never held for ever
    let mut rig = Rig::new("t4");
    rig.seat.round1("c/t4", "twoat t4", &["echo", "other"]);
    rig.until_state("c/t4-1", "done");
    let calls = cell(&rig.seat.reader(), "claude::Reply:c/t4-1", "tool_calls").unwrap();
    assert_eq!(calls.as_array().unwrap().len(), 1, "{calls}");
    rig.seat.assistant_calls("c/t4", 2, json!([]));
    rig.seat.msg("c/t4", 3, "tool", "result-of-echo");
    rig.seat.request("c/t4", 2, 4, 1);
    rig.until_state("c/t4-2", "done");
    // which of the two calls reaches the peer first is not fixed (two threads of the shim): exactly one of them is held and gets the result, the other is answered at once with the error
    let said = text(&rig.seat.reader(), "c/t4-2");
    assert!(said.contains("result-of-") && said.contains("takes one tool call at a time") && said.ends_with("|errs=1"), "one call got the result, the other the error: {said}");
}

#[test]
fn a_second_round_with_the_wrong_number_of_results_is_an_error() {
    let mut rig = Rig::new("t3");
    rig.seat.round1("c/t3", "tool t3", &["echo"]);
    rig.until_state("c/t3-1", "done");
    rig.seat.assistant_calls("c/t3", 2, json!([]));
    rig.seat.msg("c/t3", 3, "tool", "one");
    rig.seat.msg("c/t3", 4, "tool", "two");
    rig.seat.request("c/t3", 2, 5, 1);
    rig.until_state("c/t3-2", "error");
    assert!(cell(&rig.seat.reader(), "claude::Reply:c/t3-2", "error").unwrap().as_str().unwrap().contains("2"), "names the count of results");
}

#[test]
fn a_later_round_with_no_run_for_the_turn_is_an_error_not_a_guess() {
    let mut rig = Rig::new("n1");
    // the peer never saw round 1 (it was restarted): only round 2 is there
    rig.seat.msg("c/n1", 0, "system", "S"); rig.seat.msg("c/n1", 1, "user", "plain n1"); rig.seat.tools(&["echo"]);
    rig.seat.assistant_calls("c/n1", 2, json!([]));
    rig.seat.msg("c/n1", 3, "tool", "r");
    rig.seat.request("c/n1", 2, 4, 1);
    rig.until_state("c/n1-2", "error");
    assert!(cell(&rig.seat.reader(), "claude::Reply:c/n1-2", "error").unwrap().as_str().unwrap().contains("no run"));
}

#[test]
fn an_abort_ends_the_run_with_a_term_and_the_reply_is_aborted() {
    let mut rig = Rig::new("a1");
    rig.seat.round1("c/a1", "hang a1", &["echo"]);
    rig.until("the text so far", |r| cell(r, "claude::Reply:c/a1-1", "state") == Some(json!("running")) && text(r, "c/a1-1") == "thinking");
    let pid = logged("a1")["pid"].as_i64().unwrap();
    assert!(alive(pid));
    rig.seat.put("membrane::LlmRequest:c/a1-1", "abort", json!(true));
    rig.until_state("c/a1-1", "aborted");
    rig.until("the child is gone", |_| !alive(pid));
}

#[test]
fn a_request_aborted_before_it_starts_runs_nothing() {
    let mut rig = Rig::new("a2");
    rig.seat.round1("c/a2", "plain a2", &["echo"]);
    rig.seat.put("membrane::LlmRequest:c/a2-1", "abort", json!(true));
    rig.until_state("c/a2-1", "aborted");
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(!std::path::Path::new(&format!("{}/a2.json", common::log_dir())).exists(), "no child was started");
}

#[test]
fn retiring_the_request_retires_the_reply_and_its_chunks() {
    let mut rig = Rig::new("r1");
    rig.seat.round1("c/r1", "plain r1", &["echo"]);
    rig.until_state("c/r1-1", "done");
    rig.seat.retire("membrane::LlmRequest:c/r1-1");
    rig.until("reply gone", |r| cell(r, "claude::Reply:c/r1-1", "state").is_none() && cell(r, "claude::Chunk:c/r1-1:0", "text").is_none());
}

#[test]
fn retiring_the_request_of_a_run_waiting_for_a_tool_result_ends_the_child() {
    let mut rig = Rig::new("r2");
    rig.seat.round1("c/r2", "holdtool r2", &["echo"]);
    rig.until_state("c/r2-1", "done");
    let pid = logged("r2")["pid"].as_i64().unwrap();
    assert!(alive(pid), "the child holds the call");
    rig.seat.retire("membrane::LlmRequest:c/r2-1");
    rig.until("the child is gone", |_| !alive(pid));
}

#[test]
fn a_child_that_exits_non_zero_is_an_error_reply_with_its_stderr() {
    let mut rig = Rig::new("e1");
    rig.seat.round1("c/e1", "fail e1", &["echo"]);
    rig.until_state("c/e1-1", "error");
    let e = cell(&rig.seat.reader(), "claude::Reply:c/e1-1", "error").unwrap();
    let e = e.as_str().unwrap();
    assert!(e.contains("rc 3") && e.contains("boom: no auth"), "{e}");
}

#[test]
fn a_result_line_that_says_error_is_an_error_reply() {
    let mut rig = Rig::new("e2");
    rig.seat.round1("c/e2", "iserror e2", &["echo"]);
    rig.until_state("c/e2-1", "error");
    assert!(cell(&rig.seat.reader(), "claude::Reply:c/e2-1", "error").unwrap().as_str().unwrap().contains("the model said no"));
}

#[test]
fn a_reply_longer_than_the_four_chunks_the_seat_reads_is_an_error() {
    let mut rig = Rig::new("e3");
    rig.seat.round1("c/e3", "long e3", &["echo"]);   // 30000 bytes: 8 chunks of 4093
    rig.until_state("c/e3-1", "error");
    assert!(cell(&rig.seat.reader(), "claude::Reply:c/e3-1", "error").unwrap().as_str().unwrap().contains("chunk"));
}

#[test]
fn a_reply_is_cut_into_chunks_on_the_cell_size() {
    // 4093 bytes a chunk (the 4096 cap less the 3-byte CBOR header); the fake says 30000 only in the error test above, so cut a text of 9000 through the chunker itself
    let chunks = graphlink_claude::handler::cut(&"é".repeat(4500), 4093);
    assert_eq!(chunks.len(), 3);
    assert!(chunks.iter().all(|c| c.len() <= 4093));
    assert_eq!(chunks.concat(), "é".repeat(4500));
}

#[test]
fn the_api_key_is_given_to_the_child_and_appears_in_no_cell_no_argv_and_no_error() {
    let mut rig = Rig::new("k1");
    rig.seat.round1("c/k1", "plain k1", &["echo"]);
    rig.until_state("c/k1-1", "done");
    let l = logged("k1");
    assert_eq!(l["env"]["ANTHROPIC_API_KEY"], json!("KEY"), "the child has the key in its environment");
    assert!(!l["argv"].to_string().contains(KEY));
    // the credentials that outrank the key or stand in for it are removed in key mode; the auto-memory switch is the seat rung's
    for k in ["ANTHROPIC_AUTH_TOKEN", "CLAUDE_CODE_USE_BEDROCK", "CLAUDE_CODE_OAUTH_TOKEN"] { assert_eq!(l["env"][k], json!(false), "{k} must not reach the child"); }
    assert_eq!(l["env"]["CLAUDE_CODE_DISABLE_AUTO_MEMORY"], json!(true));
    // a child that prints the key in its error: the reply masks it
    rig.seat.round1("c/k2", "keyleak k2", &["echo"]);
    rig.until_state("c/k2-1", "error");
    let r = rig.seat.reader();
    let dump = format!("{:?}", [cell(&r, "claude::Reply:c/k2-1", "error"), cell(&r, "claude::Reply:c/k1-1", "state")]);
    assert!(!dump.contains(KEY), "{dump}");
    assert!(cell(&r, "claude::Reply:c/k2-1", "error").unwrap().as_str().unwrap().contains("rejected key"));
}

#[test]
fn the_login_is_used_only_when_asked_for_and_then_the_key_is_not_given() {
    let mut rig = Rig::with("l1", Auth::Login);
    rig.seat.round1("c/l1", "plain l1", &["echo"]);
    rig.until_state("c/l1-1", "done");
    let l = logged("l1");
    assert_eq!(l["env"]["ANTHROPIC_API_KEY"], json!(false));
    assert_eq!(l["env"]["ANTHROPIC_AUTH_TOKEN"], json!(false));
    assert_eq!(l["env"]["CLAUDE_CODE_USE_BEDROCK"], json!(false));
    assert_eq!(l["env"]["CLAUDE_CODE_OAUTH_TOKEN"], json!(true), "a login token is the login's own");
}

#[test]
fn a_request_missing_a_message_waits_for_it() {
    let mut rig = Rig::new("w1");
    rig.seat.msg("c/w1", 0, "system", "S");
    rig.seat.tools(&["echo"]);
    rig.seat.request("c/w1", 1, 2, 1);   // the user message is not written yet
    rig.ticks(10);
    assert_eq!(cell(&rig.seat.reader(), "claude::Reply:c/w1-1", "state"), None);
    rig.seat.msg("c/w1", 1, "user", "plain w1");
    rig.until_state("c/w1-1", "done");
}

#[test]
fn shutdown_ends_every_child_with_a_term() {
    let mut rig = Rig::new("s1");
    rig.seat.round1("c/s1", "hang s1", &["echo"]);
    rig.until("running", |r| text(r, "c/s1-1") == "thinking");
    let pid = logged("s1")["pid"].as_i64().unwrap();
    rig.h.shutdown();
    assert!(!alive(pid), "shutdown waits for the child to go");
}
