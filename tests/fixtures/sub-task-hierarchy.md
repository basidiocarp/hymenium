# Canopy Sub-Task Hierarchy

## Handoff Metadata

- **Dispatch:** `direct`
- **Owning repo:** `canopy`
- **Allowed write scope:** canopy/...
- **Cross-repo edits:** none unless this handoff explicitly says otherwise
- **Non-goals:** adjacent repo work not named in this handoff
- **Verification contract:** run the repo-local commands named in the handoff and the paired `verify-*.sh` script
- **Completion update:** once audit is clean and verification is green, update `.handoffs/HANDOFFS.md` and archive or remove the completed entry if the dashboard tracks active work only


## Problem

Canopy's `task_relationships` table supports parent/child relationships, but the
attention model and task lifecycle don't understand "all children complete = parent
complete." A parent task can show as open when every child is done, and a parent
can be marked complete even when children are still open. This needs to be resolved
before multi-model orchestration flows are safe to ship.

## What exists (state)

- **`task_relationships` table**: parent/child relationship storage exists in
  the canopy SQLite schema
- **`canopy task list`**: shows flat task list; no hierarchy rendering
- **`canopy task complete`**: completes a task independently; no child-state check
- **Attention model**: `canopy snapshot` attention semantics don't include
  "parent blocked by open children"
- **`canopy import-handoff`**: planned but not yet implemented — would parse
  handoff markdown into a task tree with one subtask per step

## What needs doing (intent)

Enforce the parent/child completion invariant: a parent task cannot be marked
complete while it has open children. Add hierarchy rendering to `canopy task list`
and include parent completion status in `canopy snapshot`.

---

### Step 1: Enforce child-completion invariant at task completion

**Project:** `canopy/`
**Effort:** 1 day
**Depends on:** nothing

Extend `canopy task complete <id>` to check for open child tasks before allowing
completion:

1. Query `task_relationships` for all children of `<id>`
2. If any children have status other than `complete` or `cancelled`, reject:

```
Error: task <id> has N open sub-tasks.

Complete or cancel all sub-tasks first, or use --force to override.

Open sub-tasks:
  <child-id-1>  <title>  [in-progress]
  <child-id-2>  <title>  [open]
```

3. `--force` bypasses the guard and logs a council event as with verification gate

When all children complete, optionally auto-suggest parent completion:
```
All sub-tasks of task <parent-id> are complete. Complete the parent task?
  canopy task complete <parent-id>
```

#### Files to modify

**`canopy-core/src/task/complete.rs`** — extend `complete_task`:

```rust
pub enum CompletionError {
    #[error("task has {0} open sub-tasks")]
    OpenChildren(usize),
    #[error("task requires script verification evidence before completion")]
    VerificationRequired,
    #[error("task not found: {0}")]
    NotFound(TaskId),
}
```

#### Verification

```bash
cd canopy && cargo build --workspace 2>&1 | tail -5
cargo test --workspace 2>&1 | tail -10
```

**Output:**
<!-- PASTE START -->

<!-- PASTE END -->

**Checklist:**
- [ ] `canopy task complete` blocked when parent has open children
- [ ] Error lists open children with status
- [ ] `--force` bypasses and logs council event
- [ ] Tasks without children complete normally (no regression)
- [ ] Build and tests pass

---

### Step 2: Add hierarchy rendering to task list

**Project:** `canopy/`
**Effort:** 4–8 hours
**Depends on:** nothing (can parallel with Step 1)

Extend `canopy task list` to support `--tree` flag that renders tasks with
indented children:

```
TASK LIST
├─ #101  [open]      Build cortina pre-compact handler
│  ├─ #102  [done]   Add handler stub
│  ├─ #103  [done]   Implement snapshot builder
│  └─ #104  [open]   Wire into hyphae session context
└─ #105  [open]      Rhizome structural fallback
```

The default `canopy task list` output (no flag) is unchanged — hierarchical view
is opt-in via `--tree`.

#### Verification

```bash
cd canopy && cargo test --workspace 2>&1 | tail -10
canopy task list --tree 2>&1 | head -20
```

**Output:**
<!-- PASTE START -->

<!-- PASTE END -->

**Checklist:**
- [ ] `canopy task list --tree` renders parent/child hierarchy
- [ ] Children shown with correct status badges
- [ ] Flat list (no `--tree`) unchanged
- [ ] Tree rendering handles multi-level nesting

---

### Step 3: Add `canopy import-handoff` command

**Project:** `canopy/`
**Effort:** 1–2 days
**Depends on:** Step 2

Parse a handoff markdown file into a canopy task tree: one parent task for the
handoff title, one child task per step. Steps are detected by `### Step N:` headings.

```bash
canopy import-handoff .handoffs/cortina/precompact-capture.md
Created task #201: PreCompact / UserPromptSubmit Capture in Cortina [parent]
  Created sub-task #202: Add PreCompact handler to cortina Claude Code adapter [step 1]
  Created sub-task #203: Add UserPromptSubmit handler [step 2]
  Created sub-task #204: Surface pre-compact captures in hyphae session context [step 3]
```

The parent task inherits the handoff's `Depends on:` notes as a `notes` field.
Each sub-task's description comes from the step description paragraph.

#### Verification

```bash
cd canopy && cargo build --workspace 2>&1 | tail -5
canopy import-handoff .handoffs/cortina/precompact-capture.md 2>&1
canopy task list --tree 2>&1 | grep -A 5 "PreCompact"
```

**Output:**
<!-- PASTE START -->

<!-- PASTE END -->

**Checklist:**
- [ ] `canopy import-handoff` creates parent task from handoff title
- [ ] One sub-task per `### Step N:` heading
- [ ] Step descriptions included in sub-task notes
- [ ] Resulting tree renders correctly in `canopy task list --tree`

---

## Completion Protocol

**This handoff is NOT complete until ALL of the following are true:**

1. Every step above has verification output pasted between the markers
2. `cargo build --workspace` and `cargo test --workspace` pass in `canopy/`
3. A parent task with open children cannot be completed without `--force`
4. `canopy task list --tree` renders the hierarchy correctly
5. All checklist items are checked

### Final Verification

```bash
cd canopy && cargo test --workspace 2>&1 | tail -5
```

**Output:**
<!-- PASTE START -->

<!-- PASTE END -->

**Required result:** all tests pass, no failures.

## Context

Gap #12 in `docs/workspace/ECOSYSTEM-REVIEW.md`. The `task_relationships` table
is the right place for parent/child — the schema already exists. The gap is
lifecycle enforcement and rendering. Required before multi-model orchestration
(gap #17) is safe: orchestrators create parent tasks and sub-tasks per agent role,
and the hierarchy must be enforced to prevent premature parent completion.
