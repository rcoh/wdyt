---
name: presenting-code-changes
description: Structures code-change reviews as a guided narrative with focused supporting tabs. Use when presenting an implementation, patch, refactor, bug fix, pull request, or UI change for feedback.
---

# Presenting Code Changes

Make a guided diff the primary tab. It should mix concise explanation, selected
diff ranges, and current-code ranges so the reader can follow the change without
cross-referencing a raw patch.

Present changes in this order: Carefully explain any changes to the public API. Next explain the tests and what cases they consider. Finally explain the detailed logic

Omit an empty section, but do not reorder the sections. Explain compatibility,
migration, and behavior at the public API before implementation details. For
tests, state the behavior and edge cases each selected test establishes.

Use `wdyt-diff` for changed ranges and `wdyt-code` when surrounding current code
is clearer:

````md
## Public API

`Client::present` adds grouped reviews without changing one-shot commands.

```wdyt-diff
{"file":"src/client.rs","side":"new","start":110,"end":145}
```

## Tests

These cases preserve tab order and reject unknown session IDs.

```wdyt-code
{"file":"tests/present.rs","start":20,"end":78}
```

## Detailed logic

The URL builder validates IDs before adding repeated query parameters.

```wdyt-diff
{"file":"src/main.rs","side":"new","start":420,"end":475}
```
````

Create the guide, then present it:

```sh
guide_id=$(
  git diff | wdyt create guided-diff REVIEW.md --title "grouped reviews" |
    jq -r '.id'
)
wdyt present "$guide_id" --title "grouped reviews"
```

Use extra tabs for supporting material that interrupts the guided narrative,
such as a full design, benchmark report, generated output, or runnable demo.
Keep the guided diff first.

When UI changes are included, include desktop and mobile screenshots in the
presentation. Put screenshots in a supporting docs or static-report tab, and
check them for clipping, overlap, blank content, and unintended overflow:

```sh
screens_id=$(wdyt create dir ./ui-review --title "UI screenshots" | jq -r '.id')
wdyt present "$guide_id" "$screens_id" --title "grouped review UI"
```
