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

## Create and present

```sh
# Create tabs without notifying or waiting
code=$(wdyt create code src/lib.rs src/main.rs --title "public API")
docs=$(wdyt create docs DESIGN.md --title "design")

# Present them in one ordered, tabbed review
wdyt present "$(printf '%s' "$code" | jq -r .id)" \
  "$(printf '%s' "$docs" | jq -r .id)" \
  --title "parser changes"
```

`create` prints one JSON object with `id`, `url`, `title`, and `kind`. It never
sends a notification and never waits. `present` sends one notification for a
URL such as `/?s=s1&s=s2&s=s3`, then waits for the overall reply by default.
Tabs preserve the supplied order; duplicate IDs are removed. The first tab owns
the overall reply, while line comments remain attached to the tab where they
were written.

All content modes are created the same way:

```sh
wdyt create code src/lib.rs                         # highlighted source
git diff | wdyt create diff                         # unified diff
wdyt create docs DESIGN.md                          # rendered Markdown
wdyt create dir ./report                            # prepared HTML/assets
PORT=$(wdyt port)
my-app --port "$PORT" &
wdyt create demo "$PORT" --title "running dashboard" # live server
```

The no-query home page lists active sessions and can compose selected sessions
into the same tabbed view. The `wdyt` label on every review links back there.
Archive controls remove finished tabs from the active list without deleting
their content or direct URLs; archived tabs can be restored from the collapsed
archive on the home page.

## Getting the reply

`present` prints the grouped URL, then blocks until the first session receives
an overall reply:

```sh
created=$(wdyt create code src/lib.rs --title "the new parser")
id=$(printf '%s' "$created" | jq -r .id)
wdyt present "$id"
# prints: http://localhost:3000/?s=<id>, then blocks
# {"text": "rename the second arg and ship it", "at": 1785287644}
```

By default the wait sits open until a reply arrives. Pass `--timeout <secs>` to
give up after a bound, in which case exit status is `2` on timeout so a script
can tell "no answer" from "answered". To send now and collect later:

```sh
created=$(wdyt create docs PLAN.md --title "the plan")
id=$(printf '%s' "$created" | jq -r .id)
wdyt present "$id" --no-wait
wdyt recv "<id>" --timeout 900
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

The CLI's stdout is strictly machine-readable: `create` emits one JSON object;
`present` emits the grouped URL and then, when waiting, the reply JSON. All
coaching goes to stderr, so piping stdout never captures noise.

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

Presentation stderr guidance varies by outcome:

| Scenario | Stderr says |
|---|---|
| `create` | Session created without notification or waiting |
| `present --no-wait` | Presentation ready; shows `collect` command |
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

`wdyt create diff` renders a unified diff — added, removed, and context lines tinted
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
wdyt recv <id>           # blocks until the reader says something, then prints it
# {"kind": "comment",
#  "thread": {"id": 4, "file": "src/diff.rs", "line": 97,
#             "snippet": "    old_first: old_start,", "text": "why is this a clone?"},
#  "message": {"id": 0, "from": "user", "text": "why is this a clone?"}}

wdyt say <id> 4 "cheap here, and the borrow would leak into the API"
wdyt recv <id>           # again, for the follow-up
```

`recv` returns either kind of event: `"kind": "comment"` for a line comment or
follow-up, and `"kind": "reply"` for the overall verdict, which is terminal. It
exits 2 when nothing arrived before `--timeout`, so the loop ends on its own.
`wdyt threads <id>` prints every thread and everything said in it.

The whole thread comes with the question — the file, the line, the quoted
snippet, and the exchange so far — because an answer is about the code, not about
the sentence.

**Each question is taken once.** Two agent processes watching the same session
cannot answer the same question, and `recv` never returns a question twice.
Reading (`wdyt threads`, and the page's own polling) takes nothing, so watching
for an answer never marks the reader's own question as picked up.

**Who is waiting on whom** is the last word in the thread, shown with the same
three states as the session's receipt: grey while nobody has the question, amber
once an agent has taken it, and gone once the answer is there — the answer being
its own acknowledgement. The page polls a read-only route every three seconds
while the tab is visible, so an answer arrives without a reload.

The reply box for a thread floats over the listing like the comment editor does,
for the same measured reason. A thread holds at most 100 messages.

## Guided diff

Guided diff is the default way to present code changes. It is a Markdown
narrative that interleaves prose, selected patch ranges, and selected current
code ranges in one ordered view:

````markdown
## Public API

`Client::present` adds grouped reviews.

```wdyt-diff
{"file":"src/client.rs","side":"new","start":110,"end":145}
```

## Tests

These cases cover stable order, duplicate IDs, and unknown sessions.

```wdyt-code
{"file":"tests/present.rs","start":20,"end":78}
```

## Detailed logic

The grouped URL repeats the `s` query parameter.
````

Create it with a patch on stdin or with `--patch <PATH>`:

```sh
guide=$(git diff | wdyt create guided-diff REVIEW.md --title "grouped reviews")
wdyt present "$(printf '%s' "$guide" | jq -r .id)"
```

Each directive is strict JSON. Paths are workspace-relative, ranges are
1-based and inclusive, and one range may select at most 500 lines. Diff excerpts
include both sides of replacements and retain validated current-file context,
which the reader can expand above or below the selected lines.

For simpler code, diff, and docs sessions, `--brief` and `--brief-file` still
add a Markdown introduction. File and line links use the printed `#f-<path>`
anchors.

## Colour schemes

`wdyt themes` lists them; `--theme` picks one for a single session, and
`wdyt config --theme` sets the lasting default:

```sh
git diff | wdyt create diff --theme GitHub
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
comments, discussions, and acknowledgement state — a `wdyt recv` has to be rerun
after a restart, but the questions it had not taken yet are still there. Sessions expire after `session_ttl_hours`
(24 by default; set it to nothing to keep them indefinitely).

The state file is an atomically replaced, versioned JSON snapshot. An active
`wdyt recv` disconnects during a restart and must be rerun, but it reconnects to
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
