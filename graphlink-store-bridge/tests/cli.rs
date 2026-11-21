//! The command line the example (and any program that serves a store) takes: every number is the owner's and has no default, and the program says which one is missing.
use graphlink_store_bridge::cli::parse;

fn args(a: &[&str]) -> Vec<String> { a.iter().map(|s| s.to_string()).collect() }
const ALL: [&str; 18] = ["--peer", "lib1", "--ns", "lib", "--slots", "256", "--key-cap", "128", "--val-cap", "4096", "--epoch", "1", "--spins", "1000000", "--depth", "32", "--nap-ms", "50"];

#[test]
fn every_number_is_required_and_the_error_names_the_missing_one() {
    let ok = parse(&args(&ALL)).unwrap();
    assert_eq!((ok.peer.as_str(), ok.ns.as_str(), ok.numbers.slots, ok.nap_ms), ("lib1", "lib", 256, 50));
    for (i, opt) in ALL.iter().enumerate().filter(|(i, _)| i % 2 == 0) {
        let mut v: Vec<&str> = ALL.to_vec(); v.drain(i..i + 2);
        let e = parse(&args(&v)).unwrap_err();
        assert!(e.contains(opt) && e.contains("no default"), "{opt}: {e}");
    }
}

#[test]
fn an_unknown_option_or_a_value_that_is_not_a_number_is_refused() {
    let mut v = ALL.to_vec(); v.push("--nap"); v.push("1");
    assert!(parse(&args(&v)).unwrap_err().contains("unknown option --nap"));
    let mut w = ALL.to_vec(); w[17] = "soon";
    assert!(parse(&args(&w)).unwrap_err().contains("not a number"));
}
