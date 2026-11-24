#import "../template.typ": report, diagram

#show: report.with(title: "jq version selection", date: "2026-11-24")

= Context

`todos.md` asked for a way to specify which `jq` version `jq!` targets.
This went through two revisions before landing:

1. First pass: since `jq!` is a from-scratch implementation (never calls
   a real `jq` binary), "version" seemed only meaningful for the
   differential test suite's ground-truth binary -- so that pass pinned
   old `jq` binaries reproducibly via nix for testing against.
2. Clarified intent: the actual goal is resilience against *upstream jq
   breaking changes* -- being able to interpret a given script as if it
   targeted any particular jq version, so a script written against an
   older jq keeps compiling even if jq's language has since changed
   underneath it. That needs `jq!`'s own parsing/lowering to branch on a
   selected version, with support for each version gated by a Cargo
   feature.
3. Final shape: version selection is a `jq!` macro argument
   (`jq!(version = "1.6", "...")`), not a whole-build setting -- different
   call sites can reasonably target different versions. The Cargo
   features control which versions are *available to select* at all.

The nix work from pass 1 wasn't wasted: pinned real jq binaries are
exactly what's needed to verify a version-gated behavior is actually
correct for that version, which is how the one concrete difference
below was found and confirmed.

= The feature: `jq!(version = "X.Y", "...")`

```rust
let f = jq!(version = "1.6", "if .a then 1 else 2 end");  // jq 1.6 semantics
let g = jq!("if .a then 1 end");                          // newest enabled (default: jq-1-8)
```

Cargo features (`jq-1-6`, `jq-1-7`, `jq-1-8`, on `jqc`/`jqc-macros`/
`jqc-compiler`, forwarding down that chain) control which versions are
*compiled in* and thus nameable; `jq-1-8` is on by default. They're
additive, not mutually exclusive -- a build can support several versions
at once, since different call sites may want different ones:

#diagram(
  boxes: (("jq!(version=\"1.6\", ...)", 0, 0), ("feature jq-1-6\nenabled?", 1, 0), ("compile with\n1.6 semantics", 2, 0), ("compile error", 2, 1)),
  arrows: ((0, 1), (1, 2), (1, 3)),
)

Naming a version whose feature isn't enabled -- even for a filter that
would otherwise be perfectly valid under that version -- is a clear
compile error pointing at the macro call, not a silent fallback.

= Architectural prerequisite: extracting `jqc-compiler`

Implementing this meant the parser needed to branch on a per-call
version, which meant the version-aware pipeline needed to be callable
directly (to validate behavior and write tests) rather than only through
the opaque `jq!` macro. `jqc-macros` was a `proc-macro = true` crate,
which Rust restricts to exporting *only* proc-macro items to other
crates -- so its internals (`ast`, `lexer`, `parser`, `lower`, `codegen`)
were moved into a new plain library crate, `jqc-compiler`. `jqc-macros`
is now a thin wrapper: it parses `jq!`'s own argument grammar (optional
`version = "X.Y",` then the filter literal) and calls
`jqc_compiler::lower::parse_and_lower(src, version)` +
`TreeWalkCodegen.compile_top(&ast)` directly. This was a mechanical move,
not a logic change -- the pipeline's behavior is unchanged except for the
new version parameter threaded through `parse_program`/`Parser`.

#diagram(
  boxes: (("jqc (facade)", 0, 0), ("jqc-macros\n(proc-macro)", 1, 0), ("jqc-compiler\n(plain lib)", 2, 0)),
  arrows: ((0, 1), (1, 2)),
)

= The one concrete version difference implemented

`jqc-compiler/src/version.rs` centralizes version-gated behavior as
methods on a `JqVersion` enum, so future differences get added there
rather than as scattered `cfg!` checks. Today there's one:
`JqVersion::if_requires_else` -- jq 1.6 requires an explicit `else` in
`if`/`then`/`end`; support for omitting it was added in jq 1.7. Verified
against the real pinned binaries from pass 1 of this task:

```
$ nix develop .#jq-1_6 -c jq 'if .a then 1 end' <<< '{"a":false}'
jq: error: syntax error, unexpected end...
jq: error: Possibly unterminated 'if' statement...

$ nix develop .#jq-1_7 -c jq 'if .a then 1 end' <<< '{"a":false}'
{ "a": false }
```

`jqc-compiler/src/parser.rs`'s `parse_if` now returns a clear error for
this case when `version.if_requires_else()` is true and no `else` was
written.

= Verification

Manually confirmed via a scratch crate (not part of the workspace, since
these are macro-expansion-time compile errors/successes that can't share
one test binary):

- `jq!(version = "1.8", "if .a then 1 end")` compiles and runs correctly.
- `jq!(version = "1.6", "if .a then 1 end")` (no `else`) -- clear compile
  error citing the jq 1.6 requirement, pointing at the filter literal.
- `jq!(version = "1.6", "if .a then 1 else 2 end")` (has `else`) compiles
  and runs correctly when the `jq-1-6` feature is enabled.
- The same call with `jq-1-6` *not* enabled -- clear compile error citing
  the missing feature, pointing at the `version` argument -- even though
  the filter itself is valid under 1.6.

Permanent tests in `jqc/tests/version_selection.rs` cover the two
compiling cases (the second gated on `#[cfg(feature = "jq-1-6")]`); the
two rejection cases aren't automated since they're compile-time failures
and this project has no compile-fail harness (trybuild or similar) for
any of `jq!`'s other diagnostics either.

`cargo build/test/clippy/fmt --workspace` pass, as does `--all-features`
(ensuring `jq-1-6`/`jq-1-7`/`jq-1-8` enabled simultaneously don't
conflict); the full differential/fuzz suites still pass unchanged
(124 + 15 cases), confirming the refactor didn't alter default behavior.
