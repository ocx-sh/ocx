"""Shared utilities for Claude Code hooks.

Provides JSON I/O, hook response builders, state/lock management,
and path utilities. Imported by all hook scripts via sys.path insertion.
"""

from __future__ import annotations

import json
import os
import sys
import time
from datetime import datetime, timezone
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
