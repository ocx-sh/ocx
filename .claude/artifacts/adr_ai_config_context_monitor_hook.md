# ADR: Context-pressure advisory hook — bridge-file + PostToolUse injection

## Metadata

**Status:** Proposed
**Date:** 2026-04-19
**Deciders:** AI config overhaul (automated planning session)
**Related Plan:** `plan_ai_config_overhaul.md` (Phase 3)
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path: Python stdlib only; extend existing `hook_utils.py`; no new dependencies
**Domain Tags:** devops (AI config), infrastructure (hook architecture)
**Supersedes:** N/A (new capability)
**Superseded By:** N/A

## Context

Per `research_ai_config_get-shit-done.md` §Pattern 1, GSD addresses the "agents are context-blind" problem with a bridge-file + PostToolUse injection pattern: a small hook writes the current context-usage estimate to `/tmp/<session>-context.json` after each tool call; a PostToolUse hook reads the file and emits an advisory block when usage crosses 35% (warn) or 25% (critical) remaining budget. The advisory appears in Claude's context as a stderr block — Claude sees it and can compact proactively.

Per `audit_ai_config_ocx.md` §Gaps vs SOTA, OCX currently has no context-monitor infrastructure. Agents discover context exhaustion only when auto-compaction fires mid-task, which is the worst possible moment — mid-task compaction loses just-computed context that the agent needs to finish the task.

Per `research_ai_config_synthesis.md` §Convergent Patterns, this is one of the top 5 pattern adoptions, and OCX's existing statusLine hook is the natural integration point (same script that renders the prompt statusline can read the bridge file).

Per `research_ai_config_sota.md` §Prompt Caching, the March 2026 cache TTL regression (1h → 5min) makes idle-gap cache_write costs more expensive — which makes proactive compaction (at 35%, not at emergency) even more valuable because compaction is a cache-friendly operation when it happens at natural task boundaries.

This is a one-way door at the medium level: adopting the bridge-file convention commits future hooks to the same interface, and the file location under `/tmp` is a cross-platform concern (Windows lacks `/tmp`; WSL and macOS share conventions but not identically).

## Decision Drivers

- Eliminate mid-task emergency compaction by giving Claude advisory warnings earlier
- Zero-context cost (hooks run outside the context window)
- Integrate with existing `statusLine` and `hook_utils.py` — no new scripts
- Must fail open (missing / malformed bridge file → emit nothing, do not block)
- Cross-platform: Linux (primary), macOS (Darwin), WSL (Linux under Windows)

## Industry Context & Research

**Research artifact:** `research_ai_config_get-shit-done.md` §Pattern 1 (193-ln hook; 35%/25% thresholds; /tmp bridge); `research_ai_config_synthesis.md` §Headline Finding 4.

**Trending approach:** Context monitoring via hook-written bridge files is the emerging standard for Claude Code configs. GSD is the first public implementation with documented thresholds.

**Key insight:** OCX already runs `post_tool_use_tracker.py` after every Edit/Write tool call. Extending that hook is cheaper than introducing a new hook, and reuses the existing `StateManager` coordination pattern from `hook_utils.py`.

## Considered Options

### Option 1: Bridge file under `/tmp/ocx-context-<session_id>.json`

**Description:** Hook-authored JSON file with `{session_id, last_tool_at, estimated_tokens_used, estimated_remaining_pct}`. Written by `post_tool_use_tracker.py` after each tool call; read by the statusLine generator and by the advisory emitter.

| Pros | Cons |
|---|---|
| Matches GSD pattern exactly | `/tmp` lifecycle varies across platforms (cleared on boot, tmpfs size limits) |
| Cross-process: any hook/tool can read it | Needs per-session cleanup in `stop_validator.py` |
| No new file system location invented | Requires estimation heuristic for token usage |

### Option 2: Bridge file under `.state/context-<session_id>.json` (project-local)

**Description:** Same schema as Option 1, but stored under the existing OCX `.state/` directory that `hook_utils.py` already manages.

| Pros | Cons |
|---|---|
| Reuses existing StateManager cleanup | Pollutes project state directory with per-session ephemera |
| Cross-worktree isolation (each worktree has its own `.state/`) | `.state/` is already committed/gitignored conventions — crossing into ephemera adds complexity |
| Natural cleanup via `stop_validator.py` | |

### Option 3: No bridge file — estimate context inline on every PostToolUse emission

**Description:** Compute estimated-remaining inside each hook invocation from scratch. No shared state.

| Pros | Cons |
|---|---|
| Simpler | Cannot debounce (every tool call re-emits on threshold crossing) |
| No cleanup needed | Estimate is fragile without accumulated state |

### Option 4: Defer to Claude Code native `InstructionsLoaded` / `PreCompact` hooks

**Description:** Wait for Anthropic to expose native context-usage telemetry in hook payloads. Do not build our own.

| Pros | Cons |
|---|---|
| Zero code, aligns with future Anthropic direction | No ETA; gap remains indefinitely |
| No estimation heuristic needed | SOTA research shows the GSD pattern is the current workaround |

## Decision Outcome

**Chosen Option:** Option 2 — bridge file under `.state/context-<session_id>.json`.

**Rationale:** OCX's `hook_utils.py` already manages `.state/` cleanup via `StateManager`. Using `/tmp` (Option 1) means introducing a second cleanup pathway with platform-specific quirks. Option 3 cannot debounce. Option 4 blocks indefinitely on external roadmap. Option 2 reuses the existing pattern, makes cross-worktree isolation natural (each worktree has its own `.state/`), and lets the existing `stop_validator.py` handle cleanup with one added line.

### Bridge File Schema

```json
{
  "session_id": "abc123",
  "started_at": "2026-04-19T10:00:00Z",
  "last_tool_at": "2026-04-19T10:42:31Z",
  "tool_call_count": 87,
  "estimated_tokens_used": 152000,
  "estimated_context_window": 200000,
  "estimated_remaining_pct": 24.0,
  "thresholds_fired": ["warn_35"]
}
```

The estimation heuristic: start from a fixed baseline (measured once per OCX config revision: always-loaded context + tool schema ≈ 12K tokens), accumulate `max(500, len(tool_input) + len(tool_output)) / 4` per tool call. This is deliberately over-counting to fail on the safe side.

### Threshold Escalation

| Threshold | Name | Emitted Block |
|---|---|---|
| 35% remaining | `warn` | `[CONTEXT PRESSURE] 35% remaining — consider task decomposition or /compact at a natural boundary.` |
| 25% remaining | `critical` | `[CONTEXT PRESSURE — CRITICAL] 25% remaining — /compact now before continuing.` |
| 10% remaining | `last-resort` | `[CONTEXT PRESSURE — LAST RESORT] 10% remaining — stop adding context; finalize current work and hand off.` |

Debounce: each threshold fires at most once per session (tracked in `thresholds_fired` array).

### Consequences

**Positive:**
- Eliminates "discovered context exhaustion at compaction" failure mode
- Zero main-context cost (hooks run outside the window)
- Cache-friendly — advisory is a stderr block, not a CLAUDE.md edit

**Negative:**
- Estimation heuristic is approximate; actual tokens may diverge
- Adds ~60 lines to `post_tool_use_tracker.py`
- Requires unit tests for threshold logic and debounce

**Risks:**
- **False positives (advisory fires too early).** Mitigation: heuristic errs on the over-count side; advisory is non-blocking so cost of a false positive is one unnecessary user-visible message.
- **False negatives (advisory fires too late).** Mitigation: 35% threshold gives Claude 70,000 tokens of headroom before true exhaustion.
- **Bridge file race on concurrent worktree sessions.** Mitigation: `.state/` is per-worktree; session_id suffix disambiguates within a worktree.

## Hook Skeleton (part of the decision — the interface is committed)

```python
# Addition to hook_utils.py
from dataclasses import dataclass
from pathlib import Path
import json
import time


@dataclass(slots=True)
class ContextBudget:
    state_dir: Path
    session_id: str

    def _path(self) -> Path:
        return self.state_dir / f"context-{self.session_id}.json"

    def tick(self, tool_input_len: int, tool_output_len: int) -> dict:
        """Update bridge file. Return the crossed threshold name or empty string."""
        # Read, mutate, write atomically. Return threshold name if crossed since last tick.
        ...

    def cleanup(self) -> None:
        self._path().unlink(missing_ok=True)


# Addition to post_tool_use_tracker.py
def emit_context_pressure_advisory(budget: ContextBudget, tool_input_len: int, tool_output_len: int) -> str:
    """Return advisory block or empty string. Fails open on any exception."""
    try:
        threshold = budget.tick(tool_input_len, tool_output_len)
    except Exception:
        return ""
    return ADVISORY_BLOCKS.get(threshold, "")
```

## Implementation Plan

1. [ ] Extend `hook_utils.py` with `ContextBudget` dataclass and associated methods
2. [ ] Extend `post_tool_use_tracker.py` to invoke `ContextBudget.tick` and emit advisory block
3. [ ] Extend `session_start_loader.py` to initialize a fresh bridge file
4. [ ] Extend `stop_validator.py` to call `ContextBudget.cleanup`
5. [ ] Update statusLine generator (if configured) to include remaining-pct when bridge file is present
6. [ ] Unit tests in `test_hooks.py`: threshold crossing, debounce, missing file, malformed file, concurrent-session isolation
7. [ ] Manual smoke test script `.claude/tests/manual/context_monitor_smoke.sh` that synthesizes 80 tool calls and asserts advisory fires
8. [ ] Document thresholds and the `ContextBudget` schema in `meta-ai-config.md` under a new "Context Pressure Advisory" section
9. [ ] Verify all existing hook unit tests pass after the extension

## Validation

- [ ] `test_hooks.py` passes including new `ContextBudget` tests
- [ ] Manual smoke script fires advisories at 35%, 25%, 10%
- [ ] Debounce works: advisory fires once per threshold per session
- [ ] Missing/malformed bridge file: hook emits nothing and exits 0 (fails open)
- [ ] `meta-ai-config.md` documents the interface

## Links

- `plan_ai_config_overhaul.md` Phase 3
- `research_ai_config_get-shit-done.md` §Pattern 1
- `research_ai_config_synthesis.md` §Headline Finding 4
- `research_ai_config_sota.md` §Prompt Caching (cache TTL regression context)

---

## Changelog

| Date | Author | Change |
|---|---|---|
| 2026-04-19 | AI config overhaul planning | Initial draft |
