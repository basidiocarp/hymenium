//! Progress monitoring and stall detection.
//!
//! Monitors workflow progress by polling canopy task state and evaluating
//! completeness gates. Detects stalled phases via heartbeat and progress
//! timeouts, and delegates recovery decisions to [`crate::retry`].

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use thiserror::Error;

pub mod progress;
pub mod handler;

pub use progress::{check_progress, is_stalled};
pub use handler::handle_signal;

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

/// Error type for monitoring operations.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MonitorError {
    #[error("canopy error: {0}")]
    CanopyError(String),

    #[error("invalid state: {0}")]
    InvalidState(String),

    #[error("phase not active: {0}")]
    PhaseNotActive(String),
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Configuration for progress monitoring thresholds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorConfig {
    /// How long to wait for a heartbeat (task status change from pending/assigned)
    /// before declaring a stall.
    #[serde(with = "duration_secs")]
    pub heartbeat_timeout: Duration,

    /// How long to wait for meaningful progress (completeness items) before
    /// declaring a stall.
    #[serde(with = "duration_secs")]
    pub progress_timeout: Duration,

    /// How often to poll canopy for completeness updates.
    #[serde(with = "duration_secs")]
    pub completeness_check_interval: Duration,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            heartbeat_timeout: Duration::from_secs(5 * 60),
            progress_timeout: Duration::from_secs(30 * 60),
            completeness_check_interval: Duration::from_secs(2 * 60),
        }
    }
}

/// Serde helper for `Duration` as whole seconds.
pub mod duration_secs {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        d.as_secs().serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        let secs = u64::deserialize(d)?;
        Ok(Duration::from_secs(secs))
    }
}

// ---------------------------------------------------------------------------
// Signals
// ---------------------------------------------------------------------------

/// A signal emitted by the progress monitor after evaluating workflow health.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ProgressSignal {
    /// The phase is making progress normally.
    Healthy {
        phase_id: String,
        last_activity: DateTime<Utc>,
    },

    /// The phase appears stalled.
    Stalled {
        phase_id: String,
        since: DateTime<Utc>,
        reason: StallReason,
    },

    /// The phase has been completed (canopy task done or completeness satisfied).
    PhaseComplete { phase_id: String },

    /// An exit gate condition has been satisfied.
    GateSatisfied { gate: String },

    /// The phase has failed in canopy.
    Failed { phase_id: String, error: String },
}

/// Reason a phase is considered stalled.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum StallReason {
    /// No heartbeat received within the configured timeout.
    HeartbeatTimeout,

    /// No code diff detected after the progress timeout.
    NoCodeDiff,

    /// Some checklist items completed but progress has stopped.
    NoPasteMarkerProgress,

    /// Agent is active but only producing status chatter, not real work.
    ///
    /// This variant is **not** emitted by [`check_progress`] — detecting chatter
    /// requires semantic analysis of agent output that canopy task state alone
    /// cannot provide. External callers (e.g., cortina signal analysis) may
    /// construct a `Stalled { reason: StatusChatterOnly, .. }` signal and pass
    /// it directly to [`crate::retry::decide_recovery`].
    StatusChatterOnly,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(super) mod test_helpers {
    use super::*;
    use crate::dispatch::{CompletenessReport, DispatchError, TaskDetail};
    use crate::workflow::engine::PhaseStatus;
    use crate::workflow::template::impl_audit_default;
    use crate::workflow::WorkflowId;
    use std::cell::RefCell;

    /// A focused test mock that lets each test control exactly what canopy
    /// returns without needing to manipulate shared state.
    pub struct TestCanopyClient {
        pub task: RefCell<Option<TaskDetail>>,
        pub completeness: CompletenessReport,
    }

    impl TestCanopyClient {
        pub fn new(task: TaskDetail, completeness: CompletenessReport) -> Self {
            Self {
                task: RefCell::new(Some(task)),
                completeness,
            }
        }
    }

    impl crate::dispatch::CanopyClient for TestCanopyClient {
        fn create_task(
            &self,
            _title: &str,
            _description: &str,
            _project_root: &str,
            _options: &crate::dispatch::TaskOptions,
        ) -> Result<String, DispatchError> {
            Ok("unused".to_string())
        }

        fn create_subtask(
            &self,
            _parent_id: &str,
            _title: &str,
            _description: &str,
            _options: &crate::dispatch::TaskOptions,
        ) -> Result<String, DispatchError> {
            Ok("unused".to_string())
        }

        fn assign_task(&self, _task_id: &str, _agent_id: &str) -> Result<(), DispatchError> {
            Ok(())
        }

        fn get_task(&self, _task_id: &str) -> Result<TaskDetail, DispatchError> {
            self.task
                .borrow()
                .clone()
                .ok_or_else(|| DispatchError::InvalidState("no task configured".to_string()))
        }

        fn check_completeness(
            &self,
            _handoff_path: &str,
        ) -> Result<CompletenessReport, DispatchError> {
            Ok(self.completeness.clone())
        }

        fn import_handoff(
            &self,
            _path: &str,
            _assign_to: Option<&str>,
        ) -> Result<crate::dispatch::ImportResult, DispatchError> {
            Ok(crate::dispatch::ImportResult {
                task_id: "unused".to_string(),
                subtask_ids: Vec::new(),
            })
        }
    }

    pub fn make_workflow() -> crate::workflow::engine::WorkflowInstance {
        let template = impl_audit_default();
        let mut wf = crate::workflow::engine::WorkflowInstance::new(
            WorkflowId("test-monitor".to_string()),
            template,
            "/test/handoff.md",
        );
        // Set up phase 0 as Active with a known canopy task ID.
        wf.phase_states[0].status = PhaseStatus::Active;
        wf.phase_states[0].canopy_task_id = Some("task-1".to_string());
        wf.phase_states[0].started_at = Some(Utc::now());
        wf
    }

    pub fn task_with_status(status: &str) -> TaskDetail {
        TaskDetail {
            task_id: "task-1".to_string(),
            title: "test".to_string(),
            status: status.to_string(),
            agent_id: Some("agent-1".to_string()),
            parent_id: None,
        }
    }

    pub fn incomplete_report(completed: usize, total: usize) -> CompletenessReport {
        CompletenessReport {
            complete: false,
            total_items: total,
            completed_items: completed,
            missing: vec!["item".to_string()],
        }
    }

    pub fn complete_report() -> CompletenessReport {
        CompletenessReport {
            complete: true,
            total_items: 3,
            completed_items: 3,
            missing: Vec::new(),
        }
    }
}

#[cfg(test)]
mod config_tests {
    use super::*;

    #[test]
    fn default_config_has_expected_timeouts() {
        let config = MonitorConfig::default();
        assert_eq!(config.heartbeat_timeout, Duration::from_secs(300));
        assert_eq!(config.progress_timeout, Duration::from_secs(1800));
        assert_eq!(config.completeness_check_interval, Duration::from_secs(120));
    }
}
