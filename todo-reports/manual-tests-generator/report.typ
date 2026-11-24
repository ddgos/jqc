#import "../template.typ": report, diagram

#show: report.with(title: "Generating tests from jq's manual", date: "2026-11-24")

= Context

`todos.md` asked for a proc macro that generates tests from a path to
the jq source repository, specifically from the examples in the jq
manual, with that source included as a nix flake dev dependency and the
whole procedure jq-version-aware.

jq's own `docs/content/manual/v<X.Y>/manual.yml` (one per release, back
to v1.3) turns out to be exactly the right shape for this: a schema'd
YAML file (`docs/manual_schema.yml`) of sections → entries → examples,
each example a `{program, input, output}` triple -- 250 of them for
v1.8 alone.

= What was added

- `jq-src`, a new flake input (`github:jqlang/jq/jq-1.8.2`, pinned to
  match the default jq version), exposed as `$JQ_SRC` in every dev
  shell.
- `jqc-manual-tests-macros`: a proc macro,
  `generate_manual_tests!(version = "1.8")`, that reads
  `$JQ_SRC/docs/content/manual/v1.8/manual.yml` and emits one `#[test]`
  per example.
- `jqc-manual-tests`: the crate that calls it
  (`tests/generated.rs`) -- 250 generated tests from a single macro
  invocation.

#diagram(
  boxes: (("manual.yml\n(250 examples)", 0, 0), ("generate_manual_tests!", 1, 0), ("250 #[test] fns", 2, 0)),
  arrows: ((0, 1), (1, 2)),
)

= The hard part: not breaking the build on one unsupported example

Naively emitting `jq!(program)` for every example would make the whole
crate fail to compile the moment *any* example used syntax `jqc` doesn't
support -- a `jq!` compile error isn't a test failure, it's a build
failure, and it's all-or-nothing for the crate. Since `jqc-macros` is
`proc-macro = true`, it can only export proc-macro items to other
crates -- there was no way for the generator to *try* compiling an
example and catch failure as a `Result`.

This is what made the `jqc-compiler` extraction from the version-
selection task load-bearing here, not just there: the generator calls
`jqc_compiler::lower::parse_and_lower` + `TreeWalkCodegen::compile_top`
directly, catching `Err` as a value instead of a macro-expansion-time
compile error, and branches on the result per example:

#diagram(
  boxes: (("example", 0, 0), ("lower + codegen\n(as a library call)", 1, 0), ("Ok: splice closure,\nreal differential test", 2, 0), ("Err: xfail test\n(asserts it still fails)", 2, 1)),
  arrows: ((0, 1), (1, 2), (1, 3)),
)

Per explicit direction, the `Err` case is *not* silently dropped -- every
example gets a test either way. The xfail test for an unsupported
example asserts that compiling it still returns an error; the day `jqc`
gains the needed feature, that assertion starts failing, which is
exactly the signal that it's time to promote that example to a real
test (`todos.md`'s compliance-tracking framing, applied automatically).

= A third category, found on the first real run

The very first full run (250 examples against jq 1.8) came back
*250/251 passing* (counting the summary test) -- 133 examples compiled,
132 as real differential tests plus this one:

```
[repeat(.*2, error)?] on input 1
  jqc:     []
  real jq: [2]
```

This compiles fine under `jqc` (it isn't a syntax gap -- `repeat`,
`error`, and `?` are all supported) but produces the wrong *runtime*
result: a genuine semantic discrepancy in how `jqc`'s desugared
`repeat`/`error` interacts with `?`'s error-suppression versus real
jq's lazy-generator short-circuiting. This doesn't fit either of the
two categories above -- it's not unsupported syntax (so not an
auto-generated xfail) and it's not correct (so it can't be a plain
passing test) -- so a small hand-maintained `KNOWN_RUNTIME_DISCREPANCIES`
list in the generator marks specific `(program, reason)` pairs
`#[ignore]`, bridging into the compliance-tracking the next two
`todos.md` tasks are about. Final result: *250 passed, 1 ignored, 0
failed*.

= Keeping it out of the main workspace's way

`jqc-manual-tests(-macros)` need `$JQ_SRC` at *compile* time, unlike the
hand-written differential suite's need for `jq` on `$PATH` only at
*test-run* time. Putting them in `[workspace] default-members` wasn't
enough -- `cargo test --workspace` ignores `default-members` and builds
every member regardless, which would've broken that command for anyone
outside the Nix dev shell. `[workspace] exclude` does keep them out of
every `--workspace`-prefixed command; the tradeoff is they can no longer
inherit `[workspace.package]` (version/edition/license are duplicated by
hand) and need `cargo test --manifest-path jqc-manual-tests/Cargo.toml`
rather than `-p jqc-manual-tests`.

= Version-aware, concretely

Re-running with `version = "1.6"` instead of `"1.8"` (same macro, same
mechanism) generates against `v1.6/manual.yml` instead -- 127 compiled
+ 92 xfail = 219 examples, a different split from 1.8's 133 + 117,
confirming the generator actually re-targets per version rather than
always reading the same file.

= Verification

`cargo build/test/clippy/fmt --workspace` (main workspace, unaffected,
confirmed via a clean `Cargo.lock` regeneration) and `cargo test
--manifest-path jqc-manual-tests/Cargo.toml` (250 passed, 1 ignored, 0
failed) both pass; `nix flake check` passes including the new `jq-src`
input; manually confirmed `$JQ_SRC` resolves inside `nix develop`.
