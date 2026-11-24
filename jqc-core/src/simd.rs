//! Explicit SIMD-accelerated builtins, called from generated code only
//! when `jq!(codegen = "simd", ...)` selects `SimdCodegen`
//! (`jqc-compiler/src/codegen.rs`) instead of the default
//! `TreeWalkCodegen` -- everything else about the two backends' output is
//! identical; this module is the entire difference.
//!
//! ASCII case-folding (`ascii_downcase`/`ascii_upcase`) is the flagship
//! case: a pure byte-wise transform (every byte's output depends only on
//! that byte), the textbook shape SIMD helps with. It's also safe to
//! apply byte-wise to UTF-8 text without any encoding-awareness at all --
//! every byte of a multi-byte UTF-8 sequence (both the leading byte,
//! 0xC0-0xFF, and continuation bytes, 0x80-0xBF) falls outside the ASCII
//! letter ranges (0x41-0x5A, 0x61-0x7A) this folds, the same reason
//! `str::to_ascii_lowercase` itself is byte-wise safe.
//!
//! x86_64 only (`SSE2`, part of that target's own baseline, so no
//! `is_x86_feature_detected!` runtime check is needed, plus an AVX2 fast
//! path gated on one at runtime): other architectures fall back to the
//! plain scalar transform, correctness-equivalent but with no SIMD
//! speedup.
//!
//! `contains` (on two strings) uses `memchr::memmem` -- an *established*
//! crate, not hand-rolled intrinsics, per the explicit steer to prefer
//! one where it provides genuine value over reinventing it: unlike
//! ASCII case-folding (see above), substring search is a case where
//! `memchr::memmem`'s own tuning (it ships Rust's standard substring-
//! search building block, used by `ripgrep` and much of the ecosystem)
//! is a much higher bar than hand-written code is likely to clear.
//!
//! See the `simd-codegen` todo-report for the actual measurements behind
//! both of the above -- including the one that *didn't* pan out
//! (`ascii_downcase`/`ascii_upcase` hand-rolled SIMD, kept anyway as an
//! honestly-reported negative result, not silently dropped).

use crate::error::JResult;
use crate::helpers::{as_str, eq_values, type_name, MAX_STRUCTURAL_DEPTH};
use serde_json::Value;

/// As `jqc-core::contains_value`, except a `(String, String)` pair is
/// tested via `memchr::memmem::find` instead of `str::contains`.
/// Object/array recursion and the kind-mismatch check are identical --
/// duplicated here (rather than, say, making `contains_at` generic over
/// "how to compare two strings") because the whole duplicated surface is
/// ~15 lines, smaller than the generalization would need to be, and
/// because this function's entire reason to exist is to differ from the
/// original in exactly one `match` arm.
pub fn simd_contains_value(lhs: &Value, rhs: &Value) -> JResult<bool> {
    fn kind(v: &Value) -> u8 {
        match v {
            Value::Null => 0,
            Value::Bool(false) => 1,
            Value::Bool(true) => 2,
            Value::Number(_) => 3,
            Value::String(_) => 4,
            Value::Array(_) => 5,
            Value::Object(_) => 6,
        }
    }
    if kind(lhs) != kind(rhs) {
        return Err(crate::error::JqError::msg(format!(
            "{} ({}) and {} ({}) cannot have their containment checked",
            type_name(lhs),
            lhs,
            type_name(rhs),
            rhs
        )));
    }
    simd_contains_at(lhs, rhs, 0)
}

fn simd_contains_at(lhs: &Value, rhs: &Value, depth: u32) -> JResult<bool> {
    if depth > MAX_STRUCTURAL_DEPTH {
        return Err(crate::error::JqError::msg("Containment check too deep"));
    }
    Ok(match (lhs, rhs) {
        (Value::Object(x), Value::Object(y)) => {
            for (k, yv) in y {
                match x.get(k) {
                    Some(xv) if simd_contains_at(xv, yv, depth + 1)? => {}
                    _ => return Ok(false),
                }
            }
            true
        }
        (Value::Array(x), Value::Array(y)) => {
            for yv in y {
                let mut found = false;
                for xv in x {
                    if simd_contains_at(xv, yv, depth + 1)? {
                        found = true;
                        break;
                    }
                }
                if !found {
                    return Ok(false);
                }
            }
            true
        }
        (Value::String(x), Value::String(y)) => {
            y.is_empty() || memchr::memmem::find(x.as_bytes(), y.as_bytes()).is_some()
        }
        (x, y) => eq_values(x, y),
    })
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use std::arch::x86_64::*;

    /// Folds one 16-byte chunk: bytes in `[range_lo, range_hi]` get
    /// `delta` added (mod 256); every other byte passes through
    /// unchanged. The unsigned range check is the standard "subtract
    /// then unsigned min" trick: `(byte - range_lo) <= width` (computed
    /// mod 256, so any byte below `range_lo` wraps to a large value,
    /// safely outside `width`) tested via SSE2's `_mm_min_epu8` -- one of
    /// the few *unsigned* ops SSE2 has for 8-bit lanes -- instead of two
    /// signed comparisons each needing their own sign-bit-flipped
    /// operand; fewer vector ops, and (confirmed empirically, see the
    /// `simd-codegen` todo-report) measurably faster.
    #[target_feature(enable = "sse2")]
    unsafe fn fold_chunk(chunk: __m128i, range_lo: u8, range_hi: u8, delta: i8) -> __m128i {
        let width = range_hi - range_lo;
        let shifted = _mm_sub_epi8(chunk, _mm_set1_epi8(range_lo as i8));
        let clamped = _mm_min_epu8(shifted, _mm_set1_epi8(width as i8));
        let in_range = _mm_cmpeq_epi8(shifted, clamped);
        let add_mask = _mm_and_si128(in_range, _mm_set1_epi8(delta));
        _mm_add_epi8(chunk, add_mask)
    }

    /// As [`fold_chunk`], but 32 lanes (`__m256i`) via AVX2 -- not part of
    /// x86_64's own baseline (unlike SSE2), so only called after an
    /// `is_x86_feature_detected!("avx2")` check.
    #[target_feature(enable = "avx2")]
    unsafe fn fold_chunk_avx2(chunk: __m256i, range_lo: u8, range_hi: u8, delta: i8) -> __m256i {
        let width = range_hi - range_lo;
        let shifted = _mm256_sub_epi8(chunk, _mm256_set1_epi8(range_lo as i8));
        let clamped = _mm256_min_epu8(shifted, _mm256_set1_epi8(width as i8));
        let in_range = _mm256_cmpeq_epi8(shifted, clamped);
        let add_mask = _mm256_and_si256(in_range, _mm256_set1_epi8(delta));
        _mm256_add_epi8(chunk, add_mask)
    }

    /// Applies [`fold_chunk`] across `bytes` in place, 16 bytes at a time,
    /// with a scalar tail for the remainder (`bytes.len() % 16`).
    #[target_feature(enable = "sse2")]
    unsafe fn fold_bytes_sse2(bytes: &mut [u8], range_lo: u8, range_hi: u8, delta: i8) {
        let (chunks, remainder) = bytes.as_chunks_mut::<16>();
        for chunk in chunks {
            let v = _mm_loadu_si128(chunk.as_ptr() as *const __m128i);
            let folded = fold_chunk(v, range_lo, range_hi, delta);
            _mm_storeu_si128(chunk.as_mut_ptr() as *mut __m128i, folded);
        }
        for b in remainder {
            if *b >= range_lo && *b <= range_hi {
                *b = b.wrapping_add(delta as u8);
            }
        }
    }

    /// As [`fold_bytes_sse2`], 32 bytes at a time via AVX2, with an
    /// SSE2-chunk-then-scalar tail for the remainder.
    #[target_feature(enable = "avx2")]
    unsafe fn fold_bytes_avx2(bytes: &mut [u8], range_lo: u8, range_hi: u8, delta: i8) {
        let (chunks, remainder) = bytes.as_chunks_mut::<32>();
        for chunk in chunks {
            let v = _mm256_loadu_si256(chunk.as_ptr() as *const __m256i);
            let folded = fold_chunk_avx2(v, range_lo, range_hi, delta);
            _mm256_storeu_si256(chunk.as_mut_ptr() as *mut __m256i, folded);
        }
        fold_bytes_sse2(remainder, range_lo, range_hi, delta);
    }

    fn fold_bytes(bytes: &mut [u8], range_lo: u8, range_hi: u8, delta: i8) {
        if is_x86_feature_detected!("avx2") {
            // SAFETY: feature detected just above.
            unsafe { fold_bytes_avx2(bytes, range_lo, range_hi, delta) }
        } else {
            // SAFETY: SSE2 is part of x86_64's own baseline ABI, always
            // present -- no runtime check needed here, unlike AVX2 above.
            unsafe { fold_bytes_sse2(bytes, range_lo, range_hi, delta) }
        }
    }

    pub(super) fn ascii_downcase(bytes: &mut [u8]) {
        fold_bytes(bytes, b'A', b'Z', 32)
    }

    pub(super) fn ascii_upcase(bytes: &mut [u8]) {
        fold_bytes(bytes, b'a', b'z', -32)
    }
}

#[cfg(not(target_arch = "x86_64"))]
mod x86 {
    pub(super) fn ascii_downcase(bytes: &mut [u8]) {
        bytes.make_ascii_lowercase();
    }
    pub(super) fn ascii_upcase(bytes: &mut [u8]) {
        bytes.make_ascii_uppercase();
    }
}

pub fn simd_ascii_downcase(v: &Value) -> JResult<Value> {
    let mut bytes = as_str(v)?.as_bytes().to_vec();
    x86::ascii_downcase(&mut bytes);
    // SAFETY: as established in this module's doc comment, an ASCII-
    // range-only byte transform can't turn valid UTF-8 (`bytes` came
    // from a `&str`) into invalid UTF-8 -- skipping `String::from_utf8`'s
    // own (O(n), redundant here) re-validation scan is exactly what
    // `str::to_ascii_lowercase` itself does internally, for the same
    // reason; skipping it is why this module exists at all; the earlier
    // version's validating `String::from_utf8(...).expect(...)` was
    // measured *slower* than the plain scalar path it was meant to beat,
    // entirely because of this redundant scan -- see the simd-codegen
    // todo-report.
    Ok(Value::String(unsafe { String::from_utf8_unchecked(bytes) }))
}

pub fn simd_ascii_upcase(v: &Value) -> JResult<Value> {
    let mut bytes = as_str(v)?.as_bytes().to_vec();
    x86::ascii_upcase(&mut bytes);
    // SAFETY: see `simd_ascii_downcase` above.
    Ok(Value::String(unsafe { String::from_utf8_unchecked(bytes) }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helpers::{ascii_downcase, ascii_upcase, contains_value};
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn downcase_matches_scalar(s: String) {
            let v = Value::String(s);
            prop_assert_eq!(simd_ascii_downcase(&v).unwrap(), ascii_downcase(&v).unwrap());
        }

        #[test]
        fn upcase_matches_scalar(s: String) {
            let v = Value::String(s);
            prop_assert_eq!(simd_ascii_upcase(&v).unwrap(), ascii_upcase(&v).unwrap());
        }

        #[test]
        fn downcase_matches_scalar_long(s in ".{0,200}") {
            let v = Value::String(s);
            prop_assert_eq!(simd_ascii_downcase(&v).unwrap(), ascii_downcase(&v).unwrap());
        }
    }

    #[test]
    fn boundary_bytes() {
        // One byte below/above each ASCII letter range, and the range
        // edges themselves -- the cases most likely to expose an
        // off-by-one in the SIMD range comparison.
        let s = "@AZ[`az{";
        let v = Value::String(s.to_string());
        assert_eq!(
            simd_ascii_downcase(&v).unwrap(),
            ascii_downcase(&v).unwrap()
        );
        assert_eq!(simd_ascii_upcase(&v).unwrap(), ascii_upcase(&v).unwrap());
    }

    #[test]
    fn exactly_one_sse2_chunk_and_one_byte_over() {
        for len in [15, 16, 17, 31, 32, 33, 63, 64, 65] {
            let s: String = (0..len).map(|i| (b'a' + (i % 26) as u8) as char).collect();
            let v = Value::String(s);
            assert_eq!(
                simd_ascii_downcase(&v).unwrap(),
                ascii_downcase(&v).unwrap()
            );
            assert_eq!(simd_ascii_upcase(&v).unwrap(), ascii_upcase(&v).unwrap());
        }
    }

    #[test]
    fn non_string_errors_like_scalar() {
        let v = Value::Bool(true);
        assert!(simd_ascii_downcase(&v).is_err());
        assert_eq!(
            simd_ascii_downcase(&v).unwrap_err().to_string(),
            ascii_downcase(&v).unwrap_err().to_string()
        );
    }

    proptest! {
        #[test]
        fn contains_matches_scalar_strings(haystack: String, needle: String) {
            let (h, n) = (Value::String(haystack), Value::String(needle));
            prop_assert_eq!(
                simd_contains_value(&h, &n).unwrap(),
                contains_value(&h, &n).unwrap()
            );
        }

        #[test]
        fn contains_matches_scalar_arrays(items: Vec<String>, needle: String) {
            let h = Value::Array(items.into_iter().map(Value::String).collect());
            let n = Value::Array(vec![Value::String(needle)]);
            prop_assert_eq!(
                simd_contains_value(&h, &n).unwrap(),
                contains_value(&h, &n).unwrap()
            );
        }
    }

    #[test]
    fn contains_kind_mismatch_errors_like_scalar() {
        let (h, n) = (Value::Bool(true), Value::Null);
        assert!(simd_contains_value(&h, &n).is_err());
        assert!(contains_value(&h, &n).is_err());
    }

    #[test]
    fn contains_empty_needle_is_always_true() {
        let h = Value::String("anything".to_string());
        let n = Value::String(String::new());
        assert!(simd_contains_value(&h, &n).unwrap());
        assert!(contains_value(&h, &n).unwrap());
    }
}
