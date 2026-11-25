# Todo

## Context

For each of the items in this document, produce a typst document about what was
done, including diagrams if applicable. Remove tasks from this list once they
are complete. Add tasks to this list if necessary. Keep changes tracked well in
git. Do these in whichever order makes the most sense.

## Tasks

Work on the known limitations. Fix any which can reasonably be fixed. Only
remove this task once limitations that are genuine limitations of the project
remain. If at any point new bugs are identified, they should be isolated with
xfail tests, then fixed as part of this task, rather than fixed immediately.

Improve upon the script and input fuzzing. Part of this should be using
arbitrary utf-8 strings for the scripts and input. Part of this should be to
work up the proptest generators until every valid script and input will in
theory be tested. Do not stop until both of these are true. Also test
arbitrary bytes (not just arbitrary valid utf-8) as input -- piped data isn't
guaranteed to be valid utf-8, so differences from real `jq` on invalid-utf-8
input could currently be going unnoticed.

Identify some area of the code that could be tidied up, and refactor as desired.
An example of this would be seemingly duplicate functions for `fn run_real_jq`.
Do not remove this task until the code is as polished as possible.

Persue whatever optimisations possible. Always keep as separate implementations
of the trait so that comparisons between implementations are easier to make.

Investigate the feasibility (and implement if it makes sense) for jqc-build, or
the jpc-build flake application to build the resulting binary into the nix store
so that I can run `"$(nix build <flake> <script>)"` and it would run.

Audit the existing code for situations where an external crate may allow for
simplifying this project.
The most downloaded crates will be good candidates
https://crates.io/crates?sort=downloads

Consider using a different allocator as per
https://docs.rs/simd-json/latest/simd_json/#allocator
