#import "../template.typ": report, diagram

#show: report.with(title: "Pareto-front performance tracking", date: "2026-11-24")

= Context

`todos.md` asked to keep a record of the Pareto front to see how
performance improves. The tasks surrounding it in the list (multi-core
and SIMD codegen, "always keep as separate implementations of the trait
so that comparisons are easier to make") make the intent clear: this is
groundwork for comparing *multiple* `Codegen` implementations against
each other once they exist, not just tracking one number over time.

= What was added

`benchmarks/pareto-front.csv` -- an append-only log, one row per
(commit, codegen implementation, benchmark, param) combination:

```
date,commit,codegen,benchmark,param,metric,value
```

`benchmarks/README.md` documents the schema and the convention: append a
row whenever a `Codegen` implementation is added or a change is
specifically meant to affect performance, not on every commit (which
would make the log noise instead of signal).

= Why a Pareto front, not a single number

#diagram(
  boxes: (("tree_walk", 0, 0), ("simd (future)", 1, 0), ("multi_core (future)", 2, 0)),
  arrows: (),
)

A single "performance" number hides the actual tradeoff that motivates
having separate `Codegen` implementations at all: a SIMD backend might
win on wall time for large inputs while costing more peak RAM; a
multi-core one might trade memory and binary size for throughput on
multi-core machines. For a given `benchmark`/`param`, the
implementations *not* dominated by another on both `wall_ns` and
`peak_rss_kb` (lower is better on both axes) are the front -- the ones
worth keeping around, as opposed to one that's simply worse on every
axis than another and could be dropped.

With only `tree_walk` implemented so far, the front is trivially that
one point. The log exists now so there's a clean baseline to compare
against once a second implementation lands, rather than needing to
reconstruct "what tree_walk used to do" retroactively.

= Seeded baseline (this commit)

30 rows: 6 `compile_time`/`peak_rss` fixtures (from
`jqc-macros/src/bench.rs`'s `FIXTURES`) and 6 runtime groups $times$ 3
sizes/depths each (from `jqc/benches/compiled_filters.rs`), all for
`tree_walk`, all measured in isolation (not alongside other benchmarks --
see the memory-safeguards and peak-RAM reports for why that matters for
the RSS figures specifically). A sample of the runtime rows:

#table(
  columns: 3,
  align: (left, right, right),
  [*benchmark/param*], [*wall time*], [],
  [identity/10], [878 ns], [],
  [identity/1000], [210 µs], [],
  [identity/100000], [53.6 ms], [],
  [sort\_by/10], [2.51 µs], [],
  [sort\_by/1000], [374 µs], [],
  [sort\_by/100000], [91.6 ms], [],
)

= Verification

`cargo build/test/clippy/fmt --workspace` pass (no code changed here,
only the log and its documentation); every seeded number was measured
directly from `cargo bench -p jqc` and `cargo test -p jqc-macros
--release -- --ignored --exact bench::compile_time_benchmarks
--nocapture` on this commit.
