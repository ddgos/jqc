//! Benchmarks the *generated* code -- i.e. what `jq!` expands to -- rather
//! than the macro-expansion step itself (that only runs once, at compile
//! time, and isn't part of the shipped binary's runtime cost).
//!
//! Run with `cargo bench -p jqc`.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use jqc::core::{json, Value};
use jqc::jq;
use std::hint::black_box;

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

/// Spawns `jq -c <filter>`, writes `payload` to its stdin, and returns its
/// stdout. Writes on a separate thread rather than blocking on
/// `write_all` before reading any output: for a payload larger than the
/// OS pipe buffer (64KB-ish on Linux), writing it all up front can
/// deadlock once jq's own stdout pipe also fills up with unread output
/// and both sides end up blocked waiting on each other.
fn jq_pipe(filter: &str, payload: String) -> Vec<u8> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut child = Command::new("jq")
        .arg("-c")
        .arg(filter)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn jq (is it on PATH?)");
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || {
        stdin.write_all(payload.as_bytes()).unwrap();
    });
    let out = child.wait_with_output().expect("jq exited");
    writer.join().expect("stdin writer thread panicked");
    out.stdout
}

/// Runs `filter` against `input` via the real `jq` binary on `PATH` (the
/// Nix dev shell provides it). This is deliberately *not* a fair "jq is
/// slow" comparison: it's dominated by spawning a process and piping JSON
/// through it as text, not by jq's own evaluation time -- which is the
/// point. `jq!` bakes the filter into the binary at compile time, so a
/// running program pays none of that per call the way shelling out to
/// real `jq` would.
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
