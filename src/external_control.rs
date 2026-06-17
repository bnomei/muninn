//! External automation surfaces for triggering recording without a human hotkey.
//!
//! Two transports converge on the same [`ExternalControlAction`] vocabulary:
//! a macOS `muninn://` custom URL scheme and a localhost streamable-HTTP MCP
//! server. Both forward actions into the tao event loop as
//! [`UserEvent::ExternalControl`], which the runtime worker maps to the same
//! [`AppEvent`] transitions used by the tray and hotkeys.

mod action;
mod mcp;
mod status;
#[cfg(target_os = "macos")]
mod url_scheme;

pub(crate) use action::{parse_url_action, ExternalControlAction, ExternalControlOutcome};
pub(crate) use mcp::spawn_mcp_server;
pub(crate) use status::RuntimeStatusHandle;
#[cfg(target_os = "macos")]
pub(crate) use url_scheme::install_url_scheme_handler;
