//! `library serve --peer <name> --ns <ns> --slots <n> --key-cap <n> --val-cap <n> --epoch <n> --spins <n> --depth <n> --nap-ms <n> [--token-file <f>] [--die-with-parent]`
//! serves the other project's README store (Library, Patron, Checkout, a few rows) to a seat as a read-only peer. To watch it follow a change, write a command on its stdin, one per line:
//!   rename-library <name>      the library's name
//!   add-patron <email>         a new patron
//!   remove-checkout <n>        the n-th checkout (1 or 2) goes
mod store;
use graphlink_store_bridge::cli::parse;
use std::io::BufRead;
use std::sync::mpsc::channel;
use store::Lib;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("serve") { eprintln!("usage: library serve --peer <name> --ns <ns> --slots <n> --key-cap <n> --val-cap <n> --epoch <n> --spins <n> --depth <n> --nap-ms <n> [--token-file <f>] [--die-with-parent]"); std::process::exit(1); }
    let cfg = parse(&args[1..]).unwrap_or_else(|e| { eprintln!("library: {e}"); std::process::exit(1) });
    let (lib, lib_id, _patrons, checkouts) = Lib::seeded();
    let (tx, rx) = channel::<String>();
    std::thread::spawn(move || { for l in std::io::stdin().lock().lines().map_while(Result::ok) { if tx.send(l).is_err() { break; } } });
    let result = graphlink_store_bridge::serve::run(&cfg, lib, move |lib| {
        while let Ok(line) = rx.try_recv() {
            let (cmd, rest) = line.split_once(' ').unwrap_or((line.as_str(), ""));
            match cmd {
                "rename-library" => { if let Some(l) = lib.store.libraries.get_mut(lib_id) { l.name = rest.to_string(); } }
                "add-patron" => { lib.add_patron(rest); }
                "remove-checkout" => { if let Some(c) = rest.trim().parse::<usize>().ok().and_then(|n| checkouts.get(n.wrapping_sub(1))) { let _ = lib.store.remove_checkout(*c); } }
                _ => eprintln!("library: unknown command {cmd:?}"),
            }
        }
    });
    if let Err(e) = result { eprintln!("library: {e}"); std::process::exit(1); }
}
