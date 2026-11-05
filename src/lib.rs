//! Hashi: a bridge that exposes local coding-agent CLIs (Claude Code, Codex and more) as an
//! authenticated HTTP API, with a live terminal dashboard.

pub mod accounts;
pub mod agent;
pub mod api;
pub mod diagnostics;
pub mod events;
pub mod guard;
pub mod install;
pub mod maintenance;
pub mod mcp;
pub mod net;
pub mod queue;
pub mod remote;
pub mod server;
pub mod shutdown;
pub mod store;
pub mod tailscale;
pub mod tls;
pub mod token;
pub mod tui;
pub mod tunnel;
pub mod uninstall;
pub mod update;
pub mod util;
pub mod web;
