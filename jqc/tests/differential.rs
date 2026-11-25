//! Differential tests: every filter here is run twice -- once through the
//! `jq!`-compiled closure, once by shelling out to the real `jq` binary on
//! the same input -- and the outputs must match exactly. This is the
//! primary correctness suite for the compiler (lexer -> parser -> lowering
//! -> codegen), not just a unit-test smoke check.
//!
//! Requires a `jq` binary on PATH (the Nix dev shell provides one).

use jqc::core::Value;
use jqc_test_support::run_real_jq;

macro_rules! difftest {
    ($name:ident, $src:literal, $input:literal) => {
        #[test]
        fn $name() {
            let compiled = jqc::jq!($src);
            let input: Value = serde_json::from_str($input).expect("test input must be valid JSON");
            let got = compiled(&input)
                .unwrap_or_else(|e| panic!("compiled filter {:?} errored: {e}", $src));
            let want = run_real_jq($src, $input);
            assert_eq!(got, want, "filter {:?} on input {}", $src, $input);
        }
    };
}

difftest!(identity, ".", r#"{"a":1}"#);
difftest!(field, ".a", r#"{"a":1,"b":2}"#);
difftest!(nested_field, ".a.b", r#"{"a":{"b":42}}"#);
difftest!(field_missing, ".a.b", r#"{"a":{}}"#);
difftest!(field_on_null, ".a.b.c", "null");
difftest!(optional_field_ok, ".a?", r#"{"a":1}"#);
difftest!(optional_field_err, ".a?", r#"[1,2,3]"#);
difftest!(index_array, ".[1]", "[10,20,30]");
difftest!(index_negative, ".[-1]", "[10,20,30]");
difftest!(index_object_str, r#".["a"]"#, r#"{"a":5}"#);
difftest!(index_multivalued, "[([10,20],[30,40])[0,1]]", "null");
difftest!(slice_basic, ".[1:3]", "[0,1,2,3,4]");
difftest!(slice_open_start, ".[:2]", "[0,1,2,3,4]");
difftest!(slice_open_end, ".[2:]", "[0,1,2,3,4]");
difftest!(slice_string, ".[1:3]", r#""abcdef""#);
difftest!(
    slice_multivalued,
    "[([1,2,3,4],[10,20,30,40])[(0,1):(2,3)]]",
    "null"
);
difftest!(iterate_array, ".[]", "[1,2,3]");
difftest!(iterate_object, ".[]", r#"{"a":1,"b":2}"#);
difftest!(pipe_basic, ".a | .b", r#"{"a":{"b":7}}"#);
difftest!(
    pipe_iterate,
    ".items[] | .x",
    r#"{"items":[{"x":1},{"x":2}]}"#
);
difftest!(comma, ".a, .b", r#"{"a":1,"b":2}"#);
difftest!(comma_cartesian, "(1,2) + (10,20)", "null");
difftest!(array_construct, "[.a, .b]", r#"{"a":1,"b":2}"#);
difftest!(array_construct_iterate, "[.[] | . * 2]", "[1,2,3]");
difftest!(object_construct, "{a: .x, b: .y}", r#"{"x":1,"y":2}"#);
difftest!(object_shorthand, "{x, y}", r#"{"x":1,"y":2,"z":3}"#);
difftest!(object_computed_key, "{(.k): .v}", r#"{"k":"hello","v":1}"#);
difftest!(add_numbers, ".a + .b", r#"{"a":1,"b":2.5}"#);
difftest!(add_whole_number_format, ".a * 2", r#"{"a":3}"#);
difftest!(add_strings, ".a + .b", r#"{"a":"foo","b":"bar"}"#);
difftest!(add_arrays, ".a + .b", r#"{"a":[1,2],"b":[3,4]}"#);
difftest!(add_objects, ".a + .b", r#"{"a":{"x":1},"b":{"x":2,"y":3}}"#);
difftest!(add_null, ".a + .b", r#"{"a":null,"b":5}"#);
difftest!(sub_numbers, ".a - .b", r#"{"a":10,"b":3}"#);
difftest!(sub_arrays, ".a - .b", r#"{"a":[1,2,3],"b":[2]}"#);
difftest!(mul_numbers, ".a * .b", r#"{"a":4,"b":2.5}"#);
difftest!(mul_string_repeat, ".a * .b", r#"{"a":"ab","b":3}"#);
difftest!(div_numbers, ".a / .b", r#"{"a":7,"b":2}"#);
difftest!(div_string_split, ".a / .b", r#"{"a":"a,b,c","b":","}"#);
difftest!(modulo, ".a % .b", r#"{"a":10,"b":3}"#);
difftest!(neg_number, "-.a", r#"{"a":5}"#);
difftest!(eq, ".a == .b", r#"{"a":1,"b":1}"#);
difftest!(ne, ".a != .b", r#"{"a":1,"b":2}"#);
difftest!(lt, ".a < .b", r#"{"a":1,"b":2}"#);
difftest!(
    cmp_mixed_types,
    "[1, \"a\", null, true, [1], {}] | sort",
    "null"
);
difftest!(and_short_circuit, ".a and .b", r#"{"a":false,"b":null}"#);
difftest!(or_short_circuit, ".a or .b", r#"{"a":true,"b":null}"#);
difftest!(not_builtin, ".a | not", r#"{"a":false}"#);
difftest!(alt_lhs_truthy, "1 // 2", "null");
difftest!(alt_fallback_null, "null // 2", "null");
difftest!(alt_fallback_false, "false // 2", "null");
difftest!(alt_fallback_empty, "empty // 2", "null");
difftest!(alt_zero_is_truthy, "0 // 99", "null");
difftest!(
    alt_filters_falsy_multi,
    "[(false, 1, null, 2) // 99]",
    "null"
);
difftest!(alt_all_falsy_multi, "[(false, null) // 99]", "null");
difftest!(alt_rhs_multi, "[(false, null) // (1, 2, 3)]", "null");
difftest!(alt_precedence_comma, "[false, 1 // 2]", "null");
difftest!(alt_precedence_or, "false // (true and false)", "null");
difftest!(alt_precedence_cmp, "null // 1 == 1", "null");
difftest!(alt_as_binding, "false // 5 as $x | $x + 1", "null");
difftest!(
    alt_error_not_caught,
    "try ((error) // 99) catch \"caught\"",
    "null"
);
difftest!(
    alt_partial_before_error,
    "[try ((1, 2, error) // 99) catch \"c\"]",
    "null"
);
difftest!(
    if_then_else,
    "if .a > 0 then \"pos\" else \"nonpos\" end",
    r#"{"a":5}"#
);
difftest!(
    if_elif,
    "if .a == 1 then \"one\" elif .a == 2 then \"two\" else \"other\" end",
    r#"{"a":2}"#
);
difftest!(if_no_else, "if .a then 1 end", r#"{"a":false}"#);
difftest!(try_catch, "try error(\"boom\") catch .", r#"null"#);
difftest!(try_no_catch, "[try error, 1]", r#"null"#);
difftest!(select_basic, "[.[] | select(. > 2)]", "[1,2,3,4]");
difftest!(map_basic, "map(. * 10)", "[1,2,3]");
difftest!(map_select, "map(select(. % 2 == 0))", "[1,2,3,4,5,6]");
difftest!(values_builtin, "[.[] | values]", "[1,null,2,null,3]");
difftest!(has_array, ".[1] | has(\"x\")", r#"[{"x":1},{"y":1}]"#);
difftest!(has_builtin, "has(\"a\")", r#"{"a":1}"#);
difftest!(in_builtin, ".k | in({\"a\":1,\"b\":2})", r#"{"k":"a"}"#);
difftest!(any_basic, "any", "[false,false,true]");
difftest!(any_cond, "any(. > 3)", "[1,2,3]");
difftest!(all_basic, "all", "[true,true,true]");
difftest!(all_cond, "all(. > 0)", "[1,2,-3]");
difftest!(recurse_default, "[..]", r#"{"a":[1,2]}"#);
difftest!(recurse_filter, "[recurse(.a?)]", r#"{"a":{"a":{"a":1}}}"#);
difftest!(reduce_sum, "reduce .[] as $x (0; . + $x)", "[1,2,3,4]");
difftest!(
    foreach_running_sum,
    "[foreach .[] as $x (0; . + $x)]",
    "[1,2,3,4]"
);
difftest!(
    foreach_extract,
    "[foreach .[] as $x (0; . + $x; . * 10)]",
    "[1,2,3]"
);
difftest!(sort_basic, "sort", "[3,1,2]");
difftest!(sort_by_field, "sort_by(.a)", r#"[{"a":3},{"a":1},{"a":2}]"#);
difftest!(
    sort_by_multi,
    "sort_by(.a, .b)",
    r#"[{"a":1,"b":2},{"a":1,"b":1}]"#
);
difftest!(
    group_by_field,
    "group_by(.a)",
    r#"[{"a":1},{"a":2},{"a":1}]"#
);
difftest!(unique_basic, "unique", "[3,1,2,1,3]");
difftest!(
    unique_by_field,
    "unique_by(.a)",
    r#"[{"a":1,"b":1},{"a":1,"b":2},{"a":2,"b":3}]"#
);
difftest!(min_basic, "min", "[3,1,2]");
difftest!(max_basic, "max", "[3,1,2]");
difftest!(min_by_field, "min_by(.a)", r#"[{"a":3},{"a":1},{"a":2}]"#);
difftest!(max_by_field, "max_by(.a)", r#"[{"a":3},{"a":1},{"a":2}]"#);
difftest!(length_array, "length", "[1,2,3]");
difftest!(length_string, "length", r#""hello""#);
difftest!(length_null, "length", "null");
difftest!(keys_basic, "keys", r#"{"b":1,"a":2}"#);
difftest!(keys_unsorted_basic, "keys_unsorted", r#"{"b":1,"a":2}"#);
difftest!(to_entries_object, "to_entries", r#"{"b":1,"a":2}"#);
difftest!(to_entries_array, "to_entries", r#"["x","y"]"#);
difftest!(
    from_entries_basic,
    "from_entries",
    r#"[{"key":"a","value":1},{"key":"b","value":2}]"#
);
difftest!(from_entries_empty, "from_entries", "[]");
difftest!(
    from_entries_name_variants,
    "from_entries",
    r#"[{"name":"a","value":1},{"Key":"b","Value":2}]"#
);
difftest!(
    with_entries_basic,
    "with_entries({key, value: (.value * 2)})",
    r#"{"a":1,"b":2}"#
);
difftest!(type_builtin, "[.[] | type]", r#"[1,"a",null,true,[1],{}]"#);
difftest!(add_builtin, "add", "[1,2,3]");
difftest!(
    add_object_values,
    // `add`'s real jq definition (`reduce .[] as $x (null; . + $x)`)
    // iterates object *values* the same as array elements -- a real bug
    // found by fuzzing: jqc's `add` previously required `as_array`
    // outright, erroring "object is not an array" on any object input.
    "add",
    r#"{"a":1,"b":2}"#
);
difftest!(add_empty_object, "add", "{}");
difftest!(
    sqrt_negative_is_null,
    // Real jq's `sqrt(-1)` is `null`, not an error and not `0` -- a real
    // bug found by fuzzing: `number_from_f64` fell back to the number
    // `0` for any non-finite float (`Number::from_f64` rejects NaN/
    // infinity, and the old fallback didn't distinguish "this must
    // become `null`" from "just pick something"), silently producing a
    // wrong numeric answer instead of jq's own null-for-NaN behavior.
    "sqrt",
    "-1"
);
difftest!(
    arithmetic_overflow_to_infinity_clamps_to_f64_max,
    // `1e308 * 10` overflows f64 arithmetic to +infinity -- real jq
    // clamps that to `f64::MAX` on output (`1.7976931348623157e+308`),
    // not literal `Infinity` (which JSON can't encode) and not `null`
    // (unlike NaN) -- confirmed directly against real jq.
    "[1e308 * 10, -1e308 * 10]",
    "null"
);
difftest!(
    tonumber_nan_and_infinity_strings,
    // Same number_from_f64 fix as sqrt_negative_is_null/
    // infinite_builtin_clamps_to_f64_max, reached through a different
    // path: Rust's f64 parser accepts "nan"/"infinity" same as real jq's
    // own number parser does.
    r#"[("nan" | tonumber), ("infinity" | tonumber)]"#,
    "null"
);
difftest!(reverse_array, "reverse", "[1,2,3]");
difftest!(range_1, "[range(5)]", "null");
difftest!(range_2, "[range(2;5)]", "null");
difftest!(range_3, "[range(0;10;3)]", "null");
difftest!(first_zero_arg, "first", "[1,2,3]");
difftest!(last_zero_arg, "last", "[1,2,3]");
difftest!(first_one_arg, "first(.[] | select(. > 1))", "[1,2,3]");
difftest!(last_one_arg, "last(.[] | select(. > 1))", "[1,2,3]");
difftest!(tostring_num, "tostring", "42");
difftest!(tonumber_str, "tonumber", r#""42""#);
difftest!(floor_num, "floor", "3.7");
difftest!(ceil_num, "ceil", "3.2");
difftest!(sqrt_num, "sqrt", "9");
difftest!(startswith_basic, r#"startswith("he")"#, r#""hello""#);
difftest!(endswith_basic, r#"endswith("lo")"#, r#""hello""#);
difftest!(contains_str, r#"contains("ell")"#, r#""hello""#);
difftest!(ltrimstr_basic, r#"ltrimstr("he")"#, r#""hello""#);
difftest!(rtrimstr_basic, r#"rtrimstr("lo")"#, r#""hello""#);
difftest!(join_basic, r#"join(",")"#, r#"["a","b","c"]"#);
difftest!(split_basic, r#"split(",")"#, r#""a,b,c""#);
difftest!(explode_basic, "explode", r#""abc""#);
difftest!(implode_basic, "implode", "[97,98,99]");
difftest!(ascii_case, "[ascii_upcase, ascii_downcase]", r#""AbC""#);
difftest!(string_interp, r#""hello \(.name)!""#, r#"{"name":"world"}"#);
difftest!(string_interp_multi, r#""\(.a)-\(.b)""#, r#"{"a":1,"b":2}"#);
difftest!(user_def_simple, "def double: . * 2; double", "21");
difftest!(user_def_with_param, "def addn(n): . + n; addn(5)", "10");
difftest!(
    user_def_filter_param,
    "def sq(f): f * f; sq(.a)",
    r#"{"a":4}"#
);
difftest!(
    user_def_recursive,
    "def fact: if . <= 1 then 1 else (. - 1 | fact) * . end; fact",
    "5"
);
difftest!(while_builtin, "[while(. < 100; . * 2)]", "1");
difftest!(
    label_break_basic,
    // `break` ends the whole enclosing `label` scope immediately -- 4
    // and 5 are never reached once $x == 3 triggers it.
    "[label $out | (1,2,3,4,5) as $x | if $x == 3 then break $out else $x end]",
    "null"
);
difftest!(
    label_break_caught_by_try,
    // Confirmed against real jq: a `try`/`catch` in between a `break`
    // and its matching `label` catches it exactly like any other
    // error -- it's not specially exempted.
    "label $out | [(1,2,3,4,5) as $x | try (if $x == 3 then break $out else $x end) catch \"caught-\\($x)\"]",
    "null"
);
difftest!(
    label_break_skips_non_matching_inner_label,
    "[label $a | (label $b | (1,2,3) as $x | if $x == 2 then break $a else $x end), 99]",
    "null"
);
#[test]
fn overflowing_number_literal_does_not_panic_codegen() {
    // `1e999999999` overflows f64 to infinity at lex time -- a bare
    // float literal can't spell that in generated Rust (`quote`'s
    // `ToTokens` for `f64` asserts `is_finite()`), so this used to panic
    // macro expansion outright. `number_from_f64` clamps infinity to
    // `f64::MAX` (matching real jq's own behavior for an *operation*
    // that overflows, confirmed directly: `jq -n '1e308 * 10'` prints
    // the same `1.7976931348623157e+308`) -- real jq's bare *literal*
    // instead keeps the exact decimal text as typed, printing it back as
    // `1E+999999999` verbatim, a real, accepted divergence given jqc's
    // `Value` is a plain f64 with no literal-text side channel, not
    // worth chasing; this test only guards against the crash, not a
    // `jq`-equivalent result for the *literal* case specifically.
    let filter = jqc::jq!("1e999999999");
    let input = jqc::core::json!(null);
    let got = filter(&input).expect("should compile and run without panicking");
    assert_eq!(got, vec![jqc::core::json!(f64::MAX)]);
}
difftest!(
    nested_def_shadows_outer_same_name_in_its_own_rest,
    // Found via jq's own C test suite (jqc-c-tests): the inner `def g`'s
    // `rest` (`f | g | f`) is lexically inside *its* scope, so `g` there
    // means the inner `.+100` def, not the outer one -- getting this
    // wrong misclassified the (actually non-recursive) outer `g` as
    // recursive and, separately, could rewrite a legitimately-shadowed
    // call into a `RecCall` for an unrelated def, producing genuine
    // infinite recursion (a real stack overflow, not just a wrong
    // answer) until fixed in contains_call/rewrite_call0.
    "def f: . + 1; def g: def g: . + 100; f | g | f; (f | g), g",
    "3"
);
difftest!(limit_basic, "[limit(2; range(0; 5))]", "null");
difftest!(limit_zero, "[limit(0; range(0; 5))]", "null");
difftest!(
    limit_more_than_available,
    "[limit(10; range(0; 3))]",
    "null"
);
difftest!(until_builtin, "until(. >= 100; . * 2)", "1");
difftest!(repeat_error_optional, "[repeat(.*2, error)?]", "1");
difftest!(optional_chain, ".a.b?.c", r#"{"a":1}"#);
difftest!(
    array_of_objects,
    "[.[] | {id: .id, doubled: (.val * 2)}]",
    r#"[{"id":1,"val":2},{"id":2,"val":3}]"#
);
difftest!(
    nested_reduce,
    "[.[] | reduce .[] as $x (0; . + $x)]",
    "[[1,2],[3,4,5]]"
);
difftest!(empty_filter, "[1, empty, 2]", "null");
difftest!(as_binding, ". as $x | $x + 1", "41");
difftest!(
    as_binding_in_pipe,
    ".a as $x | .b | . + $x",
    r#"{"a":1,"b":2}"#
);
difftest!(
    as_binding_array_destructure,
    ". as [$a, $b] | {a: $a, b: $b}",
    "[1,2,3]"
);
difftest!(
    as_binding_array_destructure_short,
    ". as [$a, $b] | {a: $a, b: $b}",
    "[1]"
);
difftest!(
    as_binding_object_destructure,
    ". as {a: $a, b: $b} | $a + $b",
    r#"{"a":1,"b":2,"c":3}"#
);
difftest!(
    as_binding_object_shorthand,
    ". as {$a, $b} | $a + $b",
    r#"{"a":1,"b":2}"#
);
difftest!(
    as_binding_object_missing_key,
    ". as {a: $a} | $a",
    r#"{"b":2}"#
);
difftest!(
    as_binding_computed_key,
    // The computed key `.k` evaluates against the value being
    // destructured (`.foo`), not the outer `.` (whose own "k" field, if
    // any, must be ignored) -- confirmed against real jq.
    ".foo as {(.k): $v} | $v",
    r#"{"foo":{"k":"x","x":99},"k":"wrong"}"#
);
difftest!(
    as_binding_nested_destructure,
    ". as [$a, {b: $b}] | $a + $b",
    "[1,{\"b\":2}]"
);
difftest!(
    as_binding_destructure_in_pipe,
    ".a as [$x, $y] | .b | . + $x + $y",
    r#"{"a":[1,2],"b":10}"#
);

difftest!(
    lazy_limit_over_infinite_while,
    // The headline bug the eager->lazy codegen rewrite fixed: `while(true;.)`
    // never terminates, so any eager evaluation of it (materializing a
    // `Vec<Value>` before `limit` gets to see any of it) stack-overflows.
    // Only works if `while`'s generator is genuinely pulled lazily.
    "[limit(3; while(true;.))]",
    "null"
);
difftest!(lazy_limit_over_repeat, "[limit(3; repeat(true))]", "null");
difftest!(
    lazy_first_over_repeat,
    // Same shape as `lazy_limit_over_repeat` but via `first`, which
    // internally uses its own `limit(1; ...)` -- regression guard for that
    // specific builtin's laziness, not just `limit` itself.
    "first(repeat(1))",
    "null"
);
difftest!(
    lazy_user_foreach_piped_into_limit,
    // Unlike the two cases above, this `foreach` is user-written (not a
    // builtin's internal use of `foreach`) and is piped into `limit` from
    // the *outside* -- proving `ForeachGen` is a real external lazy
    // generator that `limit` can pull from incrementally, not merely
    // internally lazy but collected into a `Vec` before `limit` ever sees
    // it.
    "[limit(3; foreach repeat(1) as $x (0; .+1; $x))]",
    "null"
);
difftest!(
    lazy_any_short_circuits_past_error,
    // `any`/`all` used to be implemented via `reduce`, which (unlike
    // `first`/`limit`) can't short-circuit at all -- it must drain its
    // whole source before returning, so this would hit `error` instead of
    // stopping at the first `true`. Rewritten (matching real jq's own
    // `isempty`/`first`-based definition) to actually short-circuit now
    // that `first` is genuinely lazy.
    "any(true, error; .)",
    "null"
);
difftest!(
    lazy_all_short_circuits_past_error,
    "all(false, error; .)",
    "null"
);
difftest!(
    sort_by_key_referencing_outer_foreach_var,
    // Regression test for a real compile-time bug found by fuzzing this
    // rewrite: `compile_by` (the shared codegen for `sort_by`/`group_by`/
    // `unique_by`/`min_by`/`max_by`) spliced its key filter's compiled
    // code into a runtime `for` loop (once per array element) without
    // isolating it -- fine as long as the key filter was self-contained,
    // but if it referenced an in-scope `$var` from an enclosing
    // `foreach`/`reduce`/`as` (here, `$x`, unused by the key itself but
    // still in scope), the *first* loop iteration's compiled closure
    // captured that outer variable *by move*, leaving nothing for the
    // second iteration to clone from -- a real `E0382` ("use of moved
    // value ... in previous iteration of loop"), not a runtime
    // discrepancy. Fixed by isolating `compile_by`'s key filter the same
    // way `Reduce`'s own per-item loop already isolates its `update`.
    "[foreach (1,2,3) as $x (0; sort_by((2.4|fabs))?)]",
    "null"
);

#[test]
fn optional_only_suppresses_its_own_step() {
    // `.a.b.c?` -- the `?` binds to `.c` only; the error from `.a.b`
    // (indexing the number 1) is not suppressed. Matches real jq 1.8.
    let compiled = jqc::jq!(".a.b.c?");
    let input: Value = serde_json::from_str(r#"{"a":1}"#).unwrap();
    assert!(compiled(&input).is_err());
}
