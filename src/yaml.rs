//! YAML 1.2 as a black-serde [`Format`].
//!
//! # Reading
//!
//! The whole YAML 1.2 language: block and flow collections, compact and indentless block
//! sequences, explicit (`? key`) and implicit keys of any kind, flow pairs (`[a: 1]`),
//! JSON-style adjacent values (`{"a":1}`), plain scalars over several lines, single- and
//! double-quoted scalars with all escapes and line folding, literal `|` and folded `>` block
//! scalars with chomping and indentation indicators, comments, anchors and aliases, tags
//! (shorthand, `%TAG`-declared handles and verbatim `!<...>`), the `%YAML` and `%TAG`
//! directives, and streams of several documents.
//!
//! How YAML maps onto the data model:
//!
//! - Untagged plain scalars resolve with the 1.2 core schema (`null`/`~`/empty, booleans,
//!   decimal/`0x`/`0o` integers, floats including `.inf` and `.nan//! - multi-line strings as literal block scalars (`|`);
//! - strings quoted whenever a reader could take them for something else, including YAML 1.1
//!   readers (`"no"`, `"on"`, `"1_000"`); quoted and block scalars
//!   are strings.
//! - An alias becomes a copy of its anchored value. An alias inside its own anchor would make a
//!   cycle, which a [`Value`] tree can't hold, so it's an error. Expanding more than
//!   [`Limits::max_alias_values`] values through aliases is also an error, which stops "billion
//!   laughs" input.
//! - Merge keys (`<<: *defaults`, or a list of mappings) copy in entries the mapping doesn't
//!   already have, at the `<<` position; earlier sources win. Turn this off with
//!   [`Yaml::merge_keys`].
//! - `!!str`, `!!int`, `!!float`, `!!bool`, `!!null`, `!!seq` and `!!map` set the type,
//!   `!!binary` gives [`Value::Bytes`], `!!omap` gives an (ordered) map, and `!` forces a
//!   string. Every other tag (local `!point`, `!!set`, `!!pairs`, `!!timestamp`, `%TAG` and
//!   verbatim tags) gives [`Value::Tagged`] with the full tag, around a value resolved as usual.
//! - Keys can be any value, including sequences and maps.
//! - [`parse`] reads one document; [`parse_all`] reads a stream of them.
//!
//! # Writing
//!
//! Block style, with:
//!
//! - multi-line strings as literal block scalars (`|`);
//! - strings quoted whenever a reader could take them for something else, including YAML 1.1
//!   readers (`"no"`, `"on"`, `"1_000"`);
//! - bytes as `!!binary` base64; tags as shorthand (`!point`, `!!set`) or verbatim (`!<...>`);
//! - keys that aren't scalars, or are longer than 1024 characters, as explicit `? key` entries;
//! - [`Yaml::flow_from`] to write deep containers in flow style, one `{t: 0, x: 5}` per line;
//! - optionally, with [`Yaml::anchors`], repeated subtrees written once with an anchor and
//!   referred to with aliases afterwards (`defaults: &defaults {...}` ... `base: *defaults`).
//!
//! Writing fails rather than produce text that wouldn't read back as the same value: tags YAML
//! readers would apply rather than keep (`!!int`, `!`) are errors unless [`YamlLossy`] allows
//! dropping them.
//!
//! # Tokens
//!
//! Every indicator comes from [`YamlTokens`] and can be swapped; the reader and writer both use
//! the set. YAML has a single tag per node, so a value tagged twice keeps only the outer tag.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};

use crate::tokens::{check_no_self_overlap, check_prefix_free, check_start, check_text};
use crate::{Deserialize, Error, Format, KeyIndex, Limit, Limits, Serialize, Value, copy_cost, hash_value, same, walk};

/// The default limit on nesting; see [`Limits`]. Reading and writing don't recurse, so this
/// is a policy, not a protection for the stack.
pub const MAX_DEPTH: usize = 256;
/// The prefix of the standard tags, which `!!` abbreviates.
pub const CORE_TAG_PREFIX: &str = "tag:yaml.org,2002:";

// ---------- tokens ----------

/// The spelling of every YAML indicator. Start from [`YamlTokens::STANDARD`] and change the
/// ones you want with struct update syntax; [`Yaml::with_tokens`] checks the result.
///
/// `seq_entry`, `key` and `value` only count as indicators when followed by a space (or, for
/// `value` in a flow collection, by a flow token), as in standard YAML; the document markers
/// only at the start of a line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct YamlTokens {
	pub seq_entry: &'static str,
	pub key: &'static str,
	pub value: &'static str,
	pub flow_entry: &'static str,
	pub flow_seq_open: &'static str,
	pub flow_seq_close: &'static str,
	pub flow_map_open: &'static str,
	pub flow_map_close: &'static str,
	pub comment: &'static str,
	pub anchor: &'static str,
	pub alias: &'static str,
	/// Also forms tag handles: `!!` is this token twice, `!e!` is it around a name.
	pub tag: &'static str,
	pub literal: &'static str,
	pub folded: &'static str,
	pub single_quote: &'static str,
	/// Inside a double-quoted string, this token is written escaped as `\` followed by it.
	pub double_quote: &'static str,
	pub directive: &'static str,
	pub doc_start: &'static str,
	pub doc_end: &'static str,
}

impl YamlTokens {
	pub const STANDARD: YamlTokens = YamlTokens {
		seq_entry: "-",
		key: "?",
		value: ":",
		flow_entry: ",",
		flow_seq_open: "[",
		flow_seq_close: "]",
		flow_map_open: "{",
		flow_map_close: "}",
		comment: "#",
		anchor: "&",
		alias: "*",
		tag: "!",
		literal: "|",
		folded: ">",
		single_quote: "'",
		double_quote: "\"",
		directive: "%",
		doc_start: "---",
		doc_end: "...",
	};

	fn flow(&self) -> [(&'static str, &'static str); 5] {
		[
			("flow_entry", self.flow_entry),
			("flow_seq_open", self.flow_seq_open),
			("flow_seq_close", self.flow_seq_close),
			("flow_map_open", self.flow_map_open),
			("flow_map_close", self.flow_map_close),
		]
	}

	/// Indicators that count wherever they appear.
	fn inline(&self) -> Vec<(&'static str, &'static str)> {
		let mut v = self.flow().to_vec();
		v.extend([
			("comment", self.comment),
			("anchor", self.anchor),
			("alias", self.alias),
			("tag", self.tag),
			("literal", self.literal),
			("folded", self.folded),
			("single_quote", self.single_quote),
			("double_quote", self.double_quote),
			("directive", self.directive),
		]);
		v
	}

	/// Indicators that need a space after them.
	fn spaced(&self) -> [(&'static str, &'static str); 3] {
		[("seq_entry", self.seq_entry), ("key", self.key), ("value", self.value)]
	}

	fn all(&self) -> Vec<(&'static str, &'static str)> {
		let mut v = self.inline();
		v.extend(self.spaced());
		v.extend([("doc_start", self.doc_start), ("doc_end", self.doc_end)]);
		v
	}

	/// Check that the set can be read back unambiguously.
	pub fn check(&self) -> Result<(), Error> {
		for (name, tok) in self.all() {
			check_text("YAML", name, tok)?;
		}
		let mut indicators = self.inline();
		indicators.extend(self.spaced());
		check_prefix_free("YAML", &indicators)?;
		// Document markers only count at the start of a line before a space, so they may share
		// a prefix with other indicators (`-` and `---`), but not be one.
		check_prefix_free("YAML", &[("doc_start", self.doc_start), ("doc_end", self.doc_end)])?;
		for (name, tok) in &indicators {
			for (mname, marker) in [("doc_start", self.doc_start), ("doc_end", self.doc_end)] {
				if tok == &marker {
					return check_prefix_free("YAML", &[(name, tok), (mname, marker)]);
				}
			}
		}
		// Numbers, words such as `null`, anchor names and base64 are written plain, so an
		// indicator that can interrupt them must not start like them.
		for (name, tok) in self.inline() {
			check_start("YAML", name, tok, ALNUM_ETC, "which plain values can contain")?;
		}
		check_start("YAML", "double_quote", self.double_quote, "0abt\tnvfre N_LPxuU\\/", "which would read as an escape after '\\'")?;
		check_start("YAML", "tag", self.tag, "<", "which starts a verbatim tag after the tag token")?;
		check_no_self_overlap("YAML", "double_quote", self.double_quote)?;
		Ok(())
	}
}

const ALNUM_ETC: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789.-+_";

impl Default for YamlTokens {
	fn default() -> Self {
		YamlTokens::STANDARD
	}
}

// ---------- settings ----------

/// What YAML may do with values it can't write as they are. Each is an error (of kind
/// [`Unrepresentable`](crate::ErrorKind::Unrepresentable)) unless allowed here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct YamlLossy {
	/// Tags that wouldn't read back as written, dropped: a second tag on the same value (YAML
	/// gives each value one), a tag on bytes (which need `!!binary`), empty tags and `!`, and
	/// standard tags that readers apply rather than keep (`!!int`, `!!seq`, ...) or that don't
	/// fit the value (`!!set` on a mapping with values).
	pub extra_tags: bool,
}

impl YamlLossy {
	pub const NONE: YamlLossy = YamlLossy { extra_tags: false };
	pub const ALL: YamlLossy = YamlLossy { extra_tags: true };
}

/// YAML settings for reading and writing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Yaml {
	/// Spaces per nesting level (at least 1).
	pub indent: usize,
	/// Containers at this depth or deeper are written in flow style on one line.
	pub flow_from_depth: Option<usize>,
	/// Repeated containers holding at least this many values are written once with an anchor
	/// and then as aliases. `None` writes every copy out in full.
	pub anchor_min_size: Option<usize>,
	/// Write multi-line strings as literal block scalars instead of escaped quoted strings.
	pub block_strings: bool,
	/// Read `<<` keys as merge keys.
	pub merge_keys: bool,
	/// Which lossy conversions writing may make; none by default.
	pub lossy: YamlLossy,
	tokens: YamlTokens,
	limits: Option<Limits>,
}

impl Default for Yaml {
	fn default() -> Self {
		Yaml {
			indent: 2,
			flow_from_depth: None,
			anchor_min_size: None,
			block_strings: true,
			merge_keys: true,
			lossy: YamlLossy::NONE,
			tokens: YamlTokens::STANDARD,
			limits: None,
		}
	}
}

impl Yaml {
	pub fn new() -> Self {
		Yaml::default()
	}

	pub fn indent(mut self, spaces: usize) -> Self {
		self.indent = spaces.max(1);
		self
	}

	/// Write containers at `depth` and deeper in flow style.
	pub fn flow_from(mut self, depth: usize) -> Self {
		self.flow_from_depth = Some(depth);
		self
	}

	/// Anchor repeated containers holding at least `min_size` values (the container counts as
	/// one, and so does each value in it, recursively; keys don't count).
	pub fn anchors(mut self, min_size: usize) -> Self {
		self.anchor_min_size = Some(min_size.max(2));
		self
	}

	/// Write every repeated subtree out in full.
	pub fn no_anchors(mut self) -> Self {
		self.anchor_min_size = None;
		self
	}

	pub fn block_strings(mut self, on: bool) -> Self {
		self.block_strings = on;
		self
	}

	pub fn merge_keys(mut self, on: bool) -> Self {
		self.merge_keys = on;
		self
	}

	/// Allow these lossy conversions when writing.
	pub fn lossy(mut self, lossy: YamlLossy) -> Self {
		self.lossy = lossy;
		self
	}

	/// Use these limits instead of the defaults ([`Limits::YAML`]).
	pub fn with_limits(mut self, limits: Limits) -> Self {
		self.limits = Some(limits);
		self
	}

	/// The limits reading and writing use: those set with [`with_limits`](Yaml::with_limits),
	/// or else [`Limits::YAML`].
	pub fn limits(&self) -> Limits {
		self.limits.unwrap_or(Limits::YAML)
	}

	/// Use a different token set, after checking it.
	pub fn with_tokens(mut self, tokens: YamlTokens) -> Result<Self, Error> {
		tokens.check()?;
		self.tokens = tokens;
		Ok(self)
	}

	pub fn tokens(&self) -> &YamlTokens {
		&self.tokens
	}

	/// Write one document. Fails if the value is nested deeper than `max_depth` or has tags
	/// YAML can't write (see [`YamlLossy`]); the error's path says where.
	pub fn write(&self, value: &Value) -> Result<String, Error> {
		self.check_writable(value)?;
		let mut w = Writer::new(self, value);
		w.document(value, false);
		w.out.push('\n');
		Ok(w.out)
	}

	/// Write a stream of documents, each starting with the document start marker.
	pub fn write_all(&self, docs: &[Value]) -> Result<String, Error> {
		let mut out = String::new();
		for (i, doc) in docs.iter().enumerate() {
			self.check_writable(doc).map_err(|e| e.in_index(i))?;
			let mut w = Writer::new(self, doc);
			w.document(doc, true);
			out.push_str(&w.out);
			out.push('\n');
		}
		Ok(out)
	}

	/// Check, without recursing, that `value` can be written: not too deep, and every tag
	/// writable. The writer can then assume both.
	fn check_writable(&self, value: &Value) -> Result<(), Error> {
		let max_depth = self.limits().max_depth;
		let allow = self.lossy.extra_tags;
		walk(value, max_depth, |v| {
			let Value::Tagged(tag, inner) = v else { return Ok(()) };
			let problem =
				if matches!(**inner, Value::Tagged(..)) { Some("a second tag on the same value (YAML allows one)".to_string()) } else { tag_problem(tag, inner) };
			match problem {
				Some(problem) if !allow => Err(Error::unrepresentable(format!("YAML can't write {problem} (allow dropping such tags with YamlLossy::extra_tags)"))),
				_ => Ok(()),
			}
		})
	}

	/// Read a stream that holds at most one document (none reads as null).
	pub fn read(&self, text: &str) -> Result<Value, Error> {
		let mut c = Composer::new(text, self)?;
		let first = c.next_document()?;
		if let Some(mark) = c.more_documents()? {
			return Err(error_at(mark, "the input has more than one YAML document (read it with parse_all)"));
		}
		Ok(first.unwrap_or(Value::Null))
	}

	/// Read every document in a stream.
	pub fn read_all(&self, text: &str) -> Result<Vec<Value>, Error> {
		let mut c = Composer::new(text, self)?;
		let mut docs = Vec::new();
		while let Some(doc) = c.next_document()? {
			docs.push(doc);
		}
		Ok(docs)
	}
}

impl Format for Yaml {
	fn encode(&self, value: &Value) -> Result<Vec<u8>, Error> {
		self.write(value).map(String::into_bytes)
	}

	fn decode(&self, input: &[u8]) -> Result<Value, Error> {
		self.limits().check_input(input.len())?;
		let text = std::str::from_utf8(input).map_err(|e| Error::new("YAML input is not valid UTF-8").at_offset(e.valid_up_to()))?;
		self.read(text)
	}
}

/// Why `tag` on `content` (untagged) wouldn't read back as written, if it wouldn't. Readers
/// give the standard tags meaning (`!!int` converts, `!!seq` is just a sequence), so only the
/// ones they keep as tags can be written as tags.
fn tag_problem(tag: &str, content: &Value) -> Option<String> {
	if tag.is_empty() {
		return Some("an empty tag".into());
	}
	if tag == "!" {
		return Some("the tag \"!\" (it reads back as no tag)".into());
	}
	if matches!(content, Value::Bytes(_)) {
		return Some("a tag on bytes (they need the !!binary tag)".into());
	}
	let core = tag.strip_prefix(CORE_TAG_PREFIX)?;
	let collection = matches!(content, Value::Seq(_) | Value::Map(_));
	let applied = || Some(format!("the standard tag !!{core}, which readers apply rather than keep"));
	let wrong = || Some(format!("the standard tag !!{core} on {}", content.kind()));
	match core {
		"str" | "null" | "bool" | "int" | "float" | "binary" | "merge" | "seq" | "map" | "omap" => applied(),
		"set" => match content {
			Value::Map(pairs) if pairs.iter().all(|(_, v)| v.is_null()) => None,
			_ => Some("a !!set that isn't a mapping with null values".into()),
		},
		"pairs" => match content {
			Value::Seq(items) if items.iter().all(|i| i.as_map().is_some_and(|m| m.len() == 1) && i.tag().is_none()) => None,
			_ => Some("a !!pairs that isn't a sequence of single-pair mappings".into()),
		},
		"timestamp" if collection => wrong(),
		_ if collection || matches!(content, Value::String(_)) => None,
		// Readers keep other standard tags on scalars, but around the text as a string.
		_ => wrong(),
	}
}

/// Serialize to block-style YAML.
pub fn to_string<T: Serialize + ?Sized>(value: &T) -> Result<String, Error> {
	Yaml::default().write(&value.serialize())
}

/// Deserialize a `T` from a single YAML document.
pub fn from_str<T: Deserialize>(text: &str) -> Result<T, Error> {
	T::deserialize(&parse(text)?)
}

/// Deserialize a `T` from each document in a YAML stream.
pub fn from_str_all<T: Deserialize>(text: &str) -> Result<Vec<T>, Error> {
	parse_all(text)?.iter().enumerate().map(|(i, v)| T::deserialize(v).map_err(|e| e.in_index(i))).collect()
}

/// Parse a single YAML document into a [`Value`].
pub fn parse(text: &str) -> Result<Value, Error> {
	Yaml::default().read(text)
}

/// Parse every document in a YAML stream.
pub fn parse_all(text: &str) -> Result<Vec<Value>, Error> {
	Yaml::default().read_all(text)
}

// ---------- scalar resolution (YAML 1.2 core schema) ----------

/// Resolve an untagged plain scalar with the core schema.
pub(crate) fn resolve_plain(s: &str) -> Value {
	match s {
		"" | "~" | "null" | "Null" | "NULL" => return Value::Null,
		"true" | "True" | "TRUE" => return Value::Bool(true),
		"false" | "False" | "FALSE" => return Value::Bool(false),
		".inf" | ".Inf" | ".INF" | "+.inf" | "+.Inf" | "+.INF" => return Value::Float(f64::INFINITY),
		"-.inf" | "-.Inf" | "-.INF" => return Value::Float(f64::NEG_INFINITY),
		".nan" | ".NaN" | ".NAN" => return Value::Float(f64::NAN),
		_ => {}
	}
	if let Some(hex) = s.strip_prefix("0x")
		&& !hex.is_empty()
		&& hex.bytes().all(|b| b.is_ascii_hexdigit())
	{
		return u64::from_str_radix(hex, 16).map_or_else(|_| Value::String(s.to_string()), Value::from);
	}
	if let Some(oct) = s.strip_prefix("0o")
		&& !oct.is_empty()
		&& oct.bytes().all(|b| (b'0'..=b'7').contains(&b))
	{
		return u64::from_str_radix(oct, 8).map_or_else(|_| Value::String(s.to_string()), Value::from);
	}
	let digits = s.strip_prefix(['-', '+']).unwrap_or(s);
	if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
		let t = s.strip_prefix('+').unwrap_or(s);
		if let Ok(i) = t.parse::<i64>() {
			return Value::Int(i);
		}
		if let Ok(u) = t.parse::<u64>() {
			return Value::UInt(u);
		}
		return t.parse::<f64>().map_or_else(|_| Value::String(s.to_string()), Value::Float);
	}
	if is_float_syntax(digits) {
		return s.strip_prefix('+').unwrap_or(s).parse::<f64>().map_or_else(|_| Value::String(s.to_string()), Value::Float);
	}
	Value::String(s.to_string())
}

/// `(\.[0-9]+ | [0-9]+(\.[0-9]*)?)([eE][-+]?[0-9]+)?`, with the sign already removed.
fn is_float_syntax(s: &str) -> bool {
	let b = s.as_bytes();
	let mut i = 0;
	let int_digits = b.iter().take_while(|c| c.is_ascii_digit()).count();
	i += int_digits;
	let mut frac_digits = 0;
	if b.get(i) == Some(&b'.') {
		i += 1;
		frac_digits = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
		i += frac_digits;
	}
	if int_digits == 0 && frac_digits == 0 {
		return false;
	}
	if matches!(b.get(i), Some(b'e' | b'E')) {
		i += 1;
		if matches!(b.get(i), Some(b'+' | b'-')) {
			i += 1;
		}
		let exp = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
		if exp == 0 {
			return false;
		}
		i += exp;
	}
	i == b.len()
}

// ---------- base64, for !!binary ----------

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(data: &[u8]) -> String {
	let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
	for chunk in data.chunks(3) {
		let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
		for i in 0..4 {
			if i <= chunk.len() {
				out.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
			} else {
				out.push('=');
			}
		}
	}
	out
}

/// Decode base64, ignoring whitespace and line breaks (block scalars wrap it).
fn base64_decode(text: &str) -> Option<Vec<u8>> {
	let digits: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
	let body = digits.strip_suffix(b"==").or_else(|| digits.strip_suffix(b"=")).unwrap_or(&digits);
	if body.len() == digits.len() && digits.len() % 4 == 1 {
		return None;
	}
	if digits.len() != body.len() && !digits.len().is_multiple_of(4) {
		return None;
	}
	let mut out = Vec::with_capacity(body.len() * 3 / 4);
	let (mut acc, mut bits) = (0u32, 0);
	for &b in body {
		let v = B64.iter().position(|&c| c == b)? as u32;
		acc = acc << 6 | v;
		bits += 6;
		if bits >= 8 {
			bits -= 8;
			out.push((acc >> bits) as u8);
			acc &= (1 << bits) - 1;
		}
	}
	Some(out)
}

// ---------- writing ----------

enum Shape {
	Inline,
	Block,
	Literal,
}

fn ptr(v: &Value) -> *const Value {
	v as *const Value
}

/// Measure every node, without recursing: a hash consistent with `same`, and its size (values
/// in it, including itself, not counting keys).
fn measure(root: &Value) -> HashMap<*const Value, (u64, usize)> {
	let mut info: HashMap<*const Value, (u64, usize)> = HashMap::new();
	// Post-order: a container is measured once its children have been.
	let mut stack: Vec<(&Value, bool)> = vec![(root, false)];
	while let Some((v, children_done)) = stack.pop() {
		if !children_done {
			let start = stack.len();
			match v {
				Value::Seq(items) => stack.extend(items.iter().map(|i| (i, false))),
				Value::Map(pairs) => stack.extend(pairs.iter().map(|(_, i)| (i, false))),
				Value::Tagged(_, inner) => stack.push((inner, false)),
				_ => {}
			}
			if stack.len() > start || matches!(v, Value::Seq(_) | Value::Map(_)) {
				stack.insert(start, (v, true));
				continue;
			}
		}
		let mut h = DefaultHasher::new();
		std::mem::discriminant(v).hash(&mut h);
		let size = match v {
			Value::Seq(items) => {
				let mut n = 1;
				for item in items {
					let (ih, is) = info[&ptr(item)];
					ih.hash(&mut h);
					n += is;
				}
				n
			}
			Value::Map(pairs) => {
				let mut n = 1;
				for (k, item) in pairs {
					hash_value(k, &mut h);
					let (ih, is) = info[&ptr(item)];
					ih.hash(&mut h);
					n += is;
				}
				n
			}
			Value::Tagged(tag, inner) => {
				tag.hash(&mut h);
				let (ih, is) = info[&ptr(inner)];
				ih.hash(&mut h);
				is
			}
			scalar => {
				hash_value(scalar, &mut h);
				1
			}
		};
		info.insert(ptr(v), (h.finish(), size));
	}
	info
}

/// Decides which subtrees get anchors and which are written as aliases, walking in the order
/// the writer writes, so every anchor comes before its aliases and nothing inside an aliased
/// copy is counted.
struct Planner<'a> {
	info: HashMap<*const Value, (u64, usize)>,
	min: usize,
	seen: HashMap<u64, Vec<(&'a Value, &'a str)>>,
	names: HashSet<String>,
	anchors: HashMap<*const Value, String>,
	aliases: HashMap<*const Value, *const Value>,
}

impl<'a> Planner<'a> {
	/// Walk `root` in writing order, without recursing; each value comes with a hint for its
	/// anchor's name (the key it's under).
	fn plan(&mut self, root: &'a Value) {
		let mut stack: Vec<(&'a Value, &'a str)> = vec![(root, "root")];
		while let Some((v, hint)) = stack.pop() {
			let (hash, size) = self.info[&ptr(v)];
			let content = v.untagged();
			let container = match content {
				Value::Seq(s) => !s.is_empty(),
				Value::Map(m) => !m.is_empty(),
				_ => false,
			};
			if size < self.min || !container {
				continue;
			}
			let found = self.seen.get(&hash).and_then(|b| b.iter().find(|(f, _)| same(f, v))).map(|&(f, h)| (ptr(f), h));
			if let Some((first, first_hint)) = found {
				if !self.anchors.contains_key(&first) {
					let name = self.unique_name(first_hint);
					self.anchors.insert(first, name);
				}
				self.aliases.insert(ptr(v), first);
				continue;
			}
			self.seen.entry(hash).or_default().push((v, hint));
			match content {
				Value::Seq(items) => stack.extend(items.iter().rev().map(|i| (i, "item"))),
				Value::Map(pairs) => stack.extend(pairs.iter().rev().map(|(k, i)| (i, k.as_str().unwrap_or("value")))),
				_ => {}
			}
		}
	}

	fn unique_name(&mut self, hint: &str) -> String {
		let mut base: String = hint.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').take(32).collect();
		if base.is_empty() {
			base = "node".into();
		}
		let mut name = base.clone();
		let mut n = 2;
		while self.names.contains(&name) {
			name = format!("{base}_{n}");
			n += 1;
		}
		self.names.insert(name.clone());
		name
	}
}

/// A piece of output still to be written. The writer keeps these on a heap stack instead of
/// recursing; output is only ever appended, so running them in order gives the same text a
/// recursive writer would.
enum Task<'a> {
	Text(&'static str),
	Owned(String),
	Newline(usize),
	/// A value in flow style.
	Flow(&'a Value),
	/// The body of a non-empty block collection whose first line starts at column `col` (the
	/// indentation and properties are already written).
	Block {
		c: &'a Value,
		col: usize,
		depth: usize,
	},
	/// A block sequence item, after its `- `, with its content at column `child`.
	Item {
		item: &'a Value,
		child: usize,
		depth: usize,
	},
	/// A block mapping entry at column `col`.
	Entry {
		k: &'a Value,
		item: &'a Value,
		col: usize,
		depth: usize,
	},
}

struct Writer<'a> {
	o: &'a Yaml,
	out: String,
	anchors: HashMap<*const Value, String>,
	aliases: HashMap<*const Value, *const Value>,
}

impl<'a> Writer<'a> {
	fn new(o: &'a Yaml, root: &'a Value) -> Self {
		let (anchors, aliases) = match o.anchor_min_size {
			Some(min) => {
				let mut p = Planner { info: measure(root), min, seen: HashMap::new(), names: HashSet::new(), anchors: HashMap::new(), aliases: HashMap::new() };
				p.plan(root);
				(p.anchors, p.aliases)
			}
			None => (HashMap::new(), HashMap::new()),
		};
		Writer { o, out: String::new(), anchors, aliases }
	}

	fn t(&self) -> &YamlTokens {
		&self.o.tokens
	}

	fn alias_name(&self, v: &Value) -> Option<&str> {
		let first = self.aliases.get(&ptr(v))?;
		self.anchors.get(first).map(String::as_str)
	}

	fn document(&mut self, v: &'a Value, marker: bool) {
		match self.shape(v, 0, false) {
			Shape::Block => {
				let props = self.props(v);
				if marker || !props.is_empty() {
					self.out.push_str(self.o.tokens.doc_start);
					if !props.is_empty() {
						self.out.push(' ');
						self.out.push_str(&props);
					}
					self.out.push('\n');
				}
				self.run(Task::Block { c: v.untagged(), col: 0, depth: 0 });
			}
			_ => {
				if marker {
					self.out.push_str(self.o.tokens.doc_start);
					self.out.push(' ');
				}
				self.run(Task::Flow(v));
			}
		}
	}

	/// Run `first` and every task it adds.
	fn run(&mut self, first: Task<'a>) {
		let mut tasks = vec![first];
		while let Some(task) = tasks.pop() {
			match task {
				Task::Text(s) => self.out.push_str(s),
				Task::Owned(s) => self.out.push_str(&s),
				Task::Newline(col) => self.newline(col),
				Task::Flow(v) => self.flow(v, &mut tasks),
				Task::Block { c, col, depth } => self.block(c, col, depth, &mut tasks),
				Task::Item { item, child, depth } => self.item(item, child, depth, &mut tasks),
				Task::Entry { k, item, col, depth } => {
					self.block_key(k, col);
					self.block_value(item, col + self.o.indent, depth + 1, &mut tasks);
				}
			}
		}
	}

	/// A value written in flow style on its own, such as a key.
	fn flow_text(&mut self, v: &'a Value) -> String {
		let saved = std::mem::take(&mut self.out);
		self.run(Task::Flow(v));
		std::mem::replace(&mut self.out, saved)
	}

	fn shape(&self, v: &Value, depth: usize, literal_ok: bool) -> Shape {
		if self.alias_name(v).is_some() || self.o.flow_from_depth.is_some_and(|d| depth >= d) {
			return Shape::Inline;
		}
		match v.untagged() {
			Value::Seq(s) if !s.is_empty() => Shape::Block,
			Value::Map(m) if !m.is_empty() => Shape::Block,
			Value::String(s) if literal_ok && self.o.block_strings && literal_eligible(s) => Shape::Literal,
			_ => Shape::Inline,
		}
	}

	/// The anchor and tag to write before a value, such as `&base !point`.
	fn props(&self, v: &Value) -> String {
		let t = self.t();
		let mut p = String::new();
		if let Some(name) = self.anchors.get(&ptr(v)) {
			p.push_str(t.anchor);
			p.push_str(name);
		}
		let tag = match v {
			// Tags that wouldn't read back are dropped (check_writable allowed that).
			Value::Tagged(tag, inner) if tag_problem(tag, inner.untagged()).is_none() => Some(self.tag_text(tag)),
			_ if matches!(v.untagged(), Value::Bytes(_)) => Some(format!("{0}{0}binary", t.tag)),
			_ => None,
		};
		if let Some(tag) = tag {
			if !p.is_empty() {
				p.push(' ');
			}
			p.push_str(&tag);
		}
		p
	}

	fn tag_text(&self, tag: &str) -> String {
		let t = self.t();
		let mark = t.tag;
		let shorthand = |suffix: &str| {
			let s = escape_tag(suffix, false);
			let clashes = s.contains(mark) || t.flow().iter().any(|(_, f)| s.contains(f));
			(!s.is_empty() && !clashes).then_some(s)
		};
		if let Some(suffix) = tag.strip_prefix(CORE_TAG_PREFIX)
			&& let Some(s) = shorthand(suffix)
		{
			return format!("{mark}{mark}{s}");
		}
		if let Some(suffix) = tag.strip_prefix('!')
			&& let Some(s) = shorthand(suffix)
		{
			return format!("{mark}{s}");
		}
		format!("{mark}<{}>", escape_tag(tag, true))
	}

	fn newline(&mut self, col: usize) {
		self.out.push('\n');
		self.out.extend(std::iter::repeat_n(' ', col));
	}

	/// Queue the entries or items of a non-empty block collection.
	fn block(&mut self, c: &'a Value, col: usize, depth: usize, tasks: &mut Vec<Task<'a>>) {
		let t = *self.t();
		match c {
			Value::Map(pairs) => {
				for (i, (k, item)) in pairs.iter().enumerate().rev() {
					tasks.push(Task::Entry { k, item, col, depth });
					if i > 0 {
						tasks.push(Task::Newline(col));
					}
				}
			}
			Value::Seq(items) => {
				let child = col + t.seq_entry.chars().count() + 1;
				for (i, item) in items.iter().enumerate().rev() {
					tasks.push(Task::Item { item, child, depth: depth + 1 });
					tasks.push(Task::Text(" "));
					tasks.push(Task::Text(t.seq_entry));
					if i > 0 {
						tasks.push(Task::Newline(col));
					}
				}
			}
			other => tasks.push(Task::Flow(other)),
		}
	}

	/// A block sequence item, after its `- `.
	fn item(&mut self, item: &'a Value, child: usize, depth: usize, tasks: &mut Vec<Task<'a>>) {
		match self.shape(item, depth, true) {
			Shape::Inline => tasks.push(Task::Flow(item)),
			Shape::Block => {
				let props = self.props(item);
				if !props.is_empty() {
					self.out.push_str(&props);
					self.newline(child);
				}
				tasks.push(Task::Block { c: item.untagged(), col: child, depth });
			}
			Shape::Literal => self.literal(item, child),
		}
	}

	/// A key, followed by the value indicator.
	fn block_key(&mut self, k: &'a Value, col: usize) {
		let t = *self.t();
		let text = self.flow_text(k);
		if implicit_key(k) && text.chars().count() <= MAX_IMPLICIT_KEY {
			self.out.push_str(&text);
			self.out.push_str(t.value);
			return;
		}
		self.out.push_str(t.key);
		self.out.push(' ');
		self.out.push_str(&text);
		self.newline(col);
		self.out.push_str(t.value);
	}

	/// A mapping value, after its key's value indicator.
	fn block_value(&mut self, item: &'a Value, child: usize, depth: usize, tasks: &mut Vec<Task<'a>>) {
		match self.shape(item, depth, true) {
			Shape::Inline => {
				self.out.push(' ');
				tasks.push(Task::Flow(item));
			}
			Shape::Block => {
				let props = self.props(item);
				if !props.is_empty() {
					self.out.push(' ');
					self.out.push_str(&props);
				}
				self.newline(child);
				tasks.push(Task::Block { c: item.untagged(), col: child, depth });
			}
			Shape::Literal => {
				self.out.push(' ');
				self.literal(item, child);
			}
		}
	}

	/// A multi-line string as a literal block scalar with its content at column `col`.
	fn literal(&mut self, v: &Value, col: usize) {
		let props = self.props(v);
		if !props.is_empty() {
			self.out.push_str(&props);
			self.out.push(' ');
		}
		let s = v.as_str().expect("literal shape is a string");
		let body = s.trim_end_matches('\n');
		let trailing = s.len() - body.len();
		self.out.push_str(self.o.tokens.literal);
		match trailing {
			0 => self.out.push('-'),
			1 => {}
			_ => self.out.push('+'),
		}
		for line in body.split('\n') {
			self.out.push('\n');
			if !line.is_empty() {
				self.out.extend(std::iter::repeat_n(' ', col));
				self.out.push_str(line);
			}
		}
		for _ in 1..trailing {
			self.out.push('\n');
		}
	}

	/// Write a value in flow style: scalars at once, collections as queued tasks.
	fn flow(&mut self, v: &'a Value, tasks: &mut Vec<Task<'a>>) {
		let t = *self.t();
		if let Some(name) = self.alias_name(v) {
			let name = name.to_string();
			self.out.push_str(t.alias);
			self.out.push_str(&name);
			return;
		}
		let props = self.props(v);
		if !props.is_empty() {
			self.out.push_str(&props);
			self.out.push(' ');
		}
		match v.untagged() {
			Value::Null => self.out.push_str("null"),
			Value::Bool(b) => self.out.push_str(if *b { "true" } else { "false" }),
			Value::Int(i) => {
				self.out.push_str(&format!("{i}"));
			}
			Value::UInt(u) => {
				self.out.push_str(&format!("{u}"));
			}
			Value::Float(f) if f.is_nan() => self.out.push_str(".nan"),
			Value::Float(f) if f.is_infinite() => self.out.push_str(if *f > 0.0 { ".inf" } else { "-.inf" }),
			Value::Float(f) => {
				self.out.push_str(&format!("{f:?}"));
			}
			Value::String(s) => self.string(s),
			Value::Bytes(b) => {
				let text = base64_encode(b);
				if !text.is_empty() && plain_text_ok(&text, &t) {
					self.out.push_str(&text);
				} else {
					self.quoted(&text);
				}
			}
			Value::Seq(items) => {
				self.out.push_str(t.flow_seq_open);
				tasks.push(Task::Text(t.flow_seq_close));
				for (i, item) in items.iter().enumerate().rev() {
					tasks.push(Task::Flow(item));
					if i > 0 {
						tasks.push(Task::Text(" "));
						tasks.push(Task::Text(t.flow_entry));
					}
				}
			}
			Value::Map(pairs) => {
				self.out.push_str(t.flow_map_open);
				tasks.push(Task::Text(t.flow_map_close));
				// Each entry, pushed in reverse: [", "] key ": " value, or with an explicit
				// key, [", "] "? " key " : " value.
				for (i, (k, item)) in pairs.iter().enumerate().rev() {
					tasks.push(Task::Flow(item));
					tasks.push(Task::Text(" "));
					tasks.push(Task::Text(t.value));
					// A scalar key is written now, to check its length: an implicit key can't be
					// longer than 1024 characters. (Writing scalars never recurses.)
					let scalar = implicit_key(k).then(|| self.flow_text(k));
					match scalar {
						Some(text) if text.chars().count() <= MAX_IMPLICIT_KEY => tasks.push(Task::Owned(text)),
						_ => {
							tasks.push(Task::Text(" "));
							tasks.push(scalar.map_or(Task::Flow(k), Task::Owned));
							tasks.push(Task::Text(" "));
							tasks.push(Task::Text(t.key));
						}
					}
					if i > 0 {
						tasks.push(Task::Text(" "));
						tasks.push(Task::Text(t.flow_entry));
					}
				}
			}
			Value::Tagged(..) => unreachable!("untagged"),
		}
	}

	fn string(&mut self, s: &str) {
		if plain_ok(s, self.t()) {
			self.out.push_str(s);
		} else {
			self.quoted(s);
		}
	}

	fn quoted(&mut self, s: &str) {
		let dq = self.o.tokens.double_quote;
		self.out.push_str(dq);
		let mut rest = s;
		while let Some(c) = rest.chars().next() {
			if rest.starts_with(dq) {
				self.out.push('\\');
				self.out.push_str(dq);
				rest = &rest[dq.len()..];
				continue;
			}
			match c {
				'\\' => self.out.push_str("\\\\"),
				'\n' => self.out.push_str("\\n"),
				'\t' => self.out.push_str("\\t"),
				'\r' => self.out.push_str("\\r"),
				'\0' => self.out.push_str("\\0"),
				'\u{85}' => self.out.push_str("\\N"),
				'\u{2028}' => self.out.push_str("\\L"),
				'\u{2029}' => self.out.push_str("\\P"),
				'\u{FEFF}' => self.out.push_str("\\uFEFF"),
				c if c.is_control() => {
					self.out.push_str(&format!("\\x{:02X}", c as u32));
				}
				// Characters YAML doesn't allow in its text (U+FFFE, U+FFFF).
				c if !is_printable(c) => {
					self.out.push_str(&format!("\\u{:04X}", c as u32));
				}
				c => self.out.push(c),
			}
			rest = &rest[c.len_utf8()..];
		}
		self.out.push_str(dq);
	}
}

/// The longest an implicit key (`key: value`) may be, in characters.
const MAX_IMPLICIT_KEY: usize = 1024;

/// Keys that can be written as `key: value`; others use the explicit `? key` form.
fn implicit_key(k: &Value) -> bool {
	!matches!(k.untagged(), Value::Seq(_) | Value::Map(_))
}

/// A string written as a literal block scalar reads back identically.
fn literal_eligible(s: &str) -> bool {
	s.contains('\n')
		&& !s.starts_with([' ', '\t', '\n'])
		&& !s.chars().any(|c| (c.is_control() && c != '\n' && c != '\t') || matches!(c, '\u{FEFF}' | '\u{2028}' | '\u{2029}') || !is_printable(c))
}

/// Whether `s` can be written unquoted and read back as the same text, in both block and flow
/// context.
fn plain_text_ok(s: &str, t: &YamlTokens) -> bool {
	let Some(first) = s.chars().next() else { return false };
	if first.is_whitespace() || s.ends_with(char::is_whitespace) || "@`".contains(first) {
		return false;
	}
	if t.all().iter().any(|(_, tok)| s.starts_with(tok)) {
		return false;
	}
	let flow = t.flow();
	for (i, c) in s.char_indices() {
		if c.is_control() || !is_printable(c) || matches!(c, '\u{FEFF}' | '\u{2028}' | '\u{2029}') {
			return false;
		}
		let rest = &s[i..];
		if flow.iter().any(|(_, f)| rest.starts_with(f)) {
			return false;
		}
		if let Some(after) = rest.strip_prefix(t.value)
			&& (after.is_empty() || after.starts_with([' ', '\t']) || flow.iter().any(|(_, f)| after.starts_with(f)))
		{
			return false;
		}
		if (c == ' ' || c == '\t') && rest[1..].starts_with(t.comment) {
			return false;
		}
	}
	true
}

/// Whether the string `s` can be written plain: safe text that also resolves to a string.
fn plain_ok(s: &str, t: &YamlTokens) -> bool {
	plain_text_ok(s, t) && s != "<<" && matches!(resolve_plain(s), Value::String(_)) && !yaml_1_1_scalar(s)
}

/// Text that YAML 1.1 readers (still common: PyYAML, older libraries) would read as something
/// other than a string, although 1.2 reads it as one: `yes`, `no`, `on`, `off`, and
/// numbers with `_`, `0b`, a leading `0`, or `:` (`1_000`, `0b101`, `012`, `1:30`). Writing
/// these quoted keeps them strings for every reader.
fn yaml_1_1_scalar(s: &str) -> bool {
	// Not `y` and `n`: PyYAML reads them as strings, and quoting every `y` coordinate would
	// be a high price for the few readers that don't.
	const WORDS: [&str; 4] = ["yes", "no", "on", "off"];
	if WORDS.iter().any(|w| s.eq_ignore_ascii_case(w)) {
		return true;
	}
	let body = s.strip_prefix(['-', '+']).unwrap_or(s);
	body.starts_with(|c: char| c.is_ascii_digit() || c == '.') && body.chars().all(|c| c.is_ascii_hexdigit() || "_:.+-xXoObB".contains(c))
}

/// Whether YAML allows the character in its text (others must be escaped in a double-quoted
/// string): tab, line breaks, and printable Unicode except the BOM-like U+FFFE and U+FFFF.
fn is_printable(c: char) -> bool {
	matches!(c, '\t' | '\n' | '\r' | '\u{85}')
		|| (' '..='~').contains(&c)
		|| ('\u{A0}'..='\u{D7FF}').contains(&c)
		|| ('\u{E000}'..='\u{FFFD}').contains(&c)
		|| c >= '\u{10000}'
}

/// Percent-encode the characters a tag can't hold as they are.
fn escape_tag(s: &str, verbatim: bool) -> String {
	let mut out = String::new();
	for c in s.chars() {
		let ok = c.is_ascii_alphanumeric() || "-;/?:@&=+$_.~*'()#".contains(c) || (verbatim && "!,[]".contains(c));
		if ok {
			out.push(c);
		} else {
			let mut buf = [0u8; 4];
			for b in c.encode_utf8(&mut buf).bytes() {
				out.push_str(&format!("%{b:02X}"));
			}
		}
	}
	out
}

// ---------- scanning ----------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum K {
	StreamEnd,
	YamlDirective,
	TagDirective,
	OtherDirective,
	DocStart,
	DocEnd,
	BlockSeqStart,
	BlockMapStart,
	BlockEnd,
	FlowSeqStart,
	FlowSeqEnd,
	FlowMapStart,
	FlowMapEnd,
	BlockEntry,
	FlowEntry,
	Key,
	Value,
	Alias,
	Anchor,
	Tag,
	Scalar,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Style {
	Plain,
	Single,
	Double,
	Literal,
	Folded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Mark {
	line: usize,
	col: usize,
}

fn error_at(m: Mark, msg: impl Into<String>) -> Error {
	Error::new(msg).at_line(m.line + 1, m.col + 1)
}

#[derive(Clone, Debug)]
struct Token {
	kind: K,
	mark: Mark,
	/// Scalar text, anchor or alias name, tag suffix, `%YAML` version or `%TAG` prefix.
	text: String,
	/// A tag's handle in standard form (`!`, `!!` or `!name!`; empty for verbatim tags).
	handle: String,
	style: Style,
}

impl Token {
	fn new(kind: K, mark: Mark) -> Self {
		Token { kind, mark, text: String::new(), handle: String::new(), style: Style::Plain }
	}
}

#[derive(Clone, Copy, Debug)]
struct SimpleKey {
	token_number: usize,
	required: bool,
	index: usize,
	mark: Mark,
	/// A tab that worked as indentation before the key, if there was one.
	tab: Option<Mark>,
}

/// The token set, with the indicators that count wherever they appear gathered up.
struct Toks {
	seq_entry: &'static str,
	key: &'static str,
	value: &'static str,
	flow_entry: &'static str,
	flow_seq_open: &'static str,
	flow_seq_close: &'static str,
	flow_map_open: &'static str,
	flow_map_close: &'static str,
	comment: &'static str,
	anchor: &'static str,
	alias: &'static str,
	tag: &'static str,
	literal: &'static str,
	folded: &'static str,
	single_quote: &'static str,
	double_quote: &'static str,
	directive: &'static str,
	doc_start: &'static str,
	doc_end: &'static str,
	/// Every indicator that counts wherever it appears.
	inline: Vec<&'static str>,
}

impl Toks {
	fn new(t: &YamlTokens) -> Self {
		Toks {
			seq_entry: t.seq_entry,
			key: t.key,
			value: t.value,
			flow_entry: t.flow_entry,
			flow_seq_open: t.flow_seq_open,
			flow_seq_close: t.flow_seq_close,
			flow_map_open: t.flow_map_open,
			flow_map_close: t.flow_map_close,
			comment: t.comment,
			anchor: t.anchor,
			alias: t.alias,
			tag: t.tag,
			literal: t.literal,
			folded: t.folded,
			single_quote: t.single_quote,
			double_quote: t.double_quote,
			directive: t.directive,
			doc_start: t.doc_start,
			doc_end: t.doc_end,
			inline: t.inline().iter().map(|(_, s)| *s).collect(),
		}
	}
}

fn is_break(c: char) -> bool {
	c == '\n' || c == '\r'
}

fn is_blank(c: char) -> bool {
	c == ' ' || c == '\t'
}

/// Blank, line break, or the end of input (`\0`, which can't appear in the input itself).
fn is_blankz(c: char) -> bool {
	is_blank(c) || is_break(c) || c == '\0'
}

fn is_breakz(c: char) -> bool {
	is_break(c) || c == '\0'
}

/// Hex digits and nothing else (`from_str_radix` would also take a sign).
fn parse_hex(s: &str) -> Option<u32> {
	if s.is_empty() || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
		return None;
	}
	u32::from_str_radix(s, 16).ok()
}

/// Turns text into tokens, following the structure of libyaml's scanner: block structure
/// becomes explicit start/end tokens, and a key token is inserted before a scalar (or flow
/// collection, anchor, tag) once the ':' after it is found.
///
/// It reads the text in place: positions are byte offsets into it, so scanning needs no
/// copy of the input.
struct Scanner<'a> {
	src: &'a str,
	/// The byte offset of the next character.
	pos: usize,
	/// The number of characters before `pos`, used to measure keys in characters.
	chars_before: usize,
	/// The byte offset where the current line starts.
	line_start: usize,
	line: usize,
	/// The column in characters.
	col: usize,
	tk: Toks,
	done: bool,
	flow_level: usize,
	tokens: VecDeque<Token>,
	tokens_taken: usize,
	indent: isize,
	indents: Vec<isize>,
	allow_simple_key: bool,
	simple_keys: Vec<Option<SimpleKey>>,
	/// The last token was a quoted scalar or the end of a flow collection, after which a ':'
	/// in a flow collection is a value indicator even without a space.
	adjacent_value: bool,
	/// The last token was a plain scalar that continued onto another line.
	plain_multiline: bool,
	/// A tab before the current token that works as indentation (see `skip_to_next_token`).
	tab_mark: Option<Mark>,
	/// For each open flow collection, whether it's a mapping.
	flow_maps: Vec<bool>,
	/// Pending simple keys as (flow level, index), in the order they were saved.
	key_order: VecDeque<(usize, usize)>,
	/// The line the one-line rule for keys was last checked on.
	checked_line: usize,
}

impl<'a> Scanner<'a> {
	fn new(text: &'a str, tokens: &YamlTokens) -> Result<Self, Error> {
		let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
		let (mut line, mut col) = (0, 0);
		for c in text.chars() {
			if !is_printable(c) {
				return Err(error_at(
					Mark { line, col },
					format!("the character U+{:04X} isn't allowed in YAML (write it as an escape in a double-quoted string)", c as u32),
				));
			}
			if c == '\n' {
				line += 1;
				col = 0;
			} else {
				col += 1;
			}
		}
		Ok(Scanner {
			src: text,
			pos: 0,
			chars_before: 0,
			line_start: 0,
			line: 0,
			col: 0,
			tk: Toks::new(tokens),
			done: false,
			flow_level: 0,
			tokens: VecDeque::new(),
			tokens_taken: 0,
			indent: -1,
			indents: Vec::new(),
			allow_simple_key: true,
			simple_keys: vec![None],
			adjacent_value: false,
			plain_multiline: false,
			tab_mark: None,
			flow_maps: Vec::new(),
			key_order: VecDeque::new(),
			checked_line: 0,
		})
	}

	// --- token queue ---

	fn peek(&mut self) -> Result<&Token, Error> {
		self.fill()?;
		Ok(self.tokens.front().expect("filled"))
	}

	fn next(&mut self) -> Result<Token, Error> {
		self.fill()?;
		let t = self.tokens.pop_front().expect("filled");
		self.tokens_taken += 1;
		if t.kind == K::StreamEnd {
			self.tokens.push_front(t.clone());
		}
		Ok(t)
	}

	fn fill(&mut self) -> Result<(), Error> {
		loop {
			if self.done {
				return Ok(());
			}
			if !self.tokens.is_empty() {
				self.stale_simple_keys()?;
				let next_key = self.oldest_key().map(|(_, k)| k.token_number);
				if next_key != Some(self.tokens_taken) {
					return Ok(());
				}
			}
			self.fetch_more()?;
		}
	}

	fn push(&mut self, tok: Token) {
		self.adjacent_value = matches!(tok.kind, K::FlowSeqEnd | K::FlowMapEnd) || (tok.kind == K::Scalar && matches!(tok.style, Style::Single | Style::Double));
		self.plain_multiline = false;
		self.tokens.push_back(tok);
	}

	// --- characters ---

	fn peek_c(&self) -> char {
		self.src[self.pos..].chars().next().unwrap_or('\0')
	}

	/// The character `off` bytes ahead, or `\0` past the end (or inside a character, which
	/// callers only reach when what they're looking for isn't there anyway).
	fn peek_at(&self, off: usize) -> char {
		self.src.get(self.pos + off..).and_then(|s| s.chars().next()).unwrap_or('\0')
	}

	/// The length in bytes of the character `off` bytes ahead (1 past the end).
	fn char_len_at(&self, off: usize) -> usize {
		self.peek_at(off).len_utf8()
	}

	/// The character before this point, or `\0` at the start.
	fn prev_c(&self) -> char {
		self.src[..self.pos].chars().next_back().unwrap_or('\0')
	}

	fn at_end(&self) -> bool {
		self.pos >= self.src.len()
	}

	fn mark(&self) -> Mark {
		Mark { line: self.line, col: self.col }
	}

	fn error(&self, msg: impl Into<String>) -> Error {
		error_at(self.mark(), msg)
	}

	fn forward(&mut self) {
		let c = self.peek_c();
		if c == '\0' {
			return;
		}
		self.pos += c.len_utf8();
		self.chars_before += 1;
		match c {
			'\n' => self.new_line(),
			'\r' if self.peek_c() != '\n' => self.new_line(),
			_ => self.col += 1,
		}
	}

	fn new_line(&mut self) {
		self.line += 1;
		self.col = 0;
		self.line_start = self.pos;
	}

	/// Move forward `n` bytes (always a whole number of characters).
	fn forward_n(&mut self, n: usize) {
		let end = self.pos + n;
		while self.pos < end && !self.at_end() {
			self.forward();
		}
	}

	fn line_break(&mut self) {
		if self.peek_c() == '\r' {
			self.forward();
			if self.peek_c() == '\n' {
				self.forward();
			}
		} else if self.peek_c() == '\n' {
			self.forward();
		}
	}

	/// The token starts `off` bytes ahead.
	fn at_off(&self, off: usize, t: &str) -> bool {
		self.src.get(self.pos + off..).is_some_and(|s| s.starts_with(t))
	}

	fn at(&self, t: &str) -> bool {
		self.src[self.pos..].starts_with(t)
	}

	/// The token here, followed by a blank or the end.
	fn spaced(&self, t: &str) -> bool {
		self.at(t) && is_blankz(self.peek_at(t.len()))
	}

	fn flow_indicator_at(&self, off: usize) -> bool {
		let t = &self.tk;
		[t.flow_entry, t.flow_seq_open, t.flow_seq_close, t.flow_map_open, t.flow_map_close].iter().any(|f| self.at_off(off, f))
	}

	fn at_doc_marker(&self) -> Option<K> {
		if self.col != 0 {
			return None;
		}
		if self.spaced(self.tk.doc_start) {
			Some(K::DocStart)
		} else if self.spaced(self.tk.doc_end) {
			Some(K::DocEnd)
		} else {
			None
		}
	}

	/// The next `n` bytes as a string, moving past them.
	fn take_chars(&mut self, n: usize) -> String {
		let s = self.src[self.pos..self.pos + n].to_string();
		self.forward_n(n);
		s
	}

	/// Only blanks come before this point on the line. Checked from here backwards, so it
	/// stops at the first character on a long line rather than scanning it all.
	fn in_indentation(&self) -> bool {
		self.src[self.line_start..self.pos].bytes().rev().all(|b| b == b' ' || b == b'\t')
	}

	// --- dispatch ---

	/// Everything before this point on the line is blanks and block indicators (`- `, `? `,
	/// `: `), so whitespace here works as indentation.
	fn only_indicators_before(&self) -> bool {
		let bytes = self.src.as_bytes();
		let mut i = self.line_start;
		'scan: while i < self.pos {
			if bytes[i] == b' ' || bytes[i] == b'\t' {
				i += 1;
				continue;
			}
			for t in [self.tk.seq_entry, self.tk.key, self.tk.value] {
				let end = i + t.len();
				if end <= self.pos && self.src[i..].starts_with(t) && matches!(bytes.get(end), Some(b' ' | b'\t')) {
					i = end;
					continue 'scan;
				}
			}
			return false;
		}
		true
	}

	/// Skip whitespace, comments and line breaks. Tabs are fine as separation; a tab where it
	/// would work as indentation is recorded, and becomes an error if a block structure
	/// (`- `, `? `, or a key starting a mapping) follows.
	fn skip_to_next_token(&mut self) -> Result<(), Error> {
		self.tab_mark = None;
		loop {
			while is_blank(self.peek_c()) {
				if self.peek_c() == '\t' && self.flow_level == 0 && self.tab_mark.is_none() && self.only_indicators_before() {
					self.tab_mark = Some(self.mark());
				}
				self.forward();
			}
			if self.at(self.tk.comment) {
				if self.pos > 0 && !is_blankz(self.prev_c()) {
					return Err(self.error("a comment needs a space before it"));
				}
				while !is_breakz(self.peek_c()) {
					self.forward();
				}
			}
			if is_break(self.peek_c()) {
				self.line_break();
				self.tab_mark = None;
				if self.flow_level == 0 {
					self.allow_simple_key = true;
				}
			} else {
				return Ok(());
			}
		}
	}

	fn tab_error(&self, tab: Mark) -> Error {
		error_at(tab, "tabs are not allowed for indentation (use spaces)")
	}

	/// The number of spaces at the start of the current line.
	fn leading_spaces(&self) -> usize {
		self.src[self.line_start..].bytes().take_while(|&b| b == b' ').count()
	}

	fn fetch_more(&mut self) -> Result<(), Error> {
		self.skip_to_next_token()?;
		self.stale_simple_keys()?;
		self.unwind_indent(self.col as isize);
		if self.at_end() {
			return self.fetch_stream_end();
		}
		if self.col == 0 {
			if self.at(self.tk.directive) {
				return self.fetch_directive();
			}
			if let Some(kind) = self.at_doc_marker() {
				return self.fetch_doc_marker(kind);
			}
		}
		// Lines inside a flow collection must still be indented past the block they're in
		// (tabs don't count).
		if self.flow_level > 0 && self.in_indentation() && (self.leading_spaces() as isize) <= self.indent {
			return Err(self.error("this line of a flow collection must be indented more than the block it's in"));
		}
		let tk = &self.tk;
		if self.at(tk.flow_seq_open) {
			return self.fetch_flow_start(K::FlowSeqStart, tk.flow_seq_open.len());
		}
		if self.at(tk.flow_map_open) {
			return self.fetch_flow_start(K::FlowMapStart, tk.flow_map_open.len());
		}
		if self.at(tk.flow_seq_close) {
			return self.fetch_flow_end(K::FlowSeqEnd, tk.flow_seq_close.len());
		}
		if self.at(tk.flow_map_close) {
			return self.fetch_flow_end(K::FlowMapEnd, tk.flow_map_close.len());
		}
		if self.at(tk.flow_entry) {
			return self.fetch_flow_entry();
		}
		if self.spaced(tk.seq_entry) {
			return self.fetch_block_entry();
		}
		if self.spaced(tk.key) {
			return self.fetch_key();
		}
		if self.at(tk.value) {
			let after = tk.value.len();
			if is_blankz(self.peek_at(after)) || (self.flow_level > 0 && (self.flow_indicator_at(after) || self.adjacent_value)) {
				return self.fetch_value();
			}
		}
		if self.at(tk.alias) {
			return self.fetch_anchor(K::Alias);
		}
		if self.at(tk.anchor) {
			return self.fetch_anchor(K::Anchor);
		}
		if self.at(tk.tag) {
			return self.fetch_tag();
		}
		if self.flow_level == 0 && self.at(tk.literal) {
			return self.fetch_block_scalar(false);
		}
		if self.flow_level == 0 && self.at(tk.folded) {
			return self.fetch_block_scalar(true);
		}
		if self.at(tk.single_quote) {
			return self.fetch_quoted(false);
		}
		if self.at(tk.double_quote) {
			return self.fetch_quoted(true);
		}
		if self.check_plain() {
			return self.fetch_plain();
		}
		let c = self.peek_c();
		if c == '@' || c == '`' {
			return Err(self.error(format!("'{c}' is reserved and can't start a value (quote the value)")));
		}
		Err(self.error(format!("unexpected '{}' here", self.src[self.pos..].chars().take_while(|c| !is_blankz(*c)).take(8).collect::<String>())))
	}

	/// Whether a plain scalar starts here.
	fn check_plain(&self) -> bool {
		let c = self.peek_c();
		if is_blankz(c) || c == '@' || c == '`' {
			return false;
		}
		if self.tk.inline.iter().any(|t| self.at(t)) {
			return false;
		}
		let tk = &self.tk;
		for t in [&tk.seq_entry, &tk.key, &tk.value] {
			if self.at(t) {
				// Not followed by a blank (that was an indicator); a plain scalar if what
				// follows can continue one.
				return !(self.flow_level > 0 && self.flow_indicator_at(t.len()));
			}
		}
		true
	}

	// --- simple keys and indentation ---

	/// The oldest pending simple key, with its flow level. Queue entries for keys that have
	/// since been used or removed are dropped along the way.
	fn oldest_key(&mut self) -> Option<(usize, SimpleKey)> {
		while let Some(&(level, index)) = self.key_order.front() {
			if let Some(Some(k)) = self.simple_keys.get(level)
				&& k.index == index
			{
				return Some((level, *k));
			}
			self.key_order.pop_front();
		}
		None
	}

	/// Drop pending keys that can no longer get their ':'. A key in a block mapping or a flow
	/// sequence (`[a: 1]`) must be on one line; inside a flow mapping it may span lines
	/// (`{"a"\n: 1}`). Either way it's at most 1024 characters.
	///
	/// This runs for every token, so it never looks at every nesting level: keys are queued
	/// in the order they were saved, so the ones too far back are at the front, and the line
	/// rule only needs checking when the line changes.
	fn stale_simple_keys(&mut self) -> Result<(), Error> {
		while let Some((level, k)) = self.oldest_key() {
			if self.chars_before <= k.index + MAX_IMPLICIT_KEY {
				break;
			}
			self.stale(level, k)?;
		}
		if self.line != self.checked_line {
			self.checked_line = self.line;
			for (level, index) in std::mem::take(&mut self.key_order) {
				let Some(Some(k)) = self.simple_keys.get(level).copied() else { continue };
				if k.index != index {
					continue;
				}
				let one_line = level == 0 || !self.flow_maps.get(level - 1).copied().unwrap_or(false);
				if one_line && k.mark.line != self.line {
					self.stale(level, k)?;
				} else {
					self.key_order.push_back((level, index));
				}
			}
		}
		Ok(())
	}

	fn stale(&mut self, level: usize, k: SimpleKey) -> Result<(), Error> {
		if k.required {
			return Err(error_at(k.mark, "expected ':' after this mapping key (each entry needs 'key: value')"));
		}
		self.simple_keys[level] = None;
		Ok(())
	}

	fn save_simple_key(&mut self) -> Result<(), Error> {
		let required = self.flow_level == 0 && self.indent == self.col as isize;
		if self.allow_simple_key {
			self.remove_simple_key()?;
			let token_number = self.tokens_taken + self.tokens.len();
			self.simple_keys[self.flow_level] = Some(SimpleKey { token_number, required, index: self.chars_before, mark: self.mark(), tab: self.tab_mark });
			self.key_order.push_back((self.flow_level, self.chars_before));
		}
		Ok(())
	}

	fn remove_simple_key(&mut self) -> Result<(), Error> {
		if let Some(k) = self.simple_keys[self.flow_level].take()
			&& k.required
		{
			return Err(error_at(k.mark, "expected ':' after this mapping key (each entry needs 'key: value')"));
		}
		Ok(())
	}

	fn unwind_indent(&mut self, col: isize) {
		if self.flow_level > 0 {
			return;
		}
		while self.indent > col {
			let m = self.mark();
			self.indent = self.indents.pop().unwrap_or(-1);
			self.tokens.push_back(Token::new(K::BlockEnd, m));
		}
	}

	fn add_indent(&mut self, col: isize) -> bool {
		if self.indent < col {
			self.indents.push(self.indent);
			self.indent = col;
			true
		} else {
			false
		}
	}

	// --- fetchers ---

	fn fetch_stream_end(&mut self) -> Result<(), Error> {
		self.unwind_indent(-1);
		self.remove_simple_key()?;
		self.allow_simple_key = false;
		self.simple_keys.fill(None);
		let m = self.mark();
		self.push(Token::new(K::StreamEnd, m));
		self.done = true;
		Ok(())
	}

	fn fetch_directive(&mut self) -> Result<(), Error> {
		self.unwind_indent(-1);
		self.remove_simple_key()?;
		self.allow_simple_key = false;
		let m = self.mark();
		self.forward_n(self.tk.directive.len());
		let mut name = String::new();
		while !is_blankz(self.peek_c()) {
			name.push(self.peek_c());
			self.forward();
		}
		let mut tok = Token::new(K::OtherDirective, m);
		match name.as_str() {
			"YAML" => {
				self.skip_blanks_required("a version after %YAML")?;
				let major = self.digits()?;
				if self.peek_c() != '.' {
					return Err(self.error("expected a version like 1.2"));
				}
				self.forward();
				let minor = self.digits()?;
				tok.kind = K::YamlDirective;
				tok.text = format!("{major}.{minor}");
			}
			"TAG" => {
				self.skip_blanks_required("a tag handle after %TAG")?;
				tok.handle = self.directive_handle()?;
				self.skip_blanks_required("a tag prefix after the handle")?;
				tok.text = self.tag_uri(true)?;
				if tok.text.is_empty() {
					return Err(self.error("expected a tag prefix"));
				}
				tok.kind = K::TagDirective;
			}
			"" => return Err(self.error("expected a directive name")),
			_ => {
				while !is_breakz(self.peek_c()) {
					self.forward();
				}
			}
		}
		self.rest_of_line("the directive")?;
		self.push(tok);
		Ok(())
	}

	/// After a directive or document marker, only blanks and a comment may follow.
	fn rest_of_line(&mut self, what: &str) -> Result<(), Error> {
		while is_blank(self.peek_c()) {
			self.forward();
		}
		if self.at(self.tk.comment) {
			if !is_blank(self.prev_c()) {
				return Err(self.error("a comment needs a space before it"));
			}
			while !is_breakz(self.peek_c()) {
				self.forward();
			}
		}
		if !is_breakz(self.peek_c()) {
			return Err(self.error(format!("unexpected text after {what}")));
		}
		Ok(())
	}

	fn skip_blanks_required(&mut self, what: &str) -> Result<(), Error> {
		if !is_blank(self.peek_c()) {
			return Err(self.error(format!("expected {what}")));
		}
		while is_blank(self.peek_c()) {
			self.forward();
		}
		Ok(())
	}

	fn digits(&mut self) -> Result<u32, Error> {
		let mut s = String::new();
		while self.peek_c().is_ascii_digit() {
			s.push(self.peek_c());
			self.forward();
		}
		s.parse().map_err(|_| self.error("expected a version like 1.2"))
	}

	/// A tag handle in a %TAG directive, returned in standard form.
	fn directive_handle(&mut self) -> Result<String, Error> {
		let tag_len = self.tk.tag.len();
		if !self.at(self.tk.tag) {
			return Err(self.error("expected a tag handle"));
		}
		self.forward_n(tag_len);
		let mut name = String::new();
		while self.peek_c().is_ascii_alphanumeric() || self.peek_c() == '-' {
			name.push(self.peek_c());
			self.forward();
		}
		if self.at(self.tk.tag) {
			self.forward_n(tag_len);
			return Ok(format!("!{name}!"));
		}
		if name.is_empty() && is_blank(self.peek_c()) {
			return Ok("!".into());
		}
		Err(self.error("invalid tag handle"))
	}

	fn fetch_doc_marker(&mut self, kind: K) -> Result<(), Error> {
		self.unwind_indent(-1);
		self.remove_simple_key()?;
		self.allow_simple_key = false;
		let m = self.mark();
		let len = if kind == K::DocStart { self.tk.doc_start.len() } else { self.tk.doc_end.len() };
		self.forward_n(len);
		if kind == K::DocEnd {
			self.rest_of_line("the document end marker")?;
		}
		self.push(Token::new(kind, m));
		Ok(())
	}

	fn fetch_flow_start(&mut self, kind: K, len: usize) -> Result<(), Error> {
		self.save_simple_key()?;
		self.flow_level += 1;
		self.flow_maps.push(kind == K::FlowMapStart);
		if self.simple_keys.len() <= self.flow_level {
			self.simple_keys.push(None);
		}
		self.allow_simple_key = true;
		let m = self.mark();
		self.forward_n(len);
		self.push(Token::new(kind, m));
		Ok(())
	}

	fn fetch_flow_end(&mut self, kind: K, len: usize) -> Result<(), Error> {
		self.remove_simple_key()?;
		if self.flow_level > 0 {
			self.flow_level -= 1;
			self.flow_maps.pop();
			self.simple_keys.truncate(self.flow_level + 1);
		}
		self.allow_simple_key = false;
		let m = self.mark();
		self.forward_n(len);
		self.push(Token::new(kind, m));
		Ok(())
	}

	fn fetch_flow_entry(&mut self) -> Result<(), Error> {
		self.allow_simple_key = true;
		self.remove_simple_key()?;
		let m = self.mark();
		self.forward_n(self.tk.flow_entry.len());
		self.push(Token::new(K::FlowEntry, m));
		Ok(())
	}

	fn fetch_block_entry(&mut self) -> Result<(), Error> {
		let m = self.mark();
		if self.flow_level > 0 {
			return Err(self.error("block sequence entries aren't allowed inside a flow collection"));
		}
		if !self.allow_simple_key {
			return Err(self.error("a block sequence entry isn't allowed here (start the sequence on its own line)"));
		}
		if let Some(tab) = self.tab_mark {
			return Err(self.tab_error(tab));
		}
		if self.add_indent(self.col as isize) {
			self.tokens.push_back(Token::new(K::BlockSeqStart, m));
		}
		self.allow_simple_key = true;
		self.remove_simple_key()?;
		self.forward_n(self.tk.seq_entry.len());
		self.push(Token::new(K::BlockEntry, m));
		Ok(())
	}

	fn fetch_key(&mut self) -> Result<(), Error> {
		let m = self.mark();
		if self.flow_level == 0 {
			if !self.allow_simple_key {
				return Err(self.error("an explicit mapping key isn't allowed here"));
			}
			if let Some(tab) = self.tab_mark {
				return Err(self.tab_error(tab));
			}
			if self.add_indent(self.col as isize) {
				self.tokens.push_back(Token::new(K::BlockMapStart, m));
			}
		}
		self.allow_simple_key = self.flow_level == 0;
		self.remove_simple_key()?;
		self.forward_n(self.tk.key.len());
		self.push(Token::new(K::Key, m));
		Ok(())
	}

	fn fetch_value(&mut self) -> Result<(), Error> {
		let m = self.mark();
		if let Some(k) = self.simple_keys[self.flow_level].take() {
			if self.flow_level == 0
				&& let Some(tab) = k.tab
			{
				return Err(self.tab_error(tab));
			}
			let at = k.token_number - self.tokens_taken;
			self.tokens.insert(at, Token::new(K::Key, k.mark));
			if self.flow_level == 0 && self.add_indent(k.mark.col as isize) {
				self.tokens.insert(at, Token::new(K::BlockMapStart, k.mark));
			}
			self.allow_simple_key = false;
		} else {
			if self.flow_level == 0 {
				if !self.allow_simple_key {
					let hint = if self.plain_multiline {
						"the value on the line above continues onto this line; check the indentation"
					} else {
						"quote the value if it contains ': '"
					};
					return Err(self.error(format!("mapping values are not allowed here ({hint})")));
				}
				if self.add_indent(self.col as isize) {
					self.tokens.push_back(Token::new(K::BlockMapStart, m));
				}
			}
			self.allow_simple_key = self.flow_level == 0;
		}
		self.forward_n(self.tk.value.len());
		self.push(Token::new(K::Value, m));
		Ok(())
	}

	fn fetch_anchor(&mut self, kind: K) -> Result<(), Error> {
		self.save_simple_key()?;
		self.allow_simple_key = false;
		let m = self.mark();
		let len = if kind == K::Alias { self.tk.alias.len() } else { self.tk.anchor.len() };
		self.forward_n(len);
		let mut n = 0;
		while !is_blankz(self.peek_at(n)) && !self.flow_indicator_at(n) {
			n += self.char_len_at(n);
		}
		if n == 0 {
			let what = if kind == K::Alias { "an alias" } else { "an anchor" };
			return Err(self.error(format!("expected a name for {what}")));
		}
		let mut tok = Token::new(kind, m);
		tok.text = self.take_chars(n);
		self.push(tok);
		Ok(())
	}

	fn fetch_tag(&mut self) -> Result<(), Error> {
		self.save_simple_key()?;
		self.allow_simple_key = false;
		let m = self.mark();
		let tag_len = self.tk.tag.len();
		self.forward_n(tag_len);
		let mut tok = Token::new(K::Tag, m);
		let ends = |s: &Self, off: usize| is_blankz(s.peek_at(off)) || (s.flow_level > 0 && s.flow_indicator_at(off));
		if self.peek_c() == '<' {
			self.forward();
			tok.text = self.tag_uri(true)?;
			if self.peek_c() != '>' {
				return Err(self.error("expected '>' to close the verbatim tag"));
			}
			self.forward();
			if tok.text.is_empty() {
				return Err(error_at(m, "a verbatim tag can't be empty"));
			}
		} else if ends(self, 0) {
			// The non-specific tag `!`.
			tok.handle = "!".into();
		} else {
			let mut n = 0;
			let mut named = false;
			while !ends(self, n) {
				if self.at_off(n, self.tk.tag) {
					named = true;
					break;
				}
				n += self.char_len_at(n);
			}
			if named {
				let name = self.take_chars(n);
				if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
					return Err(error_at(m, format!("invalid tag handle around {name:?}")));
				}
				self.forward_n(tag_len);
				tok.handle = format!("!{name}!");
			} else {
				tok.handle = "!".into();
			}
			tok.text = self.tag_uri(false)?;
			if tok.text.is_empty() && tok.handle != "!" {
				return Err(error_at(m, "expected a tag name after the handle"));
			}
		}
		if !ends(self, 0) {
			return Err(self.error("expected a space after the tag"));
		}
		self.push(tok);
		Ok(())
	}

	/// The characters of a tag (or %TAG prefix), with %XX escapes decoded.
	fn tag_uri(&mut self, verbatim: bool) -> Result<String, Error> {
		let mut bytes = Vec::new();
		loop {
			let c = self.peek_c();
			if !verbatim && (self.flow_indicator_at(0) || self.at(self.tk.tag)) {
				break;
			}
			let ok = c.is_ascii_alphanumeric() || "-;/?:@&=+$_.~*'()#".contains(c) || "!,[]".contains(c);
			if c == '%' {
				let hex: String = [self.peek_at(1), self.peek_at(2)].iter().collect();
				let b = parse_hex(&hex).and_then(|n| u8::try_from(n).ok()).ok_or_else(|| self.error("invalid %XX escape in a tag"))?;
				bytes.push(b);
				self.forward_n(3);
			} else if ok {
				let mut buf = [0u8; 4];
				bytes.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
				self.forward();
			} else {
				break;
			}
		}
		String::from_utf8(bytes).map_err(|_| self.error("a tag's %XX escapes aren't valid UTF-8"))
	}

	fn fetch_block_scalar(&mut self, folded: bool) -> Result<(), Error> {
		self.allow_simple_key = true;
		self.remove_simple_key()?;
		let m = self.mark();
		let text = self.block_scalar(folded)?;
		let mut tok = Token::new(K::Scalar, m);
		tok.text = text;
		tok.style = if folded { Style::Folded } else { Style::Literal };
		self.push(tok);
		Ok(())
	}

	fn block_scalar(&mut self, folded: bool) -> Result<String, Error> {
		let len = if folded { self.tk.folded.len() } else { self.tk.literal.len() };
		self.forward_n(len);
		let mut keep: Option<bool> = None;
		let mut increment: Option<usize> = None;
		for _ in 0..2 {
			match self.peek_c() {
				c @ ('+' | '-') if keep.is_none() => keep = Some(c == '+'),
				c @ '1'..='9' if increment.is_none() => increment = Some(c as usize - '0' as usize),
				'0' => return Err(self.error("the indentation indicator must be 1-9")),
				_ => break,
			}
			self.forward();
		}
		while is_blank(self.peek_c()) {
			self.forward();
		}
		if self.at(self.tk.comment) {
			if !is_blank(self.prev_c()) {
				return Err(self.error("a comment needs a space before it"));
			}
			while !is_breakz(self.peek_c()) {
				self.forward();
			}
		}
		if !is_breakz(self.peek_c()) {
			return Err(self.error("expected a line break after the block scalar header"));
		}
		self.line_break();

		// At the top level (outside any block collection) the content may start in column 0.
		let min_indent = (self.indent + 1).max(0) as usize;
		let (mut breaks, indent) = match increment {
			None => self.block_scalar_indentation(min_indent)?,
			Some(inc) => {
				let indent = if self.indent >= 0 { self.indent as usize + inc } else { inc };
				(self.block_scalar_breaks(indent)?, indent)
			}
		};
		let mut out = String::new();
		let mut line_break = "";
		while self.block_line_here(indent) {
			out.push_str(&breaks);
			let leading_non_blank = !is_blank(self.peek_c());
			let start = self.pos;
			while !is_breakz(self.peek_c()) {
				self.forward();
			}
			out.push_str(&self.src[start..self.pos]);
			// The end of the input ends the last line just as a line break would.
			if is_break(self.peek_c()) {
				self.line_break();
			}
			line_break = "\n";
			breaks = self.block_scalar_breaks(indent)?;
			if self.block_line_here(indent) {
				if folded && line_break == "\n" && leading_non_blank && !is_blank(self.peek_c()) {
					if breaks.is_empty() {
						out.push(' ');
					}
				} else {
					out.push_str(line_break);
				}
			} else {
				break;
			}
		}
		if keep != Some(false) {
			out.push_str(line_break);
		}
		if keep == Some(true) {
			out.push_str(&breaks);
		}
		Ok(out)
	}

	/// Another content line of a block scalar indented by `indent` starts here.
	fn block_line_here(&self, indent: usize) -> bool {
		self.col == indent && !self.at_end() && !(self.col == 0 && self.at_doc_marker().is_some())
	}

	/// A line that's only a tab (and blanks) where a block scalar's indentation should be.
	fn check_tab_line(&self, indent: usize) -> Result<(), Error> {
		if self.col < indent && self.peek_c() == '\t' {
			let mut n = 0;
			while is_blank(self.peek_at(n)) {
				n += 1;
			}
			if is_breakz(self.peek_at(n)) {
				return Err(self.tab_error(self.mark()));
			}
		}
		Ok(())
	}

	/// Detect a block scalar's indentation from its first line of text, collecting the empty
	/// lines before it.
	fn block_scalar_indentation(&mut self, min_indent: usize) -> Result<(String, usize), Error> {
		let mut breaks = String::new();
		let mut blank_max = 0;
		let mut blank_mark = self.mark();
		loop {
			let fresh = self.col == 0;
			while self.peek_c() == ' ' {
				self.forward();
			}
			self.check_tab_line(min_indent.max(1))?;
			if is_break(self.peek_c()) {
				if self.col > blank_max {
					blank_max = self.col;
					blank_mark = self.mark();
				}
				self.line_break();
				breaks.push('\n');
				continue;
			}
			if self.at_end() {
				// Spaces on a last line with no line break still make an empty line.
				if fresh && self.col > 0 {
					breaks.push('\n');
				}
				return Ok((breaks, blank_max.max(self.col).max(min_indent)));
			}
			if self.col >= min_indent && blank_max > self.col {
				return Err(error_at(blank_mark, "a leading empty line of this block scalar has more spaces than its first line of text"));
			}
			return Ok((breaks, self.col.max(min_indent)));
		}
	}

	fn block_scalar_breaks(&mut self, indent: usize) -> Result<String, Error> {
		let mut breaks = String::new();
		loop {
			let fresh = self.col == 0;
			while self.col < indent && self.peek_c() == ' ' {
				self.forward();
			}
			self.check_tab_line(indent)?;
			if is_break(self.peek_c()) {
				self.line_break();
				breaks.push('\n');
			} else {
				if fresh && self.at_end() && self.col > 0 {
					breaks.push('\n');
				}
				return Ok(breaks);
			}
		}
	}

	fn fetch_quoted(&mut self, double: bool) -> Result<(), Error> {
		self.save_simple_key()?;
		self.allow_simple_key = false;
		let m = self.mark();
		let text = self.quoted(double, m)?;
		let mut tok = Token::new(K::Scalar, m);
		tok.text = text;
		tok.style = if double { Style::Double } else { Style::Single };
		self.push(tok);
		Ok(())
	}

	fn quoted(&mut self, double: bool, start: Mark) -> Result<String, Error> {
		let quote = if double { self.tk.double_quote } else { self.tk.single_quote };
		self.forward_n(quote.len());
		let mut out = String::new();
		self.quoted_non_blanks(double, quote, &mut out)?;
		while !self.at(quote) {
			self.quoted_blanks(&mut out, start)?;
			self.quoted_non_blanks(double, quote, &mut out)?;
		}
		self.forward_n(quote.len());
		Ok(out)
	}

	fn quoted_non_blanks(&mut self, double: bool, quote: &str, out: &mut String) -> Result<(), Error> {
		loop {
			if self.at_end() {
				return Ok(());
			}
			if self.at(quote) {
				if !double && self.at_off(quote.len(), quote) {
					out.push_str(quote);
					self.forward_n(quote.len() * 2);
					continue;
				}
				return Ok(());
			}
			let c = self.peek_c();
			if double && c == '\\' {
				self.forward();
				if self.at(quote) {
					out.push_str(quote);
					self.forward_n(quote.len());
					continue;
				}
				let e = self.peek_c();
				if is_break(e) {
					// An escaped line break joins the lines with nothing in between.
					self.line_break();
					let breaks = self.quoted_breaks()?;
					out.push_str(&breaks);
					continue;
				}
				let ch = match e {
					'0' => '\0',
					'a' => '\u{07}',
					'b' => '\u{08}',
					't' | '\t' => '\t',
					'n' => '\n',
					'v' => '\u{0B}',
					'f' => '\u{0C}',
					'r' => '\r',
					'e' => '\u{1B}',
					' ' => ' ',
					'"' => '"',
					'/' => '/',
					'\\' => '\\',
					'N' => '\u{85}',
					'_' => '\u{A0}',
					'L' => '\u{2028}',
					'P' => '\u{2029}',
					'x' | 'u' | 'U' => {
						let n = match e {
							'x' => 2,
							'u' => 4,
							_ => 8,
						};
						let hex: String = (1..=n).map(|i| self.peek_at(i)).collect();
						let code = parse_hex(&hex).ok_or_else(|| self.error("invalid hex escape"))?;
						let ch = char::from_u32(code).ok_or_else(|| self.error("escape is not a valid character"))?;
						self.forward_n(n);
						ch
					}
					_ => return Err(self.error(format!("invalid escape '\\{e}'"))),
				};
				self.forward();
				out.push(ch);
				continue;
			}
			if is_blank(c) || is_break(c) {
				return Ok(());
			}
			out.push(c);
			self.forward();
		}
	}

	fn quoted_blanks(&mut self, out: &mut String, start: Mark) -> Result<(), Error> {
		let begin = self.pos;
		while is_blank(self.peek_c()) {
			self.forward();
		}
		if self.at_end() {
			return Err(error_at(start, "this quoted string is never closed"));
		}
		if is_break(self.peek_c()) {
			self.line_break();
			let breaks = self.quoted_breaks()?;
			if breaks.is_empty() {
				out.push(' ');
			} else {
				out.push_str(&breaks);
			}
		} else {
			out.push_str(&self.src[begin..self.pos]);
		}
		Ok(())
	}

	fn quoted_breaks(&mut self) -> Result<String, Error> {
		let mut breaks = String::new();
		loop {
			if self.at_doc_marker().is_some() {
				return Err(self.error("a document marker can't appear inside a quoted string"));
			}
			while is_blank(self.peek_c()) {
				self.forward();
			}
			if is_break(self.peek_c()) {
				self.line_break();
				breaks.push('\n');
			} else {
				// In a block collection, the continuation lines of a quoted string must be
				// indented past the collection (with spaces; tabs don't count).
				if self.flow_level == 0 && !self.at_end() && (self.leading_spaces() as isize) <= self.indent {
					return Err(self.error("the continuation of a quoted string must be indented more than its block"));
				}
				return Ok(breaks);
			}
		}
	}

	fn fetch_plain(&mut self) -> Result<(), Error> {
		self.save_simple_key()?;
		self.allow_simple_key = false;
		let m = self.mark();
		let (text, multiline) = self.plain()?;
		let mut tok = Token::new(K::Scalar, m);
		tok.text = text;
		self.push(tok);
		self.plain_multiline = multiline;
		Ok(())
	}

	fn plain(&mut self) -> Result<(String, bool), Error> {
		let mut out = String::new();
		let indent = self.indent + 1;
		let mut spaces = String::new();
		let mut crossed = false;
		let mut multiline = false;
		loop {
			if self.at(self.tk.comment) {
				break;
			}
			let start = self.pos;
			loop {
				let c = self.peek_c();
				if is_blankz(c) {
					break;
				}
				if self.at(self.tk.value) {
					let after = self.tk.value.len();
					if is_blankz(self.peek_at(after)) || (self.flow_level > 0 && self.flow_indicator_at(after)) {
						break;
					}
				}
				if self.flow_level > 0 && self.flow_indicator_at(0) {
					break;
				}
				self.forward();
			}
			if self.pos == start {
				break;
			}
			self.allow_simple_key = false;
			multiline |= crossed;
			out.push_str(&spaces);
			out.push_str(&self.src[start..self.pos]);
			(spaces, crossed) = self.plain_spaces(indent);
			if spaces.is_empty() || self.at(self.tk.comment) || (self.flow_level == 0 && (self.col as isize) < indent) {
				break;
			}
		}
		Ok((out, multiline))
	}

	/// The whitespace after part of a plain scalar, folded; empty if the scalar ends here.
	/// Continuation lines are indented with spaces to at least `indent`; tabs may only come
	/// after that.
	fn plain_spaces(&mut self, indent: isize) -> (String, bool) {
		let begin = self.pos;
		while is_blank(self.peek_c()) {
			self.forward();
		}
		if !is_break(self.peek_c()) {
			return (self.src[begin..self.pos].to_string(), false);
		}
		self.line_break();
		self.allow_simple_key = true;
		if self.at_doc_marker().is_some() {
			return (String::new(), true);
		}
		let mut breaks = String::new();
		loop {
			let c = self.peek_c();
			let tab_ok = self.flow_level > 0 || self.col as isize >= indent;
			if c == ' ' || (c == '\t' && tab_ok) {
				self.forward();
			} else if !is_break(c) {
				break;
			} else {
				self.line_break();
				breaks.push('\n');
				if self.at_doc_marker().is_some() {
					return (String::new(), true);
				}
			}
		}
		(if breaks.is_empty() { " ".into() } else { breaks }, true)
	}
}

// ---------- composing ----------

/// Collects a mapping's entries, rejecting duplicate keys and applying merge keys.
#[derive(Default)]
struct MapEntries {
	pairs: Vec<(Value, Value)>,
	index: KeyIndex,
	/// Merged entries and the position of their `<<` key.
	merges: Vec<(usize, Vec<(Value, Value)>)>,
}

impl MapEntries {
	#[inline(never)]
	fn add(&mut self, key: Value, value: Value, merge: bool, m: Mark) -> Result<(), Error> {
		if merge {
			let wrong = || error_at(m, "a '<<' merge key needs a mapping or a list of mappings");
			let sources: Vec<Vec<(Value, Value)>> = match value.into_map() {
				Ok(pairs) => vec![pairs],
				Err(other) => match other.into_seq() {
					Ok(items) => items.into_iter().map(|item| item.into_map().map_err(|_| wrong())).collect::<Result<_, _>>()?,
					Err(_) => return Err(wrong()),
				},
			};
			self.merges.push((self.pairs.len(), sources.into_iter().flatten().collect()));
			return Ok(());
		}
		if self.index.find(&self.pairs, &key).is_some() {
			return Err(error_at(m, format!("duplicate key \"{}\"", key.key_text())));
		}
		self.index.insert(&self.pairs, &key, self.pairs.len());
		self.pairs.push((key, value));
		Ok(())
	}

	#[inline(never)]
	fn finish(self) -> Value {
		let MapEntries { pairs, index, merges } = self;
		if merges.is_empty() {
			return Value::Map(pairs);
		}
		// Keys written explicitly win over merged ones, and earlier merged sources over later.
		let mut merged_keys: Vec<(Value, Value)> = Vec::new();
		let mut merged_index = KeyIndex::default();
		let mut inserts = Vec::new();
		for (at, list) in merges {
			let mut kept = Vec::new();
			for (k, v) in list {
				if index.find(&pairs, &k).is_some() || merged_index.find(&merged_keys, &k).is_some() {
					continue;
				}
				merged_index.insert(&merged_keys, &k, merged_keys.len());
				merged_keys.push((k.clone(), Value::Null));
				kept.push((k, v));
			}
			inserts.push((at, kept));
		}
		let mut out = Vec::with_capacity(pairs.len() + merged_keys.len());
		let mut inserts = inserts.into_iter().peekable();
		for (i, pair) in pairs.into_iter().enumerate() {
			while let Some((_, kept)) = inserts.next_if(|(at, _)| *at == i) {
				out.extend(kept);
			}
			out.push(pair);
		}
		for (_, kept) in inserts {
			out.extend(kept);
		}
		Value::Map(out)
	}
}

/// What composing does next.
enum Step {
	/// Start a node (`block`: block collections may start here; `indentless`: so may a
	/// sequence at the same indent as its key).
	Node { block: bool, indentless: bool },
	/// Continue the innermost open collection.
	Next,
	/// A node is finished.
	Done(Value),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
	BlockSeq {
		col: usize,
	},
	/// A block sequence at the same indent as its mapping key, which has no end token.
	Indentless,
	BlockMap {
		col: usize,
	},
	FlowSeq {
		first: bool,
	},
	FlowMap {
		first: bool,
	},
}

impl Kind {
	fn is_block(self) -> bool {
		matches!(self, Kind::BlockSeq { .. } | Kind::Indentless | Kind::BlockMap { .. })
	}

	/// Tokens that end an entry's key or value early, leaving it null.
	fn ends_entry(self, k: K) -> bool {
		match self {
			Kind::FlowSeq { .. } => matches!(k, K::FlowEntry | K::FlowSeqEnd),
			Kind::FlowMap { .. } => matches!(k, K::FlowEntry | K::FlowMapEnd),
			_ => matches!(k, K::Key | K::Value | K::BlockEnd),
		}
	}
}

/// What an open collection is waiting for.
enum Want {
	/// A sequence item.
	Item,
	/// The key of an entry that started at the mark.
	Key(Mark),
	/// A key with no value indicator after it (flow mappings only).
	BareKey(Mark),
	/// The value for `key`.
	Value { key: Value, merge: bool, m: Mark },
}

/// A collection being composed.
struct Open {
	kind: Kind,
	items: Vec<Value>,
	entries: MapEntries,
	want: Want,
	props: Props,
	/// Where the collection starts, for errors about its tag.
	at: Mark,
}

impl Open {
	/// Add a finished entry: a pair in a mapping, or a single-pair mapping in a sequence.
	fn add(&mut self, key: Value, value: Value, merge: bool, m: Mark) -> Result<(), Error> {
		self.want = Want::Item;
		if matches!(self.kind, Kind::FlowSeq { .. }) {
			// A single-pair mapping, with a merge key applied like any other.
			let mut pair = MapEntries::default();
			pair.add(key, value, merge, m)?;
			self.items.push(pair.finish());
			Ok(())
		} else {
			self.entries.add(key, value, merge, m)
		}
	}
}

fn top(stack: &mut [Open]) -> &mut Open {
	stack.last_mut().expect("an open collection")
}

struct Composer<'a> {
	sc: Scanner<'a>,
	tokens: YamlTokens,
	merge_keys: bool,
	limits: Limits,
	anchors: HashMap<String, (Value, usize)>,
	open_anchors: Vec<String>,
	expanded: usize,
	/// Values copied so far to remember anchored nodes, and how many may be.
	anchor_copies: usize,
	max_anchor_copies: usize,
	handles: Vec<(String, String)>,
	/// The node just composed was a plain `<<` scalar (or tagged `!!merge`).
	last_merge: bool,
	started: bool,
	/// Where the last scalar, alias or flow collection ended up starting.
	last_value: Option<Mark>,
}

impl<'a> Composer<'a> {
	fn new(text: &'a str, o: &Yaml) -> Result<Self, Error> {
		let limits = o.limits();
		limits.check_input(text.len())?;
		Ok(Composer {
			sc: Scanner::new(text, &o.tokens)?,
			tokens: o.tokens,
			merge_keys: o.merge_keys,
			limits,
			anchors: HashMap::new(),
			open_anchors: Vec::new(),
			expanded: 0,
			anchor_copies: 0,
			max_anchor_copies: limits.max_alias_values.saturating_add(text.len()),
			handles: Vec::new(),
			last_merge: false,
			started: false,
			last_value: None,
		})
	}

	fn peek(&mut self) -> Result<(K, Mark), Error> {
		let t = self.sc.peek()?;
		Ok((t.kind, t.mark))
	}

	fn next(&mut self) -> Result<Token, Error> {
		let t = self.sc.next()?;
		if matches!(t.kind, K::Scalar | K::Alias | K::FlowSeqEnd | K::FlowMapEnd) {
			self.last_value = Some(t.mark);
		}
		Ok(t)
	}

	/// The message for an entry that doesn't line up with its collection at column `col`.
	fn misplaced(&self, m: Mark, col: usize, what: &str, otherwise: String) -> String {
		if self.last_value.is_some_and(|l| l.line == m.line) {
			"unexpected text after the value".into()
		} else if m.col != col {
			format!("bad indentation of a {what}")
		} else {
			otherwise
		}
	}

	fn describe(&self, k: K) -> String {
		let t = &self.tokens;
		let quote = |s: &str| format!("'{s}'");
		match k {
			K::StreamEnd => "the end of the input".into(),
			K::YamlDirective | K::TagDirective | K::OtherDirective => "a directive".into(),
			K::DocStart => quote(t.doc_start),
			K::DocEnd => quote(t.doc_end),
			K::BlockSeqStart | K::BlockEntry => quote(t.seq_entry),
			K::BlockMapStart => "a mapping".into(),
			K::BlockEnd => "less-indented text".into(),
			K::FlowSeqStart => quote(t.flow_seq_open),
			K::FlowSeqEnd => quote(t.flow_seq_close),
			K::FlowMapStart => quote(t.flow_map_open),
			K::FlowMapEnd => quote(t.flow_map_close),
			K::FlowEntry => quote(t.flow_entry),
			K::Key => "a mapping key".into(),
			K::Value => quote(t.value),
			K::Alias => "an alias".into(),
			K::Anchor => "an anchor".into(),
			K::Tag => "a tag".into(),
			K::Scalar => "a value".into(),
		}
	}

	/// Skip stray document end markers; the mark of the next document if there is one.
	fn more_documents(&mut self) -> Result<Option<Mark>, Error> {
		while self.peek()?.0 == K::DocEnd {
			self.next()?;
		}
		let (k, m) = self.peek()?;
		Ok((k != K::StreamEnd).then_some(m))
	}

	fn next_document(&mut self) -> Result<Option<Value>, Error> {
		if self.more_documents()?.is_none() {
			return Ok(None);
		}
		self.handles = vec![("!".into(), "!".into()), ("!!".into(), CORE_TAG_PREFIX.into())];
		let mut declared: Vec<String> = Vec::new();
		let mut directives = false;
		let mut version_seen = false;
		loop {
			let (k, m) = self.peek()?;
			match k {
				K::YamlDirective => {
					let t = self.next()?;
					if version_seen {
						return Err(error_at(m, "only one %YAML directive is allowed per document"));
					}
					version_seen = true;
					if !t.text.starts_with("1.") {
						return Err(error_at(m, format!("unsupported YAML version {}", t.text)));
					}
				}
				K::TagDirective => {
					let t = self.next()?;
					if declared.contains(&t.handle) {
						return Err(error_at(m, format!("the tag handle {} is declared twice", t.handle)));
					}
					declared.push(t.handle.clone());
					self.handles.push((t.handle, t.text));
				}
				K::OtherDirective => {
					self.next()?;
				}
				_ => break,
			}
			directives = true;
		}
		let (k, m) = self.peek()?;
		let explicit = k == K::DocStart;
		if explicit {
			self.next()?;
		} else if directives {
			return Err(error_at(m, format!("expected '{}' after the directives", self.tokens.doc_start)));
		} else if self.started {
			// Only reachable after a document closed with '...': a bare document may follow.
		}
		self.started = true;
		let empty = matches!(self.peek()?.0, K::YamlDirective | K::TagDirective | K::OtherDirective | K::DocStart | K::DocEnd | K::StreamEnd);
		let value = if empty { Value::Null } else { self.compose(true, false)? };
		self.anchors.clear();
		self.expanded = 0;
		self.anchor_copies = 0;
		let (k, m) = self.peek()?;
		match k {
			K::DocEnd => {
				self.next()?;
			}
			K::DocStart | K::StreamEnd => {}
			K::YamlDirective | K::TagDirective | K::OtherDirective => {
				return Err(error_at(m, format!("a directive after a document needs a '{}' line before it", self.tokens.doc_end)));
			}
			other => {
				return Err(error_at(m, format!("found {} after the end of the document (check the indentation)", self.describe(other))));
			}
		}
		Ok(Some(value))
	}

	fn resolve_tag(&self, t: &Token) -> Result<String, Error> {
		if t.handle.is_empty() {
			return Ok(t.text.clone());
		}
		if t.handle == "!" && t.text.is_empty() {
			return Ok("!".into());
		}
		let prefix = self
			.handles
			.iter()
			.rev()
			.find(|(h, _)| *h == t.handle)
			.map(|(_, p)| p)
			.ok_or_else(|| error_at(t.mark, format!("undefined tag handle {} (declare it with %TAG)", t.handle)))?;
		Ok(format!("{prefix}{}", t.text))
	}

	/// One node and everything inside it. Open collections are kept on a heap stack rather
	/// than by recursing, so nesting is limited only by `max_depth`, never by the thread's
	/// stack.
	fn compose(&mut self, block: bool, indentless: bool) -> Result<Value, Error> {
		let mut stack: Vec<Open> = Vec::new();
		let mut step = Step::Node { block, indentless };
		loop {
			step = match step {
				Step::Node { block, indentless } => self.start_node(&mut stack, block, indentless)?,
				Step::Next => self.advance(&mut stack)?,
				Step::Done(v) => match stack.last_mut() {
					None => return Ok(v),
					Some(open) => self.deliver(open, v)?,
				},
			};
		}
	}

	/// Start a node: a scalar or alias is finished at once; a collection is opened.
	fn start_node(&mut self, stack: &mut Vec<Open>, block: bool, indentless: bool) -> Result<Step, Error> {
		let (k, m) = self.peek()?;
		if stack.len() > self.limits.max_depth {
			return Err(self.fail(m, Fail::TooDeep));
		}
		if k == K::Alias {
			return Ok(Step::Done(self.alias_node()?));
		}
		let props = self.properties()?;
		let (k, at) = self.peek()?;
		let kind = match k {
			K::BlockEntry if indentless => Kind::Indentless,
			K::FlowSeqStart => Kind::FlowSeq { first: true },
			K::FlowMapStart => Kind::FlowMap { first: true },
			K::BlockSeqStart if block => Kind::BlockSeq { col: at.col },
			K::BlockMapStart if block => Kind::BlockMap { col: at.col },
			_ => return Ok(Step::Done(self.leaf(props, k, at)?)),
		};
		if kind != Kind::Indentless {
			self.skip()?;
		}
		stack.push(Open { kind, items: Vec::new(), entries: MapEntries::default(), want: Want::Item, props, at });
		Ok(Step::Next)
	}

	/// Look at the next token in the innermost open collection: start its next item, or
	/// close it.
	fn advance(&mut self, stack: &mut Vec<Open>) -> Result<Step, Error> {
		let kind = stack.last().expect("an open collection").kind;
		let (mut k, mut m) = self.peek()?;
		match kind {
			Kind::BlockSeq { col } => match k {
				K::BlockEntry => {
					self.skip()?;
					if matches!(self.peek()?.0, K::BlockEntry | K::BlockEnd) {
						top(stack).items.push(Value::Null);
						return Ok(Step::Next);
					}
					Ok(Step::Node { block: true, indentless: false })
				}
				K::BlockEnd => {
					self.skip()?;
					self.close(stack)
				}
				found => Err(self.fail(m, Fail::Misplaced { col, seq: true, found })),
			},
			Kind::Indentless => {
				if k != K::BlockEntry {
					return self.close(stack);
				}
				self.skip()?;
				if matches!(self.peek()?.0, K::BlockEntry | K::Key | K::Value | K::BlockEnd) {
					top(stack).items.push(Value::Null);
					return Ok(Step::Next);
				}
				Ok(Step::Node { block: true, indentless: false })
			}
			Kind::BlockMap { col } => match k {
				K::Key | K::Value => self.start_entry(top(stack), k, m),
				K::BlockEnd => {
					self.skip()?;
					self.close(stack)
				}
				found => Err(self.fail(m, Fail::Misplaced { col, seq: false, found })),
			},
			Kind::FlowSeq { first } | Kind::FlowMap { first } => {
				let seq = matches!(kind, Kind::FlowSeq { .. });
				let end = if seq { K::FlowSeqEnd } else { K::FlowMapEnd };
				let unterminated = |k: K| matches!(k, K::StreamEnd | K::DocStart | K::DocEnd);
				if k == end {
					self.skip()?;
					return self.close(stack);
				}
				if unterminated(k) {
					return Err(self.fail(m, Fail::Unterminated { seq }));
				}
				if !first {
					if k != K::FlowEntry {
						return Err(self.fail(m, Fail::Separator { seq }));
					}
					self.skip()?;
					(k, m) = self.peek()?;
					// A trailing comma: the next step closes (or reports the missing end).
					if k == end || unterminated(k) {
						return Ok(Step::Next);
					}
				}
				top(stack).kind = if seq { Kind::FlowSeq { first: false } } else { Kind::FlowMap { first: false } };
				match k {
					K::Key | K::Value => self.start_entry(top(stack), k, m),
					K::FlowEntry => Err(self.fail(m, Fail::EmptyEntry { seq })),
					_ => {
						// In a mapping, a key with no ':' (`{a, b: 1}`); its value is null.
						if !seq {
							top(stack).want = Want::BareKey(m);
						}
						Ok(Step::Node { block: false, indentless: false })
					}
				}
			}
		}
	}

	/// A finished value for the innermost open collection.
	fn deliver(&mut self, open: &mut Open, v: Value) -> Result<Step, Error> {
		match std::mem::replace(&mut open.want, Want::Item) {
			Want::Item => {
				open.items.push(v);
				Ok(Step::Next)
			}
			Want::Key(m) => {
				let merge = self.is_merge(&v);
				self.start_value(open, v, merge, m)
			}
			Want::BareKey(m) => {
				let merge = self.is_merge(&v);
				open.add(v, Value::Null, merge, m)?;
				Ok(Step::Next)
			}
			Want::Value { key, merge, m } => {
				open.add(key, v, merge, m)?;
				Ok(Step::Next)
			}
		}
	}

	/// A mapping entry (or a pair in a flow sequence) starting with token `k`: its key after
	/// the `?` if there is one, then its value.
	fn start_entry(&mut self, open: &mut Open, k: K, m: Mark) -> Result<Step, Error> {
		if k == K::Key {
			self.skip()?;
			let next = self.peek()?.0;
			if next != K::Value && !open.kind.ends_entry(next) {
				open.want = Want::Key(m);
				let block = open.kind.is_block();
				return Ok(Step::Node { block, indentless: block });
			}
		}
		self.start_value(open, Value::Null, false, m)
	}

	fn start_value(&mut self, open: &mut Open, key: Value, merge: bool, m: Mark) -> Result<Step, Error> {
		if self.peek()?.0 == K::Value {
			self.skip()?;
			if !open.kind.ends_entry(self.peek()?.0) {
				open.want = Want::Value { key, merge, m };
				let block = open.kind.is_block();
				return Ok(Step::Node { block, indentless: block });
			}
		}
		open.add(key, Value::Null, merge, m)?;
		Ok(Step::Next)
	}

	/// Close the innermost open collection.
	fn close(&mut self, stack: &mut Vec<Open>) -> Result<Step, Error> {
		let open = stack.pop().expect("an open collection");
		let value = match open.kind {
			Kind::BlockMap { .. } | Kind::FlowMap { .. } => open.entries.finish(),
			_ => Value::Seq(open.items),
		};
		Ok(Step::Done(self.finish_node(open.props, value, open.at)?))
	}

	#[inline(never)]
	fn alias(&mut self, name: &str, m: Mark) -> Result<Value, Error> {
		if self.open_anchors.iter().any(|a| a == name) {
			return Err(error_at(m, format!("the alias {name:?} is inside its own anchor, which makes a cycle (black-serde values are trees)")));
		}
		let Some((v, n)) = self.anchors.get(name) else {
			return Err(error_at(m, format!("unknown alias {name:?} (no earlier anchor with that name in this document)")));
		};
		self.expanded = self.expanded.saturating_add(*n);
		if self.expanded > self.limits.max_alias_values {
			let msg =
				format!("aliases expand to more than max_alias_values ({}); raise it with with_limits if this document is trusted", self.limits.max_alias_values,);
			let e = Error::limit_exceeded(Limit::AliasValues, msg);
			return Err(e.at_line(m.line + 1, m.col + 1));
		}
		Ok(v.clone())
	}

	#[inline(never)]
	fn scalar(&mut self, text: String, style: Style, tag: Option<&str>, m: Mark) -> Result<Value, Error> {
		let Some(tag) = tag else {
			if style == Style::Plain {
				self.last_merge = text == "<<";
				return Ok(resolve_plain(&text));
			}
			return Ok(Value::String(text));
		};
		if tag == "!" {
			return Ok(Value::String(text));
		}
		let invalid = |what: &str| error_at(m, format!("{text:?} is not a valid {what}"));
		if let Some(core) = tag.strip_prefix(CORE_TAG_PREFIX) {
			return match core {
				"str" => Ok(Value::String(text)),
				"null" => match resolve_plain(&text) {
					Value::Null => Ok(Value::Null),
					_ => Err(invalid("!!null")),
				},
				"bool" => match resolve_plain(&text) {
					b @ Value::Bool(_) => Ok(b),
					_ => Err(invalid("!!bool")),
				},
				"int" => match resolve_plain(&text) {
					i @ (Value::Int(_) | Value::UInt(_)) => Ok(i),
					_ => Err(invalid("!!int")),
				},
				"float" => match resolve_plain(&text) {
					Value::Int(i) => Ok(Value::Float(i as f64)),
					Value::UInt(u) => Ok(Value::Float(u as f64)),
					f @ Value::Float(_) => Ok(f),
					_ => Err(invalid("!!float")),
				},
				"binary" => base64_decode(&text).map(Value::Bytes).ok_or_else(|| invalid("!!binary (base64) value")),
				"merge" if text == "<<" => {
					self.last_merge = true;
					Ok(Value::String(text))
				}
				"merge" => Err(invalid("!!merge key")),
				"seq" | "map" | "set" | "omap" | "pairs" => Err(error_at(m, format!("!!{core} can't be applied to a scalar"))),
				_ => Ok(Value::tagged(tag, Value::String(text))),
			};
		}
		// An application tag: keep it, around the scalar resolved as usual.
		let inner = if style == Style::Plain { resolve_plain(&text) } else { Value::String(text) };
		Ok(Value::tagged(tag, inner))
	}

	#[inline(never)]
	fn collection(&self, v: Value, tag: Option<&str>, m: Mark) -> Result<Value, Error> {
		let Some(tag) = tag else { return Ok(v) };
		if tag == "!" {
			return Ok(v);
		}
		let kind = if matches!(v, Value::Seq(_)) { "sequence" } else { "mapping" };
		let Some(core) = tag.strip_prefix(CORE_TAG_PREFIX) else {
			return Ok(Value::tagged(tag, v));
		};
		let is_seq = matches!(v, Value::Seq(_));
		match core {
			"seq" if is_seq => Ok(v),
			"map" if !is_seq => Ok(v),
			"set" if !is_seq => {
				if v.as_map().expect("a mapping").iter().all(|(_, x)| x.is_null()) {
					Ok(Value::tagged(tag, v))
				} else {
					Err(error_at(m, "!!set entries can't have values"))
				}
			}
			"omap" if is_seq => {
				let mut entries = MapEntries::default();
				for item in single_pairs(v.into_seq().expect("a sequence"), "!!omap", m)? {
					entries.add(item.0, item.1, false, m)?;
				}
				Ok(entries.finish())
			}
			"pairs" if is_seq => {
				let pairs = single_pairs(v.into_seq().expect("a sequence"), "!!pairs", m)?;
				Ok(Value::tagged(tag, Value::Seq(pairs.into_iter().map(|p| Value::Map(vec![p])).collect())))
			}
			"str" | "null" | "bool" | "int" | "float" | "binary" | "merge" | "timestamp" | "seq" | "map" | "set" | "omap" | "pairs" => {
				Err(error_at(m, format!("!!{core} can't be applied to a {kind}")))
			}
			_ => Ok(Value::tagged(tag, v)),
		}
	}

	// ---- helpers for composing ----

	fn skip(&mut self) -> Result<(), Error> {
		self.next().map(drop)
	}

	fn is_merge(&self, key: &Value) -> bool {
		self.merge_keys && self.last_merge && key.as_str() == Some("<<")
	}

	#[inline(never)]
	fn alias_node(&mut self) -> Result<Value, Error> {
		self.last_merge = false;
		let t = self.next()?;
		self.alias(&t.text, t.mark)
	}

	/// The anchor and tag before a node, in either order.
	#[inline(never)]
	fn properties(&mut self) -> Result<Props, Error> {
		self.last_merge = false;
		let mut props = Props::default();
		loop {
			let (k, m) = self.peek()?;
			match k {
				K::Anchor if props.anchor.is_none() => props.anchor = Some(self.next()?.text),
				K::Tag if props.tag.is_none() => {
					let t = self.next()?;
					props.tag = Some(self.resolve_tag(&t)?);
				}
				K::Anchor => return Err(error_at(m, "a value can't have two anchors")),
				K::Tag => return Err(error_at(m, "a value can't have two tags")),
				K::Alias if props.anchor.is_some() || props.tag.is_some() => {
					return Err(error_at(m, "an alias can't have an anchor or a tag"));
				}
				_ => break,
			}
		}
		if let Some(a) = &props.anchor {
			self.open_anchors.push(a.clone());
		}
		Ok(props)
	}

	/// A scalar, or the empty value of a node that has only properties.
	#[inline(never)]
	fn leaf(&mut self, props: Props, k: K, m: Mark) -> Result<Value, Error> {
		let value = match k {
			K::Scalar => {
				let t = self.next()?;
				self.scalar(t.text, t.style, props.tag.as_deref(), t.mark)?
			}
			_ if props.anchor.is_some() || props.tag.is_some() => self.scalar(String::new(), Style::Plain, props.tag.as_deref(), m)?,
			other => return Err(error_at(m, format!("expected a value, found {}", self.describe(other)))),
		};
		self.remember(props.anchor, &value, m)?;
		Ok(value)
	}

	#[inline(never)]
	fn finish_node(&mut self, props: Props, value: Value, m: Mark) -> Result<Value, Error> {
		let value = self.collection(value, props.tag.as_deref(), m)?;
		self.remember(props.anchor, &value, m)?;
		Ok(value)
	}

	/// Keep a copy of an anchored value for its aliases. Copies are counted: nested anchors
	/// copy the same values again at every level, so without a limit a small input could
	/// fill memory without a single alias. Each copy may cost up to `max_alias_values` plus
	/// the input's size, so remembering everything once (or twice) is always fine.
	fn remember(&mut self, anchor: Option<String>, value: &Value, m: Mark) -> Result<(), Error> {
		let Some(a) = anchor else { return Ok(()) };
		self.open_anchors.pop();
		let cost = copy_cost(value);
		self.anchor_copies = self.anchor_copies.saturating_add(cost);
		if self.anchor_copies > self.max_anchor_copies {
			let msg = format!(
				"anchored values copy more than max_alias_values ({}) plus the input's size; raise max_alias_values with with_limits if this document is trusted",
				self.limits.max_alias_values,
			);
			return Err(Error::limit_exceeded(Limit::AliasValues, msg).at_line(m.line + 1, m.col + 1));
		}
		self.anchors.insert(a, (value.clone(), cost));
		Ok(())
	}

	#[cold]
	#[inline(never)]
	fn fail(&self, m: Mark, f: Fail) -> Error {
		let t = &self.tokens;
		let close = |seq: bool| if seq { t.flow_seq_close } else { t.flow_map_close };
		let kind = |seq: bool| if seq { "sequence" } else { "mapping" };
		let msg = match f {
			Fail::TooDeep => {
				let e = self.limits.depth_error();
				return e.at_line(m.line + 1, m.col + 1);
			}
			Fail::Unterminated { seq } => format!("unterminated flow {} (missing '{}')", kind(seq), close(seq)),
			Fail::Separator { seq } => format!("expected '{}' or '{}'", t.flow_entry, close(seq)),
			Fail::EmptyEntry { seq } => {
				format!("expected a {} before '{}'", if seq { "value" } else { "key" }, t.flow_entry)
			}
			Fail::Misplaced { col, seq: true, found } => {
				self.misplaced(m, col, "sequence entry", format!("expected '{} ' for another sequence entry, found {}", t.seq_entry, self.describe(found)))
			}
			Fail::Misplaced { col, seq: false, found } => self.misplaced(m, col, "mapping entry", format!("expected a mapping key, found {}", self.describe(found))),
		};
		error_at(m, msg)
	}
}

/// The anchor and tag written before a node.
#[derive(Default)]
struct Props {
	anchor: Option<String>,
	tag: Option<String>,
}

#[derive(Clone, Copy)]
enum Fail {
	TooDeep,
	Unterminated { seq: bool },
	Separator { seq: bool },
	EmptyEntry { seq: bool },
	Misplaced { col: usize, seq: bool, found: K },
}

/// The entries of `!!omap` or `!!pairs`: a sequence of single-pair mappings.
fn single_pairs(items: Vec<Value>, tag: &str, m: Mark) -> Result<Vec<(Value, Value)>, Error> {
	items
		.into_iter()
		.map(|item| match item.into_map() {
			Ok(mut pairs) if pairs.len() == 1 => Ok(pairs.pop().expect("one pair")),
			_ => Err(error_at(m, format!("{tag} entries must each be a single 'key: value' mapping"))),
		})
		.collect()
}

#[cfg(test)]
mod tests;
