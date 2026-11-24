//! `generate_c_tests!(version = "1.8")` reads jq's own C test suite --
//! `tests/jq.test` in a jq source checkout at `$JQ_SRC` (see the `jq-src`
//! flake input and the Nix dev shell's `JQ_SRC`) -- and emits one
//! `#[test]` per case, using the exact same two-line-marker/blank-or-
//! comment-line-terminated grouping algorithm as jq's own test runner
//! (`src/jq_test.c`'s `run_jq_tests`), so this stays in sync with
//! whatever that file's format actually is rather than a guess at it:
//!
//! - A `%%FAIL` (or `%%FAIL IGNORE MSG`) case: the *next* line is the
//!   program, expected to fail to compile (no input line follows --
//!   jq's own runner never gets as far as running these). Always
//!   generates an xfail test (assert compiling it still fails), the same
//!   as an ordinary case jqc can't yet compile -- see below for why that
//!   choice costs nothing even when it's wrong.
//! - An ordinary case: program, then an input line, then zero or more
//!   expected-output lines jq's test file itself provides -- *not* used
//!   here (see "why re-derive" below); only their count matters, to find
//!   where this case ends and the next one begins.
//!
//! For an ordinary case:
//! - If `jqc-compiler` can't lex/parse/lower/codegen the program, an
//!   *xfail* test (assert compiling it still fails), exactly like
//!   `jqc-manual-tests-macros`: when jqc gains the needed support, this
//!   flips to a hard failure, signaling it's time to promote the case to
//!   a real test, rather than silently dropping it.
//! - If jqc *can* compile it, a real differential test -- but unlike the
//!   manual examples (curated to be valid, successful illustrations),
//!   jq's C suite is deliberately adversarial: plenty of ordinary (non-
//!   `%%FAIL`) cases expect a *runtime* error from real jq on that input.
//!   So the generated test runs both sides and tolerates either
//!   succeeding or erroring, only failing if they disagree about *which*.
//!
//! Why re-derive expected output by actually running `jq` (same as
//! `jqc-manual-tests-macros`) instead of trusting the file's own
//! expected-output lines: those lines are compared by jq's *own* runner
//! against a specific build's `jv_dump` formatting (number/key-order
//! quirks some of which are conditional on build-time options like
//! `have_decnum`), which this generator has no way to replicate exactly
//! -- running the `jq` binary actually on `$PATH` sidesteps all of that.
use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::{LitStr, Token};

use jqc_compiler::codegen::{Codegen, TreeWalkCodegen};
use jqc_compiler::lower;
use jqc_compiler::version::JqVersion;

/// One case parsed out of `jq.test`.
enum CTestCase {
    /// `%%FAIL` / `%%FAIL IGNORE MSG`: `program` is expected to fail to
    /// compile. No input -- jq's own runner never gets that far either.
    Fail { program: String },
    Normal { program: String, input: String },
}

/// `jq_test.c`'s `skipline`: blank (after leading spaces/tabs) or a `#`
/// comment -- both a case's own internal structure (there is no comment
/// *inside* a case; comments only ever separate cases) and the single
/// mechanism used to know where one case's input/expected-output lines
/// end and the gap before the next case begins.
fn is_skip_line(s: &str) -> bool {
    let t = s.trim_start_matches([' ', '\t']);
    t.is_empty() || t.starts_with('#')
}

/// Parses `src` into cases, mirroring `run_jq_tests` in `src/jq_test.c`
/// line for line (see that function for the authoritative algorithm this
/// is a Rust transliteration of).
fn parse_c_tests(src: &str) -> Vec<CTestCase> {
    let mut lines = src.lines().peekable();
    let mut cases = Vec::new();
    while let Some(line) = lines.next() {
        if is_skip_line(line) {
            continue;
        }
        if line == "%%FAIL" || line == "%%FAIL IGNORE MSG" {
            // The marker line itself isn't the program -- the next
            // non-skip line is (mirrors skipline(prog) at the top of
            // jq_test.c's main loop, which also applies to the line read
            // right after setting must_fail).
            let program = loop {
                match lines.next() {
                    Some(l) if is_skip_line(l) => continue,
                    Some(l) => break Some(l.to_string()),
                    None => break None,
                }
            };
            let Some(program) = program else { break };
            // Consume the expected error-message line(s) -- not used
            // here (see module doc comment for why).
            while let Some(l) = lines.peek() {
                if is_skip_line(l) {
                    break;
                }
                lines.next();
            }
            cases.push(CTestCase::Fail { program });
            continue;
        }
        let program = line.to_string();
        let Some(input) = lines.next() else { break };
        let input = input.to_string();
        // Consume the expected-output line(s) -- not used (see above).
        while let Some(l) = lines.peek() {
            if is_skip_line(l) {
                break;
            }
            lines.next();
        }
        cases.push(CTestCase::Normal { program, input });
    }
    cases
}

/// Cases where jqc compiles the program and produces a result that
/// genuinely diverges from real jq (not just "jq errors, jqc doesn't" or
/// vice versa -- the generated test already tolerates that symmetrically
/// -- but both sides succeeding with *different* output, or some other
/// jqc-specific wrong behavior). Each entry is `(exact program text,
/// reason shown as the test's #[ignore] message)`. Found running this
/// generator against jq's full C suite for the first time; left as
/// documented xfails rather than fixed here -- remove an entry once jqc
/// is fixed and its case goes green on its own.
const KNOWN_RUNTIME_DISCREPANCIES: &[(&str, &str)] = &[
    (
        "[limit(1; 1, error)]",
        "jqc is eager: limit(n; exp)'s `exp` is fully evaluated (hitting `error`) before \
         foreach/break ever gets a chance to short-circuit away from it, unlike real jq's lazy \
         generators -- the same root cause as the next entry, and ultimately the README's \
         documented \"eager, not lazy\" limitation, not a bug specific to limit/break",
    ),
    (
        "first(1,error(\"foo\"))",
        "same eager-evaluation root cause as limit(1; 1, error) above: first(f)'s `f` is fully \
         evaluated (hitting `error`) before first's own label/break short-circuit ever runs",
    ),
    (
        "[limit(5,7; range(9))]",
        "limit's `n` parameter is a filter (call-by-name) argument referenced three times in its \
         body (the `if`/`elif` conditions and foreach's extract clause); real jq's closure \
         semantics re-evaluate a multi-valued filter argument fresh at every *dynamic* \
         occurrence (including inside a loop, verified directly against jq), which jqc's \
         compile-time AST-substitution approach to filter params doesn't reproduce for a \
         reference sitting inside a loop body -- a deeper semantic gap than this one builtin, \
         not pursued here",
    ),
    (
        "implode|explode",
        "real jq replaces invalid codepoints (negative, >0x10FFFF, UTF-16 surrogates) with the \
         U+FFFD replacement character; jqc's implode silently drops them instead",
    ),
    (
        "[range(10)] | .[1.2:3.5]",
        "real jq's slice bounds round asymmetrically (floor the start, ceiling the end if it had \
         a fractional part -- confirmed against src/jv_aux.c's parse_slice); jqc truncates both \
         the same way",
    ),
    (
        "[range(10)] | .[1.5:3.5]",
        "same asymmetric slice-bound-rounding gap as .[1.2:3.5] above",
    ),
    (
        "[range(10)] | .[1.7:3.5]",
        "same asymmetric slice-bound-rounding gap as .[1.2:3.5] above",
    ),
    (
        ".[] as [$x, $y] | try [\"ok\", ($x | ltrimstr($y))] catch [\"ko\", .]",
        "real jq's ltrimstr/rtrimstr require *both* operands to be strings (they delegate to \
         startswith/endswith, which error otherwise); jqc's ltrimstr doesn't validate $y's type \
         and just returns unchanged instead of erroring",
    ),
    (
        ".[] as [$x, $y] | try [\"ok\", ($x | rtrimstr($y))] catch [\"ko\", .]",
        "same missing type-check gap as ltrimstr above",
    ),
    //
    // The following cluster (jq.test lines ~2568-2633) are upstream jq's
    // own regression tests for CVE-2026-33947/-40612/-43896: real jq used
    // to stack-overflow (an actual DoS vector, not just a wrong answer)
    // on a deliberately 10001-deep nested array/object, and was fixed by
    // adding an explicit depth limit to contains/equality/comparison/
    // object-merge, returning a clean "... too deep" error instead of
    // recursing unboundedly. jqc-core's equivalents (contains, PartialEq
    // via serde_json::Value, cmp_values, add_values/object merge) have
    // *no* such limit and really do stack-overflow -- this is the same
    // class of bug real jq shipped a CVE fix for, not merely a test
    // inconvenience; worth prioritizing a real depth-limit fix over
    // other items in todos.md, not just leaving these #[ignore]d
    // indefinitely. setpath/getpath/delpaths/tojson, the other builtins
    // CVE-2026-33947's own test block exercises, aren't implemented by
    // jqc at all yet (see README's "Not implemented"), so those cases
    // are already ordinary xfail_unsupported and carry no crash risk.
    (
        "reduce range(10000) as $_ ([]; [.]) | contains([[]])",
        "CVE-2026-40612's own regression test, at the depth (10000) real jq's fix *keeps* \
         working at (only *more* than that should error) -- jqc-core's contains stack-overflows \
         here too, meaning its per-level stack usage is heavier than jq's C recursion, not just \
         missing the same depth limit",
    ),
    (
        "try (reduce range(10001) as $_ ([]; [.]) as $x | $x | contains($x)) catch .",
        "CVE-2026-33947-class stack overflow: jqc-core's contains has no depth limit (real jq's \
         own fix for this CVE added one, returning \"Containment check too deep\" instead)",
    ),
    (
        "reduce range(10000) as $_ ({}; {a: .}) as $x | $x * $x | length",
        "CVE-2026-43896's own regression test, at the depth (10000) real jq's fix keeps working \
         at -- jqc-core's object-merge (`*`) stack-overflows here too, so its per-level stack \
         usage is heavier than jq's C recursion, not just missing the same depth limit",
    ),
    (
        "try (reduce range(10001) as $_ ({}; {a: .}) as $x | $x * $x) catch .",
        "CVE-2026-43896-class stack overflow: jqc-core's object-merge (`*`) has no depth limit",
    ),
    (
        "try ((reduce range(10001) as $_ ([]; [.])) as $x | (reduce range(10001) as $_ ([]; [.])) as $y | $x == $y) catch .",
        "deep structural equality recursion, same CVE-class issue as contains() above (`==` uses \
         serde_json::Value's derived PartialEq, which has no depth limit either)",
    ),
    (
        "try ((reduce range(10001) as $_ ([]; [.])) as $x | [$x, $x] | sort) catch .",
        "CVE-class stack overflow in deep ordering comparison (jqc-core's cmp_values)",
    ),
    (
        "try ((reduce range(10001) as $_ ([]; [.])) as $x | [$x, $x] | unique) catch .",
        "same deep-comparison stack overflow as the sort case above (unique also calls cmp_values)",
    ),
    (
        "try ((reduce range(10001) as $_ ({}; {a: .})) as $x | [$x, $x] | sort) catch .",
        "same deep-comparison stack overflow, via a deeply-nested object instead of array",
    ),
    (
        "try ((reduce range(10001) as $_ ({}; {a: .})) as $x | [$x, $x] | unique) catch .",
        "same deep-comparison stack overflow, via a deeply-nested object instead of array",
    ),
    //
    // The rest of this list is the first full pass of this generator
    // against jq.test, past the CVE-regression cluster above -- found,
    // documented, deliberately left unfixed per instruction rather than
    // chased down further in the same sitting as building this generator.
    (
        "any(true, error; .)",
        "same eager-evaluation root cause as limit(1; 1, error) above (see that entry): \
         any(g; cond)'s `g` is fully evaluated (hitting `error`) before any's own \
         short-circuit-on-first-true ever gets a chance to run",
    ),
    (
        "all(false, error; .)",
        "same eager-evaluation root cause as any(true, error; .) above, via all/false instead",
    ),
    (
        "split(\"\")",
        "real jq's split(\"\") splits into individual characters; jqc's split errors instead \
         (\"split input and separator must not be empty\") -- an unconditional check that \
         doesn't match real jq's actual (permissive) behavior for this specific case",
    ),
    (
        "join(\",\")",
        "real jq's join errors on a non-scalar (array/object) element (delegates to `+`, which \
         itself errors mixing a string with an array/object); jqc's join silently stringifies \
         the nested value instead of validating its type first",
    ),
    (
        "[.[] * \"abc\"]",
        "off-by-one at the n=0.0 boundary for string-repeat: real jq gives \"\" for 0 <= n < 1, \
         jqc gives null for n <= 0 (so n == 0.0 exactly comes out wrong; n < 0 and 0 < n < 1 \
         both already agree with real jq)",
    ),
    (
        "def id(x):x; 2000 as $x | def f(x):1 as $x | id([$x, x, x]); def g(x): 100 as $x | f($x,$x+x); g($x)",
        "a deeper version of the nested-def-shadowing class of bug fixed earlier in this same \
         session (see the nested_def_shadows_outer_same_name_in_its_own_rest differential test) \
         -- this one reuses the same filter-parameter name (\"x\") across three nested defs \
         (id, f, g) rather than reusing a def's own name, and jqc's call-by-name substitution \
         cross-contaminates between them: real jq gives [1,100,2100,100,2100], jqc gives \
         [1,1,2,1,2] (looks like an inner def's own parameter leaking into where an outer one's \
         argument value should be used)",
    ),
    (
        "[[.[]|[.a,.a]]?]",
        "a collector node (array construction) that fails partway through (indexing a boolean \
         with .a errors) should produce *no* value at all, not a partial one, for a `?`/try \
         around it to then suppress entirely -- real jq gives [] (the whole thing suppressed), \
         jqc gives [[null,null]] (keeping the first, successful iteration's own array, instead \
         of the failure invalidating the outer collector as a whole)",
    ),
    (
        "try [\"OK\", (.[] | error)] catch [\"KO\", .]",
        "same collector-node-should-produce-nothing-on-partial-failure class of bug as \
         [[.[]|[.a,.a]]?] above: real jq gives one value, [\"KO\",[\"b\"]] (the whole array \
         construction's failure caught as one unit); jqc gives two separate top-level values, \
         \"OK\" and [\"KO\",[\"b\"]] (the array literal's first, already-collected element \
         leaking out as its own output instead of being discarded with the rest)",
    ),
    (
        "5E500000000 > 5E-5000000000, 10000E500000000 > 10000E-5000000000",
        "a real wrong answer, not a formatting difference: both operands overflow/underflow \
         to +inf/0 in f64 either way, so inf > 0 should be true (real jq: true,true) -- jqc \
         gives false,false, meaning its comparison of extreme-magnitude numeric literals goes \
         wrong somewhere between lexing and cmp_values, not merely a display quirk like the \
         1/1e-17 case above",
    ),
    (
        "(1e999999999, 10e999999999) > (1e-1147483646, 0.1e-1147483646)",
        "same wrong-comparison-at-extreme-magnitude bug as the 5E500000000 case above",
    ),
    (
        "try join(\",\") catch .",
        "real jq's join errors on a non-scalar (array/object) element (delegates to `+`, which \
         itself errors mixing a string with an array/object); jqc's join silently stringifies \
         the nested value instead of validating its type first",
    ),
    (
        "try (1/0) catch .",
        "message-text only: both correctly error on division by zero, worded differently \
         (real jq includes both operands' values)",
    ),
    (
        "try (1/.) catch .",
        "message-text only: same division-by-zero wording gap as try (1/0) catch . above",
    ),
    (
        "try (0/0) catch .",
        "message-text only: same division-by-zero wording gap as try (1/0) catch . above",
    ),
    (
        "try (1%0) catch .",
        "message-text only: same wording gap as the division-by-zero cases above, for modulo",
    ),
    (
        "try (1%.) catch .",
        "message-text only: same modulo-by-zero wording gap as try (1%0) catch . above",
    ),
    // Message-text-only mismatches below: both sides correctly detect
    // the error, but jqc's message omits the operand's actual value
    // (and/or type name) that real jq's includes -- not a logic bug, a
    // lower-priority wording gap (would need per-builtin work to match
    // jq's exact phrasing, not pursued here).
    (
        "try 0[implode] catch .",
        "message-text only: jqc's \"Cannot index number with string\" omits the key's value \
         (real jq: \"...with string (\\\"\\\")\")",
    ),
    (
        "try (\"foobar\" | .[1.5]) catch .",
        "message-text only: same omitted-value gap as the implode-as-key case above",
    ),
    (
        "[.[]|try if . == 0 then error(\"foo\") elif . == 1 then .a elif . == 2 then empty else . end catch .]",
        "message-text only: jqc's \"Cannot index number with \\\"a\\\"\" omits the \"string\" \
         type-name word real jq's \"Cannot index number with string (\\\"a\\\")\" includes",
    ),
    (
        "try (.-.) catch .",
        "message-text only: real jq truncates a long operand's value in the message \
         (\"...-long...\"); jqc includes it in full -- both correctly identify the error",
    ),
    (
        "\"123\\u0000456\" | try tonumber catch .",
        "message-text only: both correctly reject a NUL-containing string as unparseable, \
         worded differently",
    ),
    (
        "1 / 1e-17",
        "formatting-only, not a wrong value: 1e17 is exactly representable in f64, and both \
         sides compute it -- real jq prints huge whole numbers in scientific notation \
         (\"1e+17\"), jqc's number_from_f64 prefers a plain integer literal when one is exact, \
         and serde_json::Number's derived equality treats its float/int-parsed variants as \
         unequal even when the mathematical value is identical",
    ),
    (
        "try (. * 1000000000) catch .",
        "a memory-safety gap, not just a wrong answer: real jq caps string-repeat output size \
         and errors (\"Repeat string result too long\"); jqc's string * number has no such \
         limit and actually allocates the full ~3GB result for this input -- the same class of \
         resource-exhaustion concern as the CVE-regression stack overflows above, just a \
         memory bound instead of a stack one, worth prioritizing alongside those rather than \
         leaving indefinitely",
    ),
    // (A cluster of extreme-exponent literals near this one in jq.test,
    // e.g. 9E999999999, needed no entry here at all: real jq's own
    // stdout for those contains a numeric literal too extreme for
    // serde_json to reparse, which RealJqResult::Untestable above (in
    // jqc-c-tests/tests/generated.rs) already treats as "can't verify
    // this one" rather than failing -- no per-case entry needed.)
];

/// As [`KNOWN_RUNTIME_DISCREPANCIES`], but for a `%%FAIL` case jqc
/// currently accepts where real jq's own parser specifically rejects it
/// (so the generated xfail -- "compiling it should still fail" --
/// legitimately fails, not a miscompile of something jq would accept).
const KNOWN_FAIL_CASE_ACCEPTANCES: &[(&str, &str)] = &[
    (
        ". as [] | null",
        "real jq rejects an empty [] destructuring pattern at parse time (meaningless -- binds \
         nothing); jqc's parser accepts it (Pattern::Array(vec![]))",
    ),
    (
        ". as {} | null",
        "same empty-pattern-should-be-rejected gap as the [] case above, for {}",
    ),
    (
        ". as {(true):$foo} | $foo",
        "real jq requires a destructuring pattern's computed key to be a string (rejects a \
         boolean key at parse/compile time); jqc's parser doesn't validate the key type",
    ),
    (
        "{(0):1}",
        "real jq rejects a non-string computed object-construction key (\"Cannot use number (0) \
         as object key\"); jqc doesn't validate this at all, just stringifies whatever the key \
         expression evaluates to at runtime",
    ),
    (
        "{non_const:., (0):1}",
        "same non-string-computed-key gap as {(0):1} above",
    ),
];

struct MacroInput {
    version: LitStr,
}

impl Parse for MacroInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let ident: syn::Ident = input.parse()?;
        if ident != "version" {
            return Err(syn::Error::new(
                ident.span(),
                "generate_c_tests!: expected 'version = \"X.Y\"'",
            ));
        }
        input.parse::<Token![=]>()?;
        let version: LitStr = input.parse()?;
        Ok(MacroInput { version })
    }
}

#[proc_macro]
pub fn generate_c_tests(input: TokenStream) -> TokenStream {
    let parsed = match syn::parse::<MacroInput>(input) {
        Ok(p) => p,
        Err(e) => return e.to_compile_error().into(),
    };
    let version = match JqVersion::parse(&parsed.version.value()) {
        Some(v) => v,
        None => {
            return syn::Error::new(
                parsed.version.span(),
                format!(
                    "generate_c_tests!: unknown jq version {:?}",
                    parsed.version.value()
                ),
            )
            .to_compile_error()
            .into();
        }
    };

    let jq_src = match std::env::var("JQ_SRC") {
        Ok(p) => p,
        Err(_) => {
            return syn::Error::new(
                Span::call_site(),
                "generate_c_tests!: JQ_SRC env var not set -- run inside `nix develop` \
                 (provides a pinned jq source checkout via the jq-src flake input)",
            )
            .to_compile_error()
            .into();
        }
    };
    let test_path = format!("{jq_src}/tests/jq.test");
    let src = match std::fs::read_to_string(&test_path) {
        Ok(s) => s,
        Err(e) => {
            return syn::Error::new(
                Span::call_site(),
                format!("generate_c_tests!: couldn't read {test_path}: {e}"),
            )
            .to_compile_error()
            .into();
        }
    };

    let version_token = version_to_tokens(version);
    let cases = parse_c_tests(&src);
    let mut tests = Vec::new();
    let mut compiled_count = 0usize;
    let mut xfail_count = 0usize;
    let mut fail_count = 0usize;

    for (idx, case) in cases.into_iter().enumerate() {
        match case {
            CTestCase::Fail { program } => {
                fail_count += 1;
                let fn_name = format_ident!("c_test_{idx}_xfail_expected_compile_error");
                let ignore_attr = KNOWN_FAIL_CASE_ACCEPTANCES
                    .iter()
                    .find(|(p, _)| *p == program.as_str())
                    .map(|(_, reason)| quote!(#[ignore = #reason]));
                tests.push(quote! {
                    #[test]
                    #ignore_attr
                    fn #fn_name() {
                        let result = ::jqc_compiler::lower::parse_and_lower(#program, #version_token)
                            .and_then(|ast| ::jqc_compiler::codegen::Codegen::compile_top(
                                &::jqc_compiler::codegen::TreeWalkCodegen,
                                &ast,
                            ));
                        assert!(
                            result.is_err(),
                            "jq.test %%FAIL case {:?} now compiles with jqc, but real jq's own \
                             parser specifically rejects it -- worth a manual look \
                             (jqc-c-tests-macros)",
                            #program
                        );
                    }
                });
            }
            CTestCase::Normal { program, input } => {
                let compile_result = lower::parse_and_lower(&program, version)
                    .and_then(|ast| TreeWalkCodegen.compile_top(&ast));
                match compile_result {
                    Ok(compiled_ts) => {
                        compiled_count += 1;
                        let fn_name = format_ident!("c_test_{idx}");
                        let ignore_attr = KNOWN_RUNTIME_DISCREPANCIES
                            .iter()
                            .find(|(p, _)| *p == program.as_str())
                            .map(|(_, reason)| quote!(#[ignore = #reason]));
                        tests.push(quote! {
                            #[test]
                            #ignore_attr
                            fn #fn_name() {
                                crate::check_case(#program, #compiled_ts, #input);
                            }
                        });
                    }
                    Err(_) => {
                        xfail_count += 1;
                        let fn_name = format_ident!("c_test_{idx}_xfail_unsupported");
                        tests.push(quote! {
                            #[test]
                            fn #fn_name() {
                                let result = ::jqc_compiler::lower::parse_and_lower(#program, #version_token)
                                    .and_then(|ast| ::jqc_compiler::codegen::Codegen::compile_top(
                                        &::jqc_compiler::codegen::TreeWalkCodegen,
                                        &ast,
                                    ));
                                assert!(
                                    result.is_err(),
                                    "jq.test case {:?} now compiles -- promote this xfail to a \
                                     real test (jqc-c-tests-macros)",
                                    #program
                                );
                            }
                        });
                    }
                }
            }
        }
    }

    let summary = format!(
        "jq C test suite (tests/jq.test): {compiled_count} case(s) compiled and \
         differentially tested, {xfail_count} case(s) still unsupported (xfail), \
         {fail_count} %%FAIL case(s) (expected to not compile)"
    );

    let expanded = quote! {
        #(#tests)*

        #[test]
        fn c_tests_summary() {
            eprintln!(#summary);
        }
    };
    expanded.into()
}

/// Reconstructs a `JqVersion::V1_x` token for splicing into generated
/// xfail tests, since `JqVersion` itself isn't `quote::ToTokens`.
fn version_to_tokens(version: JqVersion) -> proc_macro2::TokenStream {
    let variant = format_ident!(
        "{}",
        match version {
            JqVersion::V1_6 => "V1_6",
            JqVersion::V1_7 => "V1_7",
            JqVersion::V1_8 => "V1_8",
        }
    );
    quote!(::jqc_compiler::version::JqVersion::#variant)
}
