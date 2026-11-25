//! The lazy generator type every piece of `jq!`-generated code is built
//! from, and the handful of combinators that compose them.
//!
//! `jqc-compiler`'s codegen used to compile every jq sub-expression to a
//! `JResult<Vec<Value>>`-typed Rust expression: fully materialize, then
//! decide success/failure. That made a genuinely productive-infinite
//! generator (`repeat`, or `while`/`until`/`recurse` with a condition that
//! never goes false) impossible to bound from outside (`limit`, `label`/
//! `break`, `first`) -- whatever wrapped it never got a turn to run until
//! the infinite generator finished, which it never does, so the recursive
//! desugaring that builds it just overflowed the stack instead. Every
//! compiled expression is now `JGen`-typed instead: a real pull-based
//! iterator, so a consumer that stops asking for more (because it hit a
//! `break`, or only wanted the first item) genuinely stops the producer
//! from doing any more work, the same way jq's own generators behave.
//!
//! This also obsoletes the old `JqError::emitted`/`bubble`/`bubble1`/
//! `partial_for` machinery entirely: that existed only to simulate "a
//! `try`/`?` sees whatever a generator already produced before it
//! errored" inside the old eager model, by threading an accumulator
//! through every combinator by hand. Once evaluation is genuinely lazy,
//! that's just what happens on its own when a consumer (array
//! construction, `catch_gen`, the top-level `collect_gen`) stops pulling
//! after the first `Err` item -- nothing separate to maintain.

use crate::error::{JResult, JqError};
use serde_json::Value;

/// A lazy, pull-based jq generator. No lifetime parameter: every `JGen`
/// is built from values it owns outright (cloned out of its parent as
/// soon as they're bound, exactly where the old `Vec`-based codegen
/// already cloned), so everything here is `'static` -- codegen never has
/// to thread a lifetime through the generated code.
///
/// Convention every combinator below upholds and relies on: once a `JGen`
/// yields `Some(Err(_))`, it is *done* -- the next (and every subsequent)
/// call to `next()` returns `None`. Nothing in this module ever polls a
/// `JGen` again after seeing an `Err` from it, and every `JGen`-producing
/// combinator here (and every leaf `jqc-compiler` codegen produces --
/// `std::iter::once`/`std::iter::empty`/a plain `Vec`'s `IntoIter`) only
/// ever yields at most one `Err`, as its last item.
pub type JGen = Box<dyn Iterator<Item = JResult<Value>>>;

/// Runs `body` once per element of `upstream`'s output, flattening each
/// element's own (possibly multi-item, possibly itself failing)
/// sub-generator into this one -- the lazy counterpart of every
/// pass-through node in `jqc-compiler`'s codegen (`Pipe`, `Field`,
/// `BinOp`'s operands, …), where each element a sub-expression produces
/// directly feeds this node's own output.
///
/// `body` is only ever called from inside [`Iterator::next`], once its
/// argument is already in hand -- never while merely *constructing* the
/// combined generator -- so building a `flat_map_gen` chain that will
/// eventually recurse (by calling back into the same compiled function)
/// is always O(1) stack, no matter how deep the eventual recursion goes;
/// only actually *pulling* that far recurses, one call per `next()`.
///
/// Stops immediately -- without pulling `upstream` (or the current
/// sub-generator) any further -- the moment either side yields an `Err`,
/// matching jq's own "an error anywhere ends that generator for good"
/// semantics.
pub fn flat_map_gen(upstream: JGen, f: impl FnMut(Value) -> JGen + 'static) -> JGen {
    struct FlatMapGen<F> {
        upstream: JGen,
        current: Option<JGen>,
        f: F,
        done: bool,
    }
    impl<F: FnMut(Value) -> JGen> Iterator for FlatMapGen<F> {
        type Item = JResult<Value>;
        fn next(&mut self) -> Option<Self::Item> {
            if self.done {
                return None;
            }
            loop {
                if let Some(cur) = &mut self.current {
                    match cur.next() {
                        Some(Ok(v)) => return Some(Ok(v)),
                        Some(Err(e)) => {
                            self.done = true;
                            return Some(Err(e));
                        }
                        None => self.current = None,
                    }
                }
                match self.upstream.next() {
                    Some(Ok(v)) => self.current = Some((self.f)(v)),
                    Some(Err(e)) => {
                        self.done = true;
                        return Some(Err(e));
                    }
                    None => {
                        self.done = true;
                        return None;
                    }
                }
            }
        }
    }
    Box::new(FlatMapGen {
        upstream,
        current: None,
        f,
        done: false,
    })
}

/// Sequences `a` then `b` -- `Expr::Comma`'s codegen -- but only ever
/// calls `make_b` (from inside [`Iterator::next`]) once `a` is fully
/// exhausted, never while still building or draining `a`.
///
/// This is the one genuinely necessary piece of laziness-by-construction
/// in the whole scheme: every *other* composite node reaches a
/// recursive tail only from inside a per-item closure (an `if`/`else`
/// branch, or [`flat_map_gen`]'s own `f`), which Rust already evaluates
/// lazily. `Comma` is the one place two sibling generators are combined
/// directly in sequence rather than nested per-item -- so without this,
/// a self-recursive generator whose first comma-operand always succeeds
/// (`def r: ., (update | r); r` -- `while`/`until`/`repeat`'s own
/// desugaring) would construct its *second* operand immediately and
/// unconditionally, which recurses into building the next one, and the
/// next, with no lazy boundary in between -- stack overflow before a
/// single item is ever pulled. Deferring `make_b` here is what makes
/// that construction cheap (O(1) stack) regardless of how far the
/// eventual recursion goes.
pub fn lazy_chain(a: JGen, make_b: impl FnOnce() -> JGen + 'static) -> JGen {
    struct LazyChain {
        a: JGen,
        make_b: Option<Box<dyn FnOnce() -> JGen>>,
        b: Option<JGen>,
    }
    impl Iterator for LazyChain {
        type Item = JResult<Value>;
        fn next(&mut self) -> Option<Self::Item> {
            loop {
                if let Some(b) = &mut self.b {
                    return b.next();
                }
                match self.a.next() {
                    Some(x) => return Some(x),
                    None => {
                        let make_b = self.make_b.take().expect("lazy_chain polled after done");
                        self.b = Some(make_b());
                    }
                }
            }
        }
    }
    Box::new(LazyChain {
        a,
        make_b: Some(Box::new(make_b)),
        b: None,
    })
}

/// Forwards `body`'s items as they arrive; the moment `body` yields an
/// `Err`, calls `handle` with it: `Ok(replacement)` switches to yielding
/// `replacement`'s items instead (the rest of this generator's output),
/// `Err(e)` re-propagates `e` unchanged and ends this generator.
///
/// The runtime half of `Expr::TryCatch`/`Expr::Optional`/`Expr::Label`,
/// which differ only in `handle`:
/// - bare `?`/`try` (no `catch`): `|_e| Ok(empty())` -- unconditional.
///   Confirmed against real jq: a `break` is *not* specially exempted
///   from an intervening `try`/`?` the way a real error isn't either --
///   only a `break`'s own matching `label` catches it (`label $out | try
///   break $out catch "x"` produces `"x"`, not an uncaught break).
/// - `try`/`catch`: same (unconditional), `Ok` branch runs the compiled
///   `catch` body with `.` bound to the error's value.
/// - `label $l`: `|e| if e.break_label.as_deref() == Some(l) {
///   Ok(empty()) } else { Err(e) }` -- the one guarded case, since a
///   `label` owns only breaks naming it, not errors or other labels'
///   breaks, which must keep propagating past it.
pub fn catch_gen(
    body: JGen,
    handle: impl FnMut(JqError) -> Result<JGen, JqError> + 'static,
) -> JGen {
    enum Phase {
        Body,
        Replacement(JGen),
        Done,
    }
    struct CatchGen<H> {
        body: JGen,
        handle: H,
        phase: Phase,
    }
    impl<H: FnMut(JqError) -> Result<JGen, JqError>> Iterator for CatchGen<H> {
        type Item = JResult<Value>;
        fn next(&mut self) -> Option<Self::Item> {
            loop {
                match &mut self.phase {
                    Phase::Done => return None,
                    Phase::Replacement(g) => match g.next() {
                        Some(x) => return Some(x),
                        None => self.phase = Phase::Done,
                    },
                    Phase::Body => match self.body.next() {
                        Some(Ok(v)) => return Some(Ok(v)),
                        Some(Err(e)) => match (self.handle)(e) {
                            Ok(g) => self.phase = Phase::Replacement(g),
                            Err(e) => {
                                self.phase = Phase::Done;
                                return Some(Err(e));
                            }
                        },
                        None => {
                            self.phase = Phase::Done;
                            return None;
                        }
                    },
                }
            }
        }
    }
    Box::new(CatchGen {
        body,
        handle,
        phase: Phase::Body,
    })
}

/// `lhs // rhs` (`Expr::Alt`). `filtered` must already be `lhs` adapted
/// (via [`flat_map_gen`]) to keep only truthy values and forward errors
/// unchanged -- this just decides, by pulling exactly *one* item ahead,
/// which side wins: nothing (`filtered` exhausted with zero truthy
/// values and no error) falls back to `make_rhs()`; anything else (a
/// truthy value, or an error -- `//`'s left side is *not* caught by an
/// error the way `try`/`?` is, confirmed against real jq) commits to
/// `filtered`'s own stream, re-prepending the one item already pulled.
/// One-item lookahead rather than a full materialize-then-decide -- this
/// is strictly more lazy than deciding by draining `lhs` completely.
pub fn alt_gen(mut filtered: JGen, make_rhs: impl FnOnce() -> JGen + 'static) -> JGen {
    match filtered.next() {
        None => make_rhs(),
        Some(first) => Box::new(std::iter::once(first).chain(filtered)),
    }
}

/// Fully drains `gen`, for *collector* nodes (`[...]`, string
/// interpolation, object construction, the `sort_by` family,
/// `compile_top` itself) where the whole sub-generator must be
/// enumerated before this node's own result exists at all -- matching
/// real jq (`[1,2,error]` doesn't partially emit an array; the
/// construction fails outright) and `Result<Vec<_>, _>`'s own
/// `FromIterator` impl, which already short-circuits at the first `Err`.
pub fn collect_gen(gen: JGen) -> JResult<Vec<Value>> {
    gen.collect()
}

/// The runtime state machine behind `Expr::Foreach`
/// (`foreach source as $var (init; update; extract)`), for one `init`
/// branch (codegen cartesian-expands over `init`'s own generator via
/// [`flat_map_gen`], one `ForeachGen` per branch). A real external lazy
/// generator -- not "drain everything into a `Vec` internally, then
/// re-wrap" -- so a `foreach` over an unbounded source piped into
/// `limit`/`first`/`label`+`break` *from outside* (not just `limit`'s
/// own internal use of `foreach`, in `jqc-compiler`'s prelude) is bounded
/// correctly too.
///
/// `next()` drains three nested levels, innermost (most recently
/// produced) first: the current `extract` output, forwarding each item
/// immediately; once that's exhausted, one more value from the current
/// `update` generator, which becomes the new `state` and seeds a fresh
/// `extract` generator; once *that's* exhausted, one more item from
/// `source`, which (together with the current `state`) seeds a fresh
/// `update` generator. This matches jq's own foreach semantics exactly:
/// if `update` yields several values for one `source` item, `extract`
/// runs once per value (all individually emitted), but only the *last*
/// one becomes the state carried into the next `source` item -- since
/// each new `update` value simply overwrites `state` as it's produced.
///
/// All state is plain struct fields, mutated directly inside this one
/// `next()` -- no `Rc`/`RefCell` needed, since (unlike composing this
/// from nested [`flat_map_gen`] calls, where the inner closure would
/// need to keep mutating state that outlives any single call to the
/// outer one) everything lives in one object the whole time.
/// `update`/`extract`'s shared shape: given the current bound source
/// variable and state, produce the next generator -- factored into a
/// named alias purely to keep [`ForeachGen`]'s own field list (and
/// `clippy::type_complexity`) readable.
type ForeachStep = Box<dyn FnMut(&Value, &Value) -> JGen>;

pub struct ForeachGen {
    source: JGen,
    /// The most recently pulled source item (`$var` in `foreach source as
    /// $var (...)`) -- `extract`, like `update`, is compiled with `$var`
    /// still in scope (jq's own foreach keeps it bound for the whole
    /// body), so both closures need it, not just `state`. Starts as
    /// `Value::Null`; never read before the first successful `source`
    /// pull sets it for real.
    jv: Value,
    state: Value,
    update: ForeachStep,
    extract: ForeachStep,
    update_gen: Option<JGen>,
    extract_gen: Option<JGen>,
    done: bool,
}

impl ForeachGen {
    /// Builds the generator directly as a `JGen` (not `Self`, hence not
    /// named following the usual `new` -> `Self` convention) since that's
    /// the only way any caller ever wants this: a boxed, erased
    /// generator, assembled from already-boxed `update`/`extract`
    /// closures.
    #[allow(clippy::new_ret_no_self)]
    pub fn new(
        source: JGen,
        init: Value,
        update: impl FnMut(&Value, &Value) -> JGen + 'static,
        extract: impl FnMut(&Value, &Value) -> JGen + 'static,
    ) -> JGen {
        Box::new(ForeachGen {
            source,
            jv: Value::Null,
            state: init,
            update: Box::new(update),
            extract: Box::new(extract),
            update_gen: None,
            extract_gen: None,
            done: false,
        })
    }
}

impl Iterator for ForeachGen {
    type Item = JResult<Value>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        loop {
            if let Some(eg) = &mut self.extract_gen {
                match eg.next() {
                    Some(Ok(v)) => return Some(Ok(v)),
                    Some(Err(e)) => {
                        self.done = true;
                        return Some(Err(e));
                    }
                    None => self.extract_gen = None,
                }
                continue;
            }
            if let Some(ug) = &mut self.update_gen {
                match ug.next() {
                    Some(Ok(v)) => {
                        self.state = v;
                        self.extract_gen = Some((self.extract)(&self.jv, &self.state));
                    }
                    Some(Err(e)) => {
                        self.done = true;
                        return Some(Err(e));
                    }
                    None => self.update_gen = None,
                }
                continue;
            }
            match self.source.next() {
                Some(Ok(jv)) => {
                    self.jv = jv;
                    self.update_gen = Some((self.update)(&self.jv, &self.state));
                }
                Some(Err(e)) => {
                    self.done = true;
                    return Some(Err(e));
                }
                None => {
                    self.done = true;
                    return None;
                }
            }
        }
    }
}
