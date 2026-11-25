#import "../template.typ": report, diagram

#show: report.with(title: "Eager to lazy codegen", date: "2026-11-25")

= Context

`todos.md` asked for the compiler to switch from eager to lazy evaluation,
calling out the headline symptom directly: `[limit(3; while(true; .))]`
stack-overflowed instead of staying bounded at 3, because every
sub-expression compiled to a fully-materialized `JResult<Vec<Value>>` --
`while`'s own recursive desugaring had to finish producing its *entire*
(here, infinite) output before `limit` ever got a turn to cut it short.
Being 100% compliant with real `jq` is a stated project goal, and this was
the single largest outstanding divergence from it.

= What changed

Every compiled jq expression is now a `jqc_core::JGen` -- a boxed, pull-based
`Iterator<Item = JResult<Value>>` -- instead of an eagerly-computed
`Vec<Value>`. `jqc-core/src/gen.rs` is the new runtime support this needs: a
handful of combinators (`flat_map_gen`, `lazy_chain`, `catch_gen`, `alt_gen`,
`collect_gen`, `ForeachGen`) that every codegen'd node is built from.
`jqc-compiler/src/codegen.rs` was rewritten end to end to emit `JGen`-typed
expressions via these combinators instead of `Vec`-concatenation. The old
`JqError::emitted`/`bubble`/`bubble1`/`partial_for` machinery -- which only
ever existed to simulate "a `try`/`?` sees whatever a generator already
produced before it errored" by hand, inside the eager model -- is gone
entirely; once evaluation is genuinely lazy, that behavior just falls out of
a consumer (array construction, `catch_gen`, the top-level `collect_gen`)
stopping after the first `Err` it pulls.

#diagram(
  boxes: (
    ("AST node", 0, 0),
    ("JGen combinator", 1, 0),
    ("pull-based Iterator", 2, 0),
    ("consumer decides\nhow far to pull", 3, 0),
  ),
  arrows: ((0, 1), (1, 2), (2, 3)),
)

The top-level compiled closure's signature is unchanged
(`Fn(&Value) -> JResult<Vec<Value>>`, via `collect_gen` at the very outside)
-- only the *internal* pipeline between "input" and "collected output" is
now lazy, so this is not a breaking change to `jqc`'s public API.

= The move/lifetime debugging story

The hard part was never the laziness design itself -- it was a
`'static`/move/lifetime minefield. Every `Box<dyn FnMut(...) -> JGen +
'static>` closure can only capture *owned* values, and a value shared by two
sibling sub-expressions (or by an eager argument and something nested inside
the closure it builds) gets "used up" the first time one closure captures
it, breaking either a sibling compiled separately in the same scope, or the
closure's own repeatability if it's itself called more than once. Two
general fixes came out of the initial rewrite, both still documented
in-place in `codegen.rs`:

- `snapshot_scope`/`compile_isolated` -- every child of a multi-child node
  gets its own fresh clone of the ambient input and every in-scope variable.
- An explicit per-level re-clone threaded through recursion, for nodes that
  nest `'static` closures *within* `'static` closures three or more levels
  deep (`compile_slice`'s `from`/`to`/`base`, `compile_range`'s
  `from`/`to`/`step`, `compile_if`'s `elif` chain).

Several rounds of writing a filter, dumping its real macro expansion into a
standalone crate, and reading the exact rustc `E0507`/`E0521`/`E0382` span
were needed to find which specific shared value needed another level of
cloning.

= Second pass: fuzzing the rewrite itself

The initial rewrite passed the full existing suite unchanged and was
believed done. Treating that belief as a hypothesis rather than a
conclusion, the script fuzzer (`jqc/tests/fuzz_scripts.rs`) was extended to
actually generate the shape this task was about -- `limit`-bounded
`while(cond; update)`, previously excluded from the grammar entirely because
it used to hang -- and run for many iterations. This promptly found four
more real, previously-undetected bugs, all compile-time failures (not silent
wrong answers), all introduced by the same `'static`-closure-sharing hazard
the first pass already knew to watch for, just in spots that hadn't been
exercised yet:

+ *`any`/`all` were still eager.* They were implemented via `reduce` in the
  prelude, which must fully drain its source -- no amount of codegen
  laziness rescues a *builtin* that's structurally incapable of
  short-circuiting. Rewritten to match real jq's own `isempty`/`first`-based
  definition (`def any(generator; cond): isempty(generator|cond or
  empty)|not;`), which *does* short-circuit now that `first` is genuinely
  lazy. Fixes the then-stale `jqc-c-tests-macros` xfail entries for
  `any(true, error; .)` / `all(false, error; .)`, which real jq evaluates to
  `true`/`false` without ever touching `error`.
+ *`compile_by` (`sort_by`/`group_by`/`unique_by`/`min_by`/`max_by`) spliced
  its key filter into a runtime `for` loop without isolating it.* If the key
  filter referenced an in-scope `$var` from an enclosing `foreach`/`reduce`/
  `as`, the *first* loop iteration's compiled closure captured that outer
  variable by move, leaving nothing for the second array element --
  `E0382`, reproduced directly via `[foreach (1,2,3) as $x (0;
  sort_by((2.4|fabs))?)]`. Fixed by isolating the key filter
  (`compile_isolated` instead of a bare `compile`), the same treatment
  `Reduce`'s own per-item loop already had.
+ *A self-recursive 0-arg def (`RecFn0`, compiled as a genuine Rust `fn`
  item) can't capture anything from its lexical environment at all -- that's
  a hard Rust restriction, not a bug to work around -- yet its body was
  compiled against the full ambient scope anyway.* Any reference to an outer
  `$var`/label inside it (even one *unused* by the recursive body, since
  `compile_isolated`'s "harmless" unconditional re-clone-every-scope-var
  still emits code for each one) produced a confusing `E0434`. Since
  `while`/`until`/`repeat`/`recurse(f)` all desugar through this exact
  mechanism, this fired on `while`/etc. used *anywhere* inside so much as one
  enclosing `foreach`/`reduce`/`as`/`label` -- not an exotic case. Fixed by
  compiling `body` against an empty scope: a genuine free reference now
  surfaces as this crate's own clear, existing "undefined variable"
  diagnostic at the point of definition, instead of a wall of rustc output.
  Real support for recursive defs closing over their lexical environment
  (threading captured variables through as extra parameters) remains
  unimplemented -- noted as a new, explicit limitation rather than a crash.
+ *`Foreach`'s `update`/`extract` closures are two *siblings*, both `move`,
  both potentially referencing the same outer `$var`.* Unlike a `for`-loop
  body (where re-reading a captured value across iterations is fine), a
  `move` closure consumes whatever it captures at the point the closure
  *literal* is constructed -- so building `update_closure` first, when it
  captures the only copy of an outer `$var`, left nothing for
  `extract_closure`'s construction right after it. `E0507`, reproduced via
  `[foreach (1,2) as $x (0; [limit(3; while(true; .))])]` (note: `while`'s
  own body here never even mentions `$x`; the race is purely at
  closure-construction time, before either body runs at all). Fixed by
  giving each of the two sibling closures its own independent
  `snapshot_scope` fresh-clone *before* its own `move |...|` literal is
  built, rather than relying on each closure's internal (too-late, by then)
  `compile_isolated` re-clone.

All four are now permanent regression tests in `jqc/tests/differential.rs`
(`lazy_any_short_circuits_past_error`, `lazy_all_short_circuits_past_error`,
`sort_by_key_referencing_outer_foreach_var`, plus the original headline
`lazy_limit_over_infinite_while`/`lazy_limit_over_repeat`/
`lazy_first_over_repeat`/`lazy_user_foreach_piped_into_limit`), and the two
now-fixed `jqc-c-tests-macros` xfail entries (`[limit(1; 1, error)]`,
`first(1,error("foo"))` -- fixed by the original rewrite, just not yet
re-verified -- and `any(true, error; .)`/`all(false, error; .)`) were
removed from `KNOWN_RUNTIME_DISCREPANCIES`.

The fuzz grammar now generates `[limit(3; while(c; u))]` (not `until`/
`repeat`, deliberately: both can produce *zero* items forever rather than
merely "more than `limit` wants", which no amount of laziness rescues --
confirmed directly that real `jq` itself hangs on `[limit(3; until(null;
null))]` the same way). That same run also surfaced that `fuzz_scripts.rs`
had no per-case timeout at all, so one bad generated case could hang the
whole suite indefinitely; `run_compiled`/`run_real_jq` now both run under
`timeout 5`.

= Verification

- `cargo test/clippy/fmt --workspace`: all pass (168 differential cases, up
  from 161 before this task).
- `cargo test --manifest-path jqc-c-tests/Cargo.toml`: 513 passed, 38
  ignored (down from 42 -- the four fixed eager-evaluation entries).
- `cargo test --manifest-path jqc-manual-tests/Cargo.toml`: 251 passed.
- `nix build .#default .#example-script
  .#checks.x86_64-linux.{manual-tests,c-tests}`: all build.
- `cargo test -p jqc --test fuzz_scripts -- --ignored`, run several dozen
  times across both the second-pass investigation and final confirmation:
  zero build failures remaining from this task's own changes. The one
  residual flake it still finds (`split("")` on a non-empty string) is a
  pre-existing, already-documented discrepancy unrelated to laziness
  (`jqc-c-tests-macros`'s own `KNOWN_RUNTIME_DISCREPANCIES` list), not a new
  regression.
- README's "Known limitations" no longer lists "Eager, not lazy" at all.
