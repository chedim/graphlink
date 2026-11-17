# graphlink

The graph link's protocol library: the part of cliffvm's graph link (docs/graph-link.md in the babyseat repo) that needs no VM and no network.

## graphlink-core (0.0.1)

What is here today, moved unchanged from cliffvm's `native/src/link.rs`:

- `LinkSubs`: the subscription table. Which foreign `(eid, prop)` a VM reads (subscriptions by dependency), the notices a host takes at the end of a round (`drain` gives `subs` and `unsubs`, `due` gives the next decision time), the grace before an unread key is dropped (`LINK_GRACE_S`).
- `Replica`, `ReplicaState`: a foreign entity's local copy and the owner's verdict (`Live`, `Gone`, `Refused`).
- `is_special`: the attributes the VM provides, never replicated.

Everything else in the design (frames, encodings, the store and transport traits, shared memory, the C interface) is not built; it is design in the babyseat repo and each piece lands here as its own change with its test.

```
cargo test
```
