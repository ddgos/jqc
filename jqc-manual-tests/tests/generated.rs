//! Differential tests generated from jq's own manual examples. See
//! `jqc-manual-tests-macros` for how, and `jqc-manual-tests`'s
//! `Cargo.toml` description for why this isn't in `[workspace]
//! default-members`.

use jqc::core::Value;
use std::io::Write;
use std::process::{Command, Stdio};

fn run_real_jq(filter: &str, input: &str) -> Vec<Value> {
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
        "real jq failed for manual example {filter:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

jqc_manual_tests_macros::generate_manual_tests!(version = "1.8");
