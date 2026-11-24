//! Benchmarks the *generated* code -- i.e. what `jq!` expands to -- rather
//! than the macro-expansion step itself (that only runs once, at compile
//! time, and isn't part of the shipped binary's runtime cost).
//!
//! Run with `cargo bench -p jqc`.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use jqc::core::{json, Value};
use jqc::jq;
use std::collections::HashMap;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

fn make_items(n: usize) -> Value {
    let items: Vec<Value> = (0..n)
        .map(|i| {
            json!({
                "id": i,
                "name": format!("item-{i}"),
                "active": i % 3 != 0,
                "score": (i % 100) as f64 * 1.5,
            })
        })
        .collect();
    Value::Array(items)
}

fn make_nested(depth: usize) -> Value {
    let mut v = json!({"leaf": true});
    for i in 0..depth {
        v = json!({"a": v, "level": i});
    }
    v
}

const SIZES: &[usize] = &[10, 1_000, 100_000];

/// Real `jq` is only benchmarked at these (smaller) sizes: each sample
/// round-trips the input through a text pipe to a fresh subprocess, so
/// sampling it at 100_000 elements the same number of times Criterion
/// samples the in-process closure would make `cargo bench` take far longer
/// for a comparison point whose conclusion (process-spawn overhead
/// dominates) doesn't need the largest input to demonstrate.
const JQ_COMPARISON_SIZES: &[usize] = &[10, 1_000];

/// Spawns `cmd`, writes `payload` to its stdin, and returns its stdout.
/// Writes on a separate thread rather than blocking on `write_all` before
/// reading any output: for a payload larger than the OS pipe buffer
/// (64KB-ish on Linux), writing it all up front can deadlock once the
/// child's own stdout pipe also fills up with unread output and both
/// sides end up blocked waiting on each other.
fn spawn_pipe(cmd: &mut std::process::Command, payload: String) -> Vec<u8> {
    use std::io::Write;
    use std::process::Stdio;

    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn {cmd:?}: {e}"));
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || {
        stdin.write_all(payload.as_bytes()).unwrap();
    });
    let out = child.wait_with_output().expect("child exited");
    writer.join().expect("stdin writer thread panicked");
    out.stdout
}

fn jq_pipe(filter: &str, payload: String) -> Vec<u8> {
    use std::process::Command;
    spawn_pipe(Command::new("jq").arg("-c").arg(filter), payload)
}

/// Runs `filter` against `input` via the real `jq` binary on `PATH` (the
/// Nix dev shell provides it). This is deliberately *not* a fair "jq is
/// slow" comparison against the in-process `jqc` series above: it's
/// dominated by spawning a process and piping JSON through it as text,
/// not by jq's own evaluation time -- which is the point, since `jq!`
/// bakes the filter into the binary at compile time, so a running
/// program pays none of that per call the way shelling out to real `jq`
/// would. For the comparison that *does* hold both sides to the same
/// packaging (process spawn + text stdin/stdout), see `jqc_cold`/
/// `jqc_warm` below, via `run_jqc_subprocess`/`run_jqc_subprocess_warm`.
fn run_jq(filter: &str, input: &Value) -> Vec<Value> {
    let stdout = jq_pipe(filter, input.to_string());
    serde_json::Deserializer::from_slice(&stdout)
        .into_iter::<Value>()
        .collect::<Result<_, _>>()
        .expect("jq output should be valid JSON")
}

/// Each `jq_warm` benchmark point below batches this many evaluations
/// into one `jq` process. Deliberately *not* driven by Criterion's own
/// iteration count (via `iter_custom`): Criterion calibrates that count
/// upward until a sample takes long enough to measure reliably, and for
/// an operation this fast that meant trying to build and parse a
/// multi-gigabyte stdin/stdout payload in one go before this was fixed to
/// a constant.
const WARM_REPS: u64 = 50;

/// Complements `run_jq` above: spawns `jq` *once* and pipes `WARM_REPS`
/// copies of `input` into it back to back (jq already treats a stream of
/// concatenated top-level JSON values on stdin as that many separate
/// inputs, emitting one output per input), instead of spawning a fresh
/// process per evaluation. Process-spawn/startup cost is paid once and
/// amortized over `WARM_REPS` evaluations rather than once per
/// evaluation, isolating jq's own per-input evaluation cost from the
/// cost of repeatedly invoking it -- real usage falls somewhere between
/// this "warm" number and `run_jq`'s "cold" one, depending on whether a
/// program shells out to `jq` once per request or once per batch. The
/// per-evaluation time is this call's measured wall time divided by
/// `WARM_REPS` (left to the caller/reader, the same way the `SIZES`
/// benchmarks above report per-call time that Criterion already divides
/// across elements via `Throughput`).
fn run_jq_warm(filter: &str, input: &Value) -> Vec<Value> {
    let mut payload = String::new();
    for _ in 0..WARM_REPS {
        payload.push_str(&input.to_string());
        payload.push('\n');
    }
    let stdout = jq_pipe(filter, payload);
    let outputs: Vec<Value> = serde_json::Deserializer::from_slice(&stdout)
        .into_iter::<Value>()
        .collect::<Result<_, _>>()
        .expect("jq output should be valid JSON");
    assert_eq!(
        outputs.len() as u64,
        WARM_REPS,
        "expected {WARM_REPS} outputs from a warm jq batch, got {}",
        outputs.len()
    );
    outputs
}

/// Compiles `filter` via `jqc-build` into a standalone binary (cached per
/// process, since several benchmark points below reuse the same filter
/// string across input sizes) and returns its path.
///
/// This is the fair counterpart to `run_jq`/`run_jq_warm`: `jqc`'s own
/// `BenchmarkId::new("jqc", ...)` series above measures the in-process
/// closure, paying none of real `jq`'s process-spawn/text-pipe cost --
/// which is a deliberate, real difference (see `run_jq`'s doc comment),
/// but not one `jq_cold`/`jq_warm` can be fairly *compared against* on
/// their own terms. Running a `jqc`-compiled binary (via `jqc-build`,
/// added so a jq script can become a standalone binary at all) through
/// the exact same `spawn_pipe` plumbing isolates jq-the-language's own
/// evaluation cost from jqc-vs-jq's in-process-vs-subprocess packaging
/// difference: `jqc_cold`/`jqc_warm` vs `jq_cold`/`jq_warm` below compare
/// like with like (both pay process spawn + text stdin/stdout), while
/// `jqc` vs `jqc_cold`/`jqc_warm` shows what that packaging difference
/// alone costs for the *same* generated code.
fn jqc_binary_for(filter: &str) -> PathBuf {
    static CACHE: Mutex<Option<HashMap<String, PathBuf>>> = Mutex::new(None);
    let mut guard = CACHE.lock().unwrap();
    let cache = guard.get_or_insert_with(HashMap::new);
    if let Some(path) = cache.get(filter) {
        return path.clone();
    }

    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("jqc's manifest dir has a parent")
        .to_path_buf();
    let status = std::process::Command::new("cargo")
        .args(["build", "--release", "-p", "jqc-build"])
        .current_dir(&workspace)
        .status()
        .expect("spawn cargo to build jqc-build");
    assert!(status.success(), "building jqc-build failed");

    let id = cache.len();
    let script_path = std::env::temp_dir().join(format!("jqc-bench-{id}.jq"));
    std::fs::write(&script_path, filter).expect("write scratch script");
    let output_path = std::env::temp_dir().join(format!("jqc-bench-bin-{id}"));
    let status = std::process::Command::new(workspace.join("target/release/jqc-build"))
        .arg(&script_path)
        .arg("-o")
        .arg(&output_path)
        .status()
        .expect("spawn jqc-build");
    assert!(status.success(), "jqc-build failed for filter {filter:?}");

    cache.insert(filter.to_string(), output_path.clone());
    output_path
}

fn jqc_subprocess_pipe(filter: &str, payload: String) -> Vec<u8> {
    spawn_pipe(
        &mut std::process::Command::new(jqc_binary_for(filter)),
        payload,
    )
}

/// `jqc`-compiled-binary counterpart to `run_jq`: one process per call.
fn run_jqc_subprocess(filter: &str, input: &Value) -> Vec<Value> {
    let stdout = jqc_subprocess_pipe(filter, input.to_string());
    serde_json::Deserializer::from_slice(&stdout)
        .into_iter::<Value>()
        .collect::<Result<_, _>>()
        .expect("jqc-compiled binary output should be valid JSON")
}

/// `jqc`-compiled-binary counterpart to `run_jq_warm`: one process for
/// `WARM_REPS` calls, newline-delimited like `run_jq_warm`.
fn run_jqc_subprocess_warm(filter: &str, input: &Value) -> Vec<Value> {
    let mut payload = String::new();
    for _ in 0..WARM_REPS {
        payload.push_str(&input.to_string());
        payload.push('\n');
    }
    let stdout = jqc_subprocess_pipe(filter, payload);
    let outputs: Vec<Value> = serde_json::Deserializer::from_slice(&stdout)
        .into_iter::<Value>()
        .collect::<Result<_, _>>()
        .expect("jqc-compiled binary output should be valid JSON");
    assert_eq!(
        outputs.len() as u64,
        WARM_REPS,
        "expected {WARM_REPS} outputs from a warm jqc-compiled-binary batch, got {}",
        outputs.len()
    );
    outputs
}

fn bench_identity(c: &mut Criterion) {
    let filter = jq!(".");
    let mut group = c.benchmark_group("identity");
    for &n in SIZES {
        let input = make_items(n);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::new("jqc", n), &input, |b, input| {
            b.iter(|| filter(black_box(input)).unwrap());
        });
        if JQ_COMPARISON_SIZES.contains(&n) {
            group.bench_with_input(BenchmarkId::new("jq_cold", n), &input, |b, input| {
                b.iter(|| run_jq(".", black_box(input)));
            });
            group.bench_with_input(BenchmarkId::new("jq_warm", n), &input, |b, input| {
                b.iter(|| run_jq_warm(".", black_box(input)));
            });
            group.bench_with_input(BenchmarkId::new("jqc_cold", n), &input, |b, input| {
                b.iter(|| run_jqc_subprocess(".", black_box(input)));
            });
            group.bench_with_input(BenchmarkId::new("jqc_warm", n), &input, |b, input| {
                b.iter(|| run_jqc_subprocess_warm(".", black_box(input)));
            });
        }
    }
    group.finish();
}

fn bench_field_access(c: &mut Criterion) {
    let filter = jq!(".[0].name");
    let mut group = c.benchmark_group("field_access");
    for &n in SIZES {
        let input = make_items(n);
        group.bench_with_input(BenchmarkId::new("jqc", n), &input, |b, input| {
            b.iter(|| filter(black_box(input)).unwrap());
        });
        if JQ_COMPARISON_SIZES.contains(&n) {
            group.bench_with_input(BenchmarkId::new("jq_cold", n), &input, |b, input| {
                b.iter(|| run_jq(".[0].name", black_box(input)));
            });
            group.bench_with_input(BenchmarkId::new("jq_warm", n), &input, |b, input| {
                b.iter(|| run_jq_warm(".[0].name", black_box(input)));
            });
            group.bench_with_input(BenchmarkId::new("jqc_cold", n), &input, |b, input| {
                b.iter(|| run_jqc_subprocess(".[0].name", black_box(input)));
            });
            group.bench_with_input(BenchmarkId::new("jqc_warm", n), &input, |b, input| {
                b.iter(|| run_jqc_subprocess_warm(".[0].name", black_box(input)));
            });
        }
    }
    group.finish();
}

fn bench_map_select_pipeline(c: &mut Criterion) {
    let filter = jq!("[.[] | select(.active) | .score]");
    let mut group = c.benchmark_group("map_select_pipeline");
    for &n in SIZES {
        let input = make_items(n);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::new("jqc", n), &input, |b, input| {
            b.iter(|| filter(black_box(input)).unwrap());
        });
        if JQ_COMPARISON_SIZES.contains(&n) {
            group.bench_with_input(BenchmarkId::new("jq_cold", n), &input, |b, input| {
                b.iter(|| run_jq("[.[] | select(.active) | .score]", black_box(input)));
            });
            group.bench_with_input(BenchmarkId::new("jq_warm", n), &input, |b, input| {
                b.iter(|| run_jq_warm("[.[] | select(.active) | .score]", black_box(input)));
            });
            group.bench_with_input(BenchmarkId::new("jqc_cold", n), &input, |b, input| {
                b.iter(|| run_jqc_subprocess("[.[] | select(.active) | .score]", black_box(input)));
            });
            group.bench_with_input(BenchmarkId::new("jqc_warm", n), &input, |b, input| {
                b.iter(|| {
                    run_jqc_subprocess_warm("[.[] | select(.active) | .score]", black_box(input))
                });
            });
        }
    }
    group.finish();
}

fn bench_sort_by(c: &mut Criterion) {
    let filter = jq!("sort_by(.score)");
    let mut group = c.benchmark_group("sort_by");
    for &n in SIZES {
        let input = make_items(n);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::new("jqc", n), &input, |b, input| {
            b.iter(|| filter(black_box(input)).unwrap());
        });
        if JQ_COMPARISON_SIZES.contains(&n) {
            group.bench_with_input(BenchmarkId::new("jq_cold", n), &input, |b, input| {
                b.iter(|| run_jq("sort_by(.score)", black_box(input)));
            });
            group.bench_with_input(BenchmarkId::new("jq_warm", n), &input, |b, input| {
                b.iter(|| run_jq_warm("sort_by(.score)", black_box(input)));
            });
            group.bench_with_input(BenchmarkId::new("jqc_cold", n), &input, |b, input| {
                b.iter(|| run_jqc_subprocess("sort_by(.score)", black_box(input)));
            });
            group.bench_with_input(BenchmarkId::new("jqc_warm", n), &input, |b, input| {
                b.iter(|| run_jqc_subprocess_warm("sort_by(.score)", black_box(input)));
            });
        }
    }
    group.finish();
}

fn bench_reduce_sum(c: &mut Criterion) {
    let filter = jq!("reduce .[] as $x (0; . + $x.score)");
    let mut group = c.benchmark_group("reduce_sum");
    for &n in SIZES {
        let input = make_items(n);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::new("jqc", n), &input, |b, input| {
            b.iter(|| filter(black_box(input)).unwrap());
        });
        if JQ_COMPARISON_SIZES.contains(&n) {
            group.bench_with_input(BenchmarkId::new("jq_cold", n), &input, |b, input| {
                b.iter(|| run_jq("reduce .[] as $x (0; . + $x.score)", black_box(input)));
            });
            group.bench_with_input(BenchmarkId::new("jq_warm", n), &input, |b, input| {
                b.iter(|| run_jq_warm("reduce .[] as $x (0; . + $x.score)", black_box(input)));
            });
            group.bench_with_input(BenchmarkId::new("jqc_cold", n), &input, |b, input| {
                b.iter(|| {
                    run_jqc_subprocess("reduce .[] as $x (0; . + $x.score)", black_box(input))
                });
            });
            group.bench_with_input(BenchmarkId::new("jqc_warm", n), &input, |b, input| {
                b.iter(|| {
                    run_jqc_subprocess_warm("reduce .[] as $x (0; . + $x.score)", black_box(input))
                });
            });
        }
    }
    group.finish();
}

/// Depth 1_000 is excluded from the real-`jq` comparison below: `run_jq`
/// parses jq's text output back into a `serde_json::Value` with the
/// default (non-arbitrary-depth) deserializer, which has its own
/// recursion limit (128) well short of 1_000 levels of nested objects.
/// That's a limitation of benchmarking-via-text-roundtrip, not of either
/// `jq` or `jqc`'s own 1_000-deep handling.
const JQ_COMPARISON_DEPTHS: &[usize] = &[10, 100];

fn bench_recurse(c: &mut Criterion) {
    let filter = jq!("[recurse]");
    let mut group = c.benchmark_group("recurse_nested");
    for &depth in &[10usize, 100, 1_000] {
        let input = make_nested(depth);
        group.bench_with_input(BenchmarkId::new("jqc", depth), &input, |b, input| {
            b.iter(|| filter(black_box(input)).unwrap());
        });
        if JQ_COMPARISON_DEPTHS.contains(&depth) {
            group.bench_with_input(BenchmarkId::new("jq_cold", depth), &input, |b, input| {
                b.iter(|| run_jq("[recurse]", black_box(input)));
            });
            group.bench_with_input(BenchmarkId::new("jq_warm", depth), &input, |b, input| {
                b.iter(|| run_jq_warm("[recurse]", black_box(input)));
            });
            group.bench_with_input(BenchmarkId::new("jqc_cold", depth), &input, |b, input| {
                b.iter(|| run_jqc_subprocess("[recurse]", black_box(input)));
            });
            group.bench_with_input(BenchmarkId::new("jqc_warm", depth), &input, |b, input| {
                b.iter(|| run_jqc_subprocess_warm("[recurse]", black_box(input)));
            });
        }
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_identity,
    bench_field_access,
    bench_map_select_pipeline,
    bench_sort_by,
    bench_reduce_sum,
    bench_recurse,
);
criterion_main!(benches);
