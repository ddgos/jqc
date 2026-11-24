//! Demonstrates `jq!`: the filter below is baked in at compile time, so
//! `cargo build --release` produces a standalone binary dedicated to
//! running exactly this filter -- no jq parser/interpreter is linked in.
//!
//! `jqc_cli::run` gives this binary the same CLI as real `jq` itself
//! (`-c`/`-r`/`-C`/`-S`/`-s`/`-R`/`-n`/`-e`/`--indent`/`--tab`/…, plus
//! file arguments) -- see `jqc-cli`'s own docs for the full flag list and
//! what's deliberately not supported.

use jqc::jq;

fn main() -> std::process::ExitCode {
    let filter = jq!(r#".items[] | select(.active) | {name: .name, score: (.score * 2)}"#);
    jqc_cli::run(filter)
}
