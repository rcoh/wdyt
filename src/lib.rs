//! wdyt: let coding agents show their work.
//!
//! An agent creates a *session* holding something to look at (highlighted
//! source, rendered markdown, a static directory, or a live server it started),
//! wdyt sends the user a link over a webhook, and the page carries a reply
//! box so the user can send one note back.

pub mod client;
pub mod config;
pub mod diff;
pub mod fragment;
pub mod guided;
pub mod notify;
pub mod ports;
pub mod render;
pub mod server;
pub mod session;
pub mod ui;

/// Test-visible JS constants. These are the same strings rendered into pages,
/// exposed so integration tests can assert on their structure without parsing HTML.
pub mod ui_js {
    /// The doc/brief comment script as a static string.
    pub const DOC_COMMENT_JS_STR: &str = super::ui::DOC_COMMENT_JS;
    /// The code/diff comment script as a static string.
    pub const COMMENT_JS_STR: &str = super::ui::COMMENT_JS;
    /// The navigation enhancement script (jump-to-top + sidebar tracking).
    pub const NAV_JS_STR: &str = super::ui::NAV_JS;
}
pub mod zellij_origin;

/// A stable, URL-safe anchor for a file label.
///
/// Produces `f-<lowercased-path-with-non-alphanumerics-replaced-by-dashes>`.
/// This is the same anchor used for navigation in the rendered page and for
/// `#f-<path>` links in guided-review briefs.
///
/// # Examples
///
/// ```
/// assert_eq!(wdyt::file_anchor("src/main.rs"), "f-src-main-rs");
/// assert_eq!(wdyt::file_anchor("A B.md"), "f-a-b-md");
/// ```
pub fn file_anchor(label: &str) -> String {
    let mut out = String::with_capacity(label.len() + 2);
    out.push('f');
    out.push('-');
    for ch in label.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push('-');
        }
    }
    out
}

/// Largest line number a file-line anchor may carry, mirroring
/// [`fragment`](crate::fragment)'s bound so a hand-written anchor and a
/// JS-produced `sel-` fragment reject the same out-of-range values. This is
/// `2^53 - 1`, the largest integer JavaScript can represent exactly.
const MAX_SAFE_LINE: usize = 9_007_199_254_740_991;

/// Longest file-line anchor accepted, guarding the parser against a hostile URL.
const MAX_FILE_LINE_ANCHOR_LEN: usize = 8192;

/// A hand-writable, GitHub-style line-range anchor for a file, produced by
/// appending `-L<start>` (and optionally `-L<end>`) to [`file_anchor`].
///
/// This mirrors GitHub's `#L10` / `#L10-L20` blob-URL scheme so an agent can
/// point a guided-review reader at a specific span from a `--brief`, e.g.
/// `#f-src-lib-rs-L10` or `#f-src-lib-rs-L10-L20`. Resolution and highlighting
/// happen client-side (see the fragment scripts in `ui`); this type is the
/// canonical Rust formatter/parser, tested for parity with that JS.
///
/// The `anchor` field is the lossy `f-<path>` prefix, not the original path:
/// [`file_anchor`] is not reversible, so a parsed anchor is matched against
/// live rows by recomputing `file_anchor` for each candidate path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileLineAnchor {
    /// The `f-<path>` prefix, exactly as [`file_anchor`] would produce it.
    pub anchor: String,
    /// First line of the span (1-based, `start <= end`).
    pub start: usize,
    /// Last line of the span (1-based, `start <= end`).
    pub end: usize,
}

/// The anchor for a single line of a file, e.g. `f-src-lib-rs-L10`.
///
/// # Examples
///
/// ```
/// assert_eq!(wdyt::file_line_anchor("src/lib.rs", 10), "f-src-lib-rs-L10");
/// ```
pub fn file_line_anchor(label: &str, line: usize) -> String {
    format!("{}-L{line}", file_anchor(label))
}

/// The anchor for a line range of a file, e.g. `f-src-lib-rs-L10-L20`.
///
/// A reversed range is normalised to `start <= end`, and a degenerate range
/// (`start == end`) collapses to the single-line form so the two always agree.
///
/// # Examples
///
/// ```
/// assert_eq!(wdyt::file_range_anchor("src/lib.rs", 10, 20), "f-src-lib-rs-L10-L20");
/// // Reversed input is normalised.
/// assert_eq!(wdyt::file_range_anchor("src/lib.rs", 20, 10), "f-src-lib-rs-L10-L20");
/// // A single-line range is the single-line form.
/// assert_eq!(wdyt::file_range_anchor("src/lib.rs", 7, 7), "f-src-lib-rs-L7");
/// ```
pub fn file_range_anchor(label: &str, start: usize, end: usize) -> String {
    let (lo, hi) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    if lo == hi {
        file_line_anchor(label, lo)
    } else {
        format!("{}-L{lo}-L{hi}", file_anchor(label))
    }
}

/// Parses a hand-written `f-<path>-L<start>[-L<end>]` anchor.
///
/// Returns `None` for anything that is not a file-line anchor, including a bare
/// `f-<path>` file anchor (which the page handles natively) and out-of-range or
/// non-positive line numbers.
///
/// The `-L` marker is disambiguated from the dashes in `<path>` by parsing from
/// the right, exactly as [`fragment::parse_selection`](crate::fragment::parse_selection)
/// does for `sel-` fragments: because [`file_anchor`] lowercases every character,
/// the path portion can never contain an uppercase `L`, so a trailing
/// `-L<digits>` is unambiguously a line marker rather than part of the path.
///
/// # Examples
///
/// ```
/// let a = wdyt::parse_file_line_anchor("f-src-lib-rs-L10-L20").unwrap();
/// assert_eq!((a.anchor.as_str(), a.start, a.end), ("f-src-lib-rs", 10, 20));
/// // A path segment that is itself digits is not confused with a line marker.
/// let b = wdyt::parse_file_line_anchor("f-src-2-rs-L5").unwrap();
/// assert_eq!((b.anchor.as_str(), b.start, b.end), ("f-src-2-rs", 5, 5));
/// // A bare file anchor is not a line anchor.
/// assert_eq!(wdyt::parse_file_line_anchor("f-src-lib-rs"), None);
/// ```
pub fn parse_file_line_anchor(anchor: &str) -> Option<FileLineAnchor> {
    if anchor.len() > MAX_FILE_LINE_ANCHOR_LEN || !anchor.starts_with("f-") {
        return None;
    }
    // Peel the trailing `-L<digits>` (the end, or the single line).
    let (head, end_digits) = anchor.rsplit_once("-L")?;
    let end: usize = end_digits.parse().ok()?;
    if !(1..=MAX_SAFE_LINE).contains(&end) {
        return None;
    }
    // A second `-L<digits>` immediately before it makes this a range; anything
    // else means the peeled marker was the single line.
    let (prefix, start) = match head.rsplit_once("-L") {
        Some((prefix, start_digits)) => {
            let start: usize = start_digits.parse().ok()?;
            if !(1..=MAX_SAFE_LINE).contains(&start) {
                return None;
            }
            (prefix, start)
        }
        None => (head, end),
    };
    // The remaining prefix must still be a file anchor.
    if !prefix.starts_with("f-") {
        return None;
    }
    Some(FileLineAnchor {
        anchor: prefix.to_owned(),
        start: start.min(end),
        end: start.max(end),
    })
}

/// Validates that a set of file labels do not produce duplicate anchors.
///
/// Returns `Ok(())` if all anchors are unique. Returns `Err` naming both
/// colliding labels and their shared anchor when a collision is found.
///
/// Call this before creating a session: a collision in anchors means the page
/// cannot navigate to both files, and the guided-review links in `--brief` would
/// be ambiguous.
pub fn validate_file_anchors(labels: &[String]) -> Result<(), String> {
    use std::collections::HashMap;
    let mut seen: HashMap<String, &str> = HashMap::with_capacity(labels.len());
    for label in labels {
        let anchor = file_anchor(label);
        if let Some(previous) = seen.get(&anchor) {
            return Err(format!(
                "anchor collision: {:?} and {:?} both produce #{anchor}",
                previous, label
            ));
        }
        seen.insert(anchor, label);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_anchor_replaces_non_alphanumeric() {
        assert_eq!(file_anchor("src/main.rs"), "f-src-main-rs");
        assert_eq!(file_anchor("A B.md"), "f-a-b-md");
        assert_eq!(file_anchor("a-b/c-d.rs"), "f-a-b-c-d-rs");
    }

    #[test]
    fn file_anchor_is_deterministic() {
        assert_eq!(file_anchor("x/y.rs"), file_anchor("x/y.rs"));
    }

    #[test]
    fn file_anchor_handles_empty_label() {
        assert_eq!(file_anchor(""), "f-");
    }

    #[test]
    fn file_anchor_handles_unicode() {
        // Unicode non-ASCII becomes dashes.
        assert_eq!(file_anchor("docs/über.md"), "f-docs--ber-md");
    }

    #[test]
    fn file_anchor_handles_consecutive_separators() {
        assert_eq!(file_anchor("a//b..c"), "f-a--b--c");
    }

    #[test]
    fn file_line_anchor_appends_capital_l_marker() {
        assert_eq!(file_line_anchor("src/lib.rs", 10), "f-src-lib-rs-L10");
        assert_eq!(file_line_anchor("A B.md", 1), "f-a-b-md-L1");
    }

    #[test]
    fn file_range_anchor_normalises_and_collapses() {
        assert_eq!(
            file_range_anchor("src/lib.rs", 10, 20),
            "f-src-lib-rs-L10-L20"
        );
        // Reversed input is normalised to start <= end.
        assert_eq!(
            file_range_anchor("src/lib.rs", 20, 10),
            "f-src-lib-rs-L10-L20"
        );
        // A degenerate range collapses to the single-line form.
        assert_eq!(file_range_anchor("src/lib.rs", 7, 7), "f-src-lib-rs-L7");
    }

    #[test]
    fn parse_file_line_anchor_reads_single_and_range() {
        let single = parse_file_line_anchor("f-src-lib-rs-L10").unwrap();
        assert_eq!(single.anchor, "f-src-lib-rs");
        assert_eq!((single.start, single.end), (10, 10));

        let range = parse_file_line_anchor("f-src-lib-rs-L10-L20").unwrap();
        assert_eq!(range.anchor, "f-src-lib-rs");
        assert_eq!((range.start, range.end), (10, 20));
    }

    #[test]
    fn parse_file_line_anchor_disambiguates_dashed_paths() {
        // The path `a-b/c-d.rs` normalises to `f-a-b-c-d-rs`, full of dashes.
        // Parsing from the right on the capital-L marker keeps the path intact.
        let a = parse_file_line_anchor("f-a-b-c-d-rs-L5").unwrap();
        assert_eq!(a.anchor, "f-a-b-c-d-rs");
        assert_eq!((a.start, a.end), (5, 5));

        let b = parse_file_line_anchor("f-a-b-c-d-rs-L5-L9").unwrap();
        assert_eq!(b.anchor, "f-a-b-c-d-rs");
        assert_eq!((b.start, b.end), (5, 9));

        // A path segment that is itself digits must not be mistaken for a line.
        let c = parse_file_line_anchor("f-src-2-rs-L3").unwrap();
        assert_eq!(c.anchor, "f-src-2-rs");
        assert_eq!((c.start, c.end), (3, 3));
    }

    #[test]
    fn parse_file_line_anchor_round_trips_with_formatters() {
        for (label, start, end) in [
            ("src/lib.rs", 10usize, 10usize),
            ("src/lib.rs", 10, 20),
            ("a-b/c-d.rs", 1, 99),
            ("weird L path.rs", 4, 4),
        ] {
            let frag = file_range_anchor(label, start, end);
            let parsed = parse_file_line_anchor(&frag).unwrap();
            assert_eq!(parsed.anchor, file_anchor(label), "for {frag}");
            assert_eq!((parsed.start, parsed.end), (start, end), "for {frag}");
        }
    }

    #[test]
    fn parse_file_line_anchor_normalises_reversed_range() {
        let parsed = parse_file_line_anchor("f-src-lib-rs-L20-L10").unwrap();
        assert_eq!((parsed.start, parsed.end), (10, 20));
    }

    #[test]
    fn parse_file_line_anchor_rejects_non_line_anchors() {
        // A bare file anchor is handled natively, not as a line anchor.
        assert_eq!(parse_file_line_anchor("f-src-lib-rs"), None);
        // Not a file anchor at all.
        assert_eq!(parse_file_line_anchor("comment-3"), None);
        assert_eq!(parse_file_line_anchor("sel-abc-new-5"), None);
        // Zero and non-numeric line numbers are rejected.
        assert_eq!(parse_file_line_anchor("f-src-lib-rs-L0"), None);
        assert_eq!(parse_file_line_anchor("f-src-lib-rs-Lx"), None);
        // A string that does not even start with the file-anchor prefix.
        assert_eq!(parse_file_line_anchor("x-src-lib-rs-L5"), None);
    }

    #[test]
    fn parse_file_line_anchor_rejects_out_of_range_lines() {
        let beyond = MAX_SAFE_LINE + 1;
        assert_eq!(
            parse_file_line_anchor(&format!("f-src-lib-rs-L{beyond}")),
            None
        );
        // MAX_SAFE_LINE itself is accepted.
        let ok = parse_file_line_anchor(&format!("f-src-lib-rs-L{MAX_SAFE_LINE}")).unwrap();
        assert_eq!(ok.start, MAX_SAFE_LINE);
    }

    #[test]
    fn validate_file_anchors_accepts_distinct() {
        let labels = vec![
            "src/main.rs".to_owned(),
            "src/lib.rs".to_owned(),
            "README.md".to_owned(),
        ];
        assert!(validate_file_anchors(&labels).is_ok());
    }

    #[test]
    fn validate_file_anchors_rejects_exact_duplicates() {
        let labels = vec!["src/main.rs".to_owned(), "src/main.rs".to_owned()];
        let err = validate_file_anchors(&labels).unwrap_err();
        assert!(
            err.contains("src/main.rs"),
            "error should name the label: {err}"
        );
        assert!(
            err.contains("f-src-main-rs"),
            "error should name the anchor: {err}"
        );
    }

    #[test]
    fn validate_file_anchors_rejects_normalization_collisions() {
        // a-b.rs and a/b.rs both normalize to f-a-b-rs
        let labels = vec!["a-b.rs".to_owned(), "a/b.rs".to_owned()];
        let err = validate_file_anchors(&labels).unwrap_err();
        assert!(
            err.contains("a-b.rs"),
            "error should name first label: {err}"
        );
        assert!(
            err.contains("a/b.rs"),
            "error should name second label: {err}"
        );
        assert!(
            err.contains("f-a-b-rs"),
            "error should name the anchor: {err}"
        );
    }

    #[test]
    fn validate_file_anchors_empty_is_ok() {
        assert!(validate_file_anchors(&[]).is_ok());
    }

    #[test]
    fn validate_file_anchors_single_is_ok() {
        assert!(validate_file_anchors(&["x.rs".to_owned()]).is_ok());
    }
}
