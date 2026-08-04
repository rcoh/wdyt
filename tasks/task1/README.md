# Task 1: Separate creation from presentation

## Goal

Add one composable CLI/API:

```sh
wdyt create code src/lib.rs --title "public API"
wdyt create docs DESIGN.md --title "design"
wdyt present <session-id> <session-id>
```

`create` registers content and returns without notifying or waiting. `present`
builds one URL with repeated `s` query parameters, notifies once, and waits for
feedback on the first session by default. There is no backwards-compatibility
requirement: remove the old one-shot top-level content commands rather than
maintaining two ways to do the same thing.

## Public Contract

- `wdyt create <mode> ...` writes one JSON object to stdout with `id`, `url`,
  `title`, and `kind`.
- Creation never sends a webhook and never waits.
- `wdyt present <id>...` requires at least one ID and emits
  `http://HOST:PORT/?s=<id>&s=<id>`.
- The first ID owns the presentation-level reply and is the session that
  `present` waits on. Comments in other tabs remain owned by those sessions.
- `present` accepts `--title`, `--note`, `--no-notify`, `--no-wait`, and
  `--timeout` with the same notification/wait semantics as today's content
  commands.
- Session IDs must be validated as URL-safe daemon IDs before interpolation.
- stdout remains machine-readable; guidance remains on stderr.

## Write Scope

- `src/main.rs`
- `src/client.rs`
- `src/notify.rs` only if presentation metadata requires it
- focused CLI tests in a new test file

Do not edit `src/server.rs`, `src/ui.rs`, `src/session.rs`, or skill files.

## Acceptance

- Create-only behavior is covered for at least code and diff.
- Presentation URL order and repeated query keys are tested.
- Single-tab and multi-tab presentation commands are tested.
- Waiting, no-wait, timeout, and notification-suppression behavior are tested.
