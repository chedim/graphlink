//! A shared-memory transport for graphlink-core (docs/shared-memory.md: the control ring first). Two single-producer single-consumer rings live in one file that both processes
//! map: one carries frames from the creator to the attacher, the other the way back. A frame is a 4-byte little-endian length and the frame's JSON bytes, which may wrap around
//! the end of the ring. `recv` never waits; `send` queues what the ring cannot take yet on the sender and writes it as the reader makes room. No dependency: the mapping is
//! the `shm_open`, `shm_unlink`, `mmap` and `munmap` calls of the C library, declared here (Unix only). A region is identified by a NAME (`/gl-...`, one slash, at most 31 bytes: the shortest limit among the systems), never a path: RAM only on Linux, no file to clean up, and the same name a Windows backend would give to a named mapping. Every length read from the region is untrusted: it is checked before it is used.
#![cfg(unix)]

pub mod cells;

use std::ffi::CString;
use std::fs::File;
use std::os::raw::{c_char, c_int, c_void};
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use graphlink_core::frame::Frame;
use graphlink_core::transport::Transport;

#[cfg_attr(target_os = "linux", link(name = "rt"))]
extern "C" {
    // variadic in C (the mode is an optional third argument): Apple's arm64 passes variadic arguments differently, so it must be declared so
    fn shm_open(name: *const c_char, oflag: c_int, ...) -> c_int;
    fn shm_unlink(name: *const c_char) -> c_int;
    fn mmap(addr: *mut c_void, len: usize, prot: c_int, flags: c_int, fd: c_int, off: i64) -> *mut c_void;
    fn munmap(addr: *mut c_void, len: usize) -> c_int;
}
const PROT_READ: c_int = 1;
const PROT_WRITE: c_int = 2;
const MAP_SHARED: c_int = 1;
const O_RDONLY: c_int = 0;
const O_RDWR: c_int = 2;
#[cfg(any(target_os = "linux", target_os = "android"))]
const O_CREAT: c_int = 0x40;
#[cfg(any(target_os = "linux", target_os = "android"))]
const O_EXCL: c_int = 0x80;
#[cfg(not(any(target_os = "linux", target_os = "android")))]
const O_CREAT: c_int = 0x200;   // macOS and the BSDs
#[cfg(not(any(target_os = "linux", target_os = "android")))]
const O_EXCL: c_int = 0x800;

/// The longest region name: 31 bytes, the limit of macOS, so a name that works there works everywhere.
const NAME_MAX: usize = 31;

fn check_name(name: &str) -> Result<CString, String> {
    let ok = name.len() >= 2 && name.len() <= NAME_MAX && name.starts_with('/') && !name[1..].contains('/') && !name.contains('\0');
    if !ok { return Err(format!("{name:?} is not a valid region name (it starts with one '/', has no other '/', and is at most {NAME_MAX} bytes)")); }
    CString::new(name).map_err(|e| e.to_string())
}

/// Open a region by name; with `create_len` it is made (and sized), and fails when the name is taken. `writable` false opens it for reading only.
pub(crate) fn open_region(name: &str, create_len: Option<usize>, writable: bool) -> Result<File, String> {
    let c = check_name(name)?;
    let flags = match create_len { Some(_) => O_CREAT | O_EXCL | O_RDWR, None if writable => O_RDWR, None => O_RDONLY };
    let fd = unsafe { shm_open(c.as_ptr(), flags, 0o600 as c_int) };
    if fd < 0 { return Err(format!("cannot {} region {name}: {}", if create_len.is_some() { "create" } else { "open" }, std::io::Error::last_os_error())); }
    let file = unsafe { File::from_raw_fd(fd) };
    if let Some(len) = create_len {
        if let Err(e) = file.set_len(len as u64) { unlink_region(name); return Err(format!("cannot size region {name}: {e}")); }
    }
    Ok(file)
}

/// Remove a region name a creator left behind (it died without dropping its end). `Ok(true)` when a name was removed, `Ok(false)` when there was none. Only for a name the
/// caller owns: a live creator's region would lose its name (its mapped memory stays valid, but no one new can attach).
pub fn remove_region(name: &str) -> Result<bool, String> {
    let c = check_name(name)?;
    if unsafe { shm_unlink(c.as_ptr()) } == 0 { return Ok(true); }
    let e = std::io::Error::last_os_error();
    if e.kind() == std::io::ErrorKind::NotFound { Ok(false) } else { Err(format!("cannot remove region {name}: {e}")) }
}

pub(crate) fn unlink_region(name: &str) { if let Ok(c) = CString::new(name) { unsafe { shm_unlink(c.as_ptr()); } } }

/// A region of zeros that is not a graphlink region, removed when this is dropped (for the tests of what attach refuses).
#[doc(hidden)]
pub struct JunkRegion { name: String, _file: File }
impl Drop for JunkRegion { fn drop(&mut self) { unlink_region(&self.name); } }
#[doc(hidden)]
pub fn junk_region_for_test(name: &str, len: usize) -> Result<JunkRegion, String> { Ok(JunkRegion { name: name.to_string(), _file: open_region(name, Some(len), true)? }) }

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
    name: Option<String>,
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
    /// Make the region called `name` (it is removed when this end goes) with rings of `ring_bytes` each (a multiple of 8, at least 64).
    pub fn create(name: impl AsRef<str>, ring_bytes: usize) -> Result<Shm, String> {
        let name = name.as_ref();
        if ring_bytes < 64 || ring_bytes % 8 != 0 { return Err("a ring is a multiple of 8 bytes, at least 64".into()); }
        let len = region_len(ring_bytes);
        let file = open_region(name, Some(len), true)?;
        let base = match map(&file, len) { Ok(b) => b, Err(e) => { unlink_region(name); return Err(e); } };
        let s = Shm { base, len, ring: ring_bytes, creator: true, name: Some(name.to_string()), pending: vec![], closed: false, _file: file };
        s.u64_at(H_RING).store(ring_bytes as u64, Ordering::Relaxed);
        s.u32_at(H_LAYOUT).store(LAYOUT, Ordering::Relaxed);
        s.u64_at(H_MAGIC).store(MAGIC, Ordering::Release);   // last: an attacher that sees the magic sees a whole header
        Ok(s)
    }

    /// Open the region another process made under `name`. The header is mapped read-only and checked (magic, layout, size) before anything is trusted or written.
    pub fn attach(name: impl AsRef<str>) -> Result<Shm, String> {
        let name = name.as_ref();
        let file = open_region(name, None, true)?;
        let have = file.metadata().map_err(|e| e.to_string())?.len() as usize;
        if have < HEADER { return Err(format!("{name} is too small to be a graphlink region ({have} bytes)")); }
        let head = map_read_only(&file, HEADER)?;
        let (magic, layout, ring) = unsafe {
            (AtomicU64::from_ptr(head.add(H_MAGIC) as *mut u64).load(Ordering::Acquire), AtomicU32::from_ptr(head.add(H_LAYOUT) as *mut u32).load(Ordering::Relaxed), AtomicU64::from_ptr(head.add(H_RING) as *mut u64).load(Ordering::Relaxed) as usize)
        };
        unmap(head, HEADER);
        if magic != MAGIC { return Err(format!("{name} is not a graphlink region (no magic)")); }
        if layout != LAYOUT { return Err(format!("{name} has layout {layout}, this library speaks layout {LAYOUT}")); }
        if ring < 64 || ring % 8 != 0 || have < region_len(ring) { return Err(format!("{name} is too small for its own header (ring {ring}, region {have} bytes)")); }
        let len = region_len(ring);
        let base = map(&file, len)?;
        Ok(Shm { base, len, ring, creator: false, name: None, pending: vec![], closed: false, _file: file })
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

    /// How many bytes of frames still wait on this end because the ring was full. They go out when the reader makes room AND this end calls `flush`, `send` or `recv`: an end that
    /// stops calling leaves them queued, so a sender that is finished keeps flushing until this is zero.
    pub fn pending_bytes(&self) -> usize { self.pending.len() }

    /// Write as much of the queue as the ring has room for now, whole frames only.
    pub fn flush(&mut self) {
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
        self.flush();   // a last try for what is still queued (what does not fit is lost: the reader was not keeping up)
        self.u32_at(if self.creator { H_CLOSED_CREATOR } else { H_CLOSED_ATTACHER }).store(1, Ordering::Release);
        unsafe { munmap(self.base as *mut c_void, self.len); }
        if let Some(n) = &self.name { unlink_region(n); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_length_written_into_the_ring_that_the_bytes_cannot_hold_is_an_error_not_a_crash() {
        let p = format!("/gl-u-{}", std::process::id());
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
        let mut c = Shm::create(format!("{p}b"), 256).unwrap();
        let mut d = Shm::attach(format!("{p}b")).unwrap();
        let ring = c.tx();
        c.u64_at(ring + RING_HEAD).store(10_000, Ordering::Release);
        assert!(d.recv().unwrap_err().contains("corrupt"));
        let _ = (&mut a, &mut c);
    }
}
