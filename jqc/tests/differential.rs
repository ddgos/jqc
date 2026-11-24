//! Differential tests: every filter here is run twice -- once through the
//! `jq!`-compiled closure, once by shelling out to the real `jq` binary on
//! the same input -- and the outputs must match exactly. This is the
//! primary correctness suite for the compiler (lexer -> parser -> lowering
//! -> codegen), not just a unit-test smoke check.
//!
//! Requires a `jq` binary on PATH (the Nix dev shell provides one).

use jqc::core::Value;
use std::io::Write;
use std::process::{Command, Stdio};

fn run_real_jq(filter: &str, input: &str) -> Vec<Value> {
    let mut child = Command::new("jq")
        .arg("-c")
        .arg(filter)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn jq (is it on PATH?)");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "real jq failed for filter {filter:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

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

/// A known, tracked discrepancy: compiles and runs without erroring, but
/// its output doesn't match real `jq`. Unlike `difftest!`, this asserts
/// the *current* (wrong) output, so the test goes red -- not silently
/// stays green -- the moment `jqc` is fixed, which is the signal to
/// delete the `$want_now` override and promote it to a plain `difftest!`.
/// See `jqc-manual-tests-macros`'s `KNOWN_RUNTIME_DISCREPANCIES` for the
/// auto-generated-suite's equivalent.
macro_rules! xfail_difftest {
    ($name:ident, $src:literal, $input:literal, want_now: $want_now:expr, $reason:literal) => {
        #[test]
        fn $name() {
            let compiled = jqc::jq!($src);
            let input: Value = serde_json::from_str($input).expect("test input must be valid JSON");
            let got = compiled(&input)
                .unwrap_or_else(|e| panic!("compiled filter {:?} errored: {e}", $src));
            let real_jq = run_real_jq($src, $input);
            assert_ne!(
                got, real_jq,
                "filter {:?} on input {} now matches real jq -- promote to difftest! ({})",
                $src, $input, $reason
            );
            assert_eq!(
                got, $want_now,
                "filter {:?} on input {}: current output changed",
                $src, $input
            );
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
difftest!(type_builtin, "[.[] | type]", r#"[1,"a",null,true,[1],{}]"#);
difftest!(add_builtin, "add", "[1,2,3]");
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
difftest!(until_builtin, "until(. >= 100; . * 2)", "1");
xfail_difftest!(
    repeat_error_optional,
    "[repeat(.*2, error)?]",
    "1",
    want_now: vec![jqc::core::json!([])],
    "jqc's repeat()/error interaction with `?` doesn't match real jq's \
     lazy-generator short-circuit semantics yet (jqc: [], real jq: [2]); \
     found via jqc-manual-tests' generated suite (jq 1.8 manual example)"
);
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

#[test]
fn optional_only_suppresses_its_own_step() {
    // `.a.b.c?` -- the `?` binds to `.c` only; the error from `.a.b`
    // (indexing the number 1) is not suppressed. Matches real jq 1.8.
    let compiled = jqc::jq!(".a.b.c?");
    let input: Value = serde_json::from_str(r#"{"a":1}"#).unwrap();
    assert!(compiled(&input).is_err());
}
