use crate::retry::{decide_recovery, RecoveryAction, RetryPolicy};
use crate::store::WorkflowStore;
use crate::workflow::engine::WorkflowInstance;

use super::{MonitorError, ProgressSignal};

/// React to a progress signal by updating workflow state and deciding on
/// recovery when needed.
///
/// When a `Retry` action is decided, this function immediately increments the
/// retry counter and persists the updated phase state via the store. The caller
/// is responsible for executing the returned [`RecoveryAction`] (e.g., re-dispatch,
/// narrow scope, escalate tier, or notify the operator).
///
/// The retry count is read from the workflow's current phase state, ensuring
/// that each recovery decision uses the actual current retry count even in
/// successive retries.
///
/// Returns the [`RecoveryAction`] that the caller should execute. For healthy
/// or already-complete signals the action is `Cancel` (no recovery needed).
pub fn handle_signal(
    signal: &ProgressSignal,
    workflow: &mut WorkflowInstance,
    _retry_count: u32,
    policy: &RetryPolicy,
    store: &WorkflowStore,
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
            // Read the current retry count from the workflow's phase state.
            // This ensures recovery decisions use the actual count, not a stale value.
            let current_retry_count = workflow
                .current_phase()
                .map(|p| p.retry_count)
                .unwrap_or(0);
            let action = decide_recovery(signal, current_retry_count, policy);

            // If the recovery action is Retry, increment the retry counter and persist it.
            if matches!(action, RecoveryAction::Retry { .. }) {
                workflow.increment_retry_count().map_err(|e| {
                    MonitorError::InvalidState(format!(
                        "failed to increment retry count: {e}"
                    ))
                })?;

                // Persist the updated phase state (with incremented retry_count).
                if let Some(current_phase) = workflow.current_phase() {
                    let phase_order = workflow.current_phase_idx;
                    store
                        .upsert_phase_state(&workflow.workflow_id, current_phase, phase_order)
                        .map_err(|e| {
                            MonitorError::InvalidState(format!(
                                "failed to persist phase state after retry increment: {e}"
                            ))
                        })?;
                }
            }

            Ok(action)
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
    use crate::store::WorkflowStore;
    use crate::workflow::engine::PhaseStatus;
    use crate::workflow::template::impl_audit_default;
    use crate::workflow::WorkflowId;
    use chrono::Utc;
    use std::path::PathBuf;

    use super::super::test_helpers::make_workflow;
    use super::super::StallReason;

    /// Create an in-memory test store for unit tests.
    fn test_store() -> WorkflowStore {
        WorkflowStore::open(":memory:").expect("open in-memory store")
    }

    // -- handle_signal tests --------------------------------------------------

    #[test]
    fn handle_phase_complete_completes_phase() {
        let mut wf = make_workflow();
        let store = test_store();
        let signal = ProgressSignal::PhaseComplete {
            phase_id: "implement".to_string(),
        };
        let policy = RetryPolicy::default();

        let action = handle_signal(&signal, &mut wf, 0, &policy, &store).expect("should succeed");
        assert!(matches!(action, RecoveryAction::Cancel { .. }));
        assert_eq!(wf.phase_states[0].status, PhaseStatus::Completed);
    }

    #[test]
    fn handle_stalled_returns_retry() {
        let mut wf = make_workflow();
        let store = test_store();
        store.insert_workflow(&wf).expect("insert workflow");

        let signal = ProgressSignal::Stalled {
            phase_id: "implement".to_string(),
            since: Utc::now(),
            reason: StallReason::HeartbeatTimeout,
        };
        let policy = RetryPolicy::default();

        let action = handle_signal(&signal, &mut wf, 0, &policy, &store).expect("should succeed");
        assert!(matches!(action, RecoveryAction::Retry { .. }));
    }

    #[test]
    fn handle_stalled_increments_retry_count() {
        let mut wf = make_workflow();
        let store = test_store();

        // Insert the workflow first so we can upsert phase state
        store.insert_workflow(&wf).expect("insert workflow");

        assert_eq!(wf.phase_states[0].retry_count, 0);

        let signal = ProgressSignal::Stalled {
            phase_id: "implement".to_string(),
            since: Utc::now(),
            reason: StallReason::NoCodeDiff,
        };
        let policy = RetryPolicy::default();

        let action = handle_signal(&signal, &mut wf, 0, &policy, &store).expect("should succeed");
        assert!(matches!(action, RecoveryAction::Retry { .. }));

        // Verify retry count was incremented
        assert_eq!(wf.phase_states[0].retry_count, 1);

        // Verify it was persisted
        let loaded = store
            .get_workflow(&wf.workflow_id)
            .expect("get workflow")
            .expect("workflow should exist");
        assert_eq!(loaded.phase_states[0].retry_count, 1);
    }

    #[test]
    fn handle_healthy_returns_cancel() {
        let mut wf = make_workflow();
        let store = test_store();
        let signal = ProgressSignal::Healthy {
            phase_id: "implement".to_string(),
            last_activity: Utc::now(),
        };
        let policy = RetryPolicy::default();

        let action = handle_signal(&signal, &mut wf, 0, &policy, &store).expect("should succeed");
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
        let store = test_store();

        // Phase 0 is Pending by default — not Active
        let signal = ProgressSignal::PhaseComplete {
            phase_id: "implement".to_string(),
        };
        let policy = RetryPolicy::default();

        let result = handle_signal(&signal, &mut wf, 0, &policy, &store);
        assert!(result.is_err());
        assert!(matches!(result, Err(MonitorError::InvalidState(_))));
    }
}
