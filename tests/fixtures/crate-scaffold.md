# Hymenium: Crate Scaffold

## Handoff Metadata

- **Dispatch:** `direct`
- **Owning repo:** `hymenium`
- **Allowed write scope:** hymenium/...
- **Cross-repo edits:** none
- **Non-goals:** workflow engine logic, canopy integration, septa contracts
- **Verification contract:** cargo build, cargo test, cargo clippy
- **Completion update:** update `.handoffs/HANDOFFS.md` and archive when complete

## Problem

Hymenium needs a Rust crate with the basic module structure, Cargo.toml, and build infrastructure before any feature work can begin. The module layout must reflect the architectural separation: parser, decomposer, workflow engine, dispatcher, monitor, retry, store.

## What exists (state)

- **hymenium/**: Directory does not exist yet
- **Templates**: CLAUDE.md, AGENTS.md, README.md templates exist and have been filled in
- **ecosystem-versions.toml**: Shared dependency pins available
- **spore**: Shared infrastructure crate at version 0.4.9

## What needs doing (intent)

Create the hymenium Rust crate with module stubs, Cargo.toml with ecosystem-aligned dependencies, and basic binary entry point.

---

### Step 1: Create Cargo.toml and module structure

**Project:** `hymenium/`
**Effort:** 2-3 hours
**Depends on:** nothing

Create `hymenium/Cargo.toml` with:
- name = "hymenium"
- version = "0.1.0"
- edition = "2021"
- Dependencies aligned with ecosystem-versions.toml: anyhow 1, clap 4, chrono 0.4, serde 1, serde_json 1, thiserror 2, rusqlite 0.39, tracing 0.1, ulid 1, toml 1
- spore dependency matching ecosystem pin

Create module stubs:
- src/main.rs — CLI entry point with clap
- src/lib.rs — re-exports
- src/parser/mod.rs, src/parser/markdown.rs, src/parser/metadata.rs
- src/decompose.rs
- src/workflow/mod.rs, src/workflow/template.rs, src/workflow/engine.rs, src/workflow/gate.rs
- src/dispatch.rs
- src/monitor.rs
- src/retry.rs
- src/store.rs

Each stub should have a module-level doc comment explaining its purpose and a placeholder type or function.

#### Verification

```bash
cd hymenium && cargo build 2>&1 | tail -5
cargo test 2>&1 | tail -5
cargo clippy 2>&1 | tail -5
```

**Checklist:**
- [ ] Cargo.toml created with ecosystem-aligned deps
- [ ] All module stubs created with doc comments
- [ ] cargo build succeeds
- [ ] cargo test succeeds (no tests yet is fine)
- [ ] cargo clippy clean

---

### Step 2: Add basic CLI skeleton

**Project:** `hymenium/`
**Effort:** 1-2 hours
**Depends on:** Step 1

Add clap-based CLI with subcommands:
- `hymenium dispatch <path>` — dispatch a handoff (stub)
- `hymenium status` — show running workflows (stub)
- `hymenium decompose <path>` — split a large handoff (stub)
- `hymenium cancel <workflow-id>` — cancel a workflow (stub)

Each subcommand should print "not yet implemented" and exit 0.

#### Verification

```bash
cd hymenium && cargo build --release 2>&1 | tail -5
./target/release/hymenium --help 2>&1
./target/release/hymenium dispatch --help 2>&1
```

**Checklist:**
- [ ] CLI compiles and runs
- [ ] --help shows all subcommands
- [ ] Each subcommand accepts expected arguments
- [ ] Build and clippy pass

---

## Completion Protocol

**This handoff is NOT complete until ALL of the following are true:**

1. Every step above has verification output pasted
2. `cargo build` and `cargo clippy` pass
3. All checklist items checked

## Context

First handoff in the hymenium chain (#118a). Must complete before any feature handoffs (#118b-g) can start. Design note at docs/architecture/hymenium-design-note.md.
