//! Integration tests for H4: Canopy Phase Reconciliation.
//!
//! These tests verify that `reconcile_phases` correctly advances workflow state
//! based on Canopy task completion signals, and that the reconciliation path is
//! idempotent and handles all terminal Canopy statuses.

use hymenium::dispatch::{
    CanopyClient, CompletenessReport, DispatchError, ImportResult, PhaseReconcileOutcome,
    TaskDetail, TaskOptions, reconcile_phases,
};
use hymenium::workflow::engine::{PhaseStatus, WorkflowInstance, WorkflowStatus};
use hymenium::workflow::template::impl_audit_default;
use hymenium::workflow::WorkflowId;
use std::cell::RefCell;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// A minimal mock client that returns pre-configured statuses for task IDs.
struct StatusMock {
    statuses: RefCell<HashMap<String, String>>,
}

impl StatusMock {
    fn new() -> Self {
        Self {
            statuses: RefCell::new(HashMap::new()),
        }
    }

    fn set(self, task_id: &str, status: &str) -> Self {
        self.statuses
            .borrow_mut()
            .insert(task_id.to_string(), status.to_string());
        self
    }
}

impl CanopyClient for StatusMock {
    fn create_task(
        &self,
        _title: &str,
        _desc: &str,
        _root: &str,
        _opts: &TaskOptions,
    ) -> Result<String, DispatchError> {
        unimplemented!("not needed for reconcile tests")
    }

    fn create_subtask(
        &self,
        _parent: &str,
        _title: &str,
        _desc: &str,
        _opts: &TaskOptions,
    ) -> Result<String, DispatchError> {
        unimplemented!("not needed for reconcile tests")
    }

    fn assign_task(
        &self,
        _task_id: &str,
        _agent: &str,
        _by: &str,
    ) -> Result<(), DispatchError> {
        unimplemented!("not needed for reconcile tests")
    }

    fn get_task(&self, task_id: &str) -> Result<TaskDetail, DispatchError> {
        let statuses = self.statuses.borrow();
        let status = statuses
            .get(task_id)
            .cloned()
            .unwrap_or_else(|| "active".to_string());
        Ok(TaskDetail {
            task_id: task_id.to_string(),
            title: "test".to_string(),
            status,
            agent_id: None,
            parent_id: None,
            required_capabilities: vec![],
            has_code_diff: false,
            has_verification_passed: false,
        })
    }

    fn check_completeness(
        &self,
        _path: &str,
    ) -> Result<CompletenessReport, DispatchError> {
        Ok(CompletenessReport {
            complete: true,
            total_items: 0,
            completed_items: 0,
            missing: vec![],
        })
    }

    fn import_handoff(
        &self,
        _path: &str,
        _assign_to: Option<&str>,
    ) -> Result<ImportResult, DispatchError> {
        unimplemented!("not needed for reconcile tests")
    }
}

/// Build a workflow instance in the "just dispatched" state:
/// - All phases are `Pending`
/// - Phase states have canopy_task_id populated
/// - Status is `Dispatched`
fn dispatched_instance(id: &str) -> WorkflowInstance {
    let template = impl_audit_default();
    let mut instance = WorkflowInstance::new(
        WorkflowId(id.to_string()),
        template,
        "/handoffs/test.md",
    );
    instance.phase_states[0].canopy_task_id = Some("task-implement".to_string());
    instance.phase_states[1].canopy_task_id = Some("task-audit".to_string());
    instance.status = WorkflowStatus::Dispatched;
    instance
}

// ---------------------------------------------------------------------------
// Test: active phase becomes completed when Canopy task is completed
// ---------------------------------------------------------------------------

#[test]
fn phase_reconciliation_marks_active_phase_completed_when_canopy_task_is_complete() {
    let instance = dispatched_instance("wf-reconcile-1");
    let canopy = StatusMock::new()
        .set("task-implement", "completed")
        .set("task-audit", "active");

    let result = reconcile_phases(instance, &canopy).expect("reconcile should succeed");

    // Implement phase must be Completed.
    assert_eq!(
        result.instance.phase_states[0].status,
        PhaseStatus::Completed,
        "implement phase must be Completed after Canopy signals completion"
    );
    assert!(
        result.instance.phase_states[0].completed_at.is_some(),
        "completed_at must be set"
    );

    // Workflow should have advanced to audit phase.
    assert_eq!(
        result.instance.current_phase_idx, 1,
        "workflow must advance to audit phase after implement completes"
    );

    // Outcome for current (implement) phase should report MarkedCompleted+advanced.
    let outcome = &result.outcomes[0];
    assert!(
        matches!(
            outcome,
            PhaseReconcileOutcome::MarkedCompleted { advanced: true, .. }
        ),
        "expected MarkedCompleted with advanced=true, got: {:?}",
        outcome
    );
}

// ---------------------------------------------------------------------------
// Test: active Canopy task does not advance the phase
// ---------------------------------------------------------------------------

#[test]
fn phase_reconciliation_does_not_advance_when_canopy_task_is_still_active() {
    let instance = dispatched_instance("wf-reconcile-2");
    let canopy = StatusMock::new()
        .set("task-implement", "active")
        .set("task-audit", "active");

    let result = reconcile_phases(instance, &canopy).expect("reconcile should succeed");

    // Implement phase must still be Pending (dispatch leaves it Pending).
    assert_eq!(
        result.instance.phase_states[0].status,
        PhaseStatus::Pending,
        "implement phase must remain Pending when Canopy task is still active"
    );

    // No advancement.
    assert_eq!(
        result.instance.current_phase_idx, 0,
        "current phase index must not advance when task is still active"
    );

    assert_eq!(
        result.outcomes[0],
        PhaseReconcileOutcome::StillActive,
        "outcome for current phase must be StillActive"
    );
}

// ---------------------------------------------------------------------------
// Test: reconciliation is idempotent
// ---------------------------------------------------------------------------

#[test]
fn phase_reconciliation_is_idempotent() {
    let instance = dispatched_instance("wf-reconcile-3");
    let canopy = StatusMock::new()
        .set("task-implement", "completed")
        .set("task-audit", "active");

    // First call: implement phase completes and workflow advances to audit.
    let result1 = reconcile_phases(instance, &canopy).expect("first reconcile");
    assert_eq!(result1.instance.current_phase_idx, 1);
    assert_eq!(
        result1.instance.phase_states[0].status,
        PhaseStatus::Completed
    );

    // Second call: audit phase is still active, no change.
    let result2 = reconcile_phases(result1.instance, &canopy).expect("second reconcile");

    // Still at audit phase, still active.
    assert_eq!(result2.instance.current_phase_idx, 1);
    assert_eq!(
        result2.instance.phase_states[0].status,
        PhaseStatus::Completed,
        "implement phase must remain Completed after second reconcile"
    );
    assert_eq!(
        result2.instance.phase_states[1].status,
        PhaseStatus::Pending,
        "audit phase must remain Pending — Canopy says still active"
    );
}

// ---------------------------------------------------------------------------
// Test: workflow advances to next phase after completion (gates clear)
// ---------------------------------------------------------------------------

#[test]
fn phase_reconciliation_advances_to_next_phase_after_completion() {
    let instance = dispatched_instance("wf-reconcile-4");
    let canopy = StatusMock::new()
        .set("task-implement", "completed")
        .set("task-audit", "active");

    let result = reconcile_phases(instance, &canopy).expect("reconcile");

    // After implement completes, we should be at audit (index 1).
    assert_eq!(
        result.instance.current_phase_idx, 1,
        "workflow must be at audit phase after implement completes"
    );
    assert_eq!(
        result.instance.status,
        WorkflowStatus::Dispatched,
        "workflow status must be Dispatched (audit ready for assignment)"
    );

    // Audit phase must still be Pending (not yet completed).
    assert_eq!(
        result.instance.phase_states[1].status,
        PhaseStatus::Pending,
        "audit phase must be Pending — Canopy has not reported it complete"
    );
}

// ---------------------------------------------------------------------------
// Test: cancelled Canopy task marks phase failed
// ---------------------------------------------------------------------------

#[test]
fn phase_reconciliation_marks_phase_failed_when_canopy_task_cancelled() {
    let instance = dispatched_instance("wf-reconcile-5");
    let canopy = StatusMock::new()
        .set("task-implement", "cancelled")
        .set("task-audit", "active");

    let result = reconcile_phases(instance, &canopy).expect("reconcile");

    // Implement phase must be Failed.
    assert_eq!(
        result.instance.phase_states[0].status,
        PhaseStatus::Failed,
        "implement phase must be Failed when Canopy task is cancelled"
    );
    assert!(
        result.instance.phase_states[0].failure_reason.is_some(),
        "failure_reason must be set"
    );

    // Workflow status must reflect failure.
    assert_eq!(
        result.instance.status,
        WorkflowStatus::Failed,
        "workflow status must be Failed when a phase fails"
    );

    // Outcome must be MarkedFailed.
    assert!(
        matches!(
            result.outcomes[0],
            PhaseReconcileOutcome::MarkedFailed { .. }
        ),
        "outcome must be MarkedFailed, got: {:?}",
        result.outcomes[0]
    );

    // No advancement.
    assert_eq!(
        result.instance.current_phase_idx, 0,
        "current phase index must not advance after failure"
    );
}

// ---------------------------------------------------------------------------
// Test: both spellings of "cancelled" are treated as failure; unknown statuses
// are treated as still-active, not as failure.
// ---------------------------------------------------------------------------

#[test]
fn phase_reconciliation_handles_both_cancelled_spellings() {
    // Canopy's TaskStatus enum only has Cancelled as a failure terminal state.
    // Both locale spellings must map to MarkedFailed.
    for status in &["cancelled", "canceled"] {
        let instance = dispatched_instance(&format!("wf-spell-{status}"));
        let canopy = StatusMock::new().set("task-implement", status);

        let result = reconcile_phases(instance, &canopy)
            .unwrap_or_else(|e| panic!("reconcile failed for status {status}: {e}"));

        assert_eq!(
            result.instance.phase_states[0].status,
            PhaseStatus::Failed,
            "status '{status}' must be treated as failure"
        );
    }

    // Statuses that do not exist in Canopy's TaskStatus enum must NOT be
    // treated as failures — they fall through to StillActive.
    for status in &["failed", "rejected", "closed_as_failed"] {
        let instance = dispatched_instance(&format!("wf-spell-{status}"));
        let canopy = StatusMock::new().set("task-implement", status);

        let result = reconcile_phases(instance, &canopy)
            .unwrap_or_else(|e| panic!("reconcile failed for status {status}: {e}"));

        assert_eq!(
            result.outcomes[0],
            PhaseReconcileOutcome::StillActive,
            "non-existent Canopy status '{status}' must be treated as still-active, not failure"
        );
    }
}

// ---------------------------------------------------------------------------
// Test: phase without canopy_task_id is skipped
// ---------------------------------------------------------------------------

#[test]
fn phase_reconciliation_skips_phases_without_canopy_task_id() {
    let template = impl_audit_default();
    let mut instance = WorkflowInstance::new(
        WorkflowId("wf-no-task-id".to_string()),
        template,
        "/handoffs/test.md",
    );
    // Do NOT set canopy_task_id on implement phase.
    instance.phase_states[1].canopy_task_id = Some("task-audit".to_string());
    instance.status = WorkflowStatus::Dispatched;

    let canopy = StatusMock::new().set("task-audit", "completed");

    let result = reconcile_phases(instance, &canopy).expect("reconcile");

    // Current phase (implement) has no task ID — skipped, stays Pending.
    assert_eq!(result.instance.phase_states[0].status, PhaseStatus::Pending);
    assert_eq!(result.outcomes[0], PhaseReconcileOutcome::NoTaskId);
    // No advancement.
    assert_eq!(result.instance.current_phase_idx, 0);
}

// ---------------------------------------------------------------------------
// Test: reconcile on an already-completed phase is idempotent
// ---------------------------------------------------------------------------

#[test]
fn phase_reconciliation_already_completed_phase_is_idempotent() {
    let template = impl_audit_default();
    let mut instance = WorkflowInstance::new(
        WorkflowId("wf-already-done".to_string()),
        template,
        "/handoffs/test.md",
    );
    instance.phase_states[0].canopy_task_id = Some("task-implement".to_string());
    instance.phase_states[1].canopy_task_id = Some("task-audit".to_string());

    // Manually simulate that implement was already completed and we advanced.
    instance.start_phase().expect("start");
    instance.complete_phase().expect("complete");
    instance
        .advance(
            &hymenium::workflow::gate::MockGateEvaluator::new()
                .set_condition("code_diff_exists", true)
                .set_condition("verification_passed", true),
        )
        .expect("advance");
    // Now current_phase_idx = 1 (audit).

    let canopy = StatusMock::new()
        .set("task-implement", "completed") // should be ignored
        .set("task-audit", "active");

    let result = reconcile_phases(instance, &canopy).expect("reconcile");

    // Implement phase must remain Completed.
    assert_eq!(result.instance.phase_states[0].status, PhaseStatus::Completed);
    // Audit phase still active.
    assert_eq!(result.instance.phase_states[1].status, PhaseStatus::Pending);
    assert_eq!(result.instance.current_phase_idx, 1);
}

// ---------------------------------------------------------------------------
// Test: final phase completion marks workflow Completed (not just advanced)
// ---------------------------------------------------------------------------

#[test]
fn phase_reconciliation_final_phase_completion_marks_workflow_completed() {
    let template = impl_audit_default();
    let mut instance = WorkflowInstance::new(
        WorkflowId("wf-final".to_string()),
        template,
        "/handoffs/test.md",
    );
    instance.phase_states[0].canopy_task_id = Some("task-implement".to_string());
    instance.phase_states[1].canopy_task_id = Some("task-audit".to_string());

    // Simulate: implement already completed and advanced to audit.
    instance.start_phase().expect("start");
    instance.complete_phase().expect("complete");
    instance
        .advance(
            &hymenium::workflow::gate::MockGateEvaluator::new()
                .set_condition("code_diff_exists", true)
                .set_condition("verification_passed", true),
        )
        .expect("advance");
    // Now at audit phase (index 1).

    // Canopy reports audit as completed.
    let canopy = StatusMock::new().set("task-audit", "completed");

    let result = reconcile_phases(instance, &canopy).expect("reconcile");

    assert_eq!(
        result.instance.phase_states[1].status,
        PhaseStatus::Completed,
        "audit phase must be Completed"
    );
    assert_eq!(
        result.instance.status,
        WorkflowStatus::Completed,
        "workflow must be Completed after final phase completes"
    );
}
