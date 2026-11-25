#import "../template.typ": report, diagram

#show: report.with(title: "Codegen backends via feature flags", date: "2026-11-25")

= Context

`todos.md` asked for `Codegen` backends to be selectable via Cargo
features, "similar to the supported versions implementation"
(`JqVersion`/`jq-1-6`/`jq-1-7`/`jq-1-8`) -- so a user sure they only want
one backend isn't forced to compile the other's code. Two backends exist
today: `TreeWalkCodegen` (the default tree-walk pipeline) and
`SimdCodegen` (`jq!(codegen = "simd", ...)`, added by the `simd-codegen`
todo-report), selected dynamically at macro-expansion time via a
thread-local `Backend` flag -- never gated behind a feature.

= Asymmetry with `JqVersion`, and why only `simd` gets a real feature

`JqVersion`'s three variants are genuinely symmetric: each is an
independent, mutually-non-exclusive set of parser/lowering behaviors, so
three real features (`jq-1-6`/`jq-1-7`/`jq-1-8`) make sense. Codegen
backends aren't symmetric the same way: `SimdCodegen::compile_top` calls
the *same* free-function tree-walk pipeline `TreeWalkCodegen` does --
`compile_top`/`compile_raw`/`compile_call`'s ~100 recursive call sites --
and differs only at three builtin dispatch points
(`ascii_downcase`/`ascii_upcase`/`contains`), where it routes to
`jqc-core::simd`'s `memchr`-backed implementations instead of the plain
scalar ones. There's no separable "treewalk-only" code to exclude:
`Simd` is additive on top of `TreeWalk`, not a parallel implementation of
equal size.

So `codegen-simd` is the only feature that gates anything real:

- `jqc-core`: `memchr` becomes an optional dependency; the `simd` module
  (the actual SIMD-accelerated builtins) is `#[cfg(feature =
  "codegen-simd")]`.
- `jqc-compiler`: `SimdCodegen` itself, the `Simd` variant of the
  internal `Backend` dispatch enum, and the `Simd` arm of
  `backend_fn_name` are all `#[cfg(feature = "codegen-simd")]`.
- `jqc-macros`: naming `"simd"` in `jq!(codegen = "simd", ...)` without
  the feature is a compile error, mirroring `JqVersion`'s "valid but not
  enabled" diagnostic exactly.

`"treewalk"` has no feature of its own -- it's unconditionally
available, since it's the pipeline `simd` itself is built on.

#diagram(
  boxes: (("jq!(codegen=\"simd\")", 0, 0), ("CodegenBackend::\nis_available()", 1, 0), ("SimdCodegen\n(jqc-compiler)", 2, 0), ("jqc-core::simd\n+ memchr", 3, 0)),
  arrows: ((0, 1), (1, 2), (2, 3)),
)

= `CodegenBackend`: mirroring `JqVersion`'s shape

`jqc-compiler/src/codegen.rs` gained an enum that mirrors `JqVersion`'s
split between "is this a real name" and "is it compiled into this
build":

```rust
pub enum CodegenBackend { TreeWalk, Simd }

impl CodegenBackend {
    pub fn parse(s: &str) -> Option<Self> { /* "treewalk" | "simd" */ }
    pub fn is_available(self) -> bool {
        match self {
            Self::TreeWalk => true,
            Self::Simd => cfg!(feature = "codegen-simd"),
        }
    }
    pub fn to_codegen(self) -> Box<dyn Codegen> { /* ... */ }
}
```

`jqc-macros/src/lib.rs`'s `jq!` entry point now goes through this enum
instead of matching the argument string directly, giving the same two-
tier error `version` already has: an unknown name (`jq!(codegen =
"gpu", ...)`) is "unknown codegen backend", while a real name whose
feature is off (`jq!(codegen = "simd", ...)` without `codegen-simd`) is
"valid but not enabled -- add feature \"codegen-simd\"", pointing at the
argument either way.

= A plumbing fix needed to make the feature actually optional

Mid-task, confirming the feature actually excludes `memchr` surfaced
that `jqc`'s and `jqc-macros`' own `[dependencies]` edges to `jqc-core`/
`jqc-compiler` didn't set `default-features = false` -- meaning a
downstream crate disabling `jqc`'s `codegen-simd` feature wouldn't
actually have stopped `jqc-core`'s matching default feature (and thus
`memchr`) from being pulled in anyway, since Cargo activates a
dependency's default features per-edge unless that edge says otherwise.
This affected the *existing* `jq-1-6`/`jq-1-7`/`jq-1-8` forwarding the
same way (latent, not previously exercised), so fixing it for
`codegen-simd` fixed it for `version` too:

```toml
# jqc/Cargo.toml, jqc-macros/Cargo.toml
jqc-core    = { path = "...", default-features = false }
jqc-macros  = { path = "...", default-features = false }
jqc-compiler = { path = "...", default-features = false }
```

with each crate's own `default = [...]` list re-forwarding what should
be on by default. Confirmed with a standalone scratch crate (outside the
workspace, its own lockfile) depending on `jqc` with `default-features =
false, features = ["jq-1-8"]`: `cargo tree -i jqc-core` now shows only
the `jq-1-8` feature activated (no `codegen-simd`), and `cargo tree -i
memchr` shows `memchr` pulled in solely via `serde_json`'s own unrelated
use of it, not via `jqc-core`'s optional dependency.

= Verification

- The scratch-crate check above, both directions: `codegen = "simd"`
  without the feature is a compile error naming the missing feature and
  pointing at the argument; with `codegen-simd` added back, the same
  call compiles and runs correctly (`Ok([String("hi")])` for
  `ascii_downcase` on `"Hi"`).
- `cargo build -p jqc-core --no-default-features` and `cargo build -p
  jqc-compiler --no-default-features --features jq-1-8` both succeed
  with zero warnings (no dead-code warning on the now-conditional
  `Backend::Simd` variant/dispatch arm).
- `cargo test -p jqc --no-default-features --features jq-1-8`: the
  `simd_codegen.rs` differential suite (now `#![cfg(feature =
  "codegen-simd")]`) and the two SIMD-specific benches (now `#[cfg]`-
  gated, with a matching `criterion_group!` for each configuration)
  compile and skip cleanly instead of failing to build.
- `cargo build/test/clippy --workspace` (default features) and `cargo
  fmt --check --all` (0 diffs) all pass -- 161+28+16+1+4+11+13+10 tests
  across the workspace, unchanged from before this task.
- `nix build .#default`, `.#example-script`, and
  `.#checks.x86_64-linux.{manual-tests,c-tests}` all pass.

= What's left

Done as asked: `codegen-simd` is a real, forwarded, off-able Cargo
feature with `jqc-core`'s `memchr` dependency and `simd` module entirely
excluded when it's off, matching `JqVersion`'s UX for naming an
unavailable selection. `"treewalk"` intentionally has no feature of its
own, documented above as a deliberate asymmetry rather than an
oversight. If a future backend (e.g. the still-hypothetical multi-core
backend the `codegen-trait` todo-report mentions) turns out to need its
*own* separable pipeline rather than a few dispatch points layered on
the tree-walk one, it would be the first backend where a real
"treewalk-vs-other" exclusion becomes meaningful, and should follow this
same `CodegenBackend`/feature shape.
