//! `graphlink-claude serve <options>`: the program the seat starts from its `peers` fact. It attaches the regions the seat made and answers the seat's requests with `claude`
//! children until the seat's end of the rings is gone (or the process that started it is). `graphlink-claude mcp --socket <path>` is the stdio MCP server Claude Code spawns.
use graphlink_claude::cli::parse;
use graphlink_claude::handler::{Auth, Config, Handler};
use graphlink_claude::peer::Peer;
use std::process::exit;
use std::time::Duration;

fn fail(why: &str) -> ! { eprintln!("graphlink-claude: {why}"); exit(1) }

fn serve(args: &[String]) -> ! {
    let cfg = parse(args).unwrap_or_else(|e| fail(&e));
    // the key is the environment's, checked here so that a seat that cannot answer says so at the start and not at the first turn; it is never printed
    if cfg.auth == Auth::ApiKey && std::env::var("ANTHROPIC_API_KEY").map(|k| k.is_empty()).unwrap_or(true) {
        fail("ANTHROPIC_API_KEY is not set in the environment of this process: set it, or start with --auth login to use the claude CLI's own login (a deliberate choice)");
    }
    let parent = if cfg.die_with_parent {
        std::env::var("GRAPHLINK_PARENT").ok().and_then(|v| v.parse::<u32>().ok()).unwrap_or_else(std::os::unix::process::parent_id)
    } else { 0 };
    let mut peer = Peer::attach(&cfg.peer, &cfg.ns, &cfg.numbers).unwrap_or_else(|e| fail(&format!("cannot attach the seat's regions: {e}")));
    if let Some(path) = &cfg.token_file {
        let token = std::fs::read_to_string(path).unwrap_or_else(|e| fail(&format!("cannot read the token file {path}: {e}")));
        peer = peer.with_token(token.trim().to_string());   // never printed
    }
    let exe = std::env::current_exe().unwrap_or_else(|e| fail(&format!("cannot find this program: {e}")));
    let mut handler = Handler::new(Config { ns: cfg.ns.clone(), claude_bin: cfg.claude_bin.clone(), model: cfg.model.clone(), auth: cfg.auth, term_wait: Duration::from_millis(cfg.term_wait_ms), exe });
    eprintln!("graphlink-claude: serving {} as {} (model {})", cfg.peer, cfg.ns, cfg.model);
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
        Some("mcp") => match (args.get(1).map(String::as_str), args.get(2)) {
            (Some("--socket"), Some(path)) => { if let Err(e) = graphlink_claude::mcp::run(path) { fail(&e); } }
            _ => fail("usage: graphlink-claude mcp --socket <path>"),
        },
        _ => fail("usage: graphlink-claude serve --peer <name> --ns <namespace> --claude-bin <path> --model <alias> --slots <n> --key-cap <n> --val-cap <n> --epoch <n> --spins <n> --depth <n> --nap-ms <n> --term-wait-ms <n> [--auth api_key|login] [--token-file <path>] [--die-with-parent]"),
    }
}
