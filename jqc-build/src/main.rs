//! `jqc-build <script.jq> [-o <output-binary>]`: compiles a jq filter given
//! as a source file into a standalone, stdin/stdout-JSON-lines binary --
//! `jq!`'s compile-time specialization, but driven by a script file
//! supplied at *jqc-build's own* runtime instead of hand-written into some
//! other crate's `main.rs` at that crate's compile time (c.f.
//! `examples/src/main.rs`, which does exactly this but for one fixed,
//! hardcoded filter).
//!
//! Internally: generates a scratch Cargo project whose `main.rs` embeds
//! the script text as a `jq!` string literal (so the real parse/lower/
//! codegen work happens via the normal proc-macro path, at *that*
//! project's compile time), then runs `cargo build --release` on it and
//! reports the resulting binary's path on stdout.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Caps the `cargo build` child's address space (inherited by the `rustc`
/// it in turn spawns, since `RLIMIT_AS` carries across `fork`/`exec`) so a
/// script whose compilation blows up -- e.g. a `def` that nests several
/// prelude-builtin calls, which can trigger `jqc-compiler`'s lowering
/// pass's own exponential-ish re-lowering cost (same class of bug as the
/// `def_scope` cost cliff `jqc-compiler/src/bench.rs` guards against, but
/// not yet fully fixed) -- fails fast with an OOM in that one process
/// instead of swapping the whole machine or triggering the kernel's OOM
/// killer against unrelated processes. Nominally 32GiB, clamped to a
/// quarter of physical RAM so a machine with less than 128GiB doesn't
/// still get bitten before the limit is ever reached; override with
/// `JQC_BUILD_MEM_LIMIT_GB`.
#[cfg(unix)]
fn cap_child_memory(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;

    const NOMINAL_GB: u64 = 32;
    let phys_gb = unsafe {
        let pages = libc::sysconf(libc::_SC_PHYS_PAGES);
        let page_size = libc::sysconf(libc::_SC_PAGE_SIZE);
        (pages > 0 && page_size > 0)
            .then(|| (pages as u64 * page_size as u64) / (1024 * 1024 * 1024))
    };
    let default_gb = phys_gb
        .map(|gb| (gb / 4).clamp(1, NOMINAL_GB))
        .unwrap_or(NOMINAL_GB);
    let gb: u64 = env::var("JQC_BUILD_MEM_LIMIT_GB")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default_gb);
    let limit_bytes = gb * 1024 * 1024 * 1024;
    unsafe {
        cmd.pre_exec(move || {
            let limit = libc::rlimit {
                rlim_cur: limit_bytes,
                rlim_max: limit_bytes,
            };
            if libc::setrlimit(libc::RLIMIT_AS, &limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn cap_child_memory(_cmd: &mut Command) {}

/// Locates the workspace this scratch project's `jqc`/`jqc-cli`
/// dependencies live in.
///
/// Dev/`cargo run`-from-workspace case: `jqc-build`'s own manifest dir is
/// `<repo>/jqc-build`, so its parent is the workspace root. Packaged
/// case (`nix build`/`nix run`, where the binary runs long after its
/// build sandbox is gone): the flake wraps this binary with
/// `JQC_WORKSPACE_DIR` set to its own source (a permanent store path),
/// which takes precedence.
fn workspace_path() -> PathBuf {
    env::var_os("JQC_WORKSPACE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .expect("jqc-build's manifest dir has no parent")
                .to_path_buf()
        })
}

fn escape_rust_str_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

/// Keeps a name usable both as a Cargo package name and a file name.
fn sanitize_name(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if cleaned.is_empty() || !cleaned.chars().next().unwrap().is_ascii_alphabetic() {
        format!("jqc-script-{cleaned}")
    } else {
        cleaned
    }
}

fn parse_args() -> (PathBuf, Option<PathBuf>) {
    let mut script_path = None;
    let mut output = None;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-o" | "--output" => {
                output = Some(PathBuf::from(args.next().unwrap_or_else(|| {
                    eprintln!("jqc-build: {arg} requires a path argument");
                    std::process::exit(2);
                })));
            }
            "-h" | "--help" => {
                println!("usage: jqc-build <script.jq> [-o <output-binary>]");
                std::process::exit(0);
            }
            other => script_path = Some(PathBuf::from(other)),
        }
    }
    let script_path = script_path.unwrap_or_else(|| {
        eprintln!("usage: jqc-build <script.jq> [-o <output-binary>]");
        std::process::exit(2);
    });
    (script_path, output)
}

fn main() {
    let (script_path, output) = parse_args();

    let script = fs::read_to_string(&script_path).unwrap_or_else(|e| {
        eprintln!("jqc-build: reading {}: {e}", script_path.display());
        std::process::exit(1);
    });

    let name = sanitize_name(
        script_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("jqc-script"),
    );

    let scratch = env::temp_dir().join(format!("jqc-build-{name}-{}", std::process::id()));
    fs::create_dir_all(scratch.join("src")).unwrap_or_else(|e| {
        eprintln!("jqc-build: creating scratch dir {}: {e}", scratch.display());
        std::process::exit(1);
    });

    let workspace = workspace_path();
    let jqc_path = workspace.join("jqc");
    let jqc_cli_path = workspace.join("jqc-cli");
    let cargo_toml = format!(
        "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [dependencies]\njqc = {{ path = {jqc_path:?} }}\n\
         jqc-cli = {{ path = {jqc_cli_path:?} }}\n",
    );
    fs::write(scratch.join("Cargo.toml"), cargo_toml).expect("write scratch Cargo.toml");

    let escaped = escape_rust_str_literal(&script);
    let main_rs = format!(
        r#"use jqc::jq;

fn main() -> std::process::ExitCode {{
    let filter = jq!("{escaped}");
    jqc_cli::run(filter)
}}
"#,
    );
    fs::write(scratch.join("src").join("main.rs"), main_rs).expect("write scratch main.rs");

    let mut cmd = Command::new("cargo");
    cmd.args(["build", "--release", "--manifest-path"])
        .arg(scratch.join("Cargo.toml"));
    cap_child_memory(&mut cmd);
    let status = cmd.status().unwrap_or_else(|e| {
        eprintln!("jqc-build: spawning cargo (is it on PATH?): {e}");
        std::process::exit(1);
    });
    if !status.success() {
        eprintln!(
            "jqc-build: cargo build failed for {}",
            script_path.display()
        );
        std::process::exit(status.code().unwrap_or(1));
    }

    let built = scratch.join("target").join("release").join(&name);
    let dest = output.unwrap_or_else(|| PathBuf::from(&name));
    if let Some(parent) = dest.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).ok();
    }
    fs::copy(&built, &dest).unwrap_or_else(|e| {
        eprintln!(
            "jqc-build: copying {} to {}: {e}",
            built.display(),
            dest.display()
        );
        std::process::exit(1);
    });
    fs::remove_dir_all(&scratch).ok();
    println!("{}", dest.display());
}
