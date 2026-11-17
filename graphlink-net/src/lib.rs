//! A TCP transport for graphlink-core: one JSON frame per line, over a non-blocking socket. `recv` never waits, so an engine's thread can poll it at the end of a round;
//! `send` queues the line and writes as much as the socket takes now, the rest goes out on the next `send` or `recv`.
#[cfg(feature = "tls")]
pub mod tls;

use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};

use graphlink_core::frame::Frame;
use graphlink_core::transport::Transport;

/// the socket: plain, or wrapped in a TLS session (the feature `tls`)
enum Conn {
    Plain(TcpStream),
    #[cfg(feature = "tls")]
    Client(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
    #[cfg(feature = "tls")]
    Server(Box<rustls::StreamOwned<rustls::ServerConnection, TcpStream>>),
}

impl Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self { Conn::Plain(s) => s.read(buf), #[cfg(feature = "tls")] Conn::Client(s) => s.read(buf), #[cfg(feature = "tls")] Conn::Server(s) => s.read(buf) }
    }
}
impl Write for Conn {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self { Conn::Plain(s) => s.write(buf), #[cfg(feature = "tls")] Conn::Client(s) => s.write(buf), #[cfg(feature = "tls")] Conn::Server(s) => s.write(buf) }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self { Conn::Plain(s) => s.flush(), #[cfg(feature = "tls")] Conn::Client(s) => s.flush(), #[cfg(feature = "tls")] Conn::Server(s) => s.flush() }
    }
}

pub struct Tcp { stream: Conn, rbuf: Vec<u8>, wbuf: Vec<u8>, closed: bool }

impl Tcp {
    pub fn connect(addr: impl ToSocketAddrs) -> std::io::Result<Tcp> { Tcp::from_stream(TcpStream::connect(addr)?) }

    pub fn from_stream(stream: TcpStream) -> std::io::Result<Tcp> {
        stream.set_nonblocking(true)?;
        stream.set_nodelay(true)?;
        Ok(Tcp { stream: Conn::Plain(stream), rbuf: vec![], wbuf: vec![], closed: false })
    }

    /// Connect and speak TLS: the certificate the server presents must be one the client trusts and must cover `name` (a DNS name or an IP address). The handshake runs as the connection is polled.
    #[cfg(feature = "tls")]
    pub fn connect_tls(addr: impl ToSocketAddrs, name: &str, client: &tls::TlsClient) -> std::io::Result<Tcp> {
        let sock = TcpStream::connect(addr)?;
        sock.set_nonblocking(true)?;
        sock.set_nodelay(true)?;
        let conn = rustls::ClientConnection::new(client.0.clone(), tls::server_name(name).map_err(|e| std::io::Error::new(ErrorKind::InvalidInput, e))?)
            .map_err(|e| std::io::Error::new(ErrorKind::InvalidInput, e.to_string()))?;
        Ok(Tcp { stream: Conn::Client(Box::new(rustls::StreamOwned::new(conn, sock))), rbuf: vec![], wbuf: vec![], closed: false })
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
pub struct Listener { inner: TcpListener, #[cfg(feature = "tls")] tls: Option<tls::TlsServer> }

impl Listener {
    pub fn bind(addr: impl ToSocketAddrs) -> std::io::Result<Listener> {
        let inner = TcpListener::bind(addr)?;
        inner.set_nonblocking(true)?;
        Ok(Listener { inner, #[cfg(feature = "tls")] tls: None })
    }
    /// Every connection this listener accepts speaks TLS with this certificate.
    #[cfg(feature = "tls")]
    pub fn with_tls(mut self, server: tls::TlsServer) -> Listener { self.tls = Some(server); self }
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> { self.inner.local_addr() }
    pub fn accept(&self) -> std::io::Result<Option<Tcp>> {
        match self.inner.accept() {
            #[cfg(feature = "tls")]
            Ok((s, _)) if self.tls.is_some() => {
                s.set_nonblocking(true)?;
                s.set_nodelay(true)?;
                let conn = rustls::ServerConnection::new(self.tls.as_ref().unwrap().0.clone()).map_err(|e| std::io::Error::new(ErrorKind::InvalidInput, e.to_string()))?;
                Ok(Some(Tcp { stream: Conn::Server(Box::new(rustls::StreamOwned::new(conn, s))), rbuf: vec![], wbuf: vec![], closed: false }))
            }
            Ok((s, _)) => Tcp::from_stream(s).map(Some),
            Err(e) if e.kind() == ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(e),
        }
    }
}
