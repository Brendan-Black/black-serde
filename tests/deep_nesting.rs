//! Deep nesting must never overflow the stack, even on a 1 MB stack (a Windows main thread) in
//! a debug build: past `max_depth` it's an error, and JSON and binary handle any depth below
//! it because they keep their own stack instead of recursing.

use black_serde::binary::Binary;
use black_serde::json::Json;
use black_serde::{Format, Limit, Limits, Value};

fn on_small_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(1 << 20).spawn(f).unwrap().join().unwrap()
}

/// `[[[...[1]...]]]`, nested `depth` times, built without recursion.
fn deep_value(depth: usize) -> Value {
    let mut v = Value::Int(1);
    for _ in 0..depth {
        v = Value::Seq(vec![v]);
    }
    v
}

#[test]
fn deep_yaml_is_an_error_not_a_crash() {
    let depth = 100_000;
    let docs = [
        "[".repeat(depth) + &"]".repeat(depth),
        "{a: ".repeat(depth) + "1" + &"}".repeat(depth),
        (0..2000).map(|i| format!("{}k:\n", " ".repeat(i))).collect::<String>(),
        (0..2000).map(|i| format!("{}-\n", " ".repeat(i * 2))).collect::<String>(),
        (0..2000).map(|i| format!("{}? \n", " ".repeat(i * 2))).collect::<String>(),
    ];
    for doc in docs {
        let e = on_small_stack(move || black_serde::yaml::parse(&doc).unwrap_err());
        assert_eq!(e.limit(), Some(Limit::Depth), "{e}");
    }
    let e = on_small_stack(|| black_serde::yaml::Yaml::new().write(&deep_value(100_000)).unwrap_err());
    assert_eq!(e.limit(), Some(Limit::Depth));
}

#[test]
fn deep_json_and_binary_are_errors_not_crashes() {
    let depth = 100_000;
    let json = "[".repeat(depth) + &"]".repeat(depth);
    let e = on_small_stack(move || black_serde::json::parse(&json).unwrap_err());
    assert_eq!(e.limit(), Some(Limit::Depth));

    let mut bin = b"BSRD\x02".to_vec();
    for _ in 0..depth {
        bin.extend_from_slice(&[8, 1]);
    }
    bin.push(0);
    let e = on_small_stack(move || Binary::new().decode(&bin).unwrap_err());
    assert_eq!(e.limit(), Some(Limit::Depth));

    let e = on_small_stack(move || Json::compact().write(&deep_value(depth)).unwrap_err());
    assert_eq!(e.limit(), Some(Limit::Depth));
    let e = on_small_stack(move || Binary::new().encode(&deep_value(depth)).unwrap_err());
    assert_eq!(e.limit(), Some(Limit::Depth));
}

#[test]
fn json_and_binary_go_as_deep_as_the_limit_allows() {
    // A million levels round-trips on a 1 MB stack once the limit allows it.
    let depth = 1_000_000;
    on_small_stack(move || {
        let unlimited = Limits { max_depth: usize::MAX, ..Limits::JSON };
        let v = deep_value(depth);

        let json = Json::compact().with_limits(unlimited);
        let text = json.write(&v).unwrap();
        assert_eq!(text.len(), 2 * depth + 1);
        let back = json.read(&text).unwrap();
        assert!(back.as_seq().is_some());

        let binary = Binary::new().with_limits(unlimited);
        let bytes = binary.encode(&v).unwrap();
        let back2 = binary.decode(&bytes).unwrap();
        // Comparing and dropping these must not recurse either.
        drop(back);
        drop(back2);
        drop(v);
    });
}

#[test]
fn dropping_deep_values_doesnt_recurse() {
    on_small_stack(|| {
        drop(deep_value(1_000_000));
        let mut m = Value::Null;
        for _ in 0..1_000_000 {
            m = Value::map([("k", m)]);
        }
        drop(m);
        let mut t = Value::Null;
        for _ in 0..1_000_000 {
            t = Value::tagged("!t", Value::Seq(vec![t]));
        }
        drop(t);
    });
}
