//! Desugaring + user-`def` resolution pass, run after parsing and before
//! codegen.
//!
//! - A handful of builtins (`map`, `select`, `recurse`, `any`, `all`, …) are
//!   implemented as a small jq-source "prelude" that gets parsed together
//!   with the user script, rather than special-cased in codegen.
//! - Every `def` is either fully inlined at its call sites (call-by-name,
//!   matching jq's own semantics for filter params; value params `$x` are
//!   desugared to an `as` binding) or, if it recurses, compiled into a real
//!   recursive Rust function.
//! - Mutually-recursive and parameterized-recursive defs are out of scope
//!   and rejected with a clear error rather than silently miscompiled.

use std::rc::Rc;

use crate::ast::{Expr, ObjEntry, Param, Pattern, StrPart};
use crate::diagnostics::{Diagnostic, Span};
use crate::parser::parse_program;
use crate::version::JqVersion;

pub type LResult<T> = Result<T, Diagnostic>;

const PRELUDE: &str = r#"
def map(f): [.[] | f];
def select(f): if f then . else empty end;
def values: select(. != null);
def recurse(f): def __jqc_rec_r: ., (f | __jqc_rec_r); __jqc_rec_r;
def recurse: recurse(.[]?);
def in(xs): . as $__jqc_in_x | xs | has($__jqc_in_x);
def any(generator; cond): reduce (generator|cond) as $__jqc_any_x (false; . or $__jqc_any_x);
def any(cond): any(.[]; cond);
def any: any(.);
def all(generator; cond): reduce (generator|cond) as $__jqc_all_x (true; . and $__jqc_all_x);
def all(cond): all(.[]; cond);
def all: all(.);
def while(cond; update): def __jqc_while_r: if cond then ., (update | __jqc_while_r) else empty end; __jqc_while_r;
def until(cond; update): def __jqc_until_u: if cond then . else (update | __jqc_until_u) end; __jqc_until_u;
def repeat(f): def __jqc_repeat_r: f, __jqc_repeat_r; __jqc_repeat_r;
def not: if . then false else true end;
def error(msg): msg | error;
def to_entries: [keys_unsorted[] as $k | {key: $k, value: .[$k]}];
def from_entries: map({ (.key // .Key // .name // .Name):
  if has("value") then .value else .Value end }) | add // {};
def with_entries(f): to_entries | map(f) | from_entries;
"#;

/// `user_src`'s char offset and length within the string `parse_and_lower`
/// actually lexes/parses/lowers (`PRELUDE` plus a wrapping `(`/`)`) --
/// exposed so a caller that also runs [`crate::codegen`] on the resulting
/// `Expr` (whose spans are still relative to that larger string, since
/// codegen runs after `parse_and_lower` has already returned) can relocate
/// a codegen-stage error back to `user_src`'s own coordinates the same way
/// `parse_and_lower` relocates its own parse/lower-stage errors.
pub fn user_src_window(user_src: &str) -> (usize, usize) {
    (PRELUDE.chars().count() + 1 /* '\n' */, user_src.chars().count())
}

pub fn parse_and_lower(user_src: &str, version: JqVersion) -> LResult<Expr> {
    // Every span produced while lexing/parsing/lowering `full` is relative
    // to `full`, not `user_src` -- translate back before handing an error
    // further up, so a caller rendering against `user_src` (not `full`,
    // which it was never given) points at the right place, and so a
    // "found ..." token description never names a token that only exists
    // because of PRELUDE's own trailing wrapper (there is none now --
    // `user_src` is simply the last thing in `full`, not parenthesized,
    // since the prelude's final `;` already closes off everything before
    // it and nothing here needs user_src's own top-level parse bounded any
    // tighter than `parse_pipe` already bounds it). A span that lands
    // entirely outside the window would be a bug in `PRELUDE` itself, not
    // explainable in terms of the user's own source, so `relocate` drops
    // it rather than mistranslating; one that runs past the very end
    // (reaching real EOF) is clamped instead, so it still gets *a*
    // location, right after the user's last character.
    let (offset, len) = user_src_window(user_src);
    let full = format!("{PRELUDE}\n{user_src}");
    let ast = parse_program(&full, version).map_err(|e| e.relocate(offset, len))?;
    lower(ast, &[]).map_err(|e| e.relocate(offset, len))
}

#[derive(Clone)]
struct DefEntry {
    name: String,
    arity: usize,
    params: Vec<Param>,
    raw_body: Expr,
    // `Rc`, not `Vec`: each def_scope already contains the full history of
    // prior defs, so a plain `Vec<DefEntry>` clone (e.g. in `scope.to_vec()`
    // below) would recursively deep-clone every entry's own def_scope too --
    // O(2^n) total memory for n sequential defs, independent of whether any
    // of them are ever called. Sharing via `Rc` makes every such clone O(1).
    def_scope: Rc<Vec<DefEntry>>,
}

fn lower(expr: Expr, scope: &[DefEntry]) -> LResult<Expr> {
    match expr {
        Expr::Identity | Expr::Null | Expr::Bool(_) | Expr::Number(_) => Ok(expr),
        // `..` always resolves to the prelude's `recurse` def, so this
        // call can never hit lower_call's "unresolved, pass through to
        // codegen" path that would need a real span -- 0..0 is a dummy.
        Expr::RecurseDefault => lower_call("recurse".to_string(), vec![], 0..0, scope),
        Expr::VarRef(..) | Expr::RecCall(_) => Ok(expr),
        Expr::Field {
            base,
            name,
            optional,
        } => Ok(Expr::Field {
            base: Box::new(lower(*base, scope)?),
            name,
            optional,
        }),
        Expr::Index {
            base,
            index,
            optional,
        } => Ok(Expr::Index {
            base: Box::new(lower(*base, scope)?),
            index: Box::new(lower(*index, scope)?),
            optional,
        }),
        Expr::Slice {
            base,
            from,
            to,
            optional,
        } => Ok(Expr::Slice {
            base: Box::new(lower(*base, scope)?),
            from: from.map(|e| lower(*e, scope)).transpose()?.map(Box::new),
            to: to.map(|e| lower(*e, scope)).transpose()?.map(Box::new),
            optional,
        }),
        Expr::IterateAll { base, optional } => Ok(Expr::IterateAll {
            base: Box::new(lower(*base, scope)?),
            optional,
        }),
        Expr::Pipe(lhs, rhs) => Ok(Expr::Pipe(
            Box::new(lower(*lhs, scope)?),
            Box::new(lower(*rhs, scope)?),
        )),
        Expr::Comma(lhs, rhs) => Ok(Expr::Comma(
            Box::new(lower(*lhs, scope)?),
            Box::new(lower(*rhs, scope)?),
        )),
        Expr::Str(parts) => Ok(Expr::Str(
            parts
                .into_iter()
                .map(|p| match p {
                    StrPart::Lit(s) => Ok(StrPart::Lit(s)),
                    StrPart::Interp(e) => Ok(StrPart::Interp(Box::new(lower(*e, scope)?))),
                })
                .collect::<LResult<Vec<_>>>()?,
        )),
        Expr::ArrayConstruct(inner) => Ok(Expr::ArrayConstruct(
            inner.map(|e| lower(*e, scope)).transpose()?.map(Box::new),
        )),
        Expr::ObjectConstruct(entries) => Ok(Expr::ObjectConstruct(
            entries
                .into_iter()
                .map(|ObjEntry { key, val }| {
                    Ok(ObjEntry {
                        key: lower(key, scope)?,
                        val: lower(val, scope)?,
                    })
                })
                .collect::<LResult<Vec<_>>>()?,
        )),
        Expr::Neg(x) => Ok(Expr::Neg(Box::new(lower(*x, scope)?))),
        Expr::BinOp(op, lhs, rhs) => Ok(Expr::BinOp(
            op,
            Box::new(lower(*lhs, scope)?),
            Box::new(lower(*rhs, scope)?),
        )),
        Expr::And(lhs, rhs) => Ok(Expr::And(
            Box::new(lower(*lhs, scope)?),
            Box::new(lower(*rhs, scope)?),
        )),
        Expr::Or(lhs, rhs) => Ok(Expr::Or(
            Box::new(lower(*lhs, scope)?),
            Box::new(lower(*rhs, scope)?),
        )),
        Expr::Alt(lhs, rhs) => Ok(Expr::Alt(
            Box::new(lower(*lhs, scope)?),
            Box::new(lower(*rhs, scope)?),
        )),
        Expr::If(branches, else_branch) => Ok(Expr::If(
            branches
                .into_iter()
                .map(|(cond, then_e)| Ok((lower(cond, scope)?, lower(then_e, scope)?)))
                .collect::<LResult<Vec<_>>>()?,
            else_branch
                .map(|e| lower(*e, scope))
                .transpose()?
                .map(Box::new),
        )),
        Expr::TryCatch(body, catch) => Ok(Expr::TryCatch(
            Box::new(lower(*body, scope)?),
            catch.map(|e| lower(*e, scope)).transpose()?.map(Box::new),
        )),
        Expr::Optional(body) => Ok(Expr::Optional(Box::new(lower(*body, scope)?))),
        Expr::VarBind(source, pattern, body) => {
            let source = lower(*source, scope)?;
            let body = lower(*body, scope)?;
            bind_pattern(source, pattern, body, scope)
        }
        Expr::Reduce {
            source,
            var,
            init,
            update,
        } => Ok(Expr::Reduce {
            source: Box::new(lower(*source, scope)?),
            var,
            init: Box::new(lower(*init, scope)?),
            update: Box::new(lower(*update, scope)?),
        }),
        Expr::Foreach {
            source,
            var,
            init,
            update,
            extract,
        } => Ok(Expr::Foreach {
            source: Box::new(lower(*source, scope)?),
            var,
            init: Box::new(lower(*init, scope)?),
            update: Box::new(lower(*update, scope)?),
            extract: extract.map(|e| lower(*e, scope)).transpose()?.map(Box::new),
        }),
        Expr::RecFn0 { name, body, rest } => Ok(Expr::RecFn0 {
            name,
            body: Box::new(lower(*body, scope)?),
            rest: Box::new(lower(*rest, scope)?),
        }),
        Expr::FuncDef {
            name,
            params,
            body,
            rest,
            span,
        } => lower_funcdef(name, params, *body, *rest, span, scope),
        Expr::Call(name, args, span) => lower_call(name, args, span, scope),
    }
}

/// Desugars `source as PATTERN | body` into nested plain-`$x` `VarBind`s
/// over `Index` lookups -- e.g. `source as [$a, $b] | body` becomes
/// (informally) `source as $__jqc_destr | $__jqc_destr[0] as $a |
/// $__jqc_destr[1] as $b | body`. `source` and `body` must already be
/// lowered (this only builds new `Index`/`VarRef` glue around them, which
/// contains no calls needing resolution); a computed `(expr): pattern` key
/// still needs lowering itself, since it comes straight from the parser.
/// Reusing the fixed name `__jqc_destr` for every nesting level is safe --
/// each is a `VarBind` lexically nested inside the last, so codegen's
/// ordinary Rust block scoping shadows it the same way two sibling `let`s
/// in nested blocks would, matching how the rest of codegen already
/// avoids needing a fresh-name counter (see the module doc comment).
fn bind_pattern(source: Expr, pattern: Pattern, body: Expr, scope: &[DefEntry]) -> LResult<Expr> {
    match pattern {
        Pattern::Var(name) => Ok(Expr::VarBind(
            Box::new(source),
            Pattern::Var(name),
            Box::new(body),
        )),
        Pattern::Array(elems) => {
            let tmp = "__jqc_destr".to_string();
            let mut acc = body;
            for (i, elem) in elems.into_iter().enumerate().rev() {
                let value = Expr::Index {
                    base: Box::new(Expr::VarRef(tmp.clone(), 0..0)),
                    index: Box::new(Expr::Number(i as f64)),
                    optional: false,
                };
                acc = bind_pattern(value, elem, acc, scope)?;
            }
            Ok(Expr::VarBind(Box::new(source), Pattern::Var(tmp), Box::new(acc)))
        }
        Pattern::Object(entries) => {
            let tmp = "__jqc_destr".to_string();
            let mut acc = body;
            for (key, elem) in entries.into_iter().rev() {
                let key = lower(key, scope)?;
                // A computed `(expr): pattern` key evaluates with `.` set
                // to the value being destructured (confirmed against real
                // jq: `.foo as {(.k): $v} | $v` reads `.k` off `.foo`'s
                // value, not the outer `.`) -- piping `tmp` through `key`
                // gets that for free via Pipe's ordinary "rebind `.` to
                // the left side's output" semantics; a no-op for the
                // common case (a plain `Str` literal key, which ignores
                // its input).
                let key_against_value =
                    Expr::Pipe(Box::new(Expr::VarRef(tmp.clone(), 0..0)), Box::new(key));
                let value = Expr::Index {
                    base: Box::new(Expr::VarRef(tmp.clone(), 0..0)),
                    index: Box::new(key_against_value),
                    optional: false,
                };
                acc = bind_pattern(value, elem, acc, scope)?;
            }
            Ok(Expr::VarBind(Box::new(source), Pattern::Var(tmp), Box::new(acc)))
        }
    }
}

fn lower_funcdef(
    name: String,
    params: Vec<Param>,
    body: Expr,
    rest: Expr,
    span: Span,
    scope: &[DefEntry],
) -> LResult<Expr> {
    let arity = params.len();
    let recursive = contains_call(&body, &name, arity);
    if recursive {
        if arity != 0 {
            return Err(Diagnostic::at(
                span,
                format!("recursive function '{name}/{arity}' with parameters is not supported"),
            )
            .help("only 0-argument recursive defs can be compiled (parameterized defs are \
                   inlined call-by-name, which can't terminate if they recurse); \
                   try a `reduce`/`foreach` loop instead"));
        }
        let body = rewrite_self_calls(body, &name);
        let rest = rewrite_self_calls(rest, &name);
        return Ok(Expr::RecFn0 {
            name,
            body: Box::new(lower(body, scope)?),
            rest: Box::new(lower(rest, scope)?),
        });
    }
    let mut def_scope = scope.to_vec();
    let entry = DefEntry {
        name,
        arity,
        params,
        raw_body: body,
        def_scope: Rc::new(def_scope.clone()),
    };
    def_scope.push(entry);
    lower(rest, &def_scope)
}

fn lower_call(name: String, args: Vec<Expr>, span: Span, scope: &[DefEntry]) -> LResult<Expr> {
    if let Some(entry) = scope
        .iter()
        .rev()
        .find(|e| e.name == name && e.arity == args.len())
    {
        // Arguments are filters written at the *call site*, so they must be
        // resolved against the caller's `scope` -- e.g. `map(select(f))`
        // needs `select` visible here, even though `map`'s own def_scope
        // (captured at `map`'s definition, before `select` existed yet in
        // this prelude) doesn't have it. Lowering them up front, before
        // splicing into the callee's body, also makes the substitution
        // idempotent: re-lowering an already-lowered subtree under the
        // callee's def_scope later is a no-op for it.
        let args = args
            .into_iter()
            .map(|a| lower(a, scope))
            .collect::<LResult<Vec<_>>>()?;
        let mut specialized = entry.raw_body.clone();
        for (param, arg) in entry.params.iter().zip(args.iter()) {
            if !param.is_value {
                specialized = subst_call0(specialized, &param.name, arg);
            }
        }
        for (param, arg) in entry.params.iter().zip(args).rev() {
            if param.is_value {
                specialized = Expr::VarBind(
                    Box::new(arg),
                    Pattern::Var(param.name.clone()),
                    Box::new(specialized),
                );
            }
        }
        return lower(specialized, &entry.def_scope);
    }
    let lowered_args = args
        .into_iter()
        .map(|a| lower(a, scope))
        .collect::<LResult<Vec<_>>>()?;
    Ok(Expr::Call(name, lowered_args, span))
}

/// Does `expr` call `name/arity` anywhere (used to detect recursive defs)?
/// Stops descending into a nested def/recursive-fn body that redefines the
/// same name at the same arity, since that call refers to the inner def.
fn contains_call(expr: &Expr, name: &str, arity: usize) -> bool {
    match expr {
        Expr::Call(n, args, _) => {
            (n == name && args.len() == arity) || args.iter().any(|a| contains_call(a, name, arity))
        }
        Expr::RecCall(n) => n == name && arity == 0,
        Expr::Identity
        | Expr::RecurseDefault
        | Expr::Null
        | Expr::Bool(_)
        | Expr::Number(_)
        | Expr::VarRef(..) => false,
        Expr::Field { base, .. } | Expr::IterateAll { base, .. } => {
            contains_call(base, name, arity)
        }
        Expr::Neg(x) | Expr::Optional(x) => contains_call(x, name, arity),
        Expr::Index { base, index, .. } => {
            contains_call(base, name, arity) || contains_call(index, name, arity)
        }
        Expr::Slice { base, from, to, .. } => {
            contains_call(base, name, arity)
                || from
                    .as_deref()
                    .is_some_and(|x| contains_call(x, name, arity))
                || to.as_deref().is_some_and(|x| contains_call(x, name, arity))
        }
        Expr::Pipe(a, b)
        | Expr::Comma(a, b)
        | Expr::And(a, b)
        | Expr::Or(a, b)
        | Expr::Alt(a, b)
        | Expr::BinOp(_, a, b) => contains_call(a, name, arity) || contains_call(b, name, arity),
        Expr::Str(parts) => parts.iter().any(|p| match p {
            StrPart::Lit(_) => false,
            StrPart::Interp(e) => contains_call(e, name, arity),
        }),
        Expr::ArrayConstruct(inner) => inner
            .as_deref()
            .is_some_and(|e| contains_call(e, name, arity)),
        Expr::ObjectConstruct(entries) => entries
            .iter()
            .any(|e| contains_call(&e.key, name, arity) || contains_call(&e.val, name, arity)),
        Expr::If(branches, else_branch) => {
            branches
                .iter()
                .any(|(c, t)| contains_call(c, name, arity) || contains_call(t, name, arity))
                || else_branch
                    .as_deref()
                    .is_some_and(|e| contains_call(e, name, arity))
        }
        Expr::TryCatch(body, catch) => {
            contains_call(body, name, arity)
                || catch
                    .as_deref()
                    .is_some_and(|e| contains_call(e, name, arity))
        }
        Expr::VarBind(source, pattern, body) => {
            contains_call(source, name, arity)
                || pattern_contains_call(pattern, name, arity)
                || contains_call(body, name, arity)
        }
        Expr::Reduce {
            source,
            init,
            update,
            ..
        } => {
            contains_call(source, name, arity)
                || contains_call(init, name, arity)
                || contains_call(update, name, arity)
        }
        Expr::Foreach {
            source,
            init,
            update,
            extract,
            ..
        } => {
            contains_call(source, name, arity)
                || contains_call(init, name, arity)
                || contains_call(update, name, arity)
                || extract
                    .as_deref()
                    .is_some_and(|e| contains_call(e, name, arity))
        }
        Expr::FuncDef {
            name: inner_name,
            params,
            body,
            rest,
            ..
        } => {
            let shadowed = inner_name == name && params.len() == arity;
            (!shadowed && contains_call(body, name, arity)) || contains_call(rest, name, arity)
        }
        Expr::RecFn0 {
            name: inner_name,
            body,
            rest,
        } => {
            let shadowed = inner_name == name && arity == 0;
            (!shadowed && contains_call(body, name, arity)) || contains_call(rest, name, arity)
        }
    }
}

/// As [`contains_call`], but over a destructuring pattern's own computed
/// `(expr): pattern` key expressions (`Pattern::Var` binds a name, not an
/// expression, so it can never itself contain a call).
fn pattern_contains_call(pattern: &Pattern, name: &str, arity: usize) -> bool {
    match pattern {
        Pattern::Var(_) => false,
        Pattern::Array(elems) => elems.iter().any(|p| pattern_contains_call(p, name, arity)),
        Pattern::Object(entries) => entries
            .iter()
            .any(|(k, p)| contains_call(k, name, arity) || pattern_contains_call(p, name, arity)),
    }
}

/// Replace every 0-arg `Call(name, [])` in `expr` with `RecCall(name)`,
/// marking it as a call to the recursive function currently being compiled.
fn rewrite_self_calls(expr: Expr, name: &str) -> Expr {
    rewrite_call0(expr, name, &mut || Expr::RecCall(name.to_string()))
}

/// Replace every 0-arg `Call(pname, [])` in `expr` with a fresh clone of
/// `replacement` (call-by-name filter-argument substitution).
fn subst_call0(expr: Expr, pname: &str, replacement: &Expr) -> Expr {
    rewrite_call0(expr, pname, &mut || replacement.clone())
}

/// Shared walk for [`rewrite_self_calls`] and [`subst_call0`]: replaces
/// every 0-arg `Call(target, [])` in `expr` with `build()`, without
/// recursing into the replacement. Both callers only ever target a plain
/// 0-arg identifier, so a nested `def`/recursive-fn that redefines `target`
/// (necessarily also at arity 0) shadows it -- `target`'s body is left
/// untouched in that case, since those calls refer to the inner def.
fn rewrite_call0(expr: Expr, target: &str, build: &mut dyn FnMut() -> Expr) -> Expr {
    if matches!(&expr, Expr::Call(n, args, _) if n == target && args.is_empty()) {
        return build();
    }
    match expr {
        e @ (Expr::Identity
        | Expr::RecurseDefault
        | Expr::Null
        | Expr::Bool(_)
        | Expr::Number(_)
        | Expr::VarRef(..)
        | Expr::RecCall(_)) => e,
        Expr::Field {
            base,
            name,
            optional,
        } => Expr::Field {
            base: Box::new(rewrite_call0(*base, target, build)),
            name,
            optional,
        },
        Expr::Index {
            base,
            index,
            optional,
        } => Expr::Index {
            base: Box::new(rewrite_call0(*base, target, build)),
            index: Box::new(rewrite_call0(*index, target, build)),
            optional,
        },
        Expr::Slice {
            base,
            from,
            to,
            optional,
        } => Expr::Slice {
            base: Box::new(rewrite_call0(*base, target, build)),
            from: from.map(|x| Box::new(rewrite_call0(*x, target, build))),
            to: to.map(|x| Box::new(rewrite_call0(*x, target, build))),
            optional,
        },
        Expr::IterateAll { base, optional } => Expr::IterateAll {
            base: Box::new(rewrite_call0(*base, target, build)),
            optional,
        },
        Expr::Pipe(a, b) => Expr::Pipe(
            Box::new(rewrite_call0(*a, target, build)),
            Box::new(rewrite_call0(*b, target, build)),
        ),
        Expr::Comma(a, b) => Expr::Comma(
            Box::new(rewrite_call0(*a, target, build)),
            Box::new(rewrite_call0(*b, target, build)),
        ),
        Expr::Str(parts) => Expr::Str(
            parts
                .into_iter()
                .map(|p| match p {
                    StrPart::Lit(s) => StrPart::Lit(s),
                    StrPart::Interp(e) => {
                        StrPart::Interp(Box::new(rewrite_call0(*e, target, build)))
                    }
                })
                .collect(),
        ),
        Expr::ArrayConstruct(inner) => {
            Expr::ArrayConstruct(inner.map(|e| Box::new(rewrite_call0(*e, target, build))))
        }
        Expr::ObjectConstruct(entries) => Expr::ObjectConstruct(
            entries
                .into_iter()
                .map(|ObjEntry { key, val }| ObjEntry {
                    key: rewrite_call0(key, target, build),
                    val: rewrite_call0(val, target, build),
                })
                .collect(),
        ),
        Expr::Neg(x) => Expr::Neg(Box::new(rewrite_call0(*x, target, build))),
        Expr::BinOp(op, a, b) => Expr::BinOp(
            op,
            Box::new(rewrite_call0(*a, target, build)),
            Box::new(rewrite_call0(*b, target, build)),
        ),
        Expr::And(a, b) => Expr::And(
            Box::new(rewrite_call0(*a, target, build)),
            Box::new(rewrite_call0(*b, target, build)),
        ),
        Expr::Or(a, b) => Expr::Or(
            Box::new(rewrite_call0(*a, target, build)),
            Box::new(rewrite_call0(*b, target, build)),
        ),
        Expr::Alt(a, b) => Expr::Alt(
            Box::new(rewrite_call0(*a, target, build)),
            Box::new(rewrite_call0(*b, target, build)),
        ),
        Expr::If(branches, else_branch) => Expr::If(
            branches
                .into_iter()
                .map(|(c, t)| {
                    (
                        rewrite_call0(c, target, build),
                        rewrite_call0(t, target, build),
                    )
                })
                .collect(),
            else_branch.map(|e| Box::new(rewrite_call0(*e, target, build))),
        ),
        Expr::TryCatch(body, catch) => Expr::TryCatch(
            Box::new(rewrite_call0(*body, target, build)),
            catch.map(|e| Box::new(rewrite_call0(*e, target, build))),
        ),
        Expr::Optional(body) => Expr::Optional(Box::new(rewrite_call0(*body, target, build))),
        Expr::VarBind(source, pattern, body) => Expr::VarBind(
            Box::new(rewrite_call0(*source, target, build)),
            rewrite_pattern(pattern, target, build),
            Box::new(rewrite_call0(*body, target, build)),
        ),
        Expr::Reduce {
            source,
            var,
            init,
            update,
        } => Expr::Reduce {
            source: Box::new(rewrite_call0(*source, target, build)),
            var,
            init: Box::new(rewrite_call0(*init, target, build)),
            update: Box::new(rewrite_call0(*update, target, build)),
        },
        Expr::Foreach {
            source,
            var,
            init,
            update,
            extract,
        } => Expr::Foreach {
            source: Box::new(rewrite_call0(*source, target, build)),
            var,
            init: Box::new(rewrite_call0(*init, target, build)),
            update: Box::new(rewrite_call0(*update, target, build)),
            extract: extract.map(|e| Box::new(rewrite_call0(*e, target, build))),
        },
        Expr::Call(n, args, span) => Expr::Call(
            n,
            args.into_iter()
                .map(|a| rewrite_call0(a, target, build))
                .collect(),
            span,
        ),
        Expr::FuncDef {
            name,
            params,
            body,
            rest,
            span,
        } => {
            let shadowed = name == target && params.is_empty();
            let body = if shadowed {
                body
            } else {
                Box::new(rewrite_call0(*body, target, build))
            };
            Expr::FuncDef {
                name,
                params,
                body,
                rest: Box::new(rewrite_call0(*rest, target, build)),
                span,
            }
        }
        Expr::RecFn0 { name, body, rest } => {
            let shadowed = name == target;
            let body = if shadowed {
                body
            } else {
                Box::new(rewrite_call0(*body, target, build))
            };
            Expr::RecFn0 {
                name,
                body,
                rest: Box::new(rewrite_call0(*rest, target, build)),
            }
        }
    }
}

/// As [`rewrite_call0`], but over a destructuring pattern's own computed
/// key expressions.
fn rewrite_pattern(pattern: Pattern, target: &str, build: &mut dyn FnMut() -> Expr) -> Pattern {
    match pattern {
        Pattern::Var(name) => Pattern::Var(name),
        Pattern::Array(elems) => Pattern::Array(
            elems
                .into_iter()
                .map(|p| rewrite_pattern(p, target, build))
                .collect(),
        ),
        Pattern::Object(entries) => Pattern::Object(
            entries
                .into_iter()
                .map(|(k, p)| (rewrite_call0(k, target, build), rewrite_pattern(p, target, build)))
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Asserts a user source fails to compile, with a rendered diagnostic
    /// (relocated into the user's own source coordinates, as a real caller
    /// sees it) that contains every one of `expected`.
    fn assert_error_contains(user_src: &str, expected: &[&str]) {
        use crate::codegen::{Codegen, TreeWalkCodegen};
        let version = JqVersion::default_available().unwrap();
        // Mirrors jqc-macros: parse_and_lower's own errors come back
        // already relocated to user_src's coordinates; compile_top's
        // don't, since it runs after parse_and_lower has already
        // returned, so those need relocating here instead.
        let err = match parse_and_lower(user_src, version) {
            Err(e) => e,
            Ok(ast) => {
                let (offset, len) = user_src_window(user_src);
                TreeWalkCodegen
                    .compile_top(&ast)
                    .map_err(|e| e.relocate(offset, len))
                    .expect_err("expected this filter to fail to compile")
            }
        };
        let rendered = err.render(user_src);
        for e in expected {
            assert!(
                rendered.contains(e),
                "expected rendered error to contain {e:?}, got:\n{rendered}"
            );
        }
    }

    #[test]
    #[ignore = "manual visual check, run with --ignored --nocapture"]
    fn print_demo() {
        use crate::codegen::{Codegen, TreeWalkCodegen};
        let version = JqVersion::default_available().unwrap();
        for src in [
            "(1 + 2",
            ".a | selct",
            "$nope",
            "def f(x): f(x); f(1)",
            "\"abc",
            "1 +",
            "if . then 1 end",
        ] {
            let result = match parse_and_lower(src, version) {
                Err(e) => Err(e),
                Ok(ast) => {
                    let (offset, len) = user_src_window(src);
                    TreeWalkCodegen.compile_top(&ast).map_err(|e| e.relocate(offset, len))
                }
            };
            println!("=== {src:?} ===");
            match result {
                Ok(_) => println!("(compiled fine)"),
                Err(e) => println!("{}", e.render(src)),
            }
        }
    }

    #[test]
    fn unclosed_paren_points_at_both_ends() {
        // The user source starts after PRELUDE + "(\n" -- relocation must
        // land the span back on `(1 + 2`, not somewhere in the prelude.
        assert_error_contains(
            "(1 + 2",
            &["-->", "note:", "unclosed '(' opened here"],
        );
    }

    #[test]
    fn unknown_function_suggests_a_near_miss() {
        assert_error_contains(".a | selct", &["unknown jq function", "did you mean `select`?"]);
    }

    #[test]
    fn undefined_variable_points_at_the_dollar_name() {
        assert_error_contains("$nope", &["undefined variable $nope", "-->", "$nope"]);
    }

    #[test]
    fn recursive_def_with_params_is_rejected_with_a_suggestion() {
        assert_error_contains(
            "def f(x): f(x); f(1)",
            &["recursive function 'f/1' with parameters", "reduce"],
        );
    }

    #[test]
    fn unterminated_string_points_at_the_opening_quote() {
        assert_error_contains(r#""abc"#, &["unterminated string literal", "-->"]);
    }

    #[test]
    fn error_inside_string_interpolation_relocates_correctly() {
        assert_error_contains(r#""result: \(1 + )""#, &["-->"]);
    }

    #[test]
    fn prelude_parses() {
        for version in JqVersion::ALL.into_iter().filter(|v| v.is_available()) {
            if let Err(e) = parse_and_lower(".", version) {
                panic!(
                    "prelude failed to parse under jq {}:\n{}",
                    version.as_str(),
                    e.render(".")
                );
            }
        }
    }
}
