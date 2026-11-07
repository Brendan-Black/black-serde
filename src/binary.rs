//! A compact binary encoding of the black-serde data model.
//!
//! Layout: magic bytes (`BSRD` by default), a version byte (2), then one value. Each value is a
//! tag byte followed by its payload. Lengths and integers are LEB128 varints; signed integers
//! are zigzag-encoded first.
//!
//! | tag (default) | value | payload |
//! | --- | --- | --- |
//! | 0 | null | none |
//! | 1 / 2 | false / true | none |
//! | 3 | int | zigzag varint |
//! | 4 | uint | varint |
//! | 5 | float | 8 bytes, little-endian IEEE 754 |
//! | 6 | string | varint length, UTF-8 bytes |
//! | 7 | bytes | varint length, raw bytes |
//! | 8 | sequence | varint count, values |
//! | 9 | map | varint count, (key value, value) pairs |
//! | 10 | tagged | varint length, UTF-8 tag, value |
//!
//! Version 1 data, where map keys were bare strings (varint length and UTF-8 bytes) and there
//! were no tagged values, still decodes.
//!
//! The magic bytes and every tag byte come from [`BinaryTokens`] and can be changed.

use crate::{Deserialize, Error, Format, Limits, Serialize, Value, walk};

pub const MAGIC: &[u8; 4] = b"BSRD";
pub const VERSION: u8 = 2;
/// The default limit on nesting; see [`Limits`]. Decoding and encoding don't recurse, so this
/// is a policy, not a protection for the stack.
pub const MAX_DEPTH: usize = 256;

/// The most items a container's count may reserve room for up front. A count is checked
/// against the bytes left, but every nested container could claim the same bytes, so trusting
/// counts fully would let a small input reserve gigabytes. Larger containers grow as read.
const PREALLOCATE: usize = 1024;

/// The magic bytes and the tag byte of each kind of value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BinaryTokens {
    pub magic: &'static [u8],
    pub null: u8,
    pub bool_false: u8,
    pub bool_true: u8,
    pub int: u8,
    pub uint: u8,
    pub float: u8,
    pub string: u8,
    pub bytes: u8,
    pub seq: u8,
    pub map: u8,
    pub tagged: u8,
}

impl BinaryTokens {
    pub const STANDARD: BinaryTokens = BinaryTokens {
        magic: MAGIC,
        null: 0,
        bool_false: 1,
        bool_true: 2,
        int: 3,
        uint: 4,
        float: 5,
        string: 6,
        bytes: 7,
        seq: 8,
        map: 9,
        tagged: 10,
    };

    fn tags(&self) -> [(&'static str, u8); 11] {
        [
            ("null", self.null),
            ("bool_false", self.bool_false),
            ("bool_true", self.bool_true),
            ("int", self.int),
            ("uint", self.uint),
            ("float", self.float),
            ("string", self.string),
            ("bytes", self.bytes),
            ("seq", self.seq),
            ("map", self.map),
            ("tagged", self.tagged),
        ]
    }

    /// Check that the set can be read back unambiguously: every tag byte is different.
    pub fn check(&self) -> Result<(), Error> {
        if self.magic.is_empty() {
            return Err(Error::new("binary token magic can't be empty"));
        }
        let tags = self.tags();
        for (i, (na, a)) in tags.iter().enumerate() {
            if let Some((nb, _)) = tags[i + 1..].iter().find(|(_, b)| b == a) {
                return Err(Error::new(format!("binary tokens {na} and {nb} are both {a}, so they can't be told apart")));
            }
        }
        Ok(())
    }
}

impl Default for BinaryTokens {
    fn default() -> Self {
        BinaryTokens::STANDARD
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Binary {
    tokens: BinaryTokens,
    limits: Option<Limits>,
}

impl Binary {
    pub fn new() -> Self {
        Binary::default()
    }

    /// Use a different token set, after checking it.
    pub fn with_tokens(mut self, tokens: BinaryTokens) -> Result<Self, Error> {
        tokens.check()?;
        self.tokens = tokens;
        Ok(self)
    }

    /// Use these limits instead of the defaults ([`Limits::BINARY`]).
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = Some(limits);
        self
    }

    /// The limits encoding and decoding use: those set with
    /// [`with_limits`](Binary::with_limits), or else [`Limits::BINARY`].
    pub fn limits(&self) -> Limits {
        self.limits.unwrap_or(Limits::BINARY)
    }

    pub fn tokens(&self) -> &BinaryTokens {
        &self.tokens
    }
}

impl Format for Binary {
    /// Fails only if the value is nested deeper than `max_depth`: binary represents the
    /// whole data model.
    fn encode(&self, value: &Value) -> Result<Vec<u8>, Error> {
        let t = &self.tokens;
        let max_depth = self.limits().max_depth;
        let mut out = Vec::with_capacity(64);
        out.extend_from_slice(t.magic);
        out.push(VERSION);
        // The layout is the values in depth-first order, each container followed by its
        // contents, so walking the tree in that order and writing each header is the encoding.
        walk(value, max_depth, |v| {
            write_header(&mut out, v, t);
            Ok(())
        })?;
        Ok(out)
    }

    fn decode(&self, input: &[u8]) -> Result<Value, Error> {
        let limits = self.limits();
        limits.check_input(input.len())?;
        let t = &self.tokens;
        let n = t.magic.len();
        if input.len() <= n || &input[..n] != t.magic {
            return Err(Error::new("not black-serde binary data (bad magic bytes)").at_offset(0));
        }
        let version = input[n];
        if version != 1 && version != VERSION {
            return Err(Error::new(format!("unsupported binary format version {version}")).at_offset(n));
        }
        let mut r = Reader { src: input, pos: n + 1, t, version, max_depth: limits.max_depth };
        let v = r.value()?;
        if r.pos != input.len() {
            return Err(Error::new("trailing bytes after the value").at_offset(r.pos));
        }
        Ok(v)
    }
}

pub fn to_vec<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, Error> {
    Binary::default().encode(&value.serialize())
}

pub fn from_slice<T: Deserialize>(input: &[u8]) -> Result<T, Error> {
    T::deserialize(&Binary::default().decode(input)?)
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

fn write_str(out: &mut Vec<u8>, s: &str) {
    write_varint(out, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

/// The bytes written for `v` itself: a scalar in full, or a container's tag and count (its
/// contents follow as the next values).
fn write_header(out: &mut Vec<u8>, v: &Value, t: &BinaryTokens) {
    match v {
        Value::Null => out.push(t.null),
        Value::Bool(false) => out.push(t.bool_false),
        Value::Bool(true) => out.push(t.bool_true),
        Value::Int(i) => {
            out.push(t.int);
            write_varint(out, zigzag(*i));
        }
        Value::UInt(u) => {
            out.push(t.uint);
            write_varint(out, *u);
        }
        Value::Float(f) => {
            out.push(t.float);
            out.extend_from_slice(&f.to_le_bytes());
        }
        Value::String(s) => {
            out.push(t.string);
            write_str(out, s);
        }
        Value::Bytes(b) => {
            out.push(t.bytes);
            write_varint(out, b.len() as u64);
            out.extend_from_slice(b);
        }
        Value::Seq(items) => {
            out.push(t.seq);
            write_varint(out, items.len() as u64);
        }
        Value::Map(pairs) => {
            out.push(t.map);
            write_varint(out, pairs.len() as u64);
        }
        Value::Tagged(tag, _) => {
            out.push(t.tagged);
            write_str(out, tag);
        }
    }
}

struct Reader<'a> {
    src: &'a [u8],
    pos: usize,
    t: &'a BinaryTokens,
    version: u8,
    max_depth: usize,
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

    /// One complete value. Open containers are kept on a heap stack rather than by recursing,
    /// so nesting is limited only by `max_depth`. Depth counts sequences and maps; tags don't
    /// add to it.
    fn value(&mut self) -> Result<Value, Error> {
        enum Open {
            Seq(Vec<Value>, usize),
            /// Pairs so far, how many there are, and a key waiting for its value.
            Map(Vec<(Value, Value)>, usize, Option<Value>),
            Tagged(String),
        }
        let t = self.t;
        let mut stack: Vec<Open> = Vec::new();
        let mut depth = 0;
        loop {
            if depth > self.max_depth {
                return Err(self.too_deep());
            }
            let tag_pos = self.pos;
            let tag = self.byte()?;
            let mut v = if tag == t.seq {
                let n = self.len()?;
                if n > 0 {
                    stack.push(Open::Seq(Vec::with_capacity(n.min(PREALLOCATE)), n));
                    depth += 1;
                    continue;
                }
                Value::Seq(Vec::new())
            } else if tag == t.map {
                let n = self.len()?;
                if n > 0 {
                    // Version 1 keys are bare strings, read here rather than as values.
                    let key = if self.version == 1 { Some(self.string_value()?) } else { None };
                    stack.push(Open::Map(Vec::with_capacity(n.min(PREALLOCATE)), n, key));
                    depth += 1;
                    continue;
                }
                Value::Map(Vec::new())
            } else if tag == t.tagged && self.version >= 2 {
                let name = self.string()?;
                stack.push(Open::Tagged(name));
                continue;
            } else {
                self.scalar(tag, tag_pos)?
            };
            // Add it to the innermost open container, closing containers that are complete.
            loop {
                match stack.last_mut() {
                    None => return Ok(v),
                    Some(Open::Seq(items, n)) => {
                        items.push(v);
                        if items.len() < *n {
                            break;
                        }
                        let Some(Open::Seq(items, _)) = stack.pop() else { unreachable!() };
                        depth -= 1;
                        v = Value::Seq(items);
                    }
                    Some(Open::Map(pairs, n, key)) => match key.take() {
                        None => {
                            *key = Some(v);
                            break;
                        }
                        Some(k) => {
                            pairs.push((k, v));
                            if pairs.len() < *n {
                                if self.version == 1 {
                                    *key = Some(self.string_value()?);
                                }
                                break;
                            }
                            let Some(Open::Map(pairs, ..)) = stack.pop() else { unreachable!() };
                            depth -= 1;
                            v = Value::Map(pairs);
                        }
                    },
                    Some(Open::Tagged(_)) => {
                        let Some(Open::Tagged(name)) = stack.pop() else { unreachable!() };
                        v = Value::Tagged(name, Box::new(v));
                    }
                }
            }
        }
    }

    #[inline(never)]
    fn string_value(&mut self) -> Result<Value, Error> {
        self.string().map(Value::String)
    }

    #[inline(never)]
    fn scalar(&mut self, tag: u8, tag_pos: usize) -> Result<Value, Error> {
        let t = self.t;
        Ok(match tag {
            _ if tag == t.null => Value::Null,
            _ if tag == t.bool_false => Value::Bool(false),
            _ if tag == t.bool_true => Value::Bool(true),
            _ if tag == t.int => Value::Int(unzigzag(self.varint()?)),
            _ if tag == t.uint => Value::from(self.varint()?),
            _ if tag == t.float => Value::Float(f64::from_le_bytes(self.take(8)?.try_into().expect("8 bytes"))),
            _ if tag == t.string => Value::String(self.string()?),
            _ if tag == t.bytes => {
                let n = self.len()?;
                Value::Bytes(self.take(n)?.to_vec())
            }
            _ => {
                self.pos = tag_pos;
                return Err(self.err(&format!("unknown tag {tag}")));
            }
        })
    }

    #[cold]
    #[inline(never)]
    fn too_deep(&self) -> Error {
        Limits { max_depth: self.max_depth, ..Limits::BINARY }.depth_error().at_offset(self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every_kind() -> Value {
        Value::Map(vec![
            (Value::from("null"), Value::Null),
            (Value::from("bools"), Value::Seq(vec![Value::Bool(true), Value::Bool(false)])),
            (Value::from("ints"), Value::Seq(vec![Value::Int(0), Value::Int(-1), Value::Int(i64::MIN), Value::Int(i64::MAX)])),
            (Value::from("uint"), Value::UInt(u64::MAX)),
            (Value::from("floats"), Value::Seq(vec![Value::Float(1.5), Value::Float(-0.0), Value::Float(f64::INFINITY)])),
            (Value::from("string"), Value::from("h\u{e9}llo \u{1F600}")),
            (Value::from("bytes"), Value::Bytes(vec![0, 1, 2, 255])),
            (Value::from("empty"), Value::Map(vec![])),
            (Value::Seq(vec![Value::Int(1), Value::Null]), Value::tagged("!point", Value::map([("x", Value::Int(1))]))),
            (Value::Int(7), Value::from("int key")),
        ])
    }

    #[test]
    fn round_trip() {
        let b = Binary::new();
        let v = every_kind();
        assert_eq!(b.decode(&b.encode(&v).unwrap()).unwrap(), v);
        // NaN never equals itself, so check it separately.
        let nan = b.decode(&b.encode(&Value::Float(f64::NAN)).unwrap()).unwrap();
        assert!(matches!(nan, Value::Float(f) if f.is_nan()));
    }

    #[test]
    fn reads_version_1() {
        // {"a": 5} as version 1 wrote it: the key is a bare string.
        let v1 = b"BSRD\x01\x09\x01\x01a\x03\x0A";
        assert_eq!(Binary::new().decode(v1).unwrap(), Value::map([("a", Value::Int(5))]));
        // Version 1 had no tagged values.
        assert!(Binary::new().decode(b"BSRD\x01\x0A\x01t\x00").is_err());
    }

    #[test]
    fn varints_and_zigzag() {
        for i in [0, 1, -1, 63, -64, 64, i64::MAX, i64::MIN] {
            assert_eq!(unzigzag(zigzag(i)), i);
        }
        let mut out = Vec::new();
        write_varint(&mut out, 300);
        assert_eq!(out, [0xAC, 0x02]);
        let t = BinaryTokens::STANDARD;
        let mut r = Reader { src: &[0xFF; 11], pos: 0, t: &t, version: VERSION, max_depth: MAX_DEPTH };
        assert!(r.varint().is_err());
    }

    #[test]
    fn compact() {
        assert_eq!(Binary::new().encode(&Value::Int(5)).unwrap(), b"BSRD\x02\x03\x0A");
    }

    #[test]
    fn swapped_tokens() {
        let tokens = BinaryTokens { magic: b"MINE", int: 0xF0, map: 0x7B, string: b's', ..BinaryTokens::STANDARD };
        let b = Binary::new().with_tokens(tokens).unwrap();
        assert_eq!(b.encode(&Value::Int(5)).unwrap(), b"MINE\x02\xF0\x0A");
        let v = every_kind();
        let bytes = b.encode(&v).unwrap();
        assert_eq!(b.decode(&bytes).unwrap(), v);
        // The standard reader doesn't accept it, and vice versa.
        assert!(Binary::new().decode(&bytes).is_err());
        assert!(b.decode(&Binary::new().encode(&v).unwrap()).is_err());

        let clash = BinaryTokens { seq: 3, ..BinaryTokens::STANDARD };
        let e = clash.check().unwrap_err().to_string();
        assert!(e.contains("int") && e.contains("seq"), "{e}");
        assert!(BinaryTokens { magic: b"", ..BinaryTokens::STANDARD }.check().is_err());
    }

    #[test]
    fn rejects_bad_input() {
        let b = Binary::new();
        let good = b.encode(&every_kind()).unwrap();
        assert!(b.decode(b"JSON{}").unwrap_err().to_string().contains("magic"));
        assert!(b.decode(b"BSRD\x09\x00").unwrap_err().to_string().contains("version"));
        for cut in 5..good.len() {
            assert!(b.decode(&good[..cut]).is_err(), "truncated at {cut} should fail");
        }
        let mut trailing = good.clone();
        trailing.push(0);
        assert!(b.decode(&trailing).is_err());
        // A huge count with no data behind it must fail fast, not allocate.
        assert!(b.decode(b"BSRD\x02\x08\xFF\xFF\xFF\xFF\x0F").is_err());
        assert!(b.decode(b"BSRD\x02\x06\x02\xFF\xFE").unwrap_err().to_string().contains("UTF-8"));
        assert!(b.decode(b"BSRD\x02\x63").unwrap_err().to_string().contains("unknown tag 99 at byte 5"));
    }

    #[test]
    fn depth_limit() {
        let mut deep = b"BSRD\x02".to_vec();
        for _ in 0..MAX_DEPTH + 2 {
            deep.extend_from_slice(&[8, 1]);
        }
        deep.push(0);
        assert!(Binary::new().decode(&deep).unwrap_err().to_string().contains("nested too deeply"));
        assert_eq!(Binary::new().decode(&deep).unwrap_err().limit(), Some(crate::Limit::Depth));
        let small = Binary::new().with_limits(Limits { max_input_bytes: 6, ..Limits::BINARY });
        assert!(small.decode(b"BSRD\x02\x00").is_ok());
        assert_eq!(small.decode(b"BSRD\x02\x03\x0A").unwrap_err().limit(), Some(crate::Limit::InputSize));
    }
}
