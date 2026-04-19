"""Shared utilities for Claude Code hooks.

Provides JSON I/O, hook response builders, state/lock management,
and path utilities. Imported by all hook scripts via sys.path insertion.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
import sys
import time
import uuid
from datetime import datetime, timedelta, timezone
from pathlib import Path


# ---------------------------------------------------------------------------
# JSON I/O
# ---------------------------------------------------------------------------


def read_input() -> dict:
    """Read and parse JSON from stdin. Returns empty dict on invalid input."""
    try:
        data = sys.stdin.read()
        if not data.strip():
            return {}
        return json.loads(data)
    except (json.JSONDecodeError, OSError):
        return {}


def output_json(data: dict) -> None:
    """Print compact JSON to stdout."""
    print(json.dumps(data, separators=(",", ":")))


# ---------------------------------------------------------------------------
# Hook Response Builders
# ---------------------------------------------------------------------------


def deny(reason: str) -> dict:
    """Build a PreToolUse deny response."""
    return {
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    }


def ask(reason: str) -> dict:
    """Build a PreToolUse ask response."""
    return {
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "ask",
            "permissionDecisionReason": reason,
        }
    }


def additional_context(text: str) -> dict:
    """Build an additionalContext response for any event."""
    return {"hookSpecificOutput": {"additionalContext": text}}


# ---------------------------------------------------------------------------
# Environment
# ---------------------------------------------------------------------------


def get_project_dir() -> str | None:
    """Return CLAUDE_PROJECT_DIR or None. Never falls back to cwd."""
    return os.environ.get("CLAUDE_PROJECT_DIR")


# ---------------------------------------------------------------------------
# Path Utilities
# ---------------------------------------------------------------------------


def relative_path(file_path: str, project_dir: str) -> str:
    """Compute relative path, handling both project-relative and absolute."""
    if file_path.startswith(project_dir):
        rel = file_path[len(project_dir) :]
        return rel.lstrip("/").lstrip("\\")
    return file_path


def lock_filename(rel_path: str) -> str:
    """Convert a relative path to a safe lock filename."""
    return rel_path.replace("/", "_").replace("\\", "_") + ".lock"


# ---------------------------------------------------------------------------
# State Manager
# ---------------------------------------------------------------------------


class StateManager:
    """Manages .state/, .locks/, and .file-tracker.log for hook coordination."""

    def __init__(self, project_dir: str) -> None:
        self.project_dir = Path(project_dir)
        self.hooks_dir = self.project_dir / ".claude" / "hooks"
        self.state_dir = self.hooks_dir / ".state"
        self.lock_dir = self.hooks_dir / ".locks"
        self.tracker_file = self.hooks_dir / ".file-tracker.log"

    def ensure_dirs(self) -> None:
        """Create .state/ and .locks/ if missing."""
        self.state_dir.mkdir(parents=True, exist_ok=True)
        self.lock_dir.mkdir(parents=True, exist_ok=True)

    # --- Session tracking ---

    def write_session(self, session_id: str, source: str) -> None:
        """Write a session tracking file."""
        self.ensure_dirs()
        short = session_id[:8] if session_id else "unknown"
        data = {
            "session_id": session_id,
            "started": datetime.now(timezone.utc).isoformat(),
            "source": source,
        }
        session_file = self.state_dir / f"session_{short}.json"
        session_file.write_text(json.dumps(data))

    def remove_session(self, session_id: str) -> None:
        """Remove a session tracking file."""
        short = session_id[:8] if session_id else "unknown"
        session_file = self.state_dir / f"session_{short}.json"
        session_file.unlink(missing_ok=True)

    def count_active_sessions(self) -> int:
        """Count active session files."""
        if not self.state_dir.exists():
            return 0
        return len(list(self.state_dir.glob("session_*.json")))

    def clean_old_sessions(self, max_age_hours: int = 24) -> None:
        """Remove session files older than max_age_hours."""
        if not self.state_dir.exists():
            return
        cutoff = time.time() - (max_age_hours * 3600)
        for f in self.state_dir.glob("session_*.json"):
            try:
                if f.stat().st_mtime < cutoff:
                    f.unlink()
            except OSError:
                pass

    # --- Handoff ---

    def read_and_clear_handoff(self) -> str | None:
        """Read handoff message and delete the file. Returns None if absent."""
        handoff_file = self.state_dir / "handoff.json"
        if not handoff_file.exists():
            return None
        try:
            data = json.loads(handoff_file.read_text())
            message = data.get("message", "")
            handoff_file.unlink(missing_ok=True)
            return message if message else None
        except (json.JSONDecodeError, OSError):
            return None

    # --- File locks ---

    def check_lock(
        self, rel_path: str, session_id: str, ttl_seconds: int = 120
    ) -> str | None:
        """Check if a file is locked by another session.

        Returns the blocking session_id if locked, None if free.
        """
        lock_file = self.lock_dir / lock_filename(rel_path)
        if not lock_file.exists():
            return None
        try:
            data = json.loads(lock_file.read_text())
            lock_session = data.get("session_id", "")
            lock_time = data.get("timestamp", 0)
            if time.time() - lock_time >= ttl_seconds:
                return None  # expired
            if lock_session == session_id:
                return None  # same session
            return lock_session
        except (json.JSONDecodeError, OSError):
            return None

    def acquire_lock(
        self, rel_path: str, session_id: str, tool_name: str
    ) -> bool:
        """Acquire a file lock atomically using os.mkdir.

        Returns True if lock was acquired, False if contention.
        """
        self.ensure_dirs()
        lock_file = self.lock_dir / lock_filename(rel_path)
        atomic_dir = str(lock_file) + ".acquiring"
        try:
            os.mkdir(atomic_dir)
        except FileExistsError:
            return False
        try:
            data = {
                "session_id": session_id,
                "timestamp": int(time.time()),
                "tool": tool_name,
            }
            lock_file.write_text(json.dumps(data))
            return True
        finally:
            try:
                os.rmdir(atomic_dir)
            except OSError:
                pass

    def release_session_locks(self, session_id: str) -> None:
        """Release all locks held by a session."""
        if not self.lock_dir.exists():
            return
        for lock_file in self.lock_dir.glob("*.lock"):
            try:
                data = json.loads(lock_file.read_text())
                if data.get("session_id") == session_id:
                    lock_file.unlink()
            except (json.JSONDecodeError, OSError):
                pass

    # --- File tracker ---

    def log_modification(self, tool_name: str, rel_path: str) -> None:
        """Append a modification entry to the tracker log."""
        self.ensure_dirs()
        timestamp = datetime.now().strftime("%Y-%m-%d %H:%M:%S")
        with open(self.tracker_file, "a") as f:
            f.write(f"[{timestamp}] {tool_name}: {rel_path}\n")

    def trim_tracker(
        self, max_lines: int = 100, threshold: int = 110
    ) -> None:
        """Trim tracker log to max_lines when it exceeds threshold."""
        if not self.tracker_file.exists():
            return
        try:
            lines = self.tracker_file.read_text().splitlines()
            if len(lines) > threshold:
                self.tracker_file.write_text(
                    "\n".join(lines[-max_lines:]) + "\n"
                )
        except OSError:
            pass

    # --- Subagent log ---

    def log_subagent_completion(self) -> None:
        """Append a subagent completion entry."""
        self.ensure_dirs()
        timestamp = datetime.now().strftime("%Y-%m-%d %H:%M:%S")
        log_file = self.state_dir / "subagent.log"
        try:
            with open(log_file, "a") as f:
                f.write(f"[{timestamp}] Subagent task completed\n")
        except OSError:
            pass

    def trim_subagent_log(self, max_lines: int = 100) -> None:
        """Trim subagent log to max_lines."""
        log_file = self.state_dir / "subagent.log"
        if not log_file.exists():
            return
        try:
            lines = log_file.read_text().splitlines()
            if len(lines) > max_lines:
                log_file.write_text("\n".join(lines[-max_lines:]) + "\n")
        except OSError:
            pass

    # --- Commit verification ---

    def is_recently_verified(self, ttl_seconds: int = 300) -> bool:
        """Check if commit verification was completed recently."""
        verify_file = self.state_dir / "commit-verified"
        if not verify_file.exists():
            return False
        try:
            verified_time = int(verify_file.read_text().strip())
            return (time.time() - verified_time) < ttl_seconds
        except (ValueError, OSError):
            return False


# ---------------------------------------------------------------------------
# Cross-Session Learnings Store (Phase 4)
# See .claude/artifacts/adr_ai_config_cross_session_learnings_store.md
# ---------------------------------------------------------------------------

_LEARNING_MARKER_RE = re.compile(
    r"\[LEARNING\]\s*(\{.*?\})",
    re.DOTALL,
)


class MergeLockContention(Exception):
    """Raised when the canonical-store merge lock cannot be acquired.

    Callers should catch and skip the merge — concurrent session stops are
    rare and pending records remain on disk for the next stop to pick up.
    """


class LearningsStore:
    """Project-local, per-worktree JSONL store of cross-session learnings.

    Canonical store: ``.claude/state/learnings.jsonl``. Pending queue:
    ``.claude/hooks/.state/learnings-pending.jsonl`` (reuses StateManager path
    semantics for concurrency). Orphan quarantine:
    ``.claude/state/learnings-orphan.jsonl`` (schema-mismatch records).

    Stage 1 (first 30 days): logging-only. No promotion candidates are
    emitted. Stage 2 thresholds are tuned from corpus after the 30-day gate.

    Schema v1 (all fields required for validity)::

        {
          "schema_version": 1,
          "id": "uuid-v4",
          "created_at": "ISO-8601 UTC",
          "source_agent": "worker-reviewer | worker-builder | etc.",
          "source_session": "session_id string",
          "category": "rust|python|ts|oci|test|clippy|mirror|build|other",
          "fingerprint": "sha256(category|normalized_summary)[:16]",
          "summary": "short human-readable description, <=160 chars",
          "evidence_ref": "optional path/commit/url",
          "confidence": 0.0,
          "confidence_updated_at": "ISO-8601 UTC",
          "ttl_days": 90,
          "occurrence_count": 1
        }
    """

    SCHEMA_VERSION = 1
    VALID_CATEGORIES = frozenset({
        "rust", "python", "ts", "oci", "test", "clippy",
        "mirror", "build", "other",
    })
    SUMMARY_MAX_CHARS = 160
    CONFIDENCE_FLOOR = 0.3
    CONFIDENCE_DECAY_PER_DAY = 0.02
    CONFIDENCE_REPLENISHMENT_ON_MATCH = 0.15
    DEFAULT_TTL_DAYS = 90
    DAY30_REVIEW_TARGET_DAYS = 30

    def __init__(self, project_dir: str) -> None:
        self.project_dir = Path(project_dir)
        self.canonical_dir = self.project_dir / ".claude" / "state"
        self.canonical_path = self.canonical_dir / "learnings.jsonl"
        self.orphan_path = self.canonical_dir / "learnings-orphan.jsonl"
        self.day30_sentinel = self.canonical_dir / ".day30-review-reminder"
        # Pending queue uses the existing hook-state dir for concurrency parity
        # with other hook ephemera.
        self.pending_path = (
            self.project_dir / ".claude" / "hooks" / ".state" / "learnings-pending.jsonl"
        )

    def ensure_canonical_dir(self) -> None:
        self.canonical_dir.mkdir(parents=True, exist_ok=True)
        self.pending_path.parent.mkdir(parents=True, exist_ok=True)
        # Create day-30 review sentinel on first use.
        if not self.day30_sentinel.exists():
            target = datetime.now(timezone.utc) + timedelta(
                days=self.DAY30_REVIEW_TARGET_DAYS
            )
            self.day30_sentinel.write_text(target.isoformat())

    # ---- Helpers ----

    @staticmethod
    def _normalize_summary(summary: str) -> str:
        """Case-fold + collapse whitespace for fingerprint stability."""
        return " ".join(summary.lower().split())

    @classmethod
    def fingerprint(cls, category: str, summary: str) -> str:
        key = f"{category}|{cls._normalize_summary(summary)}"
        return hashlib.sha256(key.encode("utf-8")).hexdigest()[:16]

    @classmethod
    def is_valid(cls, record: dict) -> bool:
        required = {
            "schema_version", "id", "created_at", "source_agent",
            "category", "summary", "confidence", "ttl_days",
        }
        if not required.issubset(record.keys()):
            return False
        if record.get("schema_version") != cls.SCHEMA_VERSION:
            return False
        if record.get("category") not in cls.VALID_CATEGORIES:
            return False
        if not isinstance(record.get("summary"), str):
            return False
        if len(record["summary"]) > cls.SUMMARY_MAX_CHARS:
            return False
        try:
            c = float(record["confidence"])
        except (TypeError, ValueError):
            return False
        if not (0.0 <= c <= 1.0):
            return False
        return True

    def normalize_record(self, raw: dict, source_session: str = "") -> dict:
        """Fill in required fields for a partially-specified raw learning."""
        now = datetime.now(timezone.utc).isoformat()
        category = raw.get("category", "other")
        summary = raw.get("summary", "")[: self.SUMMARY_MAX_CHARS]
        return {
            "schema_version": self.SCHEMA_VERSION,
            "id": raw.get("id") or str(uuid.uuid4()),
            "created_at": raw.get("created_at") or now,
            "source_agent": raw.get("source_agent", "unknown"),
            "source_session": raw.get("source_session", source_session),
            "category": category,
            "fingerprint": raw.get("fingerprint") or self.fingerprint(category, summary),
            "summary": summary,
            "evidence_ref": raw.get("evidence_ref", ""),
            "confidence": float(raw.get("confidence", 0.5)),
            "confidence_updated_at": raw.get("confidence_updated_at") or now,
            "ttl_days": int(raw.get("ttl_days", self.DEFAULT_TTL_DAYS)),
            "occurrence_count": int(raw.get("occurrence_count", 1)),
        }

    # ---- I/O ----

    def append_pending(self, record: dict) -> None:
        self.ensure_canonical_dir()
        with self.pending_path.open("a", encoding="utf-8") as f:
            f.write(json.dumps(record, separators=(",", ":")) + "\n")

    def _read_jsonl(self, path: Path) -> list[dict]:
        if not path.exists():
            return []
        records: list[dict] = []
        for line in path.read_text(encoding="utf-8").splitlines():
            line = line.strip()
            if not line:
                continue
            try:
                records.append(json.loads(line))
            except json.JSONDecodeError:
                continue
        return records

    def _write_jsonl(self, path: Path, records: list[dict]) -> None:
        self.ensure_canonical_dir()
        lines = [json.dumps(r, separators=(",", ":")) for r in records]
        path.write_text("\n".join(lines) + ("\n" if lines else ""), encoding="utf-8")

    def read_canonical(self) -> list[dict]:
        return self._read_jsonl(self.canonical_path)

    def read_pending(self) -> list[dict]:
        return self._read_jsonl(self.pending_path)

    def quarantine(self, record: dict) -> None:
        self.ensure_canonical_dir()
        with self.orphan_path.open("a", encoding="utf-8") as f:
            f.write(json.dumps(record, separators=(",", ":")) + "\n")

    def clear_pending(self) -> None:
        if self.pending_path.exists():
            self.pending_path.unlink()

    # ---- Merge + cleanup ----

    def _acquire_merge_lock(
        self,
        ttl_seconds: int = 30,
        poll_attempts: int = 5,
        poll_interval: float = 0.05,
    ) -> Path:
        """Acquire the canonical-store merge lock via ``os.mkdir`` atomicity.

        Mirrors ``StateManager.acquire_lock`` semantics: the lock directory
        only exists while a merge is in progress. On contention, polls up to
        ``poll_attempts`` times at ``poll_interval`` second intervals. If the
        existing lock dir is older than ``ttl_seconds`` it is treated as
        stale and reclaimed. Raises :class:`MergeLockContention` otherwise.
        """
        self.ensure_canonical_dir()
        lock_dir = self.canonical_dir / ".merge.lock"
        for _ in range(max(1, poll_attempts)):
            try:
                os.mkdir(lock_dir)
                return lock_dir
            except FileExistsError:
                # Check for stale lock (previous crash)
                try:
                    age = time.time() - lock_dir.stat().st_mtime
                except OSError:
                    age = 0.0
                if age >= ttl_seconds:
                    # Reclaim stale lock.
                    try:
                        os.rmdir(lock_dir)
                    except OSError:
                        pass
                    continue
                time.sleep(poll_interval)
        raise MergeLockContention("canonical merge lock held by another process")

    @staticmethod
    def _release_merge_lock(lock_dir: Path) -> None:
        """Release the merge lock created by ``_acquire_merge_lock``."""
        try:
            os.rmdir(lock_dir)
        except OSError:
            # Lock may already be released by a stale-reclaim path; not fatal.
            pass

    def merge_pending(self) -> dict:
        """Merge pending queue into canonical store under a file lock.

        Returns stats dict: ``{captured, quarantined, total_unique,
        canonical_quarantined}``. Applies cleanup policy (TTL prune,
        confidence decay, fingerprint merge) on the full canonical set
        after the merge.

        The entire read + merge + write + clear-pending sequence runs under
        an :func:`os.mkdir`-based lock (parity with
        ``StateManager.acquire_lock``) so two concurrent session-stop hooks
        on the same worktree cannot clobber each other. On lock contention
        the merge is skipped — pending records stay on disk and the next
        stop picks them up. See Codex adversarial finding 1 (2026-04-19).
        """
        try:
            lock_dir = self._acquire_merge_lock()
        except MergeLockContention:
            return {
                "captured": 0,
                "quarantined": 0,
                "canonical_quarantined": 0,
                "total_unique": 0,
                "skipped": "lock contention",
            }
        try:
            return self._merge_pending_unlocked()
        finally:
            self._release_merge_lock(lock_dir)

    def _merge_pending_unlocked(self) -> dict:
        """Merge implementation without lock acquisition.

        Extracted from :meth:`merge_pending` so the lock boundary wraps the
        *entire* read+write sequence. Never call directly outside tests.
        """
        pending = self.read_pending()
        canonical = self.read_canonical()

        captured = 0
        quarantined = 0
        canonical_quarantined = 0

        # Validate + quarantine mismatched pending records
        valid_pending: list[dict] = []
        for record in pending:
            if not self.is_valid(record):
                self.quarantine(record)
                quarantined += 1
            else:
                valid_pending.append(record)

        # Validate + quarantine mismatched canonical records. Partial writes,
        # schema drift, or manual edits can leave records missing required
        # fields (e.g. ``fingerprint``); indexing such records would crash
        # the merge and — via the blanket ``except`` in stop_validator —
        # silently disable all future merges. See Codex adversarial
        # finding 2 (2026-04-19).
        valid_canonical: list[dict] = []
        for record in canonical:
            if not self.is_valid(record):
                self.quarantine(record)
                canonical_quarantined += 1
            else:
                valid_canonical.append(record)

        # Index canonical by fingerprint for dedup (only valid entries)
        by_fp: dict[str, dict] = {r["fingerprint"]: r for r in valid_canonical}
        for record in valid_pending:
            fp = record["fingerprint"]
            if fp in by_fp:
                existing = by_fp[fp]
                existing["occurrence_count"] = existing.get("occurrence_count", 1) + 1
                existing["confidence"] = min(
                    1.0,
                    float(existing.get("confidence", 0.5))
                    + self.CONFIDENCE_REPLENISHMENT_ON_MATCH,
                )
                existing["confidence_updated_at"] = datetime.now(
                    timezone.utc
                ).isoformat()
            else:
                by_fp[fp] = record
            captured += 1

        # Apply TTL prune + confidence decay
        now = datetime.now(timezone.utc)
        surviving: list[dict] = []
        for record in by_fp.values():
            # TTL
            try:
                created = datetime.fromisoformat(
                    record["created_at"].replace("Z", "+00:00")
                )
                age_days = (now - created).days
                if age_days >= int(record.get("ttl_days", self.DEFAULT_TTL_DAYS)):
                    continue
            except (KeyError, ValueError):
                continue
            # Confidence decay
            try:
                updated = datetime.fromisoformat(
                    record["confidence_updated_at"].replace("Z", "+00:00")
                )
                decay_days = (now - updated).days
                decayed = float(record["confidence"]) - (
                    decay_days * self.CONFIDENCE_DECAY_PER_DAY
                )
                record["confidence"] = max(0.0, decayed)
            except (KeyError, ValueError):
                pass
            if record.get("confidence", 0.0) < self.CONFIDENCE_FLOOR:
                continue
            surviving.append(record)

        # Persist
        self._write_jsonl(self.canonical_path, surviving)
        self.clear_pending()

        return {
            "captured": captured,
            "quarantined": quarantined,
            "canonical_quarantined": canonical_quarantined,
            "total_unique": len(surviving),
        }

    # ---- Stage-1 summary ----

    def stage1_summary(self, stats: dict) -> str:
        """One-line summary emitted at session end during the 30-day gate."""
        captured = stats.get("captured", 0)
        total = stats.get("total_unique", 0)
        quarantined = stats.get("quarantined", 0)
        canonical_quarantined = stats.get("canonical_quarantined", 0)
        if captured == 0 and quarantined == 0 and canonical_quarantined == 0:
            return ""
        msg = f"[LEARNINGS] {captured} captured this session, {total} unique total"
        if quarantined:
            msg += f", {quarantined} quarantined (schema mismatch)"
        if canonical_quarantined:
            msg += f", {canonical_quarantined} canonical entries rescued"
        msg += " — Stage 1 logging-only (30-day gate)."
        return msg


# Module-level helper — used by subagent_stop_logger.py
def parse_learning_markers(text: str) -> list[dict]:
    """Extract [LEARNING] JSON blocks from text. Returns list of parsed dicts.

    Tolerant of malformed JSON — skips rather than raising.
    """
    records: list[dict] = []
    if not text:
        return records
    for match in _LEARNING_MARKER_RE.finditer(text):
        try:
            records.append(json.loads(match.group(1)))
        except json.JSONDecodeError:
            continue
    return records
