#import "../template.typ": report, diagram

#show: report.with(title: "Generating tests from jq's own C test suite", date: "2026-11-24")

= Context

`todos.md` asked to look at jq's own source (not just the manual) for
more test cases, specifically calling out jq's C test suite
(`tests/jq.test` in a jq source checkout) as distinct from the
manual-example generator already built (`jqc-manual-tests`). That
generator's architecture -- a proc macro reading an external file at
`$JQ_SRC`, calling `jqc-compiler` as a library to classify each case,
and emitting a real differential test, an xfail, or a documented
`#[ignore]` -- turned out to transfer almost directly; the real work was
in the file format and in what the C suite's adversarial nature does to
that classification.

= `jq.test`'s format, transliterated from jq's own runner

`tests/jq.test` isn't structured data like the manual's YAML -- it's
line-oriented, and the only documentation of its grouping rules is jq's
own C test runner, `run_jq_tests` in `src/jq_test.c`. `jqc-c-tests-macros`
transliterates that function's loop directly (`parse_c_tests`,
`is_skip_line`) rather than guessing at the format, so it stays correct
if that format's edge cases aren't exactly what a first reading suggests:

#diagram(
  boxes: (
    ("jq.test line", 0, 0),
    ("is_skip_line?\n(blank/comment)", 1, 0),
    ("%%FAIL or\nprogram+input", 2, 0),
    ("CTestCase::Fail\nor ::Normal", 3, 0),
  ),
  arrows: ((0, 1), (1, 2), (2, 3)),
)

A case's own expected-output lines (for the non-`%%FAIL` form) are
consumed only to find where it ends -- never compared against, for the
same reason as the manual generator's real-`jq`-subprocess approach:
jq's test runner checks those lines against a specific build's
`jv_dump` formatting (conditional on build-time options), which this
generator has no way to replicate exactly. Running the `jq` binary on
`$PATH` sidesteps that entirely.

= A third way to classify a case, beyond "compiles" / "doesn't"

The manual generator's two-way split (real differential test if `jqc`
compiles the example, xfail if it doesn't) isn't quite enough here,
because jq's C suite -- unlike the manual's curated, all-successful
examples -- deliberately includes cases that are *expected* to error at
runtime on their given input. A naive differential test would fail the
moment it hit one of those, not because `jqc` is wrong but because the
harness assumed success. `check_case` (`jqc-c-tests/tests/generated.rs`)
tolerates that symmetrically -- both sides erroring counts as agreement,
only a *mismatch* about whether it errors (or a different successful
value) fails:

#diagram(
  boxes: (
    ("run jqc", 0, 0), ("run real jq", 1, 0),
    ("agree? (same value,\nor both error)", 2, 0),
    ("pass / fail", 3, 0),
  ),
  arrows: ((0, 1), (1, 2), (2, 3)),
)

Plus the `%%FAIL` form itself -- jq's own parser specifically rejects the
next program line, and its runner never gets as far as running it at
all. Each generates an xfail asserting `jqc` still fails to compile it
too, the same shape as the ordinary-case xfail but for a parse-time
rejection instead of a missing feature.

Two harness-level leniencies came up immediately on the first real run,
both already handled the same way as "can't verify this one" rather than
a false failure: a leading byte-order mark (real jq's JSON parser
accepts and strips it; `serde_json` doesn't), and jq's permissive
reparsing of its own extreme-magnitude numeric output (`9E999999999`,
too extreme for `serde_json` to parse back into a `Value` at all).

= Four real bugs, found and fixed along the way

Running the generator against the full suite for the first time
surfaced several genuine `jqc` bugs rather than missing features. Per
`todos.md`'s standing instruction, each was isolated and then fixed
directly (not left as a `#[ignore]`) since all four had a reasonably
contained fix, with a regression test added to the primary
`jqc/tests/differential.rs` suite:

- *Nested `def` shadowing, in both directions.* `def f: . + 1; def g: def
  g: . + 100; f | g | f; (f | g), g` -- the inner `def g`'s `rest` (`f |
  g | f`) is lexically inside *its own* scope, so the `g` there means the
  inner `. + 100` def, not the outer one. Both `contains_call` (used to
  decide whether a def is self-recursive) and `rewrite_call0` (used to
  rewrite 0-arg calls into real recursive Rust calls) only protected a
  shadowed def's own `body` from this, not its `rest` -- so a `rest`
  reference that's actually shadowed got misclassified as a call to the
  *outer* def. For `contains_call` that misclassified an actually
  non-recursive outer `g` as recursive; for `rewrite_call0` it could
  rewrite a legitimately-shadowed call into a `RecCall` meant for an
  unrelated def, producing genuine infinite recursion (a real stack
  overflow) at runtime, not just a wrong answer.
- *`$x: val` object-construction shorthand used the wrong key.* Confirmed
  against real jq: `$x: val`'s key is `$x`'s own *value* (the same as a
  parenthesized computed key `($x): val`), not the literal text `"x"` --
  the parser was treating it like the no-colon `{$x}` shorthand instead.
- *An overflowing number literal panicked macro expansion outright,
  rather than merely disagreeing with real jq.* `1e999999999` overflows
  `f64` to infinity at lex time; `quote`'s `ToTokens` for `f64` asserts
  `is_finite()` and panics on a bare infinite/NaN literal, so this was a
  hard crash building the generated code, not a test failure. `jqc`'s
  `Value` being a plain `f64` (vs. real jq's exact-decimal-text
  preservation) is an accepted, documented divergence; the fix only
  makes that divergence surface as a wrong answer instead of a panic,
  by spelling an overflowing literal as `f64::INFINITY`/`NEG_INFINITY`/
  `NAN` rather than a bare token.
- *`reduce`/`foreach`'s `update` clause discarded already-emitted output
  on its own failure.* `let __updates = (#update_c)?;` propagated an
  `Err` (e.g. a `break` from inside `update`) without merging the
  accumulator (`__out`) it already held from completed earlier
  iterations into that error's own `emitted` field, the way
  `core::bubble` does for every other fallible step -- silently
  dropping partial output a `try`/`?` around the whole `reduce`/`foreach`
  should still see. `fetch_or_bubble` replaces the bare `?` with the
  same merge-then-propagate `core::bubble` already does elsewhere.

= Scale, and what's left undone on purpose

#diagram(
  boxes: (
    ("jq.test\n(551 cases)", 0, 0),
    ("generate_c_tests!", 1, 0),
    ("551 #[test] fns", 2, 0),
  ),
  arrows: ((0, 1), (1, 2)),
)

Of the 285 real differential tests, 41 are `#[ignore]`d as documented
known discrepancies (plus 5 of the 19 `%%FAIL` cases, where `jqc`
currently accepts something real jq's parser specifically rejects) --
each with its own reason string in `KNOWN_RUNTIME_DISCREPANCIES` /
`KNOWN_FAIL_CASE_ACCEPTANCES`, from message-text-only wording gaps
(division-by-zero phrasing, omitted operand values) up to a cluster
jq's own CVE-2026-33947/-40612/-43896 regression tests exercise:
`jqc-core`'s `contains`/equality/comparison/object-merge have no depth
limit the way real jq's fix for those CVEs added, and really do
stack-overflow at the same depths real jq's fix now handles cleanly --
flagged in the source as worth prioritizing ahead of other `todos.md`
items, not merely a test inconvenience, alongside one more-severe find:
`. * 1000000000` (string repeat) has no output-size cap either and
actually allocates the full ~3GB result real jq caps and errors on
instead.

= Keeping it out of the main workspace's way

Same mechanism as `jqc-manual-tests`: `jqc-c-tests(-macros)` need
`$JQ_SRC` at *compile* time (the macro reads `jq.test` while expanding),
so they're `workspace.exclude`d to avoid breaking `cargo test --workspace`
for anyone outside the Nix dev shell, reached instead via `cargo test
--manifest-path jqc-c-tests/Cargo.toml`. A `checks.c-tests` flake output
(mirroring the existing `checks.manual-tests`) adds it to `nix flake
check` the same way.

= Verification

`cargo build/test/clippy --workspace`, `cargo fmt --check --all` (no new
drift introduced by this change -- confirmed by diffing against a
stashed working tree), and `cargo test -p jqc --test differential` (161
passed, 0 failed, including both new regression tests) all pass.
`cargo test --manifest-path jqc-c-tests/Cargo.toml` (the new suite): 504
passed, 0 failed, 47 ignored. `cargo test --manifest-path
jqc-manual-tests/Cargo.toml` (unaffected): 251 passed, 0 failed. `nix
build .#checks.x86_64-linux.c-tests` builds and passes standalone.

= What's left

This task is a one-shot "generate from jq's own C suite" -- done now
that it's built, passing, and every discrepancy it surfaced is either
fixed (the four bugs above) or documented with a reason. The CVE-class
stack-overflow and string-repeat memory-size gaps it surfaced are real
unfixed bugs, not test debt; they're left to `todos.md`'s standing
"known limitations" task, called out in the source comments as worth
prioritizing there ahead of other items.
