//! Turning files into HTML: syntax highlighting for code, comrak for markdown.

use std::path::Path;
use std::sync::OnceLock;

use anyhow::{Context, Result};
use two_face::re_exports::syntect::{
    easy::HighlightLines,
    highlighting::{Color, Style, Theme},
    html::{IncludeBackground, styled_line_to_highlighted_html},
    parsing::SyntaxSet,
    util::LinesWithEndings,
};

use crate::session::{CodeFile, DocFile};

/// Syntax and theme sets are ~1MB of embedded data and are immutable once
/// built, so they load once per process.
pub(crate) fn syntaxes() -> &'static SyntaxSet {
    static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
    SYNTAXES.get_or_init(two_face::syntax::extra_newlines)
}

fn themes() -> &'static two_face::theme::LazyThemeSet {
    static THEMES: OnceLock<two_face::theme::LazyThemeSet> = OnceLock::new();
    // `extra()` returns the embedded set; `LazyThemeSet` is the by-name API.
    THEMES.get_or_init(|| two_face::theme::LazyThemeSet::from(two_face::theme::extra()))
}

/// The theme used when the configured one is missing.
const FALLBACK_THEME: &str = "Nord";

/// Looks up a theme by name, falling back to a bundled default so a typo in
/// the config does not stop the tool from working.
pub fn theme(name: &str) -> &'static Theme {
    let set = themes();
    set.get(name)
        .or_else(|| set.get(FALLBACK_THEME))
        .unwrap_or_else(|| {
            // The embedded set always contains Nord; this arm exists only so a
            // future two-face change cannot turn a rename into a panic.
            set.get(set.theme_names().next().expect("two-face ships themes"))
                .expect("name came from the set")
        })
}

/// Theme names, for `showme themes`.
pub fn theme_names() -> Vec<&'static str> {
    let mut names: Vec<_> = themes().theme_names().collect();
    names.sort_unstable();
    names
}

/// The page background implied by a theme, so the chrome around the code
/// matches the highlighting instead of clashing with it.
pub fn theme_colors(theme: &Theme) -> ThemeColors {
    let bg = theme.settings.background.unwrap_or(Color {
        r: 0x1e,
        g: 0x1e,
        b: 0x1e,
        a: 0xff,
    });
    let fg = theme.settings.foreground.unwrap_or(Color {
        r: 0xd4,
        g: 0xd4,
        b: 0xd4,
        a: 0xff,
    });
    ThemeColors {
        background: css_rgb(bg),
        foreground: css_rgb(fg),
        // Gutter and border tones are derived from the background so every
        // theme gets a coherent set without hand-maintaining a table.
        gutter: css_rgb(shift(bg, 0.35)),
        border: css_rgb(shift(bg, 0.6)),
        muted: css_rgb(blend(fg, bg, 0.45)),
        dark: luminance(bg) < 0.5,
    }
}

#[derive(Debug, Clone)]
pub struct ThemeColors {
    pub background: String,
    pub foreground: String,
    pub gutter: String,
    pub border: String,
    pub muted: String,
    pub dark: bool,
}

fn css_rgb(c: Color) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
}

fn luminance(c: Color) -> f32 {
    // Rec. 601 luma, good enough to decide "is this a dark theme".
    (0.299 * f32::from(c.r) + 0.587 * f32::from(c.g) + 0.114 * f32::from(c.b)) / 255.0
}

/// Moves a colour toward white or black by `amount`, whichever increases
/// contrast against it.
fn shift(c: Color, amount: f32) -> Color {
    let target = if luminance(c) < 0.5 { 255.0 } else { 0.0 };
    let mix = |v: u8| (f32::from(v) + (target - f32::from(v)) * amount * 0.25) as u8;
    Color {
        r: mix(c.r),
        g: mix(c.g),
        b: mix(c.b),
        a: c.a,
    }
}

fn blend(a: Color, b: Color, t: f32) -> Color {
    let mix = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t) as u8;
    Color {
        r: mix(a.r, b.r),
        g: mix(a.g, b.g),
        b: mix(a.b, b.b),
        a: 0xff,
    }
}

/// Highlights source text into a table of line numbers and code.
///
/// A table is used rather than syntect's plain `<pre>` output so line numbers
/// can be selected-around and linked to.
pub fn highlight(label: &str, source: &str, theme: &Theme) -> Result<CodeFile> {
    let syntaxes = syntaxes();
    let syntax = Path::new(label)
        .extension()
        .and_then(|ext| ext.to_str())
        .and_then(|ext| syntaxes.find_syntax_by_extension(ext))
        .or_else(|| {
            Path::new(label)
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| syntaxes.find_syntax_by_extension(name))
        })
        .or_else(|| syntaxes.find_syntax_by_first_line(source.lines().next().unwrap_or("")))
        .unwrap_or_else(|| syntaxes.find_syntax_plain_text());

    let mut highlighter = HighlightLines::new(syntax, theme);
    let mut rows = String::new();
    let mut lines = 0usize;

    for (index, line) in LinesWithEndings::from(source).enumerate() {
        lines = index + 1;
        let regions: Vec<(Style, &str)> = highlighter
            .highlight_line(line, syntaxes)
            .context("highlighting failed")?;
        // `IncludeBackground::No` keeps per-span background colours out of the
        // markup; the container already paints the theme background.
        let mut code = styled_line_to_highlighted_html(&regions, IncludeBackground::No)
            .context("rendering highlighted line failed")?;
        if code.is_empty() {
            // An empty line still needs height.
            code.push_str("&nbsp;");
        }
        rows.push_str(&format!(
            "<tr id=\"L{n}\"><td class=\"ln\"><a href=\"#L{n}\">{n}</a></td><td class=\"code\">{code}</td></tr>",
            n = lines,
        ));
    }

    if lines == 0 {
        rows.push_str("<tr><td class=\"ln\"></td><td class=\"code\"><em>empty file</em></td></tr>");
    }

    Ok(CodeFile {
        label: label.to_owned(),
        html: format!("<table class=\"src\">{rows}</table>"),
        lines,
        language: syntax.name.clone(),
    })
}

/// Renders markdown with GFM extensions, highlighting fenced code with
/// `theme_name`.
pub fn markdown(label: &str, source: &str, theme_name: &str) -> DocFile {
    let mut options = comrak::Options::default();
    options.extension.table = true;
    options.extension.strikethrough = true;
    options.extension.autolink = true;
    options.extension.tasklist = true;
    options.extension.footnotes = true;
    // Heading anchors, so a long doc can be linked into.
    options.extension.header_id_prefix = Some(String::new());
    // GitHub-style `> [!NOTE]` callouts, which agents write often.
    options.extension.alerts = true;
    options.extension.description_lists = true;
    // Raw HTML in the markdown is passed through. The input is a file the
    // agent produced for the user to read on a loopback-only server, and
    // stripping it would mangle docs that legitimately contain HTML.
    options.render.r#unsafe = true;

    // Fenced code blocks get highlighted with the same theme as `showme code`.
    let adapter = comrak::plugins::syntect::SyntectAdapterBuilder::new()
        .syntax_set(syntaxes().clone())
        .theme_set(themes().into())
        .theme(theme_name)
        .build();
    let mut plugins = comrak::options::Plugins::default();
    plugins.render.codefence_syntax_highlighter = Some(&adapter);

    DocFile {
        label: label.to_owned(),
        html: comrak::markdown_to_html_with_plugins(source, &options, &plugins),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_rust_with_line_numbers() {
        let file = highlight("lib.rs", "fn main() {}\nlet x = 1;\n", theme("Nord")).unwrap();
        assert_eq!(file.lines, 2);
        assert!(file.html.contains("id=\"L1\""));
        assert!(file.html.contains("id=\"L2\""));
        assert_eq!(file.language, "Rust");
    }

    #[test]
    fn unknown_extension_falls_back_to_plain_text() {
        let file = highlight("notes.zzzz", "hello\n", theme("Nord")).unwrap();
        assert_eq!(file.lines, 1);
        assert!(file.html.contains("hello"));
    }

    #[test]
    fn empty_file_still_renders() {
        let file = highlight("empty.rs", "", theme("Nord")).unwrap();
        assert_eq!(file.lines, 0);
        assert!(file.html.contains("empty file"));
    }

    #[test]
    fn code_content_is_escaped() {
        // syntect escapes text; a file containing markup must not inject it.
        let file = highlight("x.txt", "<script>alert(1)</script>\n", theme("Nord")).unwrap();
        assert!(
            !file.html.contains("<script>"),
            "raw script tag survived: {}",
            file.html
        );
    }

    #[test]
    fn renders_markdown_tables_and_tasklists() {
        let doc = markdown("README.md", "| a | b |\n|---|---|\n| 1 | 2 |\n\n- [x] done\n", "Nord");
        assert!(doc.html.contains("<table>"));
        assert!(doc.html.contains("checkbox"));
    }

    #[test]
    fn highlights_fenced_code_in_markdown() {
        let doc = markdown("d.md", "```rust\nfn main() {}\n```\n", "Nord");
        // The syntect adapter emits inline styles rather than a bare <code>.
        assert!(doc.html.contains("style"), "no highlighting: {}", doc.html);
    }

    #[test]
    fn unknown_theme_falls_back() {
        let colors = theme_colors(theme("no-such-theme"));
        assert!(colors.background.starts_with('#'));
    }

    #[test]
    fn theme_list_is_not_empty() {
        assert!(theme_names().contains(&"Nord"));
    }

    #[test]
    fn dark_and_light_themes_are_detected() {
        assert!(theme_colors(theme("Nord")).dark);
        assert!(!theme_colors(theme("InspiredGitHub")).dark);
    }
}
