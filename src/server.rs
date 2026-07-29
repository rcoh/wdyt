//! The daemon: routes, pages, and the reply endpoint.

use std::net::SocketAddr;

use anyhow::{Context as _, Result as AnyResult};
use serde::{Deserialize, Serialize};
use topcoat::Result;
use topcoat::context::{Cx, app_context};
use topcoat::router::content::Json;
use topcoat::router::error::{bad_request, not_found};
use topcoat::router::{Router, page, path_param, route};
use topcoat::view::view;

use crate::config::Config;
use crate::render::{ThemeColors, theme, theme_colors};
use crate::session::{Anchor, Comment, Content, Reply, Side, Store};
use crate::ui::{Raw, diff_hunk, shell};

/// Values shared by every request.
struct State {
    store: Store,
    colors: ThemeColors,
    /// The daemon's configured theme, used when a session names none.
    theme: String,
}

#[path_param(error = not_found)]
struct SessionId(String);

/// Builds the router.
fn router(store: Store, colors: ThemeColors, theme: String) -> Router {
    Router::builder()
        .app_context(State {
            store,
            colors,
            theme,
        })
        .page(session_page)
        .page(index_page)
        .route(reply_route)
        .route(raw_route)
        .route(asset_route)
        .route(health_route)
        .route(create_route)
        .route(await_reply_route)
        .route(status_route)
        .route(list_comments_route)
        .route(comment_route)
        .route(inbox_route)
        .route(collect_route)
        .route(ack_route)
        .build()
}

/// The control API the CLI uses to create a session in the running daemon.
#[derive(Serialize, Deserialize)]
pub struct CreateSession {
    pub title: String,
    pub note: Option<String>,
    pub content: Content,
    /// Where the creating agent is running. Sent by the CLI because the daemon
    /// cannot see it: the daemon's own cwd and pane are its own, not the agent's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<crate::zellij_origin::Origin>,
    /// The theme the content was highlighted with, when not the daemon's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    /// A guided tour of the work as rendered HTML. Rendered by the CLI, which is
    /// where the theme for fenced code blocks is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<String>,
}

impl CreateSession {
    /// The two things a session cannot do without; the rest is optional context.
    pub fn new(title: String, content: Content) -> Self {
        Self {
            title,
            content,
            note: None,
            origin: None,
            theme: None,
            brief: None,
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct CreatedSession {
    pub id: String,
}

#[route(POST "/api/sessions")]
async fn create_route(cx: &Cx, Json(input): Json<CreateSession>) -> Result<Json<CreatedSession>> {
    let id = state(cx).store.insert_new(crate::session::NewSession {
        note: input.note,
        // The daemon is long-lived and shared, so it cannot detect this itself:
        // its own cwd and pane are wherever it happened to be started.
        origin: input.origin.unwrap_or_default(),
        theme: input.theme,
        brief: input.brief,
        ..crate::session::NewSession::new(input.title, input.content)
    });
    Ok(Json(CreatedSession { id }))
}

#[derive(Serialize)]
pub struct AwaitedReply {
    pub replied: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply: Option<Reply>,
}

/// Long-polls for a session's reply.
///
/// `timeout_secs` bounds the wait so a caller cannot hang forever; the response
/// says whether a reply actually arrived. Receiving it marks it *delivered* and
/// nothing more: whether anything read it is for the agent to say by acking.
#[route(GET "/api/sessions/{session_id}/reply")]
async fn await_reply_route(cx: &Cx) -> Result<Json<AwaitedReply>> {
    let id = path_param::<SessionId>(cx)?;
    let seconds = query_param(cx, "timeout_secs")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(600);

    let receiver = state(cx).store.subscribe(id).map_err(|_| not_found())?;

    let arrived = tokio::time::timeout(std::time::Duration::from_secs(seconds), receiver)
        .await
        .ok()
        .and_then(|result| result.ok())
        .is_some();

    // Stamped only once the bytes are actually going out: a peek that times out
    // has delivered nothing.
    let reply = if arrived {
        state(cx)
            .store
            .mark_delivered(id)
            .map_err(|_| not_found())?
    } else {
        None
    };

    Ok(Json(AwaitedReply {
        replied: reply.is_some(),
        reply,
    }))
}

/// Collects a reply that is already there, marking it delivered.
///
/// The long-poll route is for an agent that is still waiting. This one is for an
/// agent that has come back later and found the reply in its inbox.
#[route(POST "/api/sessions/{session_id}/collect")]
async fn collect_route(cx: &Cx) -> Result<Json<AwaitedReply>> {
    let id = path_param::<SessionId>(cx)?;
    let reply = state(cx)
        .store
        .mark_delivered(id)
        .map_err(|_| not_found())?;
    Ok(Json(AwaitedReply {
        replied: reply.is_some(),
        reply,
    }))
}

/// What an agent sends to say it has actually read the reply.
#[derive(Serialize, Deserialize)]
pub struct AckInput {
    /// What the agent is doing about the reply.
    pub note: String,
}

/// Records that an agent read the reply and is acting on it.
///
/// Separate from collecting on purpose. Collecting is the transport moving bytes;
/// this is a live model saying something about them. An agent that fetches a
/// reply and then dies — needing credentials, say — will have collected and never
/// acked, and the page has to be able to show that difference.
#[route(POST "/api/sessions/{session_id}/ack")]
async fn ack_route(cx: &Cx, Json(input): Json<AckInput>) -> Result<Json<Acked>> {
    let id = path_param::<SessionId>(cx)?;
    let note = input.note.trim();
    if note.is_empty() {
        return Err(bad_request("an ack needs a note saying what you are doing").into());
    }
    let acked = state(cx)
        .store
        .ack(id, note.to_owned())
        .map_err(|_| not_found())?;
    if !acked {
        return Err(bad_request("there is no reply on this session to acknowledge").into());
    }
    Ok(Json(Acked { acked }))
}

#[derive(Serialize, Deserialize)]
pub struct Acked {
    pub acked: bool,
}

/// How far a reply has got, for the page's own polling.
///
/// Deliberately not the same route as `await_reply_route`: this one must never
/// mark anything delivered, or the page would be reporting its own visit as the
/// agent's receipt.
#[derive(Serialize)]
pub struct ReplyStatus {
    pub replied: bool,
    /// The reply has left the daemon for an agent process.
    pub delivered: bool,
    /// An agent has said it read the reply, and what it is doing about it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ack: Option<crate::session::Ack>,
    /// Comment count, so the page can reconcile after a reconnect.
    pub comments: usize,
}

#[route(GET "/api/sessions/{session_id}/status")]
async fn status_route(cx: &Cx) -> Result<Json<ReplyStatus>> {
    let id = path_param::<SessionId>(cx)?;
    state(cx)
        .store
        .with(id, |session| {
            Json(ReplyStatus {
                replied: session.reply.is_some(),
                delivered: session
                    .reply
                    .as_ref()
                    .is_some_and(|reply| reply.delivered_at.is_some()),
                ack: session.reply.as_ref().and_then(|reply| reply.ack.clone()),
                comments: session.comments.len(),
            })
        })
        .ok_or_else(|| not_found().into())
}

/// The comments an agent collects after review.
#[derive(Serialize, Deserialize)]
pub struct Comments {
    pub comments: Vec<Comment>,
}

#[route(GET "/api/sessions/{session_id}/comments")]
async fn list_comments_route(cx: &Cx) -> Result<Json<Comments>> {
    let id = path_param::<SessionId>(cx)?;
    let comments = state(cx).store.comments(id).map_err(|_| not_found())?;
    Ok(Json(Comments { comments }))
}

#[derive(Deserialize)]
struct CommentInput {
    file: String,
    line: usize,
    /// The last line of a dragged range. Absent for a single-line comment.
    #[serde(default)]
    end_line: Option<usize>,
    side: String,
    text: String,
}

/// Adds a line comment.
///
/// The quoted snippet is looked up from the session's own diff rather than taken
/// from the request: the page should not have to be trusted to say what a line
/// said, and an anchor that matches no line is a bug worth reporting as one.
#[route(POST "/s/{session_id}/comments")]
async fn comment_route(cx: &Cx, Json(input): Json<CommentInput>) -> Result<Json<Comment>> {
    let id = path_param::<SessionId>(cx)?;
    let text = input.text.trim().to_owned();
    if text.is_empty() {
        return Err(bad_request("comment must not be empty").into());
    }
    if text.len() > MAX_REPLY_BYTES {
        return Err(bad_request("comment is too long").into());
    }
    let side = Side::parse(&input.side).ok_or_else(|| bad_request("side must be old or new"))?;

    // `Anchor::range` puts the ends in order, so a drag upwards needs no special
    // case here.
    let anchor = match input.end_line {
        Some(end) => Anchor::range(input.file, input.line, end, side),
        None => Anchor::line(input.file, input.line, side),
    };
    if anchor.span() > MAX_COMMENT_LINES {
        return Err(bad_request("comment covers too many lines").into());
    }

    let snippet = state(cx)
        .store
        .with(id, |session| snippet_for(&session.content, &anchor))
        .ok_or_else(not_found)?
        .ok_or_else(|| bad_request("no such line in this diff"))?;

    let comment = state(cx)
        .store
        .comment(id, anchor, snippet, text)
        .map_err(|_| not_found())?;
    Ok(Json(comment))
}

/// The text of the lines a comment anchors to, if any exist.
///
/// A range is joined with newlines. Returns `None` when the first line does not
/// exist, which is the case worth rejecting: a range whose tail runs past the end
/// of a hunk is quoted as far as it goes rather than refused, since a drag can
/// legitimately end on the last line shown.
fn snippet_for(content: &Content, anchor: &Anchor) -> Option<String> {
    let Content::Diff { files } = content else {
        return None;
    };
    let (file, line, side) = (&anchor.file, anchor.line, anchor.side);
    let end_line = anchor.last();
    // Addressable line numbers on this side, paired with their text. A removed
    // line is only addressable on the old side and an added one only on the new,
    // so the kind check keeps a context line's twin numbers from matching the
    // wrong side.
    let addressable: Vec<(usize, &str)> = files
        .iter()
        .filter(|f| &f.label == file)
        .flat_map(|f| &f.hunks)
        .flat_map(|h| &h.lines)
        .filter(|l| crate::session::side_of(l.kind) == side)
        .filter_map(|l| {
            let number = match side {
                Side::Old => l.old,
                Side::New => l.new,
            };
            number.map(|n| (n, l.text.as_str()))
        })
        .collect();

    // The anchor itself must exist; a range whose tail runs past the end of a
    // hunk is quoted as far as it goes, since a drag can legitimately end there.
    if !addressable.iter().any(|(n, _)| *n == line) {
        return None;
    }
    let quoted: Vec<&str> = addressable
        .iter()
        .filter(|(n, _)| (line..=end_line).contains(n))
        .map(|(_, text)| *text)
        .collect();
    Some(quoted.join("\n"))
}

/// One session's reply, as reported by `showme inbox`.
#[derive(Serialize, Deserialize)]
pub struct InboxItem {
    pub id: String,
    pub title: String,
    pub reply: Reply,
    pub comments: usize,
    pub url: String,
}

#[derive(Serialize, Deserialize)]
pub struct Inbox {
    pub items: Vec<InboxItem>,
}

/// Every reply the daemon is holding.
///
/// A foreground `--wait` is a poor place to keep a 24-hour wait: the agent's
/// turn ends long before the user gets to the notification. The daemon keeps the
/// reply either way, so this is how a later turn finds one it missed. Marks
/// nothing delivered — `showme collect` does that, deliberately, so taking a
/// reply is an explicit act.
///
/// `?unread=1` filters on the *ack*, not on delivery: a reply that was collected
/// by a process that then died is precisely the one still needing attention, and
/// filtering on delivery would hide it.
#[route(GET "/api/inbox")]
async fn inbox_route(cx: &Cx) -> Result<Json<Inbox>> {
    let unread_only = query_param(cx, "unread").is_some_and(|v| v != "false" && v != "0");
    let host = public_host(cx);
    let port = topcoat::router::uri(cx)
        .port_u16()
        .unwrap_or_else(|| request_port(cx));

    let items = state(cx)
        .store
        .replied()
        .into_iter()
        .filter(|(_, _, reply, _)| !unread_only || reply.ack.is_none())
        .map(|(id, title, reply, comments)| InboxItem {
            url: format!("http://{host}:{port}/s/{id}"),
            id,
            title,
            reply,
            comments,
        })
        .collect();
    Ok(Json(Inbox { items }))
}

/// The port the request came in on, from the Host header.
fn request_port(cx: &Cx) -> u16 {
    topcoat::router::headers(cx)
        .get(http::header::HOST)
        .and_then(|value| value.to_str().ok())
        .and_then(|host| host.rsplit_once(':'))
        .and_then(|(_, port)| port.parse().ok())
        .unwrap_or(3000)
}

/// Reads one query parameter.
fn query_param(cx: &Cx, name: &str) -> Option<String> {
    topcoat::router::uri(cx).query().and_then(|query| {
        query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.to_owned())
    })
}

/// Serves until the process is signalled.
///
/// The daemon takes the first free port in the range rather than insisting on
/// the lowest: the range was chosen because those ports are already forwarded,
/// which also means they are already full of other servers.
pub async fn serve(config: &Config, store: Store) -> AnyResult<()> {
    let mut bound = None;
    let mut last_error = None;
    for port in config.ports() {
        let addr = SocketAddr::new(config.bind, port);
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                bound = Some((listener, port));
                break;
            }
            Err(error) => last_error = Some((port, error)),
        }
    }

    let (listener, port) = bound.ok_or_else(|| {
        let (last, error) = last_error.expect("the range is non-empty");
        anyhow::anyhow!(
            "no free port in {}-{} ({last}: {error}). Is a showme daemon already \
             running? Widen the range with `showme config --ports LOW-HIGH`",
            config.port_low,
            config.port_high,
        )
    })?;

    let colors = theme_colors(theme(&config.theme));
    eprintln!("showme listening on http://{}:{port}", config.public_host);
    topcoat::serve(listener, router(store, colors, config.theme.clone()))
        .await
        .context("server error")
}

fn state(cx: &Cx) -> &State {
    app_context::<State>(cx)
}

// --- Pages ------------------------------------------------------------------

/// Lists live sessions. Handy when a link has scrolled out of Slack.
#[page("/")]
async fn index_page(cx: &Cx) -> Result {
    let state = state(cx);
    let sessions = state.store.summaries();
    let colors = &state.colors;

    let body = view! { cx =>
        <div class="wrap">
            if sessions.is_empty() {
                <p style="color: var(--muted)">
                    "No sessions yet. An agent creates one with "
                    <code>"showme code …"</code>
                    ", "
                    <code>"showme docs …"</code>
                    ", "
                    <code>"showme dir …"</code>
                    ", or "
                    <code>"showme demo …"</code>
                    "."
                </p>
            } else {
                <ul style="list-style: none; padding: 0; margin: 0">
                    for (id, title, kind, replied) in sessions {
                        <li style="padding: 12px 0; border-bottom: 1px solid var(--border)">
                            <a href=(format!("/s/{id}")) style="font-weight: 600">(title)</a>
                            <span class="kind" style="margin-left: 10px">(kind)</span>
                            if replied {
                                <span style="color: var(--muted); margin-left: 10px">"· replied"</span>
                            }
                        </li>
                    }
                </ul>
            }
        </div>
    }?;

    view! { cx =>
        // `Page::index` says the rest: no session, so no reply box, and no single
        // agent it belongs to.
        shell(
            page: crate::ui::Page::index(),
            colors: colors,
            subheader: None,
            child: body,
        )
    }
}

/// A session.
#[page("/s/{session_id}")]
async fn session_page(cx: &Cx) -> Result {
    let id = path_param::<SessionId>(cx)?;
    let state = state(cx);

    let session = state
        .store
        .with(id, |session| {
            (
                session.title.clone(),
                session.note.clone(),
                session.content.clone(),
                session.reply.clone(),
                session.content.kind(),
                session.comments.clone(),
                session.origin.clone(),
                session.theme.clone(),
                session.brief.clone(),
            )
        })
        .ok_or_else(not_found)?;
    let (title, note, content, existing_reply, kind, comments, origin, session_theme, brief) =
        session;

    // A session highlighted with its own theme needs the page's colours to match
    // it: the card's background comes from here, and a mismatch puts light code
    // on a dark card. Recomputed per request only when it differs, since
    // `theme()` hands back a `&'static` and this is a handful of colour lookups.
    let own_colors = session_theme
        .as_deref()
        .filter(|name| *name != state.theme)
        .map(|name| theme_colors(theme(name)));
    let colors = own_colors.as_ref().unwrap_or(&state.colors);

    // Framed content needs `<main>` to fill the viewport with no page scrolling
    // of its own, so the iframe can own the space below the chrome.
    let body_class = content.is_framed().then_some("framed");

    // The bar can be collapsed anywhere it competes with the content: over a
    // framed app it is someone else's screen space, and over a long diff it is
    // a sticky strip in the way of the code. Not on the index, which is nothing
    // but chrome.
    let hideable = true;

    // The agent's own tour of the work, above everything: what changed, why, and
    // what to look at first. Rendered even in the framed modes, where it is the
    // only prose the page has room for.
    let guide = match &brief {
        Some(html) => Some(view! { cx =>
            <section class="brief doc">(Raw(html.clone()))</section>
        }?),
        None => None,
    };

    let nav = match &content {
        Content::Code { files } => Some(view! { cx =>
            <nav class="files">
                for file in files {
                    <a href=(format!("#{}", anchor(&file.label)))>(&file.label)</a>
                }
            </nav>
        }?),
        Content::Diff { files } => Some(view! { cx =>
            <nav class="files">
                for file in files {
                    <a href=(format!("#{}", anchor(&file.label)))>
                        (&file.label)
                        " "
                        <span class="added">"+" (file.added)</span>
                        <span class="removed">"−" (file.removed)</span>
                    </a>
                }
            </nav>
        }?),
        Content::Docs { files } if files.len() > 1 => Some(view! { cx =>
            <nav class="files">
                for file in files {
                    <a href=(format!("#{}", anchor(&file.label)))>(&file.label)</a>
                }
            </nav>
        }?),
        _ => None,
    };

    // The nav must stay directly under the bar to keep sticking, so the brief
    // follows it rather than leading.
    let subheader = match (nav, guide) {
        (Some(nav), Some(guide)) => Some(view! { cx => (nav) (guide) }?),
        (Some(nav), None) => Some(nav),
        (None, Some(guide)) => Some(guide),
        (None, None) => None,
    };

    let body = match &content {
        Content::Code { files } => view! { cx =>
            <div class="wrap">
                for file in files {
                    <section class="file" id=(anchor(&file.label))>
                        <div class="filename">
                            <span>(&file.label)</span>
                            <span class="meta">
                                (&file.language) " · " (file.lines) " lines"
                            </span>
                        </div>
                        <div class="codebox">(Raw(file.html.clone()))</div>
                    </section>
                }
            </div>
        }?,

        Content::Diff { files } => view! { cx =>
            // `id="diff"` is what the comment script binds to, and the session
            // id travels with it so the script needs no inlined state.
            <div class="wrap" id="diff" data-session=(id)>
                for file in files {
                    <section class="file" id=(anchor(&file.label))>
                        <div class="filename">
                            <span>(&file.label)</span>
                            <span class="meta">
                                <span class="added">"+" (file.added)</span>
                                " "
                                <span class="removed">"−" (file.removed)</span>
                            </span>
                        </div>
                        <div class="codebox">
                            for hunk in &file.hunks {
                                diff_hunk(file: &file.label, hunk: hunk, comments: &comments)
                            }
                        </div>
                    </section>
                }
            </div>
        }?,

        Content::Docs { files } => view! { cx =>
            <div class="wrap">
                for file in files {
                    <section class="doc" id=(anchor(&file.label))>
                        (Raw(file.html.clone()))
                    </section>
                }
            </div>
        }?,

        Content::Static { entry, .. } => {
            // Served through /s/{id}/assets/ so relative links inside the
            // directory resolve.
            let src = format!("/s/{id}/assets/{}", entry.trim_start_matches('/'));
            view! { cx =>
                <div class="framewrap">
                    <iframe class="frame" src=(src) title="Preview"></iframe>
                </div>
            }?
        }

        Content::Demo { port, path } => {
            // The demo is framed rather than linked so the reply widget travels
            // with it. Same-origin is impossible across ports, so the iframe
            // points at the demo's own origin and the overlay sits above it.
            let src = format!("http://{}:{}{}", public_host(cx), port, path);
            view! { cx =>
                <div class="framewrap">
                    <iframe class="frame" src=(src) title="Live demo"></iframe>
                </div>
            }?
        }
    };

    let page = crate::ui::Page {
        title,
        note,
        kind: kind.to_owned(),
        session_id: id.to_owned(),
        reply: existing_reply,
        body_class: body_class.map(str::to_owned),
        hideable,
        commentable: content.is_commentable(),
        origin,
    };

    view! { cx =>
        shell(
            page: page,
            colors: colors,
            subheader: subheader,
            child: body,
        )
    }
}

/// The host the browser used, so framed URLs keep working when reached over a
/// forwarded port or a non-default hostname.
fn public_host(cx: &Cx) -> String {
    topcoat::router::headers(cx)
        .get(http::header::HOST)
        .and_then(|value| value.to_str().ok())
        // Strip the port: only the host is reused, with the demo's own port.
        .and_then(|host| host.rsplit_once(':').map(|(h, _)| h).or(Some(host)))
        .unwrap_or("localhost")
        .to_owned()
}

/// A stable, URL-safe anchor for a file label.
fn anchor(label: &str) -> String {
    let mut out = String::with_capacity(label.len() + 2);
    out.push('f');
    out.push('-');
    for ch in label.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push('-');
        }
    }
    out
}

// --- Reply ------------------------------------------------------------------

#[derive(Deserialize)]
struct ReplyInput {
    text: String,
}

#[derive(Serialize)]
struct ReplyOutput {
    accepted: bool,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

/// The longest reply accepted. Generous for a note, small enough that a stray
/// POST cannot grow the daemon's memory without bound.
const MAX_REPLY_BYTES: usize = 16 * 1024;

/// The most lines one comment may cover.
///
/// A drag across a whole file is more likely a stuck mouse button than an
/// intention, and the snippet is stored in full.
const MAX_COMMENT_LINES: usize = 500;

#[route(POST "/s/{session_id}/reply")]
async fn reply_route(cx: &Cx, Json(input): Json<ReplyInput>) -> Result<Json<ReplyOutput>> {
    let id = path_param::<SessionId>(cx)?;
    let text = input.text.trim().to_owned();

    if text.is_empty() {
        return Err(bad_request("reply must not be empty").into());
    }
    if text.len() > MAX_REPLY_BYTES {
        return Err(bad_request("reply is too long").into());
    }

    match state(cx).store.reply(id, text.clone()) {
        Ok(true) => Ok(Json(ReplyOutput {
            accepted: true,
            text,
            message: None,
        })),
        // Already answered: report it rather than silently discarding, so the
        // page can say so.
        Ok(false) => Ok(Json(ReplyOutput {
            accepted: false,
            text: String::new(),
            message: Some("this session already has a reply".to_owned()),
        })),
        Err(_) => Err(not_found().into()),
    }
}

// --- Raw content and static assets -----------------------------------------

/// The unwrapped content, for when the chrome is in the way. For a demo this
/// redirects to the demo's own origin.
#[route(GET "/s/{session_id}/raw")]
async fn raw_route(cx: &Cx) -> Result<topcoat::router::Response> {
    use topcoat::router::{Body, Response};

    let id = path_param::<SessionId>(cx)?;
    let content = state(cx)
        .store
        .with(id, |session| session.content.clone())
        .ok_or_else(not_found)?;

    let response = match content {
        Content::Demo { port, path } => Response::builder()
            .status(http::StatusCode::TEMPORARY_REDIRECT)
            .header(
                http::header::LOCATION,
                format!("http://{}:{}{}", public_host(cx), port, path),
            )
            .body(Body::empty())?,
        Content::Static { entry, .. } => Response::builder()
            .status(http::StatusCode::TEMPORARY_REDIRECT)
            .header(
                http::header::LOCATION,
                format!("/s/{id}/assets/{}", entry.trim_start_matches('/')),
            )
            .body(Body::empty())?,
        Content::Code { files } => html_response(
            files
                .iter()
                .map(|f| format!("<h2>{}</h2>{}", escape_html(&f.label), f.html))
                .collect::<String>(),
        )?,
        // The raw form of a diff is the patch itself, which is the thing
        // someone reaching for `raw` would want to copy.
        Content::Diff { files } => {
            let mut out = String::new();
            for file in &files {
                out.push_str(&format!("--- a/{0}\n+++ b/{0}\n", file.label));
                for hunk in &file.hunks {
                    out.push_str(&hunk.header);
                    out.push('\n');
                    for line in &hunk.lines {
                        out.push_str(match line.kind {
                            crate::session::LineKind::Added => "+",
                            crate::session::LineKind::Removed => "-",
                            crate::session::LineKind::Context => " ",
                        });
                        out.push_str(&line.text);
                        out.push('\n');
                    }
                }
            }
            Response::builder()
                .header(http::header::CONTENT_TYPE, "text/plain; charset=utf-8")
                .body(Body::from(out))?
        }
        Content::Docs { files } => {
            html_response(files.iter().map(|f| f.html.clone()).collect::<String>())?
        }
    };
    Ok(response)
}

fn html_response(body: String) -> Result<topcoat::router::Response> {
    use topcoat::router::{Body, Response};
    Ok(Response::builder()
        .header(http::header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(Body::from(body))?)
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Serves files from a `Static` session's directory.
///
/// Paths are resolved against the session root by `tower_http`'s `ServeDir`,
/// which rejects traversal outside it.
#[route(GET "/s/{session_id}/assets/{*asset_path}")]
async fn asset_route(cx: &Cx) -> Result<topcoat::router::Response> {
    use tower::ServiceExt as _;
    use tower_http::services::ServeDir;

    let id = path_param::<SessionId>(cx)?;
    let root = state(cx)
        .store
        .with(id, |session| match &session.content {
            Content::Static { root, .. } => Some(root.clone()),
            _ => None,
        })
        .ok_or_else(not_found)?
        .ok_or_else(not_found)?;

    // Re-derive the sub-path from the URI: it is the part after `/assets/`.
    let uri_path = topcoat::router::uri(cx).path().to_owned();
    let marker = format!("/s/{id}/assets/");
    let rest = uri_path
        .split_once(&marker)
        .map(|(_, rest)| rest.to_owned())
        .unwrap_or_default();

    let request = http::Request::builder()
        .uri(format!("/{rest}"))
        .body(topcoat::router::Body::empty())
        .map_err(|e| bad_request(format!("bad asset path: {e}")))?;

    let response = ServeDir::new(&root)
        .oneshot(request)
        .await
        .map_err(|e| bad_request(format!("serving {rest}: {e}")))?;

    Ok(response.map(topcoat::router::Body::new))
}

/// The marker the CLI looks for when scanning the port range for a daemon.
///
/// The range is full of other people's servers — that is the whole point of
/// picking it — and several of them answer 200 on `/health`. Only a body
/// carrying this string is a showme daemon.
pub const HEALTH_MARKER: &str = "showme-daemon";

#[route(GET "/health")]
async fn health_route(cx: &Cx) -> Result<Json<serde_json::Value>> {
    Ok(Json(serde_json::json!({
        "ok": true,
        "service": HEALTH_MARKER,
        "version": env!("CARGO_PKG_VERSION"),
        "sessions": state(cx).store.summaries().len(),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchors_are_url_safe_and_stable() {
        assert_eq!(anchor("src/main.rs"), "f-src-main-rs");
        assert_eq!(anchor("A B.md"), "f-a-b-md");
        // Same label, same anchor: the nav link and the section must agree.
        assert_eq!(anchor("x/y.rs"), anchor("x/y.rs"));
    }

    #[test]
    fn escape_html_escapes_markup() {
        assert_eq!(escape_html("<a> & </a>"), "&lt;a&gt; &amp; &lt;/a&gt;");
    }
}
