#import "../template.typ": report, diagram

#show: report.with(title: "Widening the script and input fuzzers", date: "2026-11-24")

= Context

`todos.md` asked to improve the script/input fuzzing in two concrete
ways: use arbitrary UTF-8 strings (not just a small curated alphabet),
and widen the proptest grammars until, in theory, every valid script and
input has a chance of being generated -- "do not stop until both are
true." Both existed already (`jqc/tests/fuzz_inputs.rs`,
`fuzz_scripts.rs`) but narrowly: inputs used `"[a-z]{0,6}"` strings only,
and the script grammar covered a dozen or so arithmetic/control-flow
shapes, nowhere near README.md's full "Supported jq subset" list.

= Arbitrary UTF-8, concretely

Every `char` is already a valid Unicode scalar value, so
`prop::collection::vec(any::<char>(), 0..N)` collected into a `String`
is already correctly-encoded arbitrary UTF-8 -- no separate "is this
valid" concern the way generating raw bytes would need. Used in both
files: `fuzz_inputs.rs`'s JSON leaves now mix the old boring alphabet
with this (keeps most failures easy to read, while still reaching
control characters, combining marks, and astral-plane codepoints needing
a UTF-16 surrogate pair in JSON's own `\uXXXX`); `fuzz_scripts.rs` gained
a `jq_string_literal` encoder (matching `jqc-compiler`'s own lexer
escape table exactly) so generated filters can contain string literals
built from the same arbitrary content, not just field-access chains.

= Grammar coverage

`fuzz_inputs.rs` grew from 14 to 28 cases: a `arb_json_stringy` generator
(strings dominate the leaf distribution, so filters that index straight
to `.a` actually hit adversarial UTF-8 most of the time) backs new cases
for every string builtin README.md lists that had no coverage at all
before -- `explode`, `ascii_downcase`/`upcase`, `tostring`/`tonumber`,
`split`/`join`, `startswith`/`endswith`/`contains`, `ltrimstr`/`rtrimstr`.

`fuzz_scripts.rs`'s grammar grew from ~12 productions to ~60, spanning
nearly all of README's "Supported jq subset" list: full arithmetic/
comparison/logical operators, array/object construction (incl. `{a}`
shorthand and computed keys), indexing/slicing, `try`/`catch`/`?`, `in`,
every 0-arg and 1-filter-argument builtin listed there, `has`, `range`,
`limit`, negative/float number literals, and -- the part needing actual
design work, not just a longer list -- scope-safe bindings:

#diagram(
  boxes: (("bound position\n(as $x | …)", 0, 0), ("body strategy\n(may splice $x)", 1, 0), ("always well-scoped", 2, 0)),
  arrows: ((0, 1), (1, 2)),
)

A bare `$x`/`break $out`/`f` leaf available *everywhere* would just as
easily land outside its own binding's scope -- a real (and, since a
build failure is how this fuzzer reports bugs, fuzzer-breaking)
undefined-variable compile error, not a `jqc` bug. `arb_body_maybe_var`/
`arb_body_maybe_break`/`arb_body_maybe_call_f` generate the binding
(`as $x`, `reduce … as $x`, `label $out`, `def f`) and its body together
instead, so a reference to the bound name only ever appears where it's
actually in scope.

= A safety catch before any of this ran unattended

`repeat`/`while`/`until`/`recurse(f)` were deliberately *never* added to
the grammar, even wrapped in `limit`, after confirming why that's not
safe: all four desugar (`jqc-compiler/src/lower.rs`'s prelude) to a 0-arg
*recursive* `def`, and `jqc` being eager, not lazy, means `limit(n; exp)`
-- itself desugaring to a `foreach exp as $x (...)` -- needs `exp`'s
*entire* output materialized before its own `break` ever runs. For a
condition that's unconditionally true, `exp` never finishes, so `limit`
doesn't bound it at all. Confirmed directly:
`[limit(3; while(true; .))]` stack-overflows rather than staying bounded
at 3. README's "Known limitations" claimed `limit`/`label`/`break`
"supported" this as a bounding mechanism -- genuinely wrong, now
corrected (they still work for cutting an early exit out of a merely
*large*, finite generator, just not an infinite one). A good example of
a fuzzer paying for itself before generating a single test case: this
would have been a hung or crashed CI run, not a reported mismatch.

= Bugs found and fixed

Both widened fuzzers immediately found real, previously-unknown `jqc`
bugs -- not from the UTF-8 content itself (no encoding-specific bug
surfaced), but from the broader *combinations* of builtins and input
shapes the old narrow grammars never reached:

- *`contains` didn't check its operands are the same kind before
  recursing.* Real jq's `f_contains` (`src/builtin.c`) requires equal
  `jv_kind` -- which distinguishes `true`/`false` as separate kinds, not
  just "boolean" -- and errors otherwise ("object ({}) and null (null)
  cannot have their containment checked"); `jqc-core`'s `contains_value`
  fell through to a same-kind-or-not-agnostic equality check instead.
- *`join` required its input to specifically be an array, and
  mis-handled its separator and non-scalar elements.* Real jq's own
  `def join($x): reduce .[] as $i (null; ACC + ELEM) // ""` (1) iterates
  `.[]`, which also works on an object's *values*, not just arrays; (2)
  combines the separator via ordinary `+`, so jq's null-absorption
  applies (a `null` separator is silently absorbed, not an error) *and*
  it's never even evaluated between fewer than two elements; (3) leaves
  an array/object element as itself to `+`-concatenate onto the string
  accumulator, which is a genuine type error, not something to
  stringify. `jqc-core`'s `join_strings` required `as_array`, required
  the separator to be a string unconditionally, and silently stringified
  non-scalar elements -- reimplemented structurally against jq's own
  definition instead of ad-hoc rules, to get the edge cases right.
- *`ltrimstr`/`rtrimstr` didn't validate their operands are strings.*
  Real jq defines these via `startswith`/`endswith`, which require (and
  validate) both operands are strings; `jqc`'s versions silently
  returned the input unchanged for any non-string pair instead of
  erroring -- already a documented `jqc-c-tests` discrepancy, now fixed
  (which in turn exposed a *different*, still-open bug: the collector-
  node-should-produce-nothing-on-partial-failure class already tracked
  elsewhere, now also hit via `[\"ok\", ($x | ltrimstr($y))]` once
  `ltrimstr` can actually fail partway through building that array).
- *`has`/`in` errored on a `null` container instead of returning
  `false`.* Real jq's `jv_has` (`src/jv_aux.c`) special-cases `null`
  unconditionally; `jqc-core`'s `has_key` had no such case.
- *`tostring`/string interpolation used `serde_json::Value`'s own
  `Display` for a non-string value*, which (a) doesn't escape `DEL`
  (0x7F) the way real jq's `jvp_dump_string` does, and (b) lowercases a
  scientific-notation exponent where real jq always uppercases it --
  both already fixed once, for `jqc-cli`'s own output formatting, in the
  previous task; this is the same fix applied where `jqc-core` itself
  stringifies a value (`tostring`, `"\(expr)"` interpolation, `+`'s
  string-coercion), a separate code path `jqc-cli` never touches.

= Verification

`cargo test -p jqc --test fuzz_inputs`: 28 passed (up from 14), 0 failed,
across several repeated runs (proptest reseeds each run). `cargo test -p
jqc --test fuzz_scripts -- --ignored`: clean across repeated runs
including stress runs at 100-150 cases (well above the default 30) to
build confidence in the much wider grammar before settling back to a
small default case count. `cargo build/test/clippy --workspace`, the
differential/manual-tests/c-tests suites, all pass -- the `jqc-c-tests`
suite's own summary moved from 504 passed/47 ignored at the start of
this task to 509 passed/42 ignored, the net effect of the `join`/
`ltrimstr`/`rtrimstr` fixes above (one `ltrimstr`/`rtrimstr` c-test pair
stays `#[ignore]`d, now pointing at the newly-exposed collector-node bug
instead of the fixed type-validation gap).

= What's left

Per `todos.md`'s own "do not stop until both are true" framing: arbitrary
UTF-8 is now genuinely used on both sides, and the script grammar covers
the large majority of README's "Supported jq subset" list, but not
literally all of it -- `?//` alternative patterns (not implemented by
`jqc` at all), parameterized non-recursive `def`, `{$a}`/computed nested
destructuring, and a few less common builtin argument shapes aren't
generated. This task is left open in `todos.md`, the same as "known
limitations" -- an asymptotic goal, not a one-shot completion -- rather
than claimed finished.
