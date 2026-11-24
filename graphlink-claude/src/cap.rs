//! The cap on a tool result the Claude CLI is handed (#1143, operator 2026-11-24). The CLI refuses a result over its own limit and the model then gets nothing; the seat does not know the CLI is there, so the
//! limit is kept HERE, where the result is given to the held call (`handler.rs`). A result of at most twice `cap` characters comes whole; a longer one is its first `cap` and its last `cap` characters, with a line
//! between them that says how much was left out and how long the whole is. Characters are counted, not bytes. The seat keeps the whole result; only the copy for the CLI is cut.
pub fn cap_result(text: &str, cap: usize) -> String {
    let total = text.chars().count();
    if total <= cap.saturating_mul(2) { return text.to_string(); }
    let head: String = text.chars().take(cap).collect();
    let tail: String = text.chars().skip(total - cap).collect();
    format!("{head}\n[{} characters left out of the middle: the result is {total} characters; the seat holds all of it]\n{tail}", total - 2 * cap)
}
