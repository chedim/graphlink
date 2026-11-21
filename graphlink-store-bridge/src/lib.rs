//! graphlink-store-bridge: a read-only window of a store made with the crates.io crate `graphlink` (Rylan12's in-memory typed store; NOT this project) onto a GraphLink seat over shared memory (docs/store-bridge.md).
pub mod bridge;
pub mod cli;
pub mod mirror;
pub mod peer;
pub mod serve;
