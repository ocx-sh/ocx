from __future__ import annotations

from pathlib import Path
from uuid import uuid4

import pytest

from src.helpers import PROJECT_ROOT
from src.runner import OcxRunner, PackageInfo

from recordings.cast_recorder import CastRecorder
from recordings.setups import SETUPS

# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------

_SCRIPTS_DIR = Path(__file__).resolve().parent / "scripts"
_CASTS_DIR = PROJECT_ROOT / "website" / "src" / "public" / "casts"

# ---------------------------------------------------------------------------
# CLI options
# ---------------------------------------------------------------------------


def pytest_addoption(parser: pytest.Parser) -> None:
    parser.addoption(
        "--cast-dir",
        default=str(_CASTS_DIR),
        help="Output directory for .cast files.",
    )


# ---------------------------------------------------------------------------
# Script parsing
# ---------------------------------------------------------------------------


def parse_script(path: Path) -> dict:
    """Parse a shell script with metadata comments.

    Metadata lines: ``# key: value``
    Command lines: everything else (non-empty, non-comment).
    """
    meta: dict[str, str] = {}
    commands: list[str] = []

    for line in path.read_text().splitlines():
        stripped = line.strip()
        if not stripped:
            continue
        if stripped.startswith("# "):
            rest = stripped[2:]
            if ": " in rest:
                key, value = rest.split(": ", 1)
                key = key.strip().lower()
                if key in ("title", "setup", "description"):
                    meta[key] = value.strip()
                    continue
        if stripped.startswith("#"):
            continue
        commands.append(stripped)

    return {"meta": meta, "commands": commands, "path": path}


def collect_scripts() -> list[dict]:
    """Discover all .sh scripts in the scripts/ directory."""
    if not _SCRIPTS_DIR.exists():
        return []
    scripts = sorted(_SCRIPTS_DIR.glob("*.sh"))
    return [parse_script(s) for s in scripts]


# ---------------------------------------------------------------------------
# Test generation
# ---------------------------------------------------------------------------


def pytest_generate_tests(metafunc: pytest.Metafunc) -> None:
    """Parametrise the ``script`` fixture from .sh files."""
    if "script" in metafunc.fixturenames:
        scripts = collect_scripts()
        ids = [s["path"].stem for s in scripts]
        metafunc.parametrize("script", scripts, ids=ids, indirect=True)


@pytest.fixture()
def script(request: pytest.FixtureRequest) -> dict:
    return request.param


# ---------------------------------------------------------------------------
# Recording-specific fixtures
# ---------------------------------------------------------------------------


@pytest.fixture(scope="session")
def cast_dir(request: pytest.FixtureRequest) -> Path:
    return Path(request.config.getoption("--cast-dir"))


@pytest.fixture()
def recorder(ocx: OcxRunner):
    env = ocx.env.copy()
    env.setdefault("TERM", "xterm-256color")
    rec = CastRecorder(env=env)
    rec.open()
    yield rec
    rec.close()


@pytest.fixture()
def setup_env(
    script: dict,
    ocx: OcxRunner,
    tmp_path: Path,
) -> dict[str, list[PackageInfo]]:
    """Provision the recording environment based on the script's # setup directive."""
    setup_name = script["meta"].get("setup", "basic")
    setup_fn = SETUPS.get(setup_name)
    if setup_fn is None:
        raise ValueError(
            f"Unknown setup '{setup_name}' in {script['path'].name}. "
            f"Available: {', '.join(SETUPS)}"
        )
    prefix = f"r_{uuid4().hex[:8]}_"
    return setup_fn(ocx, tmp_path, prefix)
