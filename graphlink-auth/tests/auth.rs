//! Tokens for graphlink peers: a link is bound to one verified token, and the token says which namespace its holder may own (the `ns` claim).
use graphlink_auth::{base64_decode, base64_encode, hex, hmac_sha256, mint, mint_with, sha256, verify};
use serde_json::json;

const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";
const NOW: f64 = 1_800_000_000.0;

#[test]
fn sha256_and_hmac_match_the_published_vectors() {
    assert_eq!(hex(&sha256(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    assert_eq!(hex(&sha256(b"")), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    // RFC 4231, test case 2
    assert_eq!(hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
}

#[test]
fn base64_round_trips_and_url_form_has_no_padding_or_plus_or_slash() {
    for data in [&b""[..], b"f", b"fo", b"foo", b"foob", b"\xfb\xff\xfe"] {
        assert_eq!(base64_decode(&base64_encode(data), false).unwrap(), data);
    }
    assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    assert_eq!(base64_decode("-_-_", true).unwrap(), base64_decode("+/+/", false).unwrap());
}

#[test]
fn a_minted_token_verifies_and_gives_back_its_claims() {
    let t = mint(SECRET, "esp32-kitchen", "Kitchen", 3600.0, NOW).unwrap();
    let c = verify(&t, SECRET, NOW + 10.0).unwrap();
    assert_eq!(c["sub"], json!("esp32-kitchen"));
    assert_eq!(c["name"], json!("Kitchen"));
    assert_eq!(c["exp"], json!(NOW as i64 + 3600));
}

#[test]
fn a_token_can_carry_the_namespace_its_holder_may_own() {
    let t = mint_with(SECRET, "kitchen-bridge", "Kitchen", &[("ns", json!("kitchen"))], 60.0, NOW).unwrap();
    assert_eq!(verify(&t, SECRET, NOW).unwrap()["ns"], json!("kitchen"));
    // a claim the signer owns cannot be overridden by the extras
    assert!(mint_with(SECRET, "a", "A", &[("exp", json!(1))], 60.0, NOW).is_err());
    assert!(mint_with(SECRET, "a", "A", &[("sub", json!("b"))], 60.0, NOW).is_err());
}

#[test]
fn a_token_that_is_expired_forged_or_odd_is_refused() {
    let t = mint(SECRET, "p", "P", 60.0, NOW).unwrap();
    assert!(verify(&t, SECRET, NOW + 60.0 + 31.0).is_err(), "past exp and the leeway");
    assert!(verify(&t, SECRET, NOW + 60.0 + 29.0).is_ok(), "within the leeway");
    assert!(verify(&t, b"another secret of more than thirty-two bytes", NOW).is_err(), "another key");
    let mut parts: Vec<&str> = t.split('.').collect();
    let forged_body = base64_encode(br#"{"sub":"admin","exp":9999999999}"#).trim_end_matches('=').replace('+', "-").replace('/', "_");
    parts[1] = &forged_body;
    assert!(verify(&parts.join("."), SECRET, NOW).is_err(), "a changed payload under the old signature");
    let none = format!("{}.{}.", base64_encode(br#"{"alg":"none"}"#).trim_end_matches('='), parts[1]);
    assert!(verify(&none, SECRET, NOW).is_err(), "alg none");
    assert!(verify("", SECRET, NOW).is_err());
    assert!(verify("a.b", SECRET, NOW).is_err());
}

#[test]
fn a_short_secret_a_bad_subject_and_a_bad_life_are_refused_at_mint() {
    assert!(mint(b"short", "p", "P", 60.0, NOW).is_err());
    assert!(mint(SECRET, "", "P", 60.0, NOW).is_err());
    assert!(mint(SECRET, "bad\nsub", "P", 60.0, NOW).is_err());
    assert!(mint(SECRET, "p", "P", 0.0, NOW).is_err());
    assert!(verify("x.y.z", b"short", NOW).is_err());
}
