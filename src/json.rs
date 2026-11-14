//! JSON (RFC 8259) as a black-serde [`Format`], with swappable tokens.
//!
//! JSON can't represent bytes, non-finite floats, map keys that aren't strings, or tags.
//! Writing any of them is an error unless [`JsonLossy`] allows the lossy conversion (bytes as
//! numbers, non-finite floats as `null`, keys as their text, tags dropped). Integers that fit
//! in `i64` read back as `Int`, larger ones that fit in `u64` as `UInt`, anything else as
//! `Float`; a number too large for `f64` is an error. Object keys must be unique, and a leading
//! byte order mark is skipped. Reading and writing keep their own stack instead of recursing,
//! so nesting is limited only by `max_depth` (see [`Limits`]).
//!
//! The punctuation and literal words come from [`JsonTokens`]. Change any of them, and the
//! reader and writer both use the new spelling. Because the tokens are recognized by the
//! tokenizer rather than substituted into the text, strings can contain them freely:
//!
//! ```
//! use black_serde::json::{Json, JsonTokens};
//! use black_serde::Value;
//!
//! let angle = Json::compact().with_tokens(JsonTokens { map_open: "<|", map_close: "|>", ..JsonTokens::STANDARD })?;
//! let v = Value::map([("text", Value::from("a <| b |> c"))]);
//! let text = angle.write(&v)?;
//! assert_eq!(text, r#"<|"text":"a <| b |> c"|>"#);
//! assert_eq!(angle.read(&text)?, v);
//! # Ok::<(), black_serde::Error>(())
//! ```

use std::fmt::{self, Write as _};

use crate::tokens::{check_no_self_overlap, check_prefix_free, check_start, check_text};
use crate::{Deserialize, Error, Format, KeyIndex, Limits, Segment, Serialize, Value};

/// The default limit on nesting; see [`Limits`].
pub const MAX_DEPTH: usize = 256;

/// The spelling of every JSON token. Start from [`JsonTokens::STANDARD`] and change the ones you
/// want with struct update syntax; [`Json::with_tokens`] checks the result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JsonTokens {
	pub map_open: &'static str,
	pub map_close: &'static str,
	pub seq_open: &'static str,
	pub seq_close: &'static str,
	/// Between entries.
	pub comma: &'static str,
	/// Between a key and its value.
	pub colon: &'static str,
	pub string_open: &'static str,
	/// Inside a string, this token is written escaped as `\` followed by the token.
	pub string_close: &'static str,
	pub null: &'static str,
	pub bool_true: &'static str,
	pub bool_false: &'static str,
}

impl JsonTokens {
	pub const STANDARD: JsonTokens = JsonTokens {
		map_open: "{",
		map_close: "}",
		seq_open: "[",
		seq_close: "]",
		comma: ",",
		colon: ":",
		string_open: "\"",
		string_close: "\"",
		null: "null",
		bool_true: "true",
		bool_false: "false",
	};

	/// Check that the set can be read back unambiguously.
	pub fn check(&self) -> Result<(), Error> {
		let t = self;
		let all = [
			("map_open", t.map_open),
			("map_close", t.map_close),
			("seq_open", t.seq_open),
			("seq_close", t.seq_close),
			("comma", t.comma),
			("colon", t.colon),
			("string_open", t.string_open),
			("string_close", t.string_close),
			("null", t.null),
			("bool_true", t.bool_true),
			("bool_false", t.bool_false),
		];
		for (name, tok) in all {
			check_text("JSON", name, tok)?;
		}
		// Where a value can start (including right after seq_open, where seq_close can too).
		let value_start = [
			("map_open", t.map_open),
			("seq_open", t.seq_open),
			("string_open", t.string_open),
			("null", t.null),
			("bool_true", t.bool_true),
			("bool_false", t.bool_false),
		];
		let mut first = value_start.to_vec();
		first.push(("seq_close", t.seq_close));
		check_prefix_free("JSON", &first)?;
		for (name, tok) in value_start.iter().chain(&[("seq_close", t.seq_close), ("map_close", t.map_close)]) {
			check_start("JSON", name, tok, "-0123456789", "which starts a number")?;
		}
		check_prefix_free("JSON", &[("string_open", t.string_open), ("map_close", t.map_close)])?;
		check_prefix_free("JSON", &[("comma", t.comma), ("seq_close", t.seq_close)])?;
		check_prefix_free("JSON", &[("comma", t.comma), ("map_close", t.map_close)])?;
		// Tokens that can follow a number must not look like more of the number.
		for (name, tok) in [("comma", t.comma), ("seq_close", t.seq_close), ("map_close", t.map_close)] {
			check_start("JSON", name, tok, "0123456789.eE+-", "which would continue a number")?;
		}
		check_start("JSON", "string_close", t.string_close, "\\/bfnrtu", "which would read as an escape after '\\'")?;
		check_no_self_overlap("JSON", "string_close", t.string_close)?;
		Ok(())
	}
}

impl Default for JsonTokens {
	fn default() -> Self {
		JsonTokens::STANDARD
	}
}

/// What JSON may do with parts of the data model it has no way to write. Each is an error
/// (of kind [`Unrepresentable`](crate::ErrorKind::Unrepresentable)) unless allowed here; when
/// allowed, it's written the lossy way described, and reads back differently.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JsonLossy {
	/// NaN and the infinities, written as `null`.
	pub non_finite_floats: bool,
	/// Map keys that aren't strings, written as their text (`1`, `true`, `[1,2]`).
	pub non_string_keys: bool,
	/// Tags, dropped: only the value is written.
	pub tags: bool,
	/// Bytes, written as a sequence of numbers.
	pub bytes: bool,
}

impl JsonLossy {
	/// Every lossy conversion is an error.
	pub const NONE: JsonLossy = JsonLossy { non_finite_floats: false, non_string_keys: false, tags: false, bytes: false };
	/// Every lossy conversion is allowed.
	pub const ALL: JsonLossy = JsonLossy { non_finite_floats: true, non_string_keys: true, tags: true, bytes: true };
}

/// JSON settings. `Json::default()` is compact standard JSON.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Json {
	/// Spaces per level; `None` writes everything on one line.
	pub indent: Option<usize>,
	/// When indenting, containers at this depth or deeper stay on one line.
	/// `Some(2)` on `{"events": [{...}, {...}]}` puts each event on its own line.
	pub inline_from_depth: Option<usize>,
	/// Which lossy conversions writing may make; none by default.
	pub lossy: JsonLossy,
	tokens: JsonTokens,
	limits: Option<Limits>,
}

impl Json {
	pub fn compact() -> Self {
		Json::default()
	}

	pub fn pretty(indent: usize) -> Self {
		Json { indent: Some(indent), ..Json::default() }
	}

	/// Keep containers at `depth` and deeper on one line.
	pub fn inline_from(mut self, depth: usize) -> Self {
		self.inline_from_depth = Some(depth);
		self
	}

	/// Allow these lossy conversions when writing.
	pub fn lossy(mut self, lossy: JsonLossy) -> Self {
		self.lossy = lossy;
		self
	}

	/// Use a different token set, after checking it.
	pub fn with_tokens(mut self, tokens: JsonTokens) -> Result<Self, Error> {
		tokens.check()?;
		self.tokens = tokens;
		Ok(self)
	}

	/// Use these limits instead of the defaults ([`Limits::JSON`]).
	pub fn with_limits(mut self, limits: Limits) -> Self {
		self.limits = Some(limits);
		self
	}

	/// The limits reading and writing use: those set with [`with_limits`](Json::with_limits),
	/// or else [`Limits::JSON`].
	pub fn limits(&self) -> Limits {
		self.limits.unwrap_or(Limits::JSON)
	}

	pub fn tokens(&self) -> &JsonTokens {
		&self.tokens
	}

	/// Write a value. Fails if the value can't be written without loss (see [`JsonLossy`]) or
	/// is nested deeper than `max_depth`; the error's path says where.
	pub fn write(&self, value: &Value) -> Result<String, Error> {
		let mut out = String::new();
		write_document(&mut out, value, self, self.limits().max_depth)?;
		Ok(out)
	}

	/// Parse a complete document written with this format's tokens. A leading byte order mark
	/// is skipped. Object keys must be unique.
	pub fn read(&self, text: &str) -> Result<Value, Error> {
		let limits = self.limits();
		limits.check_input(text.len())?;
		let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
		let mut p = Parser { src: text, pos: 0, t: &self.tokens, limits };
		p.skip_ws();
		let v = p.value()?;
		p.skip_ws();
		if p.pos < p.src.len() {
			return Err(p.error("unexpected text after the JSON value"));
		}
		Ok(v)
	}
}

impl Format for Json {
	fn encode(&self, value: &Value) -> Result<Vec<u8>, Error> {
		self.write(value).map(String::into_bytes)
	}

	fn decode(&self, input: &[u8]) -> Result<Value, Error> {
		self.limits().check_input(input.len())?;
		let text = std::str::from_utf8(input).map_err(|e| Error::new("JSON input is not valid UTF-8").at_offset(e.valid_up_to()))?;
		self.read(text)
	}
}

/// Serialize to compact JSON.
pub fn to_string<T: Serialize + ?Sized>(value: &T) -> Result<String, Error> {
	Json::compact().write(&value.serialize())
}

/// Serialize to indented JSON.
pub fn to_string_pretty<T: Serialize + ?Sized>(value: &T, indent: usize) -> Result<String, Error> {
	Json::pretty(indent).write(&value.serialize())
}

/// Deserialize a `T` from JSON text.
pub fn from_str<T: Deserialize>(text: &str) -> Result<T, Error> {
	T::deserialize(&parse(text)?)
}

/// Parse a complete standard JSON document into a [`Value`].
pub fn parse(text: &str) -> Result<Value, Error> {
	Json::default().read(text)
}

/// Compact standard JSON, for showing a value. Unlike [`Json::write`] this never fails: it makes
/// every lossy conversion in [`JsonLossy::ALL`] and has no depth limit.
impl fmt::Display for Value {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		let json = Json::compact().lossy(JsonLossy::ALL);
		let mut out = String::new();
		write_document(&mut out, self, &json, usize::MAX).map_err(|_| fmt::Error)?;
		f.write_str(&out)
	}
}

// ---------- writing ----------

/// A container being written, with its items still to go.
enum Open<'a> {
	Seq { items: std::slice::Iter<'a, Value>, count: usize },
	Map { pairs: std::slice::Iter<'a, (Value, Value)>, count: usize, key: Option<&'a Value> },
}

struct Frame<'a> {
	open: Open<'a>,
	/// Indentation for this container's items, or `None` to keep them on one line.
	indent: Option<usize>,
	depth: usize,
}

/// Add the position of the value being written (the innermost open containers' current items)
/// to an error.
fn at_path(e: Error, stack: &[Frame]) -> Error {
	let path = stack
		.iter()
		.filter_map(|frame| match &frame.open {
			Open::Seq { count, .. } => Some(Segment::Index(count.saturating_sub(1))),
			Open::Map { key: Some(k), .. } => Some(Segment::Key(k.key_text())),
			Open::Map { key: None, .. } => None,
		})
		.collect();
	e.in_path(path)
}

/// Write `root`, keeping open containers on a heap stack so any depth up to `max_depth` works.
fn write_document(out: &mut String, root: &Value, opts: &Json, max_depth: usize) -> Result<(), Error> {
	let t = &opts.tokens;
	let lossy = opts.lossy;
	let mut stack: Vec<Frame> = Vec::new();
	let mut next = Some(root);
	loop {
		if let Some(mut v) = next.take() {
			let depth = stack.len();
			if depth > max_depth {
				let limits = Limits { max_depth, ..Limits::JSON };
				return Err(at_path(limits.depth_error(), &stack));
			}
			if let Value::Tagged(tag, _) = v {
				if !lossy.tags {
					let e = Error::unrepresentable(format!("JSON can't write the tag {tag:?} (allow dropping tags with JsonLossy::tags)"));
					return Err(at_path(e, &stack));
				}
				v = v.untagged();
			}
			match v {
				Value::Null => out.push_str(t.null),
				Value::Bool(b) => out.push_str(if *b { t.bool_true } else { t.bool_false }),
				Value::Int(i) => {
					let _ = write!(out, "{i}");
				}
				Value::UInt(u) => {
					let _ = write!(out, "{u}");
				}
				Value::Float(f) if !f.is_finite() => {
					if !lossy.non_finite_floats {
						let e = Error::unrepresentable(format!("JSON can't write {f} (allow writing it as null with JsonLossy::non_finite_floats)"));
						return Err(at_path(e, &stack));
					}
					out.push_str(t.null);
				}
				// Debug formatting is the shortest text that round-trips, and keeps a ".0" or
				// an exponent so the number reads back as a float.
				Value::Float(f) => {
					let _ = write!(out, "{f:?}");
				}
				Value::String(s) => write_string_with(out, s, t),
				Value::Bytes(bytes) => {
					if !lossy.bytes {
						let e = Error::unrepresentable("JSON has no bytes type (allow writing them as numbers with JsonLossy::bytes)");
						return Err(at_path(e, &stack));
					}
					out.push_str(t.seq_open);
					for (i, b) in bytes.iter().enumerate() {
						if i > 0 {
							out.push_str(t.comma);
						}
						let _ = write!(out, "{b}");
					}
					out.push_str(t.seq_close);
				}
				Value::Seq(items) => {
					out.push_str(t.seq_open);
					let open = Open::Seq { items: items.iter(), count: 0 };
					stack.push(Frame { open, indent: indent_at(opts, depth), depth });
				}
				Value::Map(pairs) => {
					out.push_str(t.map_open);
					let open = Open::Map { pairs: pairs.iter(), count: 0, key: None };
					stack.push(Frame { open, indent: indent_at(opts, depth), depth });
				}
				Value::Tagged(..) => unreachable!("untagged above"),
			}
		}
		// Move on to the next item of the innermost open container, or close it.
		let Some(top) = stack.last_mut() else { return Ok(()) };
		let (indent, depth) = (top.indent, top.depth);
		match &mut top.open {
			Open::Seq { items, count } => match items.next() {
				Some(item) => {
					if *count > 0 {
						out.push_str(t.comma);
					}
					*count += 1;
					newline(out, indent, depth + 1);
					next = Some(item);
				}
				None => {
					if *count > 0 {
						newline(out, indent, depth);
					}
					out.push_str(t.seq_close);
					stack.pop();
				}
			},
			Open::Map { pairs, count, key } => match pairs.next() {
				Some((k, item)) => {
					if *count > 0 {
						out.push_str(t.comma);
					}
					*count += 1;
					*key = Some(k);
					newline(out, indent, depth + 1);
					write_key(out, k, opts).map_err(|e| at_path(e, &stack))?;
					out.push_str(t.colon);
					if indent.is_some() {
						out.push(' ');
					}
					next = Some(item);
				}
				None => {
					if *count > 0 {
						newline(out, indent, depth);
					}
					out.push_str(t.map_close);
					stack.pop();
				}
			},
		}
	}
}

fn write_key(out: &mut String, k: &Value, opts: &Json) -> Result<(), Error> {
	if k.tag().is_some() && !opts.lossy.tags {
		return Err(Error::unrepresentable("JSON can't write a tag on a key (allow dropping tags with JsonLossy::tags)"));
	}
	match k.untagged() {
		Value::String(s) => write_string_with(out, s, &opts.tokens),
		other if opts.lossy.non_string_keys => write_string_with(out, &other.key_text(), &opts.tokens),
		other => {
			return Err(Error::unrepresentable(format!(
				"JSON keys are strings, so it can't write the key {} (allow writing it as text with JsonLossy::non_string_keys)",
				other.key_text()
			)));
		}
	}
	Ok(())
}

/// Indentation for the items of a container at `depth`.
fn indent_at(opts: &Json, depth: usize) -> Option<usize> {
	match (opts.indent, opts.inline_from_depth) {
		(Some(_), Some(d)) if depth >= d => None,
		(i, _) => i,
	}
}

fn newline(out: &mut String, indent: Option<usize>, depth: usize) {
	if let Some(n) = indent {
		out.push('\n');
		out.extend(std::iter::repeat_n(' ', n * depth));
	}
}

/// Write `s` as a quoted standard JSON string.
pub fn write_string(out: &mut String, s: &str) {
	write_string_with(out, s, &JsonTokens::STANDARD);
}

fn write_string_with(out: &mut String, s: &str, t: &JsonTokens) {
	out.push_str(t.string_open);
	let mut rest = s;
	while let Some(c) = rest.chars().next() {
		if rest.starts_with(t.string_close) {
			out.push('\\');
			out.push_str(t.string_close);
			rest = &rest[t.string_close.len()..];
			continue;
		}
		match c {
			'\\' => out.push_str("\\\\"),
			'\n' => out.push_str("\\n"),
			'\r' => out.push_str("\\r"),
			'\t' => out.push_str("\\t"),
			'\u{08}' => out.push_str("\\b"),
			'\u{0C}' => out.push_str("\\f"),
			// Control characters, plus the two line separators JavaScript treats as newlines.
			c if (c as u32) < 0x20 || c == '\u{2028}' || c == '\u{2029}' => {
				let _ = write!(out, "\\u{:04x}", c as u32);
			}
			c => out.push(c),
		}
		rest = &rest[c.len_utf8()..];
	}
	out.push_str(t.string_close);
}

// ---------- parsing ----------

struct Parser<'a> {
	src: &'a str,
	pos: usize,
	t: &'a JsonTokens,
	limits: Limits,
}

impl Parser<'_> {
	fn error(&self, message: &str) -> Error {
		self.at(Error::new(message))
	}

	/// Add the current position to an error.
	fn at(&self, e: Error) -> Error {
		let before = &self.src.as_bytes()[..self.pos.min(self.src.len())];
		let line = before.iter().filter(|&&b| b == b'\n').count() + 1;
		let line_start = before.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
		// Count characters, not bytes, so the column is right for non-ASCII text.
		let column = String::from_utf8_lossy(&before[line_start..]).chars().count() + 1;
		e.at_line(line, column)
	}

	fn rest(&self) -> &str {
		&self.src[self.pos..]
	}

	fn peek(&self) -> Option<u8> {
		self.src.as_bytes().get(self.pos).copied()
	}

	/// Consume `token` if the input continues with it.
	fn eat(&mut self, token: &str) -> bool {
		if self.rest().starts_with(token) {
			self.pos += token.len();
			true
		} else {
			false
		}
	}

	fn skip_ws(&mut self) {
		while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
			self.pos += 1;
		}
	}

	/// One complete value. Open containers are kept on a heap stack rather than by recursing,
	/// so nesting is limited only by `max_depth`.
	fn value(&mut self) -> Result<Value, Error> {
		enum Open {
			Seq(Vec<Value>),
			/// Pairs so far, the key waiting for its value, and an index of the keys.
			Map(Vec<(Value, Value)>, Value, KeyIndex),
		}
		let t = self.t;
		let mut stack: Vec<Open> = Vec::new();
		loop {
			// Read a scalar, or open a container and go on to its first item.
			self.skip_ws();
			if stack.len() > self.limits.max_depth {
				return Err(self.at(self.limits.depth_error()));
			}
			let mut v = if self.eat(t.map_open) {
				self.skip_ws();
				if self.eat(t.map_close) {
					Value::Map(Vec::new())
				} else {
					let key = self.key()?;
					stack.push(Open::Map(Vec::new(), key, KeyIndex::default()));
					continue;
				}
			} else if self.eat(t.seq_open) {
				self.skip_ws();
				if self.eat(t.seq_close) {
					Value::Seq(Vec::new())
				} else {
					stack.push(Open::Seq(Vec::new()));
					continue;
				}
			} else {
				self.scalar()?
			};
			// Add it to the innermost open container, closing containers that end here.
			loop {
				self.skip_ws();
				match stack.last_mut() {
					None => return Ok(v),
					Some(Open::Seq(items)) => {
						items.push(v);
						if self.eat(t.comma) {
							break;
						}
						if !self.eat(t.seq_close) {
							return Err(self.error(&format!("expected '{}' or '{}'", t.comma, t.seq_close)));
						}
						let Some(Open::Seq(items)) = stack.pop() else { unreachable!() };
						v = Value::Seq(items);
					}
					Some(Open::Map(pairs, key, index)) => {
						let k = std::mem::replace(key, Value::Null);
						index.insert(pairs, &k, pairs.len());
						pairs.push((k, v));
						if self.eat(t.comma) {
							self.skip_ws();
							let at = self.pos;
							*key = self.key()?;
							if index.find(pairs, key).is_some() {
								self.pos = at;
								return Err(self.error(&format!("duplicate key {key}")));
							}
							break;
						}
						if !self.eat(t.map_close) {
							return Err(self.error(&format!("expected '{}' or '{}'", t.comma, t.map_close)));
						}
						let Some(Open::Map(pairs, ..)) = stack.pop() else { unreachable!() };
						v = Value::Map(pairs);
					}
				}
			}
		}
	}

	/// A map key and the colon after it.
	fn key(&mut self) -> Result<Value, Error> {
		let t = self.t;
		if !self.eat(t.string_open) {
			return Err(self.error("expected a string key"));
		}
		let key = self.string()?;
		self.skip_ws();
		if !self.eat(t.colon) {
			return Err(self.error(&format!("expected '{}'", t.colon)));
		}
		Ok(Value::String(key))
	}

	fn scalar(&mut self) -> Result<Value, Error> {
		let t = self.t;
		if self.pos >= self.src.len() {
			return Err(self.error("unexpected end of input"));
		}
		if self.eat(t.string_open) {
			return Ok(Value::String(self.string()?));
		}
		if self.eat(t.null) {
			return Ok(Value::Null);
		}
		if self.eat(t.bool_true) {
			return Ok(Value::Bool(true));
		}
		if self.eat(t.bool_false) {
			return Ok(Value::Bool(false));
		}
		match self.peek() {
			Some(b'-' | b'0'..=b'9') => self.number(),
			_ => Err(self.error("expected a JSON value")),
		}
	}

	fn hex4(&mut self) -> Result<u32, Error> {
		let digits = self.src.as_bytes().get(self.pos..self.pos + 4).ok_or_else(|| self.error("truncated \\u escape"))?;
		let mut n = 0;
		for &d in digits {
			let v = (d as char).to_digit(16).ok_or_else(|| self.error("invalid \\u escape"))?;
			n = n * 16 + v;
		}
		self.pos += 4;
		Ok(n)
	}

	/// The rest of a string whose opening token has been consumed.
	fn string(&mut self) -> Result<String, Error> {
		let close = self.t.string_close;
		let mut out = String::new();
		loop {
			// Copy the run of plain characters in one go.
			let start = self.pos;
			let close_first = close.as_bytes()[0];
			while let Some(b) = self.peek() {
				if b == b'\\' || b < 0x20 || (b == close_first && self.rest().starts_with(close)) {
					break;
				}
				self.pos += 1;
			}
			out.push_str(&self.src[start..self.pos]);

			match self.peek() {
				None => return Err(self.error("unterminated string")),
				Some(b'\\') => {
					self.pos += 1;
					if self.eat(close) {
						out.push_str(close);
						continue;
					}
					let esc = self.peek().ok_or_else(|| self.error("unterminated string"))?;
					self.pos += 1;
					match esc {
						b'"' => out.push('"'),
						b'\\' => out.push('\\'),
						b'/' => out.push('/'),
						b'b' => out.push('\u{08}'),
						b'f' => out.push('\u{0C}'),
						b'n' => out.push('\n'),
						b'r' => out.push('\r'),
						b't' => out.push('\t'),
						b'u' => out.push(self.unicode_escape()?),
						_ => {
							self.pos -= 1;
							return Err(self.error("invalid escape"));
						}
					}
				}
				Some(b) if b < 0x20 => return Err(self.error("control character in string")),
				Some(_) => {
					self.pos += close.len();
					return Ok(out);
				}
			}
		}
	}

	fn unicode_escape(&mut self) -> Result<char, Error> {
		let first = self.hex4()?;
		let code = match first {
			0xD800..=0xDBFF => {
				// A high surrogate must be followed by \u and a low surrogate.
				if !self.rest().starts_with("\\u") {
					return Err(self.error("unpaired surrogate in \\u escape"));
				}
				self.pos += 2;
				let second = self.hex4()?;
				if !(0xDC00..=0xDFFF).contains(&second) {
					return Err(self.error("unpaired surrogate in \\u escape"));
				}
				0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00)
			}
			0xDC00..=0xDFFF => return Err(self.error("unpaired surrogate in \\u escape")),
			c => c,
		};
		char::from_u32(code).ok_or_else(|| self.error("invalid \\u escape"))
	}

	fn number(&mut self) -> Result<Value, Error> {
		let start = self.pos;
		let digits = |p: &mut Self| {
			let s = p.pos;
			while matches!(p.peek(), Some(b'0'..=b'9')) {
				p.pos += 1;
			}
			p.pos - s
		};
		if self.peek() == Some(b'-') {
			self.pos += 1;
		}
		match self.peek() {
			Some(b'0') => {
				self.pos += 1;
				if matches!(self.peek(), Some(b'0'..=b'9')) {
					return Err(self.error("leading zeros are not allowed"));
				}
			}
			Some(b'1'..=b'9') => {
				digits(self);
			}
			_ => return Err(self.error("invalid number")),
		}
		let mut is_float = false;
		if self.peek() == Some(b'.') {
			self.pos += 1;
			is_float = true;
			if digits(self) == 0 {
				return Err(self.error("expected digits after '.'"));
			}
		}
		if matches!(self.peek(), Some(b'e' | b'E')) {
			self.pos += 1;
			is_float = true;
			if matches!(self.peek(), Some(b'+' | b'-')) {
				self.pos += 1;
			}
			if digits(self) == 0 {
				return Err(self.error("expected digits in exponent"));
			}
		}
		let text = &self.src[start..self.pos];
		// `-0` is negative zero, which only a float can hold.
		if !is_float && text != "-0" {
			if let Ok(i) = text.parse::<i64>() {
				return Ok(Value::Int(i));
			}
			if let Ok(u) = text.parse::<u64>() {
				return Ok(Value::UInt(u));
			}
		}
		let f: f64 = text.parse().map_err(|_| self.error("invalid number"))?;
		if f.is_infinite() {
			self.pos = start;
			return Err(self.error("number is too large for a 64-bit float"));
		}
		Ok(Value::Float(f))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn rt(text: &str) -> String {
		parse(text).unwrap().to_string()
	}

	#[test]
	fn literals_and_numbers() {
		assert_eq!(parse("null").unwrap(), Value::Null);
		assert_eq!(parse(" true ").unwrap(), Value::Bool(true));
		assert_eq!(parse("false").unwrap(), Value::Bool(false));
		assert_eq!(parse("0").unwrap(), Value::Int(0));
		assert_eq!(parse("-42").unwrap(), Value::Int(-42));
		assert_eq!(parse("9223372036854775807").unwrap(), Value::Int(i64::MAX));
		assert_eq!(parse("18446744073709551615").unwrap(), Value::UInt(u64::MAX));
		assert!(matches!(parse("18446744073709551616").unwrap(), Value::Float(_)));
		assert_eq!(parse("1.5").unwrap(), Value::Float(1.5));
		assert_eq!(parse("-1e3").unwrap(), Value::Float(-1000.0));
		assert_eq!(parse("2E-2").unwrap(), Value::Float(0.02));
		assert_eq!(parse("1e3").unwrap().as_i64(), Some(1000));
	}

	#[test]
	fn bad_numbers() {
		for bad in ["01", "-", "1.", ".5", "1e", "1e+", "+1", "0x10", "NaN", "Infinity", "-01"] {
			assert!(parse(bad).is_err(), "{bad} should fail");
		}
	}

	#[test]
	fn strings_and_escapes() {
		let v = parse(r#""a\"b\\c\/d\b\f\n\r\té€""#).unwrap();
		assert_eq!(v.as_str(), Some("a\"b\\c/d\u{8}\u{c}\n\r\té€"));
		assert_eq!(parse(r#""😀""#).unwrap().as_str(), Some("\u{1F600}"));
		assert_eq!(parse("\"h\u{e9}llo \u{1F600}\"").unwrap().as_str(), Some("h\u{e9}llo \u{1F600}"));
		for bad in [r#""\ud83d""#, r#""\ude00""#, r#""\ud83dA""#, r#""\x""#, r#""\u12""#, "\"a\nb\"", "\"abc"] {
			assert!(parse(bad).is_err(), "{bad} should fail");
		}
	}

	#[test]
	fn encoding_escapes() {
		let s = Value::from("q\"b\\n\n\u{1}\u{2028}\u{e9}");
		let expected = ["\"q\\\"b\\\\n\\n\\u0001", "\\u2028", "\u{e9}\""].concat();
		assert_eq!(s.to_string(), expected);
		assert_eq!(parse(&s.to_string()).unwrap(), s);
	}

	#[test]
	fn containers() {
		assert_eq!(rt(r#" { "a" : [1, 2, {"b": null}], "c": {} , "d": [] } "#), r#"{"a":[1,2,{"b":null}],"c":{},"d":[]}"#);
		let e = parse("{\"k\": 1,\n \"k\": 2}").unwrap_err();
		assert_eq!(e.to_string(), "duplicate key \"k\" at line 2, column 2");
		assert_eq!(parse("[10, 20]").unwrap().at(1), Some(&Value::Int(20)));
		for bad in ["[1,]", "[1 2]", "{\"a\" 1}", "{\"a\":1,}", "{1:2}", "[", "{", "]", "[1] x"] {
			assert!(parse(bad).is_err(), "{bad} should fail");
		}
	}

	#[test]
	fn non_string_keys_and_tags() {
		let v = Value::Map(vec![
			(Value::Int(1), Value::from("one")),
			(Value::Bool(true), Value::Null),
			(Value::Seq(vec![Value::Int(1), Value::Int(2)]), Value::tagged("!point", Value::Int(3))),
		]);
		assert_eq!(v.to_string(), r#"{"1":"one","true":null,"[1,2]":3}"#);
	}

	#[test]
	fn floats_round_trip() {
		for f in [0.1, 1.0, -2.5, 1e300, 5e-324, 123456789.125, f64::MAX] {
			let text = Value::Float(f).to_string();
			assert_eq!(parse(&text).unwrap(), Value::Float(f), "{text}");
		}
		assert_eq!(Value::Float(f64::NAN).to_string(), "null");
		assert_eq!(Value::Float(1.0).to_string(), "1.0");
	}

	#[test]
	fn pretty_and_inline() {
		let v = Value::map([
			("a", Value::Int(1)),
			("b", Value::Seq(vec![Value::map([("x", Value::Int(1)), ("y", Value::Int(2))]), Value::Null])),
			("c", Value::Map(vec![])),
		]);
		assert_eq!(
			Json::pretty(2).write(&v).unwrap(),
			"{\n  \"a\": 1,\n  \"b\": [\n    {\n      \"x\": 1,\n      \"y\": 2\n    },\n    null\n  ],\n  \"c\": {}\n}"
		);
		assert_eq!(Json::pretty(2).inline_from(2).write(&v).unwrap(), "{\n  \"a\": 1,\n  \"b\": [\n    {\"x\":1,\"y\":2},\n    null\n  ],\n  \"c\": {}\n}");
		assert_eq!(parse(&Json::pretty(4).write(&v).unwrap()).unwrap(), v);
	}

	#[test]
	fn error_positions() {
		let e = parse("{\n  \"a\": tru\n}").unwrap_err();
		assert_eq!(e.to_string(), "expected a JSON value at line 2, column 8");
		let e = parse("[\"\u{e9}\", x]").unwrap_err();
		assert_eq!(e.position(), Some(crate::Position::LineColumn { line: 1, column: 7 }));
		assert!(Json::compact().decode(&[b'"', 0xFF, b'"']).is_err());
	}

	#[test]
	fn depth_limit() {
		let ok = "[".repeat(MAX_DEPTH) + &"]".repeat(MAX_DEPTH);
		assert!(parse(&ok).is_ok());
		let deep = "[".repeat(MAX_DEPTH + 2) + &"]".repeat(MAX_DEPTH + 2);
		let e = parse(&deep).unwrap_err();
		assert_eq!(e.limit(), Some(crate::Limit::Depth));
		assert!(e.message().starts_with("nested too deeply"), "{e}");
		let shallow = Json::default().with_limits(Limits { max_depth: 3, ..Limits::JSON });
		assert!(shallow.read("[[[1]]]").is_ok());
		assert_eq!(shallow.read("[[[[1]]]]").unwrap_err().limit(), Some(crate::Limit::Depth));
		let small = Json::default().with_limits(Limits { max_input_bytes: 4, ..Limits::JSON });
		assert!(small.read("[1]").is_ok());
		assert_eq!(small.read("[1, 2]").unwrap_err().limit(), Some(crate::Limit::InputSize));
	}

	fn tricky() -> Value {
		Value::map([
			("msg", Value::from("a <| b |> c { d } [e] \"f\" « g » \\ h")),
			("list", Value::Seq(vec![Value::map([("x", Value::Int(1))]), Value::Null, Value::Bool(true), Value::Float(-1.5)])),
			("empty", Value::Map(vec![])),
			("none", Value::Seq(vec![])),
		])
	}

	#[test]
	fn swapped_tokens_are_tokens_not_text() {
		let v = tricky();
		let angle = Json::compact().with_tokens(JsonTokens { map_open: "<|", map_close: "|>", ..JsonTokens::STANDARD }).unwrap();
		let text = angle.write(&v).unwrap();
		assert_eq!(text, r#"<|"msg":"a <| b |> c { d } [e] \"f\" « g » \\ h","list":[<|"x":1|>,null,true,-1.5],"empty":<||>,"none":[]|>"#);
		assert_eq!(angle.read(&text).unwrap(), v);
		assert_eq!(angle.read(&Json::pretty(2).with_tokens(*angle.tokens()).unwrap().write(&v).unwrap()).unwrap(), v);
		// Each format only reads its own spelling.
		assert!(angle.read(&v.to_string()).is_err());
		assert!(parse(&text).is_err());

		// The find-and-replace version of the same idea corrupts strings that contain the tokens.
		let naive = v.to_string().replace('{', "<|").replace('}', "|>");
		let back = parse(&naive.replace("<|", "{").replace("|>", "}")).unwrap();
		assert_ne!(back, v);
	}

	#[test]
	fn every_token_can_be_swapped() {
		let tokens = JsonTokens {
			map_open: "(",
			map_close: ")",
			seq_open: "<",
			seq_close: ">",
			comma: ";",
			colon: "=",
			string_open: "«",
			string_close: "»",
			null: "nil",
			bool_true: "yes",
			bool_false: "no",
		};
		let fmt = Json::pretty(2).with_tokens(tokens).unwrap();
		let v = tricky();
		let text = fmt.write(&v).unwrap();
		assert!(text.contains("(\n  «msg»= «a <| b |> c { d } [e] \"f\" « g \\» \\\\ h»;"), "{text}");
		assert!(text.contains("nil") && text.contains("yes"));
		assert_eq!(fmt.read(&text).unwrap(), v);
	}

	#[test]
	fn token_sets_are_checked() {
		let bad = [
			JsonTokens { map_open: "", ..JsonTokens::STANDARD },
			JsonTokens { map_open: "< |", ..JsonTokens::STANDARD },
			// An open token that's a prefix of another value token.
			JsonTokens { map_open: "[", seq_open: "[[", ..JsonTokens::STANDARD },
			// An empty sequence would read as a map.
			JsonTokens { map_open: "]", ..JsonTokens::STANDARD },
			JsonTokens { comma: "]", ..JsonTokens::STANDARD },
			JsonTokens { null: "-", ..JsonTokens::STANDARD },
			JsonTokens { seq_close: "e]", ..JsonTokens::STANDARD },
			JsonTokens { string_close: "n", ..JsonTokens::STANDARD },
			JsonTokens { null: "nul", bool_true: "null!", ..JsonTokens::STANDARD },
		];
		for t in bad {
			assert!(t.check().is_err(), "{t:?} should be rejected");
		}
		// Closing tokens never compete with each other, so they may match.
		assert!(JsonTokens { map_close: ")", seq_close: ")", ..JsonTokens::STANDARD }.check().is_ok());
		assert!(JsonTokens::STANDARD.check().is_ok());
	}

	#[test]
	fn lossy_conversions_are_errors_with_a_path() {
		use crate::ErrorKind;
		let nan = Value::map([("scores", Value::Seq(vec![Value::Float(1.0), Value::Float(f64::NAN)]))]);
		let e = Json::compact().write(&nan).unwrap_err();
		assert_eq!(e.kind(), ErrorKind::Unrepresentable);
		assert!(e.to_string().starts_with("scores[1]: JSON can't write NaN"), "{e}");

		let key = Value::map([("m", Value::Map(vec![(Value::Int(1), Value::Null)]))]);
		let e = Json::compact().write(&key).unwrap_err();
		assert!(e.to_string().starts_with("m.1: JSON keys are strings"), "{e}");

		let tagged = Value::Seq(vec![Value::tagged("!p", Value::Int(1))]);
		assert!(Json::compact().write(&tagged).unwrap_err().to_string().starts_with("[0]: JSON can't write the tag \"!p\""));

		// Each conversion can be allowed on its own, and then happens as documented.
		let some = |l: JsonLossy| Json::compact().lossy(l);
		assert_eq!(some(JsonLossy { non_finite_floats: true, ..JsonLossy::NONE }).write(&nan).unwrap(), r#"{"scores":[1.0,null]}"#);
		assert_eq!(some(JsonLossy { non_string_keys: true, ..JsonLossy::NONE }).write(&key).unwrap(), r#"{"m":{"1":null}}"#);
		assert_eq!(some(JsonLossy { tags: true, ..JsonLossy::NONE }).write(&tagged).unwrap(), "[1]");
		assert!(some(JsonLossy { tags: true, ..JsonLossy::NONE }).write(&nan).is_err());
		// Display never fails: it allows everything.
		assert_eq!(nan.to_string(), r#"{"scores":[1.0,null]}"#);
	}

	#[test]
	fn writing_checks_depth() {
		let mut v = Value::Int(1);
		for _ in 0..4 {
			v = Value::Seq(vec![Value::Null, v]);
		}
		let json = Json::compact().with_limits(Limits { max_depth: 3, ..Limits::JSON });
		let e = json.write(&v).unwrap_err();
		assert_eq!(e.limit(), Some(crate::Limit::Depth));
		// The first value too deep is the null at the start of the innermost sequence.
		assert!(e.to_string().starts_with("[1][1][1][0]: nested too deeply"), "{e}");
		assert!(Json::compact().with_limits(Limits { max_depth: 4, ..Limits::JSON }).write(&v).is_ok());
	}
}
