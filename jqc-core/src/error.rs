use std::fmt;

use serde_json::Value;

/// Runtime error produced while evaluating a compiled jq filter.
///
/// This mirrors jq's own error model: errors carry a JSON value (usually a
/// string) and can be caught with `try`/`catch` or `?`. jq's generators (and
/// `jqc`'s own compiled ones -- see `jqc-core::gen`) are lazy and stream
/// their outputs one at a time, so a `try`/`?` only ever suppresses the
/// *error signal*; any values the failing generator had already produced
/// before it raised were already forwarded downstream as they were pulled,
/// and stay in the result on their own, with nothing extra to carry here.
#[derive(Debug, Clone, PartialEq)]
pub struct JqError {
    value: Value,
    /// `Some(name)` if this isn't really an error at all, but a `break
    /// $name` unwinding to its matching `label $name` -- which must *not*
    /// be caught by an intervening `try`/`catch`/`?` the way a real error
    /// is (only the matching `label` may catch it), even though it's
    /// threaded through the exact same `Result` machinery.
    pub break_label: Option<String>,
}

impl JqError {
    pub fn msg(s: impl Into<String>) -> Self {
        JqError {
            value: Value::String(s.into()),
            break_label: None,
        }
    }

    pub fn value(v: Value) -> Self {
        JqError {
            value: v,
            break_label: None,
        }
    }

    pub fn brk(label: impl Into<String>) -> Self {
        JqError {
            value: Value::Null,
            break_label: Some(label.into()),
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

impl std::error::Error for JqError {}

impl From<serde_json::Error> for JqError {
    fn from(e: serde_json::Error) -> Self {
        JqError::msg(e.to_string())
    }
}

pub type JResult<T> = Result<T, JqError>;
