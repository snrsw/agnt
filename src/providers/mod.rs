//! Provider adapters. The core and the TUI only see the `Adapter` trait and
//! the shared `Session` model; tool-specific quirks stay inside each module.
//!
//! An operation an adapter cannot perform returns `Unsupported` with a reason,
//! never a fake success. What each provider supports, and what has actually
//! been validated, is recorded in `docs/p0-validation.md`.

pub mod claude;
pub mod codex;
pub mod process;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;

use crate::model::{Capabilities, Operation, Provider, Runtime, RuntimeHandle, Session, SessionKey};

/// One page of discovered sessions plus what the page covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovery {
    pub sessions: Vec<Session>,
    /// Human-readable statement of what was searched (e.g. "live background
    /// jobs only"), shown in the status bar so the user knows what is missing.
    pub scope_note: String,
    /// True when `sessions` is the full set for the scope; false when the
    /// provider paginated or truncated, in which case unseen cached rows are
    /// kept and marked stale instead of dropped.
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeUpdate {
    pub key: SessionKey,
    pub runtime: Runtime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartRequest {
    /// agnt-side ID recorded before launch so a timeout can be reconciled
    /// instead of retried into a duplicate session.
    pub operation_id: String,
    pub cwd: PathBuf,
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartResult {
    pub key: SessionKey,
    pub handle: Option<RuntimeHandle>,
}

/// How a provider starts work, which differs by tool and must not be hidden.
///
/// Claude starts a background session and hands back its identity. Codex has
/// no background start at all: it only opens an interactive session, so the
/// terminal has to be given to it and the new conversation is discovered
/// afterwards. Collapsing these two into one shape would mean inventing an
/// identity for a session that does not have one yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartOutcome {
    /// The session is running in the background under this identity.
    Started(StartResult),
    /// Run this plan with the terminal; the session appears on the next refresh.
    Interactive(AttachPlan),
}

/// A command to hand the terminal to. Executed by `terminal::handoff` with an
/// argument array — never a shell string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachPlan {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    /// Shown before hand-off: how to come back to the list, what keeps running.
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopResult {
    /// The provider confirmed the execution has ended.
    Confirmed,
    /// Stop was requested; termination must be observed separately.
    Requested { note: String },
}

#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("{provider}: {} is not supported: {reason}", operation.as_str())]
    Unsupported {
        provider: Provider,
        operation: Operation,
        reason: String,
    },
    #[error("`{binary}` not found in PATH")]
    CliMissing { binary: String },
    #[error("`{command}` failed ({status}): {stderr}")]
    CliFailed {
        command: String,
        status: String,
        stderr: String,
    },
    #[error("`{command}` timed out after {timeout_ms} ms")]
    Timeout { command: String, timeout_ms: u64 },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl AdapterError {
    pub fn is_unsupported(&self) -> bool {
        matches!(self, AdapterError::Unsupported { .. })
    }
}

#[async_trait]
pub trait Adapter: Send + Sync {
    fn provider(&self) -> Provider;

    /// Resolved CLI binary, if found.
    fn binary(&self) -> Option<&Path>;

    /// `<cli> --version`, first line.
    async fn version(&self) -> Result<String, AdapterError>;

    /// What this adapter can do with the installed version.
    async fn capabilities(&self) -> Capabilities;

    async fn discover(&self) -> Result<Discovery, AdapterError>;

    /// Refresh runtime state for known keys.
    async fn observe(&self, keys: &[SessionKey]) -> Result<Vec<RuntimeUpdate>, AdapterError>;

    /// Short content, capped at `max_chars`. `Ok(None)` when nothing is available.
    async fn preview(&self, key: &SessionKey, max_chars: usize) -> Result<Option<String>, AdapterError>;

    async fn start(&self, req: StartRequest) -> Result<StartOutcome, AdapterError>;

    /// Connect to a live execution. Distinct from `resume`.
    async fn attach(&self, key: &SessionKey) -> Result<AttachPlan, AdapterError>;

    /// Reopen a saved conversation. Does not imply anything was still running.
    async fn resume(&self, key: &SessionKey) -> Result<AttachPlan, AdapterError>;

    async fn stop(&self, key: &SessionKey) -> Result<StopResult, AdapterError>;
}

pub fn all() -> Vec<Arc<dyn Adapter>> {
    vec![
        Arc::new(claude::ClaudeAdapter::detect()),
        Arc::new(codex::CodexAdapter::detect()),
    ]
}

pub fn for_provider(provider: Provider) -> Arc<dyn Adapter> {
    match provider {
        Provider::Claude => Arc::new(claude::ClaudeAdapter::detect()),
        Provider::Codex => Arc::new(codex::CodexAdapter::detect()),
    }
}
