//! Span-carrying errors shared by every pipeline stage (lex -> parse ->
//! lower -> codegen), with a rustc-style renderer: a `-->` location line,
//! the offending source line with a `|` gutter and a `^^^` underline,
//! optional secondary `note:` locations, and an optional `help:` suggestion.

use std::ops::Range;

/// Char-index range into whichever source string produced the error (not a
/// byte range -- callers that need to slice `str` directly should collect
/// `chars()` first, as [`Diagnostic::render`] does).
pub type Span = Range<usize>;

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub message: String,
    pub span: Option<Span>,
    pub help: Option<String>,
    pub notes: Vec<(Option<Span>, String)>,
}

impl Diagnostic {
    pub fn new(message: impl Into<String>) -> Self {
        Diagnostic {
            message: message.into(),
            span: None,
            help: None,
            notes: Vec::new(),
        }
    }

    pub fn at(span: Span, message: impl Into<String>) -> Self {
        Diagnostic {
            span: Some(span),
            ..Diagnostic::new(message)
        }
    }

    pub fn help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    pub fn note(mut self, span: Span, message: impl Into<String>) -> Self {
        self.notes.push((Some(span), message.into()));
        self
    }

    /// Translate this error's span(s) from "offset within some larger
    /// source that was actually parsed" to "offset within the `len`-char
    /// slice starting at `offset`". Used to turn an error from parsing
    /// `prelude + user_src` back into one addressing `user_src` alone. A
    /// span that doesn't fall entirely inside that window is a bug in the
    /// prelude itself, not something a user's source position can explain,
    /// so it's dropped rather than mistranslated.
    /// Adds `delta` to every span, unconditionally -- for mapping an error
    /// from a sub-source parsed in isolation (e.g. a `\( ... )`
    /// interpolation's body) back into the outer source it was found
    /// inside, where the sub-source started at char offset `delta`.
    pub fn shift(mut self, delta: usize) -> Self {
        let add = |s: Span| s.start + delta..s.end + delta;
        self.span = self.span.map(add);
        for (span, _) in self.notes.iter_mut() {
            *span = span.take().map(add);
        }
        self
    }

    pub fn relocate(mut self, offset: usize, len: usize) -> Self {
        let shift = |s: Span| -> Option<Span> {
            // Entirely before the window (i.e. in the prelude itself) --
            // not explainable in terms of the user's own source, so drop
            // it rather than mistranslate. Anything that overlaps the
            // window at all (including running past its end, e.g. an EOF
            // reached while still inside a wrapper this offset/len doesn't
            // know about) is clamped into range instead of dropped, so a
            // real user-facing error doesn't silently lose its location.
            if s.end <= offset {
                return None;
            }
            let start = s.start.saturating_sub(offset).min(len);
            let end = s.end.saturating_sub(offset).min(len).max(start);
            Some(start..end)
        };
        self.span = self.span.and_then(shift);
        for (span, _) in self.notes.iter_mut() {
            *span = span.take().and_then(shift);
        }
        self
    }

    pub fn render(&self, src: &str) -> String {
        let mut out = format!("error: {}\n", self.message);
        if let Some(span) = &self.span {
            render_span(&mut out, src, span);
        }
        for (span, msg) in &self.notes {
            out.push_str(&format!("note: {msg}\n"));
            if let Some(span) = span {
                render_span(&mut out, src, span);
            }
        }
        if let Some(help) = &self.help {
            out.push_str(&format!("help: {help}\n"));
        }
        out
    }
}

/// Appends a rustc-style `--> line:col` + source-snippet + caret block for
/// `span` (a char-index range into `src`) to `out`.
fn render_span(out: &mut String, src: &str, span: &Span) {
    let chars: Vec<char> = src.chars().collect();
    let start = span.start.min(chars.len());
    let end = span.end.max(start).min(chars.len());

    let mut line = 1usize;
    let mut col = 1usize;
    for &c in chars.iter().take(start) {
        if c == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    let line_start = chars[..start]
        .iter()
        .rposition(|&c| c == '\n')
        .map(|p| p + 1)
        .unwrap_or(0);
    let line_end = chars[line_start..]
        .iter()
        .position(|&c| c == '\n')
        .map(|p| line_start + p)
        .unwrap_or(chars.len());
    let line_text: String = chars[line_start..line_end].iter().collect();

    let underline_len = (end.max(start + 1) - start).min(line_end.max(start + 1) - start);
    let gutter = line.to_string();
    let pad = " ".repeat(gutter.len());

    out.push_str(&format!(" --> {line}:{col}\n"));
    out.push_str(&format!("{pad} |\n"));
    out.push_str(&format!("{gutter} | {line_text}\n"));
    out.push_str(&format!(
        "{pad} | {}{}\n",
        " ".repeat(col - 1),
        "^".repeat(underline_len.max(1))
    ));
}

/// Builtins and prelude-level defs recognized somewhere in the pipeline,
/// used only to power "did you mean ...?" suggestions on an unresolved
/// call -- not an authoritative list of what's actually supported (see the
/// README's "Supported jq subset" for that).
pub const KNOWN_NAMES: &[&str] = &[
    "map", "select", "values", "recurse", "in", "any", "all", "while", "until", "repeat", "not",
    "error", "empty", "length", "keys", "keys_unsorted", "has", "type", "add", "to_entries",
    "from_entries", "with_entries", "sort", "sort_by", "group_by", "unique", "unique_by", "min",
    "max", "min_by", "max_by", "reverse", "range", "first", "last", "floor", "ceil", "round",
    "sqrt", "fabs", "tostring", "tonumber", "explode", "implode", "ascii_downcase",
    "ascii_upcase", "startswith", "endswith", "contains", "ltrimstr", "rtrimstr", "join", "split",
];

/// Returns the entry in `candidates` closest to `target` by Levenshtein
/// edit distance, for "did you mean ...?" help text -- `None` if nothing is
/// close enough to be worth suggesting.
pub fn closest_match<'a>(target: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let max_dist = (target.len() / 3).clamp(1, 4);
    candidates
        .into_iter()
        .map(|c| (c, edit_distance(target, c)))
        .filter(|(_, d)| *d <= max_dist)
        .min_by_key(|(_, d)| *d)
        .map(|(c, _)| c)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            cur[j] = if a[i - 1] == b[j - 1] {
                prev[j - 1]
            } else {
                1 + prev[j - 1].min(prev[j]).min(cur[j - 1])
            };
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

impl std::fmt::Display for Diagnostic {
    /// A source-free fallback (just the message, plus `help` if present) --
    /// callers that have the original source text should prefer
    /// [`Diagnostic::render`] for the full rustc-style output.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)?;
        if let Some(help) = &self.help {
            write!(f, " (help: {help})")?;
        }
        Ok(())
    }
}

impl std::error::Error for Diagnostic {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_points_at_the_right_line_and_column() {
        let src = "1 +\n  .foo bar\n";
        // "bar" starts at line 2, column 8 (1-based): "  .foo bar"
        let bar_start = src.find("bar").unwrap();
        let d = Diagnostic::at(bar_start..bar_start + 3, "unexpected token");
        let rendered = d.render(src);
        assert!(rendered.contains("--> 2:8"), "{rendered}");
        assert!(rendered.contains(".foo bar"), "{rendered}");
        assert!(rendered.contains("^^^"), "{rendered}");
    }

    #[test]
    fn relocate_drops_spans_outside_the_window() {
        let d = Diagnostic::at(0..3, "oops");
        let relocated = d.relocate(10, 5);
        assert!(relocated.span.is_none());
    }

    #[test]
    fn relocate_shifts_spans_inside_the_window() {
        let d = Diagnostic::at(12..15, "oops");
        let relocated = d.relocate(10, 5);
        assert_eq!(relocated.span, Some(2..5));
    }

    #[test]
    fn closest_match_suggests_a_near_miss() {
        assert_eq!(closest_match("selec", ["map", "select", "recurse"]), Some("select"));
        assert_eq!(closest_match("zzzzzzzzzz", ["map", "select"]), None);
    }
}
