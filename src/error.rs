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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Position {
    LineColumn { line: usize, column: usize },
    Offset(usize),
}

impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Error { message: message.into(), path: Vec::new(), position: None }
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

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn path(&self) -> &[Segment] {
        &self.path
    }

    pub fn position(&self) -> Option<Position> {
        self.position
    }

    /// The path as text, e.g. `events[3].x`.
    pub fn path_string(&self) -> String {
        let mut out = String::new();
        for seg in &self.path {
            match seg {
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
