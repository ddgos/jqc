#import "../template.typ": report, diagram

#show: report.with(title: "A jq-compatible CLI for compiled binaries", date: "2026-11-24")

= Context

`todos.md` asked for `jqc-build`-compiled binaries to accept the same
options as real `jq` and to match its colored/pretty output, so a
compiled filter is a drop-in replacement for `jq '<filter>'`. Previously
each generated binary's `main.rs` was a ~30-line template: read stdin
line by line, parse each as JSON, run the filter, print each output via
`Value`'s plain `Display` -- no flags at all, and output that was
already wrong by default (`Display` is compact; real jq pretty-prints
unless told `-c`).

= What was added: `jqc-cli`

A new library crate, depended on by the scratch project `jqc-build`
generates (and by `examples`' demo binary) instead of embedding printing
logic inline. Its surface is one function:

```rust
fn main() -> std::process::ExitCode {
    let filter = jqc::jq!("...");
    jqc_cli::run(filter)
}
```

#diagram(
  boxes: (("argv", 0, 0), ("Args::parse", 1, 0), ("read input\n(stdin/files)", 2, 0), ("run filter\nper value", 3, 0), ("print\n(jq-exact format)", 4, 0)),
  arrows: ((0, 1), (1, 2), (2, 3), (3, 4)),
)

In scope: `-c`/`-r`/`-j`/`--raw-output0`/`-C`/`-M`/`-a`/`-S`/`-R`/`-s`/
`-n`/`-e`/`--tab`/`--indent N`, combined short flags (`-nc`), reading
named files instead of stdin, and jq's own exit-code convention (`0`/
`1`/`2`/`4`/`5`). Deliberately out of scope, documented in the crate's
own doc comment: `-f`/modules/`--stream`/`--seq` (not implemented
elsewhere in `jqc`), and `--arg`/`--argjson`/`--args`/`--jsonargs`/
`$ENV` -- these need a value injected into the filter at the *compiled
binary's* runtime, but `jqc`'s variable bindings are resolved entirely
at compile time from the filter's own `as` patterns; supporting them for
real needs new compiler support (a `$name` with no enclosing binding
becoming an extra parameter on the generated closure), not just a CLI
change, so it's left to a future task rather than faked.

= Reading jq's own source, not guessing at its output format

Matching "colour etc the same as jq" byte-for-byte meant reading jq's
own `src/jv_print.c` and `src/main.c` directly rather than reverse-
engineering it from examples:

- The default color table (`DEFAULT_COLORS`) and its `JQ_COLORS`-env-var
  override format, copied with the same null/false/true/number/string/
  array/object/field ordering.
- `jv_dump_term`'s exact structure: a kind's color is written once on
  entry and reset once on exit, re-applied around each `,`/`:` and the
  closing bracket so *separators* render in the container's own color
  rather than its last child's -- easy to get subtly wrong without
  reading the actual algorithm.
- `jvp_dump_string`'s exact escaping table (which control characters get
  `\b\t\r\n\f` shorthand vs.\ a bare `\u00XX`, and the surrogate-pair
  encoding for `-a`/`--ascii-output` above the BMP).
- `process()`'s exact exit-code mapping (`JQ_OK`/`JQ_OK_NULL_KIND`/
  `JQ_OK_NO_OUTPUT`/`JQ_ERROR_UNKNOWN`, and `-e`'s `abs()` of a negative
  "soft" code), and a genuinely surprising jq quirk found only by reading
  the code: `-a`/`--ascii-output` *overrides* `-r`/`--raw-output` for a
  string result, printing it as a compact ASCII-escaped JSON string
  instead of the raw bytes `-r` normally would.

One real, pre-existing `jqc` bug surfaced along the way, unrelated to any
of the above: `serde_json::Number`'s own `Display` lowercases a
scientific-notation exponent (`1e+20`); real jq's number formatter always
uppercases it (`1E+20`). Every existing differential/C-test/manual-test
suite compares *parsed* `Value`s, which treats `"1e+20"` and `"1E+20"` as
equal -- only `jqc-cli`'s own byte-exact comparison could have caught
this. Fixed with a one-line case-normalization in `write_term`.

= Verification

`cargo test -p jqc-cli --test differential`: 11 passing cases, each
comparing `format_value`'s output against a real `jq -n '.' <flags>`
subprocess across ~17 representative values (null/bool/numbers
incl.\ scientific notation/strings incl.\ unicode and control characters/
nested arrays and objects) and 9 flag combinations (default pretty,
compact, sort-keys, compact+sort-keys, `--indent 4`, `--indent 0`,
`--tab`, ascii, and color in pretty/compact/sorted forms) -- the same
differential-testing principle the rest of the project uses for filter
*evaluation*, applied here to output *formatting* instead.

End-to-end, by hand: compiled a real filter via `jqc-build`, then diffed
its output against real `jq` across `-c`/`-C`/`-S --indent 4`/`-r`/
`-e`/`-n`/`-s`/`-R` on representative input -- byte-identical in every
case, including ANSI color codes.

`cargo build/test/clippy --workspace` (18 tests total now across
`jqc-cli`'s unit + differential suites), `nix build .#example-script`,
and `nix build .#default` (which runs the full workspace test suite as
its checkPhase) all pass.

= What's left

This task is complete for the flags it set out to cover. `--arg`/
`--argjson`/`$ENV` support remains a real gap -- already listed in
README.md's "Known limitations" (`input`/`inputs`/`$ENV` not
implemented) -- and would be a natural follow-up `todos.md` task in its
own right, since it needs compiler changes, not just `jqc-cli` ones.
