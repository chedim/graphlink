# Shared memory: a table of cells holding CBOR values (DESIGN, not built)

Status: **design. Nothing here is built.** Decided by the operator: values in shared memory are CBOR items, in a table with one slot per cell. Every size, limit and number below is a DECISION for the operator and is marked as one; none is set.

## 1. Why this and not frames

A link carries frames: messages, encoded, copied. Two peers on one machine can do better. The thing a subscriber wants is the latest value of the cells it reads, not the history of changes, so the owner can keep the latest values in a region both processes map, and a reader looks at the bytes where they were written: no encoding, no copy, no queue to fill. A slow reader costs the owner nothing (the "state, not events" rule of babyseat `docs/graph-link.md`, section 4).

## 2. The region

| Part | What it holds |
|---|---|
| **header** | a layout id and version (both sides must know it; checked first), the owner's epoch, the sizes of the other parts |
| **cell table** | fixed-size slots, one per `(eid, prop)` cell: a sequence number, the key (an offset and length of its text in the arena), the cell's revision `r`, the offset and length of its value in the arena, a state (`live`, `gone`, `refused`) |
| **arena** | the key texts and the value bytes: each value is ONE CBOR item (section 3) |
| **control ring** | a small ring of the rare messages: `hello`, `sub`, `unsub`, `wr`, `res`, `bye`, `q`, `page`, `resync`, `pause`, as the frames of `docs/frames.md`, so the control side needs no new encoding |

- A cell is the key a subscription already uses (`LinkSubs` keys are `(eid, prop)`), so subscribing is "read this slot", and a change is a revision going up.
- The reader finds a slot by the key; a lookup structure (a hash index in the table) is part of the layout. Sizes (slots, arena, ring) are decisions.

## 3. The values: CBOR, the JSON-compatible subset

- A value is ONE definite-length CBOR item (RFC 8949). Allowed: UTF-8 text strings, integers, floats, `true`, `false`, `null`, arrays, and maps with text-string keys. Not allowed: byte strings, tags, NaN, infinities, indefinite lengths, non-text keys. This is exactly what JSON can hold, so a value round-trips to JSON text without loss (the wire frames and cliffvm's journal stay JSON).
- Integers are written in the shortest form. Floats are always written as 64-bit (a decision: it is a little larger than the shortest form, and it keeps equal numbers equal bytes).
- **Key order is part of the bytes.** A map's keys are written in the order the map holds them (decided by the operator); `{a,b}` and `{b,a}` are different bytes. The VM's own equality (`py_eq` in cliffvm's `expr.rs`) ignores key order, so byte equality is STRICTER than the VM's: a pure reorder is a change in the bytes and not in the VM's eyes. Early cutoff on bytes can therefore only do extra work, never miss a change; an owner that wants the VM's exact cutoff compares decoded values first. Nothing in c1f can list a dict's keys, so no logic depends on the order, only what is printed or journalled.
- CBOR is self-delimiting, so a reader can walk one item; it never trusts a length: every offset and length is checked against the region, and a malformed item is an error for that cell, not a crash.
- The encoder and decoder are built: `graphlink_core::cbor` (`encode`, and `decode` with a nesting limit the caller gives), hand written with no dependency, tested against the example vectors of RFC 8949 and against the refusals above. `decode` reads a float of any width and always keeps the order the keys came in.

## 4. One consistent read (a seqlock per slot)

The owner writes a slot like this: make its sequence number odd, write the cell, make it even (one higher). A reader reads the number, reads the cell, reads the number again, and accepts the cell only if both numbers are equal and even; otherwise it retries. The owner never waits for a reader.
- **An owner that dies mid-write** leaves an odd number that never settles. The reader needs a rule for it (how long to retry, then treat the cell as `gone` or the link as closed); the time is a decision. The header's epoch tells a reader the owner restarted.
- **An arena that fills:** values are appended and old ones are reclaimed by the owner when no slot points at them; the reader's check of "the slot did not change" covers a value that moved. The reclaim rule and the full-arena behaviour are decisions.

## 5. Safety

- The region is shared with another process, so every byte read is untrusted: bounds on every offset and length, a limit on the nesting of a value, and no allocation sized by a field in the region.
- A reader never writes into the owner's parts of the region. A writer's region is its own: a peer owns only its namespace (docs/graph-link.md, section 13), and the OS decides who may map the region at all. Same machine only.
- A peer that writes a cell outside its namespace is refused by the owner of the table, as a forwarded write is.

## 6. What stays outside

CBOR for MQTT, serial and other devices (the same value bytes, copied into a CBOR frame, no transcoding). Platform code for mapping the region (POSIX shared memory on macOS and Linux; Windows later) is a new native piece with `unsafe`; it goes in its own crate (`graphlink-shm`) so nobody pulls it in by accident.

## 7. Decisions for the operator (none is taken)

1. **Key order: decided** (insertion order, see section 3).
2. **Floats always 64-bit,** or the shortest lossless form.
3. **Sizes:** slots, arena, ring; the maximum value and key size; the nesting limit.
4. **The stuck-writer time** and the arena reclaim rule (section 4).
5. **Where the layout id and version are agreed** (the control ring's `hello` after the header check, as in docs/frames.md).
