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

  1. Show your work — the command waits for a reply by default:
       wdyt code src/lib.rs src/main.rs --title \"the new parser\"
       git diff | wdyt diff --title \"the parser rewrite\"
       wdyt docs DESIGN.md --title \"design writeup\"

  2. In a guided review brief (--brief / --brief-file), link to files with
     #f-<path> where non-alphanumerics become dashes:
       src/main.rs  → #f-src-main-rs
       a-b/c-d.rs   → #f-a-b-c-d-rs
     Point at a line span the GitHub way, appending -L<start>[-L<end>]:
       src/main.rs L10       → #f-src-main-rs-L10
       src/main.rs L10-L20   → #f-src-main-rs-L10-L20
     (line anchors resolve in code/diff; in docs they jump to the covering
     block.) After session creation, available anchors are printed to stderr.

  3. stdout is machine-readable: first the session URL, then (if waiting) the
     reply as JSON. Pass --no-wait for fire-and-forget.

  4. Collect the reply + line/block comments (blocks until one arrives):
       wdyt collect <id>   # returns reply + line/block comments as JSON
       wdyt ack <id> \"rerunning the build with your flag\"  # turns receipt green

  5. Answer the questions left on lines, while they are still reading:
       wdyt recv <id>             # blocks until they say something; JSON on stdout
       wdyt say <id> <thread> \"because the borrow would leak\"
       wdyt threads <id>          # the whole discussion, changing nothing
     Loop recv → say until recv exits 2 (nothing said in --timeout).

  6. Exit status 2 means timeout — the session stays open for later collection.

STDIN CAVEAT: --brief - and a piped diff cannot both read stdin. Use --brief-file
when piping a diff."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show source files, syntax highlighted.
    ///
    /// Waits for a reply by default. Pass --no-wait for fire-and-forget.
    /// Example: wdyt code src/lib.rs src/main.rs --title "the new parser"
    Code {
        /// Files to show.
        #[arg(required = true)]
        files: Vec<PathBuf>,
        #[command(flatten)]
        show: ShowArgs,
    },

    /// Show a unified diff, reviewable line by line with comments.
    ///
    /// Reads from stdin by default: `git diff | wdyt diff --title "..."`.
    /// Pass a file path instead of `-` to read from a file.
    /// Note: --brief - cannot be combined with a piped diff; use --brief-file.
    Diff {
        /// A patch file, or `-` to read the diff from stdin.
        #[arg(default_value = "-")]
        patch: PathBuf,
        #[command(flatten)]
        show: ShowArgs,
    },

    /// Show markdown files, rendered (GFM: tables, tasklists, callouts).
    ///
    /// Waits for a reply by default. Pass --no-wait for fire-and-forget.
    /// Example: wdyt docs DESIGN.md --title "design writeup"
    Docs {
        #[arg(required = true)]
        files: Vec<PathBuf>,
        #[command(flatten)]
        show: ShowArgs,
    },

    /// Show a directory of static files.
    Dir {
        /// Directory to serve.
        root: PathBuf,
        /// Page to open within it.
        #[arg(long, default_value = "index.html")]
        entry: String,
        #[command(flatten)]
        show: ShowArgs,
    },

    /// Show a server already running on PORT.
    Demo {
        /// The port the server is listening on.
        port: u16,
        /// Path to open on that server.
        #[arg(long, default_value = "/")]
        path: String,
        #[command(flatten)]
        show: ShowArgs,
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
    /// For markdown comments (docs/brief), a `target` field identifies where the
    /// comment was placed: "brief" for the guided review, "doc:<n>" for the nth
    /// docs file. Code/diff comments omit `target` (file path is sufficient).
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

    /// Install the wdyt agent skill into a project or globally.
    ///
    /// Writes SKILL.md so that coding agents (Kiro, Claude Code, etc.) know how
    /// to use wdyt. By default installs into the current project's .kiro/skills/
    /// and .claude/skills/ directories. Pass --global to install into the user's
    /// home directory instead (~/.kiro/skills/wdyt/ and ~/.claude/skills/wdyt/).
    ///
    /// Example: wdyt skill --global
    Skill {
        /// Install to ~/.kiro/skills/ and ~/.claude/skills/ instead of the
        /// current project.
        #[arg(long, short)]
        global: bool,
    },
}

/// Flags shared by the content-producing subcommands.
#[derive(Args)]
struct ShowArgs {
    /// Headline for the notification and the page.
    #[arg(long, short)]
    title: Option<String>,

    /// A sentence of extra context, shown under the title.
    #[arg(long, short)]
    note: Option<String>,

    /// Do not wait for a reply — fire and forget.
    ///
    /// By default every content command blocks until a reply arrives (or
    /// `--timeout` expires). Pass `--no-wait` when you only need the link.
    #[arg(long, conflicts_with = "wait")]
    no_wait: bool,

    /// Retained for backwards compatibility; waiting is now the default.
    #[arg(long, hide = true, conflicts_with = "no_wait")]
    wait: bool,

    /// Seconds to wait before giving up (exit status 2). Waits forever if unset.
    ///
    /// The session stays open after a timeout — check later with `wdyt inbox`
    /// or `wdyt collect <id>`.
    #[arg(long, conflicts_with = "no_wait")]
    timeout: Option<u64>,

    /// Print the link instead of sending a notification.
    #[arg(long)]
    no_notify: bool,

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

impl ShowArgs {
    /// Whether this invocation should wait for a reply.
    fn should_wait(&self) -> bool {
        // `--no-wait` opts out; the old `--wait` is accepted silently (it was
        // already the default).
        !self.no_wait
    }

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

        Command::Code { files, show } => {
            let theme = render::theme(show.theme(&config)?);
            let mut rendered = Vec::with_capacity(files.len());
            for path in &files {
                let source = read_text(path)?;
                rendered.push(render::highlight(&label(path), &source, theme)?);
            }
            let details = vec![summarize(files.len(), "file")];
            let title = show
                .title
                .clone()
                .unwrap_or_else(|| default_title(&files, "code"));
            show_content(
                &config,
                title,
                show,
                Content::Code { files: rendered },
                details,
            )
            .await
        }

        Command::Diff { patch, show } => {
            // Both cannot come from stdin: whichever read first would consume the
            // other's input, and the diff is the one that is usually piped.
            anyhow::ensure!(
                !(patch == *"-" && show.brief.as_deref() == Some("-")),
                "the diff and the brief cannot both come from stdin. \
                 Use `--brief-file <PATH>`, or pass the patch as a file"
            );
            // Reading from stdin is the common case: `git diff | wdyt diff`.
            let source = if patch == *"-" {
                use std::io::Read as _;
                let mut buffer = String::new();
                std::io::stdin()
                    .read_to_string(&mut buffer)
                    .context("reading the diff from stdin")?;
                anyhow::ensure!(
                    !buffer.trim().is_empty(),
                    "empty diff on stdin. Pipe one in, as with \
                     `git diff | wdyt diff`, or pass a patch file"
                );
                buffer
            } else {
                read_text(&patch)?
            };

            let mut files = wdyt::diff::parse(&source, render::theme(show.theme(&config)?))?;
            // The lines between the hunks are not in the patch. Read them from the
            // tree while we are still standing in it, so the page can offer them.
            wdyt::diff::capture_sources(&mut files);
            // A `git diff` lists files alphabetically, floating low-signal files
            // like Cargo.lock and .config/nextest.toml to the top. Order them by
            // review importance instead; the stable sort keeps patch order within
            // a rank.
            files.sort_by_key(|f| wdyt::diff::review_rank(&f.label));
            let added: usize = files.iter().map(|f| f.added).sum();
            let removed: usize = files.iter().map(|f| f.removed).sum();
            let title = show
                .title
                .clone()
                .unwrap_or_else(|| match files.as_slice() {
                    [one] => one.label.clone(),
                    many => format!("{} changed files", many.len()),
                });
            let details = vec![
                summarize(files.len(), "file"),
                format!("+{added} −{removed}"),
            ];
            show_content(&config, title, show, Content::Diff { files }, details).await
        }

        Command::Docs { files, show } => {
            let mut rendered = Vec::with_capacity(files.len());
            for path in &files {
                let source = read_text(path)?;
                rendered.push(render::markdown_commentable(
                    &label(path),
                    &source,
                    show.theme(&config)?,
                ));
            }
            let details = vec![summarize(files.len(), "doc")];
            let title = show
                .title
                .clone()
                .unwrap_or_else(|| default_title(&files, "docs"));
            show_content(
                &config,
                title,
                show,
                Content::Docs { files: rendered },
                details,
            )
            .await
        }

        Command::Dir { root, entry, show } => {
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
            let title = show
                .title
                .clone()
                .unwrap_or_else(|| format!("{}", root.file_name().unwrap_or_default().display()));
            let details = vec![root.display().to_string()];
            show_content(
                &config,
                title,
                show,
                Content::Static { root, entry },
                details,
            )
            .await
        }

        Command::Demo { port, path, show } => {
            // A demo the agent has not actually started is the most likely
            // mistake here, so check before sending a link to a dead port.
            anyhow::ensure!(
                !ports::is_free(config.bind, port),
                "nothing is listening on port {port}. Start the server first, \
                 then run `wdyt demo {port}`"
            );
            let title = show
                .title
                .clone()
                .unwrap_or_else(|| format!("live demo on port {port}"));
            let details = vec![format!("port {port}")];
            show_content(&config, title, show, Content::Demo { port, path }, details).await
        }

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

/// Creates the session, sends the notification, and optionally waits.
async fn show_content(
    config: &Config,
    title: String,
    show: ShowArgs,
    content: Content,
    details: Vec<String>,
) -> Result<()> {
    let kind = content.kind();
    let client = Client::new(config);
    // Extract wait parameters before partial moves of `show`.
    let do_wait = show.should_wait();
    let timeout = show.timeout;
    let no_notify = show.no_notify;
    // The theme travels with the session so the page's chrome agrees with the
    // highlighting the CLI already applied.
    let theme = show.theme(config)?.to_owned();
    let brief_pair = show.brief(config)?;
    let (brief, brief_sources) = match brief_pair {
        Some((html, sources)) => (Some(html), sources),
        None => (None, Vec::new()),
    };

    // Collect file labels for anchor printing (before content is moved).
    let file_labels: Vec<String> = match &content {
        Content::Code { files } => files.iter().map(|f| f.label.clone()).collect(),
        Content::Diff { files } => files.iter().map(|f| f.label.clone()).collect(),
        Content::Docs { files } => files.iter().map(|f| f.label.clone()).collect(),
        _ => Vec::new(),
    };

    // Reject duplicate/colliding anchors before creating the session. Two files
    // that normalize to the same #f- anchor would break page navigation and
    // guided-review links.
    if !file_labels.is_empty()
        && let Err(msg) = wdyt::validate_file_anchors(&file_labels)
    {
        anyhow::bail!("{msg}");
    }

    let id = client
        .create(wdyt::server::CreateSession {
            note: show.note.clone(),
            theme: Some(theme),
            brief,
            brief_sources,
            ..wdyt::server::CreateSession::new(title.clone(), content)
        })
        .await?;
    // Which agent is asking: with several running there is otherwise nothing in
    // the notification to say which pane to go back to.
    let mut details = details;
    if let Some(origin) = wdyt::zellij_origin::Origin::detect().summary() {
        details.push(origin);
    }
    // The daemon's port is discovered, not assumed: the range is chosen for
    // being forwarded, which also means other servers are already in it.
    let url = client.session_url(&id).await?;

    let notification = Notification {
        title,
        note: show.note,
        url: url.clone(),
        kind,
        details,
    };

    let webhook = (!no_notify)
        .then_some(config.webhook_url.as_deref())
        .flatten();
    let sent = notification.send(webhook, &config.webhook_field).await?;

    // The URL always goes to stdout so the agent can quote it back to the user
    // even when a notification went out.
    println!("{url}");
    // Flush stdout before blocking so a downstream reader gets the URL
    // immediately even when stdout is piped (and therefore fully buffered).
    use std::io::Write as _;
    std::io::stdout()
        .flush()
        .context("failed to flush session URL to stdout")?;

    if !sent {
        if no_notify {
            eprintln!("wdyt: session {id} ready (notification skipped)");
        } else {
            eprintln!("wdyt: session {id} ready (no webhook configured)");
        }
    }

    // Print file anchors to stderr so agents know how to link in --brief.
    if !file_labels.is_empty() {
        eprintln!("wdyt: file anchors for --brief links:");
        for label in &file_labels {
            eprintln!("  {} → #{}", label, wdyt::file_anchor(label));
        }
        // GitHub-style line spans hang off the same anchor, so an agent can
        // point the reader at an exact passage rather than a whole file.
        if let Some(label) = file_labels.first() {
            let a = wdyt::file_anchor(label);
            eprintln!(
                "  add -L<start> or -L<start>-L<end> for a line span, e.g. #{a}-L10 or #{a}-L10-L20"
            );
        }
    }

    if do_wait {
        match timeout {
            Some(seconds) => eprintln!("wdyt: waiting up to {seconds}s for a reply…"),
            None => eprintln!("wdyt: waiting for a reply…"),
        }
        match client.wait(&id, timeout_duration(timeout)).await? {
            Some(reply) => {
                println!("{}", serde_json::to_string_pretty(&reply)?);
                // Receiving it is not reading it, and the page says so until an
                // ack arrives. Prompt for one rather than leaving the user
                // looking at an amber dot.
                eprintln!(
                    "wdyt: reply received. Run `wdyt collect {id}` for line comments, then:\n  \
                     wdyt ack {id} \"<what you are doing>\"\n  \
                     wdyt recv {id}       # answer questions left on lines, as they come"
                );
            }
            None => {
                // The wait gave up, but the session has not: the daemon holds
                // the reply for as long as the session lives, so say where to
                // find it rather than letting it look lost.
                eprintln!(
                    "wdyt: no reply within {}. The session stays open:\n  \
                     wdyt collect {id}    # check for reply + comments later\n  \
                     wdyt recv {id}       # wait for a question on a line\n  \
                     wdyt inbox           # list all pending replies",
                    timeout_label(timeout)
                );
                std::process::exit(2);
            }
        }
    } else {
        // --no-wait: brief guidance on what to do next.
        eprintln!(
            "wdyt: session {id} created (not waiting). Later:\n  \
             wdyt collect {id}    # the reply and any line comments\n  \
             wdyt recv {id}       # block until a question is asked, then `wdyt say`"
        );
    }
    Ok(())
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

/// The wdyt SKILL.md, embedded at compile time.
const SKILL_MD: &str = include_str!("../skill/SKILL.md");

/// Install the bundled skill into the appropriate directories.
fn install_skill(global: bool) -> Result<()> {
    let targets: Vec<PathBuf> = if global {
        let home = directories::BaseDirs::new().context("cannot determine home directory")?;
        let home = home.home_dir();
        vec![
            home.join(".kiro/skills/wdyt"),
            home.join(".claude/skills/wdyt"),
            home.join(".codex/skills/wdyt"),
            home.join(".config/agents/skills/wdyt"),
        ]
    } else {
        let cwd = std::env::current_dir().context("cannot determine working directory")?;
        vec![
            cwd.join(".kiro/skills/wdyt"),
            cwd.join(".claude/skills/wdyt"),
            cwd.join(".codex/skills/wdyt"),
            cwd.join(".agents/skills/wdyt"),
        ]
    };

    let mut installed = Vec::new();
    for dir in &targets {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let dest = dir.join("SKILL.md");
        std::fs::write(&dest, SKILL_MD).with_context(|| format!("writing {}", dest.display()))?;
        installed.push(dest);
    }

    for path in &installed {
        eprintln!("wdyt: installed {}", path.display());
    }
    let scope = if global {
        "globally"
    } else {
        "in this project"
    };
    eprintln!("wdyt: skill installed {scope}. Agents will now know how to use wdyt.");
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
    fn parse_range_rejects_bad_input() {
        assert_eq!(parse_range("3000-3010").unwrap(), (3000, 3010));
        assert!(parse_range("3000").is_err());
        assert!(parse_range("3010-3000").is_err());
    }

    // — Parser tests for --no-wait / --wait on every content command —

    /// Helper: parse CLI args and extract the ShowArgs.
    fn parse_show(args: &[&str]) -> ShowArgs {
        let cli = Cli::parse_from(args);
        match cli.command {
            Command::Code { show, .. } => show,
            Command::Diff { show, .. } => show,
            Command::Docs { show, .. } => show,
            Command::Dir { show, .. } => show,
            Command::Demo { show, .. } => show,
            _ => panic!("not a content command"),
        }
    }

    #[test]
    fn code_waits_by_default() {
        let show = parse_show(&["wdyt", "code", "f.rs"]);
        assert!(show.should_wait());
    }

    #[test]
    fn code_no_wait_opts_out() {
        let show = parse_show(&["wdyt", "code", "f.rs", "--no-wait"]);
        assert!(!show.should_wait());
    }

    #[test]
    fn code_legacy_wait_still_accepted() {
        // --wait is silently accepted for backwards compat (it was the default).
        let show = parse_show(&["wdyt", "code", "f.rs", "--wait"]);
        assert!(show.should_wait());
    }

    #[test]
    fn diff_waits_by_default() {
        let show = parse_show(&["wdyt", "diff", "p.patch"]);
        assert!(show.should_wait());
    }

    #[test]
    fn diff_no_wait_opts_out() {
        let show = parse_show(&["wdyt", "diff", "--no-wait"]);
        assert!(!show.should_wait());
    }

    #[test]
    fn diff_legacy_wait_accepted() {
        let show = parse_show(&["wdyt", "diff", "--wait"]);
        assert!(show.should_wait());
    }

    #[test]
    fn docs_waits_by_default() {
        let show = parse_show(&["wdyt", "docs", "README.md"]);
        assert!(show.should_wait());
    }

    #[test]
    fn docs_no_wait_opts_out() {
        let show = parse_show(&["wdyt", "docs", "README.md", "--no-wait"]);
        assert!(!show.should_wait());
    }

    #[test]
    fn docs_legacy_wait_accepted() {
        let show = parse_show(&["wdyt", "docs", "README.md", "--wait"]);
        assert!(show.should_wait());
    }

    #[test]
    fn dir_waits_by_default() {
        let show = parse_show(&["wdyt", "dir", "/tmp"]);
        assert!(show.should_wait());
    }

    #[test]
    fn dir_no_wait_opts_out() {
        let show = parse_show(&["wdyt", "dir", "/tmp", "--no-wait"]);
        assert!(!show.should_wait());
    }

    #[test]
    fn dir_legacy_wait_accepted() {
        let show = parse_show(&["wdyt", "dir", "/tmp", "--wait"]);
        assert!(show.should_wait());
    }

    #[test]
    fn demo_waits_by_default() {
        let show = parse_show(&["wdyt", "demo", "8080"]);
        assert!(show.should_wait());
    }

    #[test]
    fn demo_no_wait_opts_out() {
        let show = parse_show(&["wdyt", "demo", "8080", "--no-wait"]);
        assert!(!show.should_wait());
    }

    #[test]
    fn demo_legacy_wait_accepted() {
        let show = parse_show(&["wdyt", "demo", "8080", "--wait"]);
        assert!(show.should_wait());
    }

    #[test]
    fn timeout_is_customisable() {
        let show = parse_show(&["wdyt", "code", "f.rs", "--timeout", "30"]);
        assert_eq!(show.timeout, Some(30));
        assert!(show.should_wait());
    }

    #[test]
    fn timeout_defaults_to_none_so_the_wait_sits_open() {
        // No `--timeout` means wait forever, not the old 600s default.
        let show = parse_show(&["wdyt", "code", "f.rs"]);
        assert_eq!(show.timeout, None);
        assert!(show.should_wait());
    }

    #[test]
    fn no_wait_and_timeout_conflict() {
        // --timeout explicitly supplied with --no-wait must be rejected.
        let result = Cli::try_parse_from(["wdyt", "code", "f.rs", "--no-wait", "--timeout", "5"]);
        assert!(result.is_err(), "--no-wait and --timeout should conflict");
    }

    #[test]
    fn wait_and_no_wait_conflict() {
        // --wait and --no-wait must be rejected.
        let result = Cli::try_parse_from(["wdyt", "code", "f.rs", "--wait", "--no-wait"]);
        assert!(result.is_err(), "--wait and --no-wait should conflict");
    }

    #[test]
    fn no_wait_is_not_in_help() {
        // --wait must be hidden; --no-wait must be visible.
        use clap::CommandFactory;
        let mut buf = Vec::new();
        Cli::command()
            .find_subcommand("code")
            .unwrap()
            .clone()
            .write_help(&mut buf)
            .unwrap();
        let help = String::from_utf8(buf).unwrap();
        assert!(help.contains("--no-wait"), "help should show --no-wait");
        // --wait is hidden: it should NOT appear as its own option line.
        // Check that no line starts with a --wait option description (lines
        // that only mention --no-wait are fine).
        let has_wait_option_line = help.lines().any(|line| {
            let trimmed = line.trim();
            // An option line for --wait would start with "--wait" but NOT
            // "--wait" as part of "--no-wait".
            trimmed.starts_with("--wait") && !trimmed.starts_with("--no-wait")
        });
        assert!(
            !has_wait_option_line,
            "--wait should be hidden from help output:\n{help}"
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
        let result = Cli::try_parse_from(["wdyt", "collect", "abc123", "--no-wait", "--timeout", "5"]);
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
