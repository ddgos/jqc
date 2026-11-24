#import "../template.typ": report, diagram

#show: report.with(title: "jq version selection for testing", date: "2026-11-24")

= Context

`todos.md` asked for a way to specify which `jq` version the macro
uses, defaulting to the newest nixpkgs-unstable version, with older
versions tested reproducibly via nix.

`jqc` itself never calls a real `jq` binary -- `jq!` is a from-scratch
lexer/parser/lowering/codegen pipeline for a jq subset, not a wrapper
around the interpreter. So "which jq version" has no effect on what
`jq!` expands to; the only place a specific `jq` *version* is
operationally meaningful is the differential test suite, which shells
out to a real `jq` on `PATH` as ground truth. This task is implemented
accordingly: version selection for *that*, not for `jq!`'s own behavior.

= What was added

Two new pinned flake inputs resolve to historical nixpkgs channels that
shipped specific jq versions:

```
nixpkgs-jq-1-6.url = "github:NixOS/nixpkgs/nixos-23.05"; # last with jq 1.6
nixpkgs-jq-1-7.url = "github:NixOS/nixpkgs/nixos-23.11"; # first with jq 1.7.1
```

Each gets a dev shell identical to `default` except for which `jq` is on
`PATH` -- everything else (`cargo`, `rustc`, …) still comes from the
primary `nixpkgs` input, so testing an old jq doesn't mean rebuilding an
old Rust toolchain too:

#diagram(
  boxes: (("nix develop", 0, 0), (".#default\n(jq 1.8.2)", 1, 0), (".#jq-1_6\n(jq 1.6)", 1, 1), (".#jq-1_7\n(jq 1.7.1)", 1, 2)),
  arrows: ((0, 1), (0, 2), (0, 3)),
)

`nix develop .#jq-1_6 -c cargo test -p jqc --test differential` runs the
*exact same* differential suite against jq 1.6 instead of whatever's
newest -- no Rust code changes needed, since the suite already just
shells out to whatever `jq` is on `PATH`. Both jq binaries are also
exposed directly as `packages.jq-1_6` / `packages.jq-1_7`, since flake
inputs being pinned to branch names (not exact revisions) looks
unreproducible at a glance but isn't: `flake.lock` freezes the exact
resolved commit either way, same as the `nixpkgs-unstable` input always
has.

= What this immediately found

Running the differential suite against `jq-1_6` failed one of 124 cases,
`if_no_else` (`if .a then 1 end`):

```
$ jq 'if .a then 1 end' <<< '{"a":false}'   # jq 1.6
jq: error: syntax error, unexpected end...
jq: error: Possibly unterminated 'if' statement...
```

`if`/`then`/`end` without an `else` clause is a genuine jq *language*
feature added after 1.6 -- not a `jqc` bug. `jq-1_7` (1.7.1) and the
default shell (1.8.2) both pass all 124 cases cleanly, so the dividing
line is exactly where the real jq changelog says it is. This is the
mechanism working as intended: `jqc` targets the newest jq by design, and
testing against an intentionally older one is expected to show exactly
this kind of real, documented version skew -- not something to patch
over by changing `jqc`'s own behavior to match the older version.

= Verification

`nix flake check` passes (including evaluating both new dev shells and
packages); `nix build .#jq-1_6` succeeds; the differential suite was run
directly against `.#jq-1_6` (123/124, the one documented divergence
above), `.#jq-1_7` (124/124), and confirmed unchanged against `default`
(124/124). No Rust code changed, so `cargo build/test/clippy/fmt
--workspace` are unaffected.
