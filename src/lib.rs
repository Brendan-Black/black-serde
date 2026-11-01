//! A small, dependency-free serialization framework.
//!
//! Three pieces, kept apart so each can change without the others:
//!
//! - [`Value`]: a format-neutral data model (null, bool, integers, floats, strings, bytes,
//!   sequences, maps with any keys, tagged values).
//! - [`Serialize`] / [`Deserialize`]: how a Rust type maps to and from a [`Value`].
//! - [`Format`]: how a [`Value`] maps to and from bytes. [`json`], [`yaml`] and [`binary`] are
//!   the formats included; any type that implements the traits works with all of them.
//!
//! Every format's syntax comes from a token set ([`json::JsonTokens`], [`yaml::YamlTokens`],
//! [`binary::BinaryTokens`]) that can be changed one token at a time. Each set is checked
//! before use, so no two tokens can be confused with each other.
//!
//! ```
//! use black_serde::{Deserialize, Error, Fields, MapBuilder, Serialize, Value};
//!
//! #[derive(Debug, PartialEq)]
//! struct Point { x: i32, y: i32, label: Option<String> }
//!
//! impl Serialize for Point {
//!     fn serialize(&self) -> Value {
//!         MapBuilder::new().field("x", &self.x).field("y", &self.y).field("label", &self.label).build()
//!     }
//! }
//!
//! impl Deserialize for Point {
//!     fn deserialize(v: &Value) -> Result<Self, Error> {
//!         let f = Fields::new(v)?;
//!         Ok(Point { x: f.req("x")?, y: f.req("y")?, label: f.opt("label")? })
//!     }
//! }
//!
//! let p = Point { x: 3, y: -4, label: None };
//! let text = black_serde::json::to_string(&p).unwrap();
//! assert_eq!(text, r#"{"x":3,"y":-4,"label":null}"#);
//! assert_eq!(black_serde::json::from_str::<Point>(&text).unwrap(), p);
//!
//! let bytes = black_serde::binary::to_vec(&p).unwrap();
//! assert_eq!(black_serde::binary::from_slice::<Point>(&bytes).unwrap(), p);
//! ```

mod error;
mod limits;
mod tokens;
mod value;

pub mod binary;
pub mod json;
pub mod yaml;

pub use error::{Error, ErrorKind, Limit, Position, Segment};
pub use limits::Limits;
pub use value::Value;

pub(crate) use value::{KeyIndex, hash_value, same, walk};

use std::collections::{BTreeMap, HashMap};
use std::hash::{BuildHasher, Hash};

/// A type that can be turned into a [`Value`].
pub trait Serialize {
    fn serialize(&self) -> Value;
}

/// A type that can be built from a [`Value`].
pub trait Deserialize: Sized {
    fn deserialize(value: &Value) -> Result<Self, Error>;
}

/// A way of writing a [`Value`] as bytes and reading it back.
///
/// Encoding fails rather than silently changing the data: when a value can't be represented
/// ([`ErrorKind::Unrepresentable`], unless the format's lossy options allow it) or is nested
/// deeper than the format's `max_depth` ([`Limit::Depth`]). The error's path says where.
pub trait Format {
    fn encode(&self, value: &Value) -> Result<Vec<u8>, Error>;
    fn decode(&self, input: &[u8]) -> Result<Value, Error>;
}

/// Serialize `value` with any format.
pub fn to_bytes<F: Format, T: Serialize + ?Sized>(format: &F, value: &T) -> Result<Vec<u8>, Error> {
    format.encode(&value.serialize())
}

/// Deserialize a `T` from bytes in any format.
pub fn from_bytes<F: Format, T: Deserialize>(format: &F, input: &[u8]) -> Result<T, Error> {
    T::deserialize(&format.decode(input)?)
}

// ---------- helpers for writing impls ----------

/// Builds a map [`Value`] field by field, for `Serialize` impls.
#[derive(Default)]
pub struct MapBuilder(Vec<(Value, Value)>);

impl MapBuilder {
    pub fn new() -> Self {
        MapBuilder(Vec::new())
    }

    pub fn field<T: Serialize + ?Sized>(mut self, name: &str, value: &T) -> Self {
        self.0.push((Value::from(name), value.serialize()));
        self
    }

    /// Add the field only if it is `Some`, so absent values are omitted rather than null.
    pub fn field_if<T: Serialize>(self, name: &str, value: &Option<T>) -> Self {
        match value {
            Some(v) => self.field(name, v),
            None => self,
        }
    }

    pub fn value(mut self, name: &str, value: Value) -> Self {
        self.0.push((Value::from(name), value));
        self
    }

    pub fn build(self) -> Value {
        Value::Map(self.0)
    }
}

/// Reads fields out of a map [`Value`], for `Deserialize` impls. Errors carry the field name.
pub struct Fields<'a>(&'a Value);

impl<'a> Fields<'a> {
    pub fn new(value: &'a Value) -> Result<Self, Error> {
        match value.untagged() {
            v @ Value::Map(_) => Ok(Fields(v)),
            other => Err(Error::expected("a map", other)),
        }
    }

    /// The raw value of a field, if present.
    pub fn raw(&self, name: &str) -> Option<&'a Value> {
        self.0.get(name)
    }

    /// A required field.
    pub fn req<T: Deserialize>(&self, name: &str) -> Result<T, Error> {
        let v = self.raw(name).ok_or_else(|| Error::missing_field(name))?;
        T::deserialize(v).map_err(|e| e.in_key(name))
    }

    /// An optional field: missing or null gives `None`.
    pub fn opt<T: Deserialize>(&self, name: &str) -> Result<Option<T>, Error> {
        match self.raw(name) {
            None => Ok(None),
            Some(v) if v.is_null() => Ok(None),
            Some(v) => T::deserialize(v).map(Some).map_err(|e| e.in_key(name)),
        }
    }

    /// A field with a default when missing or null.
    pub fn or<T: Deserialize>(&self, name: &str, default: T) -> Result<T, Error> {
        Ok(self.opt(name)?.unwrap_or(default))
    }
}

// ---------- impls for standard types ----------

impl Serialize for Value {
    fn serialize(&self) -> Value {
        self.clone()
    }
}

impl Deserialize for Value {
    fn deserialize(v: &Value) -> Result<Self, Error> {
        Ok(v.clone())
    }
}

impl Serialize for bool {
    fn serialize(&self) -> Value {
        Value::Bool(*self)
    }
}

impl Deserialize for bool {
    fn deserialize(v: &Value) -> Result<Self, Error> {
        v.as_bool().ok_or_else(|| Error::expected("a boolean", v))
    }
}

macro_rules! ints {
    ($($t:ty),*) => {$(
        impl Serialize for $t {
            fn serialize(&self) -> Value { Value::from(*self) }
        }
        impl Deserialize for $t {
            fn deserialize(v: &Value) -> Result<Self, Error> {
                let out_of_range = || Error::new(format!("integer out of range for {}", stringify!($t)));
                match v.untagged() {
                    Value::Int(_) | Value::UInt(_) | Value::Float(_) => {}
                    other => return Err(Error::expected("an integer", other)),
                }
                if let Some(i) = v.as_i64() {
                    return <$t>::try_from(i).map_err(|_| out_of_range());
                }
                if let Some(u) = v.as_u64() {
                    return <$t>::try_from(u).map_err(|_| out_of_range());
                }
                match v.untagged() {
                    Value::Float(f) if f.fract() != 0.0 => Err(Error::new(format!("expected an integer, found {f}"))),
                    _ => Err(out_of_range()),
                }
            }
        }
    )*};
}
ints!(i8, i16, i32, i64, isize, u8, u16, u32, u64, usize);

impl Serialize for f64 {
    fn serialize(&self) -> Value {
        Value::Float(*self)
    }
}

impl Deserialize for f64 {
    fn deserialize(v: &Value) -> Result<Self, Error> {
        v.as_f64().ok_or_else(|| Error::expected("a number", v))
    }
}

impl Serialize for f32 {
    fn serialize(&self) -> Value {
        Value::Float(*self as f64)
    }
}

impl Deserialize for f32 {
    fn deserialize(v: &Value) -> Result<Self, Error> {
        f64::deserialize(v).map(|f| f as f32)
    }
}

impl Serialize for char {
    fn serialize(&self) -> Value {
        Value::String(self.to_string())
    }
}

impl Deserialize for char {
    fn deserialize(v: &Value) -> Result<Self, Error> {
        let s = v.as_str().ok_or_else(|| Error::expected("a single-character string", v))?;
        let mut chars = s.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => Ok(c),
            _ => Err(Error::new(format!("expected a single character, found {} characters", s.chars().count()))),
        }
    }
}

impl Serialize for str {
    fn serialize(&self) -> Value {
        Value::String(self.to_string())
    }
}

impl Serialize for String {
    fn serialize(&self) -> Value {
        Value::String(self.clone())
    }
}

impl Deserialize for String {
    fn deserialize(v: &Value) -> Result<Self, Error> {
        v.as_str().map(str::to_string).ok_or_else(|| Error::expected("a string", v))
    }
}

impl Serialize for () {
    fn serialize(&self) -> Value {
        Value::Null
    }
}

impl Deserialize for () {
    fn deserialize(v: &Value) -> Result<Self, Error> {
        if v.is_null() { Ok(()) } else { Err(Error::expected("null", v)) }
    }
}

impl<T: Serialize + ?Sized> Serialize for &T {
    fn serialize(&self) -> Value {
        (**self).serialize()
    }
}

impl<T: Serialize + ?Sized> Serialize for Box<T> {
    fn serialize(&self) -> Value {
        (**self).serialize()
    }
}

impl<T: Deserialize> Deserialize for Box<T> {
    fn deserialize(v: &Value) -> Result<Self, Error> {
        T::deserialize(v).map(Box::new)
    }
}

impl<T: Serialize> Serialize for Option<T> {
    fn serialize(&self) -> Value {
        self.as_ref().map_or(Value::Null, Serialize::serialize)
    }
}

impl<T: Deserialize> Deserialize for Option<T> {
    fn deserialize(v: &Value) -> Result<Self, Error> {
        if v.is_null() { Ok(None) } else { T::deserialize(v).map(Some) }
    }
}

impl<T: Serialize> Serialize for [T] {
    fn serialize(&self) -> Value {
        Value::Seq(self.iter().map(Serialize::serialize).collect())
    }
}

impl<T: Serialize, const N: usize> Serialize for [T; N] {
    fn serialize(&self) -> Value {
        self.as_slice().serialize()
    }
}

impl<T: Serialize> Serialize for Vec<T> {
    fn serialize(&self) -> Value {
        self.as_slice().serialize()
    }
}

impl<T: Deserialize> Deserialize for Vec<T> {
    fn deserialize(v: &Value) -> Result<Self, Error> {
        match v.untagged() {
            Value::Seq(items) => {
                items.iter().enumerate().map(|(i, item)| T::deserialize(item).map_err(|e| e.in_index(i))).collect()
            }
            // Bytes decode into any integer sequence, so Vec<u8> round-trips through every format.
            Value::Bytes(bytes) => bytes
                .iter()
                .enumerate()
                .map(|(i, b)| T::deserialize(&Value::Int(*b as i64)).map_err(|e| e.in_index(i)))
                .collect(),
            other => Err(Error::expected("a sequence", other)),
        }
    }
}

/// A byte buffer that serializes as [`Value::Bytes`] (compact in binary, `!!binary` in YAML)
/// instead of as a sequence of integers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bytes(pub Vec<u8>);

impl Serialize for Bytes {
    fn serialize(&self) -> Value {
        Value::Bytes(self.0.clone())
    }
}

impl Deserialize for Bytes {
    fn deserialize(v: &Value) -> Result<Self, Error> {
        match v.untagged() {
            Value::Bytes(b) => Ok(Bytes(b.clone())),
            _ => Vec::<u8>::deserialize(v).map(Bytes),
        }
    }
}

/// Read a map key. Formats differ in what keys can be (JSON keys are always strings, YAML keys
/// are typed), so a key that doesn't fit `K` is retried as text, or text as a scalar:
/// `{"1": x}` reads into `HashMap<u32, _>`, and YAML `{1: x}` into `HashMap<String, _>`.
fn map_key<K: Deserialize>(key: &Value) -> Result<K, Error> {
    match K::deserialize(key) {
        Ok(k) => Ok(k),
        Err(e) => {
            let retry = match key.untagged() {
                Value::String(s) => match yaml::resolve_plain(s) {
                    Value::String(_) => return Err(e),
                    scalar => scalar,
                },
                Value::Seq(_) | Value::Map(_) | Value::Bytes(_) => return Err(e),
                scalar => Value::String(scalar.key_text()),
            };
            K::deserialize(&retry).map_err(|_| e)
        }
    }
}

fn map_entries<K: Deserialize, V: Deserialize>(v: &Value) -> impl Iterator<Item = Result<(K, V), Error>> + '_ {
    let pairs = v.as_map().map(|p| p.as_slice()).unwrap_or(&[]);
    pairs.iter().map(|(k, item)| {
        let path = k.key_text();
        let key = map_key(k).map_err(|e| e.in_key(&path))?;
        Ok((key, V::deserialize(item).map_err(|e| e.in_key(&path))?))
    })
}

impl<K: Serialize, V: Serialize> Serialize for BTreeMap<K, V> {
    fn serialize(&self) -> Value {
        Value::Map(self.iter().map(|(k, v)| (k.serialize(), v.serialize())).collect())
    }
}

impl<K: Deserialize + Ord, V: Deserialize> Deserialize for BTreeMap<K, V> {
    fn deserialize(v: &Value) -> Result<Self, Error> {
        if v.as_map().is_none() {
            return Err(Error::expected("a map", v));
        }
        map_entries(v).collect()
    }
}

/// Keys are written in sorted order so the output is the same on every run.
impl<K: Serialize + Ord, V: Serialize, S> Serialize for HashMap<K, V, S> {
    fn serialize(&self) -> Value {
        let mut pairs: Vec<_> = self.iter().collect();
        pairs.sort_by(|a, b| a.0.cmp(b.0));
        Value::Map(pairs.into_iter().map(|(k, v)| (k.serialize(), v.serialize())).collect())
    }
}

impl<K: Deserialize + Eq + Hash, V: Deserialize, S: BuildHasher + Default> Deserialize for HashMap<K, V, S> {
    fn deserialize(v: &Value) -> Result<Self, Error> {
        if v.as_map().is_none() {
            return Err(Error::expected("a map", v));
        }
        map_entries(v).collect()
    }
}

/// A value with a tag. YAML writes it as `!tag value` and binary keeps the tag; JSON writes only
/// the value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tagged<T> {
    pub tag: String,
    pub value: T,
}

impl<T: Serialize> Serialize for Tagged<T> {
    fn serialize(&self) -> Value {
        Value::tagged(self.tag.clone(), self.value.serialize())
    }
}

impl<T: Deserialize> Deserialize for Tagged<T> {
    fn deserialize(v: &Value) -> Result<Self, Error> {
        match v {
            Value::Tagged(tag, inner) => Ok(Tagged { tag: tag.clone(), value: T::deserialize(inner)? }),
            other => Err(Error::expected("a tagged value", other)),
        }
    }
}

macro_rules! tuples {
    ($($len:literal => ($($name:ident $idx:tt),+);)*) => {$(
        impl<$($name: Serialize),+> Serialize for ($($name,)+) {
            fn serialize(&self) -> Value {
                Value::Seq(vec![$(self.$idx.serialize()),+])
            }
        }
        impl<$($name: Deserialize),+> Deserialize for ($($name,)+) {
            fn deserialize(v: &Value) -> Result<Self, Error> {
                let items = v.as_seq().ok_or_else(|| Error::expected("a sequence", v))?;
                if items.len() != $len {
                    return Err(Error::new(format!("expected {} elements, found {}", $len, items.len())));
                }
                Ok(($($name::deserialize(&items[$idx]).map_err(|e| e.in_index($idx))?,)+))
            }
        }
    )*};
}
tuples! {
    1 => (A 0);
    2 => (A 0, B 1);
    3 => (A 0, B 1, C 2);
    4 => (A 0, B 1, C 2, D 3);
    5 => (A 0, B 1, C 2, D 3, E 4);
    6 => (A 0, B 1, C 2, D 3, E 4, F 5);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    struct Inner {
        id: u8,
        tags: Vec<String>,
    }

    impl Serialize for Inner {
        fn serialize(&self) -> Value {
            MapBuilder::new().field("id", &self.id).field("tags", &self.tags).build()
        }
    }

    impl Deserialize for Inner {
        fn deserialize(v: &Value) -> Result<Self, Error> {
            let f = Fields::new(v)?;
            Ok(Inner { id: f.req("id")?, tags: f.or("tags", Vec::new())? })
        }
    }

    #[derive(Debug, PartialEq)]
    struct Outer {
        name: String,
        items: Vec<Inner>,
        scale: f64,
        note: Option<String>,
    }

    impl Serialize for Outer {
        fn serialize(&self) -> Value {
            MapBuilder::new()
                .field("name", &self.name)
                .field("items", &self.items)
                .field("scale", &self.scale)
                .field_if("note", &self.note)
                .build()
        }
    }

    impl Deserialize for Outer {
        fn deserialize(v: &Value) -> Result<Self, Error> {
            let f = Fields::new(v)?;
            Ok(Outer { name: f.req("name")?, items: f.req("items")?, scale: f.req("scale")?, note: f.opt("note")? })
        }
    }

    fn sample() -> Outer {
        Outer {
            name: "demo".into(),
            items: vec![Inner { id: 1, tags: vec!["a".into()] }, Inner { id: 2, tags: vec![] }],
            scale: 1.5,
            note: None,
        }
    }

    #[test]
    fn same_type_through_every_format() {
        let s = sample();
        assert_eq!(json::from_str::<Outer>(&json::to_string(&s).unwrap()).unwrap(), s);
        assert_eq!(binary::from_slice::<Outer>(&binary::to_vec(&s).unwrap()).unwrap(), s);
        assert_eq!(yaml::from_str::<Outer>(&yaml::to_string(&s).unwrap()).unwrap(), s);
        assert_eq!(from_bytes::<_, Outer>(&json::Json::pretty(2), &to_bytes(&json::Json::pretty(2), &s).unwrap()).unwrap(), s);
    }

    #[test]
    fn errors_carry_the_path() {
        let e = json::from_str::<Outer>(r#"{"name":"x","scale":1,"items":[{"id":1},{"id":300}]}"#).unwrap_err();
        assert_eq!(e.to_string(), "items[1].id: integer out of range for u8");
        let e = json::from_str::<Outer>(r#"{"name":"x","scale":1,"items":[{"tags":[]}]}"#).unwrap_err();
        assert_eq!(e.to_string(), "items[0]: missing field \"id\"");
        let e = json::from_str::<Outer>(r#"{"name":5,"scale":1,"items":[]}"#).unwrap_err();
        assert_eq!(e.to_string(), "name: expected a string, found an integer");
    }

    #[test]
    fn optional_fields() {
        let s = sample();
        assert!(s.serialize().get("note").is_none());
        let with = Outer { note: Some("hi".into()), ..sample() };
        assert_eq!(json::from_str::<Outer>(&json::to_string(&with).unwrap()).unwrap(), with);
        // An explicit null is the same as missing.
        let v = json::from_str::<Outer>(r#"{"name":"x","scale":1,"items":[],"note":null}"#).unwrap();
        assert_eq!(v.note, None);
    }

    #[test]
    fn integers() {
        assert_eq!(u8::deserialize(&Value::Int(255)), Ok(255));
        assert!(u8::deserialize(&Value::Int(-1)).is_err());
        assert_eq!(i32::deserialize(&Value::Float(7.0)), Ok(7));
        assert!(i32::deserialize(&Value::Float(7.5)).unwrap_err().to_string().contains("7.5"));
        assert_eq!(u64::MAX.serialize(), Value::UInt(u64::MAX));
        assert_eq!(u64::deserialize(&Value::UInt(u64::MAX)), Ok(u64::MAX));
        assert!(i64::deserialize(&Value::UInt(u64::MAX)).is_err());
        assert_eq!(u32::deserialize(&Value::Bool(true)).unwrap_err().to_string(), "expected an integer, found a boolean");
    }

    #[test]
    fn std_types() {
        let mut hm = HashMap::new();
        hm.insert("b".to_string(), 2);
        hm.insert("a".to_string(), 1);
        assert_eq!(json::to_string(&hm).unwrap(), r#"{"a":1,"b":2}"#);
        assert_eq!(json::from_str::<HashMap<String, i32>>(r#"{"a":1,"b":2}"#).unwrap(), hm);

        let t = (1u8, "two".to_string(), Some(3.5), 'x');
        assert_eq!(json::from_str::<(u8, String, Option<f64>, char)>(&json::to_string(&t).unwrap()).unwrap(), t);
        assert!(json::from_str::<(u8, u8)>("[1]").is_err());

        let bytes = Bytes(vec![0, 1, 255]);
        assert_eq!(binary::from_slice::<Bytes>(&binary::to_vec(&bytes).unwrap()).unwrap(), bytes);
        // JSON has no bytes, so writing them is an error unless numbers are allowed instead.
        assert_eq!(json::to_string(&bytes).unwrap_err().kind(), ErrorKind::Unrepresentable);
        let lossy = json::Json::compact().lossy(json::JsonLossy { bytes: true, ..json::JsonLossy::NONE });
        assert_eq!(lossy.write(&bytes.serialize()).unwrap(), "[0,1,255]");
        assert_eq!(json::from_str::<Bytes>("[0,1,255]").unwrap(), bytes);
        assert_eq!(binary::from_slice::<Vec<u8>>(&binary::to_vec(&bytes).unwrap()).unwrap(), bytes.0);
        assert!(char::deserialize(&Value::from("ab")).is_err());
    }
}
