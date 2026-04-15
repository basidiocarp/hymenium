//! `hymenium cancel <workflow_id>` command handler.

use crate::outcomes::emit_terminal_outcome;
use crate::store::{StoreError, WorkflowStore};
use crate::workflow::engine::WorkflowStatus;
use crate::workflow::WorkflowId;
use thiserror::Error;

/// Errors that can occur during the cancel command.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CancelCommandError {
    #[error("store error: {0}")]
    Store(#[from] StoreError),

    #[error("workflow not found: {0}")]
    NotFound(String),

    #[error("cannot cancel workflow in terminal state: {status}")]
    AlreadyTerminal { status: String },
}

/// Run the `cancel` command: mark the workflow as Cancelled and persist.
///
/// Note: Canopy tasks associated with this workflow are not automatically
/// closed. Canopy-side cleanup is a separate concern and must be handled
/// via the Canopy CLI or MCP surface.
pub fn run(workflow_id: &str, store: &WorkflowStore) -> Result<(), CancelCommandError> {
    let id = WorkflowId(workflow_id.to_string());

    // Load and verify the workflow exists.
    let instance = store
        .get_workflow(&id)?
        .ok_or_else(|| CancelCommandError::NotFound(workflow_id.to_string()))?;

    // Check if workflow is already in a terminal state.
    match instance.status {
        WorkflowStatus::Completed | WorkflowStatus::Failed => {
            return Err(CancelCommandError::AlreadyTerminal {
                status: instance.status.to_string(),
            });
        }
        WorkflowStatus::Cancelled => {
            // Idempotent: cancelling an already-cancelled workflow is a no-op.
            println!("Workflow {} is already cancelled.", workflow_id);
            return Ok(());
        }
        // Allow cancellation of Pending, Dispatched, InProgress, BlockedOnGate, AwaitingRepair
        _ => {}
    }

    // Wrap the multi-step persistence in a transaction.
    store.with_transaction::<_, (), CancelCommandError>(|txn_store| {
        txn_store.update_workflow_status(&id, &WorkflowStatus::Cancelled, None)?;

        // Persist the current phase index so reloads reflect the engine's view.
        txn_store.update_current_phase_idx(&id, instance.current_phase_idx)?;

        txn_store.record_transition(
            &id,
            None,
            Some("cancelled"),
            Some("user-requested cancellation"),
        )?;

        // Re-load the instance so its status reflects Cancelled, then persist the
        // terminal outcome via the shared emit helper. This ensures the outcome
        // record has terminal_status: "cancelled" and the learning loop (#141g)
        // can read a non-empty table.
        let mut instance = txn_store
            .get_workflow(&id)?
            .ok_or_else(|| CancelCommandError::NotFound(workflow_id.to_string()))?;
        instance.status = WorkflowStatus::Cancelled;
        emit_terminal_outcome(txn_store, &instance, None, None)?;

        Ok(())
    })?;

    println!("Workflow {} cancelled.", workflow_id);
    println!(
        "Note: Canopy tasks associated with this workflow may still be open. \
         Close them via the Canopy CLI or MCP surface separately."
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
        let db_path = std::env::temp_dir().join(format!("hymenium_cancel_test_{nanos}.db"));
        WorkflowStore::open(&db_path).expect("open store")
    }

    fn insert_pending_workflow(store: &WorkflowStore, id: &str) -> WorkflowId {
        let workflow_id = WorkflowId(id.to_string());
        let inst = WorkflowInstance::new(
            workflow_id.clone(),
            impl_audit_default(),
            "/handoffs/test.md",
        );
        store.insert_workflow(&inst).expect("insert workflow");
        workflow_id
    }

    /// End-to-end cancel: create a workflow, cancel it, assert outcome exists
    /// with `terminal_status` Cancelled.
    #[test]
    fn cancel_inserts_outcome_with_cancelled_status() {
        let store = temp_store();
        let workflow_id = insert_pending_workflow(&store, "01CANCEL00000000000000001");

        run(workflow_id.0.as_str(), &store).expect("cancel should succeed");

        assert!(
            store.outcome_exists(&workflow_id).expect("outcome_exists"),
            "outcome should be present after cancel"
        );

        // Load the outcome and verify terminal_status is "cancelled".
        let outcome = store
            .get_outcome(&workflow_id)
            .expect("get_outcome")
            .expect("outcome should exist");
        assert_eq!(
            outcome.terminal_status,
            TerminalStatus::Cancelled,
            "terminal_status must be Cancelled"
        );
    }

    /// Cancelling a non-existent workflow returns `NotFound`, not a store error.
    #[test]
    fn cancel_nonexistent_workflow_returns_not_found() {
        let store = temp_store();
        let result = run("01NOTFOUND0000000000000001", &store);
        assert!(
            matches!(result, Err(CancelCommandError::NotFound(_))),
            "expected NotFound, got {result:?}"
        );
    }

    /// After cancel the workflow status in the store is Cancelled.
    #[test]
    fn cancel_sets_workflow_status_to_cancelled() {
        let store = temp_store();
        let workflow_id = insert_pending_workflow(&store, "01CANCEL00000000000000002");

        run(workflow_id.0.as_str(), &store).expect("cancel should succeed");

        let loaded = store
            .get_workflow(&workflow_id)
            .expect("get")
            .expect("should exist");
        assert_eq!(loaded.status, WorkflowStatus::Cancelled);
    }

    /// Outcome `terminal_status` round-trips through serde correctly.
    #[test]
    fn cancelled_outcome_terminal_status_round_trips() {
        let store = temp_store();
        let workflow_id = insert_pending_workflow(&store, "01CANCEL00000000000000003");
        run(workflow_id.0.as_str(), &store).expect("cancel");

        let outcome = store
            .get_outcome(&workflow_id)
            .expect("get_outcome")
            .expect("outcome should exist");
        assert_eq!(outcome.terminal_status, TerminalStatus::Cancelled);
        assert!(
            outcome.failure_type.is_none(),
            "cancelled outcomes have no failure_type"
        );
    }

    /// Attempting to cancel an already-failed workflow returns AlreadyTerminal error.
    #[test]
    fn cancel_already_failed_workflow_returns_already_terminal() {
        use crate::commands::fail;

        let store = temp_store();
        let workflow_id = insert_pending_workflow(&store, "01CANCEL00000000000000004");

        // Transition to active phase so we can fail it.
        let mut inst = store
            .get_workflow(&workflow_id)
            .expect("get")
            .expect("should exist");
        inst.start_phase().expect("start phase");
        store
            .update_workflow_status(&workflow_id, &inst.status, None)
            .expect("update status");
        for (order, phase) in inst.phase_states.iter().enumerate() {
            store
                .upsert_phase_state(&workflow_id, phase, order)
                .expect("upsert phase");
        }

        // Fail the workflow.
        fail::run(workflow_id.0.as_str(), "test failure", &store).expect("fail should succeed");

        // Now attempt to cancel.
        let result = run(workflow_id.0.as_str(), &store);
        assert!(
            matches!(&result, Err(CancelCommandError::AlreadyTerminal { status }) if status == "failed"),
            "expected AlreadyTerminal with Failed status, got {result:?}"
        );

        // Verify the original failed outcome is preserved.
        let outcome = store
            .get_outcome(&workflow_id)
            .expect("get_outcome")
            .expect("outcome should exist");
        assert_eq!(
            outcome.terminal_status,
            TerminalStatus::Failed,
            "original Failed terminal_status must be preserved"
        );
    }

    /// Attempting to cancel an already-completed workflow returns AlreadyTerminal error.
    #[test]
    fn cancel_already_completed_workflow_returns_already_terminal() {
        use crate::commands::complete;

        let store = temp_store();
        let workflow_id = insert_pending_workflow(&store, "01CANCEL00000000000000005");

        // Transition to final phase and mark completed.
        let mut inst = store
            .get_workflow(&workflow_id)
            .expect("get")
            .expect("should exist");
        inst.start_phase().expect("start phase");
        inst.complete_phase().expect("complete phase");
        // Manually advance to final phase as would happen in normal flow.
        inst.current_phase_idx = 1;
        inst.phase_states[1].status = PhaseStatus::Completed;
        inst.phase_states[1].completed_at = Some(chrono::Utc::now());
        // Update without re-inserting.
        store
            .update_workflow_status(&workflow_id, &inst.status, None)
            .expect("update status");
        store
            .update_current_phase_idx(&workflow_id, 1)
            .expect("update current_phase_idx");
        for (order, phase) in inst.phase_states.iter().enumerate() {
            store
                .upsert_phase_state(&workflow_id, phase, order)
                .expect("upsert phase");
        }

        // Complete the workflow.
        complete::run(workflow_id.0.as_str(), &store).expect("complete should succeed");

        // Now attempt to cancel.
        let result = run(workflow_id.0.as_str(), &store);
        assert!(
            matches!(&result, Err(CancelCommandError::AlreadyTerminal { status }) if status == "completed"),
            "expected AlreadyTerminal with Completed status, got {result:?}"
        );

        // Verify the original completed outcome is preserved.
        let outcome = store
            .get_outcome(&workflow_id)
            .expect("get_outcome")
            .expect("outcome should exist");
        assert_eq!(
            outcome.terminal_status,
            TerminalStatus::Completed,
            "original Completed terminal_status must be preserved"
        );
    }

    /// Cancelling an already-cancelled workflow is idempotent and returns Ok.
    #[test]
    fn cancel_already_cancelled_workflow_is_idempotent() {
        let store = temp_store();
        let workflow_id = insert_pending_workflow(&store, "01CANCEL00000000000000006");

        // Cancel once.
        run(workflow_id.0.as_str(), &store).expect("first cancel should succeed");

        // Cancel again — should be idempotent.
        run(workflow_id.0.as_str(), &store).expect("second cancel should succeed (idempotent)");

        // Verify status is still Cancelled.
        let loaded = store
            .get_workflow(&workflow_id)
            .expect("get")
            .expect("should exist");
        assert_eq!(loaded.status, WorkflowStatus::Cancelled);

        // Verify outcome still exists and has Cancelled status.
        let outcome = store
            .get_outcome(&workflow_id)
            .expect("get_outcome")
            .expect("outcome should exist");
        assert_eq!(outcome.terminal_status, TerminalStatus::Cancelled);
    }

    /// Regression: cancel must persist current_phase_idx so reloads reflect
    /// the engine's view, not a stale value.
    #[test]
    fn cancel_persists_current_phase_idx() {
        let store = temp_store();
        let workflow_id = WorkflowId("01CANCELIDX000000000000001".to_string());
        let mut inst = WorkflowInstance::new(
            workflow_id.clone(),
            impl_audit_default(),
            "/handoffs/test.md",
        );
        // Advance to phase 1 (audit) and mark it active.
        inst.start_phase().expect("start phase 0");
        inst.complete_phase().expect("complete phase 0");
        inst.current_phase_idx = 1;
        inst.phase_states[1].status = PhaseStatus::Active;
        inst.phase_states[1].started_at = Some(chrono::Utc::now());
        store.insert_workflow(&inst).expect("insert workflow");
        store
            .update_current_phase_idx(&workflow_id, 1)
            .expect("update current_phase_idx");

        // Cancel the workflow while at phase 1.
        run(workflow_id.0.as_str(), &store).expect("cancel should succeed");

        // Reload and verify current_phase_idx is 1.
        let loaded = store
            .get_workflow(&workflow_id)
            .expect("get")
            .expect("should exist");
        assert_eq!(
            loaded.current_phase_idx, 1,
            "current_phase_idx must reflect phase 1 after cancel"
        );
        assert_eq!(loaded.status, WorkflowStatus::Cancelled);
    }

    /// Failing then attempting to cancel preserves the original failure outcome.
    #[test]
    fn cancel_preserves_original_failure_outcome() {
        use crate::commands::fail;

        let store = temp_store();
        let workflow_id = insert_pending_workflow(&store, "01CANCEL00000000000000007");

        // Transition to active phase.
        let mut inst = store
            .get_workflow(&workflow_id)
            .expect("get")
            .expect("should exist");
        inst.start_phase().expect("start phase");
        store
            .update_workflow_status(&workflow_id, &inst.status, None)
            .expect("update status");
        for (order, phase) in inst.phase_states.iter().enumerate() {
            store
                .upsert_phase_state(&workflow_id, phase, order)
                .expect("upsert phase");
        }

        // Fail the workflow.
        let failure_reason = "deliberate failure for testing";
        fail::run(workflow_id.0.as_str(), failure_reason, &store).expect("fail should succeed");

        // Attempt to cancel (should reject).
        let cancel_result = run(workflow_id.0.as_str(), &store);
        assert!(
            matches!(
                cancel_result,
                Err(CancelCommandError::AlreadyTerminal { .. })
            ),
            "cancel should reject already-failed workflow"
        );

        // Verify outcome still has Failed status and the failure reason.
        let outcome = store
            .get_outcome(&workflow_id)
            .expect("get_outcome")
            .expect("outcome should exist");
        assert_eq!(
            outcome.terminal_status,
            TerminalStatus::Failed,
            "terminal_status must be Failed"
        );
        // The outcome failure_type captures the phase failure information.
        assert!(
            outcome.failure_type.is_some(),
            "failed outcome must have failure_type"
        );
    }
}
