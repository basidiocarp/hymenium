//! Workflow state machine and phase transitions.
//!
//! Implements the core state machine logic for advancing workflows through phases.
//! The `WorkflowEngine` trait is defined in CLAUDE.md and will be implemented here
//! once the workflow template engine (#118e) is built.

use crate::workflow::{WorkflowId, gate::{GateCondition, GateContext, GateEvaluator}, template::WorkflowTemplate};
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

    #[error("workflow state error: {0}")]
    StateError(String),
}

/// Result type for workflow engine operations.
pub type WorkflowEngineResult<T> = Result<T, WorkflowError>;

/// Status of a running workflow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum WorkflowStatus {
    Pending,
    Dispatched,
    InProgress,
    Blocked,
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
            WorkflowStatus::Blocked => write!(f, "blocked"),
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
    Completed,
    Failed,
    Skipped,
}

impl std::fmt::Display for PhaseStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PhaseStatus::Pending => write!(f, "pending"),
            PhaseStatus::Active => write!(f, "active"),
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
    pub status: PhaseStatus,
    pub agent_id: Option<String>,
    pub canopy_task_id: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub failure_reason: Option<String>,
}

impl PhaseState {
    /// Create a new pending phase state.
    fn new(phase_id: impl Into<String>) -> Self {
        Self {
            phase_id: phase_id.into(),
            status: PhaseStatus::Pending,
            agent_id: None,
            canopy_task_id: None,
            started_at: None,
            completed_at: None,
            failure_reason: None,
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
            .map(|phase| PhaseState::new(&phase.phase_id))
            .collect();

        Self {
            workflow_id,
            template,
            handoff_path: handoff_path.into(),
            status: WorkflowStatus::Pending,
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
        let phase = self
            .current_phase_mut()
            .ok_or_else(|| WorkflowError::StateError("no current phase".to_string()))?;

        if phase.status != PhaseStatus::Pending {
            return Err(WorkflowError::StateError(format!(
                "cannot start phase {} in status {:?}",
                phase.phase_id, phase.status
            )));
        }

        phase.mark_active();
        self.status = WorkflowStatus::InProgress;
        self.updated_at = Utc::now();
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
        Ok(())
    }

    /// Fail the current phase. Only active phases can be failed.
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

    /// Check if the current phase's exit gates are satisfied.
    pub fn can_advance(&self, evaluator: &impl GateEvaluator) -> WorkflowEngineResult<bool> {
        let phase_state = self
            .current_phase()
            .ok_or_else(|| WorkflowError::StateError("no current phase".to_string()))?;

        let template_phase = self.current_template_phase().ok_or_else(|| {
            WorkflowError::StateError("template phase not found".to_string())
        })?;

        let conditions: Vec<GateCondition> = template_phase
            .exit_gate
            .requires
            .iter()
            .map(|s| crate::workflow::gate::parse_gate_condition(s))
            .collect();

        let context = GateContext::new(self.workflow_id.clone(), &phase_state.phase_id);

        let evaluation = evaluator
            .evaluate_all(&conditions, &context)
            .map_err(|e| WorkflowError::GateFailed {
                phase_id: phase_state.phase_id.clone(),
                reason: e.to_string(),
            })?;

        Ok(evaluation.passed())
    }

    /// Advance to the next phase if gates permit.
    ///
    /// Requires the current phase to be Completed. Checks exit gates on the
    /// current phase and entry gates on the next phase. If the current phase
    /// is the final phase and its exit gates pass, the workflow is marked
    /// Completed and an error is returned (use `complete_workflow()` instead).
    pub fn advance(&mut self, evaluator: &impl GateEvaluator) -> WorkflowEngineResult<PhaseTransition> {
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
            return Err(WorkflowError::StateError(
                "already at final phase — call complete_workflow() to finish".to_string(),
            ));
        }

        // Check exit gates of current phase
        if !self.can_advance(evaluator)? {
            let phase = self.current_phase().ok_or_else(|| {
                WorkflowError::StateError("no current phase".to_string())
            })?;
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

        // Check entry gates of the next phase before advancing
        let next_idx = self.current_phase_idx + 1;
        let next_template_phase = self.template.phases.get(next_idx).ok_or_else(|| {
            WorkflowError::StateError("next template phase not found".to_string())
        })?;
        let entry_conditions: Vec<GateCondition> = next_template_phase
            .entry_gate
            .requires
            .iter()
            .map(|s| crate::workflow::gate::parse_gate_condition(s))
            .collect();
        if !entry_conditions.is_empty() {
            let entry_context = GateContext::new(
                self.workflow_id.clone(),
                &next_template_phase.phase_id,
            );
            let entry_eval = evaluator
                .evaluate_all(&entry_conditions, &entry_context)
                .map_err(|e| WorkflowError::GateFailed {
                    phase_id: next_template_phase.phase_id.clone(),
                    reason: e.to_string(),
                })?;
            if !entry_eval.passed() {
                return Err(WorkflowError::GateFailed {
                    phase_id: next_template_phase.phase_id.clone(),
                    reason: format!(
                        "entry gate conditions not met: {:?}",
                        entry_eval.failures()
                    ),
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
        assert_eq!(format!("{}", WorkflowStatus::Completed), "completed");
    }

    #[test]
    fn test_phase_status_display() {
        assert_eq!(format!("{}", PhaseStatus::Pending), "pending");
        assert_eq!(format!("{}", PhaseStatus::Active), "active");
        assert_eq!(format!("{}", PhaseStatus::Completed), "completed");
    }
}
