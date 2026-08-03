//! Sessions: one per "show me this", persisted by the daemon.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use anyhow::{Context as _, Result as AnyResult};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

const STATE_VERSION: u32 = 1;

/// What a session shows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Content {
    /// Source files, syntax highlighted.
    Code { files: Vec<CodeFile> },
    /// A unified diff, reviewable line by line.
    Diff { files: Vec<DiffFile> },
    /// Markdown, rendered.
    Docs { files: Vec<DocFile> },
    /// A directory served as-is.
    Static { root: PathBuf, entry: String },
    /// A server the agent started; wdyt frames it and adds the reply box.
    Demo { port: u16, path: String },
}

impl Content {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Code { .. } => "code",
            Self::Diff { .. } => "diff",
            Self::Docs { .. } => "docs",
            Self::Static { .. } => "static",
            Self::Demo { .. } => "demo",
        }
    }

    /// Whether the content is handed to an iframe, which changes the layout:
    /// the page must not scroll, and the top bar can be collapsed away.
    pub fn is_framed(&self) -> bool {
        matches!(self, Self::Demo { .. } | Self::Static { .. })
    }

    /// Whether lines can be commented on.
    pub fn is_commentable(&self) -> bool {
        matches!(
            self,
            Self::Diff { .. } | Self::Code { .. } | Self::Docs { .. }
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeFile {
    /// Label shown in the file list, usually the path as the agent named it.
    pub label: String,
    /// Highlighted HTML, pre-rendered at submit time so page loads are cheap.
    /// Used by the non-commentable `raw` view; the commentable page renders
    /// per-line from `highlighted` instead so it can interleave comment rows.
    pub html: String,
    /// Per-line highlighted HTML, parallel to `sources`. Pre-rendered once so the
    /// comment-affordance table can be built without re-highlighting.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub highlighted: Vec<String>,
    /// Per-line plain text, one entry per line with no trailing newline. This is
    /// what the server quotes a comment's snippet from, so the page is never
    /// trusted to say what a line said.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    #[serde(default)]
    pub lines: usize,
    /// Language, for display only.
    #[serde(default)]
    pub language: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocFile {
    pub label: String,
    /// Rendered HTML (with `data-sourcepos` attributes when commentable).
    pub html: String,
    /// Per-line plain text of the source markdown, one entry per line with no
    /// trailing newline. Used to quote authoritative snippets for comments on
    /// rendered markdown. Empty for legacy sessions or non-commentable docs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
}

/// One file's worth of a unified diff.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffFile {
    /// The path as the diff names it, preferring the new name for a rename.
    pub label: String,
    /// The hunks, in order.
    pub hunks: Vec<Hunk>,
    pub added: usize,
    pub removed: usize,
    /// Language, for display only.
    pub language: String,
    /// The new-side file in full, one entry per line without the newline.
    ///
    /// A unified diff carries only the lines inside its hunks; the context
    /// between them is what the reader wants next and what the patch does not
    /// contain. This is the file the CLI found in the working tree, kept only
    /// when it agreed with every line the diff already shows — so a session
    /// whose patch describes some other revision simply has nothing to expand.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source: Vec<String>,
}

/// A `@@` hunk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hunk {
    /// The `@@ … @@` line, including any trailing section heading.
    pub header: String,
    pub lines: Vec<DiffLine>,
    /// The old-side line the header declares the hunk starts at.
    #[serde(default)]
    pub old_start: usize,
    /// The new-side line the header declares the hunk starts at.
    ///
    /// Needed on its own because a hunk that only removes lines has no new-side
    /// numbers at all, and its position is then only knowable from the header:
    /// `@@ -6,3 +5,0 @@` deletes old 6-8 from after new line 5. Without it the
    /// hidden lines around such a hunk cannot be told apart.
    #[serde(default)]
    pub new_start: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LineKind {
    Added,
    Removed,
    Context,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffLine {
    pub kind: LineKind,
    /// Line number in the old file; absent for an added line.
    pub old: Option<usize>,
    /// Line number in the new file; absent for a removed line.
    pub new: Option<usize>,
    /// Highlighted HTML of the line's content, without the +/- marker.
    pub html: String,
    /// The line's plain text, stored so a comment can quote it.
    pub text: String,
}

/// A reply typed into the page's reply box.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reply {
    pub text: String,
    #[serde(with = "unix_seconds")]
    pub at: SystemTime,
    /// When the reply was handed to an agent process, so the page can say it
    /// left the daemon. `None` means it is still sitting here uncollected.
    ///
    /// Delivery is not reading. The bytes reaching a CLI process says nothing
    /// about whether a model ever looked at them: an agent that collects a reply
    /// and then dies needing credentials has been delivered to and has read
    /// nothing.
    #[serde(
        default,
        with = "unix_seconds_opt",
        skip_serializing_if = "Option::is_none"
    )]
    pub delivered_at: Option<SystemTime>,
    /// When an agent explicitly acknowledged the reply, saying what it is doing
    /// about it. This is the only evidence that anything is actually working.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ack: Option<Ack>,
}

/// An agent's acknowledgement that it read a reply and is acting on it.
///
/// Deliberately carries a message rather than being a bare flag: "I am working
/// on it" from a live model is the signal, and a flag could be set by the same
/// transport that merely delivered the bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ack {
    /// What the agent says it is doing about the reply.
    pub note: String,
    #[serde(with = "unix_seconds")]
    pub at: SystemTime,
}

/// Which side of a diff a comment is anchored to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    /// A line as it was before the change.
    Old,
    /// A line as it is after the change, including unchanged context.
    New,
}

impl Side {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Old => "old",
            Self::New => "new",
        }
    }

    /// Parses the value the page sends back.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "old" => Some(Self::Old),
            "new" => Some(Self::New),
            _ => None,
        }
    }
}

/// The side a line is addressed by. A removed line only exists in the old file;
/// everything else — added and context alike — is addressed by its new-side
/// number, so a context line has one canonical anchor rather than two.
pub fn side_of(kind: LineKind) -> Side {
    match kind {
        LineKind::Removed => Side::Old,
        LineKind::Added | LineKind::Context => Side::New,
    }
}

/// Where a comment attaches: a file, a line or range of lines, and a side.
///
/// The four travel together everywhere — the page sends them as a unit, the
/// server validates them as a unit, the store records them as a unit — so they
/// are one value rather than four parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anchor {
    /// The file the lines belong to, as it appears in the diff.
    pub file: String,
    /// Explicit comment target identity, separate from display label.
    ///
    /// `Some("brief")` for the guided-review brief, `Some("doc:<n>")` for the
    /// nth docs file. `None` for legacy code/diff comments where `file` alone
    /// is sufficient.
    pub target: Option<String>,
    /// The first line covered, on `side`.
    pub line: usize,
    /// The last line covered, or `None` for a single line.
    pub end_line: Option<usize>,
    pub side: Side,
}

impl Anchor {
    /// An anchor on one line.
    pub fn line(file: String, line: usize, side: Side) -> Self {
        Self {
            file,
            target: None,
            line,
            end_line: None,
            side,
        }
    }

    /// An anchor over a range, with the ends put in order.
    ///
    /// A drag upwards arrives reversed, and the page should not have to normalise
    /// what the model can.
    pub fn range(file: String, from: usize, to: usize, side: Side) -> Self {
        Self {
            file,
            target: None,
            line: from.min(to),
            end_line: Some(from.max(to)),
            side,
        }
    }

    /// The last line covered, which is `line` unless there is a range.
    pub fn last(&self) -> usize {
        self.end_line.unwrap_or(self.line).max(self.line)
    }

    /// How many lines the anchor spans.
    pub fn span(&self) -> usize {
        // Saturating arithmetic: prevents panics when last < line (should not
        // happen after normalisation, but must not panic on adversarial input).
        self.last().saturating_sub(self.line).saturating_add(1)
    }
}

/// The most messages one thread will hold.
///
/// A discussion this long has stopped being a comment on a line, and the cap
/// keeps a runaway agent from growing a session without bound.
const MAX_THREAD_MESSAGES: usize = 100;

/// Who said something in a thread.
///
/// Decided by the route it arrived on, never by the request body: the page posts
/// to `/s/…` and an agent to `/api/sessions/…`, so a page cannot claim to be the
/// agent whose answers the reader is trusting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Author {
    /// The person reviewing.
    User,
    /// The agent that made the session.
    Agent,
}

impl Author {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Agent => "agent",
        }
    }
}

/// One thing said in the discussion under a comment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// Assigned by the store, monotonic within the thread.
    pub id: u64,
    pub from: Author,
    pub text: String,
    #[serde(with = "unix_seconds")]
    pub at: SystemTime,
    /// When an agent process took this message, for the reader's receipt.
    ///
    /// Only ever set on a message from the user: it is the answer to "has anything
    /// picked up my question", and the agent's own messages need no such record.
    #[serde(
        default,
        with = "unix_seconds_opt",
        skip_serializing_if = "Option::is_none"
    )]
    pub delivered_at: Option<SystemTime>,
}

/// A comment on one line of a diff.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    /// Assigned by the store, so the page can address a comment it just made.
    pub id: u64,
    /// The file the line belongs to, as it appears in the diff.
    pub file: String,
    /// Explicit comment target identity, distinct from the display label.
    ///
    /// For guided briefs: `"brief"`. For docs: `"doc:<index>"` where index is the
    /// 0-based position in the files array. For code/diff: absent (legacy
    /// identity uses `file`). This lets the agent distinguish comments on files
    /// with duplicate labels, and disambiguates the guided-review brief from a
    /// doc file that happens to be named `"brief:"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// The first line the comment covers, on `side`.
    pub line: usize,
    /// The last line covered, for a comment dragged across a range.
    ///
    /// Equal to `line` for a single-line comment, which is the common case — so
    /// it is skipped in JSON when there is nothing extra to say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_line: Option<usize>,
    pub side: Side,
    /// The commented lines' own text, so the agent has the context without
    /// re-reading the diff it sent.
    pub snippet: String,
    pub text: String,
    #[serde(with = "unix_seconds")]
    pub at: SystemTime,
    /// The discussion under this comment, oldest first.
    ///
    /// The comment's own `text` is the first thing said and is not repeated here;
    /// a comment with no replies is a question nobody has answered yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replies: Vec<Message>,
    /// When an agent process took the comment itself, as `Message::delivered_at`
    /// records for the rest of the thread.
    #[serde(
        default,
        with = "unix_seconds_opt",
        skip_serializing_if = "Option::is_none"
    )]
    pub delivered_at: Option<SystemTime>,
}

impl Comment {
    /// The last thing said in the thread, and by whom.
    pub fn last(&self) -> (Author, &str) {
        match self.replies.last() {
            Some(message) => (message.from, message.text.as_str()),
            None => (Author::User, self.text.as_str()),
        }
    }

    /// Whether the reader is waiting on an answer: the thread's last word is
    /// theirs.
    pub fn awaiting_agent(&self) -> bool {
        self.last().0 == Author::User
    }
}

/// Everything needed to create a session.
///
/// A struct rather than a growing list of positional arguments: the title and
/// content are always known, and the rest is optional context that most callers
/// leave empty. There is no `Default` because a session with no content is not a
/// meaningful thing to ask for — use [`NewSession::new`] and fill in the rest.
#[derive(Debug, Clone)]
pub struct NewSession {
    pub title: String,
    pub content: Content,
    pub note: Option<String>,
    /// Where the creating agent is running.
    pub origin: crate::zellij_origin::Origin,
    /// The syntax theme the content was highlighted with.
    pub theme: Option<String>,
    /// A guided tour of the work, as rendered HTML.
    pub brief: Option<String>,
    /// Per-line plain text of the brief markdown source, so comments on the
    /// brief can quote authoritative snippets. Stored separately from the
    /// content's doc files because a session's brief and its docs can share
    /// label text without collision.
    pub brief_sources: Vec<String>,
}

impl NewSession {
    /// The two things a session cannot do without.
    pub fn new(title: String, content: Content) -> Self {
        Self {
            title,
            content,
            note: None,
            origin: crate::zellij_origin::Origin::default(),
            theme: None,
            brief: None,
            brief_sources: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    /// Optional longer note from the agent, shown under the title.
    pub note: Option<String>,
    pub content: Content,
    /// Where the agent that created this session is running, so a notification
    /// says which of several agents is asking.
    pub origin: crate::zellij_origin::Origin,
    /// A guided tour of the work, written by the agent and rendered above the
    /// content: what changed, why, and what to look at first.
    ///
    /// Stored as rendered HTML because the markdown is turned into it by the CLI,
    /// which is where the syntax theme for fenced code is known.
    pub brief: Option<String>,
    /// Per-line plain text of the brief markdown source, for authoritative
    /// snippet lookup on brief comments. A brief and a doc file may share label
    /// text, so comments on them are distinguished by the `file` field: the
    /// brief always uses `"brief:"` as its file identifier.
    pub brief_sources: Vec<String>,
    /// The syntax theme this session's content was highlighted with, when it
    /// differs from the daemon's default.
    ///
    /// Highlighting happens in the CLI but the page's own colours are chosen by
    /// the daemon, so the name has to travel with the session or the two would
    /// disagree — light code in a dark card, or the reverse.
    pub theme: Option<String>,
    #[serde(with = "unix_seconds")]
    pub created: SystemTime,
    /// The reply, once the user sends one. A session accepts a single reply;
    /// the sender is consumed on first use.
    pub reply: Option<Reply>,
    /// Line comments on a diff, oldest first. Unlike the reply these are not
    /// capped at one: commenting on six lines of a diff is the normal case.
    pub comments: Vec<Comment>,
}

impl Session {
    fn expired(&self, ttl: Duration, now: SystemTime) -> bool {
        now.duration_since(self.created).is_ok_and(|age| age > ttl)
    }
}

/// Shared session store. Cloneable; all clones see the same sessions.
#[derive(Clone)]
pub struct Store {
    inner: Arc<Mutex<StoreInner>>,
    counter: Arc<AtomicU64>,
    ttl: Option<Duration>,
    state_path: Option<PathBuf>,
    /// Keeps the exclusive sibling lock held for every clone's lifetime.
    _state_lock: Option<Arc<std::fs::File>>,
}

struct StoreInner {
    sessions: HashMap<String, Session>,
    /// Waiters belong to this daemon process and are deliberately not persisted.
    waiters: HashMap<String, Vec<oneshot::Sender<Reply>>>,
    /// Agents blocked on the next question in a session's threads. Separate from
    /// `waiters` because a reply ends the wait for good while a discussion goes
    /// on: these are woken repeatedly, once per thing the reader says.
    watchers: HashMap<String, Vec<oneshot::Sender<()>>>,
}

#[derive(Deserialize)]
struct StateHeader {
    version: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StateFileV1 {
    version: u32,
    sessions: Vec<Session>,
}

#[derive(Serialize)]
struct StateFileRef<'a> {
    version: u32,
    sessions: Vec<&'a Session>,
}

impl Store {
    /// Creates an in-memory store. Tests and embedded callers that do not need
    /// restart recovery can keep using this constructor.
    pub fn new(ttl: Option<Duration>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(StoreInner {
                sessions: HashMap::new(),
                waiters: HashMap::new(),
                watchers: HashMap::new(),
            })),
            counter: Arc::new(AtomicU64::new(0)),
            ttl,
            state_path: None,
            _state_lock: None,
        }
    }

    /// Opens a durable store, restoring sessions from `state_path` when present.
    ///
    /// Unknown or malformed versions fail rather than silently replacing state
    /// with an empty file. Sessions already beyond the configured TTL are
    /// discarded before the store starts serving.
    pub fn open(state_path: impl Into<PathBuf>, ttl: Option<Duration>) -> AnyResult<Self> {
        let state_path = state_path.into();
        let state_lock = Arc::new(Self::acquire_state_lock(&state_path)?);
        let mut sessions = match std::fs::read(&state_path) {
            Ok(bytes) => Self::decode_state(&state_path, &bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("reading wdyt state at {}", state_path.display()));
            }
        };

        let before = sessions.len();
        Self::evict(&mut sessions, ttl, SystemTime::now());
        if sessions.len() != before {
            Self::write_state(&state_path, &sessions)?;
        }

        Ok(Self {
            inner: Arc::new(Mutex::new(StoreInner {
                sessions,
                waiters: HashMap::new(),
                watchers: HashMap::new(),
            })),
            counter: Arc::new(AtomicU64::new(0)),
            ttl,
            state_path: Some(state_path),
            _state_lock: Some(state_lock),
        })
    }

    /// Inserts a session and returns its id.
    ///
    /// Ids come from a counter mixed with the creation time rather than a
    /// random source, which keeps the dependency list short. They are short
    /// enough to eyeball in a URL and unguessable enough for a loopback-only
    /// server.
    pub fn insert(&self, title: String, note: Option<String>, content: Content) -> String {
        self.insert_new(NewSession {
            note,
            ..NewSession::new(title, content)
        })
    }

    /// Inserts a session described in full.
    ///
    /// Takes a struct rather than a growing list of positional arguments: most
    /// callers care about two or three of these and the rest are `None`.
    pub fn insert_new(&self, new: NewSession) -> String {
        self.try_insert_new(new)
            .expect("failed to insert session into the store")
    }

    /// Fallible insertion for persistent callers that need to report disk
    /// failures instead of terminating.
    pub fn try_insert_new(&self, new: NewSession) -> Result<String, StoreError> {
        let NewSession {
            title,
            note,
            content,
            origin,
            theme,
            brief,
            brief_sources,
        } = new;
        let now = SystemTime::now();
        self.mutate(move |sessions| {
            let id = loop {
                let seq = self.counter.fetch_add(1, Ordering::Relaxed);
                let stamp = now
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos() as u64);
                // Base36 of a time/sequence mix: compact and URL-safe.
                let candidate =
                    base36(stamp.rotate_left(17) ^ seq.wrapping_mul(0x9E37_79B9_7F4A_7C15));
                if !sessions.contains_key(&candidate) {
                    break candidate;
                }
            };

            let session = Session {
                id: id.clone(),
                title,
                note,
                content,
                origin,
                theme,
                brief,
                brief_sources,
                created: now,
                reply: None,
                comments: Vec::new(),
            };

            sessions.insert(id.clone(), session);
            Self::evict(sessions, self.ttl, now);
            Ok(id)
        })
    }

    /// Runs `f` against a session, if it exists.
    pub fn with<T>(&self, id: &str, f: impl FnOnce(&Session) -> T) -> Option<T> {
        self.lock().sessions.get(id).map(f)
    }

    pub fn exists(&self, id: &str) -> bool {
        self.lock().sessions.contains_key(id)
    }

    /// Records a reply and wakes any waiters.
    ///
    /// Returns `Err` if the session is unknown, `Ok(false)` if it already had
    /// a reply. A session takes one reply: this is a "send the agent a note"
    /// box, not a chat.
    pub fn reply(&self, id: &str, text: String) -> Result<bool, StoreError> {
        let reply = Reply {
            text,
            at: SystemTime::now(),
            delivered_at: None,
            ack: None,
        };
        let reply_to_send = reply.clone();
        let accepted = self.mutate(|sessions| {
            let session = sessions.get_mut(id).ok_or(UnknownSession)?;
            if session.reply.is_some() {
                return Ok(false);
            }
            session.reply = Some(reply);
            Ok(true)
        })?;

        if accepted {
            let waiters = self.lock().waiters.remove(id).unwrap_or_default();
            for waiter in waiters {
                // A dropped receiver just means the reply-only wait gave up.
                let _ = waiter.send(reply_to_send.clone());
            }
            // A reply is also an event `recv` is subscribed to: wake anyone
            // streaming the session's updates, not just a reply-only wait.
            self.wake_watchers(id);
        }
        Ok(accepted)
    }

    /// Takes the session's reply if one is waiting undelivered, stamping it.
    ///
    /// Unlike [`Store::mark_delivered`], which re-returns an already-delivered
    /// reply so a re-fetch works, this returns `Some` only the first time — the
    /// take-once discipline `recv` needs so a reply surfaces in the stream once.
    pub fn take_reply(&self, id: &str) -> Result<Option<Reply>, StoreError> {
        self.mutate(|sessions| {
            let session = sessions.get_mut(id).ok_or(UnknownSession)?;
            match session.reply.as_mut() {
                Some(reply) if reply.delivered_at.is_none() => {
                    reply.delivered_at = Some(SystemTime::now());
                    Ok(Some(reply.clone()))
                }
                _ => Ok(None),
            }
        })
    }

    /// Marks a session's reply as delivered to an agent process, returning it.
    ///
    /// The first call stamps `delivered_at`; later ones return the reply with the
    /// original stamp, so a re-fetch does not look like a fresh delivery. This
    /// claims only that the bytes left the daemon — see [`Store::ack`] for the
    /// claim that something read them.
    pub fn mark_delivered(&self, id: &str) -> Result<Option<Reply>, StoreError> {
        self.mutate(|sessions| {
            let session = sessions.get_mut(id).ok_or(UnknownSession)?;
            let Some(reply) = session.reply.as_mut() else {
                return Ok(None);
            };
            reply.delivered_at.get_or_insert_with(SystemTime::now);
            Ok(Some(reply.clone()))
        })
    }

    /// Records an agent's acknowledgement that it read the reply.
    ///
    /// Returns `Ok(false)` when there is no reply to acknowledge: an ack without
    /// one is a bug in the agent, not something to record. A later ack replaces
    /// an earlier one, so an agent can report progress more than once.
    pub fn ack(&self, id: &str, note: String) -> Result<bool, StoreError> {
        self.mutate(|sessions| {
            let session = sessions.get_mut(id).ok_or(UnknownSession)?;
            let Some(reply) = session.reply.as_mut() else {
                return Ok(false);
            };
            // An ack implies delivery even if the agent got the text some other way.
            reply.delivered_at.get_or_insert_with(SystemTime::now);
            reply.ack = Some(Ack {
                note,
                at: SystemTime::now(),
            });
            Ok(true)
        })
    }

    /// Adds a line comment, returning it with its assigned id.
    pub fn comment(
        &self,
        id: &str,
        anchor: Anchor,
        snippet: String,
        text: String,
    ) -> Result<Comment, StoreError> {
        let Anchor {
            file,
            target,
            line,
            end_line,
            side,
        } = anchor;
        let made = self.mutate(|sessions| {
            let session = sessions.get_mut(id).ok_or(UnknownSession)?;
            // Ids are per session and monotonic, so a page can address the comment
            // it just made without a round trip for a name.
            let next = session.comments.last().map_or(1, |last| last.id + 1);
            let comment = Comment {
                id: next,
                file,
                target,
                line,
                // A range that covers one line is not a range: normalised away so
                // the common case has one representation.
                end_line: end_line.filter(|end| *end > line),
                side,
                snippet,
                text,
                at: SystemTime::now(),
                replies: Vec::new(),
                delivered_at: None,
            };
            session.comments.push(comment.clone());
            Ok(comment)
        });
        // A new comment is a new question, and an agent watching the session is
        // waiting for exactly that.
        if made.is_ok() {
            self.wake_watchers(id);
        }
        made
    }

    /// Adds a message to the discussion under a comment.
    ///
    /// The author comes from the caller — the route the message arrived on — not
    /// from anything in the message itself.
    pub fn message(
        &self,
        id: &str,
        comment_id: u64,
        from: Author,
        text: String,
    ) -> Result<Message, StoreError> {
        let message = self.mutate(|sessions| {
            let session = sessions.get_mut(id).ok_or(UnknownSession)?;
            let comment = session
                .comments
                .iter_mut()
                .find(|comment| comment.id == comment_id)
                .ok_or(UnknownSession)?;
            if comment.replies.len() >= MAX_THREAD_MESSAGES {
                return Err(UnknownSession);
            }
            let next = comment.replies.last().map_or(1, |last| last.id + 1);
            let message = Message {
                id: next,
                from,
                text,
                at: SystemTime::now(),
                delivered_at: None,
            };
            comment.replies.push(message.clone());
            Ok(message)
        })?;
        // Only the reader's own words are something an agent is waiting for.
        if from == Author::User {
            self.wake_watchers(id);
        }
        Ok(message)
    }

    /// The oldest thing the reader has said that no agent has taken yet, stamped
    /// as taken.
    ///
    /// The comment's own text counts as the first message in its thread, so a
    /// comment nobody has answered is what this returns first. Stamping here is
    /// the same discipline as `mark_delivered`: the bytes are going out now, and
    /// the page can say so — while what the agent *said* about it is the reply it
    /// posts back, which is the only thing that turns the thread green.
    pub fn take_question(&self, id: &str) -> Result<Option<(Comment, Message)>, StoreError> {
        self.mutate(|sessions| {
            let session = sessions.get_mut(id).ok_or(UnknownSession)?;
            let now = SystemTime::now();
            for comment in &mut session.comments {
                if comment.delivered_at.is_none() {
                    comment.delivered_at = Some(now);
                    // The head of the thread, presented as its first message so a
                    // caller has one shape to handle.
                    let head = Message {
                        id: 0,
                        from: Author::User,
                        text: comment.text.clone(),
                        at: comment.at,
                        delivered_at: Some(now),
                    };
                    return Ok(Some((comment.clone(), head)));
                }
                if let Some(message) = comment
                    .replies
                    .iter_mut()
                    .find(|m| m.from == Author::User && m.delivered_at.is_none())
                {
                    message.delivered_at = Some(now);
                    let taken = message.clone();
                    return Ok(Some((comment.clone(), taken)));
                }
            }
            Ok(None)
        })
    }

    /// Whether the reader has said anything undelivered in this session — an
    /// uncollected reply, or a line comment/follow-up no agent has taken.
    ///
    /// This is the "is there anything for `recv` to return right now?" check, so
    /// it spans both kinds of event, not just line questions.
    fn has_pending_event(session: &Session) -> bool {
        let undelivered_reply = session
            .reply
            .as_ref()
            .is_some_and(|reply| reply.delivered_at.is_none());
        undelivered_reply
            || session.comments.iter().any(|comment| {
                comment.delivered_at.is_none()
                    || comment
                        .replies
                        .iter()
                        .any(|m| m.from == Author::User && m.delivered_at.is_none())
            })
    }

    /// Registers interest in the next thing the reader does — a reply or a line
    /// comment alike.
    ///
    /// Resolves immediately when something is already waiting, so an agent that
    /// starts watching after the event does not hang. This is the one primitive
    /// behind `recv`: the caller then takes whichever event is pending.
    pub fn watch(&self, id: &str) -> Result<oneshot::Receiver<()>, UnknownSession> {
        let (tx, rx) = oneshot::channel();
        let mut inner = self.lock();
        let session = inner.sessions.get(id).ok_or(UnknownSession)?;
        if Self::has_pending_event(session) {
            let _ = tx.send(());
        } else {
            inner.watchers.entry(id.to_owned()).or_default().push(tx);
        }
        Ok(rx)
    }

    fn wake_watchers(&self, id: &str) {
        let watchers = self.lock().watchers.remove(id).unwrap_or_default();
        for watcher in watchers {
            // A dropped receiver just means the `recv` subscription gave up.
            let _ = watcher.send(());
        }
    }

    /// Every session that has a reply, newest first.
    ///
    /// This is how a `--wait` that timed out is recovered: the reply outlives
    /// the process that was waiting for it, so an agent coming back later can
    /// find one it never saw. Read state is included rather than filtered on,
    /// so the caller decides whether it wants the unread ones only.
    pub fn replied(&self) -> Vec<(String, String, Reply, usize)> {
        let mut items: Vec<_> = self
            .lock()
            .sessions
            .values()
            .filter_map(|s| {
                let reply = s.reply.clone()?;
                Some((
                    s.id.clone(),
                    s.title.clone(),
                    reply,
                    s.comments.len(),
                    s.created,
                ))
            })
            .collect();
        items.sort_by_key(|item| std::cmp::Reverse(item.4));
        items
            .into_iter()
            .map(|(id, title, reply, comments, _)| (id, title, reply, comments))
            .collect()
    }

    /// A session's comments, oldest first.
    pub fn comments(&self, id: &str) -> Result<Vec<Comment>, UnknownSession> {
        Ok(self
            .lock()
            .sessions
            .get(id)
            .ok_or(UnknownSession)?
            .comments
            .clone())
    }

    /// Registers interest in a session's reply.
    ///
    /// Resolves immediately when a reply already exists, so a `wait` that
    /// starts after the user has replied does not hang.
    pub fn subscribe(&self, id: &str) -> Result<oneshot::Receiver<Reply>, UnknownSession> {
        let (tx, rx) = oneshot::channel();
        let mut inner = self.lock();
        let session = inner.sessions.get(id).ok_or(UnknownSession)?;
        match &session.reply {
            Some(reply) => {
                let _ = tx.send(reply.clone());
            }
            None => inner.waiters.entry(id.to_owned()).or_default().push(tx),
        }
        Ok(rx)
    }

    /// Session ids and titles, newest first. For `wdyt list`.
    pub fn summaries(&self) -> Vec<(String, String, &'static str, bool)> {
        let mut items: Vec<_> = self
            .lock()
            .sessions
            .values()
            .map(|s| {
                (
                    s.id.clone(),
                    s.title.clone(),
                    s.content.kind(),
                    s.reply.is_some(),
                    s.created,
                )
            })
            .collect();
        items.sort_by_key(|item| std::cmp::Reverse(item.4));
        items
            .into_iter()
            .map(|(id, title, kind, replied, _)| (id, title, kind, replied))
            .collect()
    }

    /// Drops sessions past the TTL. Called on insert, which is often enough
    /// for a tool whose sessions are created by hand.
    fn evict(sessions: &mut HashMap<String, Session>, ttl: Option<Duration>, now: SystemTime) {
        let Some(ttl) = ttl else { return };
        sessions.retain(|_, session| !session.expired(ttl, now));
    }

    /// Applies a durable mutation. Persistent stores write a complete candidate
    /// snapshot before exposing it in memory, so a failed write cannot leave the
    /// running daemon ahead of what a restart would restore.
    fn mutate<T>(
        &self,
        change: impl FnOnce(&mut HashMap<String, Session>) -> Result<T, UnknownSession>,
    ) -> Result<T, StoreError> {
        let mut inner = self.lock();
        let output = if let Some(path) = &self.state_path {
            let mut candidate = inner.sessions.clone();
            let output = change(&mut candidate)?;
            Self::write_state(path, &candidate).map_err(StoreError::Persistence)?;
            inner.sessions = candidate;
            output
        } else {
            change(&mut inner.sessions)?
        };

        let StoreInner {
            sessions,
            waiters,
            watchers,
        } = &mut *inner;
        // A session that has gone takes its waiters and watchers with it.
        waiters.retain(|id, _| sessions.contains_key(id));
        watchers.retain(|id, _| sessions.contains_key(id));
        Ok(output)
    }

    fn decode_state(path: &Path, bytes: &[u8]) -> AnyResult<HashMap<String, Session>> {
        let header: StateHeader = serde_json::from_slice(bytes)
            .with_context(|| format!("parsing wdyt state header at {}", path.display()))?;
        anyhow::ensure!(
            header.version == STATE_VERSION,
            "unsupported wdyt state version {} at {} (this binary supports version {})",
            header.version,
            path.display(),
            STATE_VERSION
        );

        let state: StateFileV1 = serde_json::from_slice(bytes)
            .with_context(|| format!("parsing wdyt state at {}", path.display()))?;
        anyhow::ensure!(
            state.version == STATE_VERSION,
            "wdyt state version changed while parsing {}",
            path.display()
        );
        let mut sessions = HashMap::with_capacity(state.sessions.len());
        for session in state.sessions {
            let id = session.id.clone();
            anyhow::ensure!(
                sessions.insert(id.clone(), session).is_none(),
                "duplicate session id {id:?} in {}",
                path.display()
            );
        }
        Ok(sessions)
    }

    fn acquire_state_lock(path: &Path) -> AnyResult<std::fs::File> {
        let parent = path
            .parent()
            .context("wdyt state path has no parent directory")?;
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating wdyt state directory {}", parent.display()))?;

        let mut lock_name = path
            .file_name()
            .context("wdyt state path has no file name")?
            .to_os_string();
        lock_name.push(".lock");
        let lock_path = path.with_file_name(lock_name);

        let mut options = std::fs::OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let file = options
            .open(&lock_path)
            .with_context(|| format!("opening wdyt state lock {}", lock_path.display()))?;
        file.try_lock().with_context(|| {
            format!(
                "locking wdyt state at {} (another daemon may already be using it)",
                path.display()
            )
        })?;
        Ok(file)
    }

    fn write_state(path: &Path, sessions: &HashMap<String, Session>) -> AnyResult<()> {
        let parent = path
            .parent()
            .context("wdyt state path has no parent directory")?;
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating wdyt state directory {}", parent.display()))?;

        let mut ordered: Vec<_> = sessions.values().collect();
        ordered.sort_by_key(|session| (session.created, session.id.as_str()));
        let state = StateFileRef {
            version: STATE_VERSION,
            sessions: ordered,
        };
        let mut bytes = serde_json::to_vec(&state).context("serializing wdyt state")?;
        bytes.push(b'\n');

        let temp_path = path.with_extension(
            path.extension()
                .map(|extension| format!("{}.tmp", extension.to_string_lossy()))
                .unwrap_or_else(|| "tmp".to_owned()),
        );
        let mut options = std::fs::OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temp_path)
            .with_context(|| format!("opening temporary state file {}", temp_path.display()))?;
        file.write_all(&bytes)
            .with_context(|| format!("writing temporary state file {}", temp_path.display()))?;
        file.sync_all()
            .with_context(|| format!("syncing temporary state file {}", temp_path.display()))?;
        drop(file);
        std::fs::rename(&temp_path, path).with_context(|| {
            format!(
                "replacing wdyt state {} with {}",
                path.display(),
                temp_path.display()
            )
        })?;
        Ok(())
    }

    /// A poisoned lock means another thread panicked while holding it. The
    /// guarded map is still structurally sound, so recovering keeps the daemon
    /// serving instead of turning one bad request into a dead server.
    fn lock(&self) -> std::sync::MutexGuard<'_, StoreInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[derive(Debug)]
pub enum StoreError {
    UnknownSession,
    Persistence(anyhow::Error),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownSession => f.write_str("no such session"),
            Self::Persistence(error) => write!(f, "persisting wdyt state: {error:#}"),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::UnknownSession => None,
            Self::Persistence(error) => Some(error.as_ref()),
        }
    }
}

impl From<UnknownSession> for StoreError {
    fn from(_: UnknownSession) -> Self {
        Self::UnknownSession
    }
}

#[derive(Debug, Clone, Copy)]
pub struct UnknownSession;

impl std::fmt::Display for UnknownSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("no such session")
    }
}

impl std::error::Error for UnknownSession {}

fn base36(mut value: u64) -> String {
    const ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if value == 0 {
        return "0".to_owned();
    }
    let mut out = Vec::new();
    while value > 0 {
        out.push(ALPHABET[(value % 36) as usize]);
        value /= 36;
    }
    out.reverse();
    // Six characters is plenty to distinguish sessions in one daemon run.
    out.truncate(6);
    String::from_utf8(out).expect("alphabet is ASCII")
}

/// Serializes a `SystemTime` as whole seconds since the epoch, so the JSON that
/// `wdyt recv` prints is easy for an agent to read.
mod unix_seconds {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(time: &SystemTime, s: S) -> Result<S::Ok, S::Error> {
        let secs = time
            .duration_since(UNIX_EPOCH)
            .map_err(serde::ser::Error::custom)?
            .as_secs();
        s.serialize_u64(secs)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<SystemTime, D::Error> {
        let secs = u64::deserialize(d)?;
        Ok(UNIX_EPOCH + Duration::from_secs(secs))
    }
}

/// The same, for an optional timestamp.
mod unix_seconds_opt {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(time: &Option<SystemTime>, s: S) -> Result<S::Ok, S::Error> {
        match time {
            Some(time) => super::unix_seconds::serialize(time, s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<SystemTime>, D::Error> {
        Ok(Option::<u64>::deserialize(d)?.map(|secs| UNIX_EPOCH + Duration::from_secs(secs)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demo() -> Content {
        Content::Demo {
            port: 3005,
            path: "/".to_owned(),
        }
    }

    /// A session with one comment on a line, which is a question nobody has
    /// answered yet.
    fn asked() -> (Store, String, u64) {
        let store = Store::new(None);
        let id = store.insert("t".into(), None, demo());
        let comment = store
            .comment(
                &id,
                Anchor::line("a.rs".into(), 3, Side::New),
                "let x = y.clone();".into(),
                "why is this a clone?".into(),
            )
            .unwrap();
        (store, id, comment.id)
    }

    /// A comment is the first message in its thread: an agent asking for the next
    /// question gets the comment itself, once, and then nothing.
    #[test]
    fn a_comment_is_a_question_until_an_agent_takes_it() {
        let (store, id, thread) = asked();
        let (comment, message) = store.take_question(&id).unwrap().expect("a question");
        assert_eq!(comment.id, thread);
        assert_eq!(message.text, "why is this a clone?");
        // Id 0 marks the comment's own text rather than a reply to it.
        assert_eq!(message.id, 0);
        assert_eq!(message.from, Author::User);
        assert!(message.delivered_at.is_some(), "taking it is delivery");
        // Taken once: a second agent process must not answer the same question.
        assert!(store.take_question(&id).unwrap().is_none());
    }

    /// The agent's answer is not something the agent is waiting for; the reader's
    /// follow-up is.
    #[test]
    fn only_the_readers_words_are_questions() {
        let (store, id, thread) = asked();
        store.take_question(&id).unwrap().expect("the comment");

        store
            .message(&id, thread, Author::Agent, "because it is cheap".into())
            .unwrap();
        assert!(
            store.take_question(&id).unwrap().is_none(),
            "an agent's own answer came back as a question"
        );

        store
            .message(&id, thread, Author::User, "fair enough".into())
            .unwrap();
        let (_, taken) = store.take_question(&id).unwrap().expect("the follow-up");
        assert_eq!(taken.text, "fair enough");
        assert!(taken.id > 0, "a follow-up is not the comment itself");
        assert!(store.take_question(&id).unwrap().is_none());
    }

    /// Who has the last word says who is waiting on whom, which is what the
    /// page's receipt shows.
    #[test]
    fn the_last_word_says_who_is_waiting() {
        let (store, id, thread) = asked();
        let waiting = &store.comments(&id).unwrap()[0];
        assert!(waiting.awaiting_agent());
        assert_eq!(waiting.last(), (Author::User, "why is this a clone?"));

        store
            .message(&id, thread, Author::Agent, "because it is cheap".into())
            .unwrap();
        let answered = &store.comments(&id).unwrap()[0];
        assert!(!answered.awaiting_agent());
        assert_eq!(answered.last(), (Author::Agent, "because it is cheap"));
    }

    /// An agent that starts watching after the question was asked must not hang:
    /// the question is already there to be had.
    #[tokio::test]
    async fn watching_resolves_for_a_question_already_asked() {
        let (store, id, _) = asked();
        store
            .watch(&id)
            .unwrap()
            .await
            .expect("resolved without waiting");
    }

    /// And one that starts first is woken by the comment.
    #[tokio::test]
    async fn watching_is_woken_by_a_new_comment() {
        let store = Store::new(None);
        let id = store.insert("t".into(), None, demo());
        let watching = store.watch(&id).unwrap();

        let writer = store.clone();
        let session = id.clone();
        tokio::spawn(async move {
            writer
                .comment(
                    &session,
                    Anchor::line("a.rs".into(), 1, Side::New),
                    "x".into(),
                    "and this?".into(),
                )
                .unwrap();
        });
        tokio::time::timeout(Duration::from_secs(5), watching)
            .await
            .expect("woken within five seconds")
            .expect("sender not dropped");
    }

    /// A follow-up wakes a watcher too: a discussion is not one question.
    #[tokio::test]
    async fn watching_is_woken_again_by_a_follow_up() {
        let (store, id, thread) = asked();
        store.take_question(&id).unwrap().expect("the comment");

        let watching = store.watch(&id).unwrap();
        let writer = store.clone();
        let session = id.clone();
        tokio::spawn(async move {
            writer
                .message(&session, thread, Author::User, "one more thing".into())
                .unwrap();
        });
        tokio::time::timeout(Duration::from_secs(5), watching)
            .await
            .expect("woken within five seconds")
            .expect("sender not dropped");
    }

    #[test]
    fn a_thread_cannot_grow_without_bound() {
        let (store, id, thread) = asked();
        for n in 0..MAX_THREAD_MESSAGES {
            store
                .message(&id, thread, Author::Agent, format!("{n}"))
                .expect("within the cap");
        }
        assert!(
            store
                .message(&id, thread, Author::Agent, "one too many".into())
                .is_err()
        );
        assert_eq!(
            store.comments(&id).unwrap()[0].replies.len(),
            MAX_THREAD_MESSAGES
        );
    }

    #[test]
    fn a_message_needs_a_thread_that_exists() {
        let (store, id, _) = asked();
        assert!(
            store
                .message(&id, 99, Author::Agent, "hello?".into())
                .is_err()
        );
        assert!(
            store
                .message("nope", 1, Author::Agent, "hello?".into())
                .is_err()
        );
    }

    #[test]
    fn insert_then_read() {
        let store = Store::new(None);
        let id = store.insert("t".to_owned(), None, demo());
        assert!(store.exists(&id));
        assert_eq!(store.with(&id, |s| s.title.clone()).unwrap(), "t");
        assert!(!store.exists("nope"));
    }

    #[test]
    fn ids_are_unique() {
        let store = Store::new(None);
        let ids: std::collections::HashSet<_> = (0..500)
            .map(|_| store.insert("t".into(), None, demo()))
            .collect();
        assert_eq!(ids.len(), 500, "ids collided");
    }

    #[test]
    fn accepts_one_reply_only() {
        let store = Store::new(None);
        let id = store.insert("t".into(), None, demo());
        assert!(store.reply(&id, "first".into()).unwrap());
        assert!(!store.reply(&id, "second".into()).unwrap());
        assert_eq!(
            store.with(&id, |s| s.reply.clone()).unwrap().unwrap().text,
            "first"
        );
    }

    #[test]
    fn reply_to_unknown_session_errors() {
        let store = Store::new(None);
        assert!(store.reply("nope", "hi".into()).is_err());
        assert!(store.subscribe("nope").is_err());
    }

    #[tokio::test]
    async fn subscribe_receives_later_reply() {
        let store = Store::new(None);
        let id = store.insert("t".into(), None, demo());
        let rx = store.subscribe(&id).unwrap();
        store.reply(&id, "hello".into()).unwrap();
        assert_eq!(rx.await.unwrap().text, "hello");
    }

    #[tokio::test]
    async fn subscribe_after_reply_resolves_immediately() {
        let store = Store::new(None);
        let id = store.insert("t".into(), None, demo());
        store.reply(&id, "early".into()).unwrap();
        assert_eq!(store.subscribe(&id).unwrap().await.unwrap().text, "early");
    }

    #[test]
    fn expired_sessions_are_evicted_on_insert() {
        let store = Store::new(Some(Duration::from_secs(60)));
        let stale = store.insert("stale".into(), None, demo());
        // Backdate past the TTL.
        store.lock().sessions.get_mut(&stale).unwrap().created =
            SystemTime::now() - Duration::from_secs(3600);

        let fresh = store.insert("fresh".into(), None, demo());
        assert!(!store.exists(&stale), "stale session survived");
        assert!(store.exists(&fresh));
    }

    #[test]
    fn a_reply_is_undelivered_until_collected() {
        let store = Store::new(None);
        let id = store.insert("t".into(), None, demo());
        store.reply(&id, "hi".into()).unwrap();

        let fresh = store.with(&id, |s| s.reply.clone()).unwrap().unwrap();
        assert!(
            fresh.delivered_at.is_none(),
            "a fresh reply looked delivered"
        );

        let first = store.mark_delivered(&id).unwrap().unwrap();
        let stamp = first.delivered_at.expect("collecting stamps delivered_at");

        // A second collection is a re-fetch, not a fresh delivery: the page would
        // otherwise show the receipt moving.
        let again = store.mark_delivered(&id).unwrap().unwrap();
        assert_eq!(again.delivered_at, Some(stamp));
    }

    #[test]
    fn delivery_is_not_a_claim_that_anything_read_the_reply() {
        // The distinction the whole ack flow exists for: an agent that collects a
        // reply and then dies — needing credentials, say — has been delivered to
        // and has read nothing. Reporting that as "read" is a lie the user cannot
        // check, and it hides exactly the failure worth seeing.
        let store = Store::new(None);
        let id = store.insert("t".into(), None, demo());
        store.reply(&id, "have a look".into()).unwrap();

        let delivered = store.mark_delivered(&id).unwrap().unwrap();
        assert!(delivered.delivered_at.is_some());
        assert!(delivered.ack.is_none(), "delivery invented an ack");

        assert!(store.ack(&id, "on it: rerunning the build".into()).unwrap());
        let acked = store.with(&id, |s| s.reply.clone()).unwrap().unwrap();
        let ack = acked.ack.expect("the ack was recorded");
        assert_eq!(ack.note, "on it: rerunning the build");
    }

    #[test]
    fn an_ack_can_be_replaced_so_an_agent_can_report_progress() {
        let store = Store::new(None);
        let id = store.insert("t".into(), None, demo());
        store.reply(&id, "hi".into()).unwrap();

        store.ack(&id, "starting".into()).unwrap();
        store.ack(&id, "done, pushed".into()).unwrap();
        let reply = store.with(&id, |s| s.reply.clone()).unwrap().unwrap();
        assert_eq!(reply.ack.unwrap().note, "done, pushed");
    }

    #[test]
    fn an_ack_implies_delivery() {
        // An agent may have been handed the text some other way — an inbox dump
        // pasted into a prompt. Acking is the stronger claim, so it covers both.
        let store = Store::new(None);
        let id = store.insert("t".into(), None, demo());
        store.reply(&id, "hi".into()).unwrap();

        store.ack(&id, "read it".into()).unwrap();
        let reply = store.with(&id, |s| s.reply.clone()).unwrap().unwrap();
        assert!(reply.delivered_at.is_some(), "an ack left it undelivered");
    }

    #[test]
    fn acking_or_collecting_with_no_reply_is_not_an_error() {
        let store = Store::new(None);
        let id = store.insert("t".into(), None, demo());
        assert!(store.mark_delivered(&id).unwrap().is_none());
        assert!(store.mark_delivered("nope").is_err());
        // Nothing to acknowledge is a `false`, not a panic; but an unknown
        // session is still a hard error.
        assert!(!store.ack(&id, "hi".into()).unwrap());
        assert!(store.ack("nope", "hi".into()).is_err());
    }

    #[test]
    fn the_inbox_holds_replies_the_waiter_never_saw() {
        // The point of the inbox: a `--wait` that timed out did not lose the
        // reply, because the session outlives the process that waited.
        let store = Store::new(None);
        let seen = store.insert("seen".into(), None, demo());
        let missed = store.insert("missed".into(), None, demo());
        let silent = store.insert("silent".into(), None, demo());

        store.reply(&seen, "one".into()).unwrap();
        store.reply(&missed, "two".into()).unwrap();
        store.ack(&seen, "read it".into()).unwrap();

        let ids: Vec<_> = store.replied().into_iter().map(|item| item.0).collect();
        assert!(ids.contains(&seen) && ids.contains(&missed));
        assert!(!ids.contains(&silent), "a session with no reply appeared");

        let unread: Vec<_> = store
            .replied()
            .into_iter()
            .filter(|(_, _, reply, _)| reply.ack.is_none())
            .map(|item| item.0)
            .collect();
        assert_eq!(unread, vec![missed]);
    }

    #[test]
    fn a_collected_but_unacked_reply_stays_in_the_unread_inbox() {
        // The dropped-agent case. If "unread" filtered on delivery, the reply a
        // dead process collected would vanish from the inbox and never be acted
        // on — the one thing the inbox exists to prevent.
        let store = Store::new(None);
        let id = store.insert("t".into(), None, demo());
        store.reply(&id, "did you get this?".into()).unwrap();
        store.mark_delivered(&id).unwrap();

        let unread: Vec<_> = store
            .replied()
            .into_iter()
            .filter(|(_, _, reply, _)| reply.ack.is_none())
            .map(|item| item.0)
            .collect();
        assert_eq!(unread, vec![id], "a collected reply fell out of the inbox");
    }

    #[test]
    fn comments_accumulate_with_rising_ids() {
        // Unlike the reply, comments are not capped at one: six comments on a
        // diff is the normal case.
        let store = Store::new(None);
        let id = store.insert("t".into(), None, demo());
        let first = store
            .comment(
                &id,
                Anchor::line("a.rs".into(), 3, Side::New),
                "let x = 1;".into(),
                "why?".into(),
            )
            .unwrap();
        let second = store
            .comment(
                &id,
                Anchor::line("a.rs".into(), 9, Side::Old),
                "gone".into(),
                "keep this".into(),
            )
            .unwrap();
        assert!(second.id > first.id, "ids did not rise");
        assert_eq!(store.comments(&id).unwrap().len(), 2);
        assert!(
            store
                .comment(
                    "nope",
                    Anchor::line("a".into(), 1, Side::New),
                    String::new(),
                    "x".into(),
                )
                .is_err()
        );
    }

    #[test]
    fn a_range_that_covers_one_line_is_not_a_range() {
        // Normalised on the way in, so the common case has one representation and
        // the page does not have to render "L4–L4".
        let store = Store::new(None);
        let id = store.insert("t".into(), None, demo());

        let single = store
            .comment(
                &id,
                Anchor::range("a.rs".into(), 4, 4, Side::New),
                "x".into(),
                "hm".into(),
            )
            .unwrap();
        assert_eq!(single.end_line, None, "a one-line range was kept");

        let ranged = store
            .comment(
                &id,
                Anchor::range("a.rs".into(), 4, 8, Side::New),
                "x".into(),
                "hm".into(),
            )
            .unwrap();
        assert_eq!(ranged.end_line, Some(8));
    }

    #[test]
    fn an_anchor_puts_a_dragged_range_in_order() {
        // Dragging upwards is as natural as downwards, so the ends arrive
        // reversed. Normalising here means no caller has to think about it.
        let up = Anchor::range("a.rs".into(), 9, 2, Side::New);
        assert_eq!((up.line, up.end_line), (2, Some(9)));
        assert_eq!(up.last(), 9);
        assert_eq!(up.span(), 8);

        let down = Anchor::range("a.rs".into(), 2, 9, Side::New);
        assert_eq!(up, down, "direction changed the anchor");

        // A single line spans itself, so a length check needs no special case.
        let one = Anchor::line("a.rs".into(), 5, Side::Old);
        assert_eq!(one.last(), 5);
        assert_eq!(one.span(), 1);
    }

    #[test]
    fn a_context_line_is_addressed_on_the_new_side_only() {
        // A context line has a number on both sides, so without one canonical
        // anchor the same comment could be filed twice.
        assert_eq!(side_of(LineKind::Context), Side::New);
        assert_eq!(side_of(LineKind::Added), Side::New);
        assert_eq!(side_of(LineKind::Removed), Side::Old);
        assert_eq!(Side::parse("old"), Some(Side::Old));
        assert_eq!(Side::parse("both"), None);
    }

    #[test]
    fn anchor_span_does_not_panic_on_edge_cases() {
        // line = usize::MAX with no range: span should be 1.
        let a = Anchor::line("x".into(), usize::MAX, Side::New);
        assert_eq!(a.span(), 1);

        // line = 1, end = usize::MAX: must not overflow.
        let b = Anchor::range("x".into(), 1, usize::MAX, Side::New);
        assert!(b.span() > 0);

        // line = 0 is not valid but must not panic: span returns 1.
        let c = Anchor::line("x".into(), 0, Side::New);
        assert_eq!(c.span(), 1);
    }

    #[test]
    fn summaries_are_newest_first() {
        let store = Store::new(None);
        let first = store.insert("first".into(), None, demo());
        std::thread::sleep(Duration::from_millis(5));
        let second = store.insert("second".into(), None, demo());
        let ids: Vec<_> = store.summaries().into_iter().map(|s| s.0).collect();
        assert_eq!(ids, vec![second, first]);
    }

    #[test]
    fn persistent_store_restores_the_complete_reply_lifecycle() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sessions.json");
        let store = Store::open(&path, None).unwrap();
        let id = store.insert_new(NewSession {
            note: Some("look closely".into()),
            origin: crate::zellij_origin::Origin {
                cwd: Some("/work/repo".into()),
                ..Default::default()
            },
            theme: Some("GitHub".into()),
            brief: Some("<p>start here</p>".into()),
            brief_sources: vec!["start here".into()],
            ..NewSession::new("durable".into(), demo())
        });
        store.reply(&id, "ship it".into()).unwrap();
        store.mark_delivered(&id).unwrap();
        store.ack(&id, "running the final checks".into()).unwrap();
        store
            .comment(
                &id,
                Anchor::line("src/lib.rs".into(), 4, Side::New),
                "let answer = 42;".into(),
                "why 42?".into(),
            )
            .unwrap();
        drop(store);

        let restored = Store::open(&path, None).unwrap();
        let session = restored.with(&id, Clone::clone).expect("session restored");
        assert_eq!(session.title, "durable");
        assert_eq!(session.note.as_deref(), Some("look closely"));
        assert_eq!(session.origin.cwd.as_deref(), Some("/work/repo"));
        assert_eq!(session.theme.as_deref(), Some("GitHub"));
        assert_eq!(session.brief_sources, vec!["start here"]);
        assert_eq!(session.comments.len(), 1);
        let reply = session.reply.expect("reply restored");
        assert_eq!(reply.text, "ship it");
        assert!(reply.delivered_at.is_some());
        assert_eq!(
            reply.ack.expect("ack restored").note,
            "running the final checks"
        );

        let file: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(file["version"], STATE_VERSION);
        assert_eq!(file["sessions"][0]["id"], id);
        assert!(
            !path.with_extension("json.tmp").exists(),
            "atomic-write temporary file was left behind"
        );
    }

    #[test]
    fn unsupported_state_version_is_preserved_and_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sessions.json");
        let bytes = br#"{"version":2,"sessions":[]}"#;
        std::fs::write(&path, bytes).unwrap();

        let error = Store::open(&path, None).err().expect("version 2 accepted");
        assert!(
            error
                .to_string()
                .contains("unsupported wdyt state version 2"),
            "{error:#}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    #[test]
    fn persistent_store_excludes_a_second_writer_for_every_clones_lifetime() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sessions.json");
        let store = Store::open(&path, None).unwrap();
        let clone = store.clone();
        drop(store);

        let error = Store::open(&path, None)
            .err()
            .expect("second writer acquired the state lock");
        assert!(
            error
                .to_string()
                .contains("another daemon may already be using it"),
            "{error:#}"
        );

        drop(clone);
        Store::open(&path, None).expect("state lock was not released on final drop");
    }

    #[test]
    fn expired_sessions_are_pruned_when_state_is_loaded() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sessions.json");
        let store = Store::open(&path, None).unwrap();
        let id = store.insert("old".into(), None, demo());
        drop(store);

        let mut file: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        file["sessions"][0]["created"] = 0.into();
        std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();

        let restored = Store::open(&path, Some(Duration::from_secs(60))).unwrap();
        assert!(!restored.exists(&id));
        let rewritten: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(rewritten["sessions"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn failed_persistence_does_not_change_live_state() {
        let directory = tempfile::tempdir().unwrap();
        let state_directory = directory.path().join("state");
        let path = state_directory.join("sessions.json");
        let store = Store::open(&path, None).unwrap();
        let id = store.insert("pending".into(), None, demo());

        std::fs::remove_file(&path).unwrap();
        std::fs::remove_file(state_directory.join("sessions.json.lock")).unwrap();
        std::fs::remove_dir(&state_directory).unwrap();
        std::fs::write(&state_directory, b"not a directory").unwrap();

        let error = store.reply(&id, "must not leak".into()).unwrap_err();
        assert!(matches!(error, StoreError::Persistence(_)));
        assert!(
            store.with(&id, |session| session.reply.is_none()).unwrap(),
            "failed disk mutation became visible in memory"
        );
    }
}
