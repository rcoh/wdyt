# Task 5: Integration, screenshots, and release documentation

## Goal

Verify the complete workflow as a user sees it and document the public API.

## Work

- Add CLI integration tests for `create guided-diff` and grouped `present`.
- Add page tests for guided block order and line-number preservation.
- Run formatting, unit tests, integration tests, Clippy, and package checks.
- Start the development daemon without replacing any already-running shared
  daemon.
- Build a real grouped presentation containing:
  - a guided diff tab,
  - a documentation or design tab,
  - a UI screenshot tab.
- Capture desktop and mobile screenshots of the tabbed view and include them in
  the final UI-change presentation.
- Check screenshots for blank frames, clipped text, overlapping controls,
  unstable tab dimensions, and mobile overflow.
- Update `README.md`, CLI help, and bundled installer tests.

## Write Scope

This is the final integration task and may touch documentation, tests, and small
fixes in implementation files discovered during verification.

## Acceptance

- All automated checks pass.
- Screenshots show the actual tabbed and guided-diff UI at desktop and mobile
  sizes.
- The final review is sent through wdyt as one multi-tab presentation.
- The final guided diff follows public API, tests, then detailed logic order.
