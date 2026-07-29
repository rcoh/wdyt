//! The `showme` command.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};

use showme::client::Client;
use showme::config::Config;
use showme::notify::Notification;
use showme::ports;
use showme::render;
use showme::session::{Content, Store};

#[derive(Parser)]
#[command(
    name = "showme",
    version,
    about = "Show your work: a link in Slack, a reply box on the page.",
    // Agents read `--help` more often than people do, so spell out the flow.
    after_help = "\
Typical use by an agent:

  # Show source files
  showme code src/lib.rs src/main.rs --title \"the new parser\"

  # Show a diff for review, with line comments
  git diff | showme diff --title \"the parser rewrite\" --wait

  # Show rendered markdown
  showme docs DESIGN.md --title \"design writeup\"

  # Show a directory of prepared HTML
  showme dir ./report --title \"benchmark report\"

  # Show a live server: get a port, start the app on it, then notify
  PORT=$(showme port)
  my-app --port $PORT &
  showme demo $PORT --title \"the new dashboard\" --wait

Add --wait to any of these to block until a reply is sent, and print it.

A reply outlives the wait. If --wait times out the session is still open, so a
later turn can pick the reply up:

  showme inbox              # replies waiting, as JSON
  showme collect <id>       # take one reply and its line comments"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show source files, syntax highlighted.
    Code {
        /// Files to show.
        #[arg(required = true)]
        files: Vec<PathBuf>,
        #[command(flatten)]
        show: ShowArgs,
    },

    /// Show a unified diff, reviewable line by line.
    Diff {
        /// A patch file, or `-` to read the diff from stdin.
        #[arg(default_value = "-")]
        patch: PathBuf,
        #[command(flatten)]
        show: ShowArgs,
    },

    /// Show markdown files, rendered.
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

    /// Wait for a reply to a session and print it.
    Wait {
        /// Session id, as printed when the session was created.
        id: String,
        /// Give up after this many seconds.
        #[arg(long, default_value_t = 600)]
        timeout: u64,
    },

    /// List replies the daemon is holding, including ones sent hours ago.
    Inbox {
        /// Include replies already collected.
        #[arg(long)]
        all: bool,
    },

    /// Collect a reply that has already arrived, and any line comments.
    Collect {
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
        /// Set the syntax theme. See `showme themes`.
        #[arg(long)]
        theme: Option<String>,
        /// Print the config file path and exit.
        #[arg(long)]
        path: bool,
    },

    /// List available syntax themes.
    Themes,
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

    /// Block until a reply arrives, then print it.
    #[arg(long)]
    wait: bool,

    /// Seconds to wait with --wait.
    #[arg(long, default_value_t = 600)]
    timeout: u64,

    /// Print the link instead of sending a notification.
    #[arg(long)]
    no_notify: bool,
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    if let Err(error) = run().await {
        eprintln!("showme: {error:#}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    let config = Config::load()?;

    match cli.command {
        Command::Serve => {
            let ttl = config.session_ttl_hours.map(|h| Duration::from_secs(h * 3600));
            showme::server::serve(&config, Store::new(ttl)).await
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
            let theme = render::theme(&config.theme);
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
            show_content(&config, title, show, Content::Code { files: rendered }, details).await
        }

        Command::Diff { patch, show } => {
            // Reading from stdin is the common case: `git diff | showme diff`.
            let source = if patch == PathBuf::from("-") {
                use std::io::Read as _;
                let mut buffer = String::new();
                std::io::stdin()
                    .read_to_string(&mut buffer)
                    .context("reading the diff from stdin")?;
                anyhow::ensure!(
                    !buffer.trim().is_empty(),
                    "empty diff on stdin. Pipe one in, as with \
                     `git diff | showme diff`, or pass a patch file"
                );
                buffer
            } else {
                read_text(&patch)?
            };

            let files = showme::diff::parse(&source, render::theme(&config.theme))?;
            let added: usize = files.iter().map(|f| f.added).sum();
            let removed: usize = files.iter().map(|f| f.removed).sum();
            let title = show.title.clone().unwrap_or_else(|| match files.as_slice() {
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
                rendered.push(render::markdown(&label(path), &source, &config.theme));
            }
            let details = vec![summarize(files.len(), "doc")];
            let title = show
                .title
                .clone()
                .unwrap_or_else(|| default_title(&files, "docs"));
            show_content(&config, title, show, Content::Docs { files: rendered }, details).await
        }

        Command::Dir { root, entry, show } => {
            let root = root
                .canonicalize()
                .with_context(|| format!("{} does not exist", root.display()))?;
            anyhow::ensure!(root.is_dir(), "{} is not a directory", root.display());
            if !root.join(&entry).exists() {
                eprintln!(
                    "showme: warning: {} has no {entry}; the page may be blank",
                    root.display()
                );
            }
            let title = show
                .title
                .clone()
                .unwrap_or_else(|| format!("{}", root.file_name().unwrap_or_default().display()));
            let details = vec![root.display().to_string()];
            show_content(&config, title, show, Content::Static { root, entry }, details).await
        }

        Command::Demo { port, path, show } => {
            // A demo the agent has not actually started is the most likely
            // mistake here, so check before sending a link to a dead port.
            anyhow::ensure!(
                !ports::is_free(config.bind, port),
                "nothing is listening on port {port}. Start the server first, \
                 then run `showme demo {port}`"
            );
            let title = show
                .title
                .clone()
                .unwrap_or_else(|| format!("live demo on port {port}"));
            let details = vec![format!("port {port}")];
            show_content(&config, title, show, Content::Demo { port, path }, details).await
        }

        Command::Wait { id, timeout } => {
            let client = Client::new(&config);
            match client.wait(&id, Duration::from_secs(timeout)).await? {
                Some(reply) => {
                    println!("{}", serde_json::to_string_pretty(&reply)?);
                    Ok(())
                }
                None => {
                    eprintln!(
                        "showme: no reply within {timeout}s. The session stays open — \
                         check later with `showme inbox`, or `showme collect {id}`."
                    );
                    std::process::exit(2);
                }
            }
        }

        Command::Inbox { all } => {
            let items = Client::new(&config).inbox(!all).await?;
            // JSON, because the caller is an agent: one array to parse rather
            // than a table to scrape.
            println!("{}", serde_json::to_string_pretty(&items)?);
            if items.is_empty() {
                eprintln!(
                    "showme: no {}replies waiting",
                    if all { "" } else { "unread " }
                );
            }
            Ok(())
        }

        Command::Collect { id } => {
            let client = Client::new(&config);
            let reply = client.collect(&id).await?;
            let comments = client.comments(&id).await?;
            let out = serde_json::json!({
                "replied": reply.is_some(),
                "reply": reply,
                "comments": comments,
            });
            println!("{}", serde_json::to_string_pretty(&out)?);
            if reply.is_none() && comments.is_empty() {
                eprintln!("showme: session {id} has no reply yet");
                std::process::exit(2);
            }
            Ok(())
        }

        Command::List => {
            let client = Client::new(&config);
            let port = client.daemon_port().await.with_context(|| {
                format!(
                    "no showme daemon on ports {}-{}",
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
                    "unknown theme {theme:?}. See `showme themes`"
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
                    match showme::notify::Style::detect(url) {
                        showme::notify::Style::Trigger => println!(
                            "detected a Slack workflow trigger: sending \
                             {{\"{}\": \"…\"}}.\nIf the workflow declares a \
                             different variable, set it with \
                             `showme config --webhook-field <NAME>`.",
                            config.webhook_field
                        ),
                        showme::notify::Style::Blocks => {
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
                         # set one with: showme config --webhook-url <URL>"
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
    let id = client.create(title.clone(), show.note.clone(), content).await?;
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

    let webhook = (!show.no_notify)
        .then_some(config.webhook_url.as_deref())
        .flatten();
    let sent = notification
        .send(webhook, &config.webhook_field)
        .await?;

    // The URL always goes to stdout so the agent can quote it back to the user
    // even when a notification went out.
    println!("{url}");
    if !sent {
        eprintln!("showme: session {id} ready (no notification sent)");
    }

    if show.wait {
        eprintln!("showme: waiting up to {}s for a reply…", show.timeout);
        match client.wait(&id, Duration::from_secs(show.timeout)).await? {
            Some(reply) => println!("{}", serde_json::to_string_pretty(&reply)?),
            None => {
                // The wait gave up, but the session has not: the daemon holds
                // the reply for as long as the session lives, so say where to
                // find it rather than letting it look lost.
                eprintln!(
                    "showme: no reply within {}s. The session stays open — check \
                     later with `showme inbox`, or `showme collect {id}`.",
                    show.timeout
                );
                std::process::exit(2);
            }
        }
    }
    Ok(())
}

fn read_text(path: &PathBuf) -> Result<String> {
    let bytes =
        std::fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    String::from_utf8(bytes)
        .with_context(|| format!("{} is not valid UTF-8", path.display()))
}

/// The label shown for a file: its path as given, which is more useful than a
/// bare file name when several files share one.
fn label(path: &PathBuf) -> String {
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
}
