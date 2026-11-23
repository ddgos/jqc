//! `jqc`: compile jq filters to native Rust code at build time.
//!
//! ```
//! use jqc::jq;
//! use jqc::core::json;
//!
//! let filter = jq!(".items[] | select(.active) | .name");
//! let input = json!({"items": [{"name": "a", "active": true}, {"name": "b", "active": false}]});
//! assert_eq!(filter(&input).unwrap(), vec![json!("a")]);
//! ```
//!
//! `jq!("<filter>")` parses the filter, inlines/resolves any `def`s, and
//! generates a plain Rust closure for it *at macro-expansion time* -- there
//! is no jq parser or interpreter left in the compiled binary, just the
//! control flow equivalent to the filter you wrote.
//!
//! See the crate README for the supported jq subset and known limitations.

/// Runtime support types and builtin semantics that generated code calls
/// into: [`core::Value`], [`core::JqError`]/[`core::JResult`], and the
/// value-level operations (`add_values`, `cmp_values`, `index_field`, …)
/// backing jq's builtins. Generated code references these as
/// `::jqc::core::...`, so this module must stay named `core`.
pub mod core {
    pub use jqc_core::*;
}

pub use jqc_macros::jq;
