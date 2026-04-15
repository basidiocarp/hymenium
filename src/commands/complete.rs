//! `hymenium complete <workflow_id>` command handler.

use crate::outcomes::emit_terminal_outcome;
use crate::store::{StoreError, WorkflowStore};
use crate::workflow::engine::{WorkflowError, WorkflowStatus};
use crate::workflow::WorkflowId;
use thiserror::Error;

/// Errors that can occur during the complete command.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CompleteCommandError {
    #[error("store error: {0}")]
    Store(#[from] StoreError),

    #[error("workflow not found: {0}")]
    NotFound(String),

    #[error("engine error: {0}")]
    Engine(#[from] crate::workflow::engine::WorkflowError),
}

/// Run the `complete` command: mark the workflow as Completed and persist the outcome.
///
/// Calls `instance.complete_workflow()` on a mutable copy to mutate in-memory state,
/// then persists the new status via `store.update_workflow_status` and records
/// a transition event. Finally, emits a terminal outcome to the store.
pub fn run(workflow_id: &str, store: &WorkflowStore) -> Result<(), CompleteCommandError> {
    let id = WorkflowId(workflow_id.to_string());

    // Load the workflow. Verify it exists.
    let mut instance = store
        .get_workflow(&id)?
        .ok_or_else(|| CompleteCommandError::NotFound(workflow_id.to_string()))?;

    // Call the engine method to complete the workflow.
    // This mutates `instance` to set status to Completed.
    instance.complete_workflow()?;

    // Wrap the multi-step persistence in a transaction.
    store.with_transaction::<_, (), CompleteCommandError>(|txn_store| {
        // Persist the status update.
        txn_store.update_workflow_status(&id, &WorkflowStatus::Completed, None)?;

        // Persist the current phase index so reloads reflect the engine's view.
        txn_store.update_current_phase_idx(&id, instance.current_phase_idx)?;

        // Persist the phase state changes (timestamps, etc).
        for (order, phase) in instance.phase_states.iter().enumerate() {
            txn_store.upsert_phase_state(&id, phase, order)?;
        }

        // Record the transition to completed state. Get the current phase ID.
        let phase = instance.current_phase().ok_or_else(|| {
            CompleteCommandError::Engine(WorkflowError::StateError(
                "invariant: current_phase is None after complete_workflow".to_string(),
            ))
        })?;
        txn_store.record_transition(
            &id,
            Some(&phase.phase_id),
            Some("completed"),
            Some("workflow completed successfully"),
        )?;

        // Emit the terminal outcome. Pass None for failure and identity since callers
        // at this layer don't yet have a classified failure type or runtime context.
        emit_terminal_outcome(txn_store, &instance, None, None)?;

        Ok(())
    })?;

    println!("Workflow {} completed successfully.", workflow_id);

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
    use chrono::Utc;

    fn temp_store() -> WorkflowStore {
        // Use a unique temp-file path per test invocation to avoid collisions.
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let db_path = std::env::temp_dir().join(format!("hymenium_complete_test_{nanos}.db"));
        WorkflowStore::open(&db_path).expect("open store")
    }

    fn insert_final_phase_completed_workflow(store: &WorkflowStore, id: &str) -> WorkflowId {
        let workflow_id = WorkflowId(id.to_string());
        let mut inst = WorkflowInstance::new(
            workflow_id.clone(),
            impl_audit_default(),
            "/handoffs/test.md",
        );
        // Manually set the workflow state to simulate reaching final phase with completion.
        // This mimics what would happen after the audit phase completes naturally.
        inst.current_phase_idx = 1;
        inst.phase_states[1].status = PhaseStatus::Completed;
        inst.phase_states[1].completed_at = Some(Utc::now());
        store.insert_workflow(&inst).expect("insert workflow");
        workflow_id
    }

    /// End-to-end complete: create a workflow at final completed phase, complete it,
    /// assert outcome exists with `terminal_status` Completed.
    #[test]
    fn complete_inserts_outcome_with_completed_status() {
        let store = temp_store();
        let workflow_id =
            insert_final_phase_completed_workflow(&store, "01COMPLTEST00000000000001");

        run(workflow_id.0.as_str(), &store).expect("complete should succeed");

        assert!(
            store.outcome_exists(&workflow_id).expect("outcome_exists"),
            "outcome should be present after complete"
        );

        // Load the outcome and verify terminal_status is "completed".
        let outcome = store
            .get_outcome(&workflow_id)
            .expect("get_outcome")
            .expect("outcome should exist");
        assert_eq!(
            outcome.terminal_status,
            TerminalStatus::Completed,
            "terminal_status must be Completed"
        );
    }

    /// Completing a non-existent workflow returns `NotFound`, not a store error.
    #[test]
    fn complete_nonexistent_workflow_returns_not_found() {
        let store = temp_store();
        let result = run("01COMPLNOTFOUND000000000001", &store);
        assert!(
            matches!(result, Err(CompleteCommandError::NotFound(_))),
            "expected NotFound, got {result:?}"
        );
    }

    /// After complete the workflow status in the store is Completed.
    #[test]
    fn complete_sets_workflow_status_to_completed() {
        let store = temp_store();
        let workflow_id =
            insert_final_phase_completed_workflow(&store, "01COMPLTEST00000000000002");

        run(workflow_id.0.as_str(), &store).expect("complete should succeed");

        let loaded = store
            .get_workflow(&workflow_id)
            .expect("get")
            .expect("should exist");
        assert_eq!(loaded.status, WorkflowStatus::Completed);
    }

    /// Completed outcomes have no `failure_type` — round-trip confirms.
    #[test]
    fn complete_outcome_failure_type_is_none() {
        let store = temp_store();
        let workflow_id =
            insert_final_phase_completed_workflow(&store, "01COMPLETEFT00000000000001");
        run(workflow_id.0.as_str(), &store).expect("complete");

        let outcome = store
            .get_outcome(&workflow_id)
            .expect("get_outcome")
            .expect("outcome should exist");
        assert_eq!(outcome.terminal_status, TerminalStatus::Completed);
        assert!(
            outcome.failure_type.is_none(),
            "completed outcomes have no failure_type"
        );
    }

    /// Regression: complete must persist current_phase_idx so reloads reflect
    /// the engine's view, not a stale value from the last advance.
    #[test]
    fn complete_persists_current_phase_idx() {
        let store = temp_store();
        let workflow_id =
            insert_final_phase_completed_workflow(&store, "01COMPLIDX0000000000000001");

        run(workflow_id.0.as_str(), &store).expect("complete should succeed");

        let loaded = store
            .get_workflow(&workflow_id)
            .expect("get")
            .expect("should exist");
        assert_eq!(
            loaded.current_phase_idx, 1,
            "current_phase_idx must reflect phase 1 after complete"
        );
        assert_eq!(loaded.status, WorkflowStatus::Completed);
    }
}
