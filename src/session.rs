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
    /// A server the agent started; showme frames it and adds the reply box.
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
        matches!(self, Self::Diff { .. })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeFile {
    /// Label shown in the file list, usually the path as the agent named it.
    pub label: String,
    /// Highlighted HTML, pre-rendered at submit time so page loads are cheap.
    pub html: String,
    pub lines: usize,
    /// Language, for display only.
    pub language: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocFile {
    pub label: String,
    /// Rendered HTML.
    pub html: String,
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
    /// When the agent first collected this reply, so the page can say that it
    /// landed. `None` means it is still sitting in the daemon unread.
    #[serde(default, with = "unix_seconds_opt", skip_serializing_if = "Option::is_none")]
    pub read_at: Option<SystemTime>,
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

/// A comment on one line of a diff.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    /// Assigned by the store, so the page can address a comment it just made.
    pub id: u64,
    /// The file the line belongs to, as it appears in the diff.
    pub file: String,
    /// The line number on `side`.
    pub line: usize,
    pub side: Side,
    /// The line's own text, so the agent has the context without re-reading
    /// the diff it sent.
    pub snippet: String,
    pub text: String,
    #[serde(with = "unix_seconds")]
    pub at: SystemTime,
}

pub struct Session {
    pub id: String,
    pub title: String,
    /// Optional longer note from the agent, shown under the title.
    pub note: Option<String>,
    pub content: Content,
    pub created: SystemTime,
    /// The reply, once the user sends one. A session accepts a single reply;
    /// the sender is consumed on first use.
    pub reply: Option<Reply>,
    /// Line comments on a diff, oldest first. Unlike the reply these are not
    /// capped at one: commenting on six lines of a diff is the normal case.
    pub comments: Vec<Comment>,
    /// Wakes up a `showme wait` for this session.
    pub waiters: Vec<oneshot::Sender<Reply>>,
}

impl Session {
    fn expired(&self, ttl: Duration, now: SystemTime) -> bool {
        now.duration_since(self.created)
            .is_ok_and(|age| age > ttl)
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
            read_at: None,
        };
        session.reply = Some(reply.clone());
        for waiter in session.waiters.drain(..) {
            // A dropped receiver just means that `wait` gave up.
            let _ = waiter.send(reply.clone());
        }
        Ok(true)
    }

    /// Marks a session's reply as collected by the agent, returning it.
    ///
    /// The first call stamps `read_at`; later ones return the reply with the
    /// original stamp, so a re-read does not look like a fresh one.
    pub fn mark_read(&self, id: &str) -> Result<Option<Reply>, UnknownSession> {
        let mut sessions = self.lock();
        let session = sessions.get_mut(id).ok_or(UnknownSession)?;
        let Some(reply) = session.reply.as_mut() else {
            return Ok(None);
        };
        reply.read_at.get_or_insert_with(SystemTime::now);
        Ok(Some(reply.clone()))
    }

    /// Adds a line comment, returning its assigned id.
    pub fn comment(
        &self,
        id: &str,
        file: String,
        line: usize,
        side: Side,
        snippet: String,
        text: String,
    ) -> Result<Comment, UnknownSession> {
        let mut sessions = self.lock();
        let session = sessions.get_mut(id).ok_or(UnknownSession)?;
        // Ids are per session and monotonic, so a page can address the comment
        // it just made without a round trip for a name.
        let next = session.comments.last().map_or(1, |last| last.id + 1);
        let comment = Comment {
            id: next,
            file,
            line,
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
        Ok(self
            .lock()
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

    /// Session ids and titles, newest first. For `showme list`.
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
/// `showme wait` prints is easy for an agent to read.
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
        let ids: std::collections::HashSet<_> =
            (0..500).map(|_| store.insert("t".into(), None, demo())).collect();
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
        store
            .lock()
            .get_mut(&stale)
            .unwrap()
            .created = SystemTime::now() - Duration::from_secs(3600);

        let fresh = store.insert("fresh".into(), None, demo());
        assert!(!store.exists(&stale), "stale session survived");
        assert!(store.exists(&fresh));
    }

    #[test]
    fn a_reply_is_unread_until_collected() {
        let store = Store::new(None);
        let id = store.insert("t".into(), None, demo());
        store.reply(&id, "hi".into()).unwrap();

        let unread = store.with(&id, |s| s.reply.clone()).unwrap().unwrap();
        assert!(unread.read_at.is_none(), "a fresh reply looked read");

        let first = store.mark_read(&id).unwrap().unwrap();
        let stamp = first.read_at.expect("collecting stamps read_at");

        // A second collection is a re-read, not a fresh one: the page would
        // otherwise show the receipt moving.
        let again = store.mark_read(&id).unwrap().unwrap();
        assert_eq!(again.read_at, Some(stamp));
    }

    #[test]
    fn marking_read_with_no_reply_is_not_an_error() {
        let store = Store::new(None);
        let id = store.insert("t".into(), None, demo());
        assert!(store.mark_read(&id).unwrap().is_none());
        assert!(store.mark_read("nope").is_err());
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
        store.mark_read(&seen).unwrap();

        let ids: Vec<_> = store.replied().into_iter().map(|item| item.0).collect();
        assert!(ids.contains(&seen) && ids.contains(&missed));
        assert!(!ids.contains(&silent), "a session with no reply appeared");

        let unread: Vec<_> = store
            .replied()
            .into_iter()
            .filter(|(_, _, reply, _)| reply.read_at.is_none())
            .map(|item| item.0)
            .collect();
        assert_eq!(unread, vec![missed]);
    }

    #[test]
    fn comments_accumulate_with_rising_ids() {
        // Unlike the reply, comments are not capped at one: six comments on a
        // diff is the normal case.
        let store = Store::new(None);
        let id = store.insert("t".into(), None, demo());
        let first = store
            .comment(&id, "a.rs".into(), 3, Side::New, "let x = 1;".into(), "why?".into())
            .unwrap();
        let second = store
            .comment(&id, "a.rs".into(), 9, Side::Old, "gone".into(), "keep this".into())
            .unwrap();
        assert!(second.id > first.id, "ids did not rise");
        assert_eq!(store.comments(&id).unwrap().len(), 2);
        assert!(store.comment("nope", "a".into(), 1, Side::New, String::new(), "x".into()).is_err());
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
    fn summaries_are_newest_first() {
        let store = Store::new(None);
        let first = store.insert("first".into(), None, demo());
        std::thread::sleep(Duration::from_millis(5));
        let second = store.insert("second".into(), None, demo());
        let ids: Vec<_> = store.summaries().into_iter().map(|s| s.0).collect();
        assert_eq!(ids, vec![second, first]);
    }
}
