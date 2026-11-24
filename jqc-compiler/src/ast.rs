//! AST for the supported jq subset.

use crate::diagnostics::{Diagnostic, Span};
use crate::lexer::StrPart as LexStrPart;

#[derive(Debug, Clone)]
pub enum StrPart {
    Lit(String),
    Interp(Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone)]
pub struct ObjEntry {
    /// Key expression. For plain identifiers / string keys this is a
    /// `Str` literal; for `(expr): val` it's an arbitrary expression.
    pub key: Expr,
    pub val: Expr,
}

/// A `def` parameter: a plain name is a filter param (call-by-name, like
/// jq itself); a `$`-prefixed name is a value param, desugared to an `as`
/// binding at the call site.
#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub is_value: bool,
}

/// An `as` binding's left-hand pattern: a plain `$x`, or a destructuring
/// shape over an array/object. `Index`/`Object` bind each element
/// positionally/by key the same way plain `.[i]`/`.key` would (`null` for
/// an out-of-range index or a missing key, not an error -- see
/// `jqc_core::helpers::index_value`), and may nest arbitrarily (`[$a,
/// {b: $b}]`, …).
#[derive(Debug, Clone)]
pub enum Pattern {
    Var(String),
    Array(Vec<Pattern>),
    /// Each entry's key is an `Expr` the same way [`ObjEntry::key`] is --
    /// a `Str` literal for a plain identifier/string/shorthand key, or an
    /// arbitrary expression for a computed `(expr): pattern` key.
    Object(Vec<(Expr, Pattern)>),
}

#[derive(Debug, Clone)]
pub enum Expr {
    Identity,
    RecurseDefault,
    Field {
        base: Box<Expr>,
        name: String,
        optional: bool,
    },
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
        optional: bool,
    },
    Slice {
        base: Box<Expr>,
        from: Option<Box<Expr>>,
        to: Option<Box<Expr>>,
        optional: bool,
    },
    IterateAll {
        base: Box<Expr>,
        optional: bool,
    },
    Pipe(Box<Expr>, Box<Expr>),
    Comma(Box<Expr>, Box<Expr>),
    Null,
    Bool(bool),
    Number(f64),
    Str(Vec<StrPart>),
    ArrayConstruct(Option<Box<Expr>>),
    ObjectConstruct(Vec<ObjEntry>),
    Neg(Box<Expr>),
    BinOp(BinOp, Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    /// `a // b`: the alternative operator. Not the same as `Or` -- this
    /// filters `a`'s outputs to those that are neither `false` nor
    /// `null`, falling back to `b` only if that filtered set is empty.
    Alt(Box<Expr>, Box<Expr>),
    If(Vec<(Expr, Expr)>, Option<Box<Expr>>),
    TryCatch(Box<Expr>, Option<Box<Expr>>),
    Optional(Box<Expr>),
    /// `span` covers just the function name at the call site, for
    /// "unknown function"/"did you mean" diagnostics.
    Call(String, Vec<Expr>, Span),
    VarBind(Box<Expr>, Pattern, Box<Expr>),
    /// `span` covers the `$name` reference itself, for "undefined
    /// variable" diagnostics.
    VarRef(String, Span),
    /// Introduced by the lowering pass: a call to the [`Expr::RecFn0`]
    /// currently being compiled (as opposed to a builtin or inlineable def).
    RecCall(String),
    Reduce {
        source: Box<Expr>,
        var: String,
        init: Box<Expr>,
        update: Box<Expr>,
    },
    Foreach {
        source: Box<Expr>,
        var: String,
        init: Box<Expr>,
        update: Box<Expr>,
        extract: Option<Box<Expr>>,
    },
    FuncDef {
        name: String,
        params: Vec<Param>,
        body: Box<Expr>,
        rest: Box<Expr>,
        /// Covers the function name after `def`, for "recursive def with
        /// parameters" diagnostics.
        span: Span,
    },
    /// Introduced by the lowering pass for defs that recurse: compiled as a
    /// genuine recursive Rust function rather than inlined at call sites.
    RecFn0 {
        name: String,
        body: Box<Expr>,
        rest: Box<Expr>,
    },
}

pub fn str_literal(s: impl Into<String>) -> Expr {
    Expr::Str(vec![StrPart::Lit(s.into())])
}

/// `parse_sub` re-parses a `\( ... )` interpolation's raw text in
/// isolation, so any error it returns has a span relative to *that* text;
/// relocated here by `offset` back to the enclosing string literal's own
/// source coordinates before being handed further up.
pub fn convert_str_parts(
    parts: Vec<LexStrPart>,
    parse_sub: &mut dyn FnMut(&str) -> Result<Expr, Diagnostic>,
) -> Result<Vec<StrPart>, Diagnostic> {
    parts
        .into_iter()
        .map(|p| match p {
            LexStrPart::Lit(s) => Ok(StrPart::Lit(s)),
            LexStrPart::Interp(src, offset) => parse_sub(&src)
                .map(|e| StrPart::Interp(Box::new(e)))
                .map_err(|e| e.shift(offset)),
        })
        .collect()
}
