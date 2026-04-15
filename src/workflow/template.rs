//! Workflow template loading and validation.
//!
//! Loads and validates workflow templates from JSON configuration. Templates define
//! the phases, roles, and gate conditions for a workflow pattern.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

/// Error type for template operations.
#[derive(Debug, Error)]
pub enum TemplateError {
    #[error("failed to parse template JSON: {0}")]
    ParseError(String),

    #[error("template missing required field: {0}")]
    MissingField(String),

    #[error("transition references non-existent phase: from {from_phase} to {to_phase}")]
    InvalidPhaseReference {
        from_phase: String,
        to_phase: String,
    },

    #[error("duplicate phase ID: {0}")]
    DuplicatePhaseId(String),

    #[error("template not found: {0}")]
    NotFound(String),
}

/// Result type for template operations.
pub type TemplateResult<T> = Result<T, TemplateError>;

/// Represents a complete workflow template with phases and transitions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowTemplate {
    pub schema_version: String,
    pub template_id: String,
    pub name: String,
    pub description: String,
    pub phases: Vec<Phase>,
    pub transitions: Vec<Transition>,
}

impl WorkflowTemplate {
    /// Validate the template for structural correctness.
    pub fn validate(&self) -> TemplateResult<()> {
        // Check for duplicate phase IDs
        let mut seen_phases = std::collections::HashSet::new();
        for phase in &self.phases {
            if !seen_phases.insert(&phase.phase_id) {
                return Err(TemplateError::DuplicatePhaseId(phase.phase_id.clone()));
            }
        }

        // Check that all transitions reference valid phases
        let phase_ids: std::collections::HashSet<_> =
            self.phases.iter().map(|p| &p.phase_id).collect();

        for transition in &self.transitions {
            if !phase_ids.contains(&transition.from_phase) {
                return Err(TemplateError::InvalidPhaseReference {
                    from_phase: transition.from_phase.clone(),
                    to_phase: transition.to_phase.clone(),
                });
            }
            if !phase_ids.contains(&transition.to_phase) {
                return Err(TemplateError::InvalidPhaseReference {
                    from_phase: transition.from_phase.clone(),
                    to_phase: transition.to_phase.clone(),
                });
            }
        }

        Ok(())
    }
}

/// Process-level role for a workflow phase.
///
/// This is the abstract, user-facing label that is portable across workflow
/// templates. It maps to a default `AgentRole` via `effective_agent_role()`.
/// Use `agent_role` on the phase when you need to pin a specific dispatch
/// catalog agent.
///
/// The wire format uses lowercase names (e.g., `"implementer"`) via
/// `#[serde(rename_all = "lowercase")]`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ProcessRole {
    Implementer,
    Auditor,
    Reviewer,
    Operator,
}

impl std::fmt::Display for ProcessRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProcessRole::Implementer => write!(f, "implementer"),
            ProcessRole::Auditor => write!(f, "auditor"),
            ProcessRole::Reviewer => write!(f, "reviewer"),
            ProcessRole::Operator => write!(f, "operator"),
        }
    }
}

/// Represents a single phase within a workflow template.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Phase {
    pub phase_id: String,
    /// Abstract process role for this phase (portable, user-facing).
    pub role: ProcessRole,
    /// Optional dispatch catalog agent. When set, pins the phase to a specific
    /// agent. When absent, dispatch resolves the agent from `role` via
    /// `effective_agent_role()`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_role: Option<AgentRole>,
    pub agent_tier: AgentTier,
    pub entry_gate: Gate,
    pub exit_gate: Gate,
}

impl Phase {
    /// Returns the concrete dispatch role for this phase.
    ///
    /// Uses `agent_role` when explicitly set; falls back to a default derived
    /// from the process role.
    pub fn effective_agent_role(&self) -> AgentRole {
        self.agent_role.clone().unwrap_or(match self.role {
            ProcessRole::Implementer => AgentRole::Worker,
            ProcessRole::Auditor => AgentRole::OutputVerifier,
            ProcessRole::Reviewer => AgentRole::FinalVerifier,
            ProcessRole::Operator => AgentRole::WorkflowCoordinator,
        })
    }
}

/// Represents gate conditions for phase entry or exit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gate {
    pub requires: Vec<String>,
}

/// Agent role for a workflow phase.
///
/// The nine runtime role names match the `workflow-status-v1` contract.
/// The wire format uses human-readable names (e.g., "Spec Author") via
/// explicit `#[serde(rename = ...)]` attributes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub enum AgentRole {
    #[serde(rename = "Spec Author")]
    SpecAuthor,
    #[serde(rename = "Workflow Planner")]
    WorkflowPlanner,
    #[serde(rename = "Packet Compiler")]
    PacketCompiler,
    #[serde(rename = "Decomposition Checker")]
    DecompositionChecker,
    #[serde(rename = "Workflow Coordinator")]
    WorkflowCoordinator,
    #[serde(rename = "Worker")]
    Worker,
    #[serde(rename = "Output Verifier")]
    OutputVerifier,
    #[serde(rename = "Repair Worker")]
    RepairWorker,
    #[serde(rename = "Final Verifier")]
    FinalVerifier,
}

impl std::fmt::Display for AgentRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentRole::SpecAuthor => write!(f, "Spec Author"),
            AgentRole::WorkflowPlanner => write!(f, "Workflow Planner"),
            AgentRole::PacketCompiler => write!(f, "Packet Compiler"),
            AgentRole::DecompositionChecker => write!(f, "Decomposition Checker"),
            AgentRole::WorkflowCoordinator => write!(f, "Workflow Coordinator"),
            AgentRole::Worker => write!(f, "Worker"),
            AgentRole::OutputVerifier => write!(f, "Output Verifier"),
            AgentRole::RepairWorker => write!(f, "Repair Worker"),
            AgentRole::FinalVerifier => write!(f, "Final Verifier"),
        }
    }
}

/// Agent tier for a workflow phase.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum AgentTier {
    Opus,
    Sonnet,
    Haiku,
    Any,
}

impl std::fmt::Display for AgentTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentTier::Opus => write!(f, "opus"),
            AgentTier::Sonnet => write!(f, "sonnet"),
            AgentTier::Haiku => write!(f, "haiku"),
            AgentTier::Any => write!(f, "any"),
        }
    }
}

/// Represents a transition between phases.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transition {
    pub from_phase: String,
    pub to_phase: String,
    pub condition: String,
}

/// Registry of workflow templates with lookup and persistence.
#[derive(Debug, Clone)]
pub struct TemplateRegistry {
    templates: HashMap<String, WorkflowTemplate>,
}

impl TemplateRegistry {
    /// Create a new empty template registry.
    pub fn new() -> Self {
        Self {
            templates: HashMap::new(),
        }
    }

    /// Register a template in the registry.
    pub fn register(&mut self, template: WorkflowTemplate) -> TemplateResult<()> {
        template.validate()?;
        self.templates
            .insert(template.template_id.clone(), template);
        Ok(())
    }

    /// Get a template by ID.
    pub fn get(&self, id: &str) -> TemplateResult<&WorkflowTemplate> {
        self.templates
            .get(id)
            .ok_or_else(|| TemplateError::NotFound(id.to_string()))
    }

    /// List all registered template IDs.
    pub fn list_ids(&self) -> Vec<&str> {
        self.templates
            .keys()
            .map(std::string::String::as_str)
            .collect()
    }
}

impl Default for TemplateRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Load a workflow template from JSON.
pub fn load_from_json(json: &str) -> TemplateResult<WorkflowTemplate> {
    let template: WorkflowTemplate =
        serde_json::from_str(json).map_err(|e| TemplateError::ParseError(e.to_string()))?;
    template.validate()?;
    Ok(template)
}

/// Get the built-in implementer/auditor workflow template.
pub fn impl_audit_default() -> WorkflowTemplate {
    WorkflowTemplate {
        schema_version: "1.0".to_string(),
        template_id: "impl-audit".to_string(),
        name: "Implementer/Auditor".to_string(),
        description: "Two-phase workflow for implementation handoffs with post-implementation audit. \
                      The implementer executes the planned work and verifies it locally. \
                      The auditor reviews the code diff, checks for regressions, and validates that \
                      all verification evidence is solid before closure."
            .to_string(),
        phases: vec![
            Phase {
                phase_id: "implement".to_string(),
                role: ProcessRole::Implementer,
                agent_role: Some(AgentRole::Worker),
                agent_tier: AgentTier::Sonnet,
                entry_gate: Gate {
                    requires: vec![],
                },
                exit_gate: Gate {
                    requires: vec![
                        "code_diff_exists".to_string(),
                        "verification_passed".to_string(),
                    ],
                },
            },
            Phase {
                phase_id: "audit".to_string(),
                role: ProcessRole::Auditor,
                agent_role: Some(AgentRole::OutputVerifier),
                agent_tier: AgentTier::Sonnet,
                entry_gate: Gate {
                    requires: vec![
                        "code_diff_exists".to_string(),
                        "verification_passed".to_string(),
                    ],
                },
                exit_gate: Gate {
                    requires: vec!["audit_clean".to_string(), "findings_resolved".to_string()],
                },
            },
        ],
        transitions: vec![Transition {
            from_phase: "implement".to_string(),
            to_phase: "audit".to_string(),
            condition: "Implementation complete with verification evidence".to_string(),
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_from_json_septa_fixture() {
        let json = r#"{
            "schema_version": "1.1",
            "template_id": "impl-audit",
            "name": "Implementer/Auditor",
            "description": "Two-phase workflow",
            "phases": [
                {
                    "phase_id": "implement",
                    "role": "implementer",
                    "agent_role": "Worker",
                    "agent_tier": "sonnet",
                    "entry_gate": {"requires": []},
                    "exit_gate": {"requires": ["code_diff_exists", "verification_passed"]}
                },
                {
                    "phase_id": "audit",
                    "role": "auditor",
                    "agent_role": "Output Verifier",
                    "agent_tier": "sonnet",
                    "entry_gate": {"requires": ["code_diff_exists", "verification_passed"]},
                    "exit_gate": {"requires": ["audit_clean", "findings_resolved"]}
                }
            ],
            "transitions": [
                {
                    "from_phase": "implement",
                    "to_phase": "audit",
                    "condition": "Implementation complete"
                }
            ]
        }"#;

        let template = load_from_json(json).expect("should load valid template");
        assert_eq!(template.template_id, "impl-audit");
        assert_eq!(template.phases.len(), 2);
        assert_eq!(template.transitions.len(), 1);
        assert_eq!(template.phases[0].role, ProcessRole::Implementer);
        assert_eq!(template.phases[0].agent_role, Some(AgentRole::Worker));
        assert_eq!(template.phases[1].role, ProcessRole::Auditor);
        assert_eq!(
            template.phases[1].agent_role,
            Some(AgentRole::OutputVerifier)
        );
    }

    #[test]
    fn test_impl_audit_default_is_valid() {
        let template = impl_audit_default();
        assert_eq!(template.template_id, "impl-audit");
        assert_eq!(template.phases.len(), 2);
        template
            .validate()
            .expect("default template should be valid");
    }

    #[test]
    fn test_invalid_phase_reference_rejected() {
        let json = r#"{
            "schema_version": "1.1",
            "template_id": "bad-template",
            "name": "Bad Template",
            "description": "Invalid transitions",
            "phases": [
                {
                    "phase_id": "phase1",
                    "role": "implementer",
                    "agent_tier": "sonnet",
                    "entry_gate": {"requires": []},
                    "exit_gate": {"requires": []}
                }
            ],
            "transitions": [
                {
                    "from_phase": "phase1",
                    "to_phase": "nonexistent",
                    "condition": "bad"
                }
            ]
        }"#;

        let result = load_from_json(json);
        assert!(result.is_err());
        match result {
            Err(TemplateError::InvalidPhaseReference { .. }) => {}
            _ => panic!("expected InvalidPhaseReference error"),
        }
    }

    #[test]
    fn test_duplicate_phase_id_rejected() {
        let json = r#"{
            "schema_version": "1.1",
            "template_id": "bad-template",
            "name": "Bad Template",
            "description": "Duplicate phases",
            "phases": [
                {
                    "phase_id": "phase1",
                    "role": "implementer",
                    "agent_tier": "sonnet",
                    "entry_gate": {"requires": []},
                    "exit_gate": {"requires": []}
                },
                {
                    "phase_id": "phase1",
                    "role": "auditor",
                    "agent_tier": "sonnet",
                    "entry_gate": {"requires": []},
                    "exit_gate": {"requires": []}
                }
            ],
            "transitions": []
        }"#;

        let result = load_from_json(json);
        assert!(result.is_err());
        match result {
            Err(TemplateError::DuplicatePhaseId(_)) => {}
            _ => panic!("expected DuplicatePhaseId error"),
        }
    }

    #[test]
    fn test_missing_required_fields() {
        let json = r#"{
            "schema_version": "1.1",
            "template_id": "bad-template",
            "name": "Bad Template",
            "phases": [],
            "transitions": []
        }"#;

        let result = load_from_json(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_template_registry() {
        let mut registry = TemplateRegistry::new();
        let template = impl_audit_default();
        registry
            .register(template.clone())
            .expect("should register template");

        let retrieved = registry.get("impl-audit").expect("should find template");
        assert_eq!(retrieved.template_id, "impl-audit");

        let result = registry.get("nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn test_agent_role_display() {
        assert_eq!(format!("{}", AgentRole::Worker), "Worker");
        assert_eq!(format!("{}", AgentRole::OutputVerifier), "Output Verifier");
        assert_eq!(format!("{}", AgentRole::SpecAuthor), "Spec Author");
        assert_eq!(format!("{}", AgentRole::FinalVerifier), "Final Verifier");
    }

    #[test]
    fn test_impl_audit_roles_match_reset_contract() {
        // Proves that the effective dispatch agent roles for impl-audit use only
        // septa workflow-status-v1 role names.
        let template = impl_audit_default();
        let allowed = [
            "Spec Author",
            "Workflow Planner",
            "Packet Compiler",
            "Decomposition Checker",
            "Workflow Coordinator",
            "Worker",
            "Output Verifier",
            "Repair Worker",
            "Final Verifier",
        ];
        for phase in &template.phases {
            let role_str = phase.effective_agent_role().to_string();
            assert!(
                allowed.contains(&role_str.as_str()),
                "phase '{}' has effective_agent_role '{}' not in the allowed septa set",
                phase.phase_id,
                role_str
            );
        }
        // Verify the process roles.
        assert_eq!(template.phases[0].role, ProcessRole::Implementer);
        assert_eq!(template.phases[1].role, ProcessRole::Auditor);
        // Verify the explicit agent_role pins.
        assert_eq!(template.phases[0].agent_role, Some(AgentRole::Worker));
        assert_eq!(
            template.phases[1].agent_role,
            Some(AgentRole::OutputVerifier)
        );
        // Verify effective dispatch roles match agent_role when set.
        assert_eq!(template.phases[0].effective_agent_role(), AgentRole::Worker);
        assert_eq!(
            template.phases[1].effective_agent_role(),
            AgentRole::OutputVerifier
        );
    }

    #[test]
    fn test_process_role_display() {
        assert_eq!(format!("{}", ProcessRole::Implementer), "implementer");
        assert_eq!(format!("{}", ProcessRole::Auditor), "auditor");
        assert_eq!(format!("{}", ProcessRole::Reviewer), "reviewer");
        assert_eq!(format!("{}", ProcessRole::Operator), "operator");
    }

    #[test]
    fn test_effective_agent_role_fallback() {
        // When agent_role is None, effective_agent_role falls back from ProcessRole.
        let phase_no_agent_role = Phase {
            phase_id: "test".to_string(),
            role: ProcessRole::Implementer,
            agent_role: None,
            agent_tier: AgentTier::Sonnet,
            entry_gate: Gate { requires: vec![] },
            exit_gate: Gate { requires: vec![] },
        };
        assert_eq!(
            phase_no_agent_role.effective_agent_role(),
            AgentRole::Worker
        );

        let phase_auditor = Phase {
            phase_id: "test".to_string(),
            role: ProcessRole::Auditor,
            agent_role: None,
            agent_tier: AgentTier::Sonnet,
            entry_gate: Gate { requires: vec![] },
            exit_gate: Gate { requires: vec![] },
        };
        assert_eq!(
            phase_auditor.effective_agent_role(),
            AgentRole::OutputVerifier
        );

        let phase_reviewer = Phase {
            phase_id: "test".to_string(),
            role: ProcessRole::Reviewer,
            agent_role: None,
            agent_tier: AgentTier::Sonnet,
            entry_gate: Gate { requires: vec![] },
            exit_gate: Gate { requires: vec![] },
        };
        assert_eq!(
            phase_reviewer.effective_agent_role(),
            AgentRole::FinalVerifier
        );

        let phase_operator = Phase {
            phase_id: "test".to_string(),
            role: ProcessRole::Operator,
            agent_role: None,
            agent_tier: AgentTier::Sonnet,
            entry_gate: Gate { requires: vec![] },
            exit_gate: Gate { requires: vec![] },
        };
        assert_eq!(
            phase_operator.effective_agent_role(),
            AgentRole::WorkflowCoordinator
        );
    }

    #[test]
    fn test_agent_tier_display() {
        assert_eq!(format!("{}", AgentTier::Sonnet), "sonnet");
        assert_eq!(format!("{}", AgentTier::Opus), "opus");
    }
}
