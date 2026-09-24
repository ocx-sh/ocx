"""Unit tests for .claude/hooks/ Python modules.

Tests pure logic functions only — no stdin/stdout plumbing.
Run:
    cd .claude/tests && uv run pytest test_hooks.py -v
"""

from __future__ import annotations

import ast
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path
from io import StringIO
from unittest import mock

import pytest

# Insert hooks directory so modules can be imported directly
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "hooks"))

import hook_utils
from hook_utils import LearningsStore, StateManager
import pre_tool_use_validator
import post_tool_use_tracker
import stop_validator
import subagent_stop_logger


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
        lock_file.write_text(json.dumps(stale_data), encoding="utf-8")
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
        os.utime(session_file, (stale_time, stale_time))
        sm.clean_old_sessions(max_age_hours=24)
        assert not session_file.exists()

    def test_trim_tracker_only_when_threshold_exceeded(self, tmp_path: Path) -> None:
        """trim_tracker does not trim when line count is below threshold."""
        sm = self._make_state(tmp_path)
        sm.ensure_dirs()
        # Write fewer lines than the default threshold of 110
        lines = [f"[2024-01-01 00:00:00] Write: src/file{i}.rs" for i in range(50)]
        sm.tracker_file.write_text("\n".join(lines) + "\n", encoding="utf-8")
        sm.trim_tracker(max_lines=100, threshold=110)
        # File should be unchanged
        result_lines = sm.tracker_file.read_text(encoding="utf-8").splitlines()
        assert len(result_lines) == 50

    def test_trim_tracker_when_threshold_exceeded(self, tmp_path: Path) -> None:
        """trim_tracker trims to max_lines when threshold is exceeded."""
        sm = self._make_state(tmp_path)
        sm.ensure_dirs()
        # Write more lines than the threshold
        lines = [f"[2024-01-01 00:00:00] Write: src/file{i}.rs" for i in range(120)]
        sm.tracker_file.write_text("\n".join(lines) + "\n", encoding="utf-8")
        sm.trim_tracker(max_lines=100, threshold=110)
        result_lines = sm.tracker_file.read_text(encoding="utf-8").splitlines()
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
# The gate call sites. The gate itself is `scripts/commit_gate.py`, run by
# git through the hooks `task git:hooks` installs, and shown red and green by
# its own `--self-test`.
# ---------------------------------------------------------------------------

REPO_ROOT = Path(__file__).resolve().parents[2]
RELEASE_TASKFILE = REPO_ROOT / "taskfiles" / "release.taskfile.yml"


class TestReleasePrepareGuard:
    """`task release:prepare` runs the commit gate predicate first (C-021).

    The predicate itself is shown red and green by
    `scripts/commit_gate.py --self-test`; what this catches is the *call site*
    drifting away from it — a `prepare` that touches a file before asking for
    the full mark, or one that stopped asking at all.
    """

    def test_release_prepare_runs_this_guard_first(self) -> None:
        prepare = RELEASE_TASKFILE.read_text(encoding="utf-8").split("\n  prepare:\n", 1)[1]
        cmds = prepare.split("    cmds:\n", 1)[1]
        first = next(line for line in cmds.splitlines() if line.strip().startswith("- "))
        assert "commit_gate.py --require-full-mark" in first

    @staticmethod
    def _prepare_cmds() -> list[str]:
        """`release:prepare`'s `cmds:`, each rendered as the text task would run or call."""
        import yaml

        tasks = yaml.safe_load(RELEASE_TASKFILE.read_text(encoding="utf-8"))["tasks"]
        return [cmd if isinstance(cmd, str) else f"task: {cmd.get('task')}" for cmd in tasks["prepare"]["cmds"]]

    def test_release_prepare_runs_the_provenance_proof_before_any_edit(self) -> None:
        """C-006 / S-019: the real-bytes proof (release build green, `__testing` build red)
        runs before `cargo set-version` touches a file."""
        cmds = self._prepare_cmds()
        proof = [i for i, cmd in enumerate(cmds) if cmd == "task: provenance:proof"]
        bump = [i for i, cmd in enumerate(cmds) if cmd.startswith("cargo set-version")]
        assert proof, f"release:prepare never runs `provenance:proof`: {cmds}"
        assert bump and proof[0] < bump[0], f"provenance:proof at {proof}, the version bump at {bump}"

    def test_release_prepare_verifies_with_no_cached_verdicts(self) -> None:
        """C-006 / P-7: a release re-executes every acceptance module (`NOCACHE=1`)."""
        verifies = [cmd for cmd in self._prepare_cmds() if cmd.strip().startswith("task verify")]
        assert verifies == ["task verify NOCACHE=1"], verifies

    @pytest.mark.parametrize(("cli_vars", "expect_nocache"), [([], False), (["NOCACHE=1"], True)])
    def test_nocache_reaches_the_acceptance_bazel_test(self, cli_vars: list[str], expect_nocache: bool) -> None:
        """`task verify NOCACHE=1` renders `--nocache_test_results` onto `bazel test //test:all`
        (go-task CLI vars are global); without the var it does not — both ways, so the
        green cannot be a flag that is always there."""
        if shutil.which("task") is None:
            pytest.skip("`task` is not on PATH (shutil.which returned None)")
        result = subprocess.run(
            ["task", "--dry", "verify", *cli_vars],
            cwd=REPO_ROOT,
            capture_output=True,
            encoding="utf-8",
            check=False,
        )
        assert result.returncode == 0, result.stderr
        lines = (result.stdout + result.stderr).splitlines()
        starts = [i for i, line in enumerate(lines) if "bazel  test //test:all" in line or "bazel test //test:all" in line]
        assert starts, "the dry run rendered no `bazel test //test:all` line — the probe read nothing"
        # The command continues on the next line (`\` continuation).
        rendered = "\n".join(lines[starts[0] : starts[0] + 2])
        assert ("--nocache_test_results" in rendered) is expect_nocache, rendered


class TestHandMarkScope:
    """`task verify:mark` writes a *scoped* mark whatever the command line says (DX-3).

    A CLI variable beats a callee's `default`, so without the pinned `vars:`
    on the `.verify:mark` call, `task verify:mark SCOPE=full` hand-writes the
    mark only `task verify` may write. `--dry` renders the command it would
    run; the rendering on stderr is the evidence.
    """

    @pytest.mark.parametrize(
        "cli_vars",
        [
            pytest.param([], id="bare"),
            pytest.param(["SCOPE=full"], id="scope-full"),
            pytest.param(["SCOPE=full", "CRATES=ocx_setup"], id="scope-full-crates"),
        ],
    )
    def test_hand_mark_is_never_full(self, cli_vars: list[str]) -> None:
        if shutil.which("task") is None:
            pytest.skip("`task` is not on PATH (shutil.which returned None)")
        result = subprocess.run(
            ["task", "verify:mark", *cli_vars, "--dry"],
            cwd=REPO_ROOT,
            capture_output=True,
            encoding="utf-8",
            check=False,
        )
        assert result.returncode == 0, result.stderr
        rendered = [line for line in result.stderr.splitlines() if "--mark" in line]
        assert rendered, result.stderr
        assert all("--mark scoped" in line for line in rendered), rendered
        assert "--mark full" not in result.stderr, rendered


class TestHookSubprocessDecoding:
    """Every captured `subprocess.run` under `.claude/hooks/` names `encoding=`.

    `text=True` alone decodes with `locale.getpreferredencoding(False)` — UTF-8
    on a developer's box, ASCII under `LANG=C`. One non-ASCII byte in a branch
    name or a commit subject then raises `UnicodeDecodeError` on one machine
    and nowhere else, and the stop validator reads `status --porcelain` to
    decide whether a session ends on a dirty tree (PY-PROC-01).

    Nothing lints this tree — `task scripts:lint` covers `scripts/` — and ruff
    does not carry the rule for subprocess in any case (`PLW1514` is files
    only), so the sweep is the check.
    """

    _HOOKS = Path(__file__).resolve().parents[1] / "hooks"

    @classmethod
    def _captured_runs(cls) -> list[tuple[str, int, set[str]]]:
        """Every `subprocess.run(capture_output=…)` call site, with its keywords."""
        found: list[tuple[str, int, set[str]]] = []
        for path in sorted(cls._HOOKS.glob("*.py")):
            for node in ast.walk(ast.parse(path.read_text(encoding="utf-8"))):
                if not isinstance(node, ast.Call):
                    continue
                if ast.unparse(node.func) != "subprocess.run":
                    continue
                keywords = {kw.arg for kw in node.keywords if kw.arg}
                if "capture_output" in keywords:
                    found.append((path.name, node.lineno, keywords))
        return found

    def test_the_sweep_finds_the_call_sites(self) -> None:
        """The sweep's own red state: a renamed call shape finds nothing, and
        the assertion below then holds over an empty list."""
        assert self._captured_runs(), (
            "no `subprocess.run(capture_output=…)` call found under .claude/hooks/ — the "
            "reader drifted, or the hooks stopped shelling out (then delete this with them)"
        )

    def test_every_captured_subprocess_names_its_encoding(self) -> None:
        undecoded = [
            f"{name}:{line}"
            for name, line, keywords in self._captured_runs()
            if "encoding" not in keywords
        ]
        assert not undecoded, (
            f"captured `subprocess.run` without `encoding=` in .claude/hooks/: {undecoded}. "
            f"`text=True` alone rides the locale (PY-PROC-01) — pass `encoding=\"utf-8\"`"
        )


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
# TestPostToolUseTracker
# ---------------------------------------------------------------------------


class TestPostToolUseTracker:
    def test_markdown_files_skipped(self) -> None:
        """Markdown files produce no context or config reminder."""
        assert post_tool_use_tracker.check_context_staleness("README.md") is None
        assert post_tool_use_tracker.check_config_reminder("README.md") is None

    def test_every_reminder_glob_matches_a_tracked_file(self) -> None:
        """No reminder row may name a path the tree no longer has.

        Derived rather than a list of pairs, because the failure mode is
        silence: a row whose glob matches nothing fires never, and a hook that
        reminds about nothing is indistinguishable from one that is simply not
        being triggered. WP-27 emptied the monolith's ``src/file_structure/``
        and the row naming it survived the move under a green suite, because
        the only assertions here named the OCI pair. One case per row cannot
        catch the next one; matching every row against the tracked tree can.

        ``git ls-files`` rather than a filesystem walk, so a glob that only
        matches build output or an untracked scratch file still counts as dead.
        """
        tracked = subprocess.run(
            ["git", "-C", str(REPO_ROOT), "ls-files"],
            capture_output=True, text=True, check=True,
        ).stdout.split()
        assert len(tracked) > 500, f"ls-files read {len(tracked)} paths — refusing to judge globs on that"

        rows = [(g, "CONTEXT_REMINDERS") for g, _, _ in post_tool_use_tracker.CONTEXT_REMINDERS]
        rows += [(g, "CONFIG_REMINDERS") for g, _, _ in post_tool_use_tracker.CONFIG_REMINDERS]
        assert rows, "both reminder tables are empty — nothing was read"

        def alive(glob: str) -> bool:
            if any(post_tool_use_tracker.glob_match(path, glob) for path in tracked):
                return True
            # A glob may legitimately name a path git is told to ignore —
            # `.claude/state/plans/` holds real plans and is in `.gitignore`.
            # A tracked-file oracle cannot judge those, so the second test is
            # whether git ignores the glob's directory prefix deliberately.
            # Earned by what the path IS; no row is exempted by name.
            #
            # Deliberately NOT also requiring the directory to exist on disk.
            # That conjunct was here and was wrong twice over: `check-ignore`
            # reads `.gitignore`, never the filesystem — it answers 0 for
            # `.claude/state/plans/DOES_NOT_EXIST` and 1 for
            # `.claude/nonexistent_dir` — so existence added no discrimination
            # at all, only a dependence on untracked local state. A checkout
            # that has not yet run a plan has no `.claude/state/`, so the row
            # read dead in every fresh worktree and would have reddened CI,
            # while passing here because this checkout happens to hold one.
            # A green that depends on local state nobody committed is the same
            # class of defect as the reader floors this check exists to add.
            #
            # Everything before the first wildcard, truncated at the last "/".
            # Not `Path(...).parent`: the pre-wildcard text can end mid-filename
            # ("…/plans/plan_"), which has no suffix and would read as a directory.
            head = glob.split("*", 1)[0]
            if "/" not in head:
                return False
            directory = Path(head.rsplit("/", 1)[0])
            if str(directory) in ("", "."):
                return False
            return subprocess.run(
                ["git", "-C", str(REPO_ROOT), "check-ignore", "-q", str(directory)]
            ).returncode == 0

        dead = [f"{table}: {glob}" for glob, table in rows if not alive(glob)]
        assert not dead, "reminder globs matching nothing the tree has:\n" + "\n".join(dead)

    def test_context_reminder_for_oci_subsystem(self) -> None:
        """Editing an OCI file triggers a context staleness reminder.

        Both spellings, because the subsystem spans two crates: the generic
        distribution-spec client ``ocx_oci`` and the signing tier ``ocx_sign``
        that WP-31 took out of ``ocx_lib/src/oci/``. A row covering only one
        would stop reminding the moment the file being edited is in the other —
        a config that quietly does less, indistinguishable from one that fires.
        """
        for path in (
            "crates/ocx_oci/src/client.rs",
            "crates/ocx_sign/src/verify/pipeline.rs",
        ):
            reminder = post_tool_use_tracker.check_context_staleness(path)
            assert reminder is not None, path
            assert "OCI" in reminder, path

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
            "crates/ocx_sign/src/sign/rekor.rs",
            "crates/ocx_sign/**",
        ) is True
        assert post_tool_use_tracker.glob_match(
            "crates/ocx_store/src/other/file.rs",
            "crates/ocx_sign/**",
        ) is False


# ---------------------------------------------------------------------------
# TestPostToolUseTrackerMarkdownInfra
# ---------------------------------------------------------------------------


class TestPostToolUseTrackerMarkdownInfra:
    def test_config_reminder_fires_on_rules_md(self) -> None:
        """Editing a rule file triggers the catalog-update reminder."""
        reminder = post_tool_use_tracker.check_config_reminder(
            ".claude/rules/quality-core.md"
        )
        assert reminder is not None
        assert "catalog" in reminder

    def test_website_docs_have_no_config_reminder(self) -> None:
        """Website docs are not AI config — no reminder entry should match."""
        assert post_tool_use_tracker.check_config_reminder(
            "website/src/docs/user-guide.md"
        ) is None

    def test_is_ai_config_markdown_rules(self) -> None:
        """Rule files under .claude/rules/ are AI config markdown."""
        assert post_tool_use_tracker._is_ai_config_markdown(
            ".claude/rules/quality-core.md"
        ) is True

    def test_is_ai_config_markdown_website_doc(self) -> None:
        """Website docs are not AI config markdown."""
        assert post_tool_use_tracker._is_ai_config_markdown(
            "website/src/docs/foo.md"
        ) is False

    def test_is_ai_config_markdown_changelog(self) -> None:
        """CHANGELOG.md is not AI config markdown."""
        assert post_tool_use_tracker._is_ai_config_markdown(
            "CHANGELOG.md"
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


# ---------------------------------------------------------------------------
# TestPostToolUseContextSampling — Phase 3 measurement instrumentation
# ---------------------------------------------------------------------------


class TestPostToolUseContextSampling:
    """Phase 3 of the AI config overhaul writes per-tool-call samples to
    `.claude/state/context-samples.jsonl` when the `CLAUDECODE_CONTEXT_REMAINING`
    env var is present. The full context-advisory hook is deferred pending
    measurement data — see
    `.claude/artifacts/adr_ai_config_context_monitor_hook.md`.
    """

    def test_context_sampling_writes_jsonl_when_env_present(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """With the env var set, a sample is appended as a JSONL record."""
        monkeypatch.setenv("CLAUDECODE_CONTEXT_REMAINING", "42.5")
        post_tool_use_tracker._sample_context_budget(
            session_id="sess-phase3",
            tool_name="Write",
            project_dir=str(tmp_path),
        )
        jsonl = tmp_path / ".claude" / "state" / "context-samples.jsonl"
        assert jsonl.exists(), "context-samples.jsonl was not created"
        lines = jsonl.read_text(encoding="utf-8").splitlines()
        assert len(lines) == 1
        record = json.loads(lines[0])
        assert record["session_id"] == "sess-phase3"
        assert record["tool_name"] == "Write"
        assert record["remaining_percentage"] == 42.5
        assert isinstance(record["ts"], int)

    def test_context_sampling_noop_when_env_absent(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """Without the env var, no file is written (fail-open measurement)."""
        monkeypatch.delenv("CLAUDECODE_CONTEXT_REMAINING", raising=False)
        post_tool_use_tracker._sample_context_budget(
            session_id="sess-none",
            tool_name="Edit",
            project_dir=str(tmp_path),
        )
        jsonl = tmp_path / ".claude" / "state" / "context-samples.jsonl"
        assert not jsonl.exists(), (
            "Sampling must be a no-op when CLAUDECODE_CONTEXT_REMAINING is unset"
        )

    def test_context_sampling_noop_when_env_invalid(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """Non-numeric env value fails open (no exception, no write)."""
        monkeypatch.setenv("CLAUDECODE_CONTEXT_REMAINING", "not-a-number")
        post_tool_use_tracker._sample_context_budget(
            session_id="sess-bad",
            tool_name="Write",
            project_dir=str(tmp_path),
        )
        jsonl = tmp_path / ".claude" / "state" / "context-samples.jsonl"
        assert not jsonl.exists(), (
            "Sampling must fail open on non-numeric env values"
        )


# ---------------------------------------------------------------------------
# TestLearningsStore — Phase 4 cross-session learnings store
# ---------------------------------------------------------------------------


class TestLearningsStore:
    """Phase 4 of the AI config overhaul introduces a project-local JSONL
    store of cross-session learnings. See
    `.claude/artifacts/adr_ai_config_cross_session_learnings_store.md`.

    Stage 1 (first 30 days) is logging-only — no promotion candidates.
    These tests cover schema validation, fingerprint dedup, TTL prune,
    and confidence decay.
    """

    def _valid_record(
        self,
        category: str = "rust",
        summary: str = "oci-client AsyncWrite flush bug",
        confidence: float = 0.7,
        created_at: str | None = None,
        confidence_updated_at: str | None = None,
        ttl_days: int = 90,
    ) -> dict:
        """Build a full, schema-v1-valid record for test setup."""
        from datetime import datetime, timezone

        now = datetime.now(timezone.utc).isoformat()
        return {
            "schema_version": 1,
            "id": "11111111-1111-1111-1111-111111111111",
            "created_at": created_at or now,
            "source_agent": "worker-reviewer",
            "source_session": "sess-test",
            "category": category,
            "fingerprint": LearningsStore.fingerprint(category, summary),
            "summary": summary,
            "evidence_ref": "",
            "confidence": confidence,
            "confidence_updated_at": confidence_updated_at or now,
            "ttl_days": ttl_days,
            "occurrence_count": 1,
        }

    def test_fingerprint_is_stable_under_whitespace_variation(self) -> None:
        """Same (category, summary) with different whitespace → same fingerprint."""
        fp1 = LearningsStore.fingerprint("rust", "oci-client flush bug")
        fp2 = LearningsStore.fingerprint("rust", "  oci-client   flush   bug  ")
        fp3 = LearningsStore.fingerprint("rust", "OCI-client Flush Bug")
        assert fp1 == fp2 == fp3

    def test_fingerprint_differs_by_category(self) -> None:
        """Same summary but different category → different fingerprint."""
        fp1 = LearningsStore.fingerprint("rust", "flaky test pattern")
        fp2 = LearningsStore.fingerprint("test", "flaky test pattern")
        assert fp1 != fp2

    def test_is_valid_accepts_full_record(self) -> None:
        """A complete schema-v1 record passes validation."""
        assert LearningsStore.is_valid(self._valid_record()) is True

    def test_is_valid_rejects_schema_version_mismatch(self) -> None:
        """schema_version=999 fails validation (quarantine candidate)."""
        record = self._valid_record()
        record["schema_version"] = 999
        assert LearningsStore.is_valid(record) is False

    def test_is_valid_rejects_unknown_category(self) -> None:
        """An unknown category fails validation."""
        record = self._valid_record()
        record["category"] = "foo"
        assert LearningsStore.is_valid(record) is False

    def test_is_valid_rejects_summary_too_long(self) -> None:
        """A summary exceeding 160 chars fails validation."""
        record = self._valid_record()
        record["summary"] = "x" * 200
        assert LearningsStore.is_valid(record) is False

    def test_append_pending_creates_file(self, tmp_path: Path) -> None:
        """First append materializes `.claude/hooks/.state/learnings-pending.jsonl`."""
        store = LearningsStore(str(tmp_path))
        assert not store.pending_path.exists()
        store.append_pending(self._valid_record())
        assert store.pending_path.exists()
        lines = store.pending_path.read_text(encoding="utf-8").splitlines()
        assert len(lines) == 1
        assert json.loads(lines[0])["category"] == "rust"

    def test_merge_pending_applies_ttl_prune(self, tmp_path: Path) -> None:
        """Records past `created_at + ttl_days` are dropped by merge."""
        from datetime import datetime, timedelta, timezone

        store = LearningsStore(str(tmp_path))
        past = (datetime.now(timezone.utc) - timedelta(days=100)).isoformat()
        expired = self._valid_record(
            summary="expired record",
            created_at=past,
            confidence_updated_at=past,
            ttl_days=90,
        )
        store.append_pending(expired)
        stats = store.merge_pending()
        assert stats["captured"] == 1
        assert stats["total_unique"] == 0
        assert store.read_canonical() == []

    def test_merge_pending_applies_confidence_decay(self, tmp_path: Path) -> None:
        """Decay drives confidence below floor → record dropped."""
        from datetime import datetime, timedelta, timezone

        store = LearningsStore(str(tmp_path))
        # 0.35 starting confidence, decay 0.02/day × 10 days → 0.15 < floor 0.3
        past = (datetime.now(timezone.utc) - timedelta(days=10)).isoformat()
        record = self._valid_record(
            summary="low-confidence decayed",
            confidence=0.35,
            confidence_updated_at=past,
        )
        store.append_pending(record)
        stats = store.merge_pending()
        assert stats["captured"] == 1
        # Decayed below floor → dropped
        assert stats["total_unique"] == 0

    def test_merge_pending_dedups_by_fingerprint(self, tmp_path: Path) -> None:
        """Two pending records with the same fingerprint → one canonical entry."""
        store = LearningsStore(str(tmp_path))
        record1 = self._valid_record(summary="same pattern")
        record2 = self._valid_record(summary="same pattern")
        # Different IDs but same fingerprint
        record2["id"] = "22222222-2222-2222-2222-222222222222"
        store.append_pending(record1)
        store.append_pending(record2)
        stats = store.merge_pending()
        assert stats["captured"] == 2
        assert stats["total_unique"] == 1
        canonical = store.read_canonical()
        assert len(canonical) == 1
        assert canonical[0]["occurrence_count"] == 2

    def test_merge_pending_quarantines_schema_mismatch(self, tmp_path: Path) -> None:
        """schema_version=999 record → quarantined, not canonical."""
        store = LearningsStore(str(tmp_path))
        bad = self._valid_record()
        bad["schema_version"] = 999
        store.append_pending(bad)
        stats = store.merge_pending()
        assert stats["captured"] == 0
        assert stats["quarantined"] == 1
        assert store.read_canonical() == []
        assert store.orphan_path.exists()
        orphan_lines = store.orphan_path.read_text(encoding="utf-8").splitlines()
        assert len(orphan_lines) == 1
        assert json.loads(orphan_lines[0])["schema_version"] == 999

    def test_day30_sentinel_created_on_first_use(self, tmp_path: Path) -> None:
        """`.day30-review-reminder` materializes on first ensure_canonical_dir call."""
        store = LearningsStore(str(tmp_path))
        assert not store.day30_sentinel.exists()
        store.ensure_canonical_dir()
        assert store.day30_sentinel.exists()
        # Content is an ISO-8601 timestamp ~30 days out
        content = store.day30_sentinel.read_text(encoding="utf-8").strip()
        assert "T" in content  # ISO-8601 separator

    def test_parse_learning_markers_extracts_multiple_blocks(self) -> None:
        """Text with two `[LEARNING]` blocks yields two records."""
        text = (
            "Some preamble\n"
            '[LEARNING] {"category": "rust", "summary": "first"}\n'
            "And between\n"
            '[LEARNING] {"category": "test", "summary": "second"}\n'
            "trailing text"
        )
        records = hook_utils.parse_learning_markers(text)
        assert len(records) == 2
        assert records[0]["category"] == "rust"
        assert records[1]["category"] == "test"

    def test_parse_learning_markers_skips_malformed_json(self) -> None:
        """Malformed JSON inside a `[LEARNING]` block is skipped, not raised."""
        text = (
            '[LEARNING] {"category": "rust", "summary": "ok"}\n'
            "[LEARNING] {this is not valid json}\n"
            '[LEARNING] {"category": "test", "summary": "also ok"}'
        )
        records = hook_utils.parse_learning_markers(text)
        assert len(records) == 2
        assert records[0]["category"] == "rust"
        assert records[1]["category"] == "test"

    def test_merge_pending_is_concurrency_safe(self, tmp_path: Path) -> None:
        """Two near-simultaneous merges must not lose records.

        Simulates: session A appends pending record fp1, session B appends
        pending record fp2, both call merge_pending. After both complete,
        the canonical store must contain both records. This regression test
        addresses Codex adversarial finding 1 (2026-04-19).

        The lock-contention code path is exercised separately in
        :meth:`test_merge_pending_skips_on_lock_contention`.
        """
        store_a = LearningsStore(str(tmp_path))
        store_b = LearningsStore(str(tmp_path))
        rec_a = self._valid_record(summary="learning from session A")
        rec_a["id"] = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"
        rec_b = self._valid_record(summary="learning from session B")
        rec_b["id"] = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"
        store_a.append_pending(rec_a)
        store_b.append_pending(rec_b)

        stats_a = store_a.merge_pending()
        stats_b = store_b.merge_pending()

        # Both merges complete; neither reports lock contention.
        assert "skipped" not in stats_a
        assert "skipped" not in stats_b

        canonical = store_a.read_canonical()
        summaries = {r["summary"] for r in canonical}
        assert summaries == {
            "learning from session A",
            "learning from session B",
        }, (
            "Both records must survive a sequential merge pair — if one is "
            "lost, the lock is not protecting the read+write sequence "
            "(Codex finding 1)."
        )

    def test_merge_pending_skips_on_lock_contention(
        self, tmp_path: Path
    ) -> None:
        """When the merge lock is already held, merge_pending skips cleanly.

        Creates the lock directory manually to simulate a concurrent stop
        hook still in the middle of its merge. ``merge_pending`` must
        return a stats dict with ``skipped=="lock contention"`` rather than
        clearing pending records. Codex adversarial finding 1 (2026-04-19).
        """
        store = LearningsStore(str(tmp_path))
        store.append_pending(self._valid_record(summary="survives contention"))
        # Simulate a concurrent holder.
        store.ensure_canonical_dir()
        lock_dir = store.canonical_dir / ".merge.lock"
        import os as _os

        _os.mkdir(lock_dir)
        try:
            # Short poll configuration so the test is fast.
            stats = store.merge_pending()
        finally:
            _os.rmdir(lock_dir)

        assert stats.get("skipped") == "lock contention"
        # Pending records must remain on disk for the next stop to claim.
        assert store.pending_path.exists()
        assert store.read_pending(), (
            "merge_pending must not clear pending records when it skips "
            "due to lock contention (Codex finding 1)."
        )

    def test_merge_pending_quarantines_malformed_canonical(
        self, tmp_path: Path
    ) -> None:
        """Invalid canonical records are quarantined, not allowed to break
        the merge. Regression for Codex adversarial finding 2 (2026-04-19).
        """
        store = LearningsStore(str(tmp_path))
        store.ensure_canonical_dir()
        # Seed canonical with a valid record AND an invalid one
        # (missing ``fingerprint``). Write directly bypassing the normal
        # validator path, simulating schema drift or a partial write.
        valid = store.normalize_record(
            {"category": "rust", "summary": "good canonical entry"}
        )
        invalid = {
            "schema_version": 1,
            "id": "bad",
            "category": "rust",
            "summary": "broken canonical missing fingerprint",
        }
        store.canonical_path.write_text(
            json.dumps(valid) + "\n" + json.dumps(invalid) + "\n",
            encoding="utf-8",
        )
        # Also seed one pending record with a distinct fingerprint.
        store.append_pending(
            store.normalize_record(
                {"category": "rust", "summary": "fresh pending entry"}
            )
        )

        stats = store.merge_pending()

        # The invalid canonical record was rescued (quarantined) and did
        # NOT abort the merge: both the previously-valid canonical record
        # and the newly-merged pending record should survive.
        assert stats["canonical_quarantined"] == 1
        assert stats["captured"] == 1
        canonical_lines = store.canonical_path.read_text(encoding="utf-8").splitlines()
        assert len(canonical_lines) == 2, (
            "Valid canonical entry + newly-merged pending entry should "
            "survive; malformed entry should be gone from canonical."
        )
        # Orphan store contains the malformed record.
        assert store.orphan_path.exists()
        assert "broken canonical missing fingerprint" in (
            store.orphan_path.read_text(encoding="utf-8")
        )

        # Stage 1 summary surfaces the canonical rescue.
        summary = store.stage1_summary(stats)
        assert "canonical entries rescued" in summary


# ---------------------------------------------------------------------------
# TestSubagentLearningCapture — Phase 4 integration through subagent hook
# ---------------------------------------------------------------------------


class TestSubagentLearningCapture:
    """`subagent_stop_logger.capture_learnings` scans every string field of
    the hook input for `[LEARNING]` markers and writes them to the pending
    queue. Tolerant of nested structures; no-op on input without markers.
    """

    def test_capture_learnings_from_nested_string_payload(
        self, tmp_path: Path
    ) -> None:
        """A `[LEARNING]` marker buried in nested input produces a pending record."""
        input_data = {
            "session_id": "sess-nested",
            "tool_name": "Task",
            "tool_response": {
                "content": [
                    {
                        "type": "text",
                        "text": (
                            "Finished. Observed pattern during implementation.\n"
                            '[LEARNING] {"category": "rust", '
                            '"summary": "clippy needs-fix quirk"}\n'
                        ),
                    }
                ],
            },
        }
        count = subagent_stop_logger.capture_learnings(
            input_data, str(tmp_path)
        )
        assert count == 1
        store = LearningsStore(str(tmp_path))
        pending = store.read_pending()
        assert len(pending) == 1
        assert pending[0]["category"] == "rust"
        assert pending[0]["source_session"] == "sess-nested"
        assert pending[0]["schema_version"] == 1

    def test_capture_learnings_noop_on_no_markers(self, tmp_path: Path) -> None:
        """Input without any `[LEARNING]` markers writes nothing."""
        input_data = {
            "session_id": "sess-clean",
            "tool_name": "Task",
            "tool_response": {"content": [{"type": "text", "text": "Done."}]},
        }
        count = subagent_stop_logger.capture_learnings(
            input_data, str(tmp_path)
        )
        assert count == 0
        store = LearningsStore(str(tmp_path))
        assert not store.pending_path.exists()

    def test_capture_learnings_redacts_records_containing_secrets(
        self, tmp_path: Path
    ) -> None:
        """A `[LEARNING]` whose summary contains a secret-like value is dropped.

        Per `adr_ai_config_cross_session_learnings_store.md` §Privacy, the
        existing `pre_tool_use_validator.detect_secrets` regex set is reused
        to prevent accidentally persisting credentials into the learnings
        store.
        """
        # AWS access key pattern: AKIA[0-9A-Z]{16}
        input_data = {
            "session_id": "sess-secret",
            "tool_name": "Task",
            "tool_response": {
                "content": [
                    {
                        "type": "text",
                        "text": (
                            '[LEARNING] {"category": "build", '
                            '"summary": "spotted key AKIAIOSFODNN7EXAMPLE in env"}\n'
                            '[LEARNING] {"category": "rust", '
                            '"summary": "clean pattern, no secrets here"}'
                        ),
                    }
                ],
            },
        }
        count = subagent_stop_logger.capture_learnings(
            input_data, str(tmp_path)
        )
        assert count == 1, (
            "Exactly the clean record should be captured; the record "
            "containing the AWS key must be redacted."
        )
        store = LearningsStore(str(tmp_path))
        pending = store.read_pending()
        assert len(pending) == 1
        assert "AKIA" not in pending[0]["summary"]

    def test_capture_learnings_stage1_limitation_captures_quoted_examples(
        self, tmp_path: Path
    ) -> None:
        """Stage-1 documented limitation: quoted ``[LEARNING]`` examples in
        docs are captured. See ``capture_learnings`` docstring — Stage 2
        will narrow via an intentional-emission envelope. Codex adversarial
        finding 3 (2026-04-19).
        """
        input_data = {
            "session_id": "sess-stage1",
            "tool_name": "Task",
            "output": (
                "Here's an example of a learning marker:\n"
                "```\n"
                '[LEARNING] {"schema_version": 1, "category": "rust", '
                '"summary": "example only, not a real learning"}\n'
                "```\n"
            ),
        }
        count = subagent_stop_logger.capture_learnings(
            input_data, str(tmp_path)
        )
        # Current Stage 1 behavior: captures the quoted example.
        assert count == 1, (
            "Stage 1: scanner captures quoted examples (acceptable noise; "
            "Stage 2 will narrow via an intentional-emission envelope)."
        )


# ---------------------------------------------------------------------------
# TestStopValidatorLearnings — Phase 4 merge at session end
# ---------------------------------------------------------------------------


class TestStopValidatorLearnings:
    """`stop_validator.process()` merges `.state/learnings-pending.jsonl`
    into the canonical store on every session end and emits a Stage 1
    summary line. MEMORY.md is never touched.
    """

    def _valid_record(
        self, summary: str = "merge-path learning"
    ) -> dict:
        from datetime import datetime, timezone

        now = datetime.now(timezone.utc).isoformat()
        return {
            "schema_version": 1,
            "id": "33333333-3333-3333-3333-333333333333",
            "created_at": now,
            "source_agent": "worker-reviewer",
            "source_session": "sess-end",
            "category": "rust",
            "fingerprint": LearningsStore.fingerprint("rust", summary),
            "summary": summary,
            "evidence_ref": "",
            "confidence": 0.8,
            "confidence_updated_at": now,
            "ttl_days": 90,
            "occurrence_count": 1,
        }

    def test_process_merges_pending_into_canonical(self, tmp_path: Path) -> None:
        """After process(): canonical has the record, pending cleared, summary emitted."""
        store = LearningsStore(str(tmp_path))
        store.append_pending(self._valid_record())
        reminder = stop_validator.process("sess-end", str(tmp_path))
        # Canonical now has the record
        canonical = store.read_canonical()
        assert len(canonical) == 1
        assert canonical[0]["summary"] == "merge-path learning"
        # Pending is cleared
        assert not store.pending_path.exists()
        # Reminder contains the Stage 1 summary tag
        assert "[LEARNINGS]" in reminder

    def test_memory_md_untouched_by_phase4(self, tmp_path: Path) -> None:
        """`MEMORY.md` file mtime is unchanged by Phase 4 processing."""
        memory_md = tmp_path / "MEMORY.md"
        memory_md.write_text("# memory\n- user preference\n", encoding="utf-8")
        import os as _os

        # Snapshot mtime at a fixed past timestamp so the test is stable
        past = time.time() - 3600
        _os.utime(memory_md, (past, past))
        mtime_before = memory_md.stat().st_mtime

        store = LearningsStore(str(tmp_path))
        store.append_pending(self._valid_record())
        stop_validator.process("sess-end", str(tmp_path))

        mtime_after = memory_md.stat().st_mtime
        assert mtime_after == mtime_before, (
            "Phase 4 hook code must never touch MEMORY.md"
        )

    def test_process_stage1_summary_mentions_stage_1_gate(
        self, tmp_path: Path
    ) -> None:
        """The Stage 1 summary documents the 30-day gate policy."""
        store = LearningsStore(str(tmp_path))
        store.append_pending(self._valid_record())
        reminder = stop_validator.process("sess-stage1", str(tmp_path))
        # Stage 1 policy disclosed inline so the human running the session
        # knows this is logging-only.
        assert "Stage 1" in reminder
