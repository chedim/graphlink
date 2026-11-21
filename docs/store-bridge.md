# graphlink-store-bridge: a read-only window of a typed store onto a seat

This page says what the code does now. **Not verified: nothing has run on a real seat**; every test plays the seat with a fake (`graphlink-store-bridge/tests/common`), and the store is the one in the other project's README.

## Whose store

The store is made with the crate **`graphlink` on crates.io** (github.com/Rylan12/graphlink, MIT, 1.0.0): "a Rails inspired relational in-memory graph database library for Rust". It is **not this project**: this project's crates are `graphlink-core`, `-net`, `-shm`, `-auth`, `-bridge`, `-cli`, `-claude`, `-anthropic`, `-openai` and this one. The two share a name and nothing else (no protocol, no shared memory, no code).

What that crate gives, read from its 1.0.0 source:

- `define_schema!` turns a schema (models, `has_many`, `has_many ... through`, `belongs_to (on_delete = cascade|restrict)`, `index unique <field>`) into a typed store struct at **compile time**: a `Collection` per model (`libraries`, `patrons`, `checkouts`), a generational id type per model (`LibraryId`, ...: a `slotmap` key), `add_<model>(item) -> Id`, `<model>(id) -> Option<Entity>`, `get_<model>_by_<field>`, `update_<model>(id, closure)` and `remove_<model>(id)`.
- A model is an ordinary struct of the owner's; a relation is a `HasMany<Id>` (a list of ids) or a `BelongsTo<Id>` (one).
- **There is no way to list the rows of a model.** `Collection` has `new`, `insert`, `remove`, `get`, `get_mut`: no iterator, no length, no keys. It is in memory only: no persistence, no events when a row changes.

## What this crate is

A read-only window. A program that owns a store gives the bridge a `Mirror` for it, and a seat that lists the peer sees:

- each row as an entity `<ns>::<Model>:<id>` (`ns` is the namespace the peer owns, `id` the row's generational key as one `u64`, `mirror::ffi`),
- each field as a cell of it (`<ns>::Library:4294967297` has `name`), a plain value as it is, a `belongs_to` as the id of the related entity (`<ns>::Library:4294967297`), a `has_many` as a list of such ids,
- the node's own cell (the entity's id and an empty prop), which the seat's driver reads first.

The peer polls the store on its own tick (`--nap-ms`, required: the library has no events to wait for). Each turn it asks the mirror for the rows of every model, writes the cells whose value is not the one already shown (an unchanged cell keeps its revision), and retires the entity of a row the store no longer has (its cells become `Gone`, and their slots can be reused once the seat has acknowledged). A value larger than a cell is carried by `graphlink-shm`'s spill (0.1.4) and the reader gets it whole.

**Nothing is written by the seat.** The peer's token names no grants, and a write that reaches the peer anyway (`wr`) is answered at once with a refusal that says the entity is read-only. The store is not changed.

## The `Mirror`

```rust
pub trait Mirror {
    fn models(&self) -> Vec<String>;                 // the models, in the order they are published
    fn rows(&self, model: &str) -> Vec<Row>;         // the rows of a model now: (id, [(field, Field)])
}
pub enum Field { Value(serde_json::Value), Ref { model, id }, Refs { model, ids } }
```

The schema is the adapter's, not the bridge's: which models there are, which fields and how a field becomes a value is written once by whoever owns the schema, because the macro does not expose it at run time. `mirror::Registry` keeps the ids a store handed out (the library cannot list them) and the adapter asks the store for each: a row the store no longer has simply is not returned. `examples/library/store.rs` is the adapter for the README's Library, Patron and Checkout.

`bridge::Bridge<M>` is the turn (sans-I/O, like the other peers); `serve::run` is the loop (attach the regions the seat made, take the token from `--token-file`, turn until the seat's end of the rings is gone or, with `--die-with-parent`, the process that started it is). Every number of the command line is the owner's and has no default: `--peer --ns --slots --key-cap --val-cap --epoch --spins --depth --nap-ms` (equal to what the seat declared in its `peers` fact).

`cargo run -p graphlink-store-bridge --example library -- serve --peer <name> ...` serves the README store; a command on its stdin changes it while it runs (`rename-library <name>`, `add-patron <email>`, `remove-checkout <n>`).

The crate depends on `graphlink = "=1.0.0"` by an exact version: it uses the hidden re-export `graphlink::__private` (the `slotmap` key trait, to turn an id into a number) and the shape of the store the macro generates, neither of which a newer release has to keep.

`src/peer.rs` is `graphlink-claude`'s peer copied down to what this one uses, plus the refusal of a write and a publish that skips an unchanged value. It is now in four crates' lineage (membrama, claude, anthropic and openai copy it too) and should move into a shared crate; this change does not refactor the others.

## What it is not

- **Not a replacement for SQLite or any store.** It mirrors rows that live in another program's memory. The seat's own data stays where it is.
- **Not generic.** One `Mirror` per schema. The seat cannot reshape the store: it cannot add a model, a field or a relation (the schema is fixed when that program is compiled), and it cannot write a row.
- **Not live.** The store is polled; a change shows within one nap, not at once.
- **Not a way to enumerate a store by itself.** The adapter has to know which ids exist.

## Not verified

- A real seat: that its driver subscribes to these entities and reads the cells as the fake does, and that the refusal of a write is the answer a seat expects.
- The library's behaviour beyond what the tests drive (add, change a field, remove a row, a `has_many` list that changes). Its `update_*`, secondary indexes and `has_many ... through` are not exercised here.
- A store with a large number of rows (each turn reads every row of every model; the cost of that poll is not measured), and a schema whose fields are not plain values.
- Names: this crate and the crates.io crate `graphlink` are different projects with the same name.
