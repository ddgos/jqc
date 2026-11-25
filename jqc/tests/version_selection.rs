//! `jq!(version = "X.Y", "<filter>")` targets a specific jq language
//! version instead of the newest available one. These tests cover the
//! positive paths (a selected version compiles and runs correctly).
//!
//! The negative paths -- `version = "1.6"` rejecting `if`/`then`/`end`
//! without `else` (added in jq 1.7), and naming a version whose Cargo
//! feature isn't enabled -- are compile-time errors from the macro
//! itself, verified by hand (see the jq-version-selection todo-report)
//! rather than here; this project has no compile-fail test harness
//! (trybuild or similar) for any of `jq!`'s other diagnostics either.
//!
//! Requires a `jq` binary on PATH (the Nix dev shell provides one).
//! `#[cfg(feature = "jq-1-6")]` tests only run with that feature enabled:
//! `cargo test -p jqc --features jq-1-6`.

use jqc::core::Value;
use jqc::jq;
use jqc_test_support::run_real_jq;

#[test]
fn explicit_newest_version_matches_default() {
    let src = "if .a then 1 end";
    let input = r#"{"a":false}"#;
    let with_version = jq!(version = "1.8", "if .a then 1 end");
    let without_version = jq!("if .a then 1 end");
    let parsed: Value = serde_json::from_str(input).unwrap();
    assert_eq!(
        with_version(&parsed).unwrap(),
        without_version(&parsed).unwrap(),
        "explicit version=\"1.8\" should behave identically to no version argument"
    );
    assert_eq!(with_version(&parsed).unwrap(), run_real_jq(src, input));
}

#[cfg(feature = "jq-1-6")]
#[test]
fn jq_1_6_accepts_if_with_explicit_else() {
    // jq 1.6 requires an explicit `else`; this filter has one, so it
    // compiles and runs the same under jq-1-6 as under the default.
    let src = "if .a then 1 else 2 end";
    let input = r#"{"a":false}"#;
    let filter = jq!(version = "1.6", "if .a then 1 else 2 end");
    let parsed: Value = serde_json::from_str(input).unwrap();
    assert_eq!(filter(&parsed).unwrap(), run_real_jq(src, input));
}
