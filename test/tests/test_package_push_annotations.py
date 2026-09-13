# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""`ocx package push` writes OCI annotations on the image index.

`org.opencontainers.image.source` is the only mechanism by which a registry
links a published package back to its source repository, so these tests pin
the wire effect end-to-end against a live registry: the annotation reaches
the index of the primary tag and of every cascade tag, and a push without the
flag leaves the index annotation-free.

The `--ci-annotations` half runs under a faked GitLab environment. The runner
strips ambient environment, so the variables the flag reads exist only where a
test injects them -- which is also what makes the undetectable-CI exit code
testable on a developer machine that may itself be inside CI.
"""
from __future__ import annotations

from pathlib import Path

from src.helpers import make_package, resolved_metadata_path
from src.registry import fetch_manifest_from_registry
from src.runner import OcxRunner, current_platform

SOURCE = "org.opencontainers.image.source"
REVISION = "org.opencontainers.image.revision"
CREATED = "org.opencontainers.image.created"
VERSION = "org.opencontainers.image.version"
SOURCE_URL = "https://github.com/ocx-sh/ocx"

# What a GitLab runner exports, and what `--ci-annotations=gitlab` reads.
GITLAB_ENV = {
    "GITLAB_CI": "true",
    "CI_PROJECT_URL": "https://gitlab.example.com/acme/widget",
    "CI_COMMIT_SHA": "0123456789abcdef0123456789abcdef01234567",
    "CI_PIPELINE_CREATED_AT": "2026-09-10T08:30:00Z",
}


def index_annotations(ocx: OcxRunner, repo: str, tag: str) -> dict[str, str]:
    manifest = fetch_manifest_from_registry(ocx.registry, repo, tag)
    return manifest.get("annotations") or {}


def test_annotation_lands_on_the_primary_tag_and_every_cascade_tag(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    make_package(
        ocx,
        unique_repo,
        "1.2.3",
        tmp_path,
        cascade=True,
        extra_push_args=["--annotation", f"{SOURCE}={SOURCE_URL}", "--annotation", f"{REVISION}=deadbeef"],
    )

    # A cascading push must not leave a rolling alias with weaker provenance
    # than the version tag it points at.
    for tag in ("1.2.3", "1.2", "1", "latest"):
        annotations = index_annotations(ocx, unique_repo, tag)
        assert annotations.get(SOURCE) == SOURCE_URL, f"missing source annotation on {tag}: {annotations}"
        assert annotations.get(REVISION) == "deadbeef", f"missing revision annotation on {tag}: {annotations}"


def test_push_without_the_flag_writes_no_index_annotations(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """The absent-value case: no flag, no annotations, no behaviour change."""
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)

    assert index_annotations(ocx, unique_repo, "1.0.0") == {}


def test_a_later_plain_push_does_not_unlink_the_source_repository(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Merging into an existing index preserves annotations it already holds,
    so a pipeline that forgets the flag on one release does not silently
    unlink the package from its repository."""
    make_package(
        ocx,
        unique_repo,
        "1.0.0",
        tmp_path,
        cascade=True,
        extra_push_args=["--annotation", f"{SOURCE}={SOURCE_URL}"],
    )
    make_package(ocx, unique_repo, "1.0.1", tmp_path, cascade=True)

    # `latest` was re-pointed by the second, annotation-less push.
    assert index_annotations(ocx, unique_repo, "latest").get(SOURCE) == SOURCE_URL


def test_ci_annotations_land_on_the_primary_tag_and_every_cascade_tag(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """All four keys, on every tag the cascading push wrote.

    The bare flag rather than `--ci-annotations=gitlab`: it exercises the
    autodetect too, and a run under the faked environment must reach the same
    answer an explicit flavor does.
    """
    make_package(
        ocx,
        unique_repo,
        "1.2.3",
        tmp_path,
        cascade=True,
        extra_push_args=["--ci-annotations"],
        push_env=GITLAB_ENV,
    )

    for tag in ("1.2.3", "1.2", "1", "latest"):
        annotations = index_annotations(ocx, unique_repo, tag)
        assert annotations.get(SOURCE) == GITLAB_ENV["CI_PROJECT_URL"], f"on {tag}: {annotations}"
        assert annotations.get(REVISION) == GITLAB_ENV["CI_COMMIT_SHA"], f"on {tag}: {annotations}"
        assert annotations.get(CREATED) == GITLAB_ENV["CI_PIPELINE_CREATED_AT"], f"on {tag}: {annotations}"
        # The version the push resolved, not the rolling alias it also wrote:
        # `latest` names the same artifact as `1.2.3` and carries its version.
        assert annotations.get(VERSION) == "1.2.3", f"on {tag}: {annotations}"


def test_an_explicit_annotation_overrides_the_generated_one(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """What the publisher typed beats what the environment happened to hold --
    a wrapper that already knows the canonical source URL must be obeyed."""
    make_package(
        ocx,
        unique_repo,
        "1.0.0",
        tmp_path,
        cascade=False,
        extra_push_args=["--ci-annotations=gitlab", "--annotation", f"{SOURCE}={SOURCE_URL}"],
        push_env=GITLAB_ENV,
    )

    annotations = index_annotations(ocx, unique_repo, "1.0.0")
    assert annotations.get(SOURCE) == SOURCE_URL, annotations
    # The keys the explicit flag did not name still come from the environment.
    assert annotations.get(REVISION) == GITLAB_ENV["CI_COMMIT_SHA"], annotations


def test_bare_ci_annotations_outside_ci_is_a_usage_error(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Exit 64 before any upload: the flag is resolved first, so an
    undetectable provider costs no registry round-trip -- which is why the
    bundle argument below names a file that does not exist."""
    result = ocx.plain(
        "package",
        "push",
        "--ci-annotations",
        "-p",
        current_platform(),
        "-i",
        f"{ocx.registry}/{unique_repo}:1.0.0",
        str(tmp_path / "never-read.tar.xz"),
        check=False,
    )

    assert result.returncode == 64, result.stderr
    assert "--ci-annotations" in result.stderr, result.stderr


def test_a_spaced_ci_annotations_value_is_a_usage_error(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`--ci-annotations gitlab` (space, no `=`) is refused, naming the `=`.

    Run under the faked GitLab environment, which is the whole point: the
    grammar requires `=`, so the space form leaves the flag bare and hands
    `gitlab` to the layer positional. Autodetect then SUCCEEDS here and the
    value the user typed is consumed as a layer path -- a real runner would
    have annotated and pushed a bogus layer with nothing said. Exit 64 before
    any upload, which is why the bundle argument names a file that does not
    exist.
    """
    result = ocx.plain(
        "package",
        "push",
        "--ci-annotations",
        "gitlab",
        "-p",
        current_platform(),
        "-i",
        f"{ocx.registry}/{unique_repo}:1.0.0",
        str(tmp_path / "never-read.tar.xz"),
        env_overrides=GITLAB_ENV,
        check=False,
    )

    assert result.returncode == 64, result.stderr
    assert "--ci-annotations" in result.stderr, result.stderr
    # Not a bare `"gitlab" in stderr`: the advice clause below already names
    # both providers, so that would pass without the message ever echoing the
    # token. Anchor on the phrase that quotes what was actually given.
    assert "was given `gitlab`" in result.stderr, result.stderr
    assert "attached with `=`" in result.stderr, result.stderr


def test_a_spaced_build_timestamp_value_is_a_usage_error(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`--build-timestamp none` (space, no `=`) is refused, naming the `=`.

    The sibling of the `--ci-annotations` row above, on the same command and
    the same layer positional, and the worse of the two: this flag carries
    `default_missing_value = "datetime"`, so the space form does not merely
    drop the value -- it substitutes the **opposite** of what was typed. Bare
    resolves to `datetime`, `none` falls through to the layer positional, and
    an operator who asked for no build metadata would publish
    `1.0.0+<timestamp>` plus a bogus layer.

    `none` rather than `date` or `datetime` on purpose: it is the spelling
    whose silent absorption inverts the request rather than merely restating
    it. Exit 64 before any upload, which is why the bundle argument names a
    file that does not exist.
    """
    result = ocx.plain(
        "package",
        "push",
        "--build-timestamp",
        "none",
        "-p",
        current_platform(),
        "-i",
        f"{ocx.registry}/{unique_repo}:1.0.0",
        str(tmp_path / "never-read.tar.xz"),
        check=False,
    )

    assert result.returncode == 64, result.stderr
    assert "--build-timestamp" in result.stderr, result.stderr
    # Anchored on the phrase that quotes the token: the advice clause lists
    # every spelling, so a bare `"none" in stderr` would pass on that alone.
    assert "was given `none`" in result.stderr, result.stderr
    assert "attached with `=`" in result.stderr, result.stderr


def test_an_attached_build_timestamp_beside_a_same_named_layer_is_accepted(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """The control for the row above: `--build-timestamp=date none` is NOT refused.

    With the value attached, a layer literally named `none` is what the
    publisher meant, so the guard must not reach it -- and `Date` could not
    have come from `default_missing_value`, which is what lets the guard tell
    the two apart. Without that narrowing this invocation would exit 64.

    It still fails, on the missing layer file, which is the point: the run got
    past argv validation and into the push. The assertion is therefore that the
    refusal is NOT the guard's, checked on the guard's own phrase rather than
    on an exit code the push shares with it.
    """
    result = ocx.plain(
        "package",
        "push",
        "--build-timestamp=date",
        "-p",
        current_platform(),
        "-i",
        f"{ocx.registry}/{unique_repo}:1.0.0",
        "none",
        check=False,
    )

    assert "was given `none`" not in result.stderr, (
        f"the guard must not fire on an attached value: {result.stderr}"
    )
    assert "attached with `=`" not in result.stderr, (
        f"the guard must not fire on an attached value: {result.stderr}"
    )
    # The positive half: the run reached the layer read, which only happens
    # after argv validation passed. Without this a guard that refused with a
    # different message would still satisfy the two negatives above.
    assert "none" in result.stderr, (
        f"the push must have reached the layer named `none`: {result.stderr}"
    )


def test_the_report_lists_the_annotations_written(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`--format json` carries what landed, so a pipeline can assert on it
    without reading the index back over HTTP.

    Pushes the fixture bundle a second time under a new tag: the report is only
    reachable through `ocx.json`, and `make_package` pushes plain.
    """
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    bundle = tmp_path / f"bundle-{unique_repo.replace('/', '_')}-1.0.0.tar.xz"

    report = ocx.json(
        "package",
        "push",
        "--ci-annotations=gitlab",
        "-m",
        str(resolved_metadata_path(bundle)),
        "-i",
        f"{ocx.registry}/{unique_repo}:2.0.0",
        str(bundle),
        env_overrides=GITLAB_ENV,
    )

    assert report["annotations_written"][SOURCE] == GITLAB_ENV["CI_PROJECT_URL"], report
    assert report["annotations_written"][VERSION] == "2.0.0", report


def test_a_non_version_tag_writes_no_version_annotation(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A tag that is not a version carries no `.version`, while the rest of the
    CI set still lands - `.version` is the only key derived from the tag."""
    make_package(
        ocx,
        unique_repo,
        "nightly",
        tmp_path,
        cascade=False,
        extra_push_args=["--ci-annotations=gitlab"],
        push_env=GITLAB_ENV,
    )

    annotations = index_annotations(ocx, unique_repo, "nightly")
    assert annotations.get(SOURCE) == GITLAB_ENV["CI_PROJECT_URL"], annotations
    assert VERSION not in annotations, f"a non-version tag must annotate no version: {annotations}"


def test_ci_version_comes_from_the_build_receipt_when_identifier_is_omitted(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """The reason `--ci-annotations` earns `.version` (#450): with `-i` omitted
    the pushed tag is resolved from the build receipt `create` wrote, and only
    the push knows it - so a pipeline cannot stamp `.version` for itself.
    """
    content = tmp_path / "pkg" / "bin"
    content.mkdir(parents=True)
    tool = content / "tool"
    tool.write_text("#!/bin/sh\necho ok\n")
    tool.chmod(0o755)
    metadata = tmp_path / "metadata.json"
    metadata.write_text('{"type": "bundle", "version": 1, "env": []}')
    bundle = tmp_path / "bundle.tar.xz"

    # `create -i` records the identifier in the build receipt beside the bundle.
    identifier = f"{ocx.registry}/{unique_repo}:2.5.0"
    ocx.plain(
        "package",
        "create",
        "-m",
        str(metadata),
        "-o",
        str(bundle),
        "-p",
        current_platform(),
        "-i",
        identifier,
        str(content.parent),
    )

    # Push with NO `-i`: the tag - and therefore the version - comes from the
    # receipt, which is exactly what the CI annotation must stamp.
    report = ocx.json(
        "package",
        "push",
        "-m",
        str(resolved_metadata_path(bundle)),
        "-p",
        current_platform(),
        "--ci-annotations=gitlab",
        str(bundle),
        env_overrides=GITLAB_ENV,
    )

    assert report["annotations_written"][VERSION] == "2.5.0", report
