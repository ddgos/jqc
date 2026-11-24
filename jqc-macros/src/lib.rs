//! `jq!("<filter>")` compiles a jq filter directly into Rust code at macro
//! expansion time: lex -> parse -> desugar/inline `def`s -> codegen. The
//! expansion is a closure `Fn(&jqc::core::Value) -> jqc::core::JResult<Vec<jqc::core::Value>>`;
//! no jq parsing or AST walking happens at runtime.
//!
//! Requires the `jqc` crate (not just this one) as a dependency, since the
//! generated code references `::jqc::core::*`.

mod ast;
#[cfg(test)]
mod bench;
mod codegen;
mod lexer;
mod lower;
mod parser;

use proc_macro::TokenStream;
use syn::LitStr;

use codegen::{Codegen, TreeWalkCodegen};

#[proc_macro]
pub fn jq(input: TokenStream) -> TokenStream {
    let lit = match syn::parse::<LitStr>(input) {
        Ok(l) => l,
        Err(e) => return e.to_compile_error().into(),
    };
    let src = lit.value();
    match lower::parse_and_lower(&src).and_then(|ast| TreeWalkCodegen.compile_top(&ast)) {
        Ok(ts) => ts.into(),
        Err(msg) => syn::Error::new(lit.span(), format!("jq!: {msg}"))
            .to_compile_error()
            .into(),
    }
}
