# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Standards compliance for the execution record's ``packages[]`` block.

``ocx_lib``'s execution record (issue #214,
``.claude/artifacts/adr_exec_resolution_record.md``) deliberately borrows the
shape of two open standards for every ``packages[]`` entry rather than
inventing its own:

* **in-toto `ResourceDescriptor`** — the entry envelope itself (``name`` /
  ``uri`` / ``digest`` / ``annotations``), so ``packages`` can drop straight
  into a SLSA ``resolvedDependencies`` list.
* **Package URL / ECMA-427**, `pkg:oci` type — the ``uri`` field.

This module proves compliance against each standard's own reference
implementation — never a hand-rolled shape check, which would only prove that
ocx agrees with itself. Reference implementations:

* ``in-toto-attestation`` (PyPI, Apache-2.0) — the in-toto project's own
  protobuf-backed Python bindings. Parsing goes through
  ``google.protobuf.json_format.Parse(..., ignore_unknown_fields=False)``,
  which rejects any key the ``ResourceDescriptor``/``Statement`` proto does
  not define — the property a hand-rolled ``dict`` key check cannot give: a
  permissive reader would happily accept a foreign key forever.
* ``packageurl-python`` (PyPI, MIT) — the purl-spec project's own reference
  parser/serializer (`PackageURL.from_string` / `.to_string()`).

Coverage
--------
Every descriptor shape the record producer emits gets its own test, each
ending in a call to ``_assert_packages_are_standards_compliant(record,
frame)`` — a shared check, not per-frame reimplementations of it, since the
underlying ``ResourceDescriptor``/purl shape is identical regardless of which
frame produced the entry. ``frame`` is a human label threaded into every
failure message so a break names both the invocation shape and the specific
entry, standing in for `@pytest.mark.parametrize` (which cannot work here —
each shape needs its own fixture setup, produced only at test run time, not
at collection time).

The two positive checks (`_validate_resource_descriptor`, `_validate_purl`)
plus the DigestSet check are proven to discriminate exactly once each
(`test_foreign_key_on_a_package_entry_is_rejected`,
`test_a_transport_prefixed_digest_fails_the_digest_set_check`) — the property
under test (a foreign key / a malformed digest is rejected) is a fact about
the reference implementation and this module's own DigestSet assertion, not
about any particular frame, so proving it once rather than once per frame
avoids duplicating the same mutation proof for no new evidence. The SLSA lift
gets its own pair of negative cases, since a `Statement` is a materially
different proto with its own rejection surface.

SLSA lift
---------
`adr_exec_resolution_record.md` claims `packages[]` "lifts into SLSA
`resolvedDependencies` without a translator" (D5). Proven here by embedding a
real record's `packages[]` **by reference** — asserted via `is`, not `==` —
into a hand-built SLSA v1 provenance predicate, then validating both the
`Statement` envelope and the predicate body strictly. `in-toto-attestation`
0.9.3 does ship a real SLSA provenance predicate binding
(`in_toto_attestation.predicates.provenance.v1.provenance_pb2`, confirmed by
`test_in_toto_attestation_ships_a_slsa_provenance_predicate_binding`) whose
`BuildDefinition.resolved_dependencies` field is itself typed as a
`ResourceDescriptor`, so one strict parse of the predicate validates the
whole tree — envelope and every embedded package entry — in one pass, no
opaque-`Struct` fallback needed.

ECS / OTel vocabulary
---------------------
`process.*`/`host.*`/`os.type` fields are validated against ECS/OTel spec
subsets vendored under `test/specs/` (provenance in `test/specs/SOURCES.md`)
— this agent has no network access, so the raw upstream content was fetched
by the session lead via `WebFetch` and relayed. `process.arch` and `os.type`
are each checked against OTel, never ECS: `process.arch` because ECS has no
architecture field on `process` at all (the ADR's own documented choice), and
`os.type` because ECS documents it as reused under `host.os.type` (never
top-level) and flags its own values as conflicting with OTel's — see
`test/specs/SOURCES.md`.
"""

from __future__ import annotations

import copy
import importlib.metadata
import json
import sys
from pathlib import Path

import pytest
import yaml
from google.protobuf import json_format
from in_toto_attestation.v1.resource_descriptor import ResourceDescriptor
from in_toto_attestation.v1.statement import Statement
from packageurl import PackageURL

from src.helpers import make_package, make_package_with_entrypoints, write_ocx_toml
from src.registry import fetch_platform_manifest_digest
from src.runner import OcxRunner, PackageInfo
from src.shell_eval import run_after_sourcing
from tests.test_execution_records import (
    BARE_SHA256,
    EXIT_SUCCESS,
    _entries_with_role,
    _one_record,
    _package_root,
    _project_with_tool,
    _root_entry,
    _run_in,
    _sink,
)

# ---------------------------------------------------------------------------
# Reference-implementation bindings
# ---------------------------------------------------------------------------


def _validate_resource_descriptor(entry: dict) -> None:
    """Strictly parse ``entry`` as an in-toto ``ResourceDescriptor``.

    ``ignore_unknown_fields=False`` is the whole point: it makes a foreign key
    on the entry a parse error rather than a silently-dropped extra, which is
    exactly the failure mode a hand-rolled ``dict``-shape assertion cannot
    catch. ``validate()`` is the reference implementation's own post-parse
    check (currently: at least one of name/uri/digest must be set).
    """
    descriptor = ResourceDescriptor()
    json_format.Parse(json.dumps(entry), descriptor.pb, ignore_unknown_fields=False)
    descriptor.validate()


def _assert_digest_set_is_bare_lowercase_hex(entry: dict) -> None:
    """Every ``digest`` value is bare lowercase hex — in-toto's DigestSet rule.

    Not covered by the reference implementation's own ``validate()`` (it only
    checks presence, never the value shape), so this is a project-level
    assertion against the DigestSet convention rather than a library call —
    the ADR's format rule 1, restated as a standards obligation rather than an
    ocx-internal one.
    """
    for algorithm, value in entry["digest"].items():
        assert value == value.lower() and all(c in "0123456789abcdef" for c in value), (
            f"digest[{algorithm!r}] must be bare lowercase hex, no transport "
            f"prefix (in-toto DigestSet); got {value!r}"
        )
    if "sha256" in entry["digest"]:
        assert BARE_SHA256.match(entry["digest"]["sha256"]), (
            f"a sha256 DigestSet entry is exactly 64 hex chars; got "
            f"{entry['digest']['sha256']!r}"
        )


def _validate_purl(entry: dict) -> None:
    """Parse ``entry["uri"]`` (when present) with the purl reference impl.

    Skips entries carrying no ``uri`` — a synthetic, identity-less package
    (e.g. a degraded-identity direct launcher invocation) omits it by design
    rather than fabricating one (ADR F5/F12), and that omission is not this
    module's concern.
    """
    uri = entry.get("uri")
    if uri is None:
        return

    parsed = PackageURL.from_string(uri)
    assert parsed.type == "oci", (
        f"an ocx purl always uses the registered 'oci' type; got {parsed.type!r} "
        f"from {uri!r}"
    )
    assert parsed.version == f"sha256:{entry['digest']['sha256']}", (
        "purl version is the digest, algorithm-prefixed and unencoded (format "
        f"rule 2); got {parsed.version!r}, digest={entry['digest']!r}"
    )
    assert parsed.qualifiers.get("repository_url"), (
        f"purl must carry a repository_url qualifier; got {parsed.qualifiers!r} "
        f"from {uri!r}"
    )

    # Round-trip through the reference implementation's OWN serializer —
    # not a literal string comparison. packageurl-python normalizes
    # qualifier order (alphabetical) on every parse, so a purl authored in
    # the ADR's own order (`repository_url`, then `tag`, then `arch`) never
    # equals its own `to_string()` byte-for-byte once it carries two or more
    # qualifiers, through no fault of ours. What is actually true — and
    # actually worth proving — is that the reference implementation is
    # idempotent on our purl: parsing its own rendering yields back an
    # equal `PackageURL`.
    reparsed = PackageURL.from_string(parsed.to_string())
    assert reparsed == parsed, (
        "a purl must round-trip through packageurl-python's own to_string(); "
        f"{parsed!r} -> {parsed.to_string()!r} -> {reparsed!r}"
    )


def _assert_packages_are_standards_compliant(record: dict, frame: str) -> None:
    """Every ``packages[]`` entry in ``record`` is in-toto- and purl-valid.

    ``frame`` names the invocation shape under test (e.g. ``"package exec /
    root from tag"``), threaded into the failure so it names both the frame
    and the specific entry — the parametrization the module docstring
    explains.
    """
    assert record["packages"], f"[{frame}] a record always carries at least the root entry"
    for entry in record["packages"]:
        label = f"{frame} / entry {entry.get('name')!r}"
        try:
            _validate_resource_descriptor(entry)
            _assert_digest_set_is_bare_lowercase_hex(entry)
            _validate_purl(entry)
        except (AssertionError, json_format.ParseError, ValueError) as exc:
            raise AssertionError(f"[{label}] {exc}") from exc


# ---------------------------------------------------------------------------
# Patch-tier companion fixture — copied from
# test_execution_records.py::_publish_companion_for (not imported: it also
# mutates the shared per-host patch config file, which this module's own
# scope should own directly rather than borrow the recipe for).
# ---------------------------------------------------------------------------


def _publish_companion(
    ocx: OcxRunner, base: PackageInfo, companion_repo: str, tmp_path: Path
) -> None:
    """Publish an env-only companion and a per-base rule admitting it."""
    companion = make_package(
        ocx,
        companion_repo,
        "1.0.0",
        tmp_path / "companion",
        bins=[],
        env=[
            {
                "key": "RECORDED_COMPANION_VAR",
                "type": "constant",
                "value": "companion-value",
                "visibility": "interface",
            }
        ],
        cascade=True,
        platform="any",
    )
    config_path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    config_path.write_text(f'[patches]\nregistry = "{ocx.registry}"\nrequired = false\n')
    descriptor = tmp_path / "record_descriptor.json"
    descriptor.write_text(
        json.dumps({"version": 1, "rules": [{"match": "*", "packages": [companion.fq]}]})
    )
    publish = ocx.run(
        "patch", "publish", "--descriptor", str(descriptor), base.fq,
        format=None, check=False,
    )
    assert publish.returncode == EXIT_SUCCESS, (
        f"patch publish must succeed; rc={publish.returncode}\nstderr:\n{publish.stderr}"
    )


# ---------------------------------------------------------------------------
# Coverage — every descriptor shape the record producer emits
# ---------------------------------------------------------------------------


def test_root_resolved_from_a_tag_is_standards_compliant(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """A record for a root resolved from a floating tag binds to both standards.

    ``published_package.short`` (``repo:1.0.0``) is a tag, not a digest — the
    ordinary shape of an ``ocx package exec`` invocation.
    """
    sink = _sink(tmp_path, "standards-root")
    result = ocx.run(
        "package", "exec", "--records-dir", str(sink),
        published_package.short, "--", "hello",
        format=None, check=False,
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"package exec must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    record = _one_record(sink)
    _assert_packages_are_standards_compliant(record, "package exec / root from tag")


def test_dependency_closure_entries_are_standards_compliant(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Both the root and its declared dependency bind to both standards
    (OCI-tier ``package exec`` closure — see the project-tier variant below).
    """
    dependency = make_package(ocx, f"{unique_repo}_dep", "1.0.0", tmp_path / "dep")
    dependency_digest = fetch_platform_manifest_digest(
        ocx.registry, dependency.repo, dependency.tag
    )
    root = make_package(
        ocx,
        f"{unique_repo}_root",
        "1.0.0",
        tmp_path / "root",
        dependencies=[{"identifier": f"{dependency.fq}@{dependency_digest}"}],
    )
    sink = _sink(tmp_path, "standards-closure")

    result = ocx.run(
        "package", "exec", "--records-dir", str(sink),
        root.short, "--", "hello",
        format=None, check=False,
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"exec with a dependency closure must succeed; rc={result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )

    record = _one_record(sink)
    dependencies = _entries_with_role(record, "dependency")
    assert [entry["name"] for entry in dependencies] == [dependency.repo], (
        f"the closure must carry the declared dependency; got {dependencies!r}"
    )
    _assert_packages_are_standards_compliant(record, "package exec / dependency closure")


def test_project_tier_exec_with_dependency_closure_is_standards_compliant(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A project-tier ``ocx exec`` closure (root + dependency) binds to both
    standards, and carries ``sh.ocx.binding``/``sh.ocx.group`` — fields only a
    project binding produces, distinct from the OCI-tier closure above.
    """
    dependency = make_package(ocx, f"{unique_repo}_dep", "1.0.0", tmp_path / "dep")
    dependency_digest = fetch_platform_manifest_digest(
        ocx.registry, dependency.repo, dependency.tag
    )
    root = make_package(
        ocx,
        f"{unique_repo}_root",
        "1.0.0",
        tmp_path / "root",
        dependencies=[{"identifier": f"{dependency.fq}@{dependency_digest}"}],
    )
    project = _project_with_tool(ocx, tmp_path, root)
    sink = _sink(tmp_path, "standards-project-closure")

    result = _run_in(ocx, project, "exec", "--records-dir", str(sink), "--", "hello")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx exec with a dependency closure must succeed; rc={result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )

    record = _one_record(sink)
    root_annotations = _root_entry(record)["annotations"]
    assert "sh.ocx.binding" in root_annotations, (
        f"a project-tier root carries its binding name; got {root_annotations}"
    )
    assert "sh.ocx.group" in root_annotations, (
        f"a project-tier root carries its group; got {root_annotations}"
    )
    dependencies = _entries_with_role(record, "dependency")
    assert [entry["name"] for entry in dependencies] == [dependency.repo], (
        f"the closure must carry the declared dependency; got {dependencies!r}"
    )
    _assert_packages_are_standards_compliant(record, "exec / project-tier dependency closure")


@pytest.mark.skipif(sys.platform == "win32", reason="Unix launcher exec test")
def test_launcher_exec_degraded_frame_is_standards_compliant(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A degraded-identity entry (no ``uri``, ``sh.ocx.identity: synthetic``)
    still validates as a complete in-toto ``ResourceDescriptor`` — ``digest``
    alone is sufficient identity for the spec, which is exactly why ocx can be
    honest about ADR F5/F12 instead of fabricating a purl.
    """
    pkg = make_package_with_entrypoints(
        ocx, unique_repo, tmp_path, entrypoints=["hello"], bins=["hello"]
    )
    ocx.plain("package", "install", "--select", pkg.short)
    pkg_root = _package_root(ocx, pkg.short)
    sink = _sink(tmp_path, "standards-degraded")

    result = ocx.run(
        "launcher", "exec", str(pkg_root), "--", "hello",
        format=None, check=False,
        env_overrides={"OCX_RECORDS_DIR": str(sink)},
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"launcher exec must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    record = _one_record(sink)
    entry = _root_entry(record)
    assert "uri" not in entry, f"a degraded entry omits uri rather than fabricating one; got {entry}"
    assert entry["annotations"].get("sh.ocx.identity") == "synthetic", (
        f"a degraded entry names itself in-band; got {entry['annotations']}"
    )
    _assert_packages_are_standards_compliant(record, "launcher exec / degraded identity")


@pytest.mark.skipif(sys.platform == "win32", reason="the shim producer is POSIX-only in this phase")
def test_launcher_shim_frame_is_standards_compliant(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Both shim invocations — the cold pull and the warm re-entry — bind to
    both standards. Mirrors the cold/warm split in
    ``test_execution_records.py::test_launcher_shim_frame_emits_a_record_and_names_the_pull_only_once``.

    Does not assert ``sh.ocx.composition: "deferred"``: that annotation marks
    a root that composed lazily and still carries no content on disk at
    record-build time, and neither invocation here meets that condition — the
    pull happens inside the cold frame itself, so its own root is fully
    materialized by the time the record is built.
    """
    pkg = make_package(
        ocx,
        unique_repo,
        "1.0.0",
        tmp_path,
        bins=["hello"],
        binaries=["hello"],
        env=[
            {
                "key": "PATH",
                "type": "path",
                "required": True,
                "value": "${installPath}/bin",
                "visibility": "public",
            }
        ],
    )
    project = tmp_path / "lazy-proj"
    project.mkdir()
    write_ocx_toml(project, f'lazy-mode = "always"\n\n[tools]\nhello = "{pkg.fq}"\n')
    lock = _run_in(ocx, project, "lock", "--no-pull")
    assert lock.returncode == EXIT_SUCCESS, (
        f"ocx lock --no-pull failed: rc={lock.returncode}\nstderr:\n{lock.stderr}"
    )
    export = _run_in(ocx, project, "env", "--shell=sh")
    assert export.returncode == EXIT_SUCCESS, f"ocx env --shell=sh failed:\n{export.stderr}"

    def trigger(sink: Path) -> None:
        env = dict(ocx.env)
        env["OCX_BINARY_PIN"] = str(ocx.binary)
        env["OCX_RECORDS_DIR"] = str(sink)
        result = run_after_sourcing(export.stdout, "hello", cwd=project, env=env)
        assert result.returncode == EXIT_SUCCESS, (
            f"the shim trigger failed; rc={result.returncode}\nstderr:\n{result.stderr}"
        )

    cold = _sink(tmp_path, "standards-shim-cold")
    trigger(cold)
    cold_record = _one_record(cold)
    _assert_packages_are_standards_compliant(cold_record, "launcher shim / cold")

    pinned = cold_record["resolution"]["autoInstalled"][0]
    warm = _sink(tmp_path, "standards-shim-warm")
    result = ocx.run(
        "launcher", "shim", pinned, "--", "hello",
        format=None, check=False,
        env_overrides={"OCX_RECORDS_DIR": str(warm)},
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"launcher shim must succeed on a materialized tool; rc={result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )
    warm_record = _one_record(warm)
    _assert_packages_are_standards_compliant(warm_record, "launcher shim / warm")


def test_patched_invocation_companion_entries_are_standards_compliant(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A patch-tier companion entry — live, and under a freeze — binds to both
    standards. Mirrors
    ``test_execution_records.py::test_a_patched_invocation_records_its_companion_and_snapshot``.
    """
    base = make_package(ocx, unique_repo, "1.0.0", tmp_path / "base", cascade=True)
    companion_repo = f"{unique_repo}_companion"
    _publish_companion(ocx, base, companion_repo, tmp_path)

    install = ocx.run("package", "install", base.short, format=None, check=False)
    assert install.returncode == EXIT_SUCCESS, (
        f"install must discover and fetch the companion; rc={install.returncode}\n"
        f"stderr:\n{install.stderr}"
    )

    live_sink = _sink(tmp_path, "standards-companion-live")
    live = ocx.run(
        "package", "exec", "--records-dir", str(live_sink),
        base.short, "--", "hello",
        format=None, check=False,
    )
    assert live.returncode == EXIT_SUCCESS, (
        f"the patched exec must succeed; rc={live.returncode}\nstderr:\n{live.stderr}"
    )
    live_record = _one_record(live_sink)
    assert _entries_with_role(live_record, "companion"), (
        "the live record must carry at least the published companion"
    )
    _assert_packages_are_standards_compliant(live_record, "package exec / patched (live)")

    freeze = ocx.run("--global", "patch", "freeze", format="json", check=False)
    assert freeze.returncode == EXIT_SUCCESS, (
        f"patch freeze must succeed; rc={freeze.returncode}\nstderr:\n{freeze.stderr}"
    )
    snapshot_path = Path(json.loads(freeze.stdout)["path"])

    frozen_sink = _sink(tmp_path, "standards-companion-frozen")
    frozen = ocx.run(
        "package", "exec", "--records-dir", str(frozen_sink),
        base.short, "--", "hello",
        format=None, check=False,
        env_overrides={"OCX_PATCH_SNAPSHOT": str(snapshot_path)},
    )
    assert frozen.returncode == EXIT_SUCCESS, (
        f"the frozen exec must succeed; rc={frozen.returncode}\nstderr:\n{frozen.stderr}"
    )
    frozen_record = _one_record(frozen_sink)
    assert frozen_record["resolution"].get("patchSnapshot"), (
        f"a frozen exec must name the snapshot; got resolution={frozen_record['resolution']}"
    )
    _assert_packages_are_standards_compliant(frozen_record, "package exec / patched (frozen)")


# ---------------------------------------------------------------------------
# Negative cases — proof the checks discriminate, not just that real
# records happen to pass them
# ---------------------------------------------------------------------------


@pytest.fixture()
def real_root_entry(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> dict:
    """A real, standards-compliant root ``packages[]`` entry to mutate from."""
    sink = _sink(tmp_path, "standards-fixture")
    result = ocx.run(
        "package", "exec", "--records-dir", str(sink),
        published_package.short, "--", "hello",
        format=None, check=False,
    )
    assert result.returncode == EXIT_SUCCESS, result.stderr
    return _root_entry(_one_record(sink))


def test_foreign_key_on_a_package_entry_is_rejected(real_root_entry: dict) -> None:
    """A key the ``ResourceDescriptor`` proto does not define fails to parse.

    Starts from a real, already-passing entry and adds exactly one foreign
    key, so the only variable is the thing under test. A hand-rolled
    dict-shape check (e.g. "the required keys are present") would let this
    straight through; ``ignore_unknown_fields=False`` must not.
    """
    poisoned = copy.deepcopy(real_root_entry)
    poisoned["identifier"] = "x"

    with pytest.raises(json_format.ParseError):
        _validate_resource_descriptor(poisoned)


def test_a_transport_prefixed_digest_fails_the_digest_set_check(
    real_root_entry: dict,
) -> None:
    """A ``SHA256:``-prefixed, uppercase digest value fails the DigestSet rule.

    The in-toto reference implementation's ``validate()`` does not itself
    reject this (it only checks presence) — this is the DigestSet convention
    this module enforces independently, and the negative case proves the
    assertion actually fires rather than vacuously passing on well-formed
    input alone.
    """
    poisoned = copy.deepcopy(real_root_entry)
    poisoned["digest"] = {"sha256": "SHA256:ABC"}

    with pytest.raises(AssertionError):
        _assert_digest_set_is_bare_lowercase_hex(poisoned)


# ---------------------------------------------------------------------------
# SLSA lift — the ADR's claim that packages[] drops into resolvedDependencies
# without a translator (D5)
# ---------------------------------------------------------------------------

STATEMENT_TYPE = "https://in-toto.io/Statement/v1"
PROVENANCE_PREDICATE_TYPE = "https://slsa.dev/provenance/v1"


def test_in_toto_attestation_ships_a_slsa_provenance_predicate_binding() -> None:
    """Confirms the binding the lift test below relies on actually exists.

    Correction: an earlier draft of this test asserted the *opposite* — that
    0.9.3 shipped no provenance predicate binding — based on a directory
    listing capped at ``-maxdepth 3``, one level short of
    ``predicates/provenance/v1/provenance_pb2.py``. It ships one, and it is
    real: ``BuildDefinition.resolved_dependencies`` is itself typed as
    ``repeated in_toto_attestation.v1.ResourceDescriptor``, so the predicate
    body validates strictly end to end — no Struct-typed fallback needed.
    """
    from in_toto_attestation.predicates.provenance.v1 import provenance_pb2

    version = importlib.metadata.version("in-toto-attestation")
    assert hasattr(provenance_pb2, "Provenance"), (
        f"expected a Provenance message in {provenance_pb2.__name__} "
        f"(in-toto-attestation {version})"
    )
    resolved_dependencies_field = next(
        field
        for field in provenance_pb2.BuildDefinition.DESCRIPTOR.fields
        if field.name == "resolved_dependencies"
    )
    assert (
        resolved_dependencies_field.message_type.full_name
        == "in_toto_attestation.v1.ResourceDescriptor"
    ), (
        "resolved_dependencies must be typed as an in-toto ResourceDescriptor "
        "for the strict end-to-end parse below to mean anything; got "
        f"{resolved_dependencies_field.message_type.full_name!r}"
    )


def _build_predicate(build_type: str, packages: list[dict]) -> dict:
    """A SLSA v1 provenance predicate body wrapping ``packages`` verbatim."""
    return {
        "buildDefinition": {
            "buildType": build_type,
            "externalParameters": {},
            "resolvedDependencies": packages,
        },
        "runDetails": {"builder": {"id": build_type}},
    }


def _validate_predicate_strictly(predicate: dict) -> None:
    """Strictly parse ``predicate`` as a SLSA v1 ``Provenance`` message.

    Because ``resolved_dependencies`` is itself typed as ``ResourceDescriptor``
    (see the discovery test above), this single parse validates the predicate
    envelope AND every embedded package entry in one strict pass — a foreign
    key anywhere in the tree is rejected, not just at the top level.
    """
    from in_toto_attestation.predicates.provenance.v1 import provenance_pb2

    parsed = provenance_pb2.Provenance()
    json_format.Parse(json.dumps(predicate), parsed, ignore_unknown_fields=False)


def _build_statement(subject_name: str, subject_digest_sha256: str, predicate: dict) -> dict:
    """A minimal SLSA v1 ``Statement`` wrapping ``predicate`` verbatim."""
    return {
        "_type": STATEMENT_TYPE,
        "subject": [{"name": subject_name, "digest": {"sha256": subject_digest_sha256}}],
        "predicateType": PROVENANCE_PREDICATE_TYPE,
        "predicate": predicate,
    }


def _validate_statement_strictly(statement: dict) -> None:
    parsed = Statement([], "", {})
    json_format.Parse(json.dumps(statement), parsed.pb, ignore_unknown_fields=False)
    parsed.validate()


def test_slsa_resolved_dependencies_lift_is_standards_compliant(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """``packages[]`` drops into a SLSA v1 provenance predicate with no
    translator, validated end to end: the ``Statement`` envelope through the
    reference ``Statement`` binding, and the predicate body — including every
    embedded ``resolvedDependencies`` entry — through the reference
    ``Provenance`` binding. ``resolvedDependencies`` below is
    ``record["packages"]`` **by reference** (asserted via ``is``), proving
    "no reshaping needed" rather than merely asserting it.
    """
    sink = _sink(tmp_path, "standards-slsa")
    result = ocx.run(
        "package", "exec", "--records-dir", str(sink),
        published_package.short, "--", "hello",
        format=None, check=False,
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"package exec must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    record = _one_record(sink)
    root_digest = _root_entry(record)["digest"]["sha256"]

    predicate = _build_predicate("https://ocx.sh/test", record["packages"])
    assert predicate["buildDefinition"]["resolvedDependencies"] is record["packages"], (
        "the lift must be a verbatim reference, not a reshaped copy — proving "
        "'no translator' means literally embedding the same list object"
    )
    _validate_predicate_strictly(predicate)

    statement = _build_statement(published_package.repo, root_digest, predicate)
    _validate_statement_strictly(statement)


def test_slsa_predicate_with_foreign_key_is_rejected() -> None:
    """A key the ``Provenance`` proto does not define fails strict parsing."""
    predicate = _build_predicate("https://ocx.sh/test", [])
    predicate["bogus"] = "unexpected"

    with pytest.raises(json_format.ParseError):
        _validate_predicate_strictly(predicate)


def test_slsa_predicate_rejects_a_foreign_key_on_a_nested_resolved_dependency() -> None:
    """Strictness propagates into the embedded ``ResourceDescriptor`` list too
    — proving the lift is not a shallow, top-level-only check.
    """
    poisoned_entry = {
        "name": "x",
        "digest": {"sha256": "a" * 64},
        "identifier": "a foreign key ResourceDescriptor does not define",
    }
    predicate = _build_predicate("https://ocx.sh/test", [poisoned_entry])

    with pytest.raises(json_format.ParseError):
        _validate_predicate_strictly(predicate)


def test_slsa_statement_with_foreign_key_is_rejected() -> None:
    """A key the ``Statement`` proto does not define fails strict parsing."""
    statement = _build_statement("x", "a" * 64, _build_predicate("https://ocx.sh/test", []))
    statement["bogus"] = "unexpected"

    with pytest.raises(json_format.ParseError):
        _validate_statement_strictly(statement)


def test_slsa_statement_with_empty_predicate_type_fails_validation() -> None:
    """``validate()`` itself rejects a ``Statement`` with no predicate type."""
    statement = _build_statement("x", "a" * 64, _build_predicate("https://ocx.sh/test", []))
    statement["predicateType"] = ""

    with pytest.raises(ValueError, match="Predicate type"):
        _validate_statement_strictly(statement)


# ---------------------------------------------------------------------------
# ECS / OTel vocabulary — process.*/host.* against vendored spec subsets
# ---------------------------------------------------------------------------

SPECS_DIR = Path(__file__).resolve().parents[1] / "specs"


def _ecs_field_names(fieldset: str) -> set[str]:
    """Field names in the vendored ECS subset for ``fieldset`` (e.g. ``"process"``).

    Each vendored file is a top-level list holding one fieldset dict (the
    shape the session lead's `WebFetch` extract came back in), so the fieldset
    itself is the list's only element.
    """
    (fieldset_doc,) = yaml.safe_load(
        (SPECS_DIR / "ecs" / f"{fieldset}.subset.yml").read_text()
    )
    return {field["name"] for field in fieldset_doc["fields"]}


def _otel_enum_members(vocabulary: str, attribute_id: str) -> set[str]:
    """Enum member values of an OTel attribute (e.g. ``("host", "host.arch")``)."""
    (entry,) = yaml.safe_load((SPECS_DIR / "otel" / f"{vocabulary}.subset.yml").read_text())
    assert entry["id"] == attribute_id, (
        f"expected the vendored {vocabulary}.subset.yml to describe {attribute_id!r}; "
        f"got {entry['id']!r}"
    )
    return {member["value"] for member in entry["type"]["members"]}


def _assert_process_host_os_match_ecs_otel(
    process: dict,
    host: dict,
    os_block: dict,
    ecs_process_fields: set[str],
    ecs_user_fields: set[str],
    ecs_host_fields: set[str],
    otel_host_arch: set[str],
    otel_os_type: set[str],
) -> None:
    """Type/presence checks against the vendored ECS/OTel subsets.

    Only checks fields actually present in ``process``/``host``/``os_block``
    — presence itself, where that is the property under test, is asserted by
    the caller (see the positive test's explicit POSIX presence asserts).
    """
    if "pid" in process:
        assert "pid" in ecs_process_fields and isinstance(process["pid"], int), (
            f"process.pid is ECS `long` (integer); got {process.get('pid')!r}"
        )
    if "executable" in process:
        assert "executable" in ecs_process_fields and isinstance(
            process["executable"], str
        ), (
            f"process.executable is ECS `keyword` (string); got "
            f"{process.get('executable')!r}"
        )
    if "working_directory" in process:
        assert "working_directory" in ecs_process_fields and isinstance(
            process["working_directory"], str
        ), (
            f"process.working_directory is ECS `keyword` (string); got "
            f"{process.get('working_directory')!r}"
        )
    assert "args" not in process, (
        "process.args is a deliberate v1 exclusion (a command line can carry "
        f"secrets) — never emitted, even though ECS defines the field; got {process}"
    )
    if "name" in host:
        assert "name" in ecs_host_fields and isinstance(host["name"], str), (
            f"host.name is ECS `keyword` (string); got {host.get('name')!r}"
        )
    if "parent" in process:
        assert "pid" in ecs_process_fields and isinstance(process["parent"]["pid"], int), (
            f"process.parent.pid reuses ECS process.pid (`long`); got {process['parent']!r}"
        )
    if "user" in process:
        assert "id" in ecs_user_fields and isinstance(process["user"].get("id"), str), (
            f"process.user.id reuses ECS user.id (`keyword`); got {process['user']!r}"
        )
    if "arch" in process:
        assert "arch" not in ecs_process_fields, (
            "process.arch is deliberately outside ECS's vocabulary (no ECS "
            f"equivalent exists); vendored ECS process fields: {ecs_process_fields}"
        )
        assert process["arch"] in otel_host_arch, (
            f"process.arch must be a value from OTel's host.arch enum; got "
            f"{process['arch']!r}, enum={otel_host_arch}"
        )
    if "type" in os_block:
        # Checked against OTel, never ECS: ECS documents os.type as reused
        # under host.os.type (never top-level) and flags its own values as
        # conflicting with OTel's — see test/specs/SOURCES.md.
        assert os_block["type"] in otel_os_type, (
            f"os.type must be a value from OTel's os.type enum; got "
            f"{os_block['type']!r}, enum={otel_os_type}"
        )


def test_process_host_os_fields_match_the_vendored_ecs_otel_schema(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """``process.*``/``host.*``/``os.type`` in a real record match the vendored
    ECS/OTel field definitions. ``process.arch`` and ``os.type`` are both
    checked against OTel, not ECS — ADR's documented choice for the former, no
    top-level ECS equivalent for the latter (see ``test/specs/SOURCES.md``).
    """
    ecs_process_fields = _ecs_field_names("process")
    ecs_user_fields = _ecs_field_names("user")
    ecs_host_fields = _ecs_field_names("host")
    otel_host_arch = _otel_enum_members("host", "host.arch")
    otel_os_type = _otel_enum_members("os", "os.type")

    sink = _sink(tmp_path, "standards-ecs")
    result = ocx.run(
        "package", "exec", "--records-dir", str(sink),
        published_package.short, "--", "hello",
        format=None, check=False,
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"package exec must succeed; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    record = _one_record(sink)
    process = record["process"]
    host = record["host"]
    os_block = record["os"]

    if sys.platform != "win32":
        # An ordinary host resolves every best-effort probe — presence is the
        # non-vacuous half of this test, mirroring
        # test_execution_records.py::test_unforced_probes_are_present_in_an_ordinary_record.
        assert "parent" in process, f"a POSIX process has a parent; got {process}"
        assert "user" in process, f"a POSIX process has a user; got {process}"
    assert "type" in os_block, f"an ordinary host resolves os.type; got {os_block}"

    _assert_process_host_os_match_ecs_otel(
        process,
        host,
        os_block,
        ecs_process_fields,
        ecs_user_fields,
        ecs_host_fields,
        otel_host_arch,
        otel_os_type,
    )


def test_ecs_otel_vocabulary_check_rejects_a_mutated_process_block() -> None:
    """Proof the check above discriminates: a type violation on ``pid``, an
    off-enum ``arch``, an off-enum ``os.type``, and a resurrected ``args`` key
    are each rejected — against a baseline that is first shown to pass
    unmodified.
    """
    ecs_process_fields = _ecs_field_names("process")
    ecs_user_fields = _ecs_field_names("user")
    ecs_host_fields = _ecs_field_names("host")
    otel_host_arch = _otel_enum_members("host", "host.arch")
    otel_os_type = _otel_enum_members("os", "os.type")
    host = {"name": "batch-node"}

    def check(process: dict, os_block: dict) -> None:
        _assert_process_host_os_match_ecs_otel(
            process,
            host,
            os_block,
            ecs_process_fields,
            ecs_user_fields,
            ecs_host_fields,
            otel_host_arch,
            otel_os_type,
        )

    baseline_process = {
        "pid": 123,
        "executable": "/bin/true",
        "working_directory": "/tmp",
        "arch": "amd64",
    }
    baseline_os = {"type": "linux"}
    check(baseline_process, baseline_os)

    wrong_type = copy.deepcopy(baseline_process)
    wrong_type["pid"] = "not-an-int"
    with pytest.raises(AssertionError):
        check(wrong_type, baseline_os)

    off_enum_arch = copy.deepcopy(baseline_process)
    off_enum_arch["arch"] = "risc-v"
    with pytest.raises(AssertionError):
        check(off_enum_arch, baseline_os)

    off_enum_os_type = {"type": "amigaos"}
    with pytest.raises(AssertionError):
        check(baseline_process, off_enum_os_type)

    resurrected_args = copy.deepcopy(baseline_process)
    resurrected_args["args"] = ["--dangerous-secret"]
    with pytest.raises(AssertionError):
        check(resurrected_args, baseline_os)
