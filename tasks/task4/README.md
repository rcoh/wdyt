# Task 4: Presentation skills

## Goal

Draft concise, installable skills that separate wdyt mechanics from the judgment
used to present code changes and designs.

## Deliverables

1. Update `skill/SKILL.md` to teach create-then-present, multiple tabs, guided
   diff creation, waiting, acknowledgement, and threads.
2. Add `skill/presenting-code-changes/SKILL.md`.
3. Add `skill/presenting-designs/SKILL.md`.

The main agent will update the installer after reviewing the drafts.

## Required Code-Change Guidance

State this sentence verbatim:

> Present changes in this order: Carefully explain any changes to the public API. Next explain the tests and what cases they consider. Finally explain the detailed logic

Make guided diff the default presentation for code changes. Use extra tabs for
supporting material that does not belong in the guided narrative. When UI
changes are included, require screenshots in the presentation.

## Required Design Guidance

Use the Smithy-RS RFC three-part structure:

1. User-facing documentation for the feature.
2. Reference guide and implementation details.
3. Checklist of concrete changes.

Explain that this three-phase structure is also useful for many code changes.

## Write Scope

- `skill/SKILL.md`
- `skill/presenting-code-changes/SKILL.md`
- `skill/presenting-designs/SKILL.md`

Do not edit Rust, tests, README, or generated/local installed skill copies.

## Acceptance

- Each skill description includes specific trigger language.
- Each new skill stays under 100 lines.
- Examples use the new public CLI.
- No example uses `--no-wait` or `--timeout` as the default flow.
- The code-change skill contains the required sentence exactly.
