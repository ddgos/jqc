# jqc

A `jq!` macro that compiles a [jq](https://jqlang.org/) filter straight into
native Rust at **macro-expansion time** -- lex → parse → desugar/inline
`def`s → codegen -- so the shipped binary contains the control flow
equivalent of your filter, not a jq parser/interpreter.

```rust
use jqc::jq;
use jqc::core::json;

fn main() {
    let filter = jq!(".items[] | select(.active) | {name: .name, score: (.score * 2)}");
    let input = json!({"items": [{"name": "a", "active": true, "score": 3}]});
    println!("{:?}", filter(&input).unwrap());
}
```

Build it normally (`cargo build --release`) and you get a standalone binary
dedicated to running exactly that filter -- see `examples/src/main.rs` for a
small NDJSON-filtering CLI built this way.

## Layout

- `jqc-core` -- runtime support: the shared `Value`/`JqError` types and the
  value-level operations (`add_values`, `cmp_values`, `index_field`, sort/
  group/unique helpers, string builtins, …) that generated code calls into.
  No jq parsing happens here or anywhere at runtime.
- `jqc-macros` -- the proc-macro itself: hand-rolled lexer/parser for the jq
  subset below, a lowering pass that desugars builtins (`map`, `select`,
  `recurse`, `any`/`all`, `while`/`until`/`repeat`, …) via a small jq-source
  prelude and inlines/resolves user `def`s, and codegen that turns the
  result into a `Fn(&Value) -> JResult<Vec<Value>>` closure.
- `jqc` -- the facade crate to actually depend on; re-exports `jqc-core` as
  `jqc::core` (generated code references `::jqc::core::...`, so this is
  what you add to `Cargo.toml`, not `jqc-core`/`jqc-macros` directly) and
  `jq!` from `jqc-macros`.
- `examples` -- a tiny compiled-filter CLI demonstrating usage.

## Supported jq subset

Identity/field access (`.`, `.a.b`, `."weird key"`), indexing and slicing
(`.[0]`, `.[-1]`, `.[1:3]`), `.[]`, `?` (postfix, applies to the immediately
preceding step only -- matches real jq), `|`, `,`, literals incl. string
interpolation (`"\(expr)"`), array/object construction (incl. `{a}`/`{$a}`
shorthand and computed `(expr): val` keys), arithmetic (`+ - * / %`,
jq's type-aware semantics for each), comparisons, `and`/`or`, `if/elif/
else/end`, `try/catch`, `as $x` bindings, `reduce`, `foreach`, recursive
and non-recursive `def` (see limitation below), and builtins: `map`,
`select`, `values`, `recurse(;f)`, `in`, `any`/`all` (0/1/2-arg forms),
`while`/`until`/`repeat`, `length`, `keys`/`keys_unsorted`, `has`, `type`,
`not`, `empty`, `error`, `add`, `sort`/`sort_by`/`group_by`/`unique`/
`unique_by`/`min`/`max`/`min_by`/`max_by`, `reverse`, `range/1,2,3`,
`first`/`last` (0/1-arg), `floor`/`ceil`/`round`/`sqrt`/`fabs`, `tostring`/
`tonumber`, `explode`/`implode`, `ascii_downcase`/`ascii_upcase`,
`startswith`/`endswith`/`contains`/`ltrimstr`/`rtrimstr`/`join`/`split`.

## Known limitations

- **Eager, not lazy.** Every sub-expression evaluates to a fully-materialized
  `Vec<Value>`. This keeps codegen tractable (no generic-iterator-type
  explosion) but means **infinite generators never terminate** -- `repeat`
  and `while`/`until`/`recurse` without a condition that eventually goes
  false will hang. There's no `label`/`break`/`limit` to bound them yet.
- **No destructuring patterns** in `as` bindings, only a single `$x`.
- **`def` with parameters can't be recursive.** 0-arg recursive `def`s
  compile to a real recursive Rust `fn`; `def`s with parameters are inlined
  call-by-name at each call site (matching jq's own filter-argument
  semantics) but that approach can't terminate for a recursive, parameterized
  def, so it's rejected at compile time with a clear error instead of hanging
  or silently looping forever.
- Not implemented: `%//`-alternative operator, `path`/`paths`/`getpath`/
  `setpath`/`del`, update-assignment operators (`|=`, `+=`, …), `input`/
  `inputs`/`$ENV`, `label`/`break`, SQL-style ops, module `import`/`include`.
- Every clone is a real `Value::clone()` (deep for arrays/objects). Fine for
  typical filter sizes; a future version could switch to `Rc<Value>`
  internally for less copying on heavily-reused sub-results.

## Testing

- `cargo test -p jqc --test differential` -- the main correctness suite:
  ~120 filters run through both the compiled closure and the real `jq`
  binary (needs `jq` on `PATH`; the Nix dev shell provides it) and compared.
- `cargo test -p jqc --test fuzz_inputs` -- proptest-generated arbitrary
  JSON inputs run through a curated filter set, differentially checked
  against real `jq`.
- `cargo test -p jqc --test fuzz_scripts -- --ignored` -- proptest-generated
  arbitrary *scripts* (not just inputs). Since `jq!` needs a literal at
  macro-expansion time, this works around it by writing each generated
  script into a scratch crate and rebuilding it per case, so it's slow
  (~1s/case) and off by default.
- `cargo bench -p jqc` -- criterion benchmarks of the generated code itself
  (not the macro-expansion step, which only runs once at compile time)
  across input sizes, each with a real-`jq`-subprocess series alongside it
  (at a couple of smaller sizes, to keep subprocess-spawn overhead from
  dominating `cargo bench`'s own runtime) showing the per-call cost `jq!`
  avoids by baking the filter in at compile time.
- `jqc-macros/src/bench.rs` benchmarks and guards the macro-expansion step
  itself. `unused_defs_scaling_check` and `scaling_check` -- regression
  checks for a `def_scope` cost cliff that once blew up to 38GB+ of RSS --
  run as normal tests (part of `cargo test --workspace`, no flags needed).
  `compile_time_benchmarks` (wall time) and `peak_memory_benchmarks` (peak
  RSS) stay behind `cargo test -p jqc-macros --release -- --ignored <name>
  --nocapture`, since they're diagnostics with no pass/fail assertions,
  not correctness checks; run `peak_memory_benchmarks` alone (an exact
  `--ignored` name match), not alongside other `#[ignore]`d tests in the
  same invocation, since its RSS reading is a per-*process* high-water
  mark that sibling threads' allocations would otherwise inflate. All four
  cap this process's address space first (default: half of physical RAM,
  capped at 64GiB; override with `JQC_TEST_MEM_LIMIT_GB`) so a regression
  aborts fast instead of being left to the kernel's OOM reaper.
- `cargo tarpaulin --workspace --out Html --output-dir target/tarpaulin` --
  line coverage from the test suite. Only meaningful for `jqc-core` (the
  runtime support crate): `jqc-macros`' lex/parse/lower/codegen pipeline
  runs during the *compilation* of the tests that exercise it (`jq!` is a
  proc macro), not while the resulting test binary runs, so a runtime
  coverage tool reports near-0% there regardless of how thoroughly the
  differential suite exercises it. Read that coverage from differential
  test results instead, not from tarpaulin's numbers for that crate.

## Performance history

`benchmarks/pareto-front.csv` is an append-only log of benchmark results
tagged by commit and `Codegen` implementation (see `benchmarks/README.md`
for the schema and when to add a row). It's what lets a future SIMD or
multi-core backend be compared against the tree-walk one on equal
footing instead of only against whatever ran last.

## Dev shell

```
nix develop
```
gives you `cargo`, `rustc`, `rust-analyzer`, `rustfmt`, `clippy`, `jq`
(needed by the differential tests), and `typst`.

## Todo reports

Completed items from `todos.md` get a short writeup under
`todo-reports/<summary>/report.typ`, using the shared layout in
`todo-reports/template.typ`. Each one is its own reproducible build via
[typix](https://github.com/loqusion/typix) (`packages.report-<summary>`).
Compile every report to PDF with:

```
nix run .#build-reports
```
