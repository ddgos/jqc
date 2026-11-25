# Testing recipes for jqc -- a reproducible way to reach the commands
# README.md's "Testing" section and flake.nix's own comments document in
# full; see those for the *why* behind each one, not duplicated here.
#
# Run `just` (or `just --list`) to see this list. Everything here assumes
# a `nix develop` shell (or otherwise has `cargo`, `jq`, and `$JQ_SRC`
# already on hand) -- none of these recipes invoke `nix develop` for you.

default:
    @just --list

# --- fast inner loop -----------------------------------------------------

# Main workspace suite: differential, fuzz_inputs, version_selection, scaling-check guards, doc-tests.
test:
    cargo test --workspace

# jqc's own differential suite (~120 filters vs real jq).
test-differential:
    cargo test -p jqc --test differential

# jqc-cli's output-formatting differential suite.
test-cli:
    cargo test -p jqc-cli --test differential

# jq!(version = "...", ...) selection, incl. the jq-1.6 if/else case.
test-version-selection:
    cargo test -p jqc --test version_selection --features jq-1-6

# Proptest-generated arbitrary JSON inputs vs a curated filter set.
test-fuzz-inputs:
    cargo test -p jqc --test fuzz_inputs

lint:
    cargo clippy --workspace --all-targets

fmt:
    cargo fmt

fmt-check:
    cargo fmt --check

# --- slow / off-by-default ------------------------------------------------

# Proptest-generated arbitrary *scripts* -- slow, rebuilds a scratch crate per case; not in `test`/`ci`.
test-fuzz-scripts:
    cargo test -p jqc --test fuzz_scripts -- --ignored --nocapture

# Differential tests from jq's own manual examples. Needs $JQ_SRC.
test-manual:
    cargo test --manifest-path jqc-manual-tests/Cargo.toml

# Differential tests from jq's own C test suite. Needs $JQ_SRC.
test-c:
    cargo test --manifest-path jqc-c-tests/Cargo.toml

# Both $JQ_SRC-gated suites together.
test-excluded: test-manual test-c

# Macro-expansion wall-time benchmark (diagnostic, no pass/fail).
bench-compile-time:
    cargo test -p jqc-compiler --release -- --ignored compile_time_benchmarks --nocapture

# Macro-expansion peak-RSS benchmark. Run alone, not alongside other --ignored tests (per-process RSS reading).
bench-peak-memory:
    cargo test -p jqc-compiler --release -- --ignored peak_memory_benchmarks --nocapture

# Criterion benchmarks of the generated code itself (jqc vs jq).
bench:
    cargo bench -p jqc

# Line coverage. Only meaningful for jqc-core -- see README.md's Testing section for why.
coverage:
    cargo tarpaulin --workspace --out Html --output-dir target/tarpaulin

# --- nix -------------------------------------------------------------------

# Build the jqc-build CLI and the example-script demo.
build:
    nix build .#default .#example-script

# Builds every `checks.*` output: the fast suite plus both $JQ_SRC suites.
flake-check:
    nix flake check

# Compile every todo-reports/<name>/report.typ to PDF.
reports:
    nix run .#build-reports

# --- bundles -----------------------------------------------------------

# Formatting, lints, the fast suite, both $JQ_SRC suites, a flake check. Excludes fuzz_scripts/benchmarks.
ci: fmt-check lint test test-excluded flake-check
