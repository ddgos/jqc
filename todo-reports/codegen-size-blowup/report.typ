#import "../template.typ": report, diagram

#show: report.with(title: "The real root cause: codegen inlining a loop/merge shape per node, not per filter", date: "2026-11-24")

= Context

The `real-world-script-debugging` report tracked down a real script
ballooning to 30GB+ of RAM to a `def` nesting a few builtin calls, and
shipped a memory safeguard in `jqc-build` as defense-in-depth -- but
its root-cause diagnosis (`lower_call` re-lowering already-processed
content from scratch) turned out to be wrong. This report is the
correction: instrumenting the actual suspect directly, finding the real
cause one layer down in codegen, and fixing it there.

= The diagnosis was wrong -- here's how that showed up

Added a counter to `lower_call` (env-var-gated, since removed) and ran
the single-call `leaders_of` repro that took 27 seconds to macro-expand.
Total `lower_call` invocations: *14*. Nowhere near enough to explain tens
of seconds -- the lowering pass itself is fast; whatever's slow happens
after it. Instrumented `jqc-macros` instead, to print the size of the
`TokenStream` it hands to `rustc` (kept as a permanent, env-var-gated
diagnostic -- `JQC_DEBUG_EXPAND_SIZE=1`):

```
jq!("leaders_of(.a; [])"): expanded to 8784119 chars, 12779 braces
```

*8.7 million characters* of generated Rust, for one `def` maybe fifteen
lines long. That's not a lowering-pass problem -- the AST being lowered
is small; the *text* `jqc-compiler`'s codegen produces from it is not.

= Root cause

Every pass-through codegen node (`Pipe`, `Field`, `Index`, `BinOp`, `If`,
`VarBind`, `Alt`, …) that needed to preserve partial generator output
across an error (see the earlier `partial-output-on-error` report) used a
helper, `partial_aware_for`, that *inlined* a ~15-line loop/match/merge
shape as raw tokens at every call site:

```rust
fn partial_aware_for(sub_c, item_ident, body) -> TokenStream {
    quote! {
        {
            let mut __out = Vec::new();
            match (#sub_c) { /* ~15 lines of loop + merge */ }
            Ok(__out)
        }
    }
}
```

Composing two of these (e.g. `BinOp`'s lhs/rhs, or `Index`'s key/base)
doesn't just add their sizes -- each nests the *other's full expansion*
inside its own `#sub_c`/`#body` slots, which themselves may contain more
nested nodes doing the same thing. Generated code size grows with
something much worse than the AST's own node count once nesting depth
exceeds a handful of levels, which an ordinary script with a `def`
calling a few different builtins (each of which is itself a nested
`def`, inlined the same way) reaches easily:

#diagram(
  boxes: (("1 pass-through node\n(~15 lines inlined)", 0, 0), ("nested 2 levels\n(~15² -ish)", 1, 0), ("nested 4 levels\n(real script)", 2, 0), ("8.7M chars", 3, 0)),
  arrows: ((0, 1), (1, 2), (2, 3)),
)

This is why the existing `scaling_check`/`unused_defs_scaling_check`
fixtures (`jqc-compiler/src/bench.rs`) didn't catch it: they stress a
*single* def referencing itself repeatedly, which is a `lower_call`-shape
stress test, not a codegen-text-size one -- and the `prelude_heavy`
compile-time-benchmark fixture chains builtins via `|`, which doesn't
nest them inside each other's bodies the way `map(if (daggers | any(...))
then ... end)` does.

= Fix

Moved the loop/match/merge shape out of generated text and into a real
runtime function, `jqc_core::partial_for`, called with a small closure
instead of inlined:

```rust
pub fn partial_for<F>(sub: JResult<Vec<Value>>, mut body: F) -> JResult<Vec<Value>>
where F: FnMut(&mut Vec<Value>, Value) -> JResult<()>
{ /* the ~15-line shape, written exactly once */ }
```

```rust
fn partial_aware_for(sub_c, item_ident, body) -> TokenStream {
    quote! {
        #partial_for((#sub_c), |__out: &mut Vec<Value>, #item_ident: Value| -> JResult<()> {
            #body
            Ok(())
        })
    }
}
```

Now nesting composes *additively* in generated size (a function call
plus a small closure body per level) instead of multiplicatively (a full
copy of the loop shape per level). `bubble`/`bubble1` (the merge helpers
`body` calls) changed signature to match -- `&mut Vec<Value>` in place,
returning `JResult<()>`, instead of taking/returning an owned `Vec`:

#diagram(
  boxes: (("inlined text\n(before)", 0, 0), ("function call\n(after)", 1, 0)),
  arrows: (),
)

== A real bug found applying the fix

The first version of `bubble`/`partial_for` had the partial-output
semantics *wrong*: `bubble` drained a failing step's own `emitted` into
`out` but didn't reattach `out` back onto the propagated error, so a bare
`?` right after it would lose everything accumulated so far. The
differential suite caught this immediately -- `repeat_error_optional` and
the new `alt_partial_before_error` test both regressed from their
expected partial-preserving output back to empty. Fixed by making
`bubble`/`bubble1` do the full take-merge-reattach dance themselves (not
just append), and making `partial_for`'s own final merge *additive*
(`out.append(&mut e.emitted); e.emitted = out;`) rather than an
unconditional overwrite, since by the time it runs, `out` may already
have been drained into `e.emitted` by a `bubble` call inside `body` one
level down. Verified both orderings compose correctly by hand-tracing
`repeat_error_optional`'s nested-`Comma`-inside-`RecFn0` structure before
re-running the suite.

= Verification

#table(
  columns: 4,
  align: (left, right, right, right),
  [*case*], [*before*], [*after*], [*factor*],
  [`leaders_of`, 1 call], [8,784,119 chars / 27s], [71,044 chars, well under 1s], [123x / 27x+],
  [`leaders_of`, 2 calls], [OOM-killed (3G cap, 38s)], [6.7s, normal RAM], [--],
  [the real incident script], [33GB+ RSS, killed by hand], [9.6s, normal RAM], [--],
)
"well under 1s" is the macro-expansion portion only; total build (including
dependencies) was 5.3s.

The compiled binary from the real incident script was re-run against
sample input and produces correct output. Full regression suite: 145
differential tests (0 regressions after the `bubble`/`partial_for` fix
above), 251 manual-example tests, `cargo clippy --workspace` clean, `nix
build`/`nix flake check` green. `compile_time_benchmarks`
(`chained_pipe_length` at sizes 1/10/50/200) still scales roughly
linearly, confirming this wasn't a narrow fix for one shape at the
expense of others.

= What's left

The `JQC_BUILD_MEM_LIMIT_GB` safeguard in `jqc-build` (from the earlier
report) stays as general defense-in-depth, not because this specific bug
needs it anymore. `JQC_DEBUG_EXPAND_SIZE=1` is kept as a permanent,
opt-in canary in `jqc-macros` for catching the *next* codegen-size
regression early. No todos.md task remains for this -- it's a completed
fix, not an open limitation.
