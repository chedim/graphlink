# Shared memory: control rings and a table of cells holding CBOR values (built in `graphlink-shm`; the sections below are the design it follows)

Status: **the control rings and the cell table are built** (below); what is marked "not built" is not. Decided by the operator: values in shared memory are CBOR items, in a table with one slot per cell. Every size, limit and number is the caller's: the crate has no defaults. The numbered sections after the "Built" list are the original design; where they differ from the list, the list is what the code does.

**Built (`graphlink-shm`):** the control rings (`Shm`, a `Transport`) and the cell table (`cells`).
- The control side: the region is a NAMED region both processes map (`shm_open`: RAM only on Linux, nothing on disk): a 128-byte header (magic, layout version 1, ring size, a closed flag per end), then two single-producer rings, one per direction. A frame is a 4-byte length and its JSON bytes, wrapping around the ring's end; `recv` never waits; a sender whose ring is full queues the rest locally and writes it as the reader makes room AND the sender calls `flush`, `send` or `recv` (`pending_bytes` says how much still waits: a sender that stops calling leaves the rest queued, so one that is finished keeps flushing until it is zero; dropping an end makes one last try); every length read from the region is checked, and a corrupt ring closes the link with an error. The creator removes the name when it goes; a name a dead creator left behind is removed with `remove_region(name)` (for a name the caller owns).
- The cell table: a named region the OWNER creates and is the only writer of; a reader maps it READ-ONLY, so it cannot write into it. A slot is a sequence number (a seqlock), a state, the key, a revision and ONE CBOR value (`graphlink_core::cbor`). Capacities are FIXED when the table is made (slots, bytes of a key, bytes of an encoded value): there is no arena. A value that does not fit ONE slot is stored in more slots and read back whole (section 4c); the only error left is a table that has no slots for it. Keys are never removed (a node that goes away is `Gone`), so a lookup stops at the first empty slot. A reader gives `spins` (how many times to try a slot being written before it reports "never settled", the stuck-writer rule is the caller's number) and `depth` (how deep a value may nest). A bad slot is an error, never a panic. `epoch` tells a reader the owner restarted.
- A region is identified by a NAME: one slash and at most 31 bytes (macOS's limit; the shortest of the systems), never a path. A Windows backend would give the same name to a named file mapping; it is not built.
- Not built: CBOR frames on the control rings, a heartbeat for an end that dies without dropping (the closed flag is set only on a clean drop), and Windows.
- This replaces the arena of section 2 with fixed-capacity slots: simpler, a cap on what one slot holds (the size of a value is not capped by it: section 4c; the operator can ask for an arena later).

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

## 4b. Reclaiming slots (the owner side and the ack cell are built in `graphlink-shm`; the readers and the owners that use them are not)

**The gap.** A slot is never reused: a key stays in its slot for the life of the table, and a node that goes away becomes `Gone` and keeps the slot (section 2: lookups stop at the first empty slot, so removing a key would hide the keys placed after it). A peer that creates short-lived nodes, one per request for example, fills its table and then every write fails with "the table is full". Nodes that live as long as the table (a lamp, a sensor) never meet this. Found by membrama, which writes a chat request per model turn.

**The rule: a `Gone` slot may be reused once the reader has seen the removal.** A reader learns that a node is gone from its `Gone` cell, so the owner must not take the slot back before the reader has read it. The reader cannot write into the owner's table (its mapping is read-only on purpose), so it says what it has seen in a cell of its OWN table, which the owner already maps read-only. Each table has one reader, the peer at the other end of the link.

- **Numbering.** Each removal is numbered: a counter in the header (`gone_seq`) and, in the `Gone` cell, its own number (kept in the cell's value, which readers still see as null). A cell that is already `Gone` keeps its number. The slot is written first and the counter raised after, so a reader that has read the counter and then scans sees every removal up to it.
- **The reader's turn:** read `gone_seq` of the owner's table, scan the table, process what it found (including the `Gone` cells), then say `ack_seen(g)` with the number it read first. The acknowledgement is one cell in the reader's own table (a reserved key starting with U+0002, rewritten in place, so it never takes another slot, and left out of scans).
- **The owner's turn:** read the reader's acknowledgement (`peer_ack` on the reader's table; 0 when the reader has said nothing) and call `reuse_gone_up_to`. A new key may then take a `Gone` slot whose number is at most that. The probe chain is walked first, so a key that sits after such a slot is found and not duplicated, and a `Live` or `Refused` cell is never taken.
- **The helper.** Every end of a link is both a reader and an owner, and the order of the steps matters, so `graphlink_shm::cells::Reclaim` does them: `Reclaim::before(&theirs)` before the pass that reads the other table, and `after(seen, &theirs, &mut mine, spins, depth)` after it (it acknowledges when the count moved, and passes the other end's acknowledgement to `reuse_gone_up_to`). An acknowledgement that cannot be written (this table has no slot left for the ack cell) is an error, nothing is counted as acknowledged, and the next turn tries again. A consumer that keeps cells it could not write (the bridge does) retries them after `after`.
- **A reader that never acknowledges** (an old one, or one that has stalled) changes nothing: the table fills as it does today. There is no wrong answer in that case, only no reclaiming.
- **No change to the layout version.** The counter uses a header word that was unused, and an old reader sees the same cells.
- **More than one reader of a table** is not a case today; it would use the smallest acknowledgement.

**Not built.** A reader that does not scan the whole table (it reads only some keys) cannot acknowledge a number that covers cells it never looked at; it would acknowledge per cell, a list of slot and number pairs in the ack cell (limited by the value size). Our readers scan everything, so the single number is enough. A rebuild of the table in place under a table-level seqlock, for an owner whose reader has stalled, was considered and is not built: it would need every reader to learn a new rule and a layout bump.

**Work this needs.** `graphlink-shm` (done: the numbering, the reuse, the ack cell, tests); the readers use `Reclaim` (`graphlink-bridge` is done; cliffvm's peer driver and membrama's peer are not) and the owners call `reuse_gone_up_to` (cliffvm for the seat's table, membrama for its own); then releases in that order (graphlink, cliffvm, babyseat, membrama).

## 4c. A value larger than a slot (built in `graphlink-shm`, `Cells::put` and every read)

**The rule.** A consumer never sees the size of a slot. `put` of a value whose CBOR is longer than the table's value capacity does not fail: the table stores it in more slots, and `get` and `scan` of any reader return the whole value (`Cell.value`), with the revision of the cell. A value that fits is stored exactly as before, one slot, the same bytes; the layout version does not change.

- **Storage.** The value's JSON text (the keys in the order the value has them, floats that round-trip) is cut on character boundaries into pieces, each the most one slot holds as a CBOR text string (derived from the value capacity: its size less the string's header, 1 to 5 bytes). A piece is a cell whose key starts with U+0002 (scans leave it out, as they do the ack cell) and holds a generation number, so the pieces of two writes never mix: `\u{2}s<generation>\u{1}<piece number>`. The cell of the value's own key, the HEAD, is written `Refused` with a text that says the generation, the bytes of JSON and the number of pieces. A reader that does not know spills sees a refusal there, as it did for a value that did not fit; one that does reads the pieces and checks the head again.
- **A consistent read.** For a new value the owner writes the pieces first, then the head, then gives back the pieces of the old value. A reader that finds a piece gone looks at the head again: it has a new generation (the reader follows it) or it is the same (the table is corrupt, an error, never a half value). Nothing is allocated by a number the region says: the text grows by the pieces that are there, and the number of pieces cannot exceed the number of slots.
- **Reclaim.** The pieces are ordinary cells: when a value is replaced by a smaller one, replaced by another big one, or goes `Gone`, every piece becomes `Gone` (numbered, counted in `gone_seq`) and is reused once the reader has acknowledged it (section 4b). Nothing leaks: a value rewritten forever does not fill the table.
- **The limit that is left.** The pieces and the head must find free slots (the empty ones and the acknowledged `Gone` ones). They are counted before the first is written, so a value the table cannot hold takes no slot and is the existing "the table is full" error; the cell keeps the value it had. The key must be able to name a piece (a key capacity below the length of `\u{2}s<generation>\u{1}<number>` cannot spill and keeps the old capacity error). There is no other bound: no maximum total size beyond the slots.
- **Cost.** A big value costs `pieces + 1` slots and a read of it walks its pieces by key; a write of one counts the free slots once. Values that fit pay nothing.

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
