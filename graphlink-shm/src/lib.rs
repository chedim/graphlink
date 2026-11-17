//! A shared-memory transport for graphlink-core (docs/shared-memory.md: the control ring first). Two single-producer single-consumer rings live in one file that both processes
//! map: one carries frames from the creator to the attacher, the other the way back. A frame is a 4-byte little-endian length and the frame's JSON bytes, which may wrap around
//! the end of the ring. `recv` never waits; `send` queues what the ring cannot take yet on the sender and writes it as the reader makes room. No dependency: the mapping is
//! the two `mmap` and `munmap` calls of the C library, declared here (Unix only). Every length read from the region is untrusted: it is checked before it is used.
#![cfg(unix)]

pub mod cells;

use std::fs::{File, OpenOptions};
use std::os::raw::{c_int, c_void};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use graphlink_core::frame::Frame;
use graphlink_core::transport::Transport;

extern "C" {
    fn mmap(addr: *mut c_void, len: usize, prot: c_int, flags: c_int, fd: c_int, off: i64) -> *mut c_void;
    fn munmap(addr: *mut c_void, len: usize) -> c_int;
}
const PROT_READ: c_int = 1;
const PROT_WRITE: c_int = 2;
const MAP_SHARED: c_int = 1;

const MAGIC: u64 = u64::from_le_bytes(*b"GLNKSHM1");
const LAYOUT: u32 = 1;
const HEADER: usize = 128;
const RING_HEAD: usize = 0;     // the producer's position, only ever grows
const RING_TAIL: usize = 64;    // the consumer's position, only ever grows (on its own cache line)
const RING_DATA: usize = 128;

// header fields
const H_MAGIC: usize = 0;
const H_LAYOUT: usize = 8;
const H_RING: usize = 16;
const H_CLOSED_CREATOR: usize = 24;
const H_CLOSED_ATTACHER: usize = 28;

pub struct Shm {
    base: *mut u8,
    len: usize,
    ring: usize,
    creator: bool,
    path: Option<PathBuf>,
    pending: Vec<u8>,
    closed: bool,
    _file: File,
}

// the mapping is plain shared memory used through atomics; one end is used by one thread at a time
unsafe impl Send for Shm {}

fn region_len(ring: usize) -> usize { HEADER + 2 * (RING_DATA + ring) }

pub(crate) fn map(file: &File, len: usize) -> Result<*mut u8, String> { map_with(file, len, PROT_READ | PROT_WRITE) }

/// a mapping that cannot be written through: a reader's view of a region it does not own
pub(crate) fn map_read_only(file: &File, len: usize) -> Result<*mut u8, String> { map_with(file, len, PROT_READ) }

pub(crate) fn unmap(p: *mut u8, len: usize) { unsafe { munmap(p as *mut c_void, len); } }

fn map_with(file: &File, len: usize, prot: c_int) -> Result<*mut u8, String> {
    let p = unsafe { mmap(std::ptr::null_mut(), len, prot, MAP_SHARED, file.as_raw_fd(), 0) };
    if p as isize == -1 || p.is_null() { return Err(format!("cannot map {len} bytes: {}", std::io::Error::last_os_error())); }
    Ok(p as *mut u8)
}

impl Shm {
    /// Make the region at `path` (a file; it is removed when this end goes) with rings of `ring_bytes` each (a multiple of 8, at least 64).
    pub fn create(path: impl AsRef<Path>, ring_bytes: usize) -> Result<Shm, String> {
        if ring_bytes < 64 || ring_bytes % 8 != 0 { return Err("a ring is a multiple of 8 bytes, at least 64".into()); }
        let path = path.as_ref();
        let file = OpenOptions::new().read(true).write(true).create_new(true).open(path).map_err(|e| format!("cannot create {}: {e}", path.display()))?;
        let len = region_len(ring_bytes);
        file.set_len(len as u64).map_err(|e| format!("cannot size {}: {e}", path.display()))?;
        let base = map(&file, len)?;
        let s = Shm { base, len, ring: ring_bytes, creator: true, path: Some(path.to_path_buf()), pending: vec![], closed: false, _file: file };
        s.u64_at(H_RING).store(ring_bytes as u64, Ordering::Relaxed);
        s.u32_at(H_LAYOUT).store(LAYOUT, Ordering::Relaxed);
        s.u64_at(H_MAGIC).store(MAGIC, Ordering::Release);   // last: an attacher that sees the magic sees a whole header
        Ok(s)
    }

    /// Map the region another process made at `path`.
    pub fn attach(path: impl AsRef<Path>) -> Result<Shm, String> {
        let path = path.as_ref();
        let file = OpenOptions::new().read(true).write(true).open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        let have = file.metadata().map_err(|e| e.to_string())?.len() as usize;
        if have < HEADER { return Err(format!("{} is too small to be a graphlink region ({have} bytes)", path.display())); }
        let head = map(&file, HEADER)?;
        let (magic, layout, ring) = unsafe {
            (AtomicU64::from_ptr(head.add(H_MAGIC) as *mut u64).load(Ordering::Acquire), AtomicU32::from_ptr(head.add(H_LAYOUT) as *mut u32).load(Ordering::Relaxed), AtomicU64::from_ptr(head.add(H_RING) as *mut u64).load(Ordering::Relaxed) as usize)
        };
        unsafe { munmap(head as *mut c_void, HEADER); }
        if magic != MAGIC { return Err(format!("{} is not a graphlink region (no magic)", path.display())); }
        if layout != LAYOUT { return Err(format!("{} has layout {layout}, this library speaks layout {LAYOUT}", path.display())); }
        if ring < 64 || ring % 8 != 0 || have < region_len(ring) { return Err(format!("{} is too small for its own header (ring {ring}, file {have} bytes)", path.display())); }
        let len = region_len(ring);
        let base = map(&file, len)?;
        Ok(Shm { base, len, ring, creator: false, path: None, pending: vec![], closed: false, _file: file })
    }

    /// The other end has gone (it dropped its `Shm`), or a ring was found corrupt.
    pub fn is_closed(&self) -> bool {
        self.closed || self.u32_at(if self.creator { H_CLOSED_ATTACHER } else { H_CLOSED_CREATOR }).load(Ordering::Acquire) != 0
    }

    fn u64_at(&self, off: usize) -> &AtomicU64 { unsafe { &*(self.base.add(off) as *const AtomicU64) } }
    fn u32_at(&self, off: usize) -> &AtomicU32 { unsafe { &*(self.base.add(off) as *const AtomicU32) } }
    /// the ring this end writes (0 for the creator, 1 for the attacher) and the one it reads
    fn ring_base(&self, which: usize) -> usize { HEADER + which * (RING_DATA + self.ring) }
    fn tx(&self) -> usize { self.ring_base(if self.creator { 0 } else { 1 }) }
    fn rx(&self) -> usize { self.ring_base(if self.creator { 1 } else { 0 }) }

    fn put(&self, ring: usize, pos: u64, bytes: &[u8]) {
        let size = self.ring as u64;
        for (i, b) in bytes.iter().enumerate() { unsafe { *self.base.add(ring + RING_DATA + ((pos + i as u64) % size) as usize) = *b; } }
    }
    fn get(&self, ring: usize, pos: u64, out: &mut [u8]) {
        let size = self.ring as u64;
        for (i, o) in out.iter_mut().enumerate() { *o = unsafe { *self.base.add(ring + RING_DATA + ((pos + i as u64) % size) as usize) }; }
    }

    /// write as much of `pending` as the ring has room for, whole records only
    fn flush(&mut self) {
        let ring = self.tx();
        let (head, tail) = (self.u64_at(ring + RING_HEAD).load(Ordering::Relaxed), self.u64_at(ring + RING_TAIL).load(Ordering::Acquire));
        let mut head_now = head;
        let mut off = 0usize;
        while off + 4 <= self.pending.len() {
            let n = u32::from_le_bytes(self.pending[off..off + 4].try_into().unwrap()) as u64;
            let need = 4 + n;
            if head_now + need - tail > self.ring as u64 { break; }
            let rec = self.pending[off..off + need as usize].to_vec();
            self.put(ring, head_now, &rec);
            head_now += need;
            off += need as usize;
        }
        if head_now != head { self.u64_at(ring + RING_HEAD).store(head_now, Ordering::Release); self.pending.drain(..off); }
    }
}

impl Transport for Shm {
    type Error = String;

    fn send(&mut self, frame: &Frame) -> Result<(), String> {
        let body = frame.encode().to_string().into_bytes();
        if 4 + body.len() > self.ring { return Err(format!("a frame of {} bytes is larger than the ring ({} bytes)", body.len(), self.ring)); }
        self.pending.extend_from_slice(&(body.len() as u32).to_le_bytes());
        self.pending.extend_from_slice(&body);
        self.flush();
        Ok(())
    }

    fn recv(&mut self) -> Result<Option<Frame>, String> {
        self.flush();
        let ring = self.rx();
        let tail = self.u64_at(ring + RING_TAIL).load(Ordering::Relaxed);
        let head = self.u64_at(ring + RING_HEAD).load(Ordering::Acquire);
        let avail = head.wrapping_sub(tail);
        if avail == 0 { return Ok(None); }
        if avail > self.ring as u64 || avail < 4 { self.closed = true; return Err("the ring is corrupt (its fill is out of range)".into()); }
        let mut lenb = [0u8; 4];
        self.get(ring, tail, &mut lenb);
        let n = u32::from_le_bytes(lenb) as u64;
        if 4 + n > avail { self.closed = true; return Err("the ring is corrupt (a length beyond what was written)".into()); }
        let mut body = vec![0u8; n as usize];
        self.get(ring, tail + 4, &mut body);
        self.u64_at(ring + RING_TAIL).store(tail + 4 + n, Ordering::Release);
        let j: serde_json::Value = serde_json::from_slice(&body).map_err(|e| format!("a frame that is not JSON: {e}"))?;
        Frame::decode(&j).map(Some)
    }
}

impl Drop for Shm {
    fn drop(&mut self) {
        self.u32_at(if self.creator { H_CLOSED_CREATOR } else { H_CLOSED_ATTACHER }).store(1, Ordering::Release);
        unsafe { munmap(self.base as *mut c_void, self.len); }
        if let Some(p) = &self.path { let _ = std::fs::remove_file(p); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_length_written_into_the_ring_that_the_bytes_cannot_hold_is_an_error_not_a_crash() {
        let p = std::env::temp_dir().join(format!("graphlink-shm-corrupt-{}", std::process::id()));
        let mut a = Shm::create(&p, 256).unwrap();
        let mut b = Shm::attach(&p).unwrap();
        // a hostile writer: a record that claims 200 bytes with only 8 written
        let ring = a.tx();
        a.put(ring, 0, &200u32.to_le_bytes());
        a.put(ring, 4, &[1, 2, 3, 4]);
        a.u64_at(ring + RING_HEAD).store(8, Ordering::Release);
        let e = b.recv().unwrap_err();
        assert!(e.contains("corrupt"), "{e}");
        assert!(b.is_closed());
        // and a fill larger than the ring
        let mut c = Shm::create(p.with_extension("2"), 256).unwrap();
        let mut d = Shm::attach(p.with_extension("2")).unwrap();
        let ring = c.tx();
        c.u64_at(ring + RING_HEAD).store(10_000, Ordering::Release);
        assert!(d.recv().unwrap_err().contains("corrupt"));
        let _ = (&mut a, &mut c);
    }
}
