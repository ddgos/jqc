#import "../template.typ": report, diagram

#show: report.with(title: "Debugging a real user script: //, entries builtins, a linker gap, and a memory blowup", date: "2026-11-24")

= Context

A user ran `nix run github:ddgos/jqc -- <their own real script>` and reported
two failures in quick succession: first `linker \`cc\` not found`, then (after
that was fixed) a jq parse error. Chasing the parse error down turned up a
missing operator, then a missing set of builtins, then -- once those were
added -- the script's compilation ballooned to over 30GB of RAM and had to
be killed by hand before it took down the shared machine it was running on.
This report covers all four, in the order they were found.

= 1. `linker \`cc\` not found`

`jqc-build`'s packaged `wrapProgram` only put `cargo`/`rustc` on its `PATH`;
linking the scratch crate it generates also needs a C linker, which `nix
develop`'s `mkShell` provides for free via `stdenv` -- masking this for
anyone who'd only tested inside the dev shell. Fixed by adding
`pkgs.stdenv.cc` to the wrapper's `PATH` in `flake.nix`; verified with `env
-i` (no inherited environment at all) that the packaged binary links
correctly on a bare `PATH`.

= 2. The alternative operator, `//`

The script's `(b.daggers // [])` didn't parse: `jqc`'s lexer read `//` as
two separate `/` (division) tokens, since it had no `//`-specific case. Not
a new bug -- `//` was already on `README.md`'s "not implemented" list.

jq's actual semantics (confirmed against the real binary, since the manual
doesn't state the error-handling case): `a // b` keeps `a`'s outputs that
are neither `false` nor `null`; if that filtered set is empty, it produces
`b`'s instead. An error in `a` is *not* caught by `//` -- it propagates --
but whatever `a` had already filtered-and-produced *before* that error
stays, recoverable by an enclosing `try`:

```
$ jq -cn 'try ((false, 1, error) // 99) catch "caught"'
1
"caught"
```

Implementation: a new `Expr::Alt` AST node, a `//` lexer token (checking
for a second `/`) and parser level between `,` and `or` (right-associative,
matching real jq's grammar), and a codegen arm built on the `partial_aware_for`
helper from the earlier partial-output-on-error work -- which already does
exactly "keep what a sub-generator produced before it failed, re-raise with
that as `emitted`", so `//`'s error-not-caught-but-partial-output-kept
behavior falls out for free:

#diagram(
  boxes: (("a's outputs", 0, 0), ("filter: != false/null", 1, 0), ("non-empty?", 2, 0), ("keep filtered", 3, 0), ("eval b", 3, 1)),
  arrows: ((0, 1), (1, 2), (2, 3), (2, 4)),
)

One real Rust-inference gotcha along the way: every other `partial_aware_for`
use is immediately consumed by `bubble`/`bubble1`, whose concrete parameter
type pins the ambiguous `Ok(__out)`-without-a-pinned-error-type block; `//`
needed to inspect the filtered `Vec` directly first (to check emptiness)
before deciding what to return, so a bare `(block)?` left rustc unable to
infer the error type despite the enclosing closure's own fully-concrete
return type. Fixed by routing it through the existing `wrap_fallible`
helper (an explicit closure-return-type annotation) instead of relying on
`?` alone to resolve it.

14 new differential tests cover truthy-filtering, fallback, zero-is-truthy,
multi-valued generators on both sides, precedence against `,`/`or`/`and`/
comparisons/`as`, and the error-propagation-with-partial-output case above.

= 3. `to_entries` / `from_entries` / `with_entries`

Next error: `unsupported or unknown jq function 'to_entries/0'`. These are
themselves jq-source-expressible (jq's own `builtin.jq` defines them in
terms of `keys_unsorted`, `map`, `has`, `add`, and -- fittingly --
`//`), so they're added to `jqc-compiler`'s existing prelude mechanism
rather than as new codegen primitives, using jq 1.8.2's exact definitions:

```
def to_entries: [keys_unsorted[] as $k | {key: $k, value: .[$k]}];
def from_entries: map({ (.key // .Key // .name // .Name):
  if has("value") then .value else .Value end }) | add // {};
def with_entries(f): to_entries | map(f) | from_entries;
```

6 new differential tests, including the `from_entries` empty-input
(`add // {}`) and alternate-key-name (`.Key`/`.Name`) edge cases.

= 4. A real, severe memory blowup -- found the hard way

With both fixes in, the script still didn't compile cleanly -- it hung,
and `ps`/`free` showed its `rustc` process at *33GB RSS and climbing* after
nearly six minutes, with the host down to 14GB free and 17GB in swap.
Killed by hand immediately; the host recovered once the zombie was reaped.

== Isolating it safely

Every further test of this ran under `systemd-run --user --scope
-p MemoryMax=3G -p MemorySwapMax=256M`, so a repeat could only ever take
down its own cgroup, never the host. Bisecting the script (defining
progressively more of its `def` chain, each checked under the cap before
adding the next) found the trigger precisely: `ballots_of`,
`eligible_targets_of`, `leaders_of`, and `voting_round` *defined* (even
together) compiled in milliseconds; the moment `round_table` -- which
calls `leaders_of` twice and `voting_round` twice, each with different
arguments -- was actually *called* once, compilation exploded.

Minimal repro, found by isolating further: `leaders_of` alone (one `def`
nesting `to_entries`, `map`, a `daggers | any(. == $voter)` *inside*
`map`'s filter-argument, `group_by`, `select`, `max`) took *27 seconds* to
macro-expand for a *single* call; calling it from two different sites blew
past the 3GB cap. Confirmed unrelated to this session's `//`/entries work
specifically by reproducing the same 27-second cost with `to_entries`
replaced by its manual `keys_unsorted`-based expansion, avoiding this
session's new prelude defs entirely -- the cost is in `jqc-compiler`'s
existing call-by-name inlining machinery, triggered by nesting several
different builtin calls (not just the same one repeatedly, which
`scaling_check`'s existing `f_i: f_{i-1} | f_{i-1}` doubling-chain fixture
already covers) inside one `def`.

#diagram(
  boxes: (("leaders_of\n(defined)", 0, 0), ("round_table\n(defined)", 1, 0), ("round_table(.)\n(called once)", 2, 0), ("27s / 1 call\n(leaders_of alone)", 2, 1), ("OOM-killed\n(2+ calls, 3G cap)", 2, 2)),
  arrows: ((0, 1), (1, 2), (2, 3), (3, 4)),
)

== Root cause -- *revised*: see the `codegen-size-blowup` report

This report originally diagnosed the cause as `lower_call` re-lowering
already-expanded content from scratch at every call site -- plausible
given the AST-level shape of the script, but wrong: a follow-up pass
instrumented `lower_call` directly and found only ~14 total invocations
for the single-call `leaders_of` repro, nowhere near enough to explain 27
seconds. The *actual* cause was one level down, in codegen: `jqc-compiler`
inlined a whole loop/merge shape as raw tokens at *every* pass-through
node (`Pipe`, `Field`, `BinOp`, …), so nested pass-through structure
multiplied generated-code size rather than adding to it -- measured at
*8.7 million characters* of generated Rust for this one small function.
Fixed at the root (moved that shape into a real runtime function,
`jqc_core::partial_for`, called instead of inlined) in the
`codegen-size-blowup` report, which also re-verifies this exact script:
9.6 seconds, normal memory, correct output. The `JQC_BUILD_MEM_LIMIT_GB`
safeguard below shipped anyway, as defense-in-depth against the *next*
unknown cost cliff, not as a workaround for this one specifically.

== What shipped: a safety net

This pass adds `jqc-build`'s own safeguard: it caps its child `cargo
build`'s address space (`RLIMIT_AS`, inherited by the `rustc` `cargo`
spawns) to a quarter of physical RAM, nominally capped at 32GiB,
overridable via `JQC_BUILD_MEM_LIMIT_GB` -- the same pattern
`jqc-compiler/src/bench.rs`'s `cap_memory()` already uses for its own test
suite, now applied to the end-user-facing tool.

Verified (before the real fix landed) against the exact script that
caused the incident: under the default cap (≈9GB on the 38GB machine this
ran on), it failed cleanly in ~93 seconds (`rustc`'s own allocator
aborts; `cargo`/`jqc-build` report an ordinary build failure) with the
host's own memory untouched throughout (`free` stayed flat). A 1GB cap
(deliberately too tight, to confirm the mechanism itself works) correctly
aborted even *unrelated* dependency compilation (`serde_json`) well
before threatening the host, confirming the limit applies to the whole
child process tree as intended. Kept in place after the real fix, as
general-purpose defense-in-depth -- see the `codegen-size-blowup` report.

= Verification

`cargo test/clippy --workspace` (151 differential tests, up from 125 at
the start of this pass), `cargo test --manifest-path
jqc-manual-tests/Cargo.toml` (251 passed) all green throughout. The
`cc`-linker fix, `//`, and the entries builtins were each verified against
the real `jq` binary (differential tests) and/or by hand against jq's own
manual text for semantics the manual itself doesn't fully specify (error
handling). The memory safeguard was verified against the actual
pathological script, both under a deliberately-too-tight 1GB cap (confirms
the mechanism fires) and the real default (confirms it doesn't interfere
with normal builds and still protects the host on the real incident case).

= What's left

A new, specific task was added to `todos.md` (replacing the vague framing
the old "known limitations" item had around this): fix the `lower_call`
re-lowering inefficiency itself. The two candidate directions -- memoizing
`lower()`, or compiling filter-params as real closures -- need their own
investigation before picking one; not attempted in this pass given its
scope. `jqc-build`'s new `-o`/error-reporting UX (now flagged by a separate
`todos.md` task, asking for Rust-quality error messages with suggested
fixes) is also unrelated follow-up surfaced by this same debugging session,
not addressed here.
