use std::fmt;

use crate::Value;

/// One step into a value: a map key or a sequence index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Segment {
	Key(String),
	Index(usize),
}

/// An encoding, decoding or deserialization error.
///
/// Deserialization errors carry the path to the bad value (`events[3].x`); format errors carry
/// where in the input they were found (a line and column for text, a byte offset for binary).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
	message: String,
	path: Vec<Segment>,
	position: Option<Position>,
	kind: ErrorKind,
}

/// What sort of problem an [`Error`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorKind {
	/// The input is wrong: bad syntax, or a value of the wrong type for what's being read.
	Invalid,
	/// The input or value went over a configured limit.
	Limit(Limit),
	/// The value can't be written in this format without losing something (JSON has no NaN,
	/// for example). The format's lossy options allow it, with the loss described.
	Unrepresentable,
}

/// A configurable limit (see [`Limits`](crate::Limits)) that input went over.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Limit {
	/// Nesting deeper than `max_depth`.
	Depth,
	/// Input longer than `max_input_bytes`.
	InputSize,
	/// Aliases expanding to more than `max_alias_values`.
	AliasValues,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Position {
	LineColumn { line: usize, column: usize },
	Offset(usize),
}

impl Error {
	pub fn new(message: impl Into<String>) -> Self {
		Error { message: message.into(), path: Vec::new(), position: None, kind: ErrorKind::Invalid }
	}

	/// The input or value went over a configured limit.
	pub fn limit_exceeded(limit: Limit, message: impl Into<String>) -> Self {
		Error { kind: ErrorKind::Limit(limit), ..Error::new(message) }
	}

	/// The value can't be written in this format without losing something.
	pub fn unrepresentable(message: impl Into<String>) -> Self {
		Error { kind: ErrorKind::Unrepresentable, ..Error::new(message) }
	}

	pub fn kind(&self) -> ErrorKind {
		self.kind
	}

	/// Which limit was exceeded, if that's what this error is.
	pub fn limit(&self) -> Option<Limit> {
		match self.kind {
			ErrorKind::Limit(l) => Some(l),
			_ => None,
		}
	}

	/// "expected X, found Y" for a value of the wrong kind.
	pub fn expected(what: &str, found: &Value) -> Self {
		Error::new(format!("expected {what}, found {}", found.kind()))
	}

	pub fn missing_field(name: &str) -> Self {
		Error::new(format!("missing field \"{name}\""))
	}

	pub fn at_line(mut self, line: usize, column: usize) -> Self {
		self.position = Some(Position::LineColumn { line, column });
		self
	}

	pub fn at_offset(mut self, offset: usize) -> Self {
		self.position = Some(Position::Offset(offset));
		self
	}

	/// Note that the error happened inside map key `key`.
	pub fn in_key(mut self, key: &str) -> Self {
		self.path.insert(0, Segment::Key(key.to_string()));
		self
	}

	/// Note that the error happened inside sequence element `index`.
	pub fn in_index(mut self, index: usize) -> Self {
		self.path.insert(0, Segment::Index(index));
		self
	}

	/// Note that the error happened at `prefix` (outermost first), all at once. Adding a deep
	/// path one segment at a time with [`in_key`](Error::in_key) would take quadratic time.
	pub(crate) fn in_path(mut self, mut prefix: Vec<Segment>) -> Self {
		prefix.append(&mut self.path);
		self.path = prefix;
		self
	}

	pub fn message(&self) -> &str {
		&self.message
	}

	pub fn path(&self) -> &[Segment] {
		&self.path
	}

	pub fn position(&self) -> Option<Position> {
		self.position
	}

	/// The path as text, e.g. `events[3].x`. A key that would be misread there (empty, or
	/// holding `.`, `[`, `]`, `"`, or whitespace) is written quoted in brackets: `a["b.c"]`.
	pub fn path_string(&self) -> String {
		let mut out = String::new();
		for seg in &self.path {
			match seg {
				Segment::Key(k) if k.is_empty() || k.chars().any(|c| ".[]\"".contains(c) || c.is_whitespace() || c.is_control()) => {
					out.push('[');
					crate::json::write_string(&mut out, k);
					out.push(']');
				}
				Segment::Key(k) => {
					if !out.is_empty() {
						out.push('.');
					}
					out.push_str(k);
				}
				Segment::Index(i) => out.push_str(&format!("[{i}]")),
			}
		}
		out
	}
}

impl fmt::Display for Error {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		if !self.path.is_empty() {
			write!(f, "{}: ", self.path_string())?;
		}
		f.write_str(&self.message)?;
		match self.position {
			Some(Position::LineColumn { line, column }) => write!(f, " at line {line}, column {column}"),
			Some(Position::Offset(o)) => write!(f, " at byte {o}"),
			None => Ok(()),
		}
	}
}

impl std::error::Error for Error {}
