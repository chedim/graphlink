//! graphlink: a debugging tool and a demo peer for graphlink-core.
//!
//!   graphlink serve <addr> --ns <namespace> --nodes <file.json> [--secret-file <file>]   own the nodes of the file ({id: {prop: value}}) and answer peers; with a secret a peer's hello must carry a token (below)
//!   graphlink watch <addr> <id>  |  graphlink watch <addr> --prefix <prefix>   print each value of the nodes as one JSON line, the current one first
//!   graphlink write <addr> <id> <prop> <json>                      write a prop of a node (prints `ok` or `refused: why`; exit 1 when refused)
//!   graphlink token mint --secret-file <file> --sub <sub> --ns <namespace> --ttl <seconds>   print a token that lets its holder own <namespace> (the life is required: no default)
//!   watch and write take `--token-file <file>` (the token their hello carries) and `--ns <namespace>` (what their hello says; default graphlink-cli)
use std::collections::BTreeMap;
use std::io::Write;
use std::process::exit;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use graphlink_auth::{check_hello, mint_with};
use graphlink_core::frame::{Op, QueryBy, Target, ValBody, VERSION};
use graphlink_core::link::{Event, Link, Me};
use graphlink_core::transport::Transport;
use graphlink_net::{Listener, Tcp};
use serde_json::{json, Map, Value as J};

fn usage(msg: &str) -> ! {
    eprintln!("{msg}\nusage:\n  graphlink serve <addr> --ns <namespace> --nodes <file.json> [--secret-file <file>]\n  graphlink watch <addr> <id> | --prefix <prefix> [--token-file <file>] [--ns <namespace>]\n  graphlink write <addr> <id> <prop> <json> [--token-file <file>] [--ns <namespace>]\n  graphlink token mint --secret-file <file> --sub <sub> --ns <namespace> --ttl <seconds>");
    exit(2)
}

fn me(ns: &str, initiator: bool) -> Me {
    let epoch = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
    Me { ns: ns.into(), build: format!("graphlink-cli {}", env!("CARGO_PKG_VERSION")), epoch, support: vec![VERSION], initiator }
}

/// take `--name <value>` out of the arguments (anywhere), and give the value
fn take(args: &[String], name: &str) -> (Vec<String>, Option<String>) {
    let mut rest = vec![];
    let mut value = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == name { value = Some(args.get(i + 1).cloned().unwrap_or_else(|| usage(&format!("{name} needs a value")))); i += 2; } else { rest.push(args[i].clone()); i += 1; }
    }
    (rest, value)
}

fn read_secret(path: &str) -> Vec<u8> {
    std::fs::read_to_string(path).unwrap_or_else(|e| { eprintln!("cannot read {path}: {e}"); exit(1) }).trim().as_bytes().to_vec()
}

fn now() -> f64 { SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0) }

fn token_mint(args: &[String]) {
    let (args, secret) = take(args, "--secret-file");
    let (args, sub) = take(&args, "--sub");
    let (args, ns) = take(&args, "--ns");
    let (args, ttl) = take(&args, "--ttl");
    if let Some(extra) = args.first() { usage(&format!("unknown option {extra}")); }
    let secret = secret.unwrap_or_else(|| usage("token mint needs --secret-file"));
    let sub = sub.unwrap_or_else(|| usage("token mint needs --sub"));
    let ns = ns.unwrap_or_else(|| usage("token mint needs --ns"));
    let ttl: f64 = ttl.unwrap_or_else(|| usage("token mint needs --ttl (seconds; the life is the owner's number: there is no default)")).parse().unwrap_or_else(|_| usage("--ttl is a number of seconds"));
    match mint_with(&read_secret(&secret), &sub, &sub, &[("ns", json!(ns))], ttl, now()) {
        Ok(t) => println!("{t}"),
        Err(e) => { eprintln!("{e}"); exit(1) }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("serve") => serve(&args[1..]),
        Some("watch") => watch(&args[1..]),
        Some("write") => write(&args[1..]),
        Some("token") if args.get(1).map(String::as_str) == Some("mint") => token_mint(&args[2..]),
        _ => usage("a command is needed"),
    }
}

// ---- serve ------------------------------------------------------------------------------------------------------------------------------------

struct Node { props: Map<String, J>, r: f64 }

struct Conn { tcp: Tcp, link: Link, subs: Vec<Target>, dead: bool }

fn matches(t: &Target, id: &str) -> bool { match t { Target::Id(i) => i == id, Target::Prefix(p) => id.starts_with(p.as_str()) } }

fn val(id: &str, node: &Node, props: Option<&Vec<String>>, rid: Option<String>) -> Op {
    let body = match props {
        None => J::Object(node.props.clone()),
        Some(ps) => J::Object(node.props.iter().filter(|(k, _)| ps.contains(k)).map(|(k, v)| (k.clone(), v.clone())).collect()),
    };
    Op::Val { id: id.to_string(), r: node.r, rid, body: ValBody::Props(body) }
}

fn serve(args: &[String]) {
    let addr = args.first().unwrap_or_else(|| usage("serve needs an address"));
    let (mut ns, mut file, mut secret_file) = (None, None, None);
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() { "--ns" => { ns = args.get(i + 1); i += 2; } "--nodes" => { file = args.get(i + 1); i += 2; } "--secret-file" => { secret_file = args.get(i + 1); i += 2; } o => usage(&format!("unknown option {o}")) }
    }
    let ns = ns.unwrap_or_else(|| usage("serve needs --ns"));
    let file = file.unwrap_or_else(|| usage("serve needs --nodes"));
    let text = std::fs::read_to_string(file).unwrap_or_else(|e| { eprintln!("cannot read {file}: {e}"); exit(1) });
    let parsed: J = serde_json::from_str(&text).unwrap_or_else(|e| { eprintln!("{file} is not JSON: {e}"); exit(1) });
    let mut nodes: BTreeMap<String, Node> = parsed.as_object().unwrap_or_else(|| { eprintln!("{file} must be an object of nodes"); exit(1) })
        .iter().map(|(id, p)| (id.clone(), Node { props: p.as_object().cloned().unwrap_or_default(), r: 1.0 })).collect();

    let secret: Option<Vec<u8>> = secret_file.map(|p| read_secret(p));
    let listener = Listener::bind(addr.as_str()).unwrap_or_else(|e| { eprintln!("cannot listen on {addr}: {e}"); exit(1) });
    println!("listening {}", listener.local_addr().unwrap());
    std::io::stdout().flush().ok();

    let mut conns: Vec<Conn> = vec![];
    loop {
        while let Ok(Some(mut tcp)) = listener.accept() {
            let mut link = Link::new(me(ns, false));
            let _ = tcp.send(&link.open());
            conns.push(Conn { tcp, link, subs: vec![], dead: false });
        }
        let mut changed: Vec<String> = vec![];
        for c in conns.iter_mut() {
            while let Ok(Some(frame)) = c.tcp.recv() {
                let (events, reply) = c.link.receive(frame);
                if let Some(r) = reply { let _ = c.tcp.send(&r); }
                for ev in events {
                    // with a secret, a hello must carry a token that verifies and names the namespace the hello says; otherwise `bye`, `refused: auth`, and the connection ends
                    if let (Event::Opened { peer_ns, peer_token, .. }, Some(secret)) = (&ev, secret.as_ref()) {
                        if let Err(why) = check_hello(peer_token.as_deref(), peer_ns, &[ns.as_str()], secret, now()) {
                            eprintln!("graphlink: hello of {peer_ns:?} refused: {why}");
                            if let Ok(f) = c.link.send(vec![Op::Bye { why: "refused: auth".into() }]) { let _ = c.tcp.send(&f); }
                            c.dead = true;
                            break;
                        }
                        continue;
                    }
                    let Event::Op(op) = ev else { continue };
                    let mut out: Vec<Op> = vec![];
                    match op {
                        Op::Sub { target, props, .. } => {
                            for (id, n) in nodes.iter().filter(|(id, _)| matches(&target, id)) { out.push(val(id, n, props.as_ref(), None)); }
                            c.subs.push(target);
                        }
                        Op::Unsub { target } => c.subs.retain(|t| *t != target),
                        Op::Read { id, props, rid } => out.push(match nodes.get(&id) {
                            Some(n) => val(&id, n, Some(&props), Some(rid)),
                            None => Op::Val { id, r: 0.0, rid: Some(rid), body: ValBody::Gone },
                        }),
                        Op::Wr { wid, id, prop, value } => match nodes.get_mut(&id) {
                            Some(n) => { n.props.insert(prop, value); n.r += 1.0; changed.push(id); out.push(Op::Res { wid, refused: None }); }
                            None => out.push(Op::Res { wid, refused: Some(format!("no such node: {id}")) }),
                        },
                        Op::Query { qid, by, after, limit } => {
                            let mut ids: Vec<&String> = nodes.keys().filter(|id| match &by { QueryBy::Prefix(p) => id.starts_with(p.as_str()), QueryBy::Class(c) => id.split("::").nth(1).and_then(|rest| rest.split(':').next()) == Some(c.as_str()) }).collect();
                            if let Some(a) = &after { ids.retain(|id| id.as_str() > a.as_str()); }
                            let total = ids.len() as u64;
                            let take = limit.unwrap_or(u64::MAX) as usize;
                            let more = ids.len() > take;
                            ids.truncate(take);
                            let next = if more { ids.last().map(|s| s.to_string()) } else { None };
                            out.push(Op::Page { qid, rows: ids.iter().map(|id| json!({"id": id})).collect(), next, total: Some(total) });
                        }
                        _ => {}
                    }
                    if !out.is_empty() { if let Ok(f) = c.link.send(out) { let _ = c.tcp.send(&f); } }
                }
            }
        }
        // a write pushes the new value to every connection subscribed to the node
        for id in changed {
            let Some(n) = nodes.get(&id) else { continue };
            for c in conns.iter_mut().filter(|c| c.link.is_open() && c.subs.iter().any(|t| matches(t, &id))) {
                if let Ok(f) = c.link.send(vec![val(&id, n, None, None)]) { let _ = c.tcp.send(&f); }
            }
        }
        conns.retain(|c| !c.tcp.is_closed() && !c.dead);
        std::thread::sleep(Duration::from_millis(5));
    }
}

// ---- the client side: connect, open, wait for the peer's hello --------------------------------------------------------------------------------

fn connect(addr: &str, ns: &str, token_file: Option<&str>) -> (Tcp, Link) {
    let mut tcp = Tcp::connect(addr).unwrap_or_else(|e| { eprintln!("cannot connect to {addr}: {e}"); exit(1) });
    let mut link = Link::new(me(ns, true));
    if let Some(f) = token_file { link = link.with_token(std::fs::read_to_string(f).unwrap_or_else(|e| { eprintln!("cannot read {f}: {e}"); exit(1) }).trim().to_string()); }
    tcp.send(&link.open()).ok();
    let end = Instant::now() + Duration::from_secs(5);
    while !link.is_open() {
        if Instant::now() > end { eprintln!("{addr} did not answer the hello"); exit(1) }
        match tcp.recv() {
            Ok(Some(f)) => {
                let (events, reply) = link.receive(f);
                if let Some(r) = reply { tcp.send(&r).ok(); }
                if let Some(Event::Failed { why }) = events.iter().find(|e| matches!(e, Event::Failed { .. })) { eprintln!("{why}"); exit(1) }
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(e) => { eprintln!("{e}"); exit(1) }
        }
    }
    (tcp, link)
}

fn watch(args: &[String]) {
    let (args, token_file) = take(args, "--token-file");
    let (args, ns) = take(&args, "--ns");
    let args = &args[..];
    let addr = args.first().unwrap_or_else(|| usage("watch needs an address"));
    let target = match (args.get(1).map(String::as_str), args.get(2)) {
        (Some("--prefix"), Some(p)) => Target::Prefix(p.clone()),
        (Some(id), None) if !id.starts_with("--") => Target::Id(id.to_string()),
        _ => usage("watch needs an id, or --prefix <prefix>"),
    };
    let (mut tcp, mut link) = connect(addr, ns.as_deref().unwrap_or("graphlink-cli"), token_file.as_deref());
    tcp.send(&link.send(vec![Op::Sub { target, props: None, limit: None }]).unwrap()).ok();
    loop {
        match tcp.recv() {
            Ok(Some(f)) => {
                let (events, reply) = link.receive(f);
                if let Some(r) = reply { tcp.send(&r).ok(); }
                for ev in events {
                    match ev {
                        Event::Op(Op::Val { id, r, body, .. }) => {
                            let mut o = json!({"id": id, "r": r});
                            match body { ValBody::Props(p) => o["props"] = p, ValBody::Gone => o["gone"] = json!(true), ValBody::Refused(w) => o["refused"] = json!(w) }
                            println!("{o}");
                            std::io::stdout().flush().ok();
                        }
                        Event::Closed { .. } | Event::Failed { .. } => return,
                        _ => {}
                    }
                }
            }
            Ok(None) => { if tcp.is_closed() { return; } std::thread::sleep(Duration::from_millis(5)); }
            Err(e) => { eprintln!("{e}"); exit(1) }
        }
    }
}

fn write(args: &[String]) {
    let (args, token_file) = take(args, "--token-file");
    let (args, ns) = take(&args, "--ns");
    let args = &args[..];
    if args.len() != 4 { usage("write needs <addr> <id> <prop> <json>"); }
    let value: J = serde_json::from_str(&args[3]).unwrap_or_else(|e| usage(&format!("the value is not JSON: {e}")));
    let (mut tcp, mut link) = connect(&args[0], ns.as_deref().unwrap_or("graphlink-cli"), token_file.as_deref());
    tcp.send(&link.send(vec![Op::Wr { wid: "w1".into(), id: args[1].clone(), prop: args[2].clone(), value }]).unwrap()).ok();
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end {
        match tcp.recv() {
            Ok(Some(f)) => {
                let (events, _) = link.receive(f);
                for ev in events {
                    if let Event::Closed { why } = &ev { println!("refused: {why}"); exit(1) }
                    if let Event::Op(Op::Res { refused, .. }) = ev {
                        tcp.send(&link.send(vec![Op::Bye { why: "done".into() }]).unwrap()).ok();
                        match refused { None => { println!("ok"); return; } Some(why) => { println!("refused: {why}"); exit(1) } }
                    }
                }
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(e) => { eprintln!("{e}"); exit(1) }
        }
    }
    eprintln!("no answer"); exit(1)
}
