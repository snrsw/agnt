//! agnt — one list for local Claude Code and Codex sessions.
//!
//! Module map (design doc §4):
//! - `cli`: clap parsing and non-interactive output
//! - `app`: event loop, actions, in-memory state
//! - `model`: Session, RuntimeState, WorkflowState, Link, Capability
//! - `ui`: list, inputs, detail, dialogs
//! - `providers::{claude,codex}`: discovery, launch, attach, observation
//! - `terminal`: terminal ownership hand-off and restoration
//! - `storage`: metadata, cache, locking, schema
//! - `git` / `github`: cwd git info and link enrichment

pub mod app;
pub mod cli;
pub mod git;
pub mod github;
pub mod model;
pub mod providers;
pub mod storage;
pub mod terminal;
pub mod ui;
