//! Demonstrates `jq!`: the filter below is baked in at compile time, so
//! `cargo build --release` produces a standalone binary dedicated to
//! running exactly this filter -- no jq parser/interpreter is linked in.
//!
//! Reads newline-delimited JSON from stdin, runs the filter on each value,
//! and prints every output value as a JSON line (like `jq` itself).

use std::io::{self, BufRead, Write};

use jqc::core::Value;
use jqc::jq;

fn main() {
    let filter = jq!(r#".items[] | select(.active) | {name: .name, score: (.score * 2)}"#);

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = stdout.lock();

    for line in stdin.lock().lines() {
        let line = line.expect("read stdin");
        if line.trim().is_empty() {
            continue;
        }
        let input: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("invalid JSON input: {e}");
                std::process::exit(1);
            }
        };
        match filter(&input) {
            Ok(outputs) => {
                for v in outputs {
                    writeln!(out, "{v}").expect("write stdout");
                }
            }
            Err(e) => {
                eprintln!("jq error: {e}");
                std::process::exit(5);
            }
        }
    }
}
