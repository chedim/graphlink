# graphlink-store-bridge

A read-only window of a typed in-memory store, made with the crates.io crate `graphlink` (Rylan12's, **not** this project), onto a GraphLink seat over shared memory. The seat sees each row of the store as an
entity `<ns>::<Model>:<id>` with one cell per field; a relation is the id of the related entity. The seat cannot write to it. See `docs/store-bridge.md` in the repository.
