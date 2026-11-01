use super::*;

fn s(v: &str) -> Value {
    Value::from(v)
}

fn i(n: i64) -> Value {
    Value::Int(n)
}

fn seq(items: Vec<Value>) -> Value {
    Value::Seq(items)
}

fn sample() -> Value {
    Value::Map(vec![
        (s("name"), s("winauto")),
        (s("version"), i(1)),
        (s("ratio"), Value::Float(0.5)),
        (s("enabled"), Value::Bool(true)),
        (s("missing"), Value::Null),
        (s("tags"), seq(vec![s("input"), s("true"), s("123"), s(""), s("a: b"), s(" padded "), s("<<"), s("---"), s("- x")])),
        (
            s("events"),
            seq(vec![
                Value::map([("t", i(0)), ("type", s("move")), ("x", i(-5))]),
                Value::map([("t", i(16)), ("nested", seq(vec![seq(vec![i(1)])]))]),
            ]),
        ),
        (s("empty_seq"), seq(vec![])),
        (s("empty_map"), Value::Map(vec![])),
        (s("text"), s("line one\nline \"two\"\t\u{1}")),
        (s("poem"), s("roses\n  are red\n\nviolets\n")),
        (s("kept"), s("a\nb\n\n\n")),
        (s("stripped"), s("a\nb")),
        (s("unicode"), s("h\u{e9}llo \u{1F600}")),
        (s("big"), Value::UInt(u64::MAX)),
        (s("inf"), Value::Float(f64::NEG_INFINITY)),
        (s("bytes"), Value::Bytes(vec![0, 1, 2, 254, 255])),
        (s("no bytes"), Value::Bytes(vec![])),
        (i(42), s("int key")),
        (Value::Null, s("null key")),
        (seq(vec![i(1), i(2)]), s("sequence key")),
        (Value::map([("k", s("v"))]), s("mapping key")),
        (s("point"), Value::tagged("!point", Value::map([("x", i(1)), ("y", i(2))]))),
        (s("set"), Value::tagged("tag:yaml.org,2002:set", Value::map([("a", Value::Null), ("b", Value::Null)]))),
        (s("odd tag"), Value::tagged("tag:example.com,2026:odd thing", s("x"))),
        (s("defaults"), Value::map([("color", s("red")), ("size", i(3)), ("tags", seq(vec![s("a"), s("b")]))])),
        (s("copy"), Value::map([("color", s("red")), ("size", i(3)), ("tags", seq(vec![s("a"), s("b")]))])),
    ])
}

#[test]
fn round_trips_every_style() {
    let v = sample();
    for y in [
        Yaml::new(),
        Yaml::new().indent(4),
        Yaml::new().flow_from(2),
        Yaml::new().flow_from(0),
        Yaml::new().no_anchors(),
        Yaml::new().block_strings(false),
        Yaml::new().anchors(2),
    ] {
        let text = y.write(&v).unwrap();
        assert_eq!(parse(&text).unwrap(), v, "{y:?}\n{text}");
    }
}

#[test]
fn writes_readable_output() {
    let v = Value::map([
        ("a", i(1)),
        ("list", seq(vec![Value::map([("x", i(1)), ("y", i(2))]), s("plain")])),
        ("quoted", s("yes: no")),
    ]);
    assert_eq!(Yaml::new().write(&v).unwrap(), "a: 1\nlist:\n  - x: 1\n    y: 2\n  - plain\nquoted: \"yes: no\"\n");
    assert_eq!(Yaml::new().flow_from(2).write(&v).unwrap(), "a: 1\nlist:\n  - {x: 1, y: 2}\n  - plain\nquoted: \"yes: no\"\n");
    assert_eq!(Yaml::new().write(&i(5)).unwrap(), "5\n");
}

#[test]
fn writes_repeats_as_aliases() {
    let defaults = Value::map([("color", s("red")), ("size", i(3))]);
    let v = Value::map([
        ("defaults", defaults.clone()),
        ("a", defaults.clone()),
        ("b", Value::map([("inner", defaults.clone())])),
        ("list", seq(vec![defaults.clone(), defaults.clone()])),
    ]);
    let text = Yaml::new().write(&v).unwrap();
    assert_eq!(
        text,
        "defaults: &defaults\n  color: red\n  size: 3\na: *defaults\nb:\n  inner: *defaults\nlist:\n  - *defaults\n  - *defaults\n"
    );
    assert_eq!(parse(&text).unwrap(), v);
    // Small repeats are left alone, and anchoring can be turned off.
    let small = Value::map([("a", seq(vec![i(1)])), ("b", seq(vec![i(1)]))]);
    assert_eq!(Yaml::new().write(&small).unwrap(), "a:\n  - 1\nb:\n  - 1\n");
    assert!(!Yaml::new().no_anchors().write(&v).unwrap().contains('&'));
}

#[test]
fn aliases_dont_count_copies_inside_aliased_copies() {
    // `inner` repeats only inside `outer`, which is aliased, so it appears once in the output
    // and needs no anchor of its own.
    let inner = Value::map([("p", i(1)), ("q", i(2))]);
    let outer = Value::map([("one", inner.clone()), ("two", s("x"))]);
    let v = Value::map([("first", outer.clone()), ("second", outer.clone())]);
    let text = Yaml::new().write(&v).unwrap();
    assert_eq!(text, "first: &first\n  one:\n    p: 1\n    q: 2\n  two: x\nsecond: *first\n");
    // Two different subtrees anchored under the same key name get distinct anchors.
    let x = Value::map([("k", i(1)), ("j", i(2))]);
    let y = Value::map([("k", i(3)), ("j", i(4))]);
    let v = seq(vec![Value::map([("item", x.clone())]), Value::map([("item", y.clone())]), x.clone(), y.clone()]);
    let text = Yaml::new().write(&v).unwrap();
    assert!(text.contains("&item ") || text.contains("&item\n"), "{text}");
    assert!(text.contains("&item_2"), "{text}");
    assert_eq!(parse(&text).unwrap(), v);
}

#[test]
fn writes_tags_keys_bytes_and_block_strings() {
    let v = Value::Map(vec![
        (s("p"), Value::tagged("!point", Value::map([("x", i(1))]))),
        (s("set"), Value::tagged("tag:yaml.org,2002:set", Value::map([("a", Value::Null)]))),
        (s("bin"), Value::Bytes(b"hello".to_vec())),
        (s("long"), Value::tagged("tag:example.com,2026:x y", s("z"))),
        (seq(vec![i(1), i(2)]), s("seq key")),
        (i(7), s("int key")),
        (s("7"), s("string key")),
        (s("text"), s("one\ntwo\n")),
        (s("bare"), s("one\ntwo")),
    ]);
    let text = Yaml::new().write(&v).unwrap();
    assert_eq!(
        text,
        "p: !point\n  x: 1\nset: !!set\n  a: null\nbin: !!binary aGVsbG8=\nlong: !<tag:example.com,2026:x%20y> z\n\
         ? [1, 2]\n: seq key\n7: int key\n\"7\": string key\ntext: |\n  one\n  two\nbare: |-\n  one\n  two\n"
    );
    assert_eq!(parse(&text).unwrap(), v);
}

#[test]
fn write_all_and_parse_all() {
    let docs = vec![Value::map([("a", i(1))]), i(2), seq(vec![s("x")]), Value::Null];
    let text = Yaml::new().write_all(&docs).unwrap();
    assert_eq!(text, "---\na: 1\n--- 2\n---\n- x\n--- null\n");
    assert_eq!(parse_all(&text).unwrap(), docs);
    assert_eq!(parse_all("").unwrap(), Vec::<Value>::new());
    assert_eq!(parse_all("a\n...\nb\n").unwrap(), vec![s("a"), s("b")]);
    assert_eq!(parse_all("--- a\n--- b\n...\n%YAML 1.2\n--- c").unwrap(), vec![s("a"), s("b"), s("c")]);
    let e = parse("a: 1\n---\nb: 2").unwrap_err();
    assert!(e.message().contains("more than one YAML document"), "{e}");
    assert_eq!(from_str_all::<i32>("--- 1\n--- 2").unwrap(), vec![1, 2]);
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
    assert_eq!(c.get("ports").and_then(|p| p.at(0)).and_then(|p| p.get("containerPort")), Some(&i(80)));
    assert_eq!(c.get("ports").and_then(|p| p.at(1)), Some(&i(443)));
    assert_eq!(c.get("args"), Some(&seq(vec![s("--port"), i(8080), s("x, y")])));
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
        ("0x1F", i(31)),
        ("0o17", i(15)),
        ("-12", i(-12)),
        ("+7", i(7)),
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
    let e = parse("x: \"open").unwrap_err();
    assert!(e.message().contains("never closed"), "{e}");
    assert_eq!(e.position(), Some(crate::Position::LineColumn { line: 1, column: 4 }));
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
    assert_eq!(v, seq(vec![s("in a list\n"), s("after")]));
    assert_eq!(parse("--- |\n  top\n").unwrap(), s("top\n"));
}

#[test]
fn multi_line_plain_scalars() {
    assert_eq!(parse("a: one\n  two\n\n  three").unwrap(), Value::map([("a", s("one two\nthree"))]));
    assert_eq!(parse("one\ntwo").unwrap(), s("one two"));
    assert_eq!(parse("- a\n  - b").unwrap(), seq(vec![s("a - b")]));
    assert_eq!(parse("[a\n b, c]").unwrap(), seq(vec![s("a b"), s("c")]));
    // A comment ends a plain scalar.
    assert!(parse("a: b\n  # note\n  c").is_err());
}

#[test]
fn nested_sequences_and_compact_forms() {
    let v = parse("- - a\n  - b\n- - c\n-\n  k: v\n- \n").unwrap();
    assert_eq!(v, seq(vec![seq(vec![s("a"), s("b")]), seq(vec![s("c")]), Value::map([("k", s("v"))]), Value::Null]));
    let v = parse("key:\n- 1\n- 2\nother: x").unwrap();
    assert_eq!(v.get("key"), Some(&seq(vec![i(1), i(2)])));
    assert_eq!(v.get("other"), Some(&s("x")));
    let v = parse("\"quoted key\": 1\n'single': 2\n1: number key").unwrap();
    assert_eq!(v.get("quoted key"), Some(&i(1)));
    assert_eq!(v.get_key(&i(1)), Some(&s("number key")));
}

#[test]
fn flow_collections() {
    let v = parse("{a: [1, 2, {b: c}], 'd': , e, \"f\": [ ]}").unwrap();
    assert_eq!(
        v,
        Value::map([
            ("a", seq(vec![i(1), i(2), Value::map([("b", s("c"))])])),
            ("d", Value::Null),
            ("e", Value::Null),
            ("f", seq(vec![])),
        ])
    );
    let v = parse("list: [a,\n  b, # comment\n  c,]").unwrap();
    assert_eq!(v.get("list"), Some(&seq(vec![s("a"), s("b"), s("c")])));
    // Pairs inside sequences, JSON-style adjacent values, and plain scalars containing ':'.
    assert_eq!(parse("[a: 1, b]").unwrap(), seq(vec![Value::map([("a", i(1))]), s("b")]));
    assert_eq!(parse("{\"a\":1, 'b':x}").unwrap(), Value::map([("a", i(1)), ("b", s("x"))]));
    assert_eq!(parse("[a:b, http://x]").unwrap(), seq(vec![s("a:b"), s("http://x")]));
    assert_eq!(parse("[? a : 1]").unwrap(), seq(vec![Value::map([("a", i(1))])]));
}

#[test]
fn complex_keys() {
    let v = parse("? [a, b]\n: pair\n? |\n  block key\n: 2\n? x\n[1, 2]: flow key\n").unwrap();
    assert_eq!(
        v,
        Value::Map(vec![
            (seq(vec![s("a"), s("b")]), s("pair")),
            (s("block key\n"), i(2)),
            (s("x"), Value::Null),
            (seq(vec![i(1), i(2)]), s("flow key")),
        ])
    );
    let v = parse("? - a\n  - b\n: - c\n").unwrap();
    assert_eq!(v, Value::Map(vec![(seq(vec![s("a"), s("b")]), seq(vec![s("c")]))]));
    assert_eq!(parse("{? {k: v} : 1}").unwrap(), Value::Map(vec![(Value::map([("k", s("v"))]), i(1))]));
    // A key that's only a ':'.
    assert_eq!(parse(": v").unwrap(), Value::Map(vec![(Value::Null, s("v"))]));
}

#[test]
fn anchors_and_aliases() {
    let v = parse("base: &b {x: 1, y: [2, 3]}\ncopy: *b\nlist: [*b, &n 5, *n]\nkey: &k name\n*k : aliased key").unwrap();
    let b = Value::map([("x", i(1)), ("y", seq(vec![i(2), i(3)]))]);
    assert_eq!(v.get("copy"), Some(&b));
    assert_eq!(v.get("list"), Some(&seq(vec![b.clone(), i(5), i(5)])));
    assert_eq!(v.get("name"), Some(&s("aliased key")));
    // Anchors can be redefined; aliases use the latest.
    assert_eq!(parse("- &a 1\n- *a\n- &a 2\n- *a").unwrap(), seq(vec![i(1), i(1), i(2), i(2)]));
    // Anchored empty values and anchors on block collections.
    assert_eq!(parse("a: &e\nb: *e").unwrap(), Value::map([("a", Value::Null), ("b", Value::Null)]));
    assert_eq!(parse("a: &m\n  k: v\nb: *m").unwrap().get("b"), Some(&Value::map([("k", s("v"))])));

    let e = parse("a: *nope").unwrap_err();
    assert!(e.message().contains("unknown alias"), "{e}");
    let e = parse("a: &x [1, *x]").unwrap_err();
    assert!(e.message().contains("cycle"), "{e}");
    assert_eq!(e.position(), Some(crate::Position::LineColumn { line: 1, column: 11 }));
    // Anchors don't carry across documents.
    assert!(parse_all("--- &a 1\n--- *a").is_err());
}

#[test]
fn billion_laughs_is_stopped() {
    let mut doc = String::from("a: &a [x, x, x, x, x, x, x, x, x, x]\n");
    for (prev, name) in ["a", "b", "c", "d", "e", "f", "g", "h"].iter().zip(["b", "c", "d", "e", "f", "g", "h", "i"]) {
        let refs = vec![format!("*{prev}"); 10].join(", ");
        doc.push_str(&format!("{name}: &{name} [{refs}]\n"));
    }
    let e = parse(&doc).unwrap_err();
    assert!(e.message().contains("aliases expand to more than"), "{e}");
    // Ordinary reuse is fine, and the limit can be raised.
    let small = "a: &a [1, 2]\nb: [*a, *a, *a]\n";
    let limited = |n| Yaml::new().with_limits(Limits { max_alias_values: n, ..Limits::YAML });
    assert!(limited(9).read(small).is_ok());
    assert_eq!(limited(8).read(small).unwrap_err().limit(), Some(crate::Limit::AliasValues));
    assert_eq!(e.limit(), Some(crate::Limit::AliasValues));
}

#[test]
fn merge_keys() {
    let doc = "\
base: &base {a: 1, b: 2}
extra: &extra {b: 20, c: 30}
one:
  <<: *base
  b: 3
two:
  x: 0
  <<: [*base, *extra]
  d: 4
quoted:
  \"<<\": *base
inline: {<<: *base, a: 9}
";
    let v = parse(doc).unwrap();
    assert_eq!(v.get("one"), Some(&Value::map([("a", i(1)), ("b", i(3))])));
    assert_eq!(v.get("two"), Some(&Value::map([("x", i(0)), ("a", i(1)), ("b", i(2)), ("c", i(30)), ("d", i(4))])));
    // A quoted "<<" is an ordinary key.
    assert_eq!(v.get("quoted").and_then(|q| q.get("<<")), Some(&Value::map([("a", i(1)), ("b", i(2))])));
    assert_eq!(v.get("inline"), Some(&Value::map([("b", i(2)), ("a", i(9))])));
    // Turned off, `<<` is just a key.
    let v = Yaml::new().merge_keys(false).read("m: &m {a: 1}\nn:\n  <<: *m").unwrap();
    assert_eq!(v.get("n").and_then(|n| n.get("<<")), Some(&Value::map([("a", i(1))])));
    assert!(parse("<<: 5").unwrap_err().message().contains("merge key"));
    assert_eq!(parse("!!merge <<: {a: 1}").unwrap(), Value::map([("a", i(1))]));
}

#[test]
fn tags() {
    let doc = "\
%TAG !e! tag:example.com,2026:
---
str: !!str 123
int: !!int \"42\"
float: !!float 3
bool: !!bool true
nothing: !!null ''
force: ! 12
bin: !!binary |
  aGVs
  bG8=
point: !point {x: 1}
local: !thing 5
handle: !e!widget w
verbatim: !<tag:other.org,2026:v> v
set: !!set {a, b}
omap: !!omap [one: 1, two: 2]
pairs: !!pairs [a: 1, a: 2]
when: !!timestamp 2026-11-01
both: &anchor !!str 7
";
    let v = parse(doc).unwrap();
    assert_eq!(v.get("str"), Some(&s("123")));
    assert_eq!(v.get("int"), Some(&i(42)));
    assert_eq!(v.get("float"), Some(&Value::Float(3.0)));
    assert_eq!(v.get("bool"), Some(&Value::Bool(true)));
    assert_eq!(v.get("nothing"), Some(&Value::Null));
    assert_eq!(v.get("force"), Some(&s("12")));
    assert_eq!(v.get("bin"), Some(&Value::Bytes(b"hello".to_vec())));
    assert_eq!(v.get("point"), Some(&Value::tagged("!point", Value::map([("x", i(1))]))));
    assert_eq!(v.get("local"), Some(&Value::tagged("!thing", i(5))));
    assert_eq!(v.get("handle"), Some(&Value::tagged("tag:example.com,2026:widget", s("w"))));
    assert_eq!(v.get("verbatim"), Some(&Value::tagged("tag:other.org,2026:v", s("v"))));
    assert_eq!(v.get("set"), Some(&Value::tagged("tag:yaml.org,2002:set", Value::map([("a", Value::Null), ("b", Value::Null)]))));
    assert_eq!(v.get("omap"), Some(&Value::map([("one", i(1)), ("two", i(2))])));
    assert_eq!(
        v.get("pairs"),
        Some(&Value::tagged("tag:yaml.org,2002:pairs", seq(vec![Value::map([("a", i(1))]), Value::map([("a", i(2))])])))
    );
    assert_eq!(v.get("when"), Some(&Value::tagged("tag:yaml.org,2002:timestamp", s("2026-11-01"))));
    assert_eq!(v.get("both"), Some(&s("7")));
    // Accessors and Deserialize look through tags.
    assert_eq!(v.get("local").and_then(Value::as_i64), Some(5));
    assert_eq!(i32::deserialize(v.get("local").unwrap()), Ok(5));

    for (bad, msg) in [
        ("!!int abc", "not a valid !!int"),
        ("!!seq x", "can't be applied to a scalar"),
        ("!!str [a]", "can't be applied to a sequence"),
        ("!!set {a: 1}", "can't have values"),
        ("!!omap [a, b]", "single 'key: value'"),
        ("!x!y z", "undefined tag handle"),
        ("!!binary '%%%'", "base64"),
        ("!a !b x", "two tags"),
    ] {
        let e = parse(bad).unwrap_err();
        assert!(e.message().contains(msg), "{bad}: {e}");
    }
}

#[test]
fn empty_documents() {
    assert_eq!(parse("").unwrap(), Value::Null);
    assert_eq!(parse("# only a comment\n").unwrap(), Value::Null);
    assert_eq!(parse("---\n").unwrap(), Value::Null);
    assert_eq!(parse("--- 5").unwrap(), i(5));
    assert_eq!(parse("\u{FEFF}a: 1").unwrap(), Value::map([("a", i(1))]));
    assert_eq!(parse("a: 1\r\nb: 2\r\n").unwrap(), Value::map([("a", i(1)), ("b", i(2))]));
}

#[test]
fn errors() {
    let cases = [
        ("a: 1\n\tb: 2", "tabs are not allowed", (2, 1)),
        ("a:\n  b: 1\n c: 2", "bad indentation of a mapping entry", (3, 2)),
        ("a: 1\n  b: 2", "continues onto this line", (2, 4)),
        ("a: 1\na: 2", "duplicate key \"a\"", (2, 1)),
        ("a: 1\nb\nc: 2", "expected ':' after this mapping key", (2, 1)),
        ("a: b: c", "mapping values are not allowed", (1, 5)),
        ("a: - b", "sequence entry isn't allowed here", (1, 4)),
        ("[1, 2", "unterminated flow sequence", (1, 6)),
        ("{a: 1", "unterminated flow mapping", (1, 6)),
        ("{a: 1 ]", "expected ',' or '}'", (1, 7)),
        ("[1,,2]", "expected a value before ','", (1, 4)),
        ("- a\nb: c", "expected '- ' for another sequence entry", (2, 1)),
        ("a: 'x'b", "unexpected text after the value", (1, 7)),
        ("a: @x", "reserved", (1, 4)),
        ("a: x\u{7}", "isn't allowed in YAML", (1, 5)),
        ("a: \"x\"#c", "a comment needs a space", (1, 7)),
        ("%YAML 2.0\n---\na", "unsupported YAML version", (1, 1)),
        ("%FOO\na: 1", "expected '---' after the directives", (2, 1)),
        ("a\n%YAML 1.2\n---\nb", "", (0, 0)),
        ("key: |0\n x", "indentation indicator", (1, 7)),
    ];
    for (doc, msg, (line, column)) in cases {
        let r = parse_all(doc);
        if msg.is_empty() {
            // `%YAML` inside a document is just text.
            assert!(r.is_ok(), "{doc:?}: {r:?}");
            continue;
        }
        let e = r.unwrap_err();
        assert!(e.message().contains(msg), "{doc:?}: got {e}");
        assert_eq!(e.position(), Some(crate::Position::LineColumn { line, column }), "{doc:?}: got {e}");
    }
}

#[test]
fn depth_limit() {
    let deep = "[".repeat(MAX_DEPTH + 2) + &"]".repeat(MAX_DEPTH + 2);
    assert!(parse(&deep).unwrap_err().message().contains("nested too deeply"));
    assert_eq!(parse(&deep).unwrap_err().limit(), Some(crate::Limit::Depth));
    // Depth counts the containers around a value: here a map, a map and a sequence.
    let shallow = Yaml::new().with_limits(Limits { max_depth: 3, ..Limits::YAML });
    assert!(shallow.read("a:\n  b: [1]").is_ok());
    assert_eq!(shallow.read("a:\n  b: [[1]]").unwrap_err().limit(), Some(crate::Limit::Depth));
    let small = Yaml::new().with_limits(Limits { max_input_bytes: 5, ..Limits::YAML });
    assert!(small.read("a: 10").is_ok());
    assert_eq!(small.read("a: 100").unwrap_err().limit(), Some(crate::Limit::InputSize));
}

#[test]
fn base64() {
    for data in [&b""[..], b"f", b"fo", b"foo", b"foob", b"fooba", b"foobar", &[0, 255, 128, 7]] {
        assert_eq!(base64_decode(&base64_encode(data)).as_deref(), Some(data));
    }
    assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    assert_eq!(base64_decode("Zm9v\n YmE="), Some(b"fooba".to_vec()));
    assert_eq!(base64_decode("Zm9"), Some(b"fo".to_vec()));
    assert_eq!(base64_decode("Z"), None);
    assert_eq!(base64_decode("Zm9v!"), None);
}

// ---------- swapped tokens ----------

fn odd_tokens() -> YamlTokens {
    YamlTokens {
        seq_entry: "*-",
        key: "??",
        value: "=",
        flow_entry: ";",
        flow_seq_open: "(",
        flow_seq_close: ")",
        flow_map_open: "<|",
        flow_map_close: "|>",
        comment: "//",
        anchor: "$",
        alias: "@@",
        tag: "^",
        literal: "|:",
        folded: ">:",
        single_quote: "`",
        double_quote: "«",
        directive: "%%",
        doc_start: "===",
        doc_end: "~~~",
    }
}

#[test]
fn every_token_can_be_swapped() {
    let y = Yaml::new().with_tokens(odd_tokens()).unwrap();
    let v = sample();
    for opts in [y, y.flow_from(1), y.no_anchors(), y.block_strings(false)] {
        let text = opts.write(&v).unwrap();
        assert_eq!(opts.read(&text).unwrap(), v, "{text}");
    }
    let text = y.write(&Value::map([("list", seq(vec![i(1), i(2)])), ("m", Value::map([("k", s("a: b, c"))]))])).unwrap();
    assert_eq!(text, "list=\n  *- 1\n  *- 2\nm=\n  k= a: b, c\n");

    let doc = "\
%%YAML 1.2
===
// a comment
base= $b <|x= 1; y= (2; 3)|>
copy= @@b
text= |:
  line one
  line two
q= «say \\«hi\\« « // trailing
s= `it``s`
t= ^point <|x= 1|>
??  (k)
= complex
~~~
";
    let v = y.read(doc).unwrap();
    let b = Value::map([("x", i(1)), ("y", seq(vec![i(2), i(3)]))]);
    assert_eq!(v.get("base"), Some(&b));
    assert_eq!(v.get("copy"), Some(&b));
    assert_eq!(v.get("text"), Some(&s("line one\nline two\n")));
    assert_eq!(v.get("q"), Some(&s("say «hi« ")));
    assert_eq!(v.get("s"), Some(&s("it`s")));
    assert_eq!(v.get("t"), Some(&Value::tagged("!point", Value::map([("x", i(1))]))));
    assert_eq!(v.get_key(&seq(vec![s("k")])), Some(&s("complex")));
    // The standard spelling is now just text.
    assert_eq!(y.read("a: [1, 2]").unwrap(), s("a: [1, 2]"));
}

#[test]
fn token_sets_are_checked() {
    let st = YamlTokens::STANDARD;
    let bad = [
        YamlTokens { comment: "", ..st },
        YamlTokens { comment: "# #", ..st },
        YamlTokens { anchor: "*", ..st },
        YamlTokens { flow_seq_open: "[", flow_map_open: "[[", ..st },
        YamlTokens { seq_entry: ",", ..st },
        YamlTokens { doc_start: "...", ..st },
        YamlTokens { flow_entry: "x", ..st },
        YamlTokens { flow_entry: ".", ..st },
        YamlTokens { double_quote: "n", ..st },
        YamlTokens { tag: "<", ..st },
        YamlTokens { seq_entry: "---", ..st },
    ];
    for t in bad {
        assert!(t.check().is_err(), "{t:?} should be rejected");
    }
    assert!(st.check().is_ok());
    assert!(odd_tokens().check().is_ok());
}

#[test]
fn unwritable_tags_are_errors_with_a_path() {
    use crate::ErrorKind;
    let v = Value::map([("a", seq(vec![i(1), Value::tagged("!outer", Value::tagged("!inner", i(2)))]))]);
    let e = Yaml::new().write(&v).unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Unrepresentable);
    assert!(e.to_string().starts_with("a[1]: YAML can't write a second tag"), "{e}");
    let bytes = Value::map([("b", Value::tagged("!blob", Value::Bytes(vec![1])))]);
    assert!(Yaml::new().write(&bytes).unwrap_err().to_string().starts_with("b: YAML can't write a tag on bytes"));
    assert!(Yaml::new().write(&Value::tagged("", i(1))).is_err());
    // Allowed, the extra tag is dropped.
    let lossy = Yaml::new().lossy(YamlLossy::ALL);
    assert_eq!(lossy.read(&lossy.write(&v).unwrap()).unwrap(), Value::map([("a", seq(vec![i(1), Value::tagged("!outer", i(2))]))]));
    // A tag inside a key is found too.
    let key = Value::Map(vec![(seq(vec![Value::tagged("!x", Value::tagged("!y", i(1)))]), i(1))]);
    assert!(Yaml::new().write(&key).is_err());
}

#[test]
fn writing_checks_depth() {
    let mut v = i(1);
    for _ in 0..4 {
        v = Value::map([("k", v)]);
    }
    let e = Yaml::new().with_limits(Limits { max_depth: 3, ..Limits::YAML }).write(&v).unwrap_err();
    assert_eq!(e.limit(), Some(crate::Limit::Depth));
    assert!(e.to_string().starts_with("k.k.k.k: nested too deeply"), "{e}");
    assert!(Yaml::new().with_limits(Limits { max_depth: 4, ..Limits::YAML }).write(&v).is_ok());
}
