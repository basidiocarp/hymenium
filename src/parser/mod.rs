//! Handoff markdown parsing.
//!
//! Extracts structured information from handoff markdown documents,
//! including metadata blocks and task decomposition.

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub mod markdown;
pub mod metadata;

// Re-export the main parser function
pub use markdown::parse_handoff;

/// Error type for handoff parsing.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ParseError {
    /// Missing required section
    #[error("missing required section: {0}")]
    MissingSection(String),
    /// Invalid metadata format
    #[error("invalid metadata: {0}")]
    InvalidMetadata(String),
    /// Invalid step format
    #[error("invalid step format: {0}")]
    InvalidStep(String),
    /// Malformed code block or list
    #[error("malformed block: {0}")]
    MalformedBlock(String),
}

/// Dispatch type for a handoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Dispatchability {
    /// Direct single-agent dispatch
    Direct,
    /// Multi-step umbrella dispatch with sub-tasks
    Umbrella,
}

/// Parsed representation of a handoff document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedHandoff {
    pub title: String,
    pub metadata: Option<HandoffMetadata>,
    pub problem: String,
    pub state: Vec<String>,
    pub intent: String,
    pub steps: Vec<ParsedStep>,
    pub completion_protocol: Option<String>,
    pub context: Option<String>,
}

/// Metadata block from a handoff document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffMetadata {
    pub dispatchability: Dispatchability,
    pub owning_repo: String,
    pub allowed_write_scope: Vec<String>,
    pub cross_repo_rule: Option<String>,
    pub non_goals: Vec<String>,
    pub verification_contract: String,
    pub completion_update: String,
}

/// A single step within a handoff.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedStep {
    pub number: u32,
    pub title: String,
    pub project: Option<String>,
    pub effort: Option<String>,
    pub depends_on: Vec<String>,
    pub description: String,
    pub files_to_modify: Vec<FileModification>,
    pub verification: Option<VerificationBlock>,
    pub checklist: Vec<ChecklistItem>,
}

/// A file to be modified in a step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileModification {
    pub path: String,
    pub description: String,
}

/// Verification commands and paste markers for a step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationBlock {
    pub commands: Vec<String>,
    pub paste_markers: Vec<PasteMarker>,
}

/// A paste marker location in verification output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PasteMarker {
    pub line_number: usize,
    pub has_content: bool,
}

/// A single checklist item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChecklistItem {
    pub text: String,
    pub checked: bool,
}
