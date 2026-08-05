---
name: presenting-code-changes
description: Structures code-change reviews as a guided narrative with focused supporting tabs. Use alongside wdyt when presenting an implementation, patch, refactor, bug fix, pull request, or UI change for feedback.
---

# Presenting Code Changes

`wdyt` owns the create, present, and feedback mechanics; this skill owns the
review's narrative and evidence. Also apply `presenting-designs` when an RFC,
architecture proposal, or implementation plan is part of the review.

Make a guided diff the primary tab. It should mix concise explanation, selected
diff ranges, and current-code ranges so the reader can follow the change without
cross-referencing a raw patch.

Present changes in this order: carefully explain public API changes, then
explain the tests and cases they cover, and finally explain the detailed logic.

Omit an empty section, but do not reorder the sections. Explain compatibility,
migration, and behavior at the public API before implementation details. For
tests, state the behavior and edge cases each selected test establishes.

Use `wdyt-diff` for changed ranges and `wdyt-code` when surrounding current code
is clearer. Use the directive syntax from `wdyt`.

Use extra tabs for supporting material that interrupts the guided narrative,
such as a full design, benchmark report, generated output, or runnable demo.
Keep the guided diff first.

When UI changes are included, include desktop and mobile screenshots in the
presentation. Put screenshots in a supporting `docs` or `dir` tab, and
check them for clipping, overlap, blank content, and unintended overflow.
