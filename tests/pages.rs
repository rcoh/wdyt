//! End-to-end tests against a real daemon on a real port.
//!
//! The router's `Service` impl is over hyper's own body type, so calling it
//! in-process is awkward; binding a loopback listener is both simpler and
//! closer to what actually runs.

use std::net::{IpAddr, TcpListener};
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;

use wdyt::config::Config;
use wdyt::render::{theme, theme_colors};
use wdyt::session::{Content, GuidedBlock, Store};

/// Cross-process reservation for a test daemon port.
///
/// `nextest` runs each test in a fresh process, so process-local counters are
/// insufficient. Creating a directory is atomic across processes; holding it
/// until the daemon is dropped closes the bind handoff race between tests.
struct PortReservation {
    path: std::path::PathBuf,
}

impl PortReservation {
    fn acquire() -> (u16, Self) {
        const BAND_BASE: u16 = 20_000;
        const BAND_SIZE: u16 = 14_000;
        static NEXT: AtomicU16 = AtomicU16::new(0);
        let start = (std::process::id() as u16).wrapping_add(NEXT.fetch_add(1, Ordering::Relaxed))
            % BAND_SIZE;

        for offset in 0..BAND_SIZE {
            let port = BAND_BASE + (start.wrapping_add(offset) % BAND_SIZE);
            let path = std::env::temp_dir().join(format!("wdyt-pages-port-{port}.lock"));
            if std::fs::create_dir(&path).is_err() {
                continue;
            }
            match TcpListener::bind(("127.0.0.1", port)) {
                Ok(listener) => {
                    drop(listener);
                    return (port, Self { path });
                }
                Err(_) => {
                    let _ = std::fs::remove_dir(&path);
                }
            }
        }
        panic!("no reservable page-test port");
    }
}

impl Drop for PortReservation {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.path);
    }
}

/// A daemon on its own port, with a store the test can reach into.
struct Daemon {
    base: String,
    store: Store,
    _port_reservation: PortReservation,
}

impl Daemon {
    async fn start() -> Self {
        let (port, port_reservation) = PortReservation::acquire();

        let config = Config {
            port_low: port,
            port_high: port,
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

        // Which port it actually took is discovered rather than assumed, since it
        // steps over anything already held in its window.
        let port = Self::find_port(&config).await;
        let daemon = Self {
            base: format!("http://127.0.0.1:{port}"),
            store,
            _port_reservation: port_reservation,
        };
        daemon.wait_until_up().await;
        daemon
    }

    /// The port this test's daemon landed on, within its own window.
    async fn find_port(config: &Config) -> u16 {
        let client = wdyt::client::Client::new(config);
        for _ in 0..200 {
            if let Some(port) = client.daemon_port().await {
                return port;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!(
            "no daemon came up in {}-{}",
            config.port_low, config.port_high
        );
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

    async fn get(&self, path: &str) -> (u16, String) {
        let response = reqwest::get(format!("{}{path}", self.base))
            .await
            .expect("request sent");
        let status = response.status().as_u16();
        (status, response.text().await.expect("body"))
    }

    async fn post_json(&self, path: &str, body: serde_json::Value) -> (u16, String) {
        let response = reqwest::Client::new()
            .post(format!("{}{path}", self.base))
            .json(&body)
            .send()
            .await
            .expect("request sent");
        let status = response.status().as_u16();
        (status, response.text().await.expect("body"))
    }

    async fn post(&self, path: &str) -> (u16, String) {
        let response = reqwest::Client::new()
            .post(format!("{}{path}", self.base))
            .send()
            .await
            .expect("request sent");
        let status = response.status().as_u16();
        (status, response.text().await.expect("body"))
    }

    fn insert(&self, content: Content) -> String {
        self.store.insert("a title".to_owned(), None, content)
    }
}

fn code_session(daemon: &Daemon) -> String {
    let file = wdyt::render::highlight("x.rs", "fn main() {}\n", theme("Nord")).unwrap();
    daemon.insert(Content::Code { files: vec![file] })
}

const CODE: &str = "\
fn main() {
    let x = 1;
    println!(\"{x}\");
}
";

fn multiline_code_session(daemon: &Daemon) -> String {
    let file = wdyt::render::highlight("src/main.rs", CODE, theme("Nord")).unwrap();
    daemon.insert(Content::Code { files: vec![file] })
}

const PATCH: &str = "\
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,3 @@
 fn main() {
-    let x = 1;
+    let x = 2;
 }
";

fn diff_session(daemon: &Daemon) -> String {
    let files = wdyt::diff::parse(PATCH, theme("Nord")).unwrap();
    daemon.insert(Content::Diff { files })
}

const MULTI_FILE_PATCH: &str = "\
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,3 @@
 fn main() {
-    let x = 1;
+    let x = 2;
 }
--- a/src/util.rs
+++ b/src/util.rs
@@ -1,2 +1,3 @@
 pub fn helper() {
+    println!(\"hi\");
 }
";

fn multi_file_diff_session(daemon: &Daemon) -> String {
    let files = wdyt::diff::parse(MULTI_FILE_PATCH, theme("Nord")).unwrap();
    daemon.insert(Content::Diff { files })
}

fn titled_code_session(daemon: &Daemon, title: &str) -> String {
    let file = wdyt::render::highlight("x.rs", "fn main() {}\n", theme("Nord")).unwrap();
    daemon
        .store
        .insert(title.to_owned(), None, Content::Code { files: vec![file] })
}

fn guided_session(daemon: &Daemon) -> String {
    let prose_source = "## Public API\n\nAPI narrative.\n";
    let prose = wdyt::render::markdown_commentable("guided.md", prose_source, "Nord");

    let code_source = "fn main() {\n    let value = 2;\n    println!(\"{value}\");\n}\n";
    let code = wdyt::render::highlight("src/main.rs", code_source, theme("Nord")).unwrap();

    const GUIDED_PATCH: &str = "\
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -3,3 +3,3 @@
 before
-old value
+new value
 after
";
    let mut files = wdyt::diff::parse(GUIDED_PATCH, theme("Nord")).unwrap();
    assert!(wdyt::diff::attach_source(
        &mut files[0],
        vec![
            "hidden one".to_owned(),
            "hidden two".to_owned(),
            "before".to_owned(),
            "new value".to_owned(),
            "after".to_owned(),
            "hidden six".to_owned(),
            "hidden seven".to_owned(),
        ],
    ));
    let mut selected_hunks = files[0].hunks.clone();
    selected_hunks[0]
        .lines
        .retain(|line| line.kind != wdyt::session::LineKind::Context);

    daemon.store.insert(
        "guided changes".to_owned(),
        None,
        Content::Guided {
            blocks: vec![
                GuidedBlock::Text {
                    html: prose.html,
                    source_start: 10,
                    sources: prose.sources,
                },
                GuidedBlock::Diff {
                    file: files.remove(0),
                    side: wdyt::session::Side::New,
                    start: 4,
                    end: 4,
                    selected_hunks,
                },
                GuidedBlock::Code {
                    file: "src/main.rs".to_owned(),
                    language: code.language,
                    start: 2,
                    end: 3,
                    highlighted: code.highlighted[1..3].to_vec(),
                    sources: code.sources[1..3].to_vec(),
                },
            ],
        },
    )
}

#[tokio::test]
async fn the_index_has_no_reply_widget() {
    let daemon = Daemon::start().await;
    let (status, body) = daemon.get("/").await;
    assert_eq!(status, 200);
    // A widget here would POST to `/s//reply`: there is no session to reply to.
    assert!(!body.contains("id=\"reply\""), "{body}");
}

#[tokio::test]
async fn pages_link_the_embedded_stylesheet_asset() {
    let daemon = Daemon::start().await;
    let (status, page) = daemon.get("/").await;
    assert_eq!(status, 200, "{page}");
    assert!(
        page.contains(r#"<link rel="stylesheet" href="/assets/wdyt.css">"#),
        "{page}"
    );
    assert!(
        !page.contains("<style>"),
        "the static stylesheet was still rendered inline: {page}"
    );

    let response = reqwest::get(format!("{}/assets/wdyt.css", daemon.base))
        .await
        .expect("stylesheet request sent");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/css; charset=utf-8")
    );
    assert_eq!(
        response.text().await.expect("stylesheet body"),
        wdyt::ui::stylesheet_asset_for_test()
    );
}

#[tokio::test]
async fn a_session_links_home_and_can_be_archived_and_restored() {
    let daemon = Daemon::start().await;
    let id = titled_code_session(&daemon, "finished change");

    let (status, session) = daemon.get(&format!("/s/{id}")).await;
    assert_eq!(status, 200, "{session}");
    assert!(
        session.contains(r#"<a class="brand" href="/" aria-label="wdyt home">wdyt</a>"#),
        "{session}"
    );
    assert!(
        session.contains(&format!(r#"action="/s/{id}/archive" method="post""#)),
        "{session}"
    );

    let (status, archived_index) = daemon.post(&format!("/s/{id}/archive")).await;
    assert_eq!(status, 200, "{archived_index}");
    assert!(daemon.store.with(&id, |session| session.archived).unwrap());
    assert!(
        !archived_index.contains(&format!(r#"value="{id}""#)),
        "archived session remained selectable: {archived_index}"
    );
    assert!(archived_index.contains("Archived (1)"), "{archived_index}");
    assert!(
        archived_index.contains(&format!(r#"action="/s/{id}/restore" method="post""#)),
        "{archived_index}"
    );

    let (status, archived_session) = daemon.get(&format!("/s/{id}")).await;
    assert_eq!(status, 200, "{archived_session}");
    assert!(
        archived_session.contains(&format!(r#"action="/s/{id}/restore" method="post""#)),
        "{archived_session}"
    );

    let (status, restored_index) = daemon.post(&format!("/s/{id}/restore")).await;
    assert_eq!(status, 200, "{restored_index}");
    assert!(!daemon.store.with(&id, |session| session.archived).unwrap());
    assert!(
        restored_index.contains(&format!(r#"value="{id}""#)),
        "{restored_index}"
    );

    let (status, _) = daemon.post("/s/not-a-session/archive").await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn the_index_composes_selected_sessions_and_keeps_direct_links() {
    let daemon = Daemon::start().await;
    let first = titled_code_session(&daemon, "first change");
    let second = titled_code_session(&daemon, "second change");

    let (status, body) = daemon.get("/").await;
    assert_eq!(status, 200);
    assert!(body.contains(r#"id="session-picker" action="/" method="get""#));
    assert_eq!(
        body.matches(r#"<input type="checkbox" name="s""#).count(),
        2,
        "{body}"
    );
    assert!(body.contains(&format!(r#"value="{first}""#)), "{body}");
    assert!(body.contains(&format!(r#"value="{second}""#)), "{body}");
    assert!(body.contains("Open selected tabs"), "{body}");
    assert!(body.contains(&format!(r#"href="/s/{first}""#)), "{body}");
    assert!(body.contains(&format!(r#"href="/s/{second}""#)), "{body}");
    assert!(
        body.contains(&format!(r#"formaction="/s/{first}/archive""#)),
        "{body}"
    );
    assert!(
        body.contains(r#"form.querySelector('.picker-actions button[type="submit"]')"#),
        "the picker script must not disable the first session's archive button: {body}"
    );
}

#[tokio::test]
async fn grouped_sessions_are_ordered_deduplicated_and_share_the_first_reply() {
    let daemon = Daemon::start().await;
    let first = titled_code_session(&daemon, "first change");
    let second = titled_code_session(&daemon, "second change");

    let (status, body) = daemon
        .get(&format!("/?s={second}&s={first}&s={second}"))
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body.matches(r#"<button type="button" role="tab""#).count(),
        2,
        "{body}"
    );
    assert_eq!(
        body.matches(r#"<section class="review-panel" role="tabpanel""#)
            .count(),
        2,
        "{body}"
    );
    assert!(
        body.find("second change").unwrap() < body.find("first change").unwrap(),
        "query order was not preserved: {body}"
    );
    assert!(body.contains(r#"aria-controls="review-panel-0""#), "{body}");
    assert!(body.contains(r#"aria-labelledby="review-tab-0""#), "{body}");
    assert!(
        body.contains(&format!(r#"action="/s/{second}/reply""#)),
        "{body}"
    );
    assert!(
        !body.contains(&format!(r#"action="/s/{first}/reply""#)),
        "{body}"
    );
    assert_eq!(
        body.matches(&format!(r#"data-session="{second}""#)).count(),
        2,
        "one tab plus the shared reply should name the first session: {body}"
    );
    assert!(body.contains("ArrowRight"), "keyboard tab behavior missing");
    assert!(body.contains("event.key === 'Home'"), "{body}");
    assert!(body.contains("event.key === 'End'"), "{body}");
    assert_eq!(body.matches(r#"title="Archive tab""#).count(), 2, "{body}");
}

#[tokio::test]
async fn one_grouped_session_and_selected_tab_state_render() {
    let daemon = Daemon::start().await;
    let first = titled_code_session(&daemon, "first change");
    let second = titled_code_session(&daemon, "second change");

    let (status, one) = daemon.get(&format!("/?s={first}")).await;
    assert_eq!(status, 200, "{one}");
    assert_eq!(
        one.matches(r#"<button type="button" role="tab""#).count(),
        1,
        "{one}"
    );

    let (status, selected) = daemon
        .get(&format!("/?s={first}&s={second}&tab={second}"))
        .await;
    assert_eq!(status, 200, "{selected}");
    assert!(
        selected.contains(&format!(
            r#"<iframe class="review-frame" src="/s/{second}?embed=1""#
        )),
        "{selected}"
    );
    assert!(
        !selected.contains(&format!(
            r#"<iframe class="review-frame" src="/s/{first}?embed=1""#
        )),
        "inactive frame was not lazy: {selected}"
    );
    assert!(
        selected.contains("window.addEventListener('popstate'"),
        "{selected}"
    );
    assert!(
        selected.contains("url.searchParams.set('tab'"),
        "{selected}"
    );
}

#[tokio::test]
async fn invalid_grouped_selections_are_client_errors() {
    let daemon = Daemon::start().await;

    let (status, body) = daemon.get("/?s=").await;
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("empty"), "{body}");

    let (status, body) = daemon.get("/?s=not-a-session").await;
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("unknown session"), "{body}");

    let ids: Vec<_> = (0..13)
        .map(|index| titled_code_session(&daemon, &format!("change {index}")))
        .collect();
    let query = ids
        .iter()
        .map(|id| format!("s={id}"))
        .collect::<Vec<_>>()
        .join("&");
    let (status, body) = daemon.get(&format!("/?{query}")).await;
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("at most 12"), "{body}");
}

#[tokio::test]
async fn embedded_sessions_keep_review_content_without_duplicate_chrome() {
    let daemon = Daemon::start().await;
    let id = multiline_code_session(&daemon);

    let (status, body) = daemon.get(&format!("/s/{id}?embed=1")).await;
    assert_eq!(status, 200);
    assert!(body.contains(r#"class="embedded""#), "{body}");
    assert!(!body.contains(r#"<header class="bar">"#), "{body}");
    assert!(!body.contains(r#"id="reply""#), "{body}");
    assert!(!body.contains(r#"id="jump-top""#), "{body}");
    assert!(body.contains(r#"class="addnote""#), "{body}");
    assert!(body.contains("wdyt:commented"), "{body}");
    assert!(body.contains("handleFragment"), "{body}");
}

#[tokio::test]
async fn guided_content_renders_in_order_with_authoritative_block_targets() {
    let daemon = Daemon::start().await;
    let id = guided_session(&daemon);

    let (status, body) = daemon.get(&format!("/s/{id}")).await;
    assert_eq!(status, 200, "{body}");
    let prose = body.find("API narrative.").expect("guided prose");
    let diff = body.find("new value").expect("guided diff");
    let code = body.find("println!").expect("guided code");
    assert!(
        prose < diff && diff < code,
        "guided block order changed: {body}"
    );
    assert!(body.contains(r#"data-doc-target="guided:0""#), "{body}");
    assert!(body.contains(r#"data-doc-line-offset="9""#), "{body}");
    assert!(body.contains(r#"data-target="guided:1""#), "{body}");
    assert!(body.contains(r#"data-target="guided:2""#), "{body}");
    assert!(
        body.contains(&format!(
            r##"href="#{}""##,
            wdyt::fragment::line_fragment("guided:1", "new", 4)
        )),
        "guided diff link uses the display file instead of its block target: {body}"
    );
    assert!(
        body.contains(&format!(
            r##"href="#{}""##,
            wdyt::fragment::line_fragment("guided:2", "new", 2)
        )),
        "guided code link uses the display file instead of its block target: {body}"
    );
    assert!(
        body.contains(r#"data-from="1" data-to="3" data-place="before""#),
        "guided diff cannot expand above: {body}"
    );
    assert!(
        body.contains(r#"data-from="5" data-to="7" data-place="after""#),
        "guided diff cannot expand below: {body}"
    );
}

#[tokio::test]
async fn guided_comments_use_each_blocks_original_source_lines() {
    let daemon = Daemon::start().await;
    let id = guided_session(&daemon);

    let cases = [
        (
            serde_json::json!({
                "file": "untrusted",
                "target": "guided:0",
                "line": 12,
                "side": "new",
                "text": "explain the API"
            }),
            "guided:",
            "API narrative.",
        ),
        (
            serde_json::json!({
                "file": "untrusted",
                "target": "guided:1",
                "line": 4,
                "side": "new",
                "text": "explain the diff"
            }),
            "src/lib.rs",
            "new value",
        ),
        (
            serde_json::json!({
                "file": "untrusted",
                "target": "guided:2",
                "line": 2,
                "side": "new",
                "text": "explain the code"
            }),
            "src/main.rs",
            "    let value = 2;",
        ),
    ];

    for (request, expected_file, expected_snippet) in cases {
        let (status, body) = daemon
            .post_json(&format!("/s/{id}/comments"), request)
            .await;
        assert_eq!(status, 200, "{body}");
        let comment: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(comment["file"], expected_file, "{body}");
        assert_eq!(comment["snippet"], expected_snippet, "{body}");
    }

    let (_, page) = daemon.get(&format!("/s/{id}")).await;
    for text in ["explain the API", "explain the diff", "explain the code"] {
        assert!(page.contains(text), "missing guided comment {text}: {page}");
    }
}

#[tokio::test]
async fn guided_diff_context_can_expand_and_be_commented_on() {
    let daemon = Daemon::start().await;
    let id = guided_session(&daemon);

    let (status, body) = daemon
        .get(&format!("/s/{id}/context?file=1&from=1&to=2"))
        .await;
    assert_eq!(status, 200, "{body}");
    let context: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(context["file"], "src/lib.rs");
    assert_eq!(context["lines"][0]["new"], 1);
    assert!(
        context["lines"][0]["html"]
            .as_str()
            .unwrap()
            .contains("hidden"),
        "{body}"
    );

    let (status, body) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "ignored",
                "target": "guided:1",
                "line": 1,
                "side": "new",
                "text": "expanded context"
            }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let comment: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(comment["snippet"], "hidden one", "{body}");

    let (status, body) = daemon
        .get(&format!("/s/{id}/context?file=1&from=1&to=2"))
        .await;
    assert_eq!(status, 200, "{body}");
    let context: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        context["lines"][0]["comments"][0]["text"],
        "expanded context"
    );
    assert!(
        daemon
            .get(&format!("/s/{id}"))
            .await
            .1
            .contains("threadNode(line.comments[i])")
    );
}

#[tokio::test]
async fn grouped_pages_bridge_child_fragments_into_the_copyable_url() {
    let daemon = Daemon::start().await;
    let first = titled_code_session(&daemon, "first");
    let second = titled_code_session(&daemon, "second");
    let (_, body) = daemon.get(&format!("/?s={first}&s={second}")).await;
    assert!(body.contains("wdyt:fragment"), "{body}");
    assert!(body.contains("url.hash = event.data.hash"), "{body}");

    let (_, embedded) = daemon.get(&format!("/s/{first}?embed=1")).await;
    assert!(embedded.contains("wdyt:set-fragment"), "{embedded}");
    assert!(embedded.contains("window.parent.postMessage"), "{embedded}");
}

#[tokio::test]
async fn a_session_page_offers_a_reply_form() {
    let daemon = Daemon::start().await;
    let id = code_session(&daemon);

    let (status, body) = daemon.get(&format!("/s/{id}")).await;
    assert_eq!(status, 200);
    assert!(
        body.contains(&format!("action=\"/s/{id}/reply\"")),
        "{body}"
    );
    assert!(body.contains("<textarea"), "{body}");
    // The overlay must be fixed so it cannot reflow what is being reviewed.
    assert!(
        wdyt::ui::stylesheet_asset_for_test().contains("position: fixed"),
        "reply overlay is not fixed"
    );
}

#[tokio::test]
async fn a_replied_session_shows_the_reply_and_no_form() {
    let daemon = Daemon::start().await;
    let id = code_session(&daemon);
    assert!(daemon.store.reply(&id, "looks good".to_owned()).unwrap());

    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    assert!(body.contains("looks good"), "{body}");
    // The reply form's textarea carries `name="text"`; the comment script also
    // creates textareas (a code session is commentable), so match the reply one
    // specifically rather than any `<textarea`.
    assert!(
        !body.contains("name=\"text\""),
        "still offers a form: {body}"
    );
    // The toggle stays enabled so the reply can be re-read; a disabled one
    // would trap the text in a panel that cannot be opened. Matched as an
    // attribute, since the inline script mentions `disabled` too.
    assert!(!body.contains("disabled="), "{body}");
    assert!(body.contains("class=\"done\""), "{body}");
}

#[tokio::test]
async fn framed_modes_keep_the_header_visible() {
    let daemon = Daemon::start().await;
    let id = daemon.insert(Content::Demo {
        port: 4999,
        path: "/".to_owned(),
    });

    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    // `.framewrap` is absolutely positioned; without `body.framed main` being
    // its positioned ancestor it paints over the header.
    assert!(body.contains("class=\"framed\""), "{body}");
    assert!(
        wdyt::ui::stylesheet_asset_for_test().contains("body.framed main { position: relative; }"),
        "framed main is not a positioned ancestor"
    );
}

#[tokio::test]
async fn a_static_session_is_framed_too() {
    let daemon = Daemon::start().await;
    let dir = tempfile::tempdir().expect("a temp directory");
    let id = daemon.insert(Content::Static {
        root: dir.path().to_path_buf(),
        entry: "index.html".to_owned(),
    });

    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    assert!(body.contains("class=\"framed\""), "{body}");
}

#[tokio::test]
async fn an_unknown_session_is_not_found() {
    let daemon = Daemon::start().await;
    let (status, _) = daemon.get("/s/nosuch").await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn a_reply_is_accepted_once() {
    let daemon = Daemon::start().await;
    let id = code_session(&daemon);
    let url = format!("{}/s/{id}/reply", daemon.base);
    let client = reqwest::Client::new();

    let first: serde_json::Value = client
        .post(&url)
        .json(&serde_json::json!({ "text": "  ship it  " }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(first["accepted"], true);
    // Trimmed, so trailing whitespace from a textarea does not reach the agent.
    assert_eq!(first["text"], "ship it");

    let second: serde_json::Value = client
        .post(&url)
        .json(&serde_json::json!({ "text": "again" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(second["accepted"], false);
    assert!(second["message"].as_str().unwrap().contains("already"));
}

#[tokio::test]
async fn an_empty_reply_is_rejected() {
    let daemon = Daemon::start().await;
    let id = code_session(&daemon);

    let status = reqwest::Client::new()
        .post(format!("{}/s/{id}/reply", daemon.base))
        .json(&serde_json::json!({ "text": "   \n  " }))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, 400);
}

#[tokio::test]
async fn assets_cannot_escape_the_session_root() {
    let daemon = Daemon::start().await;
    // A real temp directory: unique per test and cleaned up on drop, so
    // concurrent runs cannot clear it out from under each other.
    let dir = tempfile::tempdir().expect("a temp directory");
    std::fs::write(dir.path().join("index.html"), "<p>inside</p>").unwrap();

    let id = daemon.insert(Content::Static {
        root: dir.path().to_path_buf(),
        entry: "index.html".to_owned(),
    });

    let (ok, body) = daemon.get(&format!("/s/{id}/assets/index.html")).await;
    assert_eq!(ok, 200);
    assert!(body.contains("inside"));

    for escape in ["../../../etc/passwd", "%2e%2e%2f%2e%2e%2fetc%2fpasswd"] {
        let (status, body) = daemon.get(&format!("/s/{id}/assets/{escape}")).await;
        assert_ne!(status, 200, "traversal served {escape}: {body}");
        assert!(!body.contains("root:"), "leaked /etc/passwd via {escape}");
    }
}

#[tokio::test]
async fn markdown_alerts_and_code_are_styled() {
    let daemon = Daemon::start().await;
    let doc = wdyt::render::markdown(
        "d.md",
        "> [!NOTE]\n> careful\n\n```rust\nfn f() {}\n```\n",
        "Nord",
    );
    let id = daemon.insert(Content::Docs { files: vec![doc] });

    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    // comrak emits the class; unstyled the callout reads as a stray heading.
    assert!(body.contains("markdown-alert"), "{body}");
    assert!(
        wdyt::ui::stylesheet_asset_for_test().contains(".markdown-alert-title"),
        "no alert styling"
    );
}

#[tokio::test]
async fn theme_colors_reach_the_page() {
    let daemon = Daemon::start().await;
    let id = code_session(&daemon);
    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    assert!(
        body.contains(&theme_colors(theme("Nord")).background),
        "{body}"
    );
}

#[tokio::test]
async fn a_diff_page_renders_both_sides_and_a_comment_affordance() {
    let daemon = Daemon::start().await;
    let id = diff_session(&daemon);

    let (status, body) = daemon.get(&format!("/s/{id}")).await;
    assert_eq!(status, 200);
    // Added and removed rows must be distinguishable, and both sides numbered.
    assert!(body.contains("class=\"add\""), "no added row: {body}");
    assert!(body.contains("class=\"del\""), "no removed row: {body}");
    assert!(body.contains("class=\"ctx\""), "no context row: {body}");
    // The comment button carries its own anchor, so the script needs no map.
    assert!(body.contains("class=\"addnote\""), "{body}");
    assert!(body.contains("data-side=\"old\""), "{body}");
    assert!(body.contains("data-side=\"new\""), "{body}");
    // And the script that binds to them is present, with the session id on the
    // container rather than inlined.
    assert!(body.contains(&format!("data-session=\"{id}\"")), "{body}");
}

#[tokio::test]
async fn a_comment_is_saved_and_shown_on_reload() {
    let daemon = Daemon::start().await;
    let id = diff_session(&daemon);

    let (status, body) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "src/lib.rs", "line": 2, "side": "new",
                "text": "why 2?"
            }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    // The quoted snippet comes from the server's own copy of the diff, not from
    // the request: the page is not trusted to say what a line said.
    assert!(body.contains("let x = 2;"), "{body}");

    let (_, page) = daemon.get(&format!("/s/{id}")).await;
    assert!(
        page.contains("why 2?"),
        "comment did not survive a reload: {page}"
    );
    assert!(page.contains("class=\"notes\""), "{page}");
}

#[tokio::test]
async fn a_comment_can_cover_a_range_of_lines() {
    let daemon = Daemon::start().await;
    let id = diff_session(&daemon);

    // Lines 1-3 on the new side: context, the added line, and the closing brace.
    let (status, body) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "src/lib.rs", "line": 1, "end_line": 3,
                "side": "new", "text": "this whole block"
            }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let comment: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(comment["line"], 1);
    assert_eq!(comment["end_line"], 3);
    // The snippet quotes every covered line, from the server's own copy.
    let snippet = comment["snippet"].as_str().unwrap();
    assert!(snippet.contains("fn main() {"), "{snippet}");
    assert!(snippet.contains("let x = 2;"), "{snippet}");
    assert_eq!(snippet.lines().count(), 3, "{snippet}");

    // The page marks the covered rows and says what the note is about.
    let (_, page) = daemon.get(&format!("/s/{id}")).await;
    assert!(page.contains("inrange"), "covered rows not marked: {page}");
    assert!(page.contains("L1–L3"), "no span label: {page}");
}

#[tokio::test]
async fn a_code_page_offers_a_comment_affordance() {
    let daemon = Daemon::start().await;
    let id = multiline_code_session(&daemon);

    let (status, body) = daemon.get(&format!("/s/{id}")).await;
    assert_eq!(status, 200);
    // The same comment button and script the diff uses, on a plain code listing.
    assert!(body.contains("class=\"addnote\""), "{body}");
    assert!(body.contains("data-side=\"new\""), "{body}");
    // The shared script binds to `[data-comments]`, and the session id travels on
    // the container rather than being inlined.
    assert!(body.contains("data-comments"), "{body}");
    assert!(body.contains(&format!("data-session=\"{id}\"")), "{body}");
}

#[tokio::test]
async fn a_comment_on_code_is_saved_and_shown_on_reload() {
    let daemon = Daemon::start().await;
    let id = multiline_code_session(&daemon);

    let (status, body) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "src/main.rs", "line": 2, "side": "new",
                "text": "why 1?"
            }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    // The quoted snippet comes from the server's own copy of the file, not from
    // the request: the page is not trusted to say what a line said.
    let comment: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(comment["snippet"], "    let x = 1;", "{body}");

    let (_, page) = daemon.get(&format!("/s/{id}")).await;
    assert!(
        page.contains("why 1?"),
        "comment did not survive a reload: {page}"
    );
    assert!(page.contains("class=\"notes\""), "{page}");
}

#[tokio::test]
async fn a_comment_on_code_can_cover_a_range_of_lines() {
    let daemon = Daemon::start().await;
    let id = multiline_code_session(&daemon);

    // Lines 2-3: the binding and the print.
    let (status, body) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "src/main.rs", "line": 2, "end_line": 3,
                "side": "new", "text": "this whole block"
            }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let comment: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(comment["line"], 2);
    assert_eq!(comment["end_line"], 3);
    // The snippet quotes every covered line, from the server's own copy.
    let snippet = comment["snippet"].as_str().unwrap();
    assert!(snippet.contains("let x = 1;"), "{snippet}");
    assert!(snippet.contains("println!"), "{snippet}");
    assert_eq!(snippet.lines().count(), 2, "{snippet}");

    // The page marks the covered rows and says what the note is about.
    let (_, page) = daemon.get(&format!("/s/{id}")).await;
    assert!(page.contains("inrange"), "covered rows not marked: {page}");
    assert!(page.contains("L2–L3"), "no span label: {page}");
}

#[tokio::test]
async fn a_comment_on_a_code_line_that_does_not_exist_is_rejected() {
    let daemon = Daemon::start().await;
    let id = multiline_code_session(&daemon);

    for bad in [
        serde_json::json!({"file": "src/main.rs", "line": 999, "side": "new", "text": "x"}),
        serde_json::json!({"file": "nope.rs", "line": 1, "side": "new", "text": "x"}),
    ] {
        let (status, body) = daemon
            .post_json(&format!("/s/{id}/comments"), bad.clone())
            .await;
        assert_eq!(status, 400, "accepted {bad}: {body}");
    }
    assert!(daemon.store.comments(&id).unwrap().is_empty());
}

#[tokio::test]
async fn a_backwards_range_is_normalised_rather_than_rejected() {
    // Dragging upwards is as natural as downwards, so the ends arrive reversed.
    let daemon = Daemon::start().await;
    let id = diff_session(&daemon);

    let (status, body) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "src/lib.rs", "line": 3, "end_line": 1,
                "side": "new", "text": "dragged upwards"
            }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let comment: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(comment["line"], 1, "ends not swapped: {body}");
    assert_eq!(comment["end_line"], 3);
}

#[tokio::test]
async fn a_single_line_comment_reports_no_range() {
    // The common case keeps its old shape, so `end_line` is absent rather than
    // echoing the start.
    let daemon = Daemon::start().await;
    let id = diff_session(&daemon);
    let (_, body) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({"file": "src/lib.rs", "line": 2, "side": "new", "text": "why 2?"}),
        )
        .await;
    let comment: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(comment["end_line"].is_null(), "{body}");
    let (_, page) = daemon.get(&format!("/s/{id}")).await;
    assert!(
        !page.contains("class=\"span\""),
        "a lone line claimed a span: {page}"
    );
}

#[tokio::test]
async fn a_comment_on_a_line_that_does_not_exist_is_rejected() {
    let daemon = Daemon::start().await;
    let id = diff_session(&daemon);

    for bad in [
        serde_json::json!({"file": "src/lib.rs", "line": 999, "side": "new", "text": "x"}),
        serde_json::json!({"file": "nope.rs", "line": 1, "side": "new", "text": "x"}),
        // The context line at 1 exists on both sides but is only addressable on
        // the new one, so a comment cannot be filed against it twice.
        serde_json::json!({"file": "src/lib.rs", "line": 1, "side": "old", "text": "x"}),
        serde_json::json!({"file": "src/lib.rs", "line": 1, "side": "sideways", "text": "x"}),
        serde_json::json!({"file": "src/lib.rs", "line": 1, "side": "new", "text": "  "}),
    ] {
        let (status, body) = daemon
            .post_json(&format!("/s/{id}/comments"), bad.clone())
            .await;
        assert_eq!(status, 400, "accepted {bad}: {body}");
    }
    assert!(daemon.store.comments(&id).unwrap().is_empty());
}

#[tokio::test]
async fn the_page_distinguishes_a_collected_reply_from_a_read_one() {
    // Three stages, not two. Collecting a reply moves bytes to a process;
    // reading it is a claim only a live model can make. An agent that collects
    // and then dies must not show as read, or the receipt is worthless.
    let daemon = Daemon::start().await;
    let id = code_session(&daemon);
    daemon.store.reply(&id, "have a look".to_owned()).unwrap();

    // Before collection the page says it is waiting, and says so in the markup
    // rather than only after a poll: a page loaded later must not flash.
    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    assert!(body.contains("receipt waiting"), "{body}");

    let (status, status_body) = daemon.get(&format!("/api/sessions/{id}/status")).await;
    assert_eq!(status, 200);
    let json: serde_json::Value = serde_json::from_str(&status_body).unwrap();
    assert_eq!(json["replied"], true);
    assert_eq!(json["delivered"], false);
    assert!(json["ack"].is_null());

    // Polling the status must not itself mark anything: otherwise the page
    // would be reporting its own visit as the agent's receipt.
    assert!(
        daemon
            .store
            .with(&id, |s| s.reply.as_ref().unwrap().delivered_at.is_none())
            .unwrap(),
        "the status route marked the reply delivered"
    );

    // Collected: picked up, but nothing has claimed to read it.
    let (status, _) = daemon.post(&format!("/api/sessions/{id}/collect")).await;
    assert_eq!(status, 200);

    let (_, status_body) = daemon.get(&format!("/api/sessions/{id}/status")).await;
    let json: serde_json::Value = serde_json::from_str(&status_body).unwrap();
    assert_eq!(json["delivered"], true);
    assert!(
        json["ack"].is_null(),
        "collecting invented an ack: {status_body}"
    );

    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    assert!(body.contains("receipt delivered"), "{body}");
    assert!(body.contains("not yet acknowledged"), "{body}");

    // Acked: an agent has said what it is doing, and the page quotes it.
    let (status, _) = daemon
        .post_json(
            &format!("/api/sessions/{id}/ack"),
            serde_json::json!({ "note": "rerunning the build with your flag" }),
        )
        .await;
    assert_eq!(status, 200);

    let (_, status_body) = daemon.get(&format!("/api/sessions/{id}/status")).await;
    let json: serde_json::Value = serde_json::from_str(&status_body).unwrap();
    assert_eq!(json["ack"]["note"], "rerunning the build with your flag");

    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    assert!(body.contains("receipt read"), "{body}");
    assert!(
        body.contains("rerunning the build with your flag"),
        "{body}"
    );
}

#[tokio::test]
async fn an_ack_needs_a_reply_and_a_note() {
    let daemon = Daemon::start().await;
    let id = code_session(&daemon);

    // Nothing to acknowledge yet.
    let (status, _) = daemon
        .post_json(
            &format!("/api/sessions/{id}/ack"),
            serde_json::json!({ "note": "on it" }),
        )
        .await;
    assert_eq!(status, 400);

    daemon.store.reply(&id, "look".to_owned()).unwrap();

    // An empty note is not an acknowledgement: the note is the whole signal.
    let (status, _) = daemon
        .post_json(
            &format!("/api/sessions/{id}/ack"),
            serde_json::json!({ "note": "   " }),
        )
        .await;
    assert_eq!(status, 400);

    let (status, _) = daemon
        .post_json(
            "/api/sessions/nosuch/ack",
            serde_json::json!({ "note": "on it" }),
        )
        .await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn a_timed_out_wait_leaves_the_reply_in_the_inbox() {
    // The mechanism the user asked for: a wait that gives up must not lose the
    // reply, so a later turn can still find it.
    let daemon = Daemon::start().await;
    let id = code_session(&daemon);

    // A wait that finds nothing must not claim a read.
    let (status, body) = daemon
        .get(&format!("/api/sessions/{id}/reply?timeout_secs=0"))
        .await;
    assert_eq!(status, 200);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap()["replied"],
        false
    );

    daemon
        .store
        .reply(&id, "sent much later".to_owned())
        .unwrap();

    let (status, body) = daemon.get("/api/inbox?unread=1").await;
    assert_eq!(status, 200);
    assert!(body.contains("sent much later"), "{body}");
    assert!(body.contains(&format!("/s/{id}")), "no link back: {body}");

    // Collecting it does NOT take it out of the unread inbox: a process that
    // fetched the bytes and then died is exactly the case still needing
    // attention, and filtering on delivery would hide it.
    daemon.post(&format!("/api/sessions/{id}/collect")).await;
    let (_, body) = daemon.get("/api/inbox?unread=1").await;
    assert!(
        body.contains("sent much later"),
        "collecting hid an unacknowledged reply: {body}"
    );

    // Acking is what clears it, since only that says something read it.
    daemon
        .post_json(
            &format!("/api/sessions/{id}/ack"),
            serde_json::json!({ "note": "got it" }),
        )
        .await;
    let (_, body) = daemon.get("/api/inbox?unread=1").await;
    assert!(!body.contains("sent much later"), "still unread: {body}");
    let (_, body) = daemon.get("/api/inbox").await;
    assert!(
        body.contains("sent much later"),
        "dropped from the inbox: {body}"
    );
}

#[tokio::test]
async fn the_page_says_which_agent_and_directory_it_came_from() {
    // With several agents running at once the title alone does not say which one
    // is asking, and the pane name is where you go to talk to it.
    let daemon = Daemon::start().await;
    let id = daemon.store.insert_new(wdyt::session::NewSession {
        origin: wdyt::zellij_origin::Origin {
            cwd: Some("/local/home/rcoh/code/wdyt".to_owned()),
            session: Some("dd-2".to_owned()),
            tab: Some("wdyt cli".to_owned()),
            pane: Some("Build wdyt CLI tool".to_owned()),
        },
        ..wdyt::session::NewSession::new("a title".to_owned(), Content::Code { files: vec![] })
    });

    let (status, body) = daemon.get(&format!("/s/{id}")).await;
    assert_eq!(status, 200);
    assert!(body.contains("dd-2"), "no session name: {body}");
    assert!(body.contains("wdyt cli"), "no tab name: {body}");
    assert!(
        body.contains("/local/home/rcoh/code/wdyt"),
        "no cwd: {body}"
    );
}

#[tokio::test]
async fn a_guided_review_renders_above_the_content_with_working_anchors() {
    // The agent's own tour of the work. Its links have to resolve to the file
    // sections on the same page, or the tour points at nothing.
    let daemon = Daemon::start().await;
    let files = wdyt::diff::parse(PATCH, theme("Nord")).unwrap();
    let brief = wdyt::render::markdown(
        "brief",
        "Start with [the library](#f-src-lib-rs).\n",
        "Nord",
    );
    let id = daemon.store.insert_new(wdyt::session::NewSession {
        brief: Some(brief.html),
        ..wdyt::session::NewSession::new("a title".to_owned(), Content::Diff { files })
    });

    let (status, body) = daemon.get(&format!("/s/{id}")).await;
    assert_eq!(status, 200);
    assert!(body.contains("class=\"brief doc\""), "{body}");
    assert!(body.contains("Start with"), "{body}");
    // The anchor the brief links to must be a real id on the page.
    assert!(body.contains("href=\"#f-src-lib-rs\""), "{body}");
    assert!(
        body.contains("id=\"f-src-lib-rs\""),
        "the anchor does not exist: {body}"
    );
}

#[tokio::test]
async fn a_session_without_a_brief_renders_no_empty_section() {
    let daemon = Daemon::start().await;
    let id = diff_session(&daemon);
    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    assert!(!body.contains("class=\"brief"), "{body}");
    // Single-file sessions should not have the file nav (avoids noisy UI).
    assert!(!body.contains("class=\"files"), "{body}");
    // But multi-file sessions retain the nav.
    let multi = multi_file_diff_session(&daemon);
    let (_, mbody) = daemon.get(&format!("/s/{multi}")).await;
    assert!(mbody.contains("class=\"files"), "{mbody}");
}

#[tokio::test]
async fn a_sessions_own_theme_colours_its_page() {
    // Highlighting happens in the CLI but the page's colours are chosen by the
    // daemon, so a per-session theme has to reach the stylesheet — otherwise a
    // light theme's code lands on the default's dark card.
    let daemon = Daemon::start().await;
    let file = wdyt::render::highlight("x.rs", "fn main() {}\n", theme("GitHub")).unwrap();
    let id = daemon.store.insert_new(wdyt::session::NewSession {
        theme: Some("GitHub".to_owned()),
        ..wdyt::session::NewSession::new("a title".to_owned(), Content::Code { files: vec![file] })
    });

    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    let github = theme_colors(theme("GitHub")).background;
    assert!(
        body.contains(&github),
        "the session's theme did not reach the page"
    );
    // And the daemon's own default must not also be in there fighting it.
    let default = theme_colors(theme("Nord")).background;
    assert!(
        !body.contains(&default) || github == default,
        "the default theme's background leaked in too"
    );
}

#[tokio::test]
async fn an_unknown_session_theme_falls_back_rather_than_failing() {
    // A theme name from a newer CLI, or a typo that got past the daemon: the page
    // must still render.
    let daemon = Daemon::start().await;
    let id = daemon.store.insert_new(wdyt::session::NewSession {
        theme: Some("No Such Theme".to_owned()),
        ..wdyt::session::NewSession::new("a title".to_owned(), Content::Code { files: vec![] })
    });
    let (status, _) = daemon.get(&format!("/s/{id}")).await;
    assert_eq!(status, 200);
}

#[tokio::test]
async fn a_session_from_a_plain_shell_shows_no_empty_origin() {
    // Nothing detected must render nothing, not an empty widget with stray
    // separators in it.
    let daemon = Daemon::start().await;
    let id = code_session(&daemon);
    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    assert!(!body.contains("class=\"origin\""), "{body}");
}

#[tokio::test]
async fn the_top_bar_can_be_hidden_on_a_framed_page() {
    let daemon = Daemon::start().await;
    let id = daemon.insert(Content::Demo {
        port: 4999,
        path: "/".to_owned(),
    });

    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    assert!(
        body.contains("id=\"chrome-toggle\""),
        "no hide control: {body}"
    );
    // And a way back, or the page needs a reload to escape.
    assert!(body.contains("id=\"chrome-show\""), "{body}");
}

#[tokio::test]
async fn the_raw_form_of_a_diff_is_the_patch() {
    let daemon = Daemon::start().await;
    let id = diff_session(&daemon);
    let (status, body) = daemon.get(&format!("/s/{id}/raw")).await;
    assert_eq!(status, 200);
    // Reconstructed rather than stored verbatim, so check it round-trips.
    let files = wdyt::diff::parse(&body, theme("Nord")).expect("re-parses as a diff");
    assert_eq!(files[0].label, "src/lib.rs");
    assert_eq!((files[0].added, files[0].removed), (1, 1));
}

#[tokio::test]
async fn the_daemon_steps_over_a_port_someone_else_holds() {
    // The port range was chosen because those ports are already forwarded,
    // which also means they are already full of other people's servers. A
    // daemon that insisted on the lowest port could never start.
    // A two-port range with the lower one held: the daemon must take the upper.
    // A wider range would pass without exercising anything.
    let (squatter, taken, free) = loop {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
        let taken = listener.local_addr().unwrap().port();
        let next = taken + 1;
        if wdyt::ports::is_free(IpAddr::from([127, 0, 0, 1]), next) {
            break (listener, taken, next);
        }
    };

    let config = Config {
        port_low: taken,
        port_high: free,
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

    // The client discovers the port rather than assuming it, and identifies the
    // daemon by a marker: the squatter would otherwise be indistinguishable.
    let client = wdyt::client::Client::new(&config);
    for _ in 0..100 {
        if client.is_up().await {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        client.daemon_port().await,
        Some(free),
        "daemon did not land on the free port (taken={taken}, free={free})"
    );
    drop(squatter);
}

#[tokio::test]
async fn a_plain_server_in_the_range_is_not_mistaken_for_the_daemon() {
    // Several of the servers that live in this range answer 200 on /health.
    // Without a marker the client would post sessions into one of them.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                use tokio::io::AsyncWriteExt as _;
                let body = "{\"ok\":true}";
                let _ = socket
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\
                             Content-Type: application/json\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await;
            });
        }
    });

    let config = Config {
        port_low: port,
        port_high: port,
        bind: IpAddr::from([127, 0, 0, 1]),
        webhook_url: None,
        ..Config::default()
    };
    let client = wdyt::client::Client::new(&config);
    assert!(!client.is_up().await, "an unrelated server passed as wdyt");
}

#[tokio::test]
async fn comments_have_stable_anchor_ids() {
    // Each comment needs a stable id so a URL with #comment-<id> scrolls to it.
    let daemon = Daemon::start().await;
    let id = diff_session(&daemon);

    let (status, body) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "src/lib.rs", "line": 2, "side": "new",
                "text": "why 2?"
            }),
        )
        .await;
    assert_eq!(status, 200);
    let comment: serde_json::Value = serde_json::from_str(&body).unwrap();
    let comment_id = comment["id"].as_u64().unwrap();

    let (_, page) = daemon.get(&format!("/s/{id}")).await;
    // The comment div must carry an id matching the fragment scheme.
    let expected_id = format!("id=\"comment-{comment_id}\"");
    assert!(
        page.contains(&expected_id),
        "comment anchor not found: {expected_id}\n{page}"
    );
}

#[tokio::test]
async fn comments_on_code_have_stable_anchor_ids() {
    let daemon = Daemon::start().await;
    let id = multiline_code_session(&daemon);

    let (status, body) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "src/main.rs", "line": 2, "side": "new",
                "text": "why 1?"
            }),
        )
        .await;
    assert_eq!(status, 200);
    let comment: serde_json::Value = serde_json::from_str(&body).unwrap();
    let comment_id = comment["id"].as_u64().unwrap();

    let (_, page) = daemon.get(&format!("/s/{id}")).await;
    let expected_id = format!("id=\"comment-{comment_id}\"");
    assert!(
        page.contains(&expected_id),
        "comment anchor not found on code page: {expected_id}\n{page}"
    );
}

#[tokio::test]
async fn the_fragment_script_is_included_on_commentable_pages() {
    let daemon = Daemon::start().await;
    let id = diff_session(&daemon);

    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    // The fragment script handles deep-link scrolling and highlighting.
    assert!(
        body.contains("handleFragment"),
        "fragment JS missing: {body}"
    );
    assert!(
        body.contains("sel-highlight"),
        "highlight class missing from JS: {body}"
    );
    assert!(body.contains("b64decode"), "base64 helpers missing: {body}");
}

#[tokio::test]
async fn the_fragment_script_is_included_on_code_pages() {
    let daemon = Daemon::start().await;
    let id = multiline_code_session(&daemon);

    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    assert!(
        body.contains("handleFragment"),
        "fragment JS missing: {body}"
    );
}

#[tokio::test]
async fn the_fragment_script_is_not_included_on_non_commentable_pages() {
    let daemon = Daemon::start().await;
    let id = daemon.insert(Content::Demo {
        port: 4999,
        path: "/".to_owned(),
    });

    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    // Non-commentable pages should not carry the fragment script.
    assert!(
        !body.contains("handleFragment"),
        "fragment JS present on demo: {body}"
    );
}

#[tokio::test]
async fn deep_link_highlight_styles_are_present() {
    let daemon = Daemon::start().await;
    let id = code_session(&daemon);

    let _ = daemon.get(&format!("/s/{id}")).await;
    let css = wdyt::ui::stylesheet_asset_for_test();
    assert!(
        css.contains("tr.sel-highlight td"),
        "no highlight style: {css}"
    );
    assert!(
        css.contains("wdyt-flash"),
        "no flash animation for comments: {css}"
    );
}

#[tokio::test]
async fn existing_file_anchors_still_work() {
    // The deep-link scheme must not break the existing #f-<path> file anchors
    // used by the nav bar and guided-review briefs.
    let daemon = Daemon::start().await;
    let id = diff_session(&daemon);

    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    // File sections still have the old-style id even on single-file sessions.
    assert!(
        body.contains("id=\"f-src-lib-rs\""),
        "file anchor missing: {body}"
    );
    // On a multi-file session, the nav links to them.
    let multi = multi_file_diff_session(&daemon);
    let (_, mbody) = daemon.get(&format!("/s/{multi}")).await;
    assert!(
        mbody.contains("href=\"#f-src-lib-rs\""),
        "nav link to file anchor missing: {mbody}"
    );
    assert!(
        mbody.contains("id=\"f-src-lib-rs\""),
        "file anchor missing on multi: {mbody}"
    );
}

#[tokio::test]
async fn the_file_sidebar_preserves_the_useful_end_of_long_paths() {
    let daemon = Daemon::start().await;
    let labels = [
        "packages/accounts/src/http/handlers/create_account_request.rs",
        "packages/accounts/src/http/handlers/delete_account_request.rs",
        "packages/billing/src/http/handlers/create_invoice_request.rs",
        "packages/billing/src/http/handlers/delete_invoice_request.rs",
    ];
    let files = labels
        .iter()
        .map(|label| wdyt::render::highlight(label, "fn handle() {}\n", theme("Nord")).unwrap())
        .collect();
    let id = daemon.insert(Content::Code { files });

    let (status, body) = daemon.get(&format!("/s/{id}")).await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#"class="files has-sidebar""#), "{body}");
    for label in labels {
        assert!(body.contains(&format!(r#"title="{label}""#)), "{body}");
        assert!(
            body.contains(&format!(r#"<span class="file-label">{label}</span>"#)),
            "{body}"
        );
    }
    assert!(
        wdyt::ui::stylesheet_asset_for_test().contains("direction: rtl; text-align: left"),
        "sidebar does not truncate paths from the left"
    );
}

#[tokio::test]
async fn the_fragment_js_handles_hashchange_and_line_clicks() {
    let daemon = Daemon::start().await;
    let id = code_session(&daemon);

    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    // The script listens for hashchange.
    assert!(
        body.contains("addEventListener('hashchange'"),
        "no hashchange listener: {body}"
    );
    // It intercepts line-number link clicks to set sel fragments.
    assert!(
        body.contains("td.ln a[href^="),
        "no line-number click interception: {body}"
    );
    // The replaceState call updates the URL fragment without a page reload.
    assert!(
        body.contains("history.replaceState"),
        "no URL update: {body}"
    );
}

#[tokio::test]
async fn code_page_renders_canonical_sel_hrefs_on_line_numbers() {
    let daemon = Daemon::start().await;
    let id = multiline_code_session(&daemon);

    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    // Line number links must be canonical sel- fragments, not bare #L<n>.
    // src/main.rs encoded as base64url is "c3JjL21haW4ucnM".
    let expected_href = format!(
        "href=\"#{}\"",
        wdyt::fragment::line_fragment("src/main.rs", "new", 1)
    );
    assert!(
        body.contains(&expected_href),
        "canonical sel href not found for code line 1: {body}"
    );
    // Line 2 also present.
    let expected_href2 = format!(
        "href=\"#{}\"",
        wdyt::fragment::line_fragment("src/main.rs", "new", 2)
    );
    assert!(
        body.contains(&expected_href2),
        "canonical sel href not found for code line 2: {body}"
    );
}

#[tokio::test]
async fn diff_page_renders_canonical_sel_hrefs_on_line_numbers() {
    let daemon = Daemon::start().await;
    let id = diff_session(&daemon);

    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    // The diff for src/lib.rs should have canonical hrefs on both sides.
    let new_href = format!(
        "href=\"#{}\"",
        wdyt::fragment::line_fragment("src/lib.rs", "new", 2)
    );
    assert!(
        body.contains(&new_href),
        "canonical sel href not found for diff new-side line 2: {body}"
    );
    let old_href = format!(
        "href=\"#{}\"",
        wdyt::fragment::line_fragment("src/lib.rs", "old", 2)
    );
    assert!(
        body.contains(&old_href),
        "canonical sel href not found for diff old-side line 2: {body}"
    );
}

#[tokio::test]
async fn comments_have_discoverable_permalinks() {
    let daemon = Daemon::start().await;
    let id = diff_session(&daemon);

    let (_, body) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "src/lib.rs", "line": 2, "side": "new",
                "text": "why 2?"
            }),
        )
        .await;
    let comment: serde_json::Value = serde_json::from_str(&body).unwrap();
    let comment_id = comment["id"].as_u64().unwrap();

    let (_, page) = daemon.get(&format!("/s/{id}")).await;
    // A permalink link should be rendered with href pointing to the comment anchor.
    let expected_link = format!("href=\"#{}\"", wdyt::fragment::comment_fragment(comment_id));
    assert!(
        page.contains(&expected_link),
        "comment permalink not found: {page}"
    );
    assert!(
        page.contains("class=\"permalink\""),
        "permalink class not found: {page}"
    );
}

#[tokio::test]
async fn code_comments_have_discoverable_permalinks() {
    let daemon = Daemon::start().await;
    let id = multiline_code_session(&daemon);

    let (_, body) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "src/main.rs", "line": 2, "side": "new",
                "text": "note here"
            }),
        )
        .await;
    let comment: serde_json::Value = serde_json::from_str(&body).unwrap();
    let comment_id = comment["id"].as_u64().unwrap();

    let (_, page) = daemon.get(&format!("/s/{id}")).await;
    let expected_link = format!("href=\"#{}\"", wdyt::fragment::comment_fragment(comment_id));
    assert!(
        page.contains(&expected_link),
        "comment permalink not found on code page: {page}"
    );
}

#[tokio::test]
async fn fragment_js_validates_safe_integers_in_page() {
    let daemon = Daemon::start().await;
    let id = diff_session(&daemon);

    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    // The page's JS must validate line numbers as safe integers.
    assert!(
        body.contains("Number.MAX_SAFE_INTEGER"),
        "no safe integer validation in page JS: {body}"
    );
    // And must NOT contain the old numeric range iteration pattern.
    assert!(
        !body.contains("line <= sel.end"),
        "still uses numeric range iteration: {body}"
    );
}

#[tokio::test]
async fn fragment_js_respects_reduced_motion_in_page() {
    let daemon = Daemon::start().await;
    let id = diff_session(&daemon);

    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    // The page's fragment JS should check prefers-reduced-motion.
    assert!(
        body.contains("prefers-reduced-motion: reduce"),
        "no reduced motion check in fragment JS: {body}"
    );
}

#[tokio::test]
async fn comment_js_uses_safe_path_matching_in_page() {
    let daemon = Daemon::start().await;
    let id = diff_session(&daemon);

    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    // The comment JS must use getAttribute comparison, NOT string interpolation
    // into CSS selectors.
    assert!(
        body.contains("getAttribute('data-file') === file"),
        "comment JS not using safe attribute comparison: {body}"
    );
    assert!(
        !body.contains(r#"'[data-file="' + file"#),
        "comment JS still uses unsafe selector interpolation: {body}"
    );
}

#[tokio::test]
async fn comment_js_updates_url_on_gesture_in_page() {
    let daemon = Daemon::start().await;
    let id = diff_session(&daemon);

    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    // When the comment editor opens, the URL fragment should update.
    assert!(
        body.contains("selFragment(filePath, side, lo, hi)"),
        "no URL update on comment gesture: {body}"
    );
}

#[tokio::test]
async fn diff_with_dashes_in_path_renders_correct_hrefs() {
    // Paths containing dashes produce base64url that also contains dashes.
    // The server must encode them correctly.
    let patch = "\
--- a/src/my-mod/test-file.rs
+++ b/src/my-mod/test-file.rs
@@ -1,2 +1,2 @@
 fn main() {
-    old();
+    new();
 }
";
    let daemon = Daemon::start().await;
    let files = wdyt::diff::parse(patch, theme("Nord")).unwrap();
    let id = daemon.insert(Content::Diff { files });

    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    // The canonical href for new-side line 2 must round-trip via the fragment module.
    let expected_href = format!(
        "href=\"#{}\"",
        wdyt::fragment::line_fragment("src/my-mod/test-file.rs", "new", 2)
    );
    assert!(
        body.contains(&expected_href),
        "canonical href for dash-path not found: {expected_href}\n{body}"
    );
}

// --- Docs mode comment tests ------------------------------------------------

const DOC_SOURCE: &str = "\
# Design

A paragraph of text.

- Item one
- Item two

```rust
fn hello() {}
```
";

fn docs_session(daemon: &Daemon) -> String {
    let doc = wdyt::render::markdown_commentable("DESIGN.md", DOC_SOURCE, "Nord");
    daemon.insert(Content::Docs { files: vec![doc] })
}

#[tokio::test]
async fn a_docs_page_has_comment_affordance() {
    let daemon = Daemon::start().await;
    let id = docs_session(&daemon);

    let (status, body) = daemon.get(&format!("/s/{id}")).await;
    assert_eq!(status, 200);
    // Docs pages must include the doc comment script and data-sourcepos blocks.
    assert!(
        body.contains("data-sourcepos"),
        "no sourcepos attributes in docs page: {body}"
    );
    assert!(
        body.contains("data-doc-file=\"DESIGN.md\""),
        "no data-doc-file attribute: {body}"
    );
    // Each docs root must carry data-doc-target for explicit identity.
    assert!(
        body.contains("data-doc-target=\"doc:0\""),
        "no data-doc-target attribute: {body}"
    );
    assert!(
        body.contains("doc-addnote"),
        "no doc comment script reference: {body}"
    );
    // The comment and fragment scripts must be included.
    assert!(
        body.contains("data-comments=\"1\""),
        "docs page missing data-comments: {body}"
    );
    assert!(
        body.contains(&format!("data-session=\"{id}\"")),
        "missing session id: {body}"
    );
}

#[tokio::test]
async fn a_comment_on_a_doc_is_saved_with_authoritative_snippet() {
    let daemon = Daemon::start().await;
    let id = docs_session(&daemon);

    // Line 3 is "A paragraph of text." in DOC_SOURCE.
    let (status, body) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "DESIGN.md", "line": 3, "side": "new",
                "text": "expand on this"
            }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let comment: serde_json::Value = serde_json::from_str(&body).unwrap();
    // The snippet must be the authoritative source line, not client-submitted.
    assert_eq!(
        comment["snippet"], "A paragraph of text.",
        "wrong snippet: {body}"
    );
    assert_eq!(comment["file"], "DESIGN.md");
    assert_eq!(comment["side"], "new");
}

#[tokio::test]
async fn a_doc_comment_is_shown_on_reload() {
    let daemon = Daemon::start().await;
    let id = docs_session(&daemon);

    let (status, _) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "DESIGN.md", "line": 3, "side": "new",
                "text": "needs more detail"
            }),
        )
        .await;
    assert_eq!(status, 200);

    // The page should include the comment data for hydration.
    let (_, page) = daemon.get(&format!("/s/{id}")).await;
    assert!(
        page.contains("needs more detail"),
        "comment not found on reload: {page}"
    );
    assert!(
        page.contains("id=\"wdyt-doc-comments\""),
        "no doc comments JSON on page: {page}"
    );
}

#[tokio::test]
async fn a_comment_on_a_doc_line_that_does_not_exist_is_rejected() {
    let daemon = Daemon::start().await;
    let id = docs_session(&daemon);

    // Line 999 does not exist.
    let (status, body) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "DESIGN.md", "line": 999, "side": "new",
                "text": "nope"
            }),
        )
        .await;
    assert_eq!(status, 400, "accepted non-existent line: {body}");

    // Wrong file.
    let (status, body) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "nope.md", "line": 1, "side": "new",
                "text": "nope"
            }),
        )
        .await;
    assert_eq!(status, 400, "accepted non-existent file: {body}");

    assert!(daemon.store.comments(&id).unwrap().is_empty());
}

#[tokio::test]
async fn a_doc_comment_range_gets_multi_line_snippet() {
    let daemon = Daemon::start().await;
    let id = docs_session(&daemon);

    // Lines 5-6: "- Item one\n- Item two"
    let (status, body) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "DESIGN.md", "line": 5, "end_line": 6,
                "side": "new", "text": "combine these"
            }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let comment: serde_json::Value = serde_json::from_str(&body).unwrap();
    let snippet = comment["snippet"].as_str().unwrap();
    assert!(snippet.contains("- Item one"), "{snippet}");
    assert!(snippet.contains("- Item two"), "{snippet}");
    assert_eq!(snippet.lines().count(), 2, "{snippet}");
}

#[tokio::test]
async fn a_brief_comment_is_saved_separately_from_doc_comments() {
    let daemon = Daemon::start().await;
    let brief_source = "Start with [the library](#f-src-lib-rs).\n\nSecond paragraph.\n";
    let brief_doc = wdyt::render::markdown_commentable("brief", brief_source, "Nord");
    let files = wdyt::diff::parse(PATCH, theme("Nord")).unwrap();
    let id = daemon.store.insert_new(wdyt::session::NewSession {
        brief: Some(brief_doc.html),
        brief_sources: brief_doc.sources,
        ..wdyt::session::NewSession::new("a title".to_owned(), Content::Diff { files })
    });

    // Comment on the brief (line 1 is "Start with ...").
    let (status, body) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "brief:", "line": 1, "side": "new",
                "text": "good intro"
            }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let comment: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        comment["snippet"], "Start with [the library](#f-src-lib-rs).",
        "wrong brief snippet: {body}"
    );
    assert_eq!(comment["file"], "brief:");

    // The brief comment should appear on the page.
    let (_, page) = daemon.get(&format!("/s/{id}")).await;
    assert!(
        page.contains("good intro"),
        "brief comment not on page: {page}"
    );
    assert!(
        page.contains("id=\"wdyt-brief-comments\""),
        "no brief comments JSON: {page}"
    );
}

#[tokio::test]
async fn brief_and_doc_comments_do_not_collide_even_with_same_label() {
    // A session could hypothetically have a doc file labeled "brief" and also
    // have a guided-review brief. The file identifiers must not collide.
    let daemon = Daemon::start().await;
    let brief_source = "The brief text.\n";
    let brief_doc = wdyt::render::markdown_commentable("brief", brief_source, "Nord");
    // A doc file also using a similar name.
    let doc = wdyt::render::markdown_commentable("brief", "Doc with same label.\n", "Nord");
    let id = daemon.store.insert_new(wdyt::session::NewSession {
        brief: Some(brief_doc.html),
        brief_sources: brief_doc.sources,
        ..wdyt::session::NewSession::new("a title".to_owned(), Content::Docs { files: vec![doc] })
    });

    // Comment on the brief (file = "brief:").
    let (status, _) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "brief:", "line": 1, "side": "new",
                "text": "on the brief"
            }),
        )
        .await;
    assert_eq!(status, 200);

    // Comment on the doc file (file = "brief").
    let (status, _) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "brief", "line": 1, "side": "new",
                "text": "on the doc"
            }),
        )
        .await;
    assert_eq!(status, 200);

    // Both should be stored and distinct.
    let comments = daemon.store.comments(&id).unwrap();
    assert_eq!(comments.len(), 2);
    assert_eq!(comments[0].file, "brief:");
    assert_eq!(comments[0].text, "on the brief");
    assert_eq!(comments[1].file, "brief");
    assert_eq!(comments[1].text, "on the doc");
    // Snippets should come from the right sources.
    assert_eq!(comments[0].snippet, "The brief text.");
    assert_eq!(comments[1].snippet, "Doc with same label.");
}

#[tokio::test]
async fn a_brief_comment_on_invalid_line_is_rejected() {
    let daemon = Daemon::start().await;
    let brief_source = "One line.\n";
    let brief_doc = wdyt::render::markdown_commentable("brief", brief_source, "Nord");
    let files = wdyt::diff::parse(PATCH, theme("Nord")).unwrap();
    let id = daemon.store.insert_new(wdyt::session::NewSession {
        brief: Some(brief_doc.html),
        brief_sources: brief_doc.sources,
        ..wdyt::session::NewSession::new("a title".to_owned(), Content::Diff { files })
    });

    let (status, body) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "brief:", "line": 999, "side": "new",
                "text": "nope"
            }),
        )
        .await;
    assert_eq!(status, 400, "accepted invalid brief line: {body}");
    assert!(daemon.store.comments(&id).unwrap().is_empty());
}

#[tokio::test]
async fn doc_comments_have_stable_anchor_ids() {
    let daemon = Daemon::start().await;
    let id = docs_session(&daemon);

    let (_, body) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "DESIGN.md", "line": 3, "side": "new",
                "text": "nice"
            }),
        )
        .await;
    let comment: serde_json::Value = serde_json::from_str(&body).unwrap();
    let comment_id = comment["id"].as_u64().unwrap();

    // The page should include the comment data with the id as a DOM attribute.
    let (_, page) = daemon.get(&format!("/s/{id}")).await;
    assert!(
        page.contains(&format!("data-comment-id=\"{comment_id}\"")),
        "comment id not in page DOM: {page}"
    );
}

#[tokio::test]
async fn docs_page_preserves_gfm_features() {
    // Tables, task lists, footnotes, callouts, and highlighted fences must all
    // still render correctly even with sourcepos enabled.
    let daemon = Daemon::start().await;
    let source = "\
# Title

| Col A | Col B |
|-------|-------|
| 1     | 2     |

- [x] Done
- [ ] Todo

> [!NOTE]
> Important

```rust
fn main() {}
```
";
    let doc = wdyt::render::markdown_commentable("features.md", source, "Nord");
    let id = daemon.insert(Content::Docs { files: vec![doc] });

    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    // Table
    assert!(body.contains("<table"), "no table: {body}");
    assert!(body.contains("<th"), "no table header: {body}");
    // Task list
    assert!(body.contains("checkbox"), "no checkbox: {body}");
    // Callout
    assert!(body.contains("markdown-alert"), "no alert callout: {body}");
    // Highlighted code fence
    assert!(body.contains("style"), "no syntax highlighting: {body}");
    // Heading anchors still work
    assert!(body.contains("id=\"title\""), "no heading anchor: {body}");
    // data-sourcepos present
    assert!(body.contains("data-sourcepos"), "no sourcepos: {body}");
}

#[tokio::test]
async fn docs_page_still_has_file_nav_and_heading_anchors() {
    let daemon = Daemon::start().await;
    let doc1 = wdyt::render::markdown_commentable("README.md", "# Hello\n\nWorld.\n", "Nord");
    let doc2 = wdyt::render::markdown_commentable("DESIGN.md", "# Design\n\nPlan.\n", "Nord");
    let id = daemon.insert(Content::Docs {
        files: vec![doc1, doc2],
    });

    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    // File nav should show both files.
    assert!(body.contains("class=\"files\""), "no file nav: {body}");
    assert!(
        body.contains("href=\"#f-readme-md\""),
        "no nav link to README: {body}"
    );
    assert!(
        body.contains("id=\"f-readme-md\""),
        "no file section anchor for README: {body}"
    );
    assert!(
        body.contains("id=\"f-design-md\""),
        "no file section anchor for DESIGN: {body}"
    );
    // Heading anchors from comrak are still present.
    assert!(
        body.contains("id=\"hello\"") || body.contains("id=\"design\""),
        "no heading id: {body}"
    );
}

#[tokio::test]
async fn the_fragment_script_is_included_on_docs_pages() {
    // Docs are now commentable, so the fragment script should be present.
    let daemon = Daemon::start().await;
    let id = docs_session(&daemon);
    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    assert!(
        body.contains("handleFragment"),
        "fragment JS missing on docs page: {body}"
    );
}

#[tokio::test]
async fn a_commentable_brief_on_a_code_session_works() {
    // The brief should be commentable even on code/diff sessions.
    let daemon = Daemon::start().await;
    let brief_source = "Look at line 2.\n";
    let brief_doc = wdyt::render::markdown_commentable("brief", brief_source, "Nord");
    let file = wdyt::render::highlight("x.rs", "fn main() {}\n", theme("Nord")).unwrap();
    let id = daemon.store.insert_new(wdyt::session::NewSession {
        brief: Some(brief_doc.html),
        brief_sources: brief_doc.sources,
        ..wdyt::session::NewSession::new("a title".to_owned(), Content::Code { files: vec![file] })
    });

    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    assert!(
        body.contains("data-doc-file=\"brief:\""),
        "brief not marked as commentable: {body}"
    );
    assert!(
        body.contains("data-sourcepos"),
        "brief has no sourcepos: {body}"
    );

    // Should be able to comment on the brief.
    let (status, resp) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "brief:", "line": 1, "side": "new",
                "text": "good note"
            }),
        )
        .await;
    assert_eq!(status, 200, "{resp}");
    let comment: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(comment["snippet"], "Look at line 2.");

    // Should also still be able to comment on the code.
    let (status, resp) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "x.rs", "line": 1, "side": "new",
                "text": "rename this"
            }),
        )
        .await;
    assert_eq!(status, 200, "{resp}");
    let comment: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(comment["snippet"], "fn main() {}");
}

#[tokio::test]
async fn collected_doc_comments_include_file_identity() {
    // When collecting comments via the API, doc and brief comments must carry
    // their file identity clearly.
    let daemon = Daemon::start().await;
    let id = docs_session(&daemon);

    daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "DESIGN.md", "line": 1, "side": "new",
                "text": "nice heading"
            }),
        )
        .await;

    let (status, body) = daemon.get(&format!("/api/sessions/{id}/comments")).await;
    assert_eq!(status, 200);
    let comments: serde_json::Value = serde_json::from_str(&body).unwrap();
    let list = comments["comments"].as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["file"], "DESIGN.md");
    assert_eq!(list[0]["side"], "new");
    assert_eq!(list[0]["text"], "nice heading");
    assert!(list[0]["snippet"].as_str().unwrap().contains("# Design"));
}

// =============================================================================
// Regression tests for TASKS.md #9 blockers
// =============================================================================

// --- Blocker 1: No raw inline JSON hydration; no </script> breakout ----------

#[tokio::test]
async fn doc_comments_json_uses_safe_script_tag_not_global_assignment() {
    // The old approach was `window.__wdyt_doc_comments = {...}` which could
    // be broken by a </script> in comment text. The new approach uses hidden DOM
    // elements with proper HTML escaping (no script tags or Raw JSON).
    let daemon = Daemon::start().await;
    let id = docs_session(&daemon);

    daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "DESIGN.md", "line": 3, "side": "new",
                "text": "a normal comment"
            }),
        )
        .await;

    let (_, page) = daemon.get(&format!("/s/{id}")).await;
    // Must use hidden DOM elements for comment data.
    assert!(
        page.contains("id=\"wdyt-doc-comments\""),
        "no wdyt-doc-comments container: {page}"
    );
    assert!(
        page.contains("hidden=\"hidden\""),
        "comment container not hidden: {page}"
    );
    assert!(
        page.contains("class=\"wdyt-comment-data\""),
        "no comment data elements: {page}"
    );
    // Must NOT use script type=application/json for comment data.
    assert!(
        !page.contains("<script type=\"application/json\" id=\"wdyt-doc-comments\""),
        "still using JSON script tag for doc comments: {page}"
    );
    // Must NOT use the old global variable injection pattern.
    assert!(
        !page.contains("window.__wdyt_doc_comments"),
        "still using unsafe global assignment: {page}"
    );
}

#[tokio::test]
async fn script_breakout_in_comment_text_is_escaped() {
    // Adversarial regression: a comment containing </script><script> must be
    // properly HTML-escaped in the hidden DOM elements (no Raw/JSON script tags).
    let daemon = Daemon::start().await;
    let id = docs_session(&daemon);

    let adversarial_text = "</script><script>alert('xss')</script>";
    let (status, _) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "DESIGN.md", "line": 3, "side": "new",
                "text": adversarial_text
            }),
        )
        .await;
    assert_eq!(status, 200);

    let (_, page) = daemon.get(&format!("/s/{id}")).await;
    // The comment data section must use hidden DOM elements, not script tags.
    let container_start = page
        .find("id=\"wdyt-doc-comments\"")
        .expect("doc comments container present");
    let after = &page[container_start..];
    // The </script> payload must appear only in its HTML-escaped form.
    // Find the comment data element.
    let data_start = after
        .find("class=\"wdyt-comment-data\"")
        .expect("comment data");
    let data_section = &after[data_start..data_start + 500];
    // The adversarial text must be HTML-escaped (< becomes &lt;).
    assert!(
        data_section.contains("&lt;/script&gt;&lt;script&gt;alert("),
        "adversarial text not properly HTML-escaped: {data_section}"
    );
    // Must NOT contain raw </script> inside the comment data.
    assert!(
        !data_section.contains("</script>"),
        "raw </script> in comment data: {data_section}"
    );
}

#[tokio::test]
async fn brief_comments_json_uses_safe_script_tag() {
    let daemon = Daemon::start().await;
    let brief_source = "Brief paragraph.\n";
    let brief_doc = wdyt::render::markdown_commentable("brief", brief_source, "Nord");
    let files = wdyt::diff::parse(PATCH, wdyt::render::theme("Nord")).unwrap();
    let id = daemon.store.insert_new(wdyt::session::NewSession {
        brief: Some(brief_doc.html),
        brief_sources: brief_doc.sources,
        ..wdyt::session::NewSession::new("a title".to_owned(), Content::Diff { files })
    });

    daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "brief:", "line": 1, "side": "new",
                "text": "test </script><script>alert(1)</script>"
            }),
        )
        .await;

    let (_, page) = daemon.get(&format!("/s/{id}")).await;
    assert!(
        page.contains("id=\"wdyt-brief-comments\""),
        "no brief comments container: {page}"
    );
    assert!(
        !page.contains("window.__wdyt_brief_comments"),
        "still using unsafe brief global: {page}"
    );
    // Verify the adversarial text is HTML-escaped in the hidden DOM element.
    let tag_start = page
        .find("id=\"wdyt-brief-comments\"")
        .expect("brief container");
    let after = &page[tag_start..];
    // The hidden container should NOT contain unescaped </script> — the text
    // is HTML-entity-escaped by the server-side view engine.
    let data_start = after
        .find("class=\"wdyt-comment-data\"")
        .expect("comment data");
    let data_section = &after[data_start..data_start + 500];
    assert!(
        data_section.contains("&lt;/script&gt;"),
        "adversarial text not escaped in brief comments: {data_section}"
    );
    assert!(
        !data_section.contains("</script>"),
        "raw </script> in brief comment data: {data_section}"
    );
}

// --- Blocker 2: Explicit typed scope for brief vs doc identity ----------------

#[tokio::test]
async fn brief_uses_explicit_file_identifier_not_label_collision() {
    // With explicit targets, the guided-review brief ("target": "brief") and a
    // doc file literally named "brief:" ("target": "doc:0") resolve to their own
    // exact stored sources. The agent can distinguish them by the `target` field
    // on collected comments.
    let daemon = Daemon::start().await;
    let brief_source = "Brief content.\n";
    let brief_doc = wdyt::render::markdown_commentable("brief", brief_source, "Nord");
    // Create a doc file with the label "brief:" (adversarial duplicate).
    let adversarial_doc = wdyt::render::markdown_commentable("brief:", "Adversarial.\n", "Nord");
    let id = daemon.store.insert_new(wdyt::session::NewSession {
        brief: Some(brief_doc.html),
        brief_sources: brief_doc.sources,
        ..wdyt::session::NewSession::new(
            "a title".to_owned(),
            Content::Docs {
                files: vec![adversarial_doc],
            },
        )
    });

    // Comment on the guided-review brief using explicit target.
    let (status, body) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "brief:", "target": "brief", "line": 1, "side": "new",
                "text": "on guided brief"
            }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let c: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        c["snippet"], "Brief content.",
        "wrong snippet for brief target"
    );
    assert_eq!(c["target"], "brief", "target not stored");

    // Comment on the adversarial doc file named "brief:" using explicit target.
    let (status, body) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "forged.md", "target": "doc:0", "line": 1, "side": "new",
                "text": "on adversarial doc"
            }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let c2: serde_json::Value = serde_json::from_str(&body).unwrap();
    // Must resolve against the doc file's sources, NOT the brief's.
    assert_eq!(
        c2["snippet"], "Adversarial.",
        "doc:0 target resolved to wrong source (got brief instead of doc)"
    );
    assert_eq!(c2["target"], "doc:0", "target not stored for doc");
    assert_eq!(
        c2["file"], "brief:",
        "explicit target did not canonicalize the forged display label"
    );

    // Without an explicit target, `brief:` is ambiguous between this document
    // and the guided brief and must be rejected rather than guessed.
    let (legacy_status, _) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "brief:", "line": 1, "side": "new", "text": "ambiguous"
            }),
        )
        .await;
    assert_eq!(legacy_status, 400);

    // Collection includes target so the agent can distinguish them.
    let comments = daemon.store.comments(&id).unwrap();
    assert_eq!(comments.len(), 2);
    assert_eq!(comments[0].target.as_deref(), Some("brief"));
    assert_eq!(comments[0].snippet, "Brief content.");
    assert_eq!(comments[1].target.as_deref(), Some("doc:0"));
    assert_eq!(comments[1].snippet, "Adversarial.");
}

#[tokio::test]
async fn brief_and_doc_with_duplicate_labels_resolve_distinctly() {
    // Two doc files with the same label must be addressable via their explicit
    // targets (doc:0, doc:1). Legacy resolution by label alone must reject the
    // ambiguity rather than silently choosing the wrong source.
    let daemon = Daemon::start().await;
    let doc_a = wdyt::render::markdown_commentable("notes.md", "First file content.\n", "Nord");
    let doc_b = wdyt::render::markdown_commentable("notes.md", "Second file content.\n", "Nord");
    let id = daemon.store.insert_new(wdyt::session::NewSession {
        ..wdyt::session::NewSession::new(
            "a title".to_owned(),
            Content::Docs {
                files: vec![doc_a, doc_b],
            },
        )
    });

    // Explicit target doc:0 → first file.
    let (status, body) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "notes.md", "target": "doc:0", "line": 1, "side": "new",
                "text": "on first"
            }),
        )
        .await;
    assert_eq!(status, 200, "target doc:0 rejected: {body}");
    let c: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(c["snippet"], "First file content.", "doc:0 wrong snippet");
    assert_eq!(c["target"], "doc:0");

    // Explicit target doc:1 → second file.
    let (status, body) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "notes.md", "target": "doc:1", "line": 1, "side": "new",
                "text": "on second"
            }),
        )
        .await;
    assert_eq!(status, 200, "target doc:1 rejected: {body}");
    let c2: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(c2["snippet"], "Second file content.", "doc:1 wrong snippet");
    assert_eq!(c2["target"], "doc:1");

    // Legacy resolution (no target) with duplicate labels must reject as ambiguous.
    let (status, _body) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "notes.md", "line": 1, "side": "new",
                "text": "ambiguous"
            }),
        )
        .await;
    assert_eq!(
        status, 400,
        "legacy resolution did not reject ambiguous duplicate label"
    );

    // Verify collection includes target for disambiguation.
    let comments = daemon.store.comments(&id).unwrap();
    assert_eq!(comments.len(), 2);
    assert_eq!(comments[0].snippet, "First file content.");
    assert_eq!(comments[1].snippet, "Second file content.");
}

#[tokio::test]
async fn collected_comments_expose_file_identity_for_brief_and_docs() {
    // When an agent collects comments, each one must carry enough identity
    // (file field) to know whether it's on the brief or a doc file.
    let daemon = Daemon::start().await;
    let brief_source = "Brief.\n";
    let brief_doc = wdyt::render::markdown_commentable("brief", brief_source, "Nord");
    let doc = wdyt::render::markdown_commentable("DESIGN.md", "Design.\n", "Nord");
    let id = daemon.store.insert_new(wdyt::session::NewSession {
        brief: Some(brief_doc.html),
        brief_sources: brief_doc.sources,
        ..wdyt::session::NewSession::new("a title".to_owned(), Content::Docs { files: vec![doc] })
    });

    daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({"file": "brief:", "line": 1, "side": "new", "text": "on brief"}),
        )
        .await;
    daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({"file": "DESIGN.md", "line": 1, "side": "new", "text": "on doc"}),
        )
        .await;

    let (_, body) = daemon.get(&format!("/api/sessions/{id}/comments")).await;
    let comments: serde_json::Value = serde_json::from_str(&body).unwrap();
    let list = comments["comments"].as_array().unwrap();
    assert_eq!(list[0]["file"], "brief:");
    assert_eq!(list[1]["file"], "DESIGN.md");
}

// --- Blocker 3: Brief must not steal COMMENT_JS/FRAGMENT_JS roots ------------

#[tokio::test]
async fn commentable_brief_does_not_steal_code_comment_root() {
    // The brief must NOT carry `data-comments` because COMMENT_JS uses
    // querySelector('[data-comments]') which would find it first, stealing
    // the code/diff line-comment functionality.
    let daemon = Daemon::start().await;
    let brief_source = "Look at this.\n";
    let brief_doc = wdyt::render::markdown_commentable("brief", brief_source, "Nord");
    let file = wdyt::render::highlight(
        "x.rs",
        "fn main() {}\nlet x = 1;\n",
        wdyt::render::theme("Nord"),
    )
    .unwrap();
    let id = daemon.store.insert_new(wdyt::session::NewSession {
        brief: Some(brief_doc.html),
        brief_sources: brief_doc.sources,
        ..wdyt::session::NewSession::new("a title".to_owned(), Content::Code { files: vec![file] })
    });

    let (_, body) = daemon.get(&format!("/s/{id}")).await;

    // The brief section must NOT have data-comments (would steal COMMENT_JS root).
    // Find the brief section.
    let brief_start = body
        .find("class=\"brief doc\"")
        .expect("brief section exists");
    let brief_tag = &body[brief_start..brief_start + 200];
    assert!(
        !brief_tag.contains("data-comments"),
        "brief steals the comment root: {brief_tag}"
    );

    // The main content wrap MUST still have data-comments.
    assert!(
        body.contains("data-comments=\"1\""),
        "code wrap lost data-comments: {body}"
    );

    // Code line comments must still work (the root is correct).
    let (status, resp) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({"file": "x.rs", "line": 1, "side": "new", "text": "works"}),
        )
        .await;
    assert_eq!(status, 200, "code comments broken: {resp}");
}

#[tokio::test]
async fn commentable_brief_on_diff_preserves_line_comments() {
    // Same as above but for diff mode.
    let daemon = Daemon::start().await;
    let brief_source = "Review this diff.\n";
    let brief_doc = wdyt::render::markdown_commentable("brief", brief_source, "Nord");
    let files = wdyt::diff::parse(PATCH, wdyt::render::theme("Nord")).unwrap();
    let id = daemon.store.insert_new(wdyt::session::NewSession {
        brief: Some(brief_doc.html),
        brief_sources: brief_doc.sources,
        ..wdyt::session::NewSession::new("a title".to_owned(), Content::Diff { files })
    });

    let (_, body) = daemon.get(&format!("/s/{id}")).await;

    // The brief section must NOT steal data-comments.
    let brief_start = body.find("class=\"brief doc\"").expect("brief section");
    let brief_tag = &body[brief_start..brief_start + 200];
    assert!(
        !brief_tag.contains("data-comments"),
        "brief steals diff comment root: {brief_tag}"
    );

    // Diff line comments must still work.
    let (status, resp) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "src/lib.rs", "line": 2, "side": "new", "text": "diff comment"
            }),
        )
        .await;
    assert_eq!(status, 200, "diff comments broken: {resp}");

    // Brief comments must also work.
    let (status, resp) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({"file": "brief:", "line": 1, "side": "new", "text": "brief comment"}),
        )
        .await;
    assert_eq!(status, 200, "brief comments broken: {resp}");
}

// --- Blocker 4: Canonicalize markdown affordances to top-level blocks ---------

#[tokio::test]
async fn doc_comment_js_targets_direct_children_only() {
    // The DOC_COMMENT_JS must only attach affordances to direct children of the
    // doc root with data-sourcepos, NOT to nested elements (table cells, list
    // items, etc.) which would break semantics.
    assert!(
        wdyt::ui_js::DOC_COMMENT_JS_STR.contains("root.children"),
        "DOC_COMMENT_JS does not iterate direct children"
    );
    // Must NOT use querySelectorAll('[data-sourcepos]') on all descendants.
    assert!(
        !wdyt::ui_js::DOC_COMMENT_JS_STR.contains("root.querySelectorAll('[data-sourcepos]')"),
        "DOC_COMMENT_JS still queries all descendants"
    );
}

#[tokio::test]
async fn doc_comments_use_exact_sourcepos_start_end() {
    // Comments on docs store the exact sourcepos start and end lines,
    // not containment-based lookup.
    let daemon = Daemon::start().await;
    let id = docs_session(&daemon);

    // Line 5 is "- Item one" in DOC_SOURCE which starts at line 5.
    let (status, body) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "DESIGN.md", "line": 5, "end_line": 6,
                "side": "new", "text": "exact range"
            }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let c: serde_json::Value = serde_json::from_str(&body).unwrap();
    // Must record exact start/end, not a containing nested block range.
    assert_eq!(c["line"], 5);
    assert_eq!(c["end_line"], 6);

    // Hydration must key by the exact normalized source range, not only the
    // starting line or containment in a larger block.
    assert!(
        wdyt::ui_js::DOC_COMMENT_JS_STR.contains("rangeToBlock[sp.startLine + ':' + sp.endLine]"),
        "markdown hydration is not keyed by exact source range"
    );
    assert!(
        wdyt::ui_js::DOC_COMMENT_JS_STR.contains("rangeToBlock[c.line + ':' + commentEnd]"),
        "stored comments are not matched by exact source range"
    );
    assert!(
        !wdyt::ui_js::DOC_COMMENT_JS_STR.contains("lineToBlock"),
        "start-only hydration fallback remains"
    );
}

// --- Blocker 5: Strict validation, no panics, safe arithmetic ----------------

#[tokio::test]
async fn line_zero_is_rejected_for_all_content_types() {
    let daemon = Daemon::start().await;

    // Code
    let id = multiline_code_session(&daemon);
    let (status, body) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({"file": "src/main.rs", "line": 0, "side": "new", "text": "x"}),
        )
        .await;
    assert_eq!(status, 400, "accepted line 0 on code: {body}");

    // Diff
    let id = diff_session(&daemon);
    let (status, body) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({"file": "src/lib.rs", "line": 0, "side": "new", "text": "x"}),
        )
        .await;
    assert_eq!(status, 400, "accepted line 0 on diff: {body}");

    // Docs
    let id = docs_session(&daemon);
    let (status, body) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({"file": "DESIGN.md", "line": 0, "side": "new", "text": "x"}),
        )
        .await;
    assert_eq!(status, 400, "accepted line 0 on docs: {body}");
}

#[tokio::test]
async fn usize_max_line_is_rejected_not_panicked() {
    let daemon = Daemon::start().await;
    let id = multiline_code_session(&daemon);

    // usize::MAX as a JSON number — serde should handle this.
    let (status, _) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({"file": "src/main.rs", "line": 9999999999u64, "side": "new", "text": "x"}),
        )
        .await;
    // Either 400 (rejected) or 422 (bad request) but definitely not 500 (panic).
    assert_ne!(status, 500, "panicked on huge line number");
}

#[tokio::test]
async fn side_must_be_new_for_docs_and_brief() {
    let daemon = Daemon::start().await;
    let id = docs_session(&daemon);

    let (status, body) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({"file": "DESIGN.md", "line": 3, "side": "old", "text": "x"}),
        )
        .await;
    assert_eq!(status, 400, "accepted side=old on docs: {body}");
}

#[tokio::test]
async fn end_line_zero_is_rejected() {
    let daemon = Daemon::start().await;
    let id = multiline_code_session(&daemon);

    let (status, body) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "src/main.rs", "line": 1, "end_line": 0,
                "side": "new", "text": "x"
            }),
        )
        .await;
    assert_eq!(status, 400, "accepted end_line 0: {body}");
}

#[test]
fn anchor_span_does_not_overflow() {
    use wdyt::session::{Anchor, Side};
    // Edge case: line = usize::MAX, end_line = None.
    let anchor = Anchor::line("x".into(), usize::MAX, Side::New);
    // Must not panic.
    assert_eq!(anchor.span(), 1);

    // Edge case: line = 1, end_line = usize::MAX.
    let anchor = Anchor::range("x".into(), 1, usize::MAX, Side::New);
    // Must not panic. The span is clamped by saturating arithmetic.
    let span = anchor.span();
    assert!(span > 0, "span was zero");
}

// --- Blocker 6: Doc/brief sel links resolve and scroll -----------------------

#[tokio::test]
async fn doc_comment_js_supports_sel_fragment_resolution() {
    // The DOC_COMMENT_JS must handle hashchange with sel- fragments that
    // target doc files, scrolling to and highlighting the correct block.
    assert!(
        wdyt::ui_js::DOC_COMMENT_JS_STR.contains("handleDocFragment"),
        "no doc fragment handler in DOC_COMMENT_JS"
    );
    assert!(
        wdyt::ui_js::DOC_COMMENT_JS_STR.contains("hashchange"),
        "no hashchange listener in DOC_COMMENT_JS"
    );
    assert!(
        wdyt::ui_js::DOC_COMMENT_JS_STR.contains("scrollIntoView"),
        "no scrolling in DOC_COMMENT_JS"
    );
    // Uses explicit target (data-doc-target), not display label, for collision-safe fragments.
    assert!(
        wdyt::ui_js::DOC_COMMENT_JS_STR.contains("selFragment(docTarget"),
        "not using explicit target identity in DOC_COMMENT_JS fragments"
    );
    // Respects reduced motion preference for scroll behavior.
    assert!(
        wdyt::ui_js::DOC_COMMENT_JS_STR.contains("prefers-reduced-motion"),
        "does not respect reduced motion in DOC_COMMENT_JS"
    );
}

// --- Blocker 7: Scripts included for commentable brief on dir/demo -----------

#[tokio::test]
async fn commentable_brief_on_demo_includes_scripts() {
    let daemon = Daemon::start().await;
    let brief_source = "Check the demo.\n";
    let brief_doc = wdyt::render::markdown_commentable("brief", brief_source, "Nord");
    let id = daemon.store.insert_new(wdyt::session::NewSession {
        brief: Some(brief_doc.html),
        brief_sources: brief_doc.sources,
        ..wdyt::session::NewSession::new(
            "a title".to_owned(),
            Content::Demo {
                port: 4999,
                path: "/".to_owned(),
            },
        )
    });

    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    // Even though Demo is normally non-commentable, a commentable brief should
    // cause the scripts to be included.
    assert!(
        body.contains("doc-addnote"),
        "DOC_COMMENT_JS missing on demo with brief: {body}"
    );
    assert!(
        body.contains("handleFragment"),
        "FRAGMENT_JS missing on demo with brief: {body}"
    );
    // The framed layout must still be preserved.
    assert!(
        body.contains("class=\"framed\""),
        "lost framed layout: {body}"
    );
    assert!(body.contains("framewrap"), "lost framewrap: {body}");
}

#[tokio::test]
async fn commentable_brief_on_static_includes_scripts() {
    let daemon = Daemon::start().await;
    let brief_source = "Check the report.\n";
    let brief_doc = wdyt::render::markdown_commentable("brief", brief_source, "Nord");
    let dir = tempfile::tempdir().expect("temp dir");
    let id = daemon.store.insert_new(wdyt::session::NewSession {
        brief: Some(brief_doc.html),
        brief_sources: brief_doc.sources,
        ..wdyt::session::NewSession::new(
            "a title".to_owned(),
            Content::Static {
                root: dir.path().to_path_buf(),
                entry: "index.html".to_owned(),
            },
        )
    });

    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    assert!(
        body.contains("doc-addnote"),
        "DOC_COMMENT_JS missing on static with brief: {body}"
    );
    assert!(
        body.contains("class=\"framed\""),
        "lost framed layout: {body}"
    );
}

// --- Blocker 8: Serde defaults and legacy render fallback --------------------

#[tokio::test]
async fn old_cli_payload_without_highlighted_and_sources_still_works() {
    // An old CLI that doesn't send `highlighted` and `sources` fields should
    // still be accepted by the daemon (serde defaults to empty vecs).
    let daemon = Daemon::start().await;

    // Simulate an old-format payload without highlighted/sources/lines/language.
    let (status, body) = daemon
        .post_json(
            "/api/sessions",
            serde_json::json!({
                "title": "old cli test",
                "content": {
                    "Code": {
                        "files": [{
                            "label": "old.rs",
                            "html": "<table class=\"src\"><tr><td>old</td></tr></table>"
                        }]
                    }
                }
            }),
        )
        .await;
    assert_eq!(status, 200, "old CLI payload rejected: {body}");
    let created: serde_json::Value = serde_json::from_str(&body).unwrap();
    let id = created["id"].as_str().unwrap();

    // The session page must render without panicking. It falls back to the
    // `html` field when `highlighted` is empty.
    let (status, page) = daemon.get(&format!("/s/{id}")).await;
    assert_eq!(status, 200, "page crashed for legacy payload");
    assert!(page.contains("old"), "no content on legacy page: {page}");
}

#[test]
fn codefile_deserializes_without_new_fields() {
    // Backward compatibility: a serialized CodeFile from an old CLI (no
    // highlighted/sources/lines/language) must deserialize with defaults.
    let json = r#"{"label":"test.rs","html":"<pre>code</pre>"}"#;
    let file: wdyt::session::CodeFile = serde_json::from_str(json).unwrap();
    assert_eq!(file.label, "test.rs");
    assert_eq!(file.html, "<pre>code</pre>");
    assert!(file.highlighted.is_empty());
    assert!(file.sources.is_empty());
    assert_eq!(file.lines, 0);
    assert_eq!(file.language, "");
}

// --- Blocker 9: Existing #f-* and heading links preserved --------------------

#[tokio::test]
async fn file_anchors_are_not_broken_by_new_scheme() {
    // The #f-<path> anchors must still work for nav links and guided-review
    // brief links. The new sel- scheme must not interfere with them.
    let daemon = Daemon::start().await;
    let files = wdyt::diff::parse(PATCH, wdyt::render::theme("Nord")).unwrap();
    let brief = wdyt::render::markdown(
        "brief",
        "See [the lib](#f-src-lib-rs) for details.\n",
        "Nord",
    );
    let id = daemon.store.insert_new(wdyt::session::NewSession {
        brief: Some(brief.html),
        ..wdyt::session::NewSession::new("a title".to_owned(), Content::Diff { files })
    });

    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    // File anchors still present.
    assert!(
        body.contains("id=\"f-src-lib-rs\""),
        "file anchor broken: {body}"
    );
    // Nav links still point to file anchors.
    assert!(
        body.contains("href=\"#f-src-lib-rs\""),
        "nav link broken: {body}"
    );
    // Brief link to file anchor still works.
    assert!(
        body.contains("href=\"#f-src-lib-rs\""),
        "brief link to file anchor broken: {body}"
    );
}

#[tokio::test]
async fn heading_anchors_preserved_in_docs_mode() {
    let daemon = Daemon::start().await;
    let doc = wdyt::render::markdown_commentable(
        "test.md",
        "# My Heading\n\nContent.\n\n## Sub Heading\n\nMore.\n",
        "Nord",
    );
    let id = daemon.insert(Content::Docs { files: vec![doc] });

    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    // Heading anchors from comrak (id="my-heading") must still exist.
    assert!(
        body.contains("id=\"my-heading\""),
        "heading anchor missing: {body}"
    );
    assert!(
        body.contains("id=\"sub-heading\""),
        "sub-heading anchor missing: {body}"
    );
}

// =============================================================================
// Task #11: Keyboard hints in comment editors
// =============================================================================

#[tokio::test]
async fn comment_editor_shows_keyboard_hint_on_code_page() {
    // The code/diff comment editor must show visible keyboard shortcuts.
    let daemon = Daemon::start().await;
    let id = code_session(&daemon);
    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    // The JS that creates the comment box includes a .kb-hint element.
    assert!(
        body.contains("kb-hint"),
        "no keyboard hint in code/diff comment editor: {body}"
    );
    // The script is present and includes the keyboard shortcut handling.
    assert!(body.contains("Escape"), "no Escape handling: {body}");
}

#[tokio::test]
async fn comment_editor_shows_keyboard_hint_on_diff_page() {
    let daemon = Daemon::start().await;
    let id = diff_session(&daemon);
    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    assert!(
        body.contains("kb-hint"),
        "no keyboard hint on diff page: {body}"
    );
}

#[tokio::test]
async fn doc_comment_editor_shows_keyboard_hint() {
    // The doc/brief comment editor must also have visible keyboard shortcuts.
    let daemon = Daemon::start().await;
    let id = docs_session(&daemon);
    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    assert!(
        body.contains("kb-hint"),
        "no keyboard hint in doc comment editor: {body}"
    );
}

#[test]
fn comment_js_has_keyboard_hint_markup() {
    // The code/diff COMMENT_JS must render a .kb-hint with accessible markup.
    assert!(
        wdyt::ui_js::COMMENT_JS_STR.contains("kb-hint"),
        "COMMENT_JS does not include keyboard hint"
    );
    // Uses <kbd> for semantic keyboard shortcut markup.
    assert!(
        wdyt::ui_js::COMMENT_JS_STR.contains("<kbd"),
        "COMMENT_JS does not use <kbd> element"
    );
    // Uses <abbr> for the shortcut names.
    assert!(
        wdyt::ui_js::COMMENT_JS_STR.contains("<abbr"),
        "COMMENT_JS does not use <abbr> element"
    );
    // Has aria-label for accessibility.
    assert!(
        wdyt::ui_js::COMMENT_JS_STR.contains("aria-label"),
        "COMMENT_JS keyboard hint missing aria-label"
    );
    // Retains keyboard behavior: Escape closes, Cmd/Ctrl+Enter sends.
    assert!(wdyt::ui_js::COMMENT_JS_STR.contains("Escape"));
    assert!(wdyt::ui_js::COMMENT_JS_STR.contains("metaKey || ev.ctrlKey"));
}

#[test]
fn doc_comment_js_has_keyboard_hint_markup() {
    // The DOC_COMMENT_JS must also include .kb-hint with accessible elements.
    assert!(
        wdyt::ui_js::DOC_COMMENT_JS_STR.contains("kb-hint"),
        "DOC_COMMENT_JS does not include keyboard hint"
    );
    assert!(
        wdyt::ui_js::DOC_COMMENT_JS_STR.contains("<kbd"),
        "DOC_COMMENT_JS does not use <kbd> element"
    );
    assert!(
        wdyt::ui_js::DOC_COMMENT_JS_STR.contains("<abbr"),
        "DOC_COMMENT_JS does not use <abbr> element"
    );
    assert!(
        wdyt::ui_js::DOC_COMMENT_JS_STR.contains("aria-label"),
        "DOC_COMMENT_JS keyboard hint missing aria-label"
    );
}

#[test]
fn keyboard_hint_css_exists_in_stylesheet() {
    let css = wdyt::ui::stylesheet_for_test();
    assert!(css.contains(".kb-hint"), "no .kb-hint CSS rule: {css}");
}

// =============================================================================
// Task #12: Navigation — jump-to-top, sidebar, responsive, single-file
// =============================================================================

#[tokio::test]
async fn jump_to_top_button_rendered_on_all_session_pages() {
    let daemon = Daemon::start().await;
    // Code page
    let id = code_session(&daemon);
    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    assert!(
        body.contains("id=\"jump-top\""),
        "no jump-to-top on code page: {body}"
    );
    assert!(
        body.contains("aria-label=\"Jump to top\""),
        "jump-to-top missing aria-label"
    );
    // Diff page
    let id = diff_session(&daemon);
    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    assert!(
        body.contains("id=\"jump-top\""),
        "no jump-to-top on diff page"
    );
    // Docs page
    let id = docs_session(&daemon);
    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    assert!(
        body.contains("id=\"jump-top\""),
        "no jump-to-top on docs page"
    );
}

#[tokio::test]
async fn jump_to_top_not_rendered_on_framed_pages() {
    // Framed pages do not scroll, so jump-to-top would be useless. The button is
    // rendered but hidden via CSS (`body.framed #jump-top { display: none }`).
    let daemon = Daemon::start().await;
    let id = daemon.store.insert_new(wdyt::session::NewSession::new(
        "demo".to_owned(),
        Content::Demo {
            port: 4999,
            path: "/".to_owned(),
        },
    ));
    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    // The button is in the DOM (for consistency) but CSS hides it.
    assert!(
        body.contains("id=\"jump-top\""),
        "jump-to-top button missing from DOM"
    );
    assert!(body.contains("framed"), "framed class missing");
}

#[tokio::test]
async fn single_file_code_page_has_no_nav() {
    let daemon = Daemon::start().await;
    let id = code_session(&daemon);
    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    // A single-file page must not gain noisy empty navigation.
    assert!(
        !body.contains("class=\"files"),
        "single-file code page has nav: {body}"
    );
}

#[tokio::test]
async fn single_file_diff_page_has_no_nav() {
    let daemon = Daemon::start().await;
    let id = diff_session(&daemon);
    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    assert!(
        !body.contains("class=\"files"),
        "single-file diff page has nav: {body}"
    );
}

#[tokio::test]
async fn multi_file_diff_page_has_nav_with_sidebar_class() {
    // A diff with more than 3 files gets the sidebar class for desktop layout.
    let daemon = Daemon::start().await;
    let patch = "\
--- a/src/a.rs
+++ b/src/a.rs
@@ -1 +1 @@
-a
+aa
--- a/src/b.rs
+++ b/src/b.rs
@@ -1 +1 @@
-b
+bb
--- a/src/c.rs
+++ b/src/c.rs
@@ -1 +1 @@
-c
+cc
--- a/src/d.rs
+++ b/src/d.rs
@@ -1 +1 @@
-d
+dd
";
    let files = wdyt::diff::parse(patch, theme("Nord")).unwrap();
    let id = daemon.insert(Content::Diff { files });
    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    assert!(
        body.contains("has-sidebar"),
        "multi-file diff missing has-sidebar class: {body}"
    );
    // All file anchors are still present.
    assert!(
        body.contains("id=\"f-src-a-rs\""),
        "anchor for a.rs missing"
    );
    assert!(
        body.contains("id=\"f-src-d-rs\""),
        "anchor for d.rs missing"
    );
}

#[tokio::test]
async fn multi_file_code_page_has_nav_with_sidebar_class() {
    let daemon = Daemon::start().await;
    let file_a = wdyt::render::highlight("a.rs", "fn a() {}\n", theme("Nord")).unwrap();
    let file_b = wdyt::render::highlight("b.rs", "fn b() {}\n", theme("Nord")).unwrap();
    let file_c = wdyt::render::highlight("c.rs", "fn c() {}\n", theme("Nord")).unwrap();
    let file_d = wdyt::render::highlight("d.rs", "fn d() {}\n", theme("Nord")).unwrap();
    let id = daemon.insert(Content::Code {
        files: vec![file_a, file_b, file_c, file_d],
    });
    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    assert!(
        body.contains("has-sidebar"),
        "multi-file code missing has-sidebar class: {body}"
    );
}

#[tokio::test]
async fn two_file_session_has_nav_without_sidebar() {
    // 2-3 files get a nav but no sidebar (the horizontal bar is fine for that).
    let daemon = Daemon::start().await;
    let file_a = wdyt::render::highlight("a.rs", "fn a() {}\n", theme("Nord")).unwrap();
    let file_b = wdyt::render::highlight("b.rs", "fn b() {}\n", theme("Nord")).unwrap();
    let id = daemon.insert(Content::Code {
        files: vec![file_a, file_b],
    });
    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    assert!(
        body.contains("class=\"files\""),
        "two-file session missing nav: {body}"
    );
    // The nav element itself should NOT have the has-sidebar class (only 2 files).
    assert!(
        !body.contains("class=\"files has-sidebar\""),
        "two-file session should not have sidebar class on nav"
    );
}

#[test]
fn nav_js_handles_jump_to_top_and_sidebar() {
    assert!(
        wdyt::ui_js::NAV_JS_STR.contains("jump-top"),
        "NAV_JS missing jump-to-top handling"
    );
    assert!(
        wdyt::ui_js::NAV_JS_STR.contains("has-sidebar"),
        "NAV_JS missing sidebar tracking"
    );
    assert!(
        wdyt::ui_js::NAV_JS_STR.contains("IntersectionObserver"),
        "NAV_JS not using IntersectionObserver for tracking"
    );
    assert!(
        wdyt::ui_js::NAV_JS_STR.contains("has-file-sidebar"),
        "NAV_JS missing body class for sidebar layout"
    );
}

#[test]
fn nav_css_has_sidebar_responsive_and_jump_to_top_rules() {
    let css = wdyt::ui::stylesheet_for_test();
    // Desktop sidebar styles exist.
    assert!(
        css.contains("nav.files.has-sidebar"),
        "no sidebar CSS: {css}"
    );
    assert!(
        css.contains("min-width: 1380px"),
        "no desktop breakpoint for sidebar"
    );
    // Jump-to-top button styles.
    assert!(css.contains("#jump-top"), "no jump-to-top CSS");
    // Framed pages hide the button.
    assert!(
        css.contains("body.framed #jump-top"),
        "no framed hide rule for jump-to-top"
    );
    // Mobile responsive rule.
    assert!(
        css.contains("max-width: 600px"),
        "no mobile responsive rule for jump-to-top"
    );
    // Sidebar body class exists for layout shift.
    assert!(
        css.contains("has-file-sidebar"),
        "no has-file-sidebar layout CSS"
    );
}

#[test]
fn nav_css_does_not_obscure_anchors_with_sidebar() {
    // When the sidebar is active, scroll-margin-top should be adjusted so
    // anchor targets are not hidden.
    let css = wdyt::ui::stylesheet_for_test();
    assert!(
        css.contains("has-file-sidebar section.file"),
        "no anchor offset adjustment for sidebar mode"
    );
}

#[tokio::test]
async fn hide_chrome_still_works_with_new_nav() {
    // The hide-chrome / nochrome behavior must be preserved.
    let daemon = Daemon::start().await;
    let id = multi_file_diff_session(&daemon);
    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    // Chrome toggle is present.
    assert!(body.contains("chrome-toggle"), "chrome toggle missing");
    assert!(body.contains("chrome-show"), "chrome show button missing");
    // The nochrome rules are in the CSS.
    assert!(
        wdyt::ui::stylesheet_asset_for_test().contains("body.nochrome"),
        "nochrome CSS rules missing"
    );
    // Dot-key binding still works.
    assert!(body.contains("key !== '.'"), "dot-key toggle missing");
}

#[tokio::test]
async fn guided_review_links_still_work_with_sidebar() {
    // A guided-review brief with #f-<path> links must still jump to files.
    let daemon = Daemon::start().await;
    let patch = "\
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,3 @@
 fn main() {
-    let x = 1;
+    let x = 2;
 }
--- a/src/util.rs
+++ b/src/util.rs
@@ -1,2 +1,3 @@
 pub fn helper() {
+    println!(\"hi\");
 }
--- a/src/other.rs
+++ b/src/other.rs
@@ -1 +1 @@
-old
+new
--- a/src/more.rs
+++ b/src/more.rs
@@ -1 +1 @@
-x
+y
";
    let files = wdyt::diff::parse(patch, theme("Nord")).unwrap();
    let brief = wdyt::render::markdown(
        "brief",
        "See [the lib](#f-src-lib-rs) for the main change.\n",
        "Nord",
    );
    let id = daemon.store.insert_new(wdyt::session::NewSession {
        brief: Some(brief.html),
        ..wdyt::session::NewSession::new("a title".to_owned(), Content::Diff { files })
    });
    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    // Brief with link is rendered.
    assert!(body.contains("#f-src-lib-rs"), "brief link missing: {body}");
    // File sections have their anchors.
    assert!(body.contains("id=\"f-src-lib-rs\""), "file anchor missing");
    // Sidebar class is present (4 files).
    assert!(
        body.contains("has-sidebar"),
        "sidebar class missing for 4 files"
    );
}

#[tokio::test]
async fn reply_widget_still_works_with_navigation_changes() {
    // The reply overlay must not be affected by the new nav features.
    let daemon = Daemon::start().await;
    let id = multi_file_diff_session(&daemon);
    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    assert!(body.contains("id=\"reply\""), "reply widget missing");
    assert!(body.contains("Reply to agent"), "reply toggle text missing");
}

// =============================================================================
// Review finding tests: CSS contracts and responsive rules
// =============================================================================

#[test]
fn finding_1_narrow_nav_scrollable_strip_css_contract() {
    // At <=1379px the nav must be a single-row horizontally scrollable strip.
    let css = wdyt::ui::stylesheet_for_test();
    // The narrow breakpoint media query exists.
    assert!(css.contains("max-width: 1379px"), "no narrow breakpoint");
    // The nav must not wrap — it scrolls horizontally.
    assert!(
        css.contains("flex-wrap: nowrap"),
        "narrow nav not set to nowrap"
    );
    assert!(
        css.contains("overflow-x: auto"),
        "narrow nav missing overflow-x: auto"
    );
    // Touch scrolling must be preserved.
    assert!(
        css.contains("-webkit-overflow-scrolling: touch"),
        "missing touch scrolling support"
    );
}

#[test]
fn finding_2_desktop_sidebar_brief_alignment() {
    // Brief and .wrap must share the same left axis when sidebar is active.
    let css = wdyt::ui::stylesheet_for_test();
    // Brief must have margin-left: 200px (same as main) to shift past sidebar.
    assert!(
        css.contains("has-file-sidebar section.brief"),
        "no brief sidebar rule"
    );
    assert!(
        css.contains("margin-left: 200px"),
        "brief not shifted by sidebar width"
    );
    // The padding formula must account for the sidebar width in centering.
    assert!(
        css.contains("100% - 200px - 1100px"),
        "brief centering formula doesn't subtract sidebar width"
    );
}

#[test]
fn finding_3_active_link_outside_desktop_media() {
    // Active link styling must be global so mobile/horizontal tracking works.
    let css = wdyt::ui::stylesheet_for_test();
    assert!(
        css.contains("nav.files a.active {"),
        "no global nav.files a.active rule"
    );
    // It must include the expected visual styles.
    assert!(
        css.contains("nav.files a.active {\n  background: var(--surface)"),
        "active rule missing background style"
    );
}

#[test]
fn finding_4_keyboard_hint_platform_conditional_visible_text() {
    // The visible hint text must be platform-conditional (Ctrl+Enter vs ⌘+Enter).
    // In COMMENT_JS:
    let js = wdyt::ui_js::COMMENT_JS_STR;
    assert!(
        js.contains("'Ctrl+Enter'"),
        "COMMENT_JS missing Ctrl+Enter visible text"
    );
    assert!(
        js.contains("'\\u2318+Enter'"),
        "COMMENT_JS missing ⌘+Enter visible text"
    );
    // In DOC_COMMENT_JS:
    let djs = wdyt::ui_js::DOC_COMMENT_JS_STR;
    assert!(
        djs.contains("'Ctrl+Enter'"),
        "DOC_COMMENT_JS missing Ctrl+Enter visible text"
    );
    assert!(
        djs.contains("'\\u2318+Enter'"),
        "DOC_COMMENT_JS missing ⌘+Enter visible text"
    );
}

#[test]
fn finding_5_jump_to_top_reduced_motion() {
    // The jump-to-top scroll must respect prefers-reduced-motion.
    let js = wdyt::ui_js::NAV_JS_STR;
    assert!(
        js.contains("prefers-reduced-motion: reduce"),
        "NAV_JS missing reduced motion media query"
    );
    // Must use conditional behavior: 'auto' when reduced, 'smooth' otherwise.
    assert!(
        js.contains("prefersReduced ? 'auto' : 'smooth'"),
        "NAV_JS missing conditional scroll behavior"
    );
}

#[test]
fn finding_6_permalink_visible_on_keyboard_focus() {
    // Comment permalinks must be visible on focus/focus-visible, not hover only.
    let css = wdyt::ui::stylesheet_for_test();
    // Code/diff permalinks.
    assert!(
        css.contains(".note .permalink:focus"),
        "code/diff permalink not visible on :focus"
    );
    assert!(
        css.contains(".note .permalink:focus-visible"),
        "code/diff permalink not visible on :focus-visible"
    );
    // Doc/brief permalinks.
    assert!(
        css.contains(".doc .doc-notes .note .permalink:focus"),
        "doc permalink not visible on :focus"
    );
    assert!(
        css.contains(".doc .doc-notes .note .permalink:focus-visible"),
        "doc permalink not visible on :focus-visible"
    );
}

// ---------- Finding 1: Security tests ----------

#[tokio::test]
async fn markdown_script_tag_cannot_execute_or_render_raw() {
    let daemon = Daemon::start().await;
    let source = "\
# Hello

<script>alert('xss')</script>

Some text.

<iframe src=\"evil\"></iframe>
";
    let doc = wdyt::render::markdown_commentable("evil.md", source, "Nord");
    let id = daemon.insert(Content::Docs { files: vec![doc] });

    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    // Raw script tag must not appear in the rendered output.
    assert!(
        !body.contains("<script>alert"),
        "raw <script> rendered in markdown output: {body}"
    );
    // Raw iframe must not appear either.
    assert!(
        !body.contains("<iframe src=\"evil\""),
        "raw <iframe> rendered in markdown output: {body}"
    );
    // GFM features still work despite disabling raw HTML.
    assert!(body.contains("Hello"), "heading missing: {body}");
    assert!(body.contains("Some text"), "text missing: {body}");
}

#[tokio::test]
async fn static_iframe_is_sandboxed_without_allow_same_origin() {
    let daemon = Daemon::start().await;
    let dir = tempfile::tempdir().expect("a temp directory");
    let id = daemon.insert(Content::Static {
        root: dir.path().to_path_buf(),
        entry: "index.html".to_owned(),
    });

    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    // The iframe must have a sandbox attribute.
    assert!(
        body.contains("sandbox="),
        "static iframe missing sandbox attribute: {body}"
    );
    // Must NOT include allow-same-origin (would defeat the sandbox for same-origin content).
    assert!(
        !body.contains("allow-same-origin"),
        "static iframe has allow-same-origin, defeating sandbox: {body}"
    );
    // Must allow scripts and forms for report functionality.
    assert!(
        body.contains("allow-scripts"),
        "static iframe missing allow-scripts: {body}"
    );
    assert!(
        body.contains("allow-forms"),
        "static iframe missing allow-forms: {body}"
    );
}

#[tokio::test]
async fn demo_iframe_is_not_sandboxed() {
    // Demo is cross-port and may remain unsandboxed.
    let daemon = Daemon::start().await;
    let id = daemon.insert(Content::Demo {
        port: 9999,
        path: "/".to_owned(),
    });

    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    // Demo iframe must NOT have a sandbox attribute (cross-origin already isolates it).
    let iframe_start = body.find("<iframe").expect("no iframe in demo page");
    let iframe_end = body[iframe_start..].find('>').unwrap() + iframe_start;
    let iframe_tag = &body[iframe_start..=iframe_end];
    assert!(
        !iframe_tag.contains("sandbox"),
        "demo iframe should not be sandboxed: {iframe_tag}"
    );
}

// ---------- Finding 2: Typed comment rendering ----------

#[tokio::test]
async fn doc_file_named_brief_renders_comment_only_under_doc_not_brief() {
    // A docs file literally named "brief:" must have its comment rendered exactly
    // once under doc:0, never in the guided-review brief section.
    let daemon = Daemon::start().await;
    let brief_source = "Guide content.\n";
    let brief_doc = wdyt::render::markdown_commentable("brief_guide", brief_source, "Nord");
    // Create a doc file with the adversarial label "brief:"
    let adversarial_doc = wdyt::render::markdown_commentable("brief:", "Doc content.\n", "Nord");
    let id = daemon.store.insert_new(wdyt::session::NewSession {
        brief: Some(brief_doc.html),
        brief_sources: brief_doc.sources,
        ..wdyt::session::NewSession::new(
            "typed comment test".to_owned(),
            Content::Docs {
                files: vec![adversarial_doc],
            },
        )
    });

    // Comment on the doc file named "brief:" using explicit target doc:0.
    let (status, body) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "brief:", "target": "doc:0", "line": 1, "side": "new",
                "text": "comment on doc file named brief:"
            }),
        )
        .await;
    assert_eq!(status, 200, "comment creation failed: {body}");
    let c: serde_json::Value = serde_json::from_str(&body).unwrap();
    let comment_id = c["id"].as_u64().unwrap();

    // Render the page and check that the comment appears exactly once.
    let (_, page) = daemon.get(&format!("/s/{id}")).await;

    // The comment ID should appear in the doc comments section (wdyt-doc-comments),
    // NOT in the brief comments section (wdyt-brief-comments).
    let brief_section = page.find("id=\"wdyt-brief-comments\"").map(|start| {
        let end = page[start..].find("</div>").unwrap() + start;
        &page[start..end]
    });
    let doc_section = page.find("id=\"wdyt-doc-comments\"").map(|start| {
        let section_start = start;
        // Find the closing </div> for this hidden container.
        let end = page[section_start..].find("</div>").unwrap() + section_start;
        &page[section_start..end]
    });

    // The brief section should NOT contain this comment's ID.
    if let Some(brief_html) = brief_section {
        assert!(
            !brief_html.contains(&format!("data-comment-id=\"{comment_id}\"")),
            "comment on doc file 'brief:' incorrectly appears in brief section"
        );
    }

    // The doc section SHOULD contain this comment's ID.
    assert!(
        doc_section.is_some(),
        "no doc comments section found on page"
    );
    let doc_html = doc_section.unwrap();
    assert!(
        doc_html.contains(&format!("data-comment-id=\"{comment_id}\"")),
        "comment on doc file 'brief:' missing from doc section: {doc_html}"
    );

    // Verify the comment has exactly one ID (stored once, not duplicated).
    let comments = daemon.store.comments(&id).unwrap();
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0].target.as_deref(), Some("doc:0"));
    assert_eq!(comments[0].file, "brief:");
}

#[tokio::test]
async fn code_file_named_brief_keeps_brief_and_code_comments_separate() {
    let daemon = Daemon::start().await;
    let brief = wdyt::render::markdown_commentable("guide", "Guide content.\n", "Nord");
    let file = wdyt::render::highlight("brief:", "let code = true;\n", theme("Nord")).unwrap();
    let id = daemon.store.insert_new(wdyt::session::NewSession {
        brief: Some(brief.html),
        brief_sources: brief.sources,
        ..wdyt::session::NewSession::new(
            "typed code comment test".to_owned(),
            Content::Code { files: vec![file] },
        )
    });

    let (_, brief_body) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "brief:", "target": "brief", "line": 1,
                "side": "new", "text": "on the guide"
            }),
        )
        .await;
    let brief_comment: serde_json::Value = serde_json::from_str(&brief_body).unwrap();

    let (status, code_body) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "brief:", "line": 1, "side": "new", "text": "on the code"
            }),
        )
        .await;
    assert_eq!(status, 200, "{code_body}");
    let code_comment: serde_json::Value = serde_json::from_str(&code_body).unwrap();
    assert_eq!(code_comment["snippet"], "let code = true;");

    let (_, page) = daemon.get(&format!("/s/{id}")).await;
    let brief_anchor = format!("id=\"comment-{}\"", brief_comment["id"]);
    let code_anchor = format!("id=\"comment-{}\"", code_comment["id"]);
    assert_eq!(
        page.matches(&brief_anchor).count(),
        0,
        "brief comment leaked into code"
    );
    assert_eq!(
        page.matches(&code_anchor).count(),
        1,
        "code comment missing or duplicated"
    );
}

// ---------- Finding 3: Fragment hashchange does not clear doc highlights ----------

#[test]
fn fragment_js_does_not_clear_on_file_anchor_fallthrough() {
    // FRAGMENT_JS must NOT call clearHighlights when it falls through to native
    // anchor handling (file anchors, headings). It should only clear highlights
    // when it positively matches a sel- or comment- fragment.
    let ui_source = include_str!("../src/ui.rs");
    let fragment_section = ui_source
        .find("const FRAGMENT_JS")
        .expect("FRAGMENT_JS const not found");
    let fragment_end = ui_source[fragment_section..].find("\"##;").unwrap() + fragment_section;
    let fragment_js = &ui_source[fragment_section..fragment_end];

    // The handleFragment function's fallthrough case (for file anchors and
    // headings) must NOT call clearHighlights.
    let handle_fn_start = fragment_js.find("function handleFragment()").unwrap();
    let handle_fn = &fragment_js[handle_fn_start..];
    // Find the "fall through" comment - it should NOT be followed by clearHighlights.
    assert!(
        handle_fn.contains("Do NOT clear"),
        "handleFragment fallthrough should explicitly state it does not clear"
    );
    // The fallthrough path should not have clearHighlights() between it and the closing brace.
    let fallthrough_pos = handle_fn.find("Do NOT clear").unwrap();
    let after_fallthrough = &handle_fn[fallthrough_pos..fallthrough_pos + 200];
    assert!(
        !after_fallthrough.contains("clearHighlights()"),
        "handleFragment calls clearHighlights on fallthrough: {after_fallthrough}"
    );
}

#[test]
fn fragment_js_scopes_clear_to_code_diff_area() {
    // The clearHighlights function in FRAGMENT_JS must scope to [data-comments],
    // not globally clear all .sel-highlight elements.
    let ui_source = include_str!("../src/ui.rs");
    // Find the FRAGMENT_JS clearHighlights function
    let fragment_section = ui_source
        .find("const FRAGMENT_JS")
        .expect("FRAGMENT_JS const not found");
    let fragment_end = ui_source[fragment_section..].find("\"##;").unwrap() + fragment_section;
    let fragment_js = &ui_source[fragment_section..fragment_end];

    // clearHighlights must scope to [data-comments] root, not select globally.
    assert!(
        fragment_js.contains("querySelector('[data-comments]')"),
        "clearHighlights not scoped to [data-comments]: should not clear doc highlights"
    );
    // The fallthrough case must NOT call clearHighlights.
    assert!(
        fragment_js.contains("Do NOT clear"),
        "fallthrough should have comment explaining it does not clear highlights"
    );
    let selection = fragment_js
        .split("function highlightSelection(sel)")
        .nth(1)
        .and_then(|tail| tail.split("function handleFragment()").next())
        .expect("highlightSelection function");
    assert!(
        !selection
            .trim_start()
            .starts_with("{\n    clearHighlights()"),
        "selection handler clears before confirming ownership"
    );
    assert!(
        selection.contains("if (!found) clearHighlights();"),
        "selection handler does not defer clearing until a code/diff match"
    );
}

// ---------- Finding 4: Daemon protocol version ----------

#[tokio::test]
async fn stale_marker_only_daemon_is_skipped() {
    // A daemon that has the marker but no protocol version field (old daemon)
    // must not be recognized by the new client.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
                let mut buf = vec![0u8; 1024];
                let _ = socket.read(&mut buf).await;
                // Old-style response: has marker but no protocol field.
                let body = r#"{"ok":true,"service":"wdyt-daemon","version":"0.0.1","sessions":0}"#;
                let _ = socket
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\
                             Content-Type: application/json\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await;
            });
        }
    });

    let config = Config {
        port_low: port,
        port_high: port,
        bind: IpAddr::from([127, 0, 0, 1]),
        webhook_url: None,
        ..Config::default()
    };
    let client = wdyt::client::Client::new(&config);
    assert!(
        !client.is_up().await,
        "old marker-only daemon should not be recognized by new client"
    );
}

#[tokio::test]
async fn compatible_protocol_daemon_is_recognized() {
    // A daemon with both the marker AND the correct protocol version is accepted.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
                let mut buf = vec![0u8; 1024];
                let _ = socket.read(&mut buf).await;
                let body = format!(
                    r#"{{"ok":true,"service":"wdyt-daemon","version":"0.1.0","protocol":{},"sessions":0}}"#,
                    wdyt::server::PROTOCOL_VERSION
                );
                let _ = socket
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\
                             Content-Type: application/json\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await;
            });
        }
    });

    let config = Config {
        port_low: port,
        port_high: port,
        bind: IpAddr::from([127, 0, 0, 1]),
        webhook_url: None,
        ..Config::default()
    };
    let client = wdyt::client::Client::new(&config);
    assert!(
        client.is_up().await,
        "compatible-protocol daemon should be recognized"
    );
}

#[tokio::test]
async fn wrong_protocol_version_daemon_is_skipped() {
    // A daemon with the marker but wrong protocol version must be skipped.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
                let mut buf = vec![0u8; 1024];
                let _ = socket.read(&mut buf).await;
                // Wrong protocol version (999).
                let body = r#"{"ok":true,"service":"wdyt-daemon","version":"0.1.0","protocol":999,"sessions":0}"#;
                let _ = socket
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\
                             Content-Type: application/json\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await;
            });
        }
    });

    let config = Config {
        port_low: port,
        port_high: port,
        bind: IpAddr::from([127, 0, 0, 1]),
        webhook_url: None,
        ..Config::default()
    };
    let client = wdyt::client::Client::new(&config);
    assert!(
        !client.is_up().await,
        "daemon with wrong protocol version should be skipped"
    );
}

// ---------- Finding 5: Diff range snippets require complete contiguous range ----------

#[tokio::test]
async fn diff_range_spanning_gap_between_hunks_is_rejected() {
    // A range that requests lines across a gap between hunks must be rejected
    // rather than returning a truncated snippet with a false end_line.
    let daemon = Daemon::start().await;
    // Craft a diff with two hunks far apart (lines 2-3 in first hunk, lines 50-51 in second).
    let gapped_patch = "\
--- a/big.rs
+++ b/big.rs
@@ -1,4 +1,4 @@
 fn first() {
-    let x = 1;
+    let x = 2;
 }
@@ -49,4 +49,4 @@
 fn second() {
-    let y = 1;
+    let y = 2;
 }
";
    let files = wdyt::diff::parse(gapped_patch, theme("Nord")).unwrap();
    let id = daemon.insert(Content::Diff { files });

    // Try to comment on a range that spans the gap (line 2 to line 50 on new side).
    let (status, body) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "big.rs", "line": 2, "end_line": 50, "side": "new",
                "text": "spans a gap"
            }),
        )
        .await;
    // Must be rejected (400) because lines 4-49 are not addressable.
    assert_eq!(
        status, 400,
        "range spanning a gap should be rejected, got: {body}"
    );
}

#[tokio::test]
async fn diff_range_endpoint_must_exist() {
    // A range where the endpoint does not exist (e.g., L2-L100 but file only has 4 lines)
    // must be rejected.
    let daemon = Daemon::start().await;
    let id = diff_session(&daemon);

    // The diff session has lines 1-3 on the new side. Try L1-L100.
    let (status, body) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "src/lib.rs", "line": 1, "end_line": 100, "side": "new",
                "text": "way past end"
            }),
        )
        .await;
    assert_eq!(
        status, 400,
        "range past end of file should be rejected, got: {body}"
    );
}

#[tokio::test]
async fn diff_valid_contiguous_range_is_accepted() {
    // A valid contiguous range within a single hunk should still work.
    let daemon = Daemon::start().await;
    let id = diff_session(&daemon);

    // The diff has lines 1, 2, 3 on new side (context, added, context).
    let (status, body) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "src/lib.rs", "line": 1, "end_line": 3, "side": "new",
                "text": "valid range"
            }),
        )
        .await;
    assert_eq!(status, 200, "valid contiguous range rejected: {body}");
    let c: serde_json::Value = serde_json::from_str(&body).unwrap();
    // The snippet must include all three lines.
    let snippet = c["snippet"].as_str().unwrap();
    assert_eq!(snippet.lines().count(), 3, "snippet: {snippet}");
}

// ---------- Finding 6: Enforce validate_file_anchors in create_route ----------

#[tokio::test]
async fn api_rejects_duplicate_colliding_file_labels() {
    // Direct API calls with colliding file anchors must be rejected (400).
    let daemon = Daemon::start().await;

    // "a-b.rs" and "a/b.rs" both produce the anchor "f-a-b-rs".
    let file_a = wdyt::render::highlight("a-b.rs", "fn a() {}\n", theme("Nord")).unwrap();
    let file_b = wdyt::render::highlight("a/b.rs", "fn b() {}\n", theme("Nord")).unwrap();

    let (status, body) = daemon
        .post_json(
            "/api/sessions",
            serde_json::json!({
                "title": "collision test",
                "content": { "Code": { "files": [
                    { "label": "a-b.rs", "html": file_a.html, "highlighted": file_a.highlighted,
                      "sources": file_a.sources, "lines": file_a.lines, "language": file_a.language },
                    { "label": "a/b.rs", "html": file_b.html, "highlighted": file_b.highlighted,
                      "sources": file_b.sources, "lines": file_b.lines, "language": file_b.language },
                ]}}
            }),
        )
        .await;
    assert_eq!(status, 400, "colliding labels accepted: {body}");
    assert!(
        body.contains("anchor collision"),
        "error message should mention collision: {body}"
    );
}

#[tokio::test]
async fn api_accepts_valid_non_colliding_labels() {
    // Non-colliding labels should be accepted.
    let daemon = Daemon::start().await;

    let file_a = wdyt::render::highlight("src/main.rs", "fn main() {}\n", theme("Nord")).unwrap();
    let file_b = wdyt::render::highlight("src/lib.rs", "pub fn lib() {}\n", theme("Nord")).unwrap();

    let (status, body) = daemon
        .post_json(
            "/api/sessions",
            serde_json::json!({
                "title": "valid test",
                "content": { "Code": { "files": [
                    { "label": "src/main.rs", "html": file_a.html, "highlighted": file_a.highlighted,
                      "sources": file_a.sources, "lines": file_a.lines, "language": file_a.language },
                    { "label": "src/lib.rs", "html": file_b.html, "highlighted": file_b.highlighted,
                      "sources": file_b.sources, "lines": file_b.lines, "language": file_b.language },
                ]}}
            }),
        )
        .await;
    assert_eq!(status, 200, "valid labels rejected: {body}");
}

#[tokio::test]
async fn code_file_ids_are_namespaced_to_prevent_cross_file_collision() {
    // In a multi-file code session, each file's line IDs must be namespaced by
    // the file anchor so id="L1" doesn't collide across files.
    let daemon = Daemon::start().await;
    let file_a = wdyt::render::highlight("src/main.rs", "fn main() {}\n", theme("Nord")).unwrap();
    let file_b = wdyt::render::highlight("src/lib.rs", "pub fn lib() {}\n", theme("Nord")).unwrap();
    let id = daemon.insert(Content::Code {
        files: vec![file_a, file_b],
    });

    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    // The file-namespaced IDs should be present.
    assert!(
        body.contains("id=\"f-src-main-rs-L1\""),
        "namespaced ID for main.rs L1 not found: {body}"
    );
    assert!(
        body.contains("id=\"f-src-lib-rs-L1\""),
        "namespaced ID for lib.rs L1 not found: {body}"
    );
    // Plain id="L1" should NOT appear (would collide across files).
    // (The #f-<file> section anchors are still present.)
    assert!(
        body.contains("id=\"f-src-main-rs\""),
        "file section anchor for main.rs missing: {body}"
    );
}

// ---------- Finding 7: Fragment parity/bounds ----------

#[test]
fn js_parser_has_same_bounds_as_rust_parser() {
    // The JS parseSel in FRAGMENT_JS must apply the same max fragment/path and
    // safe integer rules as the Rust parser.
    let ui_source = include_str!("../src/ui.rs");
    let fragment_section = ui_source
        .find("const FRAGMENT_JS")
        .expect("FRAGMENT_JS const not found");
    let fragment_end = ui_source[fragment_section..].find("\"##;").unwrap() + fragment_section;
    let fragment_js = &ui_source[fragment_section..fragment_end];

    // Must have MAX_SAFE_INTEGER check (already had safeInt).
    assert!(
        fragment_js.contains("Number.MAX_SAFE_INTEGER"),
        "JS parser missing MAX_SAFE_INTEGER check"
    );
    // Must have fragment length limit (8192).
    assert!(
        fragment_js.contains("8192"),
        "JS parser missing max fragment length (8192)"
    );
    // Must have encoded path length limit (4096).
    assert!(
        fragment_js.contains("4096"),
        "JS parser missing max encoded path length (4096)"
    );
    // Must reject empty encoded path.
    assert!(
        fragment_js.contains("!encodedPath") || fragment_js.contains("!encodedPath2"),
        "JS parser missing empty path rejection"
    );
}

// --- Expanding the context a patch leaves out -------------------------------

/// A patch over a ten-line file, with the file attached so its hidden lines can
/// be served. The patch touches line 5 only, so lines 1-3 and 7-10 are hidden.
fn expandable_diff_session(daemon: &Daemon) -> String {
    const PATCH: &str = "\
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -4,3 +4,3 @@
 four
-five
+FIVE
 six
";
    let mut files = wdyt::diff::parse(PATCH, theme("Nord")).unwrap();
    let source: Vec<String> = (1..=10)
        .map(|n| match n {
            4 => "four".to_owned(),
            5 => "FIVE".to_owned(),
            6 => "six".to_owned(),
            other => format!("line {other}"),
        })
        .collect();
    assert!(
        wdyt::diff::attach_source(&mut files[0], source),
        "the file must agree with the patch"
    );
    daemon.insert(Content::Diff { files })
}

#[tokio::test]
async fn hidden_context_is_served_with_both_line_numbers() {
    let daemon = Daemon::start().await;
    let id = expandable_diff_session(&daemon);

    let (status, body) = daemon
        .get(&format!("/s/{id}/context?file=0&from=1&to=3"))
        .await;
    assert_eq!(status, 200, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["file"], "src/lib.rs");
    let lines = json["lines"].as_array().expect("lines");
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0]["new"], 1);
    // Nothing has shifted before the change, so the sides agree.
    assert_eq!(lines[0]["old"], 1);
    // Highlighted by the daemon, so the text arrives inside syntect's spans.
    let html = lines[0]["html"].as_str().unwrap();
    assert!(html.contains("line ") && html.contains(">1"), "{body}");
}

/// The page renders a band per hidden run, carrying the range it covers; the
/// script paints the controls, so the row is where the two agree.
#[tokio::test]
async fn the_page_marks_each_hidden_run() {
    let daemon = Daemon::start().await;
    let id = expandable_diff_session(&daemon);
    let (status, page) = daemon.get(&format!("/s/{id}")).await;
    assert_eq!(status, 200);
    assert!(
        page.contains(r#"data-from="1" data-to="3" data-place="before""#),
        "no leading band: {page}"
    );
    assert!(
        page.contains(r#"data-from="7" data-to="10" data-place="after""#),
        "no trailing band: {page}"
    );
    // The `@@` header row gives way to the band that replaces it.
    assert!(!page.contains(r#"<tr class="hunk">"#), "{page}");
}

/// Without a captured file there is nothing to reveal, and the page must go back
/// to showing the hunk header rather than an expander that cannot work.
#[tokio::test]
async fn a_diff_with_no_captured_file_has_no_bands() {
    let daemon = Daemon::start().await;
    let id = diff_session(&daemon);
    let (_, page) = daemon.get(&format!("/s/{id}")).await;
    assert!(!page.contains(r#"class="gap""#), "{page}");
    assert!(page.contains(r#"<tr class="hunk">"#), "{page}");

    let (status, body) = daemon
        .get(&format!("/s/{id}/context?file=0&from=1&to=2"))
        .await;
    assert_eq!(status, 400, "{body}");
}

#[tokio::test]
async fn a_context_request_is_bounded() {
    let daemon = Daemon::start().await;
    let id = expandable_diff_session(&daemon);
    let cases = [
        ("file=0&from=0&to=2", "line 0 is not a line"),
        ("file=0&from=5&to=2", "backwards range"),
        ("file=0&from=1&to=9999", "past the end of the file"),
        ("file=0&from=1&to=3000", "more than one request may ask for"),
        ("file=7&from=1&to=2", "no such file"),
        ("from=1&to=2", "no file"),
        ("file=x&from=1&to=2", "file is not a number"),
    ];
    for (query, why) in cases {
        let (status, body) = daemon.get(&format!("/s/{id}/context?{query}")).await;
        assert_eq!(status, 400, "{why}: {query} gave {status} {body}");
    }
}

/// A line the reader revealed is in no hunk, but it is in the file the session
/// captured — and that file was only accepted because it agreed with the patch,
/// so a comment on it can be quoted with the same confidence.
#[tokio::test]
async fn a_revealed_line_can_be_commented_on() {
    let daemon = Daemon::start().await;
    let id = expandable_diff_session(&daemon);

    let (status, body) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "src/lib.rs",
                "line": 9,
                "side": "new",
                "text": "why is this here?"
            }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let comment: serde_json::Value = serde_json::from_str(&body).unwrap();
    // Quoted from the daemon's own copy of the file, not from the request.
    assert_eq!(comment["snippet"], "line 9");

    // Past the end of the file is still refused.
    let (status, _) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "src/lib.rs", "line": 99, "side": "new", "text": "nope"
            }),
        )
        .await;
    assert_eq!(status, 400);
}

// --- Discussions under a comment --------------------------------------------

/// A diff session with one comment on it, which is a thread with one message.
async fn asked_diff_session(daemon: &Daemon) -> (String, u64) {
    let id = diff_session(daemon);
    let (status, body) = daemon
        .post_json(
            &format!("/s/{id}/comments"),
            serde_json::json!({
                "file": "src/lib.rs", "line": 2, "side": "new", "text": "why is this a clone?"
            }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let comment: serde_json::Value = serde_json::from_str(&body).unwrap();
    (id, comment["id"].as_u64().unwrap())
}

/// Who said something is decided by the route it arrived on. The page cannot
/// claim to be the agent whose answers the reader is trusting.
#[tokio::test]
async fn the_route_decides_the_author() {
    let daemon = Daemon::start().await;
    let (id, thread) = asked_diff_session(&daemon).await;

    let (status, body) = daemon
        .post_json(
            &format!("/s/{id}/comments/{thread}/messages"),
            serde_json::json!({ "text": "one more thing", "from": "agent" }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let message: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        message["from"], "user",
        "the page spoke as the agent: {body}"
    );

    let (status, body) = daemon
        .post_json(
            &format!("/api/sessions/{id}/comments/{thread}/messages"),
            serde_json::json!({ "text": "because it is cheap here" }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let message: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(message["from"], "agent");
}

/// The page polls to hear the answer, and polling must not tell the reader their
/// own question has been picked up.
#[tokio::test]
async fn reading_a_discussion_does_not_take_it() {
    let daemon = Daemon::start().await;
    let (id, _) = asked_diff_session(&daemon).await;

    for route in [
        format!("/s/{id}/threads"),
        format!("/api/sessions/{id}/threads"),
    ] {
        let (status, body) = daemon.get(&route).await;
        assert_eq!(status, 200, "{body}");
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["threads"][0]["text"], "why is this a clone?");
        assert!(
            json["threads"][0]["delivered_at"].is_null(),
            "{route} marked it delivered: {body}"
        );
    }
}

/// Receiving the next question takes it: a second agent must not answer the same
/// one, and a second receive finds nothing.
#[tokio::test]
async fn the_next_question_is_taken_once() {
    let daemon = Daemon::start().await;
    let (id, thread) = asked_diff_session(&daemon).await;

    let (status, body) = daemon
        .get(&format!("/api/sessions/{id}/recv?timeout_secs=5"))
        .await;
    assert_eq!(status, 200, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["received"], true);
    assert_eq!(json["event"]["kind"], "comment");
    assert_eq!(json["event"]["message"]["text"], "why is this a clone?");
    // The thread comes with it: the line, the quoted code, and the exchange so
    // far, since an answer is about the code rather than about the sentence.
    assert_eq!(json["event"]["thread"]["line"], 2);
    assert_eq!(json["event"]["thread"]["snippet"], "    let x = 2;");

    let (_, body) = daemon
        .get(&format!("/api/sessions/{id}/recv?timeout_secs=1"))
        .await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        json["received"], false,
        "the same question came back: {body}"
    );

    // The page can now say an agent has it.
    let (_, body) = daemon.get(&format!("/s/{id}/threads")).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(!json["threads"][0]["delivered_at"].is_null(), "{body}");

    // An answer is not a question, so nothing is waiting after it.
    let (status, _) = daemon
        .post_json(
            &format!("/api/sessions/{id}/comments/{thread}/messages"),
            serde_json::json!({ "text": "because it is cheap" }),
        )
        .await;
    assert_eq!(status, 200);
    let (_, body) = daemon
        .get(&format!("/api/sessions/{id}/recv?timeout_secs=1"))
        .await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["received"], false, "{body}");
}

/// An agent already waiting is woken by what the reader writes next, rather than
/// finding out on its next poll.
#[tokio::test]
async fn a_waiting_agent_is_woken_by_a_follow_up() {
    let daemon = Daemon::start().await;
    let (id, thread) = asked_diff_session(&daemon).await;
    // Clear the comment itself out of the way.
    daemon
        .get(&format!("/api/sessions/{id}/recv?timeout_secs=5"))
        .await;

    let base = daemon.base.clone();
    let session = id.clone();
    let waiting = tokio::spawn(async move {
        reqwest::get(format!(
            "{base}/api/sessions/{session}/recv?timeout_secs=20"
        ))
        .await
        .expect("request sent")
        .text()
        .await
        .expect("body")
    });

    tokio::time::sleep(Duration::from_millis(150)).await;
    let (status, _) = daemon
        .post_json(
            &format!("/s/{id}/comments/{thread}/messages"),
            serde_json::json!({ "text": "and the one below it?" }),
        )
        .await;
    assert_eq!(status, 200);

    let body = tokio::time::timeout(Duration::from_secs(10), waiting)
        .await
        .expect("the wait ended when the reader wrote")
        .expect("task");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["received"], true, "{body}");
    assert_eq!(json["event"]["message"]["text"], "and the one below it?");
}

#[tokio::test]
async fn a_message_is_checked_before_it_is_kept() {
    let daemon = Daemon::start().await;
    let (id, thread) = asked_diff_session(&daemon).await;

    let (status, _) = daemon
        .post_json(
            &format!("/s/{id}/comments/{thread}/messages"),
            serde_json::json!({ "text": "   " }),
        )
        .await;
    assert_eq!(status, 400, "an empty message");

    let (status, _) = daemon
        .post_json(
            &format!("/s/{id}/comments/{thread}/messages"),
            serde_json::json!({ "text": "x".repeat(20_000) }),
        )
        .await;
    assert_eq!(status, 400, "too long");

    let (status, _) = daemon
        .post_json(
            &format!("/s/{id}/comments/999/messages"),
            serde_json::json!({ "text": "hello?" }),
        )
        .await;
    assert_eq!(status, 404, "no such thread");
}

/// The exchange is rendered by the server too, so a reload shows it without
/// waiting for a poll — and so it is there at all with no script.
#[tokio::test]
async fn the_page_renders_the_exchange() {
    let daemon = Daemon::start().await;
    let (id, thread) = asked_diff_session(&daemon).await;
    daemon
        .post_json(
            &format!("/api/sessions/{id}/comments/{thread}/messages"),
            serde_json::json!({ "text": "because the borrow would leak" }),
        )
        .await;

    let (status, page) = daemon.get(&format!("/s/{id}")).await;
    assert_eq!(status, 200);
    assert!(
        page.contains(&format!(r#"data-thread="{thread}""#)),
        "no thread marker: {page}"
    );
    assert!(page.contains(r#"class="said agent""#), "{page}");
    assert!(page.contains("because the borrow would leak"), "{page}");
    // The foot is the script's to paint, so the server leaves it empty.
    assert!(page.contains(r#"<div class="threadfoot">"#), "{page}");
}
