//! The turn handler: the seat's requests answered by `claude` children.
use crate::peer::Peer;
use graphlink_core::link::Event;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Auth { ApiKey, Login }

#[derive(Clone, Debug)]
pub struct Config { pub ns: String, pub claude_bin: String, pub model: String, pub auth: Auth, pub term_wait: Duration, pub exe: PathBuf }

pub struct Handler { _cfg: Config }
impl Handler {
    pub fn new(cfg: Config) -> Handler { Handler { _cfg: cfg } }
    pub fn tick(&mut self, peer: &mut Peer) -> Result<Vec<Event>, String> { peer.poll() }
    pub fn shutdown(&mut self) {}
}

/// The text cut into pieces of at most `cap` bytes, on character boundaries.
pub fn cut(_text: &str, _cap: usize) -> Vec<String> { Vec::new() }
