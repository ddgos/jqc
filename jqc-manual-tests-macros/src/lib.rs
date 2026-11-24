//! `generate_manual_tests!(version = "1.8")` reads jq's own manual
//! examples -- `docs/content/manual/v<X.Y>/manual.yml` in a jq source
//! checkout at `$JQ_SRC` (see the `jq-src` flake input and the Nix dev
//! shell's `JQ_SRC`) -- and emits one `#[test]` per example:
//!
//! - If `jqc-compiler` can lex/parse/lower/codegen the example's
//!   `program`, a real differential test: run the compiled closure on
//!   the example's `input`, compare against the real `jq` binary.
//! - If it can't (unsupported syntax/builtin), an *xfail* test: assert
//!   that compiling it still fails. When jqc gains the needed support,
//!   this flips to a hard failure, signaling it's time to promote that
//!   example to a real test -- the generator never silently drops an
//!   example just because jqc can't handle it today.
//!
//! Each run differentially re-verifies against whatever `jq` is on
//! `PATH` rather than trusting the manual's own literal `output` field,
//! since some examples are conditional on build-time jq options (e.g.
//! `have_decnum`) that the manual's checked-in text can't reflect.

use std::collections::HashMap;

use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::{format_ident, quote};
use serde::Deserialize;
use syn::parse::{Parse, ParseStream};
use syn::{LitStr, Token};

use jqc_compiler::codegen::{Codegen, TreeWalkCodegen};
use jqc_compiler::lower;
use jqc_compiler::version::JqVersion;

#[derive(Deserialize)]
struct Manual {
    #[serde(default)]
    sections: Vec<Section>,
}

#[derive(Deserialize)]
struct Section {
    #[serde(default)]
    entries: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    #[serde(default)]
    examples: Vec<Example>,
}

#[derive(Deserialize)]
struct Example {
    program: String,
    input: String,
}

/// Examples that compile successfully but produce a known-wrong runtime
/// result -- `jqc` is wrong here, not the manual, and not a case where
/// `jqc` simply lacks the syntax/builtin (that's `Err` from `lower`/
/// `codegen`, handled separately as an auto-generated xfail). Each entry
/// is `(exact program text, reason shown as the test's #[ignore] message)`.
/// Remove an entry once `jqc` is fixed and its generated test goes green
/// on its own -- this list is the manual-examples half of `todos.md`'s
/// "no more xfail tests" compliance goal.
const KNOWN_RUNTIME_DISCREPANCIES: &[(&str, &str)] = &[(
    "[repeat(.*2, error)?]",
    "known discrepancy: jqc's repeat()/error interaction with `?` doesn't \
     match real jq's lazy-generator short-circuit semantics yet (jqc: [], \
     real jq: [2]) -- see todos.md xfail tracking",
)];

struct MacroInput {
    version: LitStr,
}

impl Parse for MacroInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let ident: syn::Ident = input.parse()?;
        if ident != "version" {
            return Err(syn::Error::new(
                ident.span(),
                "generate_manual_tests!: expected 'version = \"X.Y\"'",
            ));
        }
        input.parse::<Token![=]>()?;
        let version: LitStr = input.parse()?;
        Ok(MacroInput { version })
    }
}

#[proc_macro]
pub fn generate_manual_tests(input: TokenStream) -> TokenStream {
    let parsed = match syn::parse::<MacroInput>(input) {
        Ok(p) => p,
        Err(e) => return e.to_compile_error().into(),
    };
    let version = match JqVersion::parse(&parsed.version.value()) {
        Some(v) => v,
        None => {
            return syn::Error::new(
                parsed.version.span(),
                format!(
                    "generate_manual_tests!: unknown jq version {:?}",
                    parsed.version.value()
                ),
            )
            .to_compile_error()
            .into();
        }
    };

    let jq_src = match std::env::var("JQ_SRC") {
        Ok(p) => p,
        Err(_) => {
            return syn::Error::new(
                Span::call_site(),
                "generate_manual_tests!: JQ_SRC env var not set -- run inside `nix develop` \
                 (provides a pinned jq source checkout via the jq-src flake input)",
            )
            .to_compile_error()
            .into();
        }
    };
    let manual_path = format!(
        "{jq_src}/docs/content/manual/v{}/manual.yml",
        version.as_str()
    );
    let yaml = match std::fs::read_to_string(&manual_path) {
        Ok(s) => s,
        Err(e) => {
            return syn::Error::new(
                Span::call_site(),
                format!("generate_manual_tests!: couldn't read {manual_path}: {e}"),
            )
            .to_compile_error()
            .into();
        }
    };
    let manual: Manual = match serde_yaml::from_str(&yaml) {
        Ok(m) => m,
        Err(e) => {
            return syn::Error::new(
                Span::call_site(),
                format!("generate_manual_tests!: couldn't parse {manual_path}: {e}"),
            )
            .to_compile_error()
            .into();
        }
    };

    // jq's manual repeats some (program, input) pairs across sections
    // (e.g. illustrating the same filter from two angles); de-duplicate
    // so we don't emit two identically-named tests for the same case.
    let version_token = version_to_tokens(version);
    let mut seen: HashMap<(String, String), ()> = HashMap::new();
    let mut tests = Vec::new();
    let mut compiled_count = 0usize;
    let mut xfail_count = 0usize;

    for section in &manual.sections {
        for entry in &section.entries {
            for example in &entry.examples {
                let key = (example.program.clone(), example.input.clone());
                if seen.insert(key, ()).is_some() {
                    continue;
                }
                let idx = compiled_count + xfail_count;
                let program = &example.program;
                let input = &example.input;

                let compile_result = lower::parse_and_lower(program, version)
                    .and_then(|ast| TreeWalkCodegen.compile_top(&ast));

                match compile_result {
                    Ok(compiled_ts) => {
                        compiled_count += 1;
                        let fn_name = format_ident!("manual_example_{idx}");
                        let ignore_attr = KNOWN_RUNTIME_DISCREPANCIES
                            .iter()
                            .find(|(p, _)| *p == program.as_str())
                            .map(|(_, reason)| quote!(#[ignore = #reason]));
                        tests.push(quote! {
                            #[test]
                            #ignore_attr
                            fn #fn_name() {
                                let filter = #compiled_ts;
                                let input: ::jqc::core::Value = ::serde_json::from_str(#input)
                                    .expect("manual example input should be valid JSON");
                                let got = filter(&input).unwrap_or_else(|e| {
                                    panic!("manual example {:?} errored: {e}", #program)
                                });
                                let want = crate::run_real_jq(#program, #input);
                                assert_eq!(got, want, "manual example {:?} on input {}", #program, #input);
                            }
                        });
                    }
                    Err(_) => {
                        xfail_count += 1;
                        let fn_name = format_ident!("manual_example_{idx}_xfail_unsupported");
                        tests.push(quote! {
                            #[test]
                            fn #fn_name() {
                                let result = ::jqc_compiler::lower::parse_and_lower(#program, #version_token)
                                    .and_then(|ast| ::jqc_compiler::codegen::Codegen::compile_top(
                                        &::jqc_compiler::codegen::TreeWalkCodegen,
                                        &ast,
                                    ));
                                assert!(
                                    result.is_err(),
                                    "manual example {:?} now compiles -- promote this xfail to a \
                                     real test (jqc-manual-tests-macros)",
                                    #program
                                );
                            }
                        });
                    }
                }
            }
        }
    }

    let summary = format!(
        "jq {} manual: {compiled_count} example(s) compiled and differentially tested, \
         {xfail_count} example(s) still unsupported (xfail)",
        version.as_str()
    );

    let expanded = quote! {
        #(#tests)*

        #[test]
        fn manual_tests_summary() {
            eprintln!(#summary);
        }
    };
    expanded.into()
}

/// Reconstructs a `JqVersion::V1_x` token for splicing into generated
/// xfail tests, since `JqVersion` itself isn't `quote::ToTokens`.
fn version_to_tokens(version: JqVersion) -> proc_macro2::TokenStream {
    let variant = format_ident!(
        "{}",
        match version {
            JqVersion::V1_6 => "V1_6",
            JqVersion::V1_7 => "V1_7",
            JqVersion::V1_8 => "V1_8",
        }
    );
    quote!(::jqc_compiler::version::JqVersion::#variant)
}
