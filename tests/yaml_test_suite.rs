//! Runs the official YAML test suite (https://github.com/yaml/yaml-test-suite, a `data-*`
//! release) when `YAML_TEST_SUITE` points at a checkout of it:
//!
//! ```text
//! git clone --depth 1 --branch data-2022-01-17 https://github.com/yaml/yaml-test-suite <dir>
//! YAML_TEST_SUITE=<dir> cargo test --test yaml_test_suite -- --ignored --nocapture
//! ```
//!
//! Each case must fail if the suite marks it as an error, and otherwise parse; when the suite
//! gives the expected data as JSON, the documents must match it. Set `YAML_TEST_SUITE_STRICT=1`
//! to fail the test on any mismatch rather than only reporting them.

use std::path::{Path, PathBuf};

use black_serde::Value;
use black_serde::yaml::Yaml;

/// Cases where black-serde deliberately reads YAML differently from the suite, which only
/// checks parsing.
const DIFFERENCES: &[(&str, &str)] = &[
	("2JQS", "two null keys: black-serde rejects duplicate keys, which the spec requires to be unique"),
	("X38W", "the keys [a, b] and *a are the same key, so black-serde rejects the duplicate"),
	("J7PZ", "!!omap becomes an ordered map; the suite keeps it as a list of pairs"),
	("565N", "!!binary becomes bytes; the suite keeps the base64 text"),
];

fn cases(dir: &Path, out: &mut Vec<PathBuf>) {
	if dir.join("in.yaml").exists() {
		out.push(dir.to_path_buf());
		return;
	}
	let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
	entries.sort();
	for e in entries {
		if !e.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')) {
			cases(&e, out);
		}
	}
}

/// Split a stream of JSON texts into its values.
fn json_stream(text: &str) -> Vec<Value> {
	let b = text.as_bytes();
	let mut values = Vec::new();
	let mut i = 0;
	loop {
		while i < b.len() && b[i].is_ascii_whitespace() {
			i += 1;
		}
		if i == b.len() {
			return values;
		}
		let start = i;
		match b[i] {
			b'{' | b'[' => {
				let (mut depth, mut in_str) = (0, false);
				while i < b.len() {
					match (in_str, b[i]) {
						(true, b'\\') => i += 1,
						(true, b'"') | (false, b'"') => in_str = !in_str,
						(false, b'{' | b'[') => depth += 1,
						(false, b'}' | b']') => {
							depth -= 1;
							if depth == 0 {
								i += 1;
								break;
							}
						}
						_ => {}
					}
					i += 1;
				}
			}
			b'"' => {
				i += 1;
				while b[i] != b'"' {
					i += if b[i] == b'\\' { 2 } else { 1 };
				}
				i += 1;
			}
			_ => {
				while i < b.len() && !b[i].is_ascii_whitespace() {
					i += 1;
				}
			}
		}
		values.push(black_serde::json::parse(&text[start..i]).unwrap());
	}
}

/// Reduce a value to what JSON can express: no tags, string keys, numbers as floats, and maps
/// in key order (JSON objects are unordered).
fn normalize(v: &Value) -> Value {
	match v.untagged() {
		Value::Int(_) | Value::UInt(_) | Value::Float(_) => Value::Float(v.as_f64().unwrap()),
		Value::Seq(items) => Value::Seq(items.iter().map(normalize).collect()),
		Value::Map(pairs) => {
			let mut pairs: Vec<_> = pairs.iter().map(|(k, v)| (Value::String(k.key_text()), normalize(v))).collect();
			pairs.sort_by_key(|p| p.0.key_text());
			Value::Map(pairs)
		}
		Value::Bytes(_) => Value::String("<bytes>".into()),
		other => other.clone(),
	}
}

#[test]
#[ignore]
fn yaml_test_suite() {
	let Ok(root) = std::env::var("YAML_TEST_SUITE") else {
		println!("YAML_TEST_SUITE isn't set; skipping");
		return;
	};
	let mut all = Vec::new();
	cases(Path::new(&root), &mut all);
	let yaml = Yaml::new().merge_keys(false);
	let (mut passed, mut skipped, mut failures) = (0, 0, Vec::new());
	for case in &all {
		let name = std::fs::read_to_string(case.join("===")).unwrap_or_default().trim().to_string();
		let id = case.strip_prefix(&root).unwrap().display().to_string();
		if let Some((_, why)) = DIFFERENCES.iter().find(|(d, _)| id.starts_with(d)) {
			println!("SKIP {id} ({name}): {why}");
			skipped += 1;
			continue;
		}
		let input = std::fs::read_to_string(case.join("in.yaml")).unwrap();
		let expect_error = case.join("error").exists();
		// Run each case on its own thread so a panic is reported rather than ending the run.
		let input2 = input.clone();
		let result = std::thread::spawn(move || yaml.read_all(&input2)).join();
		let problem = match (result, expect_error) {
			(Err(_), _) => Some("PANIC".to_string()),
			(Ok(Ok(_)), true) => Some("accepted invalid YAML".to_string()),
			(Ok(Err(_)), true) => None,
			(Ok(Err(e)), false) => Some(format!("rejected: {e}")),
			(Ok(Ok(docs)), false) => match std::fs::read_to_string(case.join("in.json")) {
				Ok(json) => {
					let want: Vec<Value> = json_stream(&json).iter().map(normalize).collect();
					let got: Vec<Value> = docs.iter().map(normalize).collect();
					(got != want).then(|| format!("got {}\n      want {}", Value::Seq(got), Value::Seq(want)))
				}
				Err(_) => None,
			},
		};
		match problem {
			None => passed += 1,
			Some(p) => failures.push(format!("{id} ({name}): {p}\n      input: {input:?}")),
		}
	}
	for f in &failures {
		println!("FAIL {f}");
	}
	println!("\nYAML test suite: {passed} of {} cases pass, {} fail, {skipped} skipped as deliberate differences", all.len(), failures.len());
	if std::env::var("YAML_TEST_SUITE_STRICT").is_ok() {
		assert!(failures.is_empty());
	}
}
