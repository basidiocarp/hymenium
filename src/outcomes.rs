//! Outcome emission helpers.
//!
//! Provides [`emit_terminal_outcome`], a shared helper that builds and
//! persists a [`WorkflowOutcome`] for any terminal workflow transition
//! (completed, failed, or cancelled).
//!
//! # Design
//!
//! The workflow engine (`engine.rs`) only mutates in-memory state. Production
//! callers in `commands/` must persist the terminal outcome after calling
//! engine methods. This module centralises that emission logic so it is not
//! repeated across every command.
//!
//! # Production callers
//!
//! `commands/cancel.rs`, `commands/fail.rs`, and `commands/complete.rs`
//! are the production callers that drive terminal transitions. Each loads
//! the instance, mutates it via the corresponding engine method
//! (`fail_phase`, `complete_workflow`, or direct status update for cancel),
//! persists status and transition rows, and calls `emit_terminal_outcome`
//! to land the outcome row.

use chrono::Utc;

use crate::failure::TypedFailure;
use crate::outcome::{RuntimeIdentity, WorkflowOutcome};
use crate::store::{StoreError, WorkflowStore};
use crate::workflow::engine::WorkflowInstance;

/// Build and persist a terminal [`WorkflowOutcome`] for `instance`.
///
/// Uses the current wall-clock time as the completion timestamp.
/// Passing `failure` as `Some` maps the [`crate::failure::FailureKind`] to
/// the wire [`crate::outcome::TerminalFailureType`]; passing `None` emits
/// `Unknown` when the instance status is `Failed`.
///
/// `identity` is attached when present so retrospective route analysis can
/// account for host, worktree, or session context.
///
/// # Errors
///
/// Returns a [`StoreError`] when serialisation or the `SQLite` write fails.
/// The store uses `INSERT OR REPLACE`, so calling this a second time for
/// the same workflow is safe — the latest outcome wins.
pub fn emit_terminal_outcome(
    store: &WorkflowStore,
    instance: &WorkflowInstance,
    failure: Option<&TypedFailure>,
    identity: Option<RuntimeIdentity>,
) -> Result<(), StoreError> {
    let mut outcome = WorkflowOutcome::build(instance, failure, Utc::now());
    if let Some(id) = identity {
        outcome = outcome.with_runtime_identity(id);
    }
    store.insert_outcome(&outcome)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::failure::FailureKind;
    use crate::outcome::TerminalStatus;
    use crate::store::WorkflowStore;
    use crate::workflow::engine::{WorkflowInstance, WorkflowStatus};
    use crate::workflow::template::impl_audit_default;
    use crate::workflow::WorkflowId;

    fn in_memory_store() -> WorkflowStore {
        // Use a unique temp-file path per test to avoid cross-test contention.
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!("hymenium_outcomes_test_{nanos}.db"));
        WorkflowStore::open(&path).expect("open store")
    }

    fn make_completed_instance(id: &str) -> WorkflowInstance {
        let mut inst = WorkflowInstance::new(
            WorkflowId(id.to_string()),
            impl_audit_default(),
            "/handoffs/test.md",
        );
        inst.status = WorkflowStatus::Completed;
        inst
    }

    fn make_failed_instance(id: &str) -> WorkflowInstance {
        let mut inst = WorkflowInstance::new(
            WorkflowId(id.to_string()),
            impl_audit_default(),
            "/handoffs/test.md",
        );
        inst.status = WorkflowStatus::Failed;
        inst
    }

    /// Emit a completed outcome end-to-end; assert it lands in the store with
    /// the right `terminal_status`.
    #[test]
    fn emit_completed_outcome_stores_correctly() {
        let store = in_memory_store();
        let inst = make_completed_instance("01EMITOUTCOME00000000000001");
        store.insert_workflow(&inst).expect("insert workflow");

        emit_terminal_outcome(&store, &inst, None, None).expect("emit");

        let outcome = store
            .get_outcome(&inst.workflow_id)
            .expect("get_outcome")
            .expect("outcome must exist after emit");

        assert_eq!(outcome.terminal_status, TerminalStatus::Completed);
        assert!(outcome.failure_type.is_none());
        assert!(outcome.runtime_identity.is_none());
    }

    /// Emit a failed outcome with a [`TypedFailure`] and assert the `failure_type`
    /// is mapped correctly.
    #[test]
    fn emit_failed_outcome_with_typed_failure() {
        let store = in_memory_store();
        let inst = make_failed_instance("01EMITOUTCOME00000000000002");
        store.insert_workflow(&inst).expect("insert workflow");

        let failure = TypedFailure::new(FailureKind::ContractMismatch);
        emit_terminal_outcome(&store, &inst, Some(&failure), None).expect("emit");

        let outcome = store
            .get_outcome(&inst.workflow_id)
            .expect("get_outcome")
            .expect("outcome must exist");

        assert_eq!(outcome.terminal_status, TerminalStatus::Failed);
        assert!(outcome.failure_type.is_some());
    }

    /// Emit with runtime identity and verify the identity is stored and round-trips.
    #[test]
    fn emit_with_runtime_identity_round_trips() {
        let store = in_memory_store();
        let inst = make_completed_instance("01EMITOUTCOME00000000000003");
        store.insert_workflow(&inst).expect("insert workflow");

        let identity = RuntimeIdentity {
            runtime_session_id: Some("sess_test_001".to_string()),
            project_root: Some("/projects/basidiocarp".to_string()),
            worktree_id: Some("feature-branch".to_string()),
            host_ref: Some("volva:anthropic".to_string()),
            workspace_id: None,
        };

        emit_terminal_outcome(&store, &inst, None, Some(identity)).expect("emit");

        let outcome = store
            .get_outcome(&inst.workflow_id)
            .expect("get_outcome")
            .expect("outcome must exist");

        let id = outcome
            .runtime_identity
            .expect("runtime_identity must be present");
        assert_eq!(id.runtime_session_id.as_deref(), Some("sess_test_001"));
        assert_eq!(id.host_ref.as_deref(), Some("volva:anthropic"));
        assert!(id.workspace_id.is_none());
    }
}
