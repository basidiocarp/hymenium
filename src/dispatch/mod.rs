//! Agent dispatch via canopy.
//!
//! Translates workflow phases into canopy task operations. This module is the
//! only outbound write surface to canopy — it creates tasks, assigns agents,
//! and checks completeness, but never accesses canopy's database directly.
//!
//! ## Dispatch Request Contract
//!
//! Workflow dispatch intake follows the `dispatch-request-v1` contract schema
//! defined in `septa/dispatch-request-v1.schema.json`. The orchestration layer
//! receives dispatch requests and translates them into canopy task creation
//! calls using the Canopy CLI adapter in `cli.rs`.

pub mod capability;
pub mod capability_client;
pub mod cli;
mod mock;
mod orchestrate;
pub mod task_packet;

// Re-export everything that was public in the original dispatch.rs so external
// callers see no change.
pub use capability_client::CapabilityCanopyClient;
pub use cli::CliCanopyClient;
pub use mock::MockCanopyClient;
pub use orchestrate::{agent_name, dispatch_workflow, handoff_slug};
pub use task_packet::{CapabilityRequirements, ContextBudget, TaskPacket};

use crate::workflow::engine::{WorkflowInstance, WorkflowStatus};
use crate::workflow::gate::{EvidenceGateEvaluator, PermissiveGateEvaluator};
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

/// Error type for dispatch operations.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DispatchError {
    #[error("canopy error: {0}")]
    CanopyError(String),

    #[error("task creation failed: {0}")]
    TaskCreationFailed(String),

    #[error("handoff not found: {0}")]
    HandoffNotFound(String),

    #[error("template not found: {0}")]
    TemplateNotFound(String),

    #[error("invalid state: {0}")]
    InvalidState(String),
}

// ---------------------------------------------------------------------------
// Domain types
// ---------------------------------------------------------------------------

/// Strongly-typed agent identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentId(pub String);

impl std::fmt::Display for AgentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Options for creating a canopy task.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskOptions {
    pub required_role: Option<crate::workflow::template::AgentRole>,
    pub required_tier: Option<crate::workflow::template::AgentTier>,
    pub verification_required: bool,
    /// Capability labels the claiming agent must possess.
    ///
    /// Drawn from the shared vocabulary in `dispatch/capability.rs`.
    /// An empty list means any agent can claim the task (backward-compatible).
    pub required_capabilities: Vec<String>,
    /// User or agent identity who requested this task.
    ///
    /// Used by canopy to track task provenance. If None, omitted from the create command.
    pub requested_by: Option<String>,
    /// Workflow ID to associate this task with in Canopy.
    ///
    /// Passed as `--workflow-id` to `canopy task create`. If None, omitted.
    pub workflow_id: Option<String>,
    /// Phase ID within the workflow for this task.
    ///
    /// Passed as `--phase-id` to `canopy task create`. If None, omitted.
    pub phase_id: Option<String>,
}

/// Completion signal attached by an agent when a task is marked complete.
///
/// An agent can signal whether the workflow should continue past this phase
/// and optionally provide a follow-up action (e.g., a next task ID or directive).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompletionSignal {
    /// Opaque status code from the agent (not used by hymenium for gate logic).
    #[serde(default)]
    pub status: Option<String>,
    /// If absent or false, the agent wants the workflow to stop after this phase.
    #[serde(default)]
    pub should_continue: Option<bool>,
    /// Optional next action the agent recommends (follow-up task or directive).
    #[serde(default)]
    pub next_action: Option<NextAction>,
    /// Optional summary from the agent.
    #[serde(default)]
    pub summary: Option<String>,
    /// Optional agent identifier that emitted this signal.
    #[serde(default)]
    pub agent_id: Option<String>,
}

impl CompletionSignal {
    /// Returns true if the completion signal indicates the agent wants to stop.
    /// Absent or false should_continue means the agent wants the workflow to stop.
    /// Note: a present-but-empty signal object (`{}`) has an absent
    /// should_continue and therefore also requests a stop, by schema definition.
    #[must_use]
    pub fn wants_stop(&self) -> bool {
        !self.should_continue.unwrap_or(false)
    }
}

/// Recommended follow-up action after a phase completes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NextAction {
    /// ID of a follow-up task to create or activate.
    #[serde(default)]
    pub follow_up_task_id: Option<String>,
    /// Directive (e.g., "escalate", "narrow_scope", "retry") for the next phase.
    #[serde(default)]
    pub directive: Option<String>,
}

/// Detail record for a canopy task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDetail {
    pub task_id: String,
    pub title: String,
    pub status: String,
    pub agent_id: Option<String>,
    pub parent_id: Option<String>,
    /// Capability requirements recorded at task-create time (empty = any agent).
    #[serde(default)]
    pub required_capabilities: Vec<String>,
    /// True if a code diff has been recorded for this task.
    #[serde(default)]
    pub has_code_diff: bool,
    /// True if verification evidence has been attached and passed.
    #[serde(default)]
    pub has_verification_passed: bool,
    /// Optional completion signal from the agent when task is marked complete.
    #[serde(default)]
    pub completion_signal: Option<CompletionSignal>,
}

/// Report from a completeness check on a handoff.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletenessReport {
    pub complete: bool,
    pub total_items: usize,
    pub completed_items: usize,
    pub missing: Vec<String>,
}

/// Result of importing a handoff into canopy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportResult {
    pub task_id: String,
    pub subtask_ids: Vec<String>,
}

// ---------------------------------------------------------------------------
// CanopyClient trait
// ---------------------------------------------------------------------------

/// Interface to the canopy coordination layer.
///
/// All canopy interaction flows through this trait so that tests can substitute
/// a mock without requiring a running canopy instance.
pub trait CanopyClient {
    /// Create a top-level task, returning its ID.
    fn create_task(
        &self,
        title: &str,
        description: &str,
        project_root: &str,
        options: &TaskOptions,
    ) -> Result<String, DispatchError>;

    /// Create a subtask under `parent_id`, returning its ID.
    fn create_subtask(
        &self,
        parent_id: &str,
        title: &str,
        description: &str,
        options: &TaskOptions,
    ) -> Result<String, DispatchError>;

    /// Assign a task to an agent.
    ///
    /// The `assigned_by` parameter identifies who is performing the assignment (typically the
    /// workflow orchestrator name or user identity).
    fn assign_task(
        &self,
        task_id: &str,
        agent_id: &str,
        assigned_by: &str,
    ) -> Result<(), DispatchError>;

    /// Fetch the detail record for a task.
    fn get_task(&self, task_id: &str) -> Result<TaskDetail, DispatchError>;

    /// Check whether a handoff's checklist items are all satisfied.
    fn check_completeness(&self, handoff_path: &str) -> Result<CompletenessReport, DispatchError>;

    /// Import a handoff file into canopy and return the created tasks.
    fn import_handoff(
        &self,
        path: &str,
        assign_to: Option<&str>,
    ) -> Result<ImportResult, DispatchError>;

    /// Cancel a task by ID.
    ///
    /// Used as a compensating action when partial dispatch fails mid-loop so
    /// already-created subtasks do not remain permanently orphaned in Canopy.
    /// Failures here are best-effort: if cancel itself fails, a warning is
    /// emitted but the original dispatch error is still propagated.
    fn cancel_task(&self, task_id: &str) -> Result<(), DispatchError>;
}

// ---------------------------------------------------------------------------
// Phase reconciliation
// ---------------------------------------------------------------------------

/// Outcome of reconciling a single phase against Canopy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhaseReconcileOutcome {
    /// Phase had no Canopy task ID; nothing to reconcile.
    NoTaskId,
    /// Canopy task is still active; phase left unchanged.
    StillActive,
    /// Canopy task was completed; phase is now Completed and workflow may have advanced.
    MarkedCompleted { phase_id: String, advanced: bool },
    /// Canopy task was cancelled or failed; phase is now Failed.
    MarkedFailed { phase_id: String, reason: String },
    /// Phase was already in a terminal state; reconciliation was idempotent.
    AlreadyTerminal { phase_id: String },
    /// Agent's completion signal requested workflow stop (should_continue absent/false).
    /// The phase is now Completed but did not advance. The agent may have provided
    /// a next_action directive.
    StoppedByAgentSignal {
        phase_id: String,
        next_action: Option<NextAction>,
    },
}

/// Result of reconciling all phases in a workflow against Canopy.
#[derive(Debug)]
pub struct ReconcileResult {
    /// Updated workflow instance after reconciliation.
    pub instance: WorkflowInstance,
    /// Per-phase outcomes, in phase order.
    pub outcomes: Vec<PhaseReconcileOutcome>,
}

/// Canopy task statuses that indicate a completed outcome.
///
/// Only `"completed"` maps to success. `"closed"` is NOT a success state —
/// operators can close tasks for abandonment or scope reduction.
fn is_completed_status(status: &str) -> bool {
    status == "completed"
}

/// Canopy task statuses that indicate a failure or cancellation.
///
/// Canopy's `TaskStatus` enum serialises as `snake_case`. Both spellings of
/// cancelled are accepted for defensive cross-locale handling.
fn is_failed_status(status: &str) -> bool {
    matches!(status, "cancelled" | "canceled")
}

/// Reconcile in-flight workflow phases against their Canopy task statuses.
///
/// For each phase that has a `canopy_task_id` and is not yet in a terminal
/// state, this function calls [`CanopyClient::get_task`] and:
///
/// - If the Canopy task is **completed**: marks the phase Completed and
///   attempts to advance the workflow to the next phase.
/// - If the Canopy task is **cancelled/failed**: marks the phase Failed and
///   sets the workflow status to Failed.
/// - If the Canopy task is still active: leaves the phase unchanged.
///
/// Reconciliation is **idempotent**: calling this function twice on a workflow
/// whose phases are already in a terminal state is a no-op.
///
/// ## Gate evaluation on advance
///
/// When a phase is marked completed and the workflow attempts to advance:
/// - If `force_advance` is `false`: uses `EvidenceGateEvaluator` to check
///   that the task has `has_code_diff` and `has_verification_passed` set.
/// - If `force_advance` is `true`: uses `PermissiveGateEvaluator`, which
///   passes every condition unconditionally. A completed Canopy task implies
///   the assigned agent satisfied all gate conditions as part of its work.
///   Hymenium trusts the Canopy completion signal as the gate outcome.
///
/// ## Error handling
///
/// Errors from `get_task` are returned immediately. Phase-level state errors
/// (e.g. the phase is in an unexpected state) are returned immediately.
/// Advance failures due to gate conditions are treated as non-terminal — the
/// phase is still marked completed, but `advanced = false` in the outcome.
pub fn reconcile_phases(
    mut instance: WorkflowInstance,
    canopy: &dyn CanopyClient,
    force_advance: bool,
) -> Result<ReconcileResult, DispatchError> {
    let phase_count = instance.phase_states.len();
    let mut outcomes = Vec::with_capacity(phase_count);

    // We only reconcile the current phase. Phases before current_phase_idx are
    // already in a terminal state; phases after it are not yet active.
    let current_idx = instance.current_phase_idx;

    for idx in 0..phase_count {
        let phase = &instance.phase_states[idx];

        if idx != current_idx {
            // Not the current phase — skip. Previously completed phases are
            // already terminal; future phases are not yet dispatched to agents.
            outcomes.push(PhaseReconcileOutcome::NoTaskId);
            continue;
        }

        // Only reconcile phases that have a Canopy task ID.
        let Some(task_id) = phase.canopy_task_id.clone() else {
            outcomes.push(PhaseReconcileOutcome::NoTaskId);
            continue;
        };

        // Terminal local states are already done — idempotent.
        let phase_id = phase.phase_id.clone();
        let local_status = &phase.status;
        if matches!(
            local_status,
            crate::workflow::PhaseStatus::Completed
                | crate::workflow::PhaseStatus::Failed
                | crate::workflow::PhaseStatus::Skipped
        ) {
            outcomes.push(PhaseReconcileOutcome::AlreadyTerminal {
                phase_id: phase_id.clone(),
            });
            continue;
        }

        // Fetch the Canopy task status.
        let task_detail = canopy.get_task(&task_id)?;

        if is_completed_status(&task_detail.status) {
            instance.reconcile_complete_current_phase().map_err(|e| {
                DispatchError::InvalidState(format!(
                    "reconcile complete failed for phase {phase_id}: {e}"
                ))
            })?;

            // Honor an explicit agent stop signal: a present completion_signal whose
            // should_continue is absent/false means the agent wants the workflow to
            // stop cleanly. The signal can only SUPPRESS advance — it never bypasses
            // the evidence gate.
            if let Some(signal) = &task_detail.completion_signal {
                if signal.wants_stop() {
                    instance.status = WorkflowStatus::Completed;
                    let next_action = signal.next_action.clone();
                    outcomes.push(PhaseReconcileOutcome::StoppedByAgentSignal {
                        phase_id: phase_id.clone(),
                        next_action,
                    });
                    continue;
                }
            }

            // Attempt to advance to the next phase.
            // Use the appropriate gate evaluator based on force_advance.
            let advanced = if force_advance {
                let gate_evaluator = PermissiveGateEvaluator;
                match instance.advance(&gate_evaluator) {
                    Ok(_transition) => {
                        // Advance succeeded: update workflow status to reflect the
                        // newly active phase is waiting for dispatch.
                        instance.status = WorkflowStatus::Dispatched;
                        true
                    }
                    Err(crate::workflow::engine::WorkflowError::AlreadyAtFinalPhase) => {
                        // Final phase completed — workflow is done.
                        instance.status = WorkflowStatus::Completed;
                        true
                    }
                    Err(_) => {
                        // Gate check failed or other advance error; leave the workflow
                        // at the current (now-completed) phase.
                        false
                    }
                }
            } else {
                let gate_evaluator = EvidenceGateEvaluator::new(task_detail.clone());
                match instance.advance(&gate_evaluator) {
                    Ok(_transition) => {
                        // Advance succeeded: update workflow status to reflect the
                        // newly active phase is waiting for dispatch.
                        instance.status = WorkflowStatus::Dispatched;
                        true
                    }
                    Err(crate::workflow::engine::WorkflowError::AlreadyAtFinalPhase) => {
                        // Final phase completed — workflow is done.
                        instance.status = WorkflowStatus::Completed;
                        true
                    }
                    Err(_) => {
                        // Gate check failed or other advance error; leave the workflow
                        // at the current (now-completed) phase.
                        false
                    }
                }
            };

            outcomes.push(PhaseReconcileOutcome::MarkedCompleted { phase_id, advanced });
        } else if is_failed_status(&task_detail.status) {
            let reason = format!("Canopy task {} was {}", task_id, task_detail.status);
            instance
                .reconcile_fail_current_phase(reason.clone())
                .map_err(|e| {
                    DispatchError::InvalidState(format!("reconcile fail for phase {phase_id}: {e}"))
                })?;
            outcomes.push(PhaseReconcileOutcome::MarkedFailed { phase_id, reason });
        } else {
            // Task is still active (pending, in_progress, assigned, etc.).
            outcomes.push(PhaseReconcileOutcome::StillActive);
        }
    }

    Ok(ReconcileResult { instance, outcomes })
}
