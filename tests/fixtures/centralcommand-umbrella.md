# Central Command: Multi-Project Coordination

## Handoff Metadata

- **Dispatch:** `umbrella`
- **Owning repo:** `canopy`
- **Allowed write scope:** canopy/..., hyphae/..., rhizome/...
- **Cross-repo edits:** none
- **Source read scope:** .audit/external/..., docs/foundations/...
- **Verification contract:** cargo test --all, cross-project integration check
- **Completion update:** update `.handoffs/HANDOFFS.md` and archive when complete

## Problem

The ecosystem has grown to support multiple independent projects (canopy, hyphae, rhizome) that need coordinated changes across project boundaries. A single handoff must decompose into focused sub-tasks per project while maintaining an overall completion protocol that verifies cross-project compatibility.

## What exists (state)

- **canopy/**: Task coordination ledger, Canopy MCP surface
- **hyphae/**: Memory and RAG system, Hyphae MCP surface
- **rhizome/**: Code intelligence MCP server
- **septa/**: Shared schema and contract definitions
- **ecosystem-versions.toml**: Unified version pins for cross-project dependencies

## What needs doing (intent)

Implement umbrella dispatch that coordinates related changes across multiple repositories, ensures each sub-project maintains its own build and test gates, and validates the integrated result before marking the overall work complete.

---

### Step 1: Coordinate Canopy task interface changes

**Project:** `canopy/`
**Effort:** 2 days
**Depends on:** nothing

Update Canopy's task representation to support hierarchical sub-tasks and cross-project references. Ensure backward compatibility with existing task stores.

#### Verification

```bash
cd canopy && cargo test --lib
cd canopy && cargo build --release 2>&1 | tail -5
```

**Checklist:**
- [ ] Task schema updated with sub-task fields
- [ ] Backward compatibility layer working
- [ ] Tests pass
- [ ] No clippy warnings

---

### Step 2: Add cross-project metadata to Hyphae

**Project:** `hyphae/`
**Effort:** 1 day
**Depends on:** Step 1

Extend Hyphae's memory schema to track project boundaries and cross-project references. Enable memories to reference tasks in other projects.

#### Verification

```bash
cd hyphae && cargo test --lib
cd hyphae && cargo clippy 2>&1 | tail -5
```

**Checklist:**
- [ ] Memory schema extended
- [ ] Cross-project queries working
- [ ] Tests pass

---

### Step 3: Update septa contracts for umbrella dispatch

**Project:** `septa/`
**Effort:** 4 hours
**Depends on:** nothing

Add schema definitions for umbrella dispatch payloads and cross-project coordination messages. Validate all producers and consumers.

#### Verification

```bash
cd septa && bash validate-all.sh
```

**Checklist:**
- [ ] Umbrella dispatch schema added
- [ ] All validators pass
- [ ] Schema documented

---

## Verification

```bash
cd hymenium && cargo test --all
cd rhizome && cargo test --all
cargo test --manifest-path septa/Cargo.toml
```

## Completion Protocol

This handoff is complete when:

1. All three sub-steps pass their individual verification gates
2. A cross-project integration test exercises the full umbrella dispatch flow
3. No regressions in existing task handling
4. Documentation updated in `docs/workspace/ECOSYSTEM-INTERACTIONS.md`

## Context

Related to #118 (multi-agent orchestration) and #134 (cross-repo contracts). See `.audit/external/SYNTHESIS.md` for prior art on umbrella dispatch patterns from similar systems.
