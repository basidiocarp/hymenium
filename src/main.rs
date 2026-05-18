//! Hymenium CLI: handoff workflow orchestration.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use hymenium::store::WorkflowStore;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "hymenium")]
#[command(about = "Handoff workflow orchestration for multi-agent systems", long_about = None)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Dispatch a handoff to available agents
    Dispatch {
        /// Path to the handoff document
        #[arg(value_name = "PATH")]
        path: PathBuf,
    },

    /// Show status of running workflows
    Status {
        /// Workflow ID to inspect (omit to list all active workflows)
        #[arg(value_name = "WORKFLOW_ID")]
        workflow_id: Option<String>,

        /// Output status as JSON conforming to workflow-status-v1
        #[arg(long)]
        json: bool,
    },

    /// Decompose a large handoff into focused child handoffs
    Decompose {
        /// Path to the handoff document
        #[arg(value_name = "PATH")]
        path: PathBuf,

        /// Print what would be written without creating files
        #[arg(long)]
        dry_run: bool,
    },

    /// Cancel a running workflow
    Cancel {
        /// Workflow ID to cancel
        #[arg(value_name = "WORKFLOW_ID")]
        workflow_id: String,
    },

    /// Reconcile workflow phases against Canopy task statuses
    ///
    /// Checks each active phase's Canopy task and advances the workflow
    /// if Canopy reports completion. Safe to call repeatedly (idempotent).
    Reconcile {
        /// Workflow ID to reconcile
        #[arg(value_name = "WORKFLOW_ID")]
        workflow_id: String,
    },

    /// Resume a workflow paused at a `HandoffToUser` checkpoint
    Resume {
        /// Workflow ID to resume
        #[arg(value_name = "WORKFLOW_ID")]
        workflow_id: String,
    },

    /// Force-fail a workflow with a reason
    Fail {
        /// Workflow ID to fail
        #[arg(value_name = "WORKFLOW_ID")]
        workflow_id: String,

        /// Reason for the failure
        #[arg(long)]
        reason: String,
    },

    /// Force-complete a workflow
    Complete {
        /// Workflow ID to complete
        #[arg(value_name = "WORKFLOW_ID")]
        workflow_id: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Dispatch { path } => {
            let store = open_store()?;
            let instance = hymenium::commands::dispatch::run(&path, &store)
                .with_context(|| format!("dispatch failed for {}", path.display()))?;
            println!("{}", instance.workflow_id);
        }

        Commands::Status { workflow_id, json } => {
            let store = open_store()?;
            match workflow_id {
                Some(id) => {
                    hymenium::commands::status::run_single(&id, &store, json)
                        .with_context(|| format!("status query failed for {id}"))?;
                }
                None => {
                    hymenium::commands::status::run_list(&store, json)
                        .context("status list failed")?;
                }
            }
        }

        Commands::Decompose { path, dry_run } => {
            hymenium::commands::decompose::run(&path, dry_run)
                .with_context(|| format!("decompose failed for {}", path.display()))?;
        }

        Commands::Cancel { workflow_id } => {
            let store = open_store()?;
            hymenium::commands::cancel::run(&workflow_id, &store)
                .with_context(|| format!("cancel failed for {workflow_id}"))?;
        }

        Commands::Reconcile { workflow_id } => {
            let store = open_store()?;
            hymenium::commands::reconcile::run(&workflow_id, &store)
                .with_context(|| format!("reconcile failed for {workflow_id}"))?;
        }

        Commands::Resume { workflow_id } => {
            let store = open_store()?;
            hymenium::commands::resume::run(&workflow_id, &store)
                .with_context(|| format!("resume failed for {workflow_id}"))?;
        }

        Commands::Fail {
            workflow_id,
            reason,
        } => {
            let store = open_store()?;
            hymenium::commands::fail::run(&workflow_id, &reason, &store)
                .with_context(|| format!("fail failed for {workflow_id}"))?;
        }

        Commands::Complete { workflow_id } => {
            let store = open_store()?;
            hymenium::commands::complete::run(&workflow_id, &store)
                .with_context(|| format!("complete failed for {workflow_id}"))?;
        }
    }

    Ok(())
}

/// Open the workflow store, defaulting to the path from env or XDG conventions.
fn open_store() -> Result<WorkflowStore> {
    let db_path = WorkflowStore::default_path();
    let _sweeper = hymenium::sweeper::Sweeper::start(db_path.clone())
        .context("failed to start sweeper")?;
    WorkflowStore::open(&db_path)
        .with_context(|| format!("could not open workflow store at {}", db_path.display()))
}
