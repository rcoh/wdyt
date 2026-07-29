//! Sending the notification.
//!
//! Slack has two incoming shapes and they are not interchangeable:
//!
//! * **Incoming webhooks** (`hooks.slack.com/services/…`) take Block Kit:
//!   `{"text": …, "blocks": […]}`. Mattermost and most Slack-compatible
//!   endpoints accept this too.
//! * **Workflow triggers** (`hooks.slack.com/triggers/…`) take a flat map of
//!   the variables the workflow declares, e.g. `{"content": "…"}`. Sending
//!   Block Kit to one returns `{"ok":true}` and *then* fails inside the
//!   workflow with an input-validation error, so the shape has to be right up
//!   front rather than inferred from the response.

use anyhow::{Context, Result};
use serde_json::json;

/// Which payload shape an endpoint expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// Block Kit, for a classic incoming webhook.
    Blocks,
    /// A flat variable map, for a workflow trigger.
    Trigger,
}

impl Style {
    /// Infers the shape from the URL, which is the only signal available before
    /// sending: a trigger accepts a wrong payload with `{"ok":true}` and only
    /// then reports failure inside the workflow.
    pub fn detect(url: &str) -> Self {
        if url.contains("/triggers/") {
            Self::Trigger
        } else {
            Self::Blocks
        }
    }
}

pub struct Notification {
    pub title: String,
    pub note: Option<String>,
    pub url: String,
    /// Shown as a small label: "code", "docs", "demo", "static".
    pub kind: &'static str,
    /// Extra context lines, e.g. the files included.
    pub details: Vec<String>,
}

impl Notification {
    /// The message as plain text, for endpoints that render no markup.
    fn plain(&self) -> String {
        let mut text = self.title.clone();
        if let Some(note) = &self.note {
            text.push_str(" — ");
            text.push_str(note);
        }
        // The link is the point of the message, so it goes on its own line
        // where Slack will always autolink it.
        text.push('\n');
        text.push_str(&self.url);

        let mut context = vec![self.kind.to_owned()];
        context.extend(self.details.iter().cloned());
        text.push('\n');
        text.push_str(&context.join(" · "));
        text
    }

    /// Builds the payload for `style`.
    ///
    /// `field` names the workflow variable to fill for a trigger; it is ignored
    /// for Block Kit.
    fn payload_for(&self, style: Style, field: &str) -> serde_json::Value {
        match style {
            // A trigger's variables are typed as plain text by the workflow, so
            // no markup and no escaping: whatever is sent is shown verbatim.
            Style::Trigger => json!({ field: self.plain() }),
            Style::Blocks => self.blocks_payload(),
        }
    }

    /// Builds the Block Kit payload.
    fn blocks_payload(&self) -> serde_json::Value {
        let mut blocks = vec![json!({
            "type": "section",
            "text": {
                "type": "mrkdwn",
                // The URL is the point of the message, so it leads.
                "text": format!("*<{}|{}>*", self.url, escape(&self.title)),
            }
        })];

        if let Some(note) = &self.note {
            blocks.push(json!({
                "type": "section",
                "text": { "type": "mrkdwn", "text": escape(note) }
            }));
        }

        let mut context = vec![format!("`{}`", self.kind)];
        context.extend(self.details.iter().map(|d| escape(d)));
        blocks.push(json!({
            "type": "context",
            "elements": [{ "type": "mrkdwn", "text": context.join("  ·  ") }]
        }));

        json!({
            // `text` is the notification/fallback string shown in the sidebar
            // and on mobile, where blocks are not rendered.
            "text": format!("{} — {}", self.title, self.url),
            "blocks": blocks,
        })
    }

    /// POSTs the notification. Returns `Ok(false)` when no webhook is
    /// configured, so callers can fall back to printing the link.
    ///
    /// `field` is the workflow variable filled when the endpoint is a trigger.
    pub async fn send(&self, webhook_url: Option<&str>, field: &str) -> Result<bool> {
        let Some(url) = webhook_url else {
            return Ok(false);
        };
        let payload = self.payload_for(Style::detect(url), field);

        let response = reqwest::Client::new()
            .post(url)
            .json(&payload)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .context("posting to the webhook failed")?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("webhook returned {status}: {}", body.trim());
        }
        Ok(true)
    }
}

/// Escapes the three characters Slack treats as markup control characters.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Notification {
        Notification {
            title: "New <flamegraph> UI".to_owned(),
            note: Some("a & b".to_owned()),
            url: "http://localhost:3000/s/abc".to_owned(),
            kind: "demo",
            details: vec!["2 files".to_owned()],
        }
    }

    #[test]
    fn trigger_urls_get_a_flat_variable_map() {
        // A trigger declares typed variables; Block Kit sent to one is accepted
        // with `{"ok":true}` and then fails inside the workflow.
        assert_eq!(
            Style::detect("https://hooks.slack.com/triggers/E01/994/1d8"),
            Style::Trigger
        );
        assert_eq!(
            Style::detect("https://hooks.slack.com/services/T00/B00/xxx"),
            Style::Blocks
        );

        let payload = sample().payload_for(Style::Trigger, "content");
        let content = payload["content"].as_str().expect("content is a string");
        assert!(content.contains("http://localhost:3000/s/abc"), "{content}");
        assert!(content.contains("New <flamegraph> UI"), "{content}");
        assert!(content.contains("a & b"), "{content}");
        assert!(content.contains("demo"), "{content}");
        // No Block Kit and no entity escaping: the variable renders verbatim.
        assert!(payload.get("blocks").is_none(), "{payload}");
        assert!(
            !content.contains("&amp;"),
            "escaped a plain variable: {content}"
        );
    }

    #[test]
    fn the_trigger_variable_name_is_configurable() {
        let payload = sample().payload_for(Style::Trigger, "message");
        assert!(payload["message"].is_string(), "{payload}");
        assert!(payload.get("content").is_none(), "{payload}");
    }

    #[test]
    fn payload_links_the_url_and_escapes_markup() {
        let payload = sample().blocks_payload();
        let text = payload["blocks"][0]["text"]["text"].as_str().unwrap();
        assert!(text.contains("http://localhost:3000/s/abc"));
        assert!(text.contains("&lt;flamegraph&gt;"), "not escaped: {text}");
        assert_eq!(payload["blocks"][1]["text"]["text"], "a &amp; b");
    }

    #[test]
    fn payload_has_fallback_text_with_url() {
        let payload = sample().blocks_payload();
        let fallback = payload["text"].as_str().unwrap();
        assert!(fallback.contains("http://localhost:3000/s/abc"));
    }

    #[test]
    fn context_block_carries_kind_and_details() {
        let payload = sample().blocks_payload();
        let context = payload["blocks"][2]["elements"][0]["text"]
            .as_str()
            .unwrap();
        assert!(context.contains("demo"));
        assert!(context.contains("2 files"));
    }

    #[test]
    fn note_block_is_omitted_when_absent() {
        let mut notification = sample();
        notification.note = None;
        let payload = notification.blocks_payload();
        // Title, then context: no note block in between.
        assert_eq!(payload["blocks"].as_array().unwrap().len(), 2);
        assert_eq!(payload["blocks"][1]["type"], "context");
    }

    #[tokio::test]
    async fn send_without_webhook_is_a_no_op() {
        assert!(!sample().send(None, "content").await.unwrap());
    }
}
