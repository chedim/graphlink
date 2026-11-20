//! The models this peer serves: it publishes them on its own node `<ns>::Models:main` (`names`, `backend`), so a seat routes those names to it without a
//! `cloud_models` entry; and a request names its model (`membrane::LlmRequest:<rid>.model`), which the child is started with.
mod common;
use common::{cell, logged, Rig};
use serde_json::json;

fn model_flag(tag: &str) -> Option<String> {
    let l = logged(tag);
    let argv: Vec<String> = l["argv"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
    argv.iter().position(|a| a == "--model").map(|i| argv[i + 1].clone())
}

#[test]
fn the_peer_publishes_the_models_it_serves_once_the_link_is_open() {
    let mut rig = Rig::new("m0");
    rig.until("the models node", |r| cell(r, "claude::Models:main", "names").is_some());
    let r = rig.seat.reader();
    assert_eq!(cell(&r, "claude::Models:main", "names"), Some(json!(["claude", "sonnet", "opus", "haiku"])));
    assert_eq!(cell(&r, "claude::Models:main", "backend"), Some(json!("claude-peer")));
}

#[test]
fn a_request_naming_an_alias_runs_the_child_with_that_alias() {
    let mut rig = Rig::new("m1");
    rig.seat.put("membrane::LlmRequest:c/m1-1", "model", json!("haiku"));
    rig.seat.round1("c/m1", "plain m1", &["echo"]);
    rig.until_state("c/m1-1", "done");
    assert_eq!(model_flag("m1").as_deref(), Some("haiku"));
}

#[test]
fn the_model_claude_and_a_request_with_no_model_run_the_owners_alias() {
    let mut rig = Rig::new("m2");
    rig.seat.put("membrane::LlmRequest:c/m2-1", "model", json!("claude"));
    rig.seat.round1("c/m2", "plain m2", &["echo"]);
    rig.until_state("c/m2-1", "done");
    assert_eq!(model_flag("m2").as_deref(), Some("sonnet"), "`claude` is the owner's --model");
    rig.seat.round1("c/m3", "plain m3", &["echo"]);
    rig.until_state("c/m3-1", "done");
    assert_eq!(model_flag("m3").as_deref(), Some("sonnet"), "no model: the owner's --model, as before");
}

#[test]
fn a_model_this_peer_does_not_serve_is_an_error_that_names_it_and_runs_nothing() {
    let mut rig = Rig::new("m4");
    rig.seat.put("membrane::LlmRequest:c/m4-1", "model", json!("gpt-5"));
    rig.seat.round1("c/m4", "plain m4", &["echo"]);
    rig.until_state("c/m4-1", "error");
    let e = cell(&rig.seat.reader(), "claude::Reply:c/m4-1", "error").unwrap().as_str().unwrap().to_string();
    assert!(e.contains("gpt-5") && e.contains("haiku"), "{e}");
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(!std::path::Path::new(&format!("{}/m4.json", common::log_dir())).exists(), "no child was started");
}
