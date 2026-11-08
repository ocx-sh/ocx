"""Repo-wide sweep: the old environment seams stay deleted.

The exec environment model replaced per-key forwarding (`apply_ocx_config`, the `OCX_ENV` envelope,
the launch-identity variable, the stacking scope, the ledger reconciler) with one projection over a
recorded execution state; a removed name matched as a whole identifier reds this sweep. It scans
`crates/`, `test/` and `website/`, exempting by path `CHANGELOG.md`, the frozen bodies inside
published artifacts, regenerated hub files and this file. A name that must stay is listed in
:data:`ALLOWED` with its reason. The scan is floored on files read (:data:`FILE_FLOOR`), so a reader
that stopped cannot report a clean tree; `OCX_SEAMS_SWEEP_ROOT` sweeps another checkout.
"""

from __future__ import annotations

import os
import re
import subprocess
from pathlib import Path
from typing import NamedTuple

import pytest

from src.helpers import PROJECT_ROOT

ROOTS: tuple[str, ...] = ("crates", "test", "website")

THIS_FILE = "test/lint/test_exec_env_seams_removed.py"

FILE_FLOOR = 1800
"""Fewer text files read than this fails the sweep (the tree reads about 1880; the floor leaves headroom)."""

_EXEMPT_PREFIXES: tuple[str, ...] = (
    "CHANGELOG.md",
    ".claude/artifacts/",
    "website/src/content/docs/reference/command-line/",
)
_EXEMPT_FILES: frozenset[str] = frozenset(
    {
        THIS_FILE,
        # Frozen bodies: byte-identical to what published launchers embed.
        "crates/ocx_package_manager/src/launcher/body.rs",
        "crates/ocx_package_manager/src/tasks/render_toolchain.rs",
        "crates/ocx_shim/src/main.rs",
        "crates/ocx_shim/src/core.rs",
    }
)
_EXEMPT_PATTERNS: tuple[re.Pattern[str], ...] = (
    re.compile(r"^crates/[^/]+/tests/golden/"),
    re.compile(r"^crates/ocx_sdkgen/tests/golden/"),
)

# name -> regex. Every pattern is anchored on identifier boundaries.
NAMES: dict[str, re.Pattern[str]] = {
    "apply_ocx_config": re.compile(r"\bapply_ocx_config\b"),
    "OcxConfigView": re.compile(r"\bOcxConfigView\b"),
    "forward!": re.compile(r"\bforward!"),
    "inherited_env": re.compile(r"\binherited_env\b"),
    "stacked_env": re.compile(r"\bstacked_env\b"),
    "stacked_clean_env": re.compile(r"\bstacked_clean_env\b"),
    "keep_applied": re.compile(r"\bkeep_applied\b"),
    "revert_reconciled": re.compile(r"\brevert_reconciled\b"),
    "Ledger*": re.compile(r"\bLedger[A-Z]\w*|\bLedger::"),
    "ExecScope": re.compile(r"\bExecScope\b"),
    "Child": re.compile(r"\bChild::(?:Inherit|Scrub|Forward)\b|\bocx_env::Child\b|\bchild = (?:Inherit|Scrub|Forward)\b"),
    "OCX_ENV": re.compile(r"\bOCX_ENV\b"),
    "__OCX_LAUNCH_IDENTITIES": re.compile(r"\b__OCX_LAUNCH_IDENTITIES\b"),
    "__OCX_EXEC_SCOPE": re.compile(r"\b__OCX_EXEC_SCOPE\b"),
    "--stack": re.compile(r"(?<![\w-])--stack\b"),
    "set_forwarded_env": re.compile(r"\bset_forwarded_env\b"),
    "ForwardedEnvError": re.compile(r"\bForwardedEnvError\b"),
    "forwarded_env_invalid": re.compile(r"\bforwarded_env_invalid\b"),
    "launch_identities_invalid": re.compile(r"\blaunch_identities_invalid\b"),
}

_SPELLED_DIFFERENTLY: dict[str, tuple[str, ...]] = {
    "Ledger*": ("Ledger",),
    "Child": ("Child::", "ocx_env::Child", "child = "),
}
_NEEDLES: dict[str, tuple[str, ...]] = {name: _SPELLED_DIFFERENTLY.get(name, (name,)) for name in NAMES}
"""A literal each pattern contains. A file without it holds no hit for that name, and `in` skips it
at memchr speed where the word-boundary patterns cost 9 s over the tree."""

_REJECTS_STACK = "a test that the removed `--stack` flag is rejected as an unknown argument"
_REJECTS_OCX_ENV = "a test that a stale ambient `OCX_ENV` is ignored, not applied"

ALLOWED: dict[str, dict[str, str]] = {
    "crates/ocx_sdkgen/src/compat.rs": {"Ledger*": "the contract ledger (`ledger.toml`), unrelated to the reconciler ledger"},
    "crates/ocx_sdkgen/tests/compat_gate.rs": {"Ledger*": "the contract ledger (`ledger.toml`), unrelated to the reconciler ledger"},
    "crates/ocx_cli/src/command/shell.rs": {"--stack": _REJECTS_STACK},
    "crates/ocx_cli/src/command/toolchain_exec.rs": {"--stack": _REJECTS_STACK},
    "test/tests/test_shell_open.py": {
        "--stack": _REJECTS_STACK,
        "__OCX_EXEC_SCOPE": "a test that the retired scope marker is never set",
    },
    "test/tests/test_exec_modes.py": {"--stack": _REJECTS_STACK},
    "test/tests/test_toolchain_cli.py": {"--stack": _REJECTS_STACK},
    "test/tests/test_execution_records.py": {"--stack": "asserts no record states the removed flag"},
    "test/tests/test_project_env.py": {"OCX_ENV": _REJECTS_OCX_ENV},
}

_TEXT_SUFFIXES = frozenset(
    {
        ".rs", ".py", ".md", ".mdx", ".toml", ".json", ".yml", ".yaml", ".sh", ".ps1", ".fish", ".elv",
        ".bzl", ".bazel", ".ts", ".tsx", ".mjs", ".js", ".txt", ".astro", ".css", ".tera", ".html", ".cmd", ".bat",
    }
)


class Hit(NamedTuple):
    path: str
    line: int
    name: str
    text: str


class Scan(NamedTuple):
    files_read: int
    hits: list[Hit]


def _root() -> Path:
    override = os.environ.get("OCX_SEAMS_SWEEP_ROOT")
    return Path(override) if override else PROJECT_ROOT


def _listed(root: Path) -> list[str]:
    result = subprocess.run(
        ["git", "-C", str(root), "ls-files", "-z", "--cached", "--others", "--exclude-standard", "--", *ROOTS],
        capture_output=True,
        check=True,
    )
    return [entry for entry in result.stdout.decode("utf-8", errors="replace").split("\0") if entry]


def _exempt(rel: str) -> bool:
    return (
        rel in _EXEMPT_FILES
        or rel.startswith(_EXEMPT_PREFIXES)
        or any(pattern.match(rel) for pattern in _EXEMPT_PATTERNS)
    )


def scan(root: Path, rels: list[str]) -> Scan:
    hits: list[Hit] = []
    read = 0
    for rel in rels:
        if _exempt(rel) or Path(rel).suffix not in _TEXT_SUFFIXES:
            continue
        try:
            text = (root / rel).read_text(encoding="utf-8", errors="ignore")
        except OSError:
            continue
        read += 1
        allowed = ALLOWED.get(rel, {})
        live = [
            (name, pattern)
            for name, pattern in NAMES.items()
            if name not in allowed and any(needle in text for needle in _NEEDLES[name])
        ]
        if not live:
            continue
        for number, line in enumerate(text.splitlines(), start=1):
            for name, pattern in live:
                if pattern.search(line):
                    hits.append(Hit(rel, number, name, line.strip()[:160]))
    return Scan(read, hits)


@pytest.fixture(scope="module")
def swept() -> Scan:
    """The one walk of the tree both sweep tests judge."""
    return scan(_root(), _listed(_root()))


@pytest.mark.xdist_group("exec_env_seams_sweep")
def test_the_sweep_reads_the_tree(swept: Scan) -> None:
    assert swept.files_read >= FILE_FLOOR, (
        f"the sweep read {swept.files_read} files, under the floor of {FILE_FLOOR}: "
        "a reader that stopped looks the same as a clean tree"
    )


@pytest.mark.xdist_group("exec_env_seams_sweep")
def test_no_removed_seam_is_named(swept: Scan) -> None:
    rendered = "\n".join(f"  {h.path}:{h.line}  [{h.name}]  {h.text}" for h in swept.hits)
    assert not swept.hits, f"removed exec-env seams are named again ({len(swept.hits)}):\n{rendered}"


def test_a_planted_name_reds_the_sweep(tmp_path: Path) -> None:
    """Red-proof: each forbidden name, planted in a scratch tree, is found; its neighbours are not."""
    plain = "pub fn test_run_clean_strips_inherited_env() {}\nconst S: &str = \"OCX_ENV_STATE\";\n"
    planted = (
        "".join(f"// {name}\n" for name in NAMES)
        + "fn x() { forward!(a); apply_ocx_config(); }\n"
        + "let c = ocx_env::Child::Scrub;\nLedgerEntry::new();\n"
    )
    (tmp_path / "crates" / "c").mkdir(parents=True)
    (tmp_path / "crates" / "c" / "plain.rs").write_text(plain)
    (tmp_path / "crates" / "c" / "planted.rs").write_text(planted)
    result = scan(tmp_path, ["crates/c/plain.rs", "crates/c/planted.rs"])
    assert result.files_read == 2
    assert not [h for h in result.hits if h.path.endswith("plain.rs")]
    found = {h.name for h in result.hits if h.path.endswith("planted.rs")}
    assert found >= set(NAMES), set(NAMES) - found
    (tmp_path / "crates" / "c" / "typed.rs").write_text(
        "let c = ocx_env::Child::Scrub;\nLedgerEntry::new();\nlet s = Child::Forward;\nchild = Inherit\n"
    )
    typed_hits = {h.name for h in scan(tmp_path, ["crates/c/typed.rs"]).hits}
    assert {"Child", "Ledger*"} <= typed_hits, "the literal prefilter must pass every spelling a pattern matches"
    typed = scan_text("let c = ocx_env::Child::Scrub; LedgerEntry::new();")
    assert {"Child", "Ledger*"} <= typed


def scan_text(text: str) -> set[str]:
    return {name for name, pattern in NAMES.items() if pattern.search(text)}
