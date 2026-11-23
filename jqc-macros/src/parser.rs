//! Recursive-descent parser producing [`Expr`] from the token stream
//! produced by [`crate::lexer`].

use crate::ast::{convert_str_parts, BinOp, Expr, ObjEntry};
use crate::lexer::{Lexer, Tok};

pub type PResult<T> = Result<T, String>;

/// Reserved words that must never be swallowed as a field name by `.name`
/// (e.g. `if f then . else empty end` -- the `.` there must stay the bare
/// identity filter, not become a field access `.else`).
fn is_keyword(s: &str) -> bool {
    matches!(
        s,
        "if" | "then"
            | "elif"
            | "else"
            | "end"
            | "and"
            | "or"
            | "as"
            | "def"
            | "reduce"
            | "foreach"
            | "try"
            | "catch"
    )
}

pub fn parse_program(src: &str) -> PResult<Expr> {
    let toks = Lexer::new(src).tokenize().map_err(|e| format!("jq lex error: {e}"))?;
    let mut p = Parser { toks, pos: 0 };
    let e = p.parse_pipe()?;
    p.expect_eof()?;
    Ok(e)
}

/// Used for recursively parsing `\( ... )` string-interpolation bodies.
fn parse_sub_expr(src: &str) -> Result<Expr, String> {
    parse_program(src)
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.toks[self.pos]
    }

    fn bump(&mut self) -> Tok {
        let t = self.toks[self.pos].clone();
        if self.pos + 1 < self.toks.len() {
            self.pos += 1;
        }
        t
    }

    fn expect_eof(&self) -> PResult<()> {
        if *self.peek() == Tok::Eof {
            Ok(())
        } else {
            Err(format!("unexpected trailing tokens near {:?}", self.peek()))
        }
    }

    fn is_ident(&self, s: &str) -> bool {
        matches!(self.peek(), Tok::Ident(name) if name == s)
    }

    fn eat_ident(&mut self, s: &str) -> PResult<()> {
        if self.is_ident(s) {
            self.bump();
            Ok(())
        } else {
            Err(format!("expected '{s}', found {:?}", self.peek()))
        }
    }

    fn expect(&mut self, t: Tok) -> PResult<()> {
        if *self.peek() == t {
            self.bump();
            Ok(())
        } else {
            Err(format!("expected {:?}, found {:?}", t, self.peek()))
        }
    }

    fn expect_var(&mut self) -> PResult<String> {
        match self.bump() {
            Tok::Var(name) => Ok(name),
            other => Err(format!("expected variable, found {other:?}")),
        }
    }

    // --- precedence ladder --------------------------------------------

    fn parse_pipe(&mut self) -> PResult<Expr> {
        let mut left = self.parse_comma()?;
        while *self.peek() == Tok::Pipe {
            self.bump();
            let right = self.parse_pipe()?;
            left = Expr::Pipe(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_comma(&mut self) -> PResult<Expr> {
        let mut left = self.parse_as_binding()?;
        while *self.peek() == Tok::Comma {
            self.bump();
            let right = self.parse_as_binding()?;
            left = Expr::Comma(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    /// `EXPR as $x | BODY`, falling through to plain `or`-level expr.
    fn parse_as_binding(&mut self) -> PResult<Expr> {
        let e = self.parse_or()?;
        if self.is_ident("as") {
            self.bump();
            let var = self.expect_var()?;
            self.expect(Tok::Pipe)?;
            let body = self.parse_pipe()?;
            return Ok(Expr::VarBind(Box::new(e), var, Box::new(body)));
        }
        Ok(e)
    }

    /// Pipe-without-comma, used for object-construction values.
    fn parse_pipe_no_comma(&mut self) -> PResult<Expr> {
        let mut left = self.parse_as_binding()?;
        while *self.peek() == Tok::Pipe {
            self.bump();
            let right = self.parse_pipe_no_comma()?;
            left = Expr::Pipe(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_or(&mut self) -> PResult<Expr> {
        let mut left = self.parse_and()?;
        while self.is_ident("or") {
            self.bump();
            let right = self.parse_and()?;
            left = Expr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> PResult<Expr> {
        let mut left = self.parse_cmp()?;
        while self.is_ident("and") {
            self.bump();
            let right = self.parse_cmp()?;
            left = Expr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_cmp(&mut self) -> PResult<Expr> {
        let left = self.parse_add()?;
        let op = match self.peek() {
            Tok::Eq => BinOp::Eq,
            Tok::Ne => BinOp::Ne,
            Tok::Lt => BinOp::Lt,
            Tok::Le => BinOp::Le,
            Tok::Gt => BinOp::Gt,
            Tok::Ge => BinOp::Ge,
            _ => return Ok(left),
        };
        self.bump();
        let right = self.parse_add()?;
        Ok(Expr::BinOp(op, Box::new(left), Box::new(right)))
    }

    fn parse_add(&mut self) -> PResult<Expr> {
        let mut left = self.parse_mul()?;
        loop {
            let op = match self.peek() {
                Tok::Plus => BinOp::Add,
                Tok::Minus => BinOp::Sub,
                _ => break,
            };
            self.bump();
            let right = self.parse_mul()?;
            left = Expr::BinOp(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_mul(&mut self) -> PResult<Expr> {
        let mut left = self.parse_unary()?;
        loop {
            let op = match self.peek() {
                Tok::Star => BinOp::Mul,
                Tok::Slash => BinOp::Div,
                Tok::Percent => BinOp::Mod,
                _ => break,
            };
            self.bump();
            let right = self.parse_unary()?;
            left = Expr::BinOp(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> PResult<Expr> {
        if *self.peek() == Tok::Minus {
            self.bump();
            let e = self.parse_postfix()?;
            return Ok(Expr::Neg(Box::new(e)));
        }
        self.parse_postfix()
    }

    fn parse_postfix(&mut self) -> PResult<Expr> {
        let mut base = self.parse_primary()?;
        loop {
            match self.peek().clone() {
                Tok::Dot => {
                    // Chained field access: `EXPR.name` or `EXPR."name"`.
                    // A keyword after `.` (e.g. `. else`) is NOT a field
                    // name -- leave it for the enclosing construct.
                    if matches!(&self.toks[self.pos + 1], Tok::Ident(name) if is_keyword(name)) {
                        break;
                    }
                    self.bump();
                    match self.bump() {
                        Tok::Ident(name) => base = Expr::Field(Box::new(base), name, false),
                        Tok::Str(parts) => {
                            let name = lit_str_only(&parts)?;
                            base = Expr::Field(Box::new(base), name, false);
                        }
                        other => return Err(format!("expected field name after '.', found {other:?}")),
                    }
                }
                Tok::LBracket => {
                    self.bump();
                    base = self.parse_bracket_suffix(base)?;
                }
                Tok::Question => {
                    self.bump();
                    base = mark_optional(base);
                }
                _ => break,
            }
        }
        Ok(base)
    }

    fn parse_bracket_suffix(&mut self, base: Expr) -> PResult<Expr> {
        if *self.peek() == Tok::RBracket {
            self.bump();
            return Ok(Expr::IterateAll(Box::new(base), false));
        }
        if *self.peek() == Tok::Colon {
            self.bump();
            let to = self.parse_pipe_no_comma()?;
            self.expect(Tok::RBracket)?;
            return Ok(Expr::Slice(Box::new(base), None, Some(Box::new(to)), false));
        }
        let first = self.parse_pipe_no_comma()?;
        if *self.peek() == Tok::Colon {
            self.bump();
            if *self.peek() == Tok::RBracket {
                self.bump();
                return Ok(Expr::Slice(Box::new(base), Some(Box::new(first)), None, false));
            }
            let to = self.parse_pipe_no_comma()?;
            self.expect(Tok::RBracket)?;
            return Ok(Expr::Slice(
                Box::new(base),
                Some(Box::new(first)),
                Some(Box::new(to)),
                false,
            ));
        }
        self.expect(Tok::RBracket)?;
        Ok(Expr::Index(Box::new(base), Box::new(first), false))
    }

    fn parse_primary(&mut self) -> PResult<Expr> {
        match self.peek().clone() {
            Tok::Dot => {
                self.bump();
                match self.peek().clone() {
                    Tok::Ident(name) if !is_keyword(&name) => {
                        self.bump();
                        Ok(Expr::Field(Box::new(Expr::Identity), name, false))
                    }
                    Tok::Str(parts) => {
                        self.bump();
                        let name = lit_str_only(&parts)?;
                        Ok(Expr::Field(Box::new(Expr::Identity), name, false))
                    }
                    _ => Ok(Expr::Identity),
                }
            }
            Tok::DotDot => {
                self.bump();
                Ok(Expr::RecurseDefault)
            }
            Tok::Number(n) => {
                self.bump();
                Ok(Expr::Number(n))
            }
            Tok::Str(parts) => {
                self.bump();
                let converted = convert_str_parts(parts, &mut |s| parse_sub_expr(s))?;
                Ok(Expr::Str(converted))
            }
            Tok::Var(name) => {
                self.bump();
                Ok(Expr::VarRef(name))
            }
            Tok::LParen => {
                self.bump();
                let e = self.parse_pipe()?;
                self.expect(Tok::RParen)?;
                Ok(e)
            }
            Tok::LBracket => {
                self.bump();
                if *self.peek() == Tok::RBracket {
                    self.bump();
                    return Ok(Expr::ArrayConstruct(None));
                }
                let inner = self.parse_pipe()?;
                self.expect(Tok::RBracket)?;
                Ok(Expr::ArrayConstruct(Some(Box::new(inner))))
            }
            Tok::LBrace => self.parse_object(),
            Tok::Ident(name) => self.parse_ident_primary(name),
            other => Err(format!("unexpected token {other:?}")),
        }
    }

    fn parse_ident_primary(&mut self, name: String) -> PResult<Expr> {
        match name.as_str() {
            "true" => {
                self.bump();
                Ok(Expr::Bool(true))
            }
            "false" => {
                self.bump();
                Ok(Expr::Bool(false))
            }
            "null" => {
                self.bump();
                Ok(Expr::Null)
            }
            "if" => self.parse_if(),
            "try" => self.parse_try(),
            "reduce" => self.parse_reduce(),
            "foreach" => self.parse_foreach(),
            "def" => self.parse_def(),
            "and" | "or" | "as" | "then" | "elif" | "else" | "end" | "catch" => {
                Err(format!("unexpected keyword '{name}'"))
            }
            _ => self.parse_call(name),
        }
    }

    fn parse_call(&mut self, name: String) -> PResult<Expr> {
        self.bump();
        if *self.peek() != Tok::LParen {
            return Ok(Expr::Call(name, vec![]));
        }
        self.bump();
        let mut args = Vec::new();
        if *self.peek() != Tok::RParen {
            loop {
                args.push(self.parse_pipe_no_comma_allow_comma_as_union()?);
                if *self.peek() == Tok::Semi {
                    self.bump();
                    continue;
                }
                break;
            }
        }
        self.expect(Tok::RParen)?;
        Ok(Expr::Call(name, args))
    }

    /// Function-call arguments are full filters (commas inside them are the
    /// ordinary union/generator operator, not argument separators -- `;` is
    /// the separator), so this is just `parse_pipe`.
    fn parse_pipe_no_comma_allow_comma_as_union(&mut self) -> PResult<Expr> {
        self.parse_pipe()
    }

    fn parse_if(&mut self) -> PResult<Expr> {
        self.eat_ident("if")?;
        let mut branches = Vec::new();
        let cond = self.parse_pipe()?;
        self.eat_ident("then")?;
        let then_e = self.parse_pipe()?;
        branches.push((cond, then_e));
        loop {
            if self.is_ident("elif") {
                self.bump();
                let c = self.parse_pipe()?;
                self.eat_ident("then")?;
                let t = self.parse_pipe()?;
                branches.push((c, t));
                continue;
            }
            break;
        }
        let else_e = if self.is_ident("else") {
            self.bump();
            Some(Box::new(self.parse_pipe()?))
        } else {
            None
        };
        self.eat_ident("end")?;
        Ok(Expr::If(branches, else_e))
    }

    fn parse_try(&mut self) -> PResult<Expr> {
        self.eat_ident("try")?;
        let body = self.parse_postfix()?;
        let catch = if self.is_ident("catch") {
            self.bump();
            Some(Box::new(self.parse_postfix()?))
        } else {
            None
        };
        Ok(Expr::TryCatch(Box::new(body), catch))
    }

    fn parse_reduce(&mut self) -> PResult<Expr> {
        self.eat_ident("reduce")?;
        let src = self.parse_postfix()?;
        self.eat_ident("as")?;
        let var = self.expect_var()?;
        self.expect(Tok::LParen)?;
        let init = self.parse_pipe()?;
        self.expect(Tok::Semi)?;
        let update = self.parse_pipe()?;
        self.expect(Tok::RParen)?;
        Ok(Expr::Reduce(Box::new(src), var, Box::new(init), Box::new(update)))
    }

    fn parse_foreach(&mut self) -> PResult<Expr> {
        self.eat_ident("foreach")?;
        let src = self.parse_postfix()?;
        self.eat_ident("as")?;
        let var = self.expect_var()?;
        self.expect(Tok::LParen)?;
        let init = self.parse_pipe()?;
        self.expect(Tok::Semi)?;
        let update = self.parse_pipe()?;
        let extract = if *self.peek() == Tok::Semi {
            self.bump();
            Some(Box::new(self.parse_pipe()?))
        } else {
            None
        };
        self.expect(Tok::RParen)?;
        Ok(Expr::Foreach(Box::new(src), var, Box::new(init), Box::new(update), extract))
    }

    fn parse_def(&mut self) -> PResult<Expr> {
        self.eat_ident("def")?;
        let name = match self.bump() {
            Tok::Ident(n) => n,
            other => return Err(format!("expected function name after 'def', found {other:?}")),
        };
        let mut params = Vec::new();
        if *self.peek() == Tok::LParen {
            self.bump();
            loop {
                match self.bump() {
                    Tok::Ident(p) => params.push((p, false)),
                    Tok::Var(p) => params.push((p, true)),
                    other => return Err(format!("expected parameter name, found {other:?}")),
                }
                if *self.peek() == Tok::Semi {
                    self.bump();
                    continue;
                }
                break;
            }
            self.expect(Tok::RParen)?;
        }
        self.expect(Tok::Colon)?;
        let body = self.parse_pipe()?;
        self.expect(Tok::Semi)?;
        let rest = self.parse_pipe()?;
        Ok(Expr::FuncDef {
            name,
            params: params.into_iter().map(ParamSpec::encode).collect(),
            body: Box::new(body),
            rest: Box::new(rest),
        })
    }

    fn parse_object(&mut self) -> PResult<Expr> {
        self.expect(Tok::LBrace)?;
        let mut entries = Vec::new();
        if *self.peek() != Tok::RBrace {
            loop {
                entries.push(self.parse_obj_entry()?);
                if *self.peek() == Tok::Comma {
                    self.bump();
                    continue;
                }
                break;
            }
        }
        self.expect(Tok::RBrace)?;
        Ok(Expr::ObjectConstruct(entries))
    }

    fn parse_obj_entry(&mut self) -> PResult<ObjEntry> {
        match self.peek().clone() {
            Tok::Var(name) => {
                self.bump();
                if *self.peek() == Tok::Colon {
                    self.bump();
                    let val = self.parse_pipe_no_comma()?;
                    Ok(ObjEntry {
                        key: Expr::Str(vec![crate::ast::StrPart::Lit(name)]),
                        val,
                    })
                } else {
                    Ok(ObjEntry {
                        key: Expr::Str(vec![crate::ast::StrPart::Lit(name.clone())]),
                        val: Expr::VarRef(name),
                    })
                }
            }
            Tok::Ident(name) => {
                self.bump();
                if *self.peek() == Tok::Colon {
                    self.bump();
                    let val = self.parse_pipe_no_comma()?;
                    Ok(ObjEntry {
                        key: Expr::Str(vec![crate::ast::StrPart::Lit(name)]),
                        val,
                    })
                } else {
                    Ok(ObjEntry {
                        key: Expr::Str(vec![crate::ast::StrPart::Lit(name.clone())]),
                        val: Expr::Field(Box::new(Expr::Identity), name, false),
                    })
                }
            }
            Tok::Str(parts) => {
                self.bump();
                let converted = convert_str_parts(parts, &mut |s| parse_sub_expr(s))?;
                self.expect(Tok::Colon)?;
                let val = self.parse_pipe_no_comma()?;
                Ok(ObjEntry {
                    key: Expr::Str(converted),
                    val,
                })
            }
            Tok::LParen => {
                self.bump();
                let key = self.parse_pipe()?;
                self.expect(Tok::RParen)?;
                self.expect(Tok::Colon)?;
                let val = self.parse_pipe_no_comma()?;
                Ok(ObjEntry { key, val })
            }
            other => Err(format!("unexpected token in object construction: {other:?}")),
        }
    }
}

/// Params are encoded as plain strings prefixed with `$` for value params
/// so `ast::Expr::FuncDef.params: Vec<String>` doesn't need its own type;
/// kept local so the AST module stays simple.
struct ParamSpec;
impl ParamSpec {
    fn encode((name, is_val): (String, bool)) -> String {
        if is_val {
            format!("${name}")
        } else {
            name
        }
    }
}

fn mark_optional(e: Expr) -> Expr {
    match e {
        Expr::Field(b, n, _) => Expr::Field(b, n, true),
        Expr::Index(b, i, _) => Expr::Index(b, i, true),
        Expr::Slice(b, f, t, _) => Expr::Slice(b, f, t, true),
        Expr::IterateAll(b, _) => Expr::IterateAll(b, true),
        other => Expr::Optional(Box::new(other)),
    }
}

fn lit_str_only(parts: &[crate::lexer::StrPart]) -> PResult<String> {
    match parts {
        [crate::lexer::StrPart::Lit(s)] => Ok(s.clone()),
        [] => Ok(String::new()),
        _ => Err("interpolated strings are not allowed as field names".to_string()),
    }
}
