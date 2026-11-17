# Frames and version negotiation (spec for graphlink-core; the first slice)

The unit of ordering is the **frame**, a JSON object (the CBOR encoding for devices is a later slice; it carries the same map):

```
{"v": 1, "n": 7, "ack": 4, "ops": [ ... ]}
```

- `v`: the protocol version this frame is written in.
- `n`: the sender's frame counter (this slice only carries it; ordering and duplicate rules come with the next slice).
- `ack`: the highest `n` the sender has taken from the peer.
- `ops`: the operations, in order.

## Ops of this slice

| `op` | fields | meaning |
|---|---|---|
| `hello` | `ns`, `build`, `epoch`, `have`, `support` | open: who I am, my boot epoch, the revision I hold per subscribed id (`have`, empty on first join), and every protocol version I can speak (`support`, a non-empty list) |
| `failed` | `support`, `why` | the answer to a `hello` with no version in common; `support` is what the sender speaks; the sender closes after it |
| `sub` | `id` or `prefix`, `props?`, `limit?` | subscribe; a prefix is bounded by `limit` and REFUSED, not truncated, when more match |
| `unsub` | `id` or `prefix` | drop |
| `val` | `id`, `r`, then `props` or `gone` or `refused` | the current value of a subscribed node; `r` its revision |
| `read` | `id`, `props`, `rid` | read once, no subscription; answered by a `val` carrying `rid` |
| `wr` | `wid`, `id`, `prop`, `value` | forwarded write request |
| `res` | `wid`, then `ok` or `refused` | the answer to a `wr` |
| `bye` | `why` | close |

`q` / `page`, `resync` and `pause` are the next slice.

## Rules

- **Unknown is ignored, never an error.** A receiver ignores unknown fields of a known op, and keeps an op it does not know as `Op::Unknown` (the original JSON) so it can pass it on or drop it. A device that does not implement an op answers `refused: unsupported` when the op expects an answer.
- **A frame that is not a JSON object with an `ops` array, or an op that is not an object with an `op` string, or a known op with a required field missing or of the wrong type, is a decode error** (the receiver closes the link with `bye`).
- **Version negotiation.** A `hello` lists every version its sender speaks in `support`. The receiver picks the HIGHEST version both speak and writes every later frame in that version. If there is none, it answers `failed` with its own `support` and closes; the sender may retry with a version from that list. The `hello` itself is written in the highest version its sender speaks, so a peer that speaks only older versions can still decode it (a `hello` is decodable by every version: it is the one op whose shape never changes).
- This slice has one version, `1`.

## Ordering (`n` and `ack`)

- Each side numbers the frames it sends 1, 2, 3, ... (`n`) and puts the highest `n` it has taken from the peer in `ack` (0 before any). Frames of one link are processed in `n` order.
- A frame whose `n` is not above the highest taken is a **duplicate** (a retried request) and is dropped. A frame whose `n` skips one is a **gap**: it is not taken, and the receiver is told which `n` was expected. What a gap means (ask again, resync, close) is the engine's decision; the counters only report it.
- `ack` only moves forward and never names a frame that was not sent; an ack that does either is ignored by the sender (`Outbox::acked` says so).
- How many frames may be unacknowledged before the sender stops (the window) is not set here: it is a number for the operator.

## A link and a transport

- **`Link`** is the state machine of one link to one peer, with no I/O. `open()` returns our `hello` (frame 1, written in the highest version we speak). `receive(frame)` takes a frame from the peer and returns events for the engine, and maybe a frame to send straight back. `send(ops)` returns the next frame once the link is open (our hello sent and the peer's taken); before that it is an error.
- **Events:** `Opened` (the peer's hello and the agreed version), `Op` (an op from the peer, in order, once), `Duplicate`, `Gap`, `Failed` (no version in common, or the peer said so), `Closed` (the peer said `bye`; its `Op(Bye)` is delivered first), `Refused` (an op before the peer's hello, or a frame in a version that was not negotiated). A frame that is a duplicate or a gap delivers no ops. A closed link ignores everything.
- **`Transport`** is the one edge that touches the outside: `send(&Frame)` queues a frame, `recv()` returns the next frame that arrived or `None`, and neither waits. The library starts no thread for it; the engine polls it at the end of a round. `pipe()` gives two connected in-memory ends (frames cross as JSON) for tests.

## Decisions (the operator's; nothing here sets them)

1. The version number of the first protocol is `1`.
2. Counting starts at 1 and goes up by one per frame.
3. `res` does not carry the node's revision `r` yet (babyseat `docs/waves.md`, section 6); it is added only when the operator agrees.
