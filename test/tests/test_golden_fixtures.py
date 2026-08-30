# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Run the fixture self-checks in CI.

`self_check()` was reachable only by hand, which makes it the shape this project
calls an unchecked green: a validator whose passing state is indistinguishable
from its never having run. Measured, not assumed — a `.sbom` fixture carrying an
`artifactType` (the drift `_check_sbom_sidecar`'s headline assertion exists to
catch) leaves all 5365 Rust unit tests green, because the readers that
`include_bytes!` these files pin only the manifest/document digest binding and
never look at the fields the validator asserts on.

Only the validator is run, never `regenerate()` — capture needs docker, a
registry and a Sigstore stack. `self_check()` needs committed files, `hashlib`
and `cryptography`, which is why it can be gated at all.
"""

from __future__ import annotations

import runpy
from pathlib import Path

FIXTURES = Path(__file__).parent / "fixtures"
GENERATE = FIXTURES / "golden" / "generate.py"
ATTESTATIONS = FIXTURES / "attestations.py"


def test_golden_fixtures_self_check() -> None:
    # ponytail: `run_path` over an import — `tests/` and `tests/fixtures/golden/`
    # carry no `__init__.py`, so `generate` is not importable as a package and
    # this avoids adding two files to make one call. It leaves `__name__` as
    # `"<run_path>"`, so the module's `__main__` block does not fire.
    runpy.run_path(str(GENERATE))["self_check"]()


def test_attestation_fixtures_self_check() -> None:
    """`attestations.py`'s validator, on the same terms as `generate.py`'s.

    Its own docstring said "Not a pytest test (no consumer exists yet)", which
    is the same unchecked green: a validator nothing runs. It has the same
    narrow dependency footprint -- committed files plus `cryptography`, no
    docker, registry or Sigstore stack -- so it gates the same way.
    """
    runpy.run_path(str(ATTESTATIONS))["self_check"]()
