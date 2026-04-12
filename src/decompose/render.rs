//! Rendering decomposed pieces as standalone handoff markdown.

use crate::parser::HandoffMetadata;

use super::HandoffPiece;

/// Render a child piece as a standalone handoff markdown document.
pub fn render_piece(
    piece: &HandoffPiece,
    parent_title: &str,
    parent_metadata: Option<&HandoffMetadata>,
) -> String {
    let mut out = String::new();

    // Title
    out.push_str(&format!("# {}: {}\n\n", parent_title, piece.title));

    // Metadata block
    if let Some(meta) = parent_metadata {
        out.push_str("```yaml\n");
        out.push_str(&format!("owning_repo: {}\n", meta.owning_repo));

        // Write scope derived from steps' files_to_modify (sorted for determinism)
        let mut write_scope: Vec<&str> = piece
            .steps
            .iter()
            .flat_map(|s| s.files_to_modify.iter().map(|f| f.path.as_str()))
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        write_scope.sort_unstable();
        if !write_scope.is_empty() {
            out.push_str("allowed_write_scope:\n");
            for path in &write_scope {
                out.push_str(&format!("  - {}\n", path));
            }
        }

        out.push_str(&format!(
            "verification_contract: {}\n",
            meta.verification_contract
        ));
        out.push_str("```\n\n");
    }

    // Problem section
    out.push_str("## Problem\n\n");
    out.push_str(&format!(
        "Child piece of parent handoff \"{}\".\n\n",
        parent_title
    ));

    // Steps
    out.push_str("## Steps\n\n");
    for step in &piece.steps {
        out.push_str(&format!("### Step {}: {}\n\n", step.number, step.title));

        if !step.description.is_empty() {
            out.push_str(&step.description);
            out.push_str("\n\n");
        }

        if !step.files_to_modify.is_empty() {
            out.push_str("**Files to modify:**\n\n");
            for f in &step.files_to_modify {
                out.push_str(&format!("- `{}` — {}\n", f.path, f.description));
            }
            out.push('\n');
        }

        if let Some(ref ver) = step.verification {
            out.push_str("**Verification:**\n\n```bash\n");
            for cmd in &ver.commands {
                out.push_str(cmd);
                out.push('\n');
            }
            out.push_str("```\n\n");
        }

        if !step.checklist.is_empty() {
            out.push_str("**Checklist:**\n\n");
            for item in &step.checklist {
                let check = if item.checked { "x" } else { " " };
                out.push_str(&format!("- [{}] {}\n", check, item.text));
            }
            out.push('\n');
        }
    }

    // Completion protocol
    out.push_str("## Completion Protocol\n\n");
    out.push_str("Run all verification commands above. ");
    out.push_str("Report results with pass/fail evidence.\n\n");

    // Context
    out.push_str("## Context\n\n");
    out.push_str(&format!(
        "This piece was decomposed from: **{}**\n",
        parent_title
    ));
    if !piece.depends_on.is_empty() {
        out.push_str(&format!(
            "\nDepends on piece(s): {}\n",
            piece
                .depends_on
                .iter()
                .map(|i| format!("#{}", i + 1))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    out
}
