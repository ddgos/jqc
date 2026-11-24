//! Recursive-descent parser producing [`Expr`] from the token stream
//! produced by [`crate::lexer`].

use crate::ast::{convert_str_parts, str_literal, BinOp, Expr, Param, Pattern};
use crate::diagnostics::{Diagnostic, Span};
use crate::lexer::{Lexer, StrPart, Tok};
use crate::version::JqVersion;

pub type PResult<T> = Result<T, Diagnostic>;

/// A human-readable description of a token for error messages, e.g.
/// "`)`" or "identifier `foo`" -- used in place of `{:?}` Debug output.
fn describe(t: &Tok) -> String {
    match t {
        Tok::Dot => "`.`".into(),
        Tok::DotDot => "`..`".into(),
        Tok::LBracket => "`[`".into(),
        Tok::RBracket => "`]`".into(),
        Tok::LBrace => "`{`".into(),
        Tok::RBrace => "`}`".into(),
        Tok::LParen => "`(`".into(),
        Tok::RParen => "`)`".into(),
        Tok::Pipe => "`|`".into(),
        Tok::Comma => "`,`".into(),
        Tok::Colon => "`:`".into(),
        Tok::Semi => "`;`".into(),
        Tok::Question => "`?`".into(),
        Tok::Plus => "`+`".into(),
        Tok::Minus => "`-`".into(),
        Tok::Star => "`*`".into(),
        Tok::Slash => "`/`".into(),
        Tok::SlashSlash => "`//`".into(),
        Tok::Percent => "`%`".into(),
        Tok::Eq => "`==`".into(),
        Tok::Ne => "`!=`".into(),
        Tok::Lt => "`<`".into(),
        Tok::Le => "`<=`".into(),
        Tok::Gt => "`>`".into(),
        Tok::Ge => "`>=`".into(),
        Tok::Ident(name) => format!("identifier `{name}`"),
        Tok::Var(name) => format!("variable `${name}`"),
        Tok::Number(n) => format!("number `{n}`"),
        Tok::Str(_) => "a string literal".into(),
        Tok::Eof => "end of input".into(),
    }
}


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

pub fn parse_program(src: &str, version: JqVersion) -> PResult<Expr> {
    let (toks, spans) = Lexer::new(src).tokenize()?;
    let mut parser = Parser {
        toks,
        spans,
        pos: 0,
        version,
        open_stack: Vec::new(),
    };
    let expr = parser.parse_pipe()?;
    parser.expect_eof()?;
    Ok(expr)
}

/// Used for recursively parsing `\( ... )` string-interpolation bodies,
/// which must target the same version as the enclosing parse.
fn parse_sub_expr(src: &str, version: JqVersion) -> Result<Expr, Diagnostic> {
    parse_program(src, version)
}

struct Parser {
    toks: Vec<Tok>,
    spans: Vec<Span>,
    pos: usize,
    version: JqVersion,
    /// Spans of currently-open `(`/`[`/`{` delimiters, pushed on open and
    /// popped on a successful matching close -- lets an unmatched-
    /// delimiter error point back at where it was opened, the way rustc's
    /// does.
    open_stack: Vec<(char, Span)>,
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.toks[self.pos]
    }

    fn cur_span(&self) -> Span {
        self.spans[self.pos].clone()
    }

    fn bump(&mut self) -> Tok {
        let t = self.toks[self.pos].clone();
        if self.pos + 1 < self.toks.len() {
            self.pos += 1;
        }
        t
    }

    fn err(&self, message: impl Into<String>) -> Diagnostic {
        Diagnostic::at(self.cur_span(), message)
    }

    /// Consumes an opening delimiter, remembering its span for a later
    /// unmatched-close error. Only valid when `peek()` is already known to
    /// be that delimiter.
    fn open_delim(&mut self, ch: char) {
        let span = self.cur_span();
        self.bump();
        self.open_stack.push((ch, span));
    }

    /// Like [`Self::expect`], but for an opening delimiter: also records
    /// its span via [`Self::open_delim`] on success, for a later
    /// [`Self::expect_close`] to point back at.
    fn expect_open(&mut self, t: Tok, ch: char) -> PResult<()> {
        if *self.peek() == t {
            self.open_delim(ch);
            Ok(())
        } else {
            Err(self.err(format!(
                "expected {}, found {}",
                describe(&t),
                describe(self.peek())
            )))
        }
    }

    /// Expects `close`, failing with a message that also points back at
    /// the matching open delimiter pushed by [`Self::open_delim`].
    fn expect_close(&mut self, close: Tok) -> PResult<()> {
        let (open_ch, open_span) = self
            .open_stack
            .pop()
            .expect("expect_close called without a matching open_delim");
        if *self.peek() == close {
            self.bump();
            Ok(())
        } else {
            Err(Diagnostic::at(
                self.cur_span(),
                format!("expected {}, found {}", describe(&close), describe(self.peek())),
            )
            .note(open_span, format!("unclosed '{open_ch}' opened here")))
        }
    }

    fn expect_eof(&self) -> PResult<()> {
        if *self.peek() == Tok::Eof {
            Ok(())
        } else {
            Err(self.err(format!(
                "unexpected trailing input: {}",
                describe(self.peek())
            )))
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
            Err(self.err(format!("expected '{s}', found {}", describe(self.peek()))))
        }
    }

    fn expect(&mut self, t: Tok) -> PResult<()> {
        if *self.peek() == t {
            self.bump();
            Ok(())
        } else {
            Err(self.err(format!(
                "expected {}, found {}",
                describe(&t),
                describe(self.peek())
            )))
        }
    }

    fn expect_var(&mut self) -> PResult<String> {
        let span = self.cur_span();
        match self.bump() {
            Tok::Var(name) => Ok(name),
            other => Err(Diagnostic::at(
                span,
                format!("expected a variable (e.g. `$x`), found {}", describe(&other)),
            )),
        }
    }

    /// Consumes `close`, which the caller has already confirmed is
    /// [`Self::peek`] -- used for the paths that close a delimiter
    /// successfully without going through [`Self::expect_close`] (so the
    /// matching [`Self::open_delim`] entry still gets popped).
    fn close_delim(&mut self) -> Tok {
        self.open_stack.pop();
        self.bump()
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

    /// `EXPR as PATTERN | BODY`, falling through to plain `//`-level expr.
    fn parse_as_binding(&mut self) -> PResult<Expr> {
        let expr = self.parse_alt()?;
        if self.is_ident("as") {
            self.bump();
            let pattern = self.parse_pattern()?;
            self.expect(Tok::Pipe)?;
            let body = self.parse_pipe()?;
            return Ok(Expr::VarBind(Box::new(expr), pattern, Box::new(body)));
        }
        Ok(expr)
    }

    /// An `as` binding's left-hand side: `$x`, or a destructuring
    /// `[pat, ...]`/`{entry, ...}` shape that may itself nest patterns.
    fn parse_pattern(&mut self) -> PResult<Pattern> {
        match self.peek().clone() {
            Tok::Var(name) => {
                self.bump();
                Ok(Pattern::Var(name))
            }
            Tok::LBracket => {
                self.open_delim('[');
                let mut elems = Vec::new();
                if *self.peek() != Tok::RBracket {
                    loop {
                        elems.push(self.parse_pattern()?);
                        if *self.peek() == Tok::Comma {
                            self.bump();
                            continue;
                        }
                        break;
                    }
                }
                self.expect_close(Tok::RBracket)?;
                Ok(Pattern::Array(elems))
            }
            Tok::LBrace => {
                self.open_delim('{');
                let mut entries = Vec::new();
                if *self.peek() != Tok::RBrace {
                    loop {
                        entries.push(self.parse_pattern_entry()?);
                        if *self.peek() == Tok::Comma {
                            self.bump();
                            continue;
                        }
                        break;
                    }
                }
                self.expect_close(Tok::RBrace)?;
                Ok(Pattern::Object(entries))
            }
            other => Err(self.err(format!(
                "expected a pattern (e.g. `$x`, `[$a, $b]`, or `{{a: $a}}`), found {}",
                describe(&other)
            ))),
        }
    }

    /// One `key: pattern` (or `$name` shorthand, meaning key `"name"` bound
    /// via `Pattern::Var("name")`) inside a `{...}` destructuring pattern.
    fn parse_pattern_entry(&mut self) -> PResult<(Expr, Pattern)> {
        match self.peek().clone() {
            Tok::Var(name) => {
                self.bump();
                Ok((str_literal(name.clone()), Pattern::Var(name)))
            }
            Tok::Ident(name) => {
                self.bump();
                self.expect(Tok::Colon)?;
                let pat = self.parse_pattern()?;
                Ok((str_literal(name), pat))
            }
            Tok::Str(parts) => {
                self.bump();
                let converted = convert_str_parts(parts, &mut |s| parse_sub_expr(s, self.version))?;
                self.expect(Tok::Colon)?;
                let pat = self.parse_pattern()?;
                Ok((Expr::Str(converted), pat))
            }
            Tok::LParen => {
                self.open_delim('(');
                let key = self.parse_pipe()?;
                self.expect_close(Tok::RParen)?;
                self.expect(Tok::Colon)?;
                let pat = self.parse_pattern()?;
                Ok((key, pat))
            }
            other => Err(self.err(format!(
                "expected a pattern entry (e.g. `$x`, `key: pattern`), found {}",
                describe(&other)
            ))),
        }
    }

    /// `a // b`, the alternative operator -- looser than `or`/`and`/
    /// comparisons/arithmetic, tighter than `,`/`as`/`|`; right-
    /// associative (`a // b // c` is `a // (b // c)`), matching real
    /// jq's grammar.
    fn parse_alt(&mut self) -> PResult<Expr> {
        let left = self.parse_or()?;
        if *self.peek() == Tok::SlashSlash {
            self.bump();
            let right = self.parse_alt()?;
            return Ok(Expr::Alt(Box::new(left), Box::new(right)));
        }
        Ok(left)
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
            let expr = self.parse_postfix()?;
            return Ok(Expr::Neg(Box::new(expr)));
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
                        Tok::Ident(name) => base = field(base, name, false),
                        Tok::Str(parts) => {
                            let name = lit_str_only(&parts)?;
                            base = field(base, name, false);
                        }
                        other => {
                            return Err(Diagnostic::at(
                                self.spans[self.pos - 1].clone(),
                                format!("expected field name after '.', found {}", describe(&other)),
                            ))
                        }
                    }
                }
                Tok::LBracket => {
                    self.open_delim('[');
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

    /// Unlike object-construction values, a comma inside `[...]` isn't
    /// ambiguous with anything -- `]`/`:` are the only terminators -- so
    /// index/slice-bound expressions use the full `parse_pipe` and
    /// `.[0,1]`-style comma-generator indices work.
    fn parse_bracket_suffix(&mut self, base: Expr) -> PResult<Expr> {
        if *self.peek() == Tok::RBracket {
            self.close_delim();
            return Ok(Expr::IterateAll {
                base: Box::new(base),
                optional: false,
            });
        }
        if *self.peek() == Tok::Colon {
            self.bump();
            let to = self.parse_pipe()?;
            self.expect_close(Tok::RBracket)?;
            return Ok(Expr::Slice {
                base: Box::new(base),
                from: None,
                to: Some(Box::new(to)),
                optional: false,
            });
        }
        let first = self.parse_pipe()?;
        if *self.peek() == Tok::Colon {
            self.bump();
            if *self.peek() == Tok::RBracket {
                self.close_delim();
                return Ok(Expr::Slice {
                    base: Box::new(base),
                    from: Some(Box::new(first)),
                    to: None,
                    optional: false,
                });
            }
            let to = self.parse_pipe()?;
            self.expect_close(Tok::RBracket)?;
            return Ok(Expr::Slice {
                base: Box::new(base),
                from: Some(Box::new(first)),
                to: Some(Box::new(to)),
                optional: false,
            });
        }
        self.expect_close(Tok::RBracket)?;
        Ok(Expr::Index {
            base: Box::new(base),
            index: Box::new(first),
            optional: false,
        })
    }

    fn parse_primary(&mut self) -> PResult<Expr> {
        match self.peek().clone() {
            Tok::Dot => {
                self.bump();
                match self.peek().clone() {
                    Tok::Ident(name) if !is_keyword(&name) => {
                        self.bump();
                        Ok(field(Expr::Identity, name, false))
                    }
                    Tok::Str(parts) => {
                        self.bump();
                        let name = lit_str_only(&parts)?;
                        Ok(field(Expr::Identity, name, false))
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
                let converted = convert_str_parts(parts, &mut |s| parse_sub_expr(s, self.version))?;
                Ok(Expr::Str(converted))
            }
            Tok::Var(name) => {
                let span = self.cur_span();
                self.bump();
                Ok(Expr::VarRef(name, span))
            }
            Tok::LParen => {
                self.open_delim('(');
                let expr = self.parse_pipe()?;
                self.expect_close(Tok::RParen)?;
                Ok(expr)
            }
            Tok::LBracket => {
                self.open_delim('[');
                if *self.peek() == Tok::RBracket {
                    self.close_delim();
                    return Ok(Expr::ArrayConstruct(None));
                }
                let inner = self.parse_pipe()?;
                self.expect_close(Tok::RBracket)?;
                Ok(Expr::ArrayConstruct(Some(Box::new(inner))))
            }
            Tok::LBrace => self.parse_object(),
            Tok::Ident(name) => self.parse_ident_primary(name),
            other => Err(self.err(format!("unexpected token: {}", describe(&other)))),
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
            "label" => self.parse_label(),
            "break" => self.parse_break(),
            "and" | "or" | "as" | "then" | "elif" | "else" | "end" | "catch" => {
                Err(self.err(format!("unexpected keyword '{name}' here")))
            }
            _ => self.parse_call(name),
        }
    }

    /// Call arguments are full filters -- commas inside them are the
    /// ordinary union/generator operator, not argument separators (`;` is
    /// the separator) -- so each one is parsed with `parse_pipe`.
    fn parse_call(&mut self, name: String) -> PResult<Expr> {
        let name_span = self.cur_span();
        self.bump();
        if *self.peek() != Tok::LParen {
            return Ok(Expr::Call(name, vec![], name_span));
        }
        self.open_delim('(');
        let mut args = Vec::new();
        if *self.peek() != Tok::RParen {
            loop {
                args.push(self.parse_pipe()?);
                if *self.peek() == Tok::Semi {
                    self.bump();
                    continue;
                }
                break;
            }
        }
        self.expect_close(Tok::RParen)?;
        Ok(Expr::Call(name, args, name_span))
    }

    fn parse_if(&mut self) -> PResult<Expr> {
        let if_span = self.cur_span();
        self.eat_ident("if")?;
        let mut branches = Vec::new();
        let cond = self.parse_pipe()?;
        self.eat_ident("then")?;
        let then_e = self.parse_pipe()?;
        branches.push((cond, then_e));
        while self.is_ident("elif") {
            self.bump();
            let cond = self.parse_pipe()?;
            self.eat_ident("then")?;
            let then_e = self.parse_pipe()?;
            branches.push((cond, then_e));
        }
        let else_e = if self.is_ident("else") {
            self.bump();
            Some(Box::new(self.parse_pipe()?))
        } else {
            None
        };
        if else_e.is_none() && self.version.if_requires_else() {
            return Err(Diagnostic::at(
                if_span,
                format!(
                    "jq {} requires an explicit 'else' clause in 'if'/'then'/'end'",
                    self.version.as_str()
                ),
            )
            .help("add 'else .' before 'end', or select a jq-version feature from 1.7 onward \
                   (support for omitting 'else' was added in jq 1.7)"));
        }
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

    /// `label $name | body` -- `body` is the *entire* rest of the
    /// enclosing pipe (same trick as `def ... ; rest`: this call's own
    /// `parse_pipe` greedily consumes any further `| ...` stages, so the
    /// label's scope covers all of them, not just the first).
    fn parse_label(&mut self) -> PResult<Expr> {
        self.eat_ident("label")?;
        let name = self.expect_var()?;
        self.expect(Tok::Pipe)?;
        let body = self.parse_pipe()?;
        Ok(Expr::Label(name, Box::new(body)))
    }

    fn parse_break(&mut self) -> PResult<Expr> {
        self.eat_ident("break")?;
        let span = self.cur_span();
        let name = self.expect_var()?;
        Ok(Expr::Break(name, span))
    }

    fn parse_reduce(&mut self) -> PResult<Expr> {
        self.eat_ident("reduce")?;
        let source = self.parse_postfix()?;
        self.eat_ident("as")?;
        let var = self.expect_var()?;
        self.expect_open(Tok::LParen, '(')?;
        let init = self.parse_pipe()?;
        self.expect(Tok::Semi)?;
        let update = self.parse_pipe()?;
        self.expect_close(Tok::RParen)?;
        Ok(Expr::Reduce {
            source: Box::new(source),
            var,
            init: Box::new(init),
            update: Box::new(update),
        })
    }

    fn parse_foreach(&mut self) -> PResult<Expr> {
        self.eat_ident("foreach")?;
        let source = self.parse_postfix()?;
        self.eat_ident("as")?;
        let var = self.expect_var()?;
        self.expect_open(Tok::LParen, '(')?;
        let init = self.parse_pipe()?;
        self.expect(Tok::Semi)?;
        let update = self.parse_pipe()?;
        let extract = if *self.peek() == Tok::Semi {
            self.bump();
            Some(Box::new(self.parse_pipe()?))
        } else {
            None
        };
        self.expect_close(Tok::RParen)?;
        Ok(Expr::Foreach {
            source: Box::new(source),
            var,
            init: Box::new(init),
            update: Box::new(update),
            extract,
        })
    }

    fn parse_def(&mut self) -> PResult<Expr> {
        self.eat_ident("def")?;
        let name_span = self.cur_span();
        let name = match self.bump() {
            Tok::Ident(n) => n,
            other => {
                return Err(Diagnostic::at(
                    name_span,
                    format!("expected function name after 'def', found {}", describe(&other)),
                ))
            }
        };
        let mut params = Vec::new();
        if *self.peek() == Tok::LParen {
            self.open_delim('(');
            loop {
                let span = self.cur_span();
                match self.bump() {
                    Tok::Ident(name) => params.push(Param {
                        name,
                        is_value: false,
                    }),
                    Tok::Var(name) => params.push(Param {
                        name,
                        is_value: true,
                    }),
                    other => {
                        return Err(Diagnostic::at(
                            span,
                            format!("expected parameter name, found {}", describe(&other)),
                        ))
                    }
                }
                if *self.peek() == Tok::Semi {
                    self.bump();
                    continue;
                }
                break;
            }
            self.expect_close(Tok::RParen)?;
        }
        self.expect(Tok::Colon)?;
        let body = self.parse_pipe()?;
        self.expect(Tok::Semi)?;
        let rest = self.parse_pipe()?;
        Ok(Expr::FuncDef {
            name,
            params,
            body: Box::new(body),
            rest: Box::new(rest),
            span: name_span,
        })
    }

    fn parse_object(&mut self) -> PResult<Expr> {
        self.open_delim('{');
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
        self.expect_close(Tok::RBrace)?;
        Ok(Expr::ObjectConstruct(entries))
    }

    fn parse_obj_entry(&mut self) -> PResult<crate::ast::ObjEntry> {
        use crate::ast::ObjEntry;
        match self.peek().clone() {
            Tok::Var(name) => {
                let span = self.cur_span();
                self.bump();
                if *self.peek() == Tok::Colon {
                    self.bump();
                    let val = self.parse_pipe_no_comma()?;
                    Ok(ObjEntry {
                        key: str_literal(name),
                        val,
                    })
                } else {
                    Ok(ObjEntry {
                        key: str_literal(name.clone()),
                        val: Expr::VarRef(name, span),
                    })
                }
            }
            Tok::Ident(name) => {
                self.bump();
                if *self.peek() == Tok::Colon {
                    self.bump();
                    let val = self.parse_pipe_no_comma()?;
                    Ok(ObjEntry {
                        key: str_literal(name),
                        val,
                    })
                } else {
                    Ok(ObjEntry {
                        key: str_literal(name.clone()),
                        val: field(Expr::Identity, name, false),
                    })
                }
            }
            Tok::Str(parts) => {
                self.bump();
                let converted = convert_str_parts(parts, &mut |s| parse_sub_expr(s, self.version))?;
                self.expect(Tok::Colon)?;
                let val = self.parse_pipe_no_comma()?;
                Ok(ObjEntry {
                    key: Expr::Str(converted),
                    val,
                })
            }
            Tok::LParen => {
                self.open_delim('(');
                let key = self.parse_pipe()?;
                self.expect_close(Tok::RParen)?;
                self.expect(Tok::Colon)?;
                let val = self.parse_pipe_no_comma()?;
                Ok(ObjEntry { key, val })
            }
            other => Err(self.err(format!(
                "unexpected token in object construction: {}",
                describe(&other)
            ))),
        }
    }
}

fn field(base: Expr, name: String, optional: bool) -> Expr {
    Expr::Field {
        base: Box::new(base),
        name,
        optional,
    }
}

fn mark_optional(expr: Expr) -> Expr {
    match expr {
        Expr::Field { base, name, .. } => Expr::Field {
            base,
            name,
            optional: true,
        },
        Expr::Index { base, index, .. } => Expr::Index {
            base,
            index,
            optional: true,
        },
        Expr::Slice { base, from, to, .. } => Expr::Slice {
            base,
            from,
            to,
            optional: true,
        },
        Expr::IterateAll { base, .. } => Expr::IterateAll {
            base,
            optional: true,
        },
        other => Expr::Optional(Box::new(other)),
    }
}

fn lit_str_only(parts: &[StrPart]) -> PResult<String> {
    match parts {
        [StrPart::Lit(s)] => Ok(s.clone()),
        [] => Ok(String::new()),
        _ => Err(Diagnostic::new(
            "interpolated strings are not allowed as field names",
        )
        .help("use a plain string, e.g. `.\"literal-name\"`, or `.[expr]` for a computed key")),
    }
}
