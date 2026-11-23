//! AST for the supported jq subset.

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
    /// `Literal`; for `(expr): val` it's an arbitrary expression.
    pub key: Expr,
    pub val: Expr,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Identity,
    RecurseDefault,
    Field(Box<Expr>, String, bool),
    Index(Box<Expr>, Box<Expr>, bool),
    Slice(Box<Expr>, Option<Box<Expr>>, Option<Box<Expr>>, bool),
    IterateAll(Box<Expr>, bool),
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
    If(Vec<(Expr, Expr)>, Option<Box<Expr>>),
    TryCatch(Box<Expr>, Option<Box<Expr>>),
    Optional(Box<Expr>),
    Call(String, Vec<Expr>),
    VarBind(Box<Expr>, String, Box<Expr>),
    VarRef(String),
    /// Introduced by the lowering pass: a call to a [`Expr::RecFn0`]
    /// currently being compiled (as opposed to a builtin or inlineable def).
    RecCall(String),
    Reduce(Box<Expr>, String, Box<Expr>, Box<Expr>),
    Foreach(
        Box<Expr>,
        String,
        Box<Expr>,
        Box<Expr>,
        Option<Box<Expr>>,
    ),
    FuncDef {
        name: String,
        params: Vec<String>,
        body: Box<Expr>,
        rest: Box<Expr>,
    },
    /// Introduced by the lowering pass for defs that recurse: compiled as a
    /// genuine recursive Rust function rather than inlined at call sites.
    RecFn0 {
        name: String,
        body: Box<Expr>,
        rest: Box<Expr>,
    },
}

pub fn convert_str_parts(
    parts: Vec<LexStrPart>,
    parse_sub: &mut dyn FnMut(&str) -> Result<Expr, String>,
) -> Result<Vec<StrPart>, String> {
    parts
        .into_iter()
        .map(|p| match p {
            LexStrPart::Lit(s) => Ok(StrPart::Lit(s)),
            LexStrPart::Interp(src) => parse_sub(&src).map(|e| StrPart::Interp(Box::new(e))),
        })
        .collect()
}
