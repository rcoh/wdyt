//! The page shell: shared chrome, styling, and the reply widget.

use topcoat::Result;
use topcoat::context::Cx;
use topcoat::view::{NodeViewParts, PartsWriter, component, view};

use crate::render::ThemeColors;
use crate::session::{Comment, Hunk, LineKind, Reply};

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
/* An anchored jump must not land under the sticky nav. Generous enough to clear
   it when it has wrapped to a second row on a wide diff. */
section.file, section.doc {{ scroll-margin-top: 92px; }}

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
  position: sticky; left: 0; background: var(--code-bg);
}}
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
/* The comment affordance sits in the code cell's left padding so it cannot
   shift the code, which would make the diff jump on hover. */
table.src.diff td.code {{ position: relative; padding-left: 22px; }}
table.src.diff .addnote {{
  position: absolute; left: 1px; top: 1px;
  width: 17px; height: 17px; padding: 0; line-height: 15px;
  border: none; border-radius: 4px; cursor: pointer;
  background: var(--accent); color: #fff; font: 600 13px/15px ui-sans-serif, sans-serif;
  opacity: 0; transition: opacity .1s;
}}
table.src.diff tr:hover .addnote, table.src.diff .addnote:focus {{ opacity: 1; }}
/* Comments break out of the code card's monospace world: they are prose. Inset
   as a card rather than a full-bleed band, so the diff still reads as one
   continuous listing with a note tucked into it. */
table.src.diff tr.notes td.code, table.src.diff tr.pending td.code {{
  white-space: normal; padding: 6px 14px 6px 22px;
  background: var(--code-gutter);
  font: 13px/1.55 ui-sans-serif, system-ui, sans-serif;
}}
table.src.diff .note, table.src.diff .notebox {{
  background: var(--surface); color: var(--fg);
  border: 1px solid var(--border-strong); border-left: 2px solid var(--accent);
  border-radius: 0 8px 8px 0; padding: 7px 12px; max-width: 620px;
}}
table.src.diff .note {{ margin: 4px 0; }}
table.src.diff .note .who {{
  display: block; font-size: 10.5px; text-transform: uppercase;
  letter-spacing: .07em; color: var(--muted);
}}
/* Which lines a note covers, when it is more than the one directly above it. */
table.src.diff .note .who .span {{
  margin-left: 6px; text-transform: none; letter-spacing: 0;
  font-family: ui-monospace, monospace; opacity: .85;
}}
/* Rows a comment's range covers: while dragging, and once saved.
   Marked with a bar down the gutter rather than a background tint. Tinting fights
   the diff's own colours — an accent-tinted row reads as a deleted one — so the
   row background is left to mean add/remove and nothing else. */
table.src.diff tr.inrange td.code, table.src.diff tr.selecting td.code {{
  box-shadow: inset 3px 0 0 var(--accent);
}}
/* Mid-drag the whole span also lifts slightly, since nothing is committed yet and
   the feedback has to be unmistakable. */
table.src.diff tr.selecting td {{
  background: color-mix(in srgb, var(--accent) 10%, var(--code-bg));
}}
/* A drag over code would otherwise select text and fight the range. */
table.src.diff tr.selecting {{ user-select: none; }}
table.src.diff .notebox {{ display: flex; flex-direction: column; gap: 6px; margin: 4px 0; }}
table.src.diff .notebox textarea {{
  width: 100%; min-height: 62px; resize: vertical;
  background: var(--bg); color: var(--fg);
  border: 1px solid var(--border-strong); border-radius: 8px; padding: 7px 9px;
  font: 13px/1.5 ui-sans-serif, system-ui, sans-serif;
}}
table.src.diff .notebox textarea:focus {{
  outline: none; border-color: var(--accent);
  box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 18%, transparent);
}}
table.src.diff .notebox .row {{ display: flex; gap: 6px; align-items: center; }}
table.src.diff .notebox .msg {{ color: #b3452c; font-size: 12px; margin-right: auto; }}
table.src.diff .notebox button {{
  font: 12.5px/1 ui-sans-serif, system-ui, sans-serif;
  padding: 6px 11px; border-radius: 7px; cursor: pointer;
  border: 1px solid var(--border-strong); background: var(--bg); color: var(--fg);
}}
table.src.diff .notebox button.primary {{
  background: var(--accent); border-color: var(--accent); color: #fff; font-weight: 600;
}}
.added {{ color: #3f7d58; }}
.removed {{ color: #b3452c; }}
nav.files .added, nav.files .removed {{ margin-left: 4px; }}

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
/* Fenced blocks come out of the same syntect theme as `showme code`, so they
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
  background: var(--accent); animation: showme-pulse 1.8s ease-in-out infinite;
}}
/* Picked up but unacknowledged: amber, and still pulsing. This is where an agent
   that died on a credentials prompt sits, so it must not look like success. */
#reply .receipt.delivered .dot {{
  background: #b8860b; animation: showme-pulse 1.8s ease-in-out infinite;
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
@keyframes showme-pulse {{
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
/// CLI, and showme needs to run from a plain `cargo install`.
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
  var KEY = 'showme.reply.pos';

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
  var KEY = 'showme.chrome.hidden';

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

/// Line comments on a diff.
///
/// The box is opened next to the line rather than in a dialog: a review comment
/// only makes sense against the code it is about, and moving the eye away from
/// the line to a modal loses exactly the context that matters.
const COMMENT_JS: &str = r#"
(function () {
  var root = document.getElementById('diff');
  if (!root) return;
  var session = root.getAttribute('data-session');
  var open = null;
  // A drag in progress: the `+` it started on, and the row it is currently over.
  var drag = null;

  function close() {
    if (open) { open.remove(); open = null; }
    clearPreview();
  }

  function clearPreview() {
    var marked = root.querySelectorAll('tr.selecting');
    for (var i = 0; i < marked.length; i++) marked[i].classList.remove('selecting');
  }

  // Every commentable row on one side of one file, in document order.
  function siblings(button) {
    var file = button.getAttribute('data-file');
    var side = button.getAttribute('data-side');
    var all = root.querySelectorAll(
      '.addnote[data-file="' + file + '"][data-side="' + side + '"]');
    var rows = [];
    for (var i = 0; i < all.length; i++) rows.push(all[i]);
    return rows;
  }

  // Highlight what a range would cover while the pointer is still down.
  function preview(from, to) {
    clearPreview();
    var lo = Math.min(Number(from.getAttribute('data-line')), Number(to.getAttribute('data-line')));
    var hi = Math.max(Number(from.getAttribute('data-line')), Number(to.getAttribute('data-line')));
    var buttons = siblings(from);
    for (var i = 0; i < buttons.length; i++) {
      var n = Number(buttons[i].getAttribute('data-line'));
      if (n >= lo && n <= hi) buttons[i].closest('tr').classList.add('selecting');
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
    var who = document.createElement('span');
    who.className = 'who';
    who.textContent = 'you';
    // Same shape the server renders, so a reload looks identical.
    if (comment.end_line && comment.end_line > comment.line) {
      var span = document.createElement('span');
      span.className = 'span';
      span.textContent = 'L' + comment.line + '–L' + comment.end_line;
      who.appendChild(span);
    }
    note.appendChild(who);
    note.appendChild(document.createTextNode(comment.text));
    into.appendChild(note);
  }

  function openBox(from, to) {
    // The box hangs below the last line of the range, so the whole passage it is
    // about stays visible above it.
    var lo = Math.min(Number(from.getAttribute('data-line')), Number(to.getAttribute('data-line')));
    var hi = Math.max(Number(from.getAttribute('data-line')), Number(to.getAttribute('data-line')));
    var last = hi === Number(from.getAttribute('data-line')) ? from : to;
    var line = last.closest('tr');

    // Pressing the same line twice closes the box rather than opening a second.
    var reopen = !open || open.previousElementSibling !== line;
    close();
    if (!reopen) return;

    var span = hi > lo ? ' (L' + lo + '–L' + hi + ')' : '';
    var label = hi > lo ? 'Comment on lines ' + lo + ' to ' + hi : 'Comment on this line';
    var row = document.createElement('tr');
    row.className = 'pending';
    row.innerHTML =
      '<td class="ln" colspan="2"></td>' +
      '<td class="code"><div class="notebox">' +
      '<textarea aria-label="' + label + '"></textarea>' +
      '<div class="row"><span class="msg"></span>' +
      '<button type="button" class="cancel">Cancel</button>' +
      '<button type="button" class="primary">Comment</button>' +
      '</div></div></td>';
    line.parentNode.insertBefore(row, line.nextElementSibling);
    row.querySelector('textarea').placeholder =
      (hi > lo ? 'Comment on these lines' : 'Comment on this line') + span + '…';
    open = row;

    // Keep the covered rows tinted while the box is open, so it is unambiguous
    // what a multi-line comment is about.
    if (hi > lo) {
      var buttons = siblings(from);
      for (var i = 0; i < buttons.length; i++) {
        var n = Number(buttons[i].getAttribute('data-line'));
        if (n >= lo && n <= hi) buttons[i].closest('tr').classList.add('selecting');
      }
    }

    var box = row.querySelector('textarea');
    var submit = row.querySelector('button.primary');
    var msg = row.querySelector('.msg');
    box.focus();

    row.querySelector('button.cancel').addEventListener('click', close);
    box.addEventListener('keydown', function (ev) {
      if (ev.key === 'Escape') { close(); return; }
      if ((ev.metaKey || ev.ctrlKey) && ev.key === 'Enter') submit.click();
    });

    submit.addEventListener('click', function () {
      var text = box.value.trim();
      if (!text) { box.focus(); return; }
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
        // The pending row is replaced by the saved comment, in place.
        var anchor = row.previousElementSibling;
        close();
        // The rows the comment covers keep their tint, now permanently.
        if (hi > lo) {
          var buttons = siblings(from);
          for (var i = 0; i < buttons.length; i++) {
            var n = Number(buttons[i].getAttribute('data-line'));
            if (n >= lo && n <= hi) buttons[i].closest('tr').classList.add('inrange');
          }
        }
        render(comment, noteRow(anchor).querySelector('td.code'));
      }).catch(function (err) {
        msg.textContent = String(err.message || err);
        submit.disabled = false;
      });
    });
  }
})();
"#;

/// One hunk of a diff, with a comment affordance on every line.
///
/// The line number cells are buttons rather than links: clicking one opens an
/// inline comment box anchored to that line. A line already commented on shows
/// the comment underneath, in the flow of the diff, which is where it is
/// actually useful when re-reading.
#[component]
pub async fn diff_hunk(file: &str, hunk: &Hunk, comments: &[Comment]) -> Result {
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
            let mine = |c: &&Comment| c.file == file && c.side.as_str() == side;
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
        <table class="src diff">
            <tbody>
                <tr class="hunk">
                    <td class="ln" colspan="2"></td>
                    <td class="code">(&hunk.header)</td>
                </tr>
                for row in rows {
                    <tr class=(row_class(&row))>
                        <td class="ln old">
                            if let Some(n) = row.line.old { (n) }
                        </td>
                        <td class="ln new">
                            if let Some(n) = row.line.new { (n) }
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
                                for comment in row.comments {
                                    <div class="note">
                                        <span class="who">
                                            "you"
                                            // Says what the note is about when it
                                            // covers more than the line above it.
                                            if let Some(span) = span_label(&comment) {
                                                <span class="span">(span)</span>
                                            }
                                        </span>
                                        (&comment.text)
                                    </div>
                                }
                            </td>
                        </tr>
                    }
                }
            </tbody>
        </table>
    }
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
                <title>(&title) " · showme"</title>
                <style>(Raw(stylesheet(colors)))</style>
            </head>
            <body class=(body_class)>
                <header class="bar">
                    <span class="brand">"showme"</span>
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
                        "showme ▾"
                    </button>
                    <script>(Raw(CHROME_JS))</script>
                }

                reply_widget(session_id: &session_id, existing: existing_reply)

                if commentable {
                    <script>(Raw(COMMENT_JS))</script>
                }
            </body>
        </html>
    }
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
        assert!(CHROME_JS.contains("showme.chrome.hidden"));
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
        // The session id travels on the element rather than being inlined into
        // the script, which is shared by every page.
        assert!(COMMENT_JS.contains("getAttribute('data-session')"));
        assert!(COMMENT_JS.contains("'/s/' + session + '/comments'"));
        // Two comments on one line join the same block rather than stacking
        // duplicate rows.
        assert!(COMMENT_JS.contains("classList.contains('notes')"));
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
            line: 4,
            end_line: Some(9),
            side: crate::session::Side::New,
            snippet: String::new(),
            text: String::new(),
            at: std::time::SystemTime::now(),
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
        // the code on hover and make the diff jump.
        assert!(css.contains("table.src.diff .addnote"), "{css}");
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
}
