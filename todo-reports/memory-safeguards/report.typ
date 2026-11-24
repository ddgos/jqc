#import "../template.typ": report, diagram

#show: report.with(title: "Memory safeguards for heavy tests", date: "2026-11-24")

= Context

`todos.md` asked for a safeguard around the more memory-intensive tests so
that a runaway test fails cleanly before the kernel's OOM reaper has to step
in, with a default limit of "64GB on this machine". That number turned out
to be wrong for *this* machine, discovered the hard way while verifying the
safeguard — see @incident.

= What was added

`jqc-macros/src/bench.rs` (the three `#[ignore]`d compile-time benchmarks
identified as the memory-intensive tests in this codebase) now calls a
`cap_memory()` helper before doing any work:

- Reads physical RAM via `sysconf(_SC_PHYS_PAGES) * sysconf(_SC_PAGE_SIZE)`.
- Defaults to `min(64GiB, physical_ram / 2)`, overridable with the
  `JQC_TEST_MEM_LIMIT_GB` env var.
- Applies that as `setrlimit(RLIMIT_AS, ...)` on the test process.

When a test's address-space use crosses the limit, the global allocator's
`handle_alloc_error` aborts the process immediately — a fast, visible
failure (`SIGABRT`, exit 134) instead of unbounded growth that eventually
invites the kernel OOM killer, which has no reason to prefer killing *this*
process over any other one on the machine.

#diagram(
  boxes: (("alloc request", 0, 0), ("over RLIMIT_AS?", 1, 0), ("abort (clean)", 2, 0)),
  arrows: ((0, 1), (1, 2)),
)

= Incident during verification <incident>

Running the newly-capped `unused_defs_scaling_check` to confirm the
safeguard worked revealed two things at once:

1. This machine has *38GiB* of RAM (plus 50GiB swap), not 64GB+. A flat
   64GiB `RLIMIT_AS` would have let the test consume essentially all
   physical memory — and started swapping — before the limit ever
   triggered. The test was observed hitting 94% of system RAM (~38GB RSS)
   in about ten seconds and was killed by hand before it got worse.
2. The test itself exposed a real bug: 400 *unused*, independent `def`s
   ballooned to tens of GB of RSS. `lower_funcdef` clones the whole
   recursively-nested `def_scope` at every `def` regardless of whether it's
   ever called, so memory use grows much faster than the number of defs
   would suggest. This is tracked as its own follow-up task in `todos.md`
   ("Fix the memory explosion in `lower_funcdef`") rather than patched here,
   since the safeguard's job is to contain the symptom, not fix the root
   cause.

After switching the default to scale with physical RAM (half of it, capped
at 64GiB), re-running the same test aborted cleanly at ~19GB RSS — about
half of this machine's 38GiB — well short of exhausting it.

= Verification

- Re-ran `unused_defs_scaling_check` under the new default: process printed
  `memory allocation of N bytes failed`, aborted (exit 134) at ~19GB RSS,
  and the machine's memory usage returned to baseline immediately after.
- `cargo build/test/clippy/fmt --workspace` all still pass; the default
  (non-`#[ignore]`d) test suite is unaffected since it never calls
  `cap_memory()`.
