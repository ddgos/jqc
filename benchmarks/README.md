# Performance history

`pareto-front.csv` is an append-only log of benchmark results, tagged by
commit and `Codegen` implementation, so performance changes (and new
codegen backends -- SIMD, multi-core, anything else added as a separate
`Codegen` implementation per `todos.md`) can be compared against history
instead of only against whatever ran last.

## Schema

```
date,commit,codegen,benchmark,param,metric,value
```

- `date` -- `YYYY-MM-DD`, when the row was recorded.
- `commit` -- short git SHA the numbers came from.
- `codegen` -- which `Codegen` implementation produced the measured code
  (`tree_walk` today; `simd`, `multi_core`, etc. once they exist).
- `benchmark` -- `compile_time/<fixture>` or `peak_rss/<fixture>` (from
  `jqc-macros/src/bench.rs`'s `FIXTURES`), or `runtime/<group>` (from
  `jqc/benches/compiled_filters.rs`'s groups).
- `param` -- the size/depth swept over, blank for the fixed-size
  `compile_time`/`peak_rss` rows.
- `metric` -- `wall_ns` or `peak_rss_kb`.
- `value` -- the number.

## Appending a new row

There's no automated collector yet -- run the relevant benchmark
(`cargo bench -p jqc` for runtime, `cargo test -p jqc-macros --release --
--ignored <name> --nocapture` for compile-time/memory; see the main
README's Testing section) and append a row by hand with the commit you
measured at. Do this whenever a `Codegen` implementation is added or a
change is specifically meant to improve performance -- not on every
commit, which would make the log noise rather than signal.

## Reading it as a Pareto front

For a given `benchmark`/`param`, the implementations *not* dominated by
another on both `wall_ns` and `peak_rss_kb` (lower is better on both) form
the front. With one implementation (`tree_walk`) there's nothing to
compare yet; this becomes meaningful once a second one exists.
