#import "../template.typ": report, diagram

#show: report.with(title: "Partial generator output across errors", date: "2026-11-24")

= Context

The previous task's `xfail_difftest!`/`KNOWN_RUNTIME_DISCREPANCIES` entries
tracked one concrete discrepancy: `[repeat(.*2, error)?]` on input `1` gave
`[]` in `jqc`, but `[2]` in real `jq`. `todos.md` asked to fix the
underlying bug and promote that test to green -- and to keep this task on
the list until there are *no* xfail tests left.

The root cause was architectural, not a one-line logic error. `jqc`
compiles every generator node to Rust code of the shape "build a
`Vec<Value>`, return it, or `?`-propagate an error" -- and a bare `?`
*discards* whatever had already been pushed onto that `Vec` before the
failure. Real `jq` generators are lazy and stream one value at a time, so
`try`/`?` only ever suppresses the error *signal*; any values already
produced stay in the output. `repeat(.*2, error)` is exactly this: it
emits `2` successfully, *then* raises `error` -- and jq's `?` keeps the
`2`. `jqc`'s eager `Vec`-building model was throwing the whole thing away.

= What changed

`JqError` (`jqc-core/src/error.rs`) gained an `emitted: Vec<Value>` field:
whatever a generator chain had already produced before the error that's
now propagating. Two helpers fold a sub-result into an accumulator while
preserving this on failure:

```rust
pub fn bubble(prior: Vec<Value>, next: JResult<Vec<Value>>) -> JResult<Vec<Value>>;
pub fn bubble1(prior: Vec<Value>, next: JResult<Value>) -> JResult<Vec<Value>>;
```

`jqc-compiler`'s codegen (`jqc-compiler/src/codegen.rs`) was audited node
by node and split into two categories:

- *Pass-through* nodes, where each element a sub-expression produces
  directly becomes (or feeds into) this node's own output stream --
  `Pipe`, `Comma`, `Field`/`Index`/`Slice`/`IterateAll`, `BinOp`,
  `And`/`Or`, `If`, `VarBind` (`as`), `Neg`, `range`, and the builtin-call
  cartesian product (`has`, `contains`, `startswith`, …). These now thread
  `bubble`/`bubble1` through their accumulation instead of a bare `?`.
- *Collector* nodes, where sub-elements are folded into one atomic
  combined value and never individually observable -- `[...]` array
  construction, object construction, string interpolation, `reduce`'s
  running state, and the sort-key vector behind `sort_by`/`group_by`/etc.
  These are intentionally untouched: if the thing being collected fails
  partway, there's no completed value to emit regardless, so discarding
  on error was already correct, not a bug.

A new `partial_aware_for` helper builds the common "iterate a
sub-expression's output, and if the sub-expression itself fails partway
through generating that output, still run the per-element body over
whatever it *did* produce before re-raising" shape once; pass-through
nodes with more than one nested generator (`Index`'s key/base, `Slice`'s
from/to/base, `BinOp`'s lhs/rhs) compose it at each level via `bubble`,
so a failure at any level still surfaces everything produced above it:

#diagram(
  boxes: (
    ("index_c\n(outer)", 0, 0), ("base_c\n(inner)", 1, 0), ("lookup\n+ push", 2, 0),
    ("re-raise with\nemitted = __out", 2, 1),
  ),
  arrows: ((0, 1), (1, 2), (2, 3)),
)

`TryCatch`/`Optional` (`?` with no handler) now recover `e.emitted` as
their `Ok` result instead of an empty `Vec`; a `catch` handler's own
output is appended *after* that recovered prefix, matching `try BODY
catch HANDLER`'s "already-streamed values stay, handler's output comes
next" semantics.

= Verification

`[repeat(.*2, error)?]` on `1` now compiles to `[2]`, matching real `jq`
exactly. `repeat_error_optional` was promoted from `xfail_difftest!` to a
plain `difftest!`; the now-unused `xfail_difftest!` macro was deleted;
`KNOWN_RUNTIME_DISCREPANCIES` in `jqc-manual-tests-macros` was emptied.

- `cargo test --workspace`: 125/125 differential tests pass (previously
  124 + 1 xfail), plus fuzz/version-selection/compiler-unit suites, all
  green.
- `cargo test --manifest-path jqc-manual-tests/Cargo.toml`: 251/251 pass
  (the auto-generated suite from jq's own manual examples).
- `cargo test -p jqc --test fuzz_scripts -- --ignored` (arbitrary-script
  fuzzing against real `jq`): passes.
- `cargo clippy --workspace --all-targets`: zero warnings (the refactor
  also fixed several pre-existing `needless_question_mark` lints in the
  builtin-call cartesian product by standardizing its `build` closures to
  always return `JResult<Value>`).

= What's left

Zero xfail tests remain anywhere in the project (`grep -rn xfail` across
`jqc/tests` and `jqc-manual-tests-macros` turns up only the unsupported-
syntax category the generator documents as permanent-by-design -- a
missing feature, not a wrongness bug). Per `todos.md`'s instructions,
this task is removed from the list.

One known, documented gap remains in the comment on `Index`'s codegen:
`partial_aware_for` composition handles arbitrarily-nested generator
failures correctly in general, but genuinely interleaved streaming (where
`jqc` would, like real `jq`, never even *evaluate* a later generator
step once an earlier `try`/`limit` has already decided the outcome) isn't
implemented -- `jqc` is eager, so e.g. `first(f)` forces all of `f` under
the hood rather than stopping at the first value like real `jq`'s
`label`/`break`-based definition does. That's a separate, pre-existing
limitation (laziness, not error-partiality) and out of scope here; it's
tracked under the broader "known limitations" task in `todos.md`.
