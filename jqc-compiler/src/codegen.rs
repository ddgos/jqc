//! Lowered-AST -> Rust `TokenStream` codegen.
//!
//! Every [`compile`] call takes `input`, a token stream that must be a valid
//! Rust expression of type `&jqc::core::Value` (the current `.`), and
//! returns a token stream that is a valid Rust expression of type
//! `jqc::core::JResult<Vec<jqc::core::Value>>`. Composite nodes (pipe,
//! comma, binops, …) are cartesian products over their operands' output
//! streams, matching jq's generator semantics; each sub-expression is
//! compiled into its own nested block, so plain repeated local names are
//! safe across sibling/nested calls (ordinary Rust block scoping keeps them
//! from colliding) and no fresh-name counter is needed.

use proc_macro2::{Ident, TokenStream};
use quote::{format_ident, quote};

use crate::ast::{BinOp, Expr, ObjEntry, Pattern, StrPart};
use crate::diagnostics::{closest_match, Diagnostic, KNOWN_NAMES};

pub type CResult<T> = Result<T, Diagnostic>;

/// A backend that turns a lowered jq AST into the Rust `TokenStream` that
/// `jq!` expands to. [`TreeWalkCodegen`] below is the only implementation
/// today; the trait exists so alternative backends (SIMD, multi-core --
/// see `todos.md`) can be added as separate implementations without
/// touching this one, and compared against it on equal footing.
pub trait Codegen {
    fn compile_top(&self, expr: &Expr) -> CResult<TokenStream>;
}

/// The default (and currently only) backend: a direct, recursive tree-walk
/// that compiles each AST node into nested Rust loops over `Vec<Value>`,
/// one node at a time, matching jq's cartesian-product generator semantics
/// structurally in the generated control flow.
pub struct TreeWalkCodegen;

impl Codegen for TreeWalkCodegen {
    fn compile_top(&self, expr: &Expr) -> CResult<TokenStream> {
        compile_top(expr)
    }
}

const CORE: &str = "::jqc::core";

fn core_path(name: &str) -> TokenStream {
    let path: TokenStream = CORE.parse().unwrap();
    let ident = format_ident!("{name}");
    quote!(#path::#ident)
}

fn jv_ident(name: &str) -> Ident {
    format_ident!("__jv_{}", name)
}

fn fn_ident(name: &str) -> Ident {
    format_ident!("__jqc_fn_{}", name)
}

/// Labels and variables are tracked in the same `scope: Vec<String>` (see
/// [`compile_raw`]'s `VarRef`/`Label`/`Break` arms) -- this prefix can
/// never collide with a real jq identifier (which the lexer never lets
/// contain a NUL byte), so a plain string list serves as a scope stack
/// for both without needing a second parameter threaded through every
/// `compile`/`compile_raw` call site.
fn label_key(name: &str) -> String {
    format!("\0label\0{name}")
}

/// `let #binding = #fetch_or_bubble(...)` fetches a fallible step's own
/// `Vec<Value>` result into `binding` (for further per-element processing
/// right there, unlike [`partial_aware_for`]'s pass-through case) while
/// still merging a pre-existing `__out: Vec<Value>` accumulator into the
/// error's `emitted` on failure, the same way [`core::bubble`] would --
/// used by `Expr::Reduce`/`Expr::Foreach`'s `update` clause, which must
/// fetch (not just extend-and-discard) its own result to drive the next
/// iteration's state, but can still fail (e.g. a `break` from inside it)
/// after `__out` already holds completed earlier iterations' output. A
/// bare `(#expr)?` here -- what this replaced -- would propagate that
/// error while silently discarding everything `__out` already had.
fn fetch_or_bubble(binding: TokenStream, expr: TokenStream) -> TokenStream {
    quote! {
        let #binding = match #expr {
            ::std::result::Result::Ok(v) => v,
            ::std::result::Result::Err(mut e) => {
                let mut __taken = ::std::mem::take(&mut __out);
                __taken.append(&mut e.emitted);
                e.emitted = __taken;
                return Err(e);
            }
        };
    }
}

/// Builds a `JResult<Vec<Value>>`-typed expression that iterates `sub_c`'s
/// output, binding each element to `item_ident` and running `body` once
/// per element -- `body` extends/pushes onto its `__out: &mut Vec<Value>`
/// parameter (directly, or via `#core_path("bubble"/"bubble1")`, which
/// take `&mut Vec<Value>` the same way, if it can itself fail).
///
/// This is for *pass-through* nodes, where each element `sub_c` produces
/// directly feeds a piece of this node's own output (e.g. `Pipe`'s operand,
/// `Field`'s base, `BinOp`'s operands) -- as opposed to *collector* nodes
/// (`ArrayConstruct`, string interpolation, …) where `sub_c`'s elements are
/// folded into one combined output and never individually observable.
///
/// For pass-through nodes, jq's lazy generators mean a `try`/`?` around
/// this node must still see whatever elements were already produced before
/// an error -- both elements `body` already pushed for earlier `sub_c`
/// items, *and*, if `sub_c` itself fails partway through generating items,
/// the items `sub_c` had already produced. [`core::partial_for`] (the
/// runtime function this calls) handles that; a `return Err(e)` inside
/// `body` only exits `body`'s own closure, becoming that function's `Err`
/// the normal way, which it then merges with the accumulator exactly like
/// a `body` that failed via `?` through `bubble`/`bubble1`.
///
/// Deliberately a call into a real function, not raw tokens inlined at
/// every call site: nesting this per level of pass-through structure in
/// the source composes additively in *generated code size*, rather than
/// each level re-inlining the loop/merge shape as more raw tokens (what
/// this used to do -- see `partial_for`'s own doc comment for the real
/// blowup that caused).
fn partial_aware_for(sub_c: &TokenStream, item_ident: &TokenStream, body: TokenStream) -> TokenStream {
    let partial_for = core_path("partial_for");
    let value_ty = core_path("Value");
    let jresult_ty = core_path("JResult");
    quote! {
        #partial_for((#sub_c), |__out: &mut ::std::vec::Vec<#value_ty>, #item_ident: #value_ty| -> #jresult_ty<()> {
            #body
            Ok(())
        })
    }
}

pub fn compile_top(expr: &Expr) -> CResult<TokenStream> {
    let mut scope: Vec<String> = Vec::new();
    let input_ident = format_ident!("__jqc_input");
    let input_tok = quote!(#input_ident);
    let body = compile(expr, &input_tok, &mut scope)?;
    let value_ty = core_path("Value");
    let jresult_ty = core_path("JResult");
    Ok(quote! {
        move |#input_ident: &#value_ty| -> #jresult_ty<::std::vec::Vec<#value_ty>> {
            #body
        }
    })
}

/// Compiles `expr` and wraps the result as `(|| -> JResult<Vec<Value>> {
/// ... })()`. The explicit return-type annotation pins both the value type
/// and the error type for this node right where it's produced, instead of
/// leaving a bare `{ ...; Ok(x) }` block's type to be inferred from
/// whatever the caller eventually does with it -- which in practice can be
/// several `for`/`?` layers away and not always enough for rustc to solve
/// without this anchor. A `return` inside the wrapped closure only exits
/// that closure, turning it into an `Err` the normal way; every caller
/// consumes nested results via `(...)?`, so this still aborts the whole
/// filter on error, just via cascading `?` instead of a raw unwind -- same
/// observable result, and trivial enough for LLVM to inline away in
/// release builds.
fn compile(expr: &Expr, input: &TokenStream, scope: &mut Vec<String>) -> CResult<TokenStream> {
    let inner = compile_raw(expr, input, scope)?;
    Ok(wrap_fallible(inner))
}

fn wrap_fallible(inner: TokenStream) -> TokenStream {
    let jresult_ty = core_path("JResult");
    let value_ty = core_path("Value");
    quote! {
        (|| -> #jresult_ty<::std::vec::Vec<#value_ty>> { #inner })()
    }
}

fn compile_raw(expr: &Expr, input: &TokenStream, scope: &mut Vec<String>) -> CResult<TokenStream> {
    match expr {
        Expr::Identity => Ok(quote!(Ok(::std::vec![(#input).clone()]))),
        Expr::RecurseDefault => Err(Diagnostic::new(
            "internal error: RecurseDefault should have been lowered",
        )),
        Expr::Null => {
            let value_ty = core_path("Value");
            Ok(quote!(Ok(::std::vec![#value_ty::Null])))
        }
        Expr::Bool(b) => {
            let value_ty = core_path("Value");
            Ok(quote!(Ok(::std::vec![#value_ty::Bool(#b)])))
        }
        Expr::Number(n) => {
            let value_ty = core_path("Value");
            let number_from_f64 = core_path("number_from_f64");
            // A bare float literal token can't represent a non-finite
            // value (`quote`'s `ToTokens` for `f64` asserts `is_finite()`
            // and panics otherwise) -- an overflowing literal like
            // `1e999999999` (real jq keeps such literals' exact decimal
            // text and prints them back verbatim; jqc's `Value` is a
            // plain `f64`, so this is already a divergence, not a new
            // one) parses to `f64::INFINITY` and must be spelled as that
            // expression instead of a literal.
            let n_expr = if n.is_finite() {
                quote!(#n)
            } else if n.is_nan() {
                quote!(::std::primitive::f64::NAN)
            } else if *n > 0.0 {
                quote!(::std::primitive::f64::INFINITY)
            } else {
                quote!(::std::primitive::f64::NEG_INFINITY)
            };
            Ok(quote!(Ok(
                ::std::vec![#value_ty::Number(#number_from_f64(#n_expr))]
            )))
        }
        Expr::Str(parts) => compile_str(parts, input, scope),
        Expr::Field {
            base,
            name,
            optional,
        } => {
            let index_field = core_path("index_field");
            let base_c = compile(base, input, scope)?;
            let body = quote! {
                match #index_field(&__base, #name) {
                    Ok(v) => __out.push(v),
                    Err(e) => if !#optional { return Err(e); },
                }
            };
            Ok(partial_aware_for(&base_c, &quote!(__base), body))
        }
        Expr::Index {
            base,
            index,
            optional,
        } => {
            let index_value = core_path("index_value");
            let base_c = compile(base, input, scope)?;
            let index_c = compile(index, input, scope)?;
            // As with binops, jq evaluates the bracket expression's
            // generator as the outer loop, the base as the inner one;
            // nesting `partial_aware_for` for both means a failure in
            // either generator (or a non-optional lookup itself) still
            // keeps whatever lookups already completed.
            let inner_body = quote! {
                match #index_value(&__base, &__key) {
                    Ok(v) => __out.push(v),
                    Err(e) => if !#optional { return Err(e); },
                }
            };
            let inner = partial_aware_for(&base_c, &quote!(__base), inner_body);
            let bubble = core_path("bubble");
            let outer_body = quote! {
                #bubble(__out, (#inner))?;
            };
            Ok(partial_aware_for(&index_c, &quote!(__key), outer_body))
        }
        Expr::Slice {
            base,
            from,
            to,
            optional,
        } => compile_slice(
            base,
            from.as_deref(),
            to.as_deref(),
            *optional,
            input,
            scope,
        ),
        Expr::IterateAll { base, optional } => {
            let iterate = core_path("iterate");
            let base_c = compile(base, input, scope)?;
            let body = quote! {
                match #iterate(&__base) {
                    Ok(vs) => __out.extend(vs),
                    Err(e) => if !#optional { return Err(e); },
                }
            };
            Ok(partial_aware_for(&base_c, &quote!(__base), body))
        }
        Expr::Pipe(lhs, rhs) => {
            let lhs_c = compile(lhs, input, scope)?;
            let rhs_c = compile(rhs, &quote!(&__piped), scope)?;
            let bubble = core_path("bubble");
            let body = quote! {
                #bubble(__out, (#rhs_c))?;
            };
            Ok(partial_aware_for(&lhs_c, &quote!(__piped), body))
        }
        Expr::Comma(lhs, rhs) => {
            let lhs_c = compile(lhs, input, scope)?;
            let rhs_c = compile(rhs, input, scope)?;
            let bubble = core_path("bubble");
            Ok(quote! {
                {
                    let mut __out = ::std::vec::Vec::new();
                    #bubble(&mut __out, (#lhs_c))?;
                    #bubble(&mut __out, (#rhs_c))?;
                    Ok(__out)
                }
            })
        }
        Expr::ArrayConstruct(inner) => {
            let value_ty = core_path("Value");
            match inner {
                None => Ok(quote!(Ok(
                    ::std::vec![#value_ty::Array(::std::vec::Vec::new())]
                ))),
                Some(e) => {
                    let inner_c = compile(e, input, scope)?;
                    Ok(quote!(Ok(::std::vec![#value_ty::Array((#inner_c)?)])))
                }
            }
        }
        Expr::ObjectConstruct(entries) => compile_object(entries, input, scope),
        Expr::Neg(x) => {
            let neg_value = core_path("neg_value");
            let x_c = compile(x, input, scope)?;
            let bubble1 = core_path("bubble1");
            let body = quote! {
                #bubble1(__out, #neg_value(__val))?;
            };
            Ok(partial_aware_for(&x_c, &quote!(__val), body))
        }
        Expr::BinOp(op, lhs, rhs) => compile_binop(*op, lhs, rhs, input, scope),
        Expr::And(lhs, rhs) => compile_and_or(lhs, rhs, input, scope, true),
        Expr::Or(lhs, rhs) => compile_and_or(lhs, rhs, input, scope, false),
        Expr::Alt(lhs, rhs) => {
            let lhs_c = compile(lhs, input, scope)?;
            let rhs_c = compile(rhs, input, scope)?;
            let truthy = core_path("truthy");
            // `a // b` keeps only `a`'s non-`false`/`null` outputs,
            // falling back to `b` only if that filtered set ends up
            // empty -- unlike `?`/`try`, an error in `a` is *not*
            // caught; it propagates (verified against real jq), so
            // `partial_aware_for` is exactly right here: it already
            // keeps whatever `a` had filtered-and-pushed before a later
            // failure and re-raises with that as `emitted`.
            let body = quote! {
                if #truthy(&__v) {
                    __out.push(__v);
                }
            };
            let lhs_block = partial_aware_for(&lhs_c, &quote!(__v), body);
            Ok(quote! {
                {
                    let __filtered = (#lhs_block)?;
                    if !__filtered.is_empty() {
                        Ok(__filtered)
                    } else {
                        #rhs_c
                    }
                }
            })
        }
        Expr::If(branches, else_branch) => {
            compile_if(branches, else_branch.as_deref(), input, scope)
        }
        Expr::TryCatch(body, catch) => {
            let value_ty = core_path("Value");
            let jresult_ty = core_path("JResult");
            let bubble = core_path("bubble");
            let body_c = compile(body, input, scope)?;
            // jq's `try`/`?` only suppresses the error signal -- any
            // values `body` already produced before erroring were already
            // streamed out, so they stay in the result (`e.emitted`); a
            // `catch` handler's own outputs are appended after those.
            // `break` is *not* specially exempted here -- confirmed
            // against real jq (`label $out | try break $out catch "x"`
            // produces `"x"`, not an uncaught break): only a `break`'s
            // *own matching label* is guaranteed to see it, by virtue of
            // being the nearest thing downstream that specifically checks
            // for it (`Expr::Label` below); any `try`/`catch` in between
            // catches it exactly like any other error, which is why
            // `label`-using code (e.g. this crate's own `limit` prelude
            // def) must avoid wrapping its own `break` in a `try`/`?` it
            // doesn't want to swallow it.
            let handler = match catch {
                Some(c) => {
                    let catch_c = compile(c, &quote!(&__caught), scope)?;
                    quote! {
                        Err(mut e) => {
                            let mut __out = ::std::mem::take(&mut e.emitted);
                            let __caught = e.into_value();
                            #bubble(&mut __out, (#catch_c))?;
                            Ok(__out)
                        }
                    }
                }
                None => quote! { Err(mut e) => Ok(::std::mem::take(&mut e.emitted)) },
            };
            Ok(quote! {
                {
                    let __result: #jresult_ty<::std::vec::Vec<#value_ty>> = (|| #body_c)();
                    match __result {
                        Ok(v) => Ok(v),
                        #handler
                    }
                }
            })
        }
        Expr::Optional(body) => {
            let value_ty = core_path("Value");
            let jresult_ty = core_path("JResult");
            let body_c = compile(body, input, scope)?;
            Ok(quote! {
                {
                    let __result: #jresult_ty<::std::vec::Vec<#value_ty>> = (|| #body_c)();
                    match __result {
                        Ok(v) => Ok(v),
                        // `?` catches a `break` exactly like any other
                        // error -- see the longer note on `Expr::TryCatch`.
                        Err(mut e) => Ok(::std::mem::take(&mut e.emitted)),
                    }
                }
            })
        }
        Expr::Label(name, body) => {
            let value_ty = core_path("Value");
            let jresult_ty = core_path("JResult");
            scope.push(label_key(name));
            let body_c = compile(body, input, scope);
            scope.pop();
            let body_c = body_c?;
            Ok(quote! {
                {
                    let __result: #jresult_ty<::std::vec::Vec<#value_ty>> = (|| #body_c)();
                    match __result {
                        // Only a `break` naming *this* label is ours to
                        // catch -- anything else (a real error, or a
                        // `break` for a differently-named, or outer,
                        // label) propagates unchanged, so it keeps
                        // unwinding to whichever `label`/`try` it
                        // actually belongs to.
                        Err(mut e) if e.break_label.as_deref() == Some(#name) => {
                            Ok(::std::mem::take(&mut e.emitted))
                        }
                        other => other,
                    }
                }
            })
        }
        Expr::Break(name, span) => {
            let key = label_key(name);
            if !scope.iter().any(|s| s == &key) {
                let labels_in_scope = scope
                    .iter()
                    .filter_map(|s| s.strip_prefix("\0label\0"));
                let err = Diagnostic::at(span.clone(), format!("undefined label ${name}"));
                let err = match closest_match(name, labels_in_scope) {
                    Some(sugg) => err.help(format!("did you mean break ${sugg}?")),
                    None => err.help(format!(
                        "break ${name} needs an enclosing `label ${name} | ...`"
                    )),
                };
                return Err(err);
            }
            let jq_error = core_path("JqError");
            Ok(quote!(Err(#jq_error::brk(#name))))
        }
        Expr::VarBind(source, pattern, body) => {
            let name = match pattern {
                Pattern::Var(name) => name,
                Pattern::Array(_) | Pattern::Object(_) => {
                    return Err(Diagnostic::new(
                        "internal error: destructuring pattern should have been desugared \
                         by lowering",
                    ))
                }
            };
            let jv = jv_ident(name);
            let source_c = compile(source, input, scope)?;
            scope.push(name.clone());
            let body_c = compile(body, input, scope);
            scope.pop();
            let body_c = body_c?;
            let bubble = core_path("bubble");
            let body = quote! {
                #bubble(__out, (#body_c))?;
            };
            Ok(partial_aware_for(&source_c, &quote!(#jv), body))
        }
        Expr::VarRef(name, span) => {
            if !scope.iter().any(|s| s == name) {
                // Labels share `scope` with variables (see `label_key`)
                // but live in a separate namespace -- exclude them here
                // so a typo'd `$var` never gets suggested a label name.
                let vars_in_scope = scope.iter().filter(|s| !s.starts_with('\0'));
                let mut err = Diagnostic::at(span.clone(), format!("undefined variable ${name}"));
                err = match closest_match(name, vars_in_scope.map(String::as_str)) {
                    Some(sugg) => err.help(format!("did you mean ${sugg}?")),
                    None if scope.iter().all(|s| s.starts_with('\0')) => {
                        err.help("no variables are in scope here")
                    }
                    None => err,
                };
                return Err(err);
            }
            let jv = jv_ident(name);
            Ok(quote!(Ok(::std::vec![#jv.clone()])))
        }
        Expr::Reduce {
            source,
            var,
            init,
            update,
        } => {
            let jv = jv_ident(var);
            let source_c = compile(source, input, scope)?;
            let init_c = compile(init, input, scope)?;
            scope.push(var.clone());
            let update_c = compile(update, &quote!(&__state), scope);
            scope.pop();
            let update_c = update_c?;
            let value_ty = core_path("Value");
            let fetch_updates = fetch_or_bubble(quote!(__updates), quote!(#update_c));
            Ok(quote! {
                {
                    let mut __out = ::std::vec::Vec::new();
                    for __init in (#init_c)? {
                        let mut __state = __init;
                        for #jv in (#source_c)? {
                            #fetch_updates
                            __state = __updates.into_iter().last().unwrap_or(#value_ty::Null);
                        }
                        __out.push(__state);
                    }
                    Ok(__out)
                }
            })
        }
        Expr::Foreach {
            source,
            var,
            init,
            update,
            extract,
        } => {
            let jv = jv_ident(var);
            let source_c = compile(source, input, scope)?;
            let init_c = compile(init, input, scope)?;
            scope.push(var.clone());
            let update_c = compile(update, &quote!(&__state), scope);
            let extract_c = extract
                .as_ref()
                .map(|e| compile(e, &quote!(&__state), scope))
                .transpose();
            scope.pop();
            let update_c = update_c?;
            let bubble = core_path("bubble");
            let extract_push = match extract_c? {
                Some(ec) => quote!(#bubble(&mut __out, (#ec))?;),
                None => quote!(__out.push(__state.clone());),
            };
            let fetch_updates = fetch_or_bubble(quote!(__updates), quote!(#update_c));
            Ok(quote! {
                {
                    let mut __out = ::std::vec::Vec::new();
                    for __init in (#init_c)? {
                        let mut __state = __init;
                        for #jv in (#source_c)? {
                            #fetch_updates
                            for __next_state in __updates {
                                __state = __next_state;
                                #extract_push
                            }
                        }
                    }
                    Ok(__out)
                }
            })
        }
        Expr::RecCall(name) => {
            let fname = fn_ident(name);
            Ok(quote!(#fname(#input)))
        }
        Expr::RecFn0 { name, body, rest } => {
            let fname = fn_ident(name);
            let value_ty = core_path("Value");
            let jresult_ty = core_path("JResult");
            let body_c = compile(body, &quote!(__jqc_in), scope)?;
            let rest_c = compile(rest, input, scope)?;
            Ok(quote! {
                {
                    fn #fname(__jqc_in: &#value_ty) -> #jresult_ty<::std::vec::Vec<#value_ty>> {
                        #body_c
                    }
                    #rest_c
                }
            })
        }
        Expr::FuncDef { .. } => Err(Diagnostic::new(
            "internal error: FuncDef should have been resolved by lowering",
        )),
        Expr::Call(name, args, span) => compile_call(name, args, span, input, scope),
    }
}

fn compile_str(
    parts: &[StrPart],
    input: &TokenStream,
    scope: &mut Vec<String>,
) -> CResult<TokenStream> {
    let value_ty = core_path("Value");
    let display = core_path("value_to_display_string");
    let mut steps = Vec::new();
    for part in parts {
        match part {
            StrPart::Lit(s) => steps.push(quote! {
                __out = __out.into_iter().map(|prefix: ::std::string::String| prefix + #s).collect();
            }),
            StrPart::Interp(e) => {
                let e_c = compile(e, input, scope)?;
                steps.push(quote! {
                    __out = {
                        let mut __next = ::std::vec::Vec::new();
                        for __prefix in &__out {
                            for __val in (#e_c)? {
                                __next.push(__prefix.clone() + &#display(&__val));
                            }
                        }
                        __next
                    };
                });
            }
        }
    }
    Ok(quote! {
        {
            let mut __out: ::std::vec::Vec<::std::string::String> = ::std::vec![::std::string::String::new()];
            #(#steps)*
            Ok(__out.into_iter().map(#value_ty::String).collect::<::std::vec::Vec<#value_ty>>())
        }
    })
}

fn compile_object(
    entries: &[ObjEntry],
    input: &TokenStream,
    scope: &mut Vec<String>,
) -> CResult<TokenStream> {
    let value_ty = core_path("Value");
    let map_ty = core_path("Map");
    let object_insert = core_path("object_insert");
    let mut steps = Vec::new();
    for entry in entries {
        let key_c = compile(&entry.key, input, scope)?;
        let val_c = compile(&entry.val, input, scope)?;
        steps.push(quote! {
            __partials = {
                let mut __next: ::std::vec::Vec<#map_ty<::std::string::String, #value_ty>> = ::std::vec::Vec::new();
                for __partial in &__partials {
                    for __key in (#key_c)? {
                        for __val in (#val_c)? {
                            __next.push(#object_insert(__partial.clone(), __key.clone(), __val)?);
                        }
                    }
                }
                __next
            };
        });
    }
    Ok(quote! {
        {
            let mut __partials: ::std::vec::Vec<#map_ty<::std::string::String, #value_ty>> = ::std::vec![#map_ty::new()];
            #(#steps)*
            Ok(__partials.into_iter().map(#value_ty::Object).collect::<::std::vec::Vec<#value_ty>>())
        }
    })
}

fn compile_binop(
    op: BinOp,
    lhs: &Expr,
    rhs: &Expr,
    input: &TokenStream,
    scope: &mut Vec<String>,
) -> CResult<TokenStream> {
    let lhs_c = compile(lhs, input, scope)?;
    let rhs_c = compile(rhs, input, scope)?;
    let value_ty = core_path("Value");
    let bubble1 = core_path("bubble1");
    // Each op_call evaluates to `JResult<Value>` (even the infallible
    // comparisons, wrapped in `Ok`) so it can be folded through `bubble1`
    // uniformly below, preserving already-computed combinations if a later
    // one fails.
    let op_call = match op {
        BinOp::Add => {
            let f = core_path("add_values");
            quote!(#f(__lhs, __rhs.clone()))
        }
        BinOp::Sub => {
            let f = core_path("sub_values");
            quote!(#f(__lhs, __rhs.clone()))
        }
        BinOp::Mul => {
            let f = core_path("mul_values");
            quote!(#f(__lhs, __rhs.clone()))
        }
        BinOp::Div => {
            let f = core_path("div_values");
            quote!(#f(__lhs, __rhs.clone()))
        }
        BinOp::Mod => {
            let f = core_path("mod_values");
            quote!(#f(__lhs, __rhs.clone()))
        }
        BinOp::Eq => {
            let f = core_path("try_eq_values");
            quote!(Ok(#value_ty::Bool(#f(&__lhs, &__rhs)?)))
        }
        BinOp::Ne => {
            let f = core_path("try_eq_values");
            quote!(Ok(#value_ty::Bool(!#f(&__lhs, &__rhs)?)))
        }
        BinOp::Lt => {
            let f = core_path("try_cmp_values");
            quote!(Ok(#value_ty::Bool(#f(&__lhs, &__rhs)? == ::std::cmp::Ordering::Less)))
        }
        BinOp::Le => {
            let f = core_path("try_cmp_values");
            quote!(Ok(#value_ty::Bool(#f(&__lhs, &__rhs)? != ::std::cmp::Ordering::Greater)))
        }
        BinOp::Gt => {
            let f = core_path("try_cmp_values");
            quote!(Ok(#value_ty::Bool(#f(&__lhs, &__rhs)? == ::std::cmp::Ordering::Greater)))
        }
        BinOp::Ge => {
            let f = core_path("try_cmp_values");
            quote!(Ok(#value_ty::Bool(#f(&__lhs, &__rhs)? != ::std::cmp::Ordering::Less)))
        }
    };
    // jq evaluates the right operand's generator as the outer loop (e.g.
    // `(1,2) + (10,20)` yields `11,12,21,22`, not `11,21,12,22`); nesting
    // `partial_aware_for` for both keeps already-computed combinations if
    // a later one (or either generator, partway through) fails.
    let inner_body = quote! {
        #bubble1(__out, (#op_call))?;
    };
    let inner = partial_aware_for(&lhs_c, &quote!(__lhs), inner_body);
    let bubble = core_path("bubble");
    let outer_body = quote! {
        #bubble(__out, (#inner))?;
    };
    Ok(partial_aware_for(&rhs_c, &quote!(__rhs), outer_body))
}

/// `and`/`or` short-circuit: the right operand is only evaluated (and only
/// then cartesian-expanded) per left-hand value that didn't already decide
/// the result on its own.
fn compile_and_or(
    lhs: &Expr,
    rhs: &Expr,
    input: &TokenStream,
    scope: &mut Vec<String>,
    is_and: bool,
) -> CResult<TokenStream> {
    let truthy = core_path("truthy");
    let value_ty = core_path("Value");
    let lhs_c = compile(lhs, input, scope)?;
    let rhs_c = compile(rhs, input, scope)?;
    let bubble1 = core_path("bubble1");
    let (short_circuit_on, short_circuit_value) = if is_and {
        (quote!(!), false)
    } else {
        (quote!(), true)
    };
    let rhs_body = quote! {
        #bubble1(__out, Ok(#value_ty::Bool(#truthy(&__rhs))))?;
    };
    let rhs_block = partial_aware_for(&rhs_c, &quote!(__rhs), rhs_body);
    let bubble = core_path("bubble");
    let lhs_body = quote! {
        if #short_circuit_on #truthy(&__lhs) {
            __out.push(#value_ty::Bool(#short_circuit_value));
        } else {
            #bubble(__out, (#rhs_block))?;
        }
    };
    Ok(partial_aware_for(&lhs_c, &quote!(__lhs), lhs_body))
}

fn compile_if(
    branches: &[(Expr, Expr)],
    else_branch: Option<&Expr>,
    input: &TokenStream,
    scope: &mut Vec<String>,
) -> CResult<TokenStream> {
    if branches.is_empty() {
        return match else_branch {
            Some(e) => compile(e, input, scope),
            None => compile(&Expr::Identity, input, scope),
        };
    }
    let (cond, then_e) = &branches[0];
    let cond_c = compile(cond, input, scope)?;
    let then_c = compile(then_e, input, scope)?;
    let else_c = wrap_fallible(compile_if(&branches[1..], else_branch, input, scope)?);
    let truthy = core_path("truthy");
    let bubble = core_path("bubble");
    let body = quote! {
        if #truthy(&__cond) {
            #bubble(__out, (#then_c))?;
        } else {
            #bubble(__out, (#else_c))?;
        }
    };
    Ok(partial_aware_for(&cond_c, &quote!(__cond), body))
}

fn compile_slice(
    base: &Expr,
    from: Option<&Expr>,
    to: Option<&Expr>,
    optional: bool,
    input: &TokenStream,
    scope: &mut Vec<String>,
) -> CResult<TokenStream> {
    let base_c = compile(base, input, scope)?;
    let slice_value = core_path("slice_value");
    let to_i64 = core_path("value_to_i64");
    let bubble = core_path("bubble");

    let from_c = from.map(|e| compile(e, input, scope)).transpose()?;
    let to_c = to.map(|e| compile(e, input, scope)).transpose()?;

    // jq nests these outer-to-inner as from, to, base (least significant),
    // mirroring how it treats the bracket expression as outer and the
    // receiver as inner for plain indexing. Each level is built as its own
    // `JResult<Vec<Value>>` block (via `partial_aware_for` when that level
    // is itself a generator) and folded into the level above via `bubble`,
    // so a failure anywhere -- the base lookup, a `from`/`to` bound failing
    // to convert, or either bound's generator failing partway through --
    // still keeps whatever slices were already produced.
    let base_body = quote! {
        match #slice_value(&__base, __from, __to) {
            Ok(v) => __out.push(v),
            Err(e) => if !#optional { return Err(e); },
        }
    };
    let base_block = partial_aware_for(&base_c, &quote!(__base), base_body);
    let with_to = match to_c {
        Some(c) => {
            let to_body = quote! {
                match #to_i64(&__to_src) {
                    Ok(__to_val) => {
                        let __to: ::std::option::Option<i64> = ::std::option::Option::Some(__to_val);
                        #bubble(__out, (#base_block))?;
                    }
                    Err(e) => return Err(e),
                }
            };
            partial_aware_for(&c, &quote!(__to_src), to_body)
        }
        None => quote! {
            {
                let __to: ::std::option::Option<i64> = ::std::option::Option::None;
                #base_block
            }
        },
    };
    let with_from = match from_c {
        Some(c) => {
            let from_body = quote! {
                match #to_i64(&__from_src) {
                    Ok(__from_val) => {
                        let __from: ::std::option::Option<i64> = ::std::option::Option::Some(__from_val);
                        #bubble(__out, (#with_to))?;
                    }
                    Err(e) => return Err(e),
                }
            };
            partial_aware_for(&c, &quote!(__from_src), from_body)
        }
        None => quote! {
            {
                let __from: ::std::option::Option<i64> = ::std::option::Option::None;
                #with_to
            }
        },
    };

    Ok(with_from)
}

/// Generic N-ary cartesian product over a builtin call's arguments: each
/// arg's compiled output is bound to `__a0`, `__a1`, … (outermost to
/// innermost, left to right), and `build` receives those idents to produce
/// the single pushed expression.
fn cartesian_args(
    args: &[Expr],
    input: &TokenStream,
    scope: &mut Vec<String>,
    mut build: impl FnMut(&[TokenStream]) -> TokenStream,
) -> CResult<TokenStream> {
    let compiled: Vec<TokenStream> = args
        .iter()
        .map(|a| compile(a, input, scope))
        .collect::<CResult<Vec<_>>>()?;
    let names: Vec<Ident> = (0..args.len()).map(|i| format_ident!("__a{i}")).collect();
    let name_toks: Vec<TokenStream> = names.iter().map(|n| quote!(#n)).collect();
    // `build` must produce a `JResult<Value>`-typed expression (fallible
    // helpers used directly, infallible ones `Ok`-wrapped by the caller).
    let pushed = build(&name_toks);
    let bubble1 = core_path("bubble1");
    let bubble = core_path("bubble");
    // Innermost: push one combination's result, merging it with whatever
    // earlier combinations already pushed if it fails.
    let mut block: TokenStream = quote! {
        {
            let mut __out = ::std::vec::Vec::new();
            #bubble1(&mut __out, (#pushed))?;
            Ok(__out)
        }
    };
    for (name, c) in names.iter().zip(compiled.iter()).rev() {
        let body = quote! {
            #bubble(__out, (#block))?;
        };
        block = partial_aware_for(c, &quote!(#name), body);
    }
    Ok(block)
}

fn compile_call(
    name: &str,
    args: &[Expr],
    span: &crate::diagnostics::Span,
    input: &TokenStream,
    scope: &mut Vec<String>,
) -> CResult<TokenStream> {
    let value_ty = core_path("Value");
    match (name, args.len()) {
        ("empty", 0) => Ok(quote!(Ok(::std::vec::Vec::new()))),
        ("error", 0) => {
            let err = core_path("JqError");
            Ok(quote!(Err(#err::value((#input).clone()))))
        }
        ("length", 0) => {
            let f = core_path("length_of");
            Ok(quote!(Ok(::std::vec![#f(#input)?])))
        }
        ("type", 0) => {
            let f = core_path("type_name");
            Ok(quote!(Ok(
                ::std::vec![#value_ty::String(#f(#input).to_string())]
            )))
        }
        ("keys", 0) => {
            let f = core_path("keys_of");
            Ok(quote!(Ok(::std::vec![#f(#input, true)?])))
        }
        ("keys_unsorted", 0) => {
            let f = core_path("keys_of");
            Ok(quote!(Ok(::std::vec![#f(#input, false)?])))
        }
        ("add", 0) => {
            let as_array = core_path("as_array");
            let f = core_path("add_all");
            Ok(quote!(Ok(::std::vec![#f(#as_array(#input)?)?])))
        }
        ("reverse", 0) => {
            let f = core_path("reverse_value");
            Ok(quote!(Ok(::std::vec![#f(#input)?])))
        }
        ("sort", 0) => {
            let as_array = core_path("as_array");
            let f = core_path("sort_values");
            Ok(quote!(Ok(
                ::std::vec![#value_ty::Array(#f(#as_array(#input)?.clone())?)]
            )))
        }
        ("unique", 0) => {
            let as_array = core_path("as_array");
            let f = core_path("unique_values");
            Ok(quote!(Ok(
                ::std::vec![#value_ty::Array(#f(#as_array(#input)?.clone())?)]
            )))
        }
        ("min", 0) => {
            let as_array = core_path("as_array");
            let f = core_path("min_values");
            Ok(quote!(Ok(::std::vec![#f(#as_array(#input)?)?])))
        }
        ("max", 0) => {
            let as_array = core_path("as_array");
            let f = core_path("max_values");
            Ok(quote!(Ok(::std::vec![#f(#as_array(#input)?)?])))
        }
        ("floor", 0) => simple_unary("floor_value", input),
        ("ceil", 0) => simple_unary("ceil_value", input),
        ("round", 0) => simple_unary("round_value", input),
        ("sqrt", 0) => simple_unary("sqrt_value", input),
        ("fabs", 0) => simple_unary("fabs_value", input),
        ("explode", 0) => simple_unary("explode", input),
        ("implode", 0) => simple_unary("implode", input),
        ("ascii_downcase", 0) => simple_unary("ascii_downcase", input),
        ("ascii_upcase", 0) => simple_unary("ascii_upcase", input),
        ("tonumber", 0) => simple_unary("to_number_value", input),
        ("tostring", 0) => {
            let f = core_path("to_string_value");
            Ok(quote!(Ok(::std::vec![#f(#input)])))
        }
        ("first", 0) => {
            let f = core_path("index_value");
            Ok(quote!(Ok(::std::vec![#f(#input, &#value_ty::from(0))?])))
        }
        ("last", 0) => {
            let f = core_path("index_value");
            Ok(quote!(Ok(::std::vec![#f(#input, &#value_ty::from(-1))?])))
        }
        ("first", 1) => {
            let arg_c = compile(&args[0], input, scope)?;
            Ok(quote! {
                {
                    let __items = (#arg_c)?;
                    Ok(__items.into_iter().next().into_iter().collect::<::std::vec::Vec<#value_ty>>())
                }
            })
        }
        ("last", 1) => {
            let arg_c = compile(&args[0], input, scope)?;
            Ok(quote! {
                {
                    let __items = (#arg_c)?;
                    Ok(__items.into_iter().last().into_iter().collect::<::std::vec::Vec<#value_ty>>())
                }
            })
        }
        // `build` closures here all evaluate to a `JResult<Value>`-typed
        // expression -- `cartesian_args` runs it as-is (no extra `Ok`/`?`
        // wrapping) through `bubble1`, so a fallible helper is used
        // directly and an infallible one is wrapped in `Ok` right here.
        ("has", 1) => {
            let f = core_path("has_key");
            cartesian_args(args, input, scope, |a| {
                let arg = &a[0];
                quote!(#f(#input, &#arg))
            })
        }
        ("contains", 1) => {
            let f = core_path("contains_value");
            cartesian_args(args, input, scope, |a| {
                let arg = &a[0];
                quote!(#f(#input, &#arg).map(#value_ty::Bool))
            })
        }
        ("startswith", 1) => {
            let f = core_path("startswith");
            cartesian_args(args, input, scope, |a| {
                let arg = &a[0];
                quote!(#f(#input, &#arg))
            })
        }
        ("endswith", 1) => {
            let f = core_path("endswith");
            cartesian_args(args, input, scope, |a| {
                let arg = &a[0];
                quote!(#f(#input, &#arg))
            })
        }
        ("ltrimstr", 1) => {
            let f = core_path("ltrimstr");
            cartesian_args(args, input, scope, |a| {
                let arg = &a[0];
                quote!(Ok(#f(#input, &#arg)))
            })
        }
        ("rtrimstr", 1) => {
            let f = core_path("rtrimstr");
            cartesian_args(args, input, scope, |a| {
                let arg = &a[0];
                quote!(Ok(#f(#input, &#arg)))
            })
        }
        ("join", 1) => {
            let f = core_path("join_strings");
            cartesian_args(args, input, scope, |a| {
                let arg = &a[0];
                quote!(#f(#input, &#arg))
            })
        }
        ("split", 1) => {
            let f = core_path("split_string");
            cartesian_args(args, input, scope, |a| {
                let arg = &a[0];
                quote!(#f(#input, &#arg))
            })
        }
        ("range", 1) => {
            let range_values = core_path("range_values");
            let arg_c = compile(&args[0], input, scope)?;
            let to_f64 = core_path("value_to_f64");
            let bubble = core_path("bubble");
            let jresult_ty = core_path("JResult");
            let body = quote! {
                #bubble(__out, (|| -> #jresult_ty<::std::vec::Vec<#value_ty>> {
                    Ok(#range_values(0.0, #to_f64(&__to)?, 1.0))
                })())?;
            };
            Ok(partial_aware_for(&arg_c, &quote!(__to), body))
        }
        ("range", 2) => compile_range(&args[0], &args[1], None, input, scope),
        ("range", 3) => compile_range(&args[0], &args[1], Some(&args[2]), input, scope),
        ("sort_by", 1) => compile_by(args, input, scope, "sort_by_keys", true),
        ("group_by", 1) => compile_by(args, input, scope, "group_by_keys", true),
        ("unique_by", 1) => compile_by(args, input, scope, "unique_by_keys", true),
        ("min_by", 1) => compile_by(args, input, scope, "min_by_keys", false),
        ("max_by", 1) => compile_by(args, input, scope, "max_by_keys", false),
        _ => {
            let arity = args.len();
            let err = Diagnostic::at(
                span.clone(),
                format!("unsupported or unknown jq function '{name}/{arity}'"),
            );
            let err = if KNOWN_NAMES.contains(&name) {
                err.help(format!(
                    "`{name}` is known, but not with {arity} argument{} -- check the arity",
                    if arity == 1 { "" } else { "s" }
                ))
            } else {
                match closest_match(name, KNOWN_NAMES.iter().copied()) {
                    Some(sugg) => err.help(format!("did you mean `{sugg}`?")),
                    None => err.help(
                        "see the README's \"Supported jq subset\" for what's implemented",
                    ),
                }
            };
            Err(err)
        }
    }
}

fn simple_unary(fn_name: &str, input: &TokenStream) -> CResult<TokenStream> {
    let f = core_path(fn_name);
    Ok(quote!(Ok(::std::vec![#f(#input)?])))
}

fn compile_range(
    from: &Expr,
    to: &Expr,
    step: Option<&Expr>,
    input: &TokenStream,
    scope: &mut Vec<String>,
) -> CResult<TokenStream> {
    let from_c = compile(from, input, scope)?;
    let to_c = compile(to, input, scope)?;
    let to_f64 = core_path("value_to_f64");
    let range_values = core_path("range_values");
    let bubble = core_path("bubble");
    let jresult_ty = core_path("JResult");
    let value_ty = core_path("Value");
    let to_body = match step {
        Some(s) => {
            let step_c = compile(s, input, scope)?;
            let step_body = quote! {
                #bubble(__out, (|| -> #jresult_ty<::std::vec::Vec<#value_ty>> {
                    Ok(#range_values(#to_f64(&__from)?, #to_f64(&__to)?, #to_f64(&__step)?))
                })())?;
            };
            let step_block = partial_aware_for(&step_c, &quote!(__step), step_body);
            quote! { #bubble(__out, (#step_block))?; }
        }
        None => quote! {
            #bubble(__out, (|| -> #jresult_ty<::std::vec::Vec<#value_ty>> {
                Ok(#range_values(#to_f64(&__from)?, #to_f64(&__to)?, 1.0))
            })())?;
        },
    };
    let to_block = partial_aware_for(&to_c, &quote!(__to), to_body);
    let from_body = quote! {
        #bubble(__out, (#to_block))?;
    };
    Ok(partial_aware_for(&from_c, &quote!(__from), from_body))
}

/// Shared shape for `sort_by`/`group_by`/`unique_by`/`min_by`/`max_by`:
/// evaluate `f` once per element of the (array) ambient input, building a
/// parallel `[f]`-style key vector, then call the matching core helper.
/// `wrap_array` controls whether the helper's result is already a `Value`
/// (false, for `min_by`/`max_by`) or a `Vec<Value>` to wrap in an array.
fn compile_by(
    args: &[Expr],
    input: &TokenStream,
    scope: &mut Vec<String>,
    helper_name: &str,
    wrap_array: bool,
) -> CResult<TokenStream> {
    let value_ty = core_path("Value");
    let as_array = core_path("as_array");
    let helper = core_path(helper_name);
    let key_c = compile(&args[0], &quote!(__item), scope)?;
    let result = if wrap_array {
        quote!(Ok(::std::vec![#value_ty::Array(#helper(__items, __keys)?)]))
    } else {
        quote!(Ok(::std::vec![#helper(__items, __keys)?]))
    };
    Ok(quote! {
        {
            let __items = #as_array(#input)?.clone();
            let mut __keys = ::std::vec::Vec::with_capacity(__items.len());
            for __item in &__items {
                __keys.push(#value_ty::Array((#key_c)?));
            }
            #result
        }
    })
}
