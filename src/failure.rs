//! Canonical workflow failure taxonomy.
//!
//! This module defines the **internal** classification layer for why a phase or
//! workflow failed. [`FailureKind`] is the branching key for retry and escalation
//! decisions; it is distinct from the wire-level `failure_type` in the
//! `workflow-outcome-v1` septa contract (see [`crate::outcome`]).
//!
//! # Two-layer design
//!
//! ```text
//! Detection site
//!     │
//!     ▼
//! FailureKind   ──── decides ────►  RecoveryAction  (retry / escalate / cancel)
//!     │
//!     ▼ (via FailureKind::to_terminal_failure_type)
//! TerminalFailureType  (in WorkflowOutcome — septa wire shape)
//! ```
//!
//! `FailureKind` answers "why did this phase fail?" and drives retry routing.
//! `TerminalFailureType` answers "how should we categorise the terminal outcome?"
//! for operators and downstream analytics.

use serde::{Deserialize, Serialize};

/// The canonical set of workflow failure causes.
///
/// These seven variants cover every detection-site category supported by the
/// current orchestration model. The enum is `#[non_exhaustive]` because the
/// taxonomy may grow — new variants must be accompanied by updated retry
/// branching and outcome mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FailureKind {
    /// The work request is ambiguous or underspecified; the agent cannot
    /// proceed without operator clarification. Recovery: always escalate.
    SpecAmbiguity,

    /// The task scope exceeds the agent's context budget and must be
    /// decomposed into smaller units before dispatch. Recovery: retry with
    /// narrowed scope on the first attempt, then escalate.
    TaskTooLarge,

    /// A prerequisite task has not yet completed; this phase cannot start.
    /// Recovery: cancel (dependency gating must be resolved first).
    MissingDependency,

    /// The agent ran but produced only partial output. The work may be
    /// continuable. Recovery: retry within `max_retries`, then escalate.
    ExecutionIncomplete,

    /// The agent wrote outside its allowed scope (e.g. modified files it was
    /// not permitted to touch). Retrying is unsafe without narrowed bounds.
    /// Recovery: escalate after the first occurrence.
    ScopeViolation,

    /// The agent's output does not conform to the expected contract or schema.
    /// Retrying blindly would produce the same mismatch. Recovery: escalate.
    ContractMismatch,

    /// The output is largely correct but contains a small, targeted defect.
    /// Recovery: retry once for a focused repair loop; do not narrow scope.
    MinorDefect,
}

impl FailureKind {
    /// Return a short human-readable label suitable for operator display.
    pub fn label(self) -> &'static str {
        match self {
            FailureKind::SpecAmbiguity => "spec ambiguity",
            FailureKind::TaskTooLarge => "task too large",
            FailureKind::MissingDependency => "missing dependency",
            FailureKind::ExecutionIncomplete => "execution incomplete",
            FailureKind::ScopeViolation => "scope violation",
            FailureKind::ContractMismatch => "contract mismatch",
            FailureKind::MinorDefect => "minor defect",
        }
    }
}

impl std::fmt::Display for FailureKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// A typed failure carrying both the detection-site kind and an optional
/// operator-visible detail string.
///
/// The `kind` field is the branching key for retry routing and outcome
/// categorisation. The `detail` field annotates — it does **not** replace
/// `kind` as the discriminant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypedFailure {
    pub kind: FailureKind,
    /// Free-form context string from the detection site (e.g. which gate
    /// failed, which file was out of scope). Optional.
    pub detail: Option<String>,
}

impl TypedFailure {
    /// Construct a failure with no additional detail.
    pub fn new(kind: FailureKind) -> Self {
        Self { kind, detail: None }
    }

    /// Construct a failure with operator-visible detail.
    pub fn with_detail(kind: FailureKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: Some(detail.into()),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// All seven canonical variants — update this when a variant is added.
    const ALL_VARIANTS: &[FailureKind] = &[
        FailureKind::SpecAmbiguity,
        FailureKind::TaskTooLarge,
        FailureKind::MissingDependency,
        FailureKind::ExecutionIncomplete,
        FailureKind::ScopeViolation,
        FailureKind::ContractMismatch,
        FailureKind::MinorDefect,
    ];

    #[test]
    fn variant_count_matches_canonical_seven() {
        // If you add an 8th variant without updating ALL_VARIANTS this test will
        // not catch it automatically, but the compile error on incomplete match
        // arms elsewhere will. This test at least documents the expected count.
        assert_eq!(
            ALL_VARIANTS.len(),
            7,
            "expected exactly 7 FailureKind variants"
        );
    }

    #[test]
    fn each_variant_roundtrips_through_json() {
        for &kind in ALL_VARIANTS {
            let json = serde_json::to_string(&kind)
                .unwrap_or_else(|e| panic!("serialize {kind:?} failed: {e}"));
            let roundtripped: FailureKind = serde_json::from_str(&json)
                .unwrap_or_else(|e| panic!("deserialize {kind:?} from {json} failed: {e}"));
            assert_eq!(kind, roundtripped, "round-trip mismatch for {kind:?}");
        }
    }

    #[test]
    fn each_variant_has_non_empty_label() {
        for &kind in ALL_VARIANTS {
            let label = kind.label();
            assert!(
                !label.is_empty(),
                "FailureKind::{kind:?} returned an empty label"
            );
        }
    }

    #[test]
    fn display_matches_label() {
        for &kind in ALL_VARIANTS {
            assert_eq!(
                format!("{kind}"),
                kind.label(),
                "Display for {kind:?} does not match label()"
            );
        }
    }

    #[test]
    fn typed_failure_roundtrips_with_detail() {
        let original =
            TypedFailure::with_detail(FailureKind::ContractMismatch, "schema v2 required");
        let json = serde_json::to_string(&original).expect("serialize");
        let restored: TypedFailure = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored.kind, FailureKind::ContractMismatch);
        assert_eq!(restored.detail.as_deref(), Some("schema v2 required"));
    }

    #[test]
    fn typed_failure_roundtrips_without_detail() {
        let original = TypedFailure::new(FailureKind::ScopeViolation);
        let json = serde_json::to_string(&original).expect("serialize");
        let restored: TypedFailure = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored.kind, FailureKind::ScopeViolation);
        assert!(restored.detail.is_none());
    }
}
