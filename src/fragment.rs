//! Deep-link fragment encoding and decoding.
//!
//! The page supports three kinds of URL fragments for deep-linking:
//!
//! - `#f-<path>` — file section anchors (existing, lossy, for the nav bar and
//!   guided-review links). Left alone; this module does not produce them.
//! - `#comment-<id>` — a specific comment on a line or range.
//! - `#sel-<base64url(path)>-<side>-<start>[-<end>]` — a line or range
//!   selection, deep-linkable and highlighted on load.
//!
//! The `sel-` prefix uses base64url (no padding) for the file path, making it:
//! - Round-trippable: every byte sequence maps to a unique encoding.
//! - Collision-safe with file anchors (`f-` uses only lowercase + `-`; base64url
//!   includes uppercase and `_`).
//! - CSS-safe: only `[A-Za-z0-9_-]` in the encoded path segment.
//!
//! The side is `old` or `new`. The line numbers are decimal. A range is
//! `<start>-<end>` where start ≤ end.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Encodes a file path for use in a `sel-` fragment.
pub fn encode_path(path: &str) -> String {
    URL_SAFE_NO_PAD.encode(path.as_bytes())
}

/// Decodes a base64url-encoded file path from a `sel-` fragment.
/// Maximum safe integer in JavaScript (2^53 - 1). Line numbers beyond this
/// cannot be reliably represented in the JS parser, so the Rust parser rejects
/// them for parity.
const MAX_SAFE_INTEGER: usize = 9_007_199_254_740_991;

/// Maximum length of an encoded path segment in a fragment. Limits the work
/// done by base64 decode and prevents unbounded allocation from a hostile URL.
const MAX_ENCODED_PATH_LEN: usize = 4096;

/// Maximum length of the entire fragment string. Prevents unbounded parsing work.
const MAX_FRAGMENT_LEN: usize = 8192;

/// Decodes a base64url-encoded file path from a `sel-` fragment.
/// Returns `None` for empty paths or paths that exceed the length limit.
pub fn decode_path(encoded: &str) -> Option<String> {
    if encoded.is_empty() || encoded.len() > MAX_ENCODED_PATH_LEN {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(encoded).ok()?;
    let s = String::from_utf8(bytes).ok()?;
    if s.is_empty() {
        return None;
    }
    Some(s)
}

/// Validates a line number is within safe bounds (non-zero, ≤ MAX_SAFE_INTEGER).
fn safe_line(n: usize) -> bool {
    n > 0 && n <= MAX_SAFE_INTEGER
}

/// Builds the canonical fragment for a single line selection.
pub fn line_fragment(path: &str, side: &str, line: usize) -> String {
    format!("sel-{}-{}-{}", encode_path(path), side, line)
}

/// Builds the canonical fragment for a range selection.
pub fn range_fragment(path: &str, side: &str, start: usize, end: usize) -> String {
    let (lo, hi) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    if lo == hi {
        line_fragment(path, side, lo)
    } else {
        format!("sel-{}-{}-{}-{}", encode_path(path), side, lo, hi)
    }
}

/// Builds the canonical fragment for a comment anchor.
pub fn comment_fragment(id: u64) -> String {
    format!("comment-{id}")
}

/// A parsed selection fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub path: String,
    pub side: String,
    pub start: usize,
    pub end: usize,
}

/// Parses a `sel-...` fragment (without the leading `#`).
///
/// Returns `None` for anything that does not match the expected format.
/// Rejects: zero line numbers, numbers > MAX_SAFE_INTEGER, empty paths,
/// oversized fragments/paths.
pub fn parse_selection(fragment: &str) -> Option<Selection> {
    if fragment.len() > MAX_FRAGMENT_LEN {
        return None;
    }
    let rest = fragment.strip_prefix("sel-")?;
    let (head, last) = rest.rsplit_once('-')?;
    let last: usize = last.parse().ok()?;
    if !safe_line(last) {
        return None;
    }

    let (head, previous) = head.rsplit_once('-')?;
    if previous == "old" || previous == "new" {
        return Some(Selection {
            path: decode_path(head)?,
            side: previous.to_owned(),
            start: last,
            end: last,
        });
    }

    let start: usize = previous.parse().ok()?;
    if !safe_line(start) {
        return None;
    }
    let (path_encoded, side) = head.rsplit_once('-')?;
    if side != "old" && side != "new" {
        return None;
    }

    Some(Selection {
        path: decode_path(path_encoded)?,
        side: side.to_owned(),
        start: start.min(last),
        end: start.max(last),
    })
}

/// Parses a `comment-<id>` fragment (without the leading `#`).
pub fn parse_comment(fragment: &str) -> Option<u64> {
    fragment.strip_prefix("comment-")?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_simple_path() {
        let frag = line_fragment("src/lib.rs", "new", 5);
        let sel = parse_selection(&frag).unwrap();
        assert_eq!(sel.path, "src/lib.rs");
        assert_eq!(sel.side, "new");
        assert_eq!(sel.start, 5);
        assert_eq!(sel.end, 5);
    }

    #[test]
    fn round_trips_a_range() {
        let frag = range_fragment("src/lib.rs", "old", 3, 7);
        let sel = parse_selection(&frag).unwrap();
        assert_eq!(sel.path, "src/lib.rs");
        assert_eq!(sel.side, "old");
        assert_eq!(sel.start, 3);
        assert_eq!(sel.end, 7);
    }

    #[test]
    fn round_trips_a_path_with_special_chars() {
        let frag = line_fragment("src/my-file.test.rs", "new", 42);
        let sel = parse_selection(&frag).unwrap();
        assert_eq!(sel.path, "src/my-file.test.rs");
        assert_eq!(sel.start, 42);
    }

    #[test]
    fn round_trips_a_path_with_dashes() {
        // Dashes in the path must not confuse the parser, because the path is
        // base64url-encoded (only [A-Za-z0-9_-]) and parsed from the right.
        let frag = range_fragment("a-b/c-d.rs", "new", 1, 10);
        let sel = parse_selection(&frag).unwrap();
        assert_eq!(sel.path, "a-b/c-d.rs");
        assert_eq!(sel.start, 1);
        assert_eq!(sel.end, 10);
    }

    #[test]
    fn backwards_range_is_normalised() {
        let frag = range_fragment("x.rs", "new", 10, 3);
        let sel = parse_selection(&frag).unwrap();
        assert_eq!(sel.start, 3);
        assert_eq!(sel.end, 10);
    }

    #[test]
    fn degenerate_range_becomes_single_line() {
        let frag = range_fragment("x.rs", "new", 5, 5);
        // Should be the same as a single line fragment.
        assert_eq!(frag, line_fragment("x.rs", "new", 5));
        let sel = parse_selection(&frag).unwrap();
        assert_eq!(sel.start, 5);
        assert_eq!(sel.end, 5);
    }

    #[test]
    fn invalid_fragments_return_none() {
        assert_eq!(parse_selection(""), None);
        assert_eq!(parse_selection("sel-"), None);
        assert_eq!(parse_selection("sel-abc-sideways-5"), None);
        assert_eq!(parse_selection("f-src-lib-rs"), None);
        assert_eq!(parse_selection("comment-3"), None);
    }

    #[test]
    fn comment_fragment_round_trips() {
        let frag = comment_fragment(42);
        assert_eq!(frag, "comment-42");
        assert_eq!(parse_comment(&frag), Some(42));
    }

    #[test]
    fn comment_fragment_rejects_non_numeric() {
        assert_eq!(parse_comment("comment-abc"), None);
        assert_eq!(parse_comment("comment-"), None);
        assert_eq!(parse_comment("sel-abc-new-5"), None);
    }

    #[test]
    fn path_encoding_with_a_hyphen_round_trips() {
        let path = "src/¾x.rs";
        let encoded = encode_path(path);
        assert_eq!(encoded, "c3JjL8K-eC5ycw");
        assert!(encoded.contains('-'));

        let line = parse_selection(&line_fragment(path, "new", 7)).unwrap();
        assert_eq!(line.path, path);
        assert_eq!((line.start, line.end), (7, 7));

        let range = parse_selection(&range_fragment(path, "old", 2, 5)).unwrap();
        assert_eq!(range.path, path);
        assert_eq!(range.side, "old");
        assert_eq!((range.start, range.end), (2, 5));
    }

    #[test]
    fn does_not_collide_with_file_anchors() {
        // File anchors are `f-<lowercase-dashes>`. A sel fragment for the same
        // path must be distinguishable.
        let file_anchor = "f-src-lib-rs"; // the lossy existing form
        let sel = line_fragment("src/lib.rs", "new", 1);
        assert_ne!(sel, file_anchor);
        // And the file anchor does not parse as a selection.
        assert_eq!(parse_selection(file_anchor), None);
    }

    /// Cross-language known vectors: these exact fragment strings must be
    /// producible by any implementation (JS, Rust) so deep-links are portable.
    /// The paths are chosen to exercise base64url characters including `-` and
    /// `_`, and multi-byte UTF-8.
    #[test]
    fn cross_language_known_vectors() {
        // Vector 1: simple ASCII path.
        assert_eq!(
            line_fragment("src/lib.rs", "new", 5),
            "sel-c3JjL2xpYi5ycw-new-5"
        );
        // Vector 2: UTF-8 path whose base64url encoding contains `-`.
        assert_eq!(
            line_fragment("src/¾x.rs", "old", 12),
            "sel-c3JjL8K-eC5ycw-old-12"
        );
        // Vector 3: range with a path containing underscores (bytes produce `_`).
        assert_eq!(
            range_fragment("src/my_mod/test.rs", "new", 3, 7),
            "sel-c3JjL215X21vZC90ZXN0LnJz-new-3-7"
        );
        // Vector 4: UTF-8 multi-byte path.
        assert_eq!(
            line_fragment("docs/über.md", "new", 1),
            "sel-ZG9jcy_DvGJlci5tZA-new-1"
        );
        // All vectors round-trip.
        for (path, side, start, end) in [
            ("src/lib.rs", "new", 5, 5),
            ("a-b/c-d.rs", "old", 12, 12),
            ("src/my_mod/test.rs", "new", 3, 7),
            ("docs/über.md", "new", 1, 1),
        ] {
            let frag = if start == end {
                line_fragment(path, side, start)
            } else {
                range_fragment(path, side, start, end)
            };
            let sel = parse_selection(&frag).unwrap();
            assert_eq!(sel.path, path, "round-trip failed for {path}");
            assert_eq!(sel.side, side);
            assert_eq!(sel.start, start);
            assert_eq!(sel.end, end);
        }
    }

    #[test]
    fn rejects_huge_line_numbers() {
        // A fragment with impossibly large line numbers should not panic.
        let huge = format!("sel-{}-new-{}", encode_path("x.rs"), u64::MAX);
        // usize::MAX won't parse as usize on 32-bit, but on 64-bit it would.
        // Either way the fragment module should not panic.
        let _ = parse_selection(&huge);

        // Overflow: the encoded number is larger than usize can hold.
        let overflow = format!("sel-{}-new-99999999999999999999", encode_path("x.rs"),);
        assert_eq!(parse_selection(&overflow), None);
    }

    #[test]
    fn rejects_line_numbers_above_max_safe_integer() {
        // Numbers > Number.MAX_SAFE_INTEGER (2^53 - 1) must be rejected for JS parity.
        let max_safe = 9_007_199_254_740_991usize;
        let beyond = max_safe + 1;
        let frag = format!("sel-{}-new-{}", encode_path("x.rs"), beyond);
        assert_eq!(
            parse_selection(&frag),
            None,
            "line > MAX_SAFE_INTEGER should be rejected"
        );
        // MAX_SAFE_INTEGER itself is accepted.
        let frag_ok = format!("sel-{}-new-{}", encode_path("x.rs"), max_safe);
        let sel = parse_selection(&frag_ok).unwrap();
        assert_eq!(sel.start, max_safe);
    }

    #[test]
    fn rejects_empty_path() {
        // An empty encoded path must not produce a valid selection.
        // Empty base64 encoding = "" which decodes to empty bytes.
        assert_eq!(parse_selection("sel--new-5"), None);
    }

    #[test]
    fn rejects_oversized_fragment() {
        // A fragment longer than MAX_FRAGMENT_LEN (8192) must be rejected.
        let long_path = "a".repeat(7000);
        let frag = format!("sel-{}-new-1", encode_path(&long_path));
        assert!(frag.len() > 8192);
        assert_eq!(
            parse_selection(&frag),
            None,
            "oversized fragment should be rejected"
        );
    }

    #[test]
    fn rejects_oversized_encoded_path() {
        // An encoded path segment longer than MAX_ENCODED_PATH_LEN must be rejected.
        let long_path = "x".repeat(4000);
        let encoded = encode_path(&long_path);
        assert!(encoded.len() > 4096);
        let frag = format!("sel-{}-new-1", encoded);
        assert_eq!(
            parse_selection(&frag),
            None,
            "oversized path should be rejected"
        );
    }
}
