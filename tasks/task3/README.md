# Task 3: Guided diff content model

## Goal

Add a guided review document that interleaves prose, selected diff ranges, and
selected current-code ranges in one ordered page.

The source is Markdown with top-level structured directives:

````md
## Public API

`Client::present` is additive.

```wdyt-diff
{"file":"src/client.rs","side":"new","start":110,"end":145}
```

## Tests

```wdyt-code
{"file":"tests/present.rs","start":1,"end":80}
```
````

## Model And Parsing

- Add `Content::Guided { blocks }`.
- Blocks are text, diff range, or code range, preserving source order.
- Directive bodies are strict JSON parsed with `serde_json`; reject unknown
  fields and malformed, empty, reversed, zero, or excessively large ranges.
- Normal Markdown is rendered through the existing safe Comrak configuration.
- `wdyt-diff` selects lines from a supplied unified patch by file and side.
  Include removed/added counterparts at the same change location so a
  replacement is not shown as only one half.
- A diff block retains validated full-file context as well as its initially
  visible range. The UI must be able to expand above and below the selection;
  a guided excerpt must not strand the reader when more context is needed.
- `wdyt-code` reads a workspace-relative UTF-8 file and retains original line
  numbers. Reject absolute paths and parent traversal.
- Cap each selected range at 500 lines and a guide at 100 blocks.
- Preserve authoritative plain text for future line comments.

## Write Scope

- new `src/guided.rs`
- `src/session.rs`
- `src/lib.rs`
- unit tests colocated with the new module

Do not edit `src/main.rs`, `src/server.rs`, `src/ui.rs`, or skill files. The main
agent will connect the model to CLI creation and server rendering after review.

## Acceptance

- Mixed blocks preserve order.
- Markdown remains escaped and syntax highlighting uses the selected theme.
- Old-side and new-side diff ranges are tested.
- Replacement pairs, missing files, invalid JSON, bad paths, and range limits
  are tested.
- Retained context and expansion bounds are tested.
- Persisted session serde remains backward-compatible.
