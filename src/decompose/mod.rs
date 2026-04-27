//! Handoff decomposition into child handoffs.
//!
//! Splits large or complex handoffs into smaller, independent child tasks
//! suitable for parallel execution. The decomposition respects step dependencies,
//! project boundaries, and effort estimates to produce focused pieces that can
//! each be dispatched to a single agent.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[cfg(test)]
use crate::parser::HandoffMetadata;
use crate::parser::{ParsedHandoff, ParsedStep};
use crate::workflow::template::AgentTier;

mod algorithm;
mod effort;
mod render;

// Public re-exports of submodule items.
pub use render::render_piece;

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

/// Errors that can occur during handoff decomposition.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum DecompositionError {
    /// The handoff contains no steps to decompose.
    #[error("handoff contains no steps")]
    NoSteps,

    /// An effort string could not be parsed into a duration.
    #[error("invalid effort estimate: {0}")]
    InvalidEffort(String),

    /// A configuration value is invalid.
    #[error("decomposition config error: {0}")]
    ConfigError(String),
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Configuration that controls how a handoff is split into pieces.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecompositionConfig {
    /// Maximum total effort (in seconds) for a single piece.
    /// Defaults to 4 hours (14400 seconds).
    pub max_effort_per_piece_secs: u64,

    /// Maximum number of steps in a single piece when no effort estimates exist.
    pub max_steps_per_piece: usize,

    /// When true, steps that depend on each other are kept in the same piece.
    pub respect_dependencies: bool,

    /// When true, steps targeting different projects are placed in separate pieces.
    pub respect_project_boundaries: bool,
}

impl Default for DecompositionConfig {
    fn default() -> Self {
        Self {
            max_effort_per_piece_secs: 4 * 3600, // 4 hours
            max_steps_per_piece: 3,
            respect_dependencies: true,
            respect_project_boundaries: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

/// The output of decomposing a parsed handoff.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecompositionResult {
    /// Title of the original handoff.
    pub original_title: String,

    /// The ordered list of child pieces.
    pub pieces: Vec<HandoffPiece>,

    /// Inter-piece dependency edges as `(piece_idx, depends_on_idx)`.
    pub dependency_graph: Vec<(usize, usize)>,

    /// Non-fatal warnings produced during decomposition.
    pub warnings: Vec<String>,
}

/// A single child handoff produced by decomposition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffPiece {
    /// A URL-safe slug derived from the parent title and piece number.
    pub suggested_slug: String,

    /// Human-readable title for this piece.
    pub title: String,

    /// The steps assigned to this piece.
    pub steps: Vec<ParsedStep>,

    /// Estimated effort in seconds, if computable from the step effort strings.
    pub estimated_effort_secs: Option<u64>,

    /// Recommended agent tier based on the effort or step count.
    pub suggested_tier: AgentTier,

    /// Indices into `DecompositionResult::pieces` that this piece depends on.
    pub depends_on: Vec<usize>,
}

// ---------------------------------------------------------------------------
// Core decomposition
// ---------------------------------------------------------------------------

/// Decompose a parsed handoff into focused child pieces.
///
/// The algorithm:
/// 1. Group steps by project directory.
/// 2. Within each project group, merge dependency-connected steps.
/// 3. Within each dependency group, split at effort or step-count boundaries.
/// 4. Build the inter-piece dependency graph.
/// 5. Assign suggested agent tiers.
pub fn decompose(
    handoff: &ParsedHandoff,
    config: &DecompositionConfig,
) -> Result<DecompositionResult, DecompositionError> {
    if handoff.steps.is_empty() {
        return Err(DecompositionError::NoSteps);
    }

    let parent_slug = algorithm::slugify(&handoff.title);
    let mut pieces: Vec<HandoffPiece> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    // step_number -> piece index (filled as pieces are created)
    let mut step_to_piece: HashMap<u32, usize> = HashMap::new();

    // --- 1. Group by project ---
    let project_groups =
        algorithm::group_by_project(&handoff.steps, config.respect_project_boundaries);

    for (_project, project_steps) in &project_groups {
        // --- 2. Merge dependency-connected steps into atomic chunks ---
        let atomic_chunks: Vec<Vec<&ParsedStep>> = if config.respect_dependencies {
            algorithm::merge_dependency_groups(project_steps)
        } else {
            project_steps.iter().map(|s| vec![*s]).collect()
        };

        // --- 3. Pack atomic chunks into pieces at effort / step-count boundaries ---
        let packed = algorithm::pack_chunks(&atomic_chunks, config, &mut warnings);

        for sub in packed {
            let piece_idx = pieces.len();
            for step in &sub {
                step_to_piece.insert(step.number, piece_idx);
            }

            let effort = algorithm::total_effort_secs(&sub, &mut warnings);
            let tier = match effort {
                Some(secs) => effort::tier_from_effort_secs(secs),
                None => effort::tier_from_step_count(sub.len()),
            };

            let title = algorithm::piece_title(&sub);
            let slug = format!("{}-{}", parent_slug, piece_idx + 1);

            pieces.push(HandoffPiece {
                suggested_slug: slug,
                title,
                steps: sub.into_iter().cloned().collect(),
                estimated_effort_secs: effort,
                suggested_tier: tier,
                depends_on: Vec::new(), // filled below
            });
        }
    }

    // --- 4. Build inter-piece dependency graph ---
    let all_step_numbers: HashSet<u32> = handoff.steps.iter().map(|s| s.number).collect();
    let mut dep_graph: Vec<(usize, usize)> = Vec::new();
    for (piece_idx, piece) in pieces.iter_mut().enumerate() {
        let mut dep_pieces: HashSet<usize> = HashSet::new();
        for step in &piece.steps {
            for dep_str in &step.depends_on {
                if let Some(dep_num) = algorithm::parse_dep_step_number(dep_str) {
                    if !all_step_numbers.contains(&dep_num) {
                        warnings.push(format!(
                            "step {}: dependency 'Step {}' not found in handoff steps",
                            step.number, dep_num
                        ));
                        continue;
                    }
                    if let Some(&dep_piece_idx) = step_to_piece.get(&dep_num) {
                        if dep_piece_idx != piece_idx {
                            dep_pieces.insert(dep_piece_idx);
                        }
                    }
                }
            }
        }
        let mut sorted_deps: Vec<usize> = dep_pieces.into_iter().collect();
        sorted_deps.sort_unstable();
        for &dep_idx in &sorted_deps {
            dep_graph.push((piece_idx, dep_idx));
        }
        piece.depends_on = sorted_deps;
    }

    Ok(DecompositionResult {
        original_title: handoff.title.clone(),
        pieces,
        dependency_graph: dep_graph,
        warnings,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{ChecklistItem, FileModification, VerificationBlock};

    /// Build a minimal step for testing.
    fn make_step(number: u32, title: &str) -> ParsedStep {
        ParsedStep {
            number,
            title: title.to_string(),
            project: None,
            effort: None,
            depends_on: Vec::new(),
            description: format!("Description for step {}", number),
            files_to_modify: Vec::new(),
            verification: None,
            checklist: Vec::new(),
        }
    }

    fn make_handoff(title: &str, steps: Vec<ParsedStep>) -> ParsedHandoff {
        ParsedHandoff {
            title: title.to_string(),
            metadata: None,
            problem: "Test problem".to_string(),
            state: Vec::new(),
            intent: "Test intent".to_string(),
            steps,
            completion_protocol: None,
            context: None,
        }
    }

    // --- Effort parsing ---

    #[test]
    fn parse_effort_hours() {
        assert_eq!(effort::parse_effort_secs("4 hours").unwrap(), 4 * 3600);
    }

    #[test]
    fn parse_effort_range_hours() {
        // Takes the max: 3 hours
        assert_eq!(effort::parse_effort_secs("2-3 hours").unwrap(), 3 * 3600);
    }

    #[test]
    fn parse_effort_day() {
        assert_eq!(effort::parse_effort_secs("1 day").unwrap(), 8 * 3600);
    }

    #[test]
    fn parse_effort_minutes() {
        assert_eq!(effort::parse_effort_secs("30 minutes").unwrap(), 30 * 60);
    }

    #[test]
    fn parse_effort_short_form() {
        assert_eq!(effort::parse_effort_secs("2h").unwrap(), 2 * 3600);
    }

    #[test]
    fn parse_effort_invalid() {
        assert!(effort::parse_effort_secs("lots").is_err());
    }

    // --- Single step, no split needed ---

    #[test]
    fn single_step_no_split() {
        let handoff = make_handoff("Simple Task", vec![make_step(1, "Do the thing")]);
        let config = DecompositionConfig::default();

        let result = decompose(&handoff, &config).unwrap();
        assert_eq!(result.pieces.len(), 1);
        assert_eq!(result.pieces[0].steps.len(), 1);
        assert_eq!(result.pieces[0].title, "Do the thing");
        assert!(result.dependency_graph.is_empty());
    }

    // --- No steps returns error ---

    #[test]
    fn no_steps_returns_error() {
        let handoff = make_handoff("Empty", Vec::new());
        let config = DecompositionConfig::default();

        let err = decompose(&handoff, &config).unwrap_err();
        assert_eq!(err, DecompositionError::NoSteps);
    }

    // --- Fits in one piece ---

    #[test]
    fn two_steps_within_limits_no_split() {
        let handoff = make_handoff(
            "Small Task",
            vec![make_step(1, "First"), make_step(2, "Second")],
        );
        let config = DecompositionConfig::default();

        let result = decompose(&handoff, &config).unwrap();
        assert_eq!(result.pieces.len(), 1);
        assert_eq!(result.pieces[0].steps.len(), 2);
    }

    // --- Split at step-count boundary ---

    #[test]
    fn split_at_step_count_boundary() {
        let steps: Vec<ParsedStep> = (1..=7)
            .map(|i| make_step(i, &format!("Step {}", i)))
            .collect();
        let handoff = make_handoff("Big Task", steps);
        let config = DecompositionConfig {
            max_steps_per_piece: 3,
            ..Default::default()
        };

        let result = decompose(&handoff, &config).unwrap();
        // 7 steps / 3 per piece = 3 pieces (3 + 3 + 1)
        assert_eq!(result.pieces.len(), 3);
        assert_eq!(result.pieces[0].steps.len(), 3);
        assert_eq!(result.pieces[1].steps.len(), 3);
        assert_eq!(result.pieces[2].steps.len(), 1);
    }

    // --- Split at effort boundary ---

    #[test]
    fn split_at_effort_boundary() {
        let mut s1 = make_step(1, "Quick fix");
        s1.effort = Some("1 hour".to_string());
        let mut s2 = make_step(2, "Medium task");
        s2.effort = Some("3 hours".to_string());
        let mut s3 = make_step(3, "Another task");
        s3.effort = Some("3 hours".to_string());

        let handoff = make_handoff("Effort Split", vec![s1, s2, s3]);
        let config = DecompositionConfig {
            max_effort_per_piece_secs: 4 * 3600, // 4 hours max
            ..Default::default()
        };

        let result = decompose(&handoff, &config).unwrap();
        // s1 (1h) + s2 (3h) = 4h fits in one piece
        // s3 (3h) alone would push over, so starts new piece
        assert_eq!(result.pieces.len(), 2);
        assert_eq!(result.pieces[0].steps.len(), 2);
        assert_eq!(result.pieces[1].steps.len(), 1);
    }

    // --- Split at project boundary ---

    #[test]
    fn split_at_project_boundary() {
        let mut s1 = make_step(1, "Frontend work");
        s1.project = Some("cap".to_string());
        let mut s2 = make_step(2, "Backend work");
        s2.project = Some("hymenium".to_string());
        let mut s3 = make_step(3, "More frontend");
        s3.project = Some("cap".to_string());

        let handoff = make_handoff("Cross-project", vec![s1, s2, s3]);
        let config = DecompositionConfig::default();

        let result = decompose(&handoff, &config).unwrap();
        // Two projects: cap (steps 1, 3) and hymenium (step 2)
        assert_eq!(result.pieces.len(), 2);

        let cap_piece = result
            .pieces
            .iter()
            .find(|p| p.steps.iter().any(|s| s.project.as_deref() == Some("cap")))
            .expect("should have a cap piece");
        assert_eq!(cap_piece.steps.len(), 2);

        let hym_piece = result
            .pieces
            .iter()
            .find(|p| {
                p.steps
                    .iter()
                    .any(|s| s.project.as_deref() == Some("hymenium"))
            })
            .expect("should have a hymenium piece");
        assert_eq!(hym_piece.steps.len(), 1);
    }

    // --- Dependencies keep steps together ---

    #[test]
    fn dependent_steps_stay_together() {
        let s1 = make_step(1, "Setup");
        let mut s2 = make_step(2, "Build on setup");
        s2.depends_on = vec!["Step 1".to_string()];
        let s3 = make_step(3, "Independent");
        let s4 = make_step(4, "Also independent");

        let handoff = make_handoff("Deps Test", vec![s1, s2, s3, s4]);
        let config = DecompositionConfig {
            max_steps_per_piece: 2,
            ..Default::default()
        };

        let result = decompose(&handoff, &config).unwrap();
        // Steps 1 and 2 must stay together (dependency).
        // Steps 3 and 4 can go together (within limit of 2).
        assert_eq!(result.pieces.len(), 2);

        let dep_piece = result
            .pieces
            .iter()
            .find(|p| p.steps.iter().any(|s| s.number == 1))
            .expect("should have piece with step 1");
        assert!(dep_piece.steps.iter().any(|s| s.number == 2));
    }

    // --- Inter-piece dependency graph ---

    #[test]
    fn inter_piece_dependency_graph() {
        let s1 = make_step(1, "Foundation");
        let mut s2 = make_step(2, "Depends on step 1");
        s2.depends_on = vec!["Step 1".to_string()];

        let handoff = make_handoff("Graph Test", vec![s1, s2]);
        let config = DecompositionConfig {
            max_steps_per_piece: 1,
            respect_dependencies: false, // force split despite dependency
            ..Default::default()
        };

        let result = decompose(&handoff, &config).unwrap();
        assert_eq!(result.pieces.len(), 2);
        // Piece containing step 2 should depend on piece containing step 1.
        assert!(!result.dependency_graph.is_empty());

        let step2_piece_idx = result
            .pieces
            .iter()
            .position(|p| p.steps.iter().any(|s| s.number == 2))
            .unwrap();
        let step1_piece_idx = result
            .pieces
            .iter()
            .position(|p| p.steps.iter().any(|s| s.number == 1))
            .unwrap();

        assert!(result
            .dependency_graph
            .contains(&(step2_piece_idx, step1_piece_idx)));
        assert!(result.pieces[step2_piece_idx]
            .depends_on
            .contains(&step1_piece_idx));
    }

    // --- Agent tier from effort ---

    #[test]
    fn tier_assignment_from_effort() {
        let mut s1 = make_step(1, "Quick");
        s1.effort = Some("30 minutes".to_string());

        let handoff = make_handoff("Tier Test", vec![s1]);
        let config = DecompositionConfig::default();
        let result = decompose(&handoff, &config).unwrap();
        assert_eq!(result.pieces[0].suggested_tier, AgentTier::Haiku);

        let mut s2 = make_step(1, "Medium");
        s2.effort = Some("3 hours".to_string());
        let handoff2 = make_handoff("Tier Test 2", vec![s2]);
        let result2 = decompose(&handoff2, &config).unwrap();
        assert_eq!(result2.pieces[0].suggested_tier, AgentTier::Sonnet);

        let mut s3 = make_step(1, "Large");
        s3.effort = Some("1 day".to_string());
        let handoff3 = make_handoff("Tier Test 3", vec![s3]);
        let result3 = decompose(&handoff3, &config).unwrap();
        assert_eq!(result3.pieces[0].suggested_tier, AgentTier::Opus);
    }

    // --- Agent tier from step count ---

    #[test]
    fn tier_assignment_from_step_count() {
        assert_eq!(effort::tier_from_step_count(1), AgentTier::Haiku);
        assert_eq!(effort::tier_from_step_count(2), AgentTier::Sonnet);
        assert_eq!(effort::tier_from_step_count(3), AgentTier::Sonnet);
        assert_eq!(effort::tier_from_step_count(4), AgentTier::Opus);
    }

    // --- render_piece produces valid markdown ---

    #[test]
    fn render_piece_produces_markdown_with_title_and_steps() {
        let mut step = make_step(1, "Add error types");
        step.files_to_modify = vec![FileModification {
            path: "src/errors.rs".to_string(),
            description: "Add new error enum".to_string(),
        }];
        step.verification = Some(VerificationBlock {
            commands: vec!["cargo test".to_string()],
            paste_markers: Vec::new(),
        });
        step.checklist = vec![ChecklistItem {
            text: "Error types compile".to_string(),
            checked: false,
        }];

        let piece = HandoffPiece {
            suggested_slug: "my-task-1".to_string(),
            title: "Add error types".to_string(),
            steps: vec![step],
            estimated_effort_secs: Some(3600),
            suggested_tier: AgentTier::Haiku,
            depends_on: Vec::new(),
        };

        let md = render_piece(&piece, "My Task", None);
        assert!(md.contains("# My Task: Add error types"));
        assert!(md.contains("### Step 1: Add error types"));
        assert!(md.contains("`src/errors.rs`"));
        assert!(md.contains("cargo test"));
        assert!(md.contains("- [ ] Error types compile"));
        assert!(md.contains("decomposed from: **My Task**"));
    }

    // --- render_piece with metadata ---

    #[test]
    fn render_piece_includes_metadata() {
        let piece = HandoffPiece {
            suggested_slug: "task-1".to_string(),
            title: "Do stuff".to_string(),
            steps: vec![make_step(1, "Work")],
            estimated_effort_secs: None,
            suggested_tier: AgentTier::Sonnet,
            depends_on: vec![0],
        };

        let meta = HandoffMetadata {
            dispatchability: crate::parser::Dispatchability::Umbrella,
            owning_repo: "hymenium".to_string(),
            allowed_write_scope: vec!["src/".to_string()],
            cross_repo_rule: None,
            non_goals: Vec::new(),
            verification_contract: "cargo test".to_string(),
            completion_update: "update handoff".to_string(),
            source_scope: None,
        };

        let md = render_piece(&piece, "Parent", Some(&meta));
        assert!(md.contains("owning_repo: hymenium"));
        assert!(md.contains("verification_contract: cargo test"));
        assert!(md.contains("Depends on piece(s): #1"));
    }

    // --- Slug generation ---

    #[test]
    fn slugify_produces_url_safe_string() {
        assert_eq!(algorithm::slugify("My Cool Task!"), "my-cool-task");
        assert_eq!(algorithm::slugify("hello world"), "hello-world");
        assert_eq!(algorithm::slugify("A--B  C"), "a-b-c");
    }

    // --- Fallback to step count when no effort ---

    #[test]
    fn fallback_to_step_count_when_no_effort() {
        let steps: Vec<ParsedStep> = (1..=5)
            .map(|i| make_step(i, &format!("Step {}", i)))
            .collect();
        let handoff = make_handoff("No Effort", steps);
        let config = DecompositionConfig {
            max_steps_per_piece: 2,
            ..Default::default()
        };

        let result = decompose(&handoff, &config).unwrap();
        // 5 steps, max 2 per piece = 3 pieces (2 + 2 + 1)
        assert_eq!(result.pieces.len(), 3);
        // No effort estimates → should be None
        assert!(result.pieces[0].estimated_effort_secs.is_none());
    }

    // --- Warnings on unparseable effort ---

    #[test]
    fn warnings_on_bad_effort_string() {
        let mut s1 = make_step(1, "Step one");
        s1.effort = Some("lots of work".to_string());
        let mut s2 = make_step(2, "Step two");
        s2.effort = Some("2 hours".to_string());

        let handoff = make_handoff("Warn Test", vec![s1, s2]);
        let config = DecompositionConfig::default();

        let result = decompose(&handoff, &config).unwrap();
        assert!(!result.warnings.is_empty());
        assert!(result.warnings.iter().any(|w| w.contains("step 1")));
    }

    // --- Mixed effort and no-effort steps ---

    #[test]
    fn mixed_effort_partial_estimates() {
        let mut s1 = make_step(1, "Has effort");
        s1.effort = Some("2 hours".to_string());
        let s2 = make_step(2, "No effort");

        let handoff = make_handoff("Mixed", vec![s1, s2]);
        let config = DecompositionConfig::default();

        let result = decompose(&handoff, &config).unwrap();
        assert_eq!(result.pieces.len(), 1);
        // Should still compute partial effort from step 1
        assert_eq!(result.pieces[0].estimated_effort_secs, Some(2 * 3600));
    }

    // --- Negative effort rejected ---

    #[test]
    fn negative_effort_rejected() {
        assert!(effort::parse_effort_secs("-5 hours").is_err());
    }

    // --- Partial range rejected ---

    #[test]
    fn partial_range_rejected() {
        assert!(effort::parse_effort_secs("1-x hours").is_err());
    }

    // --- Transitive dependency chain (A→B→C) ---

    #[test]
    fn transitive_deps_stay_together() {
        let s1 = make_step(1, "Foundation");
        let mut s2 = make_step(2, "Build on 1");
        s2.depends_on = vec!["Step 1".to_string()];
        let mut s3 = make_step(3, "Build on 2");
        s3.depends_on = vec!["Step 2".to_string()];
        let s4 = make_step(4, "Independent");

        let handoff = make_handoff("Transitive", vec![s1, s2, s3, s4]);
        let config = DecompositionConfig {
            max_steps_per_piece: 2,
            ..Default::default()
        };

        let result = decompose(&handoff, &config).unwrap();
        // Steps 1, 2, 3 must stay together (transitive chain), step 4 alone
        assert_eq!(result.pieces.len(), 2);

        let chain_piece = result
            .pieces
            .iter()
            .find(|p| p.steps.iter().any(|s| s.number == 1))
            .expect("should have piece with step 1");
        assert!(chain_piece.steps.iter().any(|s| s.number == 2));
        assert!(chain_piece.steps.iter().any(|s| s.number == 3));
        assert_eq!(chain_piece.steps.len(), 3);
    }

    // --- Dangling dependency produces warning ---

    #[test]
    fn dangling_dep_produces_warning() {
        let mut s1 = make_step(1, "Has bad dep");
        s1.depends_on = vec!["Step 99".to_string()];

        let handoff = make_handoff("Dangling", vec![s1]);
        let config = DecompositionConfig {
            respect_dependencies: false, // force through to dep graph phase
            ..Default::default()
        };

        let result = decompose(&handoff, &config).unwrap();
        assert!(result.warnings.iter().any(|w| w.contains("Step 99")));
    }

    // --- render_piece with multiple steps ---

    #[test]
    fn render_piece_multiple_steps() {
        let s1 = make_step(1, "First step");
        let s2 = make_step(2, "Second step");

        let piece = HandoffPiece {
            suggested_slug: "multi-1".to_string(),
            title: "Steps 1-2: First step & Second step".to_string(),
            steps: vec![s1, s2],
            estimated_effort_secs: None,
            suggested_tier: AgentTier::Sonnet,
            depends_on: Vec::new(),
        };

        let md = render_piece(&piece, "Parent", None);
        assert!(md.contains("### Step 1: First step"));
        assert!(md.contains("### Step 2: Second step"));
        assert!(md.contains("# Parent: Steps 1-2"));
    }
}
