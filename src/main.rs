//! Hymenium CLI: handoff workflow orchestration.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "hymenium")]
#[command(about = "Handoff workflow orchestration for multi-agent systems", long_about = None)]
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
    Status,

    /// Decompose a large handoff into child tasks
    Decompose {
        /// Path to the handoff document
        #[arg(value_name = "PATH")]
        path: PathBuf,
    },

    /// Cancel a running workflow
    Cancel {
        /// Workflow ID to cancel
        #[arg(value_name = "WORKFLOW_ID")]
        workflow_id: String,
    },
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Dispatch { path } => {
            println!("not yet implemented: dispatch {}", path.display());
        }
        Commands::Status => {
            println!("not yet implemented: status");
        }
        Commands::Decompose { path } => {
            println!("not yet implemented: decompose {}", path.display());
        }
        Commands::Cancel { workflow_id } => {
            println!("not yet implemented: cancel {}", workflow_id);
        }
    }
}
