//! Codex adapter.
//!
//! Planned source of truth (design doc §6): the local Codex App Server for
//! saved threads and runtime notifications. A `notLoaded` answer from one
//! server never proves a thread is dead elsewhere, so externally started
//! threads stay `Unknown`. Whether the standard CLI can detach from a running
//! turn without losing pending approvals is P0 question Q1 — until it is
//! recorded, every operation is unsupported.

use std::path::{Path, PathBuf};

use async_trait::async_trait;

use super::process::{find_in_path, version_of};
use super::{
    Adapter, AdapterError, AttachPlan, Discovery, RuntimeUpdate, StartRequest, StartResult, StopResult,
};
use crate::model::{Capabilities, Operation, Provider, SessionKey};

pub const BINARY: &str = "codex";
pub const P0_REASON: &str = "P0 not validated: record App Server thread listing, notifications, and standard-CLI detach behaviour in docs/p0-validation.md first";

#[derive(Debug, Clone)]
pub struct CodexAdapter {
    binary: Option<PathBuf>,
}

impl CodexAdapter {
    pub fn detect() -> Self {
        Self {
            binary: find_in_path(BINARY),
        }
    }

    pub fn with_binary(path: impl Into<PathBuf>) -> Self {
        Self {
            binary: Some(path.into()),
        }
    }

    fn unsupported_reason(&self) -> String {
        match &self.binary {
            None => format!("`{BINARY}` not found in PATH"),
            Some(_) => P0_REASON.to_string(),
        }
    }

    fn unsupported(&self, operation: Operation) -> AdapterError {
        AdapterError::Unsupported {
            provider: Provider::Codex,
            operation,
            reason: self.unsupported_reason(),
        }
    }
}

#[async_trait]
impl Adapter for CodexAdapter {
    fn provider(&self) -> Provider {
        Provider::Codex
    }

    fn binary(&self) -> Option<&Path> {
        self.binary.as_deref()
    }

    async fn version(&self) -> Result<String, AdapterError> {
        version_of(self.binary.as_deref(), BINARY).await
    }

    async fn capabilities(&self) -> Capabilities {
        Capabilities::none(self.unsupported_reason())
    }

    async fn discover(&self) -> Result<Discovery, AdapterError> {
        Err(self.unsupported(Operation::Discover))
    }

    async fn observe(&self, _keys: &[SessionKey]) -> Result<Vec<RuntimeUpdate>, AdapterError> {
        Err(self.unsupported(Operation::Observe))
    }

    async fn preview(&self, _key: &SessionKey, _max_chars: usize) -> Result<Option<String>, AdapterError> {
        Err(self.unsupported(Operation::Preview))
    }

    async fn start(&self, _req: StartRequest) -> Result<StartResult, AdapterError> {
        Err(self.unsupported(Operation::Start))
    }

    async fn attach(&self, _key: &SessionKey) -> Result<AttachPlan, AdapterError> {
        Err(self.unsupported(Operation::Attach))
    }

    async fn resume(&self, _key: &SessionKey) -> Result<AttachPlan, AdapterError> {
        Err(self.unsupported(Operation::Resume))
    }

    async fn stop(&self, _key: &SessionKey) -> Result<StopResult, AdapterError> {
        Err(self.unsupported(Operation::Stop))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stop_is_never_silently_successful() {
        let a = CodexAdapter::with_binary("/usr/bin/true");
        let err = a.stop(&SessionKey::new(Provider::Codex, "t")).await.unwrap_err();
        assert!(matches!(
            err,
            AdapterError::Unsupported {
                operation: Operation::Stop,
                ..
            }
        ));
    }
}
