//! showme: let coding agents show their work.
//!
//! An agent creates a *session* holding something to look at (highlighted
//! source, rendered markdown, a static directory, or a live server it started),
//! showme sends the user a link over a webhook, and the page carries a reply
//! box so the user can send one note back.

pub mod client;
pub mod config;
pub mod diff;
pub mod notify;
pub mod zellij_origin;
pub mod ports;
pub mod render;
pub mod server;
pub mod session;
pub mod ui;
