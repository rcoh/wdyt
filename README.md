# wdyt

Let coding agents show you their work: a link in Slack, a reply box on the page.

An agent finishes something worth looking at, runs `wdyt`, and you get a
notification with a link. The page renders whatever it wanted to show you —
highlighted source, a reviewable diff, rendered markdown, a static directory, or
a live server it just started — with a floating reply box in the corner. You type
one reply; the agent gets it and carries on.

## Install

```sh
cargo install --locked wdyt
```

Or from a checkout: `cargo install --locked --path .`

## Configure

```sh
wdyt config --webhook-url https://hooks.slack.com/services/...
wdyt config --ports 3000-3010      # default; the daemon takes the first free one
wdyt config                        # print current settings
wdyt config --path                 # where the file lives
wdyt themes                        # syntax themes for --theme
```

Ports default to `3000-3010` because those are commonly forwarded from a dev
host already, so `localhost:3000` links work from your laptop unchanged. That
also means the range is already full of other people's servers, so the daemon
takes whichever port in it was free and the CLI finds it by scanning — a plain
server that answers `/health` is not mistaken for one, since the daemon's
response carries an identifying marker.

Environment overrides for one-off runs: `WDYT_WEBHOOK_URL`, `WDYT_PORTS`,
`WDYT_THEME`, `WDYT_PUBLIC_HOST`.

With no webhook configured, wdyt prints the link instead of sending it, so it
is usable before it is set up.

## The modes

```sh
# Source files, syntax highlighted, with line anchors
wdyt code src/lib.rs src/main.rs --title "the new parser"

# A unified diff, reviewable line by line with comments
git diff | wdyt diff --title "the parser rewrite"

# Markdown, rendered (GFM: tables, tasklists, footnotes, > [!NOTE] callouts)
wdyt docs DESIGN.md --title "design writeup"

# A directory of prepared HTML, served with its relative assets
wdyt dir ./report --title "benchmark report"

# A live server the agent started itself
PORT=$(wdyt port)
my-app --port "$PORT" &
wdyt demo "$PORT" --title "the new dashboard"
```

Every content command waits for a reply by default and prints it as JSON after
the session URL. Pass `--no-wait` for fire-and-forget (the URL is still printed
on stdout so the agent can quote it).

## Getting the reply

Every content command blocks until a reply arrives (or `--timeout` expires), then
prints the reply as JSON:

```sh
wdyt code src/lib.rs --title "the new parser"
# prints: http://localhost:3000/s/<id>
# then blocks…
# {"text": "rename the second arg and ship it", "at": 1785287644}
```

By default the wait sits open until a reply arrives. Pass `--timeout <secs>` to
give up after a bound, in which case exit status is `2` on timeout so a script
can tell "no answer" from "answered". To send now and collect later:

```sh
wdyt docs PLAN.md --title "the plan" --no-wait
# prints: http://localhost:3000/s/<id>
wdyt wait "<id>" --timeout 900
```

One reply per session, deliberately — the reply is the verdict on the whole
thing, and a second POST is refused. Anything that needs going back and forth
happens on the lines instead: see [threaded
discussions](#discussing-a-line-with-the-agent).

**A reply outlives the wait.** If a `--timeout` expires, the session stays open and
the daemon keeps holding the reply, so a turn hours later can still pick it up:

```sh
wdyt inbox              # replies waiting, as JSON, newest first
wdyt inbox --all        # including ones already collected
wdyt collect <id>       # block until the reply lands, then take it + line comments
wdyt collect <id> --no-wait   # take whatever is there right now, don't block
```

This is what makes a reply sent a day after the agent stopped waiting still land.

## CLI output as agent skill

The CLI's stdout is strictly machine-readable (the session URL, then optionally
the reply JSON). All coaching — file anchors, next-step guidance — goes to stderr,
so piping stdout into another tool or variable never captures noise.

After creating a code, diff, or docs session, stderr prints the available file
anchors for use in `--brief` guided reviews:

```
wdyt: file anchors for --brief links:
  src/lib.rs → #f-src-lib-rs
  src/main.rs → #f-src-main-rs
  add -L<start> or -L<start>-L<end> for a line span, e.g. #f-src-lib-rs-L10 or #f-src-lib-rs-L10-L20
```

These use the exact same helper that renders the page's navigation anchors, so
the links always match. The anchor rule: prefix `f-`, then replace every
non-alphanumeric character with `-` and lowercase letters.

Post-session stderr guidance varies by outcome:

| Scenario | Stderr says |
|---|---|
| `--no-wait` | Session created; shows `collect` command |
| Reply received | Shows `collect <id>` (for line comments) and `ack <id>` |
| Timeout (exit 2) | Session still open; shows `collect` and `inbox` |

The `--help` for the root command and each subcommand is written as a concise
skill reference for agents, including examples and the stdin caveat for `--brief`.

## Did the agent actually read it?

Delivery is not reading. An agent can collect a reply and then die — needing
credentials, hitting a rate limit — and a receipt that lit up on delivery would
call that success. So the page shows three states, not two:

| Dot | Means |
|---|---|
| Grey, pulsing | Waiting for an agent to pick this up |
| Amber, pulsing | Picked up by an agent, not yet acknowledged |
| Green | The agent read this, and here is what it said |

Only an explicit ack reaches green, and it carries the agent's own words:

```sh
wdyt ack <id> "rerunning the build with your flag"
```

The note is required — a bare flag could be sent by the same transport that
merely moved the bytes, so the signal is the sentence. `wdyt inbox` shows
unread replies by default (filtering on the ack rather than on delivery), which
means a reply some dead process collected stays in the inbox instead of vanishing
from it. Pass `--all` to include already-acknowledged replies.

The page polls a read-only status route, so watching it never advances your own
receipt.

## Reviewing a diff

`wdyt diff` renders a unified diff — added, removed, and context lines tinted
and numbered on both sides, highlighted per language. Each hunk side is
reassembled and highlighted as one document before being split back into lines,
so a multi-line string or comment colours correctly instead of restarting at
every line.

Hover a line and a `+` appears in the gutter; click it for a comment box, or
**drag from one `+` to another to comment on a range**. The covered lines are
marked with a bar down the gutter — not a background tint, which would read as a
deleted line — and the note is labelled with the span it covers. `⌘/Ctrl + Enter`
saves, `Escape` cancels, and several comments can stack on one line. Comments
survive a reload and come back to the agent alongside the reply:

The editor floats over the lines below rather than pushing them down. A box that
sits *in* the diff is a row in the table holding the whole file, and then every
character typed costs a relayout and repaint of all of it — 40ms a keystroke on a
3,600-line diff, measured, against 7-11ms for the same box placed over it. A
saved comment does sit in the flow, under the line it is about, since that costs
one insertion rather than one per character.

```sh
wdyt collect <id>
# {"replied": true, "reply": {...}, "comments": [
#   {"file": "src/lib.rs", "line": 2, "side": "new",
#    "snippet": "    let x = 2;", "text": "why 2?"},
#   {"file": "src/lib.rs", "line": 5, "end_line": 9, "side": "new",
#    "snippet": "fn f() {\n    ...\n}", "text": "this whole block"}]}
```

`end_line` is present only for a range, and the snippet then quotes every line it
covers.

The quoted `snippet` is read from the daemon's own copy of the diff rather than
taken from the request, so the page is never trusted to say what a line said. A
context line is addressable only by its new-side number, so the same line cannot
collect two independent comment threads.

## Seeing what the patch left out

A unified diff shows three lines around each change, which is fine until a hunk
stops making sense on its own — and then you are opening the file in another
window, which is where the review stops. So wdyt reads the files while it is
still standing in the tree they came from, and the space between hunks becomes a
band you can open:

```
↓ 20   ⋯ 93 lines   ↑ 20    fn parse_file(lines: &[&str], theme: &Theme)
```

`↓` takes twenty lines off the top of the run, `↑` twenty off the bottom, and the
middle button takes the lot. What is left stays as a band between the two
revealed sides; when nothing is left the band goes and the code reads
continuously. The band replaces the `@@` header, keeping the section heading it
carried — the ranges it also carried are what the gutter already shows.

Revealed lines are ordinary context lines: numbered on both sides, highlighted by
the same pass, and commentable. The snippet for a comment on one is quoted from
the daemon's copy of the file, like any other.

**A patch about another revision has nothing to expand.** The file is kept only
if it agrees with every line the diff already shows, so `git diff HEAD~5 HEAD`,
or a `.patch` from elsewhere, simply gets no bands rather than context from a
file that is not the one it describes. Files above 4 MB are skipped, and one
click reveals at most a thousand lines.

## Discussing a line with the agent

A comment on a line is usually a question — "why is this a clone?" is not a
remark — and a question that gets answered tomorrow in a `collect` is not much of
a question. So a comment is the first message in a thread, and the agent can
answer it while the page is still open:

```
┌─ YOU ───────────────────────────────────────┐
│ why is this a clone?                        │
│ ┌─ AGENT ─────────────────────────────────┐ │
│ │ cheap here, and the borrow would leak   │ │
│ │ into the API                            │ │
│ └─────────────────────────────────────────┘ │
│ ● picked up by an agent            [Reply]  │
└─────────────────────────────────────────────┘
```

The agent's side is a loop of two commands:

```sh
wdyt watch <id>          # blocks until the reader says something, then prints it
# {"asked": true,
#  "thread": {"id": 4, "file": "src/diff.rs", "line": 97,
#             "snippet": "    old_first: old_start,", "text": "why is this a clone?"},
#  "message": {"id": 0, "from": "user", "text": "why is this a clone?"}}

wdyt say <id> 4 "cheap here, and the borrow would leak into the API"
wdyt watch <id>          # again, for the follow-up
```

`watch` exits 2 when nothing was asked before `--timeout`, so the loop ends on
its own. `wdyt threads <id>` prints every thread and everything said in it.

The whole thread comes with the question — the file, the line, the quoted
snippet, and the exchange so far — because an answer is about the code, not about
the sentence.

**Each question is taken once.** Two agent processes watching the same session
cannot answer the same question, and `watch` never returns a question twice.
Reading (`wdyt threads`, and the page's own polling) takes nothing, so watching
for an answer never marks the reader's own question as picked up.

**Who is waiting on whom** is the last word in the thread, shown with the same
three states as the session's receipt: grey while nobody has the question, amber
once an agent has taken it, and gone once the answer is there — the answer being
its own acknowledgement. The page polls a read-only route every three seconds
while the tab is visible, so an answer arrives without a reload.

The reply box for a thread floats over the listing like the comment editor does,
for the same measured reason. A thread holds at most 100 messages.

## Guided review

Rather than dropping a diff on someone and hoping, the agent can write the tour:
what changed, why, and what to look at first. `--brief` takes markdown, or
`--brief-file` a path:

```sh
git diff | wdyt diff --brief-file REVIEW.md
wdyt code src/*.rs --brief "Start with the parser; the rest is mechanical."
```

It renders above the content in the same column as the code, with GFM callouts
and highlighted fences. A link to `#f-<path>` jumps to that file, with
non-alphanumerics replaced by dashes — `src/diff.rs` is `#f-src-diff-rs`. To
point at a line span, append `-L<start>` or `-L<start>-L<end>`, mirroring
GitHub's `#L10` / `#L10-L20` blob URLs; wdyt scrolls to and highlights the span
(in `code` and `diff` modes, on the new side, which have real line numbers — in
`docs` it jumps to the rendered block covering those source lines):

```markdown
> [!NOTE]
> The parser bug is the interesting one.

- [src/diff.rs](#f-src-diff-rs) — the hunk-length fix
- [the off-by-one](#f-src-diff-rs-L42) — the exact line
- [the new guard](#f-src-diff-rs-L80-L95) — the whole block
- [README.md](#f-readme-md) — docs only
```

`--brief -` reads from stdin, so it cannot be combined with a diff that is also
piped in; wdyt says so rather than silently eating one of them.

## Colour schemes

`wdyt themes` lists them; `--theme` picks one for a single session, and
`wdyt config --theme` sets the lasting default:

```sh
git diff | wdyt diff --theme GitHub
wdyt config --theme "Solarized (light)"
```

The theme travels with the session, so the page's own colours follow the code:
a light theme gives a light card rather than light code on a dark one. Light
options that sit well with the warm off-white chrome are `GitHub`,
`OneHalfLight`, `Catppuccin Latte`, and `Solarized (light)`; the default `Nord`
is dark.

## Which agent is asking

With several agents running there is otherwise nothing to say which one sent a
link. Each session records where it was created — the working directory, and
under zellij the session and tab name — and shows it in the bar and in the
notification: `dd-2 › wdyt cli — /local/home/rcoh/code/wdyt`. Hovering gives
the pane title, which for an agent is usually the task it was handed.

The tab comes from the pane id rather than `zellij action current-tab-info`,
which reports whichever tab *you* are looking at and so names the wrong one
whenever the agent is in the background.

## Hiding the chrome

Every session page has a **Hide bar** control, and `.` toggles it from the
keyboard. It collapses the header and file nav to a small pill in the corner, and
the choice is remembered in `localStorage` — so a framed demo can be reviewed at
the full height of the window without wdyt's own furniture in the way, and a
long diff gets the whole viewport.

## Other commands

```sh
wdyt port          # a free port for a demo server (never the daemon's)
wdyt port --all    # every free port in the range
wdyt list          # URL of the session index
wdyt threads <id>  # every discussion in a session, and everything said in it
wdyt serve         # run the daemon in the foreground
```

Any session page also serves `/s/<id>/raw` — the plain source, or for a diff the
reconstructed patch.

The daemon starts automatically on first use and persists live sessions to
`$XDG_STATE_HOME/wdyt/sessions.json` (normally
`~/.local/state/wdyt/sessions.json`). Restarting restores their content, replies,
comments, discussions, and acknowledgement state — a `wdyt watch` has to be rerun
after a restart, but the questions it had not taken yet are still there. Sessions expire after `session_ttl_hours`
(24 by default; set it to nothing to keep them indefinitely).

The state file is an atomically replaced, versioned JSON snapshot. An active
`wdyt wait` disconnects during a restart and must be rerun, but it reconnects to
the restored session id. Set `WDYT_STATE_PATH` to use a different file, such as
`WDYT_STATE_PATH=/tmp/wdyt-test/sessions.json` for an isolated test daemon.
A sibling `.lock` file prevents two daemons from writing the same snapshot.

## The reply box

It is a `position: fixed` overlay pinned to the bottom-right: collapsed it is a
single pill, and opening it paints over the content rather than reflowing it.
That matters most in `demo` and `dir` mode, where you are reviewing carefully
laid-out UI inside an iframe and a box that participated in the layout could
misrepresent it.

`⌘/Ctrl + Enter` sends, `Escape` closes. Once sent, the panel shows what you
sent and can still be reopened on a later visit.

## Tests

```sh
cargo nextest run     # preferred
cargo test            # also works
```

[nextest](https://nexte.st) is preferred because the integration tests each start
a real daemon on a real port: a process per test keeps one test's daemon, port,
and store out of another's, and a flake gets reported rather than hidden. Ports
come from a window keyed by process id *and* a counter — the counter alone
collides under nextest, where each test process restarts it at zero.

## Notes

Loopback-only by default (`bind = "127.0.0.1"`): the ports are reached through
SSH forwarding, so there is no reason to accept traffic from the network. Static
assets are served through `tower-http`'s `ServeDir`, which refuses paths that
escape the session root.

Built on [topcoat](https://github.com/tokio-rs/topcoat) for server-rendered
HTML, [syntect](https://github.com/trishume/syntect) via
[two-face](https://github.com/nickelc/two-face) for highlighting, and
[comrak](https://github.com/kivikakk/comrak) for markdown.

## License

MIT OR Apache-2.0
