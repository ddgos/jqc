#import "../template.typ": report, diagram

#show: report.with(title: "xfail tests for known discrepancies", date: "2026-11-24")

= Context

`todos.md` asked for tests covering every known discrepancy between
`jqc` and real `jq`, marked xfail. The previous task (generating tests
from jq's manual) already surfaced the one currently-known case while
building that generator: `[repeat(.*2, error)?]` on input `1` -- `jqc`
gives `[]`, real jq gives `[2]` -- a genuine semantic gap in how `jqc`'s
`repeat`/`error` desugaring interacts with `?`, not a missing feature.

The 124 filters in `jqc/tests/differential.rs`, the primary hand-written
correctness suite, all currently pass; this is the only discrepancy on
record anywhere in the project.

= What was added

`jqc-manual-tests-macros` already tracked this via a hand-maintained
`KNOWN_RUNTIME_DISCREPANCIES` list (`#[ignore]`d in the generated
suite). This task adds the hand-written-suite equivalent: an
`xfail_difftest!` macro in `differential.rs`, alongside the existing
`difftest!`, and one use of it:

```rust
xfail_difftest!(
    repeat_error_optional,
    "[repeat(.*2, error)?]",
    "1",
    want_now: vec![jqc::core::json!([])],
    "jqc's repeat()/error interaction with `?` doesn't match real jq's \
     lazy-generator short-circuit semantics yet ..."
);
```

Unlike a plain `#[ignore]`, `xfail_difftest!` still *runs*: it asserts
the compiled output does **not** match real jq (so it fails loudly, not
silently, the moment the discrepancy is fixed) and that it still equals
the specific currently-wrong value (so an unrelated regression that
changes the wrong output to a *different* wrong output is still caught,
rather than being masked by a bare "doesn't equal jq" check):

#diagram(
  boxes: (("got != real jq?", 0, 0), ("got == current\nknown-wrong value?", 1, 0), ("pass (still xfail)", 2, 0)),
  arrows: ((0, 1), (1, 2)),
)

Two independent trackers exist by design, not duplication: the manual-
generated suite's list is for discrepancies discovered via that
auto-generated 250-example pile, which will keep growing as examples
get added upstream; the hand-written suite's `xfail_difftest!` is for
the small, curated set of filters a human specifically chose to track
in the primary suite. The one current discrepancy happens to be in
both, since it was found via the generator and is significant enough to
also want front-and-center in `differential.rs`.

= Verification

`cargo test -p jqc --test differential`: 125 passed (124 existing +
1 new xfail), 0 failed. `cargo build/test/clippy/fmt --workspace` all
pass. The full differential/fuzz/version-selection/jqc-compiler suites
remain green, confirming nothing else regressed.

= What's left

This task is a one-shot "every currently-known discrepancy has a
test" -- done once the discrepancy above is covered. `todos.md`'s next
task ("drive compliance to 100%, until there are no longer xfail
tests") is the perpetual follow-on, and now has exactly one concrete
thing to fix: `repeat`'s interaction with `error` and `?`.
