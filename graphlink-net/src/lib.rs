//! A TCP transport for graphlink-core: one JSON frame per line, over a non-blocking socket. `recv` never waits, so an engine's thread can poll it at the end of a round;
//! `send` queues the line and writes as much as the socket takes now, the rest goes out on the next `send` or `recv`.
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};

use graphlink_core::frame::Frame;
use graphlink_core::transport::Transport;

pub struct Tcp { stream: TcpStream, rbuf: Vec<u8>, wbuf: Vec<u8>, closed: bool }

impl Tcp {
    pub fn connect(addr: impl ToSocketAddrs) -> std::io::Result<Tcp> { Tcp::from_stream(TcpStream::connect(addr)?) }

    pub fn from_stream(stream: TcpStream) -> std::io::Result<Tcp> {
        stream.set_nonblocking(true)?;
        stream.set_nodelay(true)?;
        Ok(Tcp { stream, rbuf: vec![], wbuf: vec![], closed: false })
    }

    /// the peer closed its end, or a write failed
    pub fn is_closed(&self) -> bool { self.closed }

    fn flush(&mut self) {
        while !self.wbuf.is_empty() && !self.closed {
            match self.stream.write(&self.wbuf) {
                Ok(0) => self.closed = true,
                Ok(n) => { self.wbuf.drain(..n); }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(_) => self.closed = true,
            }
        }
    }

    fn fill(&mut self) {
        let mut chunk = [0u8; 4096];
        while !self.closed {
            match self.stream.read(&mut chunk) {
                Ok(0) => self.closed = true,
                Ok(n) => self.rbuf.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(_) => self.closed = true,
            }
        }
    }
}

impl Transport for Tcp {
    type Error = String;

    fn send(&mut self, frame: &Frame) -> Result<(), String> {
        if self.closed { return Err("the connection is closed".into()); }
        self.wbuf.extend_from_slice(frame.encode().to_string().as_bytes());
        self.wbuf.push(b'\n');
        self.flush();
        Ok(())
    }

    fn recv(&mut self) -> Result<Option<Frame>, String> {
        self.flush();
        self.fill();
        let Some(end) = self.rbuf.iter().position(|b| *b == b'\n') else { return Ok(None) };
        let line: Vec<u8> = self.rbuf.drain(..=end).collect();
        let j: serde_json::Value = serde_json::from_slice(&line[..line.len() - 1]).map_err(|e| format!("a line that is not JSON: {e}"))?;
        Frame::decode(&j).map(Some)
    }
}

/// A listening socket that never waits: `accept` answers `None` when nobody is there.
pub struct Listener { inner: TcpListener }

impl Listener {
    pub fn bind(addr: impl ToSocketAddrs) -> std::io::Result<Listener> {
        let inner = TcpListener::bind(addr)?;
        inner.set_nonblocking(true)?;
        Ok(Listener { inner })
    }
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> { self.inner.local_addr() }
    pub fn accept(&self) -> std::io::Result<Option<Tcp>> {
        match self.inner.accept() {
            Ok((s, _)) => Tcp::from_stream(s).map(Some),
            Err(e) if e.kind() == ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(e),
        }
    }
}
