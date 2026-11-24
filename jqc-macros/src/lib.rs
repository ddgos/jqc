//! `jq!("<filter>")` compiles a jq filter directly into Rust code at macro
//! expansion time: lex -> parse -> desugar/inline `def`s -> codegen. The
//! expansion is a closure `Fn(&jqc::core::Value) -> jqc::core::JResult<Vec<jqc::core::Value>>`;
//! no jq parsing or AST walking happens at runtime.
//!
//! `jq!(version = "1.6", "<filter>")` targets a specific jq language
//! version instead of the newest available one (the default when no
//! `version` argument is given) -- see `jqc_compiler::version` for which
//! versions are selectable and what differs between them. Which versions
//! are *available* to select is controlled by Cargo features (`jq-1-6`,
//! `jq-1-7`, `jq-1-8`, on `jqc`/`jqc-macros`/`jqc-compiler`); naming a
//! version whose feature isn't enabled is a compile error.
//!
//! `jq!(codegen = "simd", "<filter>")` selects `jqc_compiler::codegen`'s
//! `SimdCodegen` instead of the default `TreeWalkCodegen` -- identical
//! output except for `ascii_downcase`/`ascii_upcase`, which it backs
//! with an explicit-SIMD `jqc-core` implementation. `version` and
//! `codegen` can both be given, in either order, before the filter.
//!
//! Requires the `jqc` crate (not just this one) as a dependency, since the
//! generated code references `::jqc::core::*`.
//!
//! This crate is a thin `#[proc_macro]` wrapper; the actual lex/parse/
//! lower/codegen pipeline lives in `jqc-compiler`, a plain library crate --
//! `proc-macro = true` crates can only export proc-macro items to other
//! crates, so anything that needs to call the pipeline directly (e.g. a
//! test generator trying many filters and catching failures as `Result`s)
//! has to go through `jqc-compiler`, not this one.

use proc_macro::TokenStream;
use proc_macro2::Span;
use syn::parse::{Parse, ParseStream};
use syn::{LitStr, Token};

use jqc_compiler::codegen::{Codegen, SimdCodegen, TreeWalkCodegen};
use jqc_compiler::lower;
use jqc_compiler::version::JqVersion;

/// `jq!("filter")`, `jq!(version = "X.Y", "filter")`,
/// `jq!(codegen = "simd", "filter")`, or both (in either order) before
/// the filter.
struct MacroInput {
    version: Option<LitStr>,
    codegen: Option<LitStr>,
    filter: LitStr,
}

impl Parse for MacroInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut version = None;
        let mut codegen = None;
        while input.peek(syn::Ident) && input.peek2(Token![=]) {
            let ident: syn::Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            let value: LitStr = input.parse()?;
            input.parse::<Token![,]>()?;
            if ident == "version" {
                version = Some(value);
            } else if ident == "codegen" {
                codegen = Some(value);
            } else {
                return Err(syn::Error::new(
                    ident.span(),
                    format!("jq!: unexpected argument '{ident}', expected 'version' or 'codegen'"),
                ));
            }
        }
        let filter: LitStr = input.parse()?;
        Ok(MacroInput {
            version,
            codegen,
            filter,
        })
    }
}

fn supported_versions_str() -> String {
    JqVersion::ALL
        .iter()
        .map(|v| v.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

#[proc_macro]
pub fn jq(input: TokenStream) -> TokenStream {
    let parsed = match syn::parse::<MacroInput>(input) {
        Ok(p) => p,
        Err(e) => return e.to_compile_error().into(),
    };

    let version = match &parsed.version {
        Some(lit) => match JqVersion::parse(&lit.value()) {
            Some(v) => v,
            None => {
                return syn::Error::new(
                    lit.span(),
                    format!(
                        "jq!: unknown jq version {:?} (supported: {})",
                        lit.value(),
                        supported_versions_str()
                    ),
                )
                .to_compile_error()
                .into();
            }
        },
        None => match JqVersion::default_available() {
            Some(v) => v,
            None => {
                return syn::Error::new(
                    Span::call_site(),
                    "jq!: no jq-version feature enabled -- enable jq-1-8, jq-1-7, or jq-1-6 \
                     on your jqc dependency",
                )
                .to_compile_error()
                .into();
            }
        },
    };

    if !version.is_available() {
        let span = parsed
            .version
            .as_ref()
            .map(|v| v.span())
            .unwrap_or_else(Span::call_site);
        return syn::Error::new(
            span,
            format!(
                "jq!: jq version \"{}\" is valid but not enabled -- add feature \"jq-{}\" \
                 to your jqc dependency",
                version.as_str(),
                version.as_str().replace('.', "-")
            ),
        )
        .to_compile_error()
        .into();
    }

    let backend: Box<dyn Codegen> = match &parsed.codegen {
        None => Box::new(TreeWalkCodegen),
        Some(lit) => match lit.value().as_str() {
            "treewalk" => Box::new(TreeWalkCodegen),
            "simd" => Box::new(SimdCodegen),
            other => {
                return syn::Error::new(
                    lit.span(),
                    format!("jq!: unknown codegen backend {other:?} (supported: \"treewalk\", \"simd\")"),
                )
                .to_compile_error()
                .into();
            }
        },
    };

    let src = parsed.filter.value();
    // parse_and_lower's own errors come back already relocated to `src`'s
    // coordinates; compile_top's don't, since it runs after
    // parse_and_lower has already returned a plain `Expr` with no memory
    // of where the prelude/wrapper it was parsed alongside ended, so those
    // get relocated here instead, right before rendering.
    let compiled = match lower::parse_and_lower(&src, version) {
        Ok(ast) => {
            let (offset, len) = lower::user_src_window(&src);
            backend
                .compile_top(&ast)
                .map_err(|e| e.relocate(offset, len))
        }
        Err(e) => Err(e),
    };
    match compiled {
        Ok(ts) => {
            // A cheap early-warning canary for codegen-size blowups (see
            // `jqc_core::partial_for`'s doc comment for the incident this
            // is here because of): no assertion, since "how big is too
            // big" depends on the filter, but printing it makes a future
            // regression visible immediately instead of only showing up
            // as a mysteriously slow/memory-hungry downstream build.
            if std::env::var_os("JQC_DEBUG_EXPAND_SIZE").is_some() {
                let s = ts.to_string();
                eprintln!(
                    "jq!({src:?}): expanded to {} chars, {} braces",
                    s.len(),
                    s.matches('{').count()
                );
            }
            ts.into()
        }
        Err(diag) => syn::Error::new(parsed.filter.span(), format!("jq!:\n{}", diag.render(&src)))
            .to_compile_error()
            .into(),
    }
}
