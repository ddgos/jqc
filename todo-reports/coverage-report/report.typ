#import "../template.typ": report, diagram

#show: report.with(title: "Code coverage from the test suite", date: "2026-11-24")

= Context

`todos.md` asked to generate a code coverage report from the tests.
`cargo-tarpaulin` (added to the Nix dev shell) runs the workspace test
suite under Linux ptrace-based instrumentation and reports per-line
coverage; `nix develop -c cargo tarpaulin --workspace --out Html --out
Lcov --output-dir target/tarpaulin` produces both an HTML report and an
lcov file (not checked in -- `target/` is already gitignored, and this is
a point-in-time report, not something meant to be regenerated-and-diffed
in CI yet).

= Headline number, and why it's misleading

#table(
  columns: 2,
  align: (left, right),
  [*crate*], [*lines covered*],
  [jqc-core (`helpers.rs`, `error.rs`)], [313/408 (77%)],
  [jqc-macros (`ast`/`lexer`/`parser`/`lower`/`codegen`/`lib`)], [301/1192 (25%)],
  [*workspace total*], [*38.76% (707/1824)*],
)

The workspace-wide 38.76% number is not a meaningful signal on its own.
`jqc-macros/src/codegen.rs` and `ast.rs` show *zero* coverage, and
`lib.rs`'s actual `jq!` proc-macro function shows zero, despite being the
single most exercised code path in the whole repository -- every one of
the 124 differential tests and 15 fuzz-input tests invokes it.

= The real finding: proc-macro code is invisible to runtime coverage tools

`jq!` is a `#[proc_macro]`. Its lex → parse → lower → codegen pipeline
runs *while `rustc` is compiling* `differential.rs`/`fuzz_inputs.rs` --
inside the compiler process, before the test binary that tarpaulin
instruments even exists:

#diagram(
  boxes: (
    ("rustc compiles\ndifferential.rs", 0, 0), ("jq! expands\n(lex/parse/lower/codegen)", 1, 0),
    ("test binary\nproduced", 2, 0), ("tarpaulin runs\n+ instruments", 3, 0),
  ),
  arrows: ((0, 1), (1, 2), (2, 3)),
)

Tarpaulin only sees the right-hand half of that diagram. The pipeline's
own logic has already finished running, with its results baked into the
test binary as ordinary (now-uninstrumented) machine code, by the time
tarpaulin starts watching. This isn't specific to tarpaulin -- any
coverage tool that instruments the compiled *test binary's* execution has
the same blind spot for proc-macro code, since that code's "execution"
happened one compilation stage earlier.

The non-zero coverage `lower.rs` (41/257), `lexer.rs` (57/150), and
`parser.rs` (203/385) do show comes entirely from the one `#[cfg(test)]`
unit test in `lower.rs` (`prelude_parses`) that calls `parse_and_lower`
directly *inside the test binary itself* -- a genuine runtime call,
unlike every `jq!(...)` invocation elsewhere in the codebase.

= What this means in practice

- `jqc-core`'s 77% is a legitimate, useful number -- that code runs at
  the *runtime* of generated closures, which the test suite does execute
  normally.
- `jqc-macros`' real confidence signal is the differential/fuzz suite's
  pass rate (124 + 15 passing, covering a wide slice of the supported jq
  subset end-to-end), not a line-coverage percentage. Padding that
  percentage by adding `#[cfg(test)]` unit tests that call `lower`/
  `codegen` directly, duplicating what the differential suite already
  covers via `jq!`, would move the number without adding real signal --
  not done here.
- A future CI coverage gate for this repo should either exclude
  `jqc-macros` from the threshold or special-case it, rather than
  trying to raise its number through tooling.

= Verification

`cargo tarpaulin --workspace --out Html --out Lcov --output-dir
target/tarpaulin` completes cleanly and reproduces the numbers above;
`jqc-macros/src/bench.rs` (the memory/time cost-cliff checks, `#[ignore]`d
and not meant to run under coverage instrumentation) is excluded via
`--exclude-files`.
