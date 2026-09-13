use serde::{Deserialize, Serialize};

/// Adapter operations from the design doc (section 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Operation {
    Discover,
    Observe,
    Preview,
    Start,
    Attach,
    Resume,
    Stop,
}

impl Operation {
    pub const ALL: [Operation; 7] = [
        Operation::Discover,
        Operation::Observe,
        Operation::Preview,
        Operation::Start,
        Operation::Attach,
        Operation::Resume,
        Operation::Stop,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Operation::Discover => "discover",
            Operation::Observe => "observe",
            Operation::Preview => "preview",
            Operation::Start => "start",
            Operation::Attach => "attach",
            Operation::Resume => "resume",
            Operation::Stop => "stop",
        }
    }
}

/// Whether the running provider version supports an operation.
/// Unsupported operations carry a reason so `doctor` and the UI can show it;
/// they are never reported as success.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "support", rename_all = "snake_case")]
pub enum Support {
    Supported,
    Unsupported { reason: String },
}

impl Support {
    pub fn unsupported(reason: impl Into<String>) -> Self {
        Support::Unsupported {
            reason: reason.into(),
        }
    }

    pub fn is_supported(&self) -> bool {
        matches!(self, Support::Supported)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub discover: Support,
    pub observe: Support,
    pub preview: Support,
    pub start: Support,
    pub attach: Support,
    pub resume: Support,
    pub stop: Support,
}

impl Capabilities {
    /// Every operation unsupported for the same reason (e.g. CLI missing).
    pub fn none(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self {
            discover: Support::unsupported(reason.clone()),
            observe: Support::unsupported(reason.clone()),
            preview: Support::unsupported(reason.clone()),
            start: Support::unsupported(reason.clone()),
            attach: Support::unsupported(reason.clone()),
            resume: Support::unsupported(reason.clone()),
            stop: Support::unsupported(reason),
        }
    }

    pub fn get(&self, op: Operation) -> &Support {
        match op {
            Operation::Discover => &self.discover,
            Operation::Observe => &self.observe,
            Operation::Preview => &self.preview,
            Operation::Start => &self.start,
            Operation::Attach => &self.attach,
            Operation::Resume => &self.resume,
            Operation::Stop => &self.stop,
        }
    }
}
