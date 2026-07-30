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
