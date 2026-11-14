//! The format-neutral data model every type serializes into and every format encodes.

use std::hash::{Hash, Hasher};

use crate::Error;

/// A value in the black-serde data model.
///
/// Types turn themselves into a `Value` ([`Serialize`](crate::Serialize)) and back
/// ([`Deserialize`](crate::Deserialize)); a [`Format`](crate::Format) turns a `Value` into bytes
/// and back.
// Clone and PartialEq are written out (below) so that deep values don't recurse.
#[derive(Debug)]
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
	/// Key/value pairs in order. Keys can be any value (YAML allows `? [a, b] : c`); JSON
	/// writes keys that aren't strings as text.
	Map(Vec<(Value, Value)>),
	/// A value with an application tag, such as YAML's `!point {x: 1, y: 2}`. Accessors such as
	/// [`as_i64`](Value::as_i64) and [`get`](Value::get) look through tags, so a tagged value
	/// still deserializes as its content.
	Tagged(String, Box<Value>),
}

impl Value {
	/// Build a map from key/value pairs, keeping their order.
	pub fn map<K: Into<Value>, const N: usize>(pairs: [(K, Value); N]) -> Value {
		Value::Map(pairs.into_iter().map(|(k, v)| (k.into(), v)).collect())
	}

	pub fn tagged(tag: impl Into<String>, value: Value) -> Value {
		Value::Tagged(tag.into(), Box::new(value))
	}

	/// A sequence's items, taken without copying; any other value (including a tagged
	/// sequence) is given back unchanged. `Value` implements `Drop`, so its contents can't be
	/// moved out with a pattern; this and [`into_map`](Value::into_map) do it instead.
	pub fn into_seq(mut self) -> Result<Vec<Value>, Value> {
		if let Value::Seq(items) = &mut self {
			return Ok(std::mem::take(items));
		}
		Err(self)
	}

	/// A map's pairs, taken without copying; any other value is given back unchanged.
	pub fn into_map(mut self) -> Result<Vec<(Value, Value)>, Value> {
		if let Value::Map(pairs) = &mut self {
			return Ok(std::mem::take(pairs));
		}
		Err(self)
	}

	/// A string's text, taken without copying; any other value is given back unchanged.
	pub fn into_string(mut self) -> Result<String, Value> {
		if let Value::String(s) = &mut self {
			return Ok(std::mem::take(s));
		}
		Err(self)
	}

	/// A byte buffer, taken without copying; any other value is given back unchanged.
	pub fn into_bytes(mut self) -> Result<Vec<u8>, Value> {
		if let Value::Bytes(b) = &mut self {
			return Ok(std::mem::take(b));
		}
		Err(self)
	}

	/// A tagged value's tag and content, taken without copying; any other value is given back
	/// unchanged.
	pub fn into_tagged(mut self) -> Result<(String, Value), Value> {
		if let Value::Tagged(tag, inner) = &mut self {
			return Ok((std::mem::take(tag), std::mem::replace(&mut **inner, Value::Null)));
		}
		Err(self)
	}

	/// The value with any tags removed.
	pub fn untagged(&self) -> &Value {
		let mut v = self;
		while let Value::Tagged(_, inner) = v {
			v = inner;
		}
		v
	}

	/// The outermost tag, if any.
	pub fn tag(&self) -> Option<&str> {
		match self {
			Value::Tagged(t, _) => Some(t),
			_ => None,
		}
	}

	/// The name of this value's kind, for error messages.
	pub fn kind(&self) -> &'static str {
		match self.untagged() {
			Value::Null => "null",
			Value::Bool(_) => "a boolean",
			Value::Int(_) | Value::UInt(_) => "an integer",
			Value::Float(_) => "a float",
			Value::String(_) => "a string",
			Value::Bytes(_) => "bytes",
			Value::Seq(_) => "a sequence",
			Value::Map(_) => "a map",
			Value::Tagged(..) => unreachable!("untagged"),
		}
	}

	/// Look up a string key in a map. If a key appears more than once, the last one wins.
	pub fn get(&self, key: &str) -> Option<&Value> {
		self.as_map()?.iter().rev().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v)
	}

	/// Look up any key in a map.
	pub fn get_key(&self, key: &Value) -> Option<&Value> {
		self.as_map()?.iter().rev().find(|(k, _)| same(k, key)).map(|(_, v)| v)
	}

	/// Look up an index in a sequence.
	pub fn at(&self, index: usize) -> Option<&Value> {
		self.as_seq()?.get(index)
	}

	/// A map key as text: strings as they are, other scalars as they'd be written (`1`,
	/// `true`, `null`), and anything else as compact JSON. Used for JSON keys and error paths.
	pub fn key_text(&self) -> String {
		match self.untagged() {
			Value::String(s) => s.clone(),
			other => other.to_string(),
		}
	}

	pub fn is_null(&self) -> bool {
		matches!(self.untagged(), Value::Null)
	}

	pub fn as_bool(&self) -> Option<bool> {
		match *self.untagged() {
			Value::Bool(b) => Some(b),
			_ => None,
		}
	}

	/// The value as an `i64`: an integer in range, or a float with no fractional part.
	pub fn as_i64(&self) -> Option<i64> {
		match *self.untagged() {
			Value::Int(i) => Some(i),
			Value::UInt(u) => i64::try_from(u).ok(),
			Value::Float(f) if f.fract() == 0.0 && (-9.223_372_036_854_776e18..9.223_372_036_854_776e18).contains(&f) => Some(f as i64),
			_ => None,
		}
	}

	/// The value as a `u64`: a non-negative integer, or a float with no fractional part.
	pub fn as_u64(&self) -> Option<u64> {
		match *self.untagged() {
			Value::Int(i) => u64::try_from(i).ok(),
			Value::UInt(u) => Some(u),
			Value::Float(f) if f.fract() == 0.0 && (0.0..1.844_674_407_370_955_2e19).contains(&f) => Some(f as u64),
			_ => None,
		}
	}

	pub fn as_f64(&self) -> Option<f64> {
		match *self.untagged() {
			Value::Int(i) => Some(i as f64),
			Value::UInt(u) => Some(u as f64),
			Value::Float(f) => Some(f),
			_ => None,
		}
	}

	pub fn as_str(&self) -> Option<&str> {
		match self.untagged() {
			Value::String(s) => Some(s),
			_ => None,
		}
	}

	pub fn as_bytes(&self) -> Option<&[u8]> {
		match self.untagged() {
			Value::Bytes(b) => Some(b),
			_ => None,
		}
	}

	pub fn as_seq(&self) -> Option<&Vec<Value>> {
		match self.untagged() {
			Value::Seq(s) => Some(s),
			_ => None,
		}
	}

	pub fn as_map(&self) -> Option<&Vec<(Value, Value)>> {
		match self.untagged() {
			Value::Map(m) => Some(m),
			_ => None,
		}
	}
}

fn is_container(v: &Value) -> bool {
	matches!(v, Value::Seq(_) | Value::Map(_) | Value::Tagged(..))
}

/// Dropping nested containers the usual way recurses once per level, so a deep enough value
/// would overflow the stack just by being freed. Instead, children are moved onto a heap stack
/// and dropped one at a time.
impl Drop for Value {
	fn drop(&mut self) {
		fn take_children(v: &mut Value, out: &mut Vec<Value>) {
			match v {
				Value::Seq(items) => out.append(items),
				Value::Map(pairs) => {
					for (k, item) in pairs.drain(..) {
						out.push(k);
						out.push(item);
					}
				}
				Value::Tagged(_, inner) => out.push(std::mem::replace(&mut **inner, Value::Null)),
				_ => {}
			}
		}
		// Containers of plain scalars, by far the common case, need nothing special.
		let nested = match self {
			Value::Seq(items) => items.iter().any(is_container),
			Value::Map(pairs) => pairs.iter().any(|(k, v)| is_container(k) || is_container(v)),
			Value::Tagged(_, inner) => is_container(inner),
			_ => false,
		};
		if !nested {
			return;
		}
		let mut stack = Vec::new();
		take_children(self, &mut stack);
		while let Some(mut v) = stack.pop() {
			take_children(&mut v, &mut stack);
		}
	}
}

/// A step in a path, borrowed from the value being walked.
#[derive(Clone, Copy)]
enum Step<'a> {
	Index(usize),
	Key(&'a Value),
}

/// Add `path` (outermost first) to an error about the value at its end.
fn with_path(e: Error, path: &[Step]) -> Error {
	let segments = path
		.iter()
		.map(|step| match step {
			Step::Index(i) => crate::Segment::Index(*i),
			Step::Key(k) => crate::Segment::Key(k.key_text()),
		})
		.collect();
	e.in_path(segments)
}

/// Visit every value in `root`, keys included, without recursing. `check` sees each value; an
/// error from it gets the value's path. A value inside more than `max_depth` sequences and
/// maps is a depth-limit error. Tags don't add depth.
pub(crate) fn walk(root: &Value, max_depth: usize, mut check: impl FnMut(&Value) -> Result<(), Error>) -> Result<(), Error> {
	// (value, depth, path length including this value's own step, its step)
	let mut stack: Vec<(&Value, usize, usize, Option<Step>)> = vec![(root, 0, 0, None)];
	let mut path: Vec<Step> = Vec::new();
	while let Some((v, depth, plen, step)) = stack.pop() {
		path.truncate(plen - step.is_some() as usize);
		if let Some(s) = step {
			path.push(s);
		}
		if depth > max_depth {
			let limits = crate::Limits { max_depth, ..crate::Limits::JSON };
			return Err(with_path(limits.depth_error(), &path));
		}
		check(v).map_err(|e| with_path(e, &path))?;
		match v {
			Value::Seq(items) => {
				for (i, item) in items.iter().enumerate().rev() {
					stack.push((item, depth + 1, plen + 1, Some(Step::Index(i))));
				}
			}
			Value::Map(pairs) => {
				for (k, item) in pairs.iter().rev() {
					stack.push((item, depth + 1, plen + 1, Some(Step::Key(k))));
					stack.push((k, depth + 1, plen + 1, Some(Step::Key(k))));
				}
			}
			Value::Tagged(_, inner) => stack.push((inner, depth, plen, None)),
			_ => {}
		}
	}
	Ok(())
}

/// Compare two values without recursing. With `bits`, floats compare by their bits, so NaN
/// equals itself and `0.0` differs from `-0.0`; otherwise they compare as numbers.
fn equal(a: &Value, b: &Value, bits: bool) -> bool {
	let mut stack = vec![(a, b)];
	while let Some((a, b)) = stack.pop() {
		match (a, b) {
			(Value::Null, Value::Null) => {}
			(Value::Bool(x), Value::Bool(y)) if x == y => {}
			(Value::Int(x), Value::Int(y)) if x == y => {}
			(Value::UInt(x), Value::UInt(y)) if x == y => {}
			// `UInt` is meant only for values above `i64::MAX`, but nothing stops `UInt(5)`;
			// it's the same number as `Int(5)`.
			(Value::Int(i), Value::UInt(u)) | (Value::UInt(u), Value::Int(i)) if u64::try_from(*i) == Ok(*u) => {}
			(Value::Float(x), Value::Float(y)) if (bits && x.to_bits() == y.to_bits()) || (!bits && x == y) => {}
			(Value::String(x), Value::String(y)) if x == y => {}
			(Value::Bytes(x), Value::Bytes(y)) if x == y => {}
			(Value::Seq(x), Value::Seq(y)) if x.len() == y.len() => stack.extend(x.iter().zip(y)),
			(Value::Map(x), Value::Map(y)) if x.len() == y.len() => {
				for ((ka, va), (kb, vb)) in x.iter().zip(y) {
					stack.push((ka, kb));
					stack.push((va, vb));
				}
			}
			(Value::Tagged(ta, x), Value::Tagged(tb, y)) if ta == tb => stack.push((x, y)),
			_ => return false,
		}
	}
	true
}

/// Equality as numbers for floats (so NaN differs from itself, as with `f64`).
impl PartialEq for Value {
	fn eq(&self, other: &Value) -> bool {
		equal(self, other, false)
	}
}

/// Cloning nested containers the usual way recurses once per level; this keeps a heap stack
/// instead, so deep values clone without overflowing the stack.
impl Clone for Value {
	fn clone(&self) -> Self {
		fn shallow(v: &Value) -> Option<Value> {
			Some(match v {
				Value::Null => Value::Null,
				Value::Bool(b) => Value::Bool(*b),
				Value::Int(i) => Value::Int(*i),
				Value::UInt(u) => Value::UInt(*u),
				Value::Float(f) => Value::Float(*f),
				Value::String(s) => Value::String(s.clone()),
				Value::Bytes(b) => Value::Bytes(b.clone()),
				_ => return None,
			})
		}
		if let Some(v) = shallow(self) {
			return v;
		}
		enum Job<'a> {
			Visit(&'a Value),
			Seq(usize),
			Map(usize),
			Tagged(&'a str),
		}
		let mut jobs = vec![Job::Visit(self)];
		// Finished copies, in order; a container's job takes its children off the end.
		let mut done: Vec<Value> = Vec::new();
		while let Some(job) = jobs.pop() {
			match job {
				Job::Visit(v) => match v {
					Value::Seq(items) => {
						jobs.push(Job::Seq(items.len()));
						jobs.extend(items.iter().rev().map(Job::Visit));
					}
					Value::Map(pairs) => {
						jobs.push(Job::Map(pairs.len()));
						for (k, item) in pairs.iter().rev() {
							jobs.push(Job::Visit(item));
							jobs.push(Job::Visit(k));
						}
					}
					Value::Tagged(tag, inner) => {
						jobs.push(Job::Tagged(tag));
						jobs.push(Job::Visit(inner));
					}
					scalar => done.push(shallow(scalar).expect("a scalar")),
				},
				Job::Seq(n) => {
					let items = done.split_off(done.len() - n);
					done.push(Value::Seq(items));
				}
				Job::Map(n) => {
					let mut flat = done.split_off(done.len() - 2 * n).into_iter();
					let mut pairs = Vec::with_capacity(n);
					while let (Some(k), Some(v)) = (flat.next(), flat.next()) {
						pairs.push((k, v));
					}
					done.push(Value::Map(pairs));
				}
				Job::Tagged(tag) => {
					let inner = done.pop().expect("the tagged value");
					done.push(Value::Tagged(tag.to_string(), Box::new(inner)));
				}
			}
		}
		done.pop().expect("the copy")
	}
}

/// What copying `v` costs, in values (keys included), with each string, byte buffer and tag
/// also counting one value per [`BYTES_PER_VALUE`](crate::limits::BYTES_PER_VALUE) bytes, so
/// that a copied 1 MB string isn't counted as one small value. Counted without recursing.
pub(crate) fn copy_cost(v: &Value) -> usize {
	let text = |len: usize| len / crate::limits::BYTES_PER_VALUE;
	let mut n: usize = 0;
	let mut stack = vec![v];
	while let Some(v) = stack.pop() {
		n = n.saturating_add(match v {
			Value::Seq(items) => {
				stack.extend(items);
				1
			}
			Value::Map(pairs) => {
				for (k, item) in pairs {
					stack.push(k);
					stack.push(item);
				}
				1
			}
			Value::Tagged(tag, inner) => {
				stack.push(inner);
				text(tag.len())
			}
			Value::String(s) => 1 + text(s.len()),
			Value::Bytes(b) => 1 + text(b.len()),
			_ => 1,
		});
	}
	n
}

/// Structural equality where floats compare by their bits, so NaN equals itself and `0.0`
/// differs from `-0.0`. Used for map keys and for finding repeated subtrees.
pub(crate) fn same(a: &Value, b: &Value) -> bool {
	equal(a, b, true)
}

/// A hash consistent with [`same`], computed without recursing.
pub(crate) fn hash_value<H: Hasher>(v: &Value, h: &mut H) {
	let mut stack = vec![v];
	while let Some(v) = stack.pop() {
		// A `UInt` that fits in `i64` equals the `Int` with the same value, so hash it as one.
		if let Value::UInt(u) = *v
			&& let Ok(i) = i64::try_from(u)
		{
			std::mem::discriminant(&Value::Int(0)).hash(h);
			i.hash(h);
			continue;
		}
		std::mem::discriminant(v).hash(h);
		match v {
			Value::Null => {}
			Value::Bool(b) => b.hash(h),
			Value::Int(i) => i.hash(h),
			Value::UInt(u) => u.hash(h),
			Value::Float(f) => f.to_bits().hash(h),
			Value::String(s) => s.hash(h),
			Value::Bytes(b) => b.hash(h),
			Value::Seq(items) => {
				items.len().hash(h);
				stack.extend(items.iter().rev());
			}
			Value::Map(pairs) => {
				pairs.len().hash(h);
				for (k, item) in pairs.iter().rev() {
					stack.push(item);
					stack.push(k);
				}
			}
			Value::Tagged(t, inner) => {
				t.hash(h);
				stack.push(inner);
			}
		}
	}
}

pub(crate) fn key_hash(v: &Value) -> u64 {
	let mut h = std::collections::hash_map::DefaultHasher::new();
	hash_value(v, &mut h);
	h.finish()
}

/// Finds keys already in a list of pairs without comparing against every one. Small maps,
/// the common case, are searched directly; the hash index is built once a map grows past
/// [`KeyIndex::LINEAR`] keys.
#[derive(Default)]
pub(crate) struct KeyIndex {
	by_hash: Option<std::collections::HashMap<u64, Vec<usize>>>,
}

impl KeyIndex {
	const LINEAR: usize = 16;

	/// The index in `pairs` (every pair inserted so far) of a key equal to `key`.
	pub(crate) fn find(&self, pairs: &[(Value, Value)], key: &Value) -> Option<usize> {
		match &self.by_hash {
			None => pairs.iter().position(|(k, _)| same(k, key)),
			Some(m) => m.get(&key_hash(key))?.iter().copied().find(|&i| same(&pairs[i].0, key)),
		}
	}

	/// Record `key` as about to be pushed onto `pairs` at `index` (`pairs.len()`).
	pub(crate) fn insert(&mut self, pairs: &[(Value, Value)], key: &Value, index: usize) {
		if self.by_hash.is_none() {
			if index + 1 < Self::LINEAR {
				return;
			}
			let mut m: std::collections::HashMap<u64, Vec<usize>> = std::collections::HashMap::new();
			for (i, (k, _)) in pairs[..index].iter().enumerate() {
				m.entry(key_hash(k)).or_default().push(i);
			}
			self.by_hash = Some(m);
		}
		self.by_hash.as_mut().expect("built above").entry(key_hash(key)).or_default().push(index);
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
