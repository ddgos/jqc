#import "../template.typ": report, diagram

#show: report.with(title: "Fixing the def_scope memory explosion", date: "2026-11-24")

= Context

While verifying the memory-safeguard work (see the "memory-safeguards"
report), `unused_defs_scaling_check` -- a test asserting that N *unused*
`def`s compile cheaply, the way real `jq` does -- instead ballooned to
tens of GB of RSS for just 400 trivial defs and had to be killed by hand.
`todos.md` was updated with a follow-up task to fix the root cause rather
than rely on the rlimit alone; this report covers that fix.

= Root cause

Each user `def` lowers to a `DefEntry` that snapshots the *entire* scope of
prior defs it closes over, so later calls can be resolved against the
scope as it existed at definition time:

```rust
struct DefEntry {
    name: String,
    arity: usize,
    params: Vec<Param>,
    raw_body: Expr,
    def_scope: Vec<DefEntry>,   // <- the problem
}
```

Defining the $n$-th `def` did `let mut def_scope = scope.to_vec();` --
cloning a `Vec<DefEntry>` of the $n-1$ prior entries. Because `DefEntry`
derives `Clone`, cloning one entry *also* deep-clones its own `def_scope`
field, which is itself a full copy of entries `0..j-1` for entry `j`. Each
entry nested a complete copy of everything before it, nested inside *that*
copy's own copies, and so on:

#diagram(
  boxes: (
    ("entry 0", 0, 0), ("entry 1", 1, 0), ("entry 2", 2, 0), ("entry 3", 3, 0),
    ("entry 1's\ncopy of 0", 1, 1), ("entry 2's\ncopy of 0,1", 2, 1), ("entry 3's\ncopy of 0,1,2", 3, 1),
  ),
  arrows: ((1, 4), (2, 5), (3, 6)),
)

Working out the recurrence for the total heap size of an entry's
`def_scope` after $n$ defs gives $S(n) = 2^n - 1$ times the per-entry base
size -- genuine exponential growth, entirely independent of whether any of
those defs are ever called from anywhere. 400 sequential (unused) defs is
nowhere near computable at that rate; the test was killed partway through
a single recursive `Vec::clone()` call for one of the smaller `n` values
in its sweep, which is also why RSS climbed *gradually* over several
seconds rather than failing instantly -- the clone was recursing through
the nested structure one allocation at a time.

= Fix

`def_scope` only needs to be *read* after it's captured -- nothing ever
mutates a previously-captured scope. That makes it safe to share instead
of deep-copy:

```rust
def_scope: Rc<Vec<DefEntry>>,
```

Cloning a `DefEntry` now bumps a refcount for its `def_scope` instead of
recursively duplicating it, so `scope.to_vec()` and the `Rc::new(def_scope
.clone())` snapshot taken for each new entry are both $O(n)$ in the number
of *live* entries, not $O(2^n)$. `&[DefEntry]`-typed call sites
(`lower(rest, &def_scope)`, `lower(specialized, &entry.def_scope)`) needed
no changes -- `Rc<Vec<DefEntry>>` derefs to `&[DefEntry]` the same way
`Vec<DefEntry>` did.

= Verification

Re-running the exact scenario that triggered the incident, directly (not
through `cargo test`, to watch RSS in real time):

#table(
  columns: 3,
  align: (left, right, right),
  [*defs*], [*before*], [*after*],
  [50], [(part of the run that got killed)], [1.9ms],
  [100], [--], [2.1ms],
  [200], [--], [5.6ms],
  [400], [38GB+ RSS, killed by hand], [16.5ms],
)

`scaling_check` (the other memory-sensitive benchmark, chaining
`def f{i}: f{i-1} | f{i-1};`) improved similarly -- depth 6 went from
multiple seconds with a doubling-per-level trend to 6.6ms total, since
`def_scope` cloning was compounding that test's *other*, inherent cost
(call-by-name body duplication). The remaining cost there is genuine
substitution duplication, not a bug, and is unaffected by this fix.

`cargo build/test/clippy/fmt --workspace` all pass; the memory cap from
the previous report remains in place as a backstop for whatever future
cost cliffs show up next.
