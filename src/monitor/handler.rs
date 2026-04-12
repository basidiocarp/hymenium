use crate::retry::{decide_recovery, RecoveryAction, RetryPolicy};
use crate::workflow::engine::WorkflowInstance;

use super::{MonitorError, ProgressSignal};

/// React to a progress signal by updating workflow state and deciding on
/// recovery when needed.
///
/// Returns the [`RecoveryAction`] that the caller should execute. For healthy
/// or already-complete signals the action is `Cancel` (no recovery needed).
pub fn handle_signal(
    signal: &ProgressSignal,
    workflow: &mut WorkflowInstance,
    retry_count: u32,
    policy: &RetryPolicy,
) -> Result<RecoveryAction, MonitorError> {
    match signal {
        ProgressSignal::PhaseComplete { .. } => {
            workflow.complete_phase().map_err(|e| {
                MonitorError::InvalidState(format!("failed to complete phase: {e}"))
            })?;
            Ok(RecoveryAction::Cancel {
                reason: "phase completed".to_string(),
            })
        }
        ProgressSignal::GateSatisfied { .. } => Ok(RecoveryAction::Cancel {
            reason: "gate noted".to_string(),
        }),
        ProgressSignal::Stalled { .. } | ProgressSignal::Failed { .. } => {
            Ok(decide_recovery(signal, retry_count, policy))
        }
        ProgressSignal::Healthy { .. } => Ok(RecoveryAction::Cancel {
            reason: "no action needed".to_string(),
        }),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::engine::PhaseStatus;
    use crate::workflow::template::impl_audit_default;
    use crate::workflow::WorkflowId;
    use chrono::Utc;

    use super::super::test_helpers::make_workflow;
    use super::super::StallReason;

    // -- handle_signal tests --------------------------------------------------

    #[test]
    fn handle_phase_complete_completes_phase() {
        let mut wf = make_workflow();
        let signal = ProgressSignal::PhaseComplete {
            phase_id: "implement".to_string(),
        };
        let policy = RetryPolicy::default();

        let action = handle_signal(&signal, &mut wf, 0, &policy)
            .expect("should succeed");
        assert!(matches!(action, RecoveryAction::Cancel { .. }));
        assert_eq!(wf.phase_states[0].status, PhaseStatus::Completed);
    }

    #[test]
    fn handle_stalled_returns_retry() {
        let mut wf = make_workflow();
        let signal = ProgressSignal::Stalled {
            phase_id: "implement".to_string(),
            since: Utc::now(),
            reason: StallReason::HeartbeatTimeout,
        };
        let policy = RetryPolicy::default();

        let action = handle_signal(&signal, &mut wf, 0, &policy)
            .expect("should succeed");
        assert!(matches!(action, RecoveryAction::Retry { .. }));
    }

    #[test]
    fn handle_healthy_returns_cancel() {
        let mut wf = make_workflow();
        let signal = ProgressSignal::Healthy {
            phase_id: "implement".to_string(),
            last_activity: Utc::now(),
        };
        let policy = RetryPolicy::default();

        let action = handle_signal(&signal, &mut wf, 0, &policy)
            .expect("should succeed");
        assert!(matches!(action, RecoveryAction::Cancel { .. }));
    }

    // -- handle_signal on non-Active phase ----------------------------------

    #[test]
    fn handle_phase_complete_on_non_active_phase_errors() {
        let template = impl_audit_default();
        let mut wf = WorkflowInstance::new(
            WorkflowId("test-non-active".to_string()),
            template,
            "/test/handoff.md",
        );
        // Phase 0 is Pending by default — not Active
        let signal = ProgressSignal::PhaseComplete {
            phase_id: "implement".to_string(),
        };
        let policy = RetryPolicy::default();

        let result = handle_signal(&signal, &mut wf, 0, &policy);
        assert!(result.is_err());
        assert!(matches!(result, Err(MonitorError::InvalidState(_))));
    }
}
