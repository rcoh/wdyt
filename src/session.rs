//! Sessions: one per "show me this", held in memory by the daemon.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

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
}

/// A `@@` hunk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hunk {
    /// The `@@ … @@` line, including any trailing section heading.
    pub header: String,
    pub lines: Vec<DiffLine>,
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
    pub created: SystemTime,
    /// The reply, once the user sends one. A session accepts a single reply;
    /// the sender is consumed on first use.
    pub reply: Option<Reply>,
    /// Line comments on a diff, oldest first. Unlike the reply these are not
    /// capped at one: commenting on six lines of a diff is the normal case.
    pub comments: Vec<Comment>,
    /// Wakes up a `wdyt wait` for this session.
    pub waiters: Vec<oneshot::Sender<Reply>>,
}

impl Session {
    fn expired(&self, ttl: Duration, now: SystemTime) -> bool {
        now.duration_since(self.created).is_ok_and(|age| age > ttl)
    }
}

/// Shared session store. Cloneable; all clones see the same sessions.
#[derive(Clone)]
pub struct Store {
    inner: Arc<Mutex<HashMap<String, Session>>>,
    counter: Arc<AtomicU64>,
    ttl: Option<Duration>,
}

impl Store {
    pub fn new(ttl: Option<Duration>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            counter: Arc::new(AtomicU64::new(0)),
            ttl,
        }
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
        let seq = self.counter.fetch_add(1, Ordering::Relaxed);
        let stamp = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        // Base36 of a time/sequence mix: compact and URL-safe.
        let id = base36(stamp.rotate_left(17) ^ seq.wrapping_mul(0x9E37_79B9_7F4A_7C15));

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
            waiters: Vec::new(),
        };

        let mut sessions = self.lock();
        sessions.insert(id.clone(), session);
        Self::evict(&mut sessions, self.ttl, now);
        id
    }

    /// Runs `f` against a session, if it exists.
    pub fn with<T>(&self, id: &str, f: impl FnOnce(&Session) -> T) -> Option<T> {
        self.lock().get(id).map(f)
    }

    pub fn exists(&self, id: &str) -> bool {
        self.lock().contains_key(id)
    }

    /// Records a reply and wakes any waiters.
    ///
    /// Returns `Err` if the session is unknown, `Ok(false)` if it already had
    /// a reply. A session takes one reply: this is a "send the agent a note"
    /// box, not a chat.
    pub fn reply(&self, id: &str, text: String) -> Result<bool, UnknownSession> {
        let mut sessions = self.lock();
        let session = sessions.get_mut(id).ok_or(UnknownSession)?;
        if session.reply.is_some() {
            return Ok(false);
        }

        let reply = Reply {
            text,
            at: SystemTime::now(),
            delivered_at: None,
            ack: None,
        };
        session.reply = Some(reply.clone());
        for waiter in session.waiters.drain(..) {
            // A dropped receiver just means that `wait` gave up.
            let _ = waiter.send(reply.clone());
        }
        Ok(true)
    }

    /// Marks a session's reply as delivered to an agent process, returning it.
    ///
    /// The first call stamps `delivered_at`; later ones return the reply with the
    /// original stamp, so a re-fetch does not look like a fresh delivery. This
    /// claims only that the bytes left the daemon — see [`Store::ack`] for the
    /// claim that something read them.
    pub fn mark_delivered(&self, id: &str) -> Result<Option<Reply>, UnknownSession> {
        let mut sessions = self.lock();
        let session = sessions.get_mut(id).ok_or(UnknownSession)?;
        let Some(reply) = session.reply.as_mut() else {
            return Ok(None);
        };
        reply.delivered_at.get_or_insert_with(SystemTime::now);
        Ok(Some(reply.clone()))
    }

    /// Records an agent's acknowledgement that it read the reply.
    ///
    /// Returns `Ok(false)` when there is no reply to acknowledge: an ack without
    /// one is a bug in the agent, not something to record. A later ack replaces
    /// an earlier one, so an agent can report progress more than once.
    pub fn ack(&self, id: &str, note: String) -> Result<bool, UnknownSession> {
        let mut sessions = self.lock();
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
    }

    /// Adds a line comment, returning it with its assigned id.
    pub fn comment(
        &self,
        id: &str,
        anchor: Anchor,
        snippet: String,
        text: String,
    ) -> Result<Comment, UnknownSession> {
        let Anchor {
            file,
            target,
            line,
            end_line,
            side,
        } = anchor;
        let mut sessions = self.lock();
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
        };
        session.comments.push(comment.clone());
        Ok(comment)
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
        Ok(self.lock().get(id).ok_or(UnknownSession)?.comments.clone())
    }

    /// Registers interest in a session's reply.
    ///
    /// Resolves immediately when a reply already exists, so a `wait` that
    /// starts after the user has replied does not hang.
    pub fn subscribe(&self, id: &str) -> Result<oneshot::Receiver<Reply>, UnknownSession> {
        let (tx, rx) = oneshot::channel();
        let mut sessions = self.lock();
        let session = sessions.get_mut(id).ok_or(UnknownSession)?;
        match &session.reply {
            Some(reply) => {
                let _ = tx.send(reply.clone());
            }
            None => session.waiters.push(tx),
        }
        Ok(rx)
    }

    /// Session ids and titles, newest first. For `wdyt list`.
    pub fn summaries(&self) -> Vec<(String, String, &'static str, bool)> {
        let mut items: Vec<_> = self
            .lock()
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

    /// A poisoned lock means another thread panicked while holding it. The
    /// guarded map is still structurally sound, so recovering keeps the daemon
    /// serving instead of turning one bad request into a dead server.
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Session>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
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
/// `wdyt wait` prints is easy for an agent to read.
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
        store.lock().get_mut(&stale).unwrap().created =
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
}
