//! Hand-rolled lexer for the subset of jq syntax this crate compiles.

use crate::diagnostics::{Diagnostic, Span};

#[derive(Debug, Clone, PartialEq)]
pub enum StrPart {
    Lit(String),
    /// Raw jq source text found inside `\( ... )`, re-lexed/parsed
    /// recursively -- paired with the char offset (into the source the
    /// lexer was reading) of the interpolation's first character, so
    /// errors found while re-parsing it can be relocated back to that
    /// outer source.
    Interp(String, usize),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Dot,
    DotDot,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    LParen,
    RParen,
    Pipe,
    Comma,
    Colon,
    Semi,
    Question,
    Plus,
    Minus,
    Star,
    Slash,
    SlashSlash,
    Percent,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Ident(String),
    Var(String),
    Number(f64),
    Str(Vec<StrPart>),
    Eof,
}

pub struct Lexer {
    chars: Vec<char>,
    pos: usize,
}

pub type LexResult<T> = Result<T, Diagnostic>;

impl Lexer {
    pub fn new(src: &str) -> Self {
        Lexer {
            chars: src.chars().collect(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek2(&self) -> Option<char> {
        self.chars.get(self.pos + 1).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn skip_ws_and_comments(&mut self) {
        loop {
            while matches!(self.peek(), Some(c) if c.is_whitespace()) {
                self.pos += 1;
            }
            if self.peek() == Some('#') {
                while let Some(c) = self.peek() {
                    if c == '\n' {
                        break;
                    }
                    self.pos += 1;
                }
                continue;
            }
            break;
        }
    }

    /// Returns tokens paired with their char-index span in the source,
    /// one-to-one -- `spans[i]` is where `toks[i]` came from.
    pub fn tokenize(mut self) -> LexResult<(Vec<Tok>, Vec<Span>)> {
        let mut toks = Vec::new();
        let mut spans = Vec::new();
        loop {
            self.skip_ws_and_comments();
            let start = self.pos;
            let t = self.next_token()?;
            let is_eof = t == Tok::Eof;
            toks.push(t);
            spans.push(start..self.pos);
            if is_eof {
                break;
            }
        }
        Ok((toks, spans))
    }

    fn next_token(&mut self) -> LexResult<Tok> {
        let c = match self.peek() {
            None => return Ok(Tok::Eof),
            Some(c) => c,
        };
        if c == '.' {
            self.bump();
            if self.peek() == Some('.') {
                self.bump();
                return Ok(Tok::DotDot);
            }
            return Ok(Tok::Dot);
        }
        if c == '"' {
            return self.lex_string();
        }
        if c.is_ascii_digit() {
            return self.lex_number();
        }
        if c == '$' {
            self.bump();
            let name = self.lex_ident_raw();
            return Ok(Tok::Var(name));
        }
        if c.is_alphabetic() || c == '_' {
            let name = self.lex_ident_raw();
            return Ok(Tok::Ident(name));
        }
        self.bump();
        match c {
            '[' => Ok(Tok::LBracket),
            ']' => Ok(Tok::RBracket),
            '{' => Ok(Tok::LBrace),
            '}' => Ok(Tok::RBrace),
            '(' => Ok(Tok::LParen),
            ')' => Ok(Tok::RParen),
            '|' => Ok(Tok::Pipe),
            ',' => Ok(Tok::Comma),
            ':' => Ok(Tok::Colon),
            ';' => Ok(Tok::Semi),
            '?' => Ok(Tok::Question),
            '+' => Ok(Tok::Plus),
            '-' => Ok(Tok::Minus),
            '*' => Ok(Tok::Star),
            '/' => {
                if self.peek() == Some('/') {
                    self.bump();
                    Ok(Tok::SlashSlash)
                } else {
                    Ok(Tok::Slash)
                }
            }
            '%' => Ok(Tok::Percent),
            '=' => {
                if self.peek() == Some('=') {
                    self.bump();
                    Ok(Tok::Eq)
                } else {
                    Err(Diagnostic::at(self.pos - 1..self.pos, "assignment operators are not supported")
                        .help("update-assignment (`=`, `|=`, `+=`, ...) isn't implemented yet; \
                               see the README's \"Known limitations\""))
                }
            }
            '!' => {
                if self.peek() == Some('=') {
                    self.bump();
                    Ok(Tok::Ne)
                } else {
                    Err(Diagnostic::at(self.pos - 1..self.pos, "unexpected '!'")
                        .help("jq has no unary `!`; use `not` instead, e.g. `.a | not`"))
                }
            }
            '<' => {
                if self.peek() == Some('=') {
                    self.bump();
                    Ok(Tok::Le)
                } else {
                    Ok(Tok::Lt)
                }
            }
            '>' => {
                if self.peek() == Some('=') {
                    self.bump();
                    Ok(Tok::Ge)
                } else {
                    Ok(Tok::Gt)
                }
            }
            other => Err(Diagnostic::at(
                self.pos - 1..self.pos,
                format!("unexpected character '{other}'"),
            )),
        }
    }

    fn lex_ident_raw(&mut self) -> String {
        let start = self.pos;
        while matches!(self.peek(), Some(c) if c.is_alphanumeric() || c == '_') {
            self.pos += 1;
        }
        self.chars[start..self.pos].iter().collect()
    }

    fn lex_number(&mut self) -> LexResult<Tok> {
        let start = self.pos;
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.peek() == Some('.') && matches!(self.peek2(), Some(c) if c.is_ascii_digit()) {
            self.pos += 1;
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some('e') | Some('E')) {
            let save = self.pos;
            self.pos += 1;
            if matches!(self.peek(), Some('+') | Some('-')) {
                self.pos += 1;
            }
            if matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                    self.pos += 1;
                }
            } else {
                self.pos = save;
            }
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        text.parse::<f64>().map(Tok::Number).map_err(|_| {
            Diagnostic::at(start..self.pos, format!("invalid number literal '{text}'"))
        })
    }

    fn lex_string(&mut self) -> LexResult<Tok> {
        let open = self.pos;
        self.bump();
        let mut parts = Vec::new();
        let mut cur = String::new();
        loop {
            match self.bump() {
                None => {
                    return Err(Diagnostic::at(open..open + 1, "unterminated string literal")
                        .help("add a closing '\"' to match this one"))
                }
                Some('"') => break,
                Some('\\') => match self.bump() {
                    Some('n') => cur.push('\n'),
                    Some('t') => cur.push('\t'),
                    Some('r') => cur.push('\r'),
                    Some('"') => cur.push('"'),
                    Some('\\') => cur.push('\\'),
                    Some('/') => cur.push('/'),
                    Some('u') => {
                        let hex_start = self.pos;
                        let hex: String = (0..4).filter_map(|_| self.bump()).collect();
                        let code = u32::from_str_radix(&hex, 16).map_err(|_| {
                            Diagnostic::at(hex_start..self.pos, "invalid \\u escape")
                                .help("\\u must be followed by exactly 4 hex digits, e.g. \\u00e9")
                        })?;
                        if let Some(ch) = char::from_u32(code) {
                            cur.push(ch);
                        }
                    }
                    Some('(') => {
                        if !cur.is_empty() {
                            parts.push(StrPart::Lit(std::mem::take(&mut cur)));
                        }
                        let inner_start = self.pos;
                        let inner = self.lex_balanced_parens()?;
                        parts.push(StrPart::Interp(inner, inner_start));
                    }
                    Some(other) => {
                        return Err(Diagnostic::at(
                            self.pos - 1..self.pos,
                            format!("invalid escape '\\{other}'"),
                        ))
                    }
                    None => {
                        return Err(Diagnostic::at(
                            self.pos - 1..self.pos,
                            "unterminated escape sequence",
                        ))
                    }
                },
                Some(c) => cur.push(c),
            }
        }
        if !cur.is_empty() || parts.is_empty() {
            parts.push(StrPart::Lit(cur));
        }
        Ok(Tok::Str(parts))
    }

    /// Called right after consuming `\(`; returns the raw source up to the
    /// matching `)`, respecting nested parens and nested string literals.
    fn lex_balanced_parens(&mut self) -> LexResult<String> {
        let start = self.pos;
        let open = start - 2; // the `\(` that led here
        let mut depth: i32 = 1;
        loop {
            match self.bump() {
                None => {
                    return Err(Diagnostic::at(
                        open..open + 2,
                        "unterminated string interpolation",
                    )
                    .help("add a closing ')' to match this '\\('"))
                }
                Some('(') => depth += 1,
                Some(')') => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(self.chars[start..self.pos - 1].iter().collect());
                    }
                }
                Some('"') => {
                    // skip over a nested string literal verbatim (including escapes)
                    loop {
                        match self.bump() {
                            None => {
                                return Err(Diagnostic::at(
                                    open..open + 2,
                                    "unterminated string literal in interpolation",
                                ))
                            }
                            Some('"') => break,
                            Some('\\') => {
                                self.bump();
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
    }
}
