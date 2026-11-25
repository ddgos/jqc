//! Shared differential-test helper: shell out to a real `jq` binary on
//! `$PATH` and parse its line-delimited `-c` output back into `Value`s.
//! Used identically by several test suites in this repo (`jqc`'s own
//! `tests/differential.rs`/`simd_codegen.rs`/`version_selection.rs`, and
//! `jqc-manual-tests`' generated suite) that each used to carry their own
//! copy of this exact function.

use serde_json::Value;
use std::io::Write;
use std::process::{Command, Stdio};

/// Runs `jq -c <filter>` on `input`, asserting it succeeds (panicking with
/// jq's own stderr text otherwise -- these suites only use filters/inputs
/// expected to succeed; a suite that needs to tolerate a runtime error on
/// either side, like `jqc-c-tests`' adversarial C-test-suite inputs,
/// defines its own lenient variant instead) and returns its output parsed
/// one `Value` per line.
pub fn run_real_jq(filter: &str, input: &str) -> Vec<Value> {
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
    assert!(
        out.status.success(),
        "real jq failed for filter {filter:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}
