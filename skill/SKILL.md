---
name: wdyt
description: Shows work in one or more review tabs and collects the user's feedback. Use when presenting code, diffs, documents, screenshots, static reports, or live demos for review.
---

# wdyt

Use wdyt in two phases: create each tab without notifying, then present all tab
IDs once. `create` prints one JSON object containing `id`, `url`, `title`, and
`kind`; it does not notify or wait.

## Create and present

Use a guided diff as the primary tab for code changes:

```sh
guide_id=$(
  git diff | wdyt create guided-diff REVIEW.md --title "parser changes" |
    jq -r '.id'
)
design_id=$(wdyt create docs DESIGN.md --title "design" | jq -r '.id')
wdyt present "$guide_id" "$design_id" --title "parser changes"
```

`present` sends one notification with one tabbed URL and waits for feedback on
the first session by default. It preserves ID order in a URL such as
`/?s=s1&s=s2&s=s3`. Put the narrative or decision tab first because it owns the
presentation-level reply. Comments always belong to the session tab where the
user left them.

Create supporting tabs only when they add useful material:

```sh
guide_id=$(
  git diff | wdyt create guided-diff REVIEW.md --title "guided changes" |
    jq -r '.id'
)
docs_id=$(wdyt create docs NOTES.md --title "notes" | jq -r '.id')
report_id=$(wdyt create dir ./report --title "report" | jq -r '.id')
demo_id=$(wdyt create demo 3001 --title "running UI" | jq -r '.id')
wdyt present "$guide_id" "$docs_id" "$report_id" "$demo_id"
```

Creation and presentation are always separate. Use `create` for every content
mode, then make exactly one `present` call for the complete review.

## Guided diff input

A guided diff is Markdown that interleaves prose with selected diff and current
code ranges. Pipe the unified patch to `create guided-diff`:

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

## Feedback loop

Do not pass `--no-wait` or `--timeout` unless the user explicitly asks. Keep the
waiting `wdyt present` process running, preferably as a background task.

After receiving the overall reply, acknowledge it immediately:

```sh
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

For presentation structure, use the `presenting-code-changes` or
`presenting-designs` skill.
