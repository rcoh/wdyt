//! The page shell: shared chrome, styling, and the reply widget.

use topcoat::Result;
use topcoat::context::Cx;
use topcoat::view::{NodeViewParts, PartsWriter, component, view};

use crate::diff::Gap;
use crate::render::ThemeColors;
use crate::session::{Author, Comment, Hunk, LineKind, Reply, Side};

/// Pre-rendered HTML injected verbatim.
///
/// Everything wrapped in this comes from syntect, comrak, or this crate's own
/// string constants: never from an HTTP request. It is also how `<script>` and
/// `<style>` bodies are written, because `view!` escapes text nodes in those
/// elements even though HTML treats them as raw text, which would corrupt any
/// JS containing `&&` or `<`.
pub struct Raw<T>(pub T);

impl<T: Into<String>> NodeViewParts for Raw<T> {
    fn into_view_parts(self, _cx: &Cx, parts: &mut PartsWriter<'_>) {
        parts.push_str_unescaped(self.0.into());
    }
}

/// Chrome, layout, and content styling.
///
/// The chrome is a fixed warm off-white palette rather than one derived from the
/// syntax theme: it is the same frame around every kind of content, including
/// framed apps that have a look of their own, so it should not change colour
/// with the theme. The syntax theme stays where it belongs — inside the code
/// card, via `--code-*`.
fn stylesheet(colors: &ThemeColors) -> String {
    let ThemeColors {
        background,
        foreground,
        gutter,
        border,
        muted,
        ..
    } = colors;

    format!(
        r#"
*, *::before, *::after {{ box-sizing: border-box; }}
:root {{
  /* Off-whites, warm rather than grey: paper, then a raised tone for the
     header and chips, then white for cards that should read as on top. */
  --bg: #faf9f5;
  --raised: #f0eee6;
  --surface: #ffffff;
  --fg: #1f1e1d;
  --muted: #6f6b62;
  --border: #e6e3d9;
  --border-strong: #d8d4c6;
  --accent: #c96442;
  /* From the syntax theme, so the code card and its highlighting agree. */
  --code-bg: {background};
  --code-fg: {foreground};
  --code-gutter: {gutter};
  --code-border: {border};
  --code-muted: {muted};
  color-scheme: light;
}}
html, body {{ margin: 0; padding: 0; }}
body {{
  background: var(--bg);
  color: var(--fg);
  font: 14px/1.6 ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif;
  display: flex;
  flex-direction: column;
  min-height: 100vh;
}}
/* A framed page must not scroll: the iframe owns the space below the chrome and
   sizes to it. Elsewhere the body has to be free to grow past the viewport, or a
   `position: sticky` child has no distance to travel and never sticks. */
html:has(body.framed), body.framed {{ height: 100%; }}
a {{ color: var(--accent); }}

/* Header ------------------------------------------------------------------ */
header.bar {{
  display: flex; align-items: baseline; gap: 12px;
  padding: 14px 20px;
  border-bottom: 1px solid var(--border);
  background: var(--raised);
  flex: 0 0 auto;
}}
header.bar .brand {{
  font-weight: 600; letter-spacing: .06em; color: var(--accent);
  font-size: 11px; text-transform: uppercase;
}}
header.bar h1 {{ font-size: 15px; font-weight: 600; margin: 0; }}
header.bar .note {{ color: var(--muted); font-size: 13px; }}
header.bar .spacer {{ flex: 1; }}
/* Which agent is asking: the multiplexer pane to switch to, and the directory
   the work is in. Quiet and monospaced — it is a coordinate, not a headline. */
header.bar .origin {{
  display: flex; align-items: baseline; gap: 5px; min-width: 0;
  font: 11px/1.4 ui-monospace, monospace; color: var(--muted);
}}
header.bar .origin .place {{ color: var(--fg); }}
header.bar .origin .sep {{ opacity: .5; }}
/* The path is the first thing worth dropping when the bar is tight: the pane
   name identifies the agent, and the path is usually implied by it. */
header.bar .origin .cwd {{
  overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  max-width: 30ch; direction: rtl; text-align: left;
}}
.kind {{
  font: 11px/1 ui-monospace, monospace; text-transform: uppercase;
  padding: 4px 7px; border: 1px solid var(--border-strong); border-radius: 4px;
  background: var(--surface); color: var(--muted);
}}

main {{ flex: 1 1 auto; min-height: 0; }}
.wrap {{ max-width: 1100px; margin: 0 auto; padding: 24px 20px 96px; }}

/* File list --------------------------------------------------------------- */
/* Sticky, because on a long diff this is the only way to jump between files: a
   jump list that has scrolled off the top is no jump list at all. Only in the
   scrolling modes — a framed page does not scroll, and `position: sticky` inside
   a flex column there would just pin it over the iframe. */
nav.files {{
  display: flex; flex-wrap: wrap; gap: 6px;
  padding: 10px 20px; border-bottom: 1px solid var(--border);
  font: 12px/1 ui-monospace, monospace;
}}
body:not(.framed) nav.files {{
  position: sticky; top: 0; z-index: 3;
  background: var(--raised);
  /* Its own backdrop, since content scrolls underneath it. */
  box-shadow: 0 1px 0 var(--border);
}}
nav.files a {{
  padding: 5px 9px; border-radius: 4px; text-decoration: none;
  border: 1px solid transparent;
}}
nav.files a:hover {{ border-color: var(--border); }}
/* Active file indicator: visible on both horizontal (mobile) and vertical
   (desktop sidebar) nav so tracking works at every breakpoint. */
nav.files a.active {{
  background: var(--surface); border-color: var(--border);
  color: var(--accent); font-weight: 600;
}}
/* An anchored jump must not land under the sticky nav. Generous enough to clear
   it when it has wrapped to a second row on a wide diff. */
section.file, section.doc {{ scroll-margin-top: 92px; }}

/* File sidebar (desktop) -------------------------------------------------- */
/* On wide screens with multiple files, a vertical sticky sidebar replaces the
   horizontal chip bar. The horizontal bar remains as a compact fallback on
   narrower screens, and both live inside the same `nav.files` element so
   anchor links and guided-review fragment refs keep working. */
@media (min-width: 1380px) {{
  body:not(.framed):not(.nochrome) nav.files.has-sidebar {{
    position: fixed; top: 56px; left: 0; bottom: 0;
    width: 200px; z-index: 4;
    display: flex; flex-direction: column; flex-wrap: nowrap;
    gap: 2px; padding: 12px 10px;
    overflow-y: auto; overflow-x: hidden;
    border-bottom: none; border-right: 1px solid var(--border);
    box-shadow: none;
  }}
  body:not(.framed):not(.nochrome) nav.files.has-sidebar a {{
    display: block; white-space: nowrap; overflow: hidden;
    text-overflow: ellipsis; padding: 5px 8px;
  }}
  /* The main content shifts right to make room for the sidebar. */
  body:not(.framed):not(.nochrome).has-file-sidebar main {{
    margin-left: 200px;
  }}
  body:not(.framed):not(.nochrome).has-file-sidebar section.brief {{
    margin-left: 200px;
    padding-left: max(20px, calc((100% - 200px - 1100px) / 2 + 20px));
  }}
  /* Anchor offset accounts for only the filename header, not the top nav. */
  body:not(.framed):not(.nochrome).has-file-sidebar section.file,
  body:not(.framed):not(.nochrome).has-file-sidebar section.doc {{
    scroll-margin-top: 8px;
  }}
  body:not(.framed):not(.nochrome).has-file-sidebar section.file .filename {{
    top: 0;
  }}
}}
/* On narrower screens and mobile the sidebar is not shown; the horizontal
   sticky bar remains as a single-row scrollable strip so it never obscures
   content even on very narrow viewports. */
@media (max-width: 1379px) {{
  nav.files.has-sidebar {{
    flex-wrap: nowrap;
    overflow-x: auto;
    -webkit-overflow-scrolling: touch;
    scrollbar-width: thin;
  }}
  nav.files.has-sidebar a {{
    white-space: nowrap; flex: 0 0 auto;
  }}
  /* With a single-row strip (~45px), the filename header sticks below it and
     the file section must clear both. Derived for one row height. */
  body:not(.framed) nav.files.has-sidebar ~ main section.file .filename {{
    top: 45px;
  }}
  body:not(.framed) nav.files.has-sidebar ~ main section.file,
  body:not(.framed) nav.files.has-sidebar ~ main section.doc {{
    scroll-margin-top: 92px;
  }}
}}

/* Jump-to-top ------------------------------------------------------------- */
/* An always-available button when scrolled down past the first screen, so the
   user can get back to the top without scrolling through a long diff. */
#jump-top {{
  display: none;
  position: fixed; bottom: 80px; left: 20px; z-index: 2147482998;
  width: 36px; height: 36px; padding: 0;
  border: 1px solid var(--border-strong); border-radius: 999px;
  background: var(--surface); color: var(--muted);
  font: 16px/34px ui-sans-serif, system-ui, sans-serif;
  text-align: center; cursor: pointer;
  box-shadow: 0 1px 4px rgba(31,30,29,.12);
  opacity: .7; transition: opacity .15s;
}}
#jump-top:hover, #jump-top:focus {{ opacity: 1; color: var(--fg); }}
#jump-top:focus-visible {{ outline: 2px solid var(--accent); outline-offset: 2px; }}
#jump-top.visible {{ display: block; }}
/* On framed pages or when chrome is hidden, adjust position. */
body.framed #jump-top {{ display: none !important; }}
@media (max-width: 600px) {{
  #jump-top {{ left: 12px; bottom: 72px; width: 32px; height: 32px; font-size: 14px; line-height: 30px; }}
}}

/* Guided review ----------------------------------------------------------- */
/* The agent's tour of its own work. Measured column and prose type, because it
   is the one part of the page meant to be read rather than scanned. */
/* Left edge shared with `.wrap` below it, so the prose and the code it talks
   about line up on the same axis; the measure is bounded within that column
   rather than centred in it, which would read as a different page. */
section.brief {{
  /* A flex child of `body`, so it must be told to span the row: left to itself
     it shrinks to its content and drifts off the axis of the code below. */
  align-self: stretch; flex: 0 0 auto;
  box-sizing: border-box;
  border-bottom: 1px solid var(--border);
  font: 14px/1.65 ui-sans-serif, system-ui, sans-serif;
  /* Indented to `.wrap`'s own left edge — 1100px centred, plus its 20px gutter —
     so the prose starts on the same axis as the code it talks about. */
  padding: 18px 20px 20px max(20px, calc((100% - 1100px) / 2 + 20px));
}}
section.brief > * {{ max-width: 74ch; }}
section.brief > :first-child {{ margin-top: 0; }}
section.brief > :last-child {{ margin-bottom: 0; }}
/* A framed page does not scroll, so an unbounded brief would push the iframe out
   of view entirely. It scrolls within itself instead. */
body.framed section.brief {{ max-height: 30vh; overflow-y: auto; }}

/* Code -------------------------------------------------------------------- */
/* The card carries the syntax theme's own colours: the highlighting inside it
   was computed for that background, so the chrome's off-white cannot be used
   here without wrecking the contrast. */
section.file {{ margin: 0 0 28px; }}
section.file .filename {{
  display: flex; align-items: center; gap: 10px;
  font: 12px/1 ui-monospace, monospace; color: var(--muted);
  padding: 10px 12px; border: 1px solid var(--border-strong);
  border-bottom: none; border-radius: 8px 8px 0 0; background: var(--raised);
  position: sticky; top: 0; z-index: 1;
}}
/* Below the sticky jump list rather than under it: both stick, so without an
   offset the file header would slide underneath the nav. Measured, not guessed —
   the nav is 45px at one row. With the chrome hidden there is no nav to clear. */
body:not(.framed) section.file .filename {{ top: 45px; }}
body.nochrome section.file .filename {{ top: 0; }}
section.file .filename .meta {{ margin-left: auto; opacity: .8; }}
.codebox {{
  border: 1px solid var(--border-strong); border-radius: 0 0 8px 8px;
  overflow-x: auto; background: var(--code-bg); color: var(--code-fg);
}}
table.src {{
  border-collapse: collapse; width: 100%;
  font: 12.5px/1.55 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
}}
table.src td {{ padding: 0; vertical-align: top; }}
table.src td.ln {{
  width: 1%; min-width: 3.5em; text-align: right;
  padding: 0 12px 0 10px; user-select: none;
  color: var(--code-muted);
  border-right: 1px solid var(--code-border);
  background: var(--code-bg);
}}
/* The gutter only has anywhere to stick while the code box is panned sideways,
   and `position: sticky` is not free: two gutter cells a line is 7,200 sticky
   boxes on a 3,600-line diff, all of which the browser revisits in every
   pre-paint. Measured on Chromium 131, that costs ~13ms of main-thread work per
   keystroke in a comment box and per scrolled frame — for a gutter that is not
   moving. `panned` is put on by NAV_JS the moment a box is scrolled off zero,
   so the unpanned case, which is nearly all of them, pays nothing. */
.codebox.panned table.src td.ln {{ position: sticky; left: 0; }}
table.src td.ln a {{ color: inherit; text-decoration: none; }}
table.src td.code {{ padding: 0 14px; white-space: pre; }}
table.src tr:target td {{ background: var(--code-gutter); }}
table.src tr:hover td.code {{ background: color-mix(in srgb, var(--code-fg) 6%, transparent); }}

/* Diff -------------------------------------------------------------------- */
/* Added and removed lines are tinted by mixing into the code card's own
   background, so one rule works against every syntax theme. */
table.src.diff td.ln {{ min-width: 3em; }}
table.src.diff tr.add td {{ background: color-mix(in srgb, #3f7d58 16%, var(--code-bg)); }}
table.src.diff tr.del td {{ background: color-mix(in srgb, #b3452c 16%, var(--code-bg)); }}
table.src.diff tr.hunk td {{
  background: var(--code-gutter); color: var(--code-muted);
  padding: 3px 14px; white-space: pre; user-select: none;
}}
table.src.diff .marker {{ user-select: none; opacity: .65; }}
/* A run of lines the patch left out, and the way to get them. The row reads as a
   seam rather than as code, because what matters about it is that something is
   missing here. Its controls are painted by EXPAND_JS. */
table.src.diff tr.gap td {{
  background: var(--code-gutter); color: var(--code-muted);
  padding: 3px 14px; white-space: pre; user-select: none;
}}
table.src.diff tr.gap .expanders {{ display: inline-flex; gap: 4px; align-items: center; }}
table.src.diff tr.gap button {{
  font: 11px/1 ui-monospace, SFMono-Regular, Menlo, monospace;
  color: var(--code-fg);
  background: color-mix(in srgb, var(--code-fg) 8%, transparent);
  border: 1px solid var(--code-border); border-radius: 5px;
  padding: 3px 7px; cursor: pointer;
}}
table.src.diff tr.gap button:hover, table.src.diff tr.gap button:focus-visible {{
  background: var(--accent); border-color: var(--accent); color: #fff;
}}
table.src.diff tr.gap button[disabled] {{ opacity: .5; cursor: default; }}
/* The one part of the `@@` header worth keeping: which function the hunk is in. */
table.src.diff tr.gap .section {{ margin-left: 10px; opacity: .75; }}
table.src.diff tr.gap .msg {{ margin-left: 10px; color: #e08a7a; }}
/* Comment affordances are shared by the diff and the plain-code listing: both
   carry `commentable`, so one set of rules drives the `+` button, the inline
   note box, and the range highlight in either mode. The diff-only tints above
   stay scoped to `.diff`. */
/* The comment affordance sits in the code cell's left padding so it cannot
   shift the code, which would make the listing jump on hover. */
table.src.commentable td.code {{ position: relative; padding-left: 22px; }}
table.src.commentable .addnote {{
  position: absolute; left: 1px; top: 1px;
  width: 17px; height: 17px; padding: 0; line-height: 15px;
  border: none; border-radius: 4px; cursor: pointer;
  background: var(--accent); color: #fff; font: 600 13px/15px ui-sans-serif, sans-serif;
  opacity: 0; transition: opacity .1s;
}}
table.src.commentable tr:hover .addnote, table.src.commentable .addnote:focus {{ opacity: 1; }}
/* Comments break out of the code card's monospace world: they are prose. Inset
   as a card rather than a full-bleed band, so the listing still reads as one
   continuous block with a note tucked into it. */
table.src.commentable tr.notes td.code {{
  white-space: normal; padding: 6px 14px 6px 22px;
  background: var(--code-gutter);
  font: 13px/1.55 ui-sans-serif, system-ui, sans-serif;
}}
table.src.commentable .note {{
  background: var(--surface); color: var(--fg);
  border: 1px solid var(--border-strong); border-left: 2px solid var(--accent);
  border-radius: 0 8px 8px 0; padding: 7px 12px; max-width: 620px;
}}
table.src.commentable .note {{ margin: 4px 0; }}
table.src.commentable .note .who {{
  display: block; font-size: 10.5px; text-transform: uppercase;
  letter-spacing: .07em; color: var(--muted);
}}
/* A small link to copy the comment's URL: discoverable on hover, stays quiet. */
table.src.commentable .note .permalink {{
  float: right; text-decoration: none; color: var(--border-strong);
  font: 11px/1 ui-monospace, monospace; padding: 2px 4px; border-radius: 3px;
  opacity: 0; transition: opacity .12s;
}}
table.src.commentable .note:hover .permalink,
table.src.commentable .note .permalink:focus,
table.src.commentable .note .permalink:focus-visible {{ opacity: 1; }}
table.src.commentable .note .permalink:hover {{ color: var(--accent); background: var(--bg); }}
/* Which lines a note covers, when it is more than the one directly above it. */
table.src.commentable .note .who .span {{
  margin-left: 6px; text-transform: none; letter-spacing: 0;
  font-family: ui-monospace, monospace; opacity: .85;
}}
/* Rows a comment's range covers: while dragging, and once saved.
   Marked with a bar down the gutter rather than a background tint. Tinting fights
   the diff's own colours — an accent-tinted row reads as a deleted one — so the
   row background is left to mean add/remove and nothing else. */
table.src.commentable tr.inrange td.code, table.src.commentable tr.selecting td.code {{
  box-shadow: inset 3px 0 0 var(--accent);
}}
/* Mid-drag the whole span also lifts slightly, since nothing is committed yet and
   the feedback has to be unmistakable. */
table.src.commentable tr.selecting td {{
  background: color-mix(in srgb, var(--accent) 10%, var(--code-bg));
}}
/* A drag over code would otherwise select text and fight the range. */
table.src.commentable tr.selecting {{ user-select: none; }}
/* A discussion under a comment: the agent's answers and the reader's follow-ups.
   Indented under the note they belong to and marked by who said them, since the
   whole point is that two parties are talking. */
.note .said {{
  margin: 6px 0 0; padding: 5px 9px;
  border-left: 2px solid var(--border-strong);
  background: color-mix(in srgb, var(--fg) 3%, transparent);
  border-radius: 0 6px 6px 0;
}}
.note .said.agent {{ border-left-color: var(--accent); }}
.note .said .who {{
  display: block; font-size: 10.5px; text-transform: uppercase;
  letter-spacing: .07em; color: var(--muted);
}}
.note .said.agent .who {{ color: var(--accent); }}
/* The receipt and the reply control, painted by THREAD_JS. */
.note .threadfoot {{
  display: flex; align-items: center; gap: 8px;
  margin-top: 7px; font-size: 11.5px; color: var(--muted);
}}
.note .threadfoot .receipt {{ display: inline-flex; align-items: center; gap: 5px; }}
.note .threadfoot .dot {{
  width: 7px; height: 7px; border-radius: 50%; background: var(--border-strong);
}}
.note .threadfoot .waiting .dot {{
  background: var(--border-strong); animation: wdyt-pulse 1.8s ease-in-out infinite;
}}
.note .threadfoot .delivered .dot {{
  background: #b8860b; animation: wdyt-pulse 1.8s ease-in-out infinite;
}}
@media (prefers-reduced-motion: reduce) {{
  .note .threadfoot .waiting .dot, .note .threadfoot .delivered .dot {{ animation: none; }}
}}
.note .threadfoot button {{
  font: 11.5px/1 ui-sans-serif, system-ui, sans-serif;
  margin-left: auto; padding: 4px 9px; border-radius: 6px; cursor: pointer;
  border: 1px solid var(--border-strong); background: var(--bg); color: var(--fg);
}}
.note .threadfoot button:hover {{ border-color: var(--accent); color: var(--accent); }}

/* The pending editor floats over the listing instead of being a row inside it.
   A `<tr>` in the diff table means every character typed dirties a table that
   holds the whole file, and the browser answers by re-laying-out and repainting
   all of it: measured on Chromium 131 at 40ms of main-thread work per keystroke
   on a 3,600-line diff, against 12ms for the same box outside the table. So it
   is positioned in document coordinates by COMMENT_JS, below the line it is
   about — covering the lines after it, which are the ones already read. The
   shadow says it is above the code rather than in it. */
.notebox {{
  position: absolute; z-index: 6;
  display: flex; flex-direction: column; gap: 6px;
  max-width: 680px;
  background: var(--surface); color: var(--fg);
  border: 1px solid var(--border-strong); border-left: 2px solid var(--accent);
  border-radius: 0 8px 8px 0; padding: 7px 12px;
  box-shadow: 0 1px 2px rgba(31,30,29,.1), 0 10px 28px rgba(31,30,29,.22);
  font: 13px/1.55 ui-sans-serif, system-ui, sans-serif;
}}
/* What the box is about, when it is more than the line above it. */
.notebox .about {{
  font-size: 10.5px; text-transform: uppercase; letter-spacing: .07em;
  color: var(--muted);
}}
.notebox textarea {{
  width: 100%; min-height: 62px; resize: vertical;
  background: var(--bg); color: var(--fg);
  border: 1px solid var(--border-strong); border-radius: 8px; padding: 7px 9px;
  font: 13px/1.5 ui-sans-serif, system-ui, sans-serif;
}}
.notebox textarea:focus {{
  outline: none; border-color: var(--accent);
  box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 18%, transparent);
}}
.notebox .row {{ display: flex; gap: 6px; align-items: center; }}
.notebox .msg {{ color: #b3452c; font-size: 12px; margin-right: auto; }}
.notebox button {{
  font: 12.5px/1 ui-sans-serif, system-ui, sans-serif;
  padding: 6px 11px; border-radius: 7px; cursor: pointer;
  border: 1px solid var(--border-strong); background: var(--bg); color: var(--fg);
}}
.notebox button.primary {{
  background: var(--accent); border-color: var(--accent); color: #fff; font-weight: 600;
}}
/* Keyboard shortcut hint in comment editors: visible but unobtrusive. */
.kb-hint {{
  font: 11px/1 ui-sans-serif, system-ui, sans-serif;
  color: var(--muted); margin-right: auto;
}}
.kb-hint abbr {{
  text-decoration: none; border: none;
  font: inherit;
}}
.doc .doc-notebox .kb-hint {{ margin-right: auto; }}
.added {{ color: #3f7d58; }}
.removed {{ color: #b3452c; }}
nav.files .added, nav.files .removed {{ margin-left: 4px; }}

/* Deep-link highlights ---------------------------------------------------- */
/* A line targeted by a URL fragment (via :target or the JS-applied class). The
   whole row is tinted the accent, GitHub-style, so a hand-written deep link
   lands somewhere unmistakable even in a large diff; the class form is used by
   the fragment script for ranges and for hashchange without a reload. The mix
   is derived from the code card's own background, so one rule reads correctly
   against every syntax theme, light or dark. */
table.src tr:target td,
table.src tr.sel-highlight td {{
  background: color-mix(in srgb, var(--accent) 30%, var(--code-bg));
}}
/* A solid accent bar down the gutter so the eye finds the first line of the
   span at once, and the number itself shifts to the accent. `inset` on td.ln
   only, matching the ranged-comment marker so the two read as one language. */
table.src tr:target td.ln,
table.src tr.sel-highlight td.ln {{
  color: var(--accent); font-weight: 600;
  box-shadow: inset 3px 0 0 var(--accent);
}}
/* A linked comment pulses briefly so the eye finds it among a wall of notes. */
.note.sel-highlight {{
  animation: wdyt-flash 1.6s ease-out;
}}
@keyframes wdyt-flash {{
  0% {{ background: color-mix(in srgb, var(--accent) 22%, var(--surface)); }}
  100% {{ background: var(--surface); }}
}}
@media (prefers-reduced-motion: reduce) {{
  .note.sel-highlight {{ animation: none; }}
}}

/* Docs -------------------------------------------------------------------- */
.doc {{ font-size: 15px; }}
.doc h1, .doc h2, .doc h3 {{ line-height: 1.25; margin: 1.6em 0 .6em; }}
.doc h1 {{ font-size: 1.9em; }}
.doc h2 {{
  font-size: 1.45em; padding-bottom: .3em; border-bottom: 1px solid var(--border);
}}
.doc h3 {{ font-size: 1.15em; }}
.doc p, .doc ul, .doc ol, .doc blockquote, .doc table {{ margin: 0 0 1em; }}
.doc code {{
  font: .875em/1.5 ui-monospace, monospace;
  background: var(--raised); padding: .15em .4em; border-radius: 4px;
}}
/* Fenced blocks come out of the same syntect theme as `wdyt code`, so they
   get the code card's colours rather than the page's. */
.doc pre {{
  background: var(--code-bg); color: var(--code-fg);
  border: 1px solid var(--border-strong);
  border-radius: 8px; padding: 14px 16px; overflow-x: auto;
}}
.doc pre code {{ background: none; padding: 0; }}
.doc blockquote {{
  border-left: 3px solid var(--border-strong); margin-left: 0;
  padding-left: 1em; color: var(--muted);
}}
.doc table {{ border-collapse: collapse; }}
.doc th, .doc td {{
  border: 1px solid var(--border-strong); padding: 7px 12px; text-align: left;
}}
.doc th {{ background: var(--raised); }}
.doc img {{ max-width: 100%; }}
.doc hr {{ border: none; border-top: 1px solid var(--border); margin: 2em 0; }}

/* GitHub-style `> [!NOTE]` callouts, which comrak emits as .markdown-alert.
   Each keeps its conventional hue rather than the accent: the accent is
   terracotta, which a note styled in it would read as a warning. */
.doc .markdown-alert {{
  --alert: #3b6ea8;
  border-left: 3px solid var(--alert); border-radius: 0 8px 8px 0;
  padding: 2px 16px; margin: 0 0 1em;
  background: color-mix(in srgb, var(--alert) 7%, var(--surface));
}}
.doc .markdown-alert p {{ margin: .7em 0; }}
.doc .markdown-alert-title {{
  font-weight: 600; color: var(--alert);
  text-transform: uppercase; font-size: .82em; letter-spacing: .04em;
}}
.doc .markdown-alert-tip {{ --alert: #3f7d58; }}
.doc .markdown-alert-warning {{ --alert: #b07d18; }}
.doc .markdown-alert-caution {{ --alert: #b3452c; }}
.doc .markdown-alert-important {{ --alert: #7a5bb0; }}
/* Heading anchors: comrak emits an empty <a class="anchor">, shown on hover. */
.doc a.anchor {{ text-decoration: none; opacity: 0; margin-left: .3em; }}
/* Escaped as \0023 rather than a literal hash in quotes, which would
   terminate this raw string. */
.doc a.anchor::after {{ content: '\0023'; }}
.doc h1:hover a.anchor, .doc h2:hover a.anchor, .doc h3:hover a.anchor {{ opacity: .5; }}

/* Doc comments ------------------------------------------------------------ */
/* The `+` button on a commentable markdown block. Positioned inside the element
   it annotates, at the left margin, hidden until hover. Mirrors the code/diff
   affordance visually. */
.doc .doc-addnote {{
  position: absolute; left: -24px; top: 2px;
  width: 18px; height: 18px; padding: 0; line-height: 16px;
  border: none; border-radius: 4px; cursor: pointer;
  background: var(--accent); color: #fff; font: 600 13px/16px ui-sans-serif, sans-serif;
  opacity: 0; transition: opacity .1s;
  z-index: 1;
}}
.doc [data-sourcepos]:hover > .doc-addnote, .doc .doc-addnote:focus {{ opacity: 1; }}
/* The note box that appears when the user clicks `+`. */
.doc .doc-notebox {{
  background: var(--surface); color: var(--fg);
  border: 1px solid var(--border-strong); border-left: 2px solid var(--accent);
  border-radius: 0 8px 8px 0; padding: 10px 12px; margin: 6px 0; max-width: 620px;
}}
.doc .doc-notebox textarea {{
  width: 100%; min-height: 62px; resize: vertical;
  background: var(--bg); color: var(--fg);
  border: 1px solid var(--border-strong); border-radius: 8px; padding: 7px 9px;
  font: 13px/1.5 ui-sans-serif, system-ui, sans-serif;
}}
.doc .doc-notebox textarea:focus {{
  outline: none; border-color: var(--accent);
  box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 18%, transparent);
}}
.doc .doc-notebox .row {{ display: flex; gap: 6px; align-items: center; margin-top: 6px; }}
.doc .doc-notebox .msg {{ color: #b3452c; font-size: 12px; margin-right: auto; }}
.doc .doc-notebox button {{
  font: 12.5px/1 ui-sans-serif, system-ui, sans-serif;
  padding: 6px 11px; border-radius: 7px; cursor: pointer;
  border: 1px solid var(--border-strong); background: var(--bg); color: var(--fg);
}}
.doc .doc-notebox button.primary {{
  background: var(--accent); border-color: var(--accent); color: #fff; font-weight: 600;
}}
/* Saved comment notes on markdown blocks, reusing the same card style. */
.doc .doc-notes {{
  margin: 6px 0;
}}
.doc .doc-notes .note {{
  background: var(--surface); color: var(--fg);
  border: 1px solid var(--border-strong); border-left: 2px solid var(--accent);
  border-radius: 0 8px 8px 0; padding: 7px 12px; max-width: 620px;
  margin: 4px 0;
}}
.doc .doc-notes .note .who {{
  display: block; font-size: 10.5px; text-transform: uppercase;
  letter-spacing: .07em; color: var(--muted);
}}
.doc .doc-notes .note .who .span {{
  margin-left: 6px; text-transform: none; letter-spacing: 0;
  font-family: ui-monospace, monospace; opacity: .85;
}}
.doc .doc-notes .note .permalink {{
  float: right; text-decoration: none; color: var(--border-strong);
  font: 11px/1 ui-monospace, monospace; padding: 2px 4px; border-radius: 3px;
  opacity: 0; transition: opacity .12s;
}}
.doc .doc-notes .note:hover .permalink,
.doc .doc-notes .note .permalink:focus,
.doc .doc-notes .note .permalink:focus-visible {{ opacity: 1; }}
.doc .doc-notes .note .permalink:hover {{ color: var(--accent); background: var(--bg); }}
/* Deep-linked comment flash in doc notes. */
.doc .doc-notes .note.sel-highlight {{
  animation: wdyt-flash 1.6s ease-out;
}}

/* Framed content ---------------------------------------------------------- */
/* Both `dir` and `demo` hand the viewport to an iframe: the page itself must
   not scroll, and the frame must fill only the space below the chrome. */
.frame {{ width: 100%; height: 100%; border: 0; display: block; background: #fff; }}
.framewrap {{ position: absolute; inset: 0; }}
body.framed {{ overflow: hidden; }}
body.framed main {{ position: relative; }}

/* Chrome collapse --------------------------------------------------------- */
/* On a framed app the bar is 50 pixels of someone else's screen real estate,
   and reviewing a layout sometimes means seeing it at the size it will really
   have. Hiding it leaves a small pill to bring it back, because a page with no
   affordance at all is a page you have to reload to escape. */
#chrome-toggle {{
  font: 11px/1 ui-sans-serif, system-ui, sans-serif;
  padding: 5px 9px; border-radius: 6px; cursor: pointer;
  border: 1px solid var(--border-strong); background: var(--surface);
  color: var(--muted);
  /* The bar is a flex row, so a long title can squeeze this to one word per
     line. It is a two-word label: it should stay on one. */
  white-space: nowrap; flex: none;
}}
#chrome-toggle:hover {{ background: var(--bg); color: var(--fg); }}
body.nochrome header.bar, body.nochrome nav.files {{ display: none; }}
#chrome-show {{
  display: none;
  position: fixed; top: 0; left: 50%; transform: translateX(-50%);
  z-index: 2147483000;
  font: 11px/1 ui-sans-serif, system-ui, sans-serif;
  padding: 4px 12px 5px; border-radius: 0 0 8px 8px; cursor: pointer;
  border: 1px solid var(--border-strong); border-top: none;
  background: var(--surface); color: var(--muted);
  /* Mostly out of the way until the pointer finds it. */
  opacity: .35; transition: opacity .15s;
}}
#chrome-show:hover {{ opacity: 1; color: var(--fg); }}
body.nochrome #chrome-show {{ display: block; }}

/* Reply ------------------------------------------------------------------- */
/* Fixed and floating: on a demo it must not reflow the page being reviewed.
   Draggable, because the corner it defaults to is often where the framed app
   keeps its own controls. `left`/`top` are set by the drag handler, which is
   why `right`/`bottom` are the initial position rather than the only one. */
#reply {{
  position: fixed; right: 20px; bottom: 20px; z-index: 2147483000;
  font: 13px/1.5 ui-sans-serif, system-ui, sans-serif;
}}
#reply.moved {{ right: auto; bottom: auto; }}
#reply.dragging {{ opacity: .82; }}
/* Slightly recessive until hovered, so it reads as chrome over someone else's
   app rather than part of it. Dragging is what actually resolves a collision. */
#reply:not(.open) #toggle {{ opacity: .88; transition: opacity .15s; }}
#reply:not(.open):hover #toggle {{ opacity: 1; }}
#reply .grip {{
  cursor: grab; user-select: none; color: var(--border-strong);
  /* Padded rather than sized, so it is easy to grab on a trackpad. */
  padding: 2px 6px 2px 0; font-size: 14px; line-height: 1;
}}
#reply .grip:hover {{ color: var(--muted); }}
#reply.dragging .grip {{ cursor: grabbing; }}
/* Added for the duration of a drag: a cross-origin iframe would otherwise
   capture the pointer as soon as it crossed into it, stalling the drag. */
#reply-shield {{
  position: fixed; inset: 0; z-index: 2147482999; cursor: grabbing;
}}
#reply .panel {{
  display: none; flex-direction: column; gap: 8px;
  width: min(380px, calc(100vw - 40px));
  padding: 12px; border-radius: 12px;
  background: var(--surface); color: var(--fg);
  border: 1px solid var(--border-strong);
  /* A soft shadow rather than a hard one: it floats over someone else's UI. */
  box-shadow: 0 1px 2px rgba(31,30,29,.06), 0 12px 32px rgba(31,30,29,.16);
}}
#reply.open .panel {{ display: flex; }}
/* The form is the panel's only child in the unreplied state, so it carries the
   same column layout rather than letting block margins decide the spacing. */
#reply form {{ display: flex; flex-direction: column; gap: 8px; }}
#reply.open #toggle {{ display: none; }}
#reply .head {{
  display: flex; align-items: center; gap: 6px;
  color: var(--muted); font-size: 11px;
  text-transform: uppercase; letter-spacing: .06em;
}}
#reply textarea {{
  width: 100%; min-height: 84px; resize: vertical;
  background: var(--bg); color: var(--fg);
  border: 1px solid var(--border-strong); border-radius: 8px;
  padding: 9px 10px; font: 13px/1.5 ui-sans-serif, system-ui, sans-serif;
}}
#reply textarea:focus {{
  outline: none; border-color: var(--accent);
  box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 18%, transparent);
}}
#reply .row {{ display: flex; align-items: center; gap: 8px; }}
#reply .hint {{ color: var(--muted); font-size: 11.5px; margin-right: auto; }}
#reply button {{
  font: 13px/1 ui-sans-serif, system-ui, sans-serif;
  padding: 8px 13px; border-radius: 8px; cursor: pointer;
  border: 1px solid var(--border-strong); background: var(--bg); color: var(--fg);
}}
#reply button:hover {{ background: var(--raised); }}
#reply button.primary {{
  background: var(--accent); border-color: var(--accent);
  color: #fff; font-weight: 600;
}}
#reply button.primary:hover {{ background: #b5573a; }}
#reply button:disabled {{ opacity: .55; cursor: default; }}
/* Scoped `#reply #toggle` rather than `#toggle`: the generic `#reply button`
   above is an id plus a type selector, so it outranks a lone id and would win. */
#reply #toggle {{
  display: flex; align-items: center; gap: 7px;
  padding: 10px 16px; border-radius: 999px; cursor: pointer;
  background: var(--accent); color: #fff; font-weight: 600;
  border: 1px solid transparent;
  box-shadow: 0 1px 2px rgba(31,30,29,.1), 0 8px 22px rgba(31,30,29,.22);
}}
#reply #toggle:hover {{ background: #b5573a; }}
#reply #toggle.done {{
  background: var(--surface); color: var(--muted); font-weight: 500;
  border-color: var(--border-strong);
}}
#reply #toggle.done:hover {{ background: var(--raised); }}
#reply .row .spacer {{ flex: 1; }}
#reply .sent {{ color: var(--muted); }}
#reply .sent b {{ color: var(--fg); font-weight: 600; }}
/* Empty until something fails, so it must take no space until then. */
#reply .error {{ color: #b3452c; font-size: 12px; margin: 0; }}
#reply .error:empty {{ display: none; }}

/* Read receipt ------------------------------------------------------------ */
/* Sending a reply into a daemon is an act of faith otherwise: the agent may be
   waiting, or may have exited hours ago. The receipt says which — in three
   states, because "the bytes left the daemon" and "a model read them" are
   different claims and conflating them hides a dead agent. */
#reply .receipt {{
  display: flex; align-items: center; gap: 6px;
  color: var(--muted); font-size: 11.5px; margin: 0;
}}
#reply .receipt .dot {{
  width: 7px; height: 7px; border-radius: 999px; flex: 0 0 auto;
  background: var(--border-strong);
}}
/* Outstanding: a slow pulse, so a glance tells you it has not moved. */
#reply .receipt.waiting .dot {{
  background: var(--accent); animation: wdyt-pulse 1.8s ease-in-out infinite;
}}
/* Picked up but unacknowledged: amber, and still pulsing. This is where an agent
   that died on a credentials prompt sits, so it must not look like success. */
#reply .receipt.delivered .dot {{
  background: #b8860b; animation: wdyt-pulse 1.8s ease-in-out infinite;
}}
#reply .receipt.delivered {{ color: #8a6508; }}
#reply .receipt.read .dot {{ background: #3f7d58; }}
#reply .receipt.read {{ color: #3f7d58; }}
/* What the agent said it is doing: the only evidence a model was really there. */
#reply .acknote {{
  margin: 0; padding: 6px 9px;
  background: var(--surface); border: 1px solid var(--border-strong);
  border-left: 2px solid #3f7d58; border-radius: 0 6px 6px 0;
  font-size: 12px; color: var(--fg); white-space: pre-wrap;
}}
#reply .acknote:empty {{ display: none; }}
@keyframes wdyt-pulse {{
  0%, 100% {{ opacity: 1; }}
  50% {{ opacity: .25; }}
}}
@media (prefers-reduced-motion: reduce) {{
  #reply .receipt.waiting .dot, #reply .receipt.delivered .dot {{ animation: none; }}
}}
/* Mirrored onto the pill, so the receipt is visible without opening the panel. */
#reply #toggle.read {{
  background: var(--surface); color: #3f7d58; font-weight: 600;
  border-color: color-mix(in srgb, #3f7d58 45%, var(--border-strong));
}}
"#
    )
}

/// The reply widget's behaviour.
///
/// Written by hand rather than with topcoat's runtime because the runtime's
/// browser script is served from an asset bundle produced by the `topcoat`
/// CLI, and wdyt needs to run from a plain `cargo install`.
const REPLY_JS: &str = r#"
(function () {
  var root = document.getElementById('reply');
  if (!root) return;
  var toggle = document.getElementById('toggle');
  var form = root.querySelector('form');
  var box = root.querySelector('textarea');
  var send = root.querySelector('button.primary');
  var error = root.querySelector('.error');
  var closeBtn = root.querySelector('button.close');
  if (!toggle) return;

  function open() {
    root.classList.add('open');
    // The panel is much taller than the pill, so a widget dragged near the
    // bottom would open partly off screen.
    reclamp();
    // A session that already has a reply shows the sent text and has no box.
    if (box) box.focus();
  }
  function close() { root.classList.remove('open'); }

  // Set by a drag that started on the pill: the mouseup that ends the drag is
  // still followed by a click on the toggle, which would open the panel the
  // user was only trying to move.
  var justDragged = false;

  toggle.addEventListener('click', function () {
    if (justDragged) { justDragged = false; return; }
    open();
  });
  if (closeBtn) closeBtn.addEventListener('click', close);

  // Escape closes the panel, in every state.
  document.addEventListener('keydown', function (e) {
    if (e.key === 'Escape' && root.classList.contains('open')) close();
  });

  // --- Dragging ------------------------------------------------------------
  // The default corner is often where the framed app keeps its own controls,
  // so the widget can be moved out of the way. The position is remembered per
  // origin, since the same app tends to be reviewed repeatedly.
  var KEY = 'wdyt.reply.pos';

  function place(x, y) {
    // Clamp into view so it cannot be dragged somewhere unreachable.
    var rect = root.getBoundingClientRect();
    var maxX = Math.max(0, window.innerWidth - rect.width);
    var maxY = Math.max(0, window.innerHeight - rect.height);
    root.classList.add('moved');
    root.style.left = Math.min(Math.max(0, x), maxX) + 'px';
    root.style.top = Math.min(Math.max(0, y), maxY) + 'px';
  }

  try {
    var saved = JSON.parse(localStorage.getItem(KEY) || 'null');
    if (saved && typeof saved.x === 'number' && typeof saved.y === 'number') {
      place(saved.x, saved.y);
    }
  } catch (e) { /* private mode, or corrupt value: keep the default corner */ }

  // Keep it on screen after the panel opens (it is much taller than the pill)
  // and after the window is resized.
  function reclamp() {
    if (!root.classList.contains('moved')) return;
    var rect = root.getBoundingClientRect();
    place(rect.left, rect.top);
  }
  window.addEventListener('resize', reclamp);

  function startDrag(e) {
    // Left button only.
    if (e.button !== 0) return;
    var from = root.getBoundingClientRect();
    var dx = e.clientX - from.left;
    var dy = e.clientY - from.top;
    root.classList.add('dragging');

    // A framed demo is a cross-origin iframe: once the pointer crosses into it
    // the events go to the frame's document, not this one, and the drag stops
    // dead. A transparent shield over the whole viewport keeps them here.
    var shield = document.createElement('div');
    shield.id = 'reply-shield';
    document.body.appendChild(shield);

    function move(ev) { place(ev.clientX - dx, ev.clientY - dy); }
    function done() {
      root.classList.remove('dragging');
      shield.remove();
      document.removeEventListener('mousemove', move);
      document.removeEventListener('mouseup', done);
      try {
        var at = root.getBoundingClientRect();
        localStorage.setItem(KEY, JSON.stringify({ x: at.left, y: at.top }));
      } catch (e2) { /* not persisting is fine */ }
    }
    document.addEventListener('mousemove', move);
    document.addEventListener('mouseup', done);
    e.preventDefault();
  }

  // The open panel drags by its grip; the collapsed pill drags from anywhere on
  // itself, since a small pill is an awkward thing to put a separate handle on.
  var grip = root.querySelector('.grip');
  if (grip) grip.addEventListener('mousedown', startDrag);

  toggle.addEventListener('mousedown', function (e) {
    // A drag only begins once the pointer has actually moved a few pixels, so
    // that a plain click still opens the panel.
    var sx = e.clientX, sy = e.clientY, started = false;
    function maybe(ev) {
      if (started) return;
      if (Math.abs(ev.clientX - sx) + Math.abs(ev.clientY - sy) > 4) {
        started = true;
        // The click that follows this drag's mouseup must not open the panel.
        justDragged = true;
        document.removeEventListener('mousemove', maybe);
        startDrag(e);
      }
    }
    function stop() {
      document.removeEventListener('mousemove', maybe);
      document.removeEventListener('mouseup', stop);
    }
    document.addEventListener('mousemove', maybe);
    document.addEventListener('mouseup', stop);
  });

  // --- Read receipt --------------------------------------------------------
  // A reply sits in the daemon until an agent collects it, which may be
  // immediately or hours later. Polling the status endpoint — which never marks
  // anything itself — turns that into something visible.
  //
  // Three stages, because collection and reading are different events: an agent
  // can pick a reply up and then die before a model ever sees it.
  var session = root.getAttribute('data-session');

  var RECEIPT_TEXT = {
    read: 'The agent read this and replied',
    delivered: 'Picked up by an agent, not yet acknowledged',
    waiting: 'Waiting for an agent to pick this up'
  };

  function receiptEl() { return root.querySelector('.receipt'); }

  function showReceipt(stage, note) {
    var el = receiptEl();
    if (!el) return;
    el.className = 'receipt ' + stage;
    el.querySelector('.text').textContent = RECEIPT_TEXT[stage];
    if (stage === 'read') toggle.classList.add('read');

    // The agent's own note about what it is doing, inserted after the receipt.
    if (!note) return;
    var panel = el.parentNode;
    var slot = panel.querySelector('.acknote');
    if (!slot) {
      slot = document.createElement('p');
      slot.className = 'acknote';
      el.insertAdjacentElement('afterend', slot);
    }
    slot.textContent = note;
  }

  function watch() {
    if (!session) return;
    var stop = false;
    function poll() {
      if (stop) return;
      fetch('/api/sessions/' + session + '/status')
        .then(function (res) { return res.ok ? res.json() : null; })
        .then(function (s) {
          if (!s || !s.replied) return;
          var stage = s.ack ? 'read' : (s.delivered ? 'delivered' : 'waiting');
          showReceipt(stage, s.ack && s.ack.note);
          // An ack is terminal: there is nothing further to learn. Delivery is
          // not — the interesting question is whether an ack ever follows.
          if (s.ack) stop = true;
        })
        .catch(function () { /* daemon restarted or gone: keep trying */ })
        .then(function () { if (!stop) setTimeout(poll, 3000); });
    }
    poll();
  }

  // A page loaded after a reply was already sent starts watching immediately;
  // the server has rendered the receipt in whichever state it was in.
  if (receiptEl()) watch();

  // The rest is sending, which only applies while the form is still there.
  if (!form || !box || !send || !error) return;

  // Cmd/Ctrl+Enter sends without reaching for a mouse.
  box.addEventListener('keydown', function (e) {
    if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') form.requestSubmit();
  });

  form.addEventListener('submit', function (e) {
    e.preventDefault();
    var text = box.value.trim();
    if (!text) { box.focus(); return; }

    send.disabled = true;
    error.textContent = '';
    fetch(form.action, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ text: text })
    }).then(function (res) {
      return res.json().then(function (body) { return { ok: res.ok, body: body }; });
    }).then(function (r) {
      if (!r.ok || !r.body.accepted) {
        throw new Error(r.body && r.body.message ? r.body.message : 'could not send');
      }
      // Replace the form with the sent text: this is a single reply, so
      // there is nothing left to type into.
      var panel = root.querySelector('.panel');
      panel.innerHTML = '';
      var p = document.createElement('p');
      p.className = 'sent';
      p.style.margin = '0';
      var label = document.createElement('b');
      label.textContent = 'Sent to the agent';
      p.appendChild(label);
      p.appendChild(document.createElement('br'));
      p.appendChild(document.createTextNode(r.body.text));
      panel.appendChild(p);

      // Same element the server renders, so `showReceipt` works either way.
      var receipt = document.createElement('p');
      receipt.className = 'receipt waiting';
      var dot = document.createElement('span');
      dot.className = 'dot';
      var text = document.createElement('span');
      text.className = 'text';
      text.textContent = RECEIPT_TEXT.waiting;
      receipt.appendChild(dot);
      receipt.appendChild(text);
      panel.appendChild(receipt);

      // Same shape as the server renders for an already-replied session, so
      // the panel can still be dismissed and reopened.
      var row = document.createElement('div');
      row.className = 'row';
      var spacer = document.createElement('span');
      spacer.className = 'spacer';
      var dismiss = document.createElement('button');
      dismiss.type = 'button';
      dismiss.className = 'close';
      dismiss.textContent = 'Close';
      dismiss.addEventListener('click', close);
      row.appendChild(spacer);
      row.appendChild(dismiss);
      panel.appendChild(row);

      toggle.textContent = 'Reply sent';
      toggle.className = 'done';
      // Start watching for the agent to pick it up.
      watch();
    }).catch(function (err) {
      error.textContent = String(err.message || err);
      send.disabled = false;
    });
  });
})();
"#;

/// Showing and hiding the top bar.
///
/// The preference is remembered because someone who wants the chrome gone
/// usually wants it gone for the whole review, and a page reload is part of
/// reviewing a live demo.
const CHROME_JS: &str = r#"
(function () {
  var hide = document.getElementById('chrome-toggle');
  var show = document.getElementById('chrome-show');
  if (!hide || !show) return;
  var KEY = 'wdyt.chrome.hidden';

  function apply(hidden) {
    document.body.classList.toggle('nochrome', hidden);
    try { localStorage.setItem(KEY, hidden ? '1' : '0'); } catch (e) {}
  }

  try { if (localStorage.getItem(KEY) === '1') apply(true); } catch (e) {}

  hide.addEventListener('click', function () { apply(true); });
  show.addEventListener('click', function () { apply(false); });

  // A keyboard escape hatch, since the show pill is deliberately faint.
  document.addEventListener('keydown', function (e) {
    if (e.key !== '.' || e.metaKey || e.ctrlKey || e.altKey) return;
    var t = e.target;
    if (t && (t.tagName === 'TEXTAREA' || t.tagName === 'INPUT')) return;
    apply(!document.body.classList.contains('nochrome'));
  });
})();
"#;

/// Navigation enhancements: jump-to-top button and sidebar active-file tracking.
///
/// The jump-to-top appears after scrolling past the first screenful. The sidebar
/// highlights the file whose section is currently in view, using IntersectionObserver
/// for efficiency.
pub(crate) const NAV_JS: &str = r##"
(function () {
  // --- Sticky gutters, only while panned -----------------------------------
  // The line-number gutter is `position: sticky` so it survives panning a long
  // line sideways, but two sticky cells per line is thousands of sticky boxes the
  // browser revisits in every pre-paint — for a gutter that, unpanned, has
  // nowhere to go. The class turns the rule on for the box being panned only.
  var boxes = document.querySelectorAll('.codebox');
  for (var bi = 0; bi < boxes.length; bi++) {
    (function (codebox) {
      codebox.addEventListener('scroll', function () {
        var panned = codebox.scrollLeft > 0;
        // Only on the transition: toggling every event would re-invalidate the
        // gutter's style on every frame of a pan.
        if (panned !== codebox.classList.contains('panned')) {
          codebox.classList.toggle('panned', panned);
        }
      }, { passive: true });
    })(boxes[bi]);
  }

  // --- Jump to top ---------------------------------------------------------
  var btn = document.getElementById('jump-top');
  if (btn) {
    var threshold = 300;
    var ticking = false;
    function checkScroll() {
      ticking = false;
      btn.classList.toggle('visible', window.scrollY > threshold);
    }
    window.addEventListener('scroll', function () {
      if (!ticking) { ticking = true; requestAnimationFrame(checkScroll); }
    }, { passive: true });
    btn.addEventListener('click', function () {
      var prefersReduced = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
      window.scrollTo({ top: 0, behavior: prefersReduced ? 'auto' : 'smooth' });
    });
    checkScroll();
  }

  // --- Sidebar active tracking ---------------------------------------------
  var nav = document.querySelector('nav.files.has-sidebar');
  if (!nav) return;

  // Add the body class so CSS can shift the main content.
  document.body.classList.add('has-file-sidebar');

  var links = nav.querySelectorAll('a[href^="#"]');
  if (!links.length) return;

  // Map section ids to their nav links.
  var sections = [];
  for (var i = 0; i < links.length; i++) {
    var href = links[i].getAttribute('href');
    if (!href || href.length < 2) continue;
    var target = document.getElementById(href.slice(1));
    if (target) sections.push({ el: target, link: links[i] });
  }
  if (!sections.length) return;

  var current = null;
  var observer = new IntersectionObserver(function (entries) {
    // Find the topmost visible section.
    var visible = null;
    for (var j = 0; j < entries.length; j++) {
      if (entries[j].isIntersecting) {
        if (!visible || entries[j].boundingClientRect.top < visible.boundingClientRect.top) {
          visible = entries[j];
        }
      }
    }
    if (visible) {
      var id = visible.target.id;
      if (id !== current) {
        current = id;
        for (var k = 0; k < sections.length; k++) {
          sections[k].link.classList.toggle('active', sections[k].el.id === id);
        }
      }
    }
  }, { rootMargin: '-10% 0px -70% 0px', threshold: 0 });

  for (var si = 0; si < sections.length; si++) {
    observer.observe(sections[si].el);
  }
})();
"##;

/// Line comments on a diff.
///
/// The box is opened next to the line rather than in a dialog: a review comment
/// only makes sense against the code it is about, and moving the eye away from
/// the line to a modal loses exactly the context that matters.
pub(crate) const COMMENT_JS: &str = r#"
(function () {
  var root = document.querySelector('[data-comments]');
  if (!root) return;
  var session = root.getAttribute('data-session');
  var open = null;
  // A drag in progress: the `+` it started on, and the row it is currently over.
  var drag = null;

  // --- Helpers for safe integer and range validation -------------------------
  function safeLineNo(n) {
    var v = Number(n);
    return Number.isFinite(v) && v > 0 && v <= Number.MAX_SAFE_INTEGER && v === Math.floor(v) ? v : null;
  }

  function close() {
    if (open) { open.remove(); open = null; }
    clearPreview();
  }

  function clearPreview() {
    var marked = root.querySelectorAll('tr.selecting');
    for (var i = 0; i < marked.length; i++) marked[i].classList.remove('selecting');
  }

  // Every commentable row on one side of one file, in document order.
  // Uses attribute comparison on actual DOM elements rather than string
  // interpolation into selectors, which is unsafe for paths with quotes.
  //
  // Memoised, because the scan is over every `+` in the page — thousands of them
  // on a real diff — and a drag would otherwise repeat it on every pointermove.
  // Safe to cache for the session: `.addnote` buttons are rendered once by the
  // server and never added or removed, only note rows are.
  var siblingCache = Object.create(null);
  function siblings(button) {
    var file = button.getAttribute('data-file');
    var side = button.getAttribute('data-side');
    var key = side + '\u0000' + file;
    if (siblingCache[key]) return siblingCache[key];
    var all = root.querySelectorAll('.addnote');
    var rows = [];
    for (var i = 0; i < all.length; i++) {
      if (all[i].getAttribute('data-file') === file &&
          all[i].getAttribute('data-side') === side) {
        rows.push(all[i]);
      }
    }
    siblingCache[key] = rows;
    return rows;
  }

  // Revealed context brings `+` buttons the memo has never seen, so it must not
  // outlive a reveal. Announced by EXPAND_JS rather than watched for, since a
  // MutationObserver over a whole diff is a poor way to learn one fact.
  root.addEventListener('wdyt:revealed', function () {
    siblingCache = Object.create(null);
  });

  // Highlight what a range would cover while the pointer is still down.
  function preview(from, to) {
    clearPreview();
    var lo = safeLineNo(from.getAttribute('data-line'));
    var hi = safeLineNo(to.getAttribute('data-line'));
    if (lo == null || hi == null) return;
    if (lo > hi) { var t = lo; lo = hi; hi = t; }
    mark(from, lo, hi, 'selecting');
  }

  // Mark every rendered row a range covers. Iterates actual rendered buttons
  // rather than a numeric span, so a hostile huge range cannot cause unbounded
  // work.
  function mark(from, lo, hi, cls) {
    var buttons = siblings(from);
    for (var i = 0; i < buttons.length; i++) {
      var n = safeLineNo(buttons[i].getAttribute('data-line'));
      if (n != null && n >= lo && n <= hi) buttons[i].closest('tr').classList.add(cls);
    }
  }

  // A range is selected by pressing on one `+` and releasing on another. Pointer
  // events rather than mouse ones, so a trackpad drag and a touch both work.
  root.addEventListener('pointerdown', function (e) {
    var button = e.target.closest ? e.target.closest('.addnote') : null;
    if (!button || e.button !== 0) return;
    drag = { from: button, moved: false };
  });

  root.addEventListener('pointermove', function (e) {
    if (!drag) return;
    var over = e.target.closest ? e.target.closest('tr') : null;
    if (!over) return;
    var button = over.querySelector('.addnote');
    // Only within the same file and side: a range across a file boundary has no
    // meaning, and the server would reject it anyway.
    if (!button
        || button.getAttribute('data-file') !== drag.from.getAttribute('data-file')
        || button.getAttribute('data-side') !== drag.from.getAttribute('data-side')) return;
    // Still over the row the last move already drew: redrawing it would retint
    // the same rows and repaint the table for nothing, several times a second.
    if (button === drag.to) return;
    if (button !== drag.from) drag.moved = true;
    drag.to = button;
    if (drag.moved) preview(drag.from, button);
  });

  document.addEventListener('pointerup', function () {
    if (!drag) return;
    var from = drag.from;
    var to = drag.moved && drag.to ? drag.to : from;
    drag = null;
    clearPreview();
    openBox(from, to);
  });

  function noteRow(anchor) {
    // Comments on a line live in one `tr.notes` immediately after it, so a
    // second comment joins the first instead of starting a new block.
    var next = anchor.nextElementSibling;
    if (next && next.classList.contains('notes')) return next;
    var row = document.createElement('tr');
    row.className = 'notes';
    row.innerHTML = '<td class="ln" colspan="2"></td><td class="code"></td>';
    anchor.parentNode.insertBefore(row, next);
    return row;
  }

  function render(comment, into) {
    var note = document.createElement('div');
    note.className = 'note';
    // Stable anchor so the fragment script can scroll to it.
    if (comment.id != null) note.id = 'comment-' + comment.id;
    // Marks it as the head of a discussion, which is what THREAD_JS looks for:
    // the comment just made can be answered without reloading the page.
    if (comment.id != null) note.setAttribute('data-thread', comment.id);
    var who = document.createElement('span');
    who.className = 'who';
    who.textContent = 'you';
    // Same shape the server renders, so a reload looks identical.
    if (comment.end_line && comment.end_line > comment.line) {
      var span = document.createElement('span');
      span.className = 'span';
      span.textContent = 'L' + comment.line + '\u2013L' + comment.end_line;
      who.appendChild(span);
    }
    note.appendChild(who);
    // Discoverable permalink.
    if (comment.id != null) {
      var plink = document.createElement('a');
      plink.className = 'permalink';
      plink.href = '#comment-' + comment.id;
      plink.title = 'Link to this comment';
      plink.textContent = '#';
      note.appendChild(plink);
    }
    note.appendChild(document.createTextNode(comment.text));
    into.appendChild(note);
  }

  // Base64url encode (no padding) for building sel- fragments.
  function b64encode(s) {
    return btoa(unescape(encodeURIComponent(s)))
      .replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
  }

  function selFragment(path, side, start, end) {
    var enc = b64encode(path);
    if (end && end !== start) return 'sel-' + enc + '-' + side + '-' + start + '-' + end;
    return 'sel-' + enc + '-' + side + '-' + start;
  }

  // The editor floats over the listing instead of being a row inside it: a `<tr>`
  // in the diff table means every keystroke dirties a table holding the whole
  // file, which the browser answers by re-laying-out and repainting all of it
  // (measured: 40ms a character on a 3,600-line diff, 12ms for the same box
  // outside the table). Placed in document coordinates rather than fixed ones, so
  // it travels with the code as the page scrolls without a scroll handler.
  function place(box, row) {
    var code = row.querySelector('td.code') || row;
    var r = code.getBoundingClientRect();
    box.style.left = (r.left + window.scrollX + 20) + 'px';
    box.style.top = (r.bottom + window.scrollY + 2) + 'px';
    box.style.width = Math.max(280, Math.min(660, r.width - 40)) + 'px';
  }

  // A resize changes the width the box was measured against.
  window.addEventListener('resize', function () {
    if (open && open.anchorRow) place(open, open.anchorRow);
  });

  function openBox(from, to) {
    // The box hangs below the last line of the range, so the whole passage it is
    // about stays visible above it.
    var lo = safeLineNo(from.getAttribute('data-line'));
    var hi = safeLineNo(to.getAttribute('data-line'));
    if (lo == null || hi == null) return;
    if (lo > hi) { var t = lo; lo = hi; hi = t; }
    var last = hi === Number(from.getAttribute('data-line')) ? from : to;
    var line = last.closest('tr');

    // Pressing the same line twice closes the box rather than opening a second.
    var reopen = !open || open.anchorRow !== line;
    close();
    if (!reopen) return;

    // Update the URL fragment to the selection so the range is copyable.
    var filePath = from.getAttribute('data-file');
    var side = from.getAttribute('data-side');
    var frag = selFragment(filePath, side, lo, hi);
    history.replaceState(null, '', '#' + frag);

    var span = hi > lo ? ' (L' + lo + '\u2013L' + hi + ')' : '';
    var label = hi > lo ? 'Comment on lines ' + lo + ' to ' + hi : 'Comment on this line';
    var mac = navigator.platform && navigator.platform.indexOf('Mac') > -1;
    var chord = mac ? 'Cmd' : 'Ctrl';
    var box = document.createElement('div');
    box.className = 'notebox';
    box.innerHTML =
      (hi > lo ? '<span class="about">Lines ' + lo + '\u2013' + hi + '</span>' : '') +
      '<textarea aria-label="' + label + '"></textarea>' +
      '<div class="row"><span class="msg"></span>' +
      '<kbd class="kb-hint" aria-label="Keyboard shortcuts: ' + chord +
      '+Enter to send, Escape to cancel">' +
      '<abbr title="' + chord + '+Enter">' + (mac ? '\u2318+Enter' : 'Ctrl+Enter') +
      '</abbr> send \u00B7 <abbr title="Escape">Esc</abbr> cancel</kbd>' +
      '<button type="button" class="cancel">Cancel</button>' +
      '<button type="button" class="primary">Comment</button>' +
      '</div>';
    // The row it is about, for repositioning and for where the saved note goes.
    box.anchorRow = line;
    document.body.appendChild(box);
    place(box, line);
    open = box;

    // Keep the covered rows marked while the box is open, so it is unambiguous
    // what a multi-line comment is about.
    if (hi > lo) mark(from, lo, hi, 'selecting');

    var textbox = box.querySelector('textarea');
    var submit = box.querySelector('button.primary');
    var msg = box.querySelector('.msg');
    textbox.placeholder =
      (hi > lo ? 'Comment on these lines' : 'Comment on this line') + span + '\u2026';
    textbox.focus();

    // A box opened on the last visible line would hang below the fold. It is in
    // document coordinates, so scrolling brings it and the code up together.
    var below = box.getBoundingClientRect().bottom - (window.innerHeight - 12);
    if (below > 0) window.scrollBy(0, below);

    box.querySelector('button.cancel').addEventListener('click', close);
    textbox.addEventListener('keydown', function (ev) {
      if (ev.key === 'Escape') { close(); return; }
      if ((ev.metaKey || ev.ctrlKey) && ev.key === 'Enter') submit.click();
    });

    submit.addEventListener('click', function () {
      var text = textbox.value.trim();
      if (!text) { textbox.focus(); return; }
      submit.disabled = true;
      msg.textContent = '';
      fetch('/s/' + session + '/comments', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          file: from.getAttribute('data-file'),
          line: lo,
          end_line: hi,
          side: from.getAttribute('data-side'),
          text: text
        })
      }).then(function (res) {
        if (!res.ok) throw new Error('could not save the comment');
        return res.json();
      }).then(function (comment) {
        // The editor gives way to the saved comment, under the line it is about.
        close();
        // The rows the comment covers keep their mark, now permanently.
        if (hi > lo) mark(from, lo, hi, 'inrange');
        render(comment, noteRow(line).querySelector('td.code'));
        // A comment is the head of a discussion: the thread script paints its
        // receipt and reply control, and should not wait for its next poll to
        // notice a thread the reader is looking at right now.
        root.dispatchEvent(new CustomEvent('wdyt:commented'));
        // Update the URL to point at the new comment for deep-linking.
        if (comment.id != null) {
          history.replaceState(null, '', '#comment-' + comment.id);
        }
      }).catch(function (err) {
        msg.textContent = String(err.message || err);
        submit.disabled = false;
      });
    });
  }
})();
"#;

/// Revealing the lines a patch leaves out.
///
/// A unified diff shows a few lines around each change and nothing in between,
/// which is fine until a hunk stops making sense on its own — and then the reader
/// has to go and open the file, which is the moment the review stops. The daemon
/// has the whole file (see `DiffFile::source`), so the gap between two hunks can
/// be asked for and dropped into the listing.
///
/// The band the server renders carries only the range it covers; its controls and
/// counts are painted here so those labels have one implementation rather than
/// one in Rust and one in JavaScript, and so a page without script shows no
/// buttons that cannot work.
pub(crate) const EXPAND_JS: &str = r##"
(function () {
  var root = document.querySelector('[data-comments]');
  if (!root) return;
  var session = root.getAttribute('data-session');
  // About a screenful, and what a reviewer means by "a bit more"; the whole gap
  // is one click away for when it is not.
  var STEP = 20;
  // A gap can be thousands of lines long. One click inserts at most this many
  // rows, and the daemon caps the request as well.
  var MAX_AT_ONCE = 1000;

  function range(band) {
    var from = parseInt(band.getAttribute('data-from'), 10);
    var to = parseInt(band.getAttribute('data-to'), 10);
    if (!Number.isFinite(from) || !Number.isFinite(to) || from < 1 || to < from) return null;
    return { from: from, to: to, count: to - from + 1 };
  }

  function control(take, label, title) {
    var button = document.createElement('button');
    button.type = 'button';
    button.className = 'expand';
    button.setAttribute('data-take', take);
    button.textContent = label;
    button.title = title;
    return button;
  }

  function paint(band) {
    var cell = band.querySelector('td.code');
    if (!cell) return;
    // The `@@` line's section heading, which the server put here and which
    // outlives every reveal.
    var section = cell.querySelector('.section');
    cell.textContent = '';
    var left = range(band);
    if (!left) { if (section) cell.appendChild(section); return; }
    var group = document.createElement('span');
    group.className = 'expanders';
    // At the end of a file there is nothing below the band, so growing upward
    // from below it would leave the revealed lines stranded under a gap.
    var trailing = band.getAttribute('data-place') === 'after';
    if (left.count > STEP) {
      group.appendChild(control('head', '\u2193 ' + STEP,
        'Show ' + STEP + ' lines from line ' + left.from));
    }
    group.appendChild(control('all',
      '\u22EF ' + left.count + (left.count === 1 ? ' line' : ' lines'),
      'Show all the hidden lines, ' + left.from + ' to ' + left.to));
    if (left.count > STEP && !trailing) {
      group.appendChild(control('tail', '\u2191 ' + STEP,
        'Show ' + STEP + ' lines up to line ' + left.to));
    }
    cell.appendChild(group);
    if (section) cell.appendChild(section);
  }

  // A revealed line looks exactly like a context line the patch did include,
  // because that is what it is: same columns, same comment affordance.
  function contextRow(line, file) {
    var row = document.createElement('tr');
    row.className = 'ctx';
    var old = document.createElement('td');
    old.className = 'ln old';
    if (line.old != null) old.textContent = line.old;
    var current = document.createElement('td');
    current.className = 'ln new';
    current.textContent = line.new;
    var code = document.createElement('td');
    code.className = 'code';
    var add = document.createElement('button');
    add.type = 'button';
    add.className = 'addnote';
    add.title = 'Comment on this line, or drag to cover more';
    add.setAttribute('data-file', file);
    add.setAttribute('data-line', line.new);
    add.setAttribute('data-side', 'new');
    add.textContent = '+';
    code.appendChild(add);
    var marker = document.createElement('span');
    marker.className = 'marker';
    marker.textContent = ' ';
    code.appendChild(marker);
    var text = document.createElement('span');
    // Highlighted by the daemon, from the same syntect pass the diff came from.
    text.innerHTML = line.html;
    code.appendChild(text);
    row.appendChild(old);
    row.appendChild(current);
    row.appendChild(code);
    return row;
  }

  function reveal(band, take) {
    var left = range(band);
    if (!left) return;
    var from = left.from;
    var to = left.to;
    if (take === 'head') to = Math.min(to, from + STEP - 1);
    else if (take === 'tail') from = Math.max(from, to - STEP + 1);
    else to = Math.min(to, from + MAX_AT_ONCE - 1);

    var buttons = band.querySelectorAll('button');
    for (var i = 0; i < buttons.length; i++) buttons[i].disabled = true;

    // Every revealed row lands above this one, whichever end was taken, so it is
    // the thing to keep still: inserting lines above the fold would otherwise
    // carry off whatever the reader was looking at.
    var anchor = band.nextElementSibling || band.previousElementSibling || band;
    var wasAt = anchor.getBoundingClientRect().top;

    fetch('/s/' + session + '/context?file=' + encodeURIComponent(band.getAttribute('data-index')) +
          '&from=' + from + '&to=' + to)
      .then(function (res) {
        if (!res.ok) throw new Error('could not load those lines');
        return res.json();
      })
      .then(function (data) {
        var rows = document.createDocumentFragment();
        for (var i = 0; i < data.lines.length; i++) {
          rows.appendChild(contextRow(data.lines[i], data.file));
        }
        // The band stays inside what is still hidden: lines taken from the top go
        // above it, lines taken from the bottom go below.
        if (take === 'tail') {
          band.parentNode.insertBefore(rows, band.nextSibling);
          band.setAttribute('data-to', from - 1);
        } else {
          band.parentNode.insertBefore(rows, band);
          band.setAttribute('data-from', to + 1);
        }
        if (range(band)) paint(band); else band.remove();
        // The comment script memoises the `+` buttons a drag can run between, and
        // there are new ones now.
        root.dispatchEvent(new CustomEvent('wdyt:revealed'));
        window.scrollBy(0, anchor.getBoundingClientRect().top - wasAt);
      })
      .catch(function (err) {
        paint(band);
        var cell = band.querySelector('td.code');
        if (!cell) return;
        var msg = document.createElement('span');
        msg.className = 'msg';
        msg.textContent = String(err.message || err);
        cell.appendChild(msg);
      });
  }

  root.addEventListener('click', function (e) {
    var button = e.target.closest ? e.target.closest('.expand') : null;
    if (!button) return;
    var band = button.closest('tr.gap');
    if (band) reveal(band, button.getAttribute('data-take'));
  });

  var bands = root.querySelectorAll('tr.gap');
  for (var i = 0; i < bands.length; i++) paint(bands[i]);
})();
"##;

/// The discussion under a comment: the agent's answers, and answering back.
///
/// A comment used to be a note the agent read once, hours later, in a `collect`.
/// The thing a reader actually wants at that line is an answer — "why is this a
/// clone?" is a question, not a remark — so a comment is the first message in a
/// thread the agent can reply to while the page is still open.
///
/// The page polls rather than holding a socket open: the exchange is a few
/// sentences a minute at its busiest, a poll survives a daemon restart with no
/// reconnection logic, and it costs nothing while the tab is in the background.
/// It also polls a read-only route, so watching for an answer never marks the
/// reader's own question as picked up.
pub(crate) const THREAD_JS: &str = r##"
(function () {
  var root = document.querySelector('[data-comments]');
  if (!root) return;
  var session = root.getAttribute('data-session');
  // Often enough that an answer feels like an answer, rarely enough that a page
  // left open all afternoon is not a load on the daemon.
  var EVERY = 3000;
  var timer = null;
  // The reply box, if one is open. One at a time, like the comment editor.
  var box = null;

  function notes() { return root.querySelectorAll('.note[data-thread]'); }

  function foot(note) {
    var found = note.querySelector('.threadfoot');
    if (!found) {
      found = document.createElement('div');
      found.className = 'threadfoot';
      note.appendChild(found);
    }
    return found;
  }

  // What the reader is owed. Null when the agent has spoken last; otherwise
  // whether anything has picked the question up yet.
  function pending(thread) {
    var replies = thread.replies || [];
    var last = replies.length ? replies[replies.length - 1] : null;
    if (last) {
      if (last.from !== 'user') return null;
      return { delivered: !!last.delivered_at };
    }
    return { delivered: !!thread.delivered_at };
  }

  function said(message) {
    var div = document.createElement('div');
    div.className = 'said ' + (message.from === 'agent' ? 'agent' : 'user');
    div.setAttribute('data-message', message.id);
    var who = document.createElement('span');
    who.className = 'who';
    who.textContent = message.from === 'agent' ? 'agent' : 'you';
    div.appendChild(who);
    div.appendChild(document.createTextNode(message.text));
    return div;
  }

  // Whatever this note has not shown yet, in order, above the foot.
  function catchUp(note, thread) {
    var known = {};
    var shown = note.querySelectorAll('[data-message]');
    for (var i = 0; i < shown.length; i++) known[shown[i].getAttribute('data-message')] = true;
    var replies = thread.replies || [];
    var mark = foot(note);
    var added = 0;
    for (var j = 0; j < replies.length; j++) {
      if (known[String(replies[j].id)]) continue;
      note.insertBefore(said(replies[j]), mark);
      added++;
    }
    return added;
  }

  // The three states the session's own receipt uses, per thread: nobody has this
  // yet, an agent has it, the agent has answered — and the answer is the message
  // above, which is why an answered thread says nothing here.
  //
  // Repainted only when it would say something different. A poll that rebuilds
  // the foot of every thread dirties the table it sits in, and the whole diff is
  // relaid out and repainted for a receipt that has not changed — several times a
  // minute, for as long as the page is open.
  function paint(note, thread) {
    var owed = pending(thread);
    var state = !owed ? 'answered' : owed.delivered ? 'delivered' : 'waiting';
    if (note.getAttribute('data-state') === state) return;
    note.setAttribute('data-state', state);
    var mark = foot(note);
    mark.textContent = '';
    if (owed) {
      var receipt = document.createElement('span');
      receipt.className = 'receipt ' + (owed.delivered ? 'delivered' : 'waiting');
      var dot = document.createElement('span');
      dot.className = 'dot';
      receipt.appendChild(dot);
      receipt.appendChild(document.createTextNode(
        owed.delivered ? 'picked up by an agent' : 'waiting for an agent'));
      mark.appendChild(receipt);
    }
    var reply = document.createElement('button');
    reply.type = 'button';
    reply.className = 'threadreply';
    reply.textContent = 'Reply';
    reply.title = 'Say something else about this line';
    mark.appendChild(reply);
  }

  function close() {
    if (box) { box.remove(); box = null; }
  }

  // Anchored under the note, in document coordinates, and outside the table for
  // the same reason the comment editor is: typing must not relayout the diff.
  function openReply(note) {
    var already = box && box.forNote === note;
    close();
    if (already) return;
    var thread = note.getAttribute('data-thread');
    var rect = note.getBoundingClientRect();
    box = document.createElement('div');
    box.className = 'notebox';
    box.forNote = note;
    box.innerHTML =
      '<textarea aria-label="Reply in this discussion"></textarea>' +
      '<div class="row"><span class="msg"></span>' +
      '<button type="button" class="cancel">Cancel</button>' +
      '<button type="button" class="primary">Send</button>' +
      '</div>';
    box.style.left = (rect.left + window.scrollX) + 'px';
    box.style.top = (rect.bottom + window.scrollY + 4) + 'px';
    box.style.width = Math.max(280, Math.min(620, rect.width)) + 'px';
    document.body.appendChild(box);
    var text = box.querySelector('textarea');
    var send = box.querySelector('button.primary');
    var msg = box.querySelector('.msg');
    text.placeholder = 'Reply\u2026';
    text.focus();
    var below = box.getBoundingClientRect().bottom - (window.innerHeight - 12);
    if (below > 0) window.scrollBy(0, below);

    box.querySelector('button.cancel').addEventListener('click', close);
    text.addEventListener('keydown', function (e) {
      if (e.key === 'Escape') { close(); return; }
      if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') send.click();
    });
    send.addEventListener('click', function () {
      var body = text.value.trim();
      if (!body) { text.focus(); return; }
      send.disabled = true;
      msg.textContent = '';
      fetch('/s/' + session + '/comments/' + encodeURIComponent(thread) + '/messages', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ text: body })
      }).then(function (res) {
        if (!res.ok) throw new Error('could not send that');
        return res.json();
      }).then(function (message) {
        close();
        note.insertBefore(said(message), foot(note));
        // Said by the reader, so the thread is waiting on an agent again.
        paint(note, { replies: [message] });
        refresh();
      }).catch(function (err) {
        msg.textContent = String(err.message || err);
        send.disabled = false;
      });
    });
  }

  root.addEventListener('click', function (e) {
    var button = e.target.closest ? e.target.closest('.threadreply') : null;
    if (!button) return;
    var note = button.closest('.note[data-thread]');
    if (note) openReply(note);
  });

  function refresh() {
    var live = notes();
    // Nothing to hear about yet: a page with no comments on it is not waiting for
    // anything, and the request would be an empty answer either way.
    if (!live.length) return Promise.resolve();
    return fetch('/s/' + session + '/threads')
      .then(function (res) { return res.ok ? res.json() : null; })
      .then(function (data) {
        if (!data || !data.threads) return;
        var byId = {};
        for (var i = 0; i < data.threads.length; i++) byId[String(data.threads[i].id)] = data.threads[i];
        var current = notes();
        for (var j = 0; j < current.length; j++) {
          var thread = byId[current[j].getAttribute('data-thread')];
          if (!thread) continue;
          catchUp(current[j], thread);
          paint(current[j], thread);
        }
      })
      .catch(function () { /* a daemon restart is not worth a broken page */ });
  }

  function start() { if (!timer) timer = setInterval(refresh, EVERY); }
  function stop() { if (timer) { clearInterval(timer); timer = null; } }

  document.addEventListener('visibilitychange', function () {
    if (document.visibilityState === 'visible') { refresh(); start(); } else { stop(); }
  });

  // A comment just made is a thread just started.
  root.addEventListener('wdyt:commented', function () { refresh(); });

  // Paint what the server already rendered, then keep it current.
  refresh();
  start();
})();
"##;

/// Deep-link fragment handling.
///
/// On page load and on hashchange, parses the URL fragment and either:
/// - Scrolls to a comment (`#comment-<id>`) and pulses it.
/// - Scrolls to and highlights a line or range (`#sel-<b64path>-<side>-<line>[-<end>]`).
/// - Falls through to the browser's native anchor scroll for file sections and
///   markdown headings.
///
/// Also updates the URL fragment when a line number link is clicked (code mode)
/// or when a comment is saved, so the deep-link is always copy-paste-able.
const FRAGMENT_JS: &str = r##"
(function () {
  // Respect reduced motion preference for scrolling.
  var scrollBehavior = window.matchMedia('(prefers-reduced-motion: reduce)').matches
    ? 'auto' : 'smooth';

  // Base64url decode (no padding).
  function b64decode(s) {
    // Restore padding for atob.
    var pad = (4 - s.length % 4) % 4;
    var b64 = s.replace(/-/g, '+').replace(/_/g, '/') + '===='.slice(0, pad);
    try { return decodeURIComponent(escape(atob(b64))); }
    catch (e) { return null; }
  }

  // Base64url encode (no padding).
  function b64encode(s) {
    return btoa(unescape(encodeURIComponent(s)))
      .replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
  }

  // Build a sel fragment for a line or range.
  function selFragment(path, side, start, end) {
    var enc = b64encode(path);
    if (end && end !== start) return 'sel-' + enc + '-' + side + '-' + start + '-' + end;
    return 'sel-' + enc + '-' + side + '-' + start;
  }

  // Safe positive integer check: must be finite, positive, and within safe range.
  function safeInt(n) {
    return Number.isFinite(n) && n > 0 && n <= Number.MAX_SAFE_INTEGER && n === Math.floor(n);
  }

  // Mirror of the Rust `file_anchor`: 'f-' then every ASCII-alphanumeric char
  // lowercased, everything else a dash. Because the path is lowercased, it can
  // never contain a capital 'L', which is what lets a trailing '-L<n>' be told
  // apart from the dashes inside a path.
  function fileAnchor(label) {
    var out = 'f-';
    for (var i = 0; i < label.length; i++) {
      var ch = label[i];
      out += /[A-Za-z0-9]/.test(ch) ? ch.toLowerCase() : '-';
    }
    return out;
  }

  // Parse a hand-written GitHub-style file-line anchor:
  //   f-<path>-L<start>            single line
  //   f-<path>-L<start>-L<end>     range
  // Returns {anchor, start, end} or null. Parsed from the RIGHT on the capital
  // 'L' markers so dashes in <path> are never mistaken for a delimiter, the same
  // disambiguation parseSel uses for sel- fragments.
  function parseFileLine(frag) {
    if (frag.length > 8192 || frag.indexOf('f-') !== 0) return null;
    // Greedy `.*` peels the LAST -L<digits>: the end line, or the single line.
    var tail = frag.match(/^(.*)-L(\d+)$/);
    if (!tail) return null;
    var end = parseInt(tail[2], 10);
    if (!safeInt(end)) return null;
    var head = tail[1];
    var start = end;
    var prefix = head;
    // A second -L<digits> immediately before it makes this a range.
    var lead = head.match(/^(.*)-L(\d+)$/);
    if (lead) {
      start = parseInt(lead[2], 10);
      if (!safeInt(start)) return null;
      prefix = lead[1];
    }
    if (prefix.indexOf('f-') !== 0) return null;
    return { anchor: prefix, start: Math.min(start, end), end: Math.max(start, end) };
  }

  // Parse a sel fragment. Returns {path, side, start, end} or null.
  function parseSel(frag) {
    if (frag.indexOf('sel-') !== 0) return null;
    // Reject oversized fragments to prevent unbounded work.
    if (frag.length > 8192) return null;
    var rest = frag.slice(4);
    // Split from right: last 2 or 3 segments are side + line(s).
    var parts = rest.split('-');
    if (parts.length < 3) return null;
    // Try range first (last is end, second-last is start, third-last is side).
    var last = parts[parts.length - 1];
    var secondLast = parts[parts.length - 2];
    var thirdLast = parts[parts.length - 3];
    // Check if last two are numbers and third-last is side.
    if (/^\d+$/.test(last) && /^\d+$/.test(secondLast) &&
        (thirdLast === 'old' || thirdLast === 'new')) {
      var encodedPath = parts.slice(0, parts.length - 3).join('-');
      if (!encodedPath || encodedPath.length > 4096) return null;
      var path = b64decode(encodedPath);
      if (!path) return null;
      var start = parseInt(secondLast, 10);
      var end = parseInt(last, 10);
      if (!safeInt(start) || !safeInt(end)) return null;
      return { path: path, side: thirdLast, start: Math.min(start, end), end: Math.max(start, end) };
    }
    // Single line: last is line, second-last is side.
    if (/^\d+$/.test(last) && (secondLast === 'old' || secondLast === 'new')) {
      var encodedPath2 = parts.slice(0, parts.length - 2).join('-');
      if (!encodedPath2 || encodedPath2.length > 4096) return null;
      var path2 = b64decode(encodedPath2);
      if (!path2) return null;
      var line = parseInt(last, 10);
      if (!safeInt(line)) return null;
      return { path: path2, side: secondLast, start: line, end: line };
    }
    return null;
  }

  function clearHighlights() {
    // Only clear highlights within code/diff areas (under [data-comments]),
    // never doc/brief highlights which are owned by the doc comment script.
    var root = document.querySelector('[data-comments]');
    if (!root) return;
    var els = root.querySelectorAll('.sel-highlight');
    for (var i = 0; i < els.length; i++) els[i].classList.remove('sel-highlight');
    // Also clear any comment highlight (comment-* anchors live in code/diff).
    var commentEls = document.querySelectorAll('.wdyt-comment.sel-highlight');
    for (var j = 0; j < commentEls.length; j++) commentEls[j].classList.remove('sel-highlight');
  }

  function highlightComment(id) {
    var el = document.getElementById('comment-' + id);
    if (!el) return false;
    clearHighlights();
    el.classList.add('sel-highlight');
    el.scrollIntoView({ block: 'center', behavior: scrollBehavior });
    return true;
  }

  function highlightSelection(sel) {
    // Find matching rows by iterating actual rendered addnote buttons rather
    // than a numeric range, so a hostile huge range cannot cause unbounded work.
    var root = document.querySelector('[data-comments]');
    if (!root) return false;
    var buttons = root.querySelectorAll('.addnote');
    var found = false;
    for (var i = 0; i < buttons.length; i++) {
      var btn = buttons[i];
      if (btn.getAttribute('data-file') !== sel.path) continue;
      if (btn.getAttribute('data-side') !== sel.side) continue;
      var n = parseInt(btn.getAttribute('data-line'), 10);
      if (n >= sel.start && n <= sel.end) {
        var row = btn.closest('tr');
        if (row) {
          if (!found) clearHighlights();
          row.classList.add('sel-highlight');
          if (!found) {
            row.scrollIntoView({ block: 'center', behavior: scrollBehavior });
            found = true;
          }
        }
      }
    }
    return found;
  }

  // Highlight a hand-written file-line anchor (`f-<path>-L<start>[-L<end>]`).
  // The parsed anchor is lossy, so it is matched by recomputing fileAnchor for
  // each rendered line's own data-file. Only the `new` side has real line
  // numbers a reader would hand-write against, matching GitHub's blob scheme.
  function highlightFileLine(fl) {
    var root = document.querySelector('[data-comments]');
    if (!root) return false;
    var buttons = root.querySelectorAll('.addnote');
    var found = false;
    for (var i = 0; i < buttons.length; i++) {
      var btn = buttons[i];
      if (btn.getAttribute('data-side') !== 'new') continue;
      if (fileAnchor(btn.getAttribute('data-file')) !== fl.anchor) continue;
      var n = parseInt(btn.getAttribute('data-line'), 10);
      if (n >= fl.start && n <= fl.end) {
        var row = btn.closest('tr');
        if (row) {
          if (!found) clearHighlights();
          row.classList.add('sel-highlight');
          if (!found) {
            // A hand-written deep link jumps instantly: smooth scrolling a long
            // diff to a line the reader named is slow, not a courtesy. (Reduced
            // motion already forces 'auto'; this makes the fast path universal.)
            row.scrollIntoView({ block: 'center', behavior: 'auto' });
            found = true;
          }
        }
      }
    }
    return found;
  }

  function handleFragment() {
    var hash = location.hash.slice(1);
    if (!hash) return;
    // Comment anchor.
    if (hash.indexOf('comment-') === 0) {
      var id = hash.slice(8);
      if (/^\d+$/.test(id)) {
        highlightComment(id);
        return;
      }
    }
    // Selection anchor.
    var sel = parseSel(hash);
    if (sel) {
      highlightSelection(sel);
      return;
    }
    // Hand-written GitHub-style file-line anchor (f-<path>-L<start>[-L<end>]).
    var fl = parseFileLine(hash);
    if (fl && highlightFileLine(fl)) {
      return;
    }
    // Otherwise fall through to native anchor handling (f-* and headings).
    // Do NOT clear code/diff highlights here: falling through means this
    // script does not own the fragment, so it should not disturb existing state.
  }

  // On load.
  if (location.hash) {
    // Defer slightly so the browser's own scroll has settled.
    setTimeout(handleFragment, 50);
  }
  // On hashchange (clicking a line-number link, back/forward, manual edit).
  window.addEventListener('hashchange', handleFragment);

  // Intercept clicks on line-number links in code mode to set a sel fragment
  // instead of the simple #L<n> that only works for single-file sessions.
  var root = document.querySelector('[data-comments]');
  if (root) {
    root.addEventListener('click', function (e) {
      var link = e.target.closest ? e.target.closest('td.ln a[href^="#"]') : null;
      if (!link) return;
      // Find the associated addnote button on this row to get file/side/line.
      var row = link.closest('tr');
      if (!row) return;
      var btn = row.querySelector('.addnote');
      if (!btn) return;
      e.preventDefault();
      var path = btn.getAttribute('data-file');
      var side = btn.getAttribute('data-side');
      var line = btn.getAttribute('data-line');
      var frag = selFragment(path, side, parseInt(line, 10));
      history.replaceState(null, '', '#' + frag);
      clearHighlights();
      row.classList.add('sel-highlight');
    });
  }
})();
"##;

/// Comments on rendered markdown (docs and briefs).
///
/// Complementary to `COMMENT_JS`, which handles code/diff tables. This script
/// finds elements with `data-sourcepos` attributes (emitted by comrak when
/// `render.sourcepos = true`) inside containers that also have `data-doc-file`,
/// and attaches a `+` affordance to each top-level block so the user can comment.
///
/// The file identifier for the comment comes from the element's own
/// `data-doc-file` attribute — `"brief:"` for the guided-review brief, or the
/// doc file's label for docs mode. This means a brief and a doc with the same
/// label text cannot collide: the API sees different `file` values.
///
/// Comments are stored with `side: "new"` (the only meaningful value for
/// markdown) and `line`/`end_line` set to the exact sourcepos start/end from
/// `data-sourcepos`.
///
/// Comment data is read from hidden `<div>` elements with class
/// `wdyt-comment-data`, rendered server-side with proper HTML escaping. Each
/// element carries `data-comment-*` attributes for structured fields and has
/// the comment text as its `textContent`. No `<script type="application/json">`
/// or `Raw` JSON hydration is used.
pub(crate) const DOC_COMMENT_JS: &str = r##"
(function () {
  var docRoots = document.querySelectorAll('[data-doc-file]');
  if (!docRoots.length) return;

  // Load comments from hidden DOM elements (server-rendered, escaped).
  function loadComments(containerId) {
    var container = document.getElementById(containerId);
    if (!container) return [];
    var items = container.querySelectorAll('.wdyt-comment-data');
    var result = [];
    for (var i = 0; i < items.length; i++) {
      var el = items[i];
      var endLine = el.getAttribute('data-comment-end-line');
      result.push({
        id: parseInt(el.getAttribute('data-comment-id'), 10) || null,
        target: el.getAttribute('data-comment-target') || '',
        file: el.getAttribute('data-comment-file') || '',
        line: parseInt(el.getAttribute('data-comment-line'), 10) || 0,
        end_line: endLine ? parseInt(endLine, 10) : null,
        side: el.getAttribute('data-comment-side') || 'new',
        snippet: el.getAttribute('data-comment-snippet') || '',
        text: el.textContent || ''
      });
    }
    return result;
  }

  // Find the session id from the element's own data-session, or a sibling with
  // data-session. This allows the brief (which does NOT carry data-comments) to
  // find the session without stealing the COMMENT_JS root.
  function findSession(root) {
    if (root.getAttribute && root.getAttribute('data-session')) {
      return root.getAttribute('data-session');
    }
    var wrap = document.querySelector('[data-session]');
    return wrap ? wrap.getAttribute('data-session') : null;
  }

  // Parse data-sourcepos="startLine:startCol-endLine:endCol"
  function parseSourcepos(sp) {
    if (!sp) return null;
    var m = sp.match(/^(\d+):(\d+)-(\d+):(\d+)$/);
    if (!m) return null;
    return { startLine: parseInt(m[1], 10), endLine: parseInt(m[3], 10) };
  }

  function safeLineNo(n) {
    return Number.isFinite(n) && n > 0 && n <= Number.MAX_SAFE_INTEGER && n === Math.floor(n) ? n : null;
  }

  // Base64url encode (no padding).
  function b64encode(s) {
    return btoa(unescape(encodeURIComponent(s)))
      .replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
  }

  // Use the explicit target (data-doc-target) for fragment encoding, not
  // the display label. This ensures duplicate labels deep-link uniquely.
  function selFragment(docTarget, side, start, end) {
    var enc = b64encode(docTarget);
    if (end && end !== start) return 'sel-' + enc + '-' + side + '-' + start + '-' + end;
    return 'sel-' + enc + '-' + side + '-' + start;
  }

  for (var ri = 0; ri < docRoots.length; ri++) {
    (function (root) {
      var docFile = root.getAttribute('data-doc-file');
      var docTarget = root.getAttribute('data-doc-target') || docFile;
      var session = findSession(root);
      if (!session) return;

      // Only operate on DIRECT children with data-sourcepos (top-level blocks).
      // This prevents inserting affordances into nested table cells, list items,
      // etc. which would break semantics.
      var blocks = [];
      var children = root.children;
      for (var ci = 0; ci < children.length; ci++) {
        if (children[ci].getAttribute && children[ci].getAttribute('data-sourcepos')) {
          blocks.push(children[ci]);
        }
      }
      if (!blocks.length) return;

      // Existing comments for this doc root, loaded from hidden DOM elements.
      var existing = [];
      if (docTarget === 'brief') {
        existing = loadComments('wdyt-brief-comments');
      } else {
        // Filter by target if available, otherwise by file.
        var all = loadComments('wdyt-doc-comments');
        existing = all.filter(function (c) {
          if (c.target) return c.target === docTarget;
          return c.file === docFile;
        });
      }

      var openBox = null;

      function closeBox() {
        if (openBox) { openBox.remove(); openBox = null; }
      }

      function noteContainer(block) {
        // Comments live in a div immediately after the block element.
        var next = block.nextElementSibling;
        if (next && next.classList.contains('doc-notes')) return next;
        var div = document.createElement('div');
        div.className = 'doc-notes';
        block.parentNode.insertBefore(div, block.nextSibling);
        return div;
      }

      function renderNote(comment, container) {
        var note = document.createElement('div');
        note.className = 'note';
        if (comment.id != null) note.id = 'comment-' + comment.id;
        // The head of a discussion, so THREAD_JS can hang the exchange under a
        // markdown comment as it does under one on a line.
        if (comment.id != null) note.setAttribute('data-thread', comment.id);
        var who = document.createElement('span');
        who.className = 'who';
        who.textContent = 'you';
        if (comment.end_line && comment.end_line > comment.line) {
          var span = document.createElement('span');
          span.className = 'span';
          span.textContent = 'L' + comment.line + '\u2013L' + comment.end_line;
          who.appendChild(span);
        }
        note.appendChild(who);
        if (comment.id != null) {
          var plink = document.createElement('a');
          plink.className = 'permalink';
          plink.href = '#comment-' + comment.id;
          plink.title = 'Link to this comment';
          plink.textContent = '#';
          note.appendChild(plink);
        }
        note.appendChild(document.createTextNode(comment.text));
        container.appendChild(note);
      }

      // Build a map of exact source range -> block for deterministic hydration.
      var rangeToBlock = {};
      for (var i = 0; i < blocks.length; i++) {
        var sp = parseSourcepos(blocks[i].getAttribute('data-sourcepos'));
        if (sp && safeLineNo(sp.startLine) && safeLineNo(sp.endLine)) {
          rangeToBlock[sp.startLine + ':' + sp.endLine] = blocks[i];
        }
      }

      // Hydrate only an exact target+range match. A stale or legacy comment is
      // never relocated onto a different markdown block.
      for (var eci = 0; eci < existing.length; eci++) {
        var c = existing[eci];
        var commentEnd = c.end_line || c.line;
        var targetBlock = rangeToBlock[c.line + ':' + commentEnd] || null;
        if (targetBlock) {
          renderNote(c, noteContainer(targetBlock));
        }
      }

      for (var i = 0; i < blocks.length; i++) {
        (function (block) {
          var sp = parseSourcepos(block.getAttribute('data-sourcepos'));
          if (!sp) return;
          var startLine = safeLineNo(sp.startLine);
          var endLine = safeLineNo(sp.endLine);
          if (!startLine) return;

          // Make the block a positioned ancestor for the button.
          block.style.position = 'relative';

          var btn = document.createElement('button');
          btn.type = 'button';
          btn.className = 'doc-addnote';
          btn.title = 'Comment on this block';
          btn.textContent = '+';
          btn.setAttribute('data-start', startLine);
          btn.setAttribute('data-end', endLine || startLine);
          btn.setAttribute('data-doc-file', docFile);
          btn.setAttribute('data-doc-target', docTarget);
          block.insertBefore(btn, block.firstChild);

          btn.addEventListener('click', function (e) {
            e.stopPropagation();
            var lo = startLine;
            var hi = endLine || startLine;

            // Toggle: clicking the same block closes the box.
            var reopen = !openBox || openBox._block !== block;
            closeBox();
            if (!reopen) return;

            // Update URL fragment using explicit target for unique deep-links.
            var frag = selFragment(docTarget, 'new', lo, hi);
            history.replaceState(null, '', '#' + frag);

            var box = document.createElement('div');
            box.className = 'doc-notebox';
            box._block = block;
            var label = hi > lo ? 'Comment on lines ' + lo + ' to ' + hi : 'Comment on this block';
            var isMac = navigator.platform && navigator.platform.indexOf('Mac') > -1;
            var modKey = isMac ? 'Cmd' : 'Ctrl';
            box.innerHTML =
              '<textarea aria-label="' + label + '" placeholder="' + label + '\u2026"></textarea>' +
              '<div class="row"><span class="msg"></span>' +
              '<kbd class="kb-hint" aria-label="Keyboard shortcuts: ' + modKey + '+Enter to send, Escape to cancel">' +
              '<abbr title="' + modKey + '+Enter">' + (isMac ? '\u2318+Enter' : 'Ctrl+Enter') + '</abbr> send \u00B7 <abbr title="Escape">Esc</abbr> cancel</kbd>' +
              '<button type="button" class="cancel">Cancel</button>' +
              '<button type="button" class="primary">Comment</button>' +
              '</div>';
            block.parentNode.insertBefore(box, block.nextSibling);
            openBox = box;

            var textarea = box.querySelector('textarea');
            var submitBtn = box.querySelector('.primary');
            var msg = box.querySelector('.msg');
            textarea.focus();

            box.querySelector('.cancel').addEventListener('click', closeBox);
            textarea.addEventListener('keydown', function (ev) {
              if (ev.key === 'Escape') { closeBox(); return; }
              if ((ev.metaKey || ev.ctrlKey) && ev.key === 'Enter') submitBtn.click();
            });

            submitBtn.addEventListener('click', function () {
              var text = textarea.value.trim();
              if (!text) { textarea.focus(); return; }
              submitBtn.disabled = true;
              msg.textContent = '';
              fetch('/s/' + session + '/comments', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({
                  file: docFile,
                  target: docTarget,
                  line: lo,
                  end_line: hi,
                  side: 'new',
                  text: text
                })
              }).then(function (res) {
                if (!res.ok) throw new Error('could not save the comment');
                return res.json();
              }).then(function (comment) {
                closeBox();
                renderNote(comment, noteContainer(block));
                // Same announcement the line-comment script makes: a new thread
                // for THREAD_JS to hang the discussion under.
                var hook = document.querySelector('[data-comments]');
                if (hook) hook.dispatchEvent(new CustomEvent('wdyt:commented'));
                if (comment.id != null) {
                  history.replaceState(null, '', '#comment-' + comment.id);
                }
              }).catch(function (err) {
                msg.textContent = String(err.message || err);
                submitBtn.disabled = false;
              });
            });
          });
        })(blocks[i]);
      }
    })(docRoots[ri]);
  }

  // Mirror of the Rust `file_anchor` (see FRAGMENT_JS for the rationale).
  function fileAnchor(label) {
    var out = 'f-';
    for (var i = 0; i < label.length; i++) {
      var ch = label[i];
      out += /[A-Za-z0-9]/.test(ch) ? ch.toLowerCase() : '-';
    }
    return out;
  }

  // Parse a hand-written GitHub-style file-line anchor, from the right on the
  // capital-L markers so dashes in the path are never a delimiter.
  function parseFileLine(frag) {
    if (frag.length > 8192 || frag.indexOf('f-') !== 0) return null;
    var tail = frag.match(/^(.*)-L(\d+)$/);
    if (!tail) return null;
    var end = parseInt(tail[2], 10);
    if (!safeLineNo(end)) return null;
    var start = end;
    var prefix = tail[1];
    var lead = tail[1].match(/^(.*)-L(\d+)$/);
    if (lead) {
      start = parseInt(lead[2], 10);
      if (!safeLineNo(start)) return null;
      prefix = lead[1];
    }
    if (prefix.indexOf('f-') !== 0) return null;
    return { anchor: prefix, start: Math.min(start, end), end: Math.max(start, end) };
  }

  // Resolve a file-line anchor within docs: find the doc whose label matches and
  // scroll to the first rendered block whose source range overlaps the span.
  // Markdown has no per-line addressing, so the covering block is the closest
  // faithful target; a span matching no block simply falls through.
  function handleDocFileLine(fl) {
    for (var ri = 0; ri < docRoots.length; ri++) {
      var root = docRoots[ri];
      var docFile = root.getAttribute('data-doc-file');
      // The brief has no source path a reader would link to by file anchor.
      if (!docFile || docFile === 'brief:') continue;
      if (fileAnchor(docFile) !== fl.anchor) continue;
      var children = root.children;
      for (var ci = 0; ci < children.length; ci++) {
        var child = children[ci];
        var sp = parseSourcepos(child.getAttribute ? child.getAttribute('data-sourcepos') : null);
        if (!sp) continue;
        // Overlap between the block's source span and the requested one.
        if (sp.startLine <= fl.end && sp.endLine >= fl.start) {
          // Instant jump for a hand-written deep link (see FRAGMENT_JS).
          child.scrollIntoView({ block: 'center', behavior: 'auto' });
          child.classList.add('sel-highlight');
          setTimeout(function (el) { return function () { el.classList.remove('sel-highlight'); }; }(child), 2000);
          return true;
        }
      }
    }
    return false;
  }

  // Handle doc/brief #sel links: resolve, scroll, and highlight on load/hashchange.
  // Uses data-doc-target (explicit target) for resolution, not display label.
  function handleDocFragment() {
    var hash = location.hash.slice(1);
    if (!hash) return;
    // Hand-written file-line anchor (f-<path>-L<start>[-L<end>]) → covering block.
    if (hash.indexOf('sel-') !== 0) {
      var fl = parseFileLine(hash);
      if (fl) handleDocFileLine(fl);
      return;
    }
    // Try to find a matching doc-addnote button in a doc root.
    for (var ri2 = 0; ri2 < docRoots.length; ri2++) {
      var root = docRoots[ri2];
      var docTarget = root.getAttribute('data-doc-target') || root.getAttribute('data-doc-file');
      var children = root.children;
      for (var ci2 = 0; ci2 < children.length; ci2++) {
        var child = children[ci2];
        var sp = parseSourcepos(child.getAttribute ? child.getAttribute('data-sourcepos') : null);
        if (!sp) continue;
        var btn = child.querySelector('.doc-addnote');
        if (!btn) continue;
        var start = parseInt(btn.getAttribute('data-start'), 10);
        var end = parseInt(btn.getAttribute('data-end'), 10);
        // Build what our fragment would be for this block and compare using target.
        var expected = selFragment(docTarget, 'new', start, end);
        if (expected === hash) {
          // Respect reduced motion preference.
          var motion = window.matchMedia('(prefers-reduced-motion: reduce)');
          var behavior = motion.matches ? 'auto' : 'smooth';
          child.scrollIntoView({ block: 'center', behavior: behavior });
          child.classList.add('sel-highlight');
          setTimeout(function () { child.classList.remove('sel-highlight'); }, 2000);
          return;
        }
      }
    }
  }

  if (location.hash) setTimeout(handleDocFragment, 80);
  window.addEventListener('hashchange', handleDocFragment);
})();
"##;

/// A file's hunks paired with the hidden runs around them.
///
/// Worked out before the view rather than inside it: the view needs each file's
/// index (to address it in a context request) and each hunk's neighbours, and
/// `view!`'s `for` gives neither.
pub(crate) struct FileBlock<'a> {
    pub file: &'a crate::session::DiffFile,
    pub index: usize,
    pub hunks: Vec<HunkBlock<'a>>,
}

pub(crate) struct HunkBlock<'a> {
    pub hunk: &'a Hunk,
    /// The lines hidden between this hunk and the one before it.
    pub before: Option<Gap>,
    /// The lines hidden after this hunk, only ever set on a file's last one.
    pub after: Option<Gap>,
}

pub(crate) fn diff_blocks(files: &[crate::session::DiffFile]) -> Vec<FileBlock<'_>> {
    files
        .iter()
        .enumerate()
        .map(|(index, file)| {
            let gaps = crate::diff::gaps(file);
            let last = file.hunks.len().saturating_sub(1);
            FileBlock {
                file,
                index,
                hunks: file
                    .hunks
                    .iter()
                    .enumerate()
                    .map(|(at, hunk)| HunkBlock {
                        hunk,
                        before: gaps.before.get(at).copied().flatten(),
                        after: (at == last).then_some(gaps.after).flatten(),
                    })
                    .collect(),
            }
        })
        .collect()
}

/// One comment and the discussion under it.
///
/// The comment is the first thing said. The agent's answers, and any follow-up
/// from the reader, hang below it in the flow of the code, because a discussion
/// about a line is only legible next to the line it is about.
///
/// The foot of the thread — the receipt and the reply control — is painted by
/// `THREAD_JS` rather than rendered here: it changes while the page is open, and
/// having one implementation of those states beats having a server-rendered one
/// that the script must then agree with.
#[component]
pub async fn thread(comment: &Comment) -> Result {
    struct Said {
        id: u64,
        class: &'static str,
        who: &'static str,
        text: String,
    }
    let said: Vec<Said> = comment
        .replies
        .iter()
        .map(|message| Said {
            id: message.id,
            class: match message.from {
                Author::Agent => "said agent",
                Author::User => "said user",
            },
            who: match message.from {
                Author::Agent => "agent",
                Author::User => "you",
            },
            text: message.text.clone(),
        })
        .collect();
    view! {
        <div
            class="note"
            id=(format!("comment-{}", comment.id))
            data-thread=(comment.id)
        >
            <span class="who">
                "you"
                // Says what the note is about when it covers more than the line
                // above it.
                if let Some(span) = span_label(comment) {
                    <span class="span">(span)</span>
                }
            </span>
            <a
                class="permalink"
                href=(format!("#{}", crate::fragment::comment_fragment(comment.id)))
                title="Link to this comment"
            >"#"</a>
            (&comment.text)
            for message in said {
                <div class=(message.class) data-message=(message.id)>
                    <span class="who">(message.who)</span>
                    (message.text)
                </div>
            }
            <div class="threadfoot"></div>
        </div>
    }
}

/// One hunk of a diff, with a comment affordance on every line.
///
/// The line number cells are buttons rather than links: clicking one opens an
/// inline comment box anchored to that line. A line already commented on shows
/// the comment underneath, in the flow of the diff, which is where it is
/// actually useful when re-reading.
///
/// Where the patch skips lines, the `@@` header row is replaced by a band that
/// reveals them (`EXPAND_JS`). The header's own text is not worth the row once
/// there is something better to put there: it says which lines the hunk covers,
/// which the gutter already shows, and the reason to look at that row is to get
/// the code the patch left out. Its trailing section heading — the function the
/// hunk is in — is kept, since nothing else says that.
#[component]
pub async fn diff_hunk(
    file: &str,
    index: usize,
    hunk: &Hunk,
    comments: &[Comment],
    gap_before: Option<Gap>,
    gap_after: Option<Gap>,
) -> Result {
    // The section heading a `@@` line can carry — `@@ -1,5 +1,7 @@ fn foo()` — is
    // the one part of the header the gutter does not already say, so it survives
    // when an expander takes the row.
    let section = hunk
        .header
        .rsplit_once("@@")
        .map(|(_, tail)| tail.trim().to_owned())
        .filter(|tail| !tail.is_empty());
    // The anchor and its comments are worked out here rather than inside the
    // view: `view!` takes expressions, not `let` bindings with types.
    let rows: Vec<Row> = hunk
        .lines
        .iter()
        .map(|line| {
            // An added or context line is addressed by its new-side number, a
            // removed line by its old one.
            let (side, number) = match line.kind {
                LineKind::Removed => ("old", line.old),
                _ => ("new", line.new),
            };
            let line_no = number.unwrap_or(0);
            let mine =
                |c: &&Comment| c.target.is_none() && c.file == file && c.side.as_str() == side;
            Row {
                line: line.clone(),
                side,
                line_no,
                // A ranged comment hangs below the last line it covers, so the
                // whole quoted passage is above the note rather than wrapped
                // around it.
                comments: comments
                    .iter()
                    .filter(mine)
                    .filter(|c| c.end_line.unwrap_or(c.line) == line_no)
                    .cloned()
                    .collect(),
                // Whether some comment's range covers this line, so the rows it
                // spans can be tinted as a block.
                in_range: comments.iter().filter(mine).any(|c| {
                    c.end_line
                        .is_some_and(|end| (c.line..=end).contains(&line_no))
                }),
            }
        })
        .collect();

    view! {
        <table class="src diff commentable">
            <tbody>
                if let Some(gap) = gap_before {
                    <tr
                        class="gap"
                        data-file=(file)
                        data-index=(index)
                        data-from=(gap.from)
                        data-to=(gap.to)
                        data-place="before"
                    >
                        <td class="ln" colspan="2"></td>
                        // Painted by EXPAND_JS: the controls and the count have one
                        // implementation, and a page with no script gets no dead buttons.
                        <td class="code">
                            if let Some(section) = &section {
                                <span class="section">(section)</span>
                            }
                        </td>
                    </tr>
                }
                if gap_before.is_none() {
                    <tr class="hunk">
                        <td class="ln" colspan="2"></td>
                        <td class="code">(&hunk.header)</td>
                    </tr>
                }
                for row in rows {
                    <tr class=(row_class(&row))>
                        <td class="ln old">
                            if let Some(n) = row.line.old {
                                if row.side == "old" {
                                    <a href=(format!("#{}", crate::fragment::line_fragment(file, "old", n)))>(n)</a>
                                } else {
                                    (n)
                                }
                            }
                        </td>
                        <td class="ln new">
                            if let Some(n) = row.line.new {
                                if row.side == "new" {
                                    <a href=(format!("#{}", crate::fragment::line_fragment(file, "new", n)))>(n)</a>
                                } else {
                                    (n)
                                }
                            }
                        </td>
                        <td class="code">
                            // Drag from one `+` to another to comment on a range.
                            <button
                                type="button"
                                class="addnote"
                                title="Comment on this line, or drag to cover more"
                                data-file=(file)
                                data-line=(row.line_no)
                                data-side=(row.side)
                            >"+"</button>
                            <span class="marker">(marker(row.line.kind))</span>
                            (Raw(row.line.html.clone()))
                        </td>
                    </tr>
                    if !row.comments.is_empty() {
                        <tr class="notes">
                            <td class="ln" colspan="2"></td>
                            <td class="code">
                                for comment in row.comments.clone() {
                                    thread(comment: &comment)
                                }
                            </td>
                        </tr>
                    }
                }
                // The run after the last hunk, which is the rest of the file.
                if let Some(gap) = gap_after {
                    <tr
                        class="gap"
                        data-file=(file)
                        data-index=(index)
                        data-from=(gap.from)
                        data-to=(gap.to)
                        data-place="after"
                    >
                        <td class="ln" colspan="2"></td>
                        <td class="code"></td>
                    </tr>
                }
            </tbody>
        </table>
    }
}

/// A syntax-highlighted source file, with a comment affordance on every line.
///
/// The plain-code counterpart to [`diff_hunk`]: it emits the same `.addnote`
/// buttons and `tr.notes` rows so the one shared comment script drives both.
/// Code has no old/new sides, so every line is addressed on the `new` side.
///
/// `highlighted` is the per-line HTML pre-rendered by `render::highlight`; it is
/// never re-highlighted here, keeping this a render-once design. The table spans
/// three columns like the diff — a `colspan="2"` line-number cell plus the code
/// cell — so the shared script's `colspan="2"` note rows line up.
#[component]
pub async fn code_file(file: &str, highlighted: &[String], comments: &[Comment]) -> Result {
    // Line numbers are 1-indexed and every line is addressable on the new side,
    // so the anchor work mirrors `diff_hunk` with the side fixed.
    let rows: Vec<CodeRow> = highlighted
        .iter()
        .enumerate()
        .map(|(index, html)| {
            let line_no = index + 1;
            let mine = |c: &&Comment| c.target.is_none() && c.file == file && c.side == Side::New;
            CodeRow {
                line_no,
                html: html.clone(),
                // A ranged comment hangs below the last line it covers.
                comments: comments
                    .iter()
                    .filter(mine)
                    .filter(|c| c.end_line.unwrap_or(c.line) == line_no)
                    .cloned()
                    .collect(),
                in_range: comments.iter().filter(mine).any(|c| {
                    c.end_line
                        .is_some_and(|end| (c.line..=end).contains(&line_no))
                }),
            }
        })
        .collect();

    view! {
        <table class="src commentable">
            <tbody>
                if rows.is_empty() {
                    <tr>
                        <td class="ln" colspan="2"></td>
                        <td class="code"><em>"empty file"</em></td>
                    </tr>
                }
                for row in rows {
                    <tr id=(format!("{}-L{}", crate::file_anchor(file), row.line_no)) class=(if row.in_range { "inrange" } else { "" })>
                        <td class="ln" colspan="2">
                            <a href=(format!("#{}", crate::fragment::line_fragment(file, "new", row.line_no)))>(row.line_no)</a>
                        </td>
                        <td class="code">
                            // Drag from one `+` to another to comment on a range.
                            <button
                                type="button"
                                class="addnote"
                                title="Comment on this line, or drag to cover more"
                                data-file=(file)
                                data-line=(row.line_no)
                                data-side="new"
                            >"+"</button>
                            (Raw(row.html.clone()))
                        </td>
                    </tr>
                    if !row.comments.is_empty() {
                        <tr class="notes">
                            <td class="ln" colspan="2"></td>
                            <td class="code">
                                for comment in row.comments.clone() {
                                    thread(comment: &comment)
                                }
                            </td>
                        </tr>
                    }
                }
            </tbody>
        </table>
    }
}

/// A source line with its comment anchor resolved.
struct CodeRow {
    line_no: usize,
    /// The line's pre-rendered highlighted HTML.
    html: String,
    /// Comments that end on this line, shown beneath it.
    comments: Vec<Comment>,
    /// Whether a ranged comment covers this line.
    in_range: bool,
}

/// A diff line with its comment anchor resolved.
struct Row {
    line: crate::session::DiffLine,
    side: &'static str,
    line_no: usize,
    /// Comments that end on this line, shown beneath it.
    comments: Vec<Comment>,
    /// Whether a ranged comment covers this line.
    in_range: bool,
}

/// The row's classes: its diff kind, plus whether a ranged comment covers it.
fn row_class(row: &Row) -> String {
    let kind = line_class(row.line.kind);
    if row.in_range {
        format!("{kind} inrange")
    } else {
        kind.to_owned()
    }
}

/// How a ranged comment names what it covers, e.g. `L12–L18`.
///
/// `None` for a single line: the note sits directly under it, so saying so again
/// would be noise.
fn span_label(comment: &Comment) -> Option<String> {
    let end = comment.end_line?;
    (end > comment.line).then(|| format!("L{}–L{end}", comment.line))
}

/// The row class for a diff line.
fn line_class(kind: LineKind) -> &'static str {
    match kind {
        LineKind::Added => "add",
        LineKind::Removed => "del",
        LineKind::Context => "ctx",
    }
}

/// The leading `+`/`-` column. Kept as its own element so selecting the code
/// does not pick up a marker that was never in the file.
fn marker(kind: LineKind) -> &'static str {
    match kind {
        LineKind::Added => "+",
        LineKind::Removed => "-",
        LineKind::Context => " ",
    }
}

/// What each receipt stage says, in the user's terms.
///
/// "Picked up" rather than "read" for delivery: it is the honest claim, and the
/// difference between it and "read" is the whole point — an agent stuck on a
/// credentials prompt sits at "picked up" and never moves.
fn receipt_text(stage: &str) -> &'static str {
    match stage {
        "read" => "The agent read this and replied",
        "delivered" => "Picked up by an agent, not yet acknowledged",
        _ => "Waiting for an agent to pick this up",
    }
}

/// The floating reply widget.
///
/// Rendered as an overlay in every mode. On a live demo the page underneath is
/// the thing being reviewed, so the widget must never take part in its layout:
/// collapsed it is a single pill in the corner, and expanding it paints over
/// the content rather than resizing it.
///
/// An empty `session_id` means there is nothing to reply to (the index), and
/// renders nothing at all.
#[component]
pub async fn reply_widget(session_id: &str, existing: Option<Reply>) -> Result {
    if session_id.is_empty() {
        return view! {};
    }
    let action = format!("/s/{session_id}/reply");
    // Rendered server-side as well as polled, so a page loaded long after the
    // fact opens in the right state rather than flashing "waiting" first.
    //
    // Three states, not two. Delivery and reading are different claims: an agent
    // that collects a reply and then dies has been delivered to and read nothing,
    // and calling that "read" hides the failure the user most needs to see.
    let ack = existing.as_ref().and_then(|reply| reply.ack.clone());
    let delivered = existing
        .as_ref()
        .is_some_and(|reply| reply.delivered_at.is_some());
    let stage = match (&ack, delivered) {
        (Some(_), _) => "read",
        (None, true) => "delivered",
        (None, false) => "waiting",
    };

    view! {
        <div id="reply" class="reply" data-session=(session_id)>
            match existing {
                // A reply already sent: show it, and offer no way to send another.
                // Enabled, so the reply can be re-read on a later visit; the
                // panel it opens offers no way to send another.
                Some(reply) => {
                    <button
                        id="toggle"
                        type="button"
                        class=(if ack.is_some() { "done read" } else { "done" })
                    >"Reply sent"</button>
                    <div class="panel">
                        <div class="head">
                            <span class="grip" title="Drag to move" aria-hidden="true">"⠿"</span>
                            "Reply"
                        </div>
                        <p class="sent" style="margin: 0">
                            <b>"Sent to the agent"</b>
                            <br>
                            (reply.text)
                        </p>
                        <p class=(format!("receipt {stage}"))>
                            <span class="dot"></span>
                            <span class="text">(receipt_text(stage))</span>
                        </p>
                        // The agent's own words about what it is doing. This is
                        // the part that cannot be faked by the transport.
                        match &ack {
                            Some(ack) => {
                                <p class="acknote">(ack.note.clone())</p>
                            }
                            None => {}
                        }
                        <div class="row">
                            <span class="spacer"></span>
                            <button type="button" class="close">"Close"</button>
                        </div>
                    </div>
                }
                None => {
                    <button id="toggle" type="button">"Reply to agent"</button>
                    <div class="panel">
                        <div class="head">
                            <span class="grip" title="Drag to move" aria-hidden="true">"⠿"</span>
                            "Reply to agent"
                        </div>
                        <form action=(action) method="post">
                            <textarea
                                name="text"
                                placeholder="Send a note back to the agent…"
                                aria-label="Reply to the agent"
                            ></textarea>
                            <p class="error" role="alert"></p>
                            <div class="row">
                                <span class="hint">"⌘/Ctrl + Enter to send"</span>
                                <button type="button" class="close">"Close"</button>
                                <button type="submit" class="primary">"Send"</button>
                            </div>
                        </form>
                    </div>
                }
            }
        </div>
        <script>(Raw(REPLY_JS))</script>
    }
}

/// Everything the shell needs to know about the page it is wrapping.
///
/// One value rather than a dozen props: the fields are all answers to "what page
/// is this?", and a call site passing them individually reads as a wall of
/// keywords where a mistake is easy to miss.
#[derive(Debug, Clone)]
pub struct Page {
    pub title: String,
    /// A sentence of extra context, shown beside the title.
    pub note: Option<String>,
    /// The mode label: `code`, `diff`, `docs`, `demo`, `static`.
    pub kind: String,
    /// The session this page belongs to; empty on the index, which has none.
    pub session_id: String,
    /// The reply already sent, if any.
    pub reply: Option<Reply>,
    /// Extra `<body>` classes, e.g. `framed`.
    pub body_class: Option<String>,
    /// Whether the top bar can be collapsed away. Offered where the chrome
    /// competes for space with the content: a framed app, or a long diff.
    pub hideable: bool,
    /// Whether the page has commentable lines, which need the comment script.
    pub commentable: bool,
    /// Where the agent that made this session is running.
    pub origin: crate::zellij_origin::Origin,
}

impl Page {
    /// A page with no session behind it: the index.
    pub fn index() -> Self {
        Self {
            title: "Sessions".to_owned(),
            note: None,
            kind: "index".to_owned(),
            session_id: String::new(),
            reply: None,
            body_class: None,
            hideable: false,
            commentable: false,
            origin: crate::zellij_origin::Origin::default(),
        }
    }
}

/// The document shell shared by every mode.
#[component]
pub async fn shell(
    page: Page,
    colors: &ThemeColors,
    /// Rendered into `<main>`.
    child: topcoat::view::View,
    /// Optional markup after the header, e.g. a file list.
    subheader: Option<topcoat::view::View>,
) -> Result {
    let Page {
        title,
        note,
        kind,
        session_id,
        reply: existing_reply,
        body_class,
        hideable,
        commentable,
        origin,
    } = page;
    view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8">
                <meta name="viewport" content="width=device-width, initial-scale=1">
                <title>(&title) " · wdyt"</title>
                <style>(Raw(stylesheet(colors)))</style>
            </head>
            <body class=(body_class)>
                <header class="bar">
                    <span class="brand">"wdyt"</span>
                    <h1>(title)</h1>
                    if let Some(note) = note {
                        <span class="note">(note)</span>
                    }
                    <span class="spacer"></span>
                    // Which agent is asking. With several running at once the
                    // title alone does not say, and the pane is where you go to
                    // talk to it.
                    if !origin.is_empty() {
                        <span class="origin" title=(origin.pane.clone().unwrap_or_default())>
                            if let Some(session) = origin.session.clone() {
                                <span class="place">(session)</span>
                                if let Some(tab) = origin.tab.clone() {
                                    <span class="sep">"›"</span>
                                    <span class="place">(tab)</span>
                                }
                            }
                            if let Some(cwd) = origin.cwd.clone() {
                                <span class="cwd">(cwd)</span>
                            }
                        </span>
                    }
                    <span class="kind">(kind)</span>
                    if hideable {
                        <button id="chrome-toggle" type="button" title="Hide this bar (.)">
                            "Hide bar"
                        </button>
                    }
                </header>

                if let Some(subheader) = subheader {
                    (subheader)
                }

                <main>(child)</main>

                if hideable {
                    <button id="chrome-show" type="button" title="Show the bar (.)">
                        "wdyt ▾"
                    </button>
                    <script>(Raw(CHROME_JS))</script>
                }

                // Jump-to-top: always rendered, shown by JS when scrolled down.
                <button id="jump-top" type="button" aria-label="Jump to top" title="Back to top">
                    "↑"
                </button>
                <script>(Raw(NAV_JS))</script>

                reply_widget(session_id: &session_id, existing: existing_reply)

                if commentable {
                    <script>(Raw(COMMENT_JS))</script>
                    <script>(Raw(EXPAND_JS))</script>
                    <script>(Raw(THREAD_JS))</script>
                    <script>(Raw(DOC_COMMENT_JS))</script>
                    <script>(Raw(FRAGMENT_JS))</script>
                }
            </body>
        </html>
    }
}

/// Test-visible: returns the full stylesheet string for a default theme.
///
/// Integration tests need to assert on CSS rules without rendering a full page.
pub fn stylesheet_for_test() -> String {
    stylesheet(&crate::render::theme_colors(crate::render::theme("Nord")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::{theme, theme_colors};

    #[test]
    fn the_syntax_theme_reaches_the_code_card_only() {
        // The chrome is a fixed off-white palette; the theme's own colours are
        // confined to `--code-*` so a dark theme cannot darken the page.
        let colors = theme_colors(theme("Nord"));
        let css = stylesheet(&colors);
        assert!(
            css.contains(&format!("--code-bg: {}", colors.background)),
            "{css}"
        );
        assert!(css.contains("--bg: #faf9f5"), "{css}");
        assert!(css.contains("color-scheme: light"), "{css}");
        // The reply widget must be fixed so it cannot reflow demo content.
        assert!(css.contains("position: fixed"));
    }

    #[test]
    fn the_chrome_does_not_change_with_the_theme() {
        // Only the `--code-*` block may differ between a dark and a light theme.
        let dark = stylesheet(&theme_colors(theme("Nord")));
        let light = stylesheet(&theme_colors(theme("InspiredGitHub")));
        let strip = |css: &str| {
            css.lines()
                .filter(|line| !line.trim_start().starts_with("--code-"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert_eq!(strip(&dark), strip(&light));
    }

    #[test]
    fn framed_modes_share_the_full_viewport_rules() {
        // `dir` and `demo` both use `body.framed`; an absolute `.framewrap`
        // without the positioned ancestor would paint over the header.
        let css = stylesheet(&theme_colors(theme("Nord")));
        assert!(css.contains("body.framed main { position: relative; }"));
        assert!(css.contains(".framewrap { position: absolute"));
    }

    #[test]
    fn markdown_alerts_are_styled() {
        // comrak emits `.markdown-alert`; unstyled it reads as a stray heading.
        let css = stylesheet(&theme_colors(theme("Nord")));
        assert!(css.contains(".markdown-alert-title"));
        assert!(css.contains(".markdown-alert-warning"));
    }

    #[test]
    fn the_toggle_outranks_the_generic_button_rule() {
        // `#reply button` is an id plus a type selector, so a bare `#toggle`
        // loses to it and the pill renders as a plain grey button. Regression
        // test for exactly that.
        let css = stylesheet(&theme_colors(theme("Nord")));
        assert!(css.contains("#reply #toggle {"), "{css}");
        // A selector that *starts* a line with `#toggle` is the unscoped form.
        for line in css.lines() {
            assert!(
                !line.starts_with("#toggle"),
                "under-specific toggle rule: {line}"
            );
        }
    }

    #[test]
    fn the_widget_can_be_dragged_off_the_default_corner() {
        // The corner the widget defaults to is often where a framed app keeps
        // its own controls, so it has to be movable.
        let css = stylesheet(&theme_colors(theme("Nord")));
        assert!(
            css.contains("#reply.moved { right: auto; bottom: auto; }"),
            "{css}"
        );
        assert!(REPLY_JS.contains("root.classList.add('moved')"));
        // The grip the CSS styles has to exist in the markup, and the drag has
        // to be shielded from a cross-origin iframe stealing the pointer.
        assert!(css.contains("#reply .grip"), "{css}");
        assert!(REPLY_JS.contains("reply-shield"));
        assert!(css.contains("#reply-shield"), "{css}");
        // Dragging the pill must not also open the panel: the mouseup that ends
        // the drag is followed by a click on the toggle.
        assert!(REPLY_JS.contains("justDragged"));
    }

    #[test]
    fn the_top_bar_can_be_hidden() {
        // Over a framed app the bar is someone else's screen space. Hiding it
        // must leave a way back, or the page needs a reload to escape.
        let css = stylesheet(&theme_colors(theme("Nord")));
        assert!(css.contains("body.nochrome header.bar"), "{css}");
        assert!(
            css.contains("body.nochrome #chrome-show { display: block; }"),
            "{css}"
        );
        assert!(CHROME_JS.contains("chrome-toggle"));
        assert!(CHROME_JS.contains("chrome-show"));
        // Remembered, since a review usually spans several reloads.
        assert!(CHROME_JS.contains("wdyt.chrome.hidden"));
    }

    #[test]
    fn the_read_receipt_has_all_three_states() {
        let css = stylesheet(&theme_colors(theme("Nord")));
        assert!(css.contains("#reply .receipt.waiting .dot"), "{css}");
        // Picked up but unacknowledged is its own state, styled unlike read: it
        // is where an agent that died on a credentials prompt sits.
        assert!(css.contains("#reply .receipt.delivered .dot"), "{css}");
        assert!(css.contains("#reply .receipt.read .dot"), "{css}");
        assert!(
            css.contains("#reply .acknote"),
            "no style for the ack note: {css}"
        );
        // The pulse must be suppressed for anyone who asked for less motion.
        assert!(css.contains("prefers-reduced-motion"), "{css}");
        // Polling hits the status route, which never marks anything; the
        // long-poll route would have the page claiming the agent's own receipt.
        assert!(REPLY_JS.contains("'/status'") || REPLY_JS.contains("/status'"));
        assert!(!REPLY_JS.contains("/reply?"), "the page must not long-poll");
        // An ack is terminal, so the poll stops there — but delivery is not,
        // since the interesting question is whether an ack ever follows.
        assert!(REPLY_JS.contains("if (s.ack) stop = true"));
        assert!(
            !REPLY_JS.contains("if (s.delivered) stop"),
            "the poll gave up at delivery, so a later ack would never show"
        );
    }

    #[test]
    fn the_receipt_never_calls_a_mere_delivery_read() {
        // The bug this flow exists to fix: the wording for a collected-but-
        // unacknowledged reply must not claim anything read it.
        let delivered = receipt_text("delivered");
        assert!(!delivered.contains("read"), "{delivered:?} overclaims");
        assert!(receipt_text("read").contains("read"));
    }

    #[test]
    fn comment_js_posts_to_the_session_it_was_rendered_for() {
        // The script binds to a shared hook rather than the diff container by id,
        // so a code listing carrying the same attribute is driven by it too.
        assert!(COMMENT_JS.contains("querySelector('[data-comments]')"));
        // The session id travels on the element rather than being inlined into
        // the script, which is shared by every page.
        assert!(COMMENT_JS.contains("getAttribute('data-session')"));
        assert!(COMMENT_JS.contains("'/s/' + session + '/comments'"));
        // Two comments on one line join the same block rather than stacking
        // duplicate rows.
        assert!(COMMENT_JS.contains("classList.contains('notes')"));
    }

    #[test]
    fn comment_js_uses_attribute_comparison_not_selector_interpolation() {
        // Path matching must compare attributes directly rather than
        // interpolating raw file paths into CSS selectors, which would be
        // unsafe for paths containing quotes or backslashes.
        assert!(
            !COMMENT_JS.contains(r#"'[data-file="' + file + '"]'"#),
            "unsafe selector interpolation found in COMMENT_JS"
        );
        // Instead it iterates all buttons and compares via getAttribute.
        assert!(COMMENT_JS.contains("getAttribute('data-file') === file"));
    }

    #[test]
    fn comment_js_validates_line_numbers_as_safe_integers() {
        // The script must validate line numbers to protect against huge/unsafe
        // values that could cause unbounded work.
        assert!(COMMENT_JS.contains("safeLineNo"));
        assert!(COMMENT_JS.contains("Number.MAX_SAFE_INTEGER"));
    }

    #[test]
    fn comment_js_updates_url_on_range_gesture() {
        // When the + gesture opens the comment editor, the URL should update to
        // the canonical selection fragment so ranges are copyable.
        assert!(COMMENT_JS.contains("selFragment(filePath, side, lo, hi)"));
        assert!(COMMENT_JS.contains("history.replaceState(null, '', '#' + frag)"));
    }

    /// The editor must not be a row in the diff table.
    ///
    /// A `<tr>` inside it means every keystroke dirties a table holding the whole
    /// file, and the browser re-lays-out and repaints all of it: measured on
    /// Chromium 131 at 40ms of main-thread work per character on a 3,600-line
    /// diff, against 7-11ms for the same box positioned over the table. Typing
    /// into a review comment is not a place to spend a table relayout.
    #[test]
    fn the_comment_editor_floats_rather_than_reflowing_the_table() {
        // Built as a bare element and hung off the body, not inserted into a row.
        assert!(COMMENT_JS.contains("box.className = 'notebox'"));
        assert!(COMMENT_JS.contains("document.body.appendChild(box)"));
        assert!(
            !COMMENT_JS.contains("row.className = 'pending'"),
            "the editor is a pending row again: that is the 40ms-a-keystroke shape"
        );
        // Document coordinates, so a vertical scroll carries it with the code and
        // no scroll handler is needed.
        assert!(COMMENT_JS.contains("window.scrollX"));
        assert!(COMMENT_JS.contains("window.scrollY"));

        let css = stylesheet(&theme_colors(theme("Nord")));
        assert!(css.contains(".notebox {\n  position: absolute;"), "{css}");
        // And it is styled on its own, not as a descendant of the table it left.
        assert!(
            !css.contains("table.src.commentable .notebox"),
            "the editor's rules still require it to be inside the table: {css}"
        );
        assert!(!css.contains("tr.pending"), "dead pending-row rules: {css}");
    }

    /// The line-number gutter sticks only while a code box is panned sideways.
    ///
    /// It is `position: sticky` so panning a long line keeps the numbers in view,
    /// but two gutter cells a line is 7,200 sticky boxes on a 3,600-line diff, and
    /// the browser revisits every one of them in each pre-paint: ~13ms of
    /// main-thread work per keystroke and per scrolled frame, measured, for a
    /// gutter that unpanned has nowhere to go.
    #[test]
    fn the_gutter_sticks_only_while_the_code_is_panned() {
        let css = stylesheet(&theme_colors(theme("Nord")));
        assert!(
            css.contains(".codebox.panned table.src td.ln { position: sticky; left: 0; }"),
            "{css}"
        );
        assert!(
            !css.contains("position: sticky; left: 0; background: var(--code-bg)"),
            "the gutter is unconditionally sticky again: {css}"
        );
        // Turned on by the scroll that needs it, and only on the transition: a
        // toggle per scroll event would re-invalidate the gutter every frame.
        assert!(NAV_JS.contains("classList.toggle('panned', panned)"));
        assert!(NAV_JS.contains("codebox.scrollLeft > 0"));
        assert!(NAV_JS.contains("{ passive: true }"));
    }

    /// A drag redraws only when the row under the pointer changes.
    ///
    /// `pointermove` fires many times a second; retinting the same span each time
    /// repaints the table for no visible change. The button scan is memoised for
    /// the same reason — it walks every `+` in the page.
    #[test]
    fn a_drag_redraws_only_when_the_row_changes() {
        assert!(COMMENT_JS.contains("if (button === drag.to) return;"));
        assert!(COMMENT_JS.contains("siblingCache"));
        assert!(COMMENT_JS.contains("Object.create(null)"));
    }

    /// The lines a patch leaves out can be asked for.
    ///
    /// A diff shows a few lines around each change and nothing in between, and
    /// when a hunk stops making sense on its own the reader otherwise has to go
    /// and open the file — which is the moment the review stops.
    #[test]
    fn the_hidden_lines_can_be_revealed() {
        // The band carries only the range; the script paints the labels, so there
        // is one implementation of them rather than one per language.
        assert!(EXPAND_JS.contains("function paint(band)"));
        assert!(EXPAND_JS.contains("/context?file="));
        // Lines from the top of the run go above the band, lines from the bottom
        // below it, so what is still hidden stays between the two shown sides.
        assert!(EXPAND_JS.contains("insertBefore(rows, band.nextSibling)"));
        assert!(EXPAND_JS.contains("insertBefore(rows, band)"));
        // One click cannot insert an unbounded number of rows.
        assert!(EXPAND_JS.contains("MAX_AT_ONCE"));
        // A revealed line is commentable like any other context line.
        assert!(EXPAND_JS.contains("add.className = 'addnote'"));
        // And the comment script is told, since its memo of those buttons is now
        // out of date.
        assert!(EXPAND_JS.contains("CustomEvent('wdyt:revealed')"));
        assert!(COMMENT_JS.contains("addEventListener('wdyt:revealed'"));

        let css = stylesheet(&theme_colors(theme("Nord")));
        assert!(css.contains("table.src.diff tr.gap td"), "{css}");
        assert!(css.contains("table.src.diff tr.gap button"), "{css}");
    }

    /// Nothing moves under the reader when lines appear above the fold.
    #[test]
    fn revealing_lines_keeps_the_page_still() {
        assert!(EXPAND_JS.contains("getBoundingClientRect().top"));
        assert!(EXPAND_JS.contains("window.scrollBy"));
    }

    /// A comment is the start of a discussion, not a note left in a box.
    #[test]
    fn a_thread_shows_who_said_what_and_who_is_waiting() {
        // The exchange is server-rendered, so a reload shows it without a poll.
        let css = stylesheet(&theme_colors(theme("Nord")));
        assert!(css.contains(".note .said.agent"), "{css}");
        assert!(css.contains(".note .threadfoot"), "{css}");
        // The same three states the session's own receipt uses.
        assert!(css.contains(".note .threadfoot .waiting .dot"), "{css}");
        assert!(css.contains(".note .threadfoot .delivered .dot"), "{css}");

        // The page polls a read-only route: watching for an answer must not mark
        // the reader's own question as picked up.
        assert!(THREAD_JS.contains("'/s/' + session + '/threads'"));
        assert!(
            !THREAD_JS.contains("threads/next"),
            "the page must not take questions from the agent"
        );
        // Answering back posts to the page's own route, which is what makes the
        // message the reader's rather than the agent's.
        assert!(THREAD_JS.contains("'/messages'"));
        // Polling stops with the tab.
        assert!(THREAD_JS.contains("visibilitychange"));
        // The reply box is outside the table, for the same reason the comment
        // editor is: typing must not relayout the diff.
        assert!(THREAD_JS.contains("document.body.appendChild(box)"));
        assert!(THREAD_JS.contains("box.className = 'notebox'"));
        // A comment made just now is a thread just started.
        assert!(COMMENT_JS.contains("CustomEvent('wdyt:commented')"));
        assert!(THREAD_JS.contains("addEventListener('wdyt:commented'"));
        assert!(COMMENT_JS.contains("setAttribute('data-thread'"));
    }

    #[test]
    fn comment_js_renders_permalink_on_saved_comments() {
        // Saved comments should include a discoverable permalink.
        assert!(COMMENT_JS.contains("permalink"));
        assert!(COMMENT_JS.contains("'#comment-' + comment.id"));
    }

    #[test]
    fn a_range_can_be_dragged_and_is_bounded_to_one_file_and_side() {
        // Pointer events, so a trackpad drag and a touch both work where mouse
        // events would only cover the first.
        assert!(COMMENT_JS.contains("pointerdown"));
        assert!(COMMENT_JS.contains("pointerup"));
        // A range spanning two files or both sides of a diff has no meaning, and
        // the server would reject it: the drag is confined at the source.
        assert!(COMMENT_JS.contains("data-file") && COMMENT_JS.contains("data-side"));
        assert!(COMMENT_JS.contains("end_line"), "the range is never sent");
        // The pointerup listener is on the document, not the table: releasing
        // outside the diff must still finish the drag rather than wedge it.
        assert!(COMMENT_JS.contains("document.addEventListener('pointerup'"));
    }

    #[test]
    fn a_ranged_comment_is_labelled_and_marked() {
        let css = stylesheet(&theme_colors(theme("Nord")));
        // Marked with a gutter bar, not a background tint: an accent-tinted row
        // reads as a deleted one and fights the diff's own colours.
        assert!(css.contains("tr.inrange td.code"), "{css}");
        assert!(
            css.contains("box-shadow: inset 3px 0 0 var(--accent)"),
            "{css}"
        );
        assert!(css.contains("tr.selecting"), "no drag feedback: {css}");

        let ranged = Comment {
            id: 1,
            file: "a.rs".to_owned(),
            target: None,
            line: 4,
            end_line: Some(9),
            side: crate::session::Side::New,
            snippet: String::new(),
            text: String::new(),
            at: std::time::SystemTime::now(),
            replies: Vec::new(),
            delivered_at: None,
        };
        assert_eq!(span_label(&ranged).as_deref(), Some("L4–L9"));
        // A single line is already identified by the row the note hangs under.
        let single = Comment {
            end_line: None,
            ..ranged.clone()
        };
        assert_eq!(span_label(&single), None);
        // And a degenerate range is not labelled either.
        let degenerate = Comment {
            end_line: Some(4),
            ..ranged
        };
        assert_eq!(span_label(&degenerate), None);
    }

    #[test]
    fn diff_rows_are_tinted_and_commentable() {
        let css = stylesheet(&theme_colors(theme("Nord")));
        // Tints mix into the code card's own background, so one rule works for
        // every syntax theme rather than needing a per-theme table.
        assert!(css.contains("table.src.diff tr.add td"), "{css}");
        assert!(css.contains("table.src.diff tr.del td"), "{css}");
        assert!(css.contains("var(--code-bg)"), "{css}");
        // The comment button is absolutely placed: in the flow it would shift
        // the code on hover and make the diff jump. Its rules are shared with the
        // plain-code listing via `.commentable`, which both tables carry.
        assert!(css.contains("table.src.commentable .addnote"), "{css}");
        assert!(css.contains("position: absolute; left: 1px"), "{css}");
    }

    #[test]
    fn line_classes_and_markers_agree() {
        assert_eq!(
            (line_class(LineKind::Added), marker(LineKind::Added)),
            ("add", "+")
        );
        assert_eq!(
            (line_class(LineKind::Removed), marker(LineKind::Removed)),
            ("del", "-")
        );
        assert_eq!(
            (line_class(LineKind::Context), marker(LineKind::Context)),
            ("ctx", " ")
        );
    }

    #[test]
    fn reply_js_posts_json_and_is_not_escaped() {
        // Guards the `Raw` wrapper: `&&` inside the script must stay literal.
        assert!(REPLY_JS.contains("application/json"));
        assert!(REPLY_JS.contains("metaKey || e.ctrlKey"));
    }

    #[test]
    fn reply_js_bails_out_when_the_form_is_absent() {
        // An already-replied session renders no form; the script must not
        // attach listeners to null. Regression test for a page-level JS error.
        assert!(
            REPLY_JS.contains("if (!form || !box || !send || !error) return"),
            "missing the guard for a form-less panel"
        );
        // Opening and closing must still work in that state, so the sent reply
        // can be re-read; only the sending half is skipped.
        let (open_close, sending) = REPLY_JS
            .split_once("if (!form || !box || !send || !error) return")
            .expect("guard present");
        assert!(open_close.contains("toggle.addEventListener('click'"));
        assert!(open_close.contains("open();"));
        assert!(sending.contains("form.addEventListener('submit'"));
    }

    #[test]
    fn fragment_js_handles_comment_and_selection_anchors() {
        // The fragment script must handle both comment-<id> and sel-<...> forms.
        assert!(FRAGMENT_JS.contains("comment-"));
        assert!(FRAGMENT_JS.contains("parseSel"));
        assert!(FRAGMENT_JS.contains("highlightComment"));
        assert!(FRAGMENT_JS.contains("highlightSelection"));
    }

    #[test]
    fn fragment_js_listens_to_hashchange_and_page_load() {
        // On load and on navigation the fragment must be processed.
        assert!(FRAGMENT_JS.contains("addEventListener('hashchange'"));
        assert!(FRAGMENT_JS.contains("location.hash"));
        assert!(FRAGMENT_JS.contains("setTimeout(handleFragment"));
    }

    #[test]
    fn fragment_js_updates_url_on_line_click() {
        // Clicking a line number updates the URL fragment to a sel- form so the
        // deep link is copyable.
        assert!(FRAGMENT_JS.contains("history.replaceState"));
        assert!(FRAGMENT_JS.contains("selFragment"));
    }

    #[test]
    fn fragment_js_falls_through_for_file_anchors() {
        // The script must not eat fragments it does not own: file anchors and
        // heading anchors are handled natively by the browser.
        assert!(FRAGMENT_JS.contains("clearHighlights"));
        // Only `sel-` and `comment-` are handled; everything else falls through.
        assert!(FRAGMENT_JS.contains("indexOf('comment-') === 0"));
        assert!(FRAGMENT_JS.contains("indexOf('sel-') !== 0"));
    }

    #[test]
    fn fragment_js_handles_hand_written_file_line_anchors() {
        // The code/diff fragment script must parse GitHub-style `f-<path>-L<n>`
        // and `-L<start>-L<end>` anchors and highlight the matching rows.
        assert!(FRAGMENT_JS.contains("parseFileLine"));
        assert!(FRAGMENT_JS.contains("highlightFileLine"));
        // It recomputes the lossy file anchor per row rather than reversing it.
        assert!(FRAGMENT_JS.contains("fileAnchor"));
        // Parsing peels the capital-L marker from the right, so dashed paths work.
        assert!(FRAGMENT_JS.contains(r"-L(\d+)$"));
        // Only the `new` side carries hand-writable line numbers.
        assert!(FRAGMENT_JS.contains("getAttribute('data-side') !== 'new'"));
    }

    #[test]
    fn doc_comment_js_handles_hand_written_file_line_anchors() {
        // Docs map a file-line anchor to the rendered block covering those lines.
        assert!(DOC_COMMENT_JS.contains("parseFileLine"));
        assert!(DOC_COMMENT_JS.contains("handleDocFileLine"));
        // Overlap test against the block's sourcepos span.
        assert!(DOC_COMMENT_JS.contains("sp.startLine <= fl.end && sp.endLine >= fl.start"));
        // The brief has no source path, so it is skipped for file-line anchors.
        assert!(DOC_COMMENT_JS.contains("docFile === 'brief:'"));
    }

    #[test]
    fn fragment_js_respects_reduced_motion() {
        // Scrolling must use 'auto' behaviour when reduced motion is preferred.
        assert!(FRAGMENT_JS.contains("prefers-reduced-motion: reduce"));
        assert!(FRAGMENT_JS.contains("scrollBehavior"));
        assert!(FRAGMENT_JS.contains("'auto'"));
    }

    #[test]
    fn fragment_js_validates_safe_integers() {
        // The parser must reject non-positive or non-finite line numbers.
        assert!(FRAGMENT_JS.contains("safeInt"));
        assert!(FRAGMENT_JS.contains("Number.MAX_SAFE_INTEGER"));
    }

    #[test]
    fn fragment_js_iterates_dom_not_numeric_range() {
        // highlightSelection must iterate actual buttons in the DOM rather than
        // looping over a numeric range, which would be unbounded for a hostile
        // huge range.
        assert!(FRAGMENT_JS.contains("querySelectorAll('.addnote')"));
        // Must NOT contain a `for (var line = sel.start; line <= sel.end` loop.
        assert!(
            !FRAGMENT_JS.contains("line <= sel.end"),
            "fragment JS still uses numeric range iteration"
        );
    }

    #[test]
    fn deep_link_highlight_styles_exist() {
        let css = stylesheet(&theme_colors(theme("Nord")));
        // Highlighted rows get an accent tint distinct from the range marker.
        assert!(css.contains("tr.sel-highlight td"), "{css}");
        // Comments flash briefly to draw the eye.
        assert!(css.contains(".note.sel-highlight"), "{css}");
        assert!(css.contains("wdyt-flash"), "{css}");
        // Reduced motion preference is respected for the flash animation.
        assert!(css.contains("prefers-reduced-motion"), "{css}");
        // Permalink on comments is styled.
        assert!(css.contains(".note .permalink"), "{css}");
    }

    // --- Task #11: keyboard hints in comment editors -------------------------

    #[test]
    fn comment_js_includes_keyboard_hint_with_accessible_markup() {
        // The hint must be visible, using semantic elements.
        assert!(COMMENT_JS.contains("kb-hint"));
        assert!(COMMENT_JS.contains("<kbd"));
        assert!(COMMENT_JS.contains("<abbr"));
        assert!(COMMENT_JS.contains("aria-label"));
        // Retains the actual keyboard behavior.
        assert!(COMMENT_JS.contains("ev.key === 'Escape'"));
        assert!(COMMENT_JS.contains("ev.metaKey || ev.ctrlKey"));
    }

    #[test]
    fn doc_comment_js_includes_keyboard_hint() {
        assert!(DOC_COMMENT_JS.contains("kb-hint"));
        assert!(DOC_COMMENT_JS.contains("<kbd"));
        assert!(DOC_COMMENT_JS.contains("<abbr"));
        assert!(DOC_COMMENT_JS.contains("aria-label"));
    }

    #[test]
    fn keyboard_hint_is_styled_unobtrusively() {
        let css = stylesheet(&theme_colors(theme("Nord")));
        assert!(css.contains(".kb-hint"));
        // The hint uses the muted color and small font to stay unobtrusive.
        assert!(css.contains(".kb-hint"));
    }

    // --- Task #12: navigation ------------------------------------------------

    #[test]
    fn jump_to_top_is_styled_and_hidden_on_framed() {
        let css = stylesheet(&theme_colors(theme("Nord")));
        assert!(css.contains("#jump-top"), "no jump-to-top CSS");
        assert!(
            css.contains("body.framed #jump-top"),
            "framed pages must hide jump-to-top"
        );
        // Mobile responsive adjustments.
        assert!(css.contains("max-width: 600px"));
    }

    #[test]
    fn sidebar_desktop_rules_are_responsive() {
        let css = stylesheet(&theme_colors(theme("Nord")));
        // Sidebar activates at wide viewports only.
        assert!(css.contains("min-width: 1380px"));
        assert!(css.contains("nav.files.has-sidebar"));
        // Active link state is global (visible on both mobile and desktop).
        assert!(css.contains("nav.files a.active"));
        // Content shifts right.
        assert!(css.contains("has-file-sidebar main"));
    }

    #[test]
    fn nav_js_uses_intersection_observer_and_passive_scroll() {
        assert!(NAV_JS.contains("IntersectionObserver"));
        // Scroll listener is passive for performance.
        assert!(NAV_JS.contains("passive: true"));
        // Jump-to-top respects reduced motion preference.
        assert!(NAV_JS.contains("prefers-reduced-motion: reduce"));
        assert!(NAV_JS.contains("'auto' : 'smooth'"));
    }

    #[test]
    fn nochrome_does_not_activate_sidebar() {
        let css = stylesheet(&theme_colors(theme("Nord")));
        // The sidebar CSS explicitly excludes .nochrome bodies.
        assert!(css.contains("not(.nochrome)"));
    }

    // =========================================================================
    // Review finding #1: <=1379px horizontal scrollable strip
    // =========================================================================

    #[test]
    fn narrow_nav_is_single_row_scrollable_strip() {
        let css = stylesheet(&theme_colors(theme("Nord")));
        // At <=1379px, the nav must be nowrap + overflow-x: auto for horizontal
        // scrolling, and use `scrollbar-width: thin` for unobtrusive scrollbar.
        assert!(
            css.contains("max-width: 1379px"),
            "no narrow breakpoint: {css}"
        );
        assert!(
            css.contains("flex-wrap: nowrap"),
            "narrow nav not forced to nowrap: {css}"
        );
        assert!(
            css.contains("overflow-x: auto"),
            "narrow nav missing horizontal scroll: {css}"
        );
        assert!(
            css.contains("-webkit-overflow-scrolling: touch"),
            "narrow nav missing touch scrolling: {css}"
        );
        assert!(
            css.contains("scrollbar-width: thin"),
            "narrow nav missing thin scrollbar: {css}"
        );
        // Items must not wrap: each link stays on one line.
        assert!(
            css.contains("nav.files.has-sidebar a {\n    white-space: nowrap; flex: 0 0 auto;")
                || css.contains("white-space: nowrap; flex: 0 0 auto;"),
            "narrow nav links not set to nowrap: {css}"
        );
        // Scroll-margin-top derived for one row so anchors are not hidden.
        assert!(
            css.contains("scroll-margin-top: 92px"),
            "narrow nav missing scroll-margin for anchors: {css}"
        );
    }

    // =========================================================================
    // Review finding #2: desktop sidebar alignment (brief + .wrap same left axis)
    // =========================================================================

    #[test]
    fn desktop_brief_and_wrap_share_left_axis() {
        let css = stylesheet(&theme_colors(theme("Nord")));
        // The brief must have `margin-left: 200px` when sidebar is present,
        // so it shifts the same amount as `main`.
        assert!(
            css.contains("has-file-sidebar section.brief {\n    margin-left: 200px;")
                || css.contains("has-file-sidebar section.brief {\n    margin-left: 200px"),
            "brief not shifted by sidebar width: {css}"
        );
        // The brief's padding-left formula must use (100% - 200px - 1100px) to
        // account for the sidebar width, matching .wrap's centering within the
        // remaining space.
        assert!(
            css.contains("100% - 200px - 1100px"),
            "brief padding formula does not account for sidebar width: {css}"
        );
    }

    // =========================================================================
    // Review finding #3: active nav-link styling outside desktop-only media
    // =========================================================================

    #[test]
    fn active_link_styling_is_global_not_desktop_only() {
        let css = stylesheet(&theme_colors(theme("Nord")));
        // The rule `nav.files a.active` must exist at the top level (not inside
        // a min-width: 1380px media query) so horizontal/mobile tracking works.
        assert!(
            css.contains("nav.files a.active"),
            "no global active link rule: {css}"
        );
        // Verify it has the expected styles.
        assert!(
            css.contains("nav.files a.active {\n  background: var(--surface)")
                || css.contains("nav.files a.active {"),
            "active rule missing styling: {css}"
        );
    }

    // =========================================================================
    // Review finding #4: shortcut hint platform-conditional visible text
    // =========================================================================

    #[test]
    fn comment_js_keyboard_hint_renders_ctrl_on_non_mac() {
        // The visible text inside <abbr> must be conditional on platform:
        // - Mac: ⌘↩ (\u2318+Enter)
        // - Non-Mac: Ctrl↩
        assert!(
            COMMENT_JS.contains(r"'\u2318+Enter' : 'Ctrl+Enter'")
                || COMMENT_JS.contains(r#"'\u2318+Enter' : 'Ctrl+Enter'"#),
            "COMMENT_JS visible hint not platform-conditional"
        );
        // Must NOT hard-code only the Mac glyph unconditionally.
        assert!(
            !COMMENT_JS.contains(r#"'>\u2318+Enter</abbr> send"#),
            "COMMENT_JS still hard-codes Mac glyph without condition"
        );
    }

    #[test]
    fn doc_comment_js_keyboard_hint_renders_ctrl_on_non_mac() {
        // Same check for doc comment JS.
        assert!(
            DOC_COMMENT_JS.contains(r"'\u2318+Enter' : 'Ctrl+Enter'")
                || DOC_COMMENT_JS.contains(r#"'\u2318+Enter' : 'Ctrl+Enter'"#),
            "DOC_COMMENT_JS visible hint not platform-conditional"
        );
    }

    // =========================================================================
    // Review finding #5: jump-to-top respects prefers-reduced-motion
    // =========================================================================

    #[test]
    fn jump_to_top_respects_reduced_motion() {
        // The scrollTo call must query prefers-reduced-motion and use 'auto'
        // when the user prefers reduced motion.
        assert!(
            NAV_JS.contains("prefers-reduced-motion: reduce"),
            "NAV_JS does not query reduced motion"
        );
        assert!(
            NAV_JS.contains("prefersReduced") || NAV_JS.contains("prefers-reduced-motion"),
            "NAV_JS does not store reduced motion preference"
        );
        // When reduced motion is preferred, behavior should be 'auto'.
        assert!(
            NAV_JS.contains("'auto' : 'smooth'") || NAV_JS.contains("'auto':'smooth'"),
            "NAV_JS does not conditionally use 'auto' for reduced motion"
        );
        // Must NOT unconditionally use 'smooth'.
        let smooth_count = NAV_JS.matches("behavior: 'smooth'").count();
        assert_eq!(
            smooth_count, 0,
            "NAV_JS has unconditional 'smooth' scrolling ({smooth_count} occurrences)"
        );
    }

    // =========================================================================
    // Review finding #6: comment permalinks visible on keyboard focus
    // =========================================================================

    #[test]
    fn permalink_visible_on_focus_not_hover_only_code_diff() {
        let css = stylesheet(&theme_colors(theme("Nord")));
        // The permalink must become visible on :focus and :focus-visible,
        // not just on :hover.
        assert!(
            css.contains(".note .permalink:focus"),
            "code/diff permalink not visible on :focus: {css}"
        );
        assert!(
            css.contains(".note .permalink:focus-visible"),
            "code/diff permalink not visible on :focus-visible: {css}"
        );
    }

    #[test]
    fn permalink_visible_on_focus_not_hover_only_docs() {
        let css = stylesheet(&theme_colors(theme("Nord")));
        // Doc notes permalinks must also become visible on focus.
        assert!(
            css.contains(".doc .doc-notes .note .permalink:focus"),
            "doc permalink not visible on :focus: {css}"
        );
        assert!(
            css.contains(".doc .doc-notes .note .permalink:focus-visible"),
            "doc permalink not visible on :focus-visible: {css}"
        );
    }
}
