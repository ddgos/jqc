//! Property-based differential fuzzing: for a curated set of `jq!`-compiled
//! filters, generate arbitrary JSON inputs with proptest and check that the
//! compiled filter's output matches the real `jq` binary's output (or that
//! both error -- we don't require matching error *messages*, just matching
//! success/failure and, on success, matching values).
//!
//! Filters themselves have to be string literals (macro-expansion-time
//! constants), so "arbitrary scripts" are covered separately in
//! `fuzz_scripts.rs`; this file fuzzes the *input* side of a fixed filter
//! set drawn from the differential-test corpus.

use jqc::core::Value;
use proptest::prelude::*;
use std::io::Write;
use std::process::{Command, Stdio};

/// Every valid Unicode scalar value is a legal `char`, so collecting an
/// arbitrary number of them is already a correctly-encoded (if sometimes
/// bizarre: control characters, combining marks with nothing to combine
/// with, astral-plane codepoints needing a UTF-16 surrogate pair once
/// re-encoded as JSON's `\uXXXX`) arbitrary UTF-8 string -- no separate
/// "is this valid UTF-8" concern the way there would be generating raw
/// bytes instead.
fn arb_utf8_string() -> impl Strategy<Value = String> {
    prop::collection::vec(any::<char>(), 0..8).prop_map(|cs| cs.into_iter().collect())
}

/// Leaf strings mix a small, shrinkable, structurally-boring alphabet
/// (keeps most failures easy to read) with fully arbitrary UTF-8 (the
/// part this strategy exists for -- control characters, combining marks,
/// astral-plane codepoints needing a surrogate pair in JSON's own
/// `\uXXXX` escaping, lookalike whitespace, etc., none of which
/// `[a-z]{0,6}` could ever produce).
fn arb_json(depth: u32) -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        (-1000i64..1000i64).prop_map(|n| serde_json::json!(n)),
        "[a-z]{0,6}".prop_map(Value::String),
        arb_utf8_string().prop_map(Value::String),
    ];
    leaf.prop_recursive(depth, 40, 5, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
            prop::collection::btree_map(
                prop_oneof![
                    Just("a".to_string()),
                    Just("b".to_string()),
                    Just("c".to_string()),
                    Just("active".to_string()),
                    Just("score".to_string()),
                    Just("items".to_string()),
                ],
                inner,
                0..4
            )
            .prop_map(|m| Value::Object(m.into_iter().collect())),
        ]
    })
}

/// As [`arb_json`], but for filters this file specifically wants to
/// exercise on *string* input (`explode`, `split`, …): strings dominate
/// the leaf distribution instead of being one option among five, so a
/// filter indexing straight to `.a`/`.b` actually hits adversarial UTF-8
/// most of the time rather than rarely.
fn arb_json_stringy(depth: u32) -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        1 => Just(Value::Null),
        1 => any::<bool>().prop_map(Value::Bool),
        1 => (-1000i64..1000i64).prop_map(|n| serde_json::json!(n)),
        4 => arb_utf8_string().prop_map(Value::String),
    ];
    leaf.prop_recursive(depth, 40, 5, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
            prop::collection::btree_map(
                prop_oneof![
                    Just("a".to_string()),
                    Just("b".to_string()),
                    Just("c".to_string())
                ],
                inner,
                0..4
            )
            .prop_map(|m| Value::Object(m.into_iter().collect())),
        ]
    })
}

/// `Ok` on a successful `jq` run (parsed output lines), `Err(())` if `jq`
/// exited non-zero (a jq-level error, e.g. type mismatch).
fn try_run_real_jq(filter: &str, input: &str) -> Result<Vec<Value>, ()> {
    let mut child = Command::new("jq")
        .arg("-c")
        .arg(filter)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn jq (is it on PATH?)");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    if !out.status.success() {
        return Err(());
    }
    Ok(String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect())
}

macro_rules! fuzz_input_test {
    ($name:ident, $src:literal) => {
        fuzz_input_test!($name, $src, arb_json(3));
    };
    ($name:ident, $src:literal, $strategy:expr) => {
        proptest! {
            #![proptest_config(ProptestConfig::with_cases(50))]
            #[test]
            fn $name(input in $strategy) {
                let compiled = jqc::jq!($src);
                let input_str = input.to_string();
                let got = compiled(&input);
                let want = try_run_real_jq($src, &input_str);
                match (got, want) {
                    (Ok(g), Ok(w)) => prop_assert_eq!(g, w, "filter {:?} on input {}", $src, input_str),
                    (Err(_), Err(())) => {}
                    (g, w) => prop_assert!(
                        false,
                        "filter {:?} on input {}: compiled={:?} real-jq-ok={}",
                        $src, input_str, g, w.is_ok()
                    ),
                }
            }
        }
    };
}

fuzz_input_test!(fuzz_identity, ".");
fuzz_input_test!(fuzz_field_a, ".a");
fuzz_input_test!(fuzz_field_chain, ".a.b");
fuzz_input_test!(fuzz_iterate_items, ".items[]");
fuzz_input_test!(fuzz_select_active, "[.items[]? | select(.active)]");
fuzz_input_test!(fuzz_object_construct, "{x: .a, y: .b}");
fuzz_input_test!(fuzz_add, ".a + .b");
fuzz_input_test!(fuzz_eq, ".a == .b");
fuzz_input_test!(fuzz_sum_scores, "[.items[]? | .score?] | add");
fuzz_input_test!(fuzz_keys, "keys?");
fuzz_input_test!(fuzz_length, "length");
fuzz_input_test!(fuzz_type, "type");
fuzz_input_test!(fuzz_if_then_else, "if .a then .b else .c end");
fuzz_input_test!(fuzz_sort_by, "sort_by(.a)?");
fuzz_input_test!(fuzz_recurse, "[recurse]");

// Arbitrary-UTF-8-input-focused: every one of these indexes straight to
// `.a`/`.b` (or the whole input), which `arb_json_stringy` makes a string
// the large majority of the time -- unlike the filters above, which would
// mostly just exercise each builtin's type-error path under the original
// mixed-type generator.
fuzz_input_test!(fuzz_identity_stringy, ".", arb_json_stringy(2));
fuzz_input_test!(fuzz_tostring, ".a | tostring", arb_json_stringy(2));
fuzz_input_test!(fuzz_tonumber, ".a | tonumber?", arb_json_stringy(2));
fuzz_input_test!(fuzz_explode, ".a | explode?", arb_json_stringy(2));
fuzz_input_test!(
    fuzz_ascii_downcase,
    ".a | ascii_downcase?",
    arb_json_stringy(2)
);
fuzz_input_test!(fuzz_ascii_upcase, ".a | ascii_upcase?", arb_json_stringy(2));
fuzz_input_test!(fuzz_split, "(.a | split(.b))?", arb_json_stringy(2));
fuzz_input_test!(fuzz_join, "(.a | join(.b))?", arb_json_stringy(2));
fuzz_input_test!(
    fuzz_startswith,
    "(.a | startswith(.b))?",
    arb_json_stringy(2)
);
fuzz_input_test!(fuzz_endswith, "(.a | endswith(.b))?", arb_json_stringy(2));
fuzz_input_test!(
    fuzz_contains_str,
    "(.a | contains(.b))?",
    arb_json_stringy(2)
);
fuzz_input_test!(fuzz_ltrimstr, "(.a | ltrimstr(.b))?", arb_json_stringy(2));
fuzz_input_test!(fuzz_rtrimstr, "(.a | rtrimstr(.b))?", arb_json_stringy(2));
