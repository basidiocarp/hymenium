use super::capability;
use super::task_packet::{CapabilityRequirements, TaskPacket};
use super::{CanopyClient, DispatchError, TaskOptions};
use crate::classify::classify_error;
use crate::context::{
    estimate_text_tokens, BudgetContextEngine, CompressionParams, ContextEngine, ContextMessage,
    ContextMessageRole,
};
use crate::parser::ParsedHandoff;
use crate::workflow::engine::WorkflowInstance;
use crate::workflow::template::AgentRole;
use crate::workflow::template::WorkflowTemplate;
use crate::workflow::WorkflowId;
use tracing;

const DISPATCH_CONTEXT_TOKEN_BUDGET: usize = 64;

// ---------------------------------------------------------------------------
// Dispatch orchestration
// ---------------------------------------------------------------------------

/// Dispatch a workflow by creating canopy tasks for each phase.
///
/// Creates a parent task from the handoff, then one subtask per template phase.
/// The first phase's agent is assigned if the phase defines a specific role.
/// Returns a `WorkflowInstance` with status `Dispatched` and canopy task IDs
/// populated in each `PhaseState`.
///
/// The `handoff_file_path` parameter must be the actual filesystem path to the
/// handoff document (e.g. `/home/user/.handoffs/myrepo/task.md`). It is stored
/// on the `WorkflowInstance` and surfaced by `hymenium status` so operators can
/// locate the file. Passing only a repo name or relative path here will produce
/// misleading status output.
///
/// # Panics
///
/// Panics if `serde_json` fails to serialize a `TaskPacket`. This is considered
/// a programming error because `TaskPacket` contains only primitive JSON-compatible
/// types derived from `serde::Serialize` with no serde-incompatible fields.
#[allow(clippy::too_many_lines)]
pub fn dispatch_workflow(
    handoff: &ParsedHandoff,
    template: &WorkflowTemplate,
    workflow_id: &WorkflowId,
    handoff_file_path: &str,
    canopy: &dyn CanopyClient,
) -> Result<WorkflowInstance, DispatchError> {
    // Guard: template must have at least one phase.
    if template.phases.is_empty() {
        return Err(DispatchError::InvalidState(
            "template has no phases — cannot dispatch".to_string(),
        ));
    }

    // Derive a project root from the handoff metadata, falling back to ".".
    let project_root = handoff
        .metadata
        .as_ref()
        .map_or(".", |m| m.owning_repo.as_str());

    // Extract repo name (basename) for agent naming — distinct from the
    // project_root path used for canopy task creation.
    let repo_name = std::path::Path::new(project_root)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(project_root);

    let slug = handoff_slug(&handoff.title);
    if slug.is_empty() {
        return Err(DispatchError::InvalidState(
            "handoff title produced an empty slug — cannot form agent name".to_string(),
        ));
    }

    let dispatch_messages = build_dispatch_context_messages(handoff);
    let focus_topic = dispatch_focus_topic(template);
    let parent_description =
        prepare_parent_description(&dispatch_messages, focus_topic.as_deref())?;

    // Derive capability requirements from the owning repo name so tasks carry
    // the right capability hints at dispatch time.
    let repo_required_capabilities = capability::capabilities_for_repo(repo_name);

    // Create the parent canopy task from the handoff.
    let parent_task_id = canopy
        .create_task(
            &handoff.title,
            &parent_description,
            project_root,
            &TaskOptions {
                required_capabilities: repo_required_capabilities.clone(),
                requested_by: Some("hymenium".to_string()),
                ..TaskOptions::default()
            },
        )
        .inspect_err(|err| {
            log_dispatch_error("create_task", err);
        })?;

    // Build the workflow instance using the actual filesystem path so operators
    // can locate the handoff document from `hymenium status` output.
    let mut instance =
        WorkflowInstance::new(workflow_id.clone(), template.clone(), handoff_file_path);

    // Derive constraints and acceptance criteria from the handoff for packets.
    let constraints = build_constraints(handoff);
    let acceptance_criteria = build_acceptance_criteria(handoff);

    // Create a subtask for each phase and store its canopy task ID.
    //
    // KNOWN LIMITATION: If a subtask creation fails mid-loop, previously created
    // tasks in canopy are orphaned (permanently unreferenced from hymenium).
    // The CanopyClient trait does not yet expose a cancel_task method, so cleanup
    // is not possible at this time. Orphaned tasks remain in Canopy's ledger but
    // are never assigned agents or monitored for progress.
    //
    // OPERATORS: If dispatch fails partway through, inspect Canopy manually to
    // find and cancel orphaned subtasks. A future reconciliation scan
    // (see TODO below) can automate this.
    //
    // TODO(#118f-rollback): add CanopyClient::cancel_task and compensate on failure.
    for (phase, state) in template.phases.iter().zip(instance.phase_states.iter_mut()) {
        let title = format!(
            "[{}] {} \u{2014} {}",
            phase.role, phase.phase_id, handoff.title
        );

        // Build a structured task packet for this phase.
        let write_scope = handoff
            .metadata
            .as_ref()
            .map_or(&[] as &[String], |m| m.allowed_write_scope.as_slice());
        let required_capabilities = CapabilityRequirements {
            tier: phase.agent_tier.to_string(),
            tools: tools_for_write_scope(write_scope),
        };
        let goal = format!(
            "Execute the '{}' phase ({}) for handoff: {}",
            phase.phase_id, phase.role, handoff.title
        );
        let packet = TaskPacket::new(
            workflow_id.0.clone(),
            phase.phase_id.clone(),
            goal,
            constraints.clone(),
            acceptance_criteria.clone(),
            required_capabilities,
        );

        // Serialize packet as structured JSON embedded in the description.
        let packet_json = serde_json::to_string(&packet).expect(
            "TaskPacket serialization is infallible — all fields are known-good serde types",
        );
        let description = format!(
            "Phase: {} | Role: {} | Tier: {}\n\nTask packet:\n{}",
            phase.phase_id, phase.role, phase.agent_tier, packet_json
        );

        let options = TaskOptions {
            required_role: phase.agent_role.clone(),
            required_tier: Some(phase.agent_tier.clone()),
            verification_required: !phase.exit_gate.requires.is_empty(),
            required_capabilities: repo_required_capabilities.clone(),
            requested_by: Some("hymenium".to_string()),
            workflow_id: Some(workflow_id.0.clone()),
            phase_id: Some(phase.phase_id.clone()),
        };

        let subtask_id = canopy
            .create_subtask(&parent_task_id, &title, &description, &options)
            .inspect_err(|err| {
                log_dispatch_error("create_subtask", err);
            })?;

        state.canopy_task_id = Some(subtask_id);
    }

    // Assign the first phase's agent automatically.
    if let Some(first_phase) = template.phases.first() {
        let agent = agent_name(&first_phase.effective_agent_role(), repo_name, &slug, 1);
        if let Some(first_state) = instance.phase_states.first() {
            if let Some(ref task_id) = first_state.canopy_task_id {
                canopy.assign_task(task_id, &agent, "hymenium")?;
            }
        }
        if let Some(first_state) = instance.phase_states.first_mut() {
            first_state.agent_id = Some(agent);
        }
    }

    instance.status = crate::workflow::engine::WorkflowStatus::Dispatched;
    Ok(instance)
}

// ---------------------------------------------------------------------------
// Error classification logging
// ---------------------------------------------------------------------------

/// Classify a [`DispatchError`] and emit a structured log entry.
///
/// This is additive — it does not change the error returned to the caller.
/// The [`classify_error`] call uses `None` for the HTTP status because
/// `DispatchError` wraps string messages rather than raw HTTP responses;
/// body-hint signals (if any) are extracted from the formatted message.
fn log_dispatch_error(operation: &str, err: &DispatchError) {
    let body_hint = err.to_string();
    let (reason, hint) = classify_error(None, Some(&body_hint));
    tracing::warn!(
        operation,
        error = %err,
        failover_reason = ?reason,
        retryable = hint.retryable,
        should_compress = hint.should_compress,
        should_rotate_credential = hint.should_rotate_credential,
        should_fallback = hint.should_fallback,
        "dispatch error classified"
    );
}

fn build_dispatch_context_messages(handoff: &ParsedHandoff) -> Vec<ContextMessage> {
    let mut messages = vec![
        ContextMessage::text("handoff-title", ContextMessageRole::System, &handoff.title),
        ContextMessage::text(
            "handoff-problem",
            ContextMessageRole::User,
            &handoff.problem,
        ),
        ContextMessage::text(
            "handoff-intent",
            ContextMessageRole::Assistant,
            &handoff.intent,
        ),
    ];

    if let Some(context) = handoff.context.as_ref() {
        messages.push(ContextMessage::text(
            "handoff-context",
            ContextMessageRole::User,
            context,
        ));
    }

    for step in &handoff.steps {
        messages.push(ContextMessage::text(
            format!("step-{}", step.number),
            ContextMessageRole::User,
            format!("Step {}: {}\n{}", step.number, step.title, step.description),
        ));
    }

    messages
}

fn dispatch_focus_topic(template: &WorkflowTemplate) -> Option<String> {
    template
        .phases
        .first()
        .map(|phase| format!("{} {}", phase.phase_id, phase.role))
}

fn prepare_parent_description(
    dispatch_messages: &[ContextMessage],
    focus_topic: Option<&str>,
) -> Result<String, DispatchError> {
    let initial = render_context(dispatch_messages, false);
    if estimate_text_tokens(&initial) <= DISPATCH_CONTEXT_TOKEN_BUDGET {
        return Ok(initial);
    }

    let engine = BudgetContextEngine;
    let params = CompressionParams {
        focus_topic: focus_topic.map(std::string::ToString::to_string),
        token_budget: DISPATCH_CONTEXT_TOKEN_BUDGET,
    };
    let compressed = engine
        .compress(dispatch_messages, &params)
        .map_err(|error| {
            DispatchError::InvalidState(format!("context compression failed: {error}"))
        })?;

    Ok(truncate_rendered_context(
        &render_context(&compressed.messages, true),
        DISPATCH_CONTEXT_TOKEN_BUDGET,
    ))
}

fn render_context(messages: &[ContextMessage], compressed: bool) -> String {
    let mut out = String::new();
    if compressed {
        out.push_str("Compressed context:\n");
    }
    for message in messages {
        out.push_str("- ");
        out.push_str(role_label(message.role));
        out.push_str(": ");
        out.push_str(&message.content);
        out.push('\n');
    }
    out.trim().to_string()
}

fn role_label(role: ContextMessageRole) -> &'static str {
    match role {
        ContextMessageRole::System => "system",
        ContextMessageRole::User => "user",
        ContextMessageRole::Assistant => "assistant",
        ContextMessageRole::ToolCall => "tool_call",
        ContextMessageRole::ToolResult => "tool_result",
    }
}

fn truncate_rendered_context(text: &str, token_budget: usize) -> String {
    let words = text.split_whitespace().collect::<Vec<_>>();
    if words.len() <= token_budget {
        return text.to_string();
    }

    words[..token_budget].join(" ")
}

// ---------------------------------------------------------------------------
// Packet helpers
// ---------------------------------------------------------------------------

/// Source patterns that indicate write access to source code is needed.
const SOURCE_PATTERNS: &[&str] = &["src/", ".rs", ".ts", ".py", ".js", "lib/"];

/// Derive the tool list for a phase based on the handoff's allowed write scope.
///
/// If any write scope path contains a source-code pattern, the worker needs
/// full write access. Otherwise read-only access is sufficient.
fn tools_for_write_scope(allowed_write_scope: &[String]) -> Vec<String> {
    let needs_write = allowed_write_scope
        .iter()
        .any(|path| SOURCE_PATTERNS.iter().any(|pat| path.contains(pat)));
    let mut tools = vec!["bash".to_string(), "read".to_string()];
    if needs_write {
        tools.push("write".to_string());
    }
    tools
}

/// Build constraint strings from the handoff metadata.
fn build_constraints(handoff: &ParsedHandoff) -> Vec<String> {
    let mut constraints = Vec::new();
    if let Some(meta) = &handoff.metadata {
        for scope in &meta.allowed_write_scope {
            constraints.push(format!("Write scope limited to {}", scope));
        }
        // For read-only source tasks that can write artifacts, surface the boundary explicitly.
        if !meta.allowed_write_scope.is_empty() {
            let has_source_write = meta
                .allowed_write_scope
                .iter()
                .any(|p| SOURCE_PATTERNS.iter().any(|pat| p.contains(pat)));
            if !has_source_write {
                let paths = meta.allowed_write_scope.join(", ");
                constraints.push(format!(
                    "Source code is read-only; artifact writes allowed at: {}",
                    paths
                ));
            }
        }
        for goal in &meta.non_goals {
            constraints.push(format!("Non-goal (do not implement): {}", goal));
        }
    }
    constraints
}

/// Build acceptance criteria from the handoff steps' checklists.
fn build_acceptance_criteria(handoff: &ParsedHandoff) -> Vec<String> {
    let mut criteria = Vec::new();
    for step in &handoff.steps {
        if let Some(verification) = &step.verification {
            for cmd in &verification.commands {
                criteria.push(format!("Verification passes: {}", cmd));
            }
        }
        for item in &step.checklist {
            criteria.push(item.text.clone());
        }
    }
    criteria
}

// ---------------------------------------------------------------------------
// Agent naming
// ---------------------------------------------------------------------------

/// Generate an agent name following the `<role>/<repo>/<handoff-slug>/<run>` convention.
pub fn agent_name(role: &AgentRole, repo: &str, handoff_slug: &str, run: u32) -> String {
    format!("{role}/{repo}/{handoff_slug}/{run}")
}

/// Normalize a handoff title into a URL-safe slug.
///
/// Lowercases the input, replaces whitespace and non-alphanumeric characters
/// (except hyphens) with hyphens, collapses runs of hyphens, and trims
/// leading/trailing hyphens.
pub fn handoff_slug(title: &str) -> String {
    let slug: String = title
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();

    // Collapse consecutive hyphens.
    let mut collapsed = String::with_capacity(slug.len());
    let mut prev_hyphen = false;
    for c in slug.chars() {
        if c == '-' {
            if !prev_hyphen {
                collapsed.push('-');
            }
            prev_hyphen = true;
        } else {
            collapsed.push(c);
            prev_hyphen = false;
        }
    }

    collapsed.trim_matches('-').to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::MockCanopyClient;
    use crate::parser::{ParsedHandoff, ParsedStep};
    use crate::workflow::template::impl_audit_default;

    /// Build a minimal `ParsedHandoff` for testing.
    fn test_handoff() -> ParsedHandoff {
        ParsedHandoff {
            title: "Canopy Dispatch Integration".to_string(),
            metadata: None,
            problem: "Need to bridge hymenium workflows to canopy tasks".to_string(),
            state: vec!["stub dispatch.rs exists".to_string()],
            intent: "Implement dispatch layer".to_string(),
            steps: vec![ParsedStep {
                number: 1,
                title: "Implement dispatch".to_string(),
                project: None,
                effort: None,
                depends_on: Vec::new(),
                description: "Write the dispatch module".to_string(),
                files_to_modify: Vec::new(),
                verification: None,
                checklist: Vec::new(),
            }],
            completion_protocol: None,
            context: None,
        }
    }

    // -- dispatch_workflow ----------------------------------------------------

    #[test]
    fn dispatch_creates_parent_and_phase_subtasks() {
        let mock = MockCanopyClient::new();
        let template = impl_audit_default();
        let handoff = test_handoff();

        let instance = dispatch_workflow(
            &handoff,
            &template,
            &WorkflowId("wf-1".to_string()),
            "/handoffs/test.md",
            &mock,
        )
        .expect("dispatch should succeed");

        // 1 parent + 2 phase subtasks = 3 tasks total.
        assert_eq!(mock.task_count(), 3);

        // Both phase states have canopy task IDs.
        assert!(instance.phase_states[0].canopy_task_id.is_some());
        assert!(instance.phase_states[1].canopy_task_id.is_some());
    }

    #[test]
    fn dispatch_assigns_first_phase_agent() {
        let mock = MockCanopyClient::new();
        let template = impl_audit_default();
        let handoff = test_handoff();

        let instance = dispatch_workflow(
            &handoff,
            &template,
            &WorkflowId("wf-2".to_string()),
            "/handoffs/test.md",
            &mock,
        )
        .expect("dispatch should succeed");

        // First phase should have an assigned agent using reset role name.
        let agent = instance.phase_states[0]
            .agent_id
            .as_ref()
            .expect("first phase should have agent");
        assert!(agent.starts_with("Worker/"));

        // Second phase should not be assigned yet.
        assert!(instance.phase_states[1].agent_id.is_none());
    }

    #[test]
    fn dispatch_sets_status_dispatched() {
        let mock = MockCanopyClient::new();
        let template = impl_audit_default();
        let handoff = test_handoff();

        let instance = dispatch_workflow(
            &handoff,
            &template,
            &WorkflowId("wf-3".to_string()),
            "/handoffs/test.md",
            &mock,
        )
        .expect("dispatch should succeed");

        assert_eq!(
            instance.status,
            crate::workflow::engine::WorkflowStatus::Dispatched
        );
    }

    // -- agent_name -----------------------------------------------------------

    #[test]
    fn agent_name_follows_convention() {
        let name = agent_name(&AgentRole::Worker, "spore", "otel-foundation", 1);
        assert_eq!(name, "Worker/spore/otel-foundation/1");

        let name = agent_name(&AgentRole::OutputVerifier, "hymenium", "dispatch-layer", 2);
        assert_eq!(name, "Output Verifier/hymenium/dispatch-layer/2");
    }

    // -- handoff_slug ---------------------------------------------------------

    #[test]
    fn slug_lowercases_and_replaces_spaces() {
        assert_eq!(
            handoff_slug("Canopy Dispatch Integration"),
            "canopy-dispatch-integration"
        );
    }

    #[test]
    fn slug_collapses_special_chars() {
        assert_eq!(
            handoff_slug("feat: add -- new  stuff!"),
            "feat-add-new-stuff"
        );
    }

    #[test]
    fn slug_trims_leading_trailing_hyphens() {
        assert_eq!(handoff_slug("--hello world--"), "hello-world");
    }

    #[test]
    fn slug_handles_empty_string() {
        assert_eq!(handoff_slug(""), "");
    }

    // -- dispatch with impl-audit template ------------------------------------

    #[test]
    fn dispatch_impl_audit_template_end_to_end() {
        let mock = MockCanopyClient::new();
        let template = impl_audit_default();
        let handoff = test_handoff();

        let instance = dispatch_workflow(
            &handoff,
            &template,
            &WorkflowId("e2e-1".to_string()),
            "/handoffs/test.md",
            &mock,
        )
        .expect("dispatch should succeed");

        // Correct number of phases.
        assert_eq!(instance.phase_states.len(), 2);

        // Phase IDs match template.
        assert_eq!(instance.phase_states[0].phase_id, "implement");
        assert_eq!(instance.phase_states[1].phase_id, "audit");

        // First phase subtask was assigned in canopy.
        let first_task_id = instance.phase_states[0]
            .canopy_task_id
            .as_ref()
            .expect("should have task id");
        let task = mock.get_task(first_task_id).expect("task should exist");
        assert!(
            task.agent_id.is_some(),
            "first phase task should be assigned"
        );

        // Second phase subtask exists but is not assigned.
        let second_task_id = instance.phase_states[1]
            .canopy_task_id
            .as_ref()
            .expect("should have task id");
        let task = mock.get_task(second_task_id).expect("task should exist");
        assert!(
            task.agent_id.is_none(),
            "second phase task should not be assigned yet"
        );
    }

    #[test]
    fn dispatch_rejects_empty_template() {
        let mock = MockCanopyClient::new();
        let template = crate::workflow::template::WorkflowTemplate {
            schema_version: "1.0".to_string(),
            template_id: "empty".to_string(),
            name: "Empty".to_string(),
            description: "No phases".to_string(),
            phases: vec![],
            transitions: vec![],
        };
        let handoff = test_handoff();

        let result = dispatch_workflow(
            &handoff,
            &template,
            &WorkflowId("wf-empty".to_string()),
            "/handoffs/test.md",
            &mock,
        );
        assert!(result.is_err());
        assert_eq!(mock.task_count(), 0); // no tasks created
    }

    #[test]
    fn dispatch_rejects_empty_slug_title() {
        let mock = MockCanopyClient::new();
        let template = impl_audit_default();
        let mut handoff = test_handoff();
        handoff.title = "!!!".to_string(); // produces empty slug

        let result = dispatch_workflow(
            &handoff,
            &template,
            &WorkflowId("wf-slug".to_string()),
            "/handoffs/test.md",
            &mock,
        );
        assert!(result.is_err());
    }

    #[test]
    fn slug_handles_unicode() {
        // Unicode letters survive (is_alphanumeric is true for them)
        let slug = handoff_slug("Ünïcödé Title");
        assert!(!slug.is_empty());
        assert!(slug.contains("title"));
    }

    #[test]
    fn slug_all_symbols_produces_empty() {
        assert_eq!(handoff_slug("!@#$%^&*()"), "");
    }

    #[test]
    fn dispatch_agent_name_uses_repo_basename() {
        let mock = MockCanopyClient::new();
        let template = impl_audit_default();
        let mut handoff = test_handoff();
        handoff.metadata = Some(crate::parser::HandoffMetadata {
            dispatchability: crate::parser::Dispatchability::Direct,
            owning_repo: "/path/to/hymenium".to_string(),
            allowed_write_scope: vec!["src/".to_string()],
            cross_repo_rule: None,
            non_goals: Vec::new(),
            verification_contract: "cargo test".to_string(),
            completion_update: String::new(),
            source_scope: None,
        });

        let instance = dispatch_workflow(
            &handoff,
            &template,
            &WorkflowId("wf-basename".to_string()),
            "/handoffs/test.md",
            &mock,
        )
        .expect("dispatch should succeed");

        let agent = instance.phase_states[0]
            .agent_id
            .as_ref()
            .expect("first phase should have agent");
        // Should use "hymenium" not "/path/to/hymenium"
        assert!(
            agent.starts_with("Worker/hymenium/"),
            "agent name was: {agent}"
        );
    }

    #[test]
    fn dispatch_focus_topic_uses_first_phase_target() {
        let template = impl_audit_default();

        assert_eq!(
            dispatch_focus_topic(&template).as_deref(),
            Some("implement implementer")
        );
    }

    // -- task packet quality tests (H5) ---------------------------------------

    #[test]
    fn dispatch_subtask_title_includes_handoff_title_and_separator() {
        use std::cell::RefCell;

        struct TitleCapturingMock {
            inner: MockCanopyClient,
            subtask_titles: RefCell<Vec<String>>,
        }

        impl TitleCapturingMock {
            fn new() -> Self {
                Self {
                    inner: MockCanopyClient::new(),
                    subtask_titles: RefCell::new(Vec::new()),
                }
            }
        }

        use crate::dispatch::{
            CanopyClient, CompletenessReport, DispatchError, ImportResult, TaskDetail,
        };

        impl CanopyClient for TitleCapturingMock {
            fn create_task(
                &self,
                title: &str,
                description: &str,
                project_root: &str,
                options: &TaskOptions,
            ) -> Result<String, DispatchError> {
                self.inner
                    .create_task(title, description, project_root, options)
            }

            fn create_subtask(
                &self,
                parent_id: &str,
                title: &str,
                description: &str,
                options: &TaskOptions,
            ) -> Result<String, DispatchError> {
                self.subtask_titles.borrow_mut().push(title.to_string());
                self.inner
                    .create_subtask(parent_id, title, description, options)
            }

            fn assign_task(
                &self,
                task_id: &str,
                agent_id: &str,
                assigned_by: &str,
            ) -> Result<(), DispatchError> {
                self.inner.assign_task(task_id, agent_id, assigned_by)
            }

            fn get_task(&self, task_id: &str) -> Result<TaskDetail, DispatchError> {
                self.inner.get_task(task_id)
            }

            fn check_completeness(
                &self,
                handoff_path: &str,
            ) -> Result<CompletenessReport, DispatchError> {
                self.inner.check_completeness(handoff_path)
            }

            fn import_handoff(
                &self,
                path: &str,
                assign_to: Option<&str>,
            ) -> Result<ImportResult, DispatchError> {
                self.inner.import_handoff(path, assign_to)
            }
        }

        let capturing = TitleCapturingMock::new();
        let template = impl_audit_default();
        let handoff = test_handoff(); // title: "Canopy Dispatch Integration"

        dispatch_workflow(
            &handoff,
            &template,
            &WorkflowId("wf-title-1".to_string()),
            "/handoffs/test.md",
            &capturing,
        )
        .expect("dispatch should succeed");

        let titles = capturing.subtask_titles.borrow();
        assert_eq!(titles.len(), 2, "impl-audit has 2 phases");

        // Each title should contain the phase_id, the em-dash separator, and the handoff title.
        for title in titles.iter() {
            assert!(
                title.contains('\u{2014}'),
                "title missing em-dash separator: {title}"
            );
            assert!(
                title.contains("Canopy Dispatch Integration"),
                "title missing handoff title: {title}"
            );
        }

        // Spot-check the first phase title format.
        assert!(
            titles[0].starts_with("[implementer] implement"),
            "unexpected first title: {}",
            titles[0]
        );
    }

    #[test]
    fn non_goals_constraint_preserves_commas() {
        let handoff = ParsedHandoff {
            title: "Test".to_string(),
            metadata: Some(crate::parser::HandoffMetadata {
                dispatchability: crate::parser::Dispatchability::Direct,
                owning_repo: "hymenium".to_string(),
                allowed_write_scope: vec![],
                cross_repo_rule: None,
                non_goals: vec!["no foo, bar, or baz".to_string()],
                verification_contract: String::new(),
                completion_update: String::new(),
                source_scope: None,
            }),
            problem: "p".to_string(),
            state: vec![],
            intent: "i".to_string(),
            steps: vec![],
            completion_protocol: None,
            context: None,
        };

        let constraints = build_constraints(&handoff);

        // Non-goal with commas must appear as a single constraint, not split.
        let non_goal_constraints: Vec<_> = constraints
            .iter()
            .filter(|c| c.starts_with("Non-goal"))
            .collect();
        assert_eq!(
            non_goal_constraints.len(),
            1,
            "expected exactly one non-goal constraint, got: {:?}",
            non_goal_constraints
        );
        assert_eq!(
            non_goal_constraints[0],
            "Non-goal (do not implement): no foo, bar, or baz"
        );
    }

    #[test]
    fn tools_for_write_scope_docs_only_excludes_write() {
        let scope = vec!["docs/".to_string()];
        let tools = tools_for_write_scope(&scope);
        assert!(
            !tools.contains(&"write".to_string()),
            "docs-only scope should not include write, got: {tools:?}"
        );
        assert!(tools.contains(&"bash".to_string()));
        assert!(tools.contains(&"read".to_string()));
    }

    #[test]
    fn tools_for_write_scope_src_includes_write() {
        let scope = vec!["src/".to_string()];
        let tools = tools_for_write_scope(&scope);
        assert!(
            tools.contains(&"write".to_string()),
            "src/ scope should include write, got: {tools:?}"
        );
    }

    #[test]
    fn tools_for_write_scope_rs_extension_includes_write() {
        let scope = vec!["hymenium/src/dispatch/orchestrate.rs".to_string()];
        let tools = tools_for_write_scope(&scope);
        assert!(
            tools.contains(&"write".to_string()),
            ".rs scope should include write, got: {tools:?}"
        );
    }

    #[test]
    fn tools_for_write_scope_empty_excludes_write() {
        let tools = tools_for_write_scope(&[]);
        assert!(
            !tools.contains(&"write".to_string()),
            "empty scope should not include write, got: {tools:?}"
        );
    }

    #[test]
    fn artifact_boundary_constraint_added_for_read_only_source_task() {
        let handoff = ParsedHandoff {
            title: "Read-only audit task".to_string(),
            metadata: Some(crate::parser::HandoffMetadata {
                dispatchability: crate::parser::Dispatchability::Direct,
                owning_repo: "hymenium".to_string(),
                allowed_write_scope: vec!["docs/audit/".to_string()],
                cross_repo_rule: None,
                non_goals: vec![],
                verification_contract: String::new(),
                completion_update: String::new(),
                source_scope: None,
            }),
            problem: "p".to_string(),
            state: vec![],
            intent: "i".to_string(),
            steps: vec![],
            completion_protocol: None,
            context: None,
        };

        let constraints = build_constraints(&handoff);

        let boundary: Vec<_> = constraints
            .iter()
            .filter(|c| c.starts_with("Source code is read-only"))
            .collect();
        assert_eq!(
            boundary.len(),
            1,
            "expected exactly one artifact boundary constraint, got: {:?}",
            constraints
        );
        assert!(
            boundary[0].contains("docs/audit/"),
            "boundary constraint should include the artifact path: {}",
            boundary[0]
        );
    }

    #[test]
    fn dispatch_compresses_when_context_budget_is_exceeded() {
        let mock = MockCanopyClient::new();
        let template = impl_audit_default();
        let mut handoff = test_handoff();
        handoff.problem = "problem ".repeat(40);
        handoff.intent = "intent ".repeat(30);
        handoff.context = Some("background context".to_string());
        handoff.steps[0].description = "implement target ".repeat(40);

        let instance = dispatch_workflow(
            &handoff,
            &template,
            &WorkflowId("wf-compress".to_string()),
            "/handoffs/test.md",
            &mock,
        )
        .expect("dispatch should succeed");

        let description = mock
            .stored_description("mock-task-1")
            .expect("parent description should be stored");
        assert!(description.contains("Compressed context"));
        assert!(description.contains("implement"));
        assert!(!description.contains("problem problem problem problem problem problem problem problem problem problem problem problem"));
        assert!(estimate_text_tokens(&description) <= DISPATCH_CONTEXT_TOKEN_BUDGET);
        assert_eq!(instance.phase_states.len(), 2);
    }

    // -- capability requirement dispatch tests -----------------------------------

    #[test]
    fn dispatch_emits_rust_capabilities_for_hymenium_repo() {
        let mock = MockCanopyClient::new();
        let template = impl_audit_default();
        let mut handoff = test_handoff();
        handoff.metadata = Some(crate::parser::HandoffMetadata {
            dispatchability: crate::parser::Dispatchability::Direct,
            owning_repo: "/path/to/hymenium".to_string(),
            allowed_write_scope: vec!["src/".to_string()],
            cross_repo_rule: None,
            non_goals: Vec::new(),
            verification_contract: "cargo test".to_string(),
            completion_update: String::new(),
            source_scope: None,
        });

        dispatch_workflow(
            &handoff,
            &template,
            &WorkflowId("wf-caps-1".to_string()),
            "/handoffs/test.md",
            &mock,
        )
        .expect("dispatch should succeed");

        // Parent task (mock-task-1) should carry "rust" capability.
        let parent = mock
            .get_task("mock-task-1")
            .expect("parent task should exist");
        assert!(
            parent.required_capabilities.contains(&"rust".to_string()),
            "expected rust capability on parent task, got: {:?}",
            parent.required_capabilities
        );

        // Phase subtask (mock-task-2) should also carry "rust".
        let phase = mock
            .get_task("mock-task-2")
            .expect("phase task should exist");
        assert!(
            phase.required_capabilities.contains(&"rust".to_string()),
            "expected rust capability on phase task, got: {:?}",
            phase.required_capabilities
        );
    }

    #[test]
    fn dispatch_emits_no_capabilities_for_unknown_repo() {
        let mock = MockCanopyClient::new();
        let template = impl_audit_default();
        let handoff = test_handoff(); // metadata is None, project_root defaults to "."

        dispatch_workflow(
            &handoff,
            &template,
            &WorkflowId("wf-caps-2".to_string()),
            "/handoffs/test.md",
            &mock,
        )
        .expect("dispatch should succeed with empty capabilities");

        // Parent task should have empty capabilities — any agent can claim it.
        let parent = mock
            .get_task("mock-task-1")
            .expect("parent task should exist");
        assert!(
            parent.required_capabilities.is_empty(),
            "expected no capabilities for unknown repo, got: {:?}",
            parent.required_capabilities
        );
    }

    #[test]
    fn dispatch_emits_schema_capabilities_for_septa_repo() {
        let mock = MockCanopyClient::new();
        let template = impl_audit_default();
        let mut handoff = test_handoff();
        handoff.metadata = Some(crate::parser::HandoffMetadata {
            dispatchability: crate::parser::Dispatchability::Direct,
            owning_repo: "septa".to_string(),
            allowed_write_scope: vec![],
            cross_repo_rule: None,
            non_goals: Vec::new(),
            verification_contract: String::new(),
            completion_update: String::new(),
            source_scope: None,
        });

        dispatch_workflow(
            &handoff,
            &template,
            &WorkflowId("wf-caps-3".to_string()),
            "/handoffs/test.md",
            &mock,
        )
        .expect("dispatch should succeed");

        let parent = mock
            .get_task("mock-task-1")
            .expect("parent task should exist");
        assert!(
            parent.required_capabilities.contains(&"schema".to_string()),
            "expected schema capability for septa repo, got: {:?}",
            parent.required_capabilities
        );
    }

    // -- runtime identity tests -----------------------------------------------

    /// Regression: `handoff_path` on the instance must reflect the actual file
    /// path passed to `dispatch_workflow`, not the `owning_repo` metadata field.
    #[test]
    fn dispatch_runtime_identity_records_actual_handoff_path() {
        let mock = MockCanopyClient::new();
        let template = impl_audit_default();
        let mut handoff = test_handoff();
        // owning_repo is a repo name — this should NOT appear as the handoff_path.
        handoff.metadata = Some(crate::parser::HandoffMetadata {
            dispatchability: crate::parser::Dispatchability::Direct,
            owning_repo: "ccoCentralCommand".to_string(),
            allowed_write_scope: vec![],
            cross_repo_rule: None,
            non_goals: Vec::new(),
            verification_contract: String::new(),
            completion_update: String::new(),
            source_scope: None,
        });

        let actual_path = "/home/user/.handoffs/ccoCentralCommand/task.md";
        let instance = dispatch_workflow(
            &handoff,
            &template,
            &WorkflowId("wf-path-1".to_string()),
            actual_path,
            &mock,
        )
        .expect("dispatch should succeed");

        assert_eq!(
            instance.handoff_path, actual_path,
            "handoff_path must be the filesystem path, not the owning_repo name"
        );
        assert_ne!(
            instance.handoff_path, "ccoCentralCommand",
            "handoff_path must not be the owning_repo value"
        );
    }

    /// The phase subtask `TaskOptions` must carry the workflow and phase identity
    /// so Canopy can associate the created task with the right workflow row.
    #[test]
    fn dispatch_runtime_identity_phase_options_carry_workflow_and_phase_id() {
        use crate::dispatch::TaskOptions;
        use std::cell::RefCell;

        // Use a capturing mock that records the TaskOptions passed per subtask.
        struct CapturingMock {
            inner: MockCanopyClient,
            subtask_options: RefCell<Vec<TaskOptions>>,
        }

        impl CapturingMock {
            fn new() -> Self {
                Self {
                    inner: MockCanopyClient::new(),
                    subtask_options: RefCell::new(Vec::new()),
                }
            }
        }

        use crate::dispatch::{
            CanopyClient, CompletenessReport, DispatchError, ImportResult, TaskDetail,
        };

        impl CanopyClient for CapturingMock {
            fn create_task(
                &self,
                title: &str,
                description: &str,
                project_root: &str,
                options: &TaskOptions,
            ) -> Result<String, DispatchError> {
                self.inner
                    .create_task(title, description, project_root, options)
            }

            fn create_subtask(
                &self,
                parent_id: &str,
                title: &str,
                description: &str,
                options: &TaskOptions,
            ) -> Result<String, DispatchError> {
                self.subtask_options.borrow_mut().push(options.clone());
                self.inner
                    .create_subtask(parent_id, title, description, options)
            }

            fn assign_task(
                &self,
                task_id: &str,
                agent_id: &str,
                assigned_by: &str,
            ) -> Result<(), DispatchError> {
                self.inner.assign_task(task_id, agent_id, assigned_by)
            }

            fn get_task(&self, task_id: &str) -> Result<TaskDetail, DispatchError> {
                self.inner.get_task(task_id)
            }

            fn check_completeness(
                &self,
                handoff_path: &str,
            ) -> Result<CompletenessReport, DispatchError> {
                self.inner.check_completeness(handoff_path)
            }

            fn import_handoff(
                &self,
                path: &str,
                assign_to: Option<&str>,
            ) -> Result<ImportResult, DispatchError> {
                self.inner.import_handoff(path, assign_to)
            }
        }

        let capturing = CapturingMock::new();
        let template = impl_audit_default();
        let handoff = test_handoff();

        dispatch_workflow(
            &handoff,
            &template,
            &WorkflowId("wf-id-check".to_string()),
            "/handoffs/test.md",
            &capturing,
        )
        .expect("dispatch should succeed");

        let opts = capturing.subtask_options.borrow();
        assert_eq!(opts.len(), 2, "impl-audit has 2 phases");

        // First phase (implement)
        assert_eq!(
            opts[0].workflow_id.as_deref(),
            Some("wf-id-check"),
            "first subtask must carry workflow_id"
        );
        assert_eq!(
            opts[0].phase_id.as_deref(),
            Some("implement"),
            "first subtask must carry phase_id = implement"
        );

        // Second phase (audit)
        assert_eq!(
            opts[1].workflow_id.as_deref(),
            Some("wf-id-check"),
            "second subtask must carry workflow_id"
        );
        assert_eq!(
            opts[1].phase_id.as_deref(),
            Some("audit"),
            "second subtask must carry phase_id = audit"
        );
    }
}
