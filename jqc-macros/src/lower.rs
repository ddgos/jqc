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

use crate::ast::{Expr, ObjEntry, StrPart};
use crate::parser::parse_program;

pub type LResult<T> = Result<T, String>;

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
def repeat(f): def __jqc_repeat_r: ., (f | __jqc_repeat_r); __jqc_repeat_r;
def not: if . then false else true end;
def error(msg): msg | error;
"#;

pub fn parse_and_lower(user_src: &str) -> LResult<Expr> {
    let full = format!("{PRELUDE}\n({user_src}\n)");
    let ast = parse_program(&full).map_err(|e| format!("jq parse error: {e}"))?;
    lower(ast, &[])
}

#[derive(Clone)]
enum ParamKind {
    Filter(String),
    Value(String),
}

fn decode_param(p: &str) -> ParamKind {
    match p.strip_prefix('$') {
        Some(rest) => ParamKind::Value(rest.to_string()),
        None => ParamKind::Filter(p.to_string()),
    }
}

#[derive(Clone)]
struct DefEntry {
    name: String,
    arity: usize,
    params: Vec<ParamKind>,
    raw_body: Expr,
    def_scope: Vec<DefEntry>,
}

fn lower(e: Expr, scope: &[DefEntry]) -> LResult<Expr> {
    match e {
        Expr::Identity | Expr::Null | Expr::Bool(_) | Expr::Number(_) => Ok(e),
        Expr::RecurseDefault => lower_call("recurse".to_string(), vec![], scope),
        Expr::VarRef(_) | Expr::RecCall(_) => Ok(e),
        Expr::Field(b, n, opt) => Ok(Expr::Field(Box::new(lower(*b, scope)?), n, opt)),
        Expr::Index(b, i, opt) => Ok(Expr::Index(
            Box::new(lower(*b, scope)?),
            Box::new(lower(*i, scope)?),
            opt,
        )),
        Expr::Slice(b, f, t, opt) => Ok(Expr::Slice(
            Box::new(lower(*b, scope)?),
            f.map(|x| lower(*x, scope)).transpose()?.map(Box::new),
            t.map(|x| lower(*x, scope)).transpose()?.map(Box::new),
            opt,
        )),
        Expr::IterateAll(b, opt) => Ok(Expr::IterateAll(Box::new(lower(*b, scope)?), opt)),
        Expr::Pipe(a, b) => Ok(Expr::Pipe(Box::new(lower(*a, scope)?), Box::new(lower(*b, scope)?))),
        Expr::Comma(a, b) => Ok(Expr::Comma(Box::new(lower(*a, scope)?), Box::new(lower(*b, scope)?))),
        Expr::Str(parts) => Ok(Expr::Str(
            parts
                .into_iter()
                .map(|p| match p {
                    StrPart::Lit(s) => Ok(StrPart::Lit(s)),
                    StrPart::Interp(e) => Ok(StrPart::Interp(Box::new(lower(*e, scope)?))),
                })
                .collect::<LResult<Vec<_>>>()?,
        )),
        Expr::ArrayConstruct(inner) => {
            Ok(Expr::ArrayConstruct(inner.map(|e| lower(*e, scope)).transpose()?.map(Box::new)))
        }
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
        Expr::BinOp(op, a, b) => Ok(Expr::BinOp(op, Box::new(lower(*a, scope)?), Box::new(lower(*b, scope)?))),
        Expr::And(a, b) => Ok(Expr::And(Box::new(lower(*a, scope)?), Box::new(lower(*b, scope)?))),
        Expr::Or(a, b) => Ok(Expr::Or(Box::new(lower(*a, scope)?), Box::new(lower(*b, scope)?))),
        Expr::If(branches, else_e) => Ok(Expr::If(
            branches
                .into_iter()
                .map(|(c, t)| Ok((lower(c, scope)?, lower(t, scope)?)))
                .collect::<LResult<Vec<_>>>()?,
            else_e.map(|e| lower(*e, scope)).transpose()?.map(Box::new),
        )),
        Expr::TryCatch(b, c) => Ok(Expr::TryCatch(
            Box::new(lower(*b, scope)?),
            c.map(|e| lower(*e, scope)).transpose()?.map(Box::new),
        )),
        Expr::Optional(b) => Ok(Expr::Optional(Box::new(lower(*b, scope)?))),
        Expr::VarBind(src, name, body) => Ok(Expr::VarBind(
            Box::new(lower(*src, scope)?),
            name,
            Box::new(lower(*body, scope)?),
        )),
        Expr::Reduce(src, var, init, update) => Ok(Expr::Reduce(
            Box::new(lower(*src, scope)?),
            var,
            Box::new(lower(*init, scope)?),
            Box::new(lower(*update, scope)?),
        )),
        Expr::Foreach(src, var, init, update, extract) => Ok(Expr::Foreach(
            Box::new(lower(*src, scope)?),
            var,
            Box::new(lower(*init, scope)?),
            Box::new(lower(*update, scope)?),
            extract.map(|e| lower(*e, scope)).transpose()?.map(Box::new),
        )),
        Expr::RecFn0 { name, body, rest } => Ok(Expr::RecFn0 {
            name,
            body: Box::new(lower(*body, scope)?),
            rest: Box::new(lower(*rest, scope)?),
        }),
        Expr::FuncDef { name, params, body, rest } => lower_funcdef(name, params, *body, *rest, scope),
        Expr::Call(name, args) => lower_call(name, args, scope),
    }
}

fn lower_funcdef(
    name: String,
    params: Vec<String>,
    body: Expr,
    rest: Expr,
    scope: &[DefEntry],
) -> LResult<Expr> {
    let arity = params.len();
    let recursive = contains_call(&body, &name, arity);
    if recursive {
        if arity != 0 {
            return Err(format!(
                "jqc: recursive function '{name}/{arity}' with parameters is not supported \
                 (only 0-argument recursive defs can be compiled; try a `reduce`/`foreach` loop instead)"
            ));
        }
        let renamed_body = rewrite_self_calls(body, &name);
        let renamed_rest = rewrite_self_calls(rest, &name);
        let lowered_body = lower(renamed_body, scope)?;
        let lowered_rest = lower(renamed_rest, scope)?;
        return Ok(Expr::RecFn0 {
            name,
            body: Box::new(lowered_body),
            rest: Box::new(lowered_rest),
        });
    }
    let mut def_scope = scope.to_vec();
    let entry = DefEntry {
        name,
        arity,
        params: params.iter().map(|p| decode_param(p)).collect(),
        raw_body: body,
        def_scope: def_scope.clone(),
    };
    def_scope.push(entry);
    lower(rest, &def_scope)
}

fn lower_call(name: String, args: Vec<Expr>, scope: &[DefEntry]) -> LResult<Expr> {
    if let Some(entry) = scope.iter().rev().find(|e| e.name == name && e.arity == args.len()) {
        // Arguments are filters written at the *call site*, so they must be
        // resolved against the caller's `scope` -- e.g. `map(select(f))`
        // needs `select` visible here, even though `map`'s own def_scope
        // (captured at `map`'s definition, before `select` existed yet in
        // this prelude) doesn't have it. Lowering them up front, before
        // splicing into the callee's body, also makes the substitution
        // idempotent: re-lowering an already-lowered subtree under the
        // callee's def_scope later is a no-op for it.
        let args = args.into_iter().map(|a| lower(a, scope)).collect::<LResult<Vec<_>>>()?;
        let mut specialized = entry.raw_body.clone();
        for (param, arg) in entry.params.iter().zip(args.iter()) {
            match param {
                ParamKind::Filter(pname) => {
                    specialized = subst_call0(specialized, pname, arg);
                }
                ParamKind::Value(_) => {}
            }
        }
        for (param, arg) in entry.params.iter().zip(args.into_iter()).rev() {
            if let ParamKind::Value(vname) = param {
                specialized = Expr::VarBind(Box::new(arg), vname.clone(), Box::new(specialized));
            }
        }
        return lower(specialized, &entry.def_scope);
    }
    let lowered_args = args.into_iter().map(|a| lower(a, scope)).collect::<LResult<Vec<_>>>()?;
    Ok(Expr::Call(name, lowered_args))
}

/// Does `expr` call `name/arity` anywhere (used to detect recursive defs)?
/// Stops descending into a nested def/recursive-fn body that redefines the
/// same name at the same arity, since that call refers to the inner def.
fn contains_call(expr: &Expr, name: &str, arity: usize) -> bool {
    match expr {
        Expr::Call(n, args) => {
            (n == name && args.len() == arity) || args.iter().any(|a| contains_call(a, name, arity))
        }
        Expr::RecCall(n) => n == name && arity == 0,
        Expr::Identity | Expr::RecurseDefault | Expr::Null | Expr::Bool(_) | Expr::Number(_) | Expr::VarRef(_) => {
            false
        }
        Expr::Field(b, _, _) | Expr::IterateAll(b, _) | Expr::Neg(b) | Expr::Optional(b) => {
            contains_call(b, name, arity)
        }
        Expr::Index(b, i, _) => contains_call(b, name, arity) || contains_call(i, name, arity),
        Expr::Slice(b, f, t, _) => {
            contains_call(b, name, arity)
                || f.as_ref().map(|x| contains_call(x, name, arity)).unwrap_or(false)
                || t.as_ref().map(|x| contains_call(x, name, arity)).unwrap_or(false)
        }
        Expr::Pipe(a, b) | Expr::Comma(a, b) | Expr::And(a, b) | Expr::Or(a, b) | Expr::BinOp(_, a, b) => {
            contains_call(a, name, arity) || contains_call(b, name, arity)
        }
        Expr::Str(parts) => parts.iter().any(|p| match p {
            StrPart::Lit(_) => false,
            StrPart::Interp(e) => contains_call(e, name, arity),
        }),
        Expr::ArrayConstruct(inner) => inner.as_ref().map(|e| contains_call(e, name, arity)).unwrap_or(false),
        Expr::ObjectConstruct(entries) => entries
            .iter()
            .any(|e| contains_call(&e.key, name, arity) || contains_call(&e.val, name, arity)),
        Expr::If(branches, else_e) => {
            branches.iter().any(|(c, t)| contains_call(c, name, arity) || contains_call(t, name, arity))
                || else_e.as_ref().map(|e| contains_call(e, name, arity)).unwrap_or(false)
        }
        Expr::TryCatch(b, c) => {
            contains_call(b, name, arity) || c.as_ref().map(|e| contains_call(e, name, arity)).unwrap_or(false)
        }
        Expr::VarBind(src, _, body) => contains_call(src, name, arity) || contains_call(body, name, arity),
        Expr::Reduce(src, _, init, update) => {
            contains_call(src, name, arity) || contains_call(init, name, arity) || contains_call(update, name, arity)
        }
        Expr::Foreach(src, _, init, update, extract) => {
            contains_call(src, name, arity)
                || contains_call(init, name, arity)
                || contains_call(update, name, arity)
                || extract.as_ref().map(|e| contains_call(e, name, arity)).unwrap_or(false)
        }
        Expr::FuncDef { name: n2, params, body, rest } => {
            let shadowed = n2 == name && params.len() == arity;
            (!shadowed && contains_call(body, name, arity)) || contains_call(rest, name, arity)
        }
        Expr::RecFn0 { name: n2, body, rest } => {
            let shadowed = n2 == name && arity == 0;
            (!shadowed && contains_call(body, name, arity)) || contains_call(rest, name, arity)
        }
    }
}

/// Replace every 0-arg `Call(name, [])` in `expr` with `RecCall(name)`,
/// marking it as a call to the recursive function currently being compiled.
fn rewrite_self_calls(expr: Expr, name: &str) -> Expr {
    map_expr(expr, &mut |e| match e {
        Expr::Call(n, args) if n == name && args.is_empty() => Some(Expr::RecCall(n.clone())),
        _ => None,
    })
}

/// Replace every 0-arg `Call(pname, [])` in `expr` with a fresh clone of
/// `replacement` (call-by-name filter-argument substitution).
fn subst_call0(expr: Expr, pname: &str, replacement: &Expr) -> Expr {
    map_expr(expr, &mut |e| match e {
        Expr::Call(n, args) if n == pname && args.is_empty() => Some(replacement.clone()),
        _ => None,
    })
}

/// Generic structural rewrite: applies `f` at every node (pre-order); if `f`
/// returns `Some`, that replaces the node and its children are NOT
/// traversed further. Stops descending into nested `def`/recursive-fn
/// bodies that redefine the name being targeted is the caller's
/// responsibility via `f` returning `None` but leaving the subtree alone --
/// callers here only ever target 0-arg plain identifiers, which can't be
/// legally shadowed by anything except another `def` of the same name, so
/// we special-case `FuncDef`/`RecFn0` to skip their `body` when shadowed.
fn map_expr(expr: Expr, f: &mut dyn FnMut(&Expr) -> Option<Expr>) -> Expr {
    if let Some(replacement) = f(&expr) {
        return replacement;
    }
    match expr {
        e @ (Expr::Identity
        | Expr::RecurseDefault
        | Expr::Null
        | Expr::Bool(_)
        | Expr::Number(_)
        | Expr::VarRef(_)
        | Expr::RecCall(_)) => e,
        Expr::Field(b, n, opt) => Expr::Field(Box::new(map_expr(*b, f)), n, opt),
        Expr::Index(b, i, opt) => Expr::Index(Box::new(map_expr(*b, f)), Box::new(map_expr(*i, f)), opt),
        Expr::Slice(b, lo, hi, opt) => Expr::Slice(
            Box::new(map_expr(*b, f)),
            lo.map(|x| Box::new(map_expr(*x, f))),
            hi.map(|x| Box::new(map_expr(*x, f))),
            opt,
        ),
        Expr::IterateAll(b, opt) => Expr::IterateAll(Box::new(map_expr(*b, f)), opt),
        Expr::Pipe(a, b) => Expr::Pipe(Box::new(map_expr(*a, f)), Box::new(map_expr(*b, f))),
        Expr::Comma(a, b) => Expr::Comma(Box::new(map_expr(*a, f)), Box::new(map_expr(*b, f))),
        Expr::Str(parts) => Expr::Str(
            parts
                .into_iter()
                .map(|p| match p {
                    StrPart::Lit(s) => StrPart::Lit(s),
                    StrPart::Interp(e) => StrPart::Interp(Box::new(map_expr(*e, f))),
                })
                .collect(),
        ),
        Expr::ArrayConstruct(inner) => Expr::ArrayConstruct(inner.map(|e| Box::new(map_expr(*e, f)))),
        Expr::ObjectConstruct(entries) => Expr::ObjectConstruct(
            entries
                .into_iter()
                .map(|ObjEntry { key, val }| ObjEntry {
                    key: map_expr(key, f),
                    val: map_expr(val, f),
                })
                .collect(),
        ),
        Expr::Neg(x) => Expr::Neg(Box::new(map_expr(*x, f))),
        Expr::BinOp(op, a, b) => Expr::BinOp(op, Box::new(map_expr(*a, f)), Box::new(map_expr(*b, f))),
        Expr::And(a, b) => Expr::And(Box::new(map_expr(*a, f)), Box::new(map_expr(*b, f))),
        Expr::Or(a, b) => Expr::Or(Box::new(map_expr(*a, f)), Box::new(map_expr(*b, f))),
        Expr::If(branches, else_e) => Expr::If(
            branches.into_iter().map(|(c, t)| (map_expr(c, f), map_expr(t, f))).collect(),
            else_e.map(|e| Box::new(map_expr(*e, f))),
        ),
        Expr::TryCatch(b, c) => Expr::TryCatch(Box::new(map_expr(*b, f)), c.map(|e| Box::new(map_expr(*e, f)))),
        Expr::Optional(b) => Expr::Optional(Box::new(map_expr(*b, f))),
        Expr::VarBind(src, name, body) => {
            Expr::VarBind(Box::new(map_expr(*src, f)), name, Box::new(map_expr(*body, f)))
        }
        Expr::Reduce(src, var, init, update) => Expr::Reduce(
            Box::new(map_expr(*src, f)),
            var,
            Box::new(map_expr(*init, f)),
            Box::new(map_expr(*update, f)),
        ),
        Expr::Foreach(src, var, init, update, extract) => Expr::Foreach(
            Box::new(map_expr(*src, f)),
            var,
            Box::new(map_expr(*init, f)),
            Box::new(map_expr(*update, f)),
            extract.map(|e| Box::new(map_expr(*e, f))),
        ),
        Expr::Call(n, args) => Expr::Call(n, args.into_iter().map(|a| map_expr(a, f)).collect()),
        Expr::FuncDef { name, params, body, rest } => {
            let new_body = *body; // shadowing of the substitution target inside nested defs is a
                                   // rare edge case we accept as a known limitation; always descend.
            Expr::FuncDef {
                name,
                params,
                body: Box::new(map_expr(new_body, f)),
                rest: Box::new(map_expr(*rest, f)),
            }
        }
        Expr::RecFn0 { name, body, rest } => {
            Expr::RecFn0 { name, body: Box::new(map_expr(*body, f)), rest: Box::new(map_expr(*rest, f)) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prelude_parses() {
        if let Err(e) = parse_and_lower(".") {
            panic!("{e}");
        }
    }
}
