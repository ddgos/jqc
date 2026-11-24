use std::fmt;

use serde_json::Value;

/// Runtime error produced while evaluating a compiled jq filter.
///
/// This mirrors jq's own error model: errors carry a JSON value (usually a
/// string) and can be caught with `try`/`catch` or `?`. jq's generators are
/// lazy and stream their outputs one at a time, so a `try`/`?` only ever
/// suppresses the *error signal* -- any values the failing generator had
/// already produced before it raised were already emitted downstream and
/// stay in the result. `jqc`'s codegen instead builds each generator's
/// output as a plain `Vec` and only decides success/failure once the whole
/// node is done, so that "already emitted" prefix has to be carried
/// explicitly: every combinator that accumulates a `Vec<Value>` across
/// multiple sub-evaluations threads its accumulator through [`bubble`]/
/// [`bubble1`], which stash it here (`emitted`) on the way out when a
/// sub-evaluation fails, so the nearest enclosing `try`/`?` can recover it.
#[derive(Debug, Clone, PartialEq)]
pub struct JqError {
    value: Value,
    /// Outputs already produced by the generator chain leading to this
    /// error, in order, before the error occurred. Empty for errors raised
    /// by atomic (non-generator) helpers.
    pub emitted: Vec<Value>,
}

impl JqError {
    pub fn msg(s: impl Into<String>) -> Self {
        JqError {
            value: Value::String(s.into()),
            emitted: Vec::new(),
        }
    }

    pub fn value(v: Value) -> Self {
        JqError {
            value: v,
            emitted: Vec::new(),
        }
    }

    pub fn into_value(self) -> Value {
        self.value
    }
}

impl fmt::Display for JqError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.value {
            Value::String(s) => write!(f, "{s}"),
            other => write!(f, "{other}"),
        }
    }
}

/// Folds one more generator step's result into `out` (the accumulator
/// built so far by the enclosing combinator), in place. On success,
/// extends `out` and returns `Ok(())`. On failure, this is the *final*
/// merge point for that failure -- it moves `out`'s current content
/// (everything accumulated before this step) and the failing step's own
/// `emitted` (whatever *it* had already produced) both into the
/// propagated error's `emitted`, in that order, and leaves `out` empty.
/// Callers must propagate the error immediately (`?` right after this
/// call) rather than going on to accumulate more into `out` -- nothing
/// downstream of this call can recover what's now sitting in `e.emitted`
/// instead. [`partial_for`]'s own final merge follows the same rule, so
/// composing `bubble` calls across nested [`partial_for`] levels (what
/// jqc-compiler's codegen does for nested pass-through expressions) still
/// ends up with exactly one correctly-ordered `emitted` by the time the
/// error reaches a `try`/`?`. See [`JqError`] for why this is needed.
pub fn bubble(out: &mut Vec<Value>, next: JResult<Vec<Value>>) -> JResult<()> {
    match next {
        Ok(v) => {
            out.extend(v);
            Ok(())
        }
        Err(mut e) => {
            let mut taken = std::mem::take(out);
            taken.append(&mut e.emitted);
            e.emitted = taken;
            Err(e)
        }
    }
}

/// Like [`bubble`], but for combinators that push a single scalar result
/// (rather than extending by a sub-generator's `Vec`) -- e.g. a binop or
/// unary helper that returns `JResult<Value>`.
pub fn bubble1(out: &mut Vec<Value>, next: JResult<Value>) -> JResult<()> {
    match next {
        Ok(v) => {
            out.push(v);
            Ok(())
        }
        Err(mut e) => {
            let mut taken = std::mem::take(out);
            taken.append(&mut e.emitted);
            e.emitted = taken;
            Err(e)
        }
    }
}

/// Runs `body` once per element of `sub`'s output, writing results into a
/// fresh accumulator that `body` extends/pushes to directly (typically via
/// [`bubble`]/[`bubble1`]), and returns that accumulator.
///
/// This is the runtime half of pass-through nodes in `jqc-compiler`'s
/// codegen (`Pipe`, `Field`, `BinOp`, …) -- where each element a
/// sub-expression produces directly feeds this node's own output -- as
/// opposed to *collector* nodes (`[...]`, string interpolation, …) where
/// elements are folded into one combined value and never individually
/// observable. A plain `fn`, not inlined-per-call-site codegen: nesting
/// `partial_for` calls (one per level of pass-through nesting in the
/// source expression) composes additively in *generated code size* this
/// way, rather than each level re-inlining this whole loop/merge shape as
/// raw tokens -- which is what every use of this used to do, until a real
/// script with a few levels of ordinary nested calls (not even deeply
/// recursive) was measured expanding to *8.7 million characters* of
/// generated Rust for one small function, overwhelming LLVM well before
/// any jq-level cost. See the `codegen-size-blowup` todo-report.
///
/// If `sub` itself fails partway through generating elements, `body` still
/// runs over whatever elements it *did* produce (its own `emitted`) before
/// re-raising with the final accumulator attached, matching jq's
/// lazy-generator semantics: a `try`/`?` around this node sees everything
/// already produced, from any level, before the error.
pub fn partial_for<F>(sub: JResult<Vec<Value>>, mut body: F) -> JResult<Vec<Value>>
where
    F: FnMut(&mut Vec<Value>, Value) -> JResult<()>,
{
    let mut out = Vec::new();
    let (items, mut pending) = match sub {
        Ok(items) => (items, None),
        Err(mut e) => {
            let items = std::mem::take(&mut e.emitted);
            (items, Some(e))
        }
    };
    for item in items {
        if let Err(e) = body(&mut out, item) {
            pending = Some(e);
            break;
        }
    }
    match pending {
        None => Ok(out),
        Some(mut e) => {
            // `out` may already be empty here: if `body`'s own failure
            // went through `bubble`/`bubble1`, that already moved `out`'s
            // content into `e.emitted` directly (per their contract) and
            // left `out` empty. If `body` instead failed some other way
            // (e.g. a bare `return Err(e)` for an atomic, non-generator
            // helper), `out` still holds everything and `e.emitted` is
            // whatever `e` already carried (typically empty). Appending
            // `out` first then taking it back into `e.emitted` is correct
            // either way, and a no-op lookup in the already-merged case.
            out.append(&mut e.emitted);
            e.emitted = out;
            Err(e)
        }
    }
}

impl std::error::Error for JqError {}

impl From<serde_json::Error> for JqError {
    fn from(e: serde_json::Error) -> Self {
        JqError::msg(e.to_string())
    }
}

pub type JResult<T> = Result<T, JqError>;
