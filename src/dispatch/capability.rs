//! Shared capability vocabulary for task routing.
//!
//! Mirrors `canopy/src/capability.rs`. Both files must stay in sync with
//! `canopy/docs/capability-vocabulary.md`, which is the canonical definition.
//!
//! Keep the vocabulary at 10 labels or fewer. Do not add labels for every new
//! task type — compose existing labels instead.

/// Rust compilation and Cargo tooling (build, test, clippy, fmt).
pub const RUST: &str = "rust";

/// React/TypeScript work (cap dashboard, npm build).
pub const FRONTEND: &str = "frontend";

/// JSON schema and septa contract work.
pub const SCHEMA: &str = "schema";

/// `SQLite` schema migrations and direct database work.
pub const SQLITE: &str = "sqlite";

/// Markdown authoring only (no compilation required).
pub const DOCS: &str = "docs";

/// Bash/zsh scripting.
pub const SHELL: &str = "shell";

/// Workflow runtime work (hymenium, canopy internals).
pub const ORCHESTRATION: &str = "orchestration";

/// Map a repository name (the basename of `owning_repo`) to the capability labels
/// that agents working in that repo must have.
///
/// Returns an empty slice for repos with no required capability, which results
/// in a task that any agent can claim (backward-compatible).
#[must_use]
pub fn capabilities_for_repo(repo: &str) -> Vec<String> {
    match repo {
        "hymenium" | "canopy" | "mycelium" | "hyphae" | "rhizome" | "spore" | "stipe"
        | "cortina" | "annulus" | "volva" => vec![RUST.to_string()],
        "septa" => vec![SCHEMA.to_string()],
        "cap" => vec![FRONTEND.to_string()],
        "lamella" => vec![DOCS.to_string()],
        _ => vec![],
    }
}
