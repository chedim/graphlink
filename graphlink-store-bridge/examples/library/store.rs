//! The store of the other project's README (crates.io `graphlink`, Rylan12): Library, Patron, Checkout, with a `Mirror` for it. This is the adapter a store needs: the schema is a compile-time macro, so which models
//! there are, which fields they have and how a field becomes a value is written here, once, by whoever owns the schema. The library cannot list the rows of a model (its `Collection` has no iterator), so the adapter keeps
//! the ids the store handed out (`Registry`) and asks the store for each: a row the store no longer has is a row that is gone.
use graphlink::{define_schema, BelongsTo, HasMany};
use graphlink_store_bridge::mirror::{ffi, Field, Mirror, Registry, Row};
use serde_json::json;

define_schema! {
    store: Store;

    model Library {
        has_many Checkout;
        has_many Patron through Checkout;
    }

    model Patron {
        index unique email;
        has_many Checkout;
    }

    model Checkout {
        belongs_to Library (on_delete = cascade);
        belongs_to Patron (on_delete = restrict);
    }
}

pub struct Library {
    pub name: String,
    pub checkouts: HasMany<CheckoutId>,
}
pub struct Patron {
    pub email: String,
    pub checkouts: HasMany<CheckoutId>,
}
pub struct Checkout {
    pub title: String,
    pub library: BelongsTo<LibraryId>,
    pub patron: BelongsTo<PatronId>,
}

/// The store and the ids it handed out.
pub struct Lib {
    pub store: Store,
    pub libraries: Registry<LibraryId>,
    pub patrons: Registry<PatronId>,
    pub checkouts: Registry<CheckoutId>,
}

impl Lib {
    pub fn empty() -> Lib { Lib { store: Store::new(), libraries: Registry::new(), patrons: Registry::new(), checkouts: Registry::new() } }

    pub fn add_library(&mut self, name: &str) -> LibraryId {
        let id = self.store.add_library(Library { name: name.into(), checkouts: HasMany::new() });
        self.libraries.track(id); id
    }
    pub fn add_patron(&mut self, email: &str) -> PatronId {
        let id = self.store.add_patron(Patron { email: email.into(), checkouts: HasMany::new() });
        self.patrons.track(id); id
    }
    pub fn add_checkout(&mut self, title: &str, library: LibraryId, patron: PatronId) -> CheckoutId {
        let id = self.store.add_checkout(Checkout { title: title.into(), library: BelongsTo::new(library), patron: BelongsTo::new(patron) });
        self.checkouts.track(id); id
    }

    /// A few rows, as the other project's README builds them.
    pub fn seeded() -> (Lib, LibraryId, Vec<PatronId>, Vec<CheckoutId>) {
        let mut l = Lib::empty();
        let lib = l.add_library("City Library");
        let alice = l.add_patron("alice@example.com");
        let bob = l.add_patron("bob@example.com");
        let c1 = l.add_checkout("Rust Atomics", lib, alice);
        let c2 = l.add_checkout("Zero To Production", lib, bob);
        (l, lib, vec![alice, bob], vec![c1, c2])
    }
}

impl Mirror for Lib {
    fn models(&self) -> Vec<String> { vec!["Library".into(), "Patron".into(), "Checkout".into()] }

    fn rows(&self, model: &str) -> Vec<Row> {
        match model {
            "Library" => self.libraries.ids().iter().filter_map(|&id| self.store.libraries.get(id).map(|l| (ffi(id), vec![
                ("name".into(), Field::Value(json!(l.name))),
                ("checkouts".into(), Field::Refs { model: "Checkout".into(), ids: l.checkouts.iter().map(|&c| ffi(c)).collect() }),
            ]))).collect(),
            "Patron" => self.patrons.ids().iter().filter_map(|&id| self.store.patrons.get(id).map(|p| (ffi(id), vec![
                ("email".into(), Field::Value(json!(p.email))),
                ("checkouts".into(), Field::Refs { model: "Checkout".into(), ids: p.checkouts.iter().map(|&c| ffi(c)).collect() }),
            ]))).collect(),
            "Checkout" => self.checkouts.ids().iter().filter_map(|&id| self.store.checkouts.get(id).map(|c| (ffi(id), vec![
                ("title".into(), Field::Value(json!(c.title))),
                ("library".into(), Field::Ref { model: "Library".into(), id: ffi(c.library.id()) }),
                ("patron".into(), Field::Ref { model: "Patron".into(), id: ffi(c.patron.id()) }),
            ]))).collect(),
            _ => vec![],
        }
    }
}
