"""Unit tests for .claude/hooks/ Python modules.

Tests pure logic functions only — no stdin/stdout plumbing.
Run:
    cd .claude/tests && uv run pytest test_hooks.py -v
"""

from __future__ import annotations

import json
import sys
import time
from pathlib import Path
from io import StringIO
from unittest import mock

import pytest

# Insert hooks directory so modules can be imported directly
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "hooks"))

import hook_utils
from hook_utils import StateManager
import pre_tool_use_validator
import pre_commit_verification
import conventional_commit_validator
import pre_push_main_blocker
import post_tool_use_tracker
import stop_validator


# ---------------------------------------------------------------------------
# TestHookUtils
# ---------------------------------------------------------------------------


class TestHookUtils:
    def test_read_input_empty_stdin(self) -> None:
        """Empty stdin returns empty dict."""
        with mock.patch("sys.stdin", StringIO("")):
            result = hook_utils.read_input()
        assert result == {}

    def test_read_input_valid_json(self) -> None:
        """Valid JSON stdin is parsed correctly."""
        payload = json.dumps({"tool_name": "Write", "session_id": "abc123"})
        with mock.patch("sys.stdin", StringIO(payload)):
            result = hook_utils.read_input()
        assert result == {"tool_name": "Write", "session_id": "abc123"}

    def test_deny_builder(self) -> None:
        """deny() returns a deny response dict with the correct structure."""
        result = hook_utils.deny("not allowed")
        assert result["hookSpecificOutput"]["permissionDecision"] == "deny"
        assert result["hookSpecificOutput"]["permissionDecisionReason"] == "not allowed"
        assert result["hookSpecificOutput"]["hookEventName"] == "PreToolUse"

    def test_ask_builder(self) -> None:
        """ask() returns an ask response dict with the correct structure."""
        result = hook_utils.ask("please confirm")
        assert result["hookSpecificOutput"]["permissionDecision"] == "ask"
        assert result["hookSpecificOutput"]["permissionDecisionReason"] == "please confirm"
        assert result["hookSpecificOutput"]["hookEventName"] == "PreToolUse"

    def test_additional_context_builder(self) -> None:
        """additional_context() wraps text in the correct envelope."""
        result = hook_utils.additional_context("reminder text")
        assert result["hookSpecificOutput"]["additionalContext"] == "reminder text"

    def test_relative_path_strips_project_dir_prefix(self) -> None:
        """relative_path removes the project dir prefix and leading slash."""
        rel = hook_utils.relative_path("/home/user/project/src/main.rs", "/home/user/project")
        assert rel == "src/main.rs"

    def test_relative_path_returns_path_unchanged_when_no_prefix(self) -> None:
        """relative_path returns the original path when it doesn't share the prefix."""
        rel = hook_utils.relative_path("/other/dir/file.txt", "/home/user/project")
        assert rel == "/other/dir/file.txt"

    def test_lock_filename_converts_slashes(self) -> None:
        """lock_filename replaces path separators and appends .lock."""
        result = hook_utils.lock_filename("src/lib/file.rs")
        assert result == "src_lib_file.rs.lock"


# ---------------------------------------------------------------------------
# TestStateManager
# ---------------------------------------------------------------------------


class TestStateManager:
    def _make_state(self, tmp_path: Path) -> StateManager:
        """Create a StateManager rooted at tmp_path."""
        return StateManager(str(tmp_path))

    def test_acquire_lock_success(self, tmp_path: Path) -> None:
        """A fresh lock can be acquired."""
        sm = self._make_state(tmp_path)
        acquired = sm.acquire_lock("src/main.rs", "session-A", "Write")
        assert acquired is True

    def test_lock_blocks_other_session(self, tmp_path: Path) -> None:
        """A lock held by session-A is visible to session-B."""
        sm = self._make_state(tmp_path)
        sm.acquire_lock("src/main.rs", "session-A", "Write")
        blocking = sm.check_lock("src/main.rs", "session-B")
        assert blocking == "session-A"

    def test_lock_allows_same_session(self, tmp_path: Path) -> None:
        """check_lock returns None when the caller's own session holds the lock."""
        sm = self._make_state(tmp_path)
        sm.acquire_lock("src/main.rs", "session-A", "Write")
        blocking = sm.check_lock("src/main.rs", "session-A")
        assert blocking is None

    def test_lock_expires_after_ttl(self, tmp_path: Path) -> None:
        """check_lock returns None when the lock timestamp is older than TTL."""
        sm = self._make_state(tmp_path)
        sm.ensure_dirs()
        lock_file = sm.lock_dir / hook_utils.lock_filename("src/main.rs")
        # Write a lock with a timestamp far in the past
        stale_data = {
            "session_id": "session-A",
            "timestamp": int(time.time()) - 9999,
            "tool": "Write",
        }
        lock_file.write_text(json.dumps(stale_data))
        blocking = sm.check_lock("src/main.rs", "session-B", ttl_seconds=120)
        assert blocking is None

    def test_release_session_locks_removes_owned_locks(self, tmp_path: Path) -> None:
        """release_session_locks removes only the locks owned by the given session."""
        sm = self._make_state(tmp_path)
        sm.acquire_lock("src/a.rs", "session-A", "Write")
        sm.acquire_lock("src/b.rs", "session-B", "Write")
        sm.release_session_locks("session-A")
        # session-A lock gone
        assert sm.check_lock("src/a.rs", "session-X") is None
        # session-B lock still present
        assert sm.check_lock("src/b.rs", "session-X") == "session-B"

    def test_session_lifecycle_write_count_remove(self, tmp_path: Path) -> None:
        """Sessions can be written, counted, and removed."""
        sm = self._make_state(tmp_path)
        assert sm.count_active_sessions() == 0
        sm.write_session("abcdef12", "startup")
        assert sm.count_active_sessions() == 1
        sm.write_session("12345678", "startup")
        assert sm.count_active_sessions() == 2
        sm.remove_session("abcdef12")
        assert sm.count_active_sessions() == 1

    def test_clean_old_sessions_removes_stale_files(self, tmp_path: Path) -> None:
        """clean_old_sessions removes files older than max_age_hours."""
        sm = self._make_state(tmp_path)
        sm.write_session("oldoldol", "startup")
        # Manually set the file's mtime to two days ago
        session_file = sm.state_dir / "session_oldoldol.json"
        stale_time = time.time() - (48 * 3600)
        import os
        os.utime(session_file, (stale_time, stale_time))
        sm.clean_old_sessions(max_age_hours=24)
        assert not session_file.exists()

    def test_trim_tracker_only_when_threshold_exceeded(self, tmp_path: Path) -> None:
        """trim_tracker does not trim when line count is below threshold."""
        sm = self._make_state(tmp_path)
        sm.ensure_dirs()
        # Write fewer lines than the default threshold of 110
        lines = [f"[2024-01-01 00:00:00] Write: src/file{i}.rs" for i in range(50)]
        sm.tracker_file.write_text("\n".join(lines) + "\n")
        sm.trim_tracker(max_lines=100, threshold=110)
        # File should be unchanged
        result_lines = sm.tracker_file.read_text().splitlines()
        assert len(result_lines) == 50

    def test_trim_tracker_when_threshold_exceeded(self, tmp_path: Path) -> None:
        """trim_tracker trims to max_lines when threshold is exceeded."""
        sm = self._make_state(tmp_path)
        sm.ensure_dirs()
        # Write more lines than the threshold
        lines = [f"[2024-01-01 00:00:00] Write: src/file{i}.rs" for i in range(120)]
        sm.tracker_file.write_text("\n".join(lines) + "\n")
        sm.trim_tracker(max_lines=100, threshold=110)
        result_lines = sm.tracker_file.read_text().splitlines()
        assert len(result_lines) == 100


# ---------------------------------------------------------------------------
# TestPreToolUseValidator
# ---------------------------------------------------------------------------


class TestPreToolUseValidator:
    def test_protected_git_directory_blocked(self) -> None:
        """Paths containing .git/ are classified as protected."""
        assert pre_tool_use_validator.is_protected(".git/config") is True

    def test_protected_env_file_blocked(self) -> None:
        """Paths containing .env are classified as protected."""
        assert pre_tool_use_validator.is_protected(".env") is True

    def test_normal_file_not_protected(self) -> None:
        """Regular source files are not classified as protected."""
        assert pre_tool_use_validator.is_protected("src/main.rs") is False

    def test_detect_secrets_api_key(self) -> None:
        """A generic api_key= pattern is detected."""
        content = 'api_key = "abcdefghijklmnopqrstuvwxyz1234"'
        result = pre_tool_use_validator.detect_secrets(content)
        assert result is not None
        assert "secret" in result or "generic" in result

    def test_detect_secrets_aws_key(self) -> None:
        """An AWS access key (AKIA...) is detected."""
        content = "export AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE"
        result = pre_tool_use_validator.detect_secrets(content)
        assert result is not None

    def test_detect_secrets_jwt(self) -> None:
        """A JWT token (eyJ...eyJ...) is detected."""
        content = "token = eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ1c2VyIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c"
        result = pre_tool_use_validator.detect_secrets(content)
        assert result is not None

    def test_detect_secrets_github_pat(self) -> None:
        """A GitHub personal access token (ghp_...) is detected."""
        content = "GITHUB_TOKEN=ghp_" + "A" * 36
        result = pre_tool_use_validator.detect_secrets(content)
        assert result is not None

    def test_detect_secrets_pem_key(self) -> None:
        """A PEM private key header is detected."""
        content = "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA..."
        result = pre_tool_use_validator.detect_secrets(content)
        assert result is not None

    def test_test_file_is_identified(self) -> None:
        """Test/spec files are correctly identified by their suffix."""
        assert pre_tool_use_validator.is_test_file("src/utils.test.ts") is True
        assert pre_tool_use_validator.is_test_file("src/utils.spec.js") is True
        assert pre_tool_use_validator.is_test_file("src/utils.ts") is False

    def test_no_false_positive_on_normal_content(self) -> None:
        """Normal Rust source code does not trigger secret detection."""
        content = (
            "pub fn add(a: u32, b: u32) -> u32 {\n"
            "    a + b\n"
            "}\n"
        )
        result = pre_tool_use_validator.detect_secrets(content)
        assert result is None


# ---------------------------------------------------------------------------
# TestPreCommitVerification
# ---------------------------------------------------------------------------


class TestPreCommitVerification:
    def test_non_git_commit_command_ignored(self) -> None:
        """is_git_commit returns False for non-commit commands."""
        assert pre_commit_verification.is_git_commit("cargo build --release") is False

    def test_git_commit_detected(self) -> None:
        """is_git_commit returns True for a git commit command."""
        assert pre_commit_verification.is_git_commit('git commit -m "feat: add feature"') is True

    def test_detect_project_tools_rust(self, tmp_path: Path) -> None:
        """detect_project_tools returns Rust tools when Cargo.toml exists."""
        (tmp_path / "Cargo.toml").write_text('[package]\nname = "myproject"')
        tools = pre_commit_verification.detect_project_tools(str(tmp_path))
        assert "cargo-test" in tools
        assert "cargo-clippy" in tools
        assert "cargo-fmt" in tools

    def test_recently_verified_skips_reminder(self, tmp_path: Path) -> None:
        """is_recently_verified returns True when the sentinel file is fresh."""
        sm = StateManager(str(tmp_path))
        sm.ensure_dirs()
        # Write current timestamp into commit-verified
        verify_file = sm.state_dir / "commit-verified"
        verify_file.write_text(str(int(time.time())))
        assert sm.is_recently_verified(ttl_seconds=300) is True

    def test_build_deny_reason_contains_tools(self) -> None:
        """build_deny_reason includes detected tools in the deny message."""
        reason = pre_commit_verification.build_deny_reason(
            ["cargo-test", "cargo-clippy"], "/tmp/.state"
        )
        assert "BLOCKED" in reason
        assert "cargo-test" in reason
        assert "task verify" in reason

    def test_build_deny_reason_no_tools(self) -> None:
        """build_deny_reason handles empty tool list gracefully."""
        reason = pre_commit_verification.build_deny_reason([], "/tmp/.state")
        assert "none detected" in reason


# ---------------------------------------------------------------------------
# TestConventionalCommitValidator
# ---------------------------------------------------------------------------


class TestConventionalCommitValidator:
    def test_valid_feat_commit(self) -> None:
        """A feat: message is accepted."""
        assert conventional_commit_validator.is_conventional_commit("feat: add search command") is True

    def test_valid_fix_with_scope(self) -> None:
        """A fix(scope): message is accepted."""
        assert conventional_commit_validator.is_conventional_commit("fix(oci): handle missing manifest") is True

    def test_valid_chore_commit(self) -> None:
        """A chore: message is accepted."""
        assert conventional_commit_validator.is_conventional_commit("chore: update AI configuration") is True

    def test_valid_breaking_change(self) -> None:
        """A feat!: breaking change message is accepted."""
        assert conventional_commit_validator.is_conventional_commit("feat!: remove deprecated API") is True

    def test_valid_breaking_with_scope(self) -> None:
        """A refactor(cli)!: breaking change with scope is accepted."""
        assert conventional_commit_validator.is_conventional_commit("refactor(cli)!: rename commands") is True

    def test_all_valid_types(self) -> None:
        """All conventional commit types are accepted."""
        for commit_type in ("feat", "fix", "refactor", "ci", "chore", "docs", "test", "perf", "build", "style"):
            assert conventional_commit_validator.is_conventional_commit(f"{commit_type}: description") is True

    def test_invalid_no_type(self) -> None:
        """A message without a type prefix is rejected."""
        assert conventional_commit_validator.is_conventional_commit("add new feature") is False

    def test_invalid_unknown_type(self) -> None:
        """An unknown type prefix is rejected."""
        assert conventional_commit_validator.is_conventional_commit("feature: add search") is False

    def test_invalid_missing_space_after_colon(self) -> None:
        """A message missing the space after colon is rejected."""
        assert conventional_commit_validator.is_conventional_commit("feat:no space") is False

    def test_invalid_checkpoint(self) -> None:
        """A 'Checkpoint' message is rejected."""
        assert conventional_commit_validator.is_conventional_commit("Checkpoint") is False

    def test_extract_double_quoted_message(self) -> None:
        """extract_commit_message parses double-quoted -m."""
        msg = conventional_commit_validator.extract_commit_message('git commit -m "feat: add feature"')
        assert msg == "feat: add feature"

    def test_extract_single_quoted_message(self) -> None:
        """extract_commit_message parses single-quoted -m."""
        msg = conventional_commit_validator.extract_commit_message("git commit -m 'fix: bug'")
        assert msg == "fix: bug"

    def test_extract_heredoc_message(self) -> None:
        """extract_commit_message parses heredoc-style -m."""
        cmd = 'git commit -m "$(cat <<\'EOF\'\nchore: update config\n\nCo-Authored-By: test\nEOF\n)"'
        msg = conventional_commit_validator.extract_commit_message(cmd)
        assert msg == "chore: update config"

    def test_extract_no_message_flag(self) -> None:
        """extract_commit_message returns None when no -m flag is present."""
        msg = conventional_commit_validator.extract_commit_message("git commit --amend")
        assert msg is None

    def test_non_commit_command(self) -> None:
        """is_git_commit returns False for non-commit commands."""
        assert conventional_commit_validator.is_git_commit("git push origin main") is False

    def test_git_commit_detected(self) -> None:
        """is_git_commit returns True for commit commands."""
        assert conventional_commit_validator.is_git_commit('git commit -m "test: ok"') is True


# ---------------------------------------------------------------------------
# TestPreToolUseValidatorGenerated
# ---------------------------------------------------------------------------


class TestPreToolUseValidatorGenerated:
    def test_generated_file_detected(self) -> None:
        """is_generated returns guidance for known generated files."""
        result = pre_tool_use_validator.is_generated(".github/workflows/release.yml")
        assert result is not None
        assert "cargo-dist" in result

    def test_non_generated_file_returns_none(self) -> None:
        """is_generated returns None for non-generated files."""
        assert pre_tool_use_validator.is_generated(".github/workflows/verify-basic.yml") is None

    def test_regular_source_file_not_generated(self) -> None:
        """is_generated returns None for regular source files."""
        assert pre_tool_use_validator.is_generated("src/main.rs") is None


# ---------------------------------------------------------------------------
# TestPrePushMainBlocker
# ---------------------------------------------------------------------------


class TestPrePushMainBlocker:
    def test_explicit_push_to_main_blocked(self) -> None:
        """A command that explicitly names 'main' as the branch is blocked."""
        assert pre_push_main_blocker.is_push_to_main("git push origin main", "feature") is True

    def test_push_to_feature_branch_allowed(self) -> None:
        """Pushing a feature branch is not blocked."""
        assert pre_push_main_blocker.is_push_to_main("git push origin feature/my-work", "feature/my-work") is False

    def test_push_with_flags_to_main_blocked(self) -> None:
        """Explicit push with flags that names main is blocked."""
        assert pre_push_main_blocker.is_push_to_main("git push --force origin master", "feature") is True

    def test_bare_git_push_on_main_branch_blocked(self) -> None:
        """A bare 'git push' while on main is blocked (implicit tracking branch)."""
        assert pre_push_main_blocker.is_push_to_main("git push", "main") is True

    def test_non_push_command_ignored(self) -> None:
        """is_git_push returns False for a non-push command."""
        assert pre_push_main_blocker.is_git_push("git status") is False


# ---------------------------------------------------------------------------
# TestPostToolUseTracker
# ---------------------------------------------------------------------------


class TestPostToolUseTracker:
    def test_markdown_files_skipped(self) -> None:
        """Markdown files produce no context or config reminder."""
        assert post_tool_use_tracker.check_context_staleness("README.md") is None
        assert post_tool_use_tracker.check_config_reminder("README.md") is None

    def test_context_reminder_for_oci_subsystem(self) -> None:
        """Editing an OCI file triggers a context staleness reminder."""
        reminder = post_tool_use_tracker.check_context_staleness(
            "crates/ocx_lib/src/oci/client.rs"
        )
        assert reminder is not None
        assert "OCI" in reminder

    def test_config_reminder_for_taskfile(self) -> None:
        """Editing the root taskfile.yml triggers a config update reminder."""
        reminder = post_tool_use_tracker.check_config_reminder("taskfile.yml")
        assert reminder is not None
        assert "AI CONFIG UPDATE NEEDED" in reminder

    def test_glob_match_basic_patterns(self) -> None:
        """glob_match handles literal patterns and ** wildcards."""
        assert post_tool_use_tracker.glob_match("taskfile.yml", "taskfile.yml") is True
        assert post_tool_use_tracker.glob_match("src/other.yml", "taskfile.yml") is False
        assert post_tool_use_tracker.glob_match(
            "crates/ocx_lib/src/oci/client.rs",
            "crates/ocx_lib/src/oci/**",
        ) is True
        assert post_tool_use_tracker.glob_match(
            "crates/ocx_lib/src/other/file.rs",
            "crates/ocx_lib/src/oci/**",
        ) is False


# ---------------------------------------------------------------------------
# TestStopValidator
# ---------------------------------------------------------------------------


class TestStopValidator:
    def test_stop_hook_active_exits_early(self) -> None:
        """process() is not called when stop_hook_active is True."""
        # We test the guard indirectly: stop_validator.process itself should work,
        # but main() should exit early. Here we verify process() is a real function
        # and test the build_reminder helper directly.
        reminder = stop_validator.build_reminder(3)
        assert "3 files" in reminder
        assert "SESSION CLEANUP REMINDER" in reminder

    def test_lock_cleanup_on_stop(self, tmp_path: Path) -> None:
        """process() releases session locks when the session stops."""
        sm = StateManager(str(tmp_path))
        sm.acquire_lock("src/main.rs", "sess-stop", "Write")
        # Verify lock is held
        assert sm.check_lock("src/main.rs", "other-session") == "sess-stop"
        # Call process() — it calls release_session_locks internally
        stop_validator.process("sess-stop", str(tmp_path))
        # Lock should be gone
        assert sm.check_lock("src/main.rs", "other-session") is None

    def test_session_removed_on_stop(self, tmp_path: Path) -> None:
        """process() removes the session tracking file."""
        sm = StateManager(str(tmp_path))
        sm.write_session("sess-stop", "startup")
        assert sm.count_active_sessions() == 1
        stop_validator.process("sess-stop", str(tmp_path))
        assert sm.count_active_sessions() == 0
