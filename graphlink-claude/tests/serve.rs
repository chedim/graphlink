//! `graphlink-claude serve` as a process: it ends when the seat's end of the rings is gone, or the process that started it is; and it will not start without an API key
//! unless the login was asked for.
mod common;
use common::{cell, logged, FakeSeat, NUMBERS};
use serde_json::json;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn serve(seat: &FakeSeat, extra: &[&str]) -> Command {
    common::env_once();
    let mut c = Command::new(env!("CARGO_BIN_EXE_graphlink-claude"));
    c.args(["serve", "--peer", &seat.name, "--ns", "claude", "--claude-bin", &common::fake_claude(), "--model", "sonnet",
        "--slots", &NUMBERS.slots.to_string(), "--key-cap", &NUMBERS.key_cap.to_string(), "--val-cap", &NUMBERS.val_cap.to_string(), "--epoch", "1",
        "--spins", &NUMBERS.spins.to_string(), "--depth", &NUMBERS.depth.to_string(), "--nap-ms", "5", "--term-wait-ms", "5000"]);
    c.args(extra).stdout(Stdio::null()).stderr(Stdio::piped());
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
fn alive(pid: i64) -> bool { unsafe { libc::kill(pid as i32, 0) == 0 } }

#[test]
fn a_served_peer_answers_a_request_and_ends_with_the_seat_taking_its_child_along() {
    let mut seat = FakeSeat::start("v1");
    let mut p = serve(&seat, &[]).spawn().unwrap();
    // the seat's side: answer the hello, then write the turn
    let end = Instant::now() + Duration::from_secs(30);
    let r = loop {
        seat.pump();
        if let Ok(r) = graphlink_shm::cells::CellsReader::attach(format!("/gl-{}-pc", seat.name)) { break r; }
        assert!(Instant::now() < end, "the peer never made its table");
        std::thread::sleep(Duration::from_millis(20));
    };
    seat.round1("c/v1", "hang v1", &["echo"]);
    let end = Instant::now() + Duration::from_secs(30);
    while cell(&r, "claude::Reply:c/v1-1", "state") != Some(json!("running")) {
        seat.pump();
        assert!(Instant::now() < end, "no reply");
        std::thread::sleep(Duration::from_millis(20));
    }
    let pid = logged("v1")["pid"].as_i64().unwrap();
    assert!(alive(pid));
    drop(seat);   // the seat's end of the rings is gone
    let s = wait_exit(&mut p);
    assert!(s.success(), "{s:?}");
    assert!(!alive(pid), "the child was ended with the peer");
}

#[test]
fn a_dead_parent_ends_the_peer() {
    let seat = FakeSeat::start("v2");
    // a process that is already gone
    let mut gone = Command::new("true").spawn().unwrap(); let dead = gone.id(); gone.wait().unwrap();
    let mut p = serve(&seat, &["--die-with-parent"]).env("GRAPHLINK_PARENT", dead.to_string()).spawn().unwrap();
    let s = wait_exit(&mut p);
    assert!(s.success(), "{s:?}");
}

#[test]
fn without_a_key_the_api_key_mode_does_not_start_and_says_why_and_the_login_mode_does() {
    let seat = FakeSeat::start("v3");
    let mut p = serve(&seat, &[]).env_remove("ANTHROPIC_API_KEY").spawn().unwrap();
    let s = wait_exit(&mut p);
    assert_eq!(s.code(), Some(1));
    let mut err = String::new(); use std::io::Read; p.stderr.take().unwrap().read_to_string(&mut err).unwrap();
    assert!(err.contains("ANTHROPIC_API_KEY") && err.contains("--auth login"), "{err}");
    // the login mode needs no key; it ends with the seat
    let mut p = serve(&seat, &["--auth", "login"]).env_remove("ANTHROPIC_API_KEY").spawn().unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert!(p.try_wait().unwrap().is_none(), "it runs");
    drop(seat);
    assert!(wait_exit(&mut p).success());
}
