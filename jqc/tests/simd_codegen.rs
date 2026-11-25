#![cfg(feature = "codegen-simd")]
//! Differential tests for `SimdCodegen` (`jq!(codegen = "simd", ...)`):
//! same ground-truth approach as `tests/differential.rs` (run both the
//! compiled closure and real `jq`, compare), specifically for
//! `ascii_downcase`/`ascii_upcase`/`contains` -- the only builtins
//! `SimdCodegen` implements differently from the default
//! `TreeWalkCodegen` (see `jqc-compiler/src/codegen.rs`'s and
//! `jqc-core/src/simd.rs`'s own doc comments) -- plus one case
//! confirming an unrelated filter compiles and runs identically under
//! either backend.

use jqc::core::Value;
use jqc_test_support::run_real_jq;

macro_rules! simd_difftest {
    ($name:ident, $src:literal, $input:literal) => {
        #[test]
        fn $name() {
            let compiled = jqc::jq!(codegen = "simd", $src);
            let input: Value = serde_json::from_str($input).unwrap();
            let got = compiled(&input).unwrap_or_else(|e| panic!("jqc errored: {e}"));
            let want = run_real_jq($src, $input);
            assert_eq!(got, want, "filter {:?} on input {}", $src, $input);
        }
    };
}

simd_difftest!(simd_ascii_downcase_basic, "ascii_downcase", r#""HeLLo""#);
simd_difftest!(
    simd_ascii_downcase_mixed_unicode,
    "ascii_downcase",
    r#""HeLLo Wörld 日本語 CAFÉ""#
);
simd_difftest!(simd_ascii_downcase_empty, "ascii_downcase", r#""""#);
simd_difftest!(
    simd_ascii_downcase_exactly_16_bytes,
    "ascii_downcase",
    r#""ABCDEFGHIJKLMNOP""#
);
simd_difftest!(
    simd_ascii_downcase_17_bytes,
    "ascii_downcase",
    r#""ABCDEFGHIJKLMNOPQ""#
);
simd_difftest!(simd_ascii_upcase_basic, "ascii_upcase", r#""HeLLo""#);
simd_difftest!(
    simd_ascii_upcase_mixed_unicode,
    "ascii_upcase",
    r#""HeLLo Wörld 日本語 café""#
);
simd_difftest!(simd_ascii_upcase_empty, "ascii_upcase", r#""""#);
simd_difftest!(
    simd_ascii_upcase_boundary_bytes,
    "ascii_upcase",
    r#""@AZ[`az{""#
);

simd_difftest!(simd_contains_found, "contains(\"ell\")", r#""Hello World""#);
simd_difftest!(
    simd_contains_not_found,
    "contains(\"xyz\")",
    r#""Hello World""#
);
simd_difftest!(
    simd_contains_empty_needle,
    "contains(\"\")",
    r#""anything""#
);
simd_difftest!(
    simd_contains_array,
    "contains([\"a\"])",
    r#"["a", "b", "c"]"#
);
simd_difftest!(
    simd_contains_kind_mismatch,
    "try contains(null) catch .",
    "true"
);

#[test]
fn unrelated_filter_same_under_either_backend() {
    let treewalk = jqc::jq!(".a + .b");
    let simd = jqc::jq!(codegen = "simd", ".a + .b");
    let input: Value = serde_json::from_str(r#"{"a": 1, "b": 2}"#).unwrap();
    assert_eq!(treewalk(&input).unwrap(), simd(&input).unwrap());
}

#[test]
fn non_string_input_errors_the_same_under_either_backend() {
    let treewalk = jqc::jq!("ascii_downcase");
    let simd = jqc::jq!(codegen = "simd", "ascii_downcase");
    let input: Value = serde_json::from_str("5").unwrap();
    assert!(treewalk(&input).is_err());
    assert!(simd(&input).is_err());
}
