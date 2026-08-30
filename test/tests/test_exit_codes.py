# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the public exit-code contract (sysexits alignment).

These codes are the scripting surface for tools that consume OCX — scripts
do `case $?` on them. Each test exercises one specific exit code via a minimal
real-world failure that reliably triggers it.

Contract demonstrations: the xfail-marked tests below document the intended
mapping but currently fail because the product doesn't yet route these error
paths through `classify_error`. They are `strict=True` so the marker gets
removed automatically the moment the product catches up:

- 64 (UsageError): wired in Phase 6 review-fix Round 1. `app.rs` switched from
  `get_matches()` to `try_get_matches()` and now surfaces clap parse errors as
  `UsageError`, which classifies to exit 64 (EX_USAGE).
- 65 (DataError): the identifier parser is permissive — `not:::valid:::identifier`
  successfully parses and the install fails later as `NotFound` (79). Triggering
  `IdentifierError` at parse time requires either a stricter parser or a test
  fixture that can inject a pre-parsed `IdentifierError` through the CLI.
- 69 (Unavailable): a registry that *answers*, just not usefully, is hard to
  provoke from the CLI. An unroutable host never answers at all, so it is 75
  (see below), not 69.

74 (IoError) *does* have a trigger, and it is the parametrized sweep at the
bottom of this file: an operator-supplied path that is not there, or that names
a destination nothing can be written to, is the everyday shape of a filesystem
failure — no disk-full injection needed. That sweep pins one exact code per
row, never a range: 15 of the 17 rows are 74; one is 78
(`verify-malformed-ocx-toml` — the file was read fine, its contents are invalid
TOML); and one is 79 (`control-config-test-missing` — `config test`'s own table
reserves 79 for an absent candidate and 74 for a read failure that is not
not-found). What every row refuses is exit 1 `internal`, which reports an
operator's typo as an ocx bug.

Deferred codes (no reliable acceptance-test trigger available):
- 77 (PermissionDenied): filesystem EPERM not reliably injectable without root.
"""
from __future__ import annotations

import subprocess
from collections.abc import Callable
from pathlib import Path

import pytest

from src.helpers import make_package, resolved_metadata_path
from src.runner import OcxRunner


class TestExitCodes:
    """End-to-end tests for the public exit-code contract (sysexits alignment).

    These codes are the scripting surface for tools that consume OCX — scripts
    do `case $?` on them. Exercise each code via a minimal real-world failure
    that triggers it.
    """

    def test_exit_code_64_usage_error_on_bogus_flag(self, ocx: OcxRunner) -> None:
        """Unknown flag → clap rejects → exit 64 (EX_USAGE).

        Phase 6 review-fix Round 1: `app.rs` now uses `try_get_matches()` and
        surfaces clap parse errors as `UsageError`, classifying to exit 64.
        """
        result = subprocess.run(
            [str(ocx.binary), "package", "install", "--not-a-real-flag", "cmake:3.28"],
            capture_output=True,
            text=True,
            env=ocx.env,
        )
        assert result.returncode == 64, (
            f"expected exit 64 (UsageError) for unknown flag, "
            f"got {result.returncode}\nstderr: {result.stderr.strip()}"
        )

    @pytest.mark.xfail(
        strict=True,
        reason="identifier parser is permissive — malformed shapes resolve to "
        "NotFound (79) at install time rather than IdentifierError (65) at parse time",
    )
    def test_exit_code_65_data_error_on_invalid_identifier(self, ocx: OcxRunner) -> None:
        """Malformed identifier → IdentifierError → exit 65 (EX_DATAERR)."""
        result = subprocess.run(
            [str(ocx.binary), "package", "install", "not:::valid:::identifier"],
            capture_output=True,
            text=True,
            env=ocx.env,
        )
        assert result.returncode == 65, (
            f"expected exit 65 (DataError) for malformed identifier, "
            f"got {result.returncode}\nstderr: {result.stderr.strip()}"
        )

    def test_exit_code_75_tempfail_on_unroutable_registry(
        self, ocx: OcxRunner
    ) -> None:
        """Unroutable registry → ClientError::RegistryTransient → exit 75.

        The connect never completed, so nothing about the request was answered
        and the same command may succeed once the host is reachable. That is
        exactly what 75 (EX_TEMPFAIL) promises a retrying wrapper, and what 69
        would deny it.
        """
        # Port 1 is reserved and unroutable on standard systems.
        env = {**ocx.env, "OCX_DEFAULT_REGISTRY": "127.0.0.1:1"}
        result = subprocess.run(
            [str(ocx.binary), "index", "update", "some/pkg"],
            capture_output=True,
            text=True,
            env=env,
        )
        assert result.returncode == 75, (
            f"expected exit 75 (TempFail) for unroutable registry, "
            f"got {result.returncode}\nstderr: {result.stderr.strip()}"
        )


# ---------------------------------------------------------------------------
# The operator-supplied-path invariant
# ---------------------------------------------------------------------------
#
# One rule, swept across every command that takes a path from the operator:
#
#     a path that is not there, or a destination that cannot be written,
#     is never exit 1 `internal`.
#
# Exit 1 is what the downcast ladder in `classify_error` produces when it finds
# no rung — so an untyped `io::Error` reaching the envelope turns an operator's
# typo into an ocx bug report. The next site added reds here the moment its row
# is added, and the shape below is the row.
#
# Every row also carries an EXACT expected code, and this is the correction of
# an earlier design. The table used to assert only "one of {64, 65, 74, 77, 78,
# 79}", on the argument that whether a missing file is 74 or 79 was an open call
# worth leaving open. It was not: 74 and 78 were both inside that range, so the
# `control-sign--key-file` row passed identically whether `--key <missing>`
# exited 74 or 78 — and it sat green across a review pass while `verify` answered
# 78 for the same flag and the same value that `sign` and `attest` answered 74
# for. A tolerated *range* of exit codes is the "unchecked green" tell, and the
# looseness bought no flexibility; it bought a hidden contract split.
#
# So: no row is on a range. All 17 are pinned, because for all 17 the right
# answer is decided and documented —
#
#   74  every unusable operator-supplied path that reached a filesystem call:
#       the file could not be read or the destination could not be written.
#       Includes `--key <missing>` on all three of sign, attest and verify,
#       which is the parity the three key-file rows exist to hold.
#   78  `verify-malformed-ocx-toml` — the file was read fine and its *contents*
#       are not valid TOML. A config-parse failure, not an I/O failure.
#   79  `control-config-test-missing` — an explicitly named config path that is
#       absent is `NotFound` by documented contract (`reference/command-line.md`:
#       "A missing explicit path exits with code 79"), not an I/O error.
#
# `_ALLOWED_CODES` survives the tightening, one job smaller: it no longer decides
# a run, it constrains what a row may be PINNED to. That keeps the family
# property — an unusable operator-supplied path is never exit 1 `internal`, never
# a panic (101), never a clap default — as a statement about the table rather
# than a hole in it, and it means a row added tomorrow cannot be pinned to
# whatever the binary happens to print today. Widening it is then a deliberate,
# reviewable line of diff (77 for a permission-denied row, say), not an accident.
#
# `needle` is the third leg and unchanged: it stops a row from passing on a
# failure that never reached the file at all — a missing package or an
# unconfigured registry would exit non-1 whatever the product does with the path.

_ALLOWED_CODES = frozenset({74, 78, 79})

# A builder gets the runner, a unique repo name and a scratch directory, and
# returns the argv to run plus the substring stderr must carry to prove the
# *file* is what failed.
CaseBuilder = Callable[[OcxRunner, str, Path], tuple[list[str], str]]


def _missing(tmp_path: Path) -> Path:
    """A path that does not exist, under a parent that does not exist either."""
    return tmp_path / "nope" / "absent-input"


def _unwritable(tmp_path: Path, name: str) -> Path:
    """A destination whose parent component is a regular file (ENOTDIR).

    Not a `chmod 000` directory: that needs a non-root euid to mean anything
    (CI runs as root in containers) and would land 77, masking the 74/79 the
    rows below are actually about. A path *through* a file fails for everyone,
    root included. It also has to be ENOTDIR rather than a merely absent
    parent — every write site here calls `create_dir_all` on the parent first,
    so an absent one is created and the command succeeds.
    """
    blocker = tmp_path / "not-a-directory"
    blocker.write_text("a regular file standing where a directory would be\n")
    return blocker / "sub" / name


def _published(ocx: OcxRunner, repo: str, tmp_path: Path) -> tuple[Path, Path]:
    """Publishes a package and returns ``(bundle, resolved sidecar)``.

    The bundle name is `make_package`'s documented convention, not a glob —
    asserted, so a rename in the helper reds here instead of silently picking
    a different file.
    """
    make_package(ocx, repo, "1.0.0", tmp_path)
    bundle = tmp_path / f"bundle-{repo.replace('/', '_')}-1.0.0.tar.xz"
    assert bundle.exists(), f"make_package's bundle convention moved: no {bundle}"
    return bundle, resolved_metadata_path(bundle)


def _sign_identity_token_file(
    ocx: OcxRunner, repo: str, tmp_path: Path
) -> tuple[list[str], str]:
    path = _missing(tmp_path)
    # The needle is the flag, not the path: `--identity-token-file` redacts the
    # location of a credential (CWE-209) and prints the basename only.
    return (
        ["package", "sign", "--identity-token-file", str(path), f"{repo}:1.0.0"],
        "--identity-token-file",
    )


def _sign_tags_file(ocx: OcxRunner, repo: str, tmp_path: Path) -> tuple[list[str], str]:
    path = _missing(tmp_path)
    return (["package", "sign", "--tags-file", str(path), repo], str(path))


def _attest_tags_file(ocx: OcxRunner, repo: str, tmp_path: Path) -> tuple[list[str], str]:
    predicate = tmp_path / "predicate.json"
    predicate.write_text("{}")
    path = _missing(tmp_path)
    return (
        [
            "package", "attest",
            "--tags-file", str(path),
            "--predicate", str(predicate),
            "--type", "custom",
            repo,
        ],
        str(path),
    )


def _push_tags_file(ocx: OcxRunner, repo: str, tmp_path: Path) -> tuple[list[str], str]:
    """`push --tags-file` is a *write* target, so the row is the write shape.

    A missing file is not a failure there by design (`append_to_tags_file`
    treats NotFound as an empty set and creates it), and the write only happens
    once the push has landed — so this row publishes for real first.
    """
    bundle, sidecar = _published(ocx, repo, tmp_path)
    path = _unwritable(tmp_path, "pushed-tags.txt")
    return (
        [
            "package", "push",
            "-p", "linux/amd64",
            "-m", str(sidecar),
            "-i", f"{ocx.registry}/{repo}:1.0.1",
            "--tags-file", str(path),
            str(bundle),
        ],
        str(path),
    )


def _announce_tags_file(
    ocx: OcxRunner, repo: str, tmp_path: Path
) -> tuple[list[str], str]:
    path = _missing(tmp_path)
    # `--out` keeps the run off the forge: no credential, no pull request.
    return (
        [
            "package", "announce",
            "--tags-file", str(path),
            "--package", "acme/widget",
            "--out", str(tmp_path),
        ],
        str(path),
    )


def _describe_readme(ocx: OcxRunner, repo: str, tmp_path: Path) -> tuple[list[str], str]:
    path = _missing(tmp_path)
    return (
        ["package", "describe", "--readme", str(path), f"{repo}:1.0.0"],
        str(path),
    )


def _patch_publish_descriptor(
    ocx: OcxRunner, repo: str, tmp_path: Path
) -> tuple[list[str], str]:
    path = _missing(tmp_path)
    # `--registry` is not decoration: without it the command exits 64 on an
    # unconfigured patch registry before the descriptor is ever opened.
    return (
        [
            "patch", "publish",
            "--descriptor", str(path),
            "--registry", f"{ocx.registry}/patches",
            "--global",
        ],
        str(path),
    )


def _patch_test_descriptor(
    ocx: OcxRunner, repo: str, tmp_path: Path
) -> tuple[list[str], str]:
    path = _missing(tmp_path)
    return (
        [
            "patch", "test",
            "--descriptor", str(path),
            "--registry", f"{ocx.registry}/patches",
            f"{repo}:1.0.0",
        ],
        str(path),
    )


def _create_output(ocx: OcxRunner, repo: str, tmp_path: Path) -> tuple[list[str], str]:
    content = tmp_path / "content"
    (content / "bin").mkdir(parents=True)
    (content / "bin" / "hello").write_text("#!/bin/sh\necho hi\n")
    path = _unwritable(tmp_path, "bundle.tar.zst")
    # "Not a directory" survives a fix: `file_error` wraps the same io::Error,
    # so the needle matches both the current bare message and a typed one.
    return (
        ["package", "create", "--output", str(path), str(content)],
        "Not a directory",
    )


def _info_save_readme(ocx: OcxRunner, repo: str, tmp_path: Path) -> tuple[list[str], str]:
    """`info --save-readme` writes nothing unless the repository has a README."""
    readme = tmp_path / "README.md"
    readme.write_text("# a description worth saving\n")
    ocx.plain("package", "describe", "--readme", str(readme), f"{repo}:1.0.0")
    path = _unwritable(tmp_path, "saved-readme.md")
    return (["package", "info", "--save-readme", str(path), repo], str(path.parent))


def _cascade_repair_announce_tags(
    ocx: OcxRunner, repo: str, tmp_path: Path
) -> tuple[list[str], str]:
    """`--announce-tags` is written on a dry run too, so no repair is needed."""
    _published(ocx, repo, tmp_path)
    path = _unwritable(tmp_path, "announce-tags.txt")
    return (
        ["package", "cascade", "repair", "--dry-run", "--announce-tags", str(path), repo],
        str(path),
    )


def _verify_malformed_ocx_toml(
    ocx: OcxRunner, repo: str, tmp_path: Path
) -> tuple[list[str], str]:
    """A malformed file, not a missing one — the other half of the invariant.

    Read from the project `ocx.toml` in the working directory (every row runs
    with cwd=tmp_path), and read before the package is resolved, which is why
    an unpublished identifier still reaches the parse.
    """
    (tmp_path / "ocx.toml").write_text("[[trust.policy]]\nscope = 42\n")
    return (["package", "verify", f"{repo}:1.0.0"], "trust.policy")


# --- controls: these three already passed before the typing sweep, so a green
# --- row above cannot be green merely because the assertion is toothless.


def _control_config_test_missing(
    ocx: OcxRunner, repo: str, tmp_path: Path
) -> tuple[list[str], str]:
    path = _missing(tmp_path)
    return (["config", "test", str(path)], str(path))


def _control_attest_predicate(
    ocx: OcxRunner, repo: str, tmp_path: Path
) -> tuple[list[str], str]:
    path = _missing(tmp_path)
    return (
        ["package", "attest", "--predicate", str(path), "--type", "custom", f"{repo}:1.0.0"],
        str(path),
    )


def _control_sign_key_file(
    ocx: OcxRunner, repo: str, tmp_path: Path
) -> tuple[list[str], str]:
    path = _missing(tmp_path)
    return (
        ["package", "sign", "--key", str(path), f"{repo}:1.0.0"],
        "cannot read key material",
    )


# --- the other two thirds of `--key <missing>`. The sign row above is a
# --- control that predates the typing sweep; on its own it could not see that
# --- verify answered 78 for the identical flag and value, because 74 and 78
# --- were both inside the old allowlist. One flag, one value, one code.


def _attest_key_file(ocx: OcxRunner, repo: str, tmp_path: Path) -> tuple[list[str], str]:
    predicate = tmp_path / "predicate.json"
    predicate.write_text("{}")
    path = _missing(tmp_path)
    return (
        [
            "package", "attest",
            "--key", str(path),
            "--predicate", str(predicate),
            "--type", "custom",
            f"{repo}:1.0.0",
        ],
        "cannot read key material",
    )


def _verify_key_file(ocx: OcxRunner, repo: str, tmp_path: Path) -> tuple[list[str], str]:
    """The key is compiled before the target is resolved, so an unpublished
    identifier still reaches the read — the same shape the sign row relies on.

    The needle is the path itself: verify's refusal names the file, and that is
    what proves the run got as far as opening it rather than failing on the
    absent package.
    """
    path = _missing(tmp_path)
    return (
        ["package", "verify", "--key", str(path), f"{repo}:1.0.0"],
        str(path),
    )


@pytest.mark.parametrize(
    ("build", "expected"),
    [
        pytest.param(_sign_identity_token_file, 74, id="sign--identity-token-file"),
        pytest.param(_sign_tags_file, 74, id="sign--tags-file"),
        pytest.param(_attest_tags_file, 74, id="attest--tags-file"),
        pytest.param(_push_tags_file, 74, id="push--tags-file"),
        pytest.param(_announce_tags_file, 74, id="announce--tags-file"),
        pytest.param(_describe_readme, 74, id="describe--readme"),
        pytest.param(_patch_publish_descriptor, 74, id="patch-publish--descriptor"),
        pytest.param(_patch_test_descriptor, 74, id="patch-test--descriptor"),
        pytest.param(_create_output, 74, id="create--output"),
        pytest.param(_info_save_readme, 74, id="info--save-readme"),
        pytest.param(_cascade_repair_announce_tags, 74, id="cascade-repair--announce-tags"),
        pytest.param(_verify_malformed_ocx_toml, 78, id="verify-malformed-ocx-toml"),
        pytest.param(_control_config_test_missing, 79, id="control-config-test-missing"),
        pytest.param(_control_attest_predicate, 74, id="control-attest--predicate"),
        pytest.param(_control_sign_key_file, 74, id="control-sign--key-file"),
        pytest.param(_attest_key_file, 74, id="attest--key-file"),
        pytest.param(_verify_key_file, 74, id="verify--key-file"),
    ],
)
def test_an_operator_supplied_path_never_exits_internal(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, build: CaseBuilder, expected: int
) -> None:
    """An unusable operator-supplied path exits with its one documented code.

    Three assertions, three different failures caught: `needle` proves the run
    reached the file, `!= 1` names the untyped-`io::Error` regression this table
    was written for, and `== expected` is the contract. See the block comment
    above `_ALLOWED_CODES` for why every row is pinned and what the allowlist
    still does.
    """
    assert expected in _ALLOWED_CODES, (
        f"row pinned to {expected}, which is not a code this family may answer: "
        f"widen _ALLOWED_CODES deliberately, or fix the pin"
    )
    argv, needle = build(ocx, unique_repo, tmp_path)
    result = subprocess.run(
        [str(ocx.binary), *argv],
        capture_output=True,
        text=True,
        env=ocx.env,
        cwd=str(tmp_path),
    )

    assert needle in result.stderr, (
        f"the run failed before it ever reached the file, so this row proves nothing: "
        f"expected {needle!r} in stderr\n"
        f"argv: {argv}\nreturncode: {result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert result.returncode != 1, (
        f"an unusable operator-supplied path was reported as exit 1 `internal` — "
        f"an untyped io::Error reached the exit-code envelope\n"
        f"argv: {argv}\nreturncode: {result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert result.returncode == expected, (
        f"expected exactly {expected}, got {result.returncode}\n"
        f"argv: {argv}\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
