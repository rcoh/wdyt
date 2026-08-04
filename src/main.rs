//! The `wdyt` command.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};

use wdyt::client::Client;
use wdyt::config::Config;
use wdyt::notify::Notification;
use wdyt::ports;
use wdyt::render;
use wdyt::session::{Content, Store};

#[derive(Parser)]
#[command(
    name = "wdyt",
    version,
    about = "Show your work: a link in Slack, a reply box on the page.",
    // Agents read `--help` more often than people do, so spell out the flow.
    after_help = "\
WORKFLOW (for coding agents):

  1. Create one or more sessions, then present them together:
       wdyt create code src/lib.rs --title \"public API\"
       wdyt create docs DESIGN.md --title \"design\"
       wdyt create guided-diff REVIEW.md --patch changes.patch
       wdyt present <code-id> <design-id>

  2. In a guided review brief (--brief / --brief-file), link to files with
     #f-<path> where non-alphanumerics become dashes:
       src/main.rs  → #f-src-main-rs
       a-b/c-d.rs   → #f-a-b-c-d-rs
     Point at a line span the GitHub way, appending -L<start>[-L<end>]:
       src/main.rs L10       → #f-src-main-rs-L10
       src/main.rs L10-L20   → #f-src-main-rs-L10-L20
     (line anchors resolve in code/diff; in docs they jump to the covering
     block.) After session creation, available anchors are printed to stderr.

  3. stdout is machine-readable: create prints one session JSON object; present
     prints the combined URL, then (if waiting) the reply as JSON. Pass
     --no-wait to present for fire-and-forget.

  4. Collect the reply + line/block comments (blocks until one arrives):
       wdyt collect <id>   # returns reply + line/block comments as JSON
       wdyt ack <id> \"rerunning the build with your flag\"  # turns receipt green

  5. Answer the questions left on lines, while they are still reading:
       wdyt recv <id>             # blocks until they say something; JSON on stdout
       wdyt say <id> <thread> \"because the borrow would leak\"
       wdyt threads <id>          # the whole discussion, changing nothing
     Loop recv → say until recv exits 2 (nothing said in --timeout).

  6. Exit status 2 means timeout — the session stays open for later collection.

STDIN CAVEAT: --brief - and a piped `wdyt create diff` cannot both read stdin.
Use --brief-file when piping a diff."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Register content without notifying or waiting.
    ///
    /// Prints one JSON object with `id`, `url`, `title`, and `kind`. Use the
    /// returned IDs with `wdyt present`.
    Create {
        #[command(subcommand)]
        content: CreateCommand,
    },

    /// Present one or more sessions as tabs in a single notification.
    ///
    /// The first session owns the overall reply and is waited on by default.
    Present {
        /// Session IDs in tab order.
        #[arg(required = true)]
        ids: Vec<String>,
        #[command(flatten)]
        present: PresentArgs,
    },

    /// Print a free port for a demo server to bind.
    Port {
        /// Print every free port in the range instead of just the first.
        #[arg(long)]
        all: bool,
    },

    /// Subscribe to a session and print the reader's next event.
    ///
    /// The one command an agent loops on: it blocks until the reader does
    /// something — types the overall reply, or leaves/answers a line comment —
    /// and prints that one event as JSON. Call it again for the next one.
    ///
    /// Prints {"received": true, "event": {"kind": "reply", "reply": {...}}} for
    /// the overall reply, or {"kind": "comment", "thread": {...}, "message":
    /// {...}} for a line comment, whose thread carries the file, line, quoted
    /// snippet and the exchange so far. Answer a comment with `wdyt say`, and
    /// acknowledge the reply with `wdyt ack`.
    ///
    /// Blocks forever unless `--timeout` is set. Exit status 2 means nothing
    /// arrived before the timeout; the session stays open for `wdyt recv` again
    /// or `wdyt collect <id>`.
    Recv {
        /// Session id, as printed when the session was created.
        id: String,
        /// Give up after this many seconds (exit status 2 on timeout). Waits
        /// forever if unset.
        #[arg(long)]
        timeout: Option<u64>,
    },

    /// List replies the daemon is holding, including ones sent hours ago.
    ///
    /// By default shows unread replies (those not yet acknowledged with `wdyt ack`).
    /// A reply collected by a dead process stays in the unread inbox until acked.
    Inbox {
        /// Include replies already acknowledged.
        #[arg(long)]
        all: bool,
    },

    /// Collect a session's reply and any line comments.
    ///
    /// Blocks until a reply arrives (forever unless `--timeout` is set); pass
    /// `--no-wait` to return whatever is there right now instead.
    ///
    /// Returns JSON: {"replied": bool, "reply": {...}, "comments": [...]}
    /// Each comment has file, line, side, snippet, and text fields.
    /// A `target` field identifies structured content: "brief", "doc:<n>", or
    /// "guided:<n>". Ordinary code/diff comments omit it because the file path
    /// is sufficient.
    ///
    /// After collecting, acknowledge with: wdyt ack <id> "your note"
    Collect {
        /// Session id.
        id: String,
        /// Return immediately even if no reply has arrived yet.
        #[arg(long)]
        no_wait: bool,
        /// Give up after this many seconds (exit status 2 on timeout). Waits
        /// forever if unset.
        #[arg(long, conflicts_with = "no_wait")]
        timeout: Option<u64>,
    },

    /// Tell the user you read their reply and what you are doing about it.
    ///
    /// This is what lights up the receipt on their page (grey → green), so send
    /// it once you have actually read the reply and know what to do.
    /// Example: wdyt ack abc123 "rerunning the build with your flag"
    Ack {
        /// Session id.
        id: String,
        /// What you are doing about the reply, in a sentence.
        #[arg(required = true)]
        note: Vec<String>,
    },

    /// Answer one thread in a discussion.
    ///
    /// The thread number is the comment id, which `wdyt recv` and `wdyt threads`
    /// both print. The answer appears under that line on the page, in the reader's
    /// own view of the diff.
    /// Example: wdyt say abc123 4 "renamed it; the old name was from the prototype"
    Say {
        /// Session id.
        id: String,
        /// Which thread to answer.
        thread: u64,
        /// What to say.
        #[arg(required = true)]
        text: Vec<String>,
    },

    /// Print every thread in a session and everything said in it.
    ///
    /// Reading changes nothing: use it to catch up on a discussion, or after a
    /// restart, without claiming to have picked anything up. `wdyt recv` is the
    /// one that takes a question.
    Threads {
        /// Session id.
        id: String,
    },

    /// Run the daemon in the foreground.
    Serve,

    /// List live sessions.
    List,

    /// Show or change the configuration.
    Config {
        /// Set the notification webhook URL.
        #[arg(long)]
        webhook_url: Option<String>,
        /// For a Slack workflow-trigger URL, the workflow variable to fill.
        #[arg(long)]
        webhook_field: Option<String>,
        /// Set the port range, e.g. 3000-3010.
        #[arg(long)]
        ports: Option<String>,
        /// Set the syntax theme. See `wdyt themes`.
        #[arg(long)]
        theme: Option<String>,
        /// Print the config file path and exit.
        #[arg(long)]
        path: bool,
    },

    /// List available syntax themes.
    Themes,

    /// Install the wdyt agent skills into a project or globally.
    ///
    /// Installs wdyt mechanics plus code-change and design presentation skills
    /// for Kiro, Claude Code, Codex, and agents using the shared skills path.
    /// By default writes beneath the current project; pass --global to write
    /// beneath the user's home directory instead.
    ///
    /// Example: wdyt skill --global
    Skill {
        /// Install beneath the user's home directory instead of this project.
        #[arg(long, short)]
        global: bool,
    },
}

#[derive(Subcommand)]
enum CreateCommand {
    /// Register source files, syntax highlighted.
    Code {
        /// Files to show.
        #[arg(required = true)]
        files: Vec<PathBuf>,
        #[command(flatten)]
        session: SessionArgs,
    },

    /// Register a unified diff, reviewable line by line with comments.
    Diff {
        /// A patch file, or `-` to read the diff from stdin.
        #[arg(default_value = "-")]
        patch: PathBuf,
        #[command(flatten)]
        session: SessionArgs,
    },

    /// Register an ordered review mixing prose, diff ranges, and code ranges.
    GuidedDiff {
        /// Markdown review containing wdyt-diff and wdyt-code directives.
        review: PathBuf,
        /// A patch file, or `-` to read the patch from stdin.
        #[arg(long, default_value = "-")]
        patch: PathBuf,
        #[command(flatten)]
        session: GuidedSessionArgs,
    },

    /// Register markdown files, rendered as GFM.
    Docs {
        #[arg(required = true)]
        files: Vec<PathBuf>,
        #[command(flatten)]
        session: SessionArgs,
    },

    /// Register a directory of static files.
    Dir {
        /// Directory to serve.
        root: PathBuf,
        /// Page to open within it.
        #[arg(long, default_value = "index.html")]
        entry: String,
        #[command(flatten)]
        session: SessionArgs,
    },

    /// Register a server already running on PORT.
    Demo {
        /// The port the server is listening on.
        port: u16,
        /// Path to open on that server.
        #[arg(long, default_value = "/")]
        path: String,
        #[command(flatten)]
        session: SessionArgs,
    },
}

/// Metadata stored with a create-only content session.
#[derive(Args)]
struct SessionArgs {
    /// Headline for the notification and the page.
    #[arg(long, short)]
    title: Option<String>,

    /// A sentence of extra context, shown under the title.
    #[arg(long, short)]
    note: Option<String>,

    /// Syntax theme for this session only. See `wdyt themes`.
    ///
    /// The page's own colours follow the theme, so a light theme gives a light
    /// page. Set a lasting default with `wdyt config --theme`.
    #[arg(long)]
    theme: Option<String>,

    /// A guided review, as markdown, shown above the content.
    ///
    /// Explain what changed and what to look at first. A link to `#f-<path>`
    /// jumps to that file, with non-alphanumerics replaced by dashes: `src/a.rs`
    /// is `#f-src-a-rs`. Append `-L<start>` or `-L<start>-L<end>` for a line
    /// span, mirroring GitHub: `#f-src-a-rs-L10-L20`. Pass `-` to read the
    /// markdown from stdin.
    #[arg(long, short = 'b')]
    brief: Option<String>,

    /// Read the guided review from a markdown file.
    #[arg(long, conflicts_with = "brief")]
    brief_file: Option<PathBuf>,
}

/// Guided reviews carry their narrative in the content itself.
#[derive(Args)]
struct GuidedSessionArgs {
    /// Headline for the page.
    #[arg(long, short)]
    title: Option<String>,

    /// A sentence of extra context, shown under the title.
    #[arg(long, short)]
    note: Option<String>,

    /// Syntax theme for this session only. See `wdyt themes`.
    #[arg(long)]
    theme: Option<String>,
}

/// Notification and wait controls for a composed presentation.
#[derive(Args)]
struct PresentArgs {
    /// Headline for the notification.
    #[arg(long, short)]
    title: Option<String>,

    /// A sentence of extra context, shown under the title.
    #[arg(long, short)]
    note: Option<String>,

    /// Do not wait for a reply.
    #[arg(long)]
    no_wait: bool,

    /// Seconds to wait before giving up (exit status 2).
    #[arg(long, conflicts_with = "no_wait")]
    timeout: Option<u64>,

    /// Print the link instead of sending a notification.
    #[arg(long)]
    no_notify: bool,
}

impl SessionArgs {
    /// The theme to highlight with: this invocation's, or the configured default.
    fn theme<'a>(&'a self, config: &'a Config) -> Result<&'a str> {
        let name = self.theme.as_deref().unwrap_or(&config.theme);
        anyhow::ensure!(
            render::theme_names().contains(&name),
            "unknown theme {name:?}. See `wdyt themes`"
        );
        Ok(name)
    }

    /// The guided review, rendered to HTML.
    ///
    /// Rendered here rather than in the daemon because this is where the syntax
    /// theme for fenced code blocks is known.
    fn brief(&self, config: &Config) -> Result<Option<(String, Vec<String>)>> {
        let source = match (&self.brief, &self.brief_file) {
            (Some(text), _) if text == "-" => {
                use std::io::Read as _;
                let mut buffer = String::new();
                std::io::stdin()
                    .read_to_string(&mut buffer)
                    .context("reading the brief from stdin")?;
                buffer
            }
            (Some(text), _) => text.clone(),
            (None, Some(path)) => read_text(path)?,
            (None, None) => return Ok(None),
        };
        if source.trim().is_empty() {
            return Ok(None);
        }
        let theme = self.theme(config)?;
        let doc = render::markdown_commentable("brief", &source, theme);
        Ok(Some((doc.html, doc.sources)))
    }
}

impl From<GuidedSessionArgs> for SessionArgs {
    fn from(guided: GuidedSessionArgs) -> Self {
        Self {
            title: guided.title,
            note: guided.note,
            theme: guided.theme,
            brief: None,
            brief_file: None,
        }
    }
}

impl PresentArgs {
    fn should_wait(&self) -> bool {
        !self.no_wait
    }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    if let Err(error) = run().await {
        eprintln!("wdyt: {error:#}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    let config = Config::load()?;

    match cli.command {
        Command::Serve => {
            let ttl = config
                .session_ttl_hours
                .map(|h| Duration::from_secs(h * 3600));
            let store = Store::open(Config::state_path()?, ttl)?;
            wdyt::server::serve(&config, store).await
        }

        Command::Port { all } => {
            // A port the daemon is already on is not free, so it drops out of
            // this list on its own. The filter covers the case where the daemon
            // has not started yet and would take this very port.
            let taken = Client::new(&config).daemon_port().await;
            let free = ports::free_ports(config.bind, config.ports())
                .into_iter()
                .filter(|port| Some(*port) != taken)
                .collect::<Vec<_>>();
            anyhow::ensure!(
                !free.is_empty(),
                "no free port in {}-{}",
                config.port_low,
                config.port_high
            );
            if all {
                for port in free {
                    println!("{port}");
                }
            } else {
                println!("{}", free[0]);
            }
            Ok(())
        }

        Command::Create { content } => match content {
            CreateCommand::Code { files, session } => {
                let prepared = prepare_code(&config, &session, files)?;
                create_content(&config, session, prepared).await
            }
            CreateCommand::Diff { patch, session } => {
                let prepared = prepare_diff(&config, &session, patch)?;
                create_content(&config, session, prepared).await
            }
            CreateCommand::GuidedDiff {
                review,
                patch,
                session,
            } => {
                let session = SessionArgs::from(session);
                let prepared = prepare_guided_diff(&config, &session, review, patch)?;
                create_content(&config, session, prepared).await
            }
            CreateCommand::Docs { files, session } => {
                let prepared = prepare_docs(&config, &session, files)?;
                create_content(&config, session, prepared).await
            }
            CreateCommand::Dir {
                root,
                entry,
                session,
            } => {
                let prepared = prepare_dir(&session, root, entry)?;
                create_content(&config, session, prepared).await
            }
            CreateCommand::Demo {
                port,
                path,
                session,
            } => {
                let prepared = prepare_demo(&config, &session, port, path)?;
                create_content(&config, session, prepared).await
            }
        },

        Command::Present { ids, present } => present_sessions(&config, ids, present).await,

        Command::Recv { id, timeout } => {
            let client = Client::new(&config);
            let received = client.recv(&id, timeout_duration(timeout)).await?;
            let Some(event) = received.event else {
                eprintln!(
                    "wdyt: nothing arrived within {}. The session stays open — \
                     `wdyt recv {id}` again, or read the discussion with `wdyt threads {id}`.",
                    timeout_label(timeout)
                );
                std::process::exit(2);
            };
            println!("{}", serde_json::to_string_pretty(&event)?);
            match event {
                wdyt::server::UserEvent::Reply { .. } => {
                    eprintln!(
                        "wdyt: reply received — the verdict on the whole session. \
                         Next:\n  \
                         wdyt collect {id}    # the reply plus any line comments\n  \
                         wdyt ack {id} \"<what you are doing>\""
                    );
                }
                wdyt::server::UserEvent::Comment { thread, .. } => {
                    eprintln!(
                        "wdyt: {} on {}:{}. Answer it with:\n  \
                         wdyt say {id} {} \"<your answer>\"",
                        if thread.replies.is_empty() {
                            "a new comment"
                        } else {
                            "a follow-up"
                        },
                        thread.file,
                        thread.line,
                        thread.id,
                    );
                }
            }
            Ok(())
        }

        Command::Say { id, thread, text } => {
            let text = text.join(" ");
            anyhow::ensure!(!text.trim().is_empty(), "the answer must not be empty");
            let message = Client::new(&config).say(&id, thread, &text).await?;
            println!("{}", serde_json::to_string_pretty(&message)?);
            eprintln!(
                "wdyt: answered thread {thread}. Wait for the next event with:\n  \
                 wdyt recv {id}"
            );
            Ok(())
        }

        Command::Threads { id } => {
            let threads = Client::new(&config).threads(&id).await?;
            println!("{}", serde_json::to_string_pretty(&threads)?);
            let waiting = threads.iter().filter(|t| t.awaiting_agent()).count();
            if waiting > 0 {
                eprintln!(
                    "wdyt: {waiting} thread{} waiting on you. Take the next with:\n  \
                     wdyt recv {id}",
                    if waiting == 1 { "" } else { "s" }
                );
            }
            Ok(())
        }

        Command::Inbox { all } => {
            let items = Client::new(&config).inbox(!all).await?;
            // JSON, because the caller is an agent: one array to parse rather
            // than a table to scrape.
            println!("{}", serde_json::to_string_pretty(&items)?);
            if items.is_empty() {
                eprintln!(
                    "wdyt: no {}replies waiting",
                    if all { "" } else { "unread " }
                );
            }
            Ok(())
        }

        Command::Collect {
            id,
            no_wait,
            timeout,
        } => {
            let client = Client::new(&config);
            // Waiting is the default: block on the reply so an agent has one
            // command that sits open until the user acts. `--no-wait` keeps the
            // old poll-and-return recovery behaviour. Either way, both the reply
            // and the line comments come back.
            let reply = if no_wait {
                client.collect(&id).await?
            } else {
                match timeout {
                    Some(seconds) => eprintln!("wdyt: waiting up to {seconds}s for a reply…"),
                    None => eprintln!("wdyt: waiting for a reply…"),
                }
                client.wait(&id, timeout_duration(timeout)).await?
            };
            let comments = client.comments(&id).await?;
            let out = serde_json::json!({
                "replied": reply.is_some(),
                "reply": reply,
                "comments": comments,
            });
            println!("{}", serde_json::to_string_pretty(&out)?);
            if reply.is_none() && comments.is_empty() {
                eprintln!("wdyt: session {id} has no reply yet");
                std::process::exit(2);
            }
            if reply.is_some() {
                // A reply was collected — prompt for acknowledgement.
                eprintln!(
                    "wdyt: collected. Now run:\n  \
                     wdyt ack {id} \"<what you are doing>\""
                );
            } else {
                // Comments exist but no reply — ack is not valid yet.
                eprintln!(
                    "wdyt: collected {n} comment{s} (no reply to acknowledge yet)",
                    n = comments.len(),
                    s = if comments.len() == 1 { "" } else { "s" }
                );
            }
            Ok(())
        }

        Command::Ack { id, note } => {
            let note = note.join(" ");
            Client::new(&config).ack(&id, &note).await?;
            eprintln!("wdyt: acknowledged {id}");
            Ok(())
        }

        Command::List => {
            let client = Client::new(&config);
            let port = client.daemon_port().await.with_context(|| {
                format!(
                    "no wdyt daemon on ports {}-{}",
                    config.port_low, config.port_high
                )
            })?;
            println!("http://{}:{port}/", config.public_host);
            Ok(())
        }

        Command::Config {
            webhook_url,
            webhook_field,
            ports: port_range,
            theme,
            path,
        } => {
            if path {
                println!("{}", Config::path()?.display());
                return Ok(());
            }

            let mut config = config;
            let mut changed = false;
            if let Some(url) = webhook_url {
                config.webhook_url = (!url.is_empty()).then_some(url);
                changed = true;
            }
            if let Some(field) = webhook_field {
                anyhow::ensure!(!field.is_empty(), "webhook_field must not be empty");
                config.webhook_field = field;
                changed = true;
            }
            if let Some(range) = port_range {
                let (low, high) = parse_range(&range)?;
                config.port_low = low;
                config.port_high = high;
                changed = true;
            }
            if let Some(theme) = theme {
                anyhow::ensure!(
                    render::theme_names().contains(&theme.as_str()),
                    "unknown theme {theme:?}. See `wdyt themes`"
                );
                config.theme = theme;
                changed = true;
            }

            if changed {
                let path = config.save()?;
                println!("wrote {}", path.display());
                // A workflow trigger answers `{"ok":true}` even when the
                // payload is wrong and only fails inside the workflow, so name
                // the shape here rather than leaving it to be discovered.
                if let Some(url) = &config.webhook_url {
                    match wdyt::notify::Style::detect(url) {
                        wdyt::notify::Style::Trigger => println!(
                            "detected a Slack workflow trigger: sending \
                             {{\"{}\": \"…\"}}.\nIf the workflow declares a \
                             different variable, set it with \
                             `wdyt config --webhook-field <NAME>`.",
                            config.webhook_field
                        ),
                        wdyt::notify::Style::Blocks => {
                            println!("detected a Slack incoming webhook: sending Block Kit.");
                        }
                    }
                }
                println!("restart the daemon for port or theme changes to take effect");
            } else {
                println!("{}", toml::to_string_pretty(&config)?);
                if config.webhook_url.is_none() {
                    println!(
                        "# no webhook_url set: links are printed instead of sent.\n\
                         # set one with: wdyt config --webhook-url <URL>"
                    );
                }
            }
            Ok(())
        }

        Command::Themes => {
            for name in render::theme_names() {
                println!("{name}");
            }
            Ok(())
        }

        Command::Skill { global } => install_skill(global),
    }
}

struct PreparedContent {
    title: String,
    content: Content,
}

struct CreatedContent {
    id: String,
    url: String,
    title: String,
    kind: &'static str,
    file_labels: Vec<String>,
}

#[derive(serde::Serialize)]
struct CreatedOutput {
    id: String,
    url: String,
    title: String,
    kind: &'static str,
}

fn prepare_code(
    config: &Config,
    session: &SessionArgs,
    files: Vec<PathBuf>,
) -> Result<PreparedContent> {
    let theme = render::theme(session.theme(config)?);
    let mut rendered = Vec::with_capacity(files.len());
    for path in &files {
        let source = read_text(path)?;
        rendered.push(render::highlight(&label(path), &source, theme)?);
    }
    let title = session
        .title
        .clone()
        .unwrap_or_else(|| default_title(&files, "code"));
    Ok(PreparedContent {
        title,
        content: Content::Code { files: rendered },
    })
}

fn prepare_diff(config: &Config, session: &SessionArgs, patch: PathBuf) -> Result<PreparedContent> {
    // Both cannot come from stdin: whichever read first would consume the
    // other's input, and the diff is the one that is usually piped.
    anyhow::ensure!(
        !(patch == *"-" && session.brief.as_deref() == Some("-")),
        "the diff and the brief cannot both come from stdin. \
         Use `--brief-file <PATH>`, or pass the patch as a file"
    );
    let source = if patch == *"-" {
        use std::io::Read as _;
        let mut buffer = String::new();
        std::io::stdin()
            .read_to_string(&mut buffer)
            .context("reading the diff from stdin")?;
        anyhow::ensure!(
            !buffer.trim().is_empty(),
            "empty diff on stdin. Pipe one in, as with \
             `git diff | wdyt create diff`, or pass a patch file"
        );
        buffer
    } else {
        read_text(&patch)?
    };

    let mut files = wdyt::diff::parse(&source, render::theme(session.theme(config)?))?;
    wdyt::diff::capture_sources(&mut files);
    files.sort_by_key(|file| wdyt::diff::review_rank(&file.label));
    let title = session
        .title
        .clone()
        .unwrap_or_else(|| match files.as_slice() {
            [one] => one.label.clone(),
            many => format!("{} changed files", many.len()),
        });
    Ok(PreparedContent {
        title,
        content: Content::Diff { files },
    })
}

fn prepare_guided_diff(
    config: &Config,
    session: &SessionArgs,
    review: PathBuf,
    patch: PathBuf,
) -> Result<PreparedContent> {
    let review_source = read_text(&review)?;
    let patch_source = if patch == *"-" {
        use std::io::Read as _;
        let mut buffer = String::new();
        std::io::stdin()
            .read_to_string(&mut buffer)
            .context("reading the guided diff patch from stdin")?;
        buffer
    } else {
        read_text(&patch)?
    };
    let workspace = std::env::current_dir().context("determining the guided diff workspace")?;
    let blocks = wdyt::guided::parse(
        &review_source,
        &patch_source,
        &workspace,
        session.theme(config)?,
    )?;
    let title = session.title.clone().unwrap_or_else(|| label(&review));
    Ok(PreparedContent {
        title,
        content: Content::Guided { blocks },
    })
}

fn prepare_docs(
    config: &Config,
    session: &SessionArgs,
    files: Vec<PathBuf>,
) -> Result<PreparedContent> {
    let mut rendered = Vec::with_capacity(files.len());
    for path in &files {
        let source = read_text(path)?;
        rendered.push(render::markdown_commentable(
            &label(path),
            &source,
            session.theme(config)?,
        ));
    }
    let title = session
        .title
        .clone()
        .unwrap_or_else(|| default_title(&files, "docs"));
    Ok(PreparedContent {
        title,
        content: Content::Docs { files: rendered },
    })
}

fn prepare_dir(session: &SessionArgs, root: PathBuf, entry: String) -> Result<PreparedContent> {
    let root = root
        .canonicalize()
        .with_context(|| format!("{} does not exist", root.display()))?;
    anyhow::ensure!(root.is_dir(), "{} is not a directory", root.display());
    if !root.join(&entry).exists() {
        eprintln!(
            "wdyt: warning: {} has no {entry}; the page may be blank",
            root.display()
        );
    }
    let title = session
        .title
        .clone()
        .unwrap_or_else(|| root.file_name().unwrap_or_default().display().to_string());
    Ok(PreparedContent {
        title,
        content: Content::Static { root, entry },
    })
}

fn prepare_demo(
    config: &Config,
    session: &SessionArgs,
    port: u16,
    path: String,
) -> Result<PreparedContent> {
    anyhow::ensure!(
        !ports::is_free(config.bind, port),
        "nothing is listening on port {port}. Start the server first, \
         then run `wdyt create demo {port}`"
    );
    let title = session
        .title
        .clone()
        .unwrap_or_else(|| format!("live demo on port {port}"));
    Ok(PreparedContent {
        title,
        content: Content::Demo { port, path },
    })
}

async fn register_content(
    config: &Config,
    session: &SessionArgs,
    prepared: PreparedContent,
) -> Result<CreatedContent> {
    let PreparedContent { title, content } = prepared;
    let kind = content.kind();
    let file_labels = content_file_labels(&content);
    if !file_labels.is_empty()
        && let Err(message) = wdyt::validate_file_anchors(&file_labels)
    {
        anyhow::bail!("{message}");
    }

    // The theme travels with the session so the page chrome agrees with the
    // highlighting the CLI already applied.
    let theme = session.theme(config)?.to_owned();
    let (brief, brief_sources) = match session.brief(config)? {
        Some((html, sources)) => (Some(html), sources),
        None => (None, Vec::new()),
    };
    let client = Client::new(config);
    let id = client
        .create(wdyt::server::CreateSession {
            note: session.note.clone(),
            theme: Some(theme),
            brief,
            brief_sources,
            ..wdyt::server::CreateSession::new(title.clone(), content)
        })
        .await?;
    let url = client.session_url(&id).await?;
    Ok(CreatedContent {
        id,
        url,
        title,
        kind,
        file_labels,
    })
}

async fn create_content(
    config: &Config,
    session: SessionArgs,
    prepared: PreparedContent,
) -> Result<()> {
    let created = register_content(config, &session, prepared).await?;
    let output = CreatedOutput {
        id: created.id.clone(),
        url: created.url,
        title: created.title,
        kind: created.kind,
    };
    println!("{}", serde_json::to_string(&output)?);
    print_file_anchors(&created.file_labels);
    eprintln!(
        "wdyt: session {} created (not notified; not waiting)",
        created.id
    );
    Ok(())
}

async fn present_sessions(config: &Config, ids: Vec<String>, present: PresentArgs) -> Result<()> {
    let do_wait = present.should_wait();
    let timeout = present.timeout;
    let no_notify = present.no_notify;
    let client = Client::new(config);
    let prepared = client.prepare_presentation(&ids).await?;
    let ids = prepared.ids;
    let url = prepared.url;
    let owner = ids
        .first()
        .expect("presentation preparation requires at least one ID")
        .clone();
    let title = present.title.unwrap_or_else(|| {
        format!(
            "{} wdyt {}",
            ids.len(),
            if ids.len() == 1 {
                "session"
            } else {
                "sessions"
            }
        )
    });
    let notification = Notification {
        title,
        note: present.note,
        url: url.clone(),
        kind: "presentation",
        details: vec![summarize(ids.len(), "tab")],
    };
    let sent = send_notification(config, no_notify, &notification).await?;

    print_and_flush_url(&url)?;
    if !sent {
        print_notification_status(no_notify);
    }

    if do_wait {
        wait_for_presentation(&client, &owner, timeout, "presentation").await
    } else {
        eprintln!(
            "wdyt: presentation created (not waiting). The overall reply belongs to {owner}:\n  \
             wdyt collect {owner}"
        );
        Ok(())
    }
}

async fn send_notification(
    config: &Config,
    no_notify: bool,
    notification: &Notification,
) -> Result<bool> {
    let webhook = (!no_notify)
        .then_some(config.webhook_url.as_deref())
        .flatten();
    notification.send(webhook, &config.webhook_field).await
}

async fn wait_for_presentation(
    client: &Client,
    owner: &str,
    timeout: Option<u64>,
    label: &str,
) -> Result<()> {
    match timeout {
        Some(seconds) => eprintln!("wdyt: waiting up to {seconds}s for a reply…"),
        None => eprintln!("wdyt: waiting for a reply…"),
    }
    match client.wait(owner, timeout_duration(timeout)).await? {
        Some(reply) => {
            println!("{}", serde_json::to_string_pretty(&reply)?);
            eprintln!(
                "wdyt: reply received. Run `wdyt collect {owner}` for line comments, then:\n  \
                 wdyt ack {owner} \"<what you are doing>\"\n  \
                 wdyt recv {owner}       # answer questions left on lines, as they come"
            );
            Ok(())
        }
        None => {
            eprintln!(
                "wdyt: no reply within {}. The {label} stays open:\n  \
                 wdyt collect {owner}    # check for reply + comments later\n  \
                 wdyt recv {owner}       # wait for a question on a line\n  \
                 wdyt inbox              # list all pending replies",
                timeout_label(timeout)
            );
            std::process::exit(2);
        }
    }
}

fn content_file_labels(content: &Content) -> Vec<String> {
    match content {
        Content::Code { files } => files.iter().map(|file| file.label.clone()).collect(),
        Content::Diff { files } => files.iter().map(|file| file.label.clone()).collect(),
        Content::Docs { files } => files.iter().map(|file| file.label.clone()).collect(),
        _ => Vec::new(),
    }
}

fn print_and_flush_url(url: &str) -> Result<()> {
    println!("{url}");
    use std::io::Write as _;
    std::io::stdout()
        .flush()
        .context("failed to flush presentation URL to stdout")
}

fn print_notification_status(no_notify: bool) {
    if no_notify {
        eprintln!("wdyt: presentation ready (notification skipped)");
    } else {
        eprintln!("wdyt: presentation ready (no webhook configured)");
    }
}

fn print_file_anchors(file_labels: &[String]) {
    if file_labels.is_empty() {
        return;
    }
    eprintln!("wdyt: file anchors for --brief links:");
    for label in file_labels {
        eprintln!("  {} → #{}", label, wdyt::file_anchor(label));
    }
    if let Some(label) = file_labels.first() {
        let anchor = wdyt::file_anchor(label);
        eprintln!(
            "  add -L<start> or -L<start>-L<end> for a line span, \
             e.g. #{anchor}-L10 or #{anchor}-L10-L20"
        );
    }
}

/// A CLI `--timeout` in seconds as a `Duration`, or `None` to wait forever.
fn timeout_duration(timeout: Option<u64>) -> Option<Duration> {
    timeout.map(Duration::from_secs)
}

/// How the wait is described in progress and give-up messages: a bounded number
/// of seconds, or "forever" when no timeout is set.
fn timeout_label(timeout: Option<u64>) -> String {
    match timeout {
        Some(seconds) => format!("{seconds}s"),
        None => "forever".to_string(),
    }
}

fn read_text(path: &Path) -> Result<String> {
    let bytes =
        std::fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    String::from_utf8(bytes).with_context(|| format!("{} is not valid UTF-8", path.display()))
}

/// The label shown for a file: its path as given, which is more useful than a
/// bare file name when several files share one.
fn label(path: &Path) -> String {
    path.display().to_string()
}

fn default_title(files: &[PathBuf], kind: &str) -> String {
    match files {
        [one] => label(one),
        many => format!("{} {kind} files", many.len()),
    }
}

fn summarize(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

fn parse_range(value: &str) -> Result<(u16, u16)> {
    let (low, high) = value
        .trim()
        .split_once('-')
        .context("expected a range like 3000-3010")?;
    let low: u16 = low.trim().parse().context("invalid low port")?;
    let high: u16 = high.trim().parse().context("invalid high port")?;
    anyhow::ensure!(low <= high, "range {value:?} is inverted");
    Ok((low, high))
}

/// Agent skills embedded at compile time so `cargo install wdyt` is sufficient.
const SKILLS: &[(&str, &str)] = &[
    ("wdyt", include_str!("../skill/SKILL.md")),
    (
        "presenting-code-changes",
        include_str!("../skill/presenting-code-changes/SKILL.md"),
    ),
    (
        "presenting-designs",
        include_str!("../skill/presenting-designs/SKILL.md"),
    ),
];

/// Install the bundled skill into the appropriate directories.
fn install_skill(global: bool) -> Result<()> {
    let roots: Vec<PathBuf> = if global {
        let home = directories::BaseDirs::new().context("cannot determine home directory")?;
        let home = home.home_dir();
        vec![
            home.join(".kiro/skills"),
            home.join(".claude/skills"),
            home.join(".codex/skills"),
            home.join(".config/agents/skills"),
        ]
    } else {
        let cwd = std::env::current_dir().context("cannot determine working directory")?;
        vec![
            cwd.join(".kiro/skills"),
            cwd.join(".claude/skills"),
            cwd.join(".codex/skills"),
            cwd.join(".agents/skills"),
        ]
    };

    let mut installed = Vec::new();
    for root in &roots {
        for (name, contents) in SKILLS {
            let dir = root.join(name);
            std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
            let dest = dir.join("SKILL.md");
            std::fs::write(&dest, contents)
                .with_context(|| format!("writing {}", dest.display()))?;
            installed.push(dest);
        }
    }

    for path in &installed {
        eprintln!("wdyt: installed {}", path.display());
    }
    let scope = if global {
        "globally"
    } else {
        "in this project"
    };
    eprintln!("wdyt: skills installed {scope}. Agents will now know how to use wdyt.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parses_the_documented_invocations() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    #[test]
    fn default_titles_describe_the_selection() {
        assert_eq!(
            default_title(&[PathBuf::from("src/a.rs")], "code"),
            "src/a.rs"
        );
        assert_eq!(
            default_title(&[PathBuf::from("a"), PathBuf::from("b")], "code"),
            "2 code files"
        );
    }

    #[test]
    fn summarize_pluralizes() {
        assert_eq!(summarize(1, "file"), "1 file");
        assert_eq!(summarize(3, "file"), "3 files");
    }

    #[test]
    fn installer_bundles_mechanics_code_and_design_skills() {
        let names = SKILLS.iter().map(|(name, _)| *name).collect::<Vec<_>>();
        assert_eq!(
            names,
            ["wdyt", "presenting-code-changes", "presenting-designs"]
        );
        assert!(
            SKILLS
                .iter()
                .all(|(_, contents)| contents.starts_with("---"))
        );
        assert!(
            SKILLS[1]
                .1
                .contains("Present changes in this order: Carefully explain")
        );
    }

    #[test]
    fn parse_range_rejects_bad_input() {
        assert_eq!(parse_range("3000-3010").unwrap(), (3000, 3010));
        assert!(parse_range("3000").is_err());
        assert!(parse_range("3010-3000").is_err());
    }

    #[test]
    fn create_and_present_commands_parse() {
        match Cli::parse_from(["wdyt", "create", "code", "f.rs", "--title", "API"]).command {
            Command::Create {
                content: CreateCommand::Code { files, session },
            } => {
                assert_eq!(files, [PathBuf::from("f.rs")]);
                assert_eq!(session.title.as_deref(), Some("API"));
            }
            _ => panic!("expected create code"),
        }

        match Cli::parse_from([
            "wdyt",
            "create",
            "guided-diff",
            "REVIEW.md",
            "--patch",
            "changes.patch",
            "--title",
            "Review",
        ])
        .command
        {
            Command::Create {
                content:
                    CreateCommand::GuidedDiff {
                        review,
                        patch,
                        session,
                    },
            } => {
                assert_eq!(review, PathBuf::from("REVIEW.md"));
                assert_eq!(patch, PathBuf::from("changes.patch"));
                assert_eq!(session.title.as_deref(), Some("Review"));
            }
            _ => panic!("expected create guided-diff"),
        }

        match Cli::parse_from(["wdyt", "present", "abc123", "def456", "--timeout", "30"]).command {
            Command::Present { ids, present } => {
                assert_eq!(ids, ["abc123", "def456"]);
                assert_eq!(present.timeout, Some(30));
                assert!(present.should_wait());
            }
            _ => panic!("expected present"),
        }
    }

    #[test]
    fn only_create_exposes_content_modes() {
        assert!(Cli::try_parse_from(["wdyt", "code", "f.rs"]).is_err());
        assert!(Cli::try_parse_from(["wdyt", "diff", "p.patch"]).is_err());
        assert!(Cli::try_parse_from(["wdyt", "docs", "README.md"]).is_err());
        assert!(Cli::try_parse_from(["wdyt", "dir", "/tmp"]).is_err());
        assert!(Cli::try_parse_from(["wdyt", "demo", "8080"]).is_err());
        assert!(Cli::try_parse_from(["wdyt", "create", "code", "f.rs"]).is_ok());
        match Cli::parse_from(["wdyt", "create", "guided-diff", "REVIEW.md"]).command {
            Command::Create {
                content: CreateCommand::GuidedDiff { patch, .. },
            } => assert_eq!(patch, PathBuf::from("-")),
            _ => panic!("expected create guided-diff"),
        }
    }

    #[test]
    fn create_has_no_presentation_flags() {
        assert!(Cli::try_parse_from(["wdyt", "create", "code", "f.rs", "--no-wait"]).is_err());
        assert!(Cli::try_parse_from(["wdyt", "create", "code", "f.rs", "--no-notify"]).is_err());
        assert!(Cli::try_parse_from(["wdyt", "create", "code", "f.rs", "--timeout", "5"]).is_err());
        assert!(
            Cli::try_parse_from([
                "wdyt",
                "create",
                "guided-diff",
                "REVIEW.md",
                "--brief",
                "duplicate narrative"
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "wdyt",
                "create",
                "guided-diff",
                "REVIEW.md",
                "--brief-file",
                "BRIEF.md"
            ])
            .is_err()
        );
    }

    #[test]
    fn present_wait_controls_are_consistent() {
        match Cli::parse_from(["wdyt", "present", "abc123", "--no-wait"]).command {
            Command::Present { present, .. } => assert!(!present.should_wait()),
            _ => panic!("expected present"),
        }
        assert!(
            Cli::try_parse_from(["wdyt", "present", "abc123", "--no-wait", "--timeout", "5"])
                .is_err()
        );
    }

    #[test]
    fn standalone_recv_command_parses() {
        let cli = Cli::parse_from(["wdyt", "recv", "abc123", "--timeout", "30"]);
        match cli.command {
            Command::Recv { id, timeout } => {
                assert_eq!(id, "abc123");
                assert_eq!(timeout, Some(30));
            }
            _ => panic!("expected Recv"),
        }
    }

    #[test]
    fn collect_waits_by_default() {
        // A bare `collect` blocks: no --no-wait, no --timeout.
        match Cli::parse_from(["wdyt", "collect", "abc123"]).command {
            Command::Collect {
                id,
                no_wait,
                timeout,
            } => {
                assert_eq!(id, "abc123");
                assert!(!no_wait);
                assert_eq!(timeout, None);
            }
            _ => panic!("expected Collect"),
        }
    }

    #[test]
    fn collect_no_wait_and_timeout_parse() {
        match Cli::parse_from(["wdyt", "collect", "abc123", "--no-wait"]).command {
            Command::Collect { no_wait, .. } => assert!(no_wait),
            _ => panic!("expected Collect"),
        }
        match Cli::parse_from(["wdyt", "collect", "abc123", "--timeout", "5"]).command {
            Command::Collect { timeout, .. } => assert_eq!(timeout, Some(5)),
            _ => panic!("expected Collect"),
        }
    }

    #[test]
    fn collect_no_wait_and_timeout_conflict() {
        let result =
            Cli::try_parse_from(["wdyt", "collect", "abc123", "--no-wait", "--timeout", "5"]);
        assert!(result.is_err(), "--no-wait and --timeout should conflict");
    }

    /// The discussion commands, which are what an agent runs in a loop: wait for
    /// a question, answer it, wait again.
    #[test]
    fn the_discussion_commands_parse() {
        match Cli::parse_from(["wdyt", "recv", "abc123", "--timeout", "45"]).command {
            Command::Recv { id, timeout } => {
                assert_eq!(id, "abc123");
                assert_eq!(timeout, Some(45));
            }
            _ => panic!("expected Recv"),
        }
        // The answer is taken as words rather than one quoted argument, so an
        // agent that forgets the quotes still says what it meant.
        match Cli::parse_from(["wdyt", "say", "abc123", "4", "because", "it", "is", "cheap"])
            .command
        {
            Command::Say { id, thread, text } => {
                assert_eq!(id, "abc123");
                assert_eq!(thread, 4);
                assert_eq!(text.join(" "), "because it is cheap");
            }
            _ => panic!("expected Say"),
        }
        match Cli::parse_from(["wdyt", "threads", "abc123"]).command {
            Command::Threads { id } => assert_eq!(id, "abc123"),
            _ => panic!("expected Threads"),
        }
        // A thread number is required and must be a number: silently answering
        // thread 0 would put the words in the wrong place.
        assert!(Cli::try_parse_from(["wdyt", "say", "abc123", "notanumber", "hi"]).is_err());
        assert!(Cli::try_parse_from(["wdyt", "say", "abc123", "4"]).is_err());
    }
}
