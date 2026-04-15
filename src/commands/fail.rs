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

    // Call the engine method to fail the current phase.
    // This mutates `instance` to set status to Failed and marks the phase Failed.
    instance.fail_phase(reason)?;

    // Wrap the multi-step persistence in a transaction.
    store.with_transaction::<_, (), FailCommandError>(|txn_store| {
        // Persist the status update.
        txn_store.update_workflow_status(&id, &WorkflowStatus::Failed, None)?;

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
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
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
}
