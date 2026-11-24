#import "../template.typ": report, diagram

#show: report.with(title: "`label`/`break`, and `limit` for free", date: "2026-11-24")

= Context

The README flagged the lack of `label`/`break` as part of the "eager, not
lazy" limitation: with every sub-expression eagerly materialized into a
`Vec<Value>`, an infinite generator (`repeat`, unconditioned `while`) never
terminates, and there was no way for a *user's* filter to bound one itself
either. Second "known limitations" item tackled, after destructuring.

= A premise checked against real jq -- and found wrong

The starting design assumed `try`/`catch` and `?` must *not* catch a
`break` -- only its matching `label` should see it, the same way the jq
manual's wording ("`break` ... cannot be caught by `catch`") reads at a
glance. Implemented that way first (an explicit `break_label.is_some()`
bypass in both), then checked against real jq before trusting it:

```
$ echo null | jq -c 'label $out | (try break $out catch "caught")'
"caught"
```

`try`/`catch` *does* catch `break`, exactly like any other error --
confirmed further with `catch .` showing the caught value is literally
jq's internal sentinel object (`{"__jq":0}`). The manual's wording is
about `break`'s *specialness* existing at all (an ordinary `catch` has no
syntax to filter by error "type", so catching one unintentionally is a
real, documented footgun), not an enforced immunity. Removed the bypass
from both `Expr::TryCatch` and `Expr::Optional` in `codegen.rs` -- `break`
is just a `JqError` like any other, except for who specifically goes
looking for it.

#diagram(
  boxes: (("break $x\n(raises JqError{break_label:Some(x)})", 0, 0), ("try/catch, ?\n(catches anything, same as a real error)", 1, 0), ("label $x\n(catches only if name matches)", 2, 0)),
  arrows: ((0, 1), (1, 2)),
)

= Design

- `ast::Expr` gained `Label(String, Box<Expr>)` and `Break(String, Span)`
  (the span is for an "undefined label" diagnostic, mirroring `VarRef`'s
  "undefined variable" from the `error-messages` report).
- `jqc_core::error::JqError` gained `break_label: Option<String>`, and a
  `JqError::brk(name)` constructor (value `Null`, matching real jq's own
  internal sentinel being opaque/not meant to be relied on). `bubble`/
  `bubble1`/`partial_for` needed *no* changes -- they already just merge
  an error's `emitted` and re-raise, which is exactly right for a `break`
  too (whatever a label's body produced before the `break` stays).
- `label $x | body` in codegen: push a scope entry, compile `body`, then
  catch only `Err(e) if e.break_label.as_deref() == Some(name)`, converting
  it to `Ok(e.emitted)`; anything else (a real error, or a `break` for a
  *different* name) passes through unchanged -- which is also exactly
  what makes nested same-named labels under recursion work with no
  dynamic id scheme (unlike real jq's own bytecode VM, which assigns each
  `label` activation a fresh runtime id for this reason): each compiled
  `label` is a literal nested Rust closure/match in the call tree, so
  "nearest lexically enclosing match" falls out of ordinary Rust call-
  stack unwinding for free.
- *No new parameter threaded through codegen.* Labels and variables share
  one `scope: Vec<String>` (already threaded through all ~90
  `compile`/`compile_raw` call sites for `VarRef`), distinguished by a
  `\0label\0` prefix no real jq identifier can ever produce (the lexer
  never emits a NUL byte) -- `label_key()`. `VarRef`'s own "did you mean"
  suggestion filters these out so a typo'd `$var` is never offered a label
  name, and vice versa for `break`'s suggestion.

== `limit` comes along for free

With `label`/`break` in place, jq's own `builtin.jq` definition of `limit`
ports over unchanged into `PRELUDE`:

```
def limit(n; exp): if n > 0 then
    label $out | foreach exp as $item (0; .+1;
      $item, if . >= n then break $out else empty end)
  elif n == 0 then empty
  else error("limit doesn't support negative count") end;
```

No codegen changes needed beyond `label`/`break` themselves -- `foreach`
was already supported, and an error/`break` raised from inside its
`extract` clause already propagates correctly through the existing
partial-output machinery.

= Verification

6 new differential tests covering: a `break` ending generation mid-way
through an `as`-bound generator (confirming later elements never run), a
`break` caught by an intervening `try` (the corrected premise above,
re-verified as a regression test), and a `break` correctly skipping a
non-matching inner `label` to reach an outer one by name -- plus 3 for
`limit` (exact count, `n == 0`, `n` larger than the generator). Every
expected value was checked against real `jq` directly before being
written into a test, not hand-derived, after the `try`/`catch` surprise
above. Full suite: 159 differential tests (153 previous + 6), 251
manual-example tests, 15 fuzz-input tests, `cargo clippy --workspace
--all-targets` clean.

= What's left

Eager evaluation itself remains a genuine, structural limitation (an
infinite generator *without* an explicit `label`/`break`/`limit` still
hangs) -- `label`/`break` give a filter author a tool to bound one
themselves, not automatic laziness. `path`/`paths`/`getpath`/`setpath`/
`del` and update-assignment (`|=`, `+=`, …) remain unimplemented and are
tracked separately.
