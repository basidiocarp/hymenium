//! `hymenium decompose <path>` command handler.

use crate::decompose::{self, DecompositionConfig, render_piece};
use crate::parser::parse_handoff;
use std::path::Path;
use thiserror::Error;

/// Errors that can occur during the decompose command.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DecomposeCommandError {
    #[error("could not read handoff file '{path}': {source}")]
    ReadFile {
        path: String,
        source: std::io::Error,
    },

    #[error("could not parse handoff: {0}")]
    Parse(#[from] crate::parser::ParseError),

    #[error("decomposition failed: {0}")]
    Decompose(#[from] decompose::DecompositionError),

    #[error("could not write piece '{path}': {source}")]
    WriteFile {
        path: String,
        source: std::io::Error,
    },
}

/// Run the `decompose` command: parse a handoff, split it into child pieces,
/// and write each piece as a separate markdown file next to the source.
///
/// Pieces are written to the same directory as `path`, using each piece's
/// `suggested_slug` as the filename (e.g. `my-handoff-1.md`).
pub fn run(path: &Path, dry_run: bool) -> Result<(), DecomposeCommandError> {
    let source = std::fs::read_to_string(path).map_err(|e| DecomposeCommandError::ReadFile {
        path: path.display().to_string(),
        source: e,
    })?;

    let handoff = parse_handoff(&source)?;
    let config = DecompositionConfig::default();
    let result = decompose::decompose(&handoff, &config)?;

    let parent_dir = path.parent().unwrap_or(Path::new("."));

    if !result.warnings.is_empty() {
        for w in &result.warnings {
            eprintln!("warning: {w}");
        }
    }

    println!(
        "decomposed '{}' into {} piece(s)",
        result.original_title,
        result.pieces.len()
    );

    for piece in &result.pieces {
        let filename = format!("{}.md", piece.suggested_slug);
        let out_path = parent_dir.join(&filename);
        let markdown = render_piece(piece, &result.original_title, None);

        if dry_run {
            println!("  [dry-run] would write: {filename}");
            println!("    title: {}", piece.title);
            println!(
                "    steps: {}  tier: {:?}",
                piece.steps.len(),
                piece.suggested_tier
            );
        } else {
            std::fs::write(&out_path, markdown).map_err(|e| DecomposeCommandError::WriteFile {
                path: out_path.display().to_string(),
                source: e,
            })?;
            println!(
                "  wrote: {}  ({} step(s), tier: {:?})",
                filename,
                piece.steps.len(),
                piece.suggested_tier
            );
        }
    }

    Ok(())
}
