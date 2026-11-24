//! The jq filter lex -> parse -> lower -> codegen pipeline, as a plain
//! library crate. `jqc-macros` wraps [`lower::parse_and_lower`] and
//! [`codegen::TreeWalkCodegen`] in a thin `#[proc_macro]`; any other crate
//! (e.g. a test generator that needs to try compiling arbitrary filters
//! and catch failures as `Result`s, not macro-expansion-time compile
//! errors) can call them directly, which a `proc-macro = true` crate's
//! callers cannot do.

pub mod ast;
pub mod codegen;
pub mod diagnostics;
pub mod lexer;
pub mod lower;
pub mod parser;
pub mod version;

#[cfg(test)]
mod bench;
