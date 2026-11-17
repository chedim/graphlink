# graphlink-shm

Shared memory for [GraphLink](https://github.com/chedim/graphlink) peers on one machine (Unix; no dependency besides `graphlink-core` and `serde_json`).

- **`Shm`** is a `Transport`: two single-producer rings in a named region, one per direction, carrying length-prefixed JSON frames with wrap-around. A sender keeps a pending queue (`flush`, `pending_bytes`), a closed flag says when an end is gone, and a corrupt ring is an error, not a crash.
- **`cells::Cells` and `cells::CellsReader`** are a table of values: one slot per `(id, prop)` key, a seqlock per slot, CBOR values. Only the owner writes; a reader maps the region read-only. Capacities (slots, key and value sizes) and the reader's spin and depth limits are numbers the caller gives: there are no defaults.
- **Regions are named, not paths** (`shm_open`): at most 31 bytes on macOS, so a name is short and unique. `remove_region` clears a name a dead creator left behind.

```rust
use graphlink_shm::Shm;
use graphlink_core::transport::Transport;

// one program makes the region (and is dropped last), the other attaches by name
let mut a = Shm::create("/gl-demo-ctl", 4096).unwrap();
let mut b = Shm::attach("/gl-demo-ctl").unwrap();
// a.send(&frame) ... b.recv()
```

The layout is described in [docs/shared-memory.md](https://github.com/chedim/graphlink/blob/main/docs/shared-memory.md). MIT licensed.
