# Session-End Hook for Automatic Memory Capture

## Problem

When a Claude Code session ends, context is lost. No `session-summary.sh` hook exists — `mycelium/hooks/` only has `mycelium-rewrite.sh`. Valuable decisions, resolved errors, and file changes are never automatically stored in hyphae.

## What exists (state)

- **`mycelium/hooks/mycelium-rewrite.sh`**: Existing POSIX sh hook pattern — readable as reference
- **`mycelium/src/init/ecosystem.rs`**: `run_ecosystem()` configures Claude Code MCP servers; hook installation goes here
- **`hyphae` CLI**: `hyphae store --topic <topic> --content <text> --importance <level> -P <project>` works

## What needs doing (intent)

1. Create `mycelium/hooks/session-summary.sh` — a POSIX sh Stop hook that parses the transcript and stores a summary in hyphae
2. Add hook installation to `run_ecosystem()` so `mycelium init --ecosystem` wires it automatically
3. Add session-aware recall boost in `hyphae_memory_recall` to surface session summaries when relevant

---

### Step 1: Create session-summary hook script

**Project:** `mycelium/`
**Effort:** 1-2 hours
**Depends on:** nothing

Create `mycelium/hooks/session-summary.sh` (executable, POSIX sh, no bashisms). Reads Stop hook JSON from stdin (`session_id`, `transcript_path`, `cwd`). Parses transcript JSONL for: message count, files modified (from Write/Edit tool calls), commands run, errors. Stores via `hyphae store`. Exits 0 always, completes in <2 seconds. Requires `jq` and `hyphae` — exits 0 gracefully if either missing.

#### Verification

```bash
echo '{"session_id":"test","transcript_path":"/dev/null","cwd":"/tmp"}' | sh mycelium/hooks/session-summary.sh
shellcheck mycelium/hooks/session-summary.sh
```

**Output:**
<!-- PASTE START -->
printf '{"session_id":"test","transcript_path":"/dev/null","cwd":"/tmp"}\n' | sh hooks/session-summary.sh
exit 0

shellcheck hooks/session-summary.sh
no issues found

<!-- PASTE END -->

**Checklist:**
- [x] File exists at `mycelium/hooks/session-summary.sh`, is executable
- [x] POSIX sh: `#!/bin/sh`, no `[[`, no arrays, no `local`
- [x] Exits 0 when `jq` or `hyphae` missing
- [x] Extracts `session_id`, `transcript_path`, `cwd` from stdin JSON
- [x] `shellcheck` passes with no errors

---

### Step 2: Wire hook installation into ecosystem init

**Project:** `mycelium/`
**Effort:** 1 hour
**Depends on:** Step 1

In `mycelium/src/init/ecosystem.rs`, embed hook via `include_str!("../../hooks/session-summary.sh")`. Write to `~/.claude/hooks/session-summary.sh`, set executable, add Stop hook entry to `~/.claude/settings.json` (merge, don't overwrite). Idempotent.

#### Verification

```bash
cd mycelium && cargo build && cargo test && cargo clippy && cargo fmt --check
```

**Output:**
<!-- PASTE START -->
cargo build
Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.19s

cargo test
test result: ok. 1081 passed; 0 failed; 2 ignored
Doc-tests mycelium: ok

cargo clippy
Finished `dev` profile [unoptimized + debuginfo] target(s) in 5.27s
warning: 1 pre-existing clippy warning remains in `src/vcs/gh_cmd/issue.rs`

cargo fmt --check
exit 0

<!-- PASTE END -->

**Checklist:**
- [x] Hook content embedded via `include_str!`
- [x] Writes to `~/.claude/hooks/session-summary.sh` and sets executable
- [x] Merges Stop hook entry without clobbering existing hooks
- [x] Idempotent — running twice doesn't duplicate
- [x] Build, test, clippy, fmt all pass

---

### Step 3: Session-aware recall boost in hyphae

**Project:** `hyphae/`
**Effort:** 1 hour
**Depends on:** Step 1

In `hyphae_memory_recall`, detect session keywords (`"session"`, `"last time"`, `"previous"`, `"yesterday"`, `"earlier today"`). If detected, additionally search `session/` topic prefix (limit 5) and prepend results. No API change.

#### Verification

```bash
cd hyphae && cargo test -p hyphae-mcp --no-default-features 2>&1 | tail -10
```

**Output:**
<!-- PASTE START -->
cd hyphae && cargo test --no-default-features
test result: ok. 240 passed; 0 failed
test result: ok. 141 passed; 0 failed
Doc-tests hyphae_core/hyphae_ingest/hyphae_mcp/hyphae_store: ok

<!-- PASTE END -->

**Checklist:**
- [x] Session keyword detection in `tool_recall`
- [x] Session results prepended when keywords match
- [x] Tests pass

---

## Completion Protocol

**This handoff is NOT complete until ALL of the following are true:**

1. Verification output pasted for all steps
2. `shellcheck mycelium/hooks/session-summary.sh` exits 0
3. `cd mycelium && cargo build && cargo test` passes
4. `cd hyphae && cargo test --no-default-features` passes

## Context

From `.plans/session-end-hook.md`. Session memory capture is a high-value automation gap — agents lose context every session. Related: Lifecycle Capture Expansion (cortina), Context-Aware Recall (hyphae).

---

## Completion Notes (2026-04-07)

- Step 1 is implemented in [session-summary.sh](/Users/williamnewton/projects/basidiocarp/mycelium/hooks/session-summary.sh) as a POSIX `sh` Stop hook. It exits `0` when `jq` or `hyphae` is missing, parses Claude transcript JSONL, and stores a compact `session/<project>` summary via `hyphae store`.
- Step 2 is implemented in the repo's actual init path: [hook.rs](/Users/williamnewton/projects/basidiocarp/mycelium/src/init/hook.rs), [json_patch.rs](/Users/williamnewton/projects/basidiocarp/mycelium/src/init/json_patch.rs), and [mod.rs](/Users/williamnewton/projects/basidiocarp/mycelium/src/init/mod.rs). The handoff's original reference to `src/init/ecosystem.rs` was stale in the current tree.
- Installed hook wiring uses `mycelium-session-summary.sh` under `~/.claude/hooks/`, while the repo source file remains `hooks/session-summary.sh`. Legacy `session-summary.sh` registrations are recognized for migration and uninstall cleanup.
- Step 3 is now satisfied in the current workspace: `hyphae_memory_recall` includes session-keyword detection in [recall.rs](/Users/williamnewton/projects/basidiocarp/hyphae/crates/hyphae-mcp/src/tools/memory/recall.rs), and `cargo test --no-default-features` passes in `hyphae`.
- Verification completed successfully:
  - `printf '{"session_id":"test","transcript_path":"/dev/null","cwd":"/tmp"}\n' | sh hooks/session-summary.sh`
  - `shellcheck hooks/session-summary.sh`
  - `cargo build`
  - `cargo test`
  - `cargo clippy`
  - `cargo fmt --check`
  - `cd hyphae && cargo test --no-default-features`
