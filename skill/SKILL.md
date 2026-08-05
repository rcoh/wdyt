---
name: wdyt
description: Shows work in one or more review tabs and collects the user's feedback. Use when presenting code, diffs, documents, screenshots, static reports, or live demos for review. Pair with presenting-code-changes for implementations and presenting-designs for design documents.
---

# wdyt

Use wdyt in two phases: create each tab without notifying, then present all tab
IDs once. `create` prints one JSON object containing `id`, `url`, `title`, and
`kind`; it does not notify or wait.

## Create and present

Create and present a guided diff:

```sh
guide_id=$(
  git diff | wdyt create guided-diff REVIEW.md --title "parser changes" |
    jq -r '.id'
)
wdyt present "$guide_id" --title "parser changes"
```

`present` sends one notification with one tabbed URL and waits for feedback on
the first session by default. It preserves ID order in a URL such as
`/?s=s1&s=s2&s=s3`. Put the narrative or decision tab first because it owns the
presentation-level reply. Comments always belong to the session tab where the
user left them.

Create supporting tabs only when they add useful material:

```sh
code_id=$(wdyt create code src/lib.rs --title "public API" | jq -r '.id')
docs_id=$(wdyt create docs NOTES.md --title "notes" | jq -r '.id')
report_id=$(wdyt create dir ./report --title "report" | jq -r '.id')
demo_id=$(wdyt create demo 3001 --title "running UI" | jq -r '.id')
wdyt present "$guide_id" "$code_id" "$docs_id" "$report_id" "$demo_id"
```

Creation and presentation are always separate. Use `create` for every content
mode, then make exactly one `present` call for the complete review.

## Guided diff input

A guided diff is Markdown that interleaves prose with selected diff and current
code ranges. Pipe the unified patch to `create guided-diff`:

Do not substitute `create diff --brief` for a guided diff. A brief is one
introduction above a complete raw patch; only `create guided-diff` intersperses
narrative with focused code excerpts.

````md
## Changed behavior

`Client::present` is additive.

```wdyt-diff
{"file":"src/client.rs","side":"new","start":110,"end":145}
```

## Surrounding context

```wdyt-code
{"file":"src/client.rs","start":90,"end":109}
```
````

Directive bodies are strict JSON. Paths are workspace-relative and ranges are
1-based and inclusive. A `wdyt-diff` range selects from the piped patch and
requires `"side":"old"` or `"side":"new"`; a `wdyt-code` range selects from the
current workspace file.

## Feedback loop

Do not pass `--no-wait` or `--timeout` unless the user explicitly asks. Keep the
waiting `wdyt present` process running, preferably as a background task.

After `present` returns the overall reply, collect any line comments and
acknowledge the reply immediately:

```sh
wdyt collect <first-session-id>
wdyt ack <first-session-id> "applying the requested naming change"
```

Line comments start threads on their own tab. Stay in the discussion:

```sh
wdyt recv <session-id>
wdyt say <session-id> <thread-id> "renamed it and updated both callers"
wdyt recv <session-id>
```

Repeat `recv` and `say` while questions continue. Use
`wdyt threads <session-id>` to read the complete discussion without consuming
the next message. A presentation-level reply is terminal and should be
acknowledged with `ack`, not answered with `say`.

## Companion skills

Use `presenting-code-changes` alongside this skill for implementations, patches,
refactors, bug fixes, and UI changes. It defines the guided narrative and
supporting evidence; this skill defines the create, present, and feedback
mechanics.

Use `presenting-designs` alongside this skill for RFCs, architecture proposals,
and implementation plans. It defines the design structure; this skill defines
how to publish it and collect feedback.
