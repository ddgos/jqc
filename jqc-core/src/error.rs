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

/// Folds one more generator step's result into `prior` (the accumulator
/// built so far by the enclosing combinator). On success, extends and
/// returns `Ok`; on failure, prepends `prior` to whatever the failing step
/// had itself already emitted and returns that combined prefix via
/// [`JqError::emitted`], so an enclosing `try`/`?` sees everything produced
/// before the error. See [`JqError`] for why this is needed.
pub fn bubble(mut prior: Vec<Value>, next: JResult<Vec<Value>>) -> JResult<Vec<Value>> {
    match next {
        Ok(v) => {
            prior.extend(v);
            Ok(prior)
        }
        Err(mut e) => {
            prior.append(&mut e.emitted);
            e.emitted = prior;
            Err(e)
        }
    }
}

/// Like [`bubble`], but for combinators that push a single scalar result
/// (rather than extending by a sub-generator's `Vec`) -- e.g. a binop or
/// unary helper that returns `JResult<Value>`.
pub fn bubble1(mut prior: Vec<Value>, next: JResult<Value>) -> JResult<Vec<Value>> {
    match next {
        Ok(v) => {
            prior.push(v);
            Ok(prior)
        }
        Err(mut e) => {
            e.emitted = prior;
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
