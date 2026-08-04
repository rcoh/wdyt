//! End-to-end coverage for create-only sessions and composed presentations.

use std::net::IpAddr;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use wdyt::config::Config;
use wdyt::render::theme;
use wdyt::session::{Content, Store};

struct TestDaemon {
    port: u16,
    store: Store,
}

impl TestDaemon {
    async fn start() -> Self {
        const WINDOW: u16 = 8;
        const BAND_BASE: u16 = 15_000;
        const BAND_SIZE: u16 = 4_000;
        static NEXT: AtomicU16 = AtomicU16::new(0);

        let spread = (std::process::id() as u16).wrapping_mul(WINDOW);
        let offset =
            spread.wrapping_add(NEXT.fetch_add(WINDOW, Ordering::Relaxed)) % (BAND_SIZE - WINDOW);
        let base_port = BAND_BASE + offset;
        let config = Config {
            port_low: base_port,
            port_high: base_port + WINDOW - 1,
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
        for _ in 0..200 {
            if let Some(port) = client.daemon_port().await {
                return Self { port, store };
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("test daemon did not start");
    }

    fn insert_code(&self, title: &str) -> String {
        let file = wdyt::render::highlight("test.rs", "fn main() {}\n", theme("Nord")).unwrap();
        self.store
            .insert(title.to_owned(), None, Content::Code { files: vec![file] })
    }

    fn command(&self, args: &[&str], webhook: Option<&str>) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(wdyt_bin());
        command
            .args(args)
            .env("WDYT_PORTS", format!("{}-{}", self.port, self.port))
            .env("WDYT_PUBLIC_HOST", "localhost")
            .env("WDYT_THEME", "Nord")
            .env("WDYT_WEBHOOK_URL", webhook.unwrap_or(""))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }

    async fn output(&self, args: &[&str], webhook: Option<&str>) -> std::process::Output {
        tokio::time::timeout(
            Duration::from_secs(15),
            self.command(args, webhook).output(),
        )
        .await
        .expect("CLI timed out")
        .expect("failed to run wdyt")
    }

    async fn output_with_stdin(&self, args: &[&str], stdin: &str) -> std::process::Output {
        let mut command = self.command(args, None);
        command.stdin(Stdio::piped());
        tokio::time::timeout(Duration::from_secs(15), async {
            let mut child = command.spawn().expect("failed to run wdyt");
            let mut child_stdin = child.stdin.take().expect("stdin piped");
            child_stdin
                .write_all(stdin.as_bytes())
                .await
                .expect("write wdyt stdin");
            child_stdin.shutdown().await.expect("close wdyt stdin");
            drop(child_stdin);
            child.wait_with_output().await.expect("wait for wdyt")
        })
        .await
        .expect("CLI timed out")
    }

    fn presentation_url(&self, ids: &[&str]) -> String {
        let query = ids
            .iter()
            .map(|id| format!("s={id}"))
            .collect::<Vec<_>>()
            .join("&");
        format!("http://localhost:{}/?{query}", self.port)
    }
}

fn wdyt_bin() -> &'static str {
    env!("CARGO_BIN_EXE_wdyt")
}

#[tokio::test]
async fn create_code_and_diff_return_json_without_waiting() {
    let daemon = TestDaemon::start().await;
    let code = tempfile::Builder::new().suffix(".rs").tempfile().unwrap();
    std::fs::write(code.path(), "pub fn answer() -> u8 { 42 }\n").unwrap();

    let started = std::time::Instant::now();
    let output = daemon
        .output(
            &[
                "create",
                "code",
                code.path().to_str().unwrap(),
                "--title",
                "public API",
            ],
            None,
        )
        .await;
    assert!(
        output.status.success(),
        "create code failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    let code_json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("create code stdout is one JSON object");
    assert_eq!(code_json["title"], "public API");
    assert_eq!(code_json["kind"], "code");
    let code_id = code_json["id"].as_str().unwrap();
    assert!(daemon.store.exists(code_id));
    assert_eq!(
        code_json["url"],
        format!("http://localhost:{}/s/{code_id}", daemon.port)
    );

    let patch = tempfile::Builder::new()
        .suffix(".patch")
        .tempfile()
        .unwrap();
    std::fs::write(
        patch.path(),
        "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n",
    )
    .unwrap();
    let output = daemon
        .output(
            &[
                "create",
                "diff",
                patch.path().to_str().unwrap(),
                "--title",
                "implementation",
            ],
            None,
        )
        .await;
    assert!(
        output.status.success(),
        "create diff failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let diff_json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("create diff stdout is one JSON object");
    assert_eq!(diff_json["title"], "implementation");
    assert_eq!(diff_json["kind"], "diff");
    assert!(daemon.store.exists(diff_json["id"].as_str().unwrap()));
}

#[tokio::test]
async fn create_guided_diff_accepts_patch_file_and_stdin() {
    let daemon = TestDaemon::start().await;
    let review = tempfile::Builder::new().suffix(".md").tempfile().unwrap();
    std::fs::write(
        review.path(),
        "# Public API\n\n```wdyt-diff\n\
         {\"file\":\"src/lib.rs\",\"side\":\"new\",\"start\":1,\"end\":1}\n\
         ```\n",
    )
    .unwrap();
    let patch_source = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n";
    let patch = tempfile::Builder::new()
        .suffix(".patch")
        .tempfile()
        .unwrap();
    std::fs::write(patch.path(), patch_source).unwrap();

    let output = daemon
        .output(
            &[
                "create",
                "guided-diff",
                review.path().to_str().unwrap(),
                "--patch",
                patch.path().to_str().unwrap(),
                "--title",
                "guided file",
            ],
            None,
        )
        .await;
    assert!(
        output.status.success(),
        "guided patch file failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let file_json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(file_json["kind"], "guided");
    assert_eq!(file_json["title"], "guided file");

    let output = daemon
        .output_with_stdin(
            &[
                "create",
                "guided-diff",
                review.path().to_str().unwrap(),
                "--title",
                "guided stdin",
            ],
            patch_source,
        )
        .await;
    assert!(
        output.status.success(),
        "guided stdin failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdin_json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdin_json["kind"], "guided");
    assert_eq!(stdin_json["title"], "guided stdin");
}

#[tokio::test]
async fn present_deduplicates_tabs_in_first_seen_order() {
    let daemon = TestDaemon::start().await;
    let first = daemon.insert_code("first");
    let second = daemon.insert_code("second");
    let third = daemon.insert_code("third");

    let output = daemon
        .output(&["present", &first, "--no-wait", "--no-notify"], None)
        .await;
    assert!(
        output.status.success(),
        "single present failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        daemon.presentation_url(&[&first])
    );

    let output = daemon
        .output(
            &[
                "present",
                &second,
                &first,
                &second,
                &third,
                &first,
                "--title",
                "guided review",
                "--note",
                "read in order",
                "--no-wait",
                "--no-notify",
            ],
            None,
        )
        .await;
    assert!(
        output.status.success(),
        "multi present failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        daemon.presentation_url(&[&second, &first, &third])
    );
}

#[tokio::test]
async fn present_waits_for_the_first_session_reply() {
    let daemon = TestDaemon::start().await;
    let first = daemon.insert_code("first");
    let second = daemon.insert_code("second");
    let mut child = daemon
        .command(
            &["present", &first, &second, "--timeout", "30", "--no-notify"],
            None,
        )
        .spawn()
        .expect("spawn wdyt present");

    let stdout = child.stdout.take().unwrap();
    let mut stdout = tokio::io::BufReader::new(stdout);
    let mut url = String::new();
    tokio::time::timeout(Duration::from_secs(10), stdout.read_line(&mut url))
        .await
        .expect("presentation URL was not printed")
        .expect("read presentation URL");
    assert_eq!(url.trim(), daemon.presentation_url(&[&first, &second]));

    daemon
        .store
        .reply(&second, "reply on another tab".to_owned())
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        child.try_wait().unwrap().is_none(),
        "replying to the second tab ended the wait"
    );

    daemon
        .store
        .reply(&first, "overall reply".to_owned())
        .unwrap();
    let mut reply = String::new();
    tokio::time::timeout(Duration::from_secs(10), stdout.read_to_string(&mut reply))
        .await
        .expect("reply JSON was not printed")
        .expect("read reply JSON");
    let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("present did not exit")
        .expect("wait for present");
    assert!(status.success());
    let reply: serde_json::Value = serde_json::from_str(reply.trim()).expect("reply JSON");
    assert_eq!(reply["text"], "overall reply");
}

#[tokio::test]
async fn present_timeout_and_invalid_ids_fail_cleanly() {
    let daemon = TestDaemon::start().await;
    let id = daemon.insert_code("timeout");
    let output = daemon
        .output(&["present", &id, "--timeout", "1", "--no-notify"], None)
        .await;
    assert_eq!(
        output.status.code(),
        Some(2),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()
            .unwrap(),
        daemon.presentation_url(&[&id])
    );

    let output = daemon
        .output(
            &["present", "valid", "bad&id", "--no-wait", "--no-notify"],
            None,
        )
        .await;
    assert!(!output.status.success());
    assert!(output.stdout.is_empty(), "invalid IDs must not emit a URL");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("invalid session ID"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let too_long = "a".repeat(65);
    let output = daemon
        .output(&["present", &too_long, "--no-wait", "--no-notify"], None)
        .await;
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("1-64"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn present_rejects_valid_shaped_unknown_ids_before_output_or_notification() {
    let daemon = TestDaemon::start().await;
    let webhook = Webhook::start().await;
    let output = daemon
        .output(&["present", "missing123", "--no-wait"], Some(&webhook.url))
        .await;

    assert!(!output.status.success());
    assert!(output.stdout.is_empty(), "unknown IDs must not emit a URL");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unknown session ID"), "stderr: {stderr}");
    assert!(stderr.contains("missing123"), "stderr: {stderr}");
    assert_eq!(webhook.count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn present_rejects_more_than_twelve_unique_tabs() {
    let daemon = TestDaemon::start().await;
    let ids = (0..13)
        .map(|index| daemon.insert_code(&format!("tab {index}")))
        .collect::<Vec<_>>();
    let mut args = vec!["present".to_owned()];
    args.extend(ids);
    args.extend(["--no-wait".to_owned(), "--no-notify".to_owned()]);
    let args = args.iter().map(String::as_str).collect::<Vec<_>>();
    let output = daemon.output(&args, None).await;

    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "over-limit input must not emit a URL"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("at most 12 unique tabs"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

struct Webhook {
    url: String,
    count: Arc<AtomicUsize>,
    bodies: Arc<std::sync::Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Webhook {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let count = Arc::new(AtomicUsize::new(0));
        let bodies = Arc::new(std::sync::Mutex::new(Vec::new()));
        let task_count = count.clone();
        let task_bodies = bodies.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let task_count = task_count.clone();
                let task_bodies = task_bodies.clone();
                tokio::spawn(async move {
                    let mut bytes = vec![0; 16 * 1024];
                    let read = socket.read(&mut bytes).await.unwrap_or(0);
                    task_count.fetch_add(1, Ordering::SeqCst);
                    task_bodies
                        .lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(&bytes[..read]).into_owned());
                    let _ = socket
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\
                              Connection: close\r\n\r\nok",
                        )
                        .await;
                });
            }
        });
        Self {
            url,
            count,
            bodies,
            task,
        }
    }
}

impl Drop for Webhook {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[tokio::test]
async fn create_never_notifies_and_present_notifies_once_unless_suppressed() {
    let daemon = TestDaemon::start().await;
    let webhook = Webhook::start().await;
    let code = tempfile::Builder::new().suffix(".rs").tempfile().unwrap();
    std::fs::write(code.path(), "fn main() {}\n").unwrap();

    let output = daemon
        .output(
            &["create", "code", code.path().to_str().unwrap()],
            Some(&webhook.url),
        )
        .await;
    assert!(
        output.status.success(),
        "create failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let created: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let first = created["id"].as_str().unwrap().to_owned();
    let second = daemon.insert_code("second");
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(webhook.count.load(Ordering::SeqCst), 0);

    let output = daemon
        .output(
            &["present", &first, &second, "--no-wait", "--no-notify"],
            Some(&webhook.url),
        )
        .await;
    assert!(output.status.success());
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(webhook.count.load(Ordering::SeqCst), 0);

    let output = daemon
        .output(
            &["present", &first, &second, &first, &second, "--no-wait"],
            Some(&webhook.url),
        )
        .await;
    assert!(
        output.status.success(),
        "present failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(webhook.count.load(Ordering::SeqCst), 1);
    let bodies = webhook.bodies.lock().unwrap();
    assert!(
        bodies[0].contains(&daemon.presentation_url(&[&first, &second])),
        "notification did not contain the presentation URL: {}",
        bodies[0]
    );
    assert!(
        bodies[0].contains("2 tabs"),
        "notification used the unnormalized tab count: {}",
        bodies[0]
    );
}
