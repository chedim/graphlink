//! The state machine of one link to one peer (docs/frames.md, "A link"): the hello and the version negotiation, the frame counters, and what a frame taken from the peer
//! means to the engine. Sans-I/O: `receive` takes a frame and returns events (and maybe a frame to send back); `open` and `send` return the frames to give a transport.
use std::collections::BTreeMap;

use crate::frame::{negotiate, Frame, Negotiated, Op, VERSION};
use crate::order::{Inbox, Outbox, Take};

/// What this side says about itself in its `hello`.
#[derive(Clone, Debug)]
pub struct Me { pub ns: String, pub build: String, pub epoch: f64, pub support: Vec<u32> }

/// What `receive` tells the engine.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// the peer's hello was taken and a version is agreed
    Opened { peer_ns: String, peer_build: String, peer_epoch: f64, have: BTreeMap<String, f64>, version: u32 },
    /// an op from the peer, in order, once
    Op(Op),
    /// a frame already taken was dropped
    Duplicate { n: u64 },
    /// a frame skipped one; it was not taken, and `expected` is the `n` that should have come
    Gap { expected: u64 },
    /// no version in common (we sent `failed`), or the peer says it has none; the link is closed
    Failed { why: String },
    /// the peer said `bye`; the link is closed
    Closed { why: String },
    /// the frame broke a rule (an op before the hello, a version that was not negotiated); it was not delivered
    Refused { why: String },
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum State { Idle, Opening, Open, Closed }

pub struct Link { me: Me, state: State, version: u32, hello_seen: bool, inbox: Inbox, outbox: Outbox }

impl Link {
    pub fn new(me: Me) -> Link { Link { me, state: State::Idle, version: VERSION, hello_seen: false, inbox: Inbox::default(), outbox: Outbox::default() } }

    pub fn is_open(&self) -> bool { self.state == State::Open }

    fn highest(&self) -> u32 { self.me.support.iter().copied().max().unwrap_or(VERSION) }

    /// The first frame: our `hello`, written in the highest version we speak (a hello is decodable in every version).
    pub fn open(&mut self) -> Frame {
        let hello = Op::Hello { ns: self.me.ns.clone(), build: self.me.build.clone(), epoch: self.me.epoch, have: BTreeMap::new(), support: self.me.support.clone() };
        if self.state == State::Idle { self.state = if self.hello_seen { State::Open } else { State::Opening }; }
        self.outbox.frame(self.highest(), &self.inbox, vec![hello])
    }

    /// A frame for the peer; only once the link is open (our hello sent and theirs taken).
    pub fn send(&mut self, ops: Vec<Op>) -> Result<Frame, String> {
        if self.state != State::Open { return Err("the link is not open".into()); }
        Ok(self.outbox.frame(self.version, &self.inbox, ops))
    }

    /// Take a frame from the peer: the events for the engine, and a frame to send straight back (the `failed` of a version mismatch).
    pub fn receive(&mut self, frame: Frame) -> (Vec<Event>, Option<Frame>) {
        if self.state == State::Closed { return (vec![], None); }
        match self.inbox.take(frame.n) {
            Take::Duplicate => return (vec![Event::Duplicate { n: frame.n }], None),
            Take::Gap { expected } => return (vec![Event::Gap { expected }], None),
            Take::Accept => {}
        }
        self.outbox.acked(frame.ack);
        let mut events = vec![];
        let mut reply = None;
        for op in frame.ops {
            match op {
                Op::Hello { ns, build, epoch, have, support } => match negotiate(&self.me.support, &support) {
                    Negotiated::Speak(version) => {
                        self.version = version;
                        self.hello_seen = true;
                        if self.state == State::Opening { self.state = State::Open; }
                        events.push(Event::Opened { peer_ns: ns, peer_build: build, peer_epoch: epoch, have, version });
                    }
                    Negotiated::Failed(failed) => {
                        self.state = State::Closed;
                        if let Op::Failed { why, .. } = &failed { events.push(Event::Failed { why: why.clone() }); }
                        reply = Some(self.outbox.frame(self.highest(), &self.inbox, vec![failed]));
                        break;
                    }
                },
                Op::Failed { why, .. } => { self.state = State::Closed; events.push(Event::Failed { why }); break; }
                Op::Unknown(_) => {}
                _ if !self.hello_seen => { events.push(Event::Refused { why: "an op before the peer's hello".into() }); break; }
                _ if self.state == State::Open && frame.v != self.version => { events.push(Event::Refused { why: format!("a frame in version {}, the link speaks {}", frame.v, self.version) }); break; }
                Op::Bye { why } => { events.push(Event::Op(Op::Bye { why: why.clone() })); events.push(Event::Closed { why }); self.state = State::Closed; break; }
                other => events.push(Event::Op(other)),
            }
        }
        (events, reply)
    }
}
