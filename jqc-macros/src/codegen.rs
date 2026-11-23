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

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::Ident;

use crate::ast::{BinOp, Expr, StrPart};

pub type CResult<T> = Result<T, String>;

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
        Expr::RecurseDefault => Err("internal error: RecurseDefault should have been lowered".to_string()),
        Expr::Null => {
            let v = core_path("Value");
            Ok(quote!(Ok(::std::vec![#v::Null])))
        }
        Expr::Bool(b) => {
            let v = core_path("Value");
            Ok(quote!(Ok(::std::vec![#v::Bool(#b)])))
        }
        Expr::Number(n) => {
            let v = core_path("Value");
            let num = core_path("number_from_f64");
            Ok(quote!(Ok(::std::vec![#v::Number(#num(#n))])))
        }
        Expr::Str(parts) => compile_str(parts, input, scope),
        Expr::Field(base, name, opt) => {
            let index_field = core_path("index_field");
            let base_c = compile(base, input, scope)?;
            Ok(quote! {
                {
                    let mut __o = ::std::vec::Vec::new();
                    for __b in (#base_c)? {
                        match #index_field(&__b, #name) {
                            Ok(v) => __o.push(v),
                            Err(e) => if !#opt { return Err(e); },
                        }
                    }
                    Ok(__o)
                }
            })
        }
        Expr::Index(base, idx, opt) => {
            let index_value = core_path("index_value");
            let base_c = compile(base, input, scope)?;
            let idx_c = compile(idx, input, scope)?;
            Ok(quote! {
                {
                    let mut __o = ::std::vec::Vec::new();
                    for __b in (#base_c)? {
                        for __k in (#idx_c)? {
                            match #index_value(&__b, &__k) {
                                Ok(v) => __o.push(v),
                                Err(e) => if !#opt { return Err(e); },
                            }
                        }
                    }
                    Ok(__o)
                }
            })
        }
        Expr::Slice(base, from, to, opt) => compile_slice(base, from.as_deref(), to.as_deref(), *opt, input, scope),
        Expr::IterateAll(base, opt) => {
            let iterate = core_path("iterate");
            let base_c = compile(base, input, scope)?;
            Ok(quote! {
                {
                    let mut __o = ::std::vec::Vec::new();
                    for __b in (#base_c)? {
                        match #iterate(&__b) {
                            Ok(vs) => __o.extend(vs),
                            Err(e) => if !#opt { return Err(e); },
                        }
                    }
                    Ok(__o)
                }
            })
        }
        Expr::Pipe(a, b) => {
            let a_c = compile(a, input, scope)?;
            let b_c = compile(b, &quote!(&__x), scope)?;
            Ok(quote! {
                {
                    let mut __o = ::std::vec::Vec::new();
                    for __x in (#a_c)? {
                        __o.extend((#b_c)?);
                    }
                    Ok(__o)
                }
            })
        }
        Expr::Comma(a, b) => {
            let a_c = compile(a, input, scope)?;
            let b_c = compile(b, input, scope)?;
            Ok(quote! {
                {
                    let mut __o = (#a_c)?;
                    __o.extend((#b_c)?);
                    Ok(__o)
                }
            })
        }
        Expr::ArrayConstruct(inner) => {
            let v = core_path("Value");
            match inner {
                None => Ok(quote!(Ok(::std::vec![#v::Array(::std::vec::Vec::new())]))),
                Some(e) => {
                    let c = compile(e, input, scope)?;
                    Ok(quote!(Ok(::std::vec![#v::Array((#c)?)])))
                }
            }
        }
        Expr::ObjectConstruct(entries) => compile_object(entries, input, scope),
        Expr::Neg(x) => {
            let neg = core_path("neg_value");
            let c = compile(x, input, scope)?;
            Ok(quote! {
                {
                    let mut __o = ::std::vec::Vec::new();
                    for __v in (#c)? {
                        __o.push(#neg(__v)?);
                    }
                    Ok(__o)
                }
            })
        }
        Expr::BinOp(op, a, b) => compile_binop(*op, a, b, input, scope),
        Expr::And(a, b) => {
            let truthy = core_path("truthy");
            let v = core_path("Value");
            let a_c = compile(a, input, scope)?;
            let b_c = compile(b, input, scope)?;
            Ok(quote! {
                {
                    let mut __o = ::std::vec::Vec::new();
                    for __l in (#a_c)? {
                        if !#truthy(&__l) {
                            __o.push(#v::Bool(false));
                        } else {
                            for __r in (#b_c)? {
                                __o.push(#v::Bool(#truthy(&__r)));
                            }
                        }
                    }
                    Ok(__o)
                }
            })
        }
        Expr::Or(a, b) => {
            let truthy = core_path("truthy");
            let v = core_path("Value");
            let a_c = compile(a, input, scope)?;
            let b_c = compile(b, input, scope)?;
            Ok(quote! {
                {
                    let mut __o = ::std::vec::Vec::new();
                    for __l in (#a_c)? {
                        if #truthy(&__l) {
                            __o.push(#v::Bool(true));
                        } else {
                            for __r in (#b_c)? {
                                __o.push(#v::Bool(#truthy(&__r)));
                            }
                        }
                    }
                    Ok(__o)
                }
            })
        }
        Expr::If(branches, else_e) => compile_if(branches, else_e.as_deref(), input, scope),
        Expr::TryCatch(body, catch) => {
            let value_ty = core_path("Value");
            let jresult_ty = core_path("JResult");
            let body_c = compile(body, input, scope)?;
            let handler = match catch {
                Some(c) => {
                    let catch_c = compile(c, &quote!(&__ev), scope)?;
                    quote! {
                        Err(e) => {
                            let __ev = e.into_value();
                            (#catch_c)
                        }
                    }
                }
                None => quote! { Err(_e) => Ok(::std::vec::Vec::new()) },
            };
            Ok(quote! {
                {
                    let __r: #jresult_ty<::std::vec::Vec<#value_ty>> = (|| #body_c)();
                    match __r {
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
                    let __r: #jresult_ty<::std::vec::Vec<#value_ty>> = (|| #body_c)();
                    match __r {
                        Ok(v) => Ok(v),
                        Err(_) => Ok(::std::vec::Vec::new()),
                    }
                }
            })
        }
        Expr::VarBind(src, name, body) => {
            let jv = jv_ident(name);
            let src_c = compile(src, input, scope)?;
            scope.push(name.clone());
            let body_c = compile(body, input, scope);
            scope.pop();
            let body_c = body_c?;
            Ok(quote! {
                {
                    let mut __o = ::std::vec::Vec::new();
                    for #jv in (#src_c)? {
                        __o.extend((#body_c)?);
                    }
                    Ok(__o)
                }
            })
        }
        Expr::VarRef(name) => {
            if !scope.iter().any(|s| s == name) {
                return Err(format!("undefined variable ${name}"));
            }
            let jv = jv_ident(name);
            Ok(quote!(Ok(::std::vec![#jv.clone()])))
        }
        Expr::Reduce(src, var, init, update) => {
            let jv = jv_ident(var);
            let src_c = compile(src, input, scope)?;
            let init_c = compile(init, input, scope)?;
            scope.push(var.clone());
            let update_c = compile(update, &quote!(&__state), scope);
            scope.pop();
            let update_c = update_c?;
            let value_ty = core_path("Value");
            Ok(quote! {
                {
                    let mut __o = ::std::vec::Vec::new();
                    for __init in (#init_c)? {
                        let mut __state = __init;
                        for #jv in (#src_c)? {
                            let __upd = (#update_c)?;
                            __state = __upd.into_iter().last().unwrap_or(#value_ty::Null);
                        }
                        __o.push(__state);
                    }
                    Ok(__o)
                }
            })
        }
        Expr::Foreach(src, var, init, update, extract) => {
            let jv = jv_ident(var);
            let src_c = compile(src, input, scope)?;
            let init_c = compile(init, input, scope)?;
            scope.push(var.clone());
            let update_c = compile(update, &quote!(&__state), scope);
            let extract_c = extract
                .as_ref()
                .map(|e| compile(e, &quote!(&__state), scope))
                .transpose();
            scope.pop();
            let update_c = update_c?;
            let extract_push = match extract_c? {
                Some(ec) => quote!(__o.extend((#ec)?);),
                None => quote!(__o.push(__state.clone());),
            };
            Ok(quote! {
                {
                    let mut __o = ::std::vec::Vec::new();
                    for __init in (#init_c)? {
                        let mut __state = __init;
                        for #jv in (#src_c)? {
                            let __upd = (#update_c)?;
                            for __u in __upd {
                                __state = __u;
                                #extract_push
                            }
                        }
                    }
                    Ok(__o)
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
        Expr::FuncDef { .. } => Err("internal error: FuncDef should have been resolved by lowering".to_string()),
        Expr::Call(name, args) => compile_call(name, args, input, scope),
    }
}

fn compile_str(parts: &[StrPart], input: &TokenStream, scope: &mut Vec<String>) -> CResult<TokenStream> {
    let value_ty = core_path("Value");
    let display = core_path("value_to_display_string");
    let mut steps = Vec::new();
    for part in parts {
        match part {
            StrPart::Lit(s) => steps.push(quote! {
                __o = __o.into_iter().map(|p: ::std::string::String| p + #s).collect();
            }),
            StrPart::Interp(e) => {
                let c = compile(e, input, scope)?;
                steps.push(quote! {
                    __o = {
                        let mut __next = ::std::vec::Vec::new();
                        for __p in &__o {
                            for __v in (#c)? {
                                __next.push(__p.clone() + &#display(&__v));
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
            let mut __o: ::std::vec::Vec<::std::string::String> = ::std::vec![::std::string::String::new()];
            #(#steps)*
            Ok(__o.into_iter().map(#value_ty::String).collect::<::std::vec::Vec<#value_ty>>())
        }
    })
}

fn compile_object(
    entries: &[crate::ast::ObjEntry],
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
            __maps = {
                let mut __next: ::std::vec::Vec<#map_ty<::std::string::String, #value_ty>> = ::std::vec::Vec::new();
                for __m in &__maps {
                    for __k in (#key_c)? {
                        for __v in (#val_c)? {
                            __next.push(#object_insert(__m.clone(), __k.clone(), __v)?);
                        }
                    }
                }
                __next
            };
        });
    }
    Ok(quote! {
        {
            let mut __maps: ::std::vec::Vec<#map_ty<::std::string::String, #value_ty>> = ::std::vec![#map_ty::new()];
            #(#steps)*
            Ok(__maps.into_iter().map(#value_ty::Object).collect::<::std::vec::Vec<#value_ty>>())
        }
    })
}

fn compile_binop(op: BinOp, a: &Expr, b: &Expr, input: &TokenStream, scope: &mut Vec<String>) -> CResult<TokenStream> {
    let a_c = compile(a, input, scope)?;
    let b_c = compile(b, input, scope)?;
    let value_ty = core_path("Value");
    let op_call = match op {
        BinOp::Add => {
            let f = core_path("add_values");
            quote!(#f(__a, __b.clone())?)
        }
        BinOp::Sub => {
            let f = core_path("sub_values");
            quote!(#f(__a, __b.clone())?)
        }
        BinOp::Mul => {
            let f = core_path("mul_values");
            quote!(#f(__a, __b.clone())?)
        }
        BinOp::Div => {
            let f = core_path("div_values");
            quote!(#f(__a, __b.clone())?)
        }
        BinOp::Mod => {
            let f = core_path("mod_values");
            quote!(#f(__a, __b.clone())?)
        }
        BinOp::Eq => {
            let f = core_path("eq_values");
            quote!(#value_ty::Bool(#f(&__a, &__b)))
        }
        BinOp::Ne => {
            let f = core_path("eq_values");
            quote!(#value_ty::Bool(!#f(&__a, &__b)))
        }
        BinOp::Lt => {
            let f = core_path("cmp_values");
            quote!(#value_ty::Bool(#f(&__a, &__b) == ::std::cmp::Ordering::Less))
        }
        BinOp::Le => {
            let f = core_path("cmp_values");
            quote!(#value_ty::Bool(#f(&__a, &__b) != ::std::cmp::Ordering::Greater))
        }
        BinOp::Gt => {
            let f = core_path("cmp_values");
            quote!(#value_ty::Bool(#f(&__a, &__b) == ::std::cmp::Ordering::Greater))
        }
        BinOp::Ge => {
            let f = core_path("cmp_values");
            quote!(#value_ty::Bool(#f(&__a, &__b) != ::std::cmp::Ordering::Less))
        }
    };
    Ok(quote! {
        {
            let mut __o = ::std::vec::Vec::new();
            // jq evaluates the right operand's generator as the outer loop
            // (e.g. `(1,2) + (10,20)` yields `11,12,21,22`, not `11,21,12,22`).
            for __b in (#b_c)? {
                for __a in (#a_c)? {
                    __o.push(#op_call);
                }
            }
            Ok(__o)
        }
    })
}

fn compile_if(
    branches: &[(Expr, Expr)],
    else_e: Option<&Expr>,
    input: &TokenStream,
    scope: &mut Vec<String>,
) -> CResult<TokenStream> {
    if branches.is_empty() {
        return match else_e {
            Some(e) => compile(e, input, scope),
            None => compile(&Expr::Identity, input, scope),
        };
    }
    let (cond, then_e) = &branches[0];
    let cond_c = compile(cond, input, scope)?;
    let then_c = compile(then_e, input, scope)?;
    let else_c = wrap_fallible(compile_if(&branches[1..], else_e, input, scope)?);
    let truthy = core_path("truthy");
    Ok(quote! {
        {
            let mut __o = ::std::vec::Vec::new();
            for __c in (#cond_c)? {
                if #truthy(&__c) {
                    __o.extend((#then_c)?);
                } else {
                    __o.extend((#else_c)?);
                }
            }
            Ok(__o)
        }
    })
}

fn compile_slice(
    base: &Expr,
    from: Option<&Expr>,
    to: Option<&Expr>,
    opt: bool,
    input: &TokenStream,
    scope: &mut Vec<String>,
) -> CResult<TokenStream> {
    let base_c = compile(base, input, scope)?;
    let slice_value = core_path("slice_value");
    let to_i64 = core_path("value_to_i64");

    let from_c = from.map(|e| compile(e, input, scope)).transpose()?;
    let to_c = to.map(|e| compile(e, input, scope)).transpose()?;

    let core_match = quote! {
        match #slice_value(&__b, __from, __to) {
            Ok(v) => __o.push(v),
            Err(e) => if !#opt { return Err(e); },
        }
    };
    let with_to = match to_c {
        Some(c) => quote! {
            for __tv in (#c)? {
                let __to: ::std::option::Option<i64> = ::std::option::Option::Some(#to_i64(&__tv)?);
                #core_match
            }
        },
        None => quote! {
            let __to: ::std::option::Option<i64> = ::std::option::Option::None;
            #core_match
        },
    };
    let with_from = match from_c {
        Some(c) => quote! {
            for __fv in (#c)? {
                let __from: ::std::option::Option<i64> = ::std::option::Option::Some(#to_i64(&__fv)?);
                #with_to
            }
        },
        None => quote! {
            let __from: ::std::option::Option<i64> = ::std::option::Option::None;
            #with_to
        },
    };

    Ok(quote! {
        {
            let mut __o = ::std::vec::Vec::new();
            for __b in (#base_c)? {
                #with_from
            }
            Ok(__o)
        }
    })
}

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
    let inner = build(&name_toks);
    let mut body = quote!(__o.push(#inner););
    for (name, c) in names.iter().zip(compiled.iter()).rev() {
        body = quote! {
            for #name in (#c)? {
                #body
            }
        };
    }
    Ok(quote! {
        {
            let mut __o = ::std::vec::Vec::new();
            #body
            Ok(__o)
        }
    })
}

fn compile_call(name: &str, args: &[Expr], input: &TokenStream, scope: &mut Vec<String>) -> CResult<TokenStream> {
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
            Ok(quote!(Ok(::std::vec![#value_ty::String(#f(#input).to_string())])))
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
            let arr = core_path("as_array");
            let f = core_path("add_all");
            Ok(quote!(Ok(::std::vec![#f(#arr(#input)?)?])))
        }
        ("reverse", 0) => {
            let f = core_path("reverse_value");
            Ok(quote!(Ok(::std::vec![#f(#input)?])))
        }
        ("sort", 0) => {
            let arr = core_path("as_array");
            let f = core_path("sort_values");
            Ok(quote!(Ok(::std::vec![#value_ty::Array(#f(#arr(#input)?.clone()))])))
        }
        ("unique", 0) => {
            let arr = core_path("as_array");
            let f = core_path("unique_values");
            Ok(quote!(Ok(::std::vec![#value_ty::Array(#f(#arr(#input)?.clone()))])))
        }
        ("min", 0) => {
            let arr = core_path("as_array");
            let cmp = core_path("cmp_values");
            Ok(quote! {
                Ok(::std::vec![#arr(#input)?.iter().cloned().min_by(#cmp).unwrap_or(#value_ty::Null)])
            })
        }
        ("max", 0) => {
            let arr = core_path("as_array");
            let cmp = core_path("cmp_values");
            Ok(quote! {
                Ok(::std::vec![#arr(#input)?.iter().cloned().max_by(#cmp).unwrap_or(#value_ty::Null)])
            })
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
            let c = compile(&args[0], input, scope)?;
            Ok(quote! {
                {
                    let __all = (#c)?;
                    Ok(__all.into_iter().next().into_iter().collect::<::std::vec::Vec<#value_ty>>())
                }
            })
        }
        ("last", 1) => {
            let c = compile(&args[0], input, scope)?;
            Ok(quote! {
                {
                    let __all = (#c)?;
                    Ok(__all.into_iter().last().into_iter().collect::<::std::vec::Vec<#value_ty>>())
                }
            })
        }
        ("has", 1) => {
            let f = core_path("has_key");
            cartesian_args(args, input, scope, |a| { let a0 = &a[0]; quote!(#f(#input, &#a0)?) })
        }
        ("contains", 1) => {
            let f = core_path("contains_value");
            cartesian_args(args, input, scope, |a| { let a0 = &a[0]; quote!(#value_ty::Bool(#f(#input, &#a0)?)) })
        }
        ("startswith", 1) => {
            let f = core_path("startswith");
            cartesian_args(args, input, scope, |a| { let a0 = &a[0]; quote!(#f(#input, &#a0)?) })
        }
        ("endswith", 1) => {
            let f = core_path("endswith");
            cartesian_args(args, input, scope, |a| { let a0 = &a[0]; quote!(#f(#input, &#a0)?) })
        }
        ("ltrimstr", 1) => {
            let f = core_path("ltrimstr");
            cartesian_args(args, input, scope, |a| { let a0 = &a[0]; quote!(#f(#input, &#a0)) })
        }
        ("rtrimstr", 1) => {
            let f = core_path("rtrimstr");
            cartesian_args(args, input, scope, |a| { let a0 = &a[0]; quote!(#f(#input, &#a0)) })
        }
        ("join", 1) => {
            let f = core_path("join_strings");
            cartesian_args(args, input, scope, |a| { let a0 = &a[0]; quote!(#f(#input, &#a0)?) })
        }
        ("split", 1) => {
            let f = core_path("split_string");
            cartesian_args(args, input, scope, |a| { let a0 = &a[0]; quote!(#f(#input, &#a0)?) })
        }
        ("range", 1) => {
            let f = core_path("range_values");
            let c = compile(&args[0], input, scope)?;
            let to_f64 = core_path("value_to_f64");
            Ok(quote! {
                {
                    let mut __o = ::std::vec::Vec::new();
                    for __t in (#c)? {
                        __o.extend(#f(0.0, #to_f64(&__t)?, 1.0));
                    }
                    Ok(__o)
                }
            })
        }
        ("range", 2) => compile_range(&args[0], &args[1], None, input, scope),
        ("range", 3) => compile_range(&args[0], &args[1], Some(&args[2]), input, scope),
        ("sort_by", 1) => compile_by(args, input, scope, "sort_by_keys", true),
        ("group_by", 1) => compile_by(args, input, scope, "group_by_keys", true),
        ("unique_by", 1) => compile_by(args, input, scope, "unique_by_keys", true),
        ("min_by", 1) => compile_by(args, input, scope, "min_by_keys", false),
        ("max_by", 1) => compile_by(args, input, scope, "max_by_keys", false),
        _ => Err(format!(
            "jqc: unsupported or unknown jq function '{name}/{}'",
            args.len()
        )),
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
    let step_part = match step {
        Some(s) => {
            let step_c = compile(s, input, scope)?;
            quote! {
                for __s in (#step_c)? {
                    __o.extend(#range_values(#to_f64(&__f)?, #to_f64(&__t)?, #to_f64(&__s)?));
                }
            }
        }
        None => quote! {
            __o.extend(#range_values(#to_f64(&__f)?, #to_f64(&__t)?, 1.0));
        },
    };
    Ok(quote! {
        {
            let mut __o = ::std::vec::Vec::new();
            for __f in (#from_c)? {
                for __t in (#to_c)? {
                    #step_part
                }
            }
            Ok(__o)
        }
    })
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
    let key_c = compile(&args[0], &quote!(__el), scope)?;
    let result = if wrap_array {
        quote!(Ok(::std::vec![#value_ty::Array(#helper(__arr, __keys))]))
    } else {
        quote!(Ok(::std::vec![#helper(__arr, __keys)]))
    };
    Ok(quote! {
        {
            let __arr = #as_array(#input)?.clone();
            let mut __keys = ::std::vec::Vec::with_capacity(__arr.len());
            for __el in &__arr {
                __keys.push(#value_ty::Array((#key_c)?));
            }
            #result
        }
    })
}
