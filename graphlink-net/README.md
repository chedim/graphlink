# graphlink-net

A TCP transport for [`graphlink-core`](https://crates.io/crates/graphlink-core): one JSON frame per line over a non-blocking socket. `recv` never waits, so an engine's thread can poll it at the end of a round; `send` queues the line and writes what the socket takes now.

- **`Tcp`** (`connect`, `from_stream`, `is_closed`) and **`Listener`** (`bind`, `accept`: `None` when nobody is there).
- **Feature `tls`** (rustls with ring, and rcgen): `Tcp::connect_tls`, `Listener::with_tls`, and `tls::make_certs`, which makes a self-signed `cert.pem` and `key.pem` (the key mode 0600) once. One certificate is the whole PKI: the server presents it, the client trusts exactly the file it names, and the certificate must cover the name dialled.

```rust
use graphlink_net::{Listener, Tcp};

let listener = Listener::bind("127.0.0.1:0").unwrap();
let addr = listener.local_addr().unwrap();
let mut client = Tcp::connect(addr).unwrap();
// poll `listener.accept()` for the server end, then `send` / `recv` frames on both
```

Part of [GraphLink](https://github.com/chedim/graphlink). MIT licensed.
