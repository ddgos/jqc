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

fn bench_identity(c: &mut Criterion) {
    let filter = jq!(".");
    let mut group = c.benchmark_group("identity");
    for &n in SIZES {
        let input = make_items(n);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &input, |b, input| {
            b.iter(|| filter(black_box(input)).unwrap());
        });
    }
    group.finish();
}

fn bench_field_access(c: &mut Criterion) {
    let filter = jq!(".[0].name");
    let mut group = c.benchmark_group("field_access");
    for &n in SIZES {
        let input = make_items(n);
        group.bench_with_input(BenchmarkId::from_parameter(n), &input, |b, input| {
            b.iter(|| filter(black_box(input)).unwrap());
        });
    }
    group.finish();
}

fn bench_map_select_pipeline(c: &mut Criterion) {
    let filter = jq!("[.[] | select(.active) | .score]");
    let mut group = c.benchmark_group("map_select_pipeline");
    for &n in SIZES {
        let input = make_items(n);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &input, |b, input| {
            b.iter(|| filter(black_box(input)).unwrap());
        });
    }
    group.finish();
}

fn bench_sort_by(c: &mut Criterion) {
    let filter = jq!("sort_by(.score)");
    let mut group = c.benchmark_group("sort_by");
    for &n in SIZES {
        let input = make_items(n);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &input, |b, input| {
            b.iter(|| filter(black_box(input)).unwrap());
        });
    }
    group.finish();
}

fn bench_reduce_sum(c: &mut Criterion) {
    let filter = jq!("reduce .[] as $x (0; . + $x.score)");
    let mut group = c.benchmark_group("reduce_sum");
    for &n in SIZES {
        let input = make_items(n);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &input, |b, input| {
            b.iter(|| filter(black_box(input)).unwrap());
        });
    }
    group.finish();
}

fn bench_recurse(c: &mut Criterion) {
    let filter = jq!("[recurse]");
    let mut group = c.benchmark_group("recurse_nested");
    for &depth in &[10usize, 100, 1_000] {
        let input = make_nested(depth);
        group.bench_with_input(BenchmarkId::from_parameter(depth), &input, |b, input| {
            b.iter(|| filter(black_box(input)).unwrap());
        });
    }
    group.finish();
}

/// Illustrative only: cost of shelling out to the real `jq` binary per
/// call, for contrast with the in-process compiled closure above. This is
/// dominated by process-spawn overhead, not jq's own evaluation time, so
/// it's not a fair "jq is slow" comparison -- it's the point: `jq!`
/// eliminates the process-spawn-and-reparse step entirely by baking the
/// filter into the binary at compile time.
fn bench_subprocess_jq(c: &mut Criterion) {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let input = make_items(10);
    let input_str = input.to_string();
    c.bench_function("subprocess_jq_small_input", |b| {
        b.iter(|| {
            let mut child = Command::new("jq")
                .arg("-c")
                .arg("[.[] | select(.active) | .score]")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn jq");
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input_str.as_bytes())
                .unwrap();
            black_box(child.wait_with_output().unwrap());
        });
    });
}

criterion_group!(
    benches,
    bench_identity,
    bench_field_access,
    bench_map_select_pipeline,
    bench_sort_by,
    bench_reduce_sum,
    bench_recurse,
    bench_subprocess_jq,
);
criterion_main!(benches);
