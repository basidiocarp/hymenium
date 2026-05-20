//! Run a hymenium DAG workflow from a YAML file.

use crate::workflow::dag;
use anyhow::Result;
use serde_json;
use std::collections::HashMap;
use std::path::PathBuf;

/// Run a hymenium DAG workflow YAML file.
pub struct Run {
    pub workflow: PathBuf,
    pub env: Vec<(String, String)>,
    pub json: bool,
}

impl Run {
    /// Execute the run command.
    pub async fn execute(self) -> Result<()> {
        // Load the workflow
        let dag = dag::load_workflow(&self.workflow)?;

        // Build the environment: workflow env + CLI overrides
        let mut env: HashMap<String, String> = dag.env.clone();
        for (key, val) in self.env {
            env.insert(key, val);
        }

        // Execute
        let executor = dag::DagExecutor { env };
        let results = executor.run(&dag).await?;

        // Output results
        if self.json {
            println!("{}", serde_json::to_string_pretty(&results)?);
        } else {
            for r in &results {
                println!("{}: {:?}", r.node_id, r.status);
            }
        }

        Ok(())
    }
}
