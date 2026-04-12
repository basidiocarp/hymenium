//! Agent dispatch via canopy.
//!
//! Translates workflow phases into canopy task operations. This module is the
//! only outbound write surface to canopy — it creates tasks, assigns agents,
//! and checks completeness, but never accesses canopy's database directly.

mod cli;
mod mock;
mod orchestrate;

// Re-export everything that was public in the original dispatch.rs so external
// callers see no change.
pub use cli::CliCanopyClient;
pub use mock::MockCanopyClient;
pub use orchestrate::{agent_name, dispatch_workflow, handoff_slug};

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
}

/// Detail record for a canopy task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDetail {
    pub task_id: String,
    pub title: String,
    pub status: String,
    pub agent_id: Option<String>,
    pub parent_id: Option<String>,
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
    fn assign_task(&self, task_id: &str, agent_id: &str) -> Result<(), DispatchError>;

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
}
