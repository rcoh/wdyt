//! Where the agent that made a session is running.
//!
//! A notification arrives with no context about which of several agents sent it.
//! The working directory usually identifies the work, and under a multiplexer the
//! session and tab name say which pane to switch to — which is the actual next
//! action when a reply needs a conversation.

use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

/// The terminal context a session was created from.
///
/// Every field is optional: wdyt runs from plain shells, from CI, and from
/// multiplexers it knows nothing about. A missing field is shown as nothing
/// rather than guessed at.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    /// The directory the agent was invoked from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The multiplexer session, e.g. a zellij session name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The tab within that session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab: Option<String>,
    /// The pane's title, which under an agent is often the task it was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane: Option<String>,
}

impl Origin {
    /// Reads the current terminal context.
    ///
    /// Outside zellij this is just the working directory: the `ZELLIJ_SESSION_NAME`
    /// guard means a plain shell, tmux, or CI never shells out to a `zellij`
    /// binary that may not exist, and simply reports its cwd.
    pub fn detect() -> Self {
        let cwd = std::env::current_dir()
            .ok()
            .map(|path| path.display().to_string());
        let session = std::env::var("ZELLIJ_SESSION_NAME")
            .ok()
            .filter(|s| !s.is_empty());
        Self::build(cwd, session, zellij_tab_and_pane)
    }

    /// The assembly logic, with the two zellij lookups injected.
    ///
    /// Split out from [`Origin::detect`] so the not-under-zellij path can be
    /// tested without depending on the ambient environment: when there is no
    /// session, the tab/pane probe must not even run.
    fn build(
        cwd: Option<String>,
        session: Option<String>,
        probe: impl FnOnce() -> (Option<String>, Option<String>),
    ) -> Self {
        let mut origin = Self {
            cwd,
            ..Self::default()
        };
        // Only zellij is probed, because it is the one that can be asked. Other
        // multiplexers would each need their own query and none is in use here.
        if let Some(session) = session {
            origin.session = Some(session);
            let (tab, pane) = probe();
            origin.tab = tab;
            origin.pane = pane;
        }
        origin
    }

    /// Whether there is anything worth showing.
    pub fn is_empty(&self) -> bool {
        self.cwd.is_none() && self.session.is_none() && self.tab.is_none()
    }

    /// A one-line form for a notification: `dd-2 › wdyt cli — /path`.
    pub fn summary(&self) -> Option<String> {
        let mut parts = Vec::new();
        match (&self.session, &self.tab) {
            (Some(session), Some(tab)) => parts.push(format!("{session} › {tab}")),
            (Some(session), None) => parts.push(session.clone()),
            (None, Some(tab)) => parts.push(tab.clone()),
            (None, None) => {}
        }
        if let Some(cwd) = &self.cwd {
            parts.push(cwd.clone());
        }
        (!parts.is_empty()).then(|| parts.join(" — "))
    }
}

/// Asks zellij which tab and pane this process is in.
///
/// `list-panes --all` is the only view that maps a pane id to its tab name, and
/// `$ZELLIJ_PANE_ID` is the id to look for. A failure here is not an error worth
/// reporting: it costs a line of context on a page, not the session.
///
/// Deliberately not `zellij action current-tab-info`, which reports the tab the
/// *user is currently looking at* — it changes as they switch tabs and names the
/// wrong one whenever an agent is running in the background, which is the normal
/// case here. The pane id is fixed for the life of the process.
fn zellij_tab_and_pane() -> (Option<String>, Option<String>) {
    let Ok(pane_id) = std::env::var("ZELLIJ_PANE_ID") else {
        return (None, None);
    };
    let Some(table) = run("zellij", &["action", "list-panes", "--all"]) else {
        return (None, None);
    };
    parse_panes(&table, &pane_id)
}

/// Finds a pane's tab and title in `zellij action list-panes --all` output.
///
/// The table is column-aligned with two-space separators and the fields can
/// themselves contain single spaces — tab names usually do — so it is split on
/// the double space rather than on whitespace.
fn parse_panes(table: &str, pane_id: &str) -> (Option<String>, Option<String>) {
    // The id in the table carries a type prefix the environment variable lacks.
    let wanted = format!("terminal_{pane_id}");
    let mut lines = table.lines();
    let header = lines.next().unwrap_or_default();
    let column = |name: &str| header.split("  ").position(|field| field.trim() == name);

    let (Some(tab_at), Some(id_at)) = (column("TAB_NAME"), column("PANE_ID")) else {
        return (None, None);
    };
    let title_at = column("TITLE");

    for line in lines {
        let fields: Vec<&str> = line.split("  ").collect();
        if fields.get(id_at).map(|id| id.trim()) != Some(wanted.as_str()) {
            continue;
        }
        let at = |index: Option<usize>| {
            index
                .and_then(|i| fields.get(i))
                .map(|value| value.trim())
                .filter(|value| !value.is_empty() && *value != "-")
                .map(str::to_owned)
        };
        return (at(Some(tab_at)), at(title_at));
    }
    (None, None)
}

/// Runs a command, returning its stdout if it succeeded.
///
/// Timeouts are not available on `Command`, so this relies on the queries being
/// local and fast. A hung multiplexer would block session creation, which is why
/// only cheap read-only actions are ever passed here.
fn run(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real `zellij action list-panes --all` output, trimmed to three rows.
    const TABLE: &str = "\
TAB_ID  TAB_POS  TAB_NAME  PANE_ID  TYPE  TITLE  COMMAND  CWD  FOCUSED  FLOATING  EXITED  X  Y  ROWS  COLS
1  0  get new ui deployed  plugin_2  plugin  zellij:tab-bar  zellij:tab-bar  -  false  false  false  0  0  1  243
10  4  wdyt cli  terminal_28  terminal  Build wdyt CLI tool  claude  /local/home/rcoh/code  true  false  false  0  1  68  121
10  4  wdyt cli  terminal_29  terminal  a shell  -  /local/home/rcoh/code  false  false  false  122  1  68  121";

    /// A probe that fails the test if it is ever called.
    fn never_probed() -> (Option<String>, Option<String>) {
        panic!("zellij was queried when no session was set");
    }

    #[test]
    fn outside_zellij_it_reports_only_the_directory() {
        // A plain shell, tmux, or CI: no session, so the zellij binary must not
        // be invoked at all, and the origin is just the cwd.
        let origin = Origin::build(Some("/work".to_owned()), None, never_probed);
        assert_eq!(origin.cwd.as_deref(), Some("/work"));
        assert_eq!(origin.session, None);
        assert_eq!(origin.tab, None);
        assert_eq!(origin.pane, None);
        assert!(!origin.is_empty());
    }

    #[test]
    fn with_nothing_at_all_it_is_empty() {
        let origin = Origin::build(None, None, never_probed);
        assert!(origin.is_empty());
        assert_eq!(origin.summary(), None);
    }

    #[test]
    fn under_zellij_it_probes_for_tab_and_pane() {
        let origin = Origin::build(Some("/work".to_owned()), Some("dd-2".to_owned()), || {
            (Some("wdyt cli".to_owned()), Some("a task".to_owned()))
        });
        assert_eq!(origin.session.as_deref(), Some("dd-2"));
        assert_eq!(origin.tab.as_deref(), Some("wdyt cli"));
        assert_eq!(origin.pane.as_deref(), Some("a task"));
    }

    #[test]
    fn a_pane_resolves_to_its_tab_and_title() {
        let (tab, pane) = parse_panes(TABLE, "28");
        assert_eq!(tab.as_deref(), Some("wdyt cli"));
        assert_eq!(pane.as_deref(), Some("Build wdyt CLI tool"));
    }

    #[test]
    fn a_tab_name_containing_a_space_survives() {
        // Tab names are prose — "wdyt cli", "get new ui deployed" — so the
        // table cannot be split on whitespace.
        let (tab, _) = parse_panes(TABLE, "29");
        assert_eq!(tab.as_deref(), Some("wdyt cli"));
    }

    #[test]
    fn an_unknown_pane_yields_nothing_rather_than_a_wrong_tab() {
        assert_eq!(parse_panes(TABLE, "999"), (None, None));
    }

    #[test]
    fn garbage_input_is_not_a_panic() {
        assert_eq!(parse_panes("", "28"), (None, None));
        assert_eq!(parse_panes("no columns here", "28"), (None, None));
    }

    #[test]
    fn the_summary_reads_as_a_place() {
        let origin = Origin {
            cwd: Some("/local/home/rcoh/code".to_owned()),
            session: Some("dd-2".to_owned()),
            tab: Some("wdyt cli".to_owned()),
            pane: None,
        };
        assert_eq!(
            origin.summary().as_deref(),
            Some("dd-2 › wdyt cli — /local/home/rcoh/code")
        );
    }

    #[test]
    fn a_bare_shell_still_reports_its_directory() {
        let origin = Origin {
            cwd: Some("/tmp".to_owned()),
            ..Origin::default()
        };
        assert!(!origin.is_empty());
        assert_eq!(origin.summary().as_deref(), Some("/tmp"));
    }

    #[test]
    fn nothing_known_is_empty() {
        let origin = Origin::default();
        assert!(origin.is_empty());
        assert_eq!(origin.summary(), None);
    }
}
