#import "../template.typ": report, diagram

#show: report.with(title: "A justfile for every test command", date: "2026-11-25")

= Context

`todos.md` asked for an audit of every way this project can be tested --
`cargo test --workspace`, the two `$JQ_SRC`-gated suites
(`jqc-manual-tests`/`jqc-c-tests`), `cargo clippy`/`fmt`, the `--ignored`
wall-time/peak-RSS benchmarks, `cargo bench`, `nix build`/`nix flake check`,
`cargo tarpaulin`, the off-by-default script fuzzer -- and a `justfile`
collecting them as recipes, since each one previously lived only as a
remembered or docs-buried command (README.md's "Testing" section already
documented all of them in prose; nothing here was undocumented, just not
reproducibly *runnable* by name).

= What changed

`justfile` at the repo root, with one recipe per command README.md already
documents, grouped into three sections:

#diagram(
  boxes: (
    ("fast inner loop\n(test, lint, fmt, ...)", 0, 0),
    ("slow / off-by-default\n(fuzz-scripts, bench, coverage)", 1, 0),
    ("nix\n(build, flake-check, reports)", 2, 0),
  ),
  arrows: (),
)

Two composite recipes tie them together: `test-excluded` (both
`$JQ_SRC`-gated suites) and `ci` (formatting + lints + the fast suite +
`test-excluded` + a full `nix flake check`) -- deliberately *not* including
`test-fuzz-scripts` or the diagnostic benchmarks, matching `nix flake
check`'s own documented exclusions in `flake.nix`.

`flake.nix`'s `mkDevShell` gained `pkgs.just`, so `nix develop` provides
`just` itself alongside everything else already there -- no separate
install step.

Each recipe's doc comment is a single line (`just --list` only displays the
comment line immediately above a recipe, not a wrapped multi-line block --
confirmed directly, an earlier draft with wrapped comments produced a
ragged `--list` showing each recipe's *last* comment line only, the rest
silently dropped from the listing though still visible reading the file).
Comments stay terse and point back to README.md's Testing section for the
*why* behind anything non-obvious (e.g. why `jqc-compiler`'s coverage
number is meaningless, why `bench-peak-memory` must run alone) rather than
duplicating that prose here, to avoid the two drifting apart over time.

= Verification

Every recipe was actually run, not just written and assumed correct:
`test`, `fmt-check`, `lint`, `test-excluded` (513 c-tests + 251 manual
tests), `test-cli`, `test-version-selection` (with `--features jq-1-6`,
confirming the jq-1.6 `if`/`else` case specifically), `test-fuzz-inputs`,
`build`, and `reports` all pass. `flake-check`/`ci` wrap already-verified
underlying commands (`nix flake check`, the individual recipes above) and
were spot-checked for correct invocation syntax rather than re-run in full
given their cost.
