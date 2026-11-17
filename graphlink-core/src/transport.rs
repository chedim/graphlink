//! The transport trait: the one edge of the library that touches the outside. A transport carries frames to the peer and hands back the ones that arrived; it never blocks
//! the caller (`recv` answers `None` when nothing is there), so the engine's thread can poll it at the end of a round. A pipe, a socket, a WebSocket, an MQTT topic or a
//! shared-memory ring each implement it; the library never starts a thread for one.
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use serde_json::Value as J;

use crate::frame::Frame;

pub trait Transport {
    type Error: std::fmt::Debug;
    /// queue a frame for the peer
    fn send(&mut self, frame: &Frame) -> Result<(), Self::Error>;
    /// the next frame that arrived, or `None`; never waits
    fn recv(&mut self) -> Result<Option<Frame>, Self::Error>;
}

type Queue = Arc<Mutex<VecDeque<J>>>;

/// One end of an in-memory pair made by [`pipe`]. Frames cross it as JSON, so a test over a pipe exercises the wire encoding too.
pub struct PipeEnd { tx: Queue, rx: Queue }

/// Two connected ends: what one sends, the other receives, in order.
pub fn pipe() -> (PipeEnd, PipeEnd) {
    let (a, b): (Queue, Queue) = (Default::default(), Default::default());
    (PipeEnd { tx: a.clone(), rx: b.clone() }, PipeEnd { tx: b, rx: a })
}

impl Transport for PipeEnd {
    type Error = String;
    fn send(&mut self, frame: &Frame) -> Result<(), String> {
        self.tx.lock().map_err(|e| e.to_string())?.push_back(frame.encode());
        Ok(())
    }
    fn recv(&mut self) -> Result<Option<Frame>, String> {
        match self.rx.lock().map_err(|e| e.to_string())?.pop_front() { Some(j) => Frame::decode(&j).map(Some), None => Ok(None) }
    }
}
