//! Talking to the daemon, and starting it if it is not running.

use std::time::Duration;

use anyhow::{Context, Result};

use crate::config::Config;
use crate::server::{Comments, CreateSession, CreatedSession, Inbox, InboxItem, Received, Threads};
use crate::session::{Comment, Message, Reply};

pub const MAX_PRESENTATION_TABS: usize = 12;
const MAX_SESSION_ID_LEN: usize = 64;

pub struct PreparedPresentation {
    pub ids: Vec<String>,
    pub url: String,
}

/// The `timeout_secs` query string for a long-poll, or an empty string when no
/// timeout is set. Leaving it off tells the daemon to wait indefinitely.
fn timeout_query(timeout: Option<Duration>) -> String {
    match timeout {
        Some(timeout) => format!("timeout_secs={}", timeout.as_secs()),
        None => String::new(),
    }
}

fn validate_session_id(id: &str) -> Result<()> {
    anyhow::ensure!(
        !id.is_empty()
            && id.len() <= MAX_SESSION_ID_LEN
            && id
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit()),
        "invalid session ID {id:?}: expected 1-{MAX_SESSION_ID_LEN} lowercase ASCII letters or digits"
    );
    Ok(())
}

pub struct Client {
    /// The port the daemon was last seen on. Cached because every call would
    /// otherwise rescan the range.
    port: std::sync::Mutex<Option<u16>>,
    bind: std::net::IpAddr,
    ports: std::ops::RangeInclusive<u16>,
    public_host: String,
    http: reqwest::Client,
}

impl Client {
    pub fn new(config: &Config) -> Self {
        Self {
            port: std::sync::Mutex::new(None),
            // The daemon binds `config.bind`; the CLI is on the same host, so
            // it connects over loopback regardless of the public host name.
            bind: config.bind,
            ports: config.ports(),
            public_host: config.public_host.clone(),
            http: reqwest::Client::new(),
        }
    }

    /// The base URL of the running daemon, scanning the range if needed.
    ///
    /// The daemon does not own a fixed port: the range is chosen for being
    /// already forwarded, so it is also already occupied, and the daemon takes
    /// whichever port in it was free.
    async fn base(&self) -> Option<String> {
        // Read and release before probing: holding a std `Mutex` across an await
        // would block every other caller for the length of a network round trip,
        // and can deadlock if the future is moved between threads.
        let cached = *self.port.lock().expect("not poisoned");
        if let Some(port) = cached {
            let base = format!("http://{}:{port}", self.bind);
            if self.probe(&base).await {
                return Some(base);
            }
            // It moved or died; fall through and rescan.
        }
        for port in self.ports.clone() {
            let base = format!("http://{}:{port}", self.bind);
            if self.probe(&base).await {
                *self.port.lock().expect("not poisoned") = Some(port);
                return Some(base);
            }
        }
        None
    }

    /// Whether a wdyt daemon — not merely *something* — answers at `base`.
    ///
    /// Checks both the identifying marker and the protocol version. A daemon
    /// that carries the marker but lacks a compatible protocol version is
    /// treated as stale (returns `false`), so the client will skip it and
    /// start a new one. This prevents silent feature loss when a newer client
    /// encounters an older daemon.
    async fn probe(&self, base: &str) -> bool {
        let Ok(response) = self
            .http
            .get(format!("{base}/health"))
            .timeout(Duration::from_millis(600))
            .send()
            .await
        else {
            return false;
        };
        if !response.status().is_success() {
            return false;
        }
        // The range is full of other people's servers, several of which answer
        // 200 on `/health`. Only the marker identifies ours.
        let Ok(body) = response.text().await else {
            return false;
        };
        if !body.contains(crate::server::HEALTH_MARKER) {
            return false;
        }
        // Protocol version check: a daemon without a protocol field (old daemon)
        // or with a mismatched version is incompatible.
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) else {
            return false;
        };
        json.get("protocol")
            .and_then(|v| v.as_u64())
            .is_some_and(|v| v == u64::from(crate::server::PROTOCOL_VERSION))
    }

    /// The URL a browser should use for a session, once the daemon is known.
    pub async fn session_url(&self, id: &str) -> Result<String> {
        let port = self.daemon_port().await.context("no daemon running")?;
        Ok(format!("http://{}:{port}/s/{id}", self.public_host))
    }

    /// Validates, de-duplicates, and resolves the sessions for a presentation.
    ///
    /// The returned IDs retain first-seen order. Every session is confirmed
    /// through the daemon before the URL is made available to a caller.
    pub async fn prepare_presentation(&self, ids: &[String]) -> Result<PreparedPresentation> {
        let ids = Self::normalize_presentation_ids(ids)?;
        let base = self.require_base().await?;
        for id in &ids {
            self.ensure_session_exists_at(&base, id).await?;
        }
        let port = self.daemon_port().await.context("no daemon running")?;
        let query = ids
            .iter()
            .map(|id| format!("s={id}"))
            .collect::<Vec<_>>()
            .join("&");
        Ok(PreparedPresentation {
            ids,
            url: format!("http://{}:{port}/?{query}", self.public_host),
        })
    }

    /// The browser URL for a verified presentation.
    pub async fn presentation_url(&self, ids: &[String]) -> Result<String> {
        Ok(self.prepare_presentation(ids).await?.url)
    }

    /// Confirms that `id` names a live session without changing its state.
    pub async fn ensure_session_exists(&self, id: &str) -> Result<()> {
        validate_session_id(id)?;
        let base = self.require_base().await?;
        self.ensure_session_exists_at(&base, id).await
    }

    /// Normalizes presentation IDs without contacting the daemon.
    pub fn normalize_presentation_ids(ids: &[String]) -> Result<Vec<String>> {
        anyhow::ensure!(
            !ids.is_empty(),
            "a presentation requires at least one session ID"
        );
        let mut normalized = Vec::with_capacity(ids.len().min(MAX_PRESENTATION_TABS));
        for id in ids {
            validate_session_id(id)?;
            if normalized.iter().any(|seen| seen == id) {
                continue;
            }
            anyhow::ensure!(
                normalized.len() < MAX_PRESENTATION_TABS,
                "a presentation supports at most {MAX_PRESENTATION_TABS} unique tabs"
            );
            normalized.push(id.clone());
        }
        Ok(normalized)
    }

    async fn ensure_session_exists_at(&self, base: &str, id: &str) -> Result<()> {
        let response = self
            .http
            .get(format!("{base}/api/sessions/{id}/status"))
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .with_context(|| format!("could not verify session ID {id:?}"))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            anyhow::bail!("unknown session ID {id:?}");
        }
        anyhow::ensure!(
            response.status().is_success(),
            "daemon returned {} while checking session ID {id:?}",
            response.status()
        );
        Ok(())
    }

    /// The port the daemon is on, if it is running.
    pub async fn daemon_port(&self) -> Option<u16> {
        self.base().await;
        *self.port.lock().expect("not poisoned")
    }

    /// Whether a daemon is answering.
    pub async fn is_up(&self) -> bool {
        self.base().await.is_some()
    }

    /// The base URL, or an error naming the range that was searched.
    async fn require_base(&self) -> Result<String> {
        self.base().await.with_context(|| {
            format!(
                "no wdyt daemon on ports {}-{}",
                self.ports.start(),
                self.ports.end()
            )
        })
    }

    /// Creates a session, starting the daemon first if needed.
    ///
    /// The origin is filled in here rather than by the caller: this process is
    /// the one actually sitting in the agent's directory and pane.
    pub async fn create(&self, mut session: CreateSession) -> Result<String> {
        self.ensure_daemon().await?;
        session.origin = Some(crate::zellij_origin::Origin::detect());

        let base = self.require_base().await?;
        let response = self
            .http
            .post(format!("{base}/api/sessions"))
            .json(&session)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .context("could not reach the wdyt daemon")?;

        anyhow::ensure!(
            response.status().is_success(),
            "daemon rejected the session: {}",
            response.status()
        );
        let created: CreatedSession = response.json().await.context("bad daemon response")?;
        Ok(created.id)
    }

    /// Waits for a reply. With `timeout` set, gives up after it; without one,
    /// waits forever.
    pub async fn wait(&self, id: &str, timeout: Option<Duration>) -> Result<Option<Reply>> {
        #[derive(serde::Deserialize)]
        struct Response {
            reply: Option<Reply>,
        }

        let base = self.require_base().await?;
        let mut request = self.http.get(format!(
            "{base}/api/sessions/{id}/reply?{}",
            timeout_query(timeout)
        ));
        // Outlast the server's own long-poll window so the server, not the
        // client, decides when the wait is over. With no timeout the request
        // has none either, so it blocks until the reply lands.
        if let Some(timeout) = timeout {
            request = request.timeout(timeout + Duration::from_secs(15));
        }
        let response = request
            .send()
            .await
            .context("could not reach the wdyt daemon")?;

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            anyhow::bail!("no such session: {id}");
        }
        anyhow::ensure!(
            response.status().is_success(),
            "daemon returned {}",
            response.status()
        );
        Ok(response.json::<Response>().await?.reply)
    }

    /// Subscribes to the reader's next event — a reply or a line comment alike.
    ///
    /// This is the one long-poll an agent loops on: it blocks until the reader
    /// does something the agent has not seen, takes it (marking it picked up, so
    /// the page says so), and can be called again for the next one. What turns a
    /// thread green is still the answer sent back, not the taking.
    pub async fn recv(&self, id: &str, timeout: Option<Duration>) -> Result<Received> {
        let base = self.require_base().await?;
        let mut request = self.http.get(format!(
            "{base}/api/sessions/{id}/recv?{}",
            timeout_query(timeout)
        ));
        // Outlast the daemon's own window, so it decides when time is up. With
        // no timeout the request has none either, so it blocks until an event.
        if let Some(timeout) = timeout {
            request = request.timeout(timeout + Duration::from_secs(15));
        }
        let response = request
            .send()
            .await
            .context("could not reach the wdyt daemon")?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            anyhow::bail!("no such session: {id}");
        }
        anyhow::ensure!(
            response.status().is_success(),
            "daemon returned {}",
            response.status()
        );
        Ok(response.json::<Received>().await?)
    }

    /// Every thread in a session, with the whole exchange under each.
    ///
    /// Reading changes nothing: an agent can look at a discussion without
    /// claiming to have picked anything up.
    pub async fn threads(&self, id: &str) -> Result<Vec<Comment>> {
        let base = self.require_base().await?;
        let response = self
            .http
            .get(format!("{base}/api/sessions/{id}/threads"))
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .context("could not reach the wdyt daemon")?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            anyhow::bail!("no such session: {id}");
        }
        anyhow::ensure!(
            response.status().is_success(),
            "daemon returned {}",
            response.status()
        );
        Ok(response.json::<Threads>().await?.threads)
    }

    /// Answers one thread.
    pub async fn say(&self, id: &str, thread: u64, text: &str) -> Result<Message> {
        let base = self.require_base().await?;
        let response = self
            .http
            .post(format!(
                "{base}/api/sessions/{id}/comments/{thread}/messages"
            ))
            .json(&serde_json::json!({ "text": text }))
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .context("could not reach the wdyt daemon")?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            anyhow::bail!("no such session or thread: {id} #{thread}");
        }
        anyhow::ensure!(
            response.status().is_success(),
            "daemon returned {}: {}",
            response.status(),
            response.text().await.unwrap_or_default()
        );
        Ok(response.json::<Message>().await?)
    }

    /// Every reply the daemon is holding, optionally only the unread ones.    ///
    /// Does not start a daemon: an empty inbox and no daemon at all mean the
    /// same thing to the caller, and starting one to ask would be surprising.
    pub async fn inbox(&self, unread_only: bool) -> Result<Vec<InboxItem>> {
        if !self.is_up().await {
            return Ok(Vec::new());
        }
        let base = self.require_base().await?;
        let response = self
            .http
            .get(format!(
                "{base}/api/inbox{}",
                if unread_only { "?unread=1" } else { "" }
            ))
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .context("could not reach the wdyt daemon")?;
        anyhow::ensure!(
            response.status().is_success(),
            "daemon returned {}",
            response.status()
        );
        Ok(response.json::<Inbox>().await?.items)
    }

    /// Collects a reply that has already arrived, marking it read.
    pub async fn collect(&self, id: &str) -> Result<Option<Reply>> {
        #[derive(serde::Deserialize)]
        struct Response {
            reply: Option<Reply>,
        }

        let base = self.require_base().await?;
        let response = self
            .http
            .post(format!("{base}/api/sessions/{id}/collect"))
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .context("could not reach the wdyt daemon")?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            anyhow::bail!("no such session: {id}");
        }
        anyhow::ensure!(
            response.status().is_success(),
            "daemon returned {}",
            response.status()
        );
        Ok(response.json::<Response>().await?.reply)
    }

    /// Tells the daemon an agent has actually read the reply.
    ///
    /// Distinct from [`Client::collect`] on purpose: that one moves bytes, this
    /// one is a live model saying something about them. Only this lights up the
    /// page's receipt.
    pub async fn ack(&self, id: &str, note: &str) -> Result<()> {
        let base = self.require_base().await?;
        let response = self
            .http
            .post(format!("{base}/api/sessions/{id}/ack"))
            .json(&crate::server::AckInput {
                note: note.to_owned(),
            })
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .context("could not reach the wdyt daemon")?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            anyhow::bail!("no such session: {id}");
        }
        anyhow::ensure!(
            response.status().is_success(),
            "daemon rejected the ack: {}",
            response.status()
        );
        Ok(())
    }

    /// A session's line comments.
    pub async fn comments(&self, id: &str) -> Result<Vec<Comment>> {
        let base = self.require_base().await?;
        let response = self
            .http
            .get(format!("{base}/api/sessions/{id}/comments"))
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .context("could not reach the wdyt daemon")?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            anyhow::bail!("no such session: {id}");
        }
        Ok(response.json::<Comments>().await?.comments)
    }

    /// Starts a background daemon if none is running.
    ///
    /// Agents call `wdyt code …` directly without thinking about a server,
    /// so the first such call brings one up.
    async fn ensure_daemon(&self) -> Result<()> {
        if self.is_up().await {
            return Ok(());
        }

        let exe = std::env::current_exe().context("could not locate the wdyt binary")?;

        // Capture the daemon's stderr to a temp file so that, if it dies before
        // answering, the CLI can surface the daemon's own error text verbatim
        // rather than a guess. Without this the output would go to /dev/null and
        // the only recourse was running `wdyt serve` by hand to see the reason.
        let log_path = daemon_log_path();
        let log = std::fs::File::create(&log_path)
            .with_context(|| format!("creating {}", log_path.display()))?;

        let mut child = std::process::Command::new(exe)
            .arg("serve")
            // Detached: the daemon outlives this CLI invocation. Its stdout would
            // otherwise interleave with the agent's own; its stderr is diverted
            // to the log so a startup failure is not lost.
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::from(log))
            .spawn()
            .context("could not start the wdyt daemon")?;

        // Poll until it answers, but stop early if the child we just started has
        // exited: a daemon that could not bind dies at once, and polling the
        // whole window for a process that is already gone only delays the error.
        let mut child_exited = false;
        for _ in 0..60 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if self.is_up().await {
                // Success: the daemon is running and holds the log open for its
                // one "listening" line. Best-effort removal keeps temp files from
                // accumulating; on Unix the daemon keeps writing to the unlinked
                // inode, which nothing reads.
                let _ = std::fs::remove_file(&log_path);
                return Ok(());
            }
            if matches!(child.try_wait(), Ok(Some(_))) {
                child_exited = true;
                break;
            }
        }

        // It never answered. Read back whatever the daemon printed before dying
        // (the bind loop names the range and every port it tried) and surface it
        // verbatim. Remove the log either way.
        let logged = std::fs::read_to_string(&log_path)
            .ok()
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty());
        let _ = std::fs::remove_file(&log_path);

        match logged {
            Some(reason) => anyhow::bail!("the wdyt daemon could not start:\n{reason}"),
            // No output captured: either the child is still alive but not
            // answering (slow start), or it died silently. Fall back to the
            // generic guidance.
            None if child_exited => {
                anyhow::bail!(
                    "the wdyt daemon exited without starting; try `wdyt serve` to see why"
                )
            }
            None => anyhow::bail!("the wdyt daemon did not come up; try `wdyt serve` to see why"),
        }
    }
}

/// A unique path for one daemon-startup log, under the system temp directory.
///
/// Keyed by the CLI's pid and a high-resolution timestamp so two concurrent
/// invocations do not clobber each other's log.
fn daemon_log_path() -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("wdyt-serve-{}-{nanos}.log", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presentation_ids_are_deduplicated_in_first_seen_order() {
        let ids = ["second", "first", "second", "third", "first"]
            .map(str::to_owned)
            .to_vec();
        assert_eq!(
            Client::normalize_presentation_ids(&ids).unwrap(),
            ["second", "first", "third"]
        );
    }

    #[test]
    fn presentation_id_limits_are_enforced_after_deduplication() {
        let mut ids = (0..MAX_PRESENTATION_TABS)
            .map(|index| format!("id{index}"))
            .collect::<Vec<_>>();
        ids.push("id0".to_owned());
        assert_eq!(
            Client::normalize_presentation_ids(&ids).unwrap().len(),
            MAX_PRESENTATION_TABS
        );

        ids.push("overflow".to_owned());
        let error = Client::normalize_presentation_ids(&ids).unwrap_err();
        assert!(error.to_string().contains("at most 12 unique tabs"));
    }

    #[test]
    fn presentation_ids_are_length_bounded() {
        let error =
            Client::normalize_presentation_ids(&["a".repeat(MAX_SESSION_ID_LEN + 1)]).unwrap_err();
        assert!(error.to_string().contains("1-64"));
    }
}
