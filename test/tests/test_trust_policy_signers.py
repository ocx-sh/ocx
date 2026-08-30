# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the ``signers`` array on ``[[trust.policy]]`` (WP9a).

A policy accepts a **set** of signers rather than one nested matcher: each entry
is tagged ``kind = "keyless"`` or ``kind = "key"``, and mixing them in one policy
is legal — that is how a fleet migrates between signing models without touching
scope.

Two properties are worth an acceptance test rather than a unit test, because
both are only observable at the CLI boundary:

* **An empty ``signers`` array fails closed.** Refused, never read as a
  catch-all. A permissive reading would turn a deleted line into a silent
  bypass, which no unit assertion on an internal type can demonstrate to an
  operator.
* **The same path-form ``key`` value is accepted locally and refused in a
  published payload.** The rule is about *publishing*, not about the value, so
  only the pair proves it. A path in a fleet-distributed ``config.toml`` names
  the operator's disk and means nothing on any consumer's.

Sibling to ``test_trust_policy.py`` (tier precedence, identity matching), whose
Sigstore-stack fixtures the policy-compilation tests here reuse.
"""

from __future__ import annotations

import subprocess
from pathlib import Path

from src.runner import OcxRunner, PackageInfo, current_platform
from tests.fixtures.sigstore_stack import SigstoreStack

# The public half of the key pair cosign generated for the golden fixtures. It
# signs nothing outside this repository, and it is a *public* key besides — see
# `test/tests/fixtures/golden/keys/README.md`.
GOLDEN_PUBLIC_KEY_PEM = (
    Path(__file__).parent / "fixtures" / "golden" / "keys" / "cosign.pub"
).read_text()

# Config-tier-only tests disable project discovery so the CWD walk never picks
# up this repo's own dogfooding `ocx.toml`.
_NO_PROJECT: dict[str, str] = {"OCX_NO_PROJECT": "1"}


def _key_signer_policy(scope: str, *, key: str | None = None, key_pem: str | None = None) -> str:
    """Render a `[[trust.policy]]` whose single signer is a key, by reference or inline."""
    entry = 'kind = "key", ' + (f'key = "{key}"' if key is not None else f'key_pem = """\n{key_pem}"""')
    return f'[[trust.policy]]\nscope = "{scope}"\nsigners = [{{ {entry} }}]\n'


def _write_candidate(tmp_path: Path, content: str) -> Path:
    """A managed-config payload candidate, validated by `ocx config test`."""
    path = tmp_path / "candidate.toml"
    path.write_text(content)
    return path


# ──────────────────────────────────────────────────────────────────────────────
# A managed payload takes `key_pem` only
# ──────────────────────────────────────────────────────────────────────────────


def test_managed_payload_refuses_a_key_signer_named_by_path(ocx: OcxRunner, tmp_path: Path) -> None:
    """Both spellings of the path form, relative and absolute.

    Either names the operator's disk, so a check that caught only one would ship
    the other as a payload that resolves to nothing on every consumer.
    """
    for reference in ("etc/acme-release.pub", "/srv/keys/acme.pub"):
        candidate = _write_candidate(tmp_path, _key_signer_policy("ghcr.io/acme/*", key=reference))

        result = ocx.run("config", "test", str(candidate), check=False)

        assert result.returncode == 78, (
            f"`{reference}` names a path and must be refused with exit 78 (ConfigError), "
            f"got {result.returncode}\nstderr: {result.stderr.strip()}"
        )
        assert "key_pem" in result.stderr, (
            f"the refusal must name the fix for `{reference}`; got: {result.stderr.strip()}"
        )


def test_managed_payload_accepts_an_inline_key_signer(ocx: OcxRunner, tmp_path: Path) -> None:
    """The inline form is what travels — without it, the refusal above would
    leave a fleet with no way to pin a key at all."""
    candidate = _write_candidate(
        tmp_path, _key_signer_policy("ghcr.io/acme/*", key_pem=GOLDEN_PUBLIC_KEY_PEM)
    )

    result = ocx.run("config", "test", str(candidate), check=False)

    assert result.returncode == 0, f"an inline key travels with the payload: {result.stderr.strip()}"


def test_managed_payload_accepts_a_keyless_signer(ocx: OcxRunner, tmp_path: Path) -> None:
    """The refusal is scoped to key signers. A keyless policy names no file, so a
    payload that already ships one must keep publishing."""
    candidate = _write_candidate(
        tmp_path,
        '[[trust.policy]]\nscope = "ghcr.io/acme/*"\n'
        'signers = [{ kind = "keyless", identity = "ci@acme.example", '
        'oidc_issuer = "https://iss.example" }]\n',
    )

    result = ocx.run("config", "test", str(candidate), check=False)

    assert result.returncode == 0, f"a keyless signer names no operator path: {result.stderr.strip()}"


def test_the_same_key_reference_is_accepted_in_a_local_config(ocx: OcxRunner, tmp_path: Path) -> None:
    """**The half that proves the rule is about publishing, not about the value.**

    The byte-identical path-form `key` line that the payload above refuses sits
    in `$OCX_HOME/config.toml` here and breaks nothing — the operator owns that
    filesystem, and the reference resolves against the directory of the config
    that declared it. Not a containment check.
    """
    key_path = tmp_path / "acme-release.pub"
    key_path.write_text(GOLDEN_PUBLIC_KEY_PEM)
    reference = str(key_path)

    home_config = Path(ocx.env["OCX_HOME"]) / "config.toml"
    home_config.parent.mkdir(parents=True, exist_ok=True)
    home_config.write_text(_key_signer_policy("ghcr.io/acme/*", key=reference))

    # `config test` merges a candidate ONTO the machine tier, so it reads the
    # local `config.toml` in full. A refusal there would break every command
    # that resolves configuration, not just publishing. That the reference is
    # also genuinely *read from disk* is proved one test down, in
    # `test_a_key_only_policy_refuses_a_keyless_signature` — loading is a
    # weaker claim than compiling, and only compiling opens the file.
    unrelated = tmp_path / "unrelated.toml"
    unrelated.write_text('[registry]\ndefault = "corp.example.com"\n')
    read_locally = ocx.run("config", "test", str(unrelated), check=False)
    assert read_locally.returncode == 0, (
        f"a local tier may name a key by path: {read_locally.stderr.strip()}"
    )

    # And the identical text is still refused as a *published* payload.
    candidate = _write_candidate(tmp_path, _key_signer_policy("ghcr.io/acme/*", key=reference))
    published = ocx.run("config", "test", str(candidate), check=False)
    assert published.returncode == 78, (
        "the same value is refused when it is published, which is the whole rule; "
        f"got {published.returncode}"
    )


# ──────────────────────────────────────────────────────────────────────────────
# Fail-closed and mixed sets — policy compilation at verify time
# ──────────────────────────────────────────────────────────────────────────────


def _verify(
    ocx: OcxRunner,
    stack: SigstoreStack,
    pkg: PackageInfo,
    extra_env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Policy-mode verify: no certificate flags, so `[[trust.policy]]` decides."""
    return subprocess.run(
        [
            str(ocx.binary),
            "package", "verify",
            "--platform", current_platform(),
            "--rekor-url", stack.rekor_url,
            "--sigstore-trusted-root", str(stack.trust_root),
            pkg.short,
        ],
        capture_output=True,
        text=True,
        env={**ocx.env, **(extra_env or {})},
    )


def _sign(ocx: OcxRunner, stack: SigstoreStack, token: Path, pkg: PackageInfo) -> None:
    signed = subprocess.run(
        [str(ocx.binary), "package", "sign", *stack.sign_args(token), pkg.short],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert signed.returncode == 0, f"sign setup failed: {signed.stderr}"


def test_an_empty_signers_array_fails_closed(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """**The fail-closed case, end to end.**

    A policy that matches the target and names no acceptable signer accepts
    nothing. Read permissively it would mean "anyone may sign", so deleting the
    last entry from a `signers` array would silently disable the pin it was
    written to enforce — a signature this package genuinely carries must NOT
    verify through it.
    """
    pkg = published_package
    _sign(ocx, sigstore_stack, identity_token, pkg)

    scope = f"{ocx.registry}/{pkg.repo}"
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(
        f'[[trust.policy]]\nscope = "{scope}"\nsigners = []\n'
    )

    verify = _verify(ocx, sigstore_stack, pkg, extra_env=_NO_PROJECT)
    assert verify.returncode == 78, (
        "an empty signer set is a configuration error, never a catch-all; expected exit 78 "
        f"(ConfigError / TrustPolicyInvalid), got {verify.returncode}\n"
        f"stderr: {verify.stderr.strip()}"
    )
    assert scope in verify.stderr, (
        f"the refusal must name which policy is at fault; got: {verify.stderr.strip()}"
    )


def test_a_policy_omitting_signers_entirely_fails_closed(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
) -> None:
    """Absent reads as empty. The superseded `[trust.policy.keyless]` sub-table
    lands here too: unknown keys stay tolerated for fleet forward-compat, so it
    parses and then names no signer — which is exactly why the refusal has to
    come from compilation rather than from the deserializer."""
    pkg = published_package
    _sign(ocx, sigstore_stack, identity_token, pkg)

    scope = f"{ocx.registry}/{pkg.repo}"
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(
        f'[[trust.policy]]\nscope = "{scope}"\n\n'
        f'[trust.policy.keyless]\nidentity = "{sigstore_stack.identity}"\n'
        f'oidc_issuer = "{sigstore_stack.issuer}"\n'
    )

    verify = _verify(ocx, sigstore_stack, pkg, extra_env=_NO_PROJECT)
    assert verify.returncode == 78, (
        "a policy naming no signer must refuse, not silently accept the signature "
        f"the old spelling described; got {verify.returncode}\nstderr: {verify.stderr.strip()}"
    )


def test_a_mixed_keyless_and_key_policy_verifies_on_its_keyless_half(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """**Adding a signer widens acceptance; it never narrows it.**

    A keyless entry beside a key entry is the migration shape. The keyless-signed
    package must still verify — if adding the key entry refused it, the array
    would be an ALL-of, which is the opposite of what it is.
    """
    pkg = published_package
    _sign(ocx, sigstore_stack, identity_token, pkg)

    key_path = tmp_path / "acme-release.pub"
    key_path.write_text(GOLDEN_PUBLIC_KEY_PEM)

    scope = f"{ocx.registry}/{pkg.repo}"
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(
        f'[[trust.policy]]\nscope = "{scope}"\nsigners = [\n'
        f'  {{ kind = "key", key = "{key_path}" }},\n'
        f'  {{ kind = "keyless", identity = "{sigstore_stack.identity}", '
        f'oidc_issuer = "{sigstore_stack.issuer}" }},\n'
        "]\n"
    )

    verify = _verify(ocx, sigstore_stack, pkg, extra_env=_NO_PROJECT)
    assert verify.returncode == 0, (
        "the keyless entry still admits a keyless signature when a key entry sits beside it; "
        f"got {verify.returncode}\nstderr: {verify.stderr.strip()}"
    )


def test_a_key_only_policy_refuses_a_keyless_signature(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    identity_token: Path,
    tmp_path: Path,
) -> None:
    """Spec D5's other direction: a policy whose signers are all `kind = "key"`
    names nobody who signs with a certificate, so a keyless artifact must not
    pass. A key backend contributing "no objection" would make the mixed policy
    above prove nothing."""
    pkg = published_package
    _sign(ocx, sigstore_stack, identity_token, pkg)

    key_path = tmp_path / "acme-release.pub"
    key_path.write_text(GOLDEN_PUBLIC_KEY_PEM)

    scope = f"{ocx.registry}/{pkg.repo}"
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(
        _key_signer_policy(scope, key=str(key_path))
    )

    verify = _verify(ocx, sigstore_stack, pkg, extra_env=_NO_PROJECT)
    assert verify.returncode == 77, (
        "a key-only policy must not admit a keyless signature; expected the "
        f"policy refusal (77), got {verify.returncode}\nstdout: {verify.stdout.strip()}"
        f"\nstderr: {verify.stderr.strip()}"
    )

    # 77 rather than `!= 0` is the whole point, and this is what discriminates
    # it: delete the file the local path-form `key` reference names and the
    # same policy stops *compiling* (74) instead of refusing (77). A signers
    # array that were silently ignored could not tell these two apart — both
    # halves would be some non-zero code that never read the key at all.
    #
    # 74 and not 78: an unreadable key file is a filesystem failure on a path
    # the operator named, and `ocx package sign --key <missing>` has always
    # answered 74 for the same file. This assertion used to read 78, which is
    # what let the two doors disagree; the discriminator it exists for is
    # unaffected, since the point is only that this code is not the 77 above.
    key_path.unlink()
    unreadable = _verify(ocx, sigstore_stack, pkg, extra_env=_NO_PROJECT)
    assert unreadable.returncode == 74, (
        "an unreadable key file is an I/O error on an operator-supplied path, "
        "which is how we know the reference is genuinely read from disk; got "
        f"{unreadable.returncode}\nstderr: {unreadable.stderr.strip()}"
    )
