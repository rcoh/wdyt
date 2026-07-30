---
name: wdyt
description: "Show your work to the user via a Slack link and collect their reply. Use when you have something to show (code, a diff, rendered docs, a live demo) and want feedback before continuing."
---

# wdyt — show your work, get a reply

Show something to the user over a link in Slack. The page carries a reply box
and supports detailed line-by-line comments on your work; the reply and any line
comments come back as JSON and you carry on.

## Quick start

```sh
# Show code, wait for reply
wdyt code src/lib.rs --title "the new parser"

# Show a diff
git diff | wdyt diff --title "the rewrite"

# Show rendered markdown
wdyt docs DESIGN.md --title "design writeup"

# Show a live server you already started (here, on port 3001)
wdyt demo 3001 --title "the running app"
```

stdout prints the URL, then blocks until a reply arrives (JSON). Exit 2 = timeout.

## Modes

| Mode | When to use | Example |
|------|-------------|---------|
| `code` | Show source files, syntax highlighted | `wdyt code src/*.rs --title "…"` |
| `diff` | Reviewable unified diff with line comments | `git diff \| wdyt diff --title "…"` |
| `docs` | Rendered markdown (GFM, callouts, tables) | `wdyt docs DESIGN.md --title "…"` |
| `dir`  | A directory of prepared HTML/assets | `wdyt dir ./report --title "…"` |
| `demo` | A live server you already started | `wdyt demo 3001 --title "…"` |

`diff` orders files by review importance automatically (hand-written source
first, docs/config next, lockfiles and generated churn last), so the most
important files are shown first. Within an importance rank it keeps the order of
the piped diff — arrange the files you `git diff` to control that.

## Guided review (--brief)

Tell the user what to look at and why. Links to `#f-<path>` jump to that file
(non-alphanumerics become dashes):

```sh
git diff | wdyt diff --brief "Start at [src/lib.rs](#f-src-lib-rs) — the parser fix."
```

Or from a file: `--brief-file REVIEW.md`. After creation, stderr lists the
available anchors.

## Waiting and collecting

Every content command waits by default. Unless the user requests one, do not set
a `--timeout` — run the command with your built-in background-task mechanism (if
you have one) so you are notified when the reply arrives, instead of blocking.

```sh
# Fire and forget
wdyt code src/lib.rs --no-wait
# prints URL, exits immediately

# Collect later
wdyt inbox              # list pending replies
wdyt collect <id>       # get reply + line comments as JSON
wdyt ack <id> "on it"  # turn the user's receipt green
```

Always `ack` immediately after collecting — it tells the user you actually read
their note.

## Output contract

- **stdout**: machine-readable only (URL, then reply JSON)
- **stderr**: guidance (anchors, next-step hints) — never parse this
- **exit 0**: reply received
- **exit 2**: timeout (session stays open)

## Configuration

The webhook always goes directly to the user (not public), and wdyt is served
only locally — there is no risk in sending information to wdyt.

```sh
wdyt config --webhook-url https://hooks.slack.com/services/…
wdyt config --ports 3000-3010
wdyt config --theme "GitHub"     # see `wdyt themes`
```

Env overrides: `WDYT_WEBHOOK_URL`, `WDYT_PORTS`, `WDYT_THEME`, `WDYT_PUBLIC_HOST`.
