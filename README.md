# graphlink

[![DOI](https://zenodo.org/badge/DOI/10.5281/zenodo.24351364.svg)](https://doi.org/10.5281/zenodo.24351364)

A library for **GraphLink**, the protocol that joins separate cliffhanger VMs into one graph: a backend seat and a page, a seat and a device, a seat and a driver process.
It is the part of the protocol that needs no VM and no network; the VM ([cliffvm](https://github.com/chedim/babyseat)) and the transports are on the other side of it.

**Status: 0.1.3, published on crates.io** (`graphlink-core`, `-auth`, `-net`, `-shm`, `-bridge`, `-cli`). The protocol core, a TCP and a shared-memory transport, tokens, TLS and a bridge between a seat and a remote are built and tested; the cliffvm seat uses them (babyseat 0.7.63). What is not built is listed under "What is built". The design and its decisions are in the babyseat repo: `docs/graph-link.md` (the protocol and the seat's side, section 18) and `docs/waves.md` (how it meets the engine's rounds).

## The idea

- **Ownership by namespace.** Every connected VM is a root namespace (`membrane::` for a seat, `p7a::` for a page, `kitchen::` for a device). An entity belongs to the VM whose namespace its id starts with. There is no per-entity owner row.
- **Demand-driven replicas.** A VM holds a local copy of a foreign value only while something it runs reads it. The reads, not a query, decide the subscriptions, and a subscription nobody reads any more is dropped after a short grace.
- **Reads never block.** A read of a value that has not arrived yet answers "pending" and the rule that read it runs again when the value lands.
- **Writes are forwarded to the owner.** A write to a foreign entity is sent to its owner, who decides, applies it as its own write and answers with a receipt. Nothing is applied locally first.
- **One frame per peer per round.** A VM runs in rounds; at the end of one it sends the changes to each peer as a single frame, and takes the peer's answers as writes in a later round.
- **Transport independent.** The protocol is frames; a transport only carries them (HTTP long poll, WebSocket, MQTT, a pipe, shared memory).

## What is built

`graphlink-core` (its `LinkSubs`, `Replica` and `Link` were moved out of cliffvm; the rest of the crate grew here):

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

Not built yet: CBOR encoding of whole frames (the value coder itself is built; the control rings carry JSON), the store trait (what an engine gives the library), the C interface and the language bindings, a Windows backend for the named regions, the upgrade of a link from TCP to shared memory (the `upgrade` op and its link state are built; the programs do not act on it), a heartbeat for a program that hangs without ending, and a seat writing a node of another seat. `docs/frames.md` lists the decisions that are still the operator's.

## Crates

| Crate | What it is |
|---|---|
| `graphlink-core` | the protocol: frames, the counters, the link state machine, the transport trait, the subscription table |
| `graphlink-net` | a TCP transport: one JSON frame per line, non-blocking (`Tcp`, `Listener`); with the feature `tls` (rustls, rcgen): `Tcp::connect_tls`, `Listener::with_tls`, `tls::make_certs` |
| `graphlink-shm` | shared memory (Unix), no dependency: `Shm` is a transport (two rings in a named region, one per direction); `cells::Cells` and `cells::CellsReader` are the cell table (one slot per `(eid, prop)` key, a seqlock per slot, CBOR values, the owner writes and a reader maps it read-only) |
| `graphlink-cli` | the `graphlink` tool: a demo peer and a debugging aid (`serve --secret-file` checks tokens, `watch` and `write` take `--token-file` and `--ns`, `token mint` makes a token for a namespace) |
| `graphlink-auth` | HS256 tokens (JWT), std only: `mint`, `mint_with` (more claims, such as `ns`, the namespace the holder may own) and `verify` (HS256 only, `exp` and `sub` required, 30 s leeway, a secret of at least 32 bytes). The seat's door uses the same code |
| `graphlink-bridge` | `graphlink-bridge connect <addr> --peer <name> ...`: TCP outward, shared memory inward. To a seat it is a local peer that says the remote's namespace; the remote's values become cells, writes, reads, queries and subscriptions go to the remote, and a node outside the remote's namespace is dropped both ways. `connect` dials a remote, `listen` waits for one (a loopback address unless `--allow-remote`). A remote's subscription to a node of the seat's namespace goes to the seat, which answers by writing its cell table; the bridge reads those cells and sends the remote a `val` for each new one (`gone` for a node that is gone); a remote's write goes to the seat and the seat's receipt back. Tokens: `--token-file` is the token the seat minted for the peer (put in the hello to the seat), `--remote-token-file` the one a remote asks for (put in the hello to the remote, naming the seat's namespace), `--secret-file` makes the bridge check every remote's hello (the token must verify and name the remote's namespace; a refused remote is told `refused: auth` and a listening bridge goes on listening); `--allow-remote` needs `--secret-file`. `--seat-name <name>` is what the seat is called across the link (two seats are both `membrane`, so each bridge says its own name): the name goes in the hello to the remote, before the seat's ids going out (`Lamp:desk` is `<name>::Lamp:desk` on the wire), and the ids coming in must be under it (the prefix is taken off; a bare or foreign id is not the seat's). `--die-with-parent` ends the bridge when the process that started it is gone (the one named by the environment variable `GRAPHLINK_PARENT`, else the starting one): a seat that supervises its bridge sets it to its own pid. A bridge started again removes the cell table (`/gl-<name>-pc`) a killed one left. TLS: `listen --tls-dir <dir>` speaks TLS with a self-signed certificate made in `<dir>` at the first run (names: the address, `localhost` for a loopback one, `--tls-san`), `connect --tls-cert <file>` trusts exactly that certificate and checks it covers the name dialled (`--tls-name` to override); the tool takes the same (`serve --tls-dir`, `watch`/`write --tls-cert`). Not built: the `upgrade` from TCP to shared memory |
| `graphlink-claude` | `graphlink-claude serve --peer <name> ...`: Claude Code (the `claude` CLI in print mode) as a model backend of a seat, a peer over shared memory (the request, tool, turn, response cycle only; no loop). Written, tested against a fake `claude` only, not yet published: [docs/claude-peer.md](docs/claude-peer.md) |

## The `graphlink` tool

```
graphlink serve 127.0.0.1:7070 --ns kitchen --nodes nodes.json    # own the nodes of the file: {"kitchen::Lamp:desk": {"power": "off"}}
graphlink watch 127.0.0.1:7070 kitchen::Lamp:desk                 # print the node as one JSON line, now and at every change
graphlink watch 127.0.0.1:7070 --prefix kitchen::                 # every node under a prefix
graphlink write 127.0.0.1:7070 kitchen::Lamp:desk power '"on"'    # prints ok, or "refused: why" and exits 1
```

`serve` answers `sub`, `unsub`, `read`, `wr` and the paged `q`; a write bumps the node's revision and is pushed to every subscriber. Nodes live in memory and are not saved.

## Using it

```
cargo add graphlink-core            # the protocol; add graphlink-net or graphlink-shm for a transport, graphlink-auth for tokens
cargo install graphlink-cli          # the `graphlink` tool
cargo install graphlink-bridge       # the bridge between a seat and a remote
cargo install graphlink-claude       # Claude Code as a seat's model, a peer over shared memory (not yet released)
```

Or as a git dependency pinned to a commit: `graphlink-core = { git = "https://github.com/chedim/graphlink", rev = "<commit>" }`.

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
cargo test --workspace
```

License: MIT.

## Releases

The crates (`graphlink-core`, `-auth`, `-net`, `-shm`, `-bridge`, `-cli`, `-claude`) share one version, `[workspace.package]` in `Cargo.toml` (and the four sibling `version`s of `[workspace.dependencies]`, which move with it). A release is: bump those, merge into `main`, then merge `main` into the branch `release`. `.github/workflows/publish.yml` then checks that all the crates are at one version, runs the tests, publishes all seven to crates.io in dependency order and makes the GitHub release `v<version>` (a push to `release` whose version is already tagged publishes nothing). It needs the repository secret `CARGO_REGISTRY_TOKEN`. On every other push and on pull requests the same workflow runs `cargo publish --workspace --dry-run`, which packages and builds each crate as crates.io would receive it and uploads nothing. Publishing to crates.io cannot be undone (a version can be yanked, never deleted): creating the branch `release` the first time publishes the version `main` has then.
