# Task 2: Render grouped sessions as tabs

## Goal

Render `/?s=s1&s=s2&s=s3` as one accessible tabbed review. The root page
continues to list sessions when no `s` parameters are supplied.

## Behavior

- Preserve query order and de-duplicate repeated IDs.
- Reject an empty selection, unknown IDs, and more than 12 tabs with a clear
  client error.
- Use each session's title and kind for the tab label.
- Keep one stable tab strip; switching tabs must not resize the page.
- Lazy-load inactive session frames where practical.
- The first session owns the shared overall reply widget.
- Embedded session pages hide their own bar and reply widget but retain content,
  line comments, threaded discussion, and deep links.
- Browser back/forward and a copied URL preserve the selected tab.
- Arrow keys move between tabs; Home and End select the first and last tab.
- Mobile uses a horizontally scrollable tab strip without overlapping content.
- The no-query home screen remains useful rather than becoming a separate dead
  end: session rows have selection controls and a stable "Open selected tabs"
  action that builds the same repeated-`s` URL. A direct session link remains
  available for opening one item.

## Write Scope

- `src/server.rs`
- `src/ui.rs`
- `tests/pages.rs`

This task starts only after tasks 1 and 3 are integrated, so it may render the
new guided content model. Do not edit CLI or skill files.

## Acceptance

- The no-query index still lists sessions and adds the multi-select composition
  flow without automatically opening every session.
- The home-screen selection flow and generated tab URL are covered.
- One, several, duplicate, unknown, and over-limit selections are tested.
- The grouped page has correct ARIA tab/tabpanel relationships.
- Embedded pages have no duplicate reply widget or chrome.
- The primary reply POST targets the first session.
- Existing session page and comment tests continue to pass.
