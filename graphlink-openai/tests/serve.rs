//! `graphlink-openai serve` as a process: it will not start without the key its owner named, answers a request, and ends when the seat's end of the rings is gone.
mod common;
use common::*;
use serde_json::json;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn serve(seat: &FakeSeat, base_url: &str, key_env: Option<&str>) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_graphlink-openai"));
    c.args(["serve", "--peer", &seat.name, "--ns", "openai", "--curl-bin", "curl", "--base-url", base_url, "--model", "gpt-test",
        "--slots", &NUMBERS.slots.to_string(), "--key-cap", &NUMBERS.key_cap.to_string(), "--val-cap", &NUMBERS.val_cap.to_string(), "--epoch", "1",
        "--spins", &NUMBERS.spins.to_string(), "--depth", &NUMBERS.depth.to_string(), "--nap-ms", "5", "--term-wait-ms", "5000"]);
    if let Some(k) = key_env { c.args(["--key-env", k]); }
    c.env("GLO_SERVE_KEY", KEY).env_remove("GLO_SERVE_MISSING").stdout(Stdio::null()).stderr(Stdio::piped());
    c
}
fn wait_exit(child: &mut std::process::Child) -> std::process::ExitStatus {
    let end = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(s) = child.try_wait().unwrap() { return s; }
        assert!(Instant::now() < end, "the peer did not end");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_served_peer_answers_a_request_and_ends_when_the_seat_goes() {
    let api = FakeApi::start(vec![Reply::Sse(vec![delta("served"), finish("stop")])]);
    let mut seat = FakeSeat::start("v1");
    let mut p = serve(&seat, &api.base_url(), Some("GLO_SERVE_KEY")).spawn().unwrap();
    let end = Instant::now() + Duration::from_secs(30);
    let r = loop {
        seat.pump();
        if let Ok(r) = graphlink_shm::cells::CellsReader::attach(format!("/gl-{}-pc", seat.name)) { break r; }
        assert!(Instant::now() < end, "the peer never made its table");
        std::thread::sleep(Duration::from_millis(20));
    };
    seat.round1("c/v1", "hi", &[]);
    let end = Instant::now() + Duration::from_secs(30);
    while cell(&r, "openai::Reply:c/v1-1", "state") != Some(json!("done")) {
        seat.pump();
        assert!(Instant::now() < end, "no reply");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(text(&r, "c/v1-1"), "served");
    assert_eq!(api.requests()[0].header("authorization"), Some(&format!("Bearer {KEY}")[..]), "the key is the named variable's");
    drop(seat);
    assert!(wait_exit(&mut p).success());
}

#[test]
fn a_peer_whose_key_variable_is_not_set_refuses_to_start_and_names_the_variable_not_a_value() {
    let seat = FakeSeat::start("v2");
    let out = serve(&seat, "http://127.0.0.1:1/v1", Some("GLO_SERVE_MISSING")).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("GLO_SERVE_MISSING"), "{err}");
}

#[test]
fn every_number_and_the_server_are_required_with_no_default() {
    let seat = FakeSeat::start("v3");
    for drop_opt in ["--base-url", "--model", "--curl-bin", "--slots", "--nap-ms"] {
        let mut c = serve(&seat, "http://127.0.0.1:1/v1", Some("GLO_SERVE_KEY"));
        let args: Vec<String> = c.get_args().map(|a| a.to_string_lossy().to_string()).collect();
        let i = args.iter().position(|a| a == drop_opt).unwrap();
        let mut kept = args.clone(); kept.drain(i..i + 2);
        let out = Command::new(env!("CARGO_BIN_EXE_graphlink-openai")).args(kept).stderr(Stdio::piped()).stdout(Stdio::null()).output().unwrap();
        assert!(!out.status.success() && String::from_utf8_lossy(&out.stderr).contains(drop_opt), "{drop_opt}");
        let _ = &mut c;
    }
}
