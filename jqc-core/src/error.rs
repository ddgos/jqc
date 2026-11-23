use std::fmt;

use serde_json::Value;

/// Runtime error produced while evaluating a compiled jq filter.
///
/// This mirrors jq's own error model: errors carry a JSON value (usually a
/// string) and can be caught with `try`/`catch` or `?`.
#[derive(Debug, Clone, PartialEq)]
pub struct JqError(pub Value);

impl JqError {
    pub fn msg(s: impl Into<String>) -> Self {
        JqError(Value::String(s.into()))
    }

    pub fn value(v: Value) -> Self {
        JqError(v)
    }

    pub fn into_value(self) -> Value {
        self.0
    }
}

impl fmt::Display for JqError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
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
