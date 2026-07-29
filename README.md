# showme

Let coding agents show you their work: a link in Slack, a reply box on the page.

An agent finishes something worth looking at, runs `showme`, and you get a
notification with a link. The page renders whatever it wanted to show you —
highlighted source, a reviewable diff, rendered markdown, a static directory, or
a live server it just started — with a floating reply box in the corner. You type
one reply; the agent gets it and carries on.

## Install

```sh
cargo install --path .
```

## Configure

```sh
showme config --webhook-url https://hooks.slack.com/services/...
showme config --ports 3000-3010      # default; the daemon takes the first free one
showme config                        # print current settings
showme config --path                 # where the file lives
showme themes                        # syntax themes for --theme
```

Ports default to `3000-3010` because those are commonly forwarded from a dev
host already, so `localhost:3000` links work from your laptop unchanged. That
also means the range is already full of other people's servers, so the daemon
takes whichever port in it was free and the CLI finds it by scanning — a plain
server that answers `/health` is not mistaken for one, since the daemon's
response carries an identifying marker.

Environment overrides for one-off runs: `SHOWME_WEBHOOK_URL`, `SHOWME_PORTS`,
`SHOWME_THEME`, `SHOWME_PUBLIC_HOST`.

With no webhook configured, showme prints the link instead of sending it, so it
is usable before it is set up.

## The modes

```sh
# Source files, syntax highlighted, with line anchors
showme code src/lib.rs src/main.rs --title "the new parser"

# A unified diff, reviewable line by line with comments
git diff | showme diff --title "the parser rewrite"

# Markdown, rendered (GFM: tables, tasklists, footnotes, > [!NOTE] callouts)
showme docs DESIGN.md --title "design writeup"

# A directory of prepared HTML, served with its relative assets
showme dir ./report --title "benchmark report"

# A live server the agent started itself
PORT=$(showme port)
my-app --port "$PORT" &
showme demo "$PORT" --title "the new dashboard"
```

Each prints the session URL on stdout, so the agent can quote it back to you
even when a notification went out.

## Getting the reply

Add `--wait` to block until you reply, then print it as JSON:

```sh
showme code src/lib.rs --title "the new parser" --wait
# {"text": "rename the second arg and ship it", "at": 1785287644}
```

Exit status is `2` if the wait times out (`--timeout`, default 600s), so a
script can tell "no answer" from "answered". To send now and collect later:

```sh
URL=$(showme docs PLAN.md --title "the plan")
showme wait "${URL##*/}" --timeout 900
```

One reply per session, deliberately — it is a note back, not a conversation.
A second POST is refused and the page says so.

**A reply outlives the wait.** If `--wait` gives up, the session stays open and
the daemon keeps holding the reply, so a turn hours later can still pick it up:

```sh
showme inbox              # replies waiting, as JSON, newest first
showme inbox --all        # including ones already collected
showme collect <id>       # take one reply and its line comments
```

This is what makes a reply sent a day after the agent stopped waiting still land.

Collecting marks the reply read, and the page you replied on says so: the reply
panel carries a dot that pulses while the reply is still unread and turns green
with "The agent has read this" once collected. It polls a read-only status route,
so watching the page never marks your own reply read.

## Reviewing a diff

`showme diff` renders a unified diff — added, removed, and context lines tinted
and numbered on both sides, highlighted per language. Each hunk side is
reassembled and highlighted as one document before being split back into lines,
so a multi-line string or comment colours correctly instead of restarting at
every line.

Hover a line and a `+` appears in the gutter; click it for a comment box.
`⌘/Ctrl + Enter` saves, `Escape` cancels, and several comments can stack on one
line. Comments survive a reload and come back to the agent alongside the reply:

```sh
showme collect <id>
# {"replied": true, "reply": {...}, "comments": [
#   {"file": "src/lib.rs", "line": 2, "side": "new",
#    "snippet": "    let x = 2;", "text": "why 2?"}]}
```

The quoted `snippet` is read from the daemon's own copy of the diff rather than
taken from the request, so the page is never trusted to say what a line said. A
context line is addressable only by its new-side number, so the same line cannot
collect two independent comment threads.

## Hiding the chrome

Every session page has a **Hide bar** control, and `.` toggles it from the
keyboard. It collapses the header and file nav to a small pill in the corner, and
the choice is remembered in `localStorage` — so a framed demo can be reviewed at
the full height of the window without showme's own furniture in the way, and a
long diff gets the whole viewport.

## Other commands

```sh
showme port          # a free port for a demo server (never the daemon's)
showme port --all    # every free port in the range
showme list          # URL of the session index
showme serve         # run the daemon in the foreground
```

Any session page also serves `/s/<id>/raw` — the plain source, or for a diff the
reconstructed patch.

The daemon starts automatically on first use and holds sessions in memory, so
restarting it drops them. Sessions expire after `session_ttl_hours` (24 by
default; set it to nothing to keep them for the daemon's lifetime).

## The reply box

It is a `position: fixed` overlay pinned to the bottom-right: collapsed it is a
single pill, and opening it paints over the content rather than reflowing it.
That matters most in `demo` and `dir` mode, where you are reviewing carefully
laid-out UI inside an iframe and a box that participated in the layout could
misrepresent it.

`⌘/Ctrl + Enter` sends, `Escape` closes. Once sent, the panel shows what you
sent and can still be reopened on a later visit.

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
