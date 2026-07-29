//! End-to-end tests against a real daemon on a real port.
//!
//! The router's `Service` impl is over hyper's own body type, so calling it
//! in-process is awkward; binding a loopback listener is both simpler and
//! closer to what actually runs.

use std::net::{IpAddr, TcpListener};
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;

use showme::config::Config;
use showme::render::{theme, theme_colors};
use showme::session::{Content, Store};

/// A daemon on its own port, with a store the test can reach into.
struct Daemon {
    base: String,
    store: Store,
}

impl Daemon {
    async fn start() -> Self {
        // Each test's daemon needs a port no other test's daemon will take. Asking
        // the OS for a free one and handing the number over is not enough: the
        // listener has to close before the daemon rebinds it, and two tests handed
        // the same port in that gap both "come up" on it, so one reads the other's
        // store. That made the suite fail intermittently.
        //
        // So: a window of ports per test, keyed by the process id as well as a
        // counter. The counter alone would collide under nextest, which runs every
        // test in a fresh process where the counter restarts at zero.
        const WINDOW: u16 = 8;
        static NEXT: AtomicU16 = AtomicU16::new(0);
        // 20000-60000, well clear of the 3000-3010 range showme itself defaults to.
        let spread = (std::process::id() as u16).wrapping_mul(WINDOW) % 40_000;
        let base_port = 20_000 + spread.wrapping_add(NEXT.fetch_add(WINDOW, Ordering::Relaxed));

        let config = Config {
            port_low: base_port,
            // The daemon walks upwards from `port_low`, so the window gives it room
            // to step over anything already held.
            port_high: base_port.saturating_add(WINDOW - 1),
            bind: IpAddr::from([127, 0, 0, 1]),
            webhook_url: None,
            ..Config::default()
        };
        let store = Store::new(None);

        let serving = store.clone();
        let serve_config = config.clone();
        tokio::spawn(async move {
            let _ = showme::server::serve(&serve_config, serving).await;
        });

        // Which port it actually took is discovered rather than assumed, since it
        // steps over anything already held in its window.
        let port = Self::find_port(&config).await;
        let daemon = Self {
            base: format!("http://127.0.0.1:{port}"),
            store,
        };
        daemon.wait_until_up().await;
        daemon
    }

    /// The port this test's daemon landed on, within its own window.
    async fn find_port(config: &Config) -> u16 {
        let client = showme::client::Client::new(config);
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
    let file = showme::render::highlight("x.rs", "fn main() {}\n", theme("Nord")).unwrap();
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
    let files = showme::diff::parse(PATCH, theme("Nord")).unwrap();
    daemon.insert(Content::Diff { files })
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
    assert!(body.contains("position: fixed"), "{body}");
}

#[tokio::test]
async fn a_replied_session_shows_the_reply_and_no_form() {
    let daemon = Daemon::start().await;
    let id = code_session(&daemon);
    assert!(daemon.store.reply(&id, "looks good".to_owned()).unwrap());

    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    assert!(body.contains("looks good"), "{body}");
    assert!(!body.contains("<textarea"), "still offers a form: {body}");
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
        body.contains("body.framed main { position: relative; }"),
        "{body}"
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
    let doc = showme::render::markdown(
        "d.md",
        "> [!NOTE]\n> careful\n\n```rust\nfn f() {}\n```\n",
        "Nord",
    );
    let id = daemon.insert(Content::Docs { files: vec![doc] });

    let (_, body) = daemon.get(&format!("/s/{id}")).await;
    // comrak emits the class; unstyled the callout reads as a stray heading.
    assert!(body.contains("markdown-alert"), "{body}");
    assert!(body.contains(".markdown-alert-title"), "no styling: {body}");
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
    let id = daemon.store.insert_new(showme::session::NewSession {
        origin: showme::zellij_origin::Origin {
            cwd: Some("/local/home/rcoh/code/showme".to_owned()),
            session: Some("dd-2".to_owned()),
            tab: Some("showme cli".to_owned()),
            pane: Some("Build showme CLI tool".to_owned()),
        },
        ..showme::session::NewSession::new("a title".to_owned(), Content::Code { files: vec![] })
    });

    let (status, body) = daemon.get(&format!("/s/{id}")).await;
    assert_eq!(status, 200);
    assert!(body.contains("dd-2"), "no session name: {body}");
    assert!(body.contains("showme cli"), "no tab name: {body}");
    assert!(
        body.contains("/local/home/rcoh/code/showme"),
        "no cwd: {body}"
    );
}

#[tokio::test]
async fn a_guided_review_renders_above_the_content_with_working_anchors() {
    // The agent's own tour of the work. Its links have to resolve to the file
    // sections on the same page, or the tour points at nothing.
    let daemon = Daemon::start().await;
    let files = showme::diff::parse(PATCH, theme("Nord")).unwrap();
    let brief = showme::render::markdown(
        "brief",
        "Start with [the library](#f-src-lib-rs).\n",
        "Nord",
    );
    let id = daemon.store.insert_new(showme::session::NewSession {
        brief: Some(brief.html),
        ..showme::session::NewSession::new("a title".to_owned(), Content::Diff { files })
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
    // And the file nav is still there: the brief must not have displaced it.
    assert!(body.contains("class=\"files\""), "{body}");
}

#[tokio::test]
async fn a_sessions_own_theme_colours_its_page() {
    // Highlighting happens in the CLI but the page's colours are chosen by the
    // daemon, so a per-session theme has to reach the stylesheet — otherwise a
    // light theme's code lands on the default's dark card.
    let daemon = Daemon::start().await;
    let file = showme::render::highlight("x.rs", "fn main() {}\n", theme("GitHub")).unwrap();
    let id = daemon.store.insert_new(showme::session::NewSession {
        theme: Some("GitHub".to_owned()),
        ..showme::session::NewSession::new(
            "a title".to_owned(),
            Content::Code { files: vec![file] },
        )
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
    let id = daemon.store.insert_new(showme::session::NewSession {
        theme: Some("No Such Theme".to_owned()),
        ..showme::session::NewSession::new("a title".to_owned(), Content::Code { files: vec![] })
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
    let files = showme::diff::parse(&body, theme("Nord")).expect("re-parses as a diff");
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
        if showme::ports::is_free(IpAddr::from([127, 0, 0, 1]), next) {
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
        let _ = showme::server::serve(&serve_config, serving).await;
    });

    // The client discovers the port rather than assuming it, and identifies the
    // daemon by a marker: the squatter would otherwise be indistinguishable.
    let client = showme::client::Client::new(&config);
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
    let client = showme::client::Client::new(&config);
    assert!(
        !client.is_up().await,
        "an unrelated server passed as showme"
    );
}
