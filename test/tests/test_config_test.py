# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx config test`` (report-only payload validation).

``config test`` answers one operator question before a rollout: *if I publish
this file as the managed-config payload, does it parse, and what would this
machine's configuration look like afterwards?* It validates locally and
previews the merge — it publishes nothing, adopts nothing, and writes nothing.

Validation reuses the same payload validator ``ocx config push`` runs
(64 KiB cap, TOML parse into the config schema, ``[managed]`` rejection), so
the two commands can never disagree about what is publishable.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from src.helpers import SIGSTORE_DIR, push_managed_config
from src.runner import OcxRunner

pytestmark = pytest.mark.command("config_test")

# A real self-signed CA (the test stack's Fulcio root): the positive case needs
# material the certificate parser accepts, not a placeholder string.
_CA_PEM = (SIGSTORE_DIR / "keys" / "fulcio-ca.crt.pem").read_text()

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _write_home_config(ocx: OcxRunner, content: str) -> Path:
    """Write ``$OCX_HOME/config.toml`` — the machine tier the candidate merges onto."""
    path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content)
    return path


def _write_candidate(tmp_path: Path, content: str, name: str = "candidate.toml") -> Path:
    """Write a candidate payload (deliberately NOT named ``config.toml`` — the
    command takes any path)."""
    path = tmp_path / name
    path.write_text(content)
    return path


# ---------------------------------------------------------------------------
# Effective-merge preview
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# Rejections (exit 78) — shared with `ocx config push`
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# Resolution gates — a payload can parse and still be unusable
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# `[patches]` precedence
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# Unknown-key warnings (exit 0)
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# Report-only contract
# ---------------------------------------------------------------------------


def test_config_test_writes_nothing(
    ocx: OcxRunner, unique_repo: str, registry: str, tmp_path: Path
) -> None:
    """No publish, no adoption, no snapshot, no config mutation.

    The snapshot assertion is proven non-vacuous in the same fixture: the tier
    is configured against a really-published payload, and after the negative
    assert an `ocx config update` creates exactly the path that was asserted
    absent. Without that second half, the negative would pass on a machine
    where nothing could ever have written there.
    """
    home = Path(ocx.env["OCX_HOME"])
    snapshot_dir = home / "state" / "managed-config"
    machine_config = _write_home_config(ocx, '[registry]\ndefault = "machine.example"\n')
    before = machine_config.read_text()
    candidate_text = '[registry]\ndefault = "corp.example.com"\n'
    candidate = _write_candidate(tmp_path, candidate_text)

    push_managed_config(
        ocx, unique_repo, "user", '[registry]\ndefault = "published.example"\n', tmp_path
    )
    source = f"{registry}/{unique_repo}:user"

    report = ocx.json(
        "config", "test", str(candidate), env_overrides={"OCX_MANAGED_CONFIG": source}
    )
    assert report["managed"] is not None, "the tier must be configured for this test to prove anything"

    assert machine_config.read_text() == before, "the machine config must not be rewritten"
    assert candidate.read_text() == candidate_text, "the candidate must not be rewritten"
    assert not snapshot_dir.exists(), "config test must never persist a managed-config snapshot"

    # The adopting command writes the very path just asserted absent.
    ocx.json("config", "update", env_overrides={"OCX_MANAGED_CONFIG": source})
    assert (snapshot_dir / "snapshot.json").exists(), (
        "config update must create the path config test leaves alone - otherwise the "
        "assertion above proves nothing"
    )


# ---------------------------------------------------------------------------
# Loader-untouched guard
# ---------------------------------------------------------------------------
#
# Unknown-key detection is a `config test` capability ONLY. The ordinary config
# loader stays silent about them by design: one `config.toml` is read by many
# ocx versions at once, so a key a newer ocx understands must load quietly on an
# older binary. The two tests below are a pair — the first proves an ordinary
# command says nothing about the typo, the second proves the typo is genuinely
# there to be found, so the silence is not vacuous.


_TYPO_CONFIG = '[registry]\ndefault = "machine.example"\ntimeuot = 30\n'


def test_ordinary_command_stays_silent_about_an_unknown_config_key(ocx: OcxRunner) -> None:
    _write_home_config(ocx, _TYPO_CONFIG)

    result = ocx.run("about", check=False)

    assert result.returncode == 0, result.stderr
    assert "timeuot" not in result.stderr, (
        "the ordinary config loader must never warn about unknown keys "
        f"(fleet forward-compat); stderr was: {result.stderr!r}"
    )
