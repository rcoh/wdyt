# wdyt — task list

Feedback from the user's review of the ack-flow / origin / guided-review batch,
plus follow-ups. Ordered roughly as worked.

## Done

- [x] **#7 — Order diff files by importance.** `wdyt diff` sorts files
  source-first, then hand-written docs/config, then lockfiles and generated
  churn, so `Cargo.lock` and `.config/` are not what you see first.
  (`diff::review_rank`, applied in `main.rs`.) *Committed.*
- [x] **#10 — Rename `origin.rs` → `zellij_origin.rs`; verify the no-zellij
  path.** `detect()` splits env-reading from assembly so the cwd-only case never
  shells out to a `zellij` binary that may not exist; tested. *Committed.*
- [x] **#13 — Add commenting to the code viewer, not just the diff.** `CodeFile`
  now stores per-line `sources` (authoritative snippets) and `highlighted` HTML;
  a `code_file` component mirrors `diff_hunk`; the client JS binds to
  `[data-comments]` so one script drives both. Single-line + range drag verified
  in the browser. *Not yet committed.*

## In progress

_(none — #13 just finished, awaiting commit)_

## Pending

- [x] **#8 — Stable comment anchors and deep-linkable selections.** Comments need
  stable per-comment anchors, and a line/range selection should be deep-linkable
  (URL fragment that scrolls to and highlights it). The user's main reply:
  "I think you need to be able to link to selections." Touches `ui.rs`/`server.rs`.
- [x] **#9 — Whole-line comments in doc/brief markdown mode.** Extend commenting
  to rendered markdown (docs + the guided-review brief), simplest possible,
  whole-line. Builds on #13's generalized machinery.
- [x] **#11 — Visual hint that ⌘/Ctrl+Enter sends a comment.** Small hint in the
  comment box. Part of the coupled `ui.rs` pass.
- [x] **#12 — TOC: jump-to-top and/or vertical sidebar while scrolling.** The
  file nav gets hard to read with many files; add a jump-to-top, maybe a vertical
  sidebar. Part of the coupled `ui.rs` pass.
- [x] **#14 — Wait for a reply by default; add `--no-wait` to opt out.** Agents
  were failing to wait for comments because `--wait` is opt-in. Flip it: waiting
  becomes the default for the content commands; agents pass `--no-wait` for
  fire-and-forget. Keeps the timeout + inbox/ack fallback. (`main.rs`.)
- [x] **#15 — Make the `wdyt` CLI output a mini skill for agents.** Observed
  agents (1) failing to link to source files in the brief, (2) failing to wait
  for comments. `--help` and the post-session stdout/stderr should teach the
  workflow: link files with `#f-<path>` in `--brief`, wdyt waits by default,
  comments come back via `collect`, `ack` when you've read the reply. Consider
  printing the file anchors after a diff/code session. (`main.rs`.)

## Notes / open questions

- Monitoring gap the user caught: a `--wait` wrapped in `timeout` gets killed and
  nothing wakes the agent. Fix going forward: run the wait as a persistent
  background task with **no** `timeout` wrapper.
- Still open from earlier: which light syntax theme should be the default
  (`GitHub`, `OneHalfLight`, `Catppuccin Latte`, `Solarized (light)`)?
- Never sent: the topcoat findings (raw-text `<script>`/`<style>` escaping;
  `runtime`/`asset` needing the CLI bundler; `view!` rejecting typed `let`).
