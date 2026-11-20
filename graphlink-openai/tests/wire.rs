//! The pieces of the wire that are pure: the body of a call, the stream's accumulation, the curl command (the key is in its configuration on stdin, never in its argv).
use graphlink_openai::http::{curl_argv, curl_config};
use graphlink_openai::wire::{build_body, usage_of, Msg, Stream};
use serde_json::json;

fn msg(role: &str, content: &str) -> Msg { Msg { role: role.into(), content: content.into(), tool_calls: None } }

#[test]
fn the_key_is_in_the_curl_configuration_and_nowhere_in_its_argv_or_the_body_file_name() {
    let argv = curl_argv("/tmp/glo-1/body");
    let cfg = curl_config("http://h/v1/chat/completions", Some("sk-SECRET"), "/tmp/glo-1/body");
    assert!(!argv.join(" ").contains("sk-SECRET") && !argv.join(" ").to_lowercase().contains("authorization"), "{argv:?}");
    assert!(cfg.contains("Authorization: Bearer sk-SECRET"), "{cfg}");
    assert!(cfg.contains("http://h/v1/chat/completions") && cfg.contains("@/tmp/glo-1/body"));
    assert!(argv.iter().any(|a| a == "-K") || argv.iter().any(|a| a == "--config"), "the configuration is read from stdin: {argv:?}");
}

#[test]
fn a_peer_with_no_key_sends_no_authorization_header() {
    assert!(!curl_config("http://h/x", None, "/b").to_lowercase().contains("authorization"));
}

#[test]
fn a_value_with_quotes_or_backslashes_cannot_break_out_of_its_configuration_line() {
    let cfg = curl_config("http://h/x", Some("a\"b\\c\nurl = \"http://evil\""), "/b");
    assert_eq!(cfg.lines().filter(|l| l.starts_with("url")).count(), 1, "{cfg}");
}

#[test]
fn the_body_has_the_model_the_stream_switches_and_the_tools_only_when_there_are_some() {
    let b = build_body("m", &[msg("user", "hi")], &[], None).unwrap();
    assert_eq!(b, json!({"model": "m", "messages": [{"role": "user", "content": "hi"}], "stream": true, "stream_options": {"include_usage": true}}));
    let spec = json!({"type": "function", "function": {"name": "echo"}});
    let b = build_body("m", &[msg("user", "hi")], &[spec.clone()], Some(7)).unwrap();
    assert_eq!(b["tools"], json!([spec]));
    assert_eq!(b["max_tokens"], json!(7));
}

#[test]
fn an_assistant_message_gets_ids_for_its_calls_and_each_tool_message_takes_the_next_one() {
    let a = Msg { role: "assistant".into(), content: "".into(), tool_calls: Some(json!([{"type": "function", "function": {"name": "f", "arguments": "{}"}}, {"type": "function", "function": {"name": "g", "arguments": {"k": 1}}}])) };
    let b = build_body("m", &[msg("user", "x"), a, msg("tool", "r1"), msg("tool", "r2")], &[], None).unwrap();
    let m = &b["messages"];
    assert_eq!(m[1]["tool_calls"][1]["function"]["arguments"], json!("{\"k\":1}"), "arguments go as JSON text");
    assert_eq!(m[2]["tool_call_id"], m[1]["tool_calls"][0]["id"]);
    assert_eq!(m[3]["tool_call_id"], m[1]["tool_calls"][1]["id"]);
    assert!(build_body("m", &[msg("tool", "orphan")], &[], None).is_err());
    assert!(build_body("m", &[msg("user", "x"), msg("tool", "r")], &[], None).is_err());
}

#[test]
fn a_stream_accumulates_text_calls_finish_and_usage_and_knows_when_it_is_done() {
    let mut s = Stream::default();
    assert!(s.feed(&json!({"choices": [{"delta": {"content": "a"}}]}).to_string()).unwrap());
    assert!(!s.feed(&json!({"choices": [{"delta": {"role": "assistant"}}]}).to_string()).unwrap(), "no text came");
    s.feed(&json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "i", "function": {"name": "f", "arguments": "{\"a\""}}]}}]}).to_string()).unwrap();
    s.feed(&json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "function": {"arguments": ":1}"}}]}, "finish_reason": "tool_calls"}]}).to_string()).unwrap();
    s.feed(&json!({"choices": [], "usage": {"prompt_tokens": 5, "completion_tokens": 2}}).to_string()).unwrap();
    assert!(!s.done);
    s.feed("[DONE]").unwrap();
    assert!(s.done);
    assert_eq!(s.text, "a");
    assert_eq!(s.calls().unwrap(), vec![json!({"name": "f", "arguments": {"a": 1}})]);
    assert_eq!(s.finish.as_deref(), Some("tool_calls"));
    assert_eq!(s.usage, Some(json!({"read": 5, "generated": 2})));
}

#[test]
fn tool_call_arguments_that_are_not_json_are_an_error_not_a_guess() {
    let mut s = Stream::default();
    s.feed(&json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "function": {"name": "f", "arguments": "{oops"}}]}}]}).to_string()).unwrap();
    assert!(s.calls().unwrap_err().contains("f"));
}

#[test]
fn an_error_object_or_a_line_that_is_not_json_is_an_error() {
    let mut s = Stream::default();
    assert!(s.feed(&json!({"error": {"message": "boom"}}).to_string()).unwrap_err().contains("boom"));
    assert!(s.feed("not json").is_err());
}

#[test]
fn usage_leaves_out_what_the_server_did_not_say() {
    assert_eq!(usage_of(&json!({"completion_tokens": 3})), Some(json!({"generated": 3})));
    assert_eq!(usage_of(&json!({})), None);
    assert_eq!(usage_of(&json!({"prompt_tokens": 2, "prompt_tokens_details": {"cached_tokens": 5}})), Some(json!({"read": 0, "loaded": 5})));
}
