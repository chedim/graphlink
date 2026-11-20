//! `graphlink-anthropic serve` as a process: it will not start when the variable the owner named holds no key, and it does not print the key.
use std::process::{Command, Stdio};

#[test]
fn serve_refuses_to_start_without_the_key_in_the_variable_it_was_told_to_read_and_says_which() {
    let out = Command::new(env!("CARGO_BIN_EXE_graphlink-anthropic"))
        .args(["serve", "--peer", "nobody", "--ns", "anthropic", "--curl-bin", "curl", "--base-url", "http://127.0.0.1:1", "--model", "m", "--max-tokens", "10", "--anthropic-version", "2023-06-01",
               "--key-env", "GLA_NO_SUCH_KEY", "--slots", "8", "--key-cap", "64", "--val-cap", "512", "--epoch", "1", "--spins", "100", "--depth", "8", "--nap-ms", "5", "--term-wait-ms", "100"])
        .env_remove("GLA_NO_SUCH_KEY").stdout(Stdio::null()).stderr(Stdio::piped()).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let e = String::from_utf8_lossy(&out.stderr);
    assert!(e.contains("GLA_NO_SUCH_KEY"), "{e}");
}
