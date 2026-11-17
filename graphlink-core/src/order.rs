//! The frame counters (docs/frames.md, "Ordering"): `Inbox` decides what to do with the `n` of a frame taken from the peer, `Outbox` numbers the frames sent and keeps the
//! peer's acknowledgements. Plain counters, no I/O; what a gap means (resync, close) and how many unacknowledged frames are too many are the engine's decisions.
use crate::frame::{Frame, Op};

/// What to do with a frame taken from the peer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Take {
    /// the next one: process it
    Accept,
    /// already taken (a retried request): drop it
    Duplicate,
    /// one or more frames before it were not taken; `expected` is the `n` that should have come
    Gap { expected: u64 },
}

/// The frames taken from one peer: counting starts at 1, so `ack` 0 means none yet.
#[derive(Clone, Debug, Default)]
pub struct Inbox { last: u64 }

impl Inbox {
    pub fn take(&mut self, n: u64) -> Take {
        if n <= self.last { Take::Duplicate }
        else if n == self.last + 1 { self.last = n; Take::Accept }
        else { Take::Gap { expected: self.last + 1 } }
    }
    /// The highest `n` taken: what the next frame sent to this peer carries as `ack`.
    pub fn ack(&self) -> u64 { self.last }
}

/// The frames sent to one peer.
#[derive(Clone, Debug, Default)]
pub struct Outbox { sent: u64, acked: u64 }

impl Outbox {
    /// The next frame: numbered from 1, carrying what `inbox` has taken from the same peer.
    pub fn frame(&mut self, v: u32, inbox: &Inbox, ops: Vec<Op>) -> Frame {
        self.sent += 1;
        Frame { v, n: self.sent, ack: inbox.ack(), ops }
    }
    /// The peer's `ack`: false (and nothing recorded) when it goes back or names a frame never sent.
    pub fn acked(&mut self, ack: u64) -> bool {
        if ack < self.acked || ack > self.sent { return false; }
        self.acked = ack;
        true
    }
    /// Frames sent and not yet acknowledged.
    pub fn unacked(&self) -> u64 { self.sent - self.acked }
}
