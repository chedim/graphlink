//! A tool result reaches the Claude CLI capped (#1143, operator 2026-11-24: the Claude CLI refuses a tool result over its own limit and the model then gets nothing, so the limit is kept here, where the CLI is,
//! and not in the seat's door). A result of at most twice the cap comes whole; a longer one is its first `cap` and last `cap` characters with a line between them that says how much is left out.
use graphlink_claude::cap::cap_result;

#[test]
fn a_short_result_is_not_touched() {
    assert_eq!(cap_result("hello", 8), "hello");
    let exact = "x".repeat(16);
    assert_eq!(cap_result(&exact, 8), exact, "twice the cap fits whole");
    assert_eq!(cap_result("", 8), "");
}

#[test]
fn a_long_result_is_its_head_and_its_tail_with_the_size_between() {
    let long: String = (0..100).map(|i| char::from(b'a' + (i % 26) as u8)).collect();
    let out = cap_result(&long, 10);
    assert!(out.starts_with(&long[..10]), "the first 10 characters: {out}");
    assert!(out.ends_with(&long[90..]), "the last 10 characters: {out}");
    assert!(out.contains("80 characters") && out.contains("100"), "it says what is left out and the whole size: {out}");
    assert!(out.len() < long.len() + 80, "and it is not much longer than the cap: {} vs {}", out.len(), long.len());
}

#[test]
fn it_counts_characters_not_bytes() {
    let long: String = "я".repeat(50);   // two bytes each
    let out = cap_result(&long, 5);
    assert!(out.starts_with(&"я".repeat(5)) && out.ends_with(&"я".repeat(5)), "{out}");
    assert!(out.contains("40 characters"), "{out}");
    assert_eq!(cap_result(&"я".repeat(10), 5), "я".repeat(10), "ten characters is twice the cap: whole");
}
