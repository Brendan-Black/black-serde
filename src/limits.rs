//! Limits on what the readers accept, so hostile or broken input gives an error instead of
//! exhausting the stack or memory.

use crate::{Error, Limit};

/// The prefix of the environment variables that override limits.
pub const ENV_PREFIX: &str = "BLACK_SERDE_";

const DEFAULT_MAX_INPUT_BYTES: usize = 256 << 20;
const DEFAULT_MAX_ALIAS_VALUES: usize = 1_000_000;

/// How much a reader accepts. Going over any of these is an [`Error`] whose
/// [`limit()`](Error::limit) says which one.
///
/// Each format starts from its own defaults ([`Limits::JSON`], [`Limits::YAML`],
/// [`Limits::BINARY`]). Environment variables override them, and limits set in code with a
/// format's `with_limits` override both. The variables are, for each limit,
/// `BLACK_SERDE_<FORMAT>_<LIMIT>` and then `BLACK_SERDE_<LIMIT>`, with `<LIMIT>` one of
/// `MAX_DEPTH`, `MAX_INPUT_BYTES` and `MAX_ALIAS_VALUES`, and `<FORMAT>` one of `JSON`, `YAML`
/// and `BINARY`. A value is a number, optionally with a `K`, `M` or `G` suffix (powers of
/// 1024), or `unlimited`. A variable that's set to anything else makes reading fail, so a typo
/// can't silently leave the default in place.
///
/// Raising `max_depth` far above the default can let deeply nested input overflow the stack
/// of a thread with a small stack; the defaults fit in 1 MB, even in debug builds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The deepest nesting of sequences and maps.
    pub max_depth: usize,
    /// The longest input, in bytes.
    pub max_input_bytes: usize,
    /// The most values YAML aliases may expand to in one document (stops "billion laughs"
    /// input). The other formats have no aliases.
    pub max_alias_values: usize,
}

impl Limits {
    pub const JSON: Limits = Limits {
        max_depth: crate::json::MAX_DEPTH,
        max_input_bytes: DEFAULT_MAX_INPUT_BYTES,
        max_alias_values: DEFAULT_MAX_ALIAS_VALUES,
    };
    pub const YAML: Limits = Limits {
        max_depth: crate::yaml::MAX_DEPTH,
        max_input_bytes: DEFAULT_MAX_INPUT_BYTES,
        max_alias_values: DEFAULT_MAX_ALIAS_VALUES,
    };
    pub const BINARY: Limits = Limits {
        max_depth: crate::binary::MAX_DEPTH,
        max_input_bytes: DEFAULT_MAX_INPUT_BYTES,
        max_alias_values: DEFAULT_MAX_ALIAS_VALUES,
    };

    /// `base` with the environment variables for `format` (such as `"YAML"`) applied.
    pub fn from_env(format: &str, base: Limits) -> Result<Limits, Error> {
        Limits::from_lookup(format, base, |name| std::env::var(name).ok())
    }

    /// Like [`from_env`](Limits::from_env), reading variables through `lookup`.
    pub fn from_lookup(format: &str, base: Limits, lookup: impl Fn(&str) -> Option<String>) -> Result<Limits, Error> {
        let mut limits = base;
        let format = format.to_ascii_uppercase();
        for (name, field) in [
            ("MAX_DEPTH", &mut limits.max_depth),
            ("MAX_INPUT_BYTES", &mut limits.max_input_bytes),
            ("MAX_ALIAS_VALUES", &mut limits.max_alias_values),
        ] {
            for var in [format!("{ENV_PREFIX}{format}_{name}"), format!("{ENV_PREFIX}{name}")] {
                if let Some(text) = lookup(&var) {
                    *field = parse_count(&var, &text)?;
                    break;
                }
            }
        }
        Ok(limits)
    }

    pub(crate) fn check_input(&self, len: usize, format: &str) -> Result<(), Error> {
        if len > self.max_input_bytes {
            return Err(Error::limit_exceeded(
                Limit::InputSize,
                format!(
                    "the input is {len} bytes, more than max_input_bytes ({}); raise it with with_limits or {ENV_PREFIX}{format}_MAX_INPUT_BYTES",
                    self.max_input_bytes
                ),
            ));
        }
        Ok(())
    }

    pub(crate) fn depth_error(&self, format: &str) -> Error {
        Error::limit_exceeded(
            Limit::Depth,
            format!(
                "nested too deeply (more than max_depth, {} levels); raise it with with_limits or {ENV_PREFIX}{format}_MAX_DEPTH",
                self.max_depth
            ),
        )
    }
}

fn parse_count(var: &str, text: &str) -> Result<usize, Error> {
    let t = text.trim();
    if t.eq_ignore_ascii_case("unlimited") {
        return Ok(usize::MAX);
    }
    let (digits, shift) = match t.chars().last().map(|c| c.to_ascii_uppercase()) {
        Some('K') => (&t[..t.len() - 1], 10),
        Some('M') => (&t[..t.len() - 1], 20),
        Some('G') => (&t[..t.len() - 1], 30),
        _ => (t, 0),
    };
    let bad = || {
        Error::new(format!(
            "environment variable {var}={text:?} isn't a limit (use a number, optionally with K, M or G, or \"unlimited\")"
        ))
    };
    let digits: String = digits.chars().filter(|&c| c != '_').collect();
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    let n: usize = digits.parse().map_err(|_| bad())?;
    n.checked_mul(1 << shift).ok_or_else(bad)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let vars: Vec<(String, String)> = vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |name| vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
    }

    #[test]
    fn environment_overrides() {
        let l = Limits::from_lookup("yaml", Limits::YAML, lookup(&[])).unwrap();
        assert_eq!(l, Limits::YAML);
        let l = Limits::from_lookup(
            "yaml",
            Limits::YAML,
            lookup(&[("BLACK_SERDE_MAX_DEPTH", "50"), ("BLACK_SERDE_YAML_MAX_DEPTH", "20"), ("BLACK_SERDE_MAX_INPUT_BYTES", "2M")]),
        )
        .unwrap();
        assert_eq!(l.max_depth, 20, "the format's own variable wins");
        assert_eq!(l.max_input_bytes, 2 << 20);
        assert_eq!(l.max_alias_values, Limits::YAML.max_alias_values);
        let l = Limits::from_lookup("json", Limits::JSON, lookup(&[("BLACK_SERDE_MAX_ALIAS_VALUES", "unlimited")])).unwrap();
        assert_eq!(l.max_alias_values, usize::MAX);
        assert_eq!(parse_count("X", "1_000k").unwrap(), 1000 << 10);
    }

    #[test]
    fn bad_values_are_errors() {
        for bad in ["", "ten", "-1", "1.5", "99999999999999999999", "5T", "16GG"] {
            let e = Limits::from_lookup("json", Limits::JSON, lookup(&[("BLACK_SERDE_MAX_DEPTH", bad)])).unwrap_err();
            assert!(e.to_string().contains("BLACK_SERDE_MAX_DEPTH"), "{bad:?}: {e}");
        }
    }
}
