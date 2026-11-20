//! The command line of `graphlink-claude serve`.
use crate::handler::Auth;
use crate::peer::Numbers;

#[derive(Debug, PartialEq)]
pub struct Serve {
    pub peer: String, pub ns: String, pub claude_bin: String, pub model: String, pub auth: Auth,
    pub numbers: Numbers, pub nap_ms: u64, pub term_wait_ms: u64, pub die_with_parent: bool, pub token_file: Option<String>,
}

pub fn parse(_args: &[String]) -> Result<Serve, String> { Err("not built".into()) }
