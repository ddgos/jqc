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

fn arb_json(depth: u32) -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        (-1000i64..1000i64).prop_map(|n| serde_json::json!(n)),
        "[a-z]{0,6}".prop_map(Value::String),
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
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
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
        proptest! {
            #![proptest_config(ProptestConfig::with_cases(50))]
            #[test]
            fn $name(input in arb_json(3)) {
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
