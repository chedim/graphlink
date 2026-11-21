//! The turn of the bridge: the store's rows into the peer's cells. A poll of the store on the peer's own tick (no events come from the library, which has none): each row is an entity `<ns>::<Model>:<id>`, each field a cell
//! (a relation the id of the related entity), only what changed is written, a row that is not there any more is retired. Nothing is read from the seat and nothing is written by it.
use crate::mirror::{entity_id, Field, Mirror};
use crate::peer::Peer;
use serde_json::{json, Value as J};
use std::collections::HashSet;

pub struct Bridge<M: Mirror> {
    pub peer: Peer,
    pub ns: String,
    /// the store's mirror (public so that the program can change the store between two turns)
    pub mirror: M,
    warned: HashSet<String>,
}

fn value_of(ns: &str, f: &Field) -> J {
    match f {
        Field::Value(v) => v.clone(),
        Field::Ref { model, id } => json!(entity_id(ns, model, *id)),
        Field::Refs { model, ids } => J::Array(ids.iter().map(|i| json!(entity_id(ns, model, *i))).collect()),
    }
}

impl<M: Mirror> Bridge<M> {
    pub fn new(peer: Peer, ns: &str, mirror: M) -> Bridge<M> { Bridge { peer, ns: ns.to_string(), mirror, warned: HashSet::new() } }

    /// One turn: take what the seat sent (a write is refused), and once the link is open make the cells say what the store says.
    pub fn tick(&mut self) -> Result<(), String> {
        let seen = self.peer.before();
        self.peer.poll()?;
        if self.peer.is_open() { self.sync(); }
        if let Err(e) = self.peer.after(seen) { self.warn("reclaim", &format!("reclaiming slots: {e}")); }
        Ok(())
    }

    /// A problem is said once on stderr (the table full, say): the turn goes on and tries again at the next one, as the seat's own driver does for a cell that does not fit yet.
    fn warn(&mut self, kind: &str, what: &str) { if self.warned.insert(format!("{kind}:{what}")) { eprintln!("graphlink-store-bridge: {what}"); } }

    fn sync(&mut self) {
        let mut live: HashSet<String> = HashSet::new();
        for model in self.mirror.models() {
            for (id, fields) in self.mirror.rows(&model) {
                let eid = entity_id(&self.ns, &model, id);
                live.insert(eid.clone());
                for (name, f) in &fields {
                    if let Err(e) = self.peer.publish(&eid, name, &value_of(&self.ns, f)) { self.warn("publish", &format!("cannot publish {eid}.{name} yet: {e}")); }
                }
            }
        }
        for gone in self.peer.nodes().into_iter().filter(|n| !live.contains(n)) {
            if let Err(e) = self.peer.retire(&gone) { self.warn("retire", &format!("cannot retire {gone}: {e}")); }
        }
    }
}
