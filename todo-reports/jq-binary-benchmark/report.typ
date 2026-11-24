#import "../template.typ": report, diagram

#show: report.with(title: "Benchmarking the real jq binary", date: "2026-11-24")

= Context

`jqc/benches/compiled_filters.rs` already had one illustrative benchmark
point for shelling out to real `jq`, explicitly scoped as "illustrative
only, not a fair comparison." `todos.md` asked to actually benchmark the
real binary for comparison, so this extends that single point into a
proper series alongside every existing `jqc` benchmark group.

= What changed

Each of the five size-swept benchmark groups (`identity`, `field_access`,
`map_select_pipeline`, `sort_by`, `reduce_sum`) and the depth-swept
`recurse_nested` group now has a `jq` series next to its `jqc` series,
via a shared `run_jq(filter, input)` helper that pipes JSON through a
freshly spawned `jq -c <filter>` subprocess and parses its output back:

#diagram(
  boxes: (("input: Value", 0, 0), ("jq -c <filter>\n(subprocess)", 1, 0), ("stdout: Value", 2, 0)),
  arrows: ((0, 1), (1, 2)),
)

The real-`jq` series is capped at the two smaller sizes
(`JQ_COMPARISON_SIZES = [10, 1_000]`, `JQ_COMPARISON_DEPTHS = [10, 100]`
for `recurse_nested`) rather than every size `jqc` is measured at:
Criterion samples each point ~100 times by default, and at 100_000
elements that means round-tripping a large array through a text pipe to
a fresh process 100 times just to re-confirm the same conclusion (spawn
overhead dominates) the smaller sizes already show.

The old single-point `bench_subprocess_jq` is removed; it's now
subsumed by `identity`'s own `jq` series at `n = 10`.

= A real bug this surfaced

Running the new `recurse_nested/jq/1_000` point panicked:
`serde_json`'s default `Deserializer` has a recursion limit (128) well
short of the 1,000-level-deep nested object `make_nested(1_000)` builds
for that benchmark. That's a limitation of round-tripping through
`serde_json::Value` for the *benchmark's own measurement*, not of `jq` or
`jqc` -- `jqc`'s own 1,000-deep handling in the same group is unaffected
and keeps running. Depth 1,000 was dropped from `JQ_COMPARISON_DEPTHS`
rather than worked around, since the comparison's conclusion doesn't
depend on reaching that depth.

= Sample results (this run)

#table(
  columns: 4,
  align: (left, right, right, right),
  [*benchmark*], [*n*], [*jqc (in-process)*], [*jq (subprocess)*],
  [identity], [10], [867 ns], [1.84 ms],
  [identity], [1,000], [210 µs], [3.19 ms],
  [sort\_by(.score)], [10], [--], [1.82 ms],
  [sort\_by(.score)], [1,000], [374 µs], [4.12 ms],
  [sort\_by(.score)], [100,000], [82.8 ms], [--],
)

The gap narrows sharply as `n` grows (identity: ~2,100x at `n=10`, ~15x at
`n=1,000`) because the subprocess's fixed per-call cost (spawn, pipe
setup, text parse) amortizes over more elements, while `jqc`'s cost
scales with the work itself -- exactly the point the original illustrative
benchmark was making, now with numbers across sizes and filters instead
of one.

= Verification

`cargo build/test/clippy/fmt --workspace` pass; `cargo bench -p jqc --
--test` (Criterion's single-iteration dry run) exercises every
`jqc`/`jq` point across all six groups without panicking.
