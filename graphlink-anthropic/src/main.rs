//! `graphlink-anthropic serve <options>`: the program the seat starts from its `peers` fact. It attaches the regions the seat made and answers the seat's requests with calls of
//! the Messages API until the seat's end of the rings is gone (or the process that started it is).
use graphlink_anthropic::cli::parse;
use graphlink_anthropic::handler::{Config, Handler};
use graphlink_anthropic::peer::Peer;
use std::process::exit;
use std::time::Duration;

fn fail(why: &str) -> ! { eprintln!("graphlink-anthropic: {why}"); exit(1) }

fn serve(args: &[String]) -> ! {
    let cfg = parse(args).unwrap_or_else(|e| fail(&e));
    // the key is the environment's, checked here so that a seat that cannot answer says so at the start and not at the first turn; it is never printed
    if std::env::var(&cfg.key_env).map(|k| k.is_empty()).unwrap_or(true) {
        fail(&format!("{} is not set in the environment of this process: it must hold the API key", cfg.key_env));
    }
    let parent = if cfg.die_with_parent {
        std::env::var("GRAPHLINK_PARENT").ok().and_then(|v| v.parse::<u32>().ok()).unwrap_or_else(std::os::unix::process::parent_id)
    } else { 0 };
    let mut peer = Peer::attach(&cfg.peer, &cfg.ns, &cfg.numbers).unwrap_or_else(|e| fail(&format!("cannot attach the seat's regions: {e}")));
    if let Some(path) = &cfg.token_file {
        let token = std::fs::read_to_string(path).unwrap_or_else(|e| fail(&format!("cannot read the token file {path}: {e}")));
        peer = peer.with_token(token.trim().to_string());   // never printed
    }
    let mut handler = Handler::new(Config { ns: cfg.ns.clone(), curl_bin: cfg.curl_bin.clone(), base_url: cfg.base_url.clone(), model: cfg.model.clone(), max_tokens: cfg.max_tokens,
        version: cfg.version.clone(), key_env: cfg.key_env.clone(), term_wait: Duration::from_millis(cfg.term_wait_ms) });
    eprintln!("graphlink-anthropic: serving {} as {} (model {}, {})", cfg.peer, cfg.ns, cfg.model, cfg.base_url);
    loop {
        if let Err(e) = handler.tick(&mut peer) { handler.shutdown(); fail(&e); }
        if peer.is_closed() || (parent != 0 && std::os::unix::process::parent_id() != parent) { handler.shutdown(); exit(0); }
        std::thread::sleep(Duration::from_millis(cfg.nap_ms));
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("serve") => serve(&args[1..]),
        _ => fail("usage: graphlink-anthropic serve --peer <name> --ns <namespace> --curl-bin <path> --base-url <url> --model <name> --max-tokens <n> --anthropic-version <v> --key-env <VAR> --slots <n> --key-cap <n> --val-cap <n> --epoch <n> --spins <n> --depth <n> --nap-ms <n> --term-wait-ms <n> [--token-file <path>] [--die-with-parent]"),
    }
}
