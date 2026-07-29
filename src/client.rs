//! Talking to the daemon, and starting it if it is not running.

use std::time::Duration;

use anyhow::{Context, Result};

use crate::config::Config;
use crate::server::{Comments, CreateSession, CreatedSession, Inbox, InboxItem};
use crate::session::{Comment, Reply};

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

    /// Whether a showme daemon — not merely *something* — answers at `base`.
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
        response
            .text()
            .await
            .is_ok_and(|body| body.contains(crate::server::HEALTH_MARKER))
    }

    /// The URL a browser should use for a session, once the daemon is known.
    pub async fn session_url(&self, id: &str) -> Result<String> {
        let port = self.daemon_port().await.context("no daemon running")?;
        Ok(format!("http://{}:{port}/s/{id}", self.public_host))
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
                "no showme daemon on ports {}-{}",
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
        session.origin = Some(crate::origin::Origin::detect());

        let base = self.require_base().await?;
        let response = self
            .http
            .post(format!("{base}/api/sessions"))
            .json(&session)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .context("could not reach the showme daemon")?;

        anyhow::ensure!(
            response.status().is_success(),
            "daemon rejected the session: {}",
            response.status()
        );
        let created: CreatedSession = response.json().await.context("bad daemon response")?;
        Ok(created.id)
    }

    /// Waits for a reply, up to `timeout`.
    pub async fn wait(&self, id: &str, timeout: Duration) -> Result<Option<Reply>> {
        #[derive(serde::Deserialize)]
        struct Response {
            reply: Option<Reply>,
        }

        let base = self.require_base().await?;
        let response = self
            .http
            .get(format!(
                "{base}/api/sessions/{id}/reply?timeout_secs={}",
                timeout.as_secs()
            ))
            // Outlast the server's own long-poll window so the server, not the
            // client, decides when the wait is over.
            .timeout(timeout + Duration::from_secs(15))
            .send()
            .await
            .context("could not reach the showme daemon")?;

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

    /// Every reply the daemon is holding, optionally only the unread ones.
    ///
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
            .context("could not reach the showme daemon")?;
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
            .context("could not reach the showme daemon")?;
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
            .context("could not reach the showme daemon")?;
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
            .context("could not reach the showme daemon")?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            anyhow::bail!("no such session: {id}");
        }
        Ok(response.json::<Comments>().await?.comments)
    }

    /// Starts a background daemon if none is running.
    ///
    /// Agents call `showme code …` directly without thinking about a server,
    /// so the first such call brings one up.
    async fn ensure_daemon(&self) -> Result<()> {
        if self.is_up().await {
            return Ok(());
        }

        let exe = std::env::current_exe().context("could not locate the showme binary")?;
        std::process::Command::new(exe)
            .arg("serve")
            // Detached: the daemon outlives this CLI invocation. Its output
            // would otherwise interleave with the agent's own.
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .context("could not start the showme daemon")?;

        // Poll until it answers.
        for _ in 0..60 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if self.is_up().await {
                return Ok(());
            }
        }
        anyhow::bail!("the showme daemon did not come up; try `showme serve` to see why")
    }
}
