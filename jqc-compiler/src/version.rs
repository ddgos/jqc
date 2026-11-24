//! Which jq language-version semantics a particular `jq!` call targets.
//!
//! Selected *per macro invocation* (`jq!(version = "1.6", "...")`), not
//! globally for the whole build: different call sites in the same crate
//! can reasonably want different versions (e.g. while migrating scripts
//! written against an older jq one at a time). Cargo features
//! (`jq-1-6`, `jq-1-7`, `jq-1-8` on `jqc`/`jqc-macros`/`jqc-compiler`)
//! instead control which versions are *available to select at all* --
//! enabling one compiles its version-specific behavior in; a `jq!` call
//! naming a version whose feature isn't enabled is a compile error.
//! `jq-1-8` is on by default, so plain `jq!("...")` (no `version`
//! argument) works out of the box and targets the newest.
//!
//! Add a new version-gated behavior as upstream jq differences are
//! discovered (`todos.md` tracks known ones and the nix-pinned jq
//! binaries -- `nix develop .#jq-1_6` etc -- to verify against) as a
//! method here, rather than scattering `cfg!(feature = ...)` checks ad
//! hoc through the parser/lowering code.

const _: () = assert!(
    cfg!(feature = "jq-1-6") || cfg!(feature = "jq-1-7") || cfg!(feature = "jq-1-8"),
    "jqc-compiler: enable at least one jq-version feature (jq-1-6, jq-1-7, jq-1-8)"
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JqVersion {
    V1_6,
    V1_7,
    V1_8,
}

impl JqVersion {
    pub const ALL: [JqVersion; 3] = [JqVersion::V1_8, JqVersion::V1_7, JqVersion::V1_6];

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "1.6" => Some(Self::V1_6),
            "1.7" => Some(Self::V1_7),
            "1.8" => Some(Self::V1_8),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::V1_6 => "1.6",
            Self::V1_7 => "1.7",
            Self::V1_8 => "1.8",
        }
    }

    /// Whether this version's support is compiled into this build, via
    /// the matching Cargo feature.
    pub fn is_available(self) -> bool {
        match self {
            Self::V1_6 => cfg!(feature = "jq-1-6"),
            Self::V1_7 => cfg!(feature = "jq-1-7"),
            Self::V1_8 => cfg!(feature = "jq-1-8"),
        }
    }

    /// The newest available version, used for `jq!` calls with no
    /// explicit `version` argument. `None` only if every jq-version
    /// feature was somehow disabled, which the `assert!` above already
    /// rules out at compile time.
    pub fn default_available() -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.is_available())
    }

    /// jq 1.6 required an explicit `else` in `if`/`then`/`end`; support
    /// for omitting it (defaulting to `else .`) was added in jq 1.7.
    /// Verified against the real binaries: `nix develop .#jq-1_6 -c jq
    /// 'if .a then 1 end' <<< '{"a":false}'` errors ("Possibly
    /// unterminated 'if' statement"), `nix develop .#jq-1_7` and the
    /// default shell (1.8.2) both accept it.
    pub fn if_requires_else(self) -> bool {
        matches!(self, Self::V1_6)
    }
}
