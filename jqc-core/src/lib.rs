//! Runtime support crate for `jqc`.
//!
//! The `jq!` macro (in `jqc-macros`) compiles a jq filter directly into Rust
//! control flow at macro-expansion time; this crate supplies the small set
//! of value-level operations (type coercion, ordering, builtins) that the
//! generated code calls into, plus the shared [`Value`]/[`JqError`] types.
//! None of this crate parses or interprets jq syntax at runtime.

mod error;
mod helpers;
#[cfg(feature = "codegen-simd")]
mod simd;

pub use error::{bubble, bubble1, partial_for, JResult, JqError};
pub use helpers::*;
pub use serde_json::{json, Map, Number, Value};
#[cfg(feature = "codegen-simd")]
pub use simd::{simd_ascii_downcase, simd_ascii_upcase, simd_contains_value};
