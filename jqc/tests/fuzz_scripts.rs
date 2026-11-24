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
//! constructs this crate supports -- in principle spanning README.md's
//! entire "Supported jq subset" list, so that in theory any valid
//! construct has a chance of being generated, not just a handful of
//! arithmetic/control-flow shapes) and a random JSON input, build the
//! scratch crate with that filter baked into a `jq!` call, run it on the
//! input, and compare against the real `jq` binary. A build failure is
//! treated as a real bug (the grammar below is deliberately kept inside
//! the supported subset, so `jqc` should always be able to compile it) --
//! which also means every production here has to be genuinely valid jq
//! syntax, not just "probably fine": an accidental syntax error in the
//! grammar itself would misreport as a `jqc` bug.
//!
//! Variable/label scoping is handled by generating the *bound* position
//! (`as $x | BODY`, `reduce … as $x (INIT; BODY)`, `label $out | BODY`,
//! `def f: BODY; REST`) and its *body* together, via a dedicated strategy
//! that may splice in a reference to that binding (`$x`, `break $out`,
//! `f`) -- a bare, always-available `$x`/`f` leaf in the general grammar
//! would just as easily end up somewhere *outside* its own binding's
//! scope, which is a real (and, from the "every production must be valid
//! syntax" rule above, fuzzer-breaking) undefined-variable compile error,
//! not a `jqc` bug.

use jqc::core::Value;
use proptest::prelude::*;
use proptest::strategy::ValueTree;
use proptest::test_runner::{Config, TestRunner};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Encodes `s` as a jq string-literal body (between the quotes jqc's own
/// lexer accepts: `\n\t\r\"\\` plus `\u00XX` for anything else below
/// 0x20/0x7F, matching `lex_string` in `jqc-compiler/src/lexer.rs`
/// exactly, not just "some escaping that's probably fine" -- raw non-
/// ASCII passes through unescaped, which the lexer also accepts
/// directly). This is the jq*-source*-text escaper; the *Rust* string
/// literal this whole filter ends up embedded in (via `{filter:?}` in
/// `Scratch::write_filter` below) is a separate, independent escaping
/// layer that `{:?}`'s own `Debug` impl already handles correctly
/// regardless of what jq-level escapes this produces.
fn jq_string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\u{:04x}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn arb_utf8_string() -> impl Strategy<Value = String> {
    prop::collection::vec(any::<char>(), 0..6).prop_map(|cs| cs.into_iter().collect())
}

fn arb_string_literal() -> impl Strategy<Value = String> {
    arb_utf8_string().prop_map(|s| jq_string_literal(&s))
}

/// The base grammar: no free `$x`/`break $out`/`f` reference anywhere, so
/// every value it produces is valid on its own, in any context.
fn arb_filter() -> impl Strategy<Value = String> {
    let leaf = prop_oneof![
        Just(".".to_string()),
        Just(".a".to_string()),
        Just(".b".to_string()),
        Just(".c".to_string()),
        Just("true".to_string()),
        Just("false".to_string()),
        Just("null".to_string()),
        Just("empty".to_string()),
        // Parenthesized even when non-negative, so this is always a safe
        // leaf to splice in verbatim regardless of what's immediately to
        // its left (`try -4 catch .`, unparenthesized, is a real parse
        // error: found generating it this way).
        (-5i64..5).prop_map(|n| format!("({n})")),
        (-50i64..50).prop_map(|n| format!("({})", n as f64 / 10.0)),
        arb_string_literal(),
    ];
    leaf.prop_recursive(3, 60, 4, |inner| {
        prop_oneof![
            // Arithmetic and comparison.
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} + {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} - {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} * {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} / {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} % {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} == {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} != {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} < {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} <= {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} and {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} or {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} // {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} | {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a}, {b})")),
            (inner.clone(), inner.clone(), inner.clone())
                .prop_map(|(c, t, e)| format!("(if {c} then {t} else {e} end)")),
            // Construction.
            inner.clone().prop_map(|e| format!("[{e}]")),
            inner.clone().prop_map(|e| format!("{{a: {e}}}")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("{{a: {a}, b: {b}}}")),
            Just("{a}".to_string()),
            Just("{a, b}".to_string()),
            inner
                .clone()
                .prop_map(|e| format!("{{(({e} | tostring)): 1}}")),
            // Indexing/slicing -- kept to a `.`-chain directly (rather
            // than a generic postfix on any parenthesized expression) to
            // stay inside the syntax forms README.md actually documents.
            Just(".[0]".to_string()),
            Just(".[-1]".to_string()),
            Just(".[0:2]".to_string()),
            Just(".[1:]".to_string()),
            Just(".[:2]".to_string()),
            Just("[.[]?]".to_string()),
            // Error handling.
            inner.clone().prop_map(|e| format!("({e})?")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("(try {a} catch {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} | in({b}))?")),
            // Scope-safe variable/label/def bindings: the bound position
            // and its body are generated together (see module doc).
            inner.clone().prop_flat_map(|src| {
                arb_body_maybe_var(3).prop_map(move |body| format!("({src} as $x | {body})"))
            }),
            (inner.clone(), inner.clone()).prop_flat_map(|(src, init)| {
                arb_body_maybe_var(3)
                    .prop_map(move |update| format!("(reduce {src} as $x ({init}; {update}))"))
            }),
            (inner.clone(), inner.clone()).prop_flat_map(|(src, init)| {
                arb_body_maybe_var(3)
                    .prop_map(move |update| format!("[foreach {src} as $x ({init}; {update})]"))
            }),
            arb_body_maybe_break(3).prop_map(|body| format!("(label $out | {body})")),
            arb_body_maybe_call_f(3)
                .prop_map(|(def_body, rest)| format!("(def f: {def_body}; {rest})")),
            // 0-arg builtins (each `?`-guarded, since `.`'s actual shape
            // under an arbitrary nested filter is anyone's guess, and a
            // type mismatch is exactly as valid a thing to fuzz as a
            // match).
            inner.clone().prop_map(|e| format!("({e} | length?)")),
            inner.clone().prop_map(|e| format!("({e} | keys?)")),
            inner
                .clone()
                .prop_map(|e| format!("({e} | keys_unsorted?)")),
            inner.clone().prop_map(|e| format!("({e} | type)")),
            inner.clone().prop_map(|e| format!("({e} | not)")),
            inner.clone().prop_map(|e| format!("({e} | values)")),
            inner.clone().prop_map(|e| format!("({e} | add?)")),
            inner.clone().prop_map(|e| format!("({e} | to_entries?)")),
            inner.clone().prop_map(|e| format!("({e} | from_entries?)")),
            inner.clone().prop_map(|e| format!("({e} | sort?)")),
            inner.clone().prop_map(|e| format!("({e} | unique?)")),
            inner.clone().prop_map(|e| format!("({e} | min?)")),
            inner.clone().prop_map(|e| format!("({e} | max?)")),
            inner.clone().prop_map(|e| format!("({e} | reverse?)")),
            inner.clone().prop_map(|e| format!("({e} | floor?)")),
            inner.clone().prop_map(|e| format!("({e} | ceil?)")),
            inner.clone().prop_map(|e| format!("({e} | sqrt?)")),
            inner.clone().prop_map(|e| format!("({e} | fabs?)")),
            inner.clone().prop_map(|e| format!("({e} | tostring)")),
            inner.clone().prop_map(|e| format!("({e} | tonumber?)")),
            inner.clone().prop_map(|e| format!("({e} | explode?)")),
            inner.clone().prop_map(|e| format!("({e} | implode?)")),
            inner
                .clone()
                .prop_map(|e| format!("({e} | ascii_downcase?)")),
            inner.clone().prop_map(|e| format!("({e} | ascii_upcase?)")),
            inner.clone().prop_map(|e| format!("({e} | recurse)")),
            inner.clone().prop_map(|e| format!("[{e} | .[]?]")),
            // 1-filter-argument builtins.
            (inner.clone(), inner.clone()).prop_map(|(e, f)| format!("[{e} | map({f})?]")),
            (inner.clone(), inner.clone()).prop_map(|(e, f)| format!("[{e} | select({f})?]")),
            (inner.clone(), inner.clone()).prop_map(|(e, f)| format!("({e} | any({f})?)")),
            (inner.clone(), inner.clone()).prop_map(|(e, f)| format!("({e} | all({f})?)")),
            (inner.clone(), inner.clone()).prop_map(|(e, f)| format!("({e} | sort_by({f})?)")),
            (inner.clone(), inner.clone()).prop_map(|(e, f)| format!("({e} | group_by({f})?)")),
            (inner.clone(), inner.clone()).prop_map(|(e, f)| format!("({e} | unique_by({f})?)")),
            (inner.clone(), inner.clone()).prop_map(|(e, f)| format!("({e} | min_by({f})?)")),
            (inner.clone(), inner.clone()).prop_map(|(e, f)| format!("({e} | max_by({f})?)")),
            (inner.clone(), inner.clone()).prop_map(|(e, f)| format!("({e} | with_entries({f})?)")),
            (inner.clone(), inner.clone()).prop_map(|(e, f)| format!("({e} | first({f}))")),
            (inner.clone(), inner.clone()).prop_map(|(e, f)| format!("({e} | last({f}))")),
            // `repeat(f)`/`while(c;u)`/`until(c;u)`/`recurse(f)` are
            // deliberately never generated, bounded or not: all four
            // desugar (jqc-compiler/src/lower.rs's prelude) to a 0-arg
            // *recursive* `def`, and `limit(n; exp)` desugars to a
            // `foreach exp as $x (...)` whose `exp` -- jqc being eager,
            // not lazy -- must fully materialize as a `Vec<Value>`
            // *before* `foreach`'s own loop (and so `limit`'s own
            // `break`) ever runs at all. `limit(3; while(true; .))`
            // doesn't bound anything: the `while` has to finish first,
            // and for an arbitrary generated condition that may never
            // itself go false, it simply never does. Bare 0-arg
            // `recurse` is fine (bounded by the actual JSON value's own,
            // always-finite, structure -- no user-suppliable condition
            // involved), and is already generated above.
            // String builtins taking a string-literal argument.
            arb_string_literal().prop_map(|s| format!("(. | startswith({s})?)")),
            arb_string_literal().prop_map(|s| format!("(. | endswith({s})?)")),
            arb_string_literal().prop_map(|s| format!("(. | contains({s})?)")),
            arb_string_literal().prop_map(|s| format!("(. | ltrimstr({s})?)")),
            arb_string_literal().prop_map(|s| format!("(. | rtrimstr({s})?)")),
            arb_string_literal().prop_map(|s| format!("(. | split({s})?)")),
            arb_string_literal().prop_map(|s| format!("(. | join({s})?)")),
            // `range`/`has`/`limit`, args kept small to keep output sizes
            // sane.
            Just("[range(3)]".to_string()),
            Just("[range(0;5)]".to_string()),
            Just("[range(0;5;2)]".to_string()),
            inner.clone().prop_map(|e| format!("({e} | has(\"a\")?)")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("[limit(2; {a}, {b})]")),
        ]
    })
}

/// As [`arb_filter`]'s leaves, plus a `$x` reference -- used only for a
/// position that's actually inside an enclosing `as $x`/`reduce …  as
/// $x`/`foreach … as $x` binding.
fn arb_body_maybe_var(depth: u32) -> impl Strategy<Value = String> {
    prop_oneof![3 => arb_filter(), 1 => Just("$x".to_string())].prop_recursive(
        depth,
        30,
        3,
        |inner| {
            prop_oneof![
                (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} + {b})")),
                (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} | {b})")),
                (inner.clone(), inner.clone(), inner.clone())
                    .prop_map(|(c, t, e)| format!("(if {c} then {t} else {e} end)")),
            ]
        },
    )
}

/// As [`arb_body_maybe_var`], but for `label $out | BODY`: may splice in
/// `break $out` instead of a bare variable reference.
fn arb_body_maybe_break(depth: u32) -> impl Strategy<Value = String> {
    prop_oneof![3 => arb_filter(), 1 => Just("break $out".to_string())].prop_recursive(
        depth,
        30,
        3,
        |inner| {
            prop_oneof![
                (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a}, {b})")),
                (inner.clone(), inner.clone(), inner.clone())
                    .prop_map(|(c, t, e)| format!("(if {c} then {t} else {e} end)")),
            ]
        },
    )
}

/// `def f: DEF_BODY; REST` -- `DEF_BODY` never calls `f` (0-arg defs can
/// be self-recursive, but keeping this one non-recursive avoids ever
/// generating a filter that just hangs), `REST` may call it.
fn arb_body_maybe_call_f(depth: u32) -> impl Strategy<Value = (String, String)> {
    let def_body = arb_filter();
    let rest = prop_oneof![3 => arb_filter(), 1 => Just("f".to_string())].prop_recursive(
        depth,
        30,
        3,
        |inner| {
            prop_oneof![
                (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} + {b})")),
                (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} | {b})")),
            ]
        },
    );
    (def_body, rest)
}

fn arb_input() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        (-10i64..10).prop_map(|n| serde_json::json!(n)),
        Just(serde_json::json!({"a": 1, "b": 2, "c": 3})),
        Just(serde_json::json!({"a": [1, 2], "b": "x", "c": null})),
        Just(serde_json::json!([1, 2, 3])),
        Just(serde_json::json!("hello")),
        Just(serde_json::json!({"a": "café", "b": "", "c": "a\nb\tc"})),
    ]
}

struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let dir = PathBuf::from(
            std::env::var("CLAUDE_JOB_DIR")
                .unwrap_or_else(|_| std::env::temp_dir().display().to_string()),
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

fn run_real_jq(filter: &str, input: &str) -> Result<Vec<Value>, ()> {
    let mut child = Command::new("jq")
        .arg("-c")
        .arg(filter)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn jq");
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

#[test]
#[ignore = "slow: rebuilds a scratch crate per case, run explicitly with --ignored"]
fn fuzz_arbitrary_scripts() {
    let scratch = Scratch::new();
    let mut runner = TestRunner::new(Config {
        cases: 30,
        ..Config::default()
    });
    let strategy = (arb_filter(), arb_input());

    let mut failures = Vec::new();
    for i in 0..30 {
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

    assert!(
        failures.is_empty(),
        "fuzz failures:\n{}",
        failures.join("\n")
    );
}
