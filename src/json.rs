//! JSON (RFC 8259) as a black-serde [`Format`].
//!
//! Mapping from the data model: `Bytes` are written as a sequence of integers, and non-finite
//! floats (which JSON can't represent) as `null`. Integers that fit in `i64` read back as
//! `Int`, larger ones that fit in `u64` as `UInt`, anything else as `Float`.

use std::fmt::{self, Write as _};

use crate::{Deserialize, Error, Format, Serialize, Value};

/// Nesting deeper than this is rejected, so hostile input can't overflow the stack.
pub const MAX_DEPTH: usize = 256;

/// JSON output settings. `Json::default()` is compact.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Json {
    /// Spaces per level; `None` writes everything on one line.
    pub indent: Option<usize>,
    /// When indenting, containers at this depth or deeper stay on one line.
    /// `Some(2)` on `{"events": [{...}, {...}]}` puts each event on its own line.
    pub inline_from_depth: Option<usize>,
}

impl Json {
    pub fn compact() -> Self {
        Json::default()
    }

    pub fn pretty(indent: usize) -> Self {
        Json { indent: Some(indent), inline_from_depth: None }
    }

    /// Keep containers at `depth` and deeper on one line.
    pub fn inline_from(mut self, depth: usize) -> Self {
        self.inline_from_depth = Some(depth);
        self
    }

    pub fn write(&self, value: &Value) -> String {
        let mut out = String::new();
        write_value(&mut out, value, self, 0);
        out
    }
}

impl Format for Json {
    fn encode(&self, value: &Value) -> Vec<u8> {
        self.write(value).into_bytes()
    }

    fn decode(&self, input: &[u8]) -> Result<Value, Error> {
        let text = std::str::from_utf8(input)
            .map_err(|e| Error::new("JSON input is not valid UTF-8").at_offset(e.valid_up_to()))?;
        parse(text)
    }
}

/// Serialize to compact JSON.
pub fn to_string<T: Serialize + ?Sized>(value: &T) -> String {
    Json::compact().write(&value.serialize())
}

/// Serialize to indented JSON.
pub fn to_string_pretty<T: Serialize + ?Sized>(value: &T, indent: usize) -> String {
    Json::pretty(indent).write(&value.serialize())
}

/// Deserialize a `T` from JSON text.
pub fn from_str<T: Deserialize>(text: &str) -> Result<T, Error> {
    T::deserialize(&parse(text)?)
}

/// Compact JSON.
impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&Json::compact().write(self))
    }
}

// ---------- writing ----------

fn write_value(out: &mut String, v: &Value, opts: &Json, depth: usize) {
    let indent = match (opts.indent, opts.inline_from_depth) {
        (Some(_), Some(d)) if depth >= d => None,
        (i, _) => i,
    };
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Int(i) => {
            let _ = write!(out, "{i}");
        }
        Value::UInt(u) => {
            let _ = write!(out, "{u}");
        }
        Value::Float(f) if !f.is_finite() => out.push_str("null"),
        // Debug formatting is the shortest text that round-trips, and keeps a ".0" or an
        // exponent so the number reads back as a float.
        Value::Float(f) => {
            let _ = write!(out, "{f:?}");
        }
        Value::String(s) => write_string(out, s),
        Value::Bytes(bytes) => {
            out.push('[');
            for (i, b) in bytes.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                let _ = write!(out, "{b}");
            }
            out.push(']');
        }
        Value::Seq(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                newline(out, indent, depth + 1);
                write_value(out, item, opts, depth + 1);
            }
            newline(out, indent, depth);
            out.push(']');
        }
        Value::Map(pairs) => {
            if pairs.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (i, (k, item)) in pairs.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                newline(out, indent, depth + 1);
                write_string(out, k);
                out.push(':');
                if indent.is_some() {
                    out.push(' ');
                }
                write_value(out, item, opts, depth + 1);
            }
            newline(out, indent, depth);
            out.push('}');
        }
    }
}

fn newline(out: &mut String, indent: Option<usize>, depth: usize) {
    if let Some(n) = indent {
        out.push('\n');
        out.extend(std::iter::repeat_n(' ', n * depth));
    }
}

/// Write `s` as a quoted JSON string.
pub fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
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
    }
    out.push('"');
}

// ---------- parsing ----------

/// Parse a complete JSON document into a [`Value`].
pub fn parse(text: &str) -> Result<Value, Error> {
    let mut p = Parser { src: text.as_bytes(), pos: 0 };
    p.skip_ws();
    let v = p.value(0)?;
    p.skip_ws();
    if p.pos < p.src.len() {
        return Err(p.error("unexpected text after the JSON value"));
    }
    Ok(v)
}

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn error(&self, message: &str) -> Error {
        let before = &self.src[..self.pos.min(self.src.len())];
        let line = before.iter().filter(|&&b| b == b'\n').count() + 1;
        let line_start = before.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
        // Count characters, not bytes, so the column is right for non-ASCII text.
        let column = String::from_utf8_lossy(&before[line_start..]).chars().count() + 1;
        Error::new(message).at_line(line, column)
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn literal(&mut self, word: &str, v: Value) -> Result<Value, Error> {
        if self.src[self.pos..].starts_with(word.as_bytes()) {
            self.pos += word.len();
            Ok(v)
        } else {
            Err(self.error("invalid literal"))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, Error> {
        if depth > MAX_DEPTH {
            return Err(self.error("nested too deeply"));
        }
        match self.peek() {
            None => Err(self.error("unexpected end of input")),
            Some(b'n') => self.literal("null", Value::Null),
            Some(b't') => self.literal("true", Value::Bool(true)),
            Some(b'f') => self.literal("false", Value::Bool(false)),
            Some(b'"') => Ok(Value::String(self.string()?)),
            Some(b'[') => self.seq(depth),
            Some(b'{') => self.map(depth),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.error("expected a JSON value")),
        }
    }

    fn seq(&mut self, depth: usize) -> Result<Value, Error> {
        self.pos += 1; // [
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Value::Seq(items));
        }
        loop {
            self.skip_ws();
            items.push(self.value(depth + 1)?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Value::Seq(items));
                }
                _ => return Err(self.error("expected ',' or ']'")),
            }
        }
    }

    fn map(&mut self, depth: usize) -> Result<Value, Error> {
        self.pos += 1; // {
        let mut pairs = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Value::Map(pairs));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(self.error("expected a string key"));
            }
            let key = self.string()?;
            self.skip_ws();
            if self.peek() != Some(b':') {
                return Err(self.error("expected ':'"));
            }
            self.pos += 1;
            self.skip_ws();
            let v = self.value(depth + 1)?;
            pairs.push((key, v));
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Value::Map(pairs));
                }
                _ => return Err(self.error("expected ',' or '}'")),
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, Error> {
        let digits = self.src.get(self.pos..self.pos + 4).ok_or_else(|| self.error("truncated \\u escape"))?;
        let mut n = 0;
        for &d in digits {
            let v = (d as char).to_digit(16).ok_or_else(|| self.error("invalid \\u escape"))?;
            n = n * 16 + v;
        }
        self.pos += 4;
        Ok(n)
    }

    fn string(&mut self) -> Result<String, Error> {
        self.pos += 1; // opening quote
        let mut out = String::new();
        loop {
            // Copy the run of plain characters in one go.
            let start = self.pos;
            while let Some(b) = self.peek() {
                if b == b'"' || b == b'\\' || b < 0x20 {
                    break;
                }
                self.pos += 1;
            }
            // The input is a &str and we only stop on ASCII bytes, so this slice is valid UTF-8.
            out.push_str(std::str::from_utf8(&self.src[start..self.pos]).expect("valid UTF-8"));

            match self.peek() {
                None => return Err(self.error("unterminated string")),
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.pos += 1;
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
                Some(_) => return Err(self.error("control character in string")),
            }
        }
    }

    fn unicode_escape(&mut self) -> Result<char, Error> {
        let first = self.hex4()?;
        let code = match first {
            0xD800..=0xDBFF => {
                // A high surrogate must be followed by \u and a low surrogate.
                if self.src.get(self.pos..self.pos + 2) != Some(b"\\u") {
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
        let text = std::str::from_utf8(&self.src[start..self.pos]).expect("ASCII");
        if !is_float {
            if let Ok(i) = text.parse::<i64>() {
                return Ok(Value::Int(i));
            }
            if let Ok(u) = text.parse::<u64>() {
                return Ok(Value::UInt(u));
            }
        }
        text.parse::<f64>().map(Value::Float).map_err(|_| self.error("invalid number"))
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
        assert_eq!(parse(r#"{"k": 1, "k": 2}"#).unwrap().get("k"), Some(&Value::Int(2)));
        assert_eq!(parse("[10, 20]").unwrap().at(1), Some(&Value::Int(20)));
        for bad in ["[1,]", "[1 2]", "{\"a\" 1}", "{\"a\":1,}", "{1:2}", "[", "{", "]", "[1] x"] {
            assert!(parse(bad).is_err(), "{bad} should fail");
        }
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
            Json::pretty(2).write(&v),
            "{\n  \"a\": 1,\n  \"b\": [\n    {\n      \"x\": 1,\n      \"y\": 2\n    },\n    null\n  ],\n  \"c\": {}\n}"
        );
        assert_eq!(
            Json::pretty(2).inline_from(2).write(&v),
            "{\n  \"a\": 1,\n  \"b\": [\n    {\"x\":1,\"y\":2},\n    null\n  ],\n  \"c\": {}\n}"
        );
        assert_eq!(parse(&Json::pretty(4).write(&v)).unwrap(), v);
    }

    #[test]
    fn error_positions() {
        let e = parse("{\n  \"a\": tru\n}").unwrap_err();
        assert_eq!(e.to_string(), "invalid literal at line 2, column 8");
        let e = parse("[\"\u{e9}\", x]").unwrap_err();
        assert_eq!(e.position(), Some(crate::Position::LineColumn { line: 1, column: 7 }));
        assert!(Json::compact().decode(&[b'"', 0xFF, b'"']).is_err());
    }

    #[test]
    fn depth_limit() {
        let ok = "[".repeat(MAX_DEPTH) + &"]".repeat(MAX_DEPTH);
        assert!(parse(&ok).is_ok());
        let deep = "[".repeat(MAX_DEPTH + 2) + &"]".repeat(MAX_DEPTH + 2);
        assert_eq!(parse(&deep).unwrap_err().message(), "nested too deeply");
    }
}
