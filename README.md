# graphlink

A library for **GraphLink**, the protocol that joins separate cliffhanger VMs into one graph: a backend seat and a page, a seat and a device, a seat and a driver process.
It is the part of the protocol that needs no VM and no network; the VM ([cliffvm](https://github.com/chedim/babyseat)) and the transports are on the other side of it.

**Status: early.** One crate exists and one piece of the protocol is built (section "What is built"). The rest is design, kept in the babyseat repo: `docs/graph-link.md` (the protocol) and `docs/waves.md` (how it meets the engine's rounds).
Nothing here is published to crates.io; use it as a git dependency.

## The idea

- **Ownership by namespace.** Every connected VM is a root namespace (`membrane::` for a seat, `p7a::` for a page, `kitchen::` for a device). An entity belongs to the VM whose namespace its id starts with. There is no per-entity owner row.
- **Demand-driven replicas.** A VM holds a local copy of a foreign value only while something it runs reads it. The reads, not a query, decide the subscriptions, and a subscription nobody reads any more is dropped after a short grace.
- **Reads never block.** A read of a value that has not arrived yet answers "pending" and the rule that read it runs again when the value lands.
- **Writes are forwarded to the owner.** A write to a foreign entity is sent to its owner, who decides, applies it as its own write and answers with a receipt. Nothing is applied locally first.
- **One frame per peer per round.** A VM runs in rounds; at the end of one it sends the changes to each peer as a single frame, and takes the peer's answers as writes in a later round.
- **Transport independent.** The protocol is frames; a transport only carries them (HTTP long poll, WebSocket, MQTT, a pipe, shared memory).

## What is built

`graphlink-core` 0.0.1, moved unchanged out of cliffvm:

| Item | What it is |
|---|---|
| `LinkSubs` | the subscription table: `update` at the end of each round with the foreign keys rules hold and the ones read this round, `drain` for the `sub` and `unsub` notices since the last call, `due` for the next time a drop can be decided, `forget_prefix` for a namespace that left, `props_of` |
| `LINK_GRACE_S` | how long an unread subscription lives (0.5 s of engine time) |
| `Replica`, `ReplicaState` | a foreign entity's local copy: the revision applied, the props a value arrived for, and the owner's verdict (`Live`, `Gone`, `Refused`) |
| `is_special` | the attributes the VM provides itself, never replicated |
| `frame::Frame`, `frame::Op` | the frame and its ops (`hello`, `failed`, `sub`, `unsub`, `val`, `read`, `wr`, `res`, `bye`, `q`, `page`, `resync`, `pause`, `upgrade`, `upgraded`, `upgrade_refused`, and `Unknown` for an op this version does not know) with `decode` and `encode` to and from JSON; the spec is `docs/frames.md` |
| `cbor::encode`, `cbor::decode` | the CBOR subset JSON can hold: shortest integers, 64-bit floats, text, arrays, maps in insertion order; `decode` refuses tags, byte strings, indefinite lengths, NaN, duplicate keys and nesting past a limit it is given |
| `order::Inbox`, `order::Outbox` | the counters: `Inbox::take(n)` says `Accept`, `Duplicate` or `Gap { expected }`; `Outbox::frame` numbers frames from 1 and carries the `ack`; `acked` and `unacked` keep the peer's acknowledgements |
| `link::Link` | the state machine of one link to one peer, sans-I/O: `open`, `send`, `receive` (events for the engine: `Opened`, `Op`, `Duplicate`, `Gap`, `Failed`, `Closed`, `Refused`, and for an upgrade `UpgradeProposed`, `Upgraded`, `UpgradeRefused`; `propose`, `accept_upgrade`, `refuse_upgrade`) |
| `transport::Transport`, `transport::pipe` | the one edge to the outside (`send`, a non-blocking `recv`), and an in-memory pair for tests |
| `frame::negotiate` | the highest protocol version two peers share, or the `failed` op to answer with |

Not built yet (design only): the shared-memory transport, the CBOR encoding of whole frames (the value coder itself is built), the store trait (what the engine gives the library), the C interface and the language bindings. `docs/frames.md` lists the decisions that are still the operator's.

## Crates

| Crate | What it is |
|---|---|
| `graphlink-core` | the protocol: frames, the counters, the link state machine, the transport trait, the subscription table |
| `graphlink-net` | a TCP transport: one JSON frame per line, non-blocking (`Tcp`, `Listener`) |
| `graphlink-shm` | shared memory (Unix), no dependency: `Shm` is a transport (two rings in a named region, one per direction); `cells::Cells` and `cells::CellsReader` are the cell table (one slot per `(eid, prop)` key, a seqlock per slot, CBOR values, the owner writes and a reader maps it read-only) |
| `graphlink-cli` | the `graphlink` tool: a demo peer and a debugging aid |
| `graphlink-bridge` | `graphlink-bridge connect <addr> --peer <name> ...`: TCP outward, shared memory inward. To a seat it is a local peer that says the remote's namespace; the remote's values become cells, writes, reads, queries and subscriptions go to the remote, and a node outside the remote's namespace is dropped both ways. `connect` dials a remote, `listen` waits for one (a loopback address unless `--allow-remote`). A remote's subscription to a node of the seat's namespace goes to the seat, which answers by writing its cell table; the bridge reads those cells and sends the remote a `val` for each new one (`gone` for a node that is gone); a remote's write goes to the seat and the seat's receipt back. Not built: the `upgrade` from TCP to shared memory |

## The `graphlink` tool

```
graphlink serve 127.0.0.1:7070 --ns kitchen --nodes nodes.json    # own the nodes of the file: {"kitchen::Lamp:desk": {"power": "off"}}
graphlink watch 127.0.0.1:7070 kitchen::Lamp:desk                 # print the node as one JSON line, now and at every change
graphlink watch 127.0.0.1:7070 --prefix kitchen::                 # every node under a prefix
graphlink write 127.0.0.1:7070 kitchen::Lamp:desk power '"on"'    # prints ok, or "refused: why" and exits 1
```

`serve` answers `sub`, `unsub`, `read`, `wr` and the paged `q`; a write bumps the node's revision and is pushed to every subscriber. Nodes live in memory and are not saved.

## Using it

Add it as a git dependency, pinned to a commit:

```toml
[dependencies]
graphlink-core = { git = "https://github.com/chedim/graphlink", rev = "<commit>" }
```

At the end of every engine round, tell the table what the VM's rules read, and send whatever it drains to the owner (`graphlink-core/examples/subs.rs`, run with `cargo run -p graphlink-core --example subs`):

```rust
use std::collections::BTreeSet;
use graphlink_core::LinkSubs;

let mut subs = LinkSubs::default();
// held: foreign (entity, prop) keys that rules keep reading; read: the ones read in this round only; now: engine time in seconds
subs.update(&held, &read, now);
let (subscribe, unsubscribe) = subs.drain();   // JSON notices: [{"eid": "kitchen::Lamp:desk", "props": ["level", "power"]}]
// send `subscribe` and `unsubscribe` to their owners as `sub` and `unsub`; wake the engine at subs.due() if it is Some
```

When a peer leaves, `subs.forget_prefix("kitchen::")` forgets what it owned without a notice (its owner is gone).

## Threads

The crate is sans-I/O: it starts no threads, takes no locks and runs no async runtime. Its state belongs to the engine's thread, so a type must be `Send` and never needs `Sync`.
A transport is a thread (or a few) that only moves frames in and out; the engine thread owns every decision. This is how cliffvm's one engine thread already works with its drivers.

## Tests

```
cargo test
```

License: MIT.
