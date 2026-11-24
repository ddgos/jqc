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

/// Physical RAM in bytes, via `sysconf`; `None` if the syscall fails.
fn physical_memory_bytes() -> Option<u64> {
    let pages = unsafe { libc::sysconf(libc::_SC_PHYS_PAGES) };
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGE_SIZE) };
    (pages > 0 && page_size > 0).then(|| pages as u64 * page_size as u64)
}

/// Caps this process's address space so a runaway allocation in one of
/// these tests fails fast (an `abort` from the allocator) instead of being
/// left to the kernel's OOM reaper, which can take an unbounded amount of
/// time and is liable to kill unrelated processes on the machine rather
/// than just this test binary.
///
/// Nominally 64GiB, but clamped to half of physical RAM: on a machine with
/// less RAM than that (this one has 38GiB, not 64+), a flat 64GiB limit
/// would let a single test balloon to the point of swapping the whole
/// machine to a crawl -- or worse, triggering the OOM killer on unrelated
/// processes -- long before the rlimit itself is ever hit. Override with
/// `JQC_TEST_MEM_LIMIT_GB` to force a specific value regardless.
fn cap_memory() {
    const NOMINAL_GB: u64 = 64;
    let default_gb = physical_memory_bytes()
        .map(|phys| (phys / 2 / (1024 * 1024 * 1024)).clamp(1, NOMINAL_GB))
        .unwrap_or(NOMINAL_GB);
    let gb: u64 = std::env::var("JQC_TEST_MEM_LIMIT_GB")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default_gb);
    let limit_bytes = gb * 1024 * 1024 * 1024;
    let limit = libc::rlimit {
        rlim_cur: limit_bytes,
        rlim_max: limit_bytes,
    };
    let rc = unsafe { libc::setrlimit(libc::RLIMIT_AS, &limit) };
    assert_eq!(
        rc,
        0,
        "setrlimit(RLIMIT_AS, {limit_bytes}) failed: {}",
        std::io::Error::last_os_error()
    );
}

fn compile(src: &str) {
    let ast = lower::parse_and_lower(src).expect("fixture filter should compile");
    codegen::compile_top(&ast).expect("fixture filter should compile");
}

/// Shared across the time- and memory-focused benchmarks below so both
/// measure the exact same filters.
const FIXTURES: &[(&str, &str)] = &[
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

#[test]
#[ignore = "run explicitly: cargo test -p jqc-macros --release -- --ignored compile_time_benchmarks --nocapture"]
fn compile_time_benchmarks() {
    cap_memory();
    let mut c = Criterion::default();

    for (name, src) in FIXTURES {
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

/// Isolates one cost in `lower_funcdef`: `def_scope: scope.to_vec()` clones
/// the whole (recursively-nested) scope at every `def`, whether or not that
/// def is ever called. N unused defs should be cheap in a real compiler
/// (jq's own compile time is ~4ms flat up to 200 unused defs -- see the
/// investigation this is based on); this checks whether jqc's is.
///
/// Was `#[ignore]`d: before `def_scope` became an `Rc` (see `lower.rs`),
/// this took 38GB+ of RSS and had to be killed by hand well before n=400.
/// Now bounded and asserted on, it runs as a normal regression test
/// guarding against that cost cliff coming back.
#[test]
fn unused_defs_scaling_check() {
    cap_memory();
    run_with_stack(64 * 1024 * 1024, || {
        for n in [50, 100, 200, 400] {
            let mut src = String::new();
            for i in 0..n {
                src.push_str(&format!("def unused{i}: .; "));
            }
            src.push('.');
            reset_peak_rss();
            let start = std::time::Instant::now();
            compile(&src);
            let elapsed = start.elapsed();
            eprintln!("n={n} elapsed={elapsed:?} peak_rss={}", fmt_peak_rss());
            assert!(
                elapsed < std::time::Duration::from_secs(5),
                "n={n} unused defs took {elapsed:?} -- def_scope cost-cliff regression?"
            );
        }
    });
}

/// Runs `f` on a fresh thread with `stack_bytes` of stack, propagating any
/// panic. `lower()` walks a `def` chain's `rest` once per def recursively,
/// and in debug/unoptimized builds (where frames aren't shrunk the way
/// `--release` shrinks them) a few hundred sequential defs can overflow
/// the default ~8MB thread stack well short of where it would under
/// `--release` -- confirmed by `unused_defs_scaling_check` aborting with
/// a stack overflow at n=100 in a debug build before this was added.
fn run_with_stack(stack_bytes: usize, f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(stack_bytes)
        .spawn(f)
        .expect("spawn thread")
        .join()
        .expect("thread panicked");
}

/// Peak resident set size (kB) this process has reached so far, via Linux's
/// `VmHWM` ("high water mark") in `/proc/self/status`. `None` off Linux.
#[cfg(target_os = "linux")]
fn peak_rss_kb() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").expect("read /proc/self/status");
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .and_then(|rest| rest.trim().trim_end_matches(" kB").trim().parse().ok())
}
#[cfg(not(target_os = "linux"))]
fn peak_rss_kb() -> Option<u64> {
    None
}

/// Resets `VmHWM`/`VmPeak` down to the process's *current* usage, so a
/// subsequent [`peak_rss_kb`] reports only what happened in between, not
/// everything since process start. See `proc(5)` on `clear_refs`. No-op off
/// Linux, where `peak_rss_kb` always returns `None` anyway.
#[cfg(target_os = "linux")]
fn reset_peak_rss() {
    std::fs::write("/proc/self/clear_refs", "5").expect("write /proc/self/clear_refs");
}
#[cfg(not(target_os = "linux"))]
fn reset_peak_rss() {}

fn fmt_peak_rss() -> String {
    match peak_rss_kb() {
        Some(kb) => format!("{kb}KB"),
        None => "N/A (non-Linux)".to_string(),
    }
}

/// Peak RAM used by a single macro-expansion of each fixture filter -- i.e.
/// what `jq!` costs *your build* in memory, not what the generated code
/// costs at runtime. Only meaningful run one fixture at a time (not under
/// Criterion's repeated-iteration sampling), since `reset_peak_rss` would
/// otherwise just measure the high-water mark across many iterations.
///
/// Also run this test *alone*, not alongside `scaling_check` or
/// `unused_defs_scaling_check` (e.g. via `--ignored` with no name filter,
/// which runs every ignored test): `VmHWM` is a per-*process* counter, and
/// the default test harness runs multiple test functions concurrently as
/// threads in one process, so a sibling test's allocations inflate this
/// one's reading. Invoke with `--exact bench::peak_memory_benchmarks` (or
/// any single matching name) to avoid that.
#[test]
#[ignore = "run explicitly: cargo test -p jqc-macros --release -- --ignored peak_memory_benchmarks --nocapture"]
fn peak_memory_benchmarks() {
    cap_memory();
    for (name, src) in FIXTURES {
        reset_peak_rss();
        compile(src);
        eprintln!("{name}: peak_rss={}", fmt_peak_rss());
    }
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
/// minutes and over 30GB of RSS when killed. Capped at 6 here (~30s total,
/// pre-fix) -- do not raise this without raising `cap_memory`'s limit too,
/// it is not a typo. (Depth 6 now completes in single-digit milliseconds
/// post-fix; the cap stays as a backstop, not because depth 6 is still
/// expensive.)
///
/// Was `#[ignore]`d: the `def_scope` fix that sped this up (see `lower.rs`)
/// also removed the reason it needed manual, careful invocation. Now
/// bounded and asserted on, it runs as a normal regression test.
#[test]
fn scaling_check() {
    cap_memory();
    for depth in 1..=6 {
        let mut src = String::from("def f0: . + 1;\n");
        for i in 1..=depth {
            let prev = i - 1;
            src.push_str(&format!("def f{i}: f{prev} | f{prev};\n"));
        }
        src.push_str(&format!("f{depth}"));
        reset_peak_rss();
        let start = std::time::Instant::now();
        compile(&src);
        let elapsed = start.elapsed();
        eprintln!(
            "depth={depth} elapsed={elapsed:?} peak_rss={}",
            fmt_peak_rss()
        );
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "depth={depth} took {elapsed:?} -- def_scope cost-cliff regression?"
        );
    }
}
