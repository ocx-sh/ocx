#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Sign and attest versions that were published before `release.yml` did.

    scripts/backfill_release.py --package REF --identity SAN --issuer URL [--dry-run] [TAG...]

For every git tag `vX.Y.Z` (the arguments, else every version tag of the checkout) and every
platform of the package tag `X.Y.Z`: a signature by IDENTITY is added when `ocx package verify`
finds none, and a CycloneDX attestation by IDENTITY when `verify --attestation --type cyclonedx`
finds none. Each is judged on its own, so a platform that only lacks its SBOM gets only that, and
a second run changes nothing. A tag is never moved: both add referrers to the digest the tag
already names. The tag is resolved to each platform's manifest digest once, and every check and
write then names `REF@DIGEST`, so a tag that moves mid-run cannot send a signature to a manifest
that was never checked. `--dry-run` reports what would change and writes nothing.

The SBOM of a version is generated from the source at its git tag by `generate_sbom`, the one
function a repository with another build overrides; it returns the path of the CycloneDX JSON.

Exit codes: 0 every row done or skipped, 1 a row failed (a platform with no build triple fails its
row and the others continue) or a version named on the command line has no package, 64 a bad argument.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

# `verify` exits 79 when the target carries no signature or attestation and 77 when the ones it carries
# are not by the expected identity; both mean "this identity has not signed it yet". Any other code
# (network, credentials, usage) is a failure, never a licence to sign.
ABSENT = {77, 79}
TAG = re.compile(r"^v\d+\.\d+\.\d+(?:-[0-9A-Za-z]+)?$")

# OCI platform -> the Rust triple the version shipped for it; Windows archives are named `msvc`.
TRIPLE = {
    "linux/amd64": "x86_64-unknown-linux-musl",
    "linux/arm64": "aarch64-unknown-linux-musl",
    "darwin/amd64": "x86_64-apple-darwin",
    "darwin/arm64": "aarch64-apple-darwin",
    "windows/amd64": "x86_64-pc-windows-msvc",
    "windows/arm64": "aarch64-pc-windows-msvc",
}

Run = Callable[[list[str]], tuple[int, str, str]]


class BackfillError(Exception):
    """A step failed in a way that must not be read as "already done"."""


def run_ocx(argv: list[str]) -> tuple[int, str, str]:
    done = subprocess.run(
        ["ocx", *argv], capture_output=True, encoding="utf-8", check=False
    )
    return done.returncode, done.stdout, done.stderr


@dataclass(frozen=True)
class Row:
    tag: str
    platform: str
    signature: str
    sbom: str
    digest: str = "-"


def version_tags(root: Path) -> list[str]:
    done = subprocess.run(
        ["git", "tag", "--list", "v[0-9]*"],
        cwd=root,
        capture_output=True,
        encoding="utf-8",
        check=True,
    )
    return sorted(t for t in done.stdout.split() if TAG.match(t))


def platforms_of(run: Run, ref: str) -> dict[str, str]:
    """Platform -> manifest digest of the index `ref` names; empty when the package has no such tag."""
    code, out, err = run(["--remote", "--format", "json", "package", "inspect", ref])
    if code == 79:
        return {}
    if code != 0:
        raise BackfillError(f"inspecting {ref} exited {code}: {err.strip()[:200]}")
    found: dict[str, str] = {}
    for entry in json.loads(out).get("packages", []):
        for candidate in entry.get("candidates") or []:
            platform = candidate["platform"]
            name = f"{platform['os']}/{platform['architecture']}"
            if name in found:
                raise BackfillError(
                    f"{ref} lists {name} twice; the platform is ambiguous"
                )
            found[name] = candidate["digest"]
    return dict(sorted(found.items()))


def has(run: Run, subject: str, identity: str, issuer: str, attestation: bool) -> bool:
    """Whether IDENTITY has signed (or attested) `subject`, a digest-pinned platform manifest."""
    argv = [
        "package",
        "verify",
        "--certificate-identity",
        identity,
        "--certificate-oidc-issuer",
        issuer,
    ]
    if attestation:
        argv += ["--attestation", "--type", "cyclonedx"]
    code, _, err = run([*argv, subject])
    if code == 0:
        return True
    if code in ABSENT:
        return False
    raise BackfillError(f"verifying {subject} exited {code}: {err.strip()[:200]}")


def triple_of(platform: str) -> str:
    try:
        return TRIPLE[platform]
    except KeyError:
        raise BackfillError(
            f"no build triple for platform {platform}; add it to TRIPLE"
        ) from None


def generate_sbom(root: Path, version: str, triple: str, out_dir: Path) -> Path:
    """The CycloneDX JSON of `v<version>`'s `ocx` for `triple`, built from that tag's own source."""
    source = out_dir / f"source-v{version}"
    if not source.exists():
        subprocess.run(
            [
                "git",
                "worktree",
                "add",
                "--detach",
                str(source),
                f"refs/tags/v{version}",
            ],
            cwd=root,
            check=True,
        )
        subprocess.run(
            ["git", "submodule", "update", "--init", "--recursive"],
            cwd=source,
            check=True,
        )
    stem = f"ocx-{triple}"
    subprocess.run(
        [
            "ocx",
            "exec",
            "-g",
            "docs",
            "--",
            "cargo",
            "cyclonedx",
            "--format",
            "json",
            "--target",
            triple,
            "--manifest-path",
            str(source / "crates" / "ocx_cli" / "Cargo.toml"),
            "--override-filename",
            f"{stem}.cdx",
        ],
        # The current checkout's pinned tool: an old tag's ocx.toml may not declare it in this group.
        cwd=root,
        check=True,
    )
    return source / "crates" / "ocx_cli" / f"{stem}.cdx.json"


def backfill_tag(
    run: Run,
    package: str,
    tag: str,
    identity: str,
    issuer: str,
    dry_run: bool,
    sbom: Callable[[str, str], Path],
) -> list[Row]:
    version = tag.removeprefix("v")
    ref = f"{package}:{version}"
    digests = platforms_of(run, ref)
    if not digests:
        return [Row(tag, "-", "no package", "no package")]
    rows = []
    for platform, digest in digests.items():
        # The digest, never the tag: `-p` has nothing to narrow on a single manifest.
        subject = f"{ref}@{digest}"
        if has(run, subject, identity, issuer, attestation=False):
            signature = "skipped"
        elif dry_run:
            signature = "would sign"
        else:
            code, _, err = run(["package", "sign", "--no-tty", subject])
            if code != 0:
                raise BackfillError(
                    f"signing {subject} ({platform}) exited {code}: {err.strip()[:200]}"
                )
            signature = "signed"
        if has(run, subject, identity, issuer, attestation=True):
            attested = "skipped"
        elif dry_run:
            attested = "would attest"
        else:
            try:
                predicate = sbom(version, platform)
            except BackfillError as error:
                print(f"backfill_release: {tag} ({platform}): {error}", file=sys.stderr)
                rows.append(Row(tag, platform, signature, "FAILED", digest))
                continue
            code, _, err = run(
                [
                    "package",
                    "attest",
                    "--no-tty",
                    "--type",
                    "cyclonedx",
                    "--predicate",
                    str(predicate),
                    subject,
                ]
            )
            if code != 0:
                raise BackfillError(
                    f"attesting {subject} ({platform}) exited {code}: {err.strip()[:200]}"
                )
            attested = "attested"
        rows.append(Row(tag, platform, signature, attested, digest))
    return rows


def table(rows: list[Row]) -> str:
    lines = [
        "| tag | platform | digest | signature | sbom |",
        "| --- | --- | --- | --- | --- |",
    ]
    lines += [
        f"| {r.tag} | {r.platform} | {r.digest} | {r.signature} | {r.sbom} |"
        for r in rows
    ]
    return "\n".join(lines)


def main(argv: list[str] | None = None, run: Run = run_ocx) -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument(
        "--package",
        required=True,
        help="the package, without a tag (registry/namespace/name)",
    )
    parser.add_argument(
        "--identity", required=True, help="the certificate identity this run signs as"
    )
    parser.add_argument(
        "--issuer", required=True, help="the OIDC issuer of that identity"
    )
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument(
        "tags",
        nargs="*",
        help="git tags (vX.Y.Z); every version tag of the checkout when none",
    )
    args = parser.parse_args(argv)
    bad = [t for t in args.tags if not TAG.match(t)]
    if bad:
        print(f"backfill_release: not a version tag: {bad}", file=sys.stderr)
        return 64
    root = Path.cwd()
    tags = args.tags or version_tags(root)
    if not tags:
        print("backfill_release: no version tag to backfill", file=sys.stderr)
        return 64
    rows: list[Row] = []
    failed = False
    with tempfile.TemporaryDirectory() as scratch:
        for tag in tags:
            try:
                rows += backfill_tag(
                    run,
                    args.package,
                    tag,
                    args.identity,
                    args.issuer,
                    args.dry_run,
                    lambda version, platform: generate_sbom(
                        root, version, triple_of(platform), Path(scratch)
                    ),
                )
            except (BackfillError, subprocess.CalledProcessError) as error:
                print(f"backfill_release: {tag}: {error}", file=sys.stderr)
                rows.append(Row(tag, "-", "FAILED", "FAILED"))
                failed = True
    failed = failed or any("FAILED" in (r.signature, r.sbom) for r in rows)
    # A version named on the command line that has no package fails the run; discovery meets git tags that never had one.
    failed = failed or (bool(args.tags) and any(r.signature == "no package" for r in rows))
    report = table(rows)
    print(report)
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as handle:
            handle.write(report + "\n")
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
