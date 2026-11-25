#import "../template.typ": report, diagram

#show: report.with(title: "Building an arbitrary script into the nix store", date: "2026-11-25")

= Context

`todos.md` asked to investigate the feasibility of (and implement, if it
makes sense) running `"$(nix build <flake> <script>)"` for an arbitrary jq
script file, so the compiled binary lands reproducibly in the nix store the
same way `nix build .#example-script` already does for one hardcoded
script (`jqc-build/examples/select-active.jq`, via `mkJqScript` in
`flake.nix`).

= Feasibility

The literal syntax asked for isn't possible with vanilla nix flakes: flake
outputs are static attribute paths, evaluated without positional runtime
arguments -- there's no way to write `nix build .#<something> <script.jq>`
and have `<script.jq>` flow into the derivation the way a shell command's
own argv would. This is a nix limitation, not a jqc one, and no amount of
flake.nix cleverness gets around it directly.

A close, fully working equivalent *is* feasible, though, and was verified
directly rather than just reasoned about: `mkJqScript` is already exposed
as `lib.mkJqScript` (for other flakes to reuse), and nix's `--impure
--expr` flags can call it with a *runtime* path, bypassing the "flake
inputs are evaluated without args" restriction by not going through the
flake's own `packages`/`apps` attributes at all:

```
nix build --impure --no-link --print-out-paths --expr \
  '(builtins.getFlake (toString /path/to/jqrs)).lib.mkJqScript
   builtins.currentSystem /path/to/script.jq'
```

Confirmed end to end: this builds a real nix-store derivation from an
arbitrary `.jq` file picked at the command line, and the resulting
`$out/bin/<name>` binary runs correctly on real input (`--impure` is
required because reading an arbitrary path outside the flake's own source
tree is, by design, something pure evaluation can't do; `builtins.
currentSystem` sidesteps needing to know the caller's system name up
front).

#diagram(
  boxes: (
    ("script.jq\n(arbitrary path)", 0, 0),
    ("--impure --expr\ngetFlake + mkJqScript", 1, 0),
    ("nix store\nderivation", 2, 0),
    ("$out/bin/<name>", 3, 0),
  ),
  arrows: ((0, 1), (1, 2), (2, 3)),
)

= What changed

The incantation above is correct but not something to type by hand, so
`justfile` (added in the previous "collect every test command" task) gained
one more recipe:

```
just build-script myscript.jq
```

prints just the resulting binary's path -- `echo '{}' | "$(just
build-script myscript.jq)"` builds then runs it in one line, the closest
practical match to the original `"$(nix build <flake> <script>)"` ask.
`justfile_directory()` supplies the flake root regardless of the caller's
own working directory, and `set -euo pipefail` makes a build failure abort
the recipe with a non-zero exit instead of (confirmed directly, an earlier
draft without it did exactly this) silently printing a bogus
`/bin/<name>` path built from an empty `$out` when `nix build` itself
failed. `flake.nix`'s dev shell also gained `pkgs.just` itself (needed by
the previous task too, done here since this is the first thing that
actually exercises it end to end).

= Verification

Ran the recipe directly, not just read for correctness: a trivial `.`
script, a real multi-field filter (`.a + .b`) with both an absolute path
and one relative to the repo root, and a nonexistent script file (confirmed
it now fails loudly rather than printing a path that doesn't resolve to
anything runnable). `cargo fmt --check`/`clippy --workspace --all-targets`/
`cargo test --workspace` all still pass -- this task touched `flake.nix`
and `justfile` only, no Rust source.
