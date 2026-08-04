//! Parsing guided reviews that interleave prose, diff ranges, and current code.

use std::path::{Component, Path};

use anyhow::{Context as _, Result, bail, ensure};
use comrak::{Arena, Options, nodes::NodeValue, parse_document};
use serde::Deserialize;

use crate::session::{DiffFile, GuidedBlock, Hunk, LineKind, Side};

/// Maximum number of source lines one directive may select.
const MAX_RANGE_LINES: usize = 500;
/// Maximum number of rendered blocks in one guide.
const MAX_BLOCKS: usize = 100;
/// Keep the expansion payload under the same per-file bound as raw diffs.
const MAX_SOURCE_BYTES: u64 = 4 * 1024 * 1024;

/// Parses a guided-review Markdown document.
///
/// Top-level `wdyt-diff` fences select from `patch`, while top-level
/// `wdyt-code` fences select files beneath `workspace`. All other Markdown is
/// rendered with the same safe Comrak and syntax-highlighting configuration as
/// docs content.
pub fn parse(
    source: &str,
    patch: &str,
    workspace: &Path,
    theme_name: &str,
) -> Result<Vec<GuidedBlock>> {
    let directives = find_directives(source)?;
    let theme = crate::render::theme(theme_name);
    let mut diff_files = directives
        .iter()
        .any(|directive| directive.kind == DirectiveKind::Diff)
        .then(|| crate::diff::parse(patch, theme))
        .transpose()
        .context("parsing the guided review patch")?;
    if let Some(files) = diff_files.as_mut() {
        crate::diff::capture_sources_in(files, workspace);
    }
    let line_starts = line_starts(source);
    let mut blocks = Vec::new();
    let mut cursor = 0;
    let mut cursor_line = 1;

    for directive in directives {
        let directive_start = line_start(&line_starts, source.len(), directive.start_line);
        push_text(
            &mut blocks,
            &source[cursor..directive_start],
            cursor_line,
            theme_name,
        )?;

        let block = match directive.kind {
            DirectiveKind::Diff => {
                let spec: DiffDirective = parse_directive(&directive)?;
                validate_range(spec.start, spec.end, directive.start_line)?;
                let files = diff_files
                    .as_deref()
                    .expect("diff directives caused the patch to be parsed");
                select_diff(files, spec, directive.start_line)?
            }
            DirectiveKind::Code => {
                let spec: CodeDirective = parse_directive(&directive)?;
                validate_range(spec.start, spec.end, directive.start_line)?;
                select_code(workspace, spec, theme, directive.start_line)?
            }
        };
        push_block(&mut blocks, block)?;

        cursor_line = directive.end_line.saturating_add(1);
        cursor = line_start(&line_starts, source.len(), cursor_line);
    }

    push_text(&mut blocks, &source[cursor..], cursor_line, theme_name)?;
    ensure!(!blocks.is_empty(), "guided review contains no content");
    Ok(blocks)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DirectiveKind {
    Diff,
    Code,
}

impl DirectiveKind {
    fn name(self) -> &'static str {
        match self {
            Self::Diff => "wdyt-diff",
            Self::Code => "wdyt-code",
        }
    }
}

struct RawDirective {
    kind: DirectiveKind,
    body: String,
    start_line: usize,
    end_line: usize,
}

fn find_directives(source: &str) -> Result<Vec<RawDirective>> {
    let arena = Arena::new();
    let root = parse_document(&arena, source, &Options::default());
    let mut directives = Vec::new();

    for node in root.children() {
        let data = node.data();
        let NodeValue::CodeBlock(code) = &data.value else {
            continue;
        };
        if !code.fenced {
            continue;
        }
        let kind = match code.info.trim() {
            "wdyt-diff" => DirectiveKind::Diff,
            "wdyt-code" => DirectiveKind::Code,
            _ => continue,
        };
        ensure!(
            code.closed,
            "unclosed {} directive at line {}",
            kind.name(),
            data.sourcepos.start.line
        );
        directives.push(RawDirective {
            kind,
            body: code.literal.clone(),
            start_line: data.sourcepos.start.line,
            end_line: data.sourcepos.end.line,
        });
    }

    Ok(directives)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DiffDirective {
    file: String,
    side: Side,
    start: usize,
    end: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeDirective {
    file: String,
    start: usize,
    end: usize,
}

fn parse_directive<T: for<'de> Deserialize<'de>>(directive: &RawDirective) -> Result<T> {
    serde_json::from_str(&directive.body).with_context(|| {
        format!(
            "invalid {} directive at line {}",
            directive.kind.name(),
            directive.start_line
        )
    })
}

fn validate_range(start: usize, end: usize, line: usize) -> Result<()> {
    ensure!(
        start > 0 && end > 0,
        "directive at line {line} has a zero line number"
    );
    ensure!(
        start <= end,
        "directive at line {line} has a reversed range {start}..{end}"
    );
    let length = end
        .checked_sub(start)
        .and_then(|difference| difference.checked_add(1))
        .context("line range is too large")?;
    ensure!(
        length <= MAX_RANGE_LINES,
        "directive at line {line} selects {length} lines; the limit is {MAX_RANGE_LINES}"
    );
    Ok(())
}

fn push_text(
    blocks: &mut Vec<GuidedBlock>,
    source: &str,
    source_start: usize,
    theme_name: &str,
) -> Result<()> {
    if source.trim().is_empty() {
        return Ok(());
    }
    let rendered = crate::render::markdown_commentable("guided.md", source, theme_name);
    push_block(
        blocks,
        GuidedBlock::Text {
            html: rendered.html,
            source_start,
            sources: rendered.sources,
        },
    )
}

fn push_block(blocks: &mut Vec<GuidedBlock>, block: GuidedBlock) -> Result<()> {
    ensure!(
        blocks.len() < MAX_BLOCKS,
        "guided review exceeds the limit of {MAX_BLOCKS} blocks"
    );
    blocks.push(block);
    Ok(())
}

fn select_code(
    workspace: &Path,
    spec: CodeDirective,
    theme: &two_face::re_exports::syntect::highlighting::Theme,
    directive_line: usize,
) -> Result<GuidedBlock> {
    let relative = validated_relative_path(&spec.file, directive_line)?;
    let workspace = std::fs::canonicalize(workspace).with_context(|| {
        format!(
            "canonicalizing guided code workspace {}",
            workspace.display()
        )
    })?;
    ensure!(
        workspace.is_dir(),
        "guided code workspace is not a directory: {}",
        workspace.display()
    );
    let path = std::fs::canonicalize(workspace.join(relative))
        .with_context(|| format!("resolving guided code file {}", spec.file))?;
    ensure!(
        path.starts_with(&workspace),
        "wdyt-code path resolves outside the workspace: {}",
        spec.file
    );
    let metadata = path
        .metadata()
        .with_context(|| format!("inspecting guided code file {}", spec.file))?;
    ensure!(
        metadata.is_file(),
        "wdyt-code path is not a file: {}",
        spec.file
    );
    ensure!(
        metadata.len() <= MAX_SOURCE_BYTES,
        "wdyt-code file {} is {} bytes; the limit is {MAX_SOURCE_BYTES} bytes (4 MiB)",
        spec.file,
        metadata.len()
    );
    let source = std::fs::read_to_string(&path)
        .with_context(|| format!("reading guided code file {}", spec.file))?;
    let rendered = crate::render::highlight(&spec.file, &source, theme)
        .with_context(|| format!("highlighting guided code file {}", spec.file))?;
    ensure!(
        spec.end <= rendered.sources.len(),
        "wdyt-code directive at line {directive_line} selects {}..{}, but {} has {} lines",
        spec.start,
        spec.end,
        spec.file,
        rendered.sources.len()
    );
    let selected = spec.start - 1..spec.end;

    Ok(GuidedBlock::Code {
        file: spec.file,
        language: rendered.language,
        start: spec.start,
        end: spec.end,
        highlighted: rendered.highlighted[selected.clone()].to_vec(),
        sources: rendered.sources[selected].to_vec(),
    })
}

fn validated_relative_path(file: &str, directive_line: usize) -> Result<&Path> {
    ensure!(
        !file.is_empty(),
        "wdyt-code directive at line {directive_line} has an empty file path"
    );
    let path = Path::new(file);
    ensure!(
        !path.is_absolute(),
        "wdyt-code path must be workspace-relative: {file}"
    );
    let mut has_normal_component = false;
    for component in path.components() {
        match component {
            Component::Normal(_) => has_normal_component = true,
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!("wdyt-code path may not leave the workspace: {file}");
            }
        }
    }
    ensure!(
        has_normal_component,
        "wdyt-code directive at line {directive_line} has an empty file path"
    );
    Ok(path)
}

fn select_diff(
    files: &[DiffFile],
    spec: DiffDirective,
    directive_line: usize,
) -> Result<GuidedBlock> {
    ensure!(
        !spec.file.is_empty(),
        "wdyt-diff directive at line {directive_line} has an empty file path"
    );
    let file = files
        .iter()
        .find(|file| file.label == spec.file)
        .with_context(|| {
            format!(
                "wdyt-diff directive at line {directive_line} names missing file {}",
                spec.file
            )
        })?;
    let mut selected_hunks = Vec::new();
    let mut selected_lines = 0usize;

    for hunk in &file.hunks {
        let direct: Vec<bool> = hunk
            .lines
            .iter()
            .map(|line| {
                let number = match spec.side {
                    Side::Old => line.old,
                    Side::New => line.new,
                };
                number.is_some_and(|number| (spec.start..=spec.end).contains(&number))
            })
            .collect();
        selected_lines += direct.iter().filter(|selected| **selected).count();
        let mut include = direct.clone();

        // Removed and added runs with no context between them are one
        // replacement location. If the requested side touches that run, retain
        // the opposite side as well so the review never shows half a replacement.
        let mut index = 0;
        while index < hunk.lines.len() {
            if hunk.lines[index].kind == LineKind::Context {
                index += 1;
                continue;
            }
            let start = index;
            while index < hunk.lines.len() && hunk.lines[index].kind != LineKind::Context {
                index += 1;
            }
            let end = index;
            if direct[start..end].iter().any(|selected| *selected) {
                for (selected, line) in include[start..end].iter_mut().zip(&hunk.lines[start..end])
                {
                    let opposite_side = match spec.side {
                        Side::Old => line.kind == LineKind::Added,
                        Side::New => line.kind == LineKind::Removed,
                    };
                    *selected |= opposite_side;
                }
            }
        }

        let lines = hunk
            .lines
            .iter()
            .zip(include)
            .filter(|(_, include)| *include)
            .map(|(line, _)| line.clone())
            .collect::<Vec<_>>();
        if !lines.is_empty() {
            selected_hunks.push(Hunk {
                header: hunk.header.clone(),
                lines,
                old_start: hunk.old_start,
                new_start: hunk.new_start,
            });
        }
    }

    ensure!(
        selected_lines > 0,
        "wdyt-diff directive at line {directive_line} selects no {}-side lines in {}",
        spec.side.as_str(),
        spec.file
    );
    Ok(GuidedBlock::Diff {
        file: file.clone(),
        side: spec.side,
        start: spec.start,
        end: spec.end,
        selected_hunks,
    })
}

fn line_starts(source: &str) -> Vec<usize> {
    let mut starts = vec![0];
    starts.extend(
        source
            .char_indices()
            .filter_map(|(index, character)| (character == '\n').then_some(index + 1)),
    );
    starts
}

fn line_start(starts: &[usize], source_len: usize, line: usize) -> usize {
    starts
        .get(line.saturating_sub(1))
        .copied()
        .unwrap_or(source_len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Content;

    const PATCH: &str = "\
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -2,4 +2,5 @@
 fn before() {}
-old one
-old two
+new one
+new two
+new three
 tail
";

    const CURRENT_SOURCE: &str = "\
before hidden
fn before() {}
new one
new two
new three
tail
after hidden
";

    fn code_directive(file: &str, start: usize, end: usize) -> String {
        format!("```wdyt-code\n{{\"file\":{file:?},\"start\":{start},\"end\":{end}}}\n```\n")
    }

    fn diff_directive(file: &str, side: &str, start: usize, end: usize) -> String {
        format!(
            "```wdyt-diff\n\
             {{\"file\":{file:?},\"side\":{side:?},\"start\":{start},\"end\":{end}}}\n\
             ```\n"
        )
    }

    #[test]
    fn mixed_blocks_preserve_source_order_and_code_numbers() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir(workspace.path().join("tests")).unwrap();
        std::fs::write(
            workspace.path().join("tests/present.rs"),
            "first\nfn selected() {}\nthird\n",
        )
        .unwrap();
        let source = format!(
            "## Public API\n\nAdded a method.\n\n{}\
             ## Tests\n\n{}\n",
            diff_directive("src/lib.rs", "new", 3, 3),
            code_directive("tests/present.rs", 2, 2)
        );

        let blocks = parse(&source, PATCH, workspace.path(), "Nord").unwrap();
        assert_eq!(blocks.len(), 4);
        assert!(matches!(blocks[0], GuidedBlock::Text { .. }));
        assert!(matches!(blocks[1], GuidedBlock::Diff { .. }));
        assert!(matches!(blocks[2], GuidedBlock::Text { .. }));
        match &blocks[3] {
            GuidedBlock::Code {
                start,
                end,
                sources,
                highlighted,
                ..
            } => {
                assert_eq!((*start, *end), (2, 2));
                assert_eq!(sources, &["fn selected() {}"]);
                assert_eq!(highlighted.len(), 1);
            }
            block => panic!("expected code block, got {block:?}"),
        }
    }

    #[test]
    fn markdown_is_escaped_and_uses_the_selected_theme() {
        let source = "## Safe\n\n<script>alert(1)</script>\n\n```rust\nfn main() {}\n```\n";
        let nord = parse(source, "", Path::new("."), "Nord").unwrap();
        let github = parse(source, "", Path::new("."), "InspiredGitHub").unwrap();
        let nord_html = match &nord[0] {
            GuidedBlock::Text { html, .. } => html.as_str(),
            block => panic!("expected text block, got {block:?}"),
        };
        let github_html = match &github[0] {
            GuidedBlock::Text { html, .. } => html.as_str(),
            block => panic!("expected text block, got {block:?}"),
        };

        assert!(!nord_html.contains("<script>"));
        assert!(nord_html.contains("style="));
        assert_ne!(nord_html, github_html);
    }

    #[test]
    fn new_side_selection_includes_removed_replacement_lines() {
        let source = diff_directive("src/lib.rs", "new", 3, 3);
        let blocks = parse(&source, PATCH, Path::new("."), "Nord").unwrap();
        let GuidedBlock::Diff {
            side,
            selected_hunks,
            ..
        } = &blocks[0]
        else {
            panic!("expected diff block");
        };
        let lines = &selected_hunks[0].lines;

        assert_eq!(*side, Side::New);
        assert_eq!(
            lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            ["old one", "old two", "new one"]
        );
        assert_eq!(
            lines.iter().map(|line| line.kind).collect::<Vec<_>>(),
            [LineKind::Removed, LineKind::Removed, LineKind::Added]
        );
    }

    #[test]
    fn old_side_selection_includes_added_replacement_lines() {
        let source = diff_directive("src/lib.rs", "old", 4, 4);
        let blocks = parse(&source, PATCH, Path::new("."), "Nord").unwrap();
        let GuidedBlock::Diff {
            side,
            selected_hunks,
            ..
        } = &blocks[0]
        else {
            panic!("expected diff block");
        };
        let lines = &selected_hunks[0].lines;

        assert_eq!(*side, Side::Old);
        assert_eq!(
            lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            ["old two", "new one", "new two", "new three"]
        );
    }

    #[test]
    fn diff_blocks_retain_validated_source_for_expansion() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir(workspace.path().join("src")).unwrap();
        std::fs::write(workspace.path().join("src/lib.rs"), CURRENT_SOURCE).unwrap();
        let blocks = parse(
            &diff_directive("src/lib.rs", "new", 3, 3),
            PATCH,
            workspace.path(),
            "Nord",
        )
        .unwrap();
        let GuidedBlock::Diff {
            file,
            selected_hunks,
            ..
        } = &blocks[0]
        else {
            panic!("expected diff block");
        };

        assert_eq!(selected_hunks[0].lines.len(), 3);
        assert_eq!(file.hunks[0].lines.len(), 7);
        assert_eq!(
            file.source.first().map(String::as_str),
            Some("before hidden")
        );
        assert_eq!(file.source.last().map(String::as_str), Some("after hidden"));
        let gaps = crate::diff::gaps(file);
        assert_eq!(gaps.before[0], Some(crate::diff::Gap { from: 1, to: 1 }));
        assert_eq!(gaps.after, Some(crate::diff::Gap { from: 7, to: 7 }));
    }

    #[test]
    fn mismatched_workspace_source_is_not_retained() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir(workspace.path().join("src")).unwrap();
        std::fs::write(workspace.path().join("src/lib.rs"), "different\n").unwrap();
        let blocks = parse(
            &diff_directive("src/lib.rs", "new", 3, 3),
            PATCH,
            workspace.path(),
            "Nord",
        )
        .unwrap();
        let GuidedBlock::Diff { file, .. } = &blocks[0] else {
            panic!("expected diff block");
        };
        assert!(file.source.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn diff_source_symlinks_cannot_escape_the_workspace() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(workspace.path().join("src")).unwrap();
        std::fs::write(outside.path().join("lib.rs"), CURRENT_SOURCE).unwrap();
        symlink(
            outside.path().join("lib.rs"),
            workspace.path().join("src/lib.rs"),
        )
        .unwrap();

        let blocks = parse(
            &diff_directive("src/lib.rs", "new", 3, 3),
            PATCH,
            workspace.path(),
            "Nord",
        )
        .unwrap();
        let GuidedBlock::Diff { file, .. } = &blocks[0] else {
            panic!("expected diff block");
        };
        assert!(file.source.is_empty());
    }

    #[test]
    fn diff_source_uses_the_git_root_from_a_subdirectory() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(workspace.path().join("src/subdir")).unwrap();
        std::fs::write(workspace.path().join("src/lib.rs"), CURRENT_SOURCE).unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(workspace.path())
            .status()
            .unwrap();
        assert!(status.success());

        let blocks = parse(
            &diff_directive("src/lib.rs", "new", 3, 3),
            PATCH,
            &workspace.path().join("src/subdir"),
            "Nord",
        )
        .unwrap();
        let GuidedBlock::Diff { file, .. } = &blocks[0] else {
            panic!("expected diff block");
        };
        assert!(!file.source.is_empty());
    }

    #[test]
    fn missing_diff_and_code_files_are_rejected() {
        let error = parse(
            &diff_directive("src/missing.rs", "new", 1, 1),
            PATCH,
            Path::new("."),
            "Nord",
        )
        .unwrap_err();
        assert!(error.to_string().contains("missing file"), "{error:#}");

        let workspace = tempfile::tempdir().unwrap();
        let error = parse(
            &code_directive("src/missing.rs", 1, 1),
            "",
            workspace.path(),
            "Nord",
        )
        .unwrap_err();
        assert!(error.to_string().contains("guided code file"), "{error:#}");
    }

    #[test]
    fn directive_json_is_strict() {
        let malformed = "```wdyt-code\nnot json\n```\n";
        let error = parse(malformed, "", Path::new("."), "Nord").unwrap_err();
        assert!(error.to_string().contains("invalid wdyt-code"), "{error:#}");

        let unknown = "\
```wdyt-diff
{\"file\":\"src/lib.rs\",\"side\":\"new\",\"start\":1,\"end\":1,\"extra\":true}
```
";
        let error = parse(unknown, PATCH, Path::new("."), "Nord").unwrap_err();
        assert!(error.to_string().contains("invalid wdyt-diff"), "{error:#}");

        let empty = "```wdyt-code\n```\n";
        let error = parse(empty, "", Path::new("."), "Nord").unwrap_err();
        assert!(error.to_string().contains("invalid wdyt-code"), "{error:#}");
    }

    #[test]
    fn code_paths_must_stay_beneath_the_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        for file in ["../secret.rs", "/tmp/secret.rs", "."] {
            let error =
                parse(&code_directive(file, 1, 1), "", workspace.path(), "Nord").unwrap_err();
            assert!(
                error.to_string().contains("workspace")
                    || error.to_string().contains("empty file path"),
                "{file}: {error:#}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn code_symlinks_cannot_escape_the_workspace() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.rs"), "let secret = 42;\n").unwrap();
        symlink(
            outside.path().join("secret.rs"),
            workspace.path().join("linked.rs"),
        )
        .unwrap();

        let error = parse(
            &code_directive("linked.rs", 1, 1),
            "",
            workspace.path(),
            "Nord",
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("outside the workspace"),
            "{error:#}"
        );
    }

    #[test]
    fn oversized_code_files_are_rejected_before_highlighting() {
        let workspace = tempfile::tempdir().unwrap();
        let file = std::fs::File::create(workspace.path().join("large.rs")).unwrap();
        file.set_len(MAX_SOURCE_BYTES + 1).unwrap();

        let error = parse(
            &code_directive("large.rs", 1, 1),
            "",
            workspace.path(),
            "Nord",
        )
        .unwrap_err();
        assert!(error.to_string().contains("4 MiB"), "{error:#}");
    }

    #[test]
    fn zero_reversed_and_large_ranges_are_rejected() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("x.rs"), "one\n").unwrap();
        for (start, end, message) in [
            (0, 1, "zero line"),
            (2, 1, "reversed range"),
            (1, 501, "limit is 500"),
        ] {
            let error = parse(
                &code_directive("x.rs", start, end),
                "",
                workspace.path(),
                "Nord",
            )
            .unwrap_err();
            assert!(error.to_string().contains(message), "{error:#}");
        }
    }

    #[test]
    fn ranges_that_select_no_lines_are_rejected() {
        let error = parse(
            &diff_directive("src/lib.rs", "new", 100, 100),
            PATCH,
            Path::new("."),
            "Nord",
        )
        .unwrap_err();
        assert!(error.to_string().contains("selects no new-side lines"));

        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("x.rs"), "one\n").unwrap();
        let error = parse(&code_directive("x.rs", 2, 2), "", workspace.path(), "Nord").unwrap_err();
        assert!(error.to_string().contains("has 1 lines"), "{error:#}");
    }

    #[test]
    fn guides_are_limited_to_one_hundred_blocks() {
        let mut source = String::new();
        for _ in 0..=MAX_BLOCKS {
            source.push_str(&diff_directive("src/lib.rs", "new", 3, 3));
        }
        let error = parse(&source, PATCH, Path::new("."), "Nord").unwrap_err();
        assert!(error.to_string().contains("100 blocks"), "{error:#}");
    }

    #[test]
    fn nested_directive_fences_remain_markdown() {
        let source = "> ```wdyt-code\n> {\"file\":\"missing.rs\",\"start\":1,\"end\":1}\n> ```\n";
        let blocks = parse(source, "", Path::new("."), "Nord").unwrap();
        assert!(matches!(blocks.as_slice(), [GuidedBlock::Text { .. }]));
    }

    #[test]
    fn old_content_serde_remains_compatible_and_guided_round_trips() {
        let old: Content = serde_json::from_str(r#"{"Demo":{"port":3000,"path":"/"}}"#).unwrap();
        assert!(matches!(old, Content::Demo { port: 3000, .. }));

        let content = Content::Guided {
            blocks: vec![GuidedBlock::Text {
                html: "<p>hello</p>\n".into(),
                source_start: 1,
                sources: vec!["hello".into()],
            }],
        };
        let json = serde_json::to_string(&content).unwrap();
        let restored: Content = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            restored,
            Content::Guided { blocks } if blocks.len() == 1
        ));
    }
}
