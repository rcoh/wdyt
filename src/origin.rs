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
/// Every field is optional: showme runs from plain shells, from CI, and from
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
    pub fn detect() -> Self {
        let mut origin = Self {
            cwd: std::env::current_dir()
                .ok()
                .map(|path| path.display().to_string()),
            ..Self::default()
        };
        // Only zellij is probed, because it is the one that can be asked. Other
        // multiplexers would each need their own query and none is in use here.
        if let Ok(session) = std::env::var("ZELLIJ_SESSION_NAME")
            && !session.is_empty()
        {
            origin.session = Some(session);
            let (tab, pane) = zellij_tab_and_pane();
            origin.tab = tab;
            origin.pane = pane;
        }
        origin
    }

    /// Whether there is anything worth showing.
    pub fn is_empty(&self) -> bool {
        self.cwd.is_none() && self.session.is_none() && self.tab.is_none()
    }

    /// A one-line form for a notification: `dd-2 › showme cli — /path`.
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
10  4  showme cli  terminal_28  terminal  Build showme CLI tool  claude  /local/home/rcoh/code  true  false  false  0  1  68  121
10  4  showme cli  terminal_29  terminal  a shell  -  /local/home/rcoh/code  false  false  false  122  1  68  121";

    #[test]
    fn a_pane_resolves_to_its_tab_and_title() {
        let (tab, pane) = parse_panes(TABLE, "28");
        assert_eq!(tab.as_deref(), Some("showme cli"));
        assert_eq!(pane.as_deref(), Some("Build showme CLI tool"));
    }

    #[test]
    fn a_tab_name_containing_a_space_survives() {
        // Tab names are prose — "showme cli", "get new ui deployed" — so the
        // table cannot be split on whitespace.
        let (tab, _) = parse_panes(TABLE, "29");
        assert_eq!(tab.as_deref(), Some("showme cli"));
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
            tab: Some("showme cli".to_owned()),
            pane: None,
        };
        assert_eq!(
            origin.summary().as_deref(),
            Some("dd-2 › showme cli — /local/home/rcoh/code")
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
