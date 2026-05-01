//! `hymenium fail <workflow_id> <reason>` command handler.

use crate::outcomes::emit_terminal_outcome;
use crate::store::{StoreError, WorkflowStore};
use crate::workflow::engine::{WorkflowError, WorkflowStatus};
use crate::workflow::WorkflowId;
use thiserror::Error;

/// Errors that can occur during the fail command.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum FailCommandError {
    #[error("store error: {0}")]
    Store(#[from] StoreError),

    #[error("workflow not found: {0}")]
    NotFound(String),

    #[error("engine error: {0}")]
    Engine(#[from] crate::workflow::engine::WorkflowError),
}

/// Run the `fail` command: mark the workflow as Failed and persist the outcome.
///
/// Calls `instance.fail_phase(reason)` on a mutable copy to mutate in-memory state,
/// then persists the new status via `store.update_workflow_status` and records
/// a transition event. Finally, emits a terminal outcome to the store.
pub fn run(workflow_id: &str, reason: &str, store: &WorkflowStore) -> Result<(), FailCommandError> {
    let id = WorkflowId(workflow_id.to_string());

    // Load the workflow. Verify it exists.
    let mut instance = store
        .get_workflow(&id)?
        .ok_or_else(|| FailCommandError::NotFound(workflow_id.to_string()))?;

    // Already-terminal guard: if the workflow is already in a terminal status,
    // return an informative no-op instead of overwriting the outcome.
    match instance.status {
        WorkflowStatus::Completed => {
            let ts = store
                .get_outcome(&id)?
                .map_or_else(|| "unknown".to_string(), |o| o.completed_at.to_rfc3339());
            println!(
                "Workflow {} already completed at {}. No change.",
                workflow_id, ts
            );
            return Ok(());
        }
        WorkflowStatus::Cancelled => {
            let ts = store
                .get_outcome(&id)?
                .map_or_else(|| "unknown".to_string(), |o| o.completed_at.to_rfc3339());
            println!(
                "Workflow {} was cancelled at {}. Outcome preserved.",
                workflow_id, ts
            );
            return Ok(());
        }
        WorkflowStatus::Failed => {
            let ts = store
                .get_outcome(&id)?
                .map_or_else(|| "unknown".to_string(), |o| o.completed_at.to_rfc3339());
            println!(
                "Workflow {} failed at {}. Outcome preserved.",
                workflow_id, ts
            );
            return Ok(());
        }
        _ => {}
    }

    // Call the engine method to fail the current phase.
    // This mutates `instance` to set status to Failed and marks the phase Failed.
    instance.fail_phase(reason)?;

    // Wrap the multi-step persistence in a transaction.
    store.with_transaction::<_, (), FailCommandError>(|txn_store| {
        // Persist the status update.
        txn_store.update_workflow_status(&id, &WorkflowStatus::Failed, None)?;

        // Persist the current phase index so reloads reflect the engine's view.
        txn_store.update_current_phase_idx(&id, instance.current_phase_idx)?;

        // Persist the phase state changes (failure_reason, status, timestamps).
        for (order, phase) in instance.phase_states.iter().enumerate() {
            txn_store.upsert_phase_state(&id, phase, order)?;
        }

        // Record the transition to failed state. Get the current phase ID.
        let phase = instance.current_phase().ok_or_else(|| {
            FailCommandError::Engine(WorkflowError::StateError(
                "invariant: current_phase is None after fail_phase".to_string(),
            ))
        })?;
        txn_store.record_transition(&id, Some(&phase.phase_id), Some("failed"), Some(reason))?;

        // Emit the terminal outcome. Pass None for failure and identity since callers
        // at this layer don't yet have a classified failure type or runtime context.
        emit_terminal_outcome(txn_store, &instance, None, None)?;

        Ok(())
    })?;

    println!(
        "Workflow {} marked as failed with reason: {}",
        workflow_id, reason
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outcome::TerminalStatus;
    use crate::store::WorkflowStore;
    use crate::workflow::engine::{PhaseStatus, WorkflowInstance};
    use crate::workflow::template::impl_audit_default;
    use crate::workflow::WorkflowId;

    fn temp_store() -> WorkflowStore {
        // Use a unique temp-file path per test invocation to avoid collisions.
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let db_path = std::env::temp_dir().join(format!("hymenium_fail_test_{nanos}.db"));
        WorkflowStore::open(&db_path).expect("open store")
    }

    fn insert_active_workflow(store: &WorkflowStore, id: &str) -> WorkflowId {
        let workflow_id = WorkflowId(id.to_string());
        let mut inst = WorkflowInstance::new(
            workflow_id.clone(),
            impl_audit_default(),
            "/handoffs/test.md",
        );
        // Advance to active state so we can call fail_phase.
        inst.start_phase().expect("start phase");
        store.insert_workflow(&inst).expect("insert workflow");
        workflow_id
    }

    /// Insert a workflow advanced to phase 1 (audit) and active there.
    fn insert_workflow_at_phase_1(store: &WorkflowStore, id: &str) -> WorkflowId {
        let workflow_id = WorkflowId(id.to_string());
        let mut inst = WorkflowInstance::new(
            workflow_id.clone(),
            impl_audit_default(),
            "/handoffs/test.md",
        );
        // Complete phase 0 and manually advance to phase 1.
        inst.start_phase().expect("start phase 0");
        inst.complete_phase().expect("complete phase 0");
        inst.current_phase_idx = 1;
        inst.phase_states[1].status = PhaseStatus::Active;
        inst.phase_states[1].started_at = Some(chrono::Utc::now());
        store.insert_workflow(&inst).expect("insert workflow");
        // Persist the advanced phase index.
        store
            .update_current_phase_idx(&workflow_id, 1)
            .expect("update current_phase_idx");
        workflow_id
    }

    /// End-to-end fail: create a workflow at active phase, fail it, assert outcome exists
    /// with `terminal_status` Failed.
    #[test]
    fn fail_inserts_outcome_with_failed_status() {
        let store = temp_store();
        let workflow_id = insert_active_workflow(&store, "01FAILTEST0000000000000001");

        run(workflow_id.0.as_str(), "test failure reason", &store).expect("fail should succeed");

        assert!(
            store.outcome_exists(&workflow_id).expect("outcome_exists"),
            "outcome should be present after fail"
        );

        // Load the outcome and verify terminal_status is "failed".
        let outcome = store
            .get_outcome(&workflow_id)
            .expect("get_outcome")
            .expect("outcome should exist");
        assert_eq!(
            outcome.terminal_status,
            TerminalStatus::Failed,
            "terminal_status must be Failed"
        );
    }

    /// Failing a non-existent workflow returns `NotFound`, not a store error.
    #[test]
    fn fail_nonexistent_workflow_returns_not_found() {
        let store = temp_store();
        let result = run("01FAILNOTFOUND00000000000001", "reason", &store);
        assert!(
            matches!(result, Err(FailCommandError::NotFound(_))),
            "expected NotFound, got {result:?}"
        );
    }

    /// After fail the workflow status in the store is Failed.
    #[test]
    fn fail_sets_workflow_status_to_failed() {
        let store = temp_store();
        let workflow_id = insert_active_workflow(&store, "01FAILTEST0000000000000002");

        run(workflow_id.0.as_str(), "test failure reason", &store).expect("fail should succeed");

        let loaded = store
            .get_workflow(&workflow_id)
            .expect("get")
            .expect("should exist");
        assert_eq!(loaded.status, WorkflowStatus::Failed);
    }

    /// The failure reason is recorded in the phase state.
    #[test]
    fn fail_phase_reason_is_recorded() {
        let store = temp_store();
        let workflow_id = insert_active_workflow(&store, "01FAILTEST0000000000000003");
        let reason = "test failure reason";

        run(workflow_id.0.as_str(), reason, &store).expect("fail should succeed");

        let loaded = store
            .get_workflow(&workflow_id)
            .expect("get")
            .expect("should exist");
        let phase = loaded.current_phase().expect("current phase should exist");
        assert_eq!(
            phase.failure_reason.as_deref(),
            Some(reason),
            "failure_reason must be stored"
        );
        assert_eq!(
            phase.status,
            PhaseStatus::Failed,
            "phase status must be Failed"
        );
    }

    /// Regression: fail must persist `current_phase_idx` so reloads reflect
    /// the engine's view, not a stale value from the last advance.
    #[test]
    fn fail_persists_current_phase_idx() {
        let store = temp_store();
        // Workflow at phase 1 (audit), active.
        let workflow_id = insert_workflow_at_phase_1(&store, "01FAILIDX000000000000001");

        // Fail the workflow while at phase 1.
        run(workflow_id.0.as_str(), "audit failed", &store).expect("fail should succeed");

        // Reload and verify current_phase_idx is 1 (the engine's view).
        let loaded = store
            .get_workflow(&workflow_id)
            .expect("get")
            .expect("should exist");
        assert_eq!(
            loaded.current_phase_idx, 1,
            "current_phase_idx must reflect phase 1 after fail"
        );
        assert_eq!(loaded.status, WorkflowStatus::Failed);
    }

    /// Guard: failing an already-failed workflow is a no-op (exits Ok, does
    /// not overwrite the existing outcome).
    #[test]
    fn fail_already_failed_is_noop() {
        let store = temp_store();
        let workflow_id = insert_active_workflow(&store, "01FAILNOOP000000000000001");

        // First fail — transitions the workflow.
        run(workflow_id.0.as_str(), "first failure", &store).expect("first fail should succeed");

        // Second fail — should be a no-op.
        run(workflow_id.0.as_str(), "second failure attempt", &store)
            .expect("second fail should succeed (noop)");

        // Outcome is still present and still Failed.
        let outcome = store
            .get_outcome(&workflow_id)
            .expect("get_outcome")
            .expect("outcome should exist");
        assert_eq!(
            outcome.terminal_status,
            TerminalStatus::Failed,
            "terminal_status must remain Failed after noop"
        );
    }

    /// Guard: failing a cancelled workflow is a no-op (exits Ok, preserves
    /// the Cancelled outcome).
    #[test]
    fn fail_cancelled_workflow_preserves_outcome() {
        use crate::commands::cancel;

        let store = temp_store();
        // Insert a pending workflow (cancel works on pending workflows).
        let workflow_id = WorkflowId("01FAILCNCL000000000000001".to_string());
        let inst = WorkflowInstance::new(
            workflow_id.clone(),
            impl_audit_default(),
            "/handoffs/test.md",
        );
        store.insert_workflow(&inst).expect("insert workflow");

        // Cancel the workflow.
        cancel::run(workflow_id.0.as_str(), &store).expect("cancel should succeed");

        // Now try to fail it — guard should fire and return Ok.
        run(workflow_id.0.as_str(), "attempted failure", &store)
            .expect("fail on cancelled workflow should return Ok (noop)");

        // Outcome is still Cancelled.
        let outcome = store
            .get_outcome(&workflow_id)
            .expect("get_outcome")
            .expect("outcome should exist");
        assert_eq!(
            outcome.terminal_status,
            TerminalStatus::Cancelled,
            "terminal_status must remain Cancelled after noop"
        );
    }

    /// Guard: failing a completed workflow is a no-op (exits Ok, preserves
    /// the Completed outcome).
    #[test]
    fn fail_completed_workflow_preserves_outcome() {
        use crate::commands::complete;

        let store = temp_store();
        // Insert a workflow advanced to final phase with completion.
        let workflow_id = WorkflowId("01FAILCOMPL000000000000001".to_string());
        let mut inst = WorkflowInstance::new(
            workflow_id.clone(),
            impl_audit_default(),
            "/handoffs/test.md",
        );
        inst.current_phase_idx = 1;
        inst.phase_states[1].status = PhaseStatus::Completed;
        inst.phase_states[1].completed_at = Some(chrono::Utc::now());
        store.insert_workflow(&inst).expect("insert workflow");

        // Complete the workflow.
        complete::run(workflow_id.0.as_str(), &store).expect("complete should succeed");

        // Now try to fail it — guard should fire and return Ok.
        run(workflow_id.0.as_str(), "attempted failure", &store)
            .expect("fail on completed workflow should return Ok (noop)");

        // Outcome is still Completed.
        let outcome = store
            .get_outcome(&workflow_id)
            .expect("get_outcome")
            .expect("outcome should exist");
        assert_eq!(
            outcome.terminal_status,
            TerminalStatus::Completed,
            "terminal_status must remain Completed after noop"
        );
    }
}
