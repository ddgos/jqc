# Todo

## Context

For each of the items in this document, produce a typst document about what was
done, including diagrams if applicable. Remove tasks from this list once they
are complete. Add tasks to this list if necessary. Keep changes tracked well in
git. Do these in whichever order makes the most sense.

## Tasks

Work on the known limitations. Fix any which can reasonably be fixed. Only
remove this task once limitations that are genuine limitations of the project
remain.

Improve upon the script and input fuzzing. Part of this should be using
arbitrary utf-8 strings for the scripts and input. Part of this should be to
work up the proptest generators until every valid script and input will in
theory be tested. Do not stop until both of these are true.

Identify some area of the code that could be tidied up, and refactor as desired.
Do not remove this task until the code is as polished as possible.

Improve the quality of the error messages produced by `jqc-build`. Rust's error
messages should be seen as the gold standard, where the location of the error is
pinned down and fixes are suggested.

Look at the `jq` source code (not just the manual) to identify more test cases
that could be generated automatically -- e.g. jq's own C test suite
(`tests/jq.test` and similar in a jq source checkout), as distinct from the
manual-example generator that already exists (`jqc-manual-tests`).

Make the cli of the generated binaries use the same options as `jq` itself, and
output in colour etc the same as `jq`, so it can be a drop in replacement.

Look for opportunities to use SIMD optimisation techniques. Implement as a
new implementation of the codegen trait.

Persue whatever optimisations possible. Always keep as separate implementations
of the trait so that comparisons between implementations are easier to make.
