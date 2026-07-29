//! Parsing a unified diff into reviewable, highlighted lines.
//!
//! Highlighting a diff is not the same problem as highlighting a file: each
//! side is a different version of the source, and neither is complete. Rather
//! than try to highlight fragments, each side is reassembled into the text it
//! would be — old lines plus context, new lines plus context — highlighted as
//! one document, and then split back apart. That way a multi-line string or a
//! block comment spanning the hunk is coloured correctly.

use std::path::Path;

use anyhow::{Context, Result};
use two_face::re_exports::syntect::{
    easy::HighlightLines,
    highlighting::Theme,
    html::{IncludeBackground, styled_line_to_highlighted_html},
    parsing::SyntaxSet,
};

use crate::session::{DiffFile, DiffLine, Hunk, LineKind};

/// Splits a unified diff into per-file diffs and highlights them.
pub fn parse(source: &str, theme: &Theme) -> Result<Vec<DiffFile>> {
    let mut files = Vec::new();
    for raw in split_files(source) {
        if let Some(file) = parse_file(&raw, theme)? {
            files.push(file);
        }
    }
    anyhow::ensure!(
        !files.is_empty(),
        "no file diffs found. Expected unified diff output, as from \
         `git diff` or `diff -u`"
    );
    Ok(files)
}

/// Groups the lines of a diff by the file they belong to.
///
/// `diff --git` is the usual marker, but a bare `diff -u` output only has the
/// `---`/`+++` pair, so a new `---` at top level starts a file too.
fn split_files(source: &str) -> Vec<Vec<&str>> {
    let mut files: Vec<Vec<&str>> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    // Inside a hunk, a `---` line is just a removed line of content and must not
    // be mistaken for a file header. A hunk header declares how many lines it
    // covers, so the end of the hunk is known rather than guessed: without that
    // count a `diff -u` concatenation would swallow every file after the first,
    // whose `---` header arrives while the parser still believes it is in a hunk.
    let mut left = Budget::default();

    for line in source.lines() {
        let starts_file = line.starts_with("diff --git ")
            || (!left.in_hunk() && line.starts_with("--- ") && !line.starts_with("--- -"));
        if starts_file && !current.is_empty() {
            files.push(std::mem::take(&mut current));
            left = Budget::default();
        } else if left.in_hunk() {
            left.spend(line);
        }
        if let Some((old, new)) = hunk_lengths(line) {
            left = Budget { old, new };
        }
        current.push(line);
    }
    if !current.is_empty() {
        files.push(current);
    }
    files
}

fn parse_file(lines: &[&str], theme: &Theme) -> Result<Option<DiffFile>> {
    let mut old_path = None;
    let mut new_path = None;
    let mut hunks: Vec<RawHunk> = Vec::new();
    // The `---`/`+++` pair only names files in the header. Once a hunk has
    // started those same prefixes are ordinary removed and added lines — a diff
    // of a Markdown file with a `---` rule, or of a diff itself. The hunk's
    // declared lengths say where it ends, so a header after it is still a header.
    let mut left = Budget::default();

    for line in lines {
        let in_hunk = left.in_hunk();
        if in_hunk && !line.starts_with("@@") {
            left.spend(line);
        }
        if !in_hunk && line.starts_with("--- ") {
            old_path = Some(strip_prefix_dir(line[4..].trim()));
        } else if !in_hunk && line.starts_with("+++ ") {
            new_path = Some(strip_prefix_dir(line[4..].trim()));
        } else if line.starts_with("@@") {
            left = Budget::from(hunk_lengths(line).unwrap_or_default());
            let (old_start, new_start) = hunk_starts(line)
                .with_context(|| format!("could not parse hunk header {line:?}"))?;
            hunks.push(RawHunk {
                header: (*line).to_owned(),
                old_next: old_start,
                new_next: new_start,
                lines: Vec::new(),
            });
        } else if let Some(hunk) = hunks.last_mut() {
            // Within a hunk, the first character is the marker. A completely
            // empty line means an empty context line: some tools drop the
            // trailing space.
            let (kind, text) = match line.chars().next() {
                Some('+') => (LineKind::Added, &line[1..]),
                Some('-') => (LineKind::Removed, &line[1..]),
                Some(' ') => (LineKind::Context, &line[1..]),
                // `\ No newline at end of file` is a note about the previous
                // line, not a line of its own.
                Some('\\') => continue,
                None => (LineKind::Context, ""),
                // Anything else ends the hunk body: trailing git metadata such
                // as a binary notice or the `-- ` of a mail-formatted patch.
                Some(_) => continue,
            };
            let (old, new) = match kind {
                LineKind::Added => {
                    let n = hunk.new_next;
                    hunk.new_next += 1;
                    (None, Some(n))
                }
                LineKind::Removed => {
                    let o = hunk.old_next;
                    hunk.old_next += 1;
                    (Some(o), None)
                }
                LineKind::Context => {
                    let (o, n) = (hunk.old_next, hunk.new_next);
                    hunk.old_next += 1;
                    hunk.new_next += 1;
                    (Some(o), Some(n))
                }
            };
            hunk.lines.push(RawLine {
                kind,
                old,
                new,
                text: text.to_owned(),
            });
        }
    }

    if hunks.is_empty() {
        // A header with no hunks: a pure rename, a mode change, or a binary
        // file. Nothing to review line by line, so it is skipped rather than
        // shown as an empty file.
        return Ok(None);
    }

    // Prefer the new name; a deletion only has the old one.
    let label = new_path
        .filter(|p| *p != "/dev/null")
        .or(old_path)
        .unwrap_or("(unknown)")
        .to_owned();

    let added = hunks
        .iter()
        .flat_map(|h| &h.lines)
        .filter(|l| l.kind == LineKind::Added)
        .count();
    let removed = hunks
        .iter()
        .flat_map(|h| &h.lines)
        .filter(|l| l.kind == LineKind::Removed)
        .count();

    let syntaxes = crate::render::syntaxes();
    let syntax = syntax_for(&label, syntaxes);
    let highlighted = highlight_hunks(&hunks, syntax, theme, syntaxes)?;

    Ok(Some(DiffFile {
        label,
        hunks: highlighted,
        added,
        removed,
        language: syntax.name.clone(),
    }))
}

struct RawHunk {
    header: String,
    old_next: usize,
    new_next: usize,
    lines: Vec<RawLine>,
}

struct RawLine {
    kind: LineKind,
    old: Option<usize>,
    new: Option<usize>,
    text: String,
}

/// Highlights each hunk by reconstructing its two sides.
///
/// Each side is highlighted as one continuous document so that constructs
/// spanning several lines come out right, then the results are matched back to
/// the diff's lines in order.
fn highlight_hunks(
    hunks: &[RawHunk],
    syntax: &two_face::re_exports::syntect::parsing::SyntaxReference,
    theme: &Theme,
    syntaxes: &SyntaxSet,
) -> Result<Vec<Hunk>> {
    let mut out = Vec::with_capacity(hunks.len());

    for hunk in hunks {
        // Old side: removed lines and context. New side: added and context.
        let old_html = highlight_side(hunk, syntax, theme, syntaxes, |kind| {
            kind != LineKind::Added
        })?;
        let new_html = highlight_side(hunk, syntax, theme, syntaxes, |kind| {
            kind != LineKind::Removed
        })?;

        let mut old_iter = old_html.into_iter();
        let mut new_iter = new_html.into_iter();
        let mut lines = Vec::with_capacity(hunk.lines.len());

        for line in &hunk.lines {
            // A context line appears on both sides; either rendering will do,
            // and they agree unless the surrounding versions differ in a way
            // that changes the lexer state.
            let html = match line.kind {
                LineKind::Added => new_iter.next(),
                LineKind::Removed => old_iter.next(),
                LineKind::Context => {
                    let old = old_iter.next();
                    let new = new_iter.next();
                    old.or(new)
                }
            }
            .unwrap_or_else(|| escape_html(&line.text));

            lines.push(DiffLine {
                kind: line.kind,
                old: line.old,
                new: line.new,
                html,
                text: line.text.clone(),
            });
        }

        out.push(Hunk {
            header: hunk.header.clone(),
            lines,
        });
    }
    Ok(out)
}

/// Highlights the lines of one side, returning one HTML string per line.
fn highlight_side(
    hunk: &RawHunk,
    syntax: &two_face::re_exports::syntect::parsing::SyntaxReference,
    theme: &Theme,
    syntaxes: &SyntaxSet,
    keep: impl Fn(LineKind) -> bool,
) -> Result<Vec<String>> {
    let mut highlighter = HighlightLines::new(syntax, theme);
    let mut out = Vec::new();

    for line in hunk.lines.iter().filter(|l| keep(l.kind)) {
        // syntect wants the newline: some syntaxes key off it.
        let with_newline = format!("{}\n", line.text);
        let regions = highlighter
            .highlight_line(&with_newline, syntaxes)
            .context("highlighting a diff line failed")?;
        let mut html = styled_line_to_highlighted_html(&regions, IncludeBackground::No)
            .context("rendering a highlighted diff line failed")?;
        if html.trim().is_empty() {
            // An empty line still needs height.
            html.push_str("&nbsp;");
        }
        out.push(html);
    }
    Ok(out)
}

fn syntax_for<'a>(
    label: &str,
    syntaxes: &'a SyntaxSet,
) -> &'a two_face::re_exports::syntect::parsing::SyntaxReference {
    Path::new(label)
        .extension()
        .and_then(|ext| ext.to_str())
        .and_then(|ext| syntaxes.find_syntax_by_extension(ext))
        .or_else(|| {
            Path::new(label)
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| syntaxes.find_syntax_by_extension(name))
        })
        .unwrap_or_else(|| syntaxes.find_syntax_plain_text())
}

/// A rank for ordering files in a review, lower = shown first.
///
/// A `git diff` lists files alphabetically, which floats low-signal files like
/// `Cargo.lock` and `.config/nextest.toml` to the top. A reviewer wants to see
/// hand-written source first, then the docs and config a human edits, and the
/// generated or lockfile churn last. Callers sort by this with a stable sort so
/// files within a rank keep the patch's original order.
pub fn review_rank(label: &str) -> u8 {
    const SOURCE: u8 = 0;
    const DOC_CONFIG: u8 = 1;
    const LOW_SIGNAL: u8 = 2;

    let path = Path::new(label);
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(label)
        .to_ascii_lowercase();
    let lower = label.to_ascii_lowercase();

    // Lockfiles and generated bundles carry no hand-written signal.
    const LOCKFILES: [&str; 6] = [
        "cargo.lock",
        "package-lock.json",
        "yarn.lock",
        "pnpm-lock.yaml",
        "poetry.lock",
        "go.sum",
    ];
    if LOCKFILES.contains(&name.as_str())
        || name.ends_with(".min.js")
        || name.ends_with(".map")
    {
        return LOW_SIGNAL;
    }

    // Anything under a generated or vendored tree, or a dot-directory of config
    // (`.config/nextest.toml`), is churn a reviewer rarely needs to read.
    const LOW_DIRS: [&str; 6] = [
        "vendor/",
        "node_modules/",
        "dist/",
        "build/",
        "target/",
        "generated/",
    ];
    let in_low_dir = LOW_DIRS
        .iter()
        .any(|d| lower.starts_with(d) || lower.contains(&format!("/{d}")));
    let in_dot_dir = lower.starts_with('.') || lower.contains("/.");
    if in_low_dir || in_dot_dir {
        return LOW_SIGNAL;
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some(
            "rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "go" | "java" | "c" | "h" | "cpp"
            | "cc" | "cxx" | "hpp" | "rb" | "swift" | "kt" | "scala" | "cs" | "php" | "sh",
        ) => SOURCE,
        Some("md" | "toml" | "yaml" | "yml" | "json") => DOC_CONFIG,
        // An unknown or extensionless file is treated as middling: not clearly
        // source, but not known churn either.
        _ => DOC_CONFIG,
    }
}

/// Strips git's `a/` and `b/` prefixes, and any trailing tab-separated
/// timestamp that `diff -u` appends.
fn strip_prefix_dir(path: &str) -> &str {
    let path = path.split('\t').next().unwrap_or(path);
    path.strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .unwrap_or(path)
}

/// What is left of a hunk's declared line counts.
///
/// A hunk ends when both sides are spent. Tracking it this way rather than with a
/// single total means a `---` or `+++` line inside the hunk body is spent as the
/// content line it is, so the file header that follows the hunk is still seen.
#[derive(Default)]
struct Budget {
    old: usize,
    new: usize,
}

impl From<(usize, usize)> for Budget {
    fn from((old, new): (usize, usize)) -> Self {
        Self { old, new }
    }
}

impl Budget {
    fn in_hunk(&self) -> bool {
        self.old > 0 || self.new > 0
    }

    /// Charges a body line to the sides it occupies.
    fn spend(&mut self, line: &str) {
        match line.chars().next() {
            Some('+') => self.new = self.new.saturating_sub(1),
            Some('-') => self.old = self.old.saturating_sub(1),
            // `\ No newline at end of file` annotates the line before it and is
            // not itself a line of either side.
            Some('\\') => {}
            // A context line, including the empty line some tools emit for one.
            _ => {
                self.old = self.old.saturating_sub(1);
                self.new = self.new.saturating_sub(1);
            }
        }
    }
}

/// The line counts a hunk header declares, as `(old_len, new_len)`.
///
/// A hunk body is exhausted when both sides have been accounted for: a removed
/// line spends one old line, an added line one new line, and a context line one
/// of each. Counting them down is what makes the end of a hunk known rather than
/// guessed, so a `---` after it is read as the next file's header.
fn hunk_lengths(header: &str) -> Option<(usize, usize)> {
    if !header.starts_with("@@") {
        return None;
    }
    let body = header.trim_start_matches('@').trim();
    let mut parts = body.split_whitespace();
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    // `-12,7` or, for a single-line range, just `-12`, which means one line.
    let len = |spec: &str| match spec.split_once(',') {
        Some((_, len)) => len.parse::<usize>().ok(),
        None => Some(1),
    };
    Some((len(old)?, len(new)?))
}

/// Reads the starting line numbers out of `@@ -12,7 +12,9 @@`.
fn hunk_starts(header: &str) -> Option<(usize, usize)> {
    let body = header.trim_start_matches('@').trim();
    let mut parts = body.split_whitespace();
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    // `-12,7` or, for a single-line range, just `-12`.
    let first = |spec: &str| spec.split(',').next()?.parse::<usize>().ok();
    // A zero start means an empty file on that side; the first real line is 1.
    Some((first(old)?.max(1), first(new)?.max(1)))
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::theme;

    const SAMPLE: &str = "\
diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,4 +1,5 @@
 fn main() {
-    let x = 1;
+    let x = 2;
+    let y = 3;
 }
";

    #[test]
    fn parses_a_git_diff() {
        let files = parse(SAMPLE, theme("Nord")).unwrap();
        assert_eq!(files.len(), 1);
        let file = &files[0];
        assert_eq!(file.label, "src/lib.rs");
        assert_eq!(file.added, 2);
        assert_eq!(file.removed, 1);
        assert_eq!(file.language, "Rust");
        assert_eq!(file.hunks.len(), 1);
    }

    #[test]
    fn numbers_both_sides_independently() {
        let files = parse(SAMPLE, theme("Nord")).unwrap();
        let lines = &files[0].hunks[0].lines;

        // ` fn main() {` is context: line 1 on both sides.
        assert_eq!((lines[0].old, lines[0].new), (Some(1), Some(1)));
        // `-    let x = 1;` only exists in the old file.
        assert_eq!((lines[1].old, lines[1].new), (Some(2), None));
        // The two added lines only exist in the new one, and are consecutive.
        assert_eq!((lines[2].old, lines[2].new), (None, Some(2)));
        assert_eq!((lines[3].old, lines[3].new), (None, Some(3)));
        // The trailing context line advanced by one on the old side and two on
        // the new, which is the whole point of tracking them separately.
        assert_eq!((lines[4].old, lines[4].new), (Some(3), Some(4)));
    }

    #[test]
    fn every_line_gets_highlighted_html() {
        let files = parse(SAMPLE, theme("Nord")).unwrap();
        for line in &files[0].hunks[0].lines {
            assert!(!line.html.is_empty(), "unhighlighted: {line:?}");
        }
        // The content is highlighted, not just escaped.
        let joined: String = files[0].hunks[0]
            .lines
            .iter()
            .map(|l| l.html.clone())
            .collect();
        assert!(joined.contains("style="), "no highlighting: {joined}");
    }

    #[test]
    fn splits_multiple_files() {
        let two = format!(
            "{SAMPLE}diff --git a/README.md b/README.md\n\
             --- a/README.md\n\
             +++ b/README.md\n\
             @@ -1 +1 @@\n\
             -old\n\
             +new\n"
        );
        let files = parse(&two, theme("Nord")).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[1].label, "README.md");
        assert_eq!(files[1].language, "Markdown");
    }

    #[test]
    fn handles_a_plain_diff_u_with_timestamps() {
        // `diff -u` has no `diff --git` line and appends a timestamp.
        let plain = "--- old.txt\t2026-01-01 00:00:00\n\
                     +++ new.txt\t2026-01-02 00:00:00\n\
                     @@ -1,2 +1,2 @@\n\
                      keep\n\
                     -gone\n\
                     +here\n";
        let files = parse(plain, theme("Nord")).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].label, "new.txt");
    }

    #[test]
    fn a_removed_line_of_dashes_is_not_a_file_header() {
        // Inside a hunk, `---` is content. Treating it as a header would split
        // one file into two and lose the rest of the hunk.
        let tricky = "--- a/x.md\n\
                      +++ b/x.md\n\
                      @@ -1,3 +1,3 @@\n\
                       title\n\
                      --- old rule\n\
                      +++ new rule\n\
                       end\n";
        let files = parse(tricky, theme("Nord")).unwrap();
        assert_eq!(files.len(), 1, "split on a content line");
        assert_eq!(files[0].added, 1);
        assert_eq!(files[0].removed, 1);
    }

    #[test]
    fn a_second_file_is_found_after_a_hunk_ends() {
        // `diff -u` output has no `diff --git` line, so the only marker for the
        // second file is its `---`. A parser that stays "in a hunk" forever after
        // the first `@@` swallows every file but the first: the hunk's declared
        // lengths are what say it has ended.
        let two = "--- a/one.md\n\
                   +++ b/one.md\n\
                   @@ -1,2 +1,2 @@\n\
                    kept\n\
                   -was\n\
                   +now\n\
                   --- a/two.md\n\
                   +++ b/two.md\n\
                   @@ -1,1 +1,2 @@\n\
                    kept\n\
                   +added\n";
        let files = parse(two, theme("Nord")).unwrap();
        assert_eq!(
            files.iter().map(|f| f.label.as_str()).collect::<Vec<_>>(),
            ["one.md", "two.md"],
            "the second file was swallowed by the first hunk"
        );
        assert_eq!((files[1].added, files[1].removed), (1, 0));
    }

    #[test]
    fn hunk_lengths_reads_both_range_forms() {
        assert_eq!(hunk_lengths("@@ -1,2 +1,3 @@"), Some((2, 3)));
        // A single-line range omits the count.
        assert_eq!(hunk_lengths("@@ -7 +9 @@ fn f()"), Some((1, 1)));
        assert_eq!(hunk_lengths(" not a hunk"), None);
    }

    #[test]
    fn a_no_newline_marker_does_not_end_a_hunk_early() {
        // `\ No newline at end of file` annotates the line before it. Charged as
        // a line of its own it would end the hunk one line short, so a following
        // content line would be read as the start of another file.
        let patch = "--- a/x.txt\n\
                     +++ b/x.txt\n\
                     @@ -1,2 +1,2 @@\n\
                     -old\n\
                     \\ No newline at end of file\n\
                     +new\n\
                     \\ No newline at end of file\n\
                      tail\n";
        let files = parse(patch, theme("Nord")).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!((files[0].added, files[0].removed), (1, 1));
    }

    #[test]
    fn skips_headers_with_no_hunks() {
        // A pure rename has nothing to review line by line.
        let rename = "diff --git a/old b/new\n\
                      similarity index 100%\n\
                      rename from old\n\
                      rename to new\n";
        assert!(parse(rename, theme("Nord")).is_err());
    }

    #[test]
    fn rejects_input_that_is_not_a_diff() {
        let error = parse("just some text\nnot a diff\n", theme("Nord")).unwrap_err();
        assert!(error.to_string().contains("unified diff"), "{error}");
    }

    #[test]
    fn a_new_file_starts_numbering_at_one() {
        // git writes `@@ -0,0 +1,2 @@` for a new file; line 0 does not exist.
        let created = "--- /dev/null\n\
                       +++ b/new.rs\n\
                       @@ -0,0 +1,2 @@\n\
                       +fn a() {}\n\
                       +fn b() {}\n";
        let files = parse(created, theme("Nord")).unwrap();
        assert_eq!(files[0].label, "new.rs");
        assert_eq!(files[0].hunks[0].lines[0].new, Some(1));
    }

    #[test]
    fn a_deleted_file_keeps_its_old_name() {
        let deleted = "--- a/gone.rs\n\
                       +++ /dev/null\n\
                       @@ -1,1 +0,0 @@\n\
                       -fn gone() {}\n";
        let files = parse(deleted, theme("Nord")).unwrap();
        assert_eq!(files[0].label, "gone.rs");
    }

    #[test]
    fn no_newline_marker_is_not_a_line() {
        let marker = "--- a/x\n\
                      +++ b/x\n\
                      @@ -1 +1 @@\n\
                      -a\n\
                      \\ No newline at end of file\n\
                      +b\n";
        let files = parse(marker, theme("Nord")).unwrap();
        assert_eq!(files[0].hunks[0].lines.len(), 2);
    }

    #[test]
    fn hunk_starts_parses_both_range_forms() {
        assert_eq!(hunk_starts("@@ -12,7 +14,9 @@"), Some((12, 14)));
        assert_eq!(hunk_starts("@@ -3 +5 @@ fn f()"), Some((3, 5)));
        assert_eq!(hunk_starts("not a hunk"), None);
    }

    #[test]
    fn source_ranks_before_docs_before_lockfiles() {
        let source = review_rank("src/main.rs");
        let doc = review_rank("README.md");
        let lock = review_rank("Cargo.lock");
        assert!(source < doc, "source should rank before docs/config");
        assert!(doc < lock, "docs/config should rank before lockfiles");
    }

    #[test]
    fn lockfiles_and_generated_config_are_low_signal() {
        let lock = review_rank("Cargo.lock");
        assert_eq!(review_rank("package-lock.json"), lock);
        assert_eq!(review_rank(".config/nextest.toml"), lock);
        assert_eq!(review_rank("yarn.lock"), lock);
        assert_eq!(review_rank("go.sum"), lock);
        assert_eq!(review_rank("node_modules/foo/index.js"), lock);
        assert_eq!(review_rank("dist/bundle.min.js"), lock);
    }

    #[test]
    fn a_real_source_file_ranks_highest() {
        let source = review_rank("src/diff.rs");
        assert!(source < review_rank("Cargo.toml"));
        assert!(source < review_rank("Cargo.lock"));
        // A `.toml` a human edits still outranks a lockfile.
        assert!(review_rank("Cargo.toml") < review_rank("Cargo.lock"));
    }

    #[test]
    fn sort_is_stable_within_a_rank() {
        // Two source files: their relative order must be preserved.
        let mut labels = vec![
            "Cargo.lock",
            "src/zebra.rs",
            "src/alpha.rs",
            ".config/nextest.toml",
            "README.md",
        ];
        labels.sort_by_key(|l| review_rank(l));
        assert_eq!(
            labels,
            [
                "src/zebra.rs",
                "src/alpha.rs",
                "README.md",
                "Cargo.lock",
                ".config/nextest.toml",
            ],
            "stable sort must keep zebra before alpha"
        );
    }
}
