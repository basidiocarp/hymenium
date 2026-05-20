//! YAML DAG workflow loader with validation.

use crate::workflow::dag::node::DagNode;
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// A directed edge in the DAG.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagEdge {
    pub from: String,
    pub to: String,
}

/// Complete workflow DAG with metadata and configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowDag {
    pub workflow_id: String,
    pub name: String,
    pub description: String,
    pub nodes: Vec<DagNode>,
    pub edges: Vec<DagEdge>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

/// Load and validate a workflow YAML file.
pub fn load_workflow(path: &Path) -> Result<WorkflowDag> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow!("failed to read workflow file {}: {}", path.display(), e))?;

    let dag: WorkflowDag = serde_yaml::from_str(&content)
        .map_err(|e| anyhow!("failed to parse YAML from {}: {}", path.display(), e))?;

    validate_dag(&dag)?;

    Ok(dag)
}

/// Validate the DAG for consistency.
fn validate_dag(dag: &WorkflowDag) -> Result<()> {
    // Check for duplicate node IDs
    let mut seen_ids = HashSet::new();
    for node in &dag.nodes {
        let id = node.id();
        if !seen_ids.insert(id) {
            return Err(anyhow!("duplicate node ID: '{}'", id));
        }
    }

    // Check that all edge references point to known nodes
    for edge in &dag.edges {
        if !seen_ids.contains(edge.from.as_str()) {
            return Err(anyhow!(
                "edge references unknown source node '{}' (from '{}' to '{}')",
                edge.from,
                edge.from,
                edge.to
            ));
        }
        if !seen_ids.contains(edge.to.as_str()) {
            return Err(anyhow!(
                "edge references unknown target node '{}' (from '{}' to '{}')",
                edge.to,
                edge.from,
                edge.to
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_valid_workflow_loads() {
        let yaml = r#"
workflow_id: test-workflow
name: Test Workflow
description: A test workflow
nodes:
  - kind: command
    id: node-a
    skill: test-skill
edges:
  - from: node-a
    to: node-b
"#;

        let mut file = NamedTempFile::new().unwrap();
        file.write_all(yaml.as_bytes()).unwrap();

        let result = load_workflow(file.path());
        assert!(result.is_err()); // Should fail due to unknown node-b
    }

    #[test]
    fn test_duplicate_node_id_error() {
        let yaml = r#"
workflow_id: test-workflow
name: Test Workflow
description: A test workflow
nodes:
  - kind: command
    id: duplicate-id
    skill: test-skill-1
  - kind: command
    id: duplicate-id
    skill: test-skill-2
edges: []
"#;

        let mut file = NamedTempFile::new().unwrap();
        file.write_all(yaml.as_bytes()).unwrap();

        let result = load_workflow(file.path());
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("duplicate node ID"));
        assert!(err_msg.contains("duplicate-id"));
    }

    #[test]
    fn test_bad_edge_reference_error() {
        let yaml = r#"
workflow_id: test-workflow
name: Test Workflow
description: A test workflow
nodes:
  - kind: command
    id: node-a
    skill: test-skill
edges:
  - from: nonexistent
    to: node-a
"#;

        let mut file = NamedTempFile::new().unwrap();
        file.write_all(yaml.as_bytes()).unwrap();

        let result = load_workflow(file.path());
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("unknown"));
        assert!(err_msg.contains("nonexistent"));
    }

    #[test]
    fn test_bad_edge_target_reference_error() {
        let yaml = r#"
workflow_id: test-workflow
name: Test Workflow
description: A test workflow
nodes:
  - kind: command
    id: node-a
    skill: test-skill
edges:
  - from: node-a
    to: nonexistent
"#;

        let mut file = NamedTempFile::new().unwrap();
        file.write_all(yaml.as_bytes()).unwrap();

        let result = load_workflow(file.path());
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("unknown"));
        assert!(err_msg.contains("nonexistent"));
    }
}
