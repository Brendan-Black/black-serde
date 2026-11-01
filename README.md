# black-serde

A small serialization framework with **no dependencies**. It has three separate layers:

- **`Value`**: a format-neutral data model: null, bool, int, uint, float, string, bytes,
  sequence, map (maps keep their key order).
- **`Serialize` / `Deserialize`**: how a Rust type maps to and from a `Value`.
- **`Format`**: how a `Value` maps to and from bytes.

Any type that implements the traits works with every format.

| Format | Module | Notes |
| --- | --- | --- |
| JSON | `json` | RFC 8259; compact, pretty, or pretty with deep containers kept on one line (`Json::pretty(2).inline_from(2)`); errors give line and column |
| YAML | `yaml` | YAML 1.2 core schema: block and flow collections, all quoting styles and escapes, `\|`/`>` block scalars with chomping, comments. Anchors, aliases, tags, complex keys, multi-line plain scalars and multiple documents give clear errors |
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
let json = black_serde::json::to_string(&p);           // {"x":3,"y":-4}
let yaml = black_serde::yaml::to_string(&p);           // x: 3\ny: -4\n
let bin  = black_serde::binary::to_vec(&p);
let back: Point = black_serde::yaml::from_str(&yaml)?;
```

Deserialization errors include the path to the bad value, e.g.
`events[3].button: unknown mouse button "huge"`.

Built-in impls cover `bool`, all integer types (range-checked), `f32`/`f64`, `char`, `String`,
`Option`, `Box`, `Vec`, arrays, tuples up to 6, `HashMap`/`BTreeMap` with string keys
(`HashMap` is written in sorted key order, so output is deterministic), `()`, `Value`, and
`Bytes` (a byte buffer stored compactly in binary).

## Adding a format

Implement `Format`:

```rust
pub trait Format {
    fn encode(&self, value: &Value) -> Vec<u8>;
    fn decode(&self, input: &[u8]) -> Result<Value, Error>;
}
```

Then `black_serde::to_bytes(&MyFormat, &thing)` and `from_bytes` work for every type.
