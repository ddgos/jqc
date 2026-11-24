#import "../template.typ": report, diagram

#show: report.with(title: "AST / lowering refactor", date: "2026-11-24")

= Context

`todos.md` asked to finish up an in-progress refactor and commit it so the
working tree stays clean. On arrival, every crate but `jqc-core`'s public
surface had uncommitted changes touching the AST, lexer, parser, lowering
pass, and codegen.

= What the refactor changed

- `Expr`'s positional-tuple variants (`Field(Box<Expr>, String, bool)`,
  `Index(Box<Expr>, Box<Expr>, bool)`, `Slice(..)`, `IterateAll(..)`,
  `Reduce(..)`, `Foreach(..)`) became named-field struct variants
  (`Field { base, name, optional }`, etc.) across `ast.rs`, `parser.rs`,
  `lower.rs`, and `codegen.rs` — the same information, but call sites read
  without cross-referencing the variant's field order.
- `def` parameters went from `Vec<String>` (with a separate `ParamKind`
  enum reconstructed by sniffing a leading `$`) to a single `Param { name,
  is_value }` struct computed once at parse time and carried through.
- `jqc-core/src/helpers.rs`'s binary-operator helpers were renamed from
  `a`/`b` to `lhs`/`rhs` for readability; the unused `Filter` type alias in
  `jqc-core/src/lib.rs` was dropped.
- A same-crate benchmark harness (`jqc-macros/src/bench.rs`, run via
  `cargo test -p jqc-macros --release -- --ignored <name> --nocapture`)
  was added to measure macro-expansion-time cost directly, since a
  proc-macro crate can't be the target of an external `benches/`. It
  surfaced a real cost cliff: chains of non-recursive defs that each use
  the previous def twice blow up worse than the expected `2^depth`, because
  `lower_funcdef` clones the (recursively-nested) `def_scope` at every
  level. See @scaling.
- `differential.rs` gained two cases (`index_multivalued`,
  `slice_multivalued`) covering multi-valued index/slice expressions
  (e.g. `.[(0,1):(2,3)]`), exercised incidentally by the `Index`/`Slice`
  restructuring.

= Pipeline (unchanged shape, renamed internals)

#diagram(
  boxes: (("lexer", 0, 0), ("parser", 1, 0), ("lower", 2, 0), ("codegen", 3, 0)),
  arrows: ((0, 1), (1, 2), (2, 3)),
)

= Verification <scaling>

- `cargo build --workspace`, `cargo test --workspace` (139 differential +
  fuzz cases), `cargo clippy --workspace --all-targets`, and `cargo fmt
  --check` all pass clean.
- The compile-time benchmarks are `#[ignore]`d by default and capped at a
  depth that stays under a few GB of RSS — raising the depth without the
  memory safeguard added separately in `todos.md` can exhaust 30GB+ of RAM,
  per the comment left in `bench.rs`.
