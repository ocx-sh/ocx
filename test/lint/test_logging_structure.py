"""The logging oracle's row table is complete against the tree (plan C-048).

`tests/test_logging.py` pins, per library crate, one `log::` line an
`OCX_LOG=<crate>=debug` directive must select. Its `LIBRARY_TARGETS` table is
only as good as its coverage of the crates that actually log, and that is a
fact about `crates/*/src/**/*.rs`, not about the binary — so the completeness
check reads the tree here, in the lint tier (plan_test_speed_tiers.md C-009),
while the rows themselves keep running against `ocx` in acceptance.
"""

from __future__ import annotations

import ast
import re
from pathlib import Path

from src.helpers import PROJECT_ROOT

# The acceptance module's own table, read from its source rather than copied:
# a second list here would be checked against the tree while the one the
# acceptance rows run stayed free to drift. Only each row's crate name is read;
# the rest of a row names the binary's behaviour, which is acceptance's to run.
LIBRARY_TARGETS = [
    (row.elts[0].value, None, None, None)
    for node in ast.parse(
        (Path(__file__).resolve().parents[1] / "tests" / "test_logging.py").read_text(encoding="utf-8")
    ).body
    if isinstance(node, ast.Assign)
    and any(isinstance(target, ast.Name) and target.id == "LIBRARY_TARGETS" for target in node.targets)
    for row in node.value.elts
]


# The crate whose targets are the CLI's own. Excluded from the derived set
# below because it is not a library row: its coverage is ``CLI_TARGET``, which
# every parametrised case asserts as its control.
CLI_CRATE = "ocx_cli"


LOG_MACRO = re.compile(r"\blog::(?:debug|info|warn|error|trace)!")


def _crates_with_log_sites() -> set[str]:
    """Every workspace crate whose ``src/`` calls the ``log`` facade."""
    crates = PROJECT_ROOT / "crates"
    found = set()
    for manifest in crates.glob("*/Cargo.toml"):
        src = manifest.parent / "src"
        if any(LOG_MACRO.search(f.read_text(encoding="utf-8", errors="replace")) for f in src.rglob("*.rs")):
            found.add(manifest.parent.name)
    return found


def test_every_crate_that_logs_has_a_row():
    """``LIBRARY_TARGETS`` is complete against the tree, not against a comment.

    The table said "a crate is added here by the commit that extracts it" and
    nothing checked it, so a crate that gained ``log::`` call sites — or one
    extracted without its row — was invisible. Derived rather than listed: the
    subject is every crate whose ``src/`` reaches the facade.
    """
    rows = {target for target, _, _, _ in LIBRARY_TARGETS}
    logging = _crates_with_log_sites()
    assert CLI_CRATE in logging, (
        f"{CLI_CRATE} calls no log macro, so this test is reading the wrong tree; found={sorted(logging)}"
    )
    assert len(logging) > 3, f"only {len(logging)} crate(s) found to log at all — the scan lost its subject"
    assert rows == logging - {CLI_CRATE}, (
        "LIBRARY_TARGETS and the tree disagree about which crates log: "
        f"missing a row {sorted(logging - {CLI_CRATE} - rows)}, "
        f"row with no call site {sorted(rows - logging)}"
    )
