# graphlink-auth

HS256 tokens (JWT) for GraphLink peers. Standard library only: SHA-256, HMAC and base64url are written here, so there is nothing to audit but this crate.

- **`mint` / `mint_with`**: sign a token (`sub`, `name`, `iat`, `exp`, and extra claims such as `ns`, the namespace the holder may own). It refuses a secret under 32 bytes, an empty or control-character subject and a life under one second.
- **`verify`**: HS256 only, `exp` and `sub` required, 30 seconds of leeway on the time claims, a token of at most 8192 bytes. A refusal is only ever "invalid token" to the peer.
- **`check_hello`**: what a receiving side does with a `hello`: the token must verify, its `ns` claim must be the namespace the hello claims, and a reserved name (the seat's own) is never granted.

```rust
use graphlink_auth::{check_hello, mint_with, verify};
use serde_json::json;

let secret = b"0123456789abcdef0123456789abcdef"; // at least 32 bytes
let now = 1_800_000_000.0;
let token = mint_with(secret, "kitchen-bridge", "Kitchen", &[("ns", json!("kitchen"))], 3600.0, now).unwrap();
assert_eq!(verify(&token, secret, now).unwrap()["ns"], json!("kitchen"));
// a hello of the namespace `kitchen` is taken; the seat's own name is not
assert!(check_hello(Some(&token), "kitchen", &["membrane"], secret, now).is_ok());
assert!(check_hello(Some(&token), "hall", &["membrane"], secret, now).is_err());
```

Part of [GraphLink](https://github.com/chedim/graphlink). MIT licensed.
