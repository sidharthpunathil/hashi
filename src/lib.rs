//! Hashi: a bridge that exposes local coding-agent CLIs (Claude Code, Codex and more) as an
//! authenticated HTTP API, with a live terminal dashboard.

pub mod access;
pub mod access_client;
pub mod accounts;
pub mod agent;
pub mod api;
pub mod diagnostics;
pub mod doctor;
pub mod events;
pub mod forward;
pub mod guard;
pub mod install;
pub mod llm;
pub mod maintenance;
pub mod mcp;
pub mod net;
pub mod projects;
pub mod queue;
pub mod remote;
pub mod server;
pub mod settings;
pub mod shutdown;
pub mod store;
pub mod tailscale;
pub mod tls;
pub mod token;
pub mod tui;
pub mod tunnel;
pub mod uninstall;
pub mod update;
pub mod usage;
pub mod util;
pub mod verified;
pub mod web;
