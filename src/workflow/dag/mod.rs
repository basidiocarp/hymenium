//! DAG workflow executor: YAML-defined directed acyclic graph workflows.
//!
//! This module provides a complementary execution model to the phase-based
//! template system. DAG workflows are expressed as YAML files with typed
//! nodes (prompt, bash, loop, command, approval) and directed edges that
//! control execution order.
//!
//! Example workflow (review-pr):
//! ```yaml
//! workflow_id: review-pr
//! name: Parallel PR Review
//! nodes:
//!   - { kind: command, id: review-code,   skill: code-reviewer }
//!   - { kind: command, id: review-errors, skill: silent-failure-hunter }
//!   - { kind: command, id: review-tests,  skill: test-coverage-reviewer }
//!   - { kind: prompt,  id: synthesize,    prompt: "Synthesize $review-code.output ...", trigger_rule: one_success }
//! edges:
//!   - { from: review-code,   to: synthesize }
//!   - { from: review-errors, to: synthesize }
//!   - { from: review-tests,  to: synthesize }
//! ```

pub mod executor;
pub mod loader;
pub mod node;

pub use executor::{DagExecutor, NodeResult, NodeStatus};
pub use loader::{load_workflow, DagEdge, WorkflowDag};
pub use node::{
    ApprovalNode, BashNode, CommandNode, ContextMode, DagNode, HookEntry, LoopNode, NodeHooks,
    PromptNode, TriggerRule,
};
