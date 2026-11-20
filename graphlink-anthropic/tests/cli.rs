//! The command line of `graphlink-anthropic serve`: every number is the owner's and has no default; the key is never an option, only the NAME of the variable that holds it.
use graphlink_anthropic::cli::parse;

fn args(s: &str) -> Vec<String> { s.split_whitespace().map(String::from).collect() }
const ALL: &str = "--peer p --ns anthropic --curl-bin curl --base-url https://api.example.test --model claude-x --max-tokens 1000 --anthropic-version 2023-06-01 --key-env MY_KEY --slots 64 --key-cap 128 --val-cap 4096 --epoch 1 --spins 1000 --depth 32 --nap-ms 5 --term-wait-ms 3000";

#[test]
fn all_the_options_make_a_config() {
    let c = parse(&args(ALL)).unwrap();
    assert_eq!((c.peer.as_str(), c.ns.as_str(), c.curl_bin.as_str(), c.base_url.as_str(), c.model.as_str(), c.max_tokens), ("p", "anthropic", "curl", "https://api.example.test", "claude-x", 1000));
    assert_eq!((c.version.as_str(), c.key_env.as_str()), ("2023-06-01", "MY_KEY"));
    assert_eq!((c.numbers.slots, c.numbers.key_cap, c.numbers.val_cap, c.numbers.epoch, c.numbers.spins, c.numbers.depth, c.nap_ms, c.term_wait_ms), (64, 128, 4096, 1, 1000, 32, 5, 3000));
    assert!(!c.die_with_parent && c.token_file.is_none());
}

#[test]
fn every_option_and_every_number_is_required() {
    for opt in ["--peer", "--ns", "--curl-bin", "--base-url", "--model", "--max-tokens", "--anthropic-version", "--key-env", "--slots", "--key-cap", "--val-cap", "--epoch", "--spins", "--depth", "--nap-ms", "--term-wait-ms"] {
        let mut v = args(ALL);
        let i = v.iter().position(|a| a == opt).unwrap();
        v.drain(i..i + 2);
        let e = parse(&v).unwrap_err();
        assert!(e.contains(opt), "{opt}: {e}");
    }
}

#[test]
fn a_key_option_does_not_exist_and_a_bad_number_is_an_error() {
    assert!(parse(&args(&format!("{ALL} --api-key sk-x"))).unwrap_err().contains("unknown option"));
    assert!(parse(&args(&ALL.replace("--max-tokens 1000", "--max-tokens lots"))).unwrap_err().contains("--max-tokens"));
    assert!(parse(&args(&ALL.replace("--max-tokens 1000", "--max-tokens 0"))).unwrap_err().contains("--max-tokens"));
}

#[test]
fn die_with_parent_and_the_token_file_are_accepted() {
    let c = parse(&args(&format!("{ALL} --die-with-parent --token-file /t"))).unwrap();
    assert!(c.die_with_parent);
    assert_eq!(c.token_file.as_deref(), Some("/t"));
}
