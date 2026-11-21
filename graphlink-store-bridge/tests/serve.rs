//! The example (`examples/library`) as a process, over the real shared-memory regions: it serves the README store, follows a change made while it runs (a command on its stdin), refuses to start without a number, and ends
//! when the seat's end of the rings is gone. The binary is built by `cargo test` (examples are) and found next to the test's own.
mod common;
use common::*;
use serde_json::json;
use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn example() -> std::path::PathBuf {
    // where the example binary is depends on the build layout (the test binary may be in `deps/` or in a separate build directory), so cargo is asked: `cargo build --example library` (a no-op when `cargo test` built it) prints the path
    static PATH: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    PATH.get_or_init(|| {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let out = Command::new(cargo).args(["build", "-p", "graphlink-store-bridge", "--example", "library", "--message-format=json"]).current_dir(env!("CARGO_MANIFEST_DIR")).output().unwrap();
        assert!(out.status.success(), "cargo build --example library failed: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|m| m["reason"] == "compiler-artifact" && m["target"]["name"] == "library" && m["target"]["kind"].to_string().contains("example"))
            .filter_map(|m| m["executable"].as_str().map(std::path::PathBuf::from)).last().expect("cargo did not report the example's path")
    }).clone()
}
fn serve(seat: &FakeSeat) -> Child {
    Command::new(example()).args(["serve", "--peer", &seat.name, "--ns", NS, "--slots", &NUMBERS.slots.to_string(), "--key-cap", &NUMBERS.key_cap.to_string(), "--val-cap", &NUMBERS.val_cap.to_string(),
        "--epoch", "1", "--spins", &NUMBERS.spins.to_string(), "--depth", &NUMBERS.depth.to_string(), "--nap-ms", "5"]).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap()
}
fn until(what: &str, mut f: impl FnMut() -> bool) { let end = Instant::now() + Duration::from_secs(60); while !f() { assert!(Instant::now() < end, "timed out waiting for {what}"); std::thread::sleep(Duration::from_millis(20)); } }
fn find_library(r: &graphlink_shm::cells::CellsReader) -> Option<String> {
    r.scan(&format!("{NS}::Library:"), 1_000_000, 32).ok()?.into_iter().find_map(|(k, _)| k.split('\u{1}').next().map(String::from))
}

#[test]
fn the_example_serves_the_store_follows_a_change_and_ends_with_the_seat() {
    let mut seat = FakeSeat::start("p1");
    let mut p = serve(&seat);
    let mut r = None;
    until("the peer's table", || { seat.pump(); r = graphlink_shm::cells::CellsReader::attach(format!("/gl-{}-pc", seat.name)).ok(); r.is_some() });
    let r = r.unwrap();
    until("the library row", || { seat.pump(); find_library(&r).is_some() });
    let lib = find_library(&r).unwrap();
    until("its name", || { seat.pump(); cell(&r, &lib, "name") == Some(json!("City Library")) });
    p.stdin.as_mut().unwrap().write_all(b"rename-library Main Library\n").unwrap();
    until("the rename to follow", || { seat.pump(); cell(&r, &lib, "name") == Some(json!("Main Library")) });
    p.stdin.as_mut().unwrap().write_all(b"remove-checkout 1\n").unwrap();
    until("a row to be retired", || { seat.pump(); r.scan(&format!("{NS}::Checkout:"), 1_000_000, 32).unwrap().iter().any(|(_, c)| c.state == graphlink_shm::cells::CellState::Gone) });
    drop(seat);   // the seat's end of the rings is gone
    let end = Instant::now() + Duration::from_secs(60);
    let s = loop { if let Some(s) = p.try_wait().unwrap() { break s; } assert!(Instant::now() < end, "the peer did not end"); std::thread::sleep(Duration::from_millis(20)); };
    assert!(s.success(), "{s:?}");
}

#[test]
fn the_example_will_not_start_without_a_number_and_says_which() {
    let out = Command::new(example()).args(["serve", "--peer", "x", "--ns", NS, "--slots", "8"]).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("missing --key-cap") && err.contains("no default"), "{err}");
}
