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

Every content command waits by default. This is important. If you are not waiting on the socket, you will not receive the users reply.

You MUST NOT use `--no-wait` unless the user has explicitly given you guidance to use `--no-wait`. This is not negotiable.

You must not use `--timeout` unless the user has explicitly given you guidance to use `--timeout`. This is not negotiable.

If you have a built-in background-task mechanism you SHOULD use it.

```sh
wdyt ack <id> "on it"  # turn the user's receipt green
```

Always `ack` immediately after collecting — it tells the user you actually read
their note.

## Answering their questions (threads)

A line comment is usually a question, not a remark. Once you have the reply,
**stay in the discussion** until they stop asking:

```sh
wdyt watch <id>                          # blocks until they say something
# {"asked": true, "thread": {"id": 4, "file": "src/diff.rs", "line": 97,
#   "snippet": "old_first: old_start,", "text": "why is this a clone?"},
#  "message": {"id": 0, "from": "user", "text": "why is this a clone?"}}

wdyt say <id> 4 "cheap here, and the borrow would leak into the API"
wdyt watch <id>                          # and again, for the follow-up
```

Loop `watch` → `say` until `watch` exits 2 (nothing asked before `--timeout`).
Your answer appears under that line on their page while they are still reading it.

- The `thread` id is the comment id. It comes back from `watch`, from
  `wdyt threads <id>`, and from `wdyt collect <id>`.
- `watch` takes each question once, so two of your processes cannot answer the
  same one. `wdyt threads <id>` reads the whole discussion and takes nothing.
- Answer with what you did, not just what you think: "renamed it, pushed" reads
  better than "good point".
- Sending the answer is what tells them you read the question — taking it only
  turns their dot amber.

## Sessions, the daemon, and one shared port

One long-lived **daemon** serves **every session on a single port** (chosen from
`--ports`, e.g. 3007). Each `wdyt code/diff/docs/…` call just registers **another
session** on that same daemon — you never start a second daemon or pick a new
port per session. The daemon's **root URL** (`http://localhost:<port>/`) lists all
live sessions and is the stable entry point; individual `/s/<id>` links point at
one session.

- **Reuse the running daemon.** Before doing anything drastic, `wdyt list` shows
  the daemon URL and live sessions. Just run another content command to add a
  session — they coexist.
- **Never kill/restart the daemon to "reset" it.** Sessions are persisted and
  restored with the same ids, so restart is not a reset. An active `wdyt wait`
  disconnects and must be rerun after the daemon returns.
- **Don't spawn stray daemons.** Running `wdyt serve` from another checkout or
  worktree starts a *second* daemon on a *different* port, fragmenting sessions.
  Stick to the one installed daemon.
- **Share the root URL when practical.** Session ids survive restart, but an
  exact `/s/<id>` link still assumes the daemon rebinds the same port.

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

Env overrides: `WDYT_WEBHOOK_URL`, `WDYT_PORTS`, `WDYT_THEME`,
`WDYT_PUBLIC_HOST`, `WDYT_STATE_PATH`.
