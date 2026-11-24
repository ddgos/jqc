#import "../template.typ": report, diagram

#show: report.with(title: "Tidy-up pass: fmt drift and codegen call duplication", date: "2026-11-24")

= Context

`todos.md` asks to find an area of the code to tidy up, kept open until
the code is "as polished as possible" -- an ongoing task, not a one-shot
completion, the same framing as "known limitations." This pass covered
two things: a backlog of pre-existing `rustfmt` drift, and real
duplication in `jqc-compiler/src/codegen.rs`'s builtin-call dispatch.

= `rustfmt` drift: 28 lines across 7 files

`cargo fmt --check --all` had been quietly failing on `jqc-compiler`
(`codegen.rs`/`diagnostics.rs`/`lexer.rs`/`lower.rs`/`parser.rs`),
`jqc-build/src/main.rs`, and `jqc-macros/src/lib.rs` for a while --
confirmed pre-existing (present at `HEAD` before this task, not
introduced by recent work) across three separate tasks earlier in this
session, each of which only spot-checked the same couple of trailing
diff blocks a truncated `tail -40` happened to show, rather than the
full list. A plain `cargo fmt --all` fixed all 28 lines (pure
whitespace/line-wrap, confirmed via `git diff` before committing --
nothing behavioral).

= Duplicated builtin-call dispatch in `codegen.rs`

`compile_call`'s `(name, arity)` match had seven near-identical arms for
`has`/`startswith`/`endswith`/`ltrimstr`/`rtrimstr`/`join`/`split`, each
five lines of boilerplate differing only in which `jqc-core` function
name to call:

```rust
("startswith", 1) => {
    let f = core_path("startswith");
    cartesian_args(args, input, scope, |a| {
        let arg = &a[0];
        quote!(#f(#input, &#arg))
    })
}
```

Factored into one `compile_core_fn2(fn_name, args, input, scope)` helper
(mirroring the existing `simple_unary` helper's own role for 0-arg
builtins), each call site collapsing to one line:

#diagram(
  boxes: (("7 repeated\n5-line arms", 0, 0), ("compile_core_fn2\nhelper", 1, 0), ("7 one-line\ncall sites", 2, 0)),
  arrows: ((0, 1), (1, 2)),
)

`contains` stayed separate (`contains_value` returns `JResult<bool>`,
needing an extra `.map(Value::Bool)` the other six don't). `first(f)`/
`last(f)`'s two near-identical bodies (differing only in `.next()` vs.
`.last()`) got the same treatment, via a `method: &str` parameter spliced
in as an `Ident`. `codegen.rs` as a whole: 78 lines removed, 62 added (a
net -16 lines) with no behavior change.

= Verification

`cargo build/test/clippy --workspace`, `cargo fmt --check --all` (0
diffs, down from 28), and the differential/manual-tests/c-tests suites
all pass with unchanged pass/ignore counts -- confirming the refactor is
observably a no-op, as a pure dispatch-table consolidation should be.

= What's left

This task stays open, per its own framing. Candidates noticed but not
pursued this pass, for next time: `codegen.rs` is still the largest file
in the workspace (1187 lines) and has more copy-pasted shape in its
`BinOp`/`compile_raw` match arms that could likely collapse similarly;
`jqc-core/src/helpers.rs` (999 lines) wasn't looked at structurally at
all this pass, only edited piecemeal across the last few tasks' bug
fixes.
