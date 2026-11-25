//! Lowered-AST -> Rust `TokenStream` codegen.
//!
//! Every [`compile`] call takes `input`, a token stream that must be a valid
//! Rust expression of type `&jqc::core::Value` (the current `.`), and
//! returns a token stream that is a valid Rust expression of type
//! `jqc::core::JGen` -- a lazy, pull-based generator (see
//! `jqc-core/src/gen.rs`'s module doc for why: it's what lets a genuinely
//! infinite generator (`repeat`, `while(true;.)`) be bounded from outside
//! (`limit`, `label`/`break`, `first`) without needing to finish first).
//! Composite nodes (pipe, comma, binops, …) lazily flatten their operands'
//! generators together, matching jq's own generator semantics; each
//! sub-expression is compiled into its own nested closure, so plain
//! repeated local names are safe across sibling/nested calls (ordinary Rust
//! closure scoping keeps them from colliding) and no fresh-name counter is
//! needed.

use proc_macro2::{Ident, TokenStream};
use quote::{format_ident, quote};

use crate::ast::{BinOp, Expr, ObjEntry, Pattern, StrPart};
use crate::diagnostics::{closest_match, Diagnostic, KNOWN_NAMES};

pub type CResult<T> = Result<T, Diagnostic>;

/// A backend that turns a lowered jq AST into the Rust `TokenStream` that
/// `jq!` expands to. [`TreeWalkCodegen`] is the default; [`SimdCodegen`]
/// (selected via `jq!(codegen = "simd", ...)`, gated behind the
/// `codegen-simd` feature -- see [`CodegenBackend`]) produces identical
/// output for every construct *except* `ascii_downcase`/`ascii_upcase`/
/// `contains`, which it routes to explicit-SIMD/`memchr`-backed
/// `jqc-core` implementations instead of the plain scalar ones -- see
/// `jqc-core/src/simd.rs`'s module doc and the `simd-codegen` todo-report
/// for why those specific builtins, and why this doesn't attempt a
/// wholesale SIMD reinterpretation of jq's (inherently branchy,
/// heterogeneously-typed) semantics.
pub trait Codegen {
    fn compile_top(&self, expr: &Expr) -> CResult<TokenStream>;
}

/// Which [`Codegen`] backend a `jq!` call names. Mirrors
/// `crate::version::JqVersion`'s split between "is this a real backend
/// name" ([`parse`](Self::parse)) and "is it compiled into *this*
/// build" ([`is_available`](Self::is_available)) -- [`TreeWalk`](Self::TreeWalk)
/// is the pipeline [`Simd`](Self::Simd) itself is built on top of (see
/// this module's doc comment), so it's unconditionally available;
/// `Simd` only under the `codegen-simd` feature, which also controls
/// whether `jqc-core`'s `simd` module (and its `memchr` dependency) gets
/// compiled in at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodegenBackend {
    TreeWalk,
    Simd,
}

impl CodegenBackend {
    pub const ALL: [CodegenBackend; 2] = [CodegenBackend::TreeWalk, CodegenBackend::Simd];

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "treewalk" => Some(Self::TreeWalk),
            "simd" => Some(Self::Simd),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::TreeWalk => "treewalk",
            Self::Simd => "simd",
        }
    }

    pub fn is_available(self) -> bool {
        match self {
            Self::TreeWalk => true,
            Self::Simd => cfg!(feature = "codegen-simd"),
        }
    }

    /// Builds the actual backend. Panics if [`is_available`](Self::is_available)
    /// is false -- callers (the `jq!` macro) are expected to check that
    /// first and turn it into a compile error naming the missing
    /// feature, the same way `JqVersion::is_available` callers do.
    pub fn to_codegen(self) -> Box<dyn Codegen> {
        match self {
            Self::TreeWalk => Box::new(TreeWalkCodegen),
            Self::Simd => {
                #[cfg(feature = "codegen-simd")]
                {
                    Box::new(SimdCodegen)
                }
                #[cfg(not(feature = "codegen-simd"))]
                {
                    unreachable!("CodegenBackend::Simd::to_codegen without an is_available() check")
                }
            }
        }
    }
}

/// Which `jqc-core` function backs a handful of builtins whose
/// implementation varies by [`Codegen`] backend -- `ascii_downcase`/
/// `ascii_upcase`/`contains` today, the only ones `compile_call`
/// actually consults this for. A thread-local (set once, at the top of
/// `compile_top`, rather than threaded as a parameter through every one
/// of `compile`/`compile_raw`/`compile_call`'s ~100 recursive call
/// sites, the overwhelming majority of which have no reason to care
/// which backend is active) -- sound because macro expansion is single-
/// threaded per invocation and this is set, read, and reset within one
/// `compile_top` call, never observed across calls or threads.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Backend {
    TreeWalk,
    #[cfg(feature = "codegen-simd")]
    Simd,
}

thread_local! {
    static BACKEND: std::cell::Cell<Backend> = const { std::cell::Cell::new(Backend::TreeWalk) };
}

/// The default backend: a direct, recursive tree-walk that compiles each
/// AST node into nested Rust loops over `Vec<Value>`, one node at a time,
/// matching jq's cartesian-product generator semantics structurally in
/// the generated control flow.
pub struct TreeWalkCodegen;

impl Codegen for TreeWalkCodegen {
    fn compile_top(&self, expr: &Expr) -> CResult<TokenStream> {
        BACKEND.with(|b| b.set(Backend::TreeWalk));
        compile_top(expr)
    }
}

/// As [`TreeWalkCodegen`], except `ascii_downcase`/`ascii_upcase`/
/// `contains` route to `jqc-core`'s SIMD/`memchr`-backed implementations
/// instead of the plain scalar ones -- see this module's own doc
/// comment. Only exists under the `codegen-simd` feature; select it via
/// [`CodegenBackend`] rather than naming the type directly if the
/// backend might not be available.
#[cfg(feature = "codegen-simd")]
pub struct SimdCodegen;

#[cfg(feature = "codegen-simd")]
impl Codegen for SimdCodegen {
    fn compile_top(&self, expr: &Expr) -> CResult<TokenStream> {
        BACKEND.with(|b| b.set(Backend::Simd));
        let result = compile_top(expr);
        BACKEND.with(|b| b.set(Backend::TreeWalk));
        result
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

/// Many composite nodes need to reference the ambient `.` from more than
/// one place, with at least one of those places ending up inside a
/// closure that must be `'static` (passed to `flat_map_gen`/`lazy_chain`/
/// `catch_gen`/`alt_gen`/`ForeachGen`). `input` may ultimately trace back
/// to a borrowed `&Value` *parameter* (the top-level filter closure's own
/// argument, a recursive `fn`'s parameter, …) whose lifetime is tied to
/// one particular call, not `'static` -- merely *referencing* that
/// parameter (even just to `.clone()` it) from inside such a closure's
/// body is already a hard lifetime error, since the closure then captures
/// a non-`'static` reference as part of its own type.
///
/// The fix: clone `input`'s value into a fresh, block-scoped, owned local
/// *before* (lexically outside) constructing the closure, and compile
/// whatever needs to go *inside* that closure against a `&`-reference to
/// *that* local instead -- since the local is an owned `Value` (no
/// lifetime parameter at all), a closure capturing *it* is trivially
/// `'static`, however many further closures it's nested inside. The
/// returned `stmt` must be spliced immediately before, in the same block
/// as, the combinator call that builds the closure; `tok` is what to pass
/// as that inner compilation's `input`.
fn snapshot_input(input: &TokenStream) -> (TokenStream, TokenStream) {
    let value_ty = core_path("Value");
    let amb = quote!(__jqc_amb);
    // `#[allow(unused_variables)]`: some callers apply this defensively
    // (e.g. at every level of an N-ary nesting chain) without separately
    // proving each individual level's body actually goes on to reference
    // it -- a harmless extra clone either way, but without this a level
    // that happens not to need it would otherwise warn in every crate
    // that depends on `jqc` and uses the affected construct.
    let stmt = quote! {
        #[allow(unused_variables)]
        let #amb: #value_ty = (#input).clone();
    };
    (stmt, quote!(&#amb))
}

/// As [`snapshot_input`], but also re-clones every `$`-bound variable
/// currently in `scope` (not just the ambient input) into a fresh local
/// shadowing its own name -- a bound variable is exactly as liable to be
/// captured-by-move into one sibling's own internal `'static` closure
/// (practically every non-leaf node builds one, via [`flat_map_for`] or
/// similar) as the ambient input is, which would otherwise leave nothing
/// for another sibling -- compiled separately, in the same scope -- that
/// also references it. Label markers (see `label_key`) are skipped; only
/// real variables need a Rust local to shadow.
fn snapshot_scope(input: &TokenStream, scope: &[String]) -> (TokenStream, TokenStream) {
    let (mut stmt, new_input) = snapshot_input(input);
    let value_ty = core_path("Value");
    for name in scope {
        if name.starts_with('\0') {
            continue;
        }
        let jv = jv_ident(name);
        stmt.extend(quote! {
            #[allow(unused_variables)]
            let #jv: #value_ty = #jv.clone();
        });
    }
    (stmt, new_input)
}

/// Compiles `expr` as a fully self-contained, isolated block: its own
/// fresh clone of the ambient input and every in-scope variable (see
/// [`snapshot_scope`]), consumed only by its own code. Use this (instead
/// of a bare [`compile`]) for *every* child of a node with two or more
/// children that share the same enclosing scope -- `If`'s `cond`/`then`/
/// `else`, `BinOp`'s/`Index`'s/`Pipe`'s/`And`/`Or`'s/`Alt`'s/`Comma`'s two
/// operands, `cartesian_args`'s N arguments, `Slice`'s/`range`'s up-to-
/// three bounds, `VarBind`'s `source`/`body`, `TryCatch`'s `body`/`catch`
/// -- since *any* of them might itself be (or contain) a node that builds
/// its own internal `'static` closure, and that closure's capture would
/// otherwise consume a shared local out from under a sibling compiled
/// separately, whether that sibling runs earlier (as this node's own
/// eager argument) or later (nested inside a closure this node builds).
fn compile_isolated(
    expr: &Expr,
    input: &TokenStream,
    scope: &mut Vec<String>,
) -> CResult<TokenStream> {
    let (stmt, new_input) = snapshot_scope(input, scope);
    let code = compile(expr, &new_input, scope)?;
    Ok(quote! { { #stmt #code } })
}

/// Builds a `JGen`-typed expression that lazily flat-maps `sub_c`'s own
/// generator: for each item it pulls, binds it to `item_ident` and
/// evaluates `body` (itself a `JGen`-typed expression). The codegen-side
/// counterpart of `jqc_core::flat_map_gen` -- every pass-through node
/// (`Pipe`, `Field`, `BinOp`'s operands, …) nests this exactly where it
/// nests the sub-expression structurally in the source.
fn flat_map_for(sub_c: &TokenStream, item_ident: &TokenStream, body: TokenStream) -> TokenStream {
    let flat_map_gen = core_path("flat_map_gen");
    let value_ty = core_path("Value");
    let jgen_ty = core_path("JGen");
    quote! {
        #flat_map_gen((#sub_c), move |#item_ident: #value_ty| -> #jgen_ty { #body })
    }
}

/// A `JGen` yielding exactly one item: `expr`, which must itself be
/// `JResult<Value>`-typed (`Ok(v)` or `Err(e)`) -- most atomic,
/// non-generator steps (field access, binops, unary helpers, …) compile
/// straight to this, since `jqc-core`'s own helpers already return
/// `JResult<Value>` directly, with nothing left to unwrap-and-rewrap the
/// way the old `Vec`-based codegen needed.
fn gen_once(expr: TokenStream) -> TokenStream {
    let jgen_ty = core_path("JGen");
    quote! { (::std::boxed::Box::new(::std::iter::once(#expr)) as #jgen_ty) }
}

/// As [`gen_once`], but for a multi-statement `body` that needs its own
/// `?`-propagation context (chaining more than one fallible call) --
/// wraps `body` in a `(|| -> JResult<Value> { ... })()` IIFE first, the
/// same "pin the type right where it's produced" anchor [`compile`]
/// itself uses, then feeds the result to [`gen_once`].
fn gen_once_try(body: TokenStream) -> TokenStream {
    let jresult_ty = core_path("JResult");
    let value_ty = core_path("Value");
    gen_once(quote! { (|| -> #jresult_ty<#value_ty> { #body })() })
}

/// The empty `JGen` -- `empty`, an optional (`?`) lookup that found
/// nothing, a caught error with nothing to substitute, …
fn gen_empty() -> TokenStream {
    let jgen_ty = core_path("JGen");
    quote! { (::std::boxed::Box::new(::std::iter::empty()) as #jgen_ty) }
}

/// A `JGen` yielding exactly one `Err(expr)` -- `expr` must be
/// `JqError`-typed.
fn gen_err(expr: TokenStream) -> TokenStream {
    gen_once(quote! { ::std::result::Result::Err(#expr) })
}

/// A `JGen` built from `compute`, a `JResult<Vec<Value>>`-typed
/// expression (typically an IIFE that eagerly, fully drains one or more
/// sub-generators to build a combined result) -- for *collector* nodes
/// (`[...]`, string interpolation, object construction, `range`, …)
/// where jq itself has no choice but to enumerate everything (or, for
/// `range`, where the whole output is already a plain `Vec` from a
/// single arithmetic sweep) before any of it is observable. `Ok(vs)`
/// becomes a `JGen` yielding each of `vs` in turn; `Err(e)` becomes a
/// `JGen` yielding just that one error.
fn gen_from_vec(compute: TokenStream) -> TokenStream {
    let jgen_ty = core_path("JGen");
    quote! {
        match (#compute) {
            ::std::result::Result::Ok(vs) => {
                ::std::boxed::Box::new(vs.into_iter().map(::std::result::Result::Ok)) as #jgen_ty
            }
            ::std::result::Result::Err(e) => (::std::boxed::Box::new(::std::iter::once(::std::result::Result::Err(e))) as #jgen_ty),
        }
    }
}

/// `match #result_expr { Ok(v) => <one-item gen>, Err(e) => <empty if
/// `optional`, else propagate> }` -- the shared shape of `Field`/
/// `Index`/`Slice`'s per-base-item lookup (all fallible, single-result,
/// `?`-postfix-able).
fn gen_from_lookup(result_expr: TokenStream, optional: bool) -> TokenStream {
    let ok_arm = gen_once(quote!(::std::result::Result::Ok(v)));
    let err_arm = if optional {
        gen_empty()
    } else {
        gen_err(quote!(e))
    };
    quote! {
        match #result_expr {
            ::std::result::Result::Ok(v) => #ok_arm,
            ::std::result::Result::Err(e) => #err_arm,
        }
    }
}

pub fn compile_top(expr: &Expr) -> CResult<TokenStream> {
    let mut scope: Vec<String> = Vec::new();
    let input_ident = format_ident!("__jqc_input");
    let value_ty = core_path("Value");
    // The filter closure's own parameter is a borrowed `&Value` tied to
    // one particular call -- not `'static`. Every compiled `JGen` must be
    // `'static` (nothing in it may hold a non-`'static` reference), so
    // this is converted to an owned local, exactly once, before anything
    // else runs; `compile`/`compile_raw` only ever see `&__jqc_owned`
    // (and `compile_isolated`'s own fresh clones of *that*) from here on,
    // never the original borrowed parameter directly.
    let owned_input = quote!(__jqc_owned_input);
    let body = compile(expr, &quote!(&#owned_input), &mut scope)?;
    let jresult_ty = core_path("JResult");
    let collect_gen = core_path("collect_gen");
    Ok(quote! {
        move |#input_ident: &#value_ty| -> #jresult_ty<::std::vec::Vec<#value_ty>> {
            let #owned_input: #value_ty = #input_ident.clone();
            #collect_gen(#body)
        }
    })
}

/// Compiles `expr` and wraps the result as `(|| -> JGen { ... })()`. The
/// explicit return-type annotation pins the type right where it's
/// produced, instead of leaving a bare `{ ... }` block's type to be
/// inferred from whatever the caller eventually does with it -- which in
/// practice can be several combinator layers away and not always enough
/// for rustc to solve without this anchor.
fn compile(expr: &Expr, input: &TokenStream, scope: &mut Vec<String>) -> CResult<TokenStream> {
    let inner = compile_raw(expr, input, scope)?;
    Ok(wrap_fallible(inner))
}

fn wrap_fallible(inner: TokenStream) -> TokenStream {
    let jgen_ty = core_path("JGen");
    quote! {
        (|| -> #jgen_ty { #inner })()
    }
}

fn compile_raw(expr: &Expr, input: &TokenStream, scope: &mut Vec<String>) -> CResult<TokenStream> {
    match expr {
        Expr::Identity => Ok(gen_once(
            quote!(::std::result::Result::Ok((#input).clone())),
        )),
        Expr::RecurseDefault => Err(Diagnostic::new(
            "internal error: RecurseDefault should have been lowered",
        )),
        Expr::Null => {
            let value_ty = core_path("Value");
            Ok(gen_once(quote!(::std::result::Result::Ok(#value_ty::Null))))
        }
        Expr::Bool(b) => {
            let value_ty = core_path("Value");
            Ok(gen_once(
                quote!(::std::result::Result::Ok(#value_ty::Bool(#b))),
            ))
        }
        Expr::Number(n) => {
            let number_from_f64 = core_path("number_from_f64");
            // A bare float literal token can't represent a non-finite
            // value (`quote`'s `ToTokens` for `f64` asserts `is_finite()`
            // and panics otherwise) -- an overflowing literal like
            // `1e999999999` (real jq keeps such literals' exact decimal
            // text and prints them back verbatim; jqc's `Value` is a
            // plain `f64`, so this is already a divergence, not a new
            // one) parses to `f64::INFINITY` and must be spelled as that
            // expression instead of a literal. `number_from_f64` itself
            // now clamps that to `f64::MAX`/`NEG_INFINITY` to `f64::MIN`
            // (matching real jq's own behavior for an *operation* that
            // overflows, confirmed directly) rather than erroring or
            // panicking on the non-finite value.
            let n_expr = if n.is_finite() {
                quote!(#n)
            } else if n.is_nan() {
                quote!(::std::primitive::f64::NAN)
            } else if *n > 0.0 {
                quote!(::std::primitive::f64::INFINITY)
            } else {
                quote!(::std::primitive::f64::NEG_INFINITY)
            };
            Ok(gen_once(quote!(::std::result::Result::Ok(#number_from_f64(
                #n_expr
            )))))
        }
        Expr::Str(parts) => compile_str(parts, input, scope),
        Expr::Field {
            base,
            name,
            optional,
        } => {
            let index_field = core_path("index_field");
            let base_c = compile(base, input, scope)?;
            let body = gen_from_lookup(quote!(#index_field(&__base, #name)), *optional);
            Ok(flat_map_for(&base_c, &quote!(__base), body))
        }
        Expr::Index {
            base,
            index,
            optional,
        } => {
            let index_value = core_path("index_value");
            // jq evaluates the bracket expression's generator as the outer
            // loop, the base as the inner one; both are isolated (see
            // `compile_isolated`) since each may reference the same
            // in-scope variable the other does.
            let index_c = compile_isolated(index, input, scope)?;
            let base_c = compile_isolated(base, input, scope)?;
            let inner_body = gen_from_lookup(quote!(#index_value(&__base, &__key)), *optional);
            let inner = flat_map_for(&base_c, &quote!(__base), inner_body);
            Ok(flat_map_for(&index_c, &quote!(__key), inner))
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
            let jgen_ty = core_path("JGen");
            let err_arm = if *optional {
                gen_empty()
            } else {
                gen_err(quote!(e))
            };
            let body = quote! {
                match #iterate(&__base) {
                    ::std::result::Result::Ok(vs) => {
                        ::std::boxed::Box::new(vs.into_iter().map(::std::result::Result::Ok)) as #jgen_ty
                    }
                    ::std::result::Result::Err(e) => #err_arm,
                }
            };
            Ok(flat_map_for(&base_c, &quote!(__base), body))
        }
        Expr::Pipe(lhs, rhs) => {
            // `rhs` is rebound to the piped-through item (`__piped`), but
            // may still reference an *outer* in-scope variable `lhs` also
            // references -- both isolated, same reasoning as `Index`.
            let lhs_c = compile_isolated(lhs, input, scope)?;
            let rhs_c = compile_isolated(rhs, &quote!(&__piped), scope)?;
            Ok(flat_map_for(&lhs_c, &quote!(__piped), rhs_c))
        }
        Expr::Comma(lhs, rhs) => {
            let lhs_c = compile_isolated(lhs, input, scope)?;
            let rhs_c = compile_isolated(rhs, input, scope)?;
            let lazy_chain = core_path("lazy_chain");
            let jgen_ty = core_path("JGen");
            Ok(quote! {
                #lazy_chain((#lhs_c), move || -> #jgen_ty { #rhs_c })
            })
        }
        Expr::ArrayConstruct(inner) => {
            let value_ty = core_path("Value");
            match inner {
                None => Ok(gen_once(quote!(::std::result::Result::Ok(
                    #value_ty::Array(::std::vec::Vec::new())
                )))),
                Some(e) => {
                    let inner_c = compile(e, input, scope)?;
                    let collect_gen = core_path("collect_gen");
                    Ok(gen_once(
                        quote!(#collect_gen(#inner_c).map(#value_ty::Array)),
                    ))
                }
            }
        }
        Expr::ObjectConstruct(entries) => compile_object(entries, input, scope),
        Expr::Neg(x) => {
            let neg_value = core_path("neg_value");
            let x_c = compile(x, input, scope)?;
            let body = gen_once(quote!(#neg_value(__val)));
            Ok(flat_map_for(&x_c, &quote!(__val), body))
        }
        Expr::BinOp(op, lhs, rhs) => compile_binop(*op, lhs, rhs, input, scope),
        Expr::And(lhs, rhs) => compile_and_or(lhs, rhs, input, scope, true),
        Expr::Or(lhs, rhs) => compile_and_or(lhs, rhs, input, scope, false),
        Expr::Alt(lhs, rhs) => {
            let lhs_c = compile_isolated(lhs, input, scope)?;
            let rhs_c = compile_isolated(rhs, input, scope)?;
            let truthy = core_path("truthy");
            let alt_gen = core_path("alt_gen");
            let jgen_ty = core_path("JGen");
            // `a // b` keeps only `a`'s non-`false`/`null` outputs,
            // falling back to `b` only if that filtered set ends up
            // empty -- unlike `try`/`?`, an error in `a` is *not*
            // caught; it propagates (verified against real jq).
            // `alt_gen` implements exactly this by pulling one item
            // ahead of the truthy-filtered stream below.
            let gen_once_ok_v = gen_once(quote!(::std::result::Result::Ok(__v)));
            let gen_empty_arm = gen_empty();
            let filtered = flat_map_for(
                &lhs_c,
                &quote!(__v),
                quote! {
                    if #truthy(&__v) { #gen_once_ok_v } else { #gen_empty_arm }
                },
            );
            Ok(quote! {
                #alt_gen((#filtered), move || -> #jgen_ty { #rhs_c })
            })
        }
        Expr::If(branches, else_branch) => {
            compile_if(branches, else_branch.as_deref(), input, scope)
        }
        Expr::TryCatch(body, catch) => {
            let body_c = compile_isolated(body, input, scope)?;
            let catch_gen = core_path("catch_gen");
            let jq_error_ty = core_path("JqError");
            // `break` is *not* specially exempted here -- confirmed
            // against real jq (`label $out | try break $out catch "x"`
            // produces `"x"`, not an uncaught break): only a `break`'s
            // *own matching label* ([`Expr::Label`] below) is guaranteed
            // to see it.
            let handler = match catch {
                Some(c) => {
                    let catch_c = compile_isolated(c, &quote!(&__caught), scope)?;
                    quote! {
                        move |e: #jq_error_ty| {
                            let __caught = e.into_value();
                            ::std::result::Result::Ok(#catch_c)
                        }
                    }
                }
                None => {
                    let empty = gen_empty();
                    quote! {
                        move |_e: #jq_error_ty| ::std::result::Result::Ok(#empty)
                    }
                }
            };
            Ok(quote! { #catch_gen((#body_c), #handler) })
        }
        Expr::Optional(body) => {
            let body_c = compile(body, input, scope)?;
            let catch_gen = core_path("catch_gen");
            let jq_error_ty = core_path("JqError");
            let empty = gen_empty();
            // `?` catches a `break` exactly like any other error -- see
            // the longer note on `Expr::TryCatch`.
            let handler = quote! {
                move |_e: #jq_error_ty| ::std::result::Result::Ok(#empty)
            };
            Ok(quote! { #catch_gen((#body_c), #handler) })
        }
        Expr::Label(name, body) => {
            scope.push(label_key(name));
            let body_c = compile(body, input, scope);
            scope.pop();
            let body_c = body_c?;
            let catch_gen = core_path("catch_gen");
            let jq_error_ty = core_path("JqError");
            let empty = gen_empty();
            // Only a `break` naming *this* label is ours to catch --
            // anything else (a real error, or a `break` for a
            // differently-named, or outer, label) propagates unchanged,
            // so it keeps unwinding to whichever `label`/`try` it
            // actually belongs to.
            let handler = quote! {
                move |e: #jq_error_ty| {
                    if e.break_label.as_deref() == ::std::option::Option::Some(#name) {
                        ::std::result::Result::Ok(#empty)
                    } else {
                        ::std::result::Result::Err(e)
                    }
                }
            };
            Ok(quote! { #catch_gen((#body_c), #handler) })
        }
        Expr::Break(name, span) => {
            let key = label_key(name);
            if !scope.iter().any(|s| s == &key) {
                let labels_in_scope = scope.iter().filter_map(|s| s.strip_prefix("\0label\0"));
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
            Ok(gen_err(quote!(#jq_error::brk(#name))))
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
            // `body` may reference the ambient `.`/outer variables too
            // (`as` doesn't rebind `.`), ending up nested inside `source`'s
            // own (`'static`) closure below -- isolated, same as every
            // other multi-child node (see `compile_isolated`).
            let source_c = compile_isolated(source, input, scope)?;
            scope.push(name.clone());
            let body_c = compile_isolated(body, input, scope);
            scope.pop();
            let body_c = body_c?;
            Ok(flat_map_for(&source_c, &quote!(#jv), body_c))
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
            Ok(gen_once(quote!(::std::result::Result::Ok(#jv.clone()))))
        }
        Expr::Reduce {
            source,
            var,
            init,
            update,
        } => {
            let jv = jv_ident(var);
            // `init`, `source`, and (each loop iteration) `update` are
            // independent siblings sharing this scope -- each isolated
            // (see `compile_isolated`).
            let init_c = compile_isolated(init, input, scope)?;
            let source_c = compile_isolated(source, input, scope)?;
            scope.push(var.clone());
            let update_c = compile_isolated(update, &quote!(&__state), scope);
            scope.pop();
            let update_c = update_c?;
            let value_ty = core_path("Value");
            let per_init = quote! {
                let mut __state = __init;
                for #jv in (#source_c) {
                    let #jv = #jv?;
                    let mut __last: #value_ty = #value_ty::Null;
                    for __u in (#update_c) {
                        __last = __u?;
                    }
                    __state = __last;
                }
                ::std::result::Result::Ok(__state)
            };
            Ok(flat_map_for(
                &init_c,
                &quote!(__init),
                gen_once_try(per_init),
            ))
        }
        Expr::Foreach {
            source,
            var,
            init,
            update,
            extract,
        } => {
            let jv = jv_ident(var);
            let value_ty = core_path("Value");
            let jgen_ty = core_path("JGen");
            // `init`, `source`, `update`, and `extract` are independent
            // siblings sharing this scope -- each isolated (see
            // `compile_isolated`).
            let init_c = compile_isolated(init, input, scope)?;
            let source_c = compile_isolated(source, input, scope)?;
            scope.push(var.clone());
            // `ForeachGen`'s `update`/`extract` closures take `jv`/`state`
            // as borrowed `&Value` *parameters* -- fine on their own (a
            // closure parameter's lifetime is universally quantified per
            // call, not part of the closure's own `'static` capture
            // requirement), but if `update`/`extract` themselves contain
            // a node that defers part of its own compilation into a
            // *further* `'static` closure (`Comma`, `try`/`catch`,
            // `label`, `//`), that inner closure would need to capture
            // the borrowed parameter directly -- same `'static` violation
            // as `compile_top`'s `__jqc_owned_input`. Shadowed with an
            // owned clone, once, before `update_c`/`extract_c` (compiled
            // against `&__state`/`#jv` from here on) ever run.
            let owned_prelude = quote! {
                let #jv: #value_ty = #jv.clone();
                let __state: #value_ty = __state.clone();
            };
            let update_c = compile_isolated(update, &quote!(&__state), scope);
            let extract_c = extract
                .as_ref()
                .map(|e| compile_isolated(e, &quote!(&__state), scope))
                .transpose();
            scope.pop();
            let update_c = update_c?;
            let extract_c = extract_c?;
            let foreach_gen = core_path("ForeachGen");
            // `update_closure`/`extract_closure` below are two *sibling*
            // `move` closures, both passed to `ForeachGen::new` -- unlike
            // `update_c`/`extract_c`'s own internal `compile_isolated`
            // re-clones (safe: those just re-clone whatever the closure
            // already captured, fresh per *call*), a `move` closure
            // captures whatever outer-scope `$var` it references by
            // *moving* it out of its enclosing environment at the point
            // the closure *literal* is constructed, not deferred to when
            // it's called. If both closures reference the same outer
            // `$var` (one in scope from an *enclosing* `foreach`/`reduce`/
            // `as`, e.g. `limit`'s own internal foreach nested inside a
            // user-written one), constructing the first one here moves it
            // away, leaving nothing for the second's own construction --
            // a real, reproduced `E0507` (confirmed directly: `[foreach
            // (1,2) as $x (0; [limit(3; while(true; .))])]` failed to
            // compile before this fix). Each gets its own independent
            // `snapshot_scope` fresh-clone *before* its own `move |...|`
            // literal, so the two constructions never race to consume the
            // same shared local -- the same "two siblings sharing scope
            // each need their own isolated copy" reasoning as
            // `compile_isolated` itself, just applied one level further
            // out (at closure-*construction* time, not just inside each
            // closure's body).
            let (extract_snap_stmt, _) = snapshot_scope(input, scope);
            let extract_closure = match extract_c {
                Some(ec) => quote! {
                    {
                        #extract_snap_stmt
                        #[allow(unused_variables)]
                        let __jqc_extract = move |#jv: &#value_ty, __state: &#value_ty| -> #jgen_ty {
                            #owned_prelude
                            #ec
                        };
                        __jqc_extract
                    }
                },
                None => {
                    let push_state = gen_once(quote!(::std::result::Result::Ok(__state.clone())));
                    quote! { move |_: &#value_ty, __state: &#value_ty| -> #jgen_ty { #push_state } }
                }
            };
            let (update_snap_stmt, _) = snapshot_scope(input, scope);
            let update_closure = quote! {
                {
                    #update_snap_stmt
                    #[allow(unused_variables)]
                    let __jqc_update = move |#jv: &#value_ty, __state: &#value_ty| -> #jgen_ty {
                        #owned_prelude
                        #update_c
                    };
                    __jqc_update
                }
            };
            let body = quote! {
                #foreach_gen::new((#source_c), __init, #update_closure, #extract_closure)
            };
            Ok(flat_map_for(&init_c, &quote!(__init), body))
        }
        Expr::RecCall(name) => {
            let fname = fn_ident(name);
            Ok(quote!(#fname(#input)))
        }
        Expr::RecFn0 { name, body, rest } => {
            let fname = fn_ident(name);
            let value_ty = core_path("Value");
            let jgen_ty = core_path("JGen");
            // Same reasoning as `compile_top`'s own `__jqc_owned_input`:
            // `__jqc_in` is a borrowed parameter tied to one particular
            // call, not `'static` -- converted to an owned local, once,
            // before `body` (whose returned `JGen` must be `'static`)
            // ever runs.
            //
            // `body` is compiled against a *fresh, empty* scope, not
            // `scope` itself: a plain Rust `fn` item (unlike a closure)
            // can never capture anything from its enclosing scope, so an
            // ambient `$var`/label that happens to be in scope at this
            // `def`'s own call site genuinely isn't accessible from
            // inside `body` once it's compiled as one -- real jq's own
            // `def`s are proper closures that *can* reference their
            // lexical environment even when recursive, which jqc's
            // "recursive 0-arg def becomes a real `fn`" strategy doesn't
            // support; confirmed directly (`. as $limit | def r: if . >=
            // $limit then . else (.+1|r) end; 0|r` fails to compile).
            // Passing `scope` through unchanged here would silently emit
            // code referencing a Rust local that doesn't exist inside
            // `fn #fname`'s own body -- a confusing downstream rustc
            // capture error (also reproduced directly: any of this
            // crate's own `while`/`until`/`repeat`/`recurse(f)` nested
            // inside so much as one enclosing `foreach`/`reduce`/`as`/
            // `label`, *even when its own `cond`/`update`/`f` argument
            // never references that outer binding at all*, failed to
            // compile before this fix -- `compile_isolated`'s own
            // unconditional "re-clone every in-scope variable, a harmless
            // extra clone either way" doesn't hold *here*: inside a `fn`
            // item there is no outer variable to clone from at all). An
            // empty scope instead makes a genuine free reference surface
            // as this crate's own, already-existing "undefined variable"/
            // "undefined label" diagnostic (`Expr::VarRef`/`Expr::Break`
            // below) at the point `body` actually uses it, rather than a
            // wall of rustc output -- the same "reject cleanly instead of
            // emitting code that can't work" choice as the recursive-
            // with-parameters check above, just discovered one level down
            // instead of pre-checked here.
            let mut body_scope = Vec::new();
            let body_c = compile(body, &quote!(&__jqc_owned_in), &mut body_scope)?;
            let rest_c = compile(rest, input, scope)?;
            Ok(quote! {
                {
                    fn #fname(__jqc_in: &#value_ty) -> #jgen_ty {
                        let __jqc_owned_in: #value_ty = __jqc_in.clone();
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
    let jresult_ty = core_path("JResult");
    let display = core_path("value_to_display_string");
    let mut steps = Vec::new();
    for part in parts {
        match part {
            StrPart::Lit(s) => steps.push(quote! {
                __out = __out.into_iter().map(|prefix: ::std::string::String| prefix + #s).collect();
            }),
            StrPart::Interp(e) => {
                // Isolated (see `compile_isolated`): more than one
                // interpolation in the same string shares this scope, and
                // each is a fully separate statement below, not nested
                // inside one another.
                let e_c = compile_isolated(e, input, scope)?;
                steps.push(quote! {
                    __out = {
                        let mut __next = ::std::vec::Vec::new();
                        for __prefix in &__out {
                            for __val in (#e_c) {
                                let __val = __val?;
                                __next.push(__prefix.clone() + &#display(&__val));
                            }
                        }
                        __next
                    };
                });
            }
        }
    }
    let compute = quote! {
        (|| -> #jresult_ty<::std::vec::Vec<#value_ty>> {
            let mut __out: ::std::vec::Vec<::std::string::String> = ::std::vec![::std::string::String::new()];
            #(#steps)*
            ::std::result::Result::Ok(__out.into_iter().map(#value_ty::String).collect::<::std::vec::Vec<#value_ty>>())
        })()
    };
    Ok(gen_from_vec(compute))
}

fn compile_object(
    entries: &[ObjEntry],
    input: &TokenStream,
    scope: &mut Vec<String>,
) -> CResult<TokenStream> {
    let value_ty = core_path("Value");
    let map_ty = core_path("Map");
    let jresult_ty = core_path("JResult");
    let object_insert = core_path("object_insert");
    let mut steps = Vec::new();
    for entry in entries {
        // Isolated (see `compile_isolated`): `key`/`val`, and separate
        // entries, all share this scope, each a separate statement below
        // rather than nested inside one another.
        let key_c = compile_isolated(&entry.key, input, scope)?;
        let val_c = compile_isolated(&entry.val, input, scope)?;
        steps.push(quote! {
            __partials = {
                let mut __next: ::std::vec::Vec<#map_ty<::std::string::String, #value_ty>> = ::std::vec::Vec::new();
                for __partial in &__partials {
                    for __key in (#key_c) {
                        let __key = __key?;
                        for __val in (#val_c) {
                            let __val = __val?;
                            __next.push(#object_insert(__partial.clone(), __key.clone(), __val)?);
                        }
                    }
                }
                __next
            };
        });
    }
    let compute = quote! {
        (|| -> #jresult_ty<::std::vec::Vec<#value_ty>> {
            let mut __partials: ::std::vec::Vec<#map_ty<::std::string::String, #value_ty>> = ::std::vec![#map_ty::new()];
            #(#steps)*
            ::std::result::Result::Ok(__partials.into_iter().map(#value_ty::Object).collect::<::std::vec::Vec<#value_ty>>())
        })()
    };
    Ok(gen_from_vec(compute))
}

fn compile_binop(
    op: BinOp,
    lhs: &Expr,
    rhs: &Expr,
    input: &TokenStream,
    scope: &mut Vec<String>,
) -> CResult<TokenStream> {
    let rhs_c = compile_isolated(rhs, input, scope)?;
    let lhs_c = compile_isolated(lhs, input, scope)?;
    let value_ty = core_path("Value");
    // Each op_call evaluates to `JResult<Value>` (even the infallible
    // comparisons, wrapped in `Ok`), fed straight to `gen_once` below.
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
            quote!(::std::result::Result::Ok(#value_ty::Bool(#f(&__lhs, &__rhs)?)))
        }
        BinOp::Ne => {
            let f = core_path("try_eq_values");
            quote!(::std::result::Result::Ok(#value_ty::Bool(!#f(&__lhs, &__rhs)?)))
        }
        BinOp::Lt => {
            let f = core_path("try_cmp_values");
            quote!(::std::result::Result::Ok(#value_ty::Bool(#f(&__lhs, &__rhs)? == ::std::cmp::Ordering::Less)))
        }
        BinOp::Le => {
            let f = core_path("try_cmp_values");
            quote!(::std::result::Result::Ok(#value_ty::Bool(#f(&__lhs, &__rhs)? != ::std::cmp::Ordering::Greater)))
        }
        BinOp::Gt => {
            let f = core_path("try_cmp_values");
            quote!(::std::result::Result::Ok(#value_ty::Bool(#f(&__lhs, &__rhs)? == ::std::cmp::Ordering::Greater)))
        }
        BinOp::Ge => {
            let f = core_path("try_cmp_values");
            quote!(::std::result::Result::Ok(#value_ty::Bool(#f(&__lhs, &__rhs)? != ::std::cmp::Ordering::Less)))
        }
    };
    // jq evaluates the right operand's generator as the outer loop (e.g.
    // `(1,2) + (10,20)` yields `11,12,21,22`, not `11,21,12,22`).
    let inner = flat_map_for(&lhs_c, &quote!(__lhs), gen_once_try(op_call));
    Ok(flat_map_for(&rhs_c, &quote!(__rhs), inner))
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
    let lhs_c = compile_isolated(lhs, input, scope)?;
    let rhs_c = compile_isolated(rhs, input, scope)?;
    let (short_circuit_on, short_circuit_value) = if is_and {
        (quote!(!), false)
    } else {
        (quote!(), true)
    };
    let rhs_body = gen_once(quote!(::std::result::Result::Ok(#value_ty::Bool(#truthy(&__rhs)))));
    let rhs_block = flat_map_for(&rhs_c, &quote!(__rhs), rhs_body);
    let short_circuit_gen = gen_once(quote!(::std::result::Result::Ok(
        #value_ty::Bool(#short_circuit_value)
    )));
    let lhs_body = quote! {
        if #short_circuit_on #truthy(&__lhs) {
            #short_circuit_gen
        } else {
            #rhs_block
        }
    };
    Ok(flat_map_for(&lhs_c, &quote!(__lhs), lhs_body))
}

fn compile_if(
    branches: &[(Expr, Expr)],
    else_branch: Option<&Expr>,
    input: &TokenStream,
    scope: &mut Vec<String>,
) -> CResult<TokenStream> {
    if branches.is_empty() {
        // The base case's result ends up nested inside some ancestor
        // `If`'s `cond`-binding closure (see the isolation note below) --
        // isolated the same as every other leaf reached that way.
        return match else_branch {
            Some(e) => compile_isolated(e, input, scope),
            None => compile_isolated(&Expr::Identity, input, scope),
        };
    }
    let (cond, then_e) = &branches[0];
    // `cond` and `then` are both embedded directly inside this level's
    // own `cond`-binding closure and nothing deeper -- isolated against
    // `input` directly is enough (see `compile_isolated`). `else`,
    // though, is a *recursive* call: an `elif` chain nests each further
    // branch's own `cond'`/`then'` *inside* this level's closure too, so
    // (same reasoning as `compile_slice`'s `in_from`/`in_to`) it must be
    // compiled against a fresh, local re-clone safe to use *from within*
    // this closure, not `input` directly, or this closure would consume
    // its own copy constructing the next `elif` level's closure.
    let cond_c = compile_isolated(cond, input, scope)?;
    let then_c = compile_isolated(then_e, input, scope)?;
    let (in_else_snap, in_else) = snapshot_scope(input, scope);
    let else_c = wrap_fallible(compile_if(&branches[1..], else_branch, &in_else, scope)?);
    let else_c = quote! { { #in_else_snap #else_c } };
    let truthy = core_path("truthy");
    let body = quote! {
        if #truthy(&__cond) {
            #then_c
        } else {
            #else_c
        }
    };
    Ok(flat_map_for(&cond_c, &quote!(__cond), body))
}

fn compile_slice(
    base: &Expr,
    from: Option<&Expr>,
    to: Option<&Expr>,
    optional: bool,
    input: &TokenStream,
    scope: &mut Vec<String>,
) -> CResult<TokenStream> {
    let slice_value = core_path("slice_value");
    let to_i64 = core_path("value_to_i64");

    // jq nests `from` around `to` around `base` (outer to inner): `to`
    // ends up embedded inside `from`'s own resultant `'static` closure,
    // and `base` inside `to`'s -- each deeper level must be compiled
    // against a token safe to reference *from within* the enclosing
    // level's closure, meaning a fresh, local re-clone introduced right
    // there (`in_from`, `in_to`), not `input` directly. Reusing a single
    // clone across more than one nested closure boundary would have the
    // enclosing closure consume its own copy while constructing the next
    // one down, breaking its own repeatability -- see `snapshot_input`'s
    // doc. `from` itself (outermost, if present) is the one safe to
    // compile directly against `input`, since nothing nests *around* it.
    let from_c = from
        .map(|e| compile_isolated(e, input, scope))
        .transpose()?;
    let (in_from_snap, in_from) = snapshot_scope(input, scope);
    let to_c = to
        .map(|e| compile_isolated(e, &in_from, scope))
        .transpose()?;
    let (in_to_snap, in_to) = snapshot_scope(&in_from, scope);
    let base_c = compile_isolated(base, &in_to, scope)?;

    // jq nests these outer-to-inner as from, to, base (least significant),
    // mirroring how it treats the bracket expression as outer and the
    // receiver as inner for plain indexing.
    let base_body = gen_from_lookup(quote!(#slice_value(&__base, __from, __to)), optional);
    let base_block = flat_map_for(&base_c, &quote!(__base), base_body);
    let base_block = quote! { { #in_to_snap #base_block } };
    let with_to = match to_c {
        Some(c) => {
            let gen_err_arm = gen_err(quote!(e));
            let to_body = quote! {
                match #to_i64(&__to_src) {
                    ::std::result::Result::Ok(__to_val) => {
                        let __to: ::std::option::Option<i64> = ::std::option::Option::Some(__to_val);
                        #base_block
                    }
                    ::std::result::Result::Err(e) => #gen_err_arm,
                }
            };
            flat_map_for(&c, &quote!(__to_src), to_body)
        }
        None => quote! {
            {
                let __to: ::std::option::Option<i64> = ::std::option::Option::None;
                #base_block
            }
        },
    };
    let with_to = quote! { { #in_from_snap #with_to } };
    let with_from = match from_c {
        Some(c) => {
            let gen_err_arm = gen_err(quote!(e));
            let from_body = quote! {
                match #to_i64(&__from_src) {
                    ::std::result::Result::Ok(__from_val) => {
                        let __from: ::std::option::Option<i64> = ::std::option::Option::Some(__from_val);
                        #with_to
                    }
                    ::std::result::Result::Err(e) => #gen_err_arm,
                }
            };
            flat_map_for(&c, &quote!(__from_src), from_body)
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
/// arg's compiled generator is bound to `__a0`, `__a1`, … (outermost to
/// innermost, left to right), and `build` receives those idents to produce
/// the single yielded expression.
fn cartesian_args(
    args: &[Expr],
    input: &TokenStream,
    scope: &mut Vec<String>,
    mut build: impl FnMut(&TokenStream, &[TokenStream]) -> TokenStream,
) -> CResult<TokenStream> {
    // Every arg is an independent sibling sharing this scope -- each
    // isolated (see `compile_isolated`), regardless of how many levels of
    // nesting (one per other arg) it ends up embedded within below: a
    // `compile_isolated` block re-clones whatever it needs fresh every
    // time it's actually reached, so it doesn't matter how deep.
    //
    // `build`'s own direct use of the ambient input (`eff_input`, if any
    // -- e.g. `has_key(input, &arg)`) is a separate, codegen-internal
    // concern: today every caller passes exactly one arg, so `eff_input`
    // never crosses more than the one closure boundary that introduces
    // it and needs no extra treatment. A future 2+-arg use would need the
    // same per-level re-clone `compile_range`'s `__from`/`__to` get, for
    // the same reason (a shared `Value` surviving more than one nested
    // `'static` closure).
    let (snap_stmt, snap_input) = snapshot_scope(input, scope);
    let compiled: Vec<TokenStream> = args
        .iter()
        .map(|a| compile_isolated(a, input, scope))
        .collect::<CResult<Vec<_>>>()?;
    let names: Vec<Ident> = (0..args.len()).map(|i| format_ident!("__a{i}")).collect();
    let name_toks: Vec<TokenStream> = names.iter().map(|n| quote!(#n)).collect();
    // `build` must produce a `JResult<Value>`-typed expression (fallible
    // helpers used directly, infallible ones `Ok`-wrapped by the caller),
    // referencing the ambient input (if at all) via the first argument
    // passed to it, not whatever token its own caller closed over.
    let pushed = build(&snap_input, &name_toks);
    let mut block: TokenStream = gen_once(pushed);
    for (name, c) in names.iter().zip(compiled.iter()).rev() {
        block = flat_map_for(c, &quote!(#name), block);
    }
    Ok(quote! { { #snap_stmt #block } })
}

/// `NAME(arg)` builtins that are directly jqc-core's own `fn(&Value,
/// &Value) -> JResult<Value>`, called as `#fn_name(input, &arg)` -- the
/// shape shared by `has`/`startswith`/`endswith`/`ltrimstr`/`rtrimstr`/
/// `join`/`split`'s single-argument call sites below, so each of those is
/// just this plus a name instead of its own copy of the boilerplate.
fn compile_core_fn2(
    fn_name: &str,
    args: &[Expr],
    input: &TokenStream,
    scope: &mut Vec<String>,
) -> CResult<TokenStream> {
    let f = core_path(fn_name);
    cartesian_args(args, input, scope, |eff_input, a| {
        let arg = &a[0];
        quote!(#f(#eff_input, &#arg))
    })
}

/// `first(f)`: a genuinely lazy one-pull take of `f`'s own generator,
/// discarding the rest -- unlike the old eager model (which had to fully
/// materialize `f`'s output just to call `.next()` on it), this never
/// evaluates more of `f` than the single item it needs, so `first(repeat(1))`
/// no longer has to fully evaluate `repeat(1)` first.
fn compile_first_1(
    arg: &Expr,
    input: &TokenStream,
    scope: &mut Vec<String>,
) -> CResult<TokenStream> {
    let arg_c = compile(arg, input, scope)?;
    let jgen_ty = core_path("JGen");
    let empty_arm = gen_empty();
    Ok(quote! {
        {
            let mut __g = (#arg_c);
            match __g.next() {
                ::std::option::Option::Some(r) => (::std::boxed::Box::new(::std::iter::once(r)) as #jgen_ty),
                ::std::option::Option::None => #empty_arm,
            }
        }
    })
}

/// `last(f)`: must still fully drain `f`'s generator, same as real jq.
fn compile_last_1(
    arg: &Expr,
    input: &TokenStream,
    scope: &mut Vec<String>,
) -> CResult<TokenStream> {
    let arg_c = compile(arg, input, scope)?;
    let value_ty = core_path("Value");
    let jresult_ty = core_path("JResult");
    let collect_gen = core_path("collect_gen");
    let compute = quote! {
        (|| -> #jresult_ty<::std::vec::Vec<#value_ty>> {
            let __items = #collect_gen(#arg_c)?;
            ::std::result::Result::Ok(__items.into_iter().last().into_iter().collect())
        })()
    };
    Ok(gen_from_vec(compute))
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
        ("empty", 0) => Ok(gen_empty()),
        ("error", 0) => {
            let err = core_path("JqError");
            Ok(gen_err(quote!(#err::value((#input).clone()))))
        }
        ("length", 0) => {
            let f = core_path("length_of");
            Ok(gen_once(quote!(#f(#input))))
        }
        ("type", 0) => {
            let f = core_path("type_name");
            Ok(gen_once(quote!(::std::result::Result::Ok(
                #value_ty::String(#f(#input).to_string())
            ))))
        }
        ("keys", 0) => {
            let f = core_path("keys_of");
            Ok(gen_once(quote!(#f(#input, true))))
        }
        ("keys_unsorted", 0) => {
            let f = core_path("keys_of");
            Ok(gen_once(quote!(#f(#input, false))))
        }
        ("add", 0) => {
            // `add`'s real jq definition is `reduce .[] as $x (null; . +
            // $x)` -- `.[]` iterates object *values* same as array
            // elements, so `add` must too (confirmed directly: real jq's
            // `add` on `{"a":1,"b":2}` gives `3`, same as `[1,2]`) --
            // `as_array` would reject any object outright, including `{}`
            // (whose correct answer, matching the empty-array case, is
            // `null`, not an error).
            let iterate = core_path("iterate");
            let f = core_path("add_all");
            Ok(gen_once_try(quote!(#f(&#iterate(#input)?))))
        }
        ("reverse", 0) => {
            let f = core_path("reverse_value");
            Ok(gen_once(quote!(#f(#input))))
        }
        ("sort", 0) => {
            let as_array = core_path("as_array");
            let f = core_path("sort_values");
            Ok(gen_once_try(quote!(::std::result::Result::Ok(
                #value_ty::Array(#f(#as_array(#input)?.clone())?)
            ))))
        }
        ("unique", 0) => {
            let as_array = core_path("as_array");
            let f = core_path("unique_values");
            Ok(gen_once_try(quote!(::std::result::Result::Ok(
                #value_ty::Array(#f(#as_array(#input)?.clone())?)
            ))))
        }
        ("min", 0) => {
            let as_array = core_path("as_array");
            let f = core_path("min_values");
            Ok(gen_once_try(quote!(#f(#as_array(#input)?))))
        }
        ("max", 0) => {
            let as_array = core_path("as_array");
            let f = core_path("max_values");
            Ok(gen_once_try(quote!(#f(#as_array(#input)?))))
        }
        ("floor", 0) => simple_unary("floor_value", input),
        ("ceil", 0) => simple_unary("ceil_value", input),
        ("round", 0) => simple_unary("round_value", input),
        ("sqrt", 0) => simple_unary("sqrt_value", input),
        ("fabs", 0) => simple_unary("fabs_value", input),
        ("explode", 0) => simple_unary("explode", input),
        ("implode", 0) => simple_unary("implode", input),
        ("ascii_downcase", 0) => simple_unary(
            backend_fn_name("ascii_downcase", "simd_ascii_downcase"),
            input,
        ),
        ("ascii_upcase", 0) => {
            simple_unary(backend_fn_name("ascii_upcase", "simd_ascii_upcase"), input)
        }
        ("tonumber", 0) => simple_unary("to_number_value", input),
        ("tostring", 0) => {
            let f = core_path("to_string_value");
            Ok(gen_once(quote!(::std::result::Result::Ok(#f(#input)))))
        }
        ("first", 0) => {
            let f = core_path("index_value");
            Ok(gen_once(quote!(#f(#input, &#value_ty::from(0)))))
        }
        ("last", 0) => {
            let f = core_path("index_value");
            Ok(gen_once(quote!(#f(#input, &#value_ty::from(-1)))))
        }
        ("first", 1) => compile_first_1(&args[0], input, scope),
        ("last", 1) => compile_last_1(&args[0], input, scope),
        // Every one of these is jqc-core's own `fn(&Value, &Value) ->
        // JResult<Value>` called directly on `input` and the (cartesian-
        // expanded) argument -- see `compile_core_fn2`.
        ("has", 1) => compile_core_fn2("has_key", args, input, scope),
        ("startswith", 1) => compile_core_fn2("startswith", args, input, scope),
        ("endswith", 1) => compile_core_fn2("endswith", args, input, scope),
        ("ltrimstr", 1) => compile_core_fn2("ltrimstr", args, input, scope),
        ("rtrimstr", 1) => compile_core_fn2("rtrimstr", args, input, scope),
        ("join", 1) => compile_core_fn2("join_strings", args, input, scope),
        ("split", 1) => compile_core_fn2("split_string", args, input, scope),
        // As above, but `contains_value` returns `JResult<bool>` rather
        // than `JResult<Value>`, needing the extra `.map`.
        ("contains", 1) => {
            let f = core_path(backend_fn_name("contains_value", "simd_contains_value"));
            cartesian_args(args, input, scope, |eff_input, a| {
                let arg = &a[0];
                quote!(#f(#eff_input, &#arg).map(#value_ty::Bool))
            })
        }
        ("range", 1) => {
            let range_values = core_path("range_values");
            let arg_c = compile(&args[0], input, scope)?;
            let to_f64 = core_path("value_to_f64");
            let jresult_ty = core_path("JResult");
            let compute = quote! {
                (|| -> #jresult_ty<::std::vec::Vec<#value_ty>> {
                    ::std::result::Result::Ok(#range_values(0.0, #to_f64(&__to)?, 1.0))
                })()
            };
            Ok(flat_map_for(&arg_c, &quote!(__to), gen_from_vec(compute)))
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
                    None => {
                        err.help("see the README's \"Supported jq subset\" for what's implemented")
                    }
                }
            };
            Err(err)
        }
    }
}

/// Picks `simd_name` under [`SimdCodegen`], `scalar_name` under
/// [`TreeWalkCodegen`] -- the only place `compile_call` consults
/// [`BACKEND`], for the handful of builtins (`ascii_downcase`/
/// `ascii_upcase`/`contains`) `SimdCodegen` implements differently.
fn backend_fn_name(scalar_name: &'static str, simd_name: &'static str) -> &'static str {
    #[cfg(not(feature = "codegen-simd"))]
    let _ = simd_name;
    match BACKEND.with(|b| b.get()) {
        Backend::TreeWalk => scalar_name,
        #[cfg(feature = "codegen-simd")]
        Backend::Simd => simd_name,
    }
}

fn simple_unary(fn_name: &str, input: &TokenStream) -> CResult<TokenStream> {
    let f = core_path(fn_name);
    Ok(gen_once(quote!(#f(#input))))
}

fn compile_range(
    from: &Expr,
    to: &Expr,
    step: Option<&Expr>,
    input: &TokenStream,
    scope: &mut Vec<String>,
) -> CResult<TokenStream> {
    // `from` -> `to` -> `step` nest outer-to-inner (`to` ends up embedded
    // inside `from`'s own resultant `'static` closure, `step` inside
    // `to`'s), so each deeper one is compiled against a fresh, local
    // re-clone of the ambient input safe to use *from within* its
    // enclosing level's closure, not `input` directly -- same reasoning
    // as `compile_slice`'s `in_from`/`in_to`.
    let from_c = compile_isolated(from, input, scope)?;
    let (in_from_snap, in_from) = snapshot_scope(input, scope);
    let to_c = compile_isolated(to, &in_from, scope)?;
    let to_f64 = core_path("value_to_f64");
    let range_values = core_path("range_values");
    let jresult_ty = core_path("JResult");
    let value_ty = core_path("Value");
    let to_body = match step {
        Some(s) => {
            let (in_to_snap, in_to) = snapshot_scope(&in_from, scope);
            let step_c = compile_isolated(s, &in_to, scope)?;
            let compute = quote! {
                (|| -> #jresult_ty<::std::vec::Vec<#value_ty>> {
                    ::std::result::Result::Ok(#range_values(#to_f64(&__from)?, #to_f64(&__to)?, #to_f64(&__step)?))
                })()
            };
            // `__from`/`__to` are *this* (`to`'s) closure's own captured
            // fields (bound by the outer `from`/`to` closures, not this
            // one's own parameter) -- re-clone them immediately before
            // constructing `step`'s nested closure, so this closure's own
            // copies are only ever borrowed, never moved, keeping it
            // safely callable more than once -- same reasoning as
            // `compile_isolated`, just for generator-bound `Value`s
            // introduced by codegen itself rather than a jq variable.
            let reclone_outer = quote! {
                #[allow(unused_variables)]
                let __from: #value_ty = __from.clone();
                #[allow(unused_variables)]
                let __to: #value_ty = __to.clone();
            };
            let wrapped = flat_map_for(&step_c, &quote!(__step), gen_from_vec(compute));
            quote! { { #in_to_snap #reclone_outer #wrapped } }
        }
        None => {
            let compute = quote! {
                (|| -> #jresult_ty<::std::vec::Vec<#value_ty>> {
                    ::std::result::Result::Ok(#range_values(#to_f64(&__from)?, #to_f64(&__to)?, 1.0))
                })()
            };
            gen_from_vec(compute)
        }
    };
    let to_block = flat_map_for(&to_c, &quote!(__to), to_body);
    let to_block = quote! { { #in_from_snap #to_block } };
    Ok(flat_map_for(&from_c, &quote!(__from), to_block))
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
    let collect_gen = core_path("collect_gen");
    // `__item` (the loop variable below) is a borrowed `&Value` tied to
    // one loop iteration, not `'static` -- same reasoning as
    // `compile_top`'s `__jqc_owned_input`: converted to an owned local,
    // once per iteration, before `key_c` (whose compiled `JGen` must be
    // `'static`) ever runs.
    //
    // Isolated (see `compile_isolated`), not just `compile`d plain: this
    // `for` loop runs once per array element, same as `Reduce`'s own
    // per-item loop (which already isolates its `update_c` the same way)
    // -- if the key filter references an in-scope `$var` from an
    // enclosing `foreach`/`reduce`/`as` (not just `__item_owned`), a bare
    // `compile` would splice one fixed copy of that reference into the
    // loop body; the *first* runtime iteration whose compiled code builds
    // a `'static` closure (nearly every non-leaf node does, via
    // `flat_map_gen` et al.) would capture-by-move the single shared
    // outer local, leaving nothing for the second iteration to clone --
    // a real, reproduced `E0382` (confirmed directly: `[foreach (1,2,3)
    // as $x (0; sort_by((2.4|fabs))?)]` failed to compile before this
    // fix, "use of moved value: `__jv_x`... in previous iteration of
    // loop"). `compile_isolated` re-clones every in-scope variable fresh
    // *inside* the loop body text, so each runtime iteration gets its own
    // clone to move from, never touching the shared outer one more than
    // once.
    let key_c = compile_isolated(&args[0], &quote!(&__item_owned), scope)?;
    let result = if wrap_array {
        quote!(::std::result::Result::Ok(#value_ty::Array(#helper(__items, __keys)?)))
    } else {
        quote!(#helper(__items, __keys))
    };
    Ok(gen_once_try(quote! {
        let __items = #as_array(#input)?.clone();
        let mut __keys = ::std::vec::Vec::with_capacity(__items.len());
        for __item in &__items {
            let __item_owned: #value_ty = __item.clone();
            __keys.push(#value_ty::Array(#collect_gen(#key_c)?));
        }
        #result
    }))
}
