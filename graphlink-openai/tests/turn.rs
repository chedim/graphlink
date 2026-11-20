//! The turn over shared memory: the seat writes messages, tools and a request; the peer makes ONE HTTP call to the server's chat completions (a fake server here, NOT a real one,
//! reached by the real `curl`) and publishes Reply and Chunk nodes. The API keeps nothing: a round that ends in tool calls is answered by the seat's next request, which carries the whole history.
mod common;
use common::*;
use serde_json::json;

#[test]
fn a_plain_answer_is_one_call_with_the_history_the_tools_and_the_key_and_one_reply() {
    let api = FakeApi::start(vec![Reply::Sse(vec![delta("hel"), delta("lo"), finish("stop"), usage(json!({"prompt_tokens": 10, "completion_tokens": 3, "total_tokens": 13}))])]);
    let mut rig = Rig::new("p1", &api.base_url());
    rig.seat.round1("c/p1", "hi p1", &["echo"]);
    rig.until_state("c/p1-1", "done");
    let r = rig.seat.reader();
    assert_eq!(text(&r, "c/p1-1"), "hello");
    assert_eq!(cell(&r, "openai::Reply:c/p1-1", "finish"), Some(json!("stop")));
    assert_eq!(cell(&r, "openai::Reply:c/p1-1", "tool_calls"), None);
    let reqs = api.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].path, "/v1/chat/completions");
    assert_eq!(reqs[0].header("authorization"), Some(&format!("Bearer {KEY}")[..]));
    let b = &reqs[0].body;
    assert_eq!(b["model"], json!("gpt-test"));
    assert_eq!(b["stream"], json!(true));
    assert_eq!(b["stream_options"], json!({"include_usage": true}));
    assert_eq!(b["messages"], json!([{"role": "system", "content": "You are the graph."}, {"role": "user", "content": "hi p1"}]));
    assert_eq!(b["tools"][0]["function"]["name"], json!("echo"));
    assert_eq!(b["max_tokens"], json!(100), "the request's max_new is the server's max_tokens");
}

#[test]
fn with_no_tools_and_no_max_new_the_body_has_neither() {
    let api = FakeApi::start(vec![Reply::Sse(vec![delta("ok"), finish("stop")])]);
    let mut rig = Rig::new("p2", &api.base_url());
    rig.seat.msg("c/p2", 0, "user", "hi");
    rig.seat.request("c/p2", 1, 1, 0, json!({}));
    rig.until_state("c/p2-1", "done");
    let b = &api.requests()[0].body;
    assert!(b.get("tools").is_none() && b.get("max_tokens").is_none(), "{b}");
}

#[test]
fn usage_read_is_the_fresh_prompt_loaded_the_cached_part_generated_the_completion() {
    let api = FakeApi::start(vec![Reply::Sse(vec![delta("x"), finish("stop"), usage(json!({"prompt_tokens": 10, "completion_tokens": 3, "prompt_tokens_details": {"cached_tokens": 4}}))])]);
    let mut rig = Rig::new("u1", &api.base_url());
    rig.seat.round1("c/u1", "u", &[]);
    rig.until_state("c/u1-1", "done");
    assert_eq!(cell(&rig.seat.reader(), "openai::Reply:c/u1-1", "usage"), Some(json!({"read": 6, "loaded": 4, "generated": 3})));
}

#[test]
fn a_server_that_sends_no_usage_leaves_the_usage_cell_out() {
    let api = FakeApi::start(vec![Reply::Sse(vec![delta("x"), finish("stop")])]);
    let mut rig = Rig::new("u2", &api.base_url());
    rig.seat.round1("c/u2", "u", &[]);
    rig.until_state("c/u2-1", "done");
    assert_eq!(cell(&rig.seat.reader(), "openai::Reply:c/u2-1", "usage"), None);
}

#[test]
fn a_tool_call_ends_the_round_and_the_next_request_carries_the_whole_history_with_the_call_ids_made_up_in_order() {
    let api = FakeApi::start(vec![
        Reply::Sse(vec![call_delta(0, Some("call_a"), Some("echo"), ""), call_delta(0, None, None, "{\"te"), call_delta(0, None, None, "xt\":\"ping\"}"), call_delta(1, Some("call_b"), Some("echo"), "{}"), finish("tool_calls")]),
        Reply::Sse(vec![delta("pong"), finish("stop")]),
    ]);
    let mut rig = Rig::new("t1", &api.base_url());
    rig.seat.round1("c/t1", "tool t1", &["echo"]);
    rig.until_state("c/t1-1", "done");
    let r = rig.seat.reader();
    assert_eq!(cell(&r, "openai::Reply:c/t1-1", "finish"), Some(json!("tool_calls")));
    assert_eq!(cell(&r, "openai::Reply:c/t1-1", "tool_calls"), Some(json!([{"name": "echo", "arguments": {"text": "ping"}}, {"name": "echo", "arguments": {}}])));
    // round 2, as the seat writes it: the assistant message with its calls (no ids), then one tool message per call
    rig.seat.assistant_calls("c/t1", 2, "", json!([
        {"type": "function", "function": {"name": "echo", "arguments": "{\"text\": \"ping\"}"}},
        {"type": "function", "function": {"name": "echo", "arguments": "{}"}}]));
    rig.seat.msg("c/t1", 3, "tool", "ping back");
    rig.seat.msg("c/t1", 4, "tool", "second");
    rig.seat.request("c/t1", 2, 5, 1, json!({"max_new": 100}));
    rig.until_state("c/t1-2", "done");
    assert_eq!(text(&rig.seat.reader(), "c/t1-2"), "pong");
    let m = &api.requests()[1].body["messages"];
    assert_eq!(m.as_array().unwrap().len(), 5, "the whole history is sent again");
    assert_eq!(m[2]["role"], json!("assistant"));
    let id0 = m[2]["tool_calls"][0]["id"].as_str().unwrap().to_string();
    let id1 = m[2]["tool_calls"][1]["id"].as_str().unwrap().to_string();
    assert_ne!(id0, id1);
    assert_eq!(m[2]["tool_calls"][0]["type"], json!("function"));
    assert_eq!(m[2]["tool_calls"][0]["function"], json!({"name": "echo", "arguments": "{\"text\": \"ping\"}"}));
    assert_eq!(m[3], json!({"role": "tool", "tool_call_id": id0, "content": "ping back"}));
    assert_eq!(m[4], json!({"role": "tool", "tool_call_id": id1, "content": "second"}));
}

#[test]
fn a_tool_message_with_no_call_before_it_is_an_error_reply_and_no_call_is_made() {
    let api = FakeApi::start(vec![]);
    let mut rig = Rig::new("t2", &api.base_url());
    rig.seat.msg("c/t2", 0, "user", "hi");
    rig.seat.msg("c/t2", 1, "tool", "orphan");
    rig.seat.request("c/t2", 1, 2, 0, json!({}));
    rig.until_state("c/t2-1", "error");
    assert!(cell(&rig.seat.reader(), "openai::Reply:c/t2-1", "error").unwrap().as_str().unwrap().contains("tool"));
    assert!(api.requests().is_empty());
}

#[test]
fn a_status_that_is_not_ok_is_an_error_reply_with_the_body_and_the_key_masked() {
    let api = FakeApi::start(vec![Reply::Status(401, format!("{{\"error\": {{\"message\": \"bad key {KEY}\"}}}}"))]);
    let mut rig = Rig::new("e1", &api.base_url());
    rig.seat.round1("c/e1", "hi", &[]);
    rig.until_state("c/e1-1", "error");
    let e = cell(&rig.seat.reader(), "openai::Reply:c/e1-1", "error").unwrap();
    let e = e.as_str().unwrap();
    assert!(e.contains("401") && e.contains("bad key") && e.contains("[key]"), "{e}");
    assert!(!e.contains(KEY), "the key is in the error: {e}");
}

#[test]
fn a_server_that_cannot_be_reached_is_an_error_reply_from_curl() {
    let port = { let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); l.local_addr().unwrap().port() };   // nobody listens on it now
    let mut rig = Rig::new("e2", &format!("http://127.0.0.1:{port}/v1"));
    rig.seat.round1("c/e2", "hi", &[]);
    rig.until_state("c/e2-1", "error");
    let e = cell(&rig.seat.reader(), "openai::Reply:c/e2-1", "error").unwrap();
    assert!(e.as_str().unwrap().contains("curl"), "{e}");
}

#[test]
fn an_error_object_in_the_stream_is_an_error_reply() {
    let api = FakeApi::start(vec![Reply::Sse(vec![delta("par"), json!({"error": {"message": "the model is overloaded"}}).to_string()])]);
    let mut rig = Rig::new("e3", &api.base_url());
    rig.seat.round1("c/e3", "hi", &[]);
    rig.until_state("c/e3-1", "error");
    assert!(cell(&rig.seat.reader(), "openai::Reply:c/e3-1", "error").unwrap().as_str().unwrap().contains("overloaded"));
}

#[test]
fn an_abort_ends_the_call_and_the_reply_says_aborted() {
    let api = FakeApi::start(vec![Reply::SseThenHang(vec![delta("part")])]);
    let mut rig = Rig::new("a1", &api.base_url());
    rig.seat.round1("c/a1", "hang", &[]);
    rig.until("the text has begun", |r| text(r, "c/a1-1") == "part");
    rig.seat.put("membrane::LlmRequest:c/a1-1", "abort", json!(true));
    rig.until_state("c/a1-1", "aborted");
    api.wait_disconnect();
}

#[test]
fn a_retired_request_ends_its_call_and_retires_its_reply() {
    let api = FakeApi::start(vec![Reply::SseThenHang(vec![delta("part")])]);
    let mut rig = Rig::new("r1", &api.base_url());
    rig.seat.round1("c/r1", "hang", &[]);
    rig.until("the text has begun", |r| text(r, "c/r1-1") == "part");
    rig.seat.retire("membrane::LlmRequest:c/r1-1");
    let dis = api.disconnected.clone();
    rig.until("the call is ended", |_| *dis.lock().unwrap() > 0);
    rig.until("the reply is retired", |r| cell(r, "openai::Reply:c/r1-1", "state").is_none());
}

#[test]
fn a_text_longer_than_the_four_chunks_the_seat_reads_is_an_error_reply_and_the_call_is_ended() {
    let piece = "x".repeat(2000);
    let api = FakeApi::start(vec![Reply::SseThenHang((0..10).map(|_| delta(&piece)).collect())]);   // 20000 bytes: more than four chunks of 4093
    let mut rig = Rig::new("c1", &api.base_url());
    rig.seat.round1("c/c1", "long", &[]);
    rig.until_state("c/c1-1", "error");
    assert!(cell(&rig.seat.reader(), "openai::Reply:c/c1-1", "error").unwrap().as_str().unwrap().contains("chunks"));
    api.wait_disconnect();
}

#[test]
fn a_reply_cut_by_the_servers_length_limit_says_so_in_finish() {
    let api = FakeApi::start(vec![Reply::Sse(vec![delta("cut"), finish("length")])]);
    let mut rig = Rig::new("l1", &api.base_url());
    rig.seat.round1("c/l1", "hi", &[]);
    rig.until_state("c/l1-1", "done");
    assert_eq!(cell(&rig.seat.reader(), "openai::Reply:c/l1-1", "finish"), Some(json!("length")));
}
