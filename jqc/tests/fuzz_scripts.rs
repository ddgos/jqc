//! Fuzzes arbitrary *scripts*, not just inputs.
//!
//! `jq!` takes a string literal, so a script can't be handed to it at test
//! runtime -- there's no `eval` for Rust. This test works around that by
//! writing the generated script into a scratch crate's source and invoking
//! `cargo build` for each case, which is slow (roughly a second or so per
//! case even with a warm dependency cache), so it's `#[ignore]`d and capped
//! at a small case count. Run it explicitly with:
//!
//! ```text
//! cargo test -p jqc --test fuzz_scripts -- --ignored --nocapture
//! ```
//!
//! Each case: generate a random filter (from a grammar restricted to
//! constructs this crate supports) and a random JSON input, build the
//! scratch crate with that filter baked into a `jq!` call, run it on the
//! input, and compare against the real `jq` binary. A build failure is
//! treated as a real bug (the grammar below is deliberately kept inside
//! the supported subset, so `jqc` should always be able to compile it).

use jqc::core::Value;
use proptest::prelude::*;
use proptest::strategy::ValueTree;
use proptest::test_runner::{Config, TestRunner};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn arb_filter() -> impl Strategy<Value = String> {
    let leaf = prop_oneof![
        Just(".".to_string()),
        Just(".a".to_string()),
        Just(".b".to_string()),
        Just(".c".to_string()),
        (0i64..5).prop_map(|n| n.to_string()),
    ];
    leaf.prop_recursive(3, 30, 4, |inner| {
        prop_oneof![
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} + {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} - {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} == {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} | {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a}, {b})")),
            (inner.clone(), inner.clone(), inner.clone())
                .prop_map(|(c, t, e)| format!("(if {c} then {t} else {e} end)")),
            inner.clone().prop_map(|e| format!("[{e}]")),
            inner.clone().prop_map(|e| format!("({e})?")),
        ]
    })
}

fn arb_input() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        (-10i64..10).prop_map(|n| serde_json::json!(n)),
        Just(serde_json::json!({"a": 1, "b": 2, "c": 3})),
        Just(serde_json::json!({"a": [1, 2], "b": "x", "c": null})),
    ]
}

struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let dir = PathBuf::from(
            std::env::var("CLAUDE_JOB_DIR").unwrap_or_else(|_| std::env::temp_dir().display().to_string()),
        )
        .join("tmp/jqc-fuzz-scratch");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let jqc_path = Path::new(env!("CARGO_MANIFEST_DIR"));
        std::fs::write(
            dir.join("Cargo.toml"),
            format!(
                r#"[package]
name = "jqc-fuzz-scratch"
version = "0.0.0"
edition = "2021"
publish = false

[dependencies]
jqc = {{ path = {jqc_path:?} }}
serde_json = "1"
"#
            ),
        )
        .unwrap();
        Scratch { dir }
    }

    fn write_filter(&self, filter: &str) {
        let src = format!(
            r#"use jqc::jq;
fn main() {{
    let filter = jq!({filter:?});
    let mut input = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut input).unwrap();
    let v: jqc::core::Value = serde_json::from_str(&input).unwrap();
    match filter(&v) {{
        Ok(outs) => for o in outs {{ println!("{{o}}"); }},
        Err(_) => std::process::exit(5),
    }}
}}
"#
        );
        std::fs::write(self.dir.join("src/main.rs"), src).unwrap();
    }

    /// `Ok(None)` = build failed; `Ok(Some(bin_path))` = built fine.
    fn build(&self) -> Option<PathBuf> {
        let status = Command::new("cargo")
            .arg("build")
            .arg("--quiet")
            .current_dir(&self.dir)
            .status()
            .expect("run cargo build");
        if !status.success() {
            return None;
        }
        Some(self.dir.join("target/debug/jqc-fuzz-scratch"))
    }
}

fn run_compiled(bin: &Path, input: &str) -> Result<Vec<Value>, ()> {
    let mut child = Command::new(bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
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

fn run_real_jq(filter: &str, input: &str) -> Result<Vec<Value>, ()> {
    let mut child = Command::new("jq")
        .arg("-c")
        .arg(filter)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn jq");
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

#[test]
#[ignore = "slow: rebuilds a scratch crate per case, run explicitly with --ignored"]
fn fuzz_arbitrary_scripts() {
    let scratch = Scratch::new();
    let mut runner = TestRunner::new(Config {
        cases: 10,
        ..Config::default()
    });
    let strategy = (arb_filter(), arb_input());

    let mut failures = Vec::new();
    for i in 0..10 {
        let tree = strategy.new_tree(&mut runner).unwrap();
        let (filter, input) = tree.current();
        let input_str = input.to_string();

        scratch.write_filter(&filter);
        let Some(bin) = scratch.build() else {
            failures.push(format!("case {i}: BUILD FAILED for filter {filter:?}"));
            continue;
        };

        let got = run_compiled(&bin, &input_str);
        let want = run_real_jq(&filter, &input_str);
        match (&got, &want) {
            (Ok(g), Ok(w)) if g == w => {}
            (Err(_), Err(_)) => {}
            _ => failures.push(format!(
                "case {i}: MISMATCH filter={filter:?} input={input_str} compiled={got:?} real_jq_ok={}",
                want.is_ok()
            )),
        }
    }

    assert!(failures.is_empty(), "fuzz failures:\n{}", failures.join("\n"));
}
