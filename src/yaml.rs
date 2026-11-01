//! YAML 1.2 as a black-serde [`Format`].
//!
//! Supported: block mappings and sequences (including `- key: value` items and sequences at
//! the same indent as their key), flow `[...]` and `{...}` collections, plain, single-quoted
//! and double-quoted scalars (with all YAML escapes and line folding), literal `|` and folded
//! `>` block scalars with chomping and indentation indicators, comments, a leading `---` and a
//! trailing `...`. Plain scalars resolve with the YAML 1.2 core schema (`null`/`~`, booleans,
//! decimal/`0x`/`0o` integers, floats including `.inf` and `.nan`).
//!
//! Not supported (these give an error rather than a wrong result): anchors and aliases
//! (`&a`, `*a`), tags (`!tag`), complex keys (`? key`), multi-line plain scalars, and more
//! than one document per stream.
//!
//! Output is block style; `Yaml::flow_from(depth)` writes containers at or below a depth in
//! flow style, e.g. one `{t: 0, x: 5}` per line.

use std::fmt::Write as _;

use crate::{Deserialize, Error, Format, Serialize, Value};

/// Nesting deeper than this is rejected, so hostile input can't overflow the stack.
pub const MAX_DEPTH: usize = 256;

/// YAML output settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Yaml {
    /// Spaces per nesting level (at least 1).
    pub indent: usize,
    /// Containers at this depth or deeper are written in flow style on one line.
    pub flow_from_depth: Option<usize>,
}

impl Default for Yaml {
    fn default() -> Self {
        Yaml { indent: 2, flow_from_depth: None }
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

    pub fn write(&self, value: &Value) -> String {
        let mut out = String::new();
        if self.is_block(value, 0) {
            self.write_block(&mut out, value, 0, 0);
        } else {
            write_flow(&mut out, value);
        }
        out.push('\n');
        out
    }

    fn is_block(&self, v: &Value, depth: usize) -> bool {
        let non_empty = match v {
            Value::Seq(s) => !s.is_empty(),
            Value::Map(m) => !m.is_empty(),
            _ => false,
        };
        non_empty && !self.flow_from_depth.is_some_and(|d| depth >= d)
    }

    /// Write a non-empty block container whose first line starts at column `col`
    /// (the caller has already written the indentation).
    fn write_block(&self, out: &mut String, v: &Value, col: usize, depth: usize) {
        match v {
            Value::Map(pairs) => {
                for (i, (k, item)) in pairs.iter().enumerate() {
                    if i > 0 {
                        out.push('\n');
                        spaces(out, col);
                    }
                    write_scalar_string(out, k);
                    out.push(':');
                    if self.is_block(item, depth + 1) {
                        out.push('\n');
                        spaces(out, col + self.indent);
                        self.write_block(out, item, col + self.indent, depth + 1);
                    } else {
                        out.push(' ');
                        write_flow(out, item);
                    }
                }
            }
            Value::Seq(items) => {
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push('\n');
                        spaces(out, col);
                    }
                    out.push_str("- ");
                    if self.is_block(item, depth + 1) {
                        self.write_block(out, item, col + 2, depth + 1);
                    } else {
                        write_flow(out, item);
                    }
                }
            }
            other => write_flow(out, other),
        }
    }
}

impl Format for Yaml {
    fn encode(&self, value: &Value) -> Vec<u8> {
        self.write(value).into_bytes()
    }

    fn decode(&self, input: &[u8]) -> Result<Value, Error> {
        let text = std::str::from_utf8(input)
            .map_err(|e| Error::new("YAML input is not valid UTF-8").at_offset(e.valid_up_to()))?;
        parse(text)
    }
}

/// Serialize to block-style YAML.
pub fn to_string<T: Serialize + ?Sized>(value: &T) -> String {
    Yaml::default().write(&value.serialize())
}

/// Deserialize a `T` from YAML text.
pub fn from_str<T: Deserialize>(text: &str) -> Result<T, Error> {
    T::deserialize(&parse(text)?)
}

// ---------- writing ----------

fn spaces(out: &mut String, n: usize) {
    out.extend(std::iter::repeat_n(' ', n));
}

fn write_flow(out: &mut String, v: &Value) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Int(i) => {
            let _ = write!(out, "{i}");
        }
        Value::UInt(u) => {
            let _ = write!(out, "{u}");
        }
        Value::Float(f) if f.is_nan() => out.push_str(".nan"),
        Value::Float(f) if f.is_infinite() => out.push_str(if *f > 0.0 { ".inf" } else { "-.inf" }),
        Value::Float(f) => {
            let _ = write!(out, "{f:?}");
        }
        Value::String(s) => write_scalar_string(out, s),
        Value::Bytes(bytes) => {
            out.push('[');
            for (i, b) in bytes.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                let _ = write!(out, "{b}");
            }
            out.push(']');
        }
        Value::Seq(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_flow(out, item);
            }
            out.push(']');
        }
        Value::Map(pairs) => {
            out.push('{');
            for (i, (k, item)) in pairs.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_scalar_string(out, k);
                out.push_str(": ");
                write_flow(out, item);
            }
            out.push('}');
        }
    }
}

/// Whether `s` can be written unquoted and read back as the same string, in both block and
/// flow context.
fn plain_ok(s: &str) -> bool {
    let Some(first) = s.chars().next() else { return false };
    if "-?:,[]{}#&*!|>'\"%@`".contains(first) || first.is_whitespace() || s.ends_with(char::is_whitespace) {
        return false;
    }
    let bytes = s.as_bytes();
    for (i, c) in s.char_indices() {
        let next = bytes.get(i + 1).copied();
        let bad = match c {
            ',' | '[' | ']' | '{' | '}' => true,
            ':' => next.is_none_or(|n| n == b' ' || n == b'\t'),
            '#' => i > 0 && matches!(bytes[i - 1], b' ' | b'\t'),
            c => c.is_control() || c == '\u{FEFF}' || c == '\u{2028}' || c == '\u{2029}',
        };
        if bad {
            return false;
        }
    }
    matches!(resolve_plain(s), Value::String(_))
}

fn write_scalar_string(out: &mut String, s: &str) {
    if plain_ok(s) {
        out.push_str(s);
        return;
    }
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            '\u{85}' => out.push_str("\\N"),
            '\u{2028}' => out.push_str("\\L"),
            '\u{2029}' => out.push_str("\\P"),
            '\u{FEFF}' => out.push_str("\\uFEFF"),
            c if c.is_control() => {
                let _ = write!(out, "\\x{:02X}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

// ---------- scalar resolution (YAML 1.2 core schema) ----------

fn resolve_plain(s: &str) -> Value {
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

// ---------- parsing ----------

/// Parse a single YAML document into a [`Value`].
pub fn parse(text: &str) -> Result<Value, Error> {
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    let mut p = Parser { s: text.as_bytes(), pos: 0 };
    p.document()
}

struct Parser<'a> {
    s: &'a [u8],
    pos: usize,
}

fn is_blank_or_end(b: Option<u8>) -> bool {
    matches!(b, None | Some(b' ' | b'\t' | b'\n' | b'\r'))
}

impl Parser<'_> {
    // --- position helpers ---

    fn err(&self, msg: &str) -> Error {
        let before = &self.s[..self.pos.min(self.s.len())];
        let line = before.iter().filter(|&&b| b == b'\n').count() + 1;
        let start = before.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
        let column = String::from_utf8_lossy(&before[start..]).chars().count() + 1;
        Error::new(msg).at_line(line, column)
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.pos).copied()
    }

    fn peek_at(&self, off: usize) -> Option<u8> {
        self.s.get(self.pos + off).copied()
    }

    fn line_start(&self) -> usize {
        self.s[..self.pos].iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1)
    }

    fn col(&self) -> usize {
        self.pos - self.line_start()
    }

    fn at_eol(&self) -> bool {
        matches!(self.peek(), None | Some(b'\n' | b'\r'))
    }

    fn skip_inline_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.pos += 1;
        }
    }

    fn eat_newline(&mut self) -> bool {
        match self.peek() {
            Some(b'\r') => {
                self.pos += 1;
                if self.peek() == Some(b'\n') {
                    self.pos += 1;
                }
                true
            }
            Some(b'\n') => {
                self.pos += 1;
                true
            }
            _ => false,
        }
    }

    fn skip_to_eol(&mut self) {
        while !self.at_eol() {
            self.pos += 1;
        }
    }

    /// After a value: allow trailing spaces and a comment, then require the end of the line.
    fn finish_line(&mut self) -> Result<(), Error> {
        self.skip_inline_ws();
        if self.peek() == Some(b'#') {
            self.skip_to_eol();
        }
        if !self.at_eol() {
            return Err(self.err("unexpected text after the value"));
        }
        self.eat_newline();
        Ok(())
    }

    /// Move to the first content character of the next non-blank, non-comment line and return
    /// its column, or None at the end of input. Safe to call again at the same spot.
    fn next_content(&mut self) -> Result<Option<usize>, Error> {
        let ls = self.line_start();
        if self.s[ls..self.pos].iter().all(|&b| b == b' ' || b == b'\t') {
            self.pos = ls;
        }
        loop {
            let start = self.pos;
            let mut tab_at = None;
            while let Some(b @ (b' ' | b'\t')) = self.peek() {
                if b == b'\t' && tab_at.is_none() {
                    tab_at = Some(self.pos);
                }
                self.pos += 1;
            }
            match self.peek() {
                None => return Ok(None),
                Some(b'#') => self.skip_to_eol(),
                Some(b'\n' | b'\r') => {}
                Some(_) => {
                    if let Some(t) = tab_at {
                        self.pos = t;
                        return Err(self.err("tabs are not allowed for indentation"));
                    }
                    return Ok(Some(self.pos - start));
                }
            }
            self.eat_newline();
        }
    }

    fn at_doc_marker(&self) -> Option<&'static str> {
        if self.col() != 0 {
            return None;
        }
        for m in ["---", "..."] {
            if self.s[self.pos..].starts_with(m.as_bytes()) && is_blank_or_end(self.peek_at(3)) {
                return Some(m);
            }
        }
        None
    }

    fn at_seq_indicator(&self) -> bool {
        self.peek() == Some(b'-') && is_blank_or_end(self.peek_at(1))
    }

    // --- document ---

    fn document(&mut self) -> Result<Value, Error> {
        // Skip directives such as %YAML 1.2 before the first ---.
        while let Some(0) = self.next_content()? {
            if self.peek() != Some(b'%') {
                break;
            }
            self.skip_to_eol();
            self.eat_newline();
        }
        let root = match self.next_content()? {
            None => return Ok(Value::Null),
            Some(_) if self.at_doc_marker() == Some("---") => {
                self.pos += 3;
                self.skip_inline_ws();
                if self.at_eol() || self.peek() == Some(b'#') {
                    self.finish_line()?;
                    self.block_node(-1, 0)?
                } else {
                    self.node_at(self.col(), -1, 0)?
                }
            }
            Some(_) if self.at_doc_marker() == Some("...") => Value::Null,
            Some(col) => self.node_at(col, -1, 0)?,
        };
        match self.next_content()? {
            None => Ok(root),
            Some(_) => match self.at_doc_marker() {
                Some("...") => {
                    self.pos += 3;
                    self.finish_line()?;
                    match self.next_content()? {
                        None => Ok(root),
                        Some(_) => Err(self.err("only one YAML document is supported")),
                    }
                }
                Some(_) => Err(self.err("only one YAML document is supported")),
                None => Err(self.err("unexpected content (check the indentation)")),
            },
        }
    }

    // --- block structure ---

    /// A node on the following lines, indented more than `parent`; null if there is none.
    fn block_node(&mut self, parent: isize, depth: usize) -> Result<Value, Error> {
        match self.next_content()? {
            Some(col) if col as isize > parent && self.at_doc_marker().is_none() => self.node_at(col, parent, depth),
            _ => Ok(Value::Null),
        }
    }

    /// A node starting at the current position, which is at column `col`.
    fn node_at(&mut self, col: usize, parent: isize, depth: usize) -> Result<Value, Error> {
        if depth > MAX_DEPTH {
            return Err(self.err("nested too deeply"));
        }
        if self.at_seq_indicator() {
            return self.block_seq(col, depth);
        }
        if self.peek() == Some(b'?') && is_blank_or_end(self.peek_at(1)) {
            return Err(self.err("complex mapping keys (\"? key\") are not supported"));
        }
        if let Some(key) = self.map_key()? {
            return self.block_map(col, key, depth);
        }
        self.inline_value(parent, depth)
    }

    fn block_seq(&mut self, col: usize, depth: usize) -> Result<Value, Error> {
        let mut items = Vec::new();
        loop {
            self.pos += 1; // '-'
            self.skip_inline_ws();
            let item = if self.at_eol() || self.peek() == Some(b'#') {
                self.finish_line()?;
                self.block_node(col as isize, depth + 1)?
            } else {
                self.node_at(self.col(), col as isize, depth + 1)?
            };
            items.push(item);
            match self.next_content()? {
                Some(c) if c == col && self.at_seq_indicator() && self.at_doc_marker().is_none() => continue,
                Some(c) if c > col => return Err(self.err("bad indentation of a sequence entry")),
                _ => return Ok(Value::Seq(items)),
            }
        }
    }

    fn block_map(&mut self, col: usize, first_key: String, depth: usize) -> Result<Value, Error> {
        let mut pairs: Vec<(String, Value)> = Vec::new();
        let mut key = first_key;
        loop {
            self.skip_inline_ws();
            let value = if self.at_eol() || self.peek() == Some(b'#') {
                self.finish_line()?;
                match self.next_content()? {
                    Some(c) if c > col && self.at_doc_marker().is_none() => self.node_at(c, col as isize, depth + 1)?,
                    // A sequence may sit at the same indent as its key.
                    Some(c) if c == col && self.at_seq_indicator() => self.block_seq(c, depth + 1)?,
                    _ => Value::Null,
                }
            } else if self.at_seq_indicator() {
                return Err(self.err("a block sequence can't start on the same line as its key"));
            } else {
                self.inline_value(col as isize, depth + 1)?
            };
            if pairs.iter().any(|(k, _)| *k == key) {
                return Err(self.err(&format!("duplicate key \"{key}\"")));
            }
            pairs.push((key, value));
            match self.next_content()? {
                Some(c) if c == col && self.at_doc_marker().is_none() => {
                    key = match self.map_key()? {
                        Some(k) => k,
                        None => return Err(self.err("expected a mapping key")),
                    };
                }
                Some(c) if c > col => return Err(self.err("bad indentation of a mapping entry")),
                _ => return Ok(Value::Map(pairs)),
            }
        }
    }

    /// If the current line holds `key:` here, consume it and return the key.
    fn map_key(&mut self) -> Result<Option<String>, Error> {
        let save = self.pos;
        match self.peek() {
            Some(q @ (b'"' | b'\'')) => {
                let line_end = self.s[self.pos..].iter().position(|&b| b == b'\n').map_or(self.s.len(), |i| self.pos + i);
                let key = if q == b'"' { self.double_quoted()? } else { self.single_quoted()? };
                if self.pos > line_end {
                    self.pos = save;
                    return Ok(None);
                }
                self.skip_inline_ws();
                if self.peek() == Some(b':') && is_blank_or_end(self.peek_at(1)) {
                    self.pos += 1;
                    return Ok(Some(key));
                }
                self.pos = save;
                Ok(None)
            }
            Some(b'[' | b'{') | None => Ok(None),
            Some(_) => {
                let mut i = self.pos;
                while i < self.s.len() && !matches!(self.s[i], b'\n' | b'\r') {
                    let b = self.s[i];
                    if b == b'#' && i > self.pos && matches!(self.s[i - 1], b' ' | b'\t') {
                        return Ok(None);
                    }
                    if b == b':' && is_blank_or_end(self.s.get(i + 1).copied()) {
                        let key = std::str::from_utf8(&self.s[self.pos..i]).expect("ASCII boundary").trim_end();
                        if key.is_empty() {
                            return Err(self.err("empty mapping key"));
                        }
                        self.check_plain_start()?;
                        let key = key.to_string();
                        self.pos = i + 1;
                        return Ok(Some(key));
                    }
                    i += 1;
                }
                Ok(None)
            }
        }
    }

    fn check_plain_start(&self) -> Result<(), Error> {
        match self.peek() {
            Some(b'&' | b'*') => Err(self.err("anchors and aliases are not supported")),
            Some(b'!') => Err(self.err("tags are not supported")),
            Some(b'@' | b'`') => Err(self.err("'@' and '`' are reserved and can't start a plain scalar")),
            Some(b'%') if self.col() == 0 => Err(self.err("directives are only allowed before \"---\"")),
            _ => Ok(()),
        }
    }

    /// A scalar or flow collection at the current position, ending the line.
    /// `parent` is the indent of the structure that owns it (for block scalars).
    fn inline_value(&mut self, parent: isize, depth: usize) -> Result<Value, Error> {
        let v = match self.peek() {
            Some(b'|' | b'>') => return self.block_scalar(parent),
            Some(b'[' | b'{') => self.flow_node(depth)?,
            Some(b'"') => Value::String(self.double_quoted()?),
            Some(b'\'') => Value::String(self.single_quoted()?),
            _ => {
                self.check_plain_start()?;
                let start = self.pos;
                while !self.at_eol() {
                    let b = self.peek().expect("not at eol");
                    if b == b'#' && self.pos > start && matches!(self.s[self.pos - 1], b' ' | b'\t') {
                        break;
                    }
                    if b == b':' && is_blank_or_end(self.peek_at(1)) {
                        return Err(self.err("mapping values are not allowed here"));
                    }
                    self.pos += 1;
                }
                let text = std::str::from_utf8(&self.s[start..self.pos]).expect("ASCII boundary").trim_end();
                resolve_plain(text)
            }
        };
        self.finish_line()?;
        // A plain scalar can't continue onto a more-indented line in this parser.
        Ok(v)
    }

    // --- quoted scalars ---

    /// Fold a line break inside a quoted scalar: trim trailing spaces already written, skip
    /// the next line's indentation, then one break becomes a space and n breaks become n-1
    /// newlines.
    fn fold_break(&mut self, out: &mut String) {
        while out.ends_with([' ', '\t']) {
            out.pop();
        }
        let mut breaks = 0;
        loop {
            if self.eat_newline() {
                breaks += 1;
                self.skip_inline_ws();
            } else {
                break;
            }
        }
        if breaks == 1 {
            out.push(' ');
        } else {
            out.extend(std::iter::repeat_n('\n', breaks - 1));
        }
    }

    fn double_quoted(&mut self) -> Result<String, Error> {
        self.pos += 1;
        let mut out = String::new();
        loop {
            let start = self.pos;
            while let Some(b) = self.peek() {
                if matches!(b, b'"' | b'\\' | b'\n' | b'\r') {
                    break;
                }
                self.pos += 1;
            }
            out.push_str(std::str::from_utf8(&self.s[start..self.pos]).expect("ASCII boundary"));
            match self.peek() {
                None => return Err(self.err("unterminated double-quoted string")),
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\n' | b'\r') => self.fold_break(&mut out),
                Some(_) => {
                    self.pos += 1;
                    let esc = self.peek().ok_or_else(|| self.err("unterminated double-quoted string"))?;
                    if matches!(esc, b'\n' | b'\r') {
                        // An escaped line break joins the lines with nothing in between.
                        self.eat_newline();
                        self.skip_inline_ws();
                        continue;
                    }
                    self.pos += 1;
                    let c = match esc {
                        b'0' => '\0',
                        b'a' => '\u{07}',
                        b'b' => '\u{08}',
                        b't' | b'\t' => '\t',
                        b'n' => '\n',
                        b'v' => '\u{0B}',
                        b'f' => '\u{0C}',
                        b'r' => '\r',
                        b'e' => '\u{1B}',
                        b' ' => ' ',
                        b'"' => '"',
                        b'/' => '/',
                        b'\\' => '\\',
                        b'N' => '\u{85}',
                        b'_' => '\u{A0}',
                        b'L' => '\u{2028}',
                        b'P' => '\u{2029}',
                        b'x' => self.hex_escape(2)?,
                        b'u' => self.hex_escape(4)?,
                        b'U' => self.hex_escape(8)?,
                        _ => {
                            self.pos -= 1;
                            return Err(self.err("invalid escape"));
                        }
                    };
                    out.push(c);
                }
            }
        }
    }

    fn hex_escape(&mut self, n: usize) -> Result<char, Error> {
        let digits = self.s.get(self.pos..self.pos + n).ok_or_else(|| self.err("truncated escape"))?;
        let mut code = 0u32;
        for &d in digits {
            code = code * 16 + (d as char).to_digit(16).ok_or_else(|| self.err("invalid hex escape"))?;
        }
        let c = char::from_u32(code).ok_or_else(|| self.err("escape is not a valid character"))?;
        self.pos += n;
        Ok(c)
    }

    fn single_quoted(&mut self) -> Result<String, Error> {
        self.pos += 1;
        let mut out = String::new();
        loop {
            let start = self.pos;
            while let Some(b) = self.peek() {
                if matches!(b, b'\'' | b'\n' | b'\r') {
                    break;
                }
                self.pos += 1;
            }
            out.push_str(std::str::from_utf8(&self.s[start..self.pos]).expect("ASCII boundary"));
            match self.peek() {
                None => return Err(self.err("unterminated single-quoted string")),
                Some(b'\'') if self.peek_at(1) == Some(b'\'') => {
                    out.push('\'');
                    self.pos += 2;
                }
                Some(b'\'') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(_) => self.fold_break(&mut out),
            }
        }
    }

    // --- block scalars ---

    fn block_scalar(&mut self, parent: isize) -> Result<Value, Error> {
        let literal = self.peek() == Some(b'|');
        self.pos += 1;
        let mut chomp = 'c'; // clip
        let mut explicit: Option<usize> = None;
        for _ in 0..2 {
            match self.peek() {
                Some(b'-') if chomp == 'c' => chomp = 's',
                Some(b'+') if chomp == 'c' => chomp = 'k',
                Some(d @ b'1'..=b'9') if explicit.is_none() => explicit = Some((d - b'0') as usize),
                _ => break,
            }
            self.pos += 1;
        }
        self.skip_inline_ws();
        if self.peek() == Some(b'#') {
            self.skip_to_eol();
        }
        if !self.at_eol() {
            return Err(self.err("expected a line break after the block scalar header"));
        }
        self.eat_newline();

        let base = parent.max(-1) + 1; // the first column content may use
        let mut indent = explicit.map(|d| (parent.max(0) as usize) + d);
        let mut lines: Vec<String> = Vec::new();
        loop {
            let line_start = self.pos;
            let mut spaces_n: usize = 0;
            while self.peek() == Some(b' ') {
                spaces_n += 1;
                self.pos += 1;
            }
            let blank = self.at_eol();
            if self.peek().is_none() && spaces_n == 0 {
                break;
            }
            if blank {
                if let Some(ind) = indent {
                    lines.push(" ".repeat(spaces_n.saturating_sub(ind)));
                } else {
                    lines.push(String::new());
                }
                if !self.eat_newline() {
                    break;
                }
                continue;
            }
            let ind = *indent.get_or_insert(spaces_n);
            if spaces_n < ind || (spaces_n as isize) < base || (ind as isize) < base {
                self.pos = line_start;
                break;
            }
            if spaces_n == 0 && self.at_doc_marker().is_some() {
                self.pos = line_start;
                break;
            }
            let content_start = line_start + ind;
            self.skip_to_eol();
            lines.push(String::from_utf8_lossy(&self.s[content_start..self.pos]).into_owned());
            if !self.eat_newline() {
                break;
            }
        }

        let trailing = lines.iter().rev().take_while(|l| l.is_empty()).count();
        let body = &lines[..lines.len() - trailing];
        let mut out = String::new();
        if literal {
            out = body.join("\n");
        } else {
            let more = |l: &str| l.starts_with([' ', '\t']);
            let mut prev: Option<&str> = None;
            let mut empties = 0;
            for line in body {
                if line.is_empty() {
                    empties += 1;
                    continue;
                }
                match prev {
                    None => out.extend(std::iter::repeat_n('\n', empties)),
                    Some(p) => {
                        let keep = more(p) || more(line);
                        if empties == 0 {
                            out.push(if keep { '\n' } else { ' ' });
                        } else {
                            out.extend(std::iter::repeat_n('\n', empties + keep as usize));
                        }
                    }
                }
                out.push_str(line);
                prev = Some(line);
                empties = 0;
            }
        }
        match chomp {
            's' => {}
            'k' => {
                if !body.is_empty() {
                    out.push('\n');
                }
                out.extend(std::iter::repeat_n('\n', trailing));
            }
            _ => {
                if !body.is_empty() {
                    out.push('\n');
                }
            }
        }
        Ok(Value::String(out))
    }

    // --- flow collections ---

    fn skip_flow_ws(&mut self) {
        loop {
            match self.peek() {
                Some(b' ' | b'\t') => self.pos += 1,
                Some(b'\n' | b'\r') => {
                    self.eat_newline();
                }
                Some(b'#') => self.skip_to_eol(),
                _ => return,
            }
        }
    }

    fn flow_node(&mut self, depth: usize) -> Result<Value, Error> {
        if depth > MAX_DEPTH {
            return Err(self.err("nested too deeply"));
        }
        match self.peek() {
            Some(b'[') => self.flow_seq(depth),
            Some(b'{') => self.flow_map(depth),
            Some(b'"') => Ok(Value::String(self.double_quoted()?)),
            Some(b'\'') => Ok(Value::String(self.single_quoted()?)),
            _ => {
                let text = self.flow_plain()?;
                if text.is_empty() {
                    return Err(self.err("expected a value"));
                }
                Ok(resolve_plain(&text))
            }
        }
    }

    fn flow_plain(&mut self) -> Result<String, Error> {
        self.check_plain_start()?;
        let start = self.pos;
        while let Some(b) = self.peek() {
            let ends = match b {
                b',' | b'[' | b']' | b'{' | b'}' | b'\n' | b'\r' => true,
                b'#' => self.pos > start && matches!(self.s[self.pos - 1], b' ' | b'\t'),
                b':' => matches!(self.peek_at(1), None | Some(b' ' | b'\t' | b'\n' | b'\r' | b',' | b']' | b'}')),
                _ => false,
            };
            if ends {
                break;
            }
            self.pos += 1;
        }
        Ok(std::str::from_utf8(&self.s[start..self.pos]).expect("ASCII boundary").trim_end().to_string())
    }

    fn flow_seq(&mut self, depth: usize) -> Result<Value, Error> {
        self.pos += 1; // [
        let mut items = Vec::new();
        loop {
            self.skip_flow_ws();
            match self.peek() {
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Value::Seq(items));
                }
                None => return Err(self.err("unterminated flow sequence")),
                _ => {}
            }
            items.push(self.flow_node(depth + 1)?);
            self.skip_flow_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {}
                Some(b':') => return Err(self.err("key/value pairs inside a flow sequence are not supported")),
                None => return Err(self.err("unterminated flow sequence")),
                _ => return Err(self.err("expected ',' or ']'")),
            }
        }
    }

    fn flow_map(&mut self, depth: usize) -> Result<Value, Error> {
        self.pos += 1; // {
        let mut pairs: Vec<(String, Value)> = Vec::new();
        loop {
            self.skip_flow_ws();
            let key = match self.peek() {
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Value::Map(pairs));
                }
                None => return Err(self.err("unterminated flow mapping")),
                Some(b'"') => self.double_quoted()?,
                Some(b'\'') => self.single_quoted()?,
                Some(b'[' | b'{') => return Err(self.err("collection keys are not supported")),
                _ => self.flow_plain()?,
            };
            if key.is_empty() {
                return Err(self.err("empty mapping key"));
            }
            self.skip_flow_ws();
            let value = if self.peek() == Some(b':') {
                self.pos += 1;
                self.skip_flow_ws();
                if matches!(self.peek(), Some(b',' | b'}')) { Value::Null } else { self.flow_node(depth + 1)? }
            } else {
                Value::Null
            };
            if pairs.iter().any(|(k, _)| *k == key) {
                return Err(self.err(&format!("duplicate key \"{key}\"")));
            }
            pairs.push((key, value));
            self.skip_flow_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {}
                None => return Err(self.err("unterminated flow mapping")),
                _ => return Err(self.err("expected ',' or '}'")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Value {
        Value::from(v)
    }

    fn sample() -> Value {
        Value::map([
            ("name", s("winauto")),
            ("version", Value::Int(1)),
            ("ratio", Value::Float(0.5)),
            ("enabled", Value::Bool(true)),
            ("missing", Value::Null),
            ("tags", Value::Seq(vec![s("input"), s("true"), s("123"), s(""), s("a: b"), s(" padded ")])),
            (
                "events",
                Value::Seq(vec![
                    Value::map([("t", Value::Int(0)), ("type", s("move")), ("x", Value::Int(-5))]),
                    Value::map([("t", Value::Int(16)), ("nested", Value::Seq(vec![Value::Seq(vec![Value::Int(1)])]))]),
                ]),
            ),
            ("empty_seq", Value::Seq(vec![])),
            ("empty_map", Value::Map(vec![])),
            ("text", s("line one\nline \"two\"\t\u{1}")),
            ("unicode", s("h\u{e9}llo \u{1F600}")),
            ("big", Value::UInt(u64::MAX)),
            ("inf", Value::Float(f64::NEG_INFINITY)),
        ])
    }

    #[test]
    fn round_trips_block_and_flow() {
        let v = sample();
        assert_eq!(parse(&Yaml::new().write(&v)).unwrap(), v);
        assert_eq!(parse(&Yaml::new().indent(4).write(&v)).unwrap(), v);
        assert_eq!(parse(&Yaml::new().flow_from(2).write(&v)).unwrap(), v);
        assert_eq!(parse(&Yaml::new().flow_from(0).write(&v)).unwrap(), v);
    }

    #[test]
    fn writes_readable_output() {
        let v = Value::map([
            ("a", Value::Int(1)),
            ("list", Value::Seq(vec![Value::map([("x", Value::Int(1)), ("y", Value::Int(2))]), s("plain")])),
            ("quoted", s("yes: no")),
        ]);
        assert_eq!(Yaml::new().write(&v), "a: 1\nlist:\n  - x: 1\n    y: 2\n  - plain\nquoted: \"yes: no\"\n");
        assert_eq!(Yaml::new().flow_from(2).write(&v), "a: 1\nlist:\n  - {x: 1, y: 2}\n  - plain\nquoted: \"yes: no\"\n");
        assert_eq!(Yaml::new().write(&Value::Int(5)), "5\n");
    }

    #[test]
    fn parses_typical_documents() {
        let doc = r#"
# a comment
%YAML 1.2
---
apiVersion: v1
kind: Pod   # trailing comment
metadata:
  name: web
  labels: {app: web, tier: "front end"}
spec:
  containers:
  - name: nginx
    image: 'nginx:1.27'
    ports:
      - containerPort: 80
      - 443
    args: [--port, 8080, "x, y"]
  empty:
  url: http://example.com/a#b
...
"#;
        let v = parse(doc).unwrap();
        assert_eq!(v.get("kind"), Some(&s("Pod")));
        assert_eq!(v.get("metadata").and_then(|m| m.get("labels")).and_then(|l| l.get("tier")), Some(&s("front end")));
        let spec = v.get("spec").unwrap();
        let c = spec.get("containers").and_then(|c| c.at(0)).unwrap();
        assert_eq!(c.get("image"), Some(&s("nginx:1.27")));
        assert_eq!(c.get("ports").and_then(|p| p.at(0)).and_then(|p| p.get("containerPort")), Some(&Value::Int(80)));
        assert_eq!(c.get("ports").and_then(|p| p.at(1)), Some(&Value::Int(443)));
        assert_eq!(c.get("args"), Some(&Value::Seq(vec![s("--port"), Value::Int(8080), s("x, y")])));
        assert_eq!(spec.get("empty"), Some(&Value::Null));
        assert_eq!(spec.get("url"), Some(&s("http://example.com/a#b")));
    }

    #[test]
    fn scalars_resolve_with_the_core_schema() {
        let cases = [
            ("~", Value::Null),
            ("Null", Value::Null),
            ("TRUE", Value::Bool(true)),
            ("yes", s("yes")),
            ("0x1F", Value::Int(31)),
            ("0o17", Value::Int(15)),
            ("-12", Value::Int(-12)),
            ("+7", Value::Int(7)),
            ("1.5e3", Value::Float(1500.0)),
            (".5", Value::Float(0.5)),
            ("1.", Value::Float(1.0)),
            ("-.inf", Value::Float(f64::NEG_INFINITY)),
            ("1e", s("1e")),
            ("12abc", s("12abc")),
            ("0x", s("0x")),
        ];
        for (text, want) in cases {
            assert_eq!(resolve_plain(text), want, "{text}");
        }
        assert!(matches!(resolve_plain(".NaN"), Value::Float(f) if f.is_nan()));
    }

    #[test]
    fn quoted_scalars() {
        let v = parse(r#"d: "tab\there \x41\u00e9\U0001F600 \"q\" \\ \N\_""#).unwrap();
        assert_eq!(v.get("d"), Some(&s("tab\there A\u{e9}\u{1F600} \"q\" \\ \u{85}\u{a0}")));
        let v = parse("s: 'it''s # not a comment'").unwrap();
        assert_eq!(v.get("s"), Some(&s("it's # not a comment")));
        let v = parse("f: \"one\n  two\n\n  three\\\n  four\"").unwrap();
        assert_eq!(v.get("f"), Some(&s("one two\nthreefour")));
        assert!(parse("x: \"\\q\"").is_err());
        assert!(parse("x: \"open").is_err());
    }

    #[test]
    fn block_scalars() {
        let doc = "lit: |\n  a\n   b\n\n  c\n\n\nfold: >\n  one\n  two\n\n  three\n    indented\n  end\nstrip: |-\n  x\n\nkeep: |+\n  y\n\n\nlast: >2\n    deep\n";
        let v = parse(doc).unwrap();
        assert_eq!(v.get("lit"), Some(&s("a\n b\n\nc\n")));
        assert_eq!(v.get("fold"), Some(&s("one two\nthree\n  indented\nend\n")));
        assert_eq!(v.get("strip"), Some(&s("x")));
        assert_eq!(v.get("keep"), Some(&s("y\n\n\n")));
        assert_eq!(v.get("last"), Some(&s("  deep\n")));
        let v = parse("- |\n  in a list\n- after").unwrap();
        assert_eq!(v, Value::Seq(vec![s("in a list\n"), s("after")]));
    }

    #[test]
    fn nested_sequences_and_compact_forms() {
        let v = parse("- - a\n  - b\n- - c\n-\n  k: v\n- \n").unwrap();
        assert_eq!(
            v,
            Value::Seq(vec![
                Value::Seq(vec![s("a"), s("b")]),
                Value::Seq(vec![s("c")]),
                Value::map([("k", s("v"))]),
                Value::Null,
            ])
        );
        let v = parse("key:\n- 1\n- 2\nother: x").unwrap();
        assert_eq!(v.get("key"), Some(&Value::Seq(vec![Value::Int(1), Value::Int(2)])));
        assert_eq!(v.get("other"), Some(&s("x")));
        let v = parse("\"quoted key\": 1\n'single': 2\n1: number key").unwrap();
        assert_eq!(v.get("quoted key"), Some(&Value::Int(1)));
        assert_eq!(v.get("1"), Some(&s("number key")));
    }

    #[test]
    fn flow_collections() {
        let v = parse("{a: [1, 2, {b: c}], 'd': , e, \"f\": [ ]}").unwrap();
        assert_eq!(
            v,
            Value::map([
                ("a", Value::Seq(vec![Value::Int(1), Value::Int(2), Value::map([("b", s("c"))])])),
                ("d", Value::Null),
                ("e", Value::Null),
                ("f", Value::Seq(vec![])),
            ])
        );
        let v = parse("list: [a,\n  b, # comment\n  c,]").unwrap();
        assert_eq!(v.get("list"), Some(&Value::Seq(vec![s("a"), s("b"), s("c")])));
    }

    #[test]
    fn empty_documents() {
        assert_eq!(parse("").unwrap(), Value::Null);
        assert_eq!(parse("# only a comment\n").unwrap(), Value::Null);
        assert_eq!(parse("---\n").unwrap(), Value::Null);
        assert_eq!(parse("--- 5").unwrap(), Value::Int(5));
        assert_eq!(parse("\u{FEFF}a: 1").unwrap(), Value::map([("a", Value::Int(1))]));
        assert_eq!(parse("a: 1\r\nb: 2\r\n").unwrap(), Value::map([("a", Value::Int(1)), ("b", Value::Int(2))]));
    }

    #[test]
    fn errors() {
        let cases = [
            ("a: 1\n\tb: 2", "tabs are not allowed", (2, 1)),
            ("a:\n  b: 1\n c: 2", "bad indentation of a mapping entry", (3, 2)),
            ("a\n b", "unexpected content", (2, 2)),
            ("a: 1\n  b: 2", "bad indentation of a mapping entry", (2, 3)),
            ("a: &x 1", "anchors", (1, 4)),
            ("a: *x", "anchors", (1, 4)),
            ("a: !tag 1", "tags", (1, 4)),
            ("a: 1\na: 2", "duplicate key", (2, 5)),
            ("a: 1\n---\nb: 2", "one YAML document", (2, 1)),
            ("? complex", "complex mapping keys", (1, 1)),
            ("a: b: c", "mapping values are not allowed", (1, 5)),
            ("a: - b", "can't start on the same line", (1, 4)),
            ("[1, 2", "unterminated flow sequence", (1, 6)),
            ("{a: 1 ]", "expected ',' or '}'", (1, 7)),
            ("- a\n - b", "bad indentation of a sequence entry", (2, 2)),
        ];
        for (doc, msg, (line, column)) in cases {
            let e = parse(doc).unwrap_err();
            assert!(e.message().contains(msg), "{doc:?}: got {e}");
            assert_eq!(e.position(), Some(crate::Position::LineColumn { line, column }), "{doc:?}: got {e}");
        }
    }

    #[test]
    fn depth_limit() {
        let deep = "[".repeat(MAX_DEPTH + 2) + &"]".repeat(MAX_DEPTH + 2);
        assert!(parse(&deep).unwrap_err().message().contains("nested too deeply"));
    }
}
