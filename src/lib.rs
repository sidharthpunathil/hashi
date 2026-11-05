//! Hashi: a bridge that exposes local Claude Code and Codex CLIs as an
//! authenticated HTTP API, with a live terminal dashboard.

pub mod agent;
pub mod api;
pub mod diagnostics;
pub mod events;
pub mod guard;
pub mod maintenance;
pub mod mcp;
pub mod queue;
pub mod server;
pub mod store;
pub mod tls;
pub mod token;
pub mod tui;
pub mod tunnel;
pub mod util;
pub mod web;
