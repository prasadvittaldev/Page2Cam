//! Agent control: the MCP server's protocol and tools, over a [`Backend`].
//!
//! The protocol is plain JSON-RPC 2.0 (`initialize`, `ping`, `tools/list`,
//! `tools/call`); transports (stdio bridge, HTTP) live in the tray and the
//! `page2cam` binary and only pass messages to [`Server::handle`].

mod protocol;
mod redact;
mod tools;

pub use protocol::{ClientInfo, Server, PROTOCOL_VERSION, SUPPORTED_VERSIONS};
pub use redact::redact;
pub use tools::{
    tool_defs, tool_names, Backend, BackendCall, BackendReply, Nav, MAX_HTML_BYTES, MAX_IMAGE_BYTES,
};

/// Error text when agents are switched off (the bridge answers with it too).
pub const AGENTS_OFF: &str = "Turn on 'Allow agents' in the Page2Cam tray menu";
/// Error text for engine tools while Page2Cam is stopped.
pub const STOPPED: &str = "Page2Cam is stopped — call start first";
