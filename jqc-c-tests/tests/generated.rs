//! Differential tests generated from jq's own C test suite
//! (`tests/jq.test`). See `jqc-c-tests-macros` for how, and
//! `jqc-c-tests`'s `Cargo.toml` description for why this isn't in
//! `[workspace]` default-members. Distinct from `jqc-manual-tests`, which
//! generates from the manual's documentation examples instead.

use jqc::core::{JResult, Value};
use std::io::Write;
use std::process::{Command, Stdio};

/// Real `jq`'s own JSON parser (and number formatter) is more permissive
/// than `serde_json`'s -- a leading byte-order mark is valid input to jq,
/// and a sufficiently extreme numeric literal (`9E999999999`, …) prints
/// back out in a form `serde_json` can't reparse as a `Value` either.
/// `Untestable` covers both: this harness genuinely can't verify the case
/// (not a jqc bug to report), so the caller treats it the same as "jq
/// errored" -- an honest "can't compare" rather than a false mismatch.
enum RealJqResult {
    Ok(Vec<Value>),
    Err(String),
    Untestable,
}

/// Runs real `jq` on `input`, tolerating a non-zero exit (jq's C suite,
/// unlike the manual's curated examples, deliberately includes cases
/// that error at *runtime* on a given input) -- the error variant carries
/// jq's own stderr text, used only for a panic message, never compared
/// against.
fn run_real_jq_lenient(filter: &str, input: &str) -> RealJqResult {
    let mut child = Command::new("jq")
        .arg("-c")
        .arg(filter)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn jq (is it on PATH?)");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let mut values = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str(line) {
            Ok(v) => values.push(v),
            Err(_) => return RealJqResult::Untestable,
        }
    }
    if out.status.success() {
        RealJqResult::Ok(values)
    } else {
        RealJqResult::Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

/// Checks one generated case: run `filter` (the `jq!`-compiled closure
/// jqc-c-tests-macros spliced in) and real `jq`, both on `input_json`.
/// Since jq's C suite deliberately includes cases that error at runtime
/// (not just the `%%FAIL` cases, which never get this far), both sides
/// erroring is accepted as consistent without comparing exact error
/// text -- only a *disagreement* about whether it errors, or a mismatched
/// successful result, fails the test.
pub fn check_case(
    program: &str,
    filter: impl Fn(&Value) -> JResult<Vec<Value>>,
    input_json: &str,
) {
    // Real jq's own JSON parser accepts (and ignores) a leading
    // byte-order mark; `serde_json::from_str` doesn't. jq.test has an
    // explicit test for exactly this, which otherwise fails here before
    // the two sides are even compared -- strip it the same way jq does,
    // rather than let that be a false harness failure.
    let input_json = input_json.strip_prefix('\u{feff}').unwrap_or(input_json);
    // Real jq's own input parser also accepts non-standard-JSON number
    // tokens (`nan`, `Infinity`, …) that `serde_json` rejects outright --
    // same "can't verify this one" situation as `RealJqResult::Untestable`
    // above, just on the input side instead of jq's output.
    let input: Value = match serde_json::from_str(input_json) {
        Ok(v) => v,
        Err(_) => return,
    };
    let got = filter(&input);
    let want = run_real_jq_lenient(program, input_json);
    match (got, want) {
        (_, RealJqResult::Untestable) => {}
        (Ok(got), RealJqResult::Ok(want)) => {
            assert_eq!(got, want, "c-test {program:?} on input {input_json}");
        }
        (Err(_), RealJqResult::Err(_)) => {}
        (Ok(got), RealJqResult::Err(jq_err)) => panic!(
            "c-test {program:?} on input {input_json}: jqc succeeded with {got:?}, \
             but real jq errored: {jq_err}"
        ),
        (Err(e), RealJqResult::Ok(want)) => panic!(
            "c-test {program:?} on input {input_json}: jqc errored ({e}), \
             but real jq succeeded with {want:?}"
        ),
    }
}

jqc_c_tests_macros::generate_c_tests!(version = "1.8");
