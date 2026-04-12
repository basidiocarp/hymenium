//! Workflow orchestration.
//!
//! Manages the lifecycle of handoff workflows, including template loading,
//! phase transitions, and gate evaluation.

pub mod engine;
pub mod gate;
pub mod template;

// Re-export commonly used types
pub use engine::{PhaseState, PhaseStatus, PhaseTransition, WorkflowInstance, WorkflowStatus};
pub use gate::{GateCondition, GateContext, GateEvaluator, GateEvaluation, MockGateEvaluator};
pub use template::{AgentRole, AgentTier, Gate, Phase, TemplateRegistry, WorkflowTemplate};

/// Strongly-typed workflow identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct WorkflowId(pub String);

impl std::fmt::Display for WorkflowId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Represents a workflow instance.
#[derive(Debug, Clone)]
pub struct Workflow {
    pub id: WorkflowId,
    pub phase: String,
}

impl Workflow {
    /// Create a new workflow with the given ID.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: WorkflowId(id.into()),
            phase: "initialized".to_string(),
        }
    }
}
