# Todo

## Context

For each of the items in this document, produce a typst document about what was
done, including diagrams if applicable. Remove tasks from this list once they
are complete. Add tasks to this list if necessary. Keep changes tracked well in
git. Do these in whichever order makes the most sense.

## Tasks

Remove the binary produced by this crate.

I'm not sure that the benchmarks comparing this project with true `jq` is fair.
The benchmarks for this project are not interacting with the file i/o system so
naturally have a significant advantage. Remove the true `jq` benchmarks if this
is the case. Add a task at the end of the task list to make some fair benchmarks
(such as by using the result of the binary producing derivation in a later task)
to compare against true `jq`.

Consider adding `gnuplot` to the dev shell for producing benchmark graphs, but
don't discount using `plotters` since it seems like a good project too.

Improve compliance by removing xfail from a test, and making it go green. Do not
remove this task until there are no longer xfail tests. Currently one xfail:
`repeat_error_optional` in `jqc/tests/differential.rs` (and its twin in
`jqc-manual-tests-macros`'s `KNOWN_RUNTIME_DISCREPANCIES`) -- `[repeat(.*2,
error)?]` on input `1` gives `[]`, real jq gives `[2]`.

The jq manual mentions math functions. It may be the case that the `jq` provided
by nixpkgs-unstable does not provide all of these functions. Override the
provided jq package such that all of the math functions work in the nix develop
environment's jq.

Consider using github:oxalica/rust-overlay, github:numtide/flake-utils, and
https://github.com/ipetkov/crane for the flake.

Allow using https://github.com/criterion-rs/cargo-criterion in the dev shell
so that `cargo criterion` works as expected.

The dev environment should have access to every version of `jq` supported by the
crate simultaneously so that tests can be run from a single environment.
Currently if we want to test that 1.6, 1.7 and 1.8 are supported we would have
to run three separate development environments.
Consider accessing old `jq` versions by overriding the `version` attribute for
the nixpkgs jq package, rather than accessing other nixpkgs sources.

Implement a `inputs.nixpkgs.follows` attribute for our flake, so that users of
the flake can reuse a single version of nixpkgs.

Make some feature of the flake be that it exposes a nix function specifying a
derivation given a jq source file which is the compiled binary for that script.

All development work should be assumed to take place within the nix dev shell.
I think I've seen some work assuming this isn't the case. Please audit and fix
anywhere were better utilisation of nix can be made.

When I run `nix build` I see an error. When I run `nix develop -c cargo test`,
I don't. Seems like `jq` is not on path for `nix build`.

Ensure `nix flake check` runs all of the tests defined for this project.

Make some feature of the flake be that it exposes a nix function specifying a
derivation given a jq source file which is the compiled binary for that script.
Generating the program should be the result of `nix run . -- <script-file>` or
similar.

Implement a `inputs.nixpkgs.follows` attribute for our flake, so that users of
the flake can reuse a single version of nixpkgs.

Work on the known limitations. Fix any which can reasonably be fixed. Only
remove this task once limitations that are genuine limitations of the project
remain.

Improve upon the script and input fuzzing. Part of this should be using
arbitrary utf-8 strings for the scripts and input. Part of this should be to
work up the proptest generators until every valid script and input will in
theory be tested. Do not stop until both of these are true.

Identify some area of the code that could be tidied up, and refactor as desired.
Do not remove this task until the code is as polished as possible.

Look at the `jq` source code to identify more test cases that could be generated
automatically, like the tests from the examples in the manual.

When I run `nix build` I see an error. When I run `nix develop -c cargo test`,
I don't. Seems like `jq` is not on path for `nix build`.

Ensure `nix flake check` runs all of the tests defined for this project. Look at
README.md for some of the current tests.

Consider accessing old `jq` versions by overriding the `version` attribute for
the nixpkgs jq package, rather than accessing other nixpkgs sources.

All development work should be assumed to take place within the nix dev shell.
I think I've seen some work assuming this isn't the case. Please audit and fix
anywhere were better utilisation of nix can be made.

Consider using github:oxalica/rust-overlay and github:numtide/flake-utils for
the flake to simplify.

The dev environment should have access to every version of `jq` supported by the
crate simultaneously so that tests can be run from a single environment.
Currently if we want to test that 1.6, 1.7 and 1.8 are supported we would have
to run three separate development environments.

Look for opportunities to use SIMD optimisation techniques. Implement as a
new implementation of the codegen trait.

Persue whatever optimisations possible. Always keep as separate implementations
of the trait so that comparisons between implementations are easier to make.

Look for opportunities to use SIMD optimisation techniques. Implement as a
new implementation of the codegen trait.

Persue whatever optimisations possible. Always keep as separate implementations
of the trait so that comparisons between implementations are easier to make.
