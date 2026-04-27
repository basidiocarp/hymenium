//! `hymenium reconcile <workflow_id>` command handler.
//!
//! Reads each active phase's Canopy task status and advances the workflow if
//! Canopy reports completion. This is the entry point for the reconciliation
//! path described in H4 (Hymenium: Canopy Phase Reconciliation).

use crate::dispatch::{reconcile_phases, CanopyClient, CliCanopyClient, PhaseReconcileOutcome};
use crate::store::{StoreError, WorkflowStore};
use crate::workflow::engine::WorkflowStatus;
use crate::workflow::WorkflowId;
use thiserror::Error;

/// Errors that can occur during the reconcile command.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ReconcileCommandError {
    #[error("store error: {0}")]
    Store(#[from] StoreError),

    #[error("workflow not found: {0}")]
    NotFound(String),

    #[error("reconcile error: {0}")]
    Reconcile(#[from] crate::dispatch::DispatchError),
}

/// Run the `reconcile` command against a live Canopy instance.
///
/// Loads the workflow from the store, calls `reconcile_phases` via the CLI
/// Canopy client, persists the updated state, and prints a human-readable
/// summary of what changed.
pub fn run(workflow_id: &str, store: &WorkflowStore) -> Result<(), ReconcileCommandError> {
    let canopy = CliCanopyClient::new("canopy");
    run_with_client(workflow_id, store, &canopy)
}

/// Run the `reconcile` command with an injected Canopy client.
///
/// This is the testable core: callers that want to control Canopy responses
/// can pass a `MockCanopyClient` here.
pub fn run_with_client(
    workflow_id: &str,
    store: &WorkflowStore,
    canopy: &dyn CanopyClient,
) -> Result<(), ReconcileCommandError> {
    let id = WorkflowId(workflow_id.to_string());

    let instance = store
        .get_workflow(&id)?
        .ok_or_else(|| ReconcileCommandError::NotFound(workflow_id.to_string()))?;

    let result = reconcile_phases(instance, canopy)?;

    // Persist all updated phase states.
    for (order, phase) in result.instance.phase_states.iter().enumerate() {
        store.upsert_phase_state(&id, phase, order)?;
    }

    // Persist the updated workflow status and current phase index.
    store.update_workflow_status(&id, &result.instance.status, None)?;
    store.update_current_phase_idx(&id, result.instance.current_phase_idx)?;

    // Print a human-readable summary.
    print_summary(workflow_id, &result.outcomes, &result.instance.status);

    Ok(())
}

fn print_summary(
    workflow_id: &str,
    outcomes: &[PhaseReconcileOutcome],
    final_status: &WorkflowStatus,
) {
    println!("Reconcile: {workflow_id}");
    for outcome in outcomes {
        match outcome {
            PhaseReconcileOutcome::NoTaskId => {}
            PhaseReconcileOutcome::StillActive => {
                println!("  current phase: still active in Canopy");
            }
            PhaseReconcileOutcome::MarkedCompleted { phase_id, advanced } => {
                if *advanced {
                    println!("  phase {phase_id}: completed — advanced to next phase");
                } else {
                    println!("  phase {phase_id}: completed — no further advance (gate or final phase)");
                }
            }
            PhaseReconcileOutcome::MarkedFailed { phase_id, reason } => {
                println!("  phase {phase_id}: failed — {reason}");
            }
            PhaseReconcileOutcome::AlreadyTerminal { phase_id } => {
                println!("  phase {phase_id}: already in terminal state (idempotent)");
            }
        }
    }
    println!("  workflow status: {final_status}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::{MockCanopyClient, TaskDetail, TaskOptions};
    use crate::store::WorkflowStore;
    use crate::workflow::engine::{PhaseStatus, WorkflowInstance, WorkflowStatus};
    use crate::workflow::template::impl_audit_default;
    use crate::workflow::WorkflowId;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp_store() -> WorkflowStore {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time goes forward")
            .subsec_nanos();
        let path =
            std::env::temp_dir().join(format!("hymenium_reconcile_cmd_{nanos}.db"));
        WorkflowStore::open(&path).expect("open store")
    }

    fn dispatched_instance(workflow_id: &str) -> WorkflowInstance {
        use crate::workflow::engine::WorkflowStatus;
        let template = impl_audit_default();
        let mut instance = WorkflowInstance::new(
            WorkflowId(workflow_id.to_string()),
            template,
            "/handoffs/test.md",
        );
        // Simulate dispatch: assign canopy task IDs and mark status Dispatched.
        instance.phase_states[0].canopy_task_id = Some("canopy-impl-task".to_string());
        instance.phase_states[1].canopy_task_id = Some("canopy-audit-task".to_string());
        instance.status = WorkflowStatus::Dispatched;
        instance
    }

    #[test]
    fn reconcile_command_completes_phase_when_canopy_done() {
        let store = tmp_store();
        let instance = dispatched_instance("wf-cmd-1");
        store.insert_workflow(&instance).expect("insert");

        // Pre-seed mock with completed implement task.
        let mock = MockCanopyClient::new();
        mock.create_task(
            "implement",
            "desc",
            ".",
            &TaskOptions::default(),
        )
        .expect("create");
        // Manually insert the task that the phase is waiting on.
        // We need to inject it with the right ID and status.
        // MockCanopyClient doesn't expose set_task_status directly;
        // use a custom mock approach.

        // Build a mock that knows the specific task IDs.
        use crate::dispatch::{CanopyClient, CompletenessReport, DispatchError, ImportResult};
        use std::cell::RefCell;

        struct FixedStatusMock {
            task_statuses: RefCell<std::collections::HashMap<String, String>>,
        }

        impl FixedStatusMock {
            fn new() -> Self {
                Self {
                    task_statuses: RefCell::new(std::collections::HashMap::new()),
                }
            }
            fn set_status(&self, task_id: &str, status: &str) {
                self.task_statuses
                    .borrow_mut()
                    .insert(task_id.to_string(), status.to_string());
            }
        }

        impl CanopyClient for FixedStatusMock {
            fn create_task(
                &self,
                _title: &str,
                _desc: &str,
                _root: &str,
                _opts: &TaskOptions,
            ) -> Result<String, DispatchError> {
                unimplemented!()
            }
            fn create_subtask(
                &self,
                _parent: &str,
                _title: &str,
                _desc: &str,
                _opts: &TaskOptions,
            ) -> Result<String, DispatchError> {
                unimplemented!()
            }
            fn assign_task(
                &self,
                _task_id: &str,
                _agent: &str,
                _by: &str,
            ) -> Result<(), DispatchError> {
                unimplemented!()
            }
            fn get_task(&self, task_id: &str) -> Result<TaskDetail, DispatchError> {
                let statuses = self.task_statuses.borrow();
                let status = statuses.get(task_id).cloned().unwrap_or_else(|| "active".to_string());
                Ok(TaskDetail {
                    task_id: task_id.to_string(),
                    title: "test task".to_string(),
                    status,
                    agent_id: None,
                    parent_id: None,
                    required_capabilities: vec![],
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
                unimplemented!()
            }
        }

        let canopy = FixedStatusMock::new();
        canopy.set_status("canopy-impl-task", "completed");
        canopy.set_status("canopy-audit-task", "active");

        run_with_client("wf-cmd-1", &store, &canopy).expect("reconcile should succeed");

        let updated = store
            .get_workflow(&WorkflowId("wf-cmd-1".to_string()))
            .expect("load")
            .expect("should exist");

        // Implement phase should now be completed.
        assert_eq!(updated.phase_states[0].status, PhaseStatus::Completed);
        // Workflow advanced to audit phase.
        assert_eq!(updated.current_phase_idx, 1);
        assert_eq!(updated.status, WorkflowStatus::Dispatched);
    }
}
