//! Differential tests for `format_value`: byte-for-byte comparison against
//! real `jq -n <filter-string>` (needs `jq` on `$PATH`; the Nix dev shell
//! provides it), the same ground-truth approach `jqc`'s own differential
//! suite uses for filter *evaluation* -- here for output *formatting*
//! instead, since that's the part `jqc-cli` reimplements from scratch
//! rather than delegating to anything already tested elsewhere.

use jqc_cli::{format_value, Args};
use serde_json::Value;
use std::io::Write;
use std::process::{Command, Stdio};

/// Runs real `jq` with `flags` on a `. ` filter fed `input_json` on stdin,
/// returning its stdout with the trailing newline stripped (jq always
/// appends one; `format_value` deliberately doesn't, since a CLI run adds
/// the separator itself once per output).
fn run_real_jq(flags: &[&str], input_json: &str) -> String {
    let mut child = Command::new("jq")
        .args(flags)
        .arg(".")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn jq (is it on PATH?)");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input_json.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "real jq {flags:?} on {input_json:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut s = String::from_utf8(out.stdout).unwrap();
    assert_eq!(s.pop(), Some('\n'), "real jq's output always ends in \\n");
    s
}

fn check(flags: &[&str], args: &Args, color: bool, input_json: &str) {
    let want = run_real_jq(flags, input_json);
    let input: Value = serde_json::from_str(input_json).unwrap();
    let got = format_value(&input, args, color);
    assert_eq!(
        got, want,
        "mismatch for {input_json:?} with flags {flags:?}"
    );
}

const SAMPLES: &[&str] = &[
    "null",
    "true",
    "false",
    "0",
    "-17",
    "3.5",
    "1e20",
    "\"\"",
    "\"hello\"",
    "\"line1\\nline2\\ttab\\\"quote\\\\backslash\"",
    "\"unicode: \\u00e9\\u4e2d\\ud83d\\ude00\"",
    "[]",
    "{}",
    "[1,2,3]",
    "{\"b\":1,\"a\":2,\"c\":3}",
    "{\"x\":[1,{\"y\":2},3],\"z\":null}",
    "[[1,2],[3,[4,5]]]",
];

#[test]
fn default_pretty_matches_real_jq() {
    let args = Args::parse(Vec::<String>::new()).unwrap();
    for s in SAMPLES {
        check(&[], &args, false, s);
    }
}

#[test]
fn compact_matches_real_jq() {
    let args = Args::parse(["-c".to_string()]).unwrap();
    for s in SAMPLES {
        check(&["-c"], &args, false, s);
    }
}

#[test]
fn sort_keys_matches_real_jq() {
    let args = Args::parse(["-S".to_string()]).unwrap();
    for s in SAMPLES {
        check(&["-S"], &args, false, s);
    }
}

#[test]
fn compact_sort_keys_matches_real_jq() {
    let args = Args::parse(["-cS".to_string()]).unwrap();
    for s in SAMPLES {
        check(&["-c", "-S"], &args, false, s);
    }
}

#[test]
fn indent_4_matches_real_jq() {
    let args = Args::parse(["--indent".to_string(), "4".to_string()]).unwrap();
    for s in SAMPLES {
        check(&["--indent", "4"], &args, false, s);
    }
}

#[test]
fn indent_0_matches_real_jq() {
    let args = Args::parse(["--indent".to_string(), "0".to_string()]).unwrap();
    for s in SAMPLES {
        check(&["--indent", "0"], &args, false, s);
    }
}

#[test]
fn tab_matches_real_jq() {
    let args = Args::parse(["--tab".to_string()]).unwrap();
    for s in SAMPLES {
        check(&["--tab"], &args, false, s);
    }
}

#[test]
fn ascii_output_matches_real_jq() {
    let args = Args::parse(["-a".to_string()]).unwrap();
    for s in SAMPLES {
        check(&["-a"], &args, false, s);
    }
}

#[test]
fn color_pretty_matches_real_jq() {
    let args = Args::parse(["-C".to_string()]).unwrap();
    for s in SAMPLES {
        check(&["-C"], &args, true, s);
    }
}

#[test]
fn color_compact_matches_real_jq() {
    let args = Args::parse(["-Cc".to_string()]).unwrap();
    for s in SAMPLES {
        check(&["-C", "-c"], &args, true, s);
    }
}

#[test]
fn color_sorted_matches_real_jq() {
    let args = Args::parse(["-CS".to_string()]).unwrap();
    for s in SAMPLES {
        check(&["-C", "-S"], &args, true, s);
    }
}
