//! Runtime support crate for `jqc`.
//!
//! The `jq!` macro (in `jqc-macros`) compiles a jq filter directly into Rust
//! control flow at macro-expansion time; this crate supplies the small set
//! of value-level operations (type coercion, ordering, builtins) that the
//! generated code calls into, plus the shared [`Value`]/[`JqError`] types.
//! None of this crate parses or interprets jq syntax at runtime.

mod error;
mod helpers;

pub use error::{bubble, bubble1, JResult, JqError};
pub use helpers::*;
pub use serde_json::{json, Map, Number, Value};
