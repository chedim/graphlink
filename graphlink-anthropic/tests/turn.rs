//! The turn over shared memory: the seat writes messages, tools and a request; the peer makes ONE call of the Messages API per round (tests/common: a fake server, NOT the
//! real API; the HTTP is the machine's real `curl`) and publishes Reply and Chunk nodes. The API keeps no history, so every round's call carries the whole conversation.
mod common;
use common::*;
use graphlink_anthropic::handler::Config;
use serde_json::{json, Value as J};

fn reply_cell(r: &graphlink_shm::cells::CellsReader, rid: &str, prop: &str) -> Option<J> { cell(r, &format!("anthropic::Reply:{rid}"), prop) }

#[test]
fn a_plain_answer_is_one_call_with_the_owners_model_and_max_tokens_text_finish_stop_and_usage() {
    let api = FakeApi::start(|_, _| answer("hello from the api"));
    let mut rig = Rig::new("p1", &api);
    rig.seat.round1("c/p1", "hi", &["echo"]);
    rig.until_state("c/p1-1", "done");
    let r = rig.seat.reader();
    assert_eq!(text(&r, "c/p1-1"), "hello from the api");
    assert_eq!(reply_cell(&r, "c/p1-1", "finish"), Some(json!("stop")));
    assert_eq!(reply_cell(&r, "c/p1-1", "usage"), Some(json!({"read": 5, "generated": 7})));
    assert_eq!(reply_cell(&r, "c/p1-1", "tool_calls"), None);
    assert_eq!(api.calls(), 1);
    let b = api.body(0);
    assert_eq!(b["model"], json!("claude-test"));
    assert_eq!(b["max_tokens"], json!(MAX_TOKENS));
    assert_eq!(b["stream"], json!(true));
    assert_eq!(b["system"], json!("You are the graph."));
    assert_eq!(b["messages"], json!([{"role": "user", "content": [{"type": "text", "text": "hi"}]}]));
    assert_eq!(b["tools"], json!([{"name": "echo", "description": "the echo tool", "input_schema": {"type": "object", "properties": {"text": {"type": "string"}}}}]));
    assert_eq!(api.header(0, "x-api-key"), Some(KEY.to_string()));
    assert_eq!(api.header(0, "anthropic-version"), Some("2023-06-01".to_string()));
    assert_eq!(api.header(0, "content-type"), Some("application/json".to_string()));
}

#[test]
fn max_tokens_is_the_owners_number_whatever_it_is_and_a_request_with_no_tools_sends_none() {
    let api = FakeApi::start(|_, _| answer("ok"));
    let mut cfg = config(&api); cfg.max_tokens = 77;
    let mut rig = Rig::with("m1", cfg);
    rig.seat.round1("c/m1", "hi", &[]);
    rig.until_state("c/m1-1", "done");
    assert_eq!(api.body(0)["max_tokens"], json!(77));
    assert!(api.body(0).get("tools").is_none(), "{}", api.body(0));
}

#[test]
fn usage_maps_the_token_counts_onto_read_loaded_generated_and_leaves_out_what_is_not_there() {
    let api = FakeApi::start(|_, _| {
        let mut v = vec![message_start(json!({"input_tokens": 7, "cache_creation_input_tokens": 3, "cache_read_input_tokens": 50, "output_tokens": 1}))];
        v.extend(text_block(0, &["x"])); v.extend(end("end_turn", 2));
        Reply::Sse(v)
    });
    let mut rig = Rig::new("u1", &api);
    rig.seat.round1("c/u1", "cache", &[]);
    rig.until_state("c/u1-1", "done");
    // read: input + written to the prompt cache; loaded: served from the prompt cache; generated: the output tokens message_delta ends with
    assert_eq!(reply_cell(&rig.seat.reader(), "c/u1-1", "usage"), Some(json!({"read": 10, "loaded": 50, "generated": 2})));
}

#[test]
fn the_text_grows_chunk_by_chunk_across_text_blocks_and_a_length_stop_is_finish_length() {
    let api = FakeApi::start(|_, _| {
        let mut v = vec![message_start(json!({"input_tokens": 1, "output_tokens": 1}))];
        v.extend(text_block(0, &["one ", "two"])); v.extend(text_block(1, &["three"])); v.extend(end("max_tokens", 9));
        Reply::Sse(v)
    });
    let mut rig = Rig::new("l1", &api);
    rig.seat.round1("c/l1", "go", &[]);
    rig.until_state("c/l1-1", "done");
    let r = rig.seat.reader();
    assert_eq!(text(&r, "c/l1-1"), "one two\n\nthree");
    assert_eq!(reply_cell(&r, "c/l1-1", "finish"), Some(json!("length")));
}

fn tool_script(calls: usize) -> impl Fn(usize, &J) -> Reply + Send + Sync {
    move |i, _| {
        if i > 0 { return answer("pong received"); }
        let mut v = vec![message_start(json!({"input_tokens": 3, "output_tokens": 1}))];
        v.extend(text_block(0, &["let me ask"]));
        for k in 0..calls {
            if k == 0 { v.extend(tool_block(1, "toolu_API0", "echo", &["{\"te", "xt\": \"ping\"}"])); }
            else { v.extend(tool_block(1 + k as u64, &format!("toolu_API{k}"), "echo", &[&format!("{{\"text\": \"p{k}\"}}")])); }
        }
        v.extend(end("tool_use", 12));
        Reply::Sse(v)
    }
}

#[test]
fn a_tool_use_ends_the_round_and_the_next_round_sends_the_whole_history_with_the_result_paired_by_id() {
    let api = FakeApi::start(tool_script(1));
    let mut rig = Rig::new("t1", &api);
    rig.seat.round1("c/t1", "ping please", &["echo"]);
    rig.until_state("c/t1-1", "done");
    let r = rig.seat.reader();
    assert_eq!(text(&r, "c/t1-1"), "let me ask");
    assert_eq!(reply_cell(&r, "c/t1-1", "finish"), Some(json!("tool_calls")));
    assert_eq!(reply_cell(&r, "c/t1-1", "tool_calls"), Some(json!([{"name": "echo", "arguments": {"text": "ping"}}])));
    // the seat runs the tool and writes round 2: the assistant message with the call, the tool message with the result
    rig.seat.assistant_calls("c/t1", 2, "let me ask", json!([{"type": "function", "function": {"name": "echo", "arguments": {"text": "ping"}}}]));
    rig.seat.msg("c/t1", 3, "tool", "pong");
    rig.seat.request("c/t1", 2, 4, 1);
    rig.until_state("c/t1-2", "done");
    assert_eq!(text(&rig.seat.reader(), "c/t1-2"), "pong received");
    assert_eq!(api.calls(), 2, "one call per round");
    let m = api.body(1)["messages"].clone();
    assert_eq!(m.as_array().unwrap().len(), 3, "{m}");
    assert_eq!(m[0], json!({"role": "user", "content": [{"type": "text", "text": "ping please"}]}));
    assert_eq!(m[1]["role"], json!("assistant"));
    assert_eq!(m[1]["content"][0], json!({"type": "text", "text": "let me ask"}));
    assert_eq!(m[1]["content"][1]["type"], json!("tool_use"));
    assert_eq!(m[1]["content"][1]["name"], json!("echo"));
    assert_eq!(m[1]["content"][1]["input"], json!({"text": "ping"}));
    let id = m[1]["content"][1]["id"].as_str().unwrap().to_string();
    assert_eq!(m[2], json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": "pong"}]}));
    assert_eq!(api.body(1)["system"], json!("You are the graph."));
}

#[test]
fn several_calls_of_one_step_are_one_reply_and_their_results_are_one_user_message_paired_in_order() {
    let api = FakeApi::start(tool_script(2));
    let mut rig = Rig::new("t2", &api);
    rig.seat.round1("c/t2", "two", &["echo"]);
    rig.until_state("c/t2-1", "done");
    assert_eq!(reply_cell(&rig.seat.reader(), "c/t2-1", "tool_calls"), Some(json!([{"name": "echo", "arguments": {"text": "ping"}}, {"name": "echo", "arguments": {"text": "p1"}}])));
    // arguments the seat wrote as JSON text (the OpenAI form) are read too
    rig.seat.assistant_calls("c/t2", 2, "", json!([{"type": "function", "function": {"name": "echo", "arguments": "{\"text\": \"ping\"}"}}, {"type": "function", "function": {"name": "echo", "arguments": {"text": "p1"}}}]));
    rig.seat.msg("c/t2", 3, "tool", "first");
    rig.seat.msg("c/t2", 4, "tool", "second");
    rig.seat.request("c/t2", 2, 5, 1);
    rig.until_state("c/t2-2", "done");
    let m = api.body(1)["messages"].clone();
    assert_eq!(m.as_array().unwrap().len(), 3, "{m}");
    let uses = m[1]["content"].as_array().unwrap();
    assert_eq!(uses.len(), 2, "an empty text is not sent: {m}");
    assert_eq!(uses[0]["input"], json!({"text": "ping"}));
    assert_ne!(uses[0]["id"], uses[1]["id"]);
    let results = m[2]["content"].as_array().unwrap();
    assert_eq!((results[0]["tool_use_id"].clone(), results[0]["content"].clone()), (uses[0]["id"].clone(), json!("first")));
    assert_eq!((results[1]["tool_use_id"].clone(), results[1]["content"].clone()), (uses[1]["id"].clone(), json!("second")));
}

#[test]
fn results_that_do_not_match_the_calls_are_an_error_reply_and_no_call_is_made() {
    let api = FakeApi::start(|_, _| answer("never"));
    let mut rig = Rig::new("t3", &api);
    rig.seat.msg("c/t3", 0, "user", "hi");
    rig.seat.assistant_calls("c/t3", 1, "", json!([{"type": "function", "function": {"name": "echo", "arguments": {}}}]));
    rig.seat.msg("c/t3", 2, "tool", "a");
    rig.seat.msg("c/t3", 3, "tool", "b");
    rig.seat.request("c/t3", 2, 4, 0);
    rig.until_state("c/t3-2", "error");
    let e = reply_cell(&rig.seat.reader(), "c/t3-2", "error").unwrap().as_str().unwrap().to_string();
    assert!(e.contains("tool"), "{e}");
    assert_eq!(api.calls(), 0);
}

#[test]
fn an_api_error_body_is_an_error_reply_that_says_what_the_api_said_and_never_the_key() {
    let api = FakeApi::start(|_, _| Reply::Raw(401, format!("{{\"type\":\"error\",\"error\":{{\"type\":\"authentication_error\",\"message\":\"invalid x-api-key {KEY}\"}}}}")));
    let mut rig = Rig::new("e1", &api);
    rig.seat.round1("c/e1", "hi", &[]);
    rig.until_state("c/e1-1", "error");
    let e = reply_cell(&rig.seat.reader(), "c/e1-1", "error").unwrap().as_str().unwrap().to_string();
    assert!(e.contains("authentication_error") && e.contains("invalid x-api-key"), "{e}");
    assert!(e.contains("[key]") && !e.contains(KEY), "{e}");
}

#[test]
fn an_error_event_in_the_stream_is_an_error_reply() {
    let api = FakeApi::start(|_, _| Reply::Sse(vec![message_start(json!({"input_tokens": 1, "output_tokens": 1})), ("error".into(), json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}))]));
    let mut rig = Rig::new("e2", &api);
    rig.seat.round1("c/e2", "hi", &[]);
    rig.until_state("c/e2-1", "error");
    let e = reply_cell(&rig.seat.reader(), "c/e2-1", "error").unwrap().as_str().unwrap().to_string();
    assert!(e.contains("overloaded_error") && e.contains("Overloaded"), "{e}");
}

#[test]
fn a_server_that_is_not_there_is_an_error_reply_with_curls_words() {
    let dead = { let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); l.local_addr().unwrap().port() };   // closed again
    let api = FakeApi::start(|_, _| answer("never"));
    let mut cfg = config(&api); cfg.base_url = format!("http://127.0.0.1:{dead}");
    let mut rig = Rig::with("e3", cfg);
    rig.seat.round1("c/e3", "hi", &[]);
    rig.until_state("c/e3-1", "error");
    let e = reply_cell(&rig.seat.reader(), "c/e3-1", "error").unwrap().as_str().unwrap().to_string();
    assert!(e.contains("curl"), "{e}");
}

#[test]
fn a_reply_longer_than_the_four_chunks_the_seat_reads_is_an_error_that_says_so() {
    let api = FakeApi::start(|_, _| answer(&"x".repeat(17_000)));   // the chunk is 4096 - 3 bytes here: four hold 16372
    let mut rig = Rig::new("c1", &api);
    rig.seat.round1("c/c1", "long", &[]);
    rig.until_state("c/c1-1", "error");
    let e = reply_cell(&rig.seat.reader(), "c/c1-1", "error").unwrap().as_str().unwrap().to_string();
    assert!(e.contains("chunks"), "{e}");
}

#[test]
fn abort_ends_the_call_and_the_connection_goes() {
    let api = FakeApi::start(|_, _| Reply::Hang);
    let mut rig = Rig::new("a1", &api);
    rig.seat.round1("c/a1", "hang", &[]);
    let t = std::time::Instant::now();
    while api.open.load(std::sync::atomic::Ordering::SeqCst) == 0 { rig.tick(); assert!(t.elapsed().as_secs() < 60, "no call arrived"); std::thread::sleep(std::time::Duration::from_millis(10)); }
    rig.seat.put("membrane::LlmRequest:c/a1-1", "abort", json!(true));
    rig.until_state("c/a1-1", "aborted");
    let t = std::time::Instant::now();
    while api.open.load(std::sync::atomic::Ordering::SeqCst) != 0 { rig.tick(); assert!(t.elapsed().as_secs() < 60, "the connection stayed open"); std::thread::sleep(std::time::Duration::from_millis(10)); }
}

#[test]
fn the_key_is_never_on_curls_command_line() {
    let api = FakeApi::start(|_, _| answer("ok"));
    let dir = std::env::temp_dir().join(format!("gla-wrap-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("argv.log");
    let wrap = dir.join("curl");
    std::fs::write(&wrap, format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\nexec curl \"$@\"\n", log.display())).unwrap();
    { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&wrap, std::fs::Permissions::from_mode(0o755)).unwrap(); }
    let mut cfg = config(&api); cfg.curl_bin = wrap.to_string_lossy().to_string();
    let mut rig = Rig::with("k1", cfg);
    rig.seat.round1("c/k1", "hi", &[]);
    rig.until_state("c/k1-1", "done");
    let argv = std::fs::read_to_string(&log).unwrap();
    assert!(!argv.contains(KEY) && !argv.contains("sk-ant"), "{argv}");
    assert_eq!(api.header(0, "x-api-key"), Some(KEY.to_string()), "it still reached the API");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn when_the_seat_retires_a_request_its_reply_and_chunks_are_retired() {
    let api = FakeApi::start(|_, _| answer("bye"));
    let mut rig = Rig::new("r1", &api);
    rig.seat.round1("c/r1", "hi", &[]);
    rig.until_state("c/r1-1", "done");
    rig.seat.retire("membrane::LlmRequest:c/r1-1");
    rig.until("the reply retired", |r| cell(r, "anthropic::Reply:c/r1-1", "state").is_none());
    assert_eq!(cell(&rig.seat.reader(), "anthropic::Chunk:c/r1-1:0", "text"), None);
}

#[test]
fn a_config_is_all_owner_values_with_no_defaults() {
    let api = FakeApi::start(|_, _| answer("x"));
    let c: Config = config(&api);
    assert_eq!((c.model.as_str(), c.max_tokens, c.version.as_str(), c.key_env.as_str()), ("claude-test", MAX_TOKENS, "2023-06-01", KEY_ENV));
}
