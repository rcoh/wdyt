//! Integration tests for task #15: CLI help as a mini skill, file-anchor
//! printing, and post-session stderr guidance.
//!
//! These exercise the compiled binary as a subprocess, verifying the contracts
//! an agent sees: stdout stays machine-readable (URL + JSON only), and stderr
//! carries the teaching/coaching anchors and next-step guidance.

use std::net::IpAddr;
use std::process::Stdio;
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt};
use wdyt::config::Config;
use wdyt::render::theme;
use wdyt::session::{Content, Store};

// ---------- Test daemon setup (shared with cli_wait.rs pattern) ----------

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
        const BAND_BASE: u16 = 48_000;
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

    fn port_env(&self) -> String {
        format!("{}-{}", self.port, self.port)
    }

    fn reply(&self, id: &str, text: &str) {
        self.store.reply(id, text.to_owned()).unwrap();
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
}

fn wdyt_bin() -> std::path::PathBuf {
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

// ---------- Help output tests ----------

#[test]
fn root_help_contains_workflow_section() {
    let output = std::process::Command::new(wdyt_bin())
        .args(["--help"])
        .output()
        .expect("failed to run wdyt --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("WORKFLOW"),
        "root --help should contain WORKFLOW section:\n{stdout}"
    );
    assert!(
        stdout.contains("#f-"),
        "root --help should mention #f- anchors:\n{stdout}"
    );
    assert!(
        stdout.contains("wdyt collect"),
        "root --help should mention collect:\n{stdout}"
    );
    assert!(
        stdout.contains("wdyt ack"),
        "root --help should mention ack:\n{stdout}"
    );
    assert!(
        stdout.contains("--no-wait"),
        "root --help should mention --no-wait:\n{stdout}"
    );
    assert!(
        stdout.contains("--brief-file"),
        "root --help should mention --brief-file stdin caveat:\n{stdout}"
    );
}

#[test]
fn code_help_mentions_wait_behavior() {
    let output = std::process::Command::new(wdyt_bin())
        .args(["code", "--help"])
        .output()
        .expect("failed to run wdyt code --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Waits for a reply by default"),
        "code --help should explain wait default:\n{stdout}"
    );
    assert!(
        stdout.contains("--no-wait"),
        "code --help should show --no-wait:\n{stdout}"
    );
}

#[test]
fn diff_help_mentions_stdin_caveat() {
    let output = std::process::Command::new(wdyt_bin())
        .args(["diff", "--help"])
        .output()
        .expect("failed to run wdyt diff --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--brief-file"),
        "diff --help should mention --brief-file caveat:\n{stdout}"
    );
    assert!(
        stdout.contains("stdin"),
        "diff --help should mention stdin:\n{stdout}"
    );
}

#[test]
fn collect_help_mentions_json_shape() {
    let output = std::process::Command::new(wdyt_bin())
        .args(["collect", "--help"])
        .output()
        .expect("failed to run wdyt collect --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("replied"),
        "collect --help should describe JSON shape:\n{stdout}"
    );
    assert!(
        stdout.contains("comments"),
        "collect --help should mention comments:\n{stdout}"
    );
    assert!(
        stdout.contains("wdyt ack") || stdout.contains("acknowledge"),
        "collect --help should point to ack:\n{stdout}"
    );
}

#[test]
fn ack_help_mentions_receipt() {
    let output = std::process::Command::new(wdyt_bin())
        .args(["ack", "--help"])
        .output()
        .expect("failed to run wdyt ack --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("green"),
        "ack --help should mention turning receipt green:\n{stdout}"
    );
}

#[test]
fn brief_arg_help_shows_anchor_examples() {
    let output = std::process::Command::new(wdyt_bin())
        .args(["code", "--help"])
        .output()
        .expect("failed to run wdyt code --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("#f-"),
        "code --help should show #f- in --brief description:\n{stdout}"
    );
    assert!(
        stdout.contains("src/a.rs"),
        "code --help should include concrete anchor example:\n{stdout}"
    );
}

// ---------- Shared file_anchor helper tests ----------

#[test]
fn file_anchor_known_vectors() {
    // These must match what the server renders as section ids.
    assert_eq!(wdyt::file_anchor("src/main.rs"), "f-src-main-rs");
    assert_eq!(wdyt::file_anchor("src/lib.rs"), "f-src-lib-rs");
    assert_eq!(wdyt::file_anchor("A B.md"), "f-a-b-md");
    assert_eq!(wdyt::file_anchor("Cargo.lock"), "f-cargo-lock");
    assert_eq!(wdyt::file_anchor("a-b/c-d.rs"), "f-a-b-c-d-rs");
    assert_eq!(
        wdyt::file_anchor("src/my_mod/test.rs"),
        "f-src-my-mod-test-rs"
    );
}

#[test]
fn file_anchor_empty_input() {
    assert_eq!(wdyt::file_anchor(""), "f-");
}

#[test]
fn file_anchor_all_special_chars() {
    assert_eq!(wdyt::file_anchor("..."), "f----");
}

#[test]
fn file_anchor_uppercase_lowered() {
    assert_eq!(wdyt::file_anchor("README.md"), "f-readme-md");
    assert_eq!(wdyt::file_anchor("SRC/Main.RS"), "f-src-main-rs");
}

#[test]
fn file_anchor_server_uses_shared_helper() {
    // Sanity check that the server's anchor function is the shared one by
    // confirming it matches the lib's public function. This test exercises the
    // code path through server::tests indirectly — we trust the server build
    // compiles against the same crate::file_anchor.
    assert_eq!(wdyt::file_anchor("x/y.rs"), "f-x-y-rs");
}

// ---------- Subprocess stderr/stdout contract tests ----------

#[tokio::test]
async fn code_session_prints_anchors_to_stderr() {
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
                "--no-notify",
            ])
            .env("WDYT_PORTS", daemon.port_env())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("timed out")
    .expect("failed to run wdyt");

    assert!(output.status.success());

    let stderr = String::from_utf8_lossy(&output.stderr);
    // stderr should contain the file anchor line.
    assert!(
        stderr.contains("file anchors"),
        "stderr should mention file anchors: {stderr}"
    );
    // The anchor for the temp file path should be present.
    let expected_anchor = wdyt::file_anchor(tmp.path().to_str().unwrap());
    assert!(
        stderr.contains(&expected_anchor),
        "stderr should contain anchor {expected_anchor}: {stderr}"
    );

    // stdout should ONLY contain the URL, no coaching.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.trim().starts_with("http://"),
        "stdout should only contain URL: {stdout}"
    );
    assert!(
        !stdout.contains("anchor"),
        "stdout must not contain coaching: {stdout}"
    );
}

#[tokio::test]
async fn diff_session_prints_anchors_to_stderr() {
    let daemon = TestDaemon::start().await;

    let patch = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,3 +1,3 @@\n fn main() {\n-    println!(\"old\");\n+    println!(\"new\");\n }\n";
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), patch).unwrap();

    let output = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new(wdyt_bin())
            .args([
                "diff",
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
    .expect("timed out")
    .expect("failed to run wdyt");

    assert!(
        output.status.success(),
        "diff failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    // The diff contains src/lib.rs, so the anchor for it should appear.
    assert!(
        stderr.contains("#f-src-lib-rs"),
        "stderr should contain the diff file anchor: {stderr}"
    );
}

#[tokio::test]
async fn docs_session_prints_anchors_to_stderr() {
    let daemon = TestDaemon::start().await;
    let tmp = tempfile::Builder::new().suffix(".md").tempfile().unwrap();
    std::fs::write(tmp.path(), "# Hello\n\nWorld.\n").unwrap();

    let output = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new(wdyt_bin())
            .args([
                "docs",
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
    .expect("timed out")
    .expect("failed to run wdyt");

    assert!(
        output.status.success(),
        "docs failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("file anchors"),
        "stderr should mention file anchors for docs: {stderr}"
    );
}

#[tokio::test]
async fn no_wait_stderr_shows_collect_guidance() {
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
                "--no-notify",
            ])
            .env("WDYT_PORTS", daemon.port_env())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("timed out")
    .expect("failed to run wdyt");

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    // --no-wait guidance should mention collect but NOT ack (no reply yet).
    assert!(
        stderr.contains("wdyt collect"),
        "no-wait stderr should mention collect: {stderr}"
    );
    assert!(
        !stderr.contains("wdyt ack"),
        "no-wait stderr must NOT mention ack (no reply yet): {stderr}"
    );
    assert!(
        stderr.contains("created"),
        "no-wait stderr should say 'created': {stderr}"
    );
    assert!(
        !stderr.contains("sent"),
        "no-wait stderr should not say 'sent': {stderr}"
    );
}

#[tokio::test]
async fn timeout_stderr_shows_collect_guidance() {
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
    .expect("timed out waiting for test")
    .expect("failed to run wdyt");

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Timeout guidance should mention collect and inbox.
    assert!(
        stderr.contains("wdyt collect"),
        "timeout stderr should mention collect: {stderr}"
    );
    assert!(
        stderr.contains("wdyt inbox"),
        "timeout stderr should mention inbox: {stderr}"
    );
}

#[tokio::test]
async fn reply_received_stderr_shows_ack_guidance() {
    let daemon = TestDaemon::start().await;
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), "fn main() {}\n").unwrap();

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

    // Read the URL line from stdout.
    let stdout_pipe = child.stdout.take().unwrap();
    let mut reader = tokio::io::BufReader::new(stdout_pipe);
    let mut url_line = String::new();
    tokio::time::timeout(Duration::from_secs(10), reader.read_line(&mut url_line))
        .await
        .expect("URL line not printed within 10s")
        .expect("IO error reading URL line");

    let session_id = url_line.trim().rsplit('/').next().unwrap();
    daemon.reply(session_id, "looks good");

    // Read remaining stdout.
    let mut remaining = String::new();
    tokio::time::timeout(
        Duration::from_secs(10),
        reader.read_to_string(&mut remaining),
    )
    .await
    .expect("remaining stdout not read in time")
    .expect("IO error");

    let stderr_pipe = child.stderr.take().unwrap();
    let mut stderr_reader = tokio::io::BufReader::new(stderr_pipe);
    let mut stderr_content = String::new();
    tokio::time::timeout(
        Duration::from_secs(10),
        stderr_reader.read_to_string(&mut stderr_content),
    )
    .await
    .expect("stderr not read in time")
    .expect("IO error");

    let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("child did not exit")
        .expect("IO error");

    assert!(status.success());
    // stderr after receiving reply should guide to collect then ack.
    assert!(
        stderr_content.contains("wdyt collect"),
        "reply stderr should mention collect for comments: {stderr_content}"
    );
    assert!(
        stderr_content.contains("wdyt ack"),
        "reply stderr should mention ack: {stderr_content}"
    );
    // stdout should have the JSON reply, no coaching.
    assert!(
        remaining.contains("looks good"),
        "stdout should contain reply JSON: {remaining}"
    );
    assert!(
        !remaining.contains("wdyt ack"),
        "stdout must not contain coaching: {remaining}"
    );
}

#[tokio::test]
async fn collect_stderr_shows_ack_guidance() {
    let daemon = TestDaemon::start().await;
    let file = wdyt::render::highlight("src/example.rs", "fn main() {}\n", theme("Nord")).unwrap();
    let id = daemon
        .store
        .insert("test".to_owned(), None, Content::Code { files: vec![file] });
    daemon.reply(&id, "nice work");

    let output = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new(wdyt_bin())
            .args(["collect", &id])
            .env("WDYT_PORTS", daemon.port_env())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("timed out")
    .expect("failed to run wdyt");

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("wdyt ack"),
        "collect stderr should mention ack: {stderr}"
    );
    // stdout has JSON only.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).expect("should be JSON");
    assert_eq!(json["replied"], true);
    assert_eq!(json["reply"]["text"], "nice work");
}

#[tokio::test]
async fn stdout_never_contains_coaching_text() {
    // Verify the strong contract: stdout is URL + optional JSON, nothing else.
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
                "--no-notify",
            ])
            .env("WDYT_PORTS", daemon.port_env())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("timed out")
    .expect("failed to run wdyt");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    // The only line should be the URL.
    assert_eq!(
        lines.len(),
        1,
        "stdout should have exactly 1 line: {stdout}"
    );
    assert!(
        lines[0].starts_with("http://"),
        "stdout line should be URL: {stdout}"
    );
    // Negative checks: no coaching words.
    for word in ["anchor", "collect", "ack", "brief", "wait"] {
        assert!(
            !stdout.contains(word),
            "stdout must not contain coaching word '{word}': {stdout}"
        );
    }
}

// ---------- Task #15 final: anchor validation, state-correct guidance, wording ----------

#[tokio::test]
async fn code_collision_labels_fail_before_session_creation() {
    let daemon = TestDaemon::start().await;
    // Create two files whose paths normalize to the same anchor:
    // "a-b.rs" -> f-a-b-rs and "a/b.rs" -> f-a-b-rs
    let dir = tempfile::tempdir().unwrap();
    let file1 = dir.path().join("a-b.rs");
    std::fs::write(&file1, "fn a() {}").unwrap();
    let sub = dir.path().join("a");
    std::fs::create_dir_all(&sub).unwrap();
    let file2 = sub.join("b.rs");
    std::fs::write(&file2, "fn b() {}").unwrap();

    let output = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new(wdyt_bin())
            .args([
                "code",
                file1.to_str().unwrap(),
                file2.to_str().unwrap(),
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
    .expect("timed out")
    .expect("failed to run wdyt");

    // Must fail (no session created).
    assert!(
        !output.status.success(),
        "collision should fail: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("anchor collision"),
        "should mention anchor collision: {stderr}"
    );
    // stdout must be empty (no URL printed).
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.trim().is_empty(),
        "no URL should be printed on collision: {stdout}"
    );
}

#[tokio::test]
async fn diff_collision_labels_fail_before_session_creation() {
    let daemon = TestDaemon::start().await;
    // A diff with two files that collide: "a-b.rs" and "a/b.rs"
    let patch = "\
--- a/a-b.rs\n+++ b/a-b.rs\n@@ -1 +1 @@\n-old\n+new\n\
--- a/a/b.rs\n+++ b/a/b.rs\n@@ -1 +1 @@\n-old2\n+new2\n";
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), patch).unwrap();

    let output = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new(wdyt_bin())
            .args([
                "diff",
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
    .expect("timed out")
    .expect("failed to run wdyt");

    assert!(
        !output.status.success(),
        "diff collision should fail: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("anchor collision"),
        "should mention anchor collision: {stderr}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.trim().is_empty(), "no URL on collision: {stdout}");
}

#[tokio::test]
async fn docs_collision_labels_fail_before_session_creation() {
    let daemon = TestDaemon::start().await;
    let dir = tempfile::tempdir().unwrap();
    let file1 = dir.path().join("a-b.md");
    std::fs::write(&file1, "# A-B").unwrap();
    let sub = dir.path().join("a");
    std::fs::create_dir_all(&sub).unwrap();
    let file2 = sub.join("b.md");
    std::fs::write(&file2, "# A/B").unwrap();

    let output = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new(wdyt_bin())
            .args([
                "docs",
                file1.to_str().unwrap(),
                file2.to_str().unwrap(),
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
    .expect("timed out")
    .expect("failed to run wdyt");

    assert!(
        !output.status.success(),
        "docs collision should fail: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("anchor collision"),
        "should mention anchor collision: {stderr}"
    );
}

#[tokio::test]
async fn code_session_page_anchors_match_stderr_output() {
    let daemon = TestDaemon::start().await;
    // Create two distinct files with non-colliding names.
    let dir = tempfile::tempdir().unwrap();
    let file1 = dir.path().join("alpha.rs");
    std::fs::write(&file1, "fn alpha() {}").unwrap();
    let file2 = dir.path().join("beta.rs");
    std::fs::write(&file2, "fn beta() {}").unwrap();

    let output = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new(wdyt_bin())
            .args([
                "code",
                file1.to_str().unwrap(),
                file2.to_str().unwrap(),
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
    .expect("timed out")
    .expect("failed to run wdyt");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Extract the URL and fetch the page.
    let url = stdout.trim();
    let page_html = reqwest::get(url)
        .await
        .expect("fetch page")
        .text()
        .await
        .expect("page body");

    // Check that every anchor printed in stderr corresponds to an actual section id.
    let anchor1 = wdyt::file_anchor(file1.to_str().unwrap());
    let anchor2 = wdyt::file_anchor(file2.to_str().unwrap());
    assert!(
        stderr.contains(&format!("#{anchor1}")),
        "stderr should print anchor1: {stderr}"
    );
    assert!(
        stderr.contains(&format!("#{anchor2}")),
        "stderr should print anchor2: {stderr}"
    );
    // Verify section IDs exist in the page HTML.
    assert!(
        page_html.contains(&format!("id=\"{anchor1}\"")),
        "page should have section id={anchor1}"
    );
    assert!(
        page_html.contains(&format!("id=\"{anchor2}\"")),
        "page should have section id={anchor2}"
    );
    // Also verify nav links exist.
    assert!(
        page_html.contains(&format!("href=\"#{anchor1}\"")),
        "page should have nav href=#{anchor1}"
    );
    assert!(
        page_html.contains(&format!("href=\"#{anchor2}\"")),
        "page should have nav href=#{anchor2}"
    );
}

#[tokio::test]
async fn diff_session_page_anchors_match_stderr_output() {
    let daemon = TestDaemon::start().await;
    let patch = "\
--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,3 +1,3 @@\n fn main() {\n-    old();\n+    new();\n }\n\
--- a/src/util.rs\n+++ b/src/util.rs\n@@ -1 +1 @@\n-fn old() {}\n+fn new() {}\n";
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), patch).unwrap();

    let output = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new(wdyt_bin())
            .args([
                "diff",
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
    .expect("timed out")
    .expect("failed to run wdyt");

    assert!(
        output.status.success(),
        "failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let url = stdout.trim();

    let page_html = reqwest::get(url)
        .await
        .expect("fetch page")
        .text()
        .await
        .expect("page body");

    // Both files should have anchors in stderr and matching section IDs in the page.
    for file_label in ["src/lib.rs", "src/util.rs"] {
        let anchor = wdyt::file_anchor(file_label);
        assert!(
            stderr.contains(&format!("#{anchor}")),
            "stderr should have anchor for {file_label}: {stderr}"
        );
        assert!(
            page_html.contains(&format!("id=\"{anchor}\"")),
            "page should have section id={anchor} for {file_label}"
        );
        assert!(
            page_html.contains(&format!("href=\"#{anchor}\"")),
            "page should have nav href=#{anchor} for {file_label}"
        );
    }
}

#[tokio::test]
async fn docs_session_page_anchors_match_stderr_output() {
    let daemon = TestDaemon::start().await;
    let dir = tempfile::tempdir().unwrap();
    let file1 = dir.path().join("DESIGN.md");
    std::fs::write(&file1, "# Design\n\nThe design.").unwrap();
    let file2 = dir.path().join("PLAN.md");
    std::fs::write(&file2, "# Plan\n\nThe plan.").unwrap();

    let output = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new(wdyt_bin())
            .args([
                "docs",
                file1.to_str().unwrap(),
                file2.to_str().unwrap(),
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
    .expect("timed out")
    .expect("failed to run wdyt");

    assert!(
        output.status.success(),
        "failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let url = stdout.trim();

    let page_html = reqwest::get(url)
        .await
        .expect("fetch page")
        .text()
        .await
        .expect("page body");

    let anchor1 = wdyt::file_anchor(file1.to_str().unwrap());
    let anchor2 = wdyt::file_anchor(file2.to_str().unwrap());
    assert!(
        stderr.contains(&format!("#{anchor1}")),
        "stderr should have anchor for DESIGN.md: {stderr}"
    );
    assert!(
        stderr.contains(&format!("#{anchor2}")),
        "stderr should have anchor for PLAN.md: {stderr}"
    );
    assert!(
        page_html.contains(&format!("id=\"{anchor1}\"")),
        "page should have section id={anchor1}"
    );
    assert!(
        page_html.contains(&format!("id=\"{anchor2}\"")),
        "page should have section id={anchor2}"
    );
}

#[tokio::test]
async fn collect_comments_only_does_not_suggest_ack() {
    let daemon = TestDaemon::start().await;
    // Create a code session with a file that can be commented on.
    let file = wdyt::render::highlight(
        "src/example.rs",
        "fn main() {\n    // hello\n}\n",
        theme("Nord"),
    )
    .unwrap();
    let id = daemon
        .store
        .insert("test".to_owned(), None, Content::Code { files: vec![file] });

    // Add a comment without a reply.
    let anchor =
        wdyt::session::Anchor::line("src/example.rs".to_owned(), 1, wdyt::session::Side::New);
    daemon
        .store
        .comment(
            &id,
            anchor,
            "fn main() {".to_owned(),
            "why main?".to_owned(),
        )
        .unwrap();

    let output = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new(wdyt_bin())
            .args(["collect", &id])
            .env("WDYT_PORTS", daemon.port_env())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("timed out")
    .expect("failed to run wdyt");

    assert!(
        output.status.success(),
        "collect with comments-only should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Must NOT suggest ack since there's no reply.
    assert!(
        !stderr.contains("wdyt ack"),
        "comments-only collect must not suggest ack: {stderr}"
    );
    // Should indicate comments were collected without a reply.
    assert!(
        stderr.contains("no reply to acknowledge"),
        "should say no reply to acknowledge: {stderr}"
    );
    // stdout should be valid JSON with replied: false and comments present.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    assert_eq!(json["replied"], false);
    assert!(!json["comments"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn standalone_wait_success_prints_collect_and_ack_guidance() {
    let daemon = TestDaemon::start().await;
    let id = daemon.insert_code_session();
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
    .expect("timed out")
    .expect("failed to run wdyt");

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Standalone wait success should print collect and ack guidance.
    assert!(
        stderr.contains("wdyt collect"),
        "standalone wait success should mention collect: {stderr}"
    );
    assert!(
        stderr.contains("wdyt ack"),
        "standalone wait success should mention ack: {stderr}"
    );
}

#[tokio::test]
async fn no_notify_wording_is_consistent() {
    let daemon = TestDaemon::start().await;
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), "fn main() {}\n").unwrap();

    // --no-notify should say "ready" (not "sent" or contradictory messages).
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
    .expect("timed out")
    .expect("failed to run wdyt");

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Should use "ready" for the no-webhook message.
    assert!(
        stderr.contains("ready"),
        "no-notify stderr should say 'ready': {stderr}"
    );
    // Should never say "notification sent" (which contradicts "no notification").
    assert!(
        !stderr.contains("notification sent"),
        "must not say 'notification sent': {stderr}"
    );
    // --no-notify must say "notification skipped", not "no webhook configured".
    assert!(
        stderr.contains("notification skipped"),
        "no-notify stderr should say 'notification skipped': {stderr}"
    );
    assert!(
        !stderr.contains("no webhook configured"),
        "--no-notify case must not say 'no webhook configured': {stderr}"
    );
    // Should use "created" for the session status, not "sent".
    assert!(
        stderr.contains("created"),
        "session status should say 'created': {stderr}"
    );
}

#[tokio::test]
async fn ordinary_non_colliding_code_session_preserves_anchors() {
    // Ensure non-colliding labels pass through fine and anchors are printed normally.
    let daemon = TestDaemon::start().await;
    let dir = tempfile::tempdir().unwrap();
    let file1 = dir.path().join("src").join("main.rs");
    std::fs::create_dir_all(file1.parent().unwrap()).unwrap();
    std::fs::write(&file1, "fn main() {}").unwrap();
    let file2 = dir.path().join("src").join("lib.rs");
    std::fs::write(&file2, "pub fn lib() {}").unwrap();

    let output = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new(wdyt_bin())
            .args([
                "code",
                file1.to_str().unwrap(),
                file2.to_str().unwrap(),
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
    .expect("timed out")
    .expect("failed to run wdyt");

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Both anchors should appear in stderr.
    let a1 = wdyt::file_anchor(file1.to_str().unwrap());
    let a2 = wdyt::file_anchor(file2.to_str().unwrap());
    assert!(
        stderr.contains(&a1),
        "should contain anchor for main.rs: {stderr}"
    );
    assert!(
        stderr.contains(&a2),
        "should contain anchor for lib.rs: {stderr}"
    );
    // URL printed on stdout.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.trim().starts_with("http://"));
}

// ---------- Finding 9: Help/docs ----------

#[tokio::test]
async fn wait_help_mentions_exit_2_and_recovery() {
    let output = tokio::process::Command::new(wdyt_bin())
        .args(["wait", "--help"])
        .output()
        .await
        .expect("failed to run");
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(
        help.contains("exit status 2"),
        "wait help should mention exit status 2: {help}"
    );
    assert!(
        help.contains("collect") || help.contains("inbox"),
        "wait help should mention recovery via collect/inbox: {help}"
    );
}

#[tokio::test]
async fn inbox_help_mentions_ack_based_filtering() {
    let output = tokio::process::Command::new(wdyt_bin())
        .args(["inbox", "--help"])
        .output()
        .await
        .expect("failed to run");
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(
        help.contains("ack"),
        "inbox help should mention ack-based filtering: {help}"
    );
}

#[tokio::test]
async fn collect_help_describes_target_for_markdown_comments() {
    let output = tokio::process::Command::new(wdyt_bin())
        .args(["collect", "--help"])
        .output()
        .await
        .expect("failed to run");
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(
        help.contains("target"),
        "collect help should mention target field: {help}"
    );
    assert!(
        help.contains("brief") && help.contains("doc:"),
        "collect help should describe brief and doc: targets: {help}"
    );
}
