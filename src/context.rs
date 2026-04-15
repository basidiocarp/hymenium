//! Context compression primitives.
//!
//! This module defines a pluggable compression interface plus a minimal
//! in-process engine that can prune to a token budget and then normalize
//! tool-call / tool-result pairs.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors that can occur while compressing context.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ContextError {
    #[error("token budget must be greater than zero")]
    InvalidBudget,
}

/// A single context message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextMessage {
    pub id: String,
    pub role: ContextMessageRole,
    pub content: String,
    pub tool_call_id: Option<String>,
    pub tool_name: Option<String>,
}

impl ContextMessage {
    /// Build a plain text message.
    pub fn text(
        id: impl Into<String>,
        role: ContextMessageRole,
        content: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            role,
            content: content.into(),
            tool_call_id: None,
            tool_name: None,
        }
    }

    /// Build a tool call message.
    pub fn tool_call(
        id: impl Into<String>,
        tool_name: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            role: ContextMessageRole::ToolCall,
            content: content.into(),
            tool_call_id: None,
            tool_name: Some(tool_name.into()),
        }
    }

    /// Build a tool result message.
    pub fn tool_result(
        id: impl Into<String>,
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            role: ContextMessageRole::ToolResult,
            content: content.into(),
            tool_call_id: Some(tool_call_id.into()),
            tool_name: Some(tool_name.into()),
        }
    }

    pub fn token_cost(&self) -> usize {
        estimate_text_tokens(&self.content).saturating_add(4)
    }

    fn is_tool_call(&self) -> bool {
        matches!(self.role, ContextMessageRole::ToolCall)
    }

    fn is_tool_result(&self) -> bool {
        matches!(self.role, ContextMessageRole::ToolResult)
    }
}

/// The role assigned to a context message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ContextMessageRole {
    System,
    User,
    Assistant,
    ToolCall,
    ToolResult,
}

/// Parameters that steer a compression pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CompressionParams {
    pub focus_topic: Option<String>,
    pub token_budget: usize,
}

/// A single item included in a compression report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompressionItem {
    pub message_id: String,
    pub role: ContextMessageRole,
    pub note: String,
}

/// Report describing what changed during compression.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompressionReport {
    pub removed: Vec<CompressionItem>,
    pub summarized: Vec<CompressionItem>,
    pub stubbed: Vec<CompressionItem>,
}

/// The result of a compression pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompressionResult {
    pub messages: Vec<ContextMessage>,
    pub report: CompressionReport,
}

/// Context compression interface.
pub trait ContextEngine {
    fn compress(
        &self,
        messages: &[ContextMessage],
        params: &CompressionParams,
    ) -> Result<CompressionResult, ContextError>;
}

/// Lightweight in-process engine that prunes to a budget and sanitizes tool pairs.
#[derive(Debug, Default, Clone, Copy)]
pub struct BudgetContextEngine;

struct SanitizedRetention {
    output: Vec<ContextMessage>,
    retained_calls: HashSet<String>,
    retained_results: HashSet<String>,
    report: CompressionReport,
    retained_call_positions: HashMap<String, usize>,
    call_names: HashMap<String, String>,
}

impl ContextEngine for BudgetContextEngine {
    fn compress(
        &self,
        messages: &[ContextMessage],
        params: &CompressionParams,
    ) -> Result<CompressionResult, ContextError> {
        if params.token_budget == 0 {
            return Err(ContextError::InvalidBudget);
        }

        let (pruned, mut report) = prune_to_budget(messages, params);
        let (messages, sanitization) = sanitize_tool_pairs(pruned);
        report.removed.extend(sanitization.removed);
        report.stubbed.extend(sanitization.stubbed);

        Ok(CompressionResult { messages, report })
    }
}

fn prune_to_budget(
    messages: &[ContextMessage],
    params: &CompressionParams,
) -> (Vec<ContextMessage>, CompressionReport) {
    let candidate_indices = prioritized_indices(messages, params.focus_topic.as_deref());
    let mut kept_indices = HashSet::new();
    let mut total_tokens = 0usize;
    let mut report = CompressionReport::default();
    let mut truncated_fallback: Option<(usize, ContextMessage)> = None;

    for idx in candidate_indices {
        let message = &messages[idx];
        let cost = message.token_cost();
        if total_tokens.saturating_add(cost) <= params.token_budget {
            kept_indices.insert(idx);
            total_tokens = total_tokens.saturating_add(cost);
            continue;
        }

        if kept_indices.is_empty() {
            if let Some((truncated, summary)) =
                truncate_message_to_budget(message, params.token_budget)
            {
                total_tokens = truncated.token_cost();
                truncated_fallback = Some((idx, truncated));
                kept_indices.insert(idx);
                report.summarized.push(summary);
            } else {
                report.removed.push(CompressionItem {
                    message_id: message.id.clone(),
                    role: message.role,
                    note: "removed to fit token budget".to_string(),
                });
            }
        } else {
            report.removed.push(CompressionItem {
                message_id: message.id.clone(),
                role: message.role,
                note: "removed to fit token budget".to_string(),
            });
        }
    }

    let mut kept = Vec::new();
    for (idx, message) in messages.iter().enumerate() {
        if !kept_indices.contains(&idx) {
            continue;
        }

        if let Some((fallback_idx, truncated)) = truncated_fallback.as_ref() {
            if *fallback_idx == idx {
                kept.push(truncated.clone());
                continue;
            }
        }

        kept.push(message.clone());
    }

    (kept, report)
}

/// Remove orphaned tool results and insert stub results for retained tool calls.
pub fn sanitize_tool_pairs(
    messages: Vec<ContextMessage>,
) -> (Vec<ContextMessage>, CompressionReport) {
    let SanitizedRetention {
        output,
        retained_calls,
        retained_results,
        mut report,
        retained_call_positions,
        call_names,
    } = retain_and_prune(messages);
    let stubs_after_index = insert_missing_tool_stubs(
        &retained_calls,
        &retained_results,
        &retained_call_positions,
        &call_names,
        &mut report,
    );

    (stitch_stubs(output, &stubs_after_index), report)
}

fn retain_and_prune(messages: Vec<ContextMessage>) -> SanitizedRetention {
    let mut retained_calls = HashSet::new();
    let mut retained_results = HashSet::new();
    let mut report = CompressionReport::default();
    let mut output = Vec::with_capacity(messages.len());
    let mut retained_call_positions = HashMap::new();
    let mut call_names = HashMap::new();

    for message in messages {
        if message.is_tool_call() {
            if retained_calls.insert(message.id.clone()) {
                retained_call_positions.insert(message.id.clone(), output.len());
                call_names.insert(
                    message.id.clone(),
                    message
                        .tool_name
                        .clone()
                        .unwrap_or_else(|| "tool".to_string()),
                );
                output.push(message);
            } else {
                report.removed.push(CompressionItem {
                    message_id: message.id,
                    role: message.role,
                    note: "duplicate tool call removed during sanitization".to_string(),
                });
            }
            continue;
        }

        if message.is_tool_result() {
            if let Some(call_id) = message.tool_call_id.as_ref() {
                if retained_call_positions.contains_key(call_id) {
                    if retained_results.insert(call_id.clone()) {
                        output.push(message);
                    } else {
                        report.removed.push(CompressionItem {
                            message_id: message.id,
                            role: message.role,
                            note: "tool result removed during sanitization".to_string(),
                        });
                    }
                } else {
                    report.removed.push(CompressionItem {
                        message_id: message.id,
                        role: message.role,
                        note: "orphaned tool result removed".to_string(),
                    });
                }
            } else {
                report.removed.push(CompressionItem {
                    message_id: message.id,
                    role: message.role,
                    note: "tool result missing call id removed during sanitization".to_string(),
                });
            }
            continue;
        }

        output.push(message);
    }

    SanitizedRetention {
        output,
        retained_calls,
        retained_results,
        report,
        retained_call_positions,
        call_names,
    }
}

fn insert_missing_tool_stubs(
    retained_calls: &HashSet<String>,
    retained_results: &HashSet<String>,
    retained_call_positions: &HashMap<String, usize>,
    call_names: &HashMap<String, String>,
    report: &mut CompressionReport,
) -> BTreeMap<usize, Vec<ContextMessage>> {
    let mut stubs_after_index = BTreeMap::new();

    for call_id in retained_calls {
        if retained_results.contains(call_id) {
            continue;
        }

        if let Some(call_index) = retained_call_positions.get(call_id).copied() {
            if let Some(tool_name) = call_names.get(call_id) {
                let stub = ContextMessage::tool_result(
                    format!("{call_id}-stub"),
                    call_id.clone(),
                    tool_name.clone(),
                    "[stub result inserted after compression]",
                );
                report.stubbed.push(CompressionItem {
                    message_id: stub.id.clone(),
                    role: stub.role,
                    note: format!("stubbed missing tool result for call {call_id}"),
                });
                stubs_after_index
                    .entry(call_index)
                    .or_insert_with(Vec::new)
                    .push(stub);
            }
        }
    }

    stubs_after_index
}

fn stitch_stubs(
    output: Vec<ContextMessage>,
    stubs_after_index: &BTreeMap<usize, Vec<ContextMessage>>,
) -> Vec<ContextMessage> {
    if stubs_after_index.is_empty() {
        return output;
    }

    let stub_count: usize = stubs_after_index.values().map(Vec::len).sum();
    let mut stitched = Vec::with_capacity(output.len() + stub_count);
    for (idx, message) in output.into_iter().enumerate() {
        stitched.push(message);
        if let Some(stubs) = stubs_after_index.get(&idx) {
            stitched.extend(stubs.iter().cloned());
        }
    }

    stitched
}

fn prioritized_indices(messages: &[ContextMessage], focus_topic: Option<&str>) -> Vec<usize> {
    let focus_topic = focus_topic
        .map(str::trim)
        .filter(|topic| !topic.is_empty())
        .map(str::to_lowercase);

    let (mut matching, mut remaining): (Vec<_>, Vec<_>) =
        messages.iter().enumerate().partition(|(_, message)| {
            focus_topic
                .as_ref()
                .is_some_and(|topic| message.content.to_lowercase().contains(topic))
        });

    matching.reverse();
    remaining.reverse();

    matching
        .into_iter()
        .chain(remaining)
        .map(|(idx, _)| idx)
        .collect()
}

fn truncate_message_to_budget(
    message: &ContextMessage,
    token_budget: usize,
) -> Option<(ContextMessage, CompressionItem)> {
    let content_budget = token_budget.saturating_sub(4);
    if content_budget == 0 {
        return None;
    }

    let mut truncated = message.clone();
    let words = truncated.content.split_whitespace().collect::<Vec<_>>();
    if words.len() > content_budget {
        truncated.content = words[..content_budget].join(" ");
    }

    Some((
        truncated,
        CompressionItem {
            message_id: message.id.clone(),
            role: message.role,
            note: "summarized to fit token budget".to_string(),
        },
    ))
}

pub fn estimate_text_tokens(text: &str) -> usize {
    text.split_whitespace().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_removes_orphaned_tool_results() {
        let messages = vec![
            ContextMessage::text("system-1", ContextMessageRole::System, "keep me"),
            ContextMessage::tool_result("result-1", "missing-call", "search", "orphan"),
        ];

        let (messages, report) = sanitize_tool_pairs(messages);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, "system-1");
        assert_eq!(report.removed.len(), 1);
        assert!(report.stubbed.is_empty());
    }

    #[test]
    fn sanitize_inserts_stub_for_missing_result() {
        let messages = vec![ContextMessage::tool_call("call-1", "search", "call")];

        let (messages, report) = sanitize_tool_pairs(messages);

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].id, "call-1");
        assert_eq!(messages[1].tool_call_id.as_deref(), Some("call-1"));
        assert_eq!(report.stubbed.len(), 1);
    }

    #[test]
    fn sanitize_inserts_stub_after_removed_prefix_messages() {
        let messages = vec![
            ContextMessage::text("msg-1", ContextMessageRole::User, "prefix"),
            ContextMessage::tool_call("call-1", "search", "call"),
        ];

        let (messages, report) = sanitize_tool_pairs(vec![messages[1].clone()]);

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].id, "call-1");
        assert_eq!(messages[1].tool_call_id.as_deref(), Some("call-1"));
        assert_eq!(report.stubbed.len(), 1);
    }

    #[test]
    fn compress_prunes_and_sanitizes() {
        let engine = BudgetContextEngine;
        let messages = vec![
            ContextMessage::text(
                "system-1",
                ContextMessageRole::System,
                "alpha beta gamma delta",
            ),
            ContextMessage::tool_call("call-1", "search", "tool call"),
            ContextMessage::tool_result("result-1", "call-1", "search", "tool result"),
        ];
        let params = CompressionParams {
            focus_topic: Some("dispatch".to_string()),
            token_budget: 12,
        };

        let result = engine
            .compress(&messages, &params)
            .expect("compression should succeed");

        assert!(!result.messages.is_empty());
        assert!(result.report.removed.len() + result.report.summarized.len() > 0);
        assert!(
            !result.report.stubbed.is_empty()
                || result
                    .messages
                    .iter()
                    .any(|message| message.role == ContextMessageRole::ToolResult)
        );
    }

    #[test]
    fn compress_truncates_single_oversized_message_to_budget() {
        let engine = BudgetContextEngine;
        let messages = vec![ContextMessage::text(
            "msg-1",
            ContextMessageRole::User,
            "one two three four five six seven eight",
        )];
        let params = CompressionParams {
            focus_topic: None,
            token_budget: 6,
        };

        let result = engine
            .compress(&messages, &params)
            .expect("compression should succeed");

        assert_eq!(result.messages.len(), 1);
        assert!(result.messages[0].token_cost() <= params.token_budget);
        assert_eq!(result.report.summarized.len(), 1);
    }

    #[test]
    fn compress_biases_toward_focus_topic() {
        let engine = BudgetContextEngine;
        let messages = vec![
            ContextMessage::text("msg-1", ContextMessageRole::User, "background noise"),
            ContextMessage::text("msg-2", ContextMessageRole::User, "implement phase target"),
        ];
        let params = CompressionParams {
            focus_topic: Some("implement".to_string()),
            token_budget: 7,
        };

        let result = engine
            .compress(&messages, &params)
            .expect("compression should succeed");

        assert_eq!(result.messages.len(), 1);
        assert_eq!(result.messages[0].id, "msg-2");
    }
}
