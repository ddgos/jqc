#import "../template.typ": report, diagram

#show: report.with(title: "Error messages: span tracking and a rustc-style renderer", date: "2026-11-24")

= Context

Every lex/parse/lower/codegen error in `jqc-compiler` was a bare `String`
built with `format!`, Debug-printing the offending token (`found Ident("selct")`)
with no indication of *where* in the filter the problem was -- the whole
filter string literal's span was all `jq!`'s compile error could point at,
regardless of filter length. The `todos.md` task asked for Rust's own
diagnostics to be the bar: pin down the location, show the source, and
suggest a fix.

= Design

A new `jqc_compiler::diagnostics` module defines `Span` (a char-index range)
and `Diagnostic { message, span, help, notes }`, replacing `String` as the
`Err` type of every stage's result alias (`LexResult`, `PResult`, `LResult`,
`CResult`). `Diagnostic::render(src)` produces the rustc-shaped output:

```
error: unsupported or unknown jq function 'selct/0'
 --> 1:6
  |
1 | .a | selct
  |      ^^^^^
help: did you mean `select`?
```

#diagram(
  boxes: (("lex\n(+ spans)", 0, 0), ("parse\n(+ spans, delim notes)", 1, 0), ("lower\n(+ relocate)", 2, 0), ("codegen\n(+ relocate)", 3, 0), ("render(src)", 4, 0)),
  arrows: ((0, 1), (1, 2), (2, 3), (3, 4)),
)

Three mechanisms do the real work:

- *Spans everywhere.* The lexer now returns `(Vec<Tok>, Vec<Span>)` instead
  of just `Vec<Tok>`; the parser carries matching spans and a stack of
  currently-open `(`/`[`/`{` so an unmatched-close error gets a secondary
  `note:` pointing back at *where it was opened* -- the same trick rustc
  uses for unmatched delimiters. `Expr::Call`, `Expr::VarRef`, and
  `Expr::FuncDef` gained a `Span` field so the two real semantic errors
  that only show up after lowering/codegen (an unresolved call, an
  undefined `$var`, a parameterized recursive `def`) can still point at
  the right source location instead of just naming the problem.
- *"Did you mean?" suggestions.* `closest_match` does plain Levenshtein
  distance against a static list of builtin/prelude names (for unresolved
  calls) or the variables actually in scope (for undefined `$var`s),
  capped at a third of the target's length so it doesn't suggest nonsense
  for a wildly wrong name.
- *Relocation.* `parse_and_lower` actually parses `PRELUDE + "\n" +
  user_src`, so every span it produces is relative to that concatenation,
  not the user's own filter text. `Diagnostic::relocate(offset, len)`
  shifts spans back (dropping ones that land entirely in `PRELUDE` itself
  -- a jqc bug, not explainable from the user's source -- and clamping
  ones that run past the end, e.g. an unclosed delimiter whose EOF search
  reaches real end-of-input, so even that case keeps *a* location instead
  of losing it). `parse_and_lower` applies this to its own parse/lower
  errors before returning; `jqc-macros` (and tests) apply the same
  `lower::user_src_window` offset to a `compile_top` error afterward,
  since codegen runs after `parse_and_lower` has already returned and its
  spans are therefore still `PRELUDE`-relative.

== A real bug found along the way

`parse_and_lower` originally wrapped `user_src` in a synthetic `(...)` before
splicing it after `PRELUDE`, for no documented reason. That wrapper leaked
into error text: an error whose "found" token was actually the wrapper's own
closing paren rendered as "found `)`" -- misleading, since the user never
wrote one. A
genuinely unclosed `(` in the user's own filter was *silently absorbed* by
the wrapper's `)` (the parser matched it against the user's inner open
paren instead), reattributing the real error to the *outer*, synthetic
paren instead of the user's own. Removing the wrapper (full differential
suite, manual-tests suite, and fuzz suites all still pass unchanged) fixed
both: `note: unclosed '(' opened here` now correctly points at the user's
own `(`, and a mid-expression EOF reports `found end of input` rather than
a phantom `)`.

= Verification

#table(
  columns: 2,
  align: (left, left),
  [*filter*], [*rendered error*],
  [`(1 + 2`], [points at real EOF; note points back at the user's own open paren],
  [`.a | selct`], [points under `selct`; help suggests `select`],
  [`$nope`], [points under `$nope`; help notes no variables are in scope],
  [`def f(x): f(x); f(1)`], [points under `f`; help suggests a `reduce`/`foreach` loop instead],
  [`"abc` (unterminated)], [points at the opening quote; help names the missing closing quote],
)

Confirmed end-to-end through `jqc-build` too (not just the unit-level
`Diagnostic::render` calls above): compiling a `.jq` file with a typo'd
builtin now surfaces, inside rustc's own "could not compile" output, the
exact jq-source line/column, a caret under `selct`, and a "did you mean
select?" help line -- nested under the real `jq!` macro-call span the way any
`compile_error!` text would be.

New tests: 4 unit tests in `diagnostics.rs` (render position/column
correctness, `relocate`'s drop-vs-clamp behavior, `closest_match`), 6 in
`lower.rs` (one per error class above, run through the full
`parse_and_lower` + `compile_top` pipeline with relocation applied exactly
as `jqc-macros` applies it, asserting on the *rendered* text) plus a
permanent `#[ignore]`d `print_demo` test (consistent with the existing
`compile_time_benchmarks`/`peak_memory_benchmarks` diagnostics-not-assertions
convention) for eyeballing new cases by hand. Full regression suite: 145
differential tests, 251 manual-example tests, 15 fuzz-input tests, `cargo
clippy --workspace --all-targets` clean -- all unaffected by either the
`Diagnostic` plumbing or the wrapper removal.

= What's left

`lit_str_only` (interpolated string used as a field/object-key name) still
reports without a span -- it's a free function with no access to the
parser's token position, and the case is rare enough that threading one
through wasn't worth it here. Error quality for the known-unsupported
constructs (`path`/`del`/`|=`, destructuring, `label`/`break`, …) is still
just "the parser doesn't recognize this syntax at all" -- tracked
separately under the "known limitations" todo, not this one.
