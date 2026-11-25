#import "../template.typ": report, diagram

#show: report.with(title: "Serde audit: a good fit, kept as-is", date: "2026-11-25")

= Context

`todos.md` asked whether `serde`/`serde_json` is a good fit for a project
only ever concerned with JSON data, given a specialist crate might do
better, and some of serde's default behavior might be rough around the
edges here. `jqc-core::Value` is a direct re-export of `serde_json::Value`
(`pub use serde_json::{json, Map, Number, Value}`), not a wrapper -- the
entire runtime value representation *is* `serde_json::Value`, so this
question goes to the heart of the project's data model, not a peripheral
dependency choice.

= Where serde is actually used

`serde` the derive framework (`#[derive(Serialize, Deserialize)]`) is used
in exactly *one* place in the whole repo: `jqc-manual-tests-macros`
deserializes jq's own `manual.yml` (via `serde_yaml`) into typed Rust
structs at test-generation time -- build-time infrastructure, not JSON, not
the runtime value path at all.

Everywhere else -- `jqc-core`, `jqc-cli`'s `parse_json_stream` (the one
non-test-only runtime consumer, streaming stdin via
`serde_json::Deserializer::from_str`) -- `serde_json` is used purely as a
dynamic JSON value enum, parser, and formatter. No typed struct is ever
(de)serialized at runtime. `jqc-core/Cargo.toml` already enables
`preserve_order` (so `Map` is insertion-ordered, matching jq's own key-
order semantics) and doesn't enable `arbitrary_precision`/`float_roundtrip`.

#diagram(
  boxes: (
    ("serde (derive)", 0, 0),
    ("serde_yaml", 1, 0),
    ("manual.yml -> structs", 2, 0),
    ("serde_json", 0, 1),
    ("Value/Number/Map", 1, 1),
    ("jqc-core + jqc-cli", 2, 1),
  ),
  arrows: ((0, 1), (1, 2), (3, 4), (4, 5)),
)

Since nothing in the runtime path (de)serializes a typed struct, serde's
actual headline value -- derive-generated, zero-cost typed (de)serialization
-- is irrelevant here. `jqc-core` only needs "a JSON value enum with a
parser and formatter," which `serde_json::Value` already is; the unused
derive machinery is a sibling dependency, not a cost this crate pays for
using `Value` alone.

= The two documented rough edges, re-examined

- *Number formatting* (`number_from_f64` in `jqc-core/src/helpers.rs`):
  `serde_json::Number::from_f64` always keeps float formatting, so jqc
  manually chooses an integer `Number` when the value is an exact whole
  number under `1e18`. This already handles every normal case correctly --
  the one remaining divergence (extreme-magnitude literals near `1e17`) is
  a narrow, cosmetic, already-tolerated discrepancy
  (`jqc-c-tests-macros`'s own `KNOWN_RUNTIME_DISCREPANCIES`), not a serde
  defect worth re-architecting around.
- *Non-recursive `Drop`* (README's "Extremely deeply nested values... can
  still overflow the stack" limitation): genuinely unfixable while `Value`
  is a foreign type, since a non-recursive `Drop` can't be retrofitted onto
  someone else's enum. Checked the two leading SIMD-accelerated
  alternatives (`simd-json`, `sonic-rs`): neither ships a non-recursive
  `Drop` either -- both are, underneath, also plain recursive enums. No
  drop-in swap fixes this; only owning the value representation outright
  would, which is a far larger rewrite than this task's scope.

= Performance

`todo-reports/simd-codegen/report.typ` already measured hand-rolled SIMD
and `memchr::memmem` losing to plain scalar Rust at every tested size
(16B-1MB) for string operations -- the compiler's own auto-vectorization
already wins there. That's strong existing evidence against expecting a
SIMD-accelerated JSON *parser* (`simd-json`/`sonic-rs`) to meaningfully
beat `serde_json` at jq's typical input sizes either; no benchmark anywhere
in this repo shows JSON parsing itself as a bottleneck.

= Recommendation: keep `serde_json`

No crate swap. Both documented rough edges are either already handled
(numbers) or unfixable by any drop-in alternative (recursive `Drop`), and
the performance case against a SIMD JSON crate is already disproven by this
repo's own SIMD-codegen experiment. `preserve_order` is correctly already
enabled for jq's key-order semantics; nothing else needs configuring.

One real, smaller, separate finding surfaced along the way: `jqc-core`'s
`write_jq_json_string` and `jqc-cli`'s `write_json_string` are two
independent hand-rolled JSON-string-escaping implementations (the latter
additionally supporting `-a`/`--ascii-output`'s escape-everything-above-
ASCII mode) -- genuine duplication, but an internal-API cleanup question,
not a serde-suitability one. Left for the existing open-ended "tidy up"
task rather than folded into this one.
