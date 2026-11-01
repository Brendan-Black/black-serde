# black-serde

A small serialization framework with **no dependencies**. It has three separate layers:

- **`Value`**: a format-neutral data model: null, bool, int, uint, float, string, bytes,
  sequence, map (ordered, with keys of any type), and tagged values (`!point {x: 1}`).
- **`Serialize` / `Deserialize`**: how a Rust type maps to and from a `Value`.
- **`Format`**: how a `Value` maps to and from bytes.

Any type that implements the traits works with every format.

| Format | Module | Notes |
| --- | --- | --- |
| JSON | `json` | RFC 8259; compact, pretty, or pretty with deep containers kept on one line (`Json::pretty(2).inline_from(2)`); errors give line and column |
| YAML | `yaml` | The whole YAML 1.2 language (see below); passes every case of the official YAML test suite apart from four deliberate differences |
| Binary | `binary` | Compact tagged encoding with varints; documented byte layout in `binary.rs` |

## Example

```rust
use black_serde::{Deserialize, Error, Fields, MapBuilder, Serialize, Value};

struct Point { x: i32, y: i32, label: Option<String> }

impl Serialize for Point {
    fn serialize(&self) -> Value {
        MapBuilder::new().field("x", &self.x).field("y", &self.y).field_if("label", &self.label).build()
    }
}

impl Deserialize for Point {
    fn deserialize(v: &Value) -> Result<Self, Error> {
        let f = Fields::new(v)?;
        Ok(Point { x: f.req("x")?, y: f.req("y")?, label: f.opt("label")? })
    }
}

let p = Point { x: 3, y: -4, label: None };
let json = black_serde::json::to_string(&p)?;          // {"x":3,"y":-4}
let yaml = black_serde::yaml::to_string(&p)?;          // x: 3\ny: -4\n
let bin  = black_serde::binary::to_vec(&p)?;
let back: Point = black_serde::yaml::from_str(&yaml)?;
```

Deserialization errors include the path to the bad value, e.g.
`events[3].button: unknown mouse button "huge"`.

Built-in impls cover `bool`, all integer types (range-checked), `f32`/`f64`, `char`, `String`,
`Option`, `Box`, `Vec`, arrays, tuples up to 6, `HashMap`/`BTreeMap` with any key type (`HashMap`
is written in sorted key order, so output is deterministic), `()`, `Value`, `Bytes` (a byte
buffer stored compactly in binary and as `!!binary` in YAML) and `Tagged<T>`. Map keys convert
between text and scalars as needed, so `HashMap<u32, _>` round-trips through JSON's string keys.

## YAML

Reading covers block and flow collections, explicit `? key` and implicit keys of any kind, flow
pairs, multi-line plain scalars, every quoting style and escape, literal and folded block
scalars, comments, anchors and aliases, tags (shorthand, `%TAG` and verbatim), directives and
multi-document streams (`yaml::parse_all`).

- **Aliases** become copies of their anchored value. An alias inside its own anchor is an error
  (a cycle can't be a tree), and so is expanding more than `max_alias_values` values.
- **Merge keys** (`<<: *defaults`) are applied; turn them off with `Yaml::merge_keys(false)`.
- **Tags**: the standard ones set the type (`!!binary` gives bytes, `!!omap` an ordered map);
  any other tag is kept as `Value::Tagged` with its full name.

Writing anchors repeated subtrees and writes the copies as aliases:

```yaml
defaults: &defaults
  color: red
  size: 3
a: *defaults
```

Anchors are named after the key the subtree first appears under. Writing also uses literal
block scalars for multi-line strings, `!!binary` for bytes, tags and `? key` for keys that
aren't scalars. `Yaml::no_anchors()` and `Yaml::block_strings(false)` turn those off.

The official [YAML test suite](https://github.com/yaml/yaml-test-suite) runs with
`tests/yaml_test_suite.rs` (see the file for how). 398 of its 402 cases pass. The other four are
deliberate: black-serde rejects duplicate keys, which the spec forbids but the suite doesn't
check, and reads `!!omap` and `!!binary` into an ordered map and bytes.

## Swappable tokens

Every format's syntax comes from a token set that can be changed one token at a time.

```rust
use black_serde::json::{Json, JsonTokens};

let angle = Json::compact().with_tokens(JsonTokens { map_open: "<|", map_close: "|>", ..JsonTokens::STANDARD })?;
angle.write(&value)?;  // <|"text":"a <| b |> c"|>
```

- **`JsonTokens`**: braces, brackets, `,`, `:`, string quotes, `null`, `true`, `false`.
- **`YamlTokens`**: every indicator: `-`, `?`, `:`, `,`, `[`, `]`, `{`, `}`, `#`, `&`, `*`, `!`,
  `|`, `>`, both quotes, `%`, `---`, `...`.
- **`BinaryTokens`**: the magic bytes and each type's tag byte.

The tokens are recognized by the tokenizer, not substituted into the text, so strings can
contain them freely. A token set is checked before use and rejected if two tokens could be
confused where both can appear (one being a prefix of the other, a closing token that looks
like more of a number, and so on).

## Errors instead of silent changes

`Format::encode` returns a `Result`. Writing fails rather than changing the data without
saying so, and `err.kind()` tells the cases apart:

- `ErrorKind::Invalid`: the input is wrong (bad syntax, or a value of the wrong type).
- `ErrorKind::Limit(_)`: the input or value went over a limit (below).
- `ErrorKind::Unrepresentable`: the format can't write the value as it is. JSON can't write
  NaN or infinities, keys that aren't strings, tags or bytes. YAML can't write two tags on one
  value, a tag on bytes, or an empty tag. Each lossy conversion can be allowed on its own
  (`JsonLossy { non_finite_floats: true, ..JsonLossy::NONE }`, `YamlLossy::ALL`), and then
  happens as documented.

Errors give the path to the problem, e.g. `scores[1]: JSON can't write NaN`. `Value`'s
`Display` is the one writer that never fails: it shows compact JSON with every lossy
conversion allowed.

## Limits

Readers and writers reject input or values that go over a limit, with an error whose
`limit()` says which: `Limit::Depth`, `Limit::InputSize` or `Limit::AliasValues`.

JSON and binary keep their own stack instead of recursing, and dropping a `Value` doesn't
recurse either, so for them `max_depth` is only a policy: a million levels round-trip on a
1 MB stack once the limit allows it. YAML still recurses, so its limit also protects the
stack.

| Limit | JSON | YAML | Binary |
| --- | --- | --- | --- |
| `max_depth` (nesting) | 256 | 100 | 128 |
| `max_input_bytes` | 256 MiB | 256 MiB | 256 MiB |
| `max_alias_values` | | 1,000,000 | |

The depth defaults leave room to spare on a 1 MB stack (a Windows main thread) even in a debug
build, which `tests/deep_nesting.rs` checks.

To change them, either pass them in code (`Json::default().with_limits(Limits { max_depth: 1000,
..Limits::JSON })`), or set environment variables: `BLACK_SERDE_<FORMAT>_<LIMIT>`, then
`BLACK_SERDE_<LIMIT>`, e.g. `BLACK_SERDE_YAML_MAX_DEPTH=50` or `BLACK_SERDE_MAX_INPUT_BYTES=1G`.
Values are numbers with an optional `K`, `M` or `G` suffix, or `unlimited`. A variable set to
anything else makes reading fail, so a typo can't silently leave the default in place.

## Adding a format

Implement `Format`:

```rust
pub trait Format {
    fn encode(&self, value: &Value) -> Result<Vec<u8>, Error>;
    fn decode(&self, input: &[u8]) -> Result<Value, Error>;
}
```

Then `black_serde::to_bytes(&MyFormat, &thing)` and `from_bytes` work for every type.
