//! Differential tests generated from jq's own manual examples. See
//! `jqc-manual-tests-macros` for how, and `jqc-manual-tests`'s
//! `Cargo.toml` description for why this isn't in `[workspace]
//! default-members`.

use jqc_test_support::run_real_jq;

jqc_manual_tests_macros::generate_manual_tests!(version = "1.8");
