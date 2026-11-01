//! A compact binary encoding of the black-serde data model.
//!
//! Layout: the magic bytes `BSRD`, a version byte (1), then one value. Each value is a tag byte
//! followed by its payload. Lengths and integers are LEB128 varints; signed integers are
//! zigzag-encoded first.
//!
//! | tag | value | payload |
//! | --- | --- | --- |
//! | 0 | null | none |
//! | 1 / 2 | false / true | none |
//! | 3 | int | zigzag varint |
//! | 4 | uint | varint |
//! | 5 | float | 8 bytes, little-endian IEEE 754 |
//! | 6 | string | varint length, UTF-8 bytes |
//! | 7 | bytes | varint length, raw bytes |
//! | 8 | sequence | varint count, values |
//! | 9 | map | varint count, (varint key length, UTF-8 key, value) pairs |

use crate::{Deserialize, Error, Format, Serialize, Value};

pub const MAGIC: &[u8; 4] = b"BSRD";
pub const VERSION: u8 = 1;
/// Nesting deeper than this is rejected, so hostile input can't overflow the stack.
pub const MAX_DEPTH: usize = 256;

const NULL: u8 = 0;
const FALSE: u8 = 1;
const TRUE: u8 = 2;
const INT: u8 = 3;
const UINT: u8 = 4;
const FLOAT: u8 = 5;
const STRING: u8 = 6;
const BYTES: u8 = 7;
const SEQ: u8 = 8;
const MAP: u8 = 9;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Binary;

impl Format for Binary {
    fn encode(&self, value: &Value) -> Vec<u8> {
        let mut out = Vec::with_capacity(64);
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        write_value(&mut out, value);
        out
    }

    fn decode(&self, input: &[u8]) -> Result<Value, Error> {
        if input.len() < 5 || &input[..4] != MAGIC {
            return Err(Error::new("not black-serde binary data (bad magic bytes)").at_offset(0));
        }
        if input[4] != VERSION {
            return Err(Error::new(format!("unsupported binary format version {}", input[4])).at_offset(4));
        }
        let mut r = Reader { src: input, pos: 5 };
        let v = r.value(0)?;
        if r.pos != input.len() {
            return Err(Error::new("trailing bytes after the value").at_offset(r.pos));
        }
        Ok(v)
    }
}

pub fn to_vec<T: Serialize + ?Sized>(value: &T) -> Vec<u8> {
    Binary.encode(&value.serialize())
}

pub fn from_slice<T: Deserialize>(input: &[u8]) -> Result<T, Error> {
    T::deserialize(&Binary.decode(input)?)
}

fn write_varint(out: &mut Vec<u8>, mut n: u64) {
    loop {
        let byte = (n & 0x7F) as u8;
        n >>= 7;
        if n == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn zigzag(i: i64) -> u64 {
    ((i << 1) ^ (i >> 63)) as u64
}

fn unzigzag(u: u64) -> i64 {
    ((u >> 1) as i64) ^ -((u & 1) as i64)
}

fn write_value(out: &mut Vec<u8>, v: &Value) {
    match v {
        Value::Null => out.push(NULL),
        Value::Bool(false) => out.push(FALSE),
        Value::Bool(true) => out.push(TRUE),
        Value::Int(i) => {
            out.push(INT);
            write_varint(out, zigzag(*i));
        }
        Value::UInt(u) => {
            out.push(UINT);
            write_varint(out, *u);
        }
        Value::Float(f) => {
            out.push(FLOAT);
            out.extend_from_slice(&f.to_le_bytes());
        }
        Value::String(s) => {
            out.push(STRING);
            write_varint(out, s.len() as u64);
            out.extend_from_slice(s.as_bytes());
        }
        Value::Bytes(b) => {
            out.push(BYTES);
            write_varint(out, b.len() as u64);
            out.extend_from_slice(b);
        }
        Value::Seq(items) => {
            out.push(SEQ);
            write_varint(out, items.len() as u64);
            for item in items {
                write_value(out, item);
            }
        }
        Value::Map(pairs) => {
            out.push(MAP);
            write_varint(out, pairs.len() as u64);
            for (k, item) in pairs {
                write_varint(out, k.len() as u64);
                out.extend_from_slice(k.as_bytes());
                write_value(out, item);
            }
        }
    }
}

struct Reader<'a> {
    src: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn err(&self, msg: &str) -> Error {
        Error::new(msg).at_offset(self.pos)
    }

    fn byte(&mut self) -> Result<u8, Error> {
        let b = *self.src.get(self.pos).ok_or_else(|| self.err("unexpected end of data"))?;
        self.pos += 1;
        Ok(b)
    }

    fn take(&mut self, n: usize) -> Result<&[u8], Error> {
        if self.src.len() - self.pos < n {
            return Err(self.err("unexpected end of data"));
        }
        let s = &self.src[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn varint(&mut self) -> Result<u64, Error> {
        let start = self.pos;
        let mut n: u64 = 0;
        for shift in (0..64).step_by(7) {
            let b = self.byte()?;
            let bits = (b & 0x7F) as u64;
            if shift == 63 && bits > 1 {
                self.pos = start;
                return Err(self.err("varint overflows 64 bits"));
            }
            n |= bits << shift;
            if b & 0x80 == 0 {
                return Ok(n);
            }
        }
        self.pos = start;
        Err(self.err("varint is too long"))
    }

    /// A length or count; never more than the bytes left, since every item takes at least one.
    fn len(&mut self) -> Result<usize, Error> {
        let start = self.pos;
        let n = self.varint()?;
        let left = (self.src.len() - self.pos) as u64;
        if n > left {
            self.pos = start;
            return Err(self.err("length is longer than the remaining data"));
        }
        Ok(n as usize)
    }

    fn string(&mut self) -> Result<String, Error> {
        let n = self.len()?;
        let start = self.pos;
        let bytes = self.take(n)?;
        String::from_utf8(bytes.to_vec()).map_err(|e| Error::new("string is not valid UTF-8").at_offset(start + e.utf8_error().valid_up_to()))
    }

    fn value(&mut self, depth: usize) -> Result<Value, Error> {
        if depth > MAX_DEPTH {
            return Err(self.err("nested too deeply"));
        }
        let tag_pos = self.pos;
        Ok(match self.byte()? {
            NULL => Value::Null,
            FALSE => Value::Bool(false),
            TRUE => Value::Bool(true),
            INT => Value::Int(unzigzag(self.varint()?)),
            UINT => Value::UInt(self.varint()?),
            FLOAT => Value::Float(f64::from_le_bytes(self.take(8)?.try_into().expect("8 bytes"))),
            STRING => Value::String(self.string()?),
            BYTES => {
                let n = self.len()?;
                Value::Bytes(self.take(n)?.to_vec())
            }
            SEQ => {
                let n = self.len()?;
                let mut items = Vec::with_capacity(n);
                for _ in 0..n {
                    items.push(self.value(depth + 1)?);
                }
                Value::Seq(items)
            }
            MAP => {
                let n = self.len()?;
                let mut pairs = Vec::with_capacity(n);
                for _ in 0..n {
                    let k = self.string()?;
                    pairs.push((k, self.value(depth + 1)?));
                }
                Value::Map(pairs)
            }
            tag => {
                self.pos = tag_pos;
                return Err(self.err(&format!("unknown tag {tag}")));
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every_kind() -> Value {
        Value::map([
            ("null", Value::Null),
            ("bools", Value::Seq(vec![Value::Bool(true), Value::Bool(false)])),
            ("ints", Value::Seq(vec![Value::Int(0), Value::Int(-1), Value::Int(i64::MIN), Value::Int(i64::MAX)])),
            ("uint", Value::UInt(u64::MAX)),
            ("floats", Value::Seq(vec![Value::Float(1.5), Value::Float(-0.0), Value::Float(f64::INFINITY)])),
            ("string", Value::from("h\u{e9}llo \u{1F600}")),
            ("bytes", Value::Bytes(vec![0, 1, 2, 255])),
            ("empty", Value::Map(vec![])),
        ])
    }

    #[test]
    fn round_trip() {
        let v = every_kind();
        assert_eq!(Binary.decode(&Binary.encode(&v)).unwrap(), v);
        // NaN never equals itself, so check it separately.
        let nan = Binary.decode(&Binary.encode(&Value::Float(f64::NAN))).unwrap();
        assert!(matches!(nan, Value::Float(f) if f.is_nan()));
    }

    #[test]
    fn varints_and_zigzag() {
        for i in [0, 1, -1, 63, -64, 64, i64::MAX, i64::MIN] {
            assert_eq!(unzigzag(zigzag(i)), i);
        }
        let mut out = Vec::new();
        write_varint(&mut out, 300);
        assert_eq!(out, [0xAC, 0x02]);
        let mut r = Reader { src: &[0xFF; 11], pos: 0 };
        assert!(r.varint().is_err());
    }

    #[test]
    fn compact() {
        assert_eq!(Binary.encode(&Value::Int(5)), b"BSRD\x01\x03\x0A");
    }

    #[test]
    fn rejects_bad_input() {
        let good = Binary.encode(&every_kind());
        assert!(Binary.decode(b"JSON{}").unwrap_err().to_string().contains("magic"));
        assert!(Binary.decode(b"BSRD\x09\x00").unwrap_err().to_string().contains("version"));
        for cut in 5..good.len() {
            assert!(Binary.decode(&good[..cut]).is_err(), "truncated at {cut} should fail");
        }
        let mut trailing = good.clone();
        trailing.push(0);
        assert!(Binary.decode(&trailing).is_err());
        // A huge count with no data behind it must fail fast, not allocate.
        assert!(Binary.decode(b"BSRD\x01\x08\xFF\xFF\xFF\xFF\x0F").is_err());
        assert!(Binary.decode(b"BSRD\x01\x06\x02\xFF\xFE").unwrap_err().to_string().contains("UTF-8"));
        assert!(Binary.decode(b"BSRD\x01\x63").unwrap_err().to_string().contains("unknown tag 99 at byte 5"));
    }

    #[test]
    fn depth_limit() {
        let mut deep = b"BSRD\x01".to_vec();
        for _ in 0..MAX_DEPTH + 2 {
            deep.extend_from_slice(&[SEQ, 1]);
        }
        deep.push(NULL);
        assert!(Binary.decode(&deep).unwrap_err().to_string().contains("nested too deeply"));
    }
}
