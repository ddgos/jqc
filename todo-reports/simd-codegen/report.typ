#import "../template.typ": report, diagram

#show: report.with(title: "A SIMD codegen backend -- and an honest result", date: "2026-11-24")

= Context

`todos.md` asked to look for SIMD optimisation opportunities and
implement one as a new `Codegen` trait implementation, kept separate
from `TreeWalkCodegen` so the two can be compared. This report covers
both the implementation and -- the more interesting part -- what
actually happened when it was measured.

= Wiring a second backend into `jq!`

`jq!("...")` always called `TreeWalkCodegen` directly; selecting a
*different* implementation needed a new macro argument,
`jq!(codegen = "simd", "...")` (mirroring the existing `version = "X.Y"`
selector, combinable with it in either order).

#diagram(
  boxes: (("jq!(codegen=\"simd\", …)", 0, 0), ("SimdCodegen", 1, 0), ("compile_top\n(shared)", 2, 0), ("BACKEND\nthread-local", 3, 0)),
  arrows: ((0, 1), (1, 2), (2, 3)),
)

`SimdCodegen` needs to change the output of only two or three specific
builtin dispatches deep inside `compile_call`, out of the ~100 recursive
call sites `compile`/`compile_raw` have. Rather than threading a new
parameter through every one of those (the overwhelming majority of which
have no reason to care which backend is active), `compile_top` sets a
thread-local `Backend` flag once at the top, read only at the few
dispatch points that need it -- sound because macro expansion is single-
threaded per invocation and the flag is set, read, and reset within one
call, never observed across calls or threads.

= Two SIMD attempts, two honest negative results

*`ascii_downcase`/`ascii_upcase`* -- the textbook case: a pure byte-wise
transform, safe to apply without any UTF-8-awareness at all (every byte
of a multi-byte sequence, leading or continuation, falls outside the
ASCII letter ranges this folds). Implemented with explicit SSE2
intrinsics (16 lanes, always available on x86_64) plus a runtime-
detected AVX2 path (32 lanes), using the standard "subtract then
unsigned-min" range-check trick. Correctness: a proptest suite (40+
cases/run, plus hand-picked boundary bytes and exact chunk-size
boundaries at every power-of-two-ish length from 15 to 65) confirms
every output matches the existing scalar `ascii_downcase`/`ascii_upcase`
exactly.

Performance, measured via `cargo bench`, told a different story at every
iteration:

#table(
  columns: 3,
  [*Input size*], [*`treewalk` (scalar)*], [*`simd` (best: AVX2)*],
  [16 B], [9.7 ns], [10.1 ns],
  [1 KB], [31.6 ns], [33.4 ns],
  [1 MB], [26.6 µs], [35.5 µs],
)

The SIMD path is *slower* at every size, by a widening margin as size
grows. The first version (SSE2 only, plus a redundant `String::from_utf8`
re-validation scan) was worse still (~45-61 µs at 1 MB); removing that
redundant scan and later adding an AVX2 path each measurably helped, but
never closed the gap with `str::to_ascii_lowercase`'s own plain scalar
loop, which LLVM's auto-vectorizer already handles very well at `-O3`
with LTO (this project's release profile). Hand-rolled SIMD needs to clear
a bar the compiler has often already cleared for a transform this
simple and regular -- confirmed here, not merely asserted.

*`contains`* (string-in-string) -- per explicit direction mid-task to
prefer an established crate over hand-rolled code where one provides
genuine value, this one uses `memchr::memmem::find` (the substring-
search building block behind `ripgrep` and much of the ecosystem)
instead of intrinsics. Object/array recursion and the kind-mismatch
check (confirmed against real jq: `true | contains(false)` errors, since
`jv_kind` distinguishes `true`/`false` as separate kinds) are unchanged
from the scalar `contains_value`; only the `(String, String)` leaf
differs. Correctness: proptest across arbitrary string/array pairs,
plus kind-mismatch and empty-needle edge cases, confirmed exact
agreement with the scalar version, and a differential suite (mirroring
`tests/differential.rs`'s own real-`jq`-ground-truth approach) confirmed
both against real jq directly.

Benchmarked against a worst-case haystack (needle never present,
forcing a full scan):

#table(
  columns: 3,
  [*Haystack size*], [*`treewalk` (`str::contains`)*], [*`simd` (`memchr`)*],
  [16 B], [50.0 ns], [53.2 ns],
  [1 KB], [72.9 ns], [125.5 ns],
  [1 MB], [20.2 µs], [31.4 µs],
)

Same outcome: `memchr` loses too, by a larger margin than the hand-
rolled case folding. `str::contains` already resolves to a well-tuned
substring search in current Rust's standard library; `memchr::memmem`'s
own setup cost (building a search state for the needle) isn't
recovered at these sizes for a needle this short.

= Why keep code that doesn't win

Both implementations are correct, both are properly tested, and both
are measurably slower than the baseline they were meant to beat. This
is a legitimate, useful result to keep, not a reason to delete the
work: `todos.md`'s own framing explicitly wants separate
implementations kept around specifically *for comparison* -- an honest
"we tried, here's the measured answer, here's why" is exactly that
comparison, and silently dropping a negative result would make the next
person re-discover the same dead end by hand. `SimdCodegen` stays
available (`jq!(codegen = "simd", ...)`) as a working, differential-
tested second backend other builtins could still reasonably target --
just not these two, on this hardware/toolchain, with these approaches.

= A related question, answered but not implemented

Mid-task: would `impl Iterator<Item = Value>` give lazy evaluation "for
free"? Answered directly (no code changes, by request) -- no:
`impl Trait` return types are monomorphized per concrete type, and jq's
branching constructs (`if`/`try`/`catch`, recursive `def`) need two
structurally different sub-expressions to produce the *same* concrete
return type, which `impl Trait` can't express without boxing. That's
precisely the "generic-iterator-type explosion" README already cites as
why this codebase chose eager `Vec<Value>` -- confirmed as a real wall,
not a hypothetical one. `Box<dyn Iterator<Item = JResult<Value>>>` is
the realistic lazy design (solves the branching problem via type
erasure) but gives up "zero cost": dynamic dispatch and a heap
allocation per generator step. Left as a candidate for a future
`todos.md` task, not pursued here.

= Verification

`cargo test -p jqc-core` (10 unit/property tests for the SIMD module),
`cargo test -p jqc --test simd_codegen` (16 differential tests against
real jq, covering both backends), `cargo build/test/clippy --workspace`,
`cargo fmt --check --all` (0 diffs), and `nix build .#default`/
`.#checks.x86_64-linux.{manual-tests,c-tests}` all pass.

= What's left

This task is done for what it asked (look for opportunities, implement
one, keep it comparable) -- with the caveat that "opportunity" turned
out, on measurement, not to be one for either builtin tried. The next
`todos.md` task ("pursue whatever optimisations possible... keep
separate implementations") is the natural place for further SIMD/
performance exploration -- `simd-json` (replacing `serde_json` for
*input parsing*, the one place every filter run touches regardless of
which builtins it uses) was raised as a more promising target than
either builtin tried here, not yet investigated.
