#import "../template.typ": report, diagram

#show: report.with(title: "Nix tooling: jqc-build, flake check, fair benchmarks, multi-jq dev shell", date: "2026-11-24")

= Context

This batch closes out most of `todos.md`'s remaining Nix/tooling tasks,
several of which turned out to be interlocking: the flake already had a
dangling reference to a `jqc-build` binary that didn't exist yet (part of
why `nix build` failed), and building that binary for real is also what
the "compile a jq script file into a derivation" and "fair benchmark
against real jq" tasks both needed.

= `jqc-build`: compiling a script file, not just a macro call

New workspace member, `jqc-build/src/main.rs`. `jq!("...")` needs a
string literal at *someone's* compile time; `jqc-build <script.jq> [-o
<out>]` supplies that someone at its own *runtime*: it generates a
scratch Cargo project whose `main.rs` embeds the script text as a `jq!`
call (plus the same stdin/stdout NDJSON plumbing `examples/src/main.rs`
hand-writes for one fixed filter), runs `cargo build --release` on it,
and reports the resulting binary's path.

#diagram(
  boxes: (("script.jq", 0, 0), ("scratch crate\n(jq! embedded)", 1, 0), ("cargo build", 2, 0), ("standalone binary", 3, 0)),
  arrows: ((0, 1), (1, 2), (2, 3)),
)

Locating the `jqc` crate to depend on is the one wrinkle: at `cargo
run`/dev-shell time, `jqc-build`'s own `CARGO_MANIFEST_DIR` sibling
(`../jqc`) works fine, but a packaged, installed `jqc-build` runs long
after its own build sandbox is gone. `JQC_WORKSPACE_DIR` (checked first,
falling back to the manifest-dir trick) is set by the Nix package to
`self` -- the flake's own source, a permanent store path -- so this
works either way.

`nix run . -- <script-file>` (now the flake's default app) and `nix
build .#example-script` (a committed sample,
`jqc-build/examples/select-active.jq`, built via the `mkJqScript` Nix
function below) both verified end-to-end, producing a binary that runs
correctly on real input.

= `mkJqScript`: the Nix-function half of the same feature

`flake.nix` exposes `lib.mkJqScript = system: scriptFile: <derivation>`.
It builds `jqc-build` from source, then runs *it* (rather than letting
`buildRustPackage` build `src` itself) as a custom `buildPhase` to
compile the given script. The offline part is the subtle bit: `jqc-build`
shells out to its own nested `cargo build` for the scratch crate it
generates, and that needs to resolve `serde_json` et al. without network
access inside the Nix sandbox. Setting `cargoLock.lockFile = ./Cargo.lock`
on this derivation (the *main workspace's* lockfile, which already pins
those same versions) is enough: `cargoSetupHook` populates
`$CARGO_HOME`'s vendor directory and offline config before `buildPhase`
runs, and the nested `cargo build` inherits that environment. Verified
with `nix build .#example-script -L`: fully offline, no network calls,
correct output.

`packages.example-script` is the concrete, always-built proof this
works; it's also what `nix flake show`/`nix flake check` exercise.

= Fixed: `nix build` failing where `nix develop -c cargo test` didn't

Two independent bugs, both now fixed on `packages.default`:

- The differential suite's checkPhase (`cargo test`) shells out to `jq`
  as ground truth. The Nix *dev shell* provides `jq` on `PATH`; the
  sandboxed package *build* didn't. Fix: `nativeCheckInputs = [ pkgs.jq ];`.
- `postInstall` already tried to `wrapProgram $out/bin/jqc-build`, but
  no such binary existed anywhere in the workspace before this batch --
  apparently scaffolded ahead of writing the tool itself. Now that
  `jqc-build` exists, that `wrapProgram` succeeds, and additionally sets
  `JQC_WORKSPACE_DIR` (see above) and puts `cargo`/`rustc` on its `PATH`.

`nix build` now succeeds end-to-end (confirmed with `-L`, full checkPhase
log: differential/fuzz/version-selection/scaling-check suites, 125+ tests,
all passing inside the sandboxed build).

= `nix run .` is now `jqc-build`, not the `examples` demo

Per explicit direction: `examples`' `compiled-filter` (one hardcoded
illustrative filter) stays in the repo and in `packages.default`'s
`$out/bin`, but is no longer what bare `nix run .` means -- that's now
`jqc-build`, the general-purpose tool. The old behavior is still reachable
via `nix run .#default-example`.

= `nix flake check` now actually runs the test suite

Previously there was no `checks` output at all, so `nix flake check`
only *evaluated* `packages`/`apps` (type-checked the Nix expressions)
without ever building or running anything -- `README.md`'s "Testing"
section was aspirational as far as `nix flake check` went. Two checks
now:

- `checks.default = packages.default` -- its checkPhase already runs
  `cargo test --workspace` (differential, version\_selection, fuzz\_inputs,
  the `jqc-compiler` scaling-check regression guards, doc-tests).
- `checks.manual-tests` -- the one suite excluded from the main workspace
  (`jqc-manual-tests`, excluded because it needs `$JQ_SRC` at *compile*
  time, which a plain `--workspace` run can't assume). Builds it directly
  with `JQ_SRC = "${jq-src}"` and `nativeCheckInputs = [ pkgs.jq ]`.

Deliberately excluded: `fuzz_scripts -- --ignored` (README calls out as
slow/off-by-default by design) and the wall-time/peak-RSS benchmarks
under `--ignored` (diagnostics with no pass/fail assertion, not
correctness checks). `nix flake check -L` passes, building both.

= Dev shell: every pinned `jq` version at once, plus `gnuplot`

Previously, testing against 1.6/1.7/1.8 meant three separate `nix
develop` shells. `mkDevShell` now takes an `extraJqBins` list;
`devShells.default` passes in `jq-1.6`/`jq-1.7` wrapper scripts
(`pkgs.writeShellScriptBin`, built once via `mkVersionedJqBin`) alongside
the default `jq`, so one shell has all three on `PATH` simultaneously.
Verified: `jq --version` → 1.8.2, `jq-1.6 --version` → jq-1.6, `jq-1.7
--version` → jq-1.7.1, all in the same shell. The per-version devShells
(`.#jq-1_6`/`.#jq-1_7`, which swap the *default* `jq` name itself) stay,
for anything that specifically shells out to `jq` by that name.

`gnuplot` was added to every devShell per `todos.md`'s "consider" --
`plotters` (the other option floated) was deliberately not added as a
Rust dependency: there's only one `Codegen` implementation
(`tree_walk`) today, so `benchmarks/pareto-front.csv` has nothing
multi-dimensional to plot yet (see the pareto-front-tracking report);
worth reconsidering once a second implementation exists and gnuplot
proves insufficient.

= `cargo-criterion` in the dev shell

`pkgs.cargo-criterion` added to `mkDevShell`'s package list, so `cargo
criterion -p jqc` works (criterion's own harness supports `cargo bench`
already; `cargo-criterion` adds its nicer CLI/HTML reports on top).

= Investigated, no fix needed: math function availability

`todos.md` worried nixpkgs-unstable's `jq` might not provide every math
function the manual documents. Checked exhaustively against the manual's
own three lists (one-input C math functions, two-input, three-input --
`docs/content/manual/manual.yml`'s "Math" section) via `jq -n builtins`:
every single one is present and correctly arity-matched on this `jq`
(1.8.2, glibc/Linux) -- including the easy-to-miss ones (`gamma`, `j0`/
`j1`/`y0`/`y1` Bessel functions, `lgamma_r`, `erf`/`erfc`). `frexp`/`modf`
initially looked missing under a naive check because the manual
miscategorizes them as two-input when they're actually `/0` in jq's own
implementation (returning a `[mantissa, exponent]`-style pair) -- not an
actual gap, an upstream manual quirk. No override needed; removed from
`todos.md`.

= Investigated, decision recorded: rust-overlay / flake-utils / crane

`todos.md` asked to "consider" `oxalica/rust-overlay`, `numtide/flake-utils`,
and `ipetkov/crane`. Decision: not adopted, for now.

- `flake-utils`: `forAllSystems` is already the one thing this flake
  would use it for, and it's three lines.
- `rust-overlay`: would let pinning an exact rustc/component set, but
  nixpkgs' `rustc`/`cargo`/`clippy`/`rustfmt` already work together fine
  here, and the project doesn't need a toolchain version nixpkgs-unstable
  lacks.
- `crane`: its main win is a separate, cached dependency-only build layer
  for faster incremental/CI rebuilds. There's no CI pipeline in this repo
  today, and local rebuilds are already fast; revisit if either changes.

= Investigated, decision confirmed: old jq versions via separate nixpkgs
  inputs, not an overridden `version` attribute

`todos.md` floated overriding the nixpkgs `jq` package's `version`
attribute directly instead of pulling in separate pinned `nixpkgs-jq-1-6`/
`nixpkgs-jq-1-7` flake inputs (which already existed going into this
batch). Confirmed that's not viable as stated: nixpkgs' `jq` derivation
pins an exact `src`/hash alongside `version` -- overriding just the
version string without the matching source would either rebuild the same
code under a misleading label or fail outright, since Nix derivations
are content-addressed by their actual inputs, not a semantic version
field. Separate pinned nixpkgs channel inputs (each with version/src/hash
already correctly paired upstream) is the idiomatic way to get genuinely
different jq builds reproducibly; kept as-is.

= Investigated, already satisfied: `inputs.nixpkgs.follows`

Any flake with a named top-level input (`nixpkgs`, as this one already
has) can already be overridden by a consumer via `inputs.jqc.inputs.
nixpkgs.follows = "nixpkgs";` in *their* flake -- nothing additional
needs implementing on this side. (`typix.inputs.nixpkgs.follows =
"nixpkgs"` was already set, so this flake's own `typix` dependency
doesn't pull a second nixpkgs either.) The two version-pinned inputs
(`nixpkgs-jq-1-6`/`nixpkgs-jq-1-7`) deliberately don't follow the main
`nixpkgs` -- that would defeat their purpose of testing against
intentionally-older jq releases.

= Audited: work assuming the Nix dev shell

Checked for CI configs, shell scripts, or a pinned `rust-toolchain` file
that might run `cargo`/`jq` outside `nix develop` -- found none (no
`.github`, no loose shell scripts, no `rust-toolchain*`). The one real
instance found was the `nix build` checkPhase gap above (a sandboxed
package build, not a dev-shell session, silently relying on `jq` being
on `PATH` the way the dev shell happens to provide) -- now fixed.

= Fair `jqc` vs. real `jq` benchmarks

`todos.md` flagged the existing `jq_cold`/`jq_warm` benchmark series as
potentially unfair: `jqc`'s own series runs in-process, while `jq_cold`/
`jq_warm` pay for process spawn and text stdin/stdout -- not an
apples-to-apples comparison, which the code already said explicitly in
`run_jq`'s doc comment. Rather than removing it, this adds the fair
counterpart the comment was waiting for: `jqc_cold`/`jqc_warm`, which run
a `jqc-build`-compiled binary of the *same* filter through the identical
`spawn_pipe` plumbing.

#diagram(
  boxes: (("jqc (in-process)", 0, 0), ("jq\_cold/warm", 1, 0), ("jqc\_cold/warm", 2, 0)),
  arrows: (),
)

`jqc` vs. `jqc_cold`/`jqc_warm` now isolates what in-process-vs-subprocess
packaging alone costs for the same generated code; `jqc_cold`/`jqc_warm`
vs. `jq_cold`/`jq_warm` is the genuinely fair comparison, both sides
paying for spawn + text I/O. `jqc_binary_for` compiles each distinct
filter once (cached per process) rather than per sample.
`cargo bench -p jqc --bench compiled_filters -- --test` (criterion's
single-sample smoke mode): 66/66 points succeed, including every new
`jqc_cold`/`jqc_warm` point, with no panics.

= Verification summary

`cargo build/test/clippy --workspace`, `nix build`, `nix build
.#example-script`, `nix run . -- <script>`, `nix run .#default-example`,
`nix flake check -L`, and the benchmark smoke test (above) were all run
directly as part of this batch, not just planned -- see each section
above for what each one confirmed.

= What's left

`todos.md` keeps: known limitations (perpetual, fix what's reasonable),
fuzzing improvements (arbitrary UTF-8 scripts/inputs, broader proptest
coverage), ongoing code tidying, generating test cases from jq's own C
test suite (not just the manual, which `jqc-manual-tests` already
covers), and SIMD/further-optimization codegen backends -- all
explicitly open-ended per their own wording ("do not remove until...").
