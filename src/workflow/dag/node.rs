//! DAG node types and execution semantics.

use serde::{Deserialize, Serialize};

/// Typed node kind enum for DAG workflows.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DagNode {
    Prompt(PromptNode),
    Bash(BashNode),
    Loop(LoopNode),
    Command(CommandNode),
    Approval(ApprovalNode),
}

impl DagNode {
    /// Extract the node ID from any variant.
    pub fn id(&self) -> &str {
        match self {
            Self::Prompt(n) => &n.id,
            Self::Bash(n) => &n.id,
            Self::Loop(n) => &n.id,
            Self::Command(n) => &n.id,
            Self::Approval(n) => &n.id,
        }
    }

    /// Get the trigger rule for this node.
    pub fn trigger_rule(&self) -> TriggerRule {
        match self {
            Self::Prompt(n) => n.trigger_rule.clone(),
            Self::Bash(n) => n.trigger_rule.clone(),
            Self::Loop(n) => n.trigger_rule.clone(),
            Self::Command(n) => n.trigger_rule.clone(),
            Self::Approval(n) => n.trigger_rule.clone(),
        }
    }

    /// Get the context mode for this node (only Prompt and Loop support this).
    pub fn context_mode(&self) -> ContextMode {
        match self {
            Self::Prompt(n) => n.context.clone(),
            Self::Loop(n) => n.context.clone(),
            _ => ContextMode::Inherit,
        }
    }
}

/// Prompt node: requests text generation from an LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptNode {
    pub id: String,
    pub prompt: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub context: ContextMode,
    #[serde(default)]
    pub trigger_rule: TriggerRule,
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
    #[serde(default)]
    pub denied_tools: Option<Vec<String>>,
    #[serde(default)]
    pub hooks: Option<NodeHooks>,
}

/// Bash node: runs a shell command and captures output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BashNode {
    pub id: String,
    pub command: String,
    #[serde(default)]
    pub trigger_rule: TriggerRule,
}

/// Loop node: repeatedly calls a prompt until a condition is met.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopNode {
    pub id: String,
    pub prompt: String,
    pub until: String,
    #[serde(default = "default_max_iterations")]
    pub max_iterations: usize,
    #[serde(default)]
    pub trigger_rule: TriggerRule,
    #[serde(default)]
    pub context: ContextMode,
    #[serde(default)]
    pub hooks: Option<NodeHooks>,
}

fn default_max_iterations() -> usize {
    5
}

/// Command node: invokes a lamella skill by name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandNode {
    pub id: String,
    pub skill: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub trigger_rule: TriggerRule,
}

/// Approval node: prompts the operator for yes/no approval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalNode {
    pub id: String,
    pub prompt: String,
    #[serde(default)]
    pub trigger_rule: TriggerRule,
}

/// Controls when a node with multiple incoming edges becomes ready.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TriggerRule {
    /// All predecessors must succeed (default).
    #[default]
    AllSuccess,
    /// Fires as soon as any predecessor succeeds; fault-tolerant fan-in.
    OneSuccess,
}

/// Whether a node inherits model context from prior turns or starts fresh.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextMode {
    /// Inherit context from prior turns (default).
    #[default]
    Inherit,
    /// Start with a clean slate.
    Fresh,
}

/// Hook configuration for a node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeHooks {
    #[serde(default)]
    pub pre_tool_use: Vec<HookEntry>,
    #[serde(default)]
    pub post_tool_use: Vec<HookEntry>,
}

/// A single hook entry matching and action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookEntry {
    pub matcher: String,
    #[serde(default)]
    pub additional_context: Option<String>,
    #[serde(default)]
    pub system_message: Option<String>,
    #[serde(default)]
    pub deny: bool,
}
