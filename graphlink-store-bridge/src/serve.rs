//! The loop of a program that serves a store: attach the regions the seat made, then turn until the seat's end of the rings is gone (or the process that started it is). `between` runs once per turn, before the store is
//! read, so that the program can change the store (the example takes commands on stdin).
use crate::bridge::Bridge;
use crate::cli::Serve;
use crate::mirror::Mirror;
use crate::peer::Peer;
use std::time::Duration;

pub fn run<M: Mirror>(cfg: &Serve, mirror: M, mut between: impl FnMut(&mut M)) -> Result<(), String> {
    let parent = if cfg.die_with_parent {
        std::env::var("GRAPHLINK_PARENT").ok().and_then(|v| v.parse::<u32>().ok()).unwrap_or_else(std::os::unix::process::parent_id)
    } else { 0 };
    let mut peer = Peer::attach(&cfg.peer, &cfg.ns, &cfg.numbers).map_err(|e| format!("cannot attach the seat's regions: {e}"))?;
    if let Some(path) = &cfg.token_file {
        let token = std::fs::read_to_string(path).map_err(|e| format!("cannot read the token file {path}: {e}"))?;
        peer = peer.with_token(token.trim().to_string());   // never printed
    }
    let mut bridge = Bridge::new(peer, &cfg.ns, mirror);
    eprintln!("graphlink-store-bridge: serving {} as {}", cfg.peer, cfg.ns);
    loop {
        between(&mut bridge.mirror);
        bridge.tick()?;
        if bridge.peer.is_closed() || (parent != 0 && std::os::unix::process::parent_id() != parent) { return Ok(()); }
        std::thread::sleep(Duration::from_millis(cfg.nap_ms));
    }
}
