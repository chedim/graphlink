//! The cell table (docs/shared-memory.md, sections 2 to 4): one slot per `(eid, prop)` key in a file both processes map. The OWNER creates it and is the only writer; a READER maps it
//! read-only and so cannot write into it at all. A slot is written under a sequence number (a seqlock): the owner makes it odd, writes, makes it even; a reader reads the number,
//! the slot, the number again and keeps the slot only when both numbers are equal and even. The owner never waits for a reader. A value is ONE CBOR item (the JSON subset of
//! `graphlink_core::cbor`). Slots have a fixed capacity for the key and the value, given when the table is made, so there is no allocator and nothing to reclaim: a value that does
//! not fit is an error the owner sees, and the table has a fixed number of slots. Keys are never removed (a node that goes away becomes `Gone`), so a lookup stops at the first empty slot.
//! A value larger than a slot is the owner's problem and not the caller's: it is stored as its JSON text cut into pieces, in more slots, and the reader gets it back whole (docs/shared-memory.md,
//! section 4c). A value that fits a slot is stored exactly as before.
//! Every byte a reader takes from the region is untrusted: lengths are checked against the capacities, and a bad slot is an error, never a panic or an out-of-bounds read.
use std::sync::atomic::{fence, AtomicU32, AtomicU64, Ordering};

use graphlink_core::cbor;
use serde_json::Value as J;

use crate::{map, map_read_only, open_region, unlink_region, unmap};

const MAGIC: u64 = u64::from_le_bytes(*b"GLNKCEL1");
const LAYOUT: u32 = 1;
const HEADER: usize = 128;
const H_MAGIC: usize = 0;
const H_LAYOUT: usize = 8;
const H_SLOTS: usize = 16;
const H_KEY_CAP: usize = 24;
const H_VAL_CAP: usize = 28;
const H_EPOCH: usize = 32;
const H_CLOSED: usize = 40;
const H_GONE: usize = 48;   // how many removals the owner has made (a Gone cell carries its number); raised AFTER the slot is written

/// The text a spilled value's head cell carries (the head is the cell of the value's own key, in the `Refused` state, so a reader that does not know spills sees a refusal, as it saw a value that
/// did not fit): `\u{2}spill\u{1}<generation in hex>\u{1}<bytes of the JSON text>\u{1}<number of pieces>`. The pieces are cells whose key starts with U+0002 (scans leave them out) and holds the
/// generation, so the pieces of one write are never mixed with those of another: `\u{2}s<generation in hex>\u{1}<piece number>`, each value one CBOR text string.
const SPILL_MARK: &str = "\u{2}spill\u{1}";

/// The key of the cell in which a reader says how far it has seen the other table's removals. It starts with U+0002, which no node id uses (ids use U+0001 between the node and the prop), and scans leave it out.
const ACK_KEY: &str = "\u{2}ack";

const S_SEQ: usize = 0;
const S_STATE: usize = 8;
const S_KEY_LEN: usize = 12;
const S_REV: usize = 16;
const S_VAL_LEN: usize = 24;
const S_KEY: usize = 32;

const EMPTY: u32 = 0;
const LIVE: u32 = 1;
const GONE: u32 = 2;
const REFUSED: u32 = 3;

/// What a cell says about its node.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CellState { Live, Gone, Refused }

/// A cell as a reader sees it: the revision, the state, and the value (for `Refused`, the reason as a string; for `Gone`, null).
#[derive(Clone, Debug, PartialEq)]
pub struct Cell { pub rev: f64, pub state: CellState, pub value: J }

fn up8(n: usize) -> usize { (n + 7) & !7 }

/// FNV-1a: the same in every process (a std hasher is seeded per process)
fn hash(key: &[u8]) -> u64 { key.iter().fold(0xcbf29ce484222325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x100000001b3)) }

fn piece_key(gen: u64, k: usize) -> String { format!("\u{2}s{gen:x}\u{1}{k}") }
/// The bytes of the header of a CBOR text string of `n` bytes.
fn text_header(n: usize) -> usize { if n < 24 { 1 } else if n < 256 { 2 } else if n < 65536 { 3 } else { 5 } }
/// The generation, the bytes of JSON text and the number of pieces of a spill head's value (one CBOR text string), or `None` for a value that is not one.
fn parse_head(val: &[u8]) -> Option<(u64, u64, u64)> {
    let J::String(s) = cbor::decode(val, 1).ok()? else { return None };
    let mut it = s.strip_prefix(SPILL_MARK)?.split('\u{1}');
    let gen = u64::from_str_radix(it.next()?, 16).ok()?;
    let total = it.next()?.parse().ok()?;
    let n = it.next()?.parse().ok()?;
    if it.next().is_some() { return None; }
    Some((gen, total, n))
}
fn nesting(v: &J) -> usize {
    match v { J::Array(a) => 1 + a.iter().map(nesting).max().unwrap_or(0), J::Object(o) => 1 + o.values().map(nesting).max().unwrap_or(0), _ => 0 }
}

struct View { base: *mut u8, slots: u64, key_cap: usize, val_cap: usize, stride: usize }

impl View {
    fn at(&self, off: usize) -> *mut u8 { unsafe { self.base.add(off) } }
    fn slot(&self, i: u64) -> usize { HEADER + i as usize * self.stride }
    fn u64_at(&self, off: usize) -> &AtomicU64 { unsafe { &*(self.at(off) as *const AtomicU64) } }
    fn u32_at(&self, off: usize) -> &AtomicU32 { unsafe { &*(self.at(off) as *const AtomicU32) } }
    fn val_off(&self) -> usize { S_KEY + up8(self.key_cap) }

    fn copy_out(&self, off: usize, n: usize) -> Vec<u8> { (0..n).map(|i| unsafe { std::ptr::read_volatile(self.at(off + i)) }).collect() }

    /// one slot, read consistently: (state, key, rev, value bytes)
    fn read_slot(&self, i: u64, spins: u64) -> Result<(u32, Vec<u8>, f64, Vec<u8>), String> {
        let base = self.slot(i);
        for _ in 0..spins.max(1) {
            let s1 = self.u64_at(base + S_SEQ).load(Ordering::Acquire);
            if s1 & 1 == 1 { std::hint::spin_loop(); continue; }
            let state = self.u32_at(base + S_STATE).load(Ordering::Relaxed);
            let key_len = self.u32_at(base + S_KEY_LEN).load(Ordering::Relaxed) as usize;
            let val_len = self.u32_at(base + S_VAL_LEN).load(Ordering::Relaxed) as usize;
            let rev = f64::from_bits(self.u64_at(base + S_REV).load(Ordering::Relaxed));
            // copy no more than the capacities, whatever the lengths say; the lengths are judged after the sequence number is confirmed
            let key = self.copy_out(base + S_KEY, key_len.min(self.key_cap));
            let val = self.copy_out(base + self.val_off(), val_len.min(self.val_cap));
            fence(Ordering::Acquire);
            if self.u64_at(base + S_SEQ).load(Ordering::Relaxed) != s1 { std::hint::spin_loop(); continue; }
            if state > REFUSED || key_len > self.key_cap || val_len > self.val_cap { return Err(format!("corrupt slot {i} (state {state}, key length {key_len}, value length {val_len})")); }
            return Ok((state, key, rev, val));
        }
        Err(format!("slot {i} never settled: the writer may be stuck"))
    }

    fn cell(&self, state: u32, rev: f64, val: &[u8], depth: usize) -> Result<Cell, String> {
        // a Gone cell's value is the number of its removal (the owner's bookkeeping): a reader sees null
        let value = if state == GONE { J::Null } else { cbor::decode(val, depth).map_err(|e| format!("corrupt value: {e}"))? };
        Ok(Cell { rev, state: match state { LIVE => CellState::Live, GONE => CellState::Gone, _ => CellState::Refused }, value })
    }

    /// The slot of `key` as it is now: (slot, state, revision, value bytes), or `None`.
    fn lookup(&self, key: &[u8], spins: u64) -> Result<Option<(u64, u32, f64, Vec<u8>)>, String> {
        let h = hash(key);
        for step in 0..self.slots {
            let i = (h.wrapping_add(step)) % self.slots;
            let (state, k, rev, val) = self.read_slot(i, spins)?;
            if state == EMPTY { return Ok(None); }
            if k == key { return Ok(Some((i, state, rev, val))); }
        }
        Ok(None)
    }

    /// A slot as the cell it is: a spill head is the whole value of its pieces, anything else is decoded as it is.
    fn resolve(&self, key: &[u8], state: u32, rev: f64, val: Vec<u8>, spins: u64, depth: usize) -> Result<Cell, String> {
        if state == REFUSED { if let Some(h) = parse_head(&val) { return self.assemble(key, h, rev, val, spins, depth); } }
        self.cell(state, rev, &val, depth)
    }

    /// The value of a spill head, from its pieces. The owner writes the pieces of a new value before its head and frees the pieces of the old one after it, so a reader that finds a piece gone
    /// looks at the head again: it has a new generation (follow it) or it is the same (a corrupt table). Nothing is allocated by a number the region says: the text grows by the pieces that are there.
    fn assemble(&self, key: &[u8], head: (u64, u64, u64), rev: f64, val: Vec<u8>, spins: u64, depth: usize) -> Result<Cell, String> {
        let (mut head, mut rev, mut val) = (head, rev, val);
        for _ in 0..spins.max(1) {
            let (gen, total, n) = head;
            if n == 0 || n > self.slots { return Err(format!("corrupt spilled value ({n} pieces in a table of {} slots)", self.slots)); }
            let mut json: Vec<u8> = Vec::new();
            let mut whole = true;
            for k in 0..n as usize {
                match self.lookup(piece_key(gen, k).as_bytes(), spins)? {
                    Some((_, LIVE, _, v)) => match cbor::decode(&v, 1) { Ok(J::String(t)) => json.extend_from_slice(t.as_bytes()), _ => return Err("corrupt spilled value (a piece that is not text)".into()) },
                    _ => { whole = false; break; }
                }
            }
            let now = self.lookup(key, spins)?;
            let same = matches!(&now, Some((_, st, _, v)) if *st == REFUSED && *v == val);
            if whole && same {
                if json.len() as u64 != total { return Err(format!("corrupt spilled value ({} bytes of pieces, {total} said)", json.len())); }
                let value: J = serde_json::from_slice(&json).map_err(|e| format!("corrupt value: {e}"))?;
                if nesting(&value) > depth { return Err("corrupt value: nested too deeply".into()); }
                return Ok(Cell { rev, state: CellState::Live, value });
            }
            if same { return Err("corrupt spilled value (a piece is missing)".into()); }
            // the cell changed under the read: take what it is now
            match now {
                None => return Err("a spilled value went while it was read".into()),
                Some((_, st, r, v)) => {
                    if st == REFUSED { if let Some(h) = parse_head(&v) { head = h; rev = r; val = v; continue; } }
                    return self.cell(st, r, &v, depth);
                }
            }
        }
        Err("a spilled value kept changing while it was read".into())
    }

    fn get(&self, key: &str, spins: u64, depth: usize) -> Result<Option<Cell>, String> {
        match self.lookup(key.as_bytes(), spins)? {
            None => Ok(None),
            Some((_, state, rev, val)) => self.resolve(key.as_bytes(), state, rev, val, spins, depth).map(Some),
        }
    }

    fn gone_seq(&self) -> u64 { self.u64_at(H_GONE).load(Ordering::Acquire) }

    fn scan(&self, prefix: &str, spins: u64, depth: usize) -> Result<Vec<(String, Cell)>, String> {
        let mut out = vec![];
        for i in 0..self.slots {
            let (state, k, rev, val) = self.read_slot(i, spins)?;
            if state == EMPTY || !k.starts_with(prefix.as_bytes()) || k.first() == Some(&2u8) { continue; }
            let cell = self.resolve(&k, state, rev, val, spins, depth)?;
            let key = String::from_utf8(k).map_err(|_| format!("corrupt slot {i} (a key that is not UTF-8)"))?;
            out.push((key, cell));
        }
        Ok(out)
    }
}

/// The owner's end: creates the table, writes it, removes the file when it goes.
pub struct Cells { v: View, len: usize, name: String, _file: std::fs::File, reusable: u64, ack_rev: u64, spill_gen: u64 }

unsafe impl Send for Cells {}

impl Cells {
    /// `slots`: how many cells; `key_cap` and `val_cap`: the most bytes of a key and of an encoded value; `epoch`: told to readers so they can tell the owner restarted.
    pub fn create(name: impl AsRef<str>, slots: u64, key_cap: u32, val_cap: u32, epoch: u64) -> Result<Cells, String> {
        if slots == 0 { return Err("a table needs at least one slot".into()); }
        if key_cap == 0 || val_cap == 0 { return Err("the key and value capacities must be above zero".into()); }
        let name = name.as_ref().to_string();
        let stride = S_KEY + up8(key_cap as usize) + up8(val_cap as usize);
        let len = HEADER + slots as usize * stride;
        let file = open_region(&name, Some(len), true)?;
        let base = match map(&file, len) { Ok(b) => b, Err(e) => { unlink_region(&name); return Err(e); } };
        let c = Cells { v: View { base, slots, key_cap: key_cap as usize, val_cap: val_cap as usize, stride }, len, name, _file: file, reusable: 0, ack_rev: 0, spill_gen: 0 };
        c.v.u32_at(H_LAYOUT).store(LAYOUT, Ordering::Relaxed);
        c.v.u64_at(H_SLOTS).store(slots, Ordering::Relaxed);
        c.v.u32_at(H_KEY_CAP).store(key_cap, Ordering::Relaxed);
        c.v.u32_at(H_VAL_CAP).store(val_cap, Ordering::Relaxed);
        c.v.u64_at(H_EPOCH).store(epoch, Ordering::Relaxed);
        c.v.u64_at(H_MAGIC).store(MAGIC, Ordering::Release);
        Ok(c)
    }

    fn write_slot(&mut self, i: u64, state: u32, key: &[u8], rev: f64, val: &[u8]) {
        let base = self.v.slot(i);
        let seq = self.v.u64_at(base + S_SEQ).load(Ordering::Relaxed);
        self.v.u64_at(base + S_SEQ).store(seq + 1, Ordering::Relaxed);
        fence(Ordering::Release);
        self.v.u32_at(base + S_STATE).store(state, Ordering::Relaxed);
        self.v.u32_at(base + S_KEY_LEN).store(key.len() as u32, Ordering::Relaxed);
        self.v.u32_at(base + S_VAL_LEN).store(val.len() as u32, Ordering::Relaxed);
        self.v.u64_at(base + S_REV).store(rev.to_bits(), Ordering::Relaxed);
        for (j, b) in key.iter().enumerate() { unsafe { std::ptr::write_volatile(self.v.at(base + S_KEY + j), *b); } }
        let vo = self.v.val_off();
        for (j, b) in val.iter().enumerate() { unsafe { std::ptr::write_volatile(self.v.at(base + vo + j), *b); } }
        self.v.u64_at(base + S_SEQ).store(seq + 2, Ordering::Release);
    }

    /// The removal number a `Gone` slot carries (its value), or `None` for a value that is not one.
    fn stamp(&self, i: u64) -> Option<u64> {
        let (_, _, _, val) = self.v.read_slot(i, 1).ok()?;
        match cbor::decode(&val, 2).ok()? { J::Number(n) => n.as_u64(), _ => None }
    }

    /// The slot of `key`, or the slot a new `key` would take: the first empty one, or a `Gone` slot the reader has acknowledged (its removal number is at most
    /// `reusable`). The whole probe chain is walked first, because the key may sit after such a slot.
    fn find(&self, key: &[u8]) -> Result<(u64, bool), String> {
        let h = hash(key);
        let mut reusable: Option<u64> = None;
        for step in 0..self.v.slots {
            let i = h.wrapping_add(step) % self.v.slots;
            let base = self.v.slot(i);
            let state = self.v.u32_at(base + S_STATE).load(Ordering::Relaxed);
            if state == EMPTY { return Ok((reusable.unwrap_or(i), false)); }
            let n = self.v.u32_at(base + S_KEY_LEN).load(Ordering::Relaxed) as usize;
            if n == key.len() && self.v.copy_out(base + S_KEY, n) == key { return Ok((i, true)); }
            if state == GONE && reusable.is_none() && self.stamp(i).is_some_and(|g| g <= self.reusable) { reusable = Some(i); }
        }
        match reusable { Some(i) => Ok((i, false)), None => Err(format!("the table is full ({} slots)", self.v.slots)) }
    }

    /// The generation and number of pieces of the spill head in slot `i`, if it is one.
    fn spill_of(&self, i: u64) -> Option<(u64, u64)> {
        let (state, _, _, val) = self.v.read_slot(i, 1).ok()?;
        if state != REFUSED { return None; }
        parse_head(&val).map(|(g, _, n)| (g, n))
    }

    /// Give back the pieces of a spilled value that is not the cell's value any more (each becomes `Gone`, and is reused once the reader has seen it go like any other cell).
    fn free_pieces(&mut self, gen: u64, n: u64) {
        for k in 0..n as usize {
            let key = piece_key(gen, k);
            if let Ok((_, true)) = self.find(key.as_bytes()) { let _ = self.store(&key, 0.0, GONE, &[]); }
        }
    }

    /// How many slots a new key can take now: the empty ones and the `Gone` ones the reader has acknowledged.
    fn free_slots(&self) -> u64 {
        (0..self.v.slots).filter(|&i| {
            let state = self.v.u32_at(self.v.slot(i) + S_STATE).load(Ordering::Relaxed);
            state == EMPTY || (state == GONE && self.stamp(i).is_some_and(|g| g <= self.reusable))
        }).count() as u64
    }

    /// The largest text a slot holds as one CBOR text string.
    fn piece_cap(&self) -> usize {
        let cap = self.v.val_cap;
        let mut n = cap.saturating_sub(1);
        while n > 0 && text_header(n) + n > cap { n -= 1; }
        n
    }

    fn store(&mut self, key: &str, rev: f64, state: u32, val: &[u8]) -> Result<(), String> {
        if key.len() > self.v.key_cap { return Err(format!("the key is {} bytes, the capacity of a key is {}", key.len(), self.v.key_cap)); }
        if val.len() > self.v.val_cap { return Err(format!("the value is {} bytes encoded, the capacity of a value is {}", val.len(), self.v.val_cap)); }
        let (i, found) = self.find(key.as_bytes())?;
        // the value this cell had may be a spilled one: its pieces are given back AFTER the cell says what it is now, so a reader in the middle of reading the old value finds the new head
        let old = if found { self.spill_of(i) } else { None };
        if state == GONE {
            // a removal is numbered once: a cell that is already Gone keeps its number; the count in the header is raised after the slot is written, so a reader that
            // read the count and then scans sees every removal up to it
            let existing = if found && self.v.u32_at(self.v.slot(i) + S_STATE).load(Ordering::Relaxed) == GONE { self.stamp(i) } else { None };
            let (n, raise) = match existing { Some(n) => (n, false), None => (self.v.u64_at(H_GONE).load(Ordering::Relaxed) + 1, true) };
            self.write_slot(i, GONE, key.as_bytes(), rev, &cbor::encode(&J::from(n)));
            if raise { self.v.u64_at(H_GONE).store(n, Ordering::Release); }
        } else {
            self.write_slot(i, state, key.as_bytes(), rev, val);
        }
        if let Some((g, n)) = old { self.free_pieces(g, n); }
        Ok(())
    }

    /// A value that does not fit a slot: its JSON text in pieces (new cells, a new generation), then the head, then the old pieces are given back. If the table cannot hold it all, the pieces
    /// written so far are given back and the cell keeps what it had (the error is the table-full one).
    fn store_spilled(&mut self, key: &str, rev: f64, value: &J, encoded: usize) -> Result<(), String> {
        let too_big = || format!("the value is {encoded} bytes encoded, the capacity of a value is {}", self.v.val_cap);
        if key.len() > self.v.key_cap { return Err(format!("the key is {} bytes, the capacity of a key is {}", key.len(), self.v.key_cap)); }
        let json = serde_json::to_string(value).map_err(|e| e.to_string())?;
        let cap = self.piece_cap();
        if cap < 4 { return Err(too_big()); }
        let mut chunks: Vec<&str> = Vec::new();
        let mut rest = json.as_str();
        while !rest.is_empty() {
            let mut at = rest.len().min(cap);
            while !rest.is_char_boundary(at) { at -= 1; }
            chunks.push(&rest[..at]);
            rest = &rest[at..];
        }
        let gen = self.spill_gen + 1;
        if piece_key(gen, chunks.len()).len() > self.v.key_cap { return Err(too_big()); }
        // a value the table cannot hold takes no slot at all: the pieces and (for a new key) the head must find free slots, counted before the first is written
        let need = chunks.len() as u64 + if matches!(self.find(key.as_bytes()), Ok((_, true))) { 0 } else { 1 };
        if self.free_slots() < need { return Err(format!("the table is full ({} slots)", self.v.slots)); }
        let mut written: Vec<String> = Vec::new();
        let mut fail: Option<String> = None;
        for (k, c) in chunks.iter().enumerate() {
            let pk = piece_key(gen, k);
            match self.store(&pk, 0.0, LIVE, &cbor::encode(&J::String((*c).to_string()))) { Ok(()) => written.push(pk), Err(e) => { fail = Some(e); break; } }
        }
        if fail.is_none() {
            let head = format!("{SPILL_MARK}{gen:x}\u{1}{}\u{1}{}", json.len(), chunks.len());
            if let Err(e) = self.store(key, rev, REFUSED, &cbor::encode(&J::String(head))) { fail = Some(e); }
        }
        if let Some(e) = fail {
            for pk in &written { if let Ok((_, true)) = self.find(pk.as_bytes()) { let _ = self.store(pk, 0.0, GONE, &[]); } }
            return Err(e);
        }
        self.spill_gen = gen;
        Ok(())
    }

    /// Set the cell of `key` to `value` at revision `rev`.
    pub fn put(&mut self, key: &str, rev: f64, value: &J) -> Result<(), String> {
        let encoded = cbor::encode(value);
        if encoded.len() > self.v.val_cap { return self.store_spilled(key, rev, value, encoded.len()); }
        self.store(key, rev, LIVE, &encoded)
    }

    /// Mark the cell `Gone` or `Refused` (with the reason in `why`); `Live` is `put`.
    pub fn mark(&mut self, key: &str, rev: f64, state: CellState, why: &str) -> Result<(), String> {
        match state {
            CellState::Live => Err("a live cell is written with put".into()),
            CellState::Gone => self.store(key, rev, GONE, &cbor::encode(&J::Null)),
            CellState::Refused => self.store(key, rev, REFUSED, &cbor::encode(&J::String(why.to_string()))),
        }
    }

    pub fn get(&self, key: &str, spins: u64, depth: usize) -> Result<Option<Cell>, String> { self.v.get(key, spins, depth) }
    pub fn scan(&self, prefix: &str, spins: u64, depth: usize) -> Result<Vec<(String, Cell)>, String> { self.v.scan(prefix, spins, depth) }

    /// How many removals (a cell becoming `Gone`) this table has had.
    pub fn gone_seq(&self) -> u64 { self.v.gone_seq() }

    /// Allow the slots of `Gone` cells whose removal number is at most `g` to be given to new keys. The caller passes what the table's reader has acknowledged
    /// (`CellsReader::peer_ack` on the reader's table); with more than one reader, the smallest of their acknowledgements.
    pub fn reuse_gone_up_to(&mut self, g: u64) { self.reusable = g; }

    /// This end is a reader of another table: say that every removal of that table up to `g` has been seen, in a cell of this (its own) table. Read `gone_seq` of the
    /// other table first, then scan it, then call this with what was read; the same cell is rewritten each time, so it never takes another slot.
    pub fn ack_seen(&mut self, g: u64) -> Result<(), String> {
        self.ack_rev += 1;
        let rev = self.ack_rev as f64;
        self.store(ACK_KEY, rev, LIVE, &cbor::encode(&J::from(g)))
    }

    #[doc(hidden)]
    pub fn leave_slot_mid_write_for_test(&mut self, key: &str) {
        if let Ok((i, true)) = self.find(key.as_bytes()) { let b = self.v.slot(i); let s = self.v.u64_at(b + S_SEQ).load(Ordering::Relaxed); self.v.u64_at(b + S_SEQ).store(s | 1, Ordering::Release); }
    }
    #[doc(hidden)]
    pub fn corrupt_slot_for_test(&mut self, key: &str) {
        if let Ok((i, true)) = self.find(key.as_bytes()) { let b = self.v.slot(i); self.v.u32_at(b + S_KEY_LEN).store(u32::MAX, Ordering::Release); }
    }
}

impl Drop for Cells {
    fn drop(&mut self) {
        self.v.u32_at(H_CLOSED).store(1, Ordering::Release);
        unmap(self.v.base, self.len);
        unlink_region(&self.name);
    }
}

/// A reader's end: the file mapped read-only. It can read, never write.
pub struct CellsReader { v: View, len: usize, _file: std::fs::File }

unsafe impl Send for CellsReader {}

impl CellsReader {
    pub fn attach(name: impl AsRef<str>) -> Result<CellsReader, String> {
        let name = name.as_ref();
        let file = open_region(name, None, false)?;
        let have = file.metadata().map_err(|e| e.to_string())?.len() as usize;
        if have < HEADER { return Err(format!("{name} is not a graphlink cell table (too small: {have} bytes)")); }
        let head = map_read_only(&file, HEADER)?;
        let rd = |off: usize| unsafe { (&*(head.add(off) as *const AtomicU64)).load(Ordering::Acquire) };
        let (magic, layout, slots, caps) = (rd(H_MAGIC), unsafe { (&*(head.add(H_LAYOUT) as *const AtomicU32)).load(Ordering::Relaxed) }, rd(H_SLOTS), rd(H_KEY_CAP));
        unmap(head, HEADER);
        if magic != MAGIC { return Err(format!("{name} is not a graphlink cell table (no magic)")); }
        if layout != LAYOUT { return Err(format!("{name} has layout {layout}, this library speaks layout {LAYOUT}")); }
        let (key_cap, val_cap) = ((caps & 0xffff_ffff) as usize, (caps >> 32) as usize);
        if slots == 0 || key_cap == 0 || val_cap == 0 { return Err(format!("{name} has an empty header")); }
        let stride = S_KEY + up8(key_cap) + up8(val_cap);
        let len = slots.checked_mul(stride as u64).and_then(|n| n.checked_add(HEADER as u64)).ok_or("a table size that overflows")? as usize;
        if have < len { return Err(format!("{name} is too small for its own header ({have} bytes, {len} needed)")); }
        let base = map_read_only(&file, len)?;
        Ok(CellsReader { v: View { base, slots, key_cap, val_cap, stride }, len, _file: file })
    }

    /// the owner's epoch: it changes when the owner restarts
    pub fn epoch(&self) -> u64 { self.v.u64_at(H_EPOCH).load(Ordering::Relaxed) }
    /// How many removals the owner has made.
    pub fn gone_seq(&self) -> u64 { self.v.gone_seq() }

    /// The removals of the OTHER table that this table's owner says it has seen (0 when it has said nothing): the owner of that table reads it here.
    pub fn peer_ack(&self, spins: u64, depth: usize) -> Result<u64, String> {
        Ok(match self.v.get(ACK_KEY, spins, depth)? { Some(c) if c.state == CellState::Live => c.value.as_u64().unwrap_or(0), _ => 0 })
    }
    /// the owner has gone (it dropped its end)
    pub fn is_closed(&self) -> bool { self.v.u32_at(H_CLOSED).load(Ordering::Acquire) != 0 }

    /// The cell of `key`. `spins`: how many times to try a slot that is being written before giving up (the stuck-writer rule is the caller's number); `depth`: how deeply a value may nest.
    pub fn get(&self, key: &str, spins: u64, depth: usize) -> Result<Option<Cell>, String> { self.v.get(key, spins, depth) }
    /// Every cell whose key starts with `prefix`.
    pub fn scan(&self, prefix: &str, spins: u64, depth: usize) -> Result<Vec<(String, Cell)>, String> { self.v.scan(prefix, spins, depth) }
}

impl Drop for CellsReader {
    fn drop(&mut self) { unmap(self.v.base, self.len); }
}


/// One end's bookkeeping for reclaiming slots (docs/shared-memory.md, section 4b). Each end of a link is both a reader (of the other's table) and an owner (of its own), and each turn
/// does the two halves with the same order every time: read the count of the other table's removals BEFORE the pass that reads it, say AFTER the pass that they have been seen, and give
/// the slots the other end has acknowledged to new keys. Every consumer of the tables does this, so it lives here.
#[derive(Debug, Default)]
pub struct Reclaim { acked: u64 }

impl Reclaim {
    pub fn new() -> Reclaim { Reclaim::default() }
    /// The count to carry through the pass: call it before reading the other table.
    pub fn before(theirs: &CellsReader) -> u64 { theirs.gone_seq() }
    /// Call it after the pass that read `theirs`, with what `before` returned. It says in `mine` (this end's own table) that every removal up to `seen` has been seen, when that
    /// is news, and lets `mine` give the `Gone` slots the other end has acknowledged to new keys. If the acknowledgement cannot be written (this table has no slot left for it) the
    /// error is returned and nothing is counted as acknowledged, so the next turn tries again; the reuse is applied in any case.
    pub fn after(&mut self, seen: u64, theirs: &CellsReader, mine: &mut Cells, spins: u64, depth: usize) -> Result<(), String> {
        mine.reuse_gone_up_to(theirs.peer_ack(spins, depth)?);
        if seen != self.acked {
            mine.ack_seen(seen)?;
            self.acked = seen;
        }
        Ok(())
    }
    /// The count this end has acknowledged so far.
    pub fn acked(&self) -> u64 { self.acked }
}
