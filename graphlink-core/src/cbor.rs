//! The CBOR subset that JSON can hold (docs/shared-memory.md, section 3): UTF-8 text, integers, floats, booleans, null, arrays and maps with text keys, all of definite
//! length. `encode` writes the shortest form of an integer, a float always as 64-bit, and a map's keys in the order the map holds them. `decode` reads any float width, keeps
//! the order the keys came in, and refuses everything else (tags, byte strings, simple values, indefinite lengths, NaN, infinities, duplicate keys), a nesting deeper than the
//! limit it is given, and a length the remaining bytes cannot hold (before it allocates anything for it). Plain functions over bytes; no I/O.
use serde_json::{Map, Number, Value as J};

fn head(out: &mut Vec<u8>, major: u8, n: u64) {
    let m = major << 5;
    if n < 24 { out.push(m | n as u8); }
    else if n <= 0xff { out.push(m | 24); out.push(n as u8); }
    else if n <= 0xffff { out.push(m | 25); out.extend_from_slice(&(n as u16).to_be_bytes()); }
    else if n <= 0xffff_ffff { out.push(m | 26); out.extend_from_slice(&(n as u32).to_be_bytes()); }
    else { out.push(m | 27); out.extend_from_slice(&n.to_be_bytes()); }
}

/// A value as CBOR.
pub fn encode(v: &J) -> Vec<u8> {
    let mut out = vec![];
    put(&mut out, v);
    out
}

fn put(out: &mut Vec<u8>, v: &J) {
    match v {
        J::Null => out.push(0xf6),
        J::Bool(false) => out.push(0xf4),
        J::Bool(true) => out.push(0xf5),
        J::Number(n) => {
            if let Some(u) = n.as_u64() { head(out, 0, u); }
            else if let Some(i) = n.as_i64() { head(out, 1, (-1 - i) as u64); }
            else { out.push(0xfb); out.extend_from_slice(&n.as_f64().unwrap_or(0.0).to_bits().to_be_bytes()); }
        }
        J::String(s) => { head(out, 3, s.len() as u64); out.extend_from_slice(s.as_bytes()); }
        J::Array(a) => { head(out, 4, a.len() as u64); for x in a { put(out, x); } }
        J::Object(m) => { head(out, 5, m.len() as u64); for (k, x) in m { head(out, 3, k.len() as u64); out.extend_from_slice(k.as_bytes()); put(out, x); } }
    }
}

/// CBOR as a value. `max_depth` is how many arrays and maps may be nested.
pub fn decode(bytes: &[u8], max_depth: usize) -> Result<J, String> {
    let mut r = Reader { b: bytes, pos: 0 };
    let v = r.item(max_depth)?;
    if r.pos != bytes.len() { return Err(format!("trailing bytes after the value ({} left)", bytes.len() - r.pos)); }
    Ok(v)
}

struct Reader<'a> { b: &'a [u8], pos: usize }

fn half(h: u16) -> f64 {
    let (sign, exp, mant) = (h >> 15, ((h >> 10) & 0x1f) as i32, (h & 0x3ff) as f64);
    let v = match exp { 0 => mant * 2f64.powi(-24), 31 => if mant == 0.0 { f64::INFINITY } else { f64::NAN }, _ => (1024.0 + mant) * 2f64.powi(exp - 25) };
    if sign == 1 { -v } else { v }
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if n > self.b.len() - self.pos { return Err("truncated: the bytes end inside a value".into()); }
        let s = &self.b[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn arg(&mut self, info: u8) -> Result<u64, String> {
        Ok(match info {
            0..=23 => info as u64,
            24 => self.take(1)?[0] as u64,
            25 => u16::from_be_bytes(self.take(2)?.try_into().unwrap()) as u64,
            26 => u32::from_be_bytes(self.take(4)?.try_into().unwrap()) as u64,
            27 => u64::from_be_bytes(self.take(8)?.try_into().unwrap()),
            31 => return Err("indefinite length is not allowed".into()),
            _ => return Err(format!("reserved additional information {info}")),
        })
    }

    fn float(f: f64) -> Result<J, String> {
        if f.is_nan() { return Err("NaN is not allowed".into()); }
        if f.is_infinite() { return Err("infinity is not allowed".into()); }
        Ok(J::Number(Number::from_f64(f).ok_or("a float JSON cannot hold")?))
    }

    fn item(&mut self, levels: usize) -> Result<J, String> {
        let first = self.take(1)?[0];
        let (major, info) = (first >> 5, first & 31);
        match major {
            0 => Ok(J::Number(self.arg(info)?.into())),
            1 => {
                let n = self.arg(info)?;
                if n > i64::MAX as u64 { return Err("a negative integer out of the range JSON numbers hold here".into()); }
                Ok(J::Number((-1 - n as i64).into()))
            }
            2 => Err("a byte string is not allowed".into()),
            3 => {
                let len = self.arg(info)?;
                let len = usize::try_from(len).map_err(|_| "truncated: a length beyond the bytes".to_string())?;
                let s = self.take(len)?;
                Ok(J::String(std::str::from_utf8(s).map_err(|_| "text that is not valid UTF-8".to_string())?.to_string()))
            }
            4 => {
                let len = self.arg(info)?;
                if levels == 0 { return Err("nested too deep".into()); }
                if len > (self.b.len() - self.pos) as u64 { return Err("truncated: more array items than bytes".into()); }
                let mut a = Vec::with_capacity(len as usize);
                for _ in 0..len { a.push(self.item(levels - 1)?); }
                Ok(J::Array(a))
            }
            5 => {
                let len = self.arg(info)?;
                if levels == 0 { return Err("nested too deep".into()); }
                if len > (self.b.len() - self.pos) as u64 / 2 { return Err("truncated: more map entries than bytes".into()); }
                let mut m = Map::new();
                for _ in 0..len {
                    let k = match self.item(levels - 1)? { J::String(s) => s, _ => return Err("a map key must be a text string".into()) };
                    let v = self.item(levels - 1)?;
                    if m.insert(k.clone(), v).is_some() { return Err(format!("duplicate map key {k:?}")); }
                }
                Ok(J::Object(m))
            }
            6 => Err("a tag is not allowed".into()),
            _ => match info {
                20 => Ok(J::Bool(false)),
                21 => Ok(J::Bool(true)),
                22 => Ok(J::Null),
                25 => Self::float(half(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))),
                26 => Self::float(f32::from_be_bytes(self.take(4)?.try_into().unwrap()) as f64),
                27 => Self::float(f64::from_be_bytes(self.take(8)?.try_into().unwrap())),
                31 => Err("a break outside an indefinite length is not allowed".into()),
                _ => Err("a simple value other than false, true and null is not allowed".into()),
            },
        }
    }
}
