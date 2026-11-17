# graphlink-core

The protocol core of **GraphLink**: how programs share a graph. No network, no threads, no VM: a state machine you drive with frames, so it runs over any transport (a socket, a pipe, shared memory, a radio).

- **Ownership by namespace.** A node id is `<namespace>::<Class>:<id>`; exactly one program owns each namespace. Others hold demand-driven replicas and ask the owner to write.
- **`Link`**: one link to one peer, sans-I/O. `open()` gives your `hello`; `receive(frame)` gives events (`Opened`, `Op`, `Closed`, ...) and maybe a reply; `send(ops)` gives the next frame. Frames carry counters, so a duplicate or a gap is noticed; versions are negotiated (the highest both sides speak); the `hello` can carry a token.
- **Ops:** `hello`, `sub` / `unsub`, `val`, `read`, `wr` / `res` (a forwarded write and its receipt), `q` / `page`, `resync`, `pause`, `upgrade`, `bye`. Unknown ops and fields are ignored, so versions can grow.
- **`LinkSubs` and `Replica`**: the subscription table and the notes about replicas an engine keeps.
- **`Transport`**: the one trait a carrier implements (`send` a frame, `recv` the next, never waiting). `pipe()` is an in-memory pair for tests. The `graphlink-net` and `graphlink-shm` crates carry TCP and shared memory.
- **`cbor`**: a JSON-subset CBOR coder (shortest integers, insertion-ordered maps; it refuses tags, byte strings, indefinite lengths and NaN).

```rust
use graphlink_core::frame::VERSION;
use graphlink_core::link::{Event, Link, Me};
use graphlink_core::transport::{pipe, Transport};

let me = |ns: &str| Me { ns: ns.into(), build: "demo".into(), epoch: 1.0, support: vec![VERSION], initiator: false };
let (mut ta, mut tb) = pipe();
let (mut a, mut b) = (Link::new(me("seat")), Link::new(me("kitchen")));
ta.send(&a.open()).unwrap();
tb.send(&b.open()).unwrap();
// what one side sends, the other receives
let (events, _reply) = a.receive(ta.recv().unwrap().unwrap());
assert!(matches!(&events[0], Event::Opened { peer_ns, .. } if peer_ns == "kitchen"));
```

The protocol (frames, ops, rules) is described in [docs/frames.md](https://github.com/chedim/graphlink/blob/main/docs/frames.md). MIT licensed.
