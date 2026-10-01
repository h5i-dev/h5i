use h5i_app_json::{write, Tok, Value};
use proptest::prelude::*;

fn to_serde(v: &Value) -> serde_json::Value {
    match v {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => (*b).into(),
        Value::Num(n) => (*n).into(),
        Value::Str(s) => String::from_utf8(s.clone()).unwrap().into(),
        Value::Arr(xs) => xs.iter().map(to_serde).collect(),
        Value::Obj(fs) => {
            // Last duplicate key wins in serde_json, as in most parsers.
            let mut m = serde_json::Map::new();
            for (k, v) in fs {
                m.insert(String::from_utf8(k.clone()).unwrap(), to_serde(v));
            }
            m.into()
        }
    }
}

fn text(v: &Value) -> String {
    String::from_utf8(v.to_bytes()).unwrap()
}

#[test]
fn escapes() {
    assert_eq!(text(&Value::str("a\"b")), r#""a\"b""#);
    assert_eq!(text(&Value::str("a\\b")), r#""a\\b""#);
    assert_eq!(text(&Value::str("\n\t\u{0}\u{1f}")), r#""\u000a\u0009\u0000\u001f""#);
    assert_eq!(text(&Value::str("é漢😀")), "\"é漢😀\"");
    assert_eq!(text(&Value::str("</script>")), "\"</script>\"");
}

#[test]
fn structure() {
    let v = Value::obj([
        ("a", Value::Arr(vec![1u64.into(), Value::Null, true.into()])),
        ("b", Value::obj([("c", Value::str("x"))])),
        ("e", Value::Arr(vec![])),
    ]);
    assert_eq!(text(&v), r#"{"a":[1,null,true],"b":{"c":"x"},"e":[]}"#);
    assert_eq!(String::from_utf8(write(&vec![Tok::Num(0), Tok::Num(u64::MAX)])).unwrap(), "0,18446744073709551615");
}

/// A body that tries to close its string and add a field stays one string.
#[test]
fn no_breakout() {
    let evil = "\",\"admin\":true,\"x\":\"";
    let v = Value::obj([("body", Value::str(evil))]);
    let parsed: serde_json::Value = serde_json::from_slice(&v.to_bytes()).unwrap();
    assert_eq!(parsed, serde_json::json!({ "body": evil }));
}

fn arb_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<u64>().prop_map(Value::Num),
        any::<String>().prop_map(|s| Value::Str(s.into_bytes())),
    ];
    leaf.prop_recursive(4, 64, 6, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Arr),
            prop::collection::vec((any::<String>(), inner), 0..6)
                .prop_map(|fs| Value::Obj(fs.into_iter().map(|(k, v)| (k.into_bytes(), v)).collect())),
        ]
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]
    #[test]
    fn serde_reads_back(v in arb_value()) {
        let parsed: serde_json::Value = serde_json::from_slice(&v.to_bytes()).unwrap();
        prop_assert_eq!(parsed, to_serde(&v));
    }
}
