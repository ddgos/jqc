#import "../template.typ": report, diagram

#show: report.with(title: "Destructuring patterns in `as` bindings", date: "2026-11-24")

= Context

The README's "Known limitations" called out "No destructuring patterns in
`as` bindings, only a single `\$x`" -- real jq supports `EXPR as [$a, $b]
| ...` and `EXPR as {a: $a, $b} | ...`, nestable and with computed keys.
Picked as the first "known limitations" item to close: self-contained
(touches binding sites only, not the rest of the language), and a genuine
gap rather than a structural tradeoff like eager evaluation.

= Design: desugar at lowering time, leave codegen untouched

Rather than teaching `codegen.rs` a new node, destructuring is desugared
in `lower.rs` into the `Expr::VarBind`/`Expr::Index` nodes codegen already
knows how to compile. `ast::Pattern` is the new piece:

```rust
enum Pattern {
    Var(String),
    Array(Vec<Pattern>),
    Object(Vec<(Expr, Pattern)>),  // key expr (as in ObjEntry), sub-pattern
}
```

`Expr::VarBind`'s second field changed from `String` to `Pattern` (a plain
`$x` is just `Pattern::Var`, so every existing call site either stays
untouched -- `rewrite_call0`'s generic recursion doesn't care what type
the field is -- or gets a one-line wrap, e.g. a `def`'s `$x` value-param
desugaring now builds `Pattern::Var(param.name.clone())`).

`lower()`'s `VarBind` arm fully lowers `source` and `body` first, then
hands off to `bind_pattern`, which expands a pattern into nested `$x`
`VarBind`s over `Index` lookups -- informally:

```
source as [$a, $b]  |  body
  ⇒
source as $__jqc_destr
  | $__jqc_destr[0] as $a
  | $__jqc_destr[1] as $b
  | body
```

#diagram(
  boxes: (("parse\n(Pattern)", 0, 0), ("lower\n(fully lower source/body)", 1, 0), ("bind_pattern\n(nested VarBind+Index)", 2, 0), ("codegen\n(only ever sees Pattern::Var)", 3, 0)),
  arrows: ((0, 1), (1, 2), (2, 3)),
)

Reusing the fixed name `__jqc_destr` at every nesting level (no
fresh-name counter) is safe: each is a `VarBind` lexically nested inside
the last, and codegen already compiles each sub-expression into its own
nested Rust block (see `codegen.rs`'s module doc comment) -- ordinary
block scoping shadows it the same way two sibling `let`s in nested blocks
would. `codegen.rs` only ever sees `Pattern::Var` by the time it runs;
`Pattern::Array`/`Object` reaching it is an internal-error sentinel, the
same convention used for `FuncDef`/`RecurseDefault` elsewhere in that file.

== A real semantics question, checked against real jq

A computed object-pattern key, `{(EXPR): PATTERN}`, raised a question the
jq manual doesn't spell out: does `EXPR` see the outer `.`, or the value
currently being destructured? Checked directly:

```
$ echo '{"foo":{"k":"x","x":99},"k":"wrong"}' | jq -c '.foo as {(.k): $v} | $v'
99
```

`.k` resolved against `.foo`'s value (giving `"x"`, then `.x` → `99`), not
the outer `.` (whose own `"k"` is `"wrong"` and was correctly ignored).
`bind_pattern` gets this by piping the synthetic temp through the key
expression before indexing with it (`$__jqc_destr | key_expr`, via a
plain `Expr::Pipe` -- reusing Pipe's existing "rebind `.` to the left
side's output" semantics rather than inventing new codegen machinery),
which is a no-op for the common case of a plain string-literal key.

`contains_call`/`rewrite_call0` (used for recursive-def detection and
call-by-name substitution) also gained `pattern_contains_call`/
`rewrite_pattern` counterparts, so a call hiding inside a pattern's
computed key -- `def f: . as {(f): $x} | $x;`, however unlikely in
practice -- is still seen correctly rather than silently skipped.

= Verification

11 new differential tests (`jqc/tests/differential.rs`), each checked
against real `jq`, not just hand-predicted output: array destructuring
(exact-length and short/missing-element), object destructuring (plain,
`$name` shorthand, missing key → `null`), the computed-key case above,
nesting (`[$a, {b: $b}]`), and destructuring mid-pipe. All 153
differential tests (142 previous + 11 new), 251 manual-example tests, 15
fuzz-input tests, and `cargo clippy --workspace --all-targets` pass with
no regressions.

= What's left

`?//` alternative patterns (`as [$a] ?// {a: $a}`, for matching one of
several shapes) aren't implemented -- `parse_pattern` has nowhere to
recognize `?//`, so it fails with a normal (now well-located, per the
`error-messages` report) parse error rather than miscompiling. Noted in
the README; not pursued here since it's a distinct, separable feature
rather than a corner of the same one.
