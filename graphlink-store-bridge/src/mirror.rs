//! What a store tells the bridge. The other project's schema is a compile-time macro, so the bridge cannot look at a store and find its models or fields: the owner of the schema writes a `Mirror` for it (the model names, and
//! for a model its rows), once. The library has no way to list the rows of a model either (its `Collection` has `insert`, `remove`, `get` and `get_mut`, no iterator), so an adapter keeps the ids the store handed out
//! (`Registry`) and asks the store for each one: a row the store no longer has is a row that is gone.
use graphlink::__private::Key;
use serde_json::Value as J;

/// A field of a row: a plain value, or a relation. A relation is published as the id of the related entity (`<ns>::<Model>:<id>`), a list of them for a has-many.
#[derive(Clone, Debug, PartialEq)]
pub enum Field {
    Value(J),
    Ref { model: String, id: u64 },
    Refs { model: String, ids: Vec<u64> },
}

/// A row: its id (the number `ffi` makes of the store's id) and its fields, in the order they are published.
pub type Row = (u64, Vec<(String, Field)>);

pub trait Mirror {
    /// The models of the store, in the order they are published.
    fn models(&self) -> Vec<String>;
    /// The rows of a model that are in the store now.
    fn rows(&self, model: &str) -> Vec<Row>;
}

/// The entity id of a row: `<ns>::<Model>:<id>`.
pub fn entity_id(ns: &str, model: &str, id: u64) -> String { format!("{ns}::{model}:{id}") }

/// The number of a store id: its generational key as the library's own `slotmap` gives it (index and version in one `u64`), so a slot that is reused is another row.
pub fn ffi<K: Key>(id: K) -> u64 { id.data().as_ffi() }

/// The ids a store handed out, for a model. `track` when the store adds a row; a row that is removed stays in the registry until `forget` and is simply not found by the store, so the adapter may leave it.
pub struct Registry<K: Key> { ids: Vec<K> }
impl<K: Key> Registry<K> {
    pub fn new() -> Registry<K> { Registry { ids: Vec::new() } }
    pub fn track(&mut self, id: K) { self.ids.push(id); }
    pub fn forget(&mut self, id: K) { self.ids.retain(|&x| x != id); }
    pub fn ids(&self) -> &[K] { &self.ids }
}
impl<K: Key> Default for Registry<K> { fn default() -> Self { Registry::new() } }
