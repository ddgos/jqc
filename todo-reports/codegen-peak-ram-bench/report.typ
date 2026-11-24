#import "../template.typ": report, diagram

#show: report.with(title: "Peak RAM benchmarks for the codegen process", date: "2026-11-24")

= Context

`todos.md` asked for benchmarks of the peak RAM usage of the codegen
process -- the lex → parse → lower → codegen pipeline that `jq!` runs at
*macro-expansion time*, i.e. what it costs your build, not the runtime
cost of the generated closure (`jqc/benches/compiled_filters.rs` already
covers that). `jqc-macros/src/bench.rs` already had wall-clock benchmarks
for this pipeline; this adds the memory side.

= How peak RSS is measured

On Linux, `/proc/self/status`'s `VmHWM` field is a kernel-maintained
high-water mark of this process's resident set size. Writing `"5"` to
`/proc/self/clear_refs` resets that high-water mark down to the process's
*current* RSS (documented in `proc(5)`), so bracketing one `compile()` call
with a reset-then-read gives (approximately) the peak memory that one
call added, not everything the process has used since start:

#diagram(
  boxes: (("reset_peak_rss", 0, 0), ("compile(src)", 1, 0), ("peak_rss_kb()", 2, 0)),
  arrows: ((0, 1), (1, 2)),
)

Off Linux, both functions are stubbed (`peak_rss_kb` returns `None`,
`reset_peak_rss` is a no-op) so the benchmarks still run, just without a
number to report.

= What was added

- `peak_memory_benchmarks` -- a new `#[ignore]`d test reusing the same
  `FIXTURES` table as `compile_time_benchmarks` (now a shared `const` so
  both measure identical filters), reporting peak RSS per fixture.
- `unused_defs_scaling_check` and `scaling_check` (the two cost-cliff
  regression tests from the earlier `def_scope` memory-explosion fix) now
  report peak RSS alongside their existing wall-clock timing, since they
  already sweep increasing sizes and the memory curve is informative for
  free.
- All of the above still run under `cap_memory()`'s `RLIMIT_AS` guard from
  the memory-safeguards task, so a future regression here fails fast
  rather than repeating the earlier incident.

= Results (this run, post def_scope fix)

Each test below was run *alone* (`cargo test -p jqc-macros --release --
--ignored --exact bench::<name> --nocapture`), not alongside the other
`#[ignore]`d memory tests. `VmHWM` is a per-*process* counter, and Rust's
default test harness runs multiple test functions concurrently as
threads within one process -- running several of these benchmarks
together inflates each one's reading with sibling threads' allocations.
(An earlier draft of this report mistakenly used numbers from exactly
that kind of combined run; the figures below replace them.)

#table(
  columns: 2,
  align: (left, right),
  [*fixture*], [*peak RSS*],
  [trivial], [5,372 KB],
  [field\_chain], [5,400 KB],
  [pipeline], [5,428 KB],
  [prelude\_heavy], [5,444 KB],
  [defs\_non\_recursive], [5,532 KB],
  [recursive\_def], [5,532 KB],
)

#table(
  columns: 3,
  align: (left, right, right),
  [*unused defs (n)*], [*peak RSS*], [],
  [50], [7,212 KB], [],
  [100], [9,916 KB], [],
  [200], [17,756 KB], [],
  [400], [43,896 KB], [],
)

#table(
  columns: 2,
  align: (left, right),
  [*recursive-doubling depth*], [*peak RSS*],
  [1], [5,440 KB],
  [2], [5,492 KB],
  [3], [5,552 KB],
  [4], [5,596 KB],
  [5], [5,880 KB],
  [6], [6,480 KB],
)

All comfortably within normal process overhead -- a useful baseline now
that the exponential `def_scope` blowup is fixed; future codegen work
(the trait-based multi-codegen and SIMD/multicore tasks later in
`todos.md`) can compare against these numbers rather than the pre-fix
state.

= Verification

`cargo build/test/clippy/fmt --workspace` all pass; each test above was
run in isolation and produced the tables above.
