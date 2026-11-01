//! The format-neutral data model every type serializes into and every format encodes.

/// A value in the black-serde data model.
///
/// Types turn themselves into a `Value` ([`Serialize`](crate::Serialize)) and back
/// ([`Deserialize`](crate::Deserialize)); a [`Format`](crate::Format) turns a `Value` into bytes
/// and back. Maps keep their keys in insertion order.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    /// Only used for unsigned integers above `i64::MAX`; smaller ones are `Int`.
    UInt(u64),
    Float(f64),
    String(String),
    Bytes(Vec<u8>),
    Seq(Vec<Value>),
    Map(Vec<(String, Value)>),
}

impl Value {
    /// Build a map from key/value pairs, keeping their order.
    pub fn map<K: Into<String>, const N: usize>(pairs: [(K, Value); N]) -> Value {
        Value::Map(pairs.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }

    /// The name of this value's kind, for error messages.
    pub fn kind(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "a boolean",
            Value::Int(_) | Value::UInt(_) => "an integer",
            Value::Float(_) => "a float",
            Value::String(_) => "a string",
            Value::Bytes(_) => "bytes",
            Value::Seq(_) => "a sequence",
            Value::Map(_) => "a map",
        }
    }

    /// Look up a key in a map. If a key appears more than once, the last one wins.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_map()?.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// Look up an index in a sequence.
    pub fn at(&self, index: usize) -> Option<&Value> {
        self.as_seq()?.get(index)
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    pub fn as_bool(&self) -> Option<bool> {
        match *self {
            Value::Bool(b) => Some(b),
            _ => None,
        }
    }

    /// The value as an `i64`: an integer in range, or a float with no fractional part.
    pub fn as_i64(&self) -> Option<i64> {
        match *self {
            Value::Int(i) => Some(i),
            Value::UInt(u) => i64::try_from(u).ok(),
            Value::Float(f) if f.fract() == 0.0 && (-9.223_372_036_854_776e18..9.223_372_036_854_776e18).contains(&f) => {
                Some(f as i64)
            }
            _ => None,
        }
    }

    /// The value as a `u64`: a non-negative integer, or a float with no fractional part.
    pub fn as_u64(&self) -> Option<u64> {
        match *self {
            Value::Int(i) => u64::try_from(i).ok(),
            Value::UInt(u) => Some(u),
            Value::Float(f) if f.fract() == 0.0 && (0.0..1.844_674_407_370_955_2e19).contains(&f) => Some(f as u64),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match *self {
            Value::Int(i) => Some(i as f64),
            Value::UInt(u) => Some(u as f64),
            Value::Float(f) => Some(f),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Value::Bytes(b) => Some(b),
            _ => None,
        }
    }

    pub fn as_seq(&self) -> Option<&Vec<Value>> {
        match self {
            Value::Seq(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_map(&self) -> Option<&Vec<(String, Value)>> {
        match self {
            Value::Map(m) => Some(m),
            _ => None,
        }
    }
}

macro_rules! from_signed {
    ($($t:ty),*) => {$(
        impl From<$t> for Value {
            fn from(n: $t) -> Self { Value::Int(n as i64) }
        }
    )*};
}
from_signed!(i8, i16, i32, i64, isize, u8, u16, u32);

impl From<u64> for Value {
    fn from(n: u64) -> Self {
        i64::try_from(n).map_or(Value::UInt(n), Value::Int)
    }
}

impl From<usize> for Value {
    fn from(n: usize) -> Self {
        Value::from(n as u64)
    }
}

impl From<f64> for Value {
    fn from(f: f64) -> Self {
        Value::Float(f)
    }
}

impl From<f32> for Value {
    fn from(f: f32) -> Self {
        Value::Float(f as f64)
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::String(s.to_string())
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::String(s)
    }
}

impl From<Vec<Value>> for Value {
    fn from(s: Vec<Value>) -> Self {
        Value::Seq(s)
    }
}
