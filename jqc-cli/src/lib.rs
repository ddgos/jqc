//! jq-compatible CLI for `jqc-build`-generated binaries (and anything else
//! embedding a compiled [`jqc::jq!`] filter): argument parsing and output
//! formatting matching real jq's own (`src/main.c`/`src/jv_print.c`) byte
//! for byte, so a `jqc-build`-compiled filter is a drop-in replacement for
//! `jq '<filter>'` for the common case of filtering JSON on stdin/files.
//!
//! Deliberately not in scope: `-f`/`--from-file` (the filter is already
//! fixed at compile time, not supplied per-run), `-L`/module `import`
//! (modules aren't implemented), `--stream`/`--seq` (streaming parse isn't
//! implemented), and `--arg`/`--argjson`/`--args`/`--jsonargs`/`$ENV`
//! (jqc's variable bindings are resolved entirely at compile time from the
//! filter's own `as` patterns; there's no slot for a value injected at the
//! compiled binary's runtime without new compiler support -- see
//! README.md's "Known limitations").

use jqc_core::{JResult, Value};
use std::io::{self, IsTerminal, Read, Write};
use std::process::ExitCode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Indent {
    Spaces(u32),
    Tab,
}

/// Parsed CLI arguments. Construct via [`Args::parse`]; `Default` matches
/// plain `jq '<filter>'` with no flags (pretty-printed, auto color,
/// newline-delimited JSON in, JSON out).
#[derive(Debug, Clone)]
pub struct Args {
    raw_output: bool,
    raw_output0: bool,
    join_output: bool,
    compact: bool,
    color: Option<bool>,
    ascii: bool,
    sort_keys: bool,
    raw_input: bool,
    slurp: bool,
    null_input: bool,
    exit_status: bool,
    indent: Indent,
    files: Vec<String>,
}

impl Default for Args {
    fn default() -> Self {
        Args {
            raw_output: false,
            raw_output0: false,
            join_output: false,
            compact: false,
            color: None,
            ascii: false,
            sort_keys: false,
            raw_input: false,
            slurp: false,
            null_input: false,
            exit_status: false,
            indent: Indent::Spaces(2),
            files: Vec::new(),
        }
    }
}

impl Args {
    fn pretty(&self) -> bool {
        !self.compact
    }

    /// Parses an argv-style argument list (not including argv[0]). Returns
    /// `Err(message)` for an unknown or unsupported flag.
    ///
    /// Argument-splitting itself (combined short flags like `-nc`,
    /// `--long value`/`--long=value`, `--` ending flag parsing, `-` as a
    /// plain positional rather than an empty flag cluster) is
    /// `lexopt` -- a minimal, unopinionated argv tokenizer -- rather than
    /// hand-rolled: that part has no jq-specific behavior to get right,
    /// just the usual argv conventions every CLI parser needs, which is
    /// exactly what makes it a reasonable thing to delegate instead of
    /// reimplementing.
    pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Self, String> {
        use lexopt::prelude::*;

        let mut a = Args::default();
        let mut parser = lexopt::Parser::from_args(args);
        while let Some(arg) = parser.next().map_err(|e| e.to_string())? {
            match arg {
                Short('s') | Long("slurp") => a.slurp = true,
                Short('r') | Long("raw-output") => a.raw_output = true,
                Long("raw-output0") => {
                    a.raw_output = true;
                    a.raw_output0 = true;
                }
                Short('j') | Long("join-output") => {
                    a.raw_output = true;
                    a.join_output = true;
                }
                Short('c') | Long("compact-output") => a.compact = true,
                Short('C') | Long("color-output") => a.color = Some(true),
                Short('M') | Long("monochrome-output") => a.color = Some(false),
                Short('a') | Long("ascii-output") => a.ascii = true,
                Short('S') | Long("sort-keys") => a.sort_keys = true,
                Short('R') | Long("raw-input") => a.raw_input = true,
                Short('n') | Long("null-input") => a.null_input = true,
                Short('e') | Long("exit-status") => a.exit_status = true,
                Long("unbuffered") => {}
                Long("tab") => a.indent = Indent::Tab,
                Long("indent") => {
                    let v = parser.value().map_err(|e| e.to_string())?;
                    let v = v.string().map_err(|_| "--indent: not valid UTF-8")?;
                    let n: u32 = v
                        .parse()
                        .map_err(|_| format!("--indent: not a number: {v}"))?;
                    a.indent = Indent::Spaces(n.min(7));
                }
                Long(
                    long @ ("arg" | "argjson" | "args" | "jsonargs" | "slurpfile" | "rawfile"),
                ) => {
                    return Err(format!(
                        "--{long} is not supported by jqc-compiled binaries (no $ENV/\
                         named-argument support -- see README.md's Known limitations)"
                    ))
                }
                Long(other) => return Err(format!("unknown option: --{other}")),
                Short(other) => return Err(format!("unknown option: -{other}")),
                Value(v) => a.files.push(
                    v.into_string()
                        .map_err(|_| "file argument: not valid UTF-8")?,
                ),
            }
        }
        Ok(a)
    }
}

/// jq's own default ANSI SGR codes (`src/jv_print.c`'s `DEFAULT_COLORS`),
/// indexed null/false/true/number/string/array/object/object-key -- the
/// same order as `jv_kind` (minus `JV_KIND_INVALID`, which never reaches
/// output here).
const DEFAULT_COLORS: [&str; 8] = [
    "0;90", "0;39", "0;39", "0;39", "0;32", "1;39", "1;39", "1;34",
];

/// `JQ_COLORS` overrides the defaults above, colon-separated, in the same
/// order; a blank or non `[0-9;]*` field leaves that slot at its default
/// (matching `jq_set_colors` in `src/jv_print.c`).
fn colors_from_env() -> [String; 8] {
    let mut colors = DEFAULT_COLORS.map(str::to_string);
    if let Ok(val) = std::env::var("JQ_COLORS") {
        for (slot, part) in colors.iter_mut().zip(val.split(':')) {
            if !part.is_empty() && part.chars().all(|c| c.is_ascii_digit() || c == ';') {
                *slot = part.to_string();
            }
        }
    }
    colors
}

fn write_indent(out: &mut dyn Write, depth: u32, indent: Indent) -> io::Result<()> {
    match indent {
        Indent::Tab => {
            for _ in 0..depth {
                write!(out, "\t")?;
            }
        }
        Indent::Spaces(n) => {
            for _ in 0..(depth * n) {
                write!(out, " ")?;
            }
        }
    }
    Ok(())
}

fn write_unicode_escape(out: &mut dyn Write, code: u32) -> io::Result<()> {
    if code <= 0xffff {
        write!(out, "\\u{code:04x}")
    } else {
        let c = code - 0x10000;
        let hi = 0xd800 | ((c & 0xffc00) >> 10);
        let lo = 0xdc00 | (c & 0x3ff);
        write!(out, "\\u{hi:04x}\\u{lo:04x}")
    }
}

/// Matches `jvp_dump_string` in `src/jv_print.c` exactly: printable ASCII
/// passes through (escaping only `"`/`\`), the usual control-character
/// shorthands (`\b\t\r\n\f`) are used where they apply, every other
/// control character (and DEL) becomes `\u00XX`, and non-ASCII either
/// passes through as raw UTF-8 or becomes `\uXXXX` (with a surrogate pair
/// above the BMP) when `ascii` is set.
fn write_json_string(out: &mut dyn Write, s: &str, ascii: bool) -> io::Result<()> {
    write!(out, "\"")?;
    for c in s.chars() {
        let code = c as u32;
        if (0x20..=0x7e).contains(&code) {
            if c == '"' || c == '\\' {
                write!(out, "\\")?;
            }
            write!(out, "{c}")?;
        } else if code < 0x20 || code == 0x7f {
            match c {
                '\u{8}' => write!(out, "\\b")?,
                '\t' => write!(out, "\\t")?,
                '\r' => write!(out, "\\r")?,
                '\n' => write!(out, "\\n")?,
                '\u{c}' => write!(out, "\\f")?,
                _ => write_unicode_escape(out, code)?,
            }
        } else if ascii {
            write_unicode_escape(out, code)?;
        } else {
            write!(out, "{c}")?;
        }
    }
    write!(out, "\"")
}

fn kind_index(v: &Value) -> usize {
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

const FIELD_COLOR: usize = 7;

/// Writes `v` as JSON, following `jv_dump_term` (`src/jv_print.c`)
/// structurally: a kind's own color (if any) is written once on entry and
/// reset once on exit, with the color re-applied around each separator
/// (`,`/`:`) and the closing bracket so they render in the container's own
/// color rather than its last child's.
fn write_term(
    out: &mut dyn Write,
    v: &Value,
    args: &Args,
    colors: &[String; 8],
    color: bool,
    depth: u32,
) -> io::Result<()> {
    let kind = kind_index(v);
    if color {
        write!(out, "\x1b[{}m", colors[kind])?;
    }
    match v {
        Value::Null => write!(out, "null")?,
        Value::Bool(false) => write!(out, "false")?,
        Value::Bool(true) => write!(out, "true")?,
        // serde_json's own float formatter lowercases the scientific-
        // notation exponent marker ("1e+20"); real jq's jvp_dtoa_fmt
        // always uppercases it ("1E+20") -- confirmed directly against
        // jq, not merely assumed. 'e' never appears in a JSON number
        // token other than as that marker, so a blanket replace is safe.
        Value::Number(n) => write!(out, "{}", n.to_string().replace('e', "E"))?,
        Value::String(s) => write_json_string(out, s, args.ascii)?,
        Value::Array(items) => {
            if items.is_empty() {
                write!(out, "[]")?;
            } else {
                write!(out, "[")?;
                for (i, item) in items.iter().enumerate() {
                    if i != 0 {
                        if color {
                            write!(out, "\x1b[{}m", colors[kind])?;
                        }
                        write!(out, ",")?;
                    }
                    if color {
                        write!(out, "\x1b[0m")?;
                    }
                    if args.pretty() {
                        writeln!(out)?;
                        write_indent(out, depth + 1, args.indent)?;
                    }
                    write_term(out, item, args, colors, color, depth + 1)?;
                }
                if args.pretty() {
                    writeln!(out)?;
                    write_indent(out, depth, args.indent)?;
                }
                if color {
                    write!(out, "\x1b[{}m", colors[kind])?;
                }
                write!(out, "]")?;
            }
        }
        Value::Object(map) => {
            if map.is_empty() {
                write!(out, "{{}}")?;
            } else {
                write!(out, "{{")?;
                let mut keys: Vec<&String> = map.keys().collect();
                if args.sort_keys {
                    keys.sort();
                }
                for (i, k) in keys.into_iter().enumerate() {
                    if i != 0 {
                        if color {
                            write!(out, "\x1b[{}m", colors[kind])?;
                        }
                        write!(out, ",")?;
                    }
                    if color {
                        write!(out, "\x1b[0m")?;
                    }
                    if args.pretty() {
                        writeln!(out)?;
                        write_indent(out, depth + 1, args.indent)?;
                    }
                    if color {
                        write!(out, "\x1b[{}m", colors[FIELD_COLOR])?;
                    }
                    write_json_string(out, k, args.ascii)?;
                    if color {
                        write!(out, "\x1b[0m")?;
                        write!(out, "\x1b[{}m", colors[kind])?;
                    }
                    write!(out, ":")?;
                    if color {
                        write!(out, "\x1b[0m")?;
                    }
                    if args.pretty() {
                        write!(out, " ")?;
                    }
                    write_term(out, &map[k], args, colors, color, depth + 1)?;
                }
                if args.pretty() {
                    writeln!(out)?;
                    write_indent(out, depth, args.indent)?;
                }
                if color {
                    write!(out, "\x1b[{}m", colors[kind])?;
                }
                write!(out, "}}")?;
            }
        }
    }
    if color {
        write!(out, "\x1b[0m")?;
    }
    Ok(())
}

/// Renders `v` to a `String` the way jq would print it under `args`
/// (without the trailing newline/NUL a CLI run adds between outputs).
pub fn format_value(v: &Value, args: &Args, color: bool) -> String {
    let colors = colors_from_env();
    let mut buf = Vec::new();
    write_term(&mut buf, v, args, &colors, color, 0).expect("writing to a Vec<u8> never fails");
    String::from_utf8(buf).expect("jq's own output is always valid UTF-8")
}

fn resolve_color(args: &Args, out_is_tty: bool) -> bool {
    args.color.unwrap_or(out_is_tty)
}

/// Mirrors `process()`'s per-result branch in `src/main.c` exactly,
/// including its one surprising case: `-r`/`--raw-output` on a string
/// value is overridden by `-a`/`--ascii-output`, which prints it as a
/// plain compact ASCII-escaped JSON string instead of raw bytes (real
/// jq's `jv_dumpf(result, stdout, JV_PRINT_ASCII)` -- note *only*
/// `JV_PRINT_ASCII`, so pretty/color/sort-keys don't apply to this one
/// element either).
fn print_one(out: &mut dyn Write, v: &Value, args: &Args, color: bool) -> io::Result<()> {
    if let (true, Value::String(s)) = (args.raw_output, v) {
        if args.ascii {
            write_json_string(out, s, true)?;
        } else {
            if args.raw_output0 && s.contains('\0') {
                return Err(io::Error::other(
                    "Cannot dump a string containing NUL with --raw-output0 option",
                ));
            }
            write!(out, "{s}")?;
        }
    } else {
        write!(out, "{}", format_value(v, args, color))?;
    }
    if args.raw_output0 {
        out.write_all(b"\0")?;
    } else if !args.join_output {
        writeln!(out)?;
    }
    Ok(())
}

/// Truthiness of the last output, for `-e`/`--exit-status`: `true` for
/// anything but `false`/`null` (matching jq's own `JQ_OK`/`JQ_OK_NULL_KIND`
/// split in `src/main.c`).
fn is_truthy(v: &Value) -> bool {
    !matches!(v, Value::Null | Value::Bool(false))
}

/// Parses a stream of whitespace-or-not-separated JSON values from `src`
/// (jq's own input reader doesn't require newline separation between
/// documents either) into one `Vec`, erroring with the first parse
/// failure if any.
fn parse_json_stream(src: &str) -> Result<Vec<Value>, serde_json::Error> {
    serde_json::Deserializer::from_str(src)
        .into_iter::<Value>()
        .collect()
}

fn raw_input_values(src: &str, slurp: bool) -> Vec<Value> {
    if slurp {
        vec![Value::String(src.to_string())]
    } else {
        src.lines().map(|l| Value::String(l.to_string())).collect()
    }
}

/// Runs `filter` over `args`' inputs, writing formatted output to `out` and
/// reading raw input bytes from `read_input` (called once per named file,
/// or once for stdin if no files were given) -- the testable core behind
/// [`run`], which wires this to real stdin/stdout/argv.
pub fn run_with(
    filter: impl Fn(&Value) -> JResult<Vec<Value>>,
    args: &Args,
    mut read_input: impl FnMut() -> io::Result<String>,
    out: &mut dyn Write,
    out_is_tty: bool,
) -> i32 {
    let color = resolve_color(args, out_is_tty);

    let inputs: Vec<Value> = if args.null_input {
        vec![Value::Null]
    } else {
        let src = match read_input() {
            Ok(s) => s,
            Err(e) => {
                eprintln!("jq: error: {e}");
                return 2;
            }
        };
        if args.raw_input {
            raw_input_values(&src, args.slurp)
        } else {
            let parsed = match parse_json_stream(&src) {
                Ok(vs) => vs,
                Err(e) => {
                    eprintln!("jq: error (at <stdin>): {e}");
                    return 2;
                }
            };
            if args.slurp {
                vec![Value::Array(parsed)]
            } else {
                parsed
            }
        }
    };

    let mut had_error = false;
    let mut had_output = false;
    let mut last_truthy = false;
    for input in &inputs {
        match filter(input) {
            Ok(outputs) => {
                for v in &outputs {
                    had_output = true;
                    last_truthy = is_truthy(v);
                    if let Err(e) = print_one(out, v, args, color) {
                        eprintln!("jq: error: writing output: {e}");
                        return 2;
                    }
                }
            }
            Err(e) => {
                eprintln!("jq: error: {e}");
                had_error = true;
            }
        }
    }
    let _ = out.flush();

    if had_error {
        5
    } else if args.exit_status {
        if !had_output {
            4
        } else if !last_truthy {
            1
        } else {
            0
        }
    } else {
        0
    }
}

/// Reads all of `args.files` (each file's raw bytes, concatenated -- `-`
/// means stdin) into one buffer, or all of stdin if no files were given,
/// matching jq's own "files behave like `cat`'s arguments" input model.
fn read_all_inputs(files: &[String]) -> io::Result<String> {
    if files.is_empty() {
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf)?;
        return Ok(buf);
    }
    let mut buf = String::new();
    for f in files {
        if f == "-" {
            io::stdin().read_to_string(&mut buf)?;
        } else {
            buf.push_str(&std::fs::read_to_string(f)?);
        }
    }
    Ok(buf)
}

/// Parses `std::env::args()` and runs `filter` against real stdin/stdout,
/// matching jq's own exit-code conventions (`0`/`1`/`2`/`4`/`5`; see
/// `src/main.c`'s `JQ_OK*`/`JQ_ERROR*`). This is the entire body a
/// `jqc-build`-generated `main` needs:
///
/// ```ignore
/// fn main() -> std::process::ExitCode {
///     let filter = jqc::jq!("...");
///     jqc_cli::run(filter)
/// }
/// ```
pub fn run(filter: impl Fn(&Value) -> JResult<Vec<Value>>) -> ExitCode {
    let args = match Args::parse(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("jq: error: {e}");
            return ExitCode::from(2);
        }
    };
    let files = args.files.clone();
    let stdout = io::stdout();
    let is_tty = stdout.is_terminal();
    let mut lock = stdout.lock();
    let code = run_with(filter, &args, || read_all_inputs(&files), &mut lock, is_tty);
    ExitCode::from(code as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_combined_short_flags() {
        let a = Args::parse(["-nc".to_string()]).unwrap();
        assert!(a.null_input);
        assert!(a.compact);
    }

    #[test]
    fn parses_indent() {
        let a = Args::parse(["--indent".to_string(), "4".to_string()]).unwrap();
        assert_eq!(a.indent, Indent::Spaces(4));
    }

    #[test]
    fn files_are_positional() {
        let a =
            Args::parse(["-c".to_string(), "a.json".to_string(), "b.json".to_string()]).unwrap();
        assert_eq!(a.files, vec!["a.json", "b.json"]);
    }

    #[test]
    fn unsupported_flag_is_an_error() {
        assert!(Args::parse(["--arg".to_string()]).is_err());
    }
}
