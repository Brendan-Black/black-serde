//! One test per problem found in review, so none of them comes back.

use std::collections::{BTreeMap, HashMap};

use black_serde::binary::Binary;
use black_serde::json::{Json, JsonTokens};
use black_serde::yaml::{Yaml, YamlLossy, YamlTokens};
use black_serde::{Deserialize, ErrorKind, Fields, Format, Limit, Value};

fn varint(out: &mut Vec<u8>, mut n: u64) {
	loop {
		let b = (n & 0x7F) as u8;
		n >>= 7;
		if n == 0 {
			out.push(b);
			return;
		}
		out.push(b | 0x80);
	}
}

// ---------- memory ----------

/// Each nested map claims as many entries as there are bytes left. Trusting those counts
/// reserved about 16 GB for this 1 MB input.
#[test]
fn binary_counts_dont_reserve_memory_up_front() {
	let size = 1 << 20;
	let mut input = b"BSRD\x02".to_vec();
	for _ in 0..250 {
		input.push(9);
		varint(&mut input, (size - 4000) as u64);
	}
	input.resize(size, 0);
	let e = Binary::new().decode(&input).unwrap_err();
	assert!(e.to_string().contains("unexpected end of data"), "{e}");
}

/// Nested anchors and not one alias: each anchor kept its own copy of everything inside it.
#[test]
fn nested_anchors_are_counted() {
	let mut doc = String::new();
	for i in 0..200 {
		doc.push_str(&format!("[&a{i} "));
	}
	doc.push('[');
	doc.push_str(&vec!["1"; 100_000].join(","));
	doc.push(']');
	doc.push_str(&"]".repeat(200));
	let e = black_serde::yaml::parse(&doc).unwrap_err();
	assert_eq!(e.limit(), Some(Limit::AliasValues), "{e}");
	// An anchor around the whole of a large document is fine.
	let big = format!("a: &a [{}]\n", vec!["1"; 1_500_000].join(","));
	assert!(black_serde::yaml::parse(&big).is_ok());
}

/// A 1 MB string copied by 10,000 aliases counted as 10,000 values, well under the limit.
#[test]
fn aliases_of_long_strings_count_their_length() {
	let doc = format!("a: &a \"{}\"\nb: [{}]\n", "x".repeat(1 << 20), vec!["*a"; 10_000].join(","));
	let e = black_serde::yaml::parse(&doc).unwrap_err();
	assert_eq!(e.limit(), Some(Limit::AliasValues), "{e}");
}

/// The scanner reads the text in place, by byte offsets; keys are still measured in characters.
#[test]
fn yaml_keys_are_measured_in_characters() {
	let key = "é".repeat(1000); // 2000 bytes, 1000 characters
	let v = black_serde::yaml::parse(&format!("{key}: 1\n")).unwrap();
	assert_eq!(v.get(&key), Some(&Value::Int(1)));
	let too_long = "é".repeat(1100);
	assert!(black_serde::yaml::parse(&format!("{too_long}: 1\n")).is_err());
	let v = black_serde::yaml::parse("- &ä x\n- *ä\n- !%C3%BC y\n").unwrap();
	assert_eq!(v.at(2), Some(&Value::tagged("!ü", Value::from("y"))));
	assert_eq!(v.at(1), Some(&Value::from("x")));
}

// ---------- writing what can be read back ----------

#[test]
fn yaml_escapes_characters_it_cant_hold() {
	for s in ["x\u{FFFE}y", "\u{FFFF}", "a\n\u{FFFE}\nb\n"] {
		let v = Value::map([("k", Value::from(s))]);
		let text = Yaml::new().write(&v).unwrap();
		assert_eq!(black_serde::yaml::parse(&text).unwrap(), v, "{text}");
	}
}

#[test]
fn yaml_refuses_tags_that_wouldnt_read_back() {
	let core = |s: &str| format!("tag:yaml.org,2002:{s}");
	let bad = [
		Value::tagged("!", Value::Int(5)),
		Value::tagged(core("str"), Value::Int(5)),
		Value::tagged(core("int"), Value::from("abc")),
		Value::tagged(core("seq"), Value::Seq(vec![Value::Int(1)])),
		Value::tagged(core("set"), Value::map([("a", Value::Int(1))])),
		Value::tagged(core("timestamp"), Value::Int(1)),
	];
	for v in bad {
		let e = Yaml::new().write(&v).unwrap_err();
		assert_eq!(e.kind(), ErrorKind::Unrepresentable, "{v:?}");
		// Allowed, the tag is dropped and what's written reads back.
		let text = Yaml::new().lossy(YamlLossy::ALL).write(&v).unwrap();
		assert_eq!(black_serde::yaml::parse(&text).unwrap(), *v.untagged(), "{text}");
	}
	let good = [
		Value::tagged(core("set"), Value::map([("a", Value::Null)])),
		Value::tagged(core("timestamp"), Value::from("2001-12-14")),
		Value::tagged("!point", Value::Int(5)),
	];
	for v in good {
		let text = Yaml::new().write(&v).unwrap();
		assert_eq!(black_serde::yaml::parse(&text).unwrap(), v, "{text}");
	}
}

#[test]
fn self_overlapping_quote_tokens_are_rejected() {
	assert!(Json::compact().with_tokens(JsonTokens { string_open: "''", string_close: "''", ..JsonTokens::STANDARD }).is_err());
	assert!(Yaml::new().with_tokens(YamlTokens { double_quote: "\"\"", ..YamlTokens::STANDARD }).is_err());
	assert!(Json::compact().with_tokens(JsonTokens { string_open: "<<", string_close: ">>", ..JsonTokens::STANDARD }).is_err());
	assert!(Json::compact().with_tokens(JsonTokens { string_open: "«", string_close: "»", ..JsonTokens::STANDARD }).is_ok());
}

#[test]
fn flow_style_writes_long_keys_explicitly() {
	let long = "k".repeat(1100);
	let v = Value::map([("outer", Value::Map(vec![(Value::from(long.as_str()), Value::Int(1))]))]);
	for y in [Yaml::new(), Yaml::new().flow_from(0), Yaml::new().flow_from(1)] {
		let text = y.write(&v).unwrap();
		assert_eq!(black_serde::yaml::parse(&text).unwrap(), v, "{text}");
	}
}

#[test]
fn yaml_quotes_what_yaml_1_1_would_misread() {
	let v = Value::map([("country", Value::from("no")), ("on", Value::from("Yes")), ("n", Value::from("1_000")), ("y", Value::from("12:30"))]);
	assert_eq!(Yaml::new().write(&v).unwrap(), "country: \"no\"\n\"on\": \"Yes\"\nn: \"1_000\"\ny: \"12:30\"\n");
}

#[test]
fn yaml_writes_no_anchors_unless_asked() {
	let p = Value::map([("x", Value::Int(0)), ("y", Value::Int(0))]);
	let v = Value::Seq(vec![p.clone(), p]);
	assert!(!Yaml::new().write(&v).unwrap().contains('&'));
	assert!(Yaml::new().anchors(3).write(&v).unwrap().contains('&'));
}

// ---------- reading without silent changes ----------

#[test]
fn map_keys_only_convert_from_canonical_text() {
	assert_eq!(black_serde::json::from_str::<HashMap<u32, i32>>(r#"{"10": 1, "1": 2}"#).unwrap().len(), 2);
	for bad in [r#"{"010": 1}"#, r#"{"0xA": 1}"#, r#"{"+10": 1}"#] {
		assert!(black_serde::json::from_str::<HashMap<u32, i32>>(bad).is_err(), "{bad}");
	}
	assert_eq!(black_serde::yaml::from_str::<BTreeMap<bool, i32>>("{\"true\": 1}").unwrap()[&true], 1);
}

#[test]
fn keys_that_become_the_same_are_an_error() {
	let e = black_serde::yaml::from_str::<HashMap<String, String>>("{1: a, '1': b}").unwrap_err();
	assert!(e.to_string().contains("duplicate key"), "{e}");
	let e = black_serde::yaml::from_str::<BTreeMap<String, String>>("{1: a, '1': b}").unwrap_err();
	assert!(e.to_string().contains("duplicate key"), "{e}");
}

#[test]
fn json_duplicate_keys_are_an_error() {
	let e = black_serde::json::parse(r#"{"a": 1, "b": {"c": 1, "c": 2}}"#).unwrap_err();
	assert!(e.to_string().starts_with("duplicate key \"c\""), "{e}");
	// Small and large objects use different lookups; both must catch it.
	let keys: Vec<String> = (0..40).map(|i| format!("\"k{i}\": {i}")).collect();
	let text = format!("{{{}, \"k7\": 0}}", keys.join(", "));
	assert!(black_serde::json::parse(&text).is_err());
}

#[test]
fn f32_out_of_range_is_an_error() {
	assert!(f32::deserialize(&Value::Float(1e300)).is_err());
	assert_eq!(f32::deserialize(&Value::Float(0.1)), Ok(0.1f32));
	assert!(f32::deserialize(&Value::Float(f64::INFINITY)).unwrap().is_infinite());
}

#[test]
fn json_numbers() {
	assert!(black_serde::json::parse("1e400").is_err());
	assert!(matches!(black_serde::json::parse("-0").unwrap(), Value::Float(f) if f == 0.0 && f.is_sign_negative()));
	assert_eq!(black_serde::json::parse("\u{FEFF}[1]").unwrap(), Value::Seq(vec![Value::Int(1)]));
}

#[test]
fn yaml_hex_escapes_are_strict() {
	for bad in [r#""\x+1""#, r#""\u+041""#, "!a%+1 x"] {
		assert!(black_serde::yaml::parse(bad).is_err(), "{bad}");
	}
	assert_eq!(black_serde::yaml::parse(r#""\x41B""#).unwrap(), Value::from("AB"));
}

#[test]
fn int_and_uint_with_the_same_value_are_equal() {
	assert_eq!(Value::UInt(5), Value::Int(5));
	assert_ne!(Value::UInt(u64::MAX), Value::Int(-1));
	// Binary reads small unsigned values back as Int, and YAML treats them as the same key.
	assert_eq!(Binary::new().decode(b"BSRD\x02\x04\x05").unwrap(), Value::Int(5));
	let m = Value::Map(vec![(Value::UInt(1), Value::Null), (Value::Int(1), Value::Null)]);
	let e = black_serde::yaml::parse(&Yaml::new().lossy(YamlLossy::ALL).write(&m).unwrap()).unwrap_err();
	assert!(e.to_string().contains("duplicate key"), "{e}");
}

#[test]
fn merge_keys_work_in_flow_sequence_pairs() {
	let v = black_serde::yaml::parse("[<<: {a: 1}]").unwrap();
	assert_eq!(v, Value::Seq(vec![Value::map([("a", Value::Int(1))])]));
}

// ---------- API ----------

#[test]
fn arrays_deserialize() {
	assert_eq!(black_serde::json::from_str::<[u8; 3]>("[1,2,3]").unwrap(), [1, 2, 3]);
	let e = black_serde::json::from_str::<[u8; 3]>("[1,2]").unwrap_err();
	assert_eq!(e.to_string(), "expected 3 elements, found 2");
}

#[test]
fn unknown_fields_can_be_refused() {
	let v = black_serde::json::parse(r#"{"x": 1, "colour": "red"}"#).unwrap();
	assert!(Fields::new(&v).unwrap().deny_unknown(&["x", "colour"]).is_ok());
	let e = Fields::new(&v).unwrap().deny_unknown(&["x", "color"]).unwrap_err();
	assert_eq!(e.to_string(), "colour: unknown field (expected one of x, color)");
}

#[test]
fn values_move_out_without_copying() {
	assert_eq!(Value::from("s").into_string(), Ok("s".to_string()));
	assert_eq!(Value::Bytes(vec![1]).into_bytes(), Ok(vec![1]));
	assert_eq!(Value::tagged("!t", Value::Int(1)).into_tagged(), Ok(("!t".to_string(), Value::Int(1))));
	assert_eq!(Value::Int(1).into_string(), Err(Value::Int(1)));
}

#[test]
fn error_paths_quote_awkward_keys() {
	let v = black_serde::json::parse(r#"{"a.b": {"": [1, {"c d": "x"}]}}"#).unwrap();
	#[derive(Debug)]
	struct N;
	impl Deserialize for N {
		fn deserialize(v: &Value) -> Result<Self, black_serde::Error> {
			Err(black_serde::Error::expected("a number", v))
		}
	}
	let e = black_serde::json::from_str::<HashMap<String, HashMap<String, (i32, HashMap<String, N>)>>>(&v.to_string()).unwrap_err();
	assert_eq!(e.path_string(), r#"["a.b"][""][1]["c d"]"#);
}

#[test]
fn environment_variables_are_ignored_unless_asked_for() {
	// A bad value would make every read fail if the environment were consulted.
	unsafe { std::env::set_var("BLACK_SERDE_JSON_MAX_DEPTH", "not a number") };
	assert!(black_serde::json::parse("[1]").is_ok());
	assert!(black_serde::Limits::from_env("JSON", black_serde::Limits::JSON).is_err());
	unsafe { std::env::remove_var("BLACK_SERDE_JSON_MAX_DEPTH") };
}

#[test]
fn error_messages_read_naturally() {
	let e = black_serde::yaml::parse("[a] b").unwrap_err();
	assert_eq!(e.to_string(), "found a value after the end of the document (check the indentation) at line 1, column 5");
}
