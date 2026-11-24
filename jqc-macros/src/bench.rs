//! Macro-expansion-time cost of the lex -> parse -> lower -> codegen
//! pipeline itself -- i.e. what `jq!` adds to *your build*, not the
//! runtime cost of the code it generates (see `jqc`'s
//! `benches/compiled_filters.rs` for that).
//!
//! This crate is a proc-macro crate, which may only export its
//! `#[proc_macro]` items to other crates -- a normal external `benches/`
//! target couldn't call `lower`/`codegen` at all. Driving Criterion from a
//! `#[test]` instead keeps this a same-crate unit test, which has no such
//! restriction.
//!
//! Run with:
//! `cargo test -p jqc-macros --release -- --ignored compile_time_benchmarks --nocapture`

use criterion::{BenchmarkId, Criterion};

use crate::{codegen, lower};

fn compile(src: &str) {
    let ast = lower::parse_and_lower(src).expect("fixture filter should compile");
    codegen::compile_top(&ast).expect("fixture filter should compile");
}

#[test]
#[ignore = "run explicitly: cargo test -p jqc-macros --release -- --ignored compile_time_benchmarks --nocapture"]
fn compile_time_benchmarks() {
    let mut c = Criterion::default();

    let fixtures: &[(&str, &str)] = &[
        ("trivial", "."),
        ("field_chain", ".a.b.c.d.e.f.g.h"),
        (
            "pipeline",
            ".items[] | select(.active) | {name: .name, score: (.score * 2)}",
        ),
        (
            "prelude_heavy",
            "[recurse] | sort_by(.) | group_by(.) | unique | map(length)",
        ),
        (
            "defs_non_recursive",
            "def a: . + 1; def b: a | a; def c: b | b; c",
        ),
        (
            "recursive_def",
            "def fact: if . <= 1 then 1 else (. - 1 | fact) * . end; fact",
        ),
    ];
    for (name, src) in fixtures {
        c.bench_function(name, |b| b.iter(|| compile(src)));
    }

    // Every compile pays a fixed cost parsing/lowering the ~18-def prelude
    // regardless of the user's filter; this sweep instead shows how cost
    // scales with the *user* filter's own size, via a chain of pipes.
    let mut group = c.benchmark_group("chained_pipe_length");
    for &n in &[1usize, 10, 50, 200] {
        let src = vec!["select(. != null)"; n].join(" | ");
        group.bench_with_input(BenchmarkId::from_parameter(n), &src, |b, src| {
            b.iter(|| compile(src));
        });
    }
    group.finish();
}

/// Characterizes a real, severe cost cliff: a chain of non-recursive defs
/// where each level's body uses the previous level twice (`def f{i}: f{i-1}
/// | f{i-1};`) blows up *worse* than the 2^depth you'd expect from plain
/// non-memoized call-by-name duplication alone -- `lower_call`'s
/// `def_scope: Vec<DefEntry>` snapshot is cloned (recursively, since each
/// entry's own `def_scope` nests all the earlier ones) at every level, so
/// the per-level cost grows too. Measured wall-clock time roughly doubled
/// per level up to depth 5, then the ratio itself started climbing
/// (2.7x, then 4.6x at depth 7); depth 8 was still running after 2.5
/// minutes and over 30GB of RSS when killed. Capped at 6 here (~30s total)
/// -- do not raise this without a memory cap, it is not a typo.
/// Isolates the *other* cost in `lower_funcdef`: `def_scope: scope.to_vec()`
/// clones the whole (recursively-nested) scope at every `def`, whether or
/// not that def is ever called. N unused defs should be cheap in a real
/// compiler (jq's own compile time is ~4ms flat up to 200 unused defs --
/// see the investigation this is based on); this checks whether jqc's is.
#[test]
#[ignore = "run explicitly: cargo test -p jqc-macros --release -- --ignored unused_defs_scaling_check --nocapture"]
fn unused_defs_scaling_check() {
    for n in [50, 100, 200, 400] {
        let mut src = String::new();
        for i in 0..n {
            src.push_str(&format!("def unused{i}: .; "));
        }
        src.push('.');
        let start = std::time::Instant::now();
        compile(&src);
        eprintln!("n={n} elapsed={:?}", start.elapsed());
    }
}

#[test]
#[ignore = "run explicitly: cargo test -p jqc-macros --release -- --ignored scaling_check --nocapture"]
fn scaling_check() {
    for depth in 1..=6 {
        let mut src = String::from("def f0: . + 1;\n");
        for i in 1..=depth {
            let prev = i - 1;
            src.push_str(&format!("def f{i}: f{prev} | f{prev};\n"));
        }
        src.push_str(&format!("f{depth}"));
        let start = std::time::Instant::now();
        compile(&src);
        eprintln!("depth={depth} elapsed={:?}", start.elapsed());
    }
}
