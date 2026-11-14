"""`backfill_release.py`'s proofs, against a fake `ocx`: what it adds, what it skips, and what it refuses to guess."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path

import backfill_release as bf
import pytest
from cdx_stub import run_cyclonedx

IDENTITY = (
    "https://github.com/ocx-sh/ocx/.github/workflows/backfill.yml@refs/heads/main"
)
ISSUER = "https://token.actions.githubusercontent.com"
PACKAGE = "ghcr.io/ocx-sh/ocx/cli"


class FakeOcx:
    """A registry of `(ref, platform) -> {"sig", "sbom"}` held by IDENTITY; every call is recorded.

    `package inspect` answers each platform's manifest digest; verify, sign and attest only answer for
    that digest-pinned subject, and refuse `-p` on it as ocx does for a reference with nothing to narrow.
    """

    def __init__(
        self,
        platforms: dict[str, list[str]],
        held: dict[tuple[str, str], set[str]] | None = None,
    ) -> None:
        self.platforms = platforms
        self.held = held or {}
        self.calls: list[list[str]] = []
        self.verify_exit: int | None = None
        self.subjects = {
            f"{ref}@{digest_of(ref, platform)}": (ref, platform)
            for ref, listed in platforms.items()
            for platform in listed
        }

    def __call__(self, argv: list[str]) -> tuple[int, str, str]:
        self.calls.append(argv)
        ref = argv[-1]
        if argv[:2] == ["--remote", "--format"]:
            listed = self.platforms.get(ref)
            if listed is None:
                return 79, "", "not found"
            candidates = [
                {
                    "digest": digest_of(ref, platform),
                    "platform": dict(zip(("os", "architecture"), platform.split("/"))),
                }
                for platform in listed
            ]
            return (
                0,
                json.dumps({"packages": [{"name": ref, "candidates": candidates}]}),
                "",
            )
        verb = argv[1]
        if ref not in self.subjects:
            return 64, "", f"{ref} is not a digest-pinned platform manifest"
        if "-p" in argv:
            return 64, "", "a platform narrows an index and this is a single manifest"
        key = self.subjects[ref]
        if verb == "verify":
            if self.verify_exit is not None:
                return self.verify_exit, "", "boom"
            kind = "sbom" if "--attestation" in argv else "sig"
            return (
                (0, "", "")
                if kind in self.held.get(key, set())
                else (79, "", "no signatures found")
            )
        kind = "sbom" if verb == "attest" else "sig"
        self.held.setdefault(key, set()).add(kind)
        return 0, "", ""

    def writes(self) -> list[str]:
        return [
            c[1]
            for c in self.calls
            if len(c) > 1 and c[0] == "package" and c[1] in {"sign", "attest"}
        ]

    def inspects(self) -> list[list[str]]:
        return [c for c in self.calls if c[:2] == ["--remote", "--format"]]


def digest_of(ref: str, platform: str) -> str:
    return "sha256:" + hashlib.sha256(f"{ref} {platform}".encode()).hexdigest()


def tag_rows(fake: FakeOcx, dry_run: bool = False) -> list[bf.Row]:
    return bf.backfill_tag(
        fake,
        PACKAGE,
        "v0.6.6",
        IDENTITY,
        ISSUER,
        dry_run,
        lambda version, platform: Path("sbom.json"),
    )


def test_an_unsigned_platform_gets_its_signature_and_its_sbom() -> None:
    fake = FakeOcx({f"{PACKAGE}:0.6.6": ["linux/amd64", "darwin/arm64"]})
    rows = tag_rows(fake)
    assert [(r.platform, r.signature, r.sbom) for r in rows] == [
        ("darwin/arm64", "signed", "attested"),
        ("linux/amd64", "signed", "attested"),
    ]


def test_a_second_run_writes_nothing() -> None:
    fake = FakeOcx({f"{PACKAGE}:0.6.6": ["linux/amd64"]})
    tag_rows(fake)
    fake.calls.clear()
    rows = tag_rows(fake)
    assert fake.writes() == []
    assert [(r.signature, r.sbom) for r in rows] == [("skipped", "skipped")]


def test_each_half_is_judged_on_its_own() -> None:
    ref = f"{PACKAGE}:0.6.6"
    fake = FakeOcx({ref: ["linux/amd64"]}, {(ref, "linux/amd64"): {"sig"}})
    rows = tag_rows(fake)
    assert fake.writes() == ["attest"]
    assert (rows[0].signature, rows[0].sbom) == ("skipped", "attested")


def test_a_platform_the_release_identity_signed_is_skipped_and_never_signed_again() -> None:
    release = "https://github.com/ocx-sh/ocx/.github/workflows/release.yml@refs/heads/main"
    ref = f"{PACKAGE}:0.6.6"
    fake = FakeOcx({ref: ["linux/amd64"]}, {(ref, "linux/amd64"): {"sig", "sbom"}})
    real = fake.__call__

    def only_release(argv: list[str]) -> tuple[int, str, str]:
        if "verify" in argv and IDENTITY in argv:
            fake.calls.append(argv)
            return 79, "", "no signatures found"
        return real(argv)

    rows = bf.backfill_tag(
        only_release, PACKAGE, "v0.6.6", IDENTITY, ISSUER, False,
        lambda version, platform: Path("sbom.json"), (release,),
    )
    assert fake.writes() == []
    assert [(r.signature, r.sbom) for r in rows] == [("skipped", "skipped")]


def test_a_dry_run_reports_and_writes_nothing() -> None:
    fake = FakeOcx({f"{PACKAGE}:0.6.6": ["linux/amd64"]})
    rows = tag_rows(fake, dry_run=True)
    assert fake.writes() == []
    assert (rows[0].signature, rows[0].sbom) == ("would sign", "would attest")


def test_a_verify_failure_that_is_not_absence_never_licenses_a_signature() -> None:
    fake = FakeOcx({f"{PACKAGE}:0.6.6": ["linux/amd64"]})
    fake.verify_exit = 69
    with pytest.raises(bf.BackfillError, match="exited 69"):
        tag_rows(fake)
    assert fake.writes() == []


def test_a_version_with_no_package_is_reported_not_failed() -> None:
    rows = tag_rows(FakeOcx({}))
    assert [(r.platform, r.signature) for r in rows] == [("-", "no package")]


def test_main_refuses_a_ref_that_is_not_a_version_tag() -> None:
    assert (
        bf.main(
            ["--package", PACKAGE, "--identity", IDENTITY, "--issuer", ISSUER, "main"],
            run=FakeOcx({}),
        )
        == 64
    )


def test_main_fails_a_requested_version_that_has_no_package(capsys: pytest.CaptureFixture[str]) -> None:
    """An explicit tag that names nothing is a typo or a lost package; only discovery may skip it."""
    code = bf.main(
        ["--package", PACKAGE, "--identity", IDENTITY, "--issuer", ISSUER, "v9.9.9"],
        run=FakeOcx({}),
    )
    assert code == 1
    assert "no package" in capsys.readouterr().out


def test_main_exits_1_when_a_row_fails_and_still_prints_the_table(
    capsys: pytest.CaptureFixture[str],
) -> None:
    fake = FakeOcx({f"{PACKAGE}:0.6.6": ["linux/amd64"]})
    fake.verify_exit = 69
    code = bf.main(
        ["--package", PACKAGE, "--identity", IDENTITY, "--issuer", ISSUER, "v0.6.6"],
        run=fake,
    )
    assert code == 1
    assert "FAILED" in capsys.readouterr().out


def test_every_check_and_write_targets_the_digest_resolved_once() -> None:
    ref = f"{PACKAGE}:0.6.6"
    fake = FakeOcx({ref: ["linux/amd64", "darwin/arm64"]})
    rows = tag_rows(fake)
    assert len(fake.inspects()) == 1, (
        "each tag's platforms and digests are resolved once"
    )
    acted = [c for c in fake.calls if c[:1] == ["package"]]
    assert len(acted) == 2 * 4, (
        "verify and write, for the signature and the sbom, on each platform"
    )
    for call in acted:
        assert call[-1] in fake.subjects, f"{call} does not name a resolved digest"
        assert "-p" not in call, f"{call} narrows a digest-pinned manifest"
    assert {r.platform: r.digest for r in rows} == {
        "linux/amd64": digest_of(ref, "linux/amd64"),
        "darwin/arm64": digest_of(ref, "darwin/arm64"),
    }
    assert digest_of(ref, "linux/amd64") in bf.table(rows)


def test_a_platform_with_no_build_triple_fails_its_row_while_the_others_continue(
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    ref = f"{PACKAGE}:0.6.6"
    fake = FakeOcx({ref: ["linux/riscv64", "linux/amd64"]})
    monkeypatch.setattr(bf, "generate_sbom", lambda *_: Path("sbom.json"))
    code = bf.main(
        ["--package", PACKAGE, "--identity", IDENTITY, "--issuer", ISSUER, "v0.6.6"],
        run=fake,
    )
    out = capsys.readouterr()
    assert code == 1, "an unknown platform fails the run at the end"
    rows = {
        line.split("|")[2].strip(): line
        for line in out.out.splitlines()
        if line.startswith("| v")
    }
    assert "FAILED" in rows["linux/riscv64"]
    assert "attested" in rows["linux/amd64"], "the other platform still gets its sbom"
    assert "linux/riscv64" in out.err and "no build triple" in out.err


def test_generate_sbom_runs_the_current_toolchain_against_the_old_checkout(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """An old tag's ocx.toml may not declare cargo-cyclonedx in the group the release uses."""
    calls: list[tuple[list[str], Path]] = []

    def stub(argv: list[str], cwd: Path, check: bool) -> None:
        calls.append((argv, cwd))
        if argv[0] == "ocx":
            run_cyclonedx(argv)

    monkeypatch.setattr(bf.subprocess, "run", stub)
    (tmp_path / "source-v0.6.6").mkdir()
    root = tmp_path / "root"
    out = bf.generate_sbom(root, "0.6.6", "x86_64-unknown-linux-gnu", tmp_path)
    argv, cwd = calls[-1]
    assert cwd == root, "the tool resolves from the current checkout's ocx.toml"
    assert argv[:4] == ["ocx", "exec", "-g", "docs"]
    manifest = argv[argv.index("--manifest-path") + 1]
    assert manifest == str(tmp_path / "source-v0.6.6/crates/ocx_cli/Cargo.toml")
    assert out == tmp_path / "source-v0.6.6/crates/ocx_cli/ocx-x86_64-unknown-linux-gnu.cdx.json"
    assert out.is_file(), "the returned path is the file the tool wrote"


def test_limit_stops_after_n_tags_that_needed_work_and_counts_the_rest(
    capsys: pytest.CaptureFixture[str],
) -> None:
    held = {(f"{PACKAGE}:0.6.5", "linux/amd64"): {"sig", "sbom"}}
    fake = FakeOcx(
        {f"{PACKAGE}:{v}": ["linux/amd64"] for v in ("0.6.4", "0.6.5", "0.6.6", "0.6.7")},
        held,
    )
    code = bf.main(
        ["--package", PACKAGE, "--identity", IDENTITY, "--issuer", ISSUER, "--limit", "1", "--dry-run",
         "v0.6.5", "v0.6.4", "v0.6.6", "v0.6.7"],
        run=fake,
    )
    out = capsys.readouterr()
    tags = [line.split("|")[1].strip() for line in out.out.splitlines() if line.startswith("| v")]
    assert code == 0
    assert tags == ["v0.6.5", "v0.6.4"], "a tag with nothing to do does not use the budget"
    assert "2 tags remain" in out.err


def test_a_tag_that_never_had_a_package_does_not_spend_the_limit(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.setattr(bf, "version_tags", lambda root: ["v0.6.3", "v0.6.4", "v0.6.5"])
    fake = FakeOcx({f"{PACKAGE}:{v}": ["linux/amd64"] for v in ("0.6.4", "0.6.5")})
    code = bf.main(
        ["--package", PACKAGE, "--identity", IDENTITY, "--issuer", ISSUER, "--limit", "1", "--dry-run"],
        run=fake,
    )
    out = capsys.readouterr()
    tags = [line.split("|")[1].strip() for line in out.out.splitlines() if line.startswith("| v")]
    assert code == 0
    assert tags == ["v0.6.3", "v0.6.4"], "the no-package tag is skipped and the next tag with work is processed"
    assert "1 tags remain" in out.err


@pytest.mark.parametrize("limit", ["0", "-1"])
def test_a_limit_below_one_is_a_bad_argument(limit: str) -> None:
    argv = ["--package", PACKAGE, "--identity", IDENTITY, "--issuer", ISSUER, "--limit", limit, "v0.6.6"]
    assert bf.main(argv, run=FakeOcx({})) == 64
