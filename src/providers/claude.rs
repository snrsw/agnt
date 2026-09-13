//! Claude Code adapter.
//!
//! Planned source of truth (design doc §6): the official agent-view JSON list
//! for live/background sessions, with the short job ID kept as the runtime
//! handle and the conversation UUID as the identity. Which fields that list
//! carries, and how past non-background sessions are enumerated, are P0
//! questions (Q4) — until they are recorded, every operation is unsupported.

use std::path::{Path, PathBuf};

use async_trait::async_trait;

use super::process::{find_in_path, version_of};
use super::{
    Adapter, AdapterError, AttachPlan, Discovery, RuntimeUpdate, StartRequest, StartResult, StopResult,
};
use crate::model::{Capabilities, Operation, Provider, SessionKey};

pub const BINARY: &str = "claude";
pub const P0_REASON: &str = "P0 not validated: record the agent-view JSON list format and attach/detach behaviour in docs/p0-validation.md first";

#[derive(Debug, Clone)]
pub struct ClaudeAdapter {
    binary: Option<PathBuf>,
}

impl ClaudeAdapter {
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
            provider: Provider::Claude,
            operation,
            reason: self.unsupported_reason(),
        }
    }
}

#[async_trait]
impl Adapter for ClaudeAdapter {
    fn provider(&self) -> Provider {
        Provider::Claude
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
    async fn missing_binary_is_reported_per_operation() {
        let a = ClaudeAdapter { binary: None };
        let caps = a.capabilities().await;
        for op in Operation::ALL {
            assert!(!caps.get(op).is_supported(), "{op:?}");
        }
        let err = a.discover().await.unwrap_err();
        assert!(err.is_unsupported());
        assert!(err.to_string().contains("not found in PATH"));
        assert!(matches!(a.version().await, Err(AdapterError::CliMissing { .. })));
    }

    #[tokio::test]
    async fn present_binary_still_unsupported_until_p0() {
        let a = ClaudeAdapter::with_binary("/usr/bin/true");
        let err = a
            .start(StartRequest {
                operation_id: "op".into(),
                cwd: "/tmp".into(),
                prompt: None,
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("P0 not validated"));
    }
}
