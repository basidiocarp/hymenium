//! `SQLite` persistence for workflow state.
//!
//! Manages durable storage of workflow state, progress, and history.

use std::path::PathBuf;

/// Represents the workflow state store.
#[derive(Debug, Clone)]
pub struct WorkflowStore {
    pub db_path: PathBuf,
}

impl WorkflowStore {
    /// Create a new workflow store at the given path.
    pub fn new(db_path: impl Into<PathBuf>) -> Self {
        Self {
            db_path: db_path.into(),
        }
    }
}
