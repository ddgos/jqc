//! Shared proptest JSON-value generators -- used by both `fuzz_inputs.rs`
//! (arbitrary input against a curated filter set) and `fuzz_scripts.rs`
//! (arbitrary scripts, which need an input richer than a few fixed
//! examples to actually exercise them meaningfully). Not itself a test
//! binary: `tests/common/mod.rs`, not `tests/common.rs`, is the standard
//! convention Cargo's test auto-discovery skips.

#![allow(dead_code)] // not every consumer uses every generator here.

use jqc::core::Value;
use proptest::prelude::*;

/// Every valid Unicode scalar value is a legal `char`, so collecting an
/// arbitrary number of them is already a correctly-encoded (if sometimes
/// bizarre: control characters, combining marks with nothing to combine
/// with, astral-plane codepoints needing a UTF-16 surrogate pair once
/// re-encoded as JSON's `\uXXXX`) arbitrary UTF-8 string -- no separate
/// "is this valid UTF-8" concern the way there would be generating raw
/// bytes instead.
pub fn arb_utf8_string() -> impl Strategy<Value = String> {
    prop::collection::vec(any::<char>(), 0..8).prop_map(|cs| cs.into_iter().collect())
}

/// Leaf strings mix a small, shrinkable, structurally-boring alphabet
/// (keeps most failures easy to read) with fully arbitrary UTF-8 (the
/// part this strategy exists for -- control characters, combining marks,
/// astral-plane codepoints needing a surrogate pair in JSON's own
/// `\uXXXX` escaping, lookalike whitespace, etc., none of which
/// `[a-z]{0,6}` could ever produce). Object keys are drawn from a fixed
/// small alphabet (`a`/`b`/`c`/`active`/`score`/`items`) matching the
/// field names both consumers' filters actually reference, so a filter
/// indexing `.a`/`.items[]`/etc. hits real data some of the time instead
/// of almost always missing.
pub fn arb_json(depth: u32) -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        (-1000i64..1000i64).prop_map(|n| serde_json::json!(n)),
        "[a-z]{0,6}".prop_map(Value::String),
        arb_utf8_string().prop_map(Value::String),
    ];
    leaf.prop_recursive(depth, 40, 5, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
            prop::collection::btree_map(
                prop_oneof![
                    Just("a".to_string()),
                    Just("b".to_string()),
                    Just("c".to_string()),
                    Just("active".to_string()),
                    Just("score".to_string()),
                    Just("items".to_string()),
                ],
                inner,
                0..4
            )
            .prop_map(|m| Value::Object(m.into_iter().collect())),
        ]
    })
}

/// As [`arb_json`], but for filters that specifically want to exercise
/// *string* input (`explode`, `split`, …): strings dominate the leaf
/// distribution instead of being one option among five, so a filter
/// indexing straight to `.a`/`.b` actually hits adversarial UTF-8 most of
/// the time rather than rarely.
pub fn arb_json_stringy(depth: u32) -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        1 => Just(Value::Null),
        1 => any::<bool>().prop_map(Value::Bool),
        1 => (-1000i64..1000i64).prop_map(|n| serde_json::json!(n)),
        4 => arb_utf8_string().prop_map(Value::String),
    ];
    leaf.prop_recursive(depth, 40, 5, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
            prop::collection::btree_map(
                prop_oneof![
                    Just("a".to_string()),
                    Just("b".to_string()),
                    Just("c".to_string())
                ],
                inner,
                0..4
            )
            .prop_map(|m| Value::Object(m.into_iter().collect())),
        ]
    })
}
