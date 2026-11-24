#import "../template.typ": report, diagram

#show: report.with(title: "Un-ignoring the scaling-check regression tests", date: "2026-11-24")

= Context

After the `def_scope` memory fix (see the "def-scope-fix" report), the
natural follow-up question: now that `unused_defs_scaling_check` and
`scaling_check` are fast and bounded instead of dangerous, can they run
as normal tests instead of needing `--ignored` and manual care?

= A second, unrelated bug found while checking

Before un-ignoring anything, each test was run in `cargo test`'s default
*debug* profile (the profile `cargo test --workspace` actually uses,
unlike the `--release` the doc comments recommended) to check it was
safe there too. `scaling_check` was fine. `unused_defs_scaling_check`
stack-overflowed and aborted at `n=100`:

```
thread 'bench::unused_defs_scaling_check' (79565) has overflowed its stack
fatal runtime error: stack overflow, aborting
```

This is unrelated to the `def_scope` cloning fix. `lower()` walks a `def`
chain by recursing once per def through `lower_funcdef`'s `rest` --
`def unused0: .; def unused1: .; ...` lowers to nested `FuncDef`s, each
one's `rest` containing the next, so lowering $n$ sequential defs
recurses to depth $n$ (plus a fixed ~18 for the prelude). Debug
(unoptimized) builds keep much more per-frame state live than
`--release` does, so that linear-depth recursion overflows the default
~8MB thread stack at a far shallower depth in debug builds than in
release ones -- `n=400` was already fine in `--release` (confirmed in the
peak-RAM report), but `n=100` overflowed in debug.

#diagram(
  boxes: (("def chain, depth n", 0, 0), ("lower() recurses\nonce per def", 1, 0), ("8MB stack\n(debug build)", 2, 0)),
  arrows: ((0, 1), (1, 2)),
)

= Fix

A `run_with_stack(stack_bytes, f)` helper spawns `f` on a thread with an
explicit, larger stack (64MB) and joins it. `unused_defs_scaling_check`'s
whole loop now runs inside one such thread -- one thread spawn for the
test, not one per size, so the measured per-size timings aren't skewed
by thread-creation overhead. Verified `n=400` now completes in both
debug and `--release` builds.

= Un-ignoring

Both tests previously only printed diagnostics (`eprintln!`) with no
pass/fail criterion beyond "didn't crash." Each loop iteration now
asserts its wall time stays under 5 seconds (current worst case:
~25ms for `unused_defs_scaling_check` at $n=400$ in a debug build, ~8ms
for `scaling_check` at depth 6) -- enormous headroom over normal timing,
but tight enough to fail loudly if the `def_scope` cost cliff (or
something like it) comes back. Both `#[ignore]` attributes are removed;
they now run as part of plain `cargo test --workspace`, confirmed stable
across several repeated runs (~0.05s total for both, debug build).

`compile_time_benchmarks` and `peak_memory_benchmarks` stay `#[ignore]`d
-- they're Criterion/diagnostic harnesses with no assertions at all, kept
out of the default suite because they're benchmarks, not correctness
checks, which `cap_memory()`'s safeguard was never gating on in the first
place.

= Verification

`cargo build/test/clippy/fmt --workspace` all pass; `cargo test -p
jqc-macros` (default debug profile, no flags) now runs and passes
`bench::scaling_check` and `bench::unused_defs_scaling_check` alongside
every other test, repeated 3x with no flakiness observed.
