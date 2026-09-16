use serde::{Deserialize, Serialize};

/// Observed execution state. Comes from the provider; never set by the user.
///
/// Unknown values from newer providers deserialize to `Unknown` rather than
/// failing, so a stale cache cannot block the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeState {
    Starting,
    Running,
    WaitingApproval,
    WaitingInput,
    Idle,
    Stopped,
    Error,
    #[default]
    #[serde(other)]
    Unknown,
}

impl RuntimeState {
    pub const ALL: [RuntimeState; 8] = [
        RuntimeState::WaitingApproval,
        RuntimeState::WaitingInput,
        RuntimeState::Error,
        RuntimeState::Running,
        RuntimeState::Starting,
        RuntimeState::Idle,
        RuntimeState::Stopped,
        RuntimeState::Unknown,
    ];

    /// Display label from the design doc (section 5).
    pub fn label(self) -> &'static str {
        match self {
            RuntimeState::Starting => "Starting",
            RuntimeState::Running => "Running",
            RuntimeState::WaitingApproval => "Waiting approval",
            RuntimeState::WaitingInput => "Waiting input",
            RuntimeState::Idle => "Idle",
            RuntimeState::Stopped => "Stopped",
            RuntimeState::Error => "Error",
            RuntimeState::Unknown => "Unknown",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            RuntimeState::Starting => "starting",
            RuntimeState::Running => "running",
            RuntimeState::WaitingApproval => "waiting_approval",
            RuntimeState::WaitingInput => "waiting_input",
            RuntimeState::Idle => "idle",
            RuntimeState::Stopped => "stopped",
            RuntimeState::Error => "error",
            RuntimeState::Unknown => "unknown",
        }
    }

    /// Default list order: waiting first, then error, running, starting,
    /// idle, stopped, unknown. Both waiting states share one rank.
    pub fn sort_rank(self) -> u8 {
        match self {
            RuntimeState::WaitingApproval | RuntimeState::WaitingInput => 0,
            RuntimeState::Error => 1,
            RuntimeState::Running => 2,
            RuntimeState::Starting => 3,
            RuntimeState::Idle => 4,
            RuntimeState::Stopped => 5,
            RuntimeState::Unknown => 6,
        }
    }

    /// Whether the provider reports a live execution behind this state.
    /// Used by the hide dialog to decide whether "stop first" is offered.
    pub fn is_live(self) -> bool {
        matches!(
            self,
            RuntimeState::Starting
                | RuntimeState::Running
                | RuntimeState::WaitingApproval
                | RuntimeState::WaitingInput
                | RuntimeState::Idle
        )
    }
}

/// User-assigned classification. Only the user changes it; a finished turn,
/// a merged PR, or a stopped process never moves it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowState {
    #[default]
    Unclassified,
    InProgress,
    Review,
    Done,
}

impl WorkflowState {
    pub const ALL: [WorkflowState; 4] = [
        WorkflowState::InProgress,
        WorkflowState::Review,
        WorkflowState::Done,
        WorkflowState::Unclassified,
    ];

    pub fn label(self) -> &'static str {
        match self {
            WorkflowState::Unclassified => "Unclassified",
            WorkflowState::InProgress => "In progress",
            WorkflowState::Review => "Review",
            WorkflowState::Done => "Done",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            WorkflowState::Unclassified => "unclassified",
            WorkflowState::InProgress => "in_progress",
            WorkflowState::Review => "review",
            WorkflowState::Done => "done",
        }
    }

    pub fn next(self) -> Self {
        match self {
            WorkflowState::Unclassified => WorkflowState::InProgress,
            WorkflowState::InProgress => WorkflowState::Review,
            WorkflowState::Review => WorkflowState::Done,
            WorkflowState::Done => WorkflowState::Unclassified,
        }
    }

    pub fn sort_rank(self) -> u8 {
        match self {
            WorkflowState::InProgress => 0,
            WorkflowState::Review => 1,
            WorkflowState::Unclassified => 2,
            WorkflowState::Done => 3,
        }
    }
}

/// Which axis the list is grouped by (Tab toggles).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Grouping {
    #[default]
    Runtime,
    Workflow,
}

impl Grouping {
    pub fn toggle(self) -> Self {
        match self {
            Grouping::Runtime => Grouping::Workflow,
            Grouping::Workflow => Grouping::Runtime,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Grouping::Runtime => "runtime state",
            Grouping::Workflow => "work classification",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_runtime_value_deserializes_to_unknown() {
        let s: RuntimeState = serde_json::from_str("\"paused_by_newer_version\"").unwrap();
        assert_eq!(s, RuntimeState::Unknown);
        let s: RuntimeState = serde_json::from_str("\"waiting_approval\"").unwrap();
        assert_eq!(s, RuntimeState::WaitingApproval);
    }

    #[test]
    fn waiting_states_sort_first_and_unknown_last() {
        let mut v = RuntimeState::ALL.to_vec();
        v.reverse();
        v.sort_by_key(|s| s.sort_rank());
        assert_eq!(v.first().unwrap().sort_rank(), 0);
        assert_eq!(*v.last().unwrap(), RuntimeState::Unknown);
    }

    #[test]
    fn workflow_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&WorkflowState::InProgress).unwrap(),
            "\"in_progress\""
        );
        assert_eq!(
            serde_json::from_str::<WorkflowState>("\"review\"").unwrap(),
            WorkflowState::Review
        );
    }
}
