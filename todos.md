# Todo

## Context

For each of the items in this document, produce a typst document about what was
done, including diagrams if applicable. Remove tasks from this list once they
are complete. Add tasks to this list if necessary. Keep changes tracked well in
git. Do these in whichever order makes the most sense.

## Tasks

Make the change from eager evaluation to lazy evaluation. Being 100% compliant
with real `jq` is a goal of this project.

STATUS (in progress, mid-session checkpoint): the core rewrite is done and
working -- `jqc-compiler/src/codegen.rs` now compiles every node to a
`jqc_core::JGen` (`Box<dyn Iterator<Item = JResult<Value>>>`) instead of
`JResult<Vec<Value>>`, via new combinators in `jqc-core/src/gen.rs`
(`flat_map_gen`, `lazy_chain`, `catch_gen`, `alt_gen`, `collect_gen`,
`ForeachGen`). `JqError::emitted`/`bubble`/`bubble1`/`partial_for` are deleted
entirely (superseded by genuine laziness). The headline bug is fixed and
manually verified: `[limit(3; while(true;.))]` now returns `[true,true,true]`
instead of stack-overflowing, same for `first(repeat(1))` and a *user-written*
`foreach` over an unbounded source piped into `limit` from outside (not just
`limit`'s own internal use of `foreach`) -- `ForeachGen` is a real external
lazy generator, not "drain internally then re-wrap". The full existing suite
(`cargo test --workspace`, 161+28+16+... cases) passes unchanged, as does
`cargo clippy --workspace --all-targets` (0 warnings) and `cargo fmt --check`.

The hard part turned out to be a `'static`/lifetime-and-move minefield, not
the lazy-evaluation design itself: every `Box<dyn FnMut(...) -> JGen + 'static>`
closure can only capture *owned* values, and a value shared by two sibling
sub-expressions (or by an eager argument and something nested inside the
closure it builds) gets "used up" the first time one closure captures it --
breaking the *other* sibling, or breaking the closure's own repeatability if
it's itself called more than once. Two general fixes came out of this,
documented with full reasoning in `jqc-compiler/src/codegen.rs` itself:
`snapshot_scope`/`compile_isolated` (every child of a multi-child node gets
its own fresh clone of the ambient input + every in-scope variable), and,
for nodes that nest `'static` closures *within* `'static` closures three or
more levels deep (`compile_slice`'s `from`/`to`/`base`, `compile_range`'s
`from`/`to`/`step`, `compile_if`'s `elif` chain), an explicit per-level
re-clone threaded through the recursion (`in_from`/`in_to`-style local
names) -- reusing one clone across more than one nested closure boundary
silently compiles fine right up until a second call proves it unsound, so
this needed several rounds of writing a filter, dumping its real macro
expansion (temporarily, via `JQC_DEBUG_EXPAND_FULL`, since reverted) into a
standalone crate, and reading the *exact* rustc E0507/E0521/E0382 span to
find which specific shared value needed another level of cloning.

Remaining before this task can be marked done:
- No new *permanent* tests yet for the fixed cases -- add to
  `jqc/tests/differential.rs`: `[limit(3; while(true;.))]`,
  `[limit(3; repeat(true))]`, `first(repeat(1))`, and
  `[limit(3; foreach repeat(1) as $x (0; .+1; $x))]` (the user-written-foreach-
  piped-into-limit case specifically, since that's what proves `ForeachGen`
  is genuinely externally lazy and not just internally-lazy-but-collected).
- `README.md`'s "Known limitations" bullet ("Eager, not lazy...") needs
  updating/removing now that it's no longer true.
- `nix build .#default .#example-script
  .#checks.x86_64-linux.{manual-tests,c-tests}` haven't been re-run since the
  rewrite (should still pass -- nothing about the nix-specific build paths
  changed -- but unverified this session).
- Write `todo-reports/eager-to-lazy/report.typ` (same template/pattern as
  every other entry in `todo-reports/`) documenting the design, the
  `'static`/move-conflict debugging story above (genuinely the most
  interesting part), and verification results; build its PDF via `nix run
  .#build-reports`; only then remove this task from this list.
- One known, deliberately-undone latent gap, left as a code comment at its
  one call site (`cartesian_args` in `jqc-compiler/src/codegen.rs`): N-ary
  builtin calls with 3+ arguments would need the same per-level re-clone
  treatment for their *own* generator-bound names (`__a0`, `__a1`, …) that
  `compile_range`'s `__from`/`__to` got, but every current builtin routed
  through `cartesian_args` takes at most 1 argument, so it's untested and
  unfixed -- would need addressing (or at least a differential test proving
  it either works or doesn't) before adding any 2+-arg builtin through that
  path.

Audit the current usage of serde. Is it a good fit for the project? This project
is only concerned with json data, so using a specialist crate might provide
advantages. Futhermore, some of the default behaviour provided by serde may not
be a good match for this project, making the code rough around the edges.

Work on the known limitations. Fix any which can reasonably be fixed. Only
remove this task once limitations that are genuine limitations of the project
remain. If at any point new bugs are identified, they should be isolated with
xfail tests, then fixed as part of this task, rather than fixed immediately.

Improve upon the script and input fuzzing. Part of this should be using
arbitrary utf-8 strings for the scripts and input. Part of this should be to
work up the proptest generators until every valid script and input will in
theory be tested. Do not stop until both of these are true. Also test
arbitrary bytes (not just arbitrary valid utf-8) as input -- piped data isn't
guaranteed to be valid utf-8, so differences from real `jq` on invalid-utf-8
input could currently be going unnoticed.

Identify some area of the code that could be tidied up, and refactor as desired.
An example of this would be seemingly duplicate functions for `fn run_real_jq`.
Do not remove this task until the code is as polished as possible.

Persue whatever optimisations possible. Always keep as separate implementations
of the trait so that comparisons between implementations are easier to make.

Investigate the feasibility (and implement if it makes sense) for jqc-build, or
the jpc-build flake application to build the resulting binary into the nix store
so that I can run `"$(nix build <flake> <script>)"` and it would run.

Audit the existing code for situations where an external crate may allow for
simplifying this project.
The most downloaded crates will be good candidates
https://crates.io/crates?sort=downloads

Consider using a different allocator as per
https://docs.rs/simd-json/latest/simd_json/#allocator

Audit all the ways this project can be tested (cargo test --workspace, the
nix-gated jqc-manual-tests/jqc-c-tests suites, cargo clippy/fmt, the ignored
--release benchmark/scaling tests, cargo bench, nix build/flake check, the
cargo-tarpaulin coverage setup, the ultrareview-style fuzz --ignored targets,
...), and create a justfile collecting them as recipes, to make testing more
reproducible (right now each of these lives only as a remembered or
docs-buried command).
