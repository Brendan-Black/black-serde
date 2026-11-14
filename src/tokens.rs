//! Checks shared by the formats' token sets.

use crate::Error;

/// A text token must be non-empty and free of whitespace and control characters, since
/// whitespace separates tokens.
pub(crate) fn check_text(format: &str, name: &str, token: &str) -> Result<(), Error> {
	if token.is_empty() {
		return Err(Error::new(format!("{format} token {name} can't be empty")));
	}
	if let Some(c) = token.chars().find(|c| c.is_whitespace() || c.is_control()) {
		return Err(Error::new(format!("{format} token {name} ({token:?}) can't contain {c:?}")));
	}
	Ok(())
}

/// Tokens that can appear at the same point must be distinguishable: none may equal, or be a
/// prefix of, another.
pub(crate) fn check_prefix_free(format: &str, group: &[(&str, &str)]) -> Result<(), Error> {
	for (i, (na, a)) in group.iter().enumerate() {
		for (nb, b) in &group[i + 1..] {
			if a.starts_with(b) || b.starts_with(a) {
				return Err(Error::new(format!("{format} tokens {na} ({a:?}) and {nb} ({b:?}) can't be told apart where both can appear")));
			}
		}
	}
	Ok(())
}

/// The token must not start with any of `chars`.
pub(crate) fn check_start(format: &str, name: &str, token: &str, chars: &str, why: &str) -> Result<(), Error> {
	match token.chars().next() {
		Some(c) if chars.contains(c) => Err(Error::new(format!("{format} token {name} ({token:?}) can't start with {c:?}, {why}"))),
		_ => Ok(()),
	}
}

/// A closing quote token must not overlap itself (start with what it ends with, like `''` or
/// `aba`): then text ending in part of the token runs into the real closing token, and the
/// reader finds the end too early.
pub(crate) fn check_no_self_overlap(format: &str, name: &str, token: &str) -> Result<(), Error> {
	let overlap = token.char_indices().skip(1).any(|(i, _)| token.starts_with(&token[i..]));
	if overlap {
		return Err(Error::new(format!("{format} token {name} ({token:?}) can't start with what it ends with, or text ending in part of it would close it early")));
	}
	Ok(())
}
