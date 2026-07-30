//! Subprocess / integration tests for the default-wait / --no-wait behaviour.
//!
//! Each test starts its own daemon on an isolated port range and invokes the
//! compiled `wdyt` binary as a subprocess, so the exit-status and stdout/
//! stderr contracts are exercised exactly as an agent would see them.
//!
//! Safety invariants:
//! - Every spawned child uses `kill_on_drop(true)` so panics / timeouts do not
//!   leave orphan processes blocking ports.
//! - All potentially-blocking operations are wrapped in `tokio::time::timeout`
//!   to guarantee tests terminate even if the daemon or child hangs.

use std::net::IpAddr;
use std::process::Stdio;
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;

use wdyt::config::Config;
use wdyt::render::theme;
use wdyt::session::{Content, Store};
use tokio::io::{AsyncBufReadExt, AsyncReadExt};

/// A daemon on its own port range, with helpers for creating sessions and
/// sending replies programmatically.
struct TestDaemon {
    base: String,
    port: u16,
    store: Store,
}

impl TestDaemon {
    async fn start() -> Self {
        // Band allocation (disjoint across test binaries to prevent collisions):
        //   pages.rs:    20000-33999
        //   cli_wait.rs: 34000-47999
        //   cli_skill.rs: 48000-61999
        const WINDOW: u16 = 8;
        const BAND_BASE: u16 = 34_000;
        const BAND_SIZE: u16 = 14_000;
        static NEXT: AtomicU16 = AtomicU16::new(0);
        let spread = (std::process::id() as u16).wrapping_mul(WINDOW) % BAND_SIZE;
        let base_port = BAND_BASE + spread.wrapping_add(NEXT.fetch_add(WINDOW, Ordering::Relaxed));

        let config = Config {
            port_low: base_port,
            port_high: base_port.saturating_add(WINDOW - 1),
            bind: IpAddr::from([127, 0, 0, 1]),
            webhook_url: None,
            ..Config::default()
        };

        let store = Store::new(None);
        let serving = store.clone();
        let serve_config = config.clone();
        tokio::spawn(async move {
            let _ = wdyt::server::serve(&serve_config, serving).await;
        });

        // Discover the port.
        let client = wdyt::client::Client::new(&config);
        let port = Self::find_port(&client).await;
        let daemon = Self {
            base: format!("http://127.0.0.1:{port}"),
            port,
            store,
        };
        daemon.wait_until_up().await;
        daemon
    }

    async fn find_port(client: &wdyt::client::Client) -> u16 {
        for _ in 0..200 {
            if let Some(port) = client.daemon_port().await {
                return port;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("no daemon came up");
    }

    async fn wait_until_up(&self) {
        for _ in 0..100 {
            if reqwest::get(format!("{}/health", self.base)).await.is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("daemon did not come up");
    }

    /// Insert a code session directly into the store and return its id.
    fn insert_code_session(&self) -> String {
        let file = wdyt::render::highlight("test.rs", "fn main() {}\n", theme("Nord")).unwrap();
        self.store.insert(
            "test session".to_owned(),
            None,
            Content::Code { files: vec![file] },
        )
    }

    /// Reply to a session directly through the store.
    fn reply(&self, id: &str, text: &str) {
        self.store.reply(id, text.to_owned()).unwrap();
    }

    /// The port range string to pass as WDYT_PORTS.
    fn port_env(&self) -> String {
        format!("{}-{}", self.port, self.port)
    }
}

/// Path to the compiled `wdyt` binary.
fn wdyt_bin() -> std::path::PathBuf {
    // The test binary is in target/debug/deps/; the main binary is one level up.
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // deps/
    path.pop(); // debug/
    path.push("wdyt");
    assert!(
        path.exists(),
        "binary not found at {}; run `cargo build` first",
        path.display()
    );
    path
}

#[tokio::test]
async fn no_wait_returns_promptly_with_status_0() {
    let daemon = TestDaemon::start().await;
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), "fn main() {}\n").unwrap();

    let start = std::time::Instant::now();
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new(wdyt_bin())
            .args([
                "code",
                tmp.path().to_str().unwrap(),
                "--no-wait",
                "--no-notify",
            ])
            .env("WDYT_PORTS", daemon.port_env())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("--no-wait timed out (10s)")
    .expect("failed to run wdyt");
    let elapsed = start.elapsed();

    assert!(
        output.status.success(),
        "exit status was {:?}; stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    // --no-wait must return well within 5 seconds (not waiting for a reply).
    assert!(
        elapsed < Duration::from_secs(5),
        "took {elapsed:?} — that is not prompt"
    );
    // stdout must contain a URL.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("http://"),
        "stdout should contain the URL: {stdout}"
    );
}

#[tokio::test]
async fn default_wait_times_out_with_exit_status_2() {
    let daemon = TestDaemon::start().await;
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), "fn main() {}\n").unwrap();

    let output = tokio::time::timeout(
        Duration::from_secs(15),
        tokio::process::Command::new(wdyt_bin())
            .args([
                "code",
                tmp.path().to_str().unwrap(),
                "--timeout",
                "1", // 1 second timeout
                "--no-notify",
            ])
            .env("WDYT_PORTS", daemon.port_env())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("timeout test did not finish (15s)")
    .expect("failed to run wdyt");

    assert_eq!(
        output.status.code(),
        Some(2),
        "expected exit status 2 on timeout; got {:?}; stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    // stdout must still contain the URL (printed before blocking).
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("http://"),
        "stdout should contain the URL even on timeout: {stdout}"
    );
    // stderr should mention the timeout.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no reply within"),
        "stderr should explain timeout: {stderr}"
    );
}

#[tokio::test]
async fn default_wait_prints_json_reply_and_exits_0() {
    let daemon = TestDaemon::start().await;
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), "fn main() {}\n").unwrap();

    // Start the CLI subprocess with a long timeout; we'll reply quickly.
    let mut child = tokio::process::Command::new(wdyt_bin())
        .args([
            "code",
            tmp.path().to_str().unwrap(),
            "--timeout",
            "30",
            "--no-notify",
        ])
        .env("WDYT_PORTS", daemon.port_env())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("failed to run wdyt");

    // Read the first stdout line under a timeout — this directly proves the
    // flush happened without a fixed sleep.
    let stdout_pipe = child.stdout.take().expect("stdout piped");
    let mut reader = tokio::io::BufReader::new(stdout_pipe);
    let mut url_line = String::new();
    tokio::time::timeout(Duration::from_secs(10), reader.read_line(&mut url_line))
        .await
        .expect("URL line not printed within 10s")
        .expect("IO error reading URL line");
    assert!(
        url_line.contains("http://"),
        "first line should be the URL: {url_line:?}"
    );

    // Extract session ID from the URL.
    let session_id = url_line
        .trim()
        .rsplit('/')
        .next()
        .expect("URL should have path segments");

    // Reply to the session.
    daemon.reply(session_id, "ship it");

    // Read remaining stdout (the JSON reply) from the same reader.
    let mut remaining = String::new();
    tokio::time::timeout(
        Duration::from_secs(10),
        reader.read_to_string(&mut remaining),
    )
    .await
    .expect("remaining stdout not read within 10s")
    .expect("IO error reading remaining stdout");

    // Wait for the process to exit.
    let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("child did not exit in time")
        .expect("IO error waiting on child");

    assert!(status.success(), "exit status was {:?}", status.code(),);
    // Remaining stdout should contain the JSON reply.
    assert!(
        remaining.contains("ship it"),
        "no reply JSON in remaining stdout: {remaining}"
    );
    // Verify it's valid JSON.
    let json_str = remaining.trim();
    let reply: serde_json::Value =
        serde_json::from_str(json_str).expect("reply should be valid JSON");
    assert_eq!(reply["text"], "ship it");
}

#[tokio::test]
async fn timed_out_session_is_still_collectible() {
    let daemon = TestDaemon::start().await;
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), "fn main() {}\n").unwrap();

    // Let it time out with --timeout 1.
    let output = tokio::time::timeout(
        Duration::from_secs(15),
        tokio::process::Command::new(wdyt_bin())
            .args([
                "code",
                tmp.path().to_str().unwrap(),
                "--timeout",
                "1",
                "--no-notify",
            ])
            .env("WDYT_PORTS", daemon.port_env())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("timeout test did not finish (15s)")
    .expect("failed to run wdyt");

    assert_eq!(output.status.code(), Some(2));

    // Extract session id from the URL in stdout.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let url_line = stdout.lines().next().expect("stdout should have URL line");
    let session_id = url_line
        .trim()
        .rsplit('/')
        .next()
        .expect("URL should have path segments");

    // Reply after the timeout.
    daemon.reply(session_id, "sent much later");

    // Verify it's in the inbox via the HTTP API.
    let response = reqwest::get(format!("{}/api/inbox?unread=1", daemon.base))
        .await
        .unwrap();
    let body = response.text().await.unwrap();
    assert!(
        body.contains("sent much later"),
        "reply not in inbox: {body}"
    );

    // Exercise `wdyt collect <id>` CLI — must exit 0 and return the reply.
    let collect_output = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new(wdyt_bin())
            .args(["collect", session_id])
            .env("WDYT_PORTS", daemon.port_env())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("collect did not finish (10s)")
    .expect("failed to run wdyt collect");

    assert!(
        collect_output.status.success(),
        "collect exit status was {:?}; stderr: {}",
        collect_output.status.code(),
        String::from_utf8_lossy(&collect_output.stderr)
    );
    let collect_stdout = String::from_utf8_lossy(&collect_output.stdout);
    assert!(
        collect_stdout.contains("sent much later"),
        "collect stdout should contain the reply: {collect_stdout}"
    );
    // Verify the collect output is valid JSON with the expected shape.
    let collect_json: serde_json::Value =
        serde_json::from_str(collect_stdout.trim()).expect("collect output should be valid JSON");
    assert_eq!(collect_json["replied"], true);
    assert_eq!(collect_json["reply"]["text"], "sent much later");
}

#[tokio::test]
async fn standalone_wait_still_works() {
    let daemon = TestDaemon::start().await;
    let id = daemon.insert_code_session();

    // `wdyt wait <id> --timeout 1` with no reply should exit 2.
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new(wdyt_bin())
            .args(["wait", &id, "--timeout", "1"])
            .env("WDYT_PORTS", daemon.port_env())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("standalone wait did not finish (10s)")
    .expect("failed to run wdyt");

    assert_eq!(
        output.status.code(),
        Some(2),
        "expected exit 2; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Now reply and try again.
    daemon.reply(&id, "approved");

    let output = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new(wdyt_bin())
            .args(["wait", &id, "--timeout", "5"])
            .env("WDYT_PORTS", daemon.port_env())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("standalone wait with reply did not finish (10s)")
    .expect("failed to run wdyt");

    assert!(
        output.status.success(),
        "expected exit 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("approved"), "no reply in stdout: {stdout}");
}

#[tokio::test]
async fn no_wait_and_timeout_are_rejected() {
    // Ensure the CLI rejects --no-wait combined with --timeout.
    let daemon = TestDaemon::start().await;
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), "fn main() {}\n").unwrap();

    let output = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new(wdyt_bin())
            .args([
                "code",
                tmp.path().to_str().unwrap(),
                "--no-wait",
                "--timeout",
                "5",
                "--no-notify",
            ])
            .env("WDYT_PORTS", daemon.port_env())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("conflict test did not finish (10s)")
    .expect("failed to run wdyt");

    assert!(
        !output.status.success(),
        "should fail when --no-wait and --timeout are combined"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot be used with") || stderr.contains("conflict"),
        "stderr should mention conflict: {stderr}"
    );
}

#[tokio::test]
async fn wait_and_no_wait_are_rejected() {
    // Ensure the CLI rejects --wait combined with --no-wait.
    let daemon = TestDaemon::start().await;
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), "fn main() {}\n").unwrap();

    let output = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new(wdyt_bin())
            .args([
                "code",
                tmp.path().to_str().unwrap(),
                "--wait",
                "--no-wait",
                "--no-notify",
            ])
            .env("WDYT_PORTS", daemon.port_env())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("conflict test did not finish (10s)")
    .expect("failed to run wdyt");

    assert!(
        !output.status.success(),
        "should fail when --wait and --no-wait are combined"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot be used with") || stderr.contains("conflict"),
        "stderr should mention conflict: {stderr}"
    );
}
