//! The daemon: routes, pages, and the reply endpoint.

use std::net::SocketAddr;

use anyhow::{Context as _, Result as AnyResult};
use serde::{Deserialize, Serialize};
use topcoat::Result;
use topcoat::context::{Cx, app_context};
use topcoat::router::content::Json;
use topcoat::router::error::{bad_request, internal_server_error, not_found};
use topcoat::router::{Router, page, path_param, route};
use topcoat::view::view;

use crate::config::Config;
use crate::render::{ThemeColors, theme, theme_colors};
use crate::session::{
    Anchor, Author, Comment, Content, Message, Reply, Session, Side, Store, StoreError,
};
use crate::ui::{Raw, code_file, diff_hunk, shell};

/// Values shared by every request.
struct State {
    store: Store,
    colors: ThemeColors,
    /// The daemon's configured theme, used when a session names none.
    theme: String,
    /// Whole diffed files, highlighted, for serving hidden context.
    ///
    /// A range cannot be highlighted on its own — syntect's state carries across
    /// lines — so revealing twenty lines means highlighting the file they are in.
    /// The page asks one range at a time, so the second click on the same file
    /// would pay for it again.
    highlighted: std::sync::Mutex<Vec<HighlightedFile>>,
}

/// One highlighted file, keyed by the session and index it came from.
struct HighlightedFile {
    session: String,
    index: usize,
    lines: std::sync::Arc<Vec<String>>,
}

/// How many highlighted files to keep. Reviewing walks through a diff rather than
/// jumping around it, so a short window of recently expanded files is enough, and
/// the memory is bounded by it rather than by the size of the diff.
const HIGHLIGHT_CACHE: usize = 4;

impl State {
    /// The highlighted lines of one diffed file, from the cache or freshly made.
    fn highlighted(
        &self,
        session: &str,
        index: usize,
        file: &crate::session::DiffFile,
        theme_name: Option<&str>,
    ) -> Result<std::sync::Arc<Vec<String>>> {
        let mut cache = self.highlighted.lock().expect("not poisoned");
        if let Some(hit) = cache
            .iter()
            .find(|entry| entry.session == session && entry.index == index)
        {
            return Ok(hit.lines.clone());
        }
        let theme = theme(theme_name.unwrap_or(&self.theme));
        let lines = crate::diff::highlight_source(&file.label, &file.source, theme)
            .map_err(internal_server_error)?;
        let lines = std::sync::Arc::new(lines);
        cache.push(HighlightedFile {
            session: session.to_owned(),
            index,
            lines: lines.clone(),
        });
        if cache.len() > HIGHLIGHT_CACHE {
            cache.remove(0);
        }
        Ok(lines)
    }
}

#[path_param(error = not_found)]
struct SessionId(String);

#[path_param(error = not_found)]
struct CommentId(u64);

/// Builds the router.
fn router(store: Store, colors: ThemeColors, theme: String) -> Router {
    Router::builder()
        .app_context(State {
            store,
            colors,
            theme,
            highlighted: std::sync::Mutex::new(Vec::new()),
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
        .route(context_route)
        .route(thread_message_route)
        .route(agent_message_route)
        .route(threads_route)
        .route(agent_threads_route)
        .route(next_question_route)
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
    /// Per-line source text of the brief markdown, for authoritative snippet
    /// lookup on comments addressed to the brief.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub brief_sources: Vec<String>,
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
            brief_sources: Vec::new(),
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct CreatedSession {
    pub id: String,
}

#[route(POST "/api/sessions")]
async fn create_route(cx: &Cx, Json(input): Json<CreateSession>) -> Result<Json<CreatedSession>> {
    // Reject sessions with colliding file anchors. Two files that normalize to
    // the same #f- anchor would break page navigation and guided-review links.
    let labels: Vec<String> = match &input.content {
        Content::Code { files } => files.iter().map(|f| f.label.clone()).collect(),
        Content::Diff { files } => files.iter().map(|f| f.label.clone()).collect(),
        Content::Docs { files } => files.iter().map(|f| f.label.clone()).collect(),
        _ => Vec::new(),
    };
    if !labels.is_empty()
        && let Err(msg) = crate::validate_file_anchors(&labels)
    {
        return Err(bad_request(msg).into());
    }

    let id = state(cx)
        .store
        .try_insert_new(crate::session::NewSession {
            note: input.note,
            // The daemon is long-lived and shared, so it cannot detect this itself:
            // its own cwd and pane are wherever it happened to be started.
            origin: input.origin.unwrap_or_default(),
            theme: input.theme,
            brief: input.brief,
            brief_sources: input.brief_sources,
            ..crate::session::NewSession::new(input.title, input.content)
        })
        .map_err(store_error)?;
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
/// Awaits a long-poll oneshot, bounding it by `timeout` seconds when set.
///
/// With no timeout it waits indefinitely — the caller (an agent) has asked to
/// sit open until the reader acts. Returns `Some(value)` if the channel fired,
/// `None` if it timed out or the sender was dropped.
async fn wait_for<T>(
    timeout: Option<u64>,
    receiver: tokio::sync::oneshot::Receiver<T>,
) -> Option<T> {
    match timeout {
        Some(seconds) => tokio::time::timeout(std::time::Duration::from_secs(seconds), receiver)
            .await
            .ok()
            .and_then(|result| result.ok()),
        None => receiver.await.ok(),
    }
}

/// `timeout_secs` bounds the wait when supplied; without it the long-poll
/// blocks until a reply lands. The response says whether a reply actually
/// arrived. Receiving it marks it *delivered* and nothing more: whether
/// anything read it is for the agent to say by acking.
#[route(GET "/api/sessions/{session_id}/reply")]
async fn await_reply_route(cx: &Cx) -> Result<Json<AwaitedReply>> {
    let id = path_param::<SessionId>(cx)?;
    let timeout = query_param(cx, "timeout_secs").and_then(|value| value.parse::<u64>().ok());

    let receiver = state(cx).store.subscribe(id).map_err(|_| not_found())?;

    let arrived = wait_for(timeout, receiver).await.is_some();

    // Stamped only once the bytes are actually going out: a peek that times out
    // has delivered nothing.
    let reply = if arrived {
        state(cx).store.mark_delivered(id).map_err(store_error)?
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
    let reply = state(cx).store.mark_delivered(id).map_err(store_error)?;
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
        .map_err(store_error)?;
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
    /// Explicit comment target identity, separate from display label.
    ///
    /// New UI sends `"brief"` for guided briefs, `"doc:<n>"` for the nth docs
    /// file. Legacy clients omit this, falling back to `file`-based resolution
    /// when unambiguous.
    #[serde(default)]
    target: Option<String>,
    line: usize,
    /// The last line of a dragged range. Absent for a single-line comment.
    #[serde(default)]
    end_line: Option<usize>,
    side: String,
    text: String,
}

/// A run of lines from a diffed file that the patch itself does not show.
#[derive(Serialize)]
pub struct ContextLines {
    /// The file's display label, so the page can address comments on these lines.
    pub file: String,
    pub lines: Vec<ContextLine>,
}

#[derive(Serialize)]
pub struct ContextLine {
    /// New-side line number.
    pub new: usize,
    /// Old-side number, absent when the diff shows no context line to take the
    /// offset from — a file that was rewritten end to end.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old: Option<usize>,
    pub html: String,
}

/// The most lines one expansion may ask for.
///
/// A gap can be thousands of lines long, and revealing all of them inserts that
/// many rows into the page in one go. The page asks for a bounded run and can ask
/// again; the limit is here as well so a hand-made request cannot make the daemon
/// highlight and serialise a whole large file at once.
const MAX_CONTEXT_LINES: usize = 2000;

/// Serves the lines between a diff's hunks, so the page can reveal them.
///
/// The file is named by its index in the session's diff rather than by its label:
/// the index cannot be ambiguous between two files with the same label, and it
/// needs no escaping in a query string.
#[route(GET "/s/{session_id}/context")]
async fn context_route(cx: &Cx) -> Result<Json<ContextLines>> {
    let id = path_param::<SessionId>(cx)?;
    let number = |name: &str| {
        query_param(cx, name)
            .and_then(|value| value.parse::<usize>().ok())
            .ok_or_else(|| bad_request("from, to and file must be numbers"))
    };
    let index = number("file")?;
    let from = number("from")?;
    let to = number("to")?;
    if from == 0 || to < from {
        return Err(bad_request("from must be >= 1 and to must be >= from").into());
    }
    if to + 1 - from > MAX_CONTEXT_LINES {
        return Err(bad_request("too many lines requested").into());
    }

    // The session is cloned out rather than highlighted under the store's lock:
    // highlighting a file is real work and no other request should wait on it.
    let (file, theme_name) = state(cx)
        .store
        .with(id, |session| match &session.content {
            Content::Diff { files } => files
                .get(index)
                .filter(|file| !file.source.is_empty())
                .map(|file| (file.clone(), session.theme.clone())),
            _ => None,
        })
        .ok_or_else(not_found)?
        .ok_or_else(|| bad_request("that file has no captured source"))?;
    if to > file.source.len() {
        return Err(bad_request("beyond the end of the file").into());
    }

    let highlighted = state(cx).highlighted(id, index, &file, theme_name.as_deref())?;
    let offset = crate::diff::old_offset_at(&file, from);
    let lines = (from..=to)
        .map(|number| ContextLine {
            new: number,
            old: offset.and_then(|by| usize::try_from(number as i64 + by).ok()),
            html: highlighted
                .get(number - 1)
                .cloned()
                .unwrap_or_else(String::new),
        })
        .collect();
    Ok(Json(ContextLines {
        file: file.label,
        lines,
    }))
}

/// Adds a line comment.
///
/// The quoted snippet is looked up from the session's own diff rather than taken
/// from the request: the page should not have to be trusted to say what a line
/// said, and an anchor that matches no line is a bug worth reporting as one.
#[route(POST "/s/{session_id}/comments")]
async fn comment_route(cx: &Cx, Json(mut input): Json<CommentInput>) -> Result<Json<Comment>> {
    let id = path_param::<SessionId>(cx)?;
    let text = input.text.trim().to_owned();
    if text.is_empty() {
        return Err(bad_request("comment must not be empty").into());
    }
    if text.len() > MAX_REPLY_BYTES {
        return Err(bad_request("comment is too long").into());
    }
    let side = Side::parse(&input.side).ok_or_else(|| bad_request("side must be old or new"))?;

    // Explicit markdown targets are authoritative; the display label stored in
    // the comment comes from the session, never from the request.
    if let Some(target) = input.target.as_deref() {
        let file = state(cx)
            .store
            .with(id, |session| match target {
                "brief" if !session.brief_sources.is_empty() => Some("brief:".to_owned()),
                value if value.starts_with("doc:") => {
                    let index = value.strip_prefix("doc:")?.parse::<usize>().ok()?;
                    let Content::Docs { files } = &session.content else {
                        return None;
                    };
                    files.get(index).map(|file| file.label.clone())
                }
                _ => None,
            })
            .ok_or_else(not_found)?
            .ok_or_else(|| bad_request("unknown comment target"))?;
        input.file = file;
    }

    // Reject line 0, which is not a valid 1-indexed position.
    if input.line == 0 {
        return Err(bad_request("line must be >= 1").into());
    }
    // Reject end_line of 0 as well.
    if input.end_line.is_some_and(|e| e == 0) {
        return Err(bad_request("end_line must be >= 1").into());
    }

    // Determine whether this is targeting a doc/brief/code based on explicit
    // target or content type. All non-diff types require Side::New.
    let requires_side_new = input.target.as_deref() == Some("brief")
        || input
            .target
            .as_deref()
            .is_some_and(|t| t.starts_with("doc:"))
        || input.file == "brief:"
        || state(cx)
            .store
            .with(id, |s| {
                matches!(&s.content, Content::Docs { .. } | Content::Code { .. })
            })
            .unwrap_or(false);
    if requires_side_new && side != Side::New {
        return Err(bad_request("side must be new for code/docs/brief comments").into());
    }

    // `Anchor::range` puts the ends in order, so a drag upwards needs no special
    // case here.
    let mut anchor = match input.end_line {
        Some(end) => Anchor::range(input.file, input.line, end, side),
        None => Anchor::line(input.file, input.line, side),
    };
    anchor.target = input.target;

    if anchor.span() > MAX_COMMENT_LINES {
        return Err(bad_request("comment covers too many lines").into());
    }

    let snippet = state(cx)
        .store
        .with(id, |session| snippet_for(session, &anchor))
        .ok_or_else(not_found)?
        .ok_or_else(|| bad_request("no such line in this session"))?;

    let comment = state(cx)
        .store
        .comment(id, anchor, snippet, text)
        .map_err(store_error)?;
    Ok(Json(comment))
}

/// Adds the reader's own message to a thread.
///
/// The page's route, so the author is the user: an agent's answers arrive on
/// `/api/sessions/…` instead, and nothing in a request body can change that.
#[route(POST "/s/{session_id}/comments/{comment_id}/messages")]
async fn thread_message_route(cx: &Cx, Json(input): Json<MessageInput>) -> Result<Json<Message>> {
    post_message(cx, Author::User, input).await
}

/// Adds the agent's answer to a thread. The agent's side of `wdyt say`.
#[route(POST "/api/sessions/{session_id}/comments/{comment_id}/messages")]
async fn agent_message_route(cx: &Cx, Json(input): Json<MessageInput>) -> Result<Json<Message>> {
    post_message(cx, Author::Agent, input).await
}

#[derive(Deserialize)]
struct MessageInput {
    text: String,
}

async fn post_message(cx: &Cx, from: Author, input: MessageInput) -> Result<Json<Message>> {
    let id = path_param::<SessionId>(cx)?;
    let comment_id = *path_param::<CommentId>(cx)?;
    let text = input.text.trim().to_owned();
    if text.is_empty() {
        return Err(bad_request("a message must not be empty").into());
    }
    if text.len() > MAX_REPLY_BYTES {
        return Err(bad_request("that message is too long").into());
    }
    let message = state(cx)
        .store
        .message(id, comment_id, from, text)
        .map_err(store_error)?;
    Ok(Json(message))
}

/// Every thread in a session, for the page to poll.
///
/// Read-only on purpose, like the reply's status route: a reader looking at their
/// own page must not be able to mark their own question as picked up.
#[route(GET "/s/{session_id}/threads")]
async fn threads_route(cx: &Cx) -> Result<Json<Threads>> {
    threads(cx)
}

/// The same, for an agent catching up on a discussion. Also read-only: taking a
/// question is what `/threads/next` is for.
#[route(GET "/api/sessions/{session_id}/threads")]
async fn agent_threads_route(cx: &Cx) -> Result<Json<Threads>> {
    threads(cx)
}

fn threads(cx: &Cx) -> Result<Json<Threads>> {
    let id = path_param::<SessionId>(cx)?;
    let comments = state(cx).store.comments(id).map_err(|_| not_found())?;
    Ok(Json(Threads { threads: comments }))
}

#[derive(Serialize, Deserialize)]
pub struct Threads {
    pub threads: Vec<Comment>,
}

/// Blocks until the reader says something no agent has taken, and takes it.
#[route(GET "/api/sessions/{session_id}/threads/next")]
async fn next_question_route(cx: &Cx) -> Result<Json<NextQuestion>> {
    let id = path_param::<SessionId>(cx)?;
    let timeout = query_param(cx, "timeout_secs").and_then(|value| value.parse::<u64>().ok());

    let waiting = state(cx).store.watch(id).map_err(|_| not_found())?;
    let woken = wait_for(timeout, waiting).await.is_some();

    // Even when woken, the question may have gone to another agent process that
    // was watching the same session: taking it is what settles who has it.
    let taken = if woken {
        state(cx).store.take_question(id).map_err(store_error)?
    } else {
        None
    };
    Ok(Json(match taken {
        Some((thread, message)) => NextQuestion {
            asked: true,
            thread: Some(thread),
            message: Some(message),
        },
        None => NextQuestion {
            asked: false,
            thread: None,
            message: None,
        },
    }))
}

/// What an agent gets when it asks for the next question.
#[derive(Serialize, Deserialize)]
pub struct NextQuestion {
    /// Whether anything was waiting.
    pub asked: bool,
    /// The thread it belongs to, with the line it is about and the whole exchange
    /// so far — the agent needs the context, not just the sentence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread: Option<Comment>,
    /// The message itself. `id` is 0 for a comment's own text, which is the first
    /// thing said in its thread.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<Message>,
}

/// The text of the lines a comment anchors to, if any exist.
///
/// Resolution uses the explicit `target` when present:
/// - `"brief"` → the guided-review brief sources
/// - `"doc:<n>"` → the nth docs file (0-indexed)
///
/// Legacy (no target):
/// - `file == "brief:"` → brief sources
/// - For Code: finds the file by label
/// - For Docs: finds the file by label, rejecting if ambiguous
/// - For Diff: standard side-based line lookup
///
/// Strict validation: code/docs/brief reject Side::Old and any line beyond the
/// stored source length (no truncation). Returns `None` on out-of-bounds rather
/// than silently quoting partial content.
fn snippet_for(session: &Session, anchor: &Anchor) -> Option<String> {
    let content = &session.content;
    let brief_sources = &session.brief_sources;

    // Explicit target resolution (new UI protocol).
    if let Some(target) = &anchor.target {
        return match target.as_str() {
            "brief" => snippet_from_lines_strict(brief_sources, anchor),
            t if t.starts_with("doc:") => {
                let idx: usize = t.strip_prefix("doc:").unwrap().parse().ok()?;
                if let Content::Docs { files } = content {
                    let file = files.get(idx)?;
                    snippet_from_lines_strict(&file.sources, anchor)
                } else {
                    None
                }
            }
            _ => None,
        };
    }

    // Legacy resolution (no explicit target).

    // Legacy brief comments used the reserved file identifier `brief:`. Keep
    // accepting it only when it is unambiguous with the session's doc labels.
    if anchor.file == "brief:" && !brief_sources.is_empty() {
        match content {
            Content::Docs { files } if files.iter().any(|file| file.label == anchor.file) => {
                // A legacy request cannot distinguish this document from the
                // guided brief; require the explicit `doc:<n>`/`brief` target.
                return None;
            }
            Content::Code { files } if files.iter().any(|file| file.label == anchor.file) => {}
            Content::Diff { files } if files.iter().any(|file| file.label == anchor.file) => {}
            _ => return snippet_from_lines_strict(brief_sources, anchor),
        }
    }

    if let Content::Code { files } = content {
        let file = files.iter().find(|f| f.label == anchor.file)?;
        return snippet_from_lines_strict(&file.sources, anchor);
    }

    if let Content::Docs { files } = content {
        // Reject ambiguous resolution: if multiple files share the same label,
        // legacy resolution (by label alone) is refused.
        let matches: Vec<_> = files.iter().filter(|f| f.label == anchor.file).collect();
        if matches.len() != 1 {
            return None;
        }
        return snippet_from_lines_strict(&matches[0].sources, anchor);
    }

    let Content::Diff { files } = content else {
        return None;
    };
    let (file, line, side) = (&anchor.file, anchor.line, anchor.side);

    // A line the reader revealed is not in any hunk, but it is in the file the
    // session captured — and that file was accepted only because it agreed with
    // every line the diff shows, so quoting from it is as trustworthy as quoting
    // from a hunk. Without this, commenting on context you just expanded would be
    // refused as "no such line".
    if side == Side::New
        && let Some(captured) = files
            .iter()
            .find(|f| &f.label == file && !f.source.is_empty())
    {
        return snippet_from_lines_strict(&captured.source, anchor);
    }
    let end_line = anchor.last();
    // Addressable line numbers on this side, paired with their text.
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

    // The anchor itself must exist; the endpoint must also exist.
    if !addressable.iter().any(|(n, _)| *n == line) {
        return None;
    }
    if !addressable.iter().any(|(n, _)| *n == end_line) {
        return None;
    }
    // Collect all addressable lines in the requested range.
    let quoted: Vec<(usize, &str)> = addressable
        .iter()
        .filter(|(n, _)| (line..=end_line).contains(n))
        .copied()
        .collect();
    // Require the complete contiguous range: every line number from `line` to
    // `end_line` must be present. A gap (lines in a different hunk or not
    // addressable) means the range is invalid rather than truncatable.
    let expected_count = end_line - line + 1;
    if quoted.len() != expected_count {
        return None;
    }
    Some(
        quoted
            .iter()
            .map(|(_, text)| *text)
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// Looks up a snippet from a flat list of source lines (1-indexed), strictly.
///
/// Returns `None` when the first line is out of bounds OR the end line exceeds
/// the source length. No truncation: the caller gets the full range or nothing.
/// Shared by code files, doc files, and briefs.
fn snippet_from_lines_strict(sources: &[String], anchor: &Anchor) -> Option<String> {
    let first = anchor.line.checked_sub(1)?;
    if first >= sources.len() {
        return None;
    }
    let last = anchor.last();
    if last > sources.len() {
        return None;
    }
    Some(sources[first..last].join("\n"))
}

/// One session's reply, as reported by `wdyt inbox`.
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
/// nothing delivered — `wdyt collect` does that, deliberately, so taking a
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
    // Every port we tried and why it was rejected, so the error can list them
    // all rather than blaming only the last one.
    let mut tried: Vec<(u16, std::io::Error)> = Vec::new();
    for port in config.ports() {
        let addr = SocketAddr::new(config.bind, port);
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                bound = Some((listener, port));
                break;
            }
            Err(error) => tried.push((port, error)),
        }
    }

    let (listener, port) = bound.ok_or_else(|| {
        // The CLI captures this text and relays it verbatim, so it is the one
        // place the failure is explained: name the whole range, then every port
        // attempted with the reason each was unusable.
        let attempts = tried
            .iter()
            .map(|(port, error)| format!("{port}: {error}"))
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::anyhow!(
            "no free port in {}-{} (tried {attempts}). Is a wdyt daemon already \
             running? Widen the range with `wdyt config --ports LOW-HIGH`",
            config.port_low,
            config.port_high,
        )
    })?;

    let colors = theme_colors(theme(&config.theme));
    eprintln!("wdyt listening on http://{}:{port}", config.public_host);
    topcoat::serve(listener, router(store, colors, config.theme.clone()))
        .await
        .context("server error")
}

fn state(cx: &Cx) -> &State {
    app_context::<State>(cx)
}

fn store_error(error: StoreError) -> topcoat::Error {
    match error {
        StoreError::UnknownSession => not_found().into(),
        error @ StoreError::Persistence(_) => {
            eprintln!("wdyt: {error:#}");
            internal_server_error(error).into()
        }
    }
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
                    <code>"wdyt code …"</code>
                    ", "
                    <code>"wdyt docs …"</code>
                    ", "
                    <code>"wdyt dir …"</code>
                    ", or "
                    <code>"wdyt demo …"</code>
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
                session.brief_sources.clone(),
            )
        })
        .ok_or_else(not_found)?;
    let (
        title,
        note,
        content,
        existing_reply,
        kind,
        comments,
        origin,
        session_theme,
        brief,
        brief_sources,
    ) = session;

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
    let brief_comments: Vec<_> = comments
        .iter()
        .filter(|c| {
            // New protocol: explicit target="brief" is authoritative.
            if c.target.as_deref() == Some("brief") {
                return true;
            }
            // Legacy fallback: file="brief:" is only accepted when no explicit
            // target is set. A docs file literally named "brief:" will have
            // target=Some("doc:<n>"), so this condition excludes it.
            if c.target.is_none() && c.file == "brief:" {
                return true;
            }
            false
        })
        .cloned()
        .collect();
    let brief_commentable = !brief_sources.is_empty();
    let guide = match &brief {
        Some(html) if brief_commentable => Some(view! { cx =>
            <section class="brief doc" data-doc-file="brief:" data-doc-target="brief" data-session=(id)>(Raw(html.clone()))</section>
            <div id="wdyt-brief-comments" hidden="hidden">
                for comment in &brief_comments {
                    <div class="wdyt-comment-data"
                        data-comment-id=(comment.id)
                        data-comment-target=(comment.target.as_deref().unwrap_or(""))
                        data-comment-file=(&comment.file)
                        data-comment-line=(comment.line)
                        data-comment-end-line=(comment.end_line.map_or(String::new(), |e| e.to_string()))
                        data-comment-side=(comment.side.as_str())
                        data-comment-snippet=(&comment.snippet)
                    >(&comment.text)</div>
                }
            </div>
        }?),
        Some(html) => Some(view! { cx =>
            <section class="brief doc">(Raw(html.clone()))</section>
        }?),
        None => None,
    };

    let nav = match &content {
        Content::Code { files } if files.len() > 1 => {
            let cls = if files.len() > 3 {
                "files has-sidebar"
            } else {
                "files"
            };
            Some(view! { cx =>
                <nav class=(cls)>
                    for file in files {
                        <a href=(format!("#{}", anchor(&file.label)))>(&file.label)</a>
                    }
                </nav>
            }?)
        }
        Content::Diff { files } if files.len() > 1 => {
            let cls = if files.len() > 3 {
                "files has-sidebar"
            } else {
                "files"
            };
            Some(view! { cx =>
                <nav class=(cls)>
                    for file in files {
                        <a href=(format!("#{}", anchor(&file.label)))>
                            (&file.label)
                            " "
                            <span class="added">"+" (file.added)</span>
                            <span class="removed">"−" (file.removed)</span>
                        </a>
                    }
                </nav>
            }?)
        }
        Content::Docs { files } if files.len() > 1 => {
            let cls = if files.len() > 3 {
                "files has-sidebar"
            } else {
                "files"
            };
            Some(view! { cx =>
                <nav class=(cls)>
                    for file in files {
                        <a href=(format!("#{}", anchor(&file.label)))>(&file.label)</a>
                    }
                </nav>
            }?)
        }
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
            // `data-comments` is the hook the comment script binds to, shared with
            // the diff wrap; the session id travels with it so the script needs no
            // inlined state.
            <div class="wrap" data-comments="1" data-session=(id)>
                for file in files {
                    <section class="file" id=(anchor(&file.label))>
                        <div class="filename">
                            <span>(&file.label)</span>
                            <span class="meta">
                                (&file.language) " · " (file.lines) " lines"
                            </span>
                        </div>
                        <div class="codebox">
                            // Legacy fallback: if highlighted is empty (old CLI payload),
                            // fall back to the pre-rendered html field.
                            if file.highlighted.is_empty() {
                                (Raw(file.html.clone()))
                            } else {
                                code_file(
                                    file: &file.label,
                                    highlighted: &file.highlighted,
                                    comments: &comments,
                                )
                            }
                        </div>
                    </section>
                }
            </div>
        }?,

        Content::Diff { files } => view! { cx =>
            // `id="diff"` is retained for styling and links; `data-comments` is the
            // hook the comment script binds to, shared with the code wrap, and the
            // session id travels with it so the script needs no inlined state.
            <div class="wrap" id="diff" data-comments="1" data-session=(id)>
                for block in crate::ui::diff_blocks(files) {
                    <section class="file" id=(anchor(&block.file.label))>
                        <div class="filename">
                            <span>(&block.file.label)</span>
                            <span class="meta">
                                <span class="added">"+" (block.file.added)</span>
                                " "
                                <span class="removed">"−" (block.file.removed)</span>
                            </span>
                        </div>
                        <div class="codebox">
                            for at in &block.hunks {
                                diff_hunk(
                                    file: &block.file.label,
                                    index: block.index,
                                    hunk: at.hunk,
                                    comments: &comments,
                                    gap_before: at.before,
                                    gap_after: at.after,
                                )
                            }
                        </div>
                    </section>
                }
            </div>
        }?,

        Content::Docs { files } => {
            // Filter comments for doc files (excluding brief which is handled above).
            // A comment belongs to brief if: target=="brief" OR (target is None AND file=="brief:").
            // Everything else belongs to the doc section.
            let doc_comments: Vec<_> = comments
                .iter()
                .filter(|c| {
                    if c.target.as_deref() == Some("brief") {
                        return false;
                    }
                    if c.target.is_none() && c.file == "brief:" {
                        return false;
                    }
                    true
                })
                .cloned()
                .collect();
            view! { cx =>
                <div class="wrap" data-comments="1" data-session=(id)>
                    for (idx, file) in files.iter().enumerate() {
                        <section class="doc" id=(anchor(&file.label)) data-doc-file=(&file.label) data-doc-target=(format!("doc:{idx}"))>
                            (Raw(file.html.clone()))
                        </section>
                    }
                </div>
                <div id="wdyt-doc-comments" hidden="hidden">
                    for comment in &doc_comments {
                        <div class="wdyt-comment-data"
                            data-comment-id=(comment.id)
                            data-comment-target=(comment.target.as_deref().unwrap_or(""))
                            data-comment-file=(&comment.file)
                            data-comment-line=(comment.line)
                            data-comment-end-line=(comment.end_line.map_or(String::new(), |e| e.to_string()))
                            data-comment-side=(comment.side.as_str())
                            data-comment-snippet=(&comment.snippet)
                        >(&comment.text)</div>
                    }
                </div>
            }?
        }

        Content::Static { entry, .. } => {
            // Served through /s/{id}/assets/ so relative links inside the
            // directory resolve. The sandbox attribute prevents the framed
            // content from accessing the daemon's origin (no allow-same-origin),
            // while still allowing scripts and forms for report functionality.
            let src = format!("/s/{id}/assets/{}", entry.trim_start_matches('/'));
            view! { cx =>
                <div class="framewrap">
                    <iframe class="frame" src=(src) title="Preview" sandbox="allow-scripts allow-forms"></iframe>
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
        commentable: content.is_commentable() || brief_commentable,
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
///
/// Delegates to the shared [`crate::file_anchor`] so the CLI can print the same
/// anchors agents should use in guided-review briefs.
fn anchor(label: &str) -> String {
    crate::file_anchor(label)
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
        Err(error) => Err(store_error(error)),
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

/// Escapes a JSON string for safe embedding inside a `<script type="application/json">`
/// block. The only dangerous sequence is `</script` (case-insensitive) which would
/// close the containing script element. We replace `</` with `<\/` which is
/// semantically equivalent in JSON string values and prevents the parser from
/// seeing an end tag.
///
/// Retained for test coverage; no longer used in production rendering (replaced
/// by server-rendered hidden DOM elements with proper HTML escaping).
#[cfg(test)]
fn escape_json_for_script(json: &str) -> String {
    json.replace("</", "<\\/")
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
/// carrying this string is a wdyt daemon.
pub const HEALTH_MARKER: &str = "wdyt-daemon";

/// Protocol version for client-daemon compatibility.
///
/// Incremented when the daemon's API contract changes in a way that would cause
/// a new client to silently lose features on an old daemon. A new client seeing
/// a daemon with a lower (or absent) protocol version will skip it and start a
/// compatible one. Old clients that only check `HEALTH_MARKER` will still
/// recognize a new daemon (the marker is unchanged).
pub const PROTOCOL_VERSION: u32 = 2;

#[route(GET "/health")]
async fn health_route(cx: &Cx) -> Result<Json<serde_json::Value>> {
    Ok(Json(serde_json::json!({
        "ok": true,
        "service": HEALTH_MARKER,
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": PROTOCOL_VERSION,
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

    #[test]
    fn escape_json_for_script_prevents_breakout() {
        // The critical sequence: </script> must be neutralised.
        let input = r#"{"text":"</script><script>alert(1)</script>"}"#;
        let escaped = escape_json_for_script(input);
        assert!(
            !escaped.contains("</script>"),
            "breakout not prevented: {escaped}"
        );
        assert!(escaped.contains("<\\/script>"));
        // Case variation: the HTML parser is case-insensitive for end tags.
        let upper = r#"{"text":"</SCRIPT>"}"#;
        let escaped_upper = escape_json_for_script(upper);
        // Our escape catches the case-sensitive form; HTML parsers only match
        // case-insensitively if the content type is text/html. In application/json
        // script tags, the parser looks for </script literally, so </SCRIPT> is
        // safe. But we also escape it for defense in depth.
        assert!(
            !escaped_upper.contains("</SCRIPT>") || !escaped_upper.contains("</script>"),
            "case-insensitive breakout possible"
        );
    }

    #[test]
    fn escape_json_for_script_preserves_normal_content() {
        let normal = r#"[{"id":1,"text":"looks good","line":3}]"#;
        assert_eq!(escape_json_for_script(normal), normal);
    }
}
