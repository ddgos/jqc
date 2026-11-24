#import "../template.typ": report, diagram

#show: report.with(title: "External-crate audit: three swaps, one real bug found", date: "2026-11-24")

= Context

`todos.md` asked for an audit of the codebase for places an external
crate would genuinely simplify things, following on from the previous
task's own back-and-forth on this question (hand-rolled SIMD and
`memchr` both losing to Rust's own standard library there -- a reminder
that "use a crate" isn't automatically a win, only a thing worth
checking). This pass surveyed `jqc-compiler`, `jqc-cli`, `jqc-build`,
and `jqc-core` for hand-rolled code duplicating well-established crate
functionality, implemented the clear wins, and documents the rest.

= Implemented

*Levenshtein distance* (`jqc-compiler/src/diagnostics.rs`, "did you mean
...?" suggestions): a 16-line hand-rolled DP table replaced with
`strsim::levenshtein`. Textbook Levenshtein has no jq-specific behavior
to get right -- there was nothing project-specific being reinvented, so
nothing lost by not reinventing it.

*CLI argument parsing* (`jqc-cli/src/lib.rs`'s `Args::parse`): the
argv-splitting mechanics (combined short flags like `-nc`, `--long
value`/`--long=value`, `--` ending flag parsing, `-` as a plain
positional rather than an empty flag cluster) are now `lexopt` -- a
minimal, unopinionated argv tokenizer -- instead of hand-rolled string
matching. jq's own *flag semantics* (which flag means what) are
unchanged and still jqc's own code; only the "how do argv tokens even
get split into flags/values/positionals" layer, which has no jq-specific
content, moved to the crate. Verified against jqc-cli's own differential
suite (unaffected -- this only touches parsing, not formatting) and a
fresh end-to-end check of a real `jqc-build`-compiled binary across
combined short flags, `--`, `-s`, `-R`, and an unknown-flag error.

*Scratch-directory lifecycle* (`jqc-build/src/main.rs`): replaced a
hand-rolled `env::temp_dir().join(format!("jqc-build-{name}-{pid}"))`
plus a single `fs::remove_dir_all(&scratch).ok()` at the very end with
`tempfile::TempDir`. This one surfaced an actual, confirmed bug along
the way, not just a style improvement.

= The bug `tempfile` surfaced

Every early-exit path in `jqc-build`'s `main` -- cargo failing to even
spawn, a failed build, a failed copy -- called `std::process::exit`
directly. `std::process::exit` is documented to skip `Drop` entirely,
so none of those paths ever reached the cleanup call at the bottom of
`main`: a build *failure* (plausibly the single most common failure
mode of all) silently leaked the entire scratch Cargo project --
`Cargo.toml`, `src/`, and a full `target/` of built dependencies --
every time.

#diagram(
  boxes: (("cargo build\nfails", 0, 0), ("process::exit\n(code)", 1, 0), ("Drop skipped:\nscratch dir leaks", 2, 0)),
  arrows: ((0, 1), (1, 2)),
)

Confirmed concretely, not just in theory: `/tmp` on this machine had
~650MB across 17 leaked `jqc-build-<name>-<pid>` directories from real
prior use (predating this session), matching the bug's exact signature.
Left in place at the user's own call -- not this task's to clean up --
but it's the clearest evidence the bug was real, not hypothetical.

Swapping to `tempfile::TempDir` alone wouldn't have fixed this on its
own: `TempDir`'s `Drop` *also* doesn't run across `std::process::exit`,
for the same documented reason. The actual fix is `fn main() ->
std::process::ExitCode`, with every exit path now a normal `return`
instead of a direct call to `std::process::exit` -- which does let
`main`'s locals (`scratch_dir` included) drop normally. Confirmed by
reproducing the leak and the fix side by side: a filter that fails to
compile (a 0-arg-only recursive-`def`-with-params restriction violation)
now cleans up its scratch directory on exit; before the fix, that exact
case was exactly what leaked.

= Surveyed, not implemented

*JSON stringification* (`jqc-core/src/simd.rs`'s `write_jq_json`/
`write_jq_json_string`, and separately `jqc-cli`'s own `write_term`):
both hand-walk a `Value` tree with a `match`, matching jq's own
escaping/number-formatting rules by hand. `serde_json::ser::Formatter`
is specifically designed for exactly this kind of customization (it's
how `PrettyFormatter` itself works) and could plausibly unify both --
jqc-core's compact-only stringifier and jqc-cli's full pretty/color/
sort-keys printer -- into one shared implementation, removing the
duplication between them that already caused one real bug this session
(jqc-core's stringifier missing escaping/exponent-case fixes already
made in jqc-cli's). Not attempted here: `sort_keys` needs output order
different from the `Value`'s own iteration order, which a `Formatter`'s
hooks don't control (the `Serializer` iterates in whatever order
`Value`'s own `Serialize` impl does) -- reconciling that cleanly, across
two recently-stabilized and differentially-tested components, is a
real redesign, not a drop-in swap.

*Diagnostic rendering* (`jqc-compiler/src/diagnostics.rs`'s `render`/
`render_span`): hand-rolled rustc-style `--> line:col` + gutter +
caret-underline output is exactly what `codespan-reporting`/`ariadne`
exist for, and would add color for free (the current renderer is plain
text). Not swapped: `Diagnostic`'s `shift`/`relocate` machinery (for
translating a span from "offset in `prelude + user_src`" back to
"offset in `user_src` alone") is genuinely project-specific and would
stay regardless of what renders the final output; the rendered *text
itself* is also user-visible, embedded verbatim in `jq!`'s own macro
compile-errors, so changing its format is a visible product change
needing explicit buy-in, not an invisible internal refactor.

*Lexer/parser* (`jqc-compiler/src/lexer.rs`/`parser.rs`): surveyed
against parser-combinator crates (`chumsky`/`winnow`/`nom`) and a
derive-based lexer (`logos`). Not recommended: both are deeply
intertwined with custom span-tracking and "did you mean" diagnostics
already tuned against real jq's own error messages, both are correct
and already covered by the differential/manual-tests/c-tests suites,
and a rewrite's risk (reintroducing subtle parsing bugs in a
well-exercised 950+362-line hand-written pair) isn't clearly repaid by
a external-crate win the way the three implemented swaps above were.

= Verification

`cargo build/test/clippy --workspace` (all suites, including
`jqc-cli`'s differential suite and `jqc-compiler`'s "did you mean"
tests), `cargo fmt --check --all` (0 diffs), `nix build .#default` and
`.#example-script`, and a manual end-to-end pass of a real `jqc-build`-
compiled binary (combined short flags, `--`, slurp, raw-input, unknown-
flag error) all pass.

= What's left

This task stays open, per its own framing -- an audit is naturally
ongoing, not a one-shot completion, the same as "known limitations" and
"tidy up." The two surveyed-but-not-implemented candidates above
(JSON-stringification unification via `serde_json::ser::Formatter`,
diagnostic rendering via `codespan-reporting`/`ariadne`) are documented
here specifically so a future pass doesn't have to re-derive the same
trade-offs from scratch.
