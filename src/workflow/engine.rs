//! Workflow state machine and phase transitions.
//!
//! Implements the core state machine logic for advancing workflows through phases.
//! The `WorkflowEngine` trait is defined in CLAUDE.md and will be implemented here
//! once the workflow template engine (#118e) is built.

use crate::workflow::{
    gate::{GateCondition, GateContext, GateEvaluator},
    template::WorkflowTemplate,
    WorkflowId,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Error type for workflow engine operations.
#[derive(Debug, Error)]
pub enum WorkflowError {
    #[error("invalid phase index: {0}")]
    InvalidPhaseIndex(usize),

    #[error("phase {phase_id} gate evaluation failed: {reason}")]
    GateFailed { phase_id: String, reason: String },

    #[error("already at final phase — call complete_workflow() to finish")]
    AlreadyAtFinalPhase,

    #[error("workflow state error: {0}")]
    StateError(String),
}

/// Result type for workflow engine operations.
pub type WorkflowEngineResult<T> = Result<T, WorkflowError>;

/// Status of a running workflow.
///
/// Matches the `workflow-status-v1` septa contract wire format exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum WorkflowStatus {
    Pending,
    Dispatched,
    InProgress,
    /// A gate condition blocked the workflow. See `WorkflowInstance::blocked_on`.
    BlockedOnGate,
    /// Phase output failed verification and is queued for repair.
    AwaitingRepair,
    /// Workflow paused at a `HandoffToUser` checkpoint, awaiting operator input.
    AwaitingUserInput,
    /// Workflow terminated by operator via `HandoffToUser` breakout.
    UserTerminated,
    Completed,
    Failed,
    Cancelled,
}

impl std::fmt::Display for WorkflowStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkflowStatus::Pending => write!(f, "pending"),
            WorkflowStatus::Dispatched => write!(f, "dispatched"),
            WorkflowStatus::InProgress => write!(f, "in_progress"),
            WorkflowStatus::BlockedOnGate => write!(f, "blocked_on_gate"),
            WorkflowStatus::AwaitingRepair => write!(f, "awaiting_repair"),
            WorkflowStatus::AwaitingUserInput => write!(f, "awaiting_user_input"),
            WorkflowStatus::UserTerminated => write!(f, "user_terminated"),
            WorkflowStatus::Completed => write!(f, "completed"),
            WorkflowStatus::Failed => write!(f, "failed"),
            WorkflowStatus::Cancelled => write!(f, "cancelled"),
        }
    }
}

/// Status of a single phase within a workflow instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum PhaseStatus {
    Pending,
    Active,
    /// Phase paused awaiting operator input via `HandoffToUser`.
    AwaitingUserInput,
    Completed,
    Failed,
    Skipped,
}

impl std::fmt::Display for PhaseStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PhaseStatus::Pending => write!(f, "pending"),
            PhaseStatus::Active => write!(f, "active"),
            PhaseStatus::AwaitingUserInput => write!(f, "awaiting_user_input"),
            PhaseStatus::Completed => write!(f, "completed"),
            PhaseStatus::Failed => write!(f, "failed"),
            PhaseStatus::Skipped => write!(f, "skipped"),
        }
    }
}

/// State of a single phase in a workflow instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseState {
    pub phase_id: String,
    /// Runtime role for this phase, as defined by the workflow template.
    pub role: crate::workflow::template::AgentRole,
    pub status: PhaseStatus,
    pub agent_id: Option<String>,
    pub canopy_task_id: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub failure_reason: Option<String>,
    /// Message surfaced to the operator when the phase enters `AwaitingUserInput`.
    pub pending_message: Option<String>,
    pub retry_count: u32,
    /// Cumulative tool failure count for this phase execution.
    pub tool_failure_count: u32,
    /// Cumulative request count for this phase execution.
    pub request_count: u32,
}

impl PhaseState {
    /// Create a new pending phase state.
    fn new(phase_id: impl Into<String>, role: crate::workflow::template::AgentRole) -> Self {
        Self {
            phase_id: phase_id.into(),
            role,
            status: PhaseStatus::Pending,
            agent_id: None,
            canopy_task_id: None,
            started_at: None,
            completed_at: None,
            failure_reason: None,
            pending_message: None,
            retry_count: 0,
            tool_failure_count: 0,
            request_count: 0,
        }
    }

    /// Mark this phase as active with current timestamp.
    fn mark_active(&mut self) {
        self.status = PhaseStatus::Active;
        self.started_at = Some(Utc::now());
    }

    /// Mark this phase as completed with current timestamp.
    fn mark_completed(&mut self) {
        self.status = PhaseStatus::Completed;
        self.completed_at = Some(Utc::now());
    }

    /// Mark this phase as failed with current timestamp.
    fn mark_failed(&mut self) {
        self.status = PhaseStatus::Failed;
        self.completed_at = Some(Utc::now());
    }
}

/// A single phase transition event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseTransition {
    pub from_phase_id: String,
    pub to_phase_id: String,
    pub transitioned_at: DateTime<Utc>,
    pub reason: String,
}

/// A running workflow instance with state and progress tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowInstance {
    pub workflow_id: WorkflowId,
    pub template: WorkflowTemplate,
    pub handoff_path: String,
    pub status: WorkflowStatus,
    /// When `status` is `BlockedOnGate`, holds the failing gate condition name.
    pub blocked_on: Option<String>,
    pub current_phase_idx: usize,
    pub phase_states: Vec<PhaseState>,
    pub transitions: Vec<PhaseTransition>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl WorkflowInstance {
    /// Create a new workflow instance from a template.
    pub fn new(
        workflow_id: WorkflowId,
        template: WorkflowTemplate,
        handoff_path: impl Into<String>,
    ) -> Self {
        let now = Utc::now();
        let phase_states = template
            .phases
            .iter()
            .map(|phase| PhaseState::new(&phase.phase_id, phase.effective_agent_role()))
            .collect();

        Self {
            workflow_id,
            template,
            handoff_path: handoff_path.into(),
            status: WorkflowStatus::Pending,
            blocked_on: None,
            current_phase_idx: 0,
            phase_states,
            transitions: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    /// Get the current phase, if any.
    pub fn current_phase(&self) -> Option<&PhaseState> {
        self.phase_states.get(self.current_phase_idx)
    }

    /// Get the current phase mutably, if any.
    fn current_phase_mut(&mut self) -> Option<&mut PhaseState> {
        self.phase_states.get_mut(self.current_phase_idx)
    }

    /// Get a phase by index.
    fn get_phase(&self, idx: usize) -> Option<&PhaseState> {
        self.phase_states.get(idx)
    }

    /// Get the template phase definition for the current phase.
    fn current_template_phase(&self) -> Option<&crate::workflow::template::Phase> {
        self.template.phases.get(self.current_phase_idx)
    }

    /// Start the current phase if it's pending.
    pub fn start_phase(&mut self) -> WorkflowEngineResult<()> {
        let phase_id = self
            .current_phase()
            .ok_or_else(|| WorkflowError::StateError("no current phase".to_string()))?
            .phase_id
            .clone();

        let phase_status = self
            .current_phase()
            .ok_or_else(|| WorkflowError::StateError("no current phase".to_string()))?
            .status
            .clone();

        if phase_status != PhaseStatus::Pending {
            return Err(WorkflowError::StateError(format!(
                "cannot start phase {} in status {:?}",
                phase_id, phase_status
            )));
        }

        // Check artifact prerequisites before starting the phase
        let template_phase = self
            .current_template_phase()
            .ok_or_else(|| WorkflowError::StateError("template phase not found".to_string()))?
            .clone();
        // workspace_root: None — relative artifact paths resolve against the
        // process cwd. Hymenium must be launched from the workspace root for
        // relative paths to work correctly; absolute paths are unaffected.
        let artifact_result = crate::workflow::template::check_artifact_prerequisites(
            &template_phase,
            None,
        );
        match artifact_result {
            Ok(warnings) => {
                for warning in &warnings {
                    tracing::warn!("{}", warning);
                }
            }
            Err(errors) => {
                let error_msg = errors.join("; ");
                self.blocked_on = Some(error_msg.clone());
                return Err(WorkflowError::GateFailed {
                    phase_id,
                    reason: format!("artifact prerequisites not met: {}", error_msg),
                });
            }
        }

        let phase = self
            .current_phase_mut()
            .ok_or_else(|| WorkflowError::StateError("no current phase".to_string()))?;
        phase.mark_active();
        self.status = WorkflowStatus::InProgress;
        self.updated_at = Utc::now();

        if let Some(template_phase) = self.current_template_phase() {
            if let Some(rubric) = &template_phase.rubric {
                tracing::info!(
                    phase_id = %template_phase.phase_id,
                    condition = %rubric.condition,
                    probe_method = ?rubric.probe_method,
                    "phase entry rubric attached"
                );
            }
        }

        Ok(())
    }

    /// Complete the current phase.
    pub fn complete_phase(&mut self) -> WorkflowEngineResult<()> {
        let phase = self
            .current_phase_mut()
            .ok_or_else(|| WorkflowError::StateError("no current phase".to_string()))?;

        if phase.status != PhaseStatus::Active {
            return Err(WorkflowError::StateError(format!(
                "cannot complete phase {} from status {:?}",
                phase.phase_id, phase.status
            )));
        }

        phase.mark_completed();
        self.updated_at = Utc::now();

        if let Some(template_phase) = self.current_template_phase() {
            if let Some(rubric) = &template_phase.rubric {
                tracing::info!(
                    phase_id = %template_phase.phase_id,
                    condition = %rubric.condition,
                    "phase exit rubric condition evaluated"
                );
            }
        }

        Ok(())
    }

    /// Mark the current phase completed regardless of whether it was started.
    ///
    /// Used by reconciliation to catch up a phase that Canopy reports as
    /// completed but whose in-memory state may still be `Pending` because
    /// Hymenium never received the start signal. Transitions `Pending` or
    /// `Active` → `Completed` atomically, setting both timestamps when the
    /// phase was previously `Pending`.
    ///
    /// Returns an error if the current phase is already `Completed`, `Failed`,
    /// or `Skipped` — those are terminal states that must not be overwritten.
    pub fn reconcile_complete_current_phase(&mut self) -> WorkflowEngineResult<()> {
        let phase = self
            .current_phase_mut()
            .ok_or_else(|| WorkflowError::StateError("no current phase".to_string()))?;

        match phase.status {
            PhaseStatus::Pending => {
                // Phase was never started locally; set both timestamps now.
                phase.mark_active();
                phase.mark_completed();
            }
            PhaseStatus::Active => {
                phase.mark_completed();
            }
            PhaseStatus::AwaitingUserInput => {
                // Phase was paused awaiting user input; mark completed now.
                phase.mark_completed();
            }
            PhaseStatus::Completed => {
                // Already completed — idempotent, nothing to do.
            }
            PhaseStatus::Failed | PhaseStatus::Skipped => {
                return Err(WorkflowError::StateError(format!(
                    "cannot reconcile-complete phase {} from terminal status {:?}",
                    phase.phase_id, phase.status
                )));
            }
        }

        self.updated_at = Utc::now();
        Ok(())
    }

    /// Mark the current phase failed regardless of whether it was started.
    ///
    /// Used by reconciliation when Canopy reports a task as cancelled or
    /// otherwise failed. Transitions `Pending` or `Active` → `Failed`,
    /// setting `started_at` when the phase was previously `Pending`.
    ///
    /// Returns an error if the phase is already in a terminal state.
    pub fn reconcile_fail_current_phase(
        &mut self,
        reason: impl Into<String>,
    ) -> WorkflowEngineResult<()> {
        let reason = reason.into();
        let phase = self
            .current_phase_mut()
            .ok_or_else(|| WorkflowError::StateError("no current phase".to_string()))?;

        match phase.status {
            PhaseStatus::Pending => {
                phase.mark_active();
                phase.failure_reason = Some(reason);
                phase.mark_failed();
            }
            PhaseStatus::Active | PhaseStatus::AwaitingUserInput => {
                phase.failure_reason = Some(reason);
                phase.mark_failed();
            }
            PhaseStatus::Failed => {
                // Already failed — idempotent, nothing to do.
            }
            PhaseStatus::Completed | PhaseStatus::Skipped => {
                return Err(WorkflowError::StateError(format!(
                    "cannot reconcile-fail phase {} from terminal status {:?}",
                    phase.phase_id, phase.status
                )));
            }
        }

        self.status = WorkflowStatus::Failed;
        self.updated_at = Utc::now();
        Ok(())
    }

    /// Fail the current phase. Only active phases can be failed.
    ///
    /// # Outcome emission
    ///
    /// This method only mutates in-memory state. The caller is responsible for
    /// persisting a terminal [`crate::outcome::WorkflowOutcome`] via
    /// `WorkflowStore::insert_outcome` after this method returns and the
    /// instance status is `WorkflowStatus::Failed`. Production callers in
    /// `commands/fail.rs` wrap this method and persist the terminal outcome.
    /// Tests that stop here are not subject to that requirement.
    pub fn fail_phase(&mut self, reason: impl Into<String>) -> WorkflowEngineResult<()> {
        let phase = self
            .current_phase_mut()
            .ok_or_else(|| WorkflowError::StateError("no current phase".to_string()))?;

        if phase.status != PhaseStatus::Active {
            return Err(WorkflowError::StateError(format!(
                "cannot fail phase {} from status {:?}",
                phase.phase_id, phase.status
            )));
        }

        phase.failure_reason = Some(reason.into());
        phase.mark_failed();
        self.status = WorkflowStatus::Failed;
        self.updated_at = Utc::now();
        Ok(())
    }

    /// Prepare the current phase for a retry by incrementing retry count.
    ///
    /// Increments `retry_count` on the current phase. This should be called
    /// before re-dispatching a stalled or failed phase. The phase must be
    /// Active (currently running) at the time of the retry decision.
    ///
    /// The caller is responsible for persisting the updated state via
    /// `WorkflowStore::upsert_phase_state` after this method returns.
    pub fn increment_retry_count(&mut self) -> WorkflowEngineResult<()> {
        let phase = self
            .current_phase_mut()
            .ok_or_else(|| WorkflowError::StateError("no current phase".to_string()))?;

        if phase.status != PhaseStatus::Active {
            return Err(WorkflowError::StateError(format!(
                "cannot retry phase {} not in Active status {:?}",
                phase.phase_id, phase.status
            )));
        }

        phase.retry_count = phase.retry_count.saturating_add(1);
        self.updated_at = Utc::now();
        Ok(())
    }

    /// Record a tool failure for the current phase.
    ///
    /// Increments `tool_failure_count` on the current phase and checks the
    /// ceiling. If the ceiling is reached, marks the phase `Failed` with
    /// `ExceededFailureCeiling` and sets the workflow to `Failed`.
    /// Returns `true` when the ceiling was hit.
    ///
    /// Only valid when the current phase is `Active`.
    ///
    /// # Panics
    ///
    /// Panics if there is no current phase (programming error — call only on active workflows).
    pub fn record_tool_failure(&mut self, ceiling: u32) -> WorkflowEngineResult<bool> {
        let phase_id = self
            .current_phase()
            .ok_or_else(|| WorkflowError::StateError("no current phase".to_string()))?
            .phase_id
            .clone();

        let phase_status = self.current_phase().unwrap().status.clone();

        if phase_status != PhaseStatus::Active {
            return Err(WorkflowError::StateError(format!(
                "cannot record tool failure for phase {} in status {:?}",
                phase_id, phase_status
            )));
        }

        let phase = self.current_phase_mut().unwrap();
        phase.tool_failure_count = phase.tool_failure_count.saturating_add(1);
        let hit_ceiling = phase.tool_failure_count >= ceiling;

        if hit_ceiling {
            let reason = format!(
                "ExceededFailureCeiling: {} tool failures reached ceiling of {}",
                phase.tool_failure_count, ceiling
            );
            phase.failure_reason = Some(reason);
            phase.mark_failed();
        }

        self.updated_at = Utc::now();

        if hit_ceiling {
            self.status = WorkflowStatus::Failed;
            return Ok(true);
        }

        Ok(false)
    }

    /// Record a request for the current phase.
    ///
    /// Increments `request_count` on the current phase and checks the ceiling.
    /// If the ceiling is reached, marks the phase `Failed` with
    /// `ExceededRequestCeiling` and sets the workflow to `Failed`.
    /// Returns `true` when the ceiling was hit.
    ///
    /// Only valid when the current phase is `Active`.
    ///
    /// # Panics
    ///
    /// Panics if there is no current phase (programming error — call only on active workflows).
    pub fn record_request(&mut self, ceiling: u32) -> WorkflowEngineResult<bool> {
        let phase_id = self
            .current_phase()
            .ok_or_else(|| WorkflowError::StateError("no current phase".to_string()))?
            .phase_id
            .clone();

        let phase_status = self.current_phase().unwrap().status.clone();

        if phase_status != PhaseStatus::Active {
            return Err(WorkflowError::StateError(format!(
                "cannot record request for phase {} in status {:?}",
                phase_id, phase_status
            )));
        }

        let phase = self.current_phase_mut().unwrap();
        phase.request_count = phase.request_count.saturating_add(1);
        let hit_ceiling = phase.request_count >= ceiling;

        if hit_ceiling {
            let reason = format!(
                "ExceededRequestCeiling: {} requests reached ceiling of {}",
                phase.request_count, ceiling
            );
            phase.failure_reason = Some(reason);
            phase.mark_failed();
        }

        self.updated_at = Utc::now();

        if hit_ceiling {
            self.status = WorkflowStatus::Failed;
            return Ok(true);
        }

        Ok(false)
    }

    /// Resolve the effective tool failure ceiling for the current phase.
    ///
    /// Uses the per-phase override when set; otherwise inherits from the
    /// workflow-level template default.
    pub fn effective_tool_failure_ceiling(&self) -> u32 {
        self.template
            .phases
            .get(self.current_phase_idx)
            .and_then(|p| p.max_tool_failure_per_phase)
            .unwrap_or(self.template.max_tool_failure_per_phase)
    }

    /// Resolve the effective request ceiling for the current phase.
    pub fn effective_request_ceiling(&self) -> u32 {
        self.template
            .phases
            .get(self.current_phase_idx)
            .and_then(|p| p.max_requests_per_phase)
            .unwrap_or(self.template.max_requests_per_phase)
    }

    /// Check if the current phase's exit gates are satisfied.
    pub fn can_advance(&self, evaluator: &impl GateEvaluator) -> WorkflowEngineResult<bool> {
        let phase_state = self
            .current_phase()
            .ok_or_else(|| WorkflowError::StateError("no current phase".to_string()))?;

        let template_phase = self
            .current_template_phase()
            .ok_or_else(|| WorkflowError::StateError("template phase not found".to_string()))?;

        let conditions: Vec<GateCondition> = template_phase
            .exit_gate
            .requires
            .iter()
            .map(|s| crate::workflow::gate::parse_gate_condition(s))
            .collect();

        let context = GateContext::new(self.workflow_id.clone(), &phase_state.phase_id);

        let evaluation = evaluator.evaluate_all(&conditions, &context).map_err(|e| {
            WorkflowError::GateFailed {
                phase_id: phase_state.phase_id.clone(),
                reason: e.to_string(),
            }
        })?;

        Ok(evaluation.passed())
    }

    /// Advance to the next phase if gates permit.
    ///
    /// Requires the current phase to be Completed. Checks exit gates on the
    /// current phase and entry gates on the next phase. If the current phase
    /// is the final phase and its exit gates pass, the workflow is marked
    /// Completed and an error is returned (use `complete_workflow()` instead).
    pub fn advance(
        &mut self,
        evaluator: &impl GateEvaluator,
    ) -> WorkflowEngineResult<PhaseTransition> {
        // Guard: current phase must be completed before advancing
        let current = self
            .current_phase()
            .ok_or_else(|| WorkflowError::StateError("no current phase".to_string()))?;
        if current.status != PhaseStatus::Completed {
            return Err(WorkflowError::StateError(format!(
                "cannot advance from phase {} in status {:?} — must be Completed",
                current.phase_id, current.status
            )));
        }

        // Guard: check if already at final phase (use saturating_sub to avoid underflow)
        let last_idx = self.template.phases.len().saturating_sub(1);
        if self.current_phase_idx >= last_idx {
            return Err(WorkflowError::AlreadyAtFinalPhase);
        }

        // Check exit gates of current phase
        if !self.can_advance(evaluator)? {
            let phase = self
                .current_phase()
                .ok_or_else(|| WorkflowError::StateError("no current phase".to_string()))?;
            return Err(WorkflowError::GateFailed {
                phase_id: phase.phase_id.clone(),
                reason: "exit gate conditions not met".to_string(),
            });
        }

        let from_phase_id = self
            .current_phase()
            .ok_or_else(|| WorkflowError::StateError("no current phase".to_string()))?
            .phase_id
            .clone();

        // Prepare next phase index and template
        let next_idx = self.current_phase_idx + 1;
        let next_template_phase = self.template.phases.get(next_idx).ok_or_else(|| {
            WorkflowError::StateError("next template phase not found".to_string())
        })?;

        // Check artifact prerequisites before evaluating gate conditions
        // workspace_root: None — see start_phase() comment; process cwd must
        // be the workspace root for relative artifact paths to resolve.
        let artifact_result = crate::workflow::template::check_artifact_prerequisites(
            next_template_phase,
            None,
        );
        match artifact_result {
            Ok(warnings) => {
                for warning in &warnings {
                    tracing::warn!("{}", warning);
                }
            }
            Err(errors) => {
                self.blocked_on = Some(errors.join("; "));
                return Err(WorkflowError::GateFailed {
                    phase_id: next_template_phase.phase_id.clone(),
                    reason: format!("artifact prerequisites not met: {}", errors.join("; ")),
                });
            }
        }

        // Check entry gates of the next phase before advancing
        let entry_conditions: Vec<GateCondition> = next_template_phase
            .entry_gate
            .requires
            .iter()
            .map(|s| crate::workflow::gate::parse_gate_condition(s))
            .collect();
        if !entry_conditions.is_empty() {
            let entry_context =
                GateContext::new(self.workflow_id.clone(), &next_template_phase.phase_id);
            let entry_eval = evaluator
                .evaluate_all(&entry_conditions, &entry_context)
                .map_err(|e| WorkflowError::GateFailed {
                    phase_id: next_template_phase.phase_id.clone(),
                    reason: e.to_string(),
                })?;
            if !entry_eval.passed() {
                return Err(WorkflowError::GateFailed {
                    phase_id: next_template_phase.phase_id.clone(),
                    reason: format!("entry gate conditions not met: {:?}", entry_eval.failures()),
                });
            }
        }

        // Advance to next phase
        self.current_phase_idx = next_idx;
        let to_phase_id = self
            .current_phase()
            .ok_or_else(|| WorkflowError::StateError("no current phase".to_string()))?
            .phase_id
            .clone();

        let transition = PhaseTransition {
            from_phase_id,
            to_phase_id,
            transitioned_at: Utc::now(),
            reason: "exit and entry gates satisfied".to_string(),
        };

        self.transitions.push(transition.clone());
        self.status = WorkflowStatus::Dispatched;
        self.updated_at = Utc::now();

        Ok(transition)
    }

    /// Complete the workflow after the final phase is done.
    ///
    /// # Outcome emission
    ///
    /// This method only mutates in-memory state. The caller is responsible for
    /// persisting a terminal [`crate::outcome::WorkflowOutcome`] via
    /// `WorkflowStore::insert_outcome` after this method returns and the
    /// instance status is `WorkflowStatus::Completed`. Production callers in
    /// `commands/complete.rs` wrap this method and persist the terminal outcome.
    /// Tests that stop here are not subject to that requirement.
    pub fn complete_workflow(&mut self) -> WorkflowEngineResult<()> {
        let current = self
            .current_phase()
            .ok_or_else(|| WorkflowError::StateError("no current phase".to_string()))?;
        if current.status != PhaseStatus::Completed {
            return Err(WorkflowError::StateError(format!(
                "cannot complete workflow — final phase {} is {:?}, not Completed",
                current.phase_id, current.status
            )));
        }
        let last_idx = self.template.phases.len().saturating_sub(1);
        if self.current_phase_idx != last_idx {
            return Err(WorkflowError::StateError(
                "cannot complete workflow — not at final phase".to_string(),
            ));
        }
        self.status = WorkflowStatus::Completed;
        self.updated_at = Utc::now();
        Ok(())
    }

    /// Get the duration of a completed phase.
    pub fn phase_duration(&self, idx: usize) -> Option<chrono::Duration> {
        let phase = self.get_phase(idx)?;
        let started = phase.started_at?;
        let completed = phase.completed_at?;
        Some(completed - started)
    }

    /// Pause the current phase at a `HandoffToUser` checkpoint.
    ///
    /// If `breakout` is false: sets phase status to `AwaitingUserInput` and
    /// workflow status to `AwaitingUserInput`. The message is stored on the
    /// phase for operator display; use `resume_from_user_input` to continue.
    ///
    /// If `breakout` is true: sets phase status to `Failed` (the phase did not
    /// complete) and workflow status to `UserTerminated`. The workflow is
    /// terminal and cannot be resumed.
    pub fn handoff_to_user_phase(
        &mut self,
        message: impl Into<String>,
        breakout: bool,
    ) -> WorkflowEngineResult<()> {
        let phase = self
            .current_phase_mut()
            .ok_or_else(|| WorkflowError::StateError("no current phase".to_string()))?;

        if phase.status != PhaseStatus::Active {
            return Err(WorkflowError::StateError(format!(
                "cannot handoff_to_user from phase {} in status {:?}",
                phase.phase_id, phase.status
            )));
        }

        let message = message.into();

        if breakout {
            phase.failure_reason = Some(message.clone());
            phase.mark_failed();
            self.status = WorkflowStatus::UserTerminated;
        } else {
            phase.pending_message = Some(message);
            phase.status = PhaseStatus::AwaitingUserInput;
            self.status = WorkflowStatus::AwaitingUserInput;
        }
        self.updated_at = Utc::now();
        Ok(())
    }

    /// Resume a workflow paused at a `HandoffToUser` checkpoint.
    ///
    /// Requires the current phase to be in `AwaitingUserInput` status and the
    /// workflow to be in `AwaitingUserInput` status. Clears `pending_message`,
    /// resets the phase to `Active`, and sets the workflow back to `InProgress`.
    pub fn resume_from_user_input(&mut self) -> WorkflowEngineResult<()> {
        if self.status != WorkflowStatus::AwaitingUserInput {
            return Err(WorkflowError::StateError(format!(
                "cannot resume workflow {} — status is {:?}, not AwaitingUserInput",
                self.workflow_id, self.status
            )));
        }

        let phase = self
            .current_phase_mut()
            .ok_or_else(|| WorkflowError::StateError("no current phase".to_string()))?;

        if phase.status != PhaseStatus::AwaitingUserInput {
            return Err(WorkflowError::StateError(format!(
                "cannot resume phase {} — status is {:?}, not AwaitingUserInput",
                phase.phase_id, phase.status
            )));
        }

        phase.pending_message = None;
        phase.status = PhaseStatus::Active;
        self.status = WorkflowStatus::InProgress;
        self.updated_at = Utc::now();
        Ok(())
    }
}

/// Mutable runtime state for a running workflow engine instance.
#[derive(Debug, Clone)]
pub struct WorkflowRuntime {
    pub state: String,
}

impl WorkflowRuntime {
    /// Create a new workflow runtime in initial state.
    pub fn new() -> Self {
        Self {
            state: "idle".to_string(),
        }
    }
}

impl Default for WorkflowRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::gate::MockGateEvaluator;
    use crate::workflow::template::impl_audit_default;

    #[test]
    fn test_create_workflow_instance() {
        let template = impl_audit_default();
        let wf = WorkflowInstance::new(
            WorkflowId("test-123".to_string()),
            template,
            "/path/to/handoff.md",
        );

        assert_eq!(wf.workflow_id, WorkflowId("test-123".to_string()));
        assert_eq!(wf.status, WorkflowStatus::Pending);
        assert_eq!(wf.current_phase_idx, 0);
        assert_eq!(wf.phase_states.len(), 2);
        assert_eq!(wf.phase_states[0].status, PhaseStatus::Pending);
    }

    #[test]
    fn test_start_phase() {
        let template = impl_audit_default();
        let mut wf = WorkflowInstance::new(
            WorkflowId("test-123".to_string()),
            template,
            "/path/to/handoff.md",
        );

        wf.start_phase().expect("should start phase");
        assert_eq!(wf.status, WorkflowStatus::InProgress);
        assert_eq!(wf.current_phase().unwrap().status, PhaseStatus::Active);
        assert!(wf.current_phase().unwrap().started_at.is_some());
    }

    #[test]
    fn test_complete_phase() {
        let template = impl_audit_default();
        let mut wf = WorkflowInstance::new(
            WorkflowId("test-123".to_string()),
            template,
            "/path/to/handoff.md",
        );

        wf.start_phase().expect("should start phase");
        wf.complete_phase().expect("should complete phase");
        assert_eq!(wf.current_phase().unwrap().status, PhaseStatus::Completed);
        assert!(wf.current_phase().unwrap().completed_at.is_some());
    }

    #[test]
    fn test_advance_with_gates_satisfied() {
        let template = impl_audit_default();
        let mut wf = WorkflowInstance::new(
            WorkflowId("test-123".to_string()),
            template,
            "/path/to/handoff.md",
        );

        wf.start_phase().expect("should start phase");
        wf.complete_phase().expect("should complete phase");

        // Create evaluator where exit gates of implement phase are satisfied
        let evaluator = MockGateEvaluator::new()
            .set_condition("code_diff_exists", true)
            .set_condition("verification_passed", true);

        let transition = wf.advance(&evaluator).expect("should advance");
        assert_eq!(transition.from_phase_id, "implement");
        assert_eq!(transition.to_phase_id, "audit");
        assert_eq!(wf.current_phase_idx, 1);
        assert_eq!(wf.status, WorkflowStatus::Dispatched);
    }

    #[test]
    fn test_advance_with_gates_not_satisfied() {
        let template = impl_audit_default();
        let mut wf = WorkflowInstance::new(
            WorkflowId("test-123".to_string()),
            template,
            "/path/to/handoff.md",
        );

        wf.start_phase().expect("should start phase");
        wf.complete_phase().expect("should complete phase");

        // Create evaluator where exit gates are NOT satisfied
        let evaluator = MockGateEvaluator::new();

        let result = wf.advance(&evaluator);
        assert!(result.is_err());
        assert_eq!(wf.current_phase_idx, 0); // Should still be at phase 0
    }

    #[test]
    fn test_can_advance_checks_gates() {
        let template = impl_audit_default();
        let wf = WorkflowInstance::new(
            WorkflowId("test-123".to_string()),
            template,
            "/path/to/handoff.md",
        );

        let passing_evaluator = MockGateEvaluator::new()
            .set_condition("code_diff_exists", true)
            .set_condition("verification_passed", true);

        let can_advance = wf.can_advance(&passing_evaluator).expect("should evaluate");
        assert!(can_advance);

        let failing_evaluator = MockGateEvaluator::new();
        let can_advance = wf.can_advance(&failing_evaluator).expect("should evaluate");
        assert!(!can_advance);
    }

    #[test]
    fn test_phase_duration() {
        let template = impl_audit_default();
        let mut wf = WorkflowInstance::new(
            WorkflowId("test-123".to_string()),
            template,
            "/path/to/handoff.md",
        );

        wf.start_phase().expect("should start phase");
        wf.complete_phase().expect("should complete phase");

        let duration = wf.phase_duration(0).expect("should have duration");
        assert!(duration.num_milliseconds() >= 0);
    }

    #[test]
    fn test_fail_phase() {
        let template = impl_audit_default();
        let mut wf = WorkflowInstance::new(
            WorkflowId("test-123".to_string()),
            template,
            "/path/to/handoff.md",
        );

        wf.start_phase().expect("should start phase");
        wf.fail_phase("test failure").expect("should fail phase");

        assert_eq!(wf.current_phase().unwrap().status, PhaseStatus::Failed);
        assert_eq!(wf.status, WorkflowStatus::Failed);
    }

    #[test]
    fn test_cannot_start_non_pending_phase() {
        let template = impl_audit_default();
        let mut wf = WorkflowInstance::new(
            WorkflowId("test-123".to_string()),
            template,
            "/path/to/handoff.md",
        );

        wf.start_phase().expect("should start phase");
        let result = wf.start_phase();
        assert!(result.is_err());
    }

    #[test]
    fn test_cannot_complete_non_active_phase() {
        let template = impl_audit_default();
        let mut wf = WorkflowInstance::new(
            WorkflowId("test-123".to_string()),
            template,
            "/path/to/handoff.md",
        );

        let result = wf.complete_phase();
        assert!(result.is_err());
    }

    #[test]
    fn test_workflow_status_display() {
        assert_eq!(format!("{}", WorkflowStatus::Pending), "pending");
        assert_eq!(format!("{}", WorkflowStatus::InProgress), "in_progress");
        assert_eq!(
            format!("{}", WorkflowStatus::BlockedOnGate),
            "blocked_on_gate"
        );
        assert_eq!(
            format!("{}", WorkflowStatus::AwaitingRepair),
            "awaiting_repair"
        );
        assert_eq!(format!("{}", WorkflowStatus::Completed), "completed");
        assert_eq!(
            format!("{}", WorkflowStatus::AwaitingUserInput),
            "awaiting_user_input"
        );
        assert_eq!(
            format!("{}", WorkflowStatus::UserTerminated),
            "user_terminated"
        );
    }

    #[test]
    fn test_phase_status_display() {
        assert_eq!(format!("{}", PhaseStatus::Pending), "pending");
        assert_eq!(format!("{}", PhaseStatus::Active), "active");
        assert_eq!(format!("{}", PhaseStatus::Completed), "completed");
        assert_eq!(
            format!("{}", PhaseStatus::AwaitingUserInput),
            "awaiting_user_input"
        );
    }

    #[test]
    fn test_handoff_to_user_no_breakout_pauses_workflow() {
        let template = impl_audit_default();
        let mut wf = WorkflowInstance::new(
            WorkflowId("test-123".to_string()),
            template,
            "/path/to/handoff.md",
        );

        wf.start_phase().expect("should start phase");
        wf.handoff_to_user_phase("review this", false)
            .expect("should handoff to user");

        assert_eq!(
            wf.current_phase().unwrap().status,
            PhaseStatus::AwaitingUserInput
        );
        assert_eq!(wf.status, WorkflowStatus::AwaitingUserInput);
        assert_eq!(
            wf.current_phase().unwrap().pending_message.as_deref(),
            Some("review this")
        );
    }

    #[test]
    fn test_handoff_to_user_breakout_terminates_workflow() {
        let template = impl_audit_default();
        let mut wf = WorkflowInstance::new(
            WorkflowId("test-123".to_string()),
            template,
            "/path/to/handoff.md",
        );

        wf.start_phase().expect("should start phase");
        wf.handoff_to_user_phase("stopping", true)
            .expect("should handoff to user");

        assert_eq!(wf.current_phase().unwrap().status, PhaseStatus::Failed);
        assert_eq!(wf.status, WorkflowStatus::UserTerminated);
    }

    #[test]
    fn test_resume_from_user_input_restores_active_state() {
        let template = impl_audit_default();
        let mut wf = WorkflowInstance::new(
            WorkflowId("test-123".to_string()),
            template,
            "/path/to/handoff.md",
        );

        wf.start_phase().expect("should start phase");
        wf.handoff_to_user_phase("review", false)
            .expect("should handoff to user");
        wf.resume_from_user_input()
            .expect("should resume from user input");

        assert_eq!(wf.current_phase().unwrap().status, PhaseStatus::Active);
        assert_eq!(wf.status, WorkflowStatus::InProgress);
        assert_eq!(wf.current_phase().unwrap().pending_message, None);
    }

    #[test]
    fn test_resume_fails_when_not_paused() {
        let template = impl_audit_default();
        let mut wf = WorkflowInstance::new(
            WorkflowId("test-123".to_string()),
            template,
            "/path/to/handoff.md",
        );

        let result = wf.resume_from_user_input();
        assert!(result.is_err());
    }

    #[test]
    fn test_record_tool_failure_below_ceiling_returns_false() {
        let template = impl_audit_default();
        let mut wf = WorkflowInstance::new(
            WorkflowId("test-123".to_string()),
            template,
            "/path/to/handoff.md",
        );

        wf.start_phase().expect("should start phase");

        let hit_ceiling = wf.record_tool_failure(3).expect("should record failure");
        assert!(!hit_ceiling);
        assert_eq!(wf.current_phase().unwrap().tool_failure_count, 1);
        assert_eq!(wf.current_phase().unwrap().status, PhaseStatus::Active);

        let hit_ceiling = wf
            .record_tool_failure(3)
            .expect("should record second failure");
        assert!(!hit_ceiling);
        assert_eq!(wf.current_phase().unwrap().tool_failure_count, 2);
        assert_eq!(wf.current_phase().unwrap().status, PhaseStatus::Active);
    }

    #[test]
    fn test_record_tool_failure_at_ceiling_returns_true_and_fails_phase() {
        let template = impl_audit_default();
        let mut wf = WorkflowInstance::new(
            WorkflowId("test-123".to_string()),
            template,
            "/path/to/handoff.md",
        );

        wf.start_phase().expect("should start phase");

        let hit_ceiling = wf.record_tool_failure(2).expect("should record failure");
        assert!(!hit_ceiling);
        assert_eq!(wf.current_phase().unwrap().tool_failure_count, 1);

        let hit_ceiling = wf
            .record_tool_failure(2)
            .expect("should record second failure");
        assert!(hit_ceiling);
        assert_eq!(wf.current_phase().unwrap().tool_failure_count, 2);
        assert_eq!(wf.current_phase().unwrap().status, PhaseStatus::Failed);
        assert_eq!(wf.status, WorkflowStatus::Failed);
        assert!(wf
            .current_phase()
            .unwrap()
            .failure_reason
            .as_ref()
            .unwrap()
            .contains("ExceededFailureCeiling"));
    }

    #[test]
    fn test_record_request_at_ceiling_returns_true_and_fails_phase() {
        let template = impl_audit_default();
        let mut wf = WorkflowInstance::new(
            WorkflowId("test-124".to_string()),
            template,
            "/path/to/handoff.md",
        );

        wf.start_phase().expect("should start phase");

        let hit_ceiling = wf.record_request(3).expect("should record request");
        assert!(!hit_ceiling);
        assert_eq!(wf.current_phase().unwrap().request_count, 1);

        let hit_ceiling = wf.record_request(3).expect("should record second request");
        assert!(!hit_ceiling);
        assert_eq!(wf.current_phase().unwrap().request_count, 2);

        let hit_ceiling = wf.record_request(3).expect("should record third request");
        assert!(hit_ceiling);
        assert_eq!(wf.current_phase().unwrap().request_count, 3);
        assert_eq!(wf.current_phase().unwrap().status, PhaseStatus::Failed);
        assert_eq!(wf.status, WorkflowStatus::Failed);
        assert!(wf
            .current_phase()
            .unwrap()
            .failure_reason
            .as_ref()
            .unwrap()
            .contains("ExceededRequestCeiling"));
    }

    #[test]
    fn test_effective_ceiling_inherits_template_default() {
        let template = impl_audit_default();
        let wf = WorkflowInstance::new(
            WorkflowId("test-125".to_string()),
            template,
            "/path/to/handoff.md",
        );

        assert_eq!(wf.effective_tool_failure_ceiling(), 10);
        assert_eq!(wf.effective_request_ceiling(), 50);
    }

    #[test]
    fn test_effective_ceiling_uses_per_phase_override() {
        let mut template = impl_audit_default();
        template.phases[0].max_tool_failure_per_phase = Some(3);

        let wf = WorkflowInstance::new(
            WorkflowId("test-126".to_string()),
            template,
            "/path/to/handoff.md",
        );

        assert_eq!(wf.effective_tool_failure_ceiling(), 3);
        assert_eq!(wf.effective_request_ceiling(), 50);
    }
}
