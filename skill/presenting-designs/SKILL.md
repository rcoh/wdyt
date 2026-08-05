---
name: presenting-designs
description: Writes and presents feature designs using the Smithy-RS RFC three-part structure. Use alongside wdyt when drafting or reviewing an RFC, design document, architecture proposal, implementation plan, or substantial code change.
---

# Presenting Designs

`wdyt` owns the create, present, and feedback mechanics; this skill owns the
document structure. Also apply `presenting-code-changes` when implementation
changes accompany the design.

Write designs in three parts, in this order:

1. **User-facing documentation for the feature.** Write the documentation a
   user would read after release: concepts, behavior, examples, constraints, and
   error cases. This makes the intended experience concrete before internals.
2. **Reference guide and implementation details.** Define contracts, data
   models, invariants, alternatives, compatibility, operational concerns, and
   the implementation approach.
3. **Checklist of concrete changes.** List the code, tests, documentation,
   migration, rollout, and verification work needed to complete the design.

Use headings that make the three parts explicit. Keep unresolved decisions next
to the part they affect rather than hiding them in a general notes section.

This three-phase structure is also useful for many code changes: desired
behavior first, technical reference second, and an auditable implementation
checklist last.

Present the rendered design as the primary `wdyt` tab. If implementation
changes accompany it, put the guided diff first and the design in a supporting
tab.

When the design changes UI, include desktop and mobile screenshots in another
tab so the claimed user-facing behavior can be inspected.
