# Hymenium Agent Notes

## Purpose

Hymenium automates the implementer/auditor workflow that is currently instruction-driven. Work here produces workflow templates, parsing logic, and dispatch automation that make multi-agent task execution a first-class, gate-enforced protocol rather than a prose convention. Keep the engine thin: phase gates and dispatch are the enforcement points; business logic belongs in templates, not in the engine itself.

---

## Source of Truth

- `src/` — all workflow logic; this is the authoritative implementation.
- `templates/` — authoritative workflow pattern definitions; these are the data the engine interprets. Do not hard-code workflow shapes in `engine.rs`.
- `../septa/` — authoritative cross-tool payload shapes; update contract schemas there before changing implementing code.
- `../ecosystem-versions.toml` — shared dependency pins; check before upgrading `spore`, `rusqlite`, or other shared crates.

When `src/` and `templates/` drift, templates win — the engine should interpret whatever the template says, not enforce its own view of the pattern.

---

## Before You Start

Before writing code, verify:

1. **Contracts**: read `../septa/README.md` — find which contracts Hymenium produces (`workflow-status-v1`, `dispatch-request-v1`) and which it consumes (`canopy-task-detail-v1`). Update contract schemas before changing implementing code.
2. **Versions**: read `../ecosystem-versions.toml` — verify `spore` and `rusqlite` pins before upgrading any shared dependency.
3. **Design note**: read `../docs/architecture/hymenium-design-note.md` before making structural changes to the engine or gate layer.
4. **Canopy boundary**: confirm that any new outbound write goes through `src/dispatch.rs` and not via a direct database call or ad hoc shell-out.

---

## Preferred Commands

Use these for most work:

```bash
cargo build --release       # Build the binary
cargo test                  # All unit and integration tests
```

For targeted work:

```bash
cargo test parser           # Parser and metadata extraction only
cargo test workflow         # Engine, gate, and template tests only
cargo test --ignored        # Canopy round-trip integration tests (requires running Canopy)
cargo clippy                # Lint; CI treats warnings as errors
cargo fmt --check           # Format check before committing
```

---

## Repo Architecture

Hymenium is a linear pipeline: parser feeds decomposer, decomposer feeds workflow engine, engine feeds dispatcher, monitor feeds retry. Keep each stage cohesive and resist the temptation to shortcut across stages.

Key boundaries:

- `src/parser/` — owns intake only; produces `ParsedHandoff`, nothing else. Does not make dispatch decisions.
- `src/workflow/gate.rs` — the single enforcement point for phase transition preconditions. Phase gates must be checked here, never inlined into `engine.rs` or `dispatch.rs`.
- `src/dispatch.rs` — the only module that writes to Canopy. All outbound task creation and agent assignment flows through here.
- `src/monitor.rs` — reads Canopy state; does not write to it. Escalation decisions go to `retry.rs`.
- `templates/` — data, not code. Adding a new workflow pattern should not require engine changes.

Current direction:

- Implementer/auditor is the first workflow template; design the engine to support additional patterns without special-casing that one.
- Keep the Canopy boundary clean: MCP tools only, never direct SQLite access.
- Gate enforcement belongs in `gate.rs`; keep it out of callers.

---

## Working Rules

- Never access Canopy's database directly. Use MCP tools or the Canopy CLI. The database path is Canopy's internal concern.
- Always communicate with Canopy via MCP or CLI. If a needed operation is missing from Canopy's surface, add it to Canopy first.
- Workflow templates are data, not code. New patterns go in `templates/`, not in `engine.rs` branches.
- Phase gates are non-negotiable. Do not add escape hatches or `--force` flags that skip gate evaluation.
- When changing a cross-tool payload, update the `../septa/` schema and fixture before touching implementing code, then validate: `cd ../septa && bash validate-all.sh`.
- Run `cargo clippy` and `cargo fmt --check` before closing any implementation task.

---

## Multi-Agent Patterns

For substantial Hymenium work, default to two agents:

**1. Primary implementation worker**
- Owns write scope for the engine, parser, gate, or dispatch layer
- Specific files in scope: `src/workflow/`, `src/parser/`, `src/dispatch.rs`, `src/store.rs`
- Does not cross into: Canopy source, Septa schemas (read only; update via a septa-scoped change), or other ecosystem repos

**2. Independent validator**
- Does not duplicate implementation — reviews the broader shape
- Specifically looks for:
  - Gate bypass: any path that reaches `dispatch.rs` without passing through `gate.rs`
  - Canopy boundary violations: direct SQLite access or ad hoc shell-outs that bypass MCP
  - Contract drift: `dispatch-request-v1` or `workflow-status-v1` payloads that do not match `../septa/` schemas
  - Template coupling: workflow-specific logic that leaked into `engine.rs` rather than living in templates
  - Missing recovery paths: stall detection or retry logic that silently swallows errors

Add a docs worker when `README.md`, `CLAUDE.md`, `AGENTS.md`, or `docs/` content changed materially.

Sequencing: do not block on the validator immediately. Continue local work in parallel; wait only when the next editing decision depends on the review result.

---

## Skills to Load

Use these for most work in this repo:

- `basidiocarp-implementer-auditor` — this is both the spec that Hymenium automates and the right workflow to apply when working on Hymenium itself; load it before starting any substantial feature work
- `systematic-debugging` — before fixing any unexplained engine stall, dispatch failure, or gate regression

Use these when the task needs them:

- `rust-router` — when adding new modules or reshaping the pipeline structure
- `test-writing` — when gate logic or retry behavior changes need stronger coverage
- `basidiocarp-workspace-router` — when the change spills into `../septa/` or requires Canopy changes

---

## Authoring Reference

Read before making changes to workflow templates or design docs:

| Doc | When to Read |
|-----|--------------|
| `../docs/architecture/hymenium-design-note.md` | **Always** before structural changes to the engine or gate layer |
| `../septa/README.md` | **Always** when changing a cross-tool payload |
| `templates/README.md` | **Always when editing workflow templates** — template format spec |

---

## Done Means

A task is not complete until:

- [ ] `cargo test` passes in `hymenium/`
- [ ] `cargo clippy` passes with no warnings
- [ ] `../septa/` schemas and fixtures are updated if any cross-tool payload changed, and `cd ../septa && bash validate-all.sh` passes
- [ ] Canopy integration has been tested when `dispatch.rs` or `monitor.rs` changed (use `cargo test --ignored` against a running Canopy instance)
- [ ] Any skipped validation or follow-up work is stated explicitly in the final response

If validation was skipped, say so clearly and explain why.

---

## Near-Term Priorities

Current direction — do not work against these:

- Build the implementer/auditor template as the canonical first workflow; do not hard-code its shape into the engine
- Keep all Canopy writes inside `dispatch.rs`; resist adding secondary write paths elsewhere
- Keep gate enforcement inside `gate.rs`; do not let callers skip gate checks by calling dispatch directly
