#import "../template.typ": report, diagram

#show: report.with(title: "Serde, revisited: scoped, then confirmed kept", date: "2026-11-25")

= Context

The `serde-audit` todo-report concluded "keep `serde_json`" based on reasoning
about its two documented rough edges (number formatting, non-recursive
`Drop`) before either had a *second*, independently-confirmed concrete
defect behind it. Fuzzing the next day found the NaN/infinity-representation
gap directly (`((-2)|sqrt) | type` diverges from real jq, among others --
see the `fuzz-scripts`-era commits and README's "Known limitations"). Two
confirmed, concrete issues tracing to `serde_json::Value`/`Number`'s own
design -- not one reasoned-about cost -- warranted re-opening the question
more skeptically rather than standing on the earlier conclusion.

= What was scoped

Unlike the first audit (which surveyed *usage*), this pass estimated the
actual *migration cost* of a custom `Value` enum (a plain `f64` field
instead of `serde_json::Number`, non-recursive `Drop`):

- `jqc-compiler` (the codegen/macro layer) barely touches `Number` at all --
  it emits calls into `jqc-core` functions opaquely, never matching
  `Number`'s own structure. This is the single biggest reason the scope is
  *smaller* than it might look: the compiler layer is essentially
  insulated from the value representation entirely.
- `jqc-core/src/helpers.rs`: ~36 `Number`-touching call sites, but
  concentrated through 2-3 shared helpers (`number_from_f64`, `num_f64`,
  `cmp_values_at`) already touched this session fixing the NaN/infinity
  bug -- not scattered across the file.
- `jqc-core/src/gen.rs`/`simd.rs`: trivial `use serde_json::Value`
  import-only references, no structural `Number` matching.
- `jqc-cli`'s stdin parsing (`serde_json::Deserializer::from_str(src)
  .into_iter::<Value>()`) would need a hand-written `serde::Deserialize`
  impl for the custom type -- serde_json's own `Value` deserializer is a
  direct template to adapt, a well-trodden pattern (`simd-json`/`sonic-rs`
  both do exactly this for their own custom value types).
- `jqc-cli` also has its own number-formatting code, independently
  duplicating logic against `jqc-core`'s -- the same class of duplication
  `serde-audit` already flagged separately (`write_jq_json_string`/
  `write_json_string`). A custom `Value` migration would touch this too.
- Test files could keep writing `serde_json::json!({...})` literals via a
  one-time `From<serde_json::Value> for Value` conversion, rather than
  rewriting every test's value construction by hand.

#diagram(
  boxes: (
    ("jqc-compiler\n(insulated)", 0, 0),
    ("jqc-core/helpers.rs\n(~36 sites)", 1, 0),
    ("jqc-cli\n(Deserialize + fmt)", 2, 0),
    ("tests\n(From<> glue)", 3, 0),
  ),
  arrows: (),
)

Net estimate: real, but bounded -- on the order of 300-400 lines of new/
changed code, concentrated in two or three files, not a sprawling rewrite
touching the whole codebase.

= Decision

Presented the scope estimate to the user directly rather than deciding
unilaterally -- this is a foundational, high-blast-radius change to the
project's core data representation, reversing a recommendation made in the
same session, and the `todos.md` entry itself asked for exactly that
judgment call ("only implement if the answer is genuinely yes, not just it
would fix these two issues in isolation").

Decision: *keep `serde_json`*. The two confirmed issues (NaN/infinity
consumed before print, deep-nesting stack overflow past ~500 levels) are
narrow, already-documented edge cases, not worth a foundational rewrite of
the value representation right now, even though the migration itself is
more bounded than it might first appear. Both limitations remain documented
in README.md exactly as they were (no change needed there -- this pass only
re-confirmed the conclusion with a harder number behind it, it didn't change
the user-facing behavior or the documentation of it).
