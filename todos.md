# Todo

## Context

For each of the items in this document, produce a typst document about what was
done, including diagrams if applicable. Remove tasks from this list once they
are complete. Add tasks to this list if necessary. Keep changes tracked well in
git. Do these in whichever order makes the most sense.

## Tasks

Benchmark the actual `jq` binary to compare with.

Generate a code coverage report from tests.

Keep a record of the pareto front to see how performance improves.

Add functionality to specify which `jq` version should be used for the macro.
This should default to the most recent `jq` version (whatever is supported by
nixpkgs-unstable). Old versions of `jq` should be tested against by including
them in a nix compatible way for reproducable builds.

Write a proc macro that runs generates tests from a path to the `jq` source code
repository. This source code should be included as a nix flake dev dependency.
In particular, generate tests from the examples included in the jq manual. This
procedure should also be `jq` version aware.

Write tests to cover every known discrepancy between our results and the true
`jq` results. These should be marked as xfail, or the Rust equivalent.

Improve compliance by removing xfail from a test, and making it go green. Do not
remove this task until there are no longer xfail tests.

Once codegen is generic, and compliance with `jq` is 100%, add a feature flag to
the crate to enable multi-core utilisation by the generated code. This codegen
should be separate from the original codegen and implement the codegen trait. 

The jq manual mentions math functions. It may be the case that the `jq` provided
by nixpkgs-unstable does not provide all of these functions. Override the
provided jq package such that all of the math functions work in the nix develop
environment's jq.

Improve upon the script and input fuzzing. Part of this should be using
arbitrary utf-8 strings for the scripts and input. Part of this should be to
work up the proptest generators until every valid script and input will in
theory be tested. Do not stop until both of these are true.

Make some feature of the flake be that it can generate derivations given a jq
source file which is the compiled binary for that script. If this doesn't make
sense then explain, since my nix knowledge is imperfect.

Look for opportunities to use SIMD optimisation techniques. Implement as a
new implementation of the codegen trait.

Persue whatever optimisations possible. Always keep as separate implementations
of the trait so that comparisons between implementations are easier to make.

Identify some area of the code that could be tidied up, and refactor as desired.
Do not remove this task until the code is as polished as possible.
