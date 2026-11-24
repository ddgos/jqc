#import "../template.typ": report, diagram

#show: report.with(title: "Codegen as a trait", date: "2026-11-24")

= Context

`todos.md` asked to make codegen a trait so multiple backends can
co-exist, with the current implementation becoming one of them. This is
groundwork for later tasks on the same list -- a multi-core backend and a
SIMD backend, each meant to be a separate implementation comparable against
the others rather than a rewrite of the existing one.

= What changed

`jqc-macros/src/codegen.rs` gained:

```rust
pub trait Codegen {
    fn compile_top(&self, expr: &Expr) -> CResult<TokenStream>;
}

pub struct TreeWalkCodegen;

impl Codegen for TreeWalkCodegen {
    fn compile_top(&self, expr: &Expr) -> CResult<TokenStream> {
        compile_top(expr)  // unchanged, pre-existing free function
    }
}
```

"Tree-walk" names the actual strategy -- a direct recursive descent over
the lowered AST that emits nested Rust loops over `Vec<Value>` per node,
mirroring jq's cartesian-product generator semantics directly in the
generated control flow -- to distinguish it from what a SIMD or multi-core
backend would do structurally differently, not just "the default one."

`jqc-macros/src/lib.rs`'s `jq!` entry point now goes through the trait:

```rust
TreeWalkCodegen.compile_top(&ast)
```

instead of calling `codegen::compile_top` directly.

#diagram(
  boxes: (("lower() AST", 0, 0), ("Codegen trait", 1, 0), ("TreeWalkCodegen", 2, 0), ("TokenStream", 3, 0)),
  arrows: ((0, 1), (1, 2), (2, 3)),
)

= Why this shape

The existing `compile_top`/`compile_raw`/`compile_*` free-function pipeline
is untouched -- it's still the concrete implementation, just reached
through the trait now instead of directly. `TreeWalkCodegen` is a
zero-sized unit struct today, but the trait method takes `&self`
specifically so a future backend can carry real configuration (SIMD lane
width, thread count, …) as struct fields without changing the trait's
shape. No backend-selection mechanism (a macro argument, a feature flag,
…) was added here -- `todos.md`'s multi-core task already specifies one
(a feature flag) for when that backend exists, and inventing a selection
API before there's a second implementation to select between would be
guessing at its shape.

= Verification

`cargo build/test/clippy/fmt --workspace` all pass, including the full
124-case differential suite and 15 fuzz-input cases -- `jq!`'s generated
code is unchanged, only how it's reached internally.
