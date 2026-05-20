//! DAG executor with topological sort and concurrent node execution.

use crate::workflow::dag::loader::WorkflowDag;
use crate::workflow::dag::node::{DagNode, TriggerRule};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use tokio::task::JoinSet;

/// Result of a single node execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NodeResult {
    pub node_id: String,
    pub status: NodeStatus,
    pub output: String,
}

/// Execution status of a node.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    Success,
    Failed,
    Skipped,
}

/// DAG executor: manages topological execution with parallel branches.
pub struct DagExecutor {
    pub env: HashMap<String, String>,
}

impl DagExecutor {
    /// Execute a DAG workflow, returning results for all nodes.
    pub async fn run(&self, dag: &WorkflowDag) -> Result<Vec<NodeResult>> {
        // Build adjacency lists for the DAG
        let mut incoming: HashMap<String, Vec<String>> = HashMap::new();
        let mut outgoing: HashMap<String, Vec<String>> = HashMap::new();
        let mut node_map: HashMap<String, &DagNode> = HashMap::new();

        for node in &dag.nodes {
            incoming.insert(node.id().to_string(), Vec::new());
            outgoing.insert(node.id().to_string(), Vec::new());
            node_map.insert(node.id().to_string(), node);
        }

        for edge in &dag.edges {
            outgoing
                .entry(edge.from.clone())
                .or_default()
                .push(edge.to.clone());
            incoming
                .entry(edge.to.clone())
                .or_default()
                .push(edge.from.clone());
        }

        // Build a separate predecessors map from the original edges BEFORE topological sort.
        // This map will be used for execution readiness checks and is not mutated by the sort.
        let mut predecessors: HashMap<String, Vec<String>> = HashMap::new();
        for node in &dag.nodes {
            predecessors.insert(node.id().to_string(), Vec::new());
        }
        for edge in &dag.edges {
            predecessors
                .entry(edge.to.clone())
                .or_default()
                .push(edge.from.clone());
        }

        // Topological sort using Kahn's algorithm
        let mut queue: VecDeque<String> = VecDeque::new();
        for node in &dag.nodes {
            if incoming[node.id()].is_empty() {
                queue.push_back(node.id().to_string());
            }
        }

        let mut topo_order = Vec::new();
        while let Some(node_id) = queue.pop_front() {
            topo_order.push(node_id.clone());
            for next_id in &outgoing[&node_id] {
                incoming
                    .get_mut(next_id)
                    .unwrap()
                    .retain(|id| id != &node_id);
                if incoming[next_id].is_empty() {
                    queue.push_back(next_id.clone());
                }
            }
        }

        // Cycle detection: if not all nodes are in topological order, there's a cycle
        if topo_order.len() != dag.nodes.len() {
            return Err(anyhow!(
                "workflow DAG contains a cycle; {} node(s) are unreachable",
                dag.nodes.len() - topo_order.len()
            ));
        }

        // Execute nodes in waves: all ready nodes in parallel
        let mut results: HashMap<String, NodeResult> = HashMap::new();
        let mut completed: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut skipped: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut merged_env = self.env.clone();
        merged_env.extend(dag.env.clone());

        loop {
            // Find all ready nodes
            let mut ready = Vec::new();
            for node_id in &topo_order {
                if completed.contains(node_id) || skipped.contains(node_id) {
                    continue;
                }

                // Check predecessors based on trigger_rule
                let node = node_map[node_id];
                let node_predecessors = &predecessors[node_id];

                if node_predecessors.is_empty() {
                    ready.push(node_id.clone());
                } else {
                    let pred_results: Vec<_> = node_predecessors
                        .iter()
                        .filter_map(|pid| results.get(pid))
                        .collect();

                    match node.trigger_rule() {
                        TriggerRule::AllSuccess => {
                            if pred_results.len() == node_predecessors.len()
                                && pred_results.iter().all(|r| r.status == NodeStatus::Success)
                            {
                                ready.push(node_id.clone());
                            }
                        }
                        TriggerRule::OneSuccess => {
                            if pred_results.iter().any(|r| r.status == NodeStatus::Success) {
                                ready.push(node_id.clone());
                            }
                        }
                    }
                }
            }

            if ready.is_empty() {
                break; // No more ready nodes
            }

            // Execute all ready nodes concurrently
            let mut join_set = JoinSet::new();
            for node_id in &ready {
                let node = node_map[node_id].clone();
                let env = merged_env.clone();
                let results_clone = results.clone();
                join_set.spawn(execute_node(node, env, results_clone));
            }

            while let Some(res) = join_set.join_next().await {
                match res {
                    Ok(Ok(result)) => {
                        completed.insert(result.node_id.clone());
                        results.insert(result.node_id.clone(), result);
                    }
                    Ok(Err(e)) => return Err(e),
                    Err(e) => return Err(anyhow!("task join error: {}", e)),
                }
            }

            // For nodes that just completed, check their successors for one_success trigger rule
            // If a successor has one_success and any predecessor succeeded, mark other predecessors as skipped
            for node_id in &ready {
                if let Some(result) = results.get(node_id) {
                    if result.status == NodeStatus::Success {
                        // Check each successor of this completed node
                        for succ_id in &outgoing[node_id] {
                            let succ_node = node_map[succ_id];
                            // If the successor has one_success and hasn't fired yet, skip other predecessors
                            if succ_node.trigger_rule() == TriggerRule::OneSuccess
                                && !completed.contains(succ_id)
                                && !skipped.contains(succ_id)
                            {
                                // Mark all OTHER predecessors of this successor as skipped
                                for other_pred in &predecessors[succ_id] {
                                    if other_pred != node_id
                                        && !completed.contains(other_pred)
                                        && !skipped.contains(other_pred)
                                    {
                                        skipped.insert(other_pred.clone());
                                        results.insert(
                                            other_pred.clone(),
                                            NodeResult {
                                                node_id: other_pred.clone(),
                                                status: NodeStatus::Skipped,
                                                output: String::new(),
                                            },
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Collect results in topological order
        // Ensure all nodes have a result entry; nodes with no result are marked as Skipped
        let mut final_results = Vec::new();
        for node_id in &topo_order {
            if let Some(result) = results.get(node_id) {
                final_results.push(result.clone());
            } else {
                // Node has no result entry; mark it as Skipped
                final_results.push(NodeResult {
                    node_id: node_id.clone(),
                    status: NodeStatus::Skipped,
                    output: String::new(),
                });
            }
        }

        Ok(final_results)
    }
}

/// Execute a single node and return its result.
async fn execute_node(
    node: DagNode,
    env: HashMap<String, String>,
    prior_results: HashMap<String, NodeResult>,
) -> Result<NodeResult> {
    match node {
        DagNode::Bash(bash_node) => {
            let expanded_cmd = expand_variables(&bash_node.command, &env, &prior_results);
            match tokio::process::Command::new("sh")
                .arg("-c")
                .arg(&expanded_cmd)
                .envs(env.clone())
                .output()
                .await
            {
                Ok(output) => {
                    let success = output.status.success();
                    let output_str = String::from_utf8_lossy(&output.stdout).to_string();
                    Ok(NodeResult {
                        node_id: bash_node.id.clone(),
                        status: if success {
                            NodeStatus::Success
                        } else {
                            NodeStatus::Failed
                        },
                        output: output_str,
                    })
                }
                Err(e) => Ok(NodeResult {
                    node_id: bash_node.id.clone(),
                    status: NodeStatus::Failed,
                    output: format!("execution error: {}", e),
                }),
            }
        }

        DagNode::Command(cmd_node) => {
            match which::which(&cmd_node.skill) {
                Ok(bin_path) => {
                    let mut cmd = tokio::process::Command::new(bin_path);
                    for arg in &cmd_node.args {
                        let expanded = expand_variables(arg, &env, &prior_results);
                        cmd.arg(expanded);
                    }

                    match cmd.output().await {
                        Ok(output) => {
                            let success = output.status.success();
                            let output_str = String::from_utf8_lossy(&output.stdout).to_string();
                            Ok(NodeResult {
                                node_id: cmd_node.id.clone(),
                                status: if success {
                                    NodeStatus::Success
                                } else {
                                    NodeStatus::Failed
                                },
                                output: output_str,
                            })
                        }
                        Err(e) => Ok(NodeResult {
                            node_id: cmd_node.id.clone(),
                            status: NodeStatus::Failed,
                            output: format!("execution error: {}", e),
                        }),
                    }
                }
                Err(_) => Ok(NodeResult {
                    node_id: cmd_node.id.clone(),
                    status: NodeStatus::Failed,
                    output: format!("skill '{}' not found in PATH", cmd_node.skill),
                }),
            }
        }

        DagNode::Prompt(ref _prompt_node) => {
            // TODO: dispatch via canopy
            Ok(NodeResult {
                node_id: node.id().to_string(),
                status: NodeStatus::Skipped,
                output: String::new(),
            })
        }

        DagNode::Loop(ref _loop_node) => {
            // TODO: dispatch via canopy
            Ok(NodeResult {
                node_id: node.id().to_string(),
                status: NodeStatus::Skipped,
                output: String::new(),
            })
        }

        DagNode::Approval(approval_node) => {
            println!("{}", approval_node.prompt);
            // Wrap stdin read in spawn_blocking to avoid blocking tokio worker thread
            let response = tokio::task::spawn_blocking(|| {
                let mut input = String::new();
                match std::io::stdin().read_line(&mut input) {
                    Ok(_) => input.trim().to_lowercase(),
                    Err(_) => String::new(),
                }
            })
            .await
            .unwrap_or_default();

            let success = response == "y" || response == "yes";
            Ok(NodeResult {
                node_id: approval_node.id.clone(),
                status: if success {
                    NodeStatus::Success
                } else {
                    NodeStatus::Failed
                },
                output: response,
            })
        }
    }
}

/// Expand $NODE_ID.output references in a string.
///
/// Environment variables ($ENV_KEY) are NOT expanded here; they are passed via process environment
/// to the subprocess via `.envs()` on the Command object to prevent shell injection attacks.
fn expand_variables(
    text: &str,
    _env: &HashMap<String, String>,
    results: &HashMap<String, NodeResult>,
) -> String {
    let mut result = text.to_string();

    // Expand $node_id.output references, sorted longest-first to avoid prefix collision
    let mut vars: Vec<(&str, &str)> = results
        .iter()
        .map(|(k, v)| (k.as_str(), v.output.as_str()))
        .collect();
    vars.sort_by_key(|(k, _)| std::cmp::Reverse(k.len()));

    for (node_id, output) in vars {
        let pattern = format!("${}.output", node_id);
        result = result.replace(&pattern, output);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::dag::loader::DagEdge;
    use crate::workflow::dag::node::{CommandNode, TriggerRule};

    #[tokio::test]
    async fn test_simple_linear_dag() {
        let dag = WorkflowDag {
            workflow_id: "test".to_string(),
            name: "Test".to_string(),
            description: "Test workflow".to_string(),
            nodes: vec![
                DagNode::Command(CommandNode {
                    id: "node-a".to_string(),
                    skill: "true".to_string(),
                    args: vec![],
                    trigger_rule: TriggerRule::AllSuccess,
                }),
                DagNode::Command(CommandNode {
                    id: "node-b".to_string(),
                    skill: "true".to_string(),
                    args: vec![],
                    trigger_rule: TriggerRule::AllSuccess,
                }),
            ],
            edges: vec![DagEdge {
                from: "node-a".to_string(),
                to: "node-b".to_string(),
            }],
            env: HashMap::new(),
        };

        let executor = DagExecutor {
            env: HashMap::new(),
        };

        let results = executor.run(&dag).await.unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].node_id, "node-a");
        assert_eq!(results[0].status, NodeStatus::Success);
        assert_eq!(results[1].node_id, "node-b");
        assert_eq!(results[1].status, NodeStatus::Success);
    }

    #[tokio::test]
    async fn test_parallel_branches_with_one_success_fan_in() {
        // The review-pr workflow from the fixture:
        // review-code, review-errors, review-tests run in parallel
        // synthesize fires with trigger_rule: one_success
        let dag = WorkflowDag {
            workflow_id: "review-pr".to_string(),
            name: "Parallel PR Review".to_string(),
            description: "Test workflow".to_string(),
            nodes: vec![
                DagNode::Command(CommandNode {
                    id: "review-code".to_string(),
                    skill: "true".to_string(),
                    args: vec![],
                    trigger_rule: TriggerRule::AllSuccess,
                }),
                DagNode::Command(CommandNode {
                    id: "review-errors".to_string(),
                    skill: "true".to_string(),
                    args: vec![],
                    trigger_rule: TriggerRule::AllSuccess,
                }),
                DagNode::Command(CommandNode {
                    id: "review-tests".to_string(),
                    skill: "true".to_string(),
                    args: vec![],
                    trigger_rule: TriggerRule::AllSuccess,
                }),
                DagNode::Prompt(crate::workflow::dag::node::PromptNode {
                    id: "synthesize".to_string(),
                    prompt: "Synthesize findings".to_string(),
                    model: None,
                    context: crate::workflow::dag::node::ContextMode::Fresh,
                    trigger_rule: TriggerRule::OneSuccess,
                    allowed_tools: None,
                    denied_tools: None,
                    hooks: None,
                }),
            ],
            edges: vec![
                DagEdge {
                    from: "review-code".to_string(),
                    to: "synthesize".to_string(),
                },
                DagEdge {
                    from: "review-errors".to_string(),
                    to: "synthesize".to_string(),
                },
                DagEdge {
                    from: "review-tests".to_string(),
                    to: "synthesize".to_string(),
                },
            ],
            env: HashMap::new(),
        };

        let executor = DagExecutor {
            env: HashMap::new(),
        };

        let results = executor.run(&dag).await.unwrap();

        // All 4 nodes should be in results
        assert_eq!(results.len(), 4);

        // The three reviewers should succeed
        let review_code = results.iter().find(|r| r.node_id == "review-code").unwrap();
        assert_eq!(review_code.status, NodeStatus::Success);

        let review_errors = results
            .iter()
            .find(|r| r.node_id == "review-errors")
            .unwrap();
        assert_eq!(review_errors.status, NodeStatus::Success);

        let review_tests = results
            .iter()
            .find(|r| r.node_id == "review-tests")
            .unwrap();
        assert_eq!(review_tests.status, NodeStatus::Success);

        // The synthesize prompt node should be Skipped (Phase 1 scope)
        let synthesize = results
            .iter()
            .find(|r| r.node_id == "synthesize")
            .unwrap();
        assert_eq!(synthesize.status, NodeStatus::Skipped);
    }

    #[test]
    fn test_expand_variables() {
        let mut results = HashMap::new();
        results.insert(
            "node-a".to_string(),
            NodeResult {
                node_id: "node-a".to_string(),
                status: NodeStatus::Success,
                output: "output from a".to_string(),
            },
        );

        let mut env = HashMap::new();
        env.insert("KEY".to_string(), "value".to_string());

        // expand_variables now only substitutes $node_id.output patterns; env vars are passed via subprocess environment
        let text = "Got: $node-a.output and $KEY";
        let expanded = expand_variables(text, &env, &results);
        assert_eq!(expanded, "Got: output from a and $KEY");
    }
}
