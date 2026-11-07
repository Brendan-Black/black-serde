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

To reject misspelled fields instead of ignoring them, add `.deny_unknown(&["x", "y", "label"])?`
after `Fields::new(v)?`.

Built-in impls cover `bool`, the integer types up to 64 bits (range-checked; `i128`/`u128` aren't
supported), `f32`/`f64` (an `f32` out of range is an error), `char`, `String`, `Option`, `Box`,
`Vec`, arrays, tuples up to 6, `HashMap`/`BTreeMap` with any key type (`HashMap` is written in
sorted key order, so output is deterministic), `()`, `Value`, `Bytes` (a byte buffer stored
compactly in binary and as `!!binary` in YAML) and `Tagged<T>`. Map keys convert between text
and scalars as needed, so `HashMap<u32, _>` round-trips through JSON's string keys; only a
scalar's canonical text converts (`"10"`, not `"010"` or `"0xA"`), and two keys that convert to
the same one are an error rather than one silently replacing the other.

`Option` writes `None` as null, so null always reads back as `None`: `Some(None)` in an
`Option<Option<T>>` (or `Some(())`) can't be told apart from `None`.

Deserializing goes through a `Value`, and `Deserialize` borrows it, so strings and byte buffers
are copied out of it. That keeps the traits simple at the cost of speed; code that owns a
`Value` can move parts out with `into_string`, `into_bytes`, `into_seq`, `into_map` and
`into_tagged`.

## YAML

Reading covers block and flow collections, explicit `? key` and implicit keys of any kind, flow
pairs, multi-line plain scalars, every quoting style and escape, literal and folded block
scalars, comments, anchors and aliases, tags (shorthand, `%TAG` and verbatim), directives and
multi-document streams (`yaml::parse_all`).

- **Aliases** become copies of their anchored value. An alias inside its own anchor is an error
  (a cycle can't be a tree), and so is copying more than `max_alias_values` values (see
  Limits).
- **Merge keys** (`<<: *defaults`) are applied, as most YAML in the wild expects, though YAML 1.2
  dropped them; turn them off with `Yaml::merge_keys(false)`.
- **Tags**: the standard ones set the type (`!!binary` gives bytes, `!!omap` an ordered map);
  any other tag is kept as `Value::Tagged` with its full name.
- **Duplicate keys** are an error, as the spec requires.

Writing uses literal block scalars for multi-line strings, `!!binary` for bytes, tags, and
`? key` for keys that aren't scalars or are too long to be implicit. Strings are quoted whenever
a reader could take them for something else, including YAML 1.1 readers such as PyYAML
(`"no"`, `"on"`, `"1_000"`, `"12:30"`). `Yaml::block_strings(false)` writes every string on one
line.

`Yaml::anchors(n)` writes repeated subtrees of at least `n` values once, with an anchor named
after the key it first appears under, and the copies as aliases:

```yaml
defaults: &defaults
  color: red
  size: 3
a: *defaults
```

It's off by default: output meant for people reads and diffs better without it.

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
like more of a number, a closing quote that starts with what it ends with, like `''`, and so
on).

## Errors instead of silent changes

`Format::encode` returns a `Result`. Writing fails rather than changing the data without
saying so, and `err.kind()` tells the cases apart:

- `ErrorKind::Invalid`: the input is wrong (bad syntax, or a value of the wrong type).
- `ErrorKind::Limit(_)`: the input or value went over a limit (below).
- `ErrorKind::Unrepresentable`: the format can't write the value as it is. JSON can't write
  NaN or infinities, keys that aren't strings, tags or bytes. YAML can't write a tag that
  wouldn't read back as the same tag on the same value: two tags on one value, a tag on bytes,
  an empty tag or `!`, or a standard tag that readers apply rather than keep (`!!int`, `!!str`,
  `!!seq`, ...). Each lossy conversion can be allowed on its own
  (`JsonLossy { non_finite_floats: true, ..JsonLossy::NONE }`, `YamlLossy::ALL`), and then
  happens as documented.

Reading is just as strict: JSON objects with duplicate keys and numbers too large for a
float are errors, as are YAML duplicate keys and malformed escapes.

Errors give the path to the problem, e.g. `scores[1]: JSON can't write NaN`. `Value`'s
`Display` is the one writer that never fails: it shows compact JSON with every lossy
conversion allowed.

## Limits

Readers and writers reject input or values that go over a limit, with an error whose
`limit()` says which: `Limit::Depth`, `Limit::InputSize` or `Limit::AliasValues`.

Nothing in black-serde recurses on the shape of a value: every reader and writer keeps its own
stack, and so do dropping, cloning, comparing and hashing a `Value`. So `max_depth` is only a
policy, not a protection for the thread's stack: once the limit allows it, a million levels
of nesting round-trip through every format on a 1 MB stack (a Windows main thread) in a debug
build, which `tests/deep_nesting.rs` checks. The exceptions are `Deserialize` impls for nested
Rust types, which follow the type's own structure, and `Value`'s derived `Debug`.

| Limit | JSON | YAML | Binary |
| --- | --- | --- | --- |
| `max_depth` (nesting) | 256 | 256 | 256 |
| `max_input_bytes` | 256 MiB | 256 MiB | 256 MiB |
| `max_alias_values` | | 1,000,000 | |

`max_alias_values` bounds what YAML copies for aliases, so "billion laughs" input fails instead
of filling memory. It counts values, plus one value per 32 bytes of each copied string, so
1,000 aliases of a 1 MB string count as about 32 million. Remembering anchored values (each
anchor keeps a copy for its aliases) is allowed up to `max_alias_values` plus the input's size,
which covers any ordinary document but not hundreds of nested anchors around the same data.

Binary input can't make the reader reserve memory it hasn't seen: a container's count is
checked against the bytes left, and only a little room is reserved up front.

To change the limits, pass them in code: `Json::default().with_limits(Limits { max_depth:
1000, ..Limits::JSON })`. Nothing is read from the environment unless the program asks:
`Limits::from_env("YAML", Limits::YAML)?` applies `BLACK_SERDE_YAML_<LIMIT>` and then
`BLACK_SERDE_<LIMIT>` variables (e.g. `BLACK_SERDE_YAML_MAX_DEPTH=50`,
`BLACK_SERDE_MAX_INPUT_BYTES=1G`). Values are numbers with an optional `K`, `M` or `G` suffix,
or `unlimited`; anything else is an error, so a typo can't silently leave the default in
place.

## Adding a format

Implement `Format`:

```rust
pub trait Format {
    fn encode(&self, value: &Value) -> Result<Vec<u8>, Error>;
    fn decode(&self, input: &[u8]) -> Result<Value, Error>;
}
```

Then `black_serde::to_bytes(&MyFormat, &thing)` and `from_bytes` work for every type.
