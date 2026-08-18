# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the config-driven rungs of the trust-root ladder.

Contract sources: ``.claude/artifacts/adr_offline_verify_trust_cache.md``
(amendment: six-rung ladder) and ``.claude/artifacts/adr_managed_config_tier.md``
(amendment: publish-time inlining).

``test_offline_verify.py`` covers the rungs an operator reaches by hand — the
``--trusted-root`` flag, ``OCX_SIGSTORE_TRUSTED_ROOT``, and the warm cache. The
rungs below it are the ones a *fleet* uses, and they are what this file proves:

3. ``[trust.sigstore] trusted_root_json`` in a ``config.toml``
4. the convention path ``$OCX_HOME/sigstore/trusted-root.json``

plus the delivery channel that puts (3) on every machine: a managed-config
package, whose path-form ``trusted_root`` ``ocx config push`` inlines at publish
time.

Every case runs ``--offline`` against an unreachable Rekor. Offline is what makes
the assertion falsifiable: with no trust material the run must exit 78, so a pass
can only have come from the rung under test — never from a cached key, and never
from a TUF fetch.
"""
from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

from src.helpers import push_managed_config
from src.runner import OcxRunner, PackageInfo, current_platform
from tests.fixtures import adversarial
from tests.fixtures.sigstore_stack import SigstoreStack


def _sign(ocx: OcxRunner, stack: SigstoreStack, token: Path, pkg: PackageInfo) -> None:
    """Signs ``pkg`` online against the real stack; publishes the referrer."""
    result = subprocess.run(
        [str(ocx.binary), "package", "sign", *stack.sign_args(token), pkg.short],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode == 0, f"sign setup failed: {result.stderr}"


def _verify_offline(
    ocx: OcxRunner,
    stack: SigstoreStack,
    pkg: PackageInfo,
    *,
    extra_env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Offline verify with NO trust-root flag and NO ``OCX_SIGSTORE_TRUSTED_ROOT``.

    The Rekor URL is a dead port, so nothing can be fetched even if the offline
    gate were to leak: a pass proves the trust root was resolved locally.
    """
    return subprocess.run(
        [
            str(ocx.binary),
            "package", "verify",
            "--certificate-identity", stack.identity,
            "--certificate-oidc-issuer", stack.issuer,
            "--rekor-url", adversarial.unreachable_rekor_url(),
            "--platform", current_platform(),
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env={**ocx.env, "OCX_OFFLINE": "1", **(extra_env or {})},
    )


def _prepare(ocx: OcxRunner, stack: SigstoreStack, token: Path, pkg: PackageInfo) -> None:
    """Signs the package and populates the local index so the tag resolves offline."""
    _sign(ocx, stack, token, pkg)
    ocx.json("package", "install", "--select", pkg.short)


def _adopt_managed(ocx: OcxRunner, ref: str) -> None:
    """Fetches the managed-config snapshot for ``ref`` — online, once.

    ``OCX_MANAGED_CONFIG`` names the source; it does not fetch it. Without this
    step an ``--offline`` run fails at "snapshot required but absent", which is
    exit 78 for a reason that has nothing to do with trust material — the shape
    that would make the negative test below pass for free.
    """
    result = subprocess.run(
        [str(ocx.binary), "config", "update"],
        capture_output=True,
        text=True,
        env={**ocx.env, "OCX_MANAGED_CONFIG": ref},
    )
    assert result.returncode == 0, f"managed-config adoption failed: {result.stderr}"


def _drop_trust_cache(ocx: OcxRunner) -> None:
    """Removes the trust-root cache — rung 5, which would otherwise rescue a
    run whose rung under test was just taken away."""
    shutil.rmtree(Path(ocx.env["OCX_HOME"]) / "state" / "trust_root", ignore_errors=True)


# ──────────────────────────────────────────────────────────────────────────────
# Rung 4 — $OCX_HOME/sigstore/trusted-root.json, no config, no flag, no env
# ──────────────────────────────────────────────────────────────────────────────


def test_home_convention_path_carries_the_trust_root_alone(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """A file at the convention path is enough — and taking it away breaks it.

    Both halves matter. The first proves the rung is wired at all; the second
    proves the first was not passing for some other reason, by deleting the file
    *and* the cache the successful run just warmed and demanding exit 78.
    """
    pkg = published_package
    _prepare(ocx, sigstore_stack, identity_token, pkg)

    convention = Path(ocx.env["OCX_HOME"]) / "sigstore" / "trusted-root.json"
    convention.parent.mkdir(parents=True, exist_ok=True)
    convention.write_bytes(sigstore_stack.trusted_root_json.read_bytes())

    green = _verify_offline(ocx, sigstore_stack, pkg)
    assert green.returncode == 0, (
        f"$OCX_HOME/sigstore/trusted-root.json must be consulted with no flag and no "
        f"env var, got {green.returncode}\nstderr: {green.stderr.strip()}"
    )

    convention.unlink()
    _drop_trust_cache(ocx)

    red = _verify_offline(ocx, sigstore_stack, pkg)
    assert red.returncode == 78, (
        f"with the convention path removed and the cache cold, offline verify must "
        f"fail closed at 78 — otherwise the green above proved nothing. Got "
        f"{red.returncode}\nstderr: {red.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Rung 3 — [trust.sigstore] trusted_root_json inlined in a config.toml
# ──────────────────────────────────────────────────────────────────────────────


def test_config_tier_inline_trusted_root_json_carries_the_trust_root(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """``trusted_root_json`` in ``$OCX_HOME/config.toml`` verifies with no file path.

    This is the form a fleet receives, so it must work with nothing on disk but
    the config itself — no sidecar JSON, no env var.
    """
    pkg = published_package
    _prepare(ocx, sigstore_stack, identity_token, pkg)

    document = sigstore_stack.trusted_root_json.read_text()
    config = Path(ocx.env["OCX_HOME"]) / "config.toml"
    config.write_text(f"[trust.sigstore]\ntrusted_root_json = '''\n{document}\n'''\n")

    result = _verify_offline(ocx, sigstore_stack, pkg)
    assert result.returncode == 0, (
        f"[trust.sigstore] trusted_root_json must supply the trust root on its own, got "
        f"{result.returncode}\nstderr: {result.stderr.strip()}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Delivery — `ocx config push` inlines the path form; the fleet gets the document
# ──────────────────────────────────────────────────────────────────────────────


def test_managed_config_delivers_the_trust_root_to_a_machine_with_no_local_copy(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """The whole point of the feature, end to end.

    The operator publishes a config naming the trust root by a path on *their*
    disk; ``ocx config push`` inlines the document. A consumer adopting that
    package — digest-pinned, holding no trust file of its own — verifies.
    """
    pkg = published_package
    _prepare(ocx, sigstore_stack, identity_token, pkg)

    (tmp_path / "trusted_root.json").write_bytes(sigstore_stack.trusted_root_json.read_bytes())
    payload = '[trust.sigstore]\ntrusted_root = "trusted_root.json"\n'
    digest = push_managed_config(ocx, f"{unique_repo}_cfg", "v1", payload, tmp_path)

    pinned = f"{ocx.registry}/{unique_repo}_cfg@{digest}"
    _adopt_managed(ocx, pinned)

    result = _verify_offline(ocx, sigstore_stack, pkg, extra_env={"OCX_MANAGED_CONFIG": pinned})
    assert result.returncode == 0, (
        f"a digest-pinned managed config must deliver the inlined trust root to a "
        f"machine holding no local copy, got {result.returncode}\n"
        f"stderr: {result.stderr.strip()}"
    )


def test_a_tag_pinned_managed_source_may_not_carry_a_trust_root(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """Same payload, tag-pinned: the trust root is dropped and verify fails closed.

    A tag can be moved by whoever can push, so an unpinned source could swap the
    root that decides what this machine trusts. The identical digest-pinned case
    above is what makes this a discriminating test rather than a tautology.
    """
    pkg = published_package
    _prepare(ocx, sigstore_stack, identity_token, pkg)

    (tmp_path / "trusted_root.json").write_bytes(sigstore_stack.trusted_root_json.read_bytes())
    payload = '[trust.sigstore]\ntrusted_root = "trusted_root.json"\n'
    push_managed_config(ocx, f"{unique_repo}_cfg", "v1", payload, tmp_path)

    floating = f"{ocx.registry}/{unique_repo}_cfg:v1"
    _adopt_managed(ocx, floating)

    result = _verify_offline(ocx, sigstore_stack, pkg, extra_env={"OCX_MANAGED_CONFIG": floating})
    assert result.returncode == 78, (
        f"a tag-pinned managed source must not be allowed to supply the trust root, "
        f"got {result.returncode}\nstderr: {result.stderr.strip()}"
    )
    # The snapshot IS present (adopted above), so 78 must be the trust-material
    # refusal and not "managed config snapshot required but absent" — otherwise
    # this test would pass without the loader ever dropping anything.
    assert "snapshot required" not in result.stderr, (
        f"the snapshot must be present, so exit 78 has to come from the missing trust "
        f"root, not from an unadopted tier: {result.stderr.strip()}"
    )
