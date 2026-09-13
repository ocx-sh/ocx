# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for archive-extraction containment under `ocx package create`.

`--extract` unpacks an untrusted archive into a scratch tree. An entry whose
path escapes that tree — a `..` traversal, an absolute path, or a symlink whose
target leaves the root — must be refused (exit 65), never written outside it.

The same guarded loop is reached by **two** routes, and both are pinned here:

- `ocx package create --extract`, which unpacks an operator-chosen archive
  (`Archive::extract_with_options`). The operator had to pick the hostile file.
- **Layer materialization**, where the bytes come from a *registry* — which is
  the route that matters more, because nothing about it requires the victim to
  have chosen anything. It splits again by how the layer arrives:
  `ocx package install` on a published package streams it through
  `Client::pull_layer` -> `archive::extract_tar_from_reader`, while a local
  file layer handed to `ocx package test` goes through `pull_local`'s
  `extract_archive_to_temp` -> the same `Archive::extract_with_options` that
  `--extract` uses.

The discriminating unit tests live in `archive/tar.rs` and `archive/zip.rs`
(they can observe a write that lands outside the root, which is invisible from
here — the internal scratch dir is not exposed). These end-to-end tests pin the
operator-visible contract: a hostile archive fails the command with a data
error and leaves no bundle behind.
"""

from __future__ import annotations

from pathlib import Path

from src import current_platform
from src.helpers import build_archive, make_package, resolved_metadata_path
from src.runner import OcxRunner

EXIT_DATA_ERR = 65  # archive::Error::{EntryEscape, SymlinkEscape}

# Distinct stderr fragments per error kind, so a test that expects one refusal
# cannot pass on the other. The path-traversal kinds report "escapes the
# extraction root" (archive::Error::EntryEscape); an escaping symlink reports
# "escapes the root directory" (archive::Error::SymlinkEscape).
ENTRY_ESCAPE = "escapes the extraction root"
SYMLINK_ESCAPE = "escapes the root directory"


def _create(ocx: OcxRunner, source: Path, out: Path, *args: str):
    return ocx.plain("package", "create", "-o", str(out), *args, "--extract", str(source), check=False)


def test_tar_dotdot_entry_is_refused(ocx: OcxRunner, tmp_path: Path):
    """A tar entry named `../evil` escapes the extraction root; the extractor
    refuses the archive rather than writing through the traversal."""
    archive = build_archive(tmp_path / "evil.tar.gz", {"../evil": ("pwned", 0o644)})
    out = tmp_path / "bundle.tar.xz"

    result = _create(ocx, archive, out)

    assert result.returncode == EXIT_DATA_ERR, result.stdout + result.stderr
    assert ENTRY_ESCAPE in result.stderr, result.stderr
    assert SYMLINK_ESCAPE not in result.stderr, result.stderr
    assert not out.exists(), "a refused extraction must leave no bundle behind"


def test_zip_dotdot_entry_is_refused(ocx: OcxRunner, tmp_path: Path):
    """D7: a zip entry named `../evil` has no enclosed name, so extraction
    refuses the archive — the tar path's behaviour, matched for zip."""
    archive = build_archive(tmp_path / "evil.zip", {"../evil": ("pwned", 0o644)})
    out = tmp_path / "bundle.tar.xz"

    result = _create(ocx, archive, out)

    assert result.returncode == EXIT_DATA_ERR, result.stdout + result.stderr
    assert ENTRY_ESCAPE in result.stderr, result.stderr
    assert not out.exists(), "a refused extraction must leave no bundle behind"


def test_absolute_entry_is_refused(ocx: OcxRunner, tmp_path: Path):
    """An absolute entry path cannot be contained by a root it does not descend
    from; the extractor refuses it."""
    archive = build_archive(tmp_path / "abs.tar.gz", {"/etc/ocx-pwned": ("pwned", 0o644)})
    out = tmp_path / "bundle.tar.xz"

    result = _create(ocx, archive, out)

    assert result.returncode == EXIT_DATA_ERR, result.stdout + result.stderr
    assert ENTRY_ESCAPE in result.stderr, result.stderr
    assert not out.exists(), "a refused extraction must leave no bundle behind"


def test_escaping_symlink_entry_is_refused(ocx: OcxRunner, tmp_path: Path):
    """A symlink entry whose target leaves the root is refused before it is
    created, so no later entry can be walked through it out of the tree."""
    archive = build_archive(
        tmp_path / "link.tar.gz",
        {"hello-1.2.3/README.md": ("# hello\n", 0o644)},
        symlinks={"hello-1.2.3/escape": "../../../../../../etc"},
    )
    out = tmp_path / "bundle.tar.xz"

    result = _create(ocx, archive, out)

    assert result.returncode == EXIT_DATA_ERR, result.stdout + result.stderr
    # The symlink kind reports its own message; asserting the traversal-entry
    # fragment is absent keeps this from passing if the symlink were misrouted
    # to the EntryEscape path.
    assert SYMLINK_ESCAPE in result.stderr, result.stderr
    assert ENTRY_ESCAPE not in result.stderr, result.stderr
    assert not out.exists(), "a refused extraction must leave no bundle behind"


# ---------------------------------------------------------------------------
# The layer route. `--extract` requires the operator to have chosen a hostile
# archive; these fire on whatever a registry serves, which is the shape that
# originally wrote `$OCX_HOME/PWNED-ON-INSTALL`.
# ---------------------------------------------------------------------------

# The file the exploit wrote, kept as a distinct marker so the sweep below
# cannot be satisfied by some unrelated file the run happens to leave behind.
PWNED = "PWNED-ON-INSTALL"


def _hostile_layer(target: Path) -> Path:
    """The two-entry escape: a symlink out of the root, then a write through it.

    Entry order is load-bearing and `build_archive` guarantees it (symlinks are
    written first): the link lands inside the root, and the following regular
    file is authored to traverse *through* it. Refusing the link is what stops
    the second entry, so a guard that only screened regular-file paths would
    still be walked out of the tree here.

    The `../` chain is deliberately longer than the layer content path is deep
    (`$OCX_HOME/layers/<registry>/sha256/<2hex>/<rest>/content/`), so it lands
    above `$OCX_HOME` rather than merely somewhere else inside it.

    **Two independent guards defend this shape, and the exit code cannot tell
    them apart.** Measured by neutering `symlink::validate_target`'s
    containment verdict: the run still exits 65 and still writes nothing, but
    the refusal comes from the *entry* guard instead (`EntryEscape`, "escapes
    the extraction root") once the second entry is walked through the link that
    was let through. That is why every row below asserts the expected fragment
    is present AND the sibling fragment is absent -- on exit code alone these
    rows would be green with the symlink guard gone.
    """
    return build_archive(
        target,
        {f"escape/{PWNED}": ("pwned", 0o644)},
        symlinks={"escape": "../../../../../../../../.."},
    )


def _no_marker_outside(ocx: OcxRunner) -> None:
    """Nothing was written outside the sandbox — the actual security property.

    Checked against `$OCX_HOME`'s parent, not just the home: the exploit's own
    target was `$OCX_HOME/PWNED-ON-INSTALL`, so a sweep rooted at the home
    would look *inside* the thing that was escaped into. Both are swept.
    """
    home = ocx.ocx_home
    for root in (home, home.parent):
        hits = sorted(str(p) for p in root.rglob(PWNED))
        assert not hits, f"a hostile layer entry escaped the extraction root: wrote {hits}"


def test_a_registry_served_hostile_layer_is_refused_on_install(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """The `pull_layer` route: the bytes arrive from a registry, not from a path.

    This is the half the plan's Block finding required alongside `--extract`,
    and the more serious one -- `ocx package install` streams the layer through
    `Client::pull_layer` -> `archive::extract_tar_from_reader`, so the victim
    chose a package name, not a file.

    Reaching it takes a real publish, because `package push` does not extract:
    a legitimate package is created first only to obtain the compiled metadata
    sidecar (`push -m` requires `create`'s output, not an authoring file), then
    the hostile tarball is pushed under a second tag as the layer. The install
    is what extracts, and what must refuse.
    """
    make_package(ocx, unique_repo, "1.0.0", tmp_path)
    sidecar = resolved_metadata_path(tmp_path / f"bundle-{unique_repo}-1.0.0.tar.xz")
    evil = _hostile_layer(tmp_path / "evil.tar.gz")

    # Publishing hostile bytes is the precondition, not the assertion. Named
    # explicitly so a failure here reads as "the fixture broke" rather than as
    # the guard under test having fired in the wrong place.
    pushed = ocx.plain(
        "package", "push",
        "-p", current_platform(),
        "-m", str(sidecar),
        "-i", f"{ocx.registry}/{unique_repo}:1.1.0",
        str(evil),
        check=False,
    )
    assert pushed.returncode == 0, (
        f"precondition: pushing the hostile layer must succeed (push does not "
        f"extract, so the refusal belongs to install)\n{pushed.stdout}{pushed.stderr}"
    )

    result = ocx.plain("package", "install", f"{unique_repo}:1.1.0", check=False)

    assert result.returncode == EXIT_DATA_ERR, result.stdout + result.stderr
    # The symlink kind, specifically: this archive's escape is the link, and
    # asserting the traversal-entry fragment is absent keeps the row from
    # passing on a sibling error the way a bare non-zero check would.
    assert SYMLINK_ESCAPE in result.stderr, result.stderr
    assert ENTRY_ESCAPE not in result.stderr, result.stderr
    _no_marker_outside(ocx)


def test_a_hostile_local_layer_is_refused_by_package_test(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """The other layer route, and a different command from `--extract`.

    `ocx package test` hands a local file layer to `pull_local`, which reaches
    `Archive::extract_with_options` -- the same function `--extract` calls, so
    this row pins the *reachability* through a second command rather than a
    second guard. No registry round-trip: the refusal must land before the
    package is ever materialized, which is why the trailing command names a
    binary that would fail loudly if it ever ran.
    """
    make_package(ocx, unique_repo, "1.0.0", tmp_path)
    sidecar = resolved_metadata_path(tmp_path / f"bundle-{unique_repo}-1.0.0.tar.xz")
    evil = _hostile_layer(tmp_path / "evil.tar.gz")

    result = ocx.plain(
        "package", "test",
        "-p", current_platform(),
        "-m", str(sidecar),
        "-i", f"{unique_repo}:1.1.0",
        str(evil),
        "--", "/bin/false",
        check=False,
    )

    assert result.returncode == EXIT_DATA_ERR, result.stdout + result.stderr
    assert SYMLINK_ESCAPE in result.stderr, result.stderr
    assert ENTRY_ESCAPE not in result.stderr, result.stderr
    _no_marker_outside(ocx)

def test_reverse_order_ladder_is_refused_on_the_registry_stream_route(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """L2 finding — the ladder with its hop planted AFTER the link that uses it.

    `e1` -> `a/..` arrives while `a` is absent: the target's missing tail folds
    lexically to the root and the per-entry predicate accepts it. Then `a` -> `.`
    lands, and `e1` now physically resolves to the root's parent. No write goes
    through it (the ancestor guard holds), but the extracted tree carries an
    escaping link. Every other route re-packs the tree and that re-pack's own
    sweep caught this by accident — `--extract` and the `package test` local
    layer both do. The registry stream route (`install` -> `pull_layer` ->
    `extract_tar_from_reader`) is the one that does not: the bytes go from the
    wire into `layers/<digest>/content/` with nothing in between, so before the
    extractor's post-loop sweep this tree was finalized as-is. That is the route
    this row takes, and the one a victim reaches by choosing a package name.
    """
    make_package(ocx, unique_repo, "1.0.0", tmp_path)
    sidecar = resolved_metadata_path(tmp_path / f"bundle-{unique_repo}-1.0.0.tar.xz")
    # Insertion order is entry order: `e1` before `a`.
    evil = build_archive(
        tmp_path / "reverse.tar.gz",
        {"README.md": ("# reverse\n", 0o644)},
        symlinks={"e1": "a/..", "a": "."},
    )

    pushed = ocx.plain(
        "package", "push",
        "-p", current_platform(),
        "-m", str(sidecar),
        "-i", f"{ocx.registry}/{unique_repo}:1.1.0",
        str(evil),
        check=False,
    )
    assert pushed.returncode == 0, (
        f"precondition: pushing the hostile layer must succeed (push does not "
        f"extract, so the refusal belongs to install)\n{pushed.stdout}{pushed.stderr}"
    )

    result = ocx.plain("package", "install", f"{unique_repo}:1.1.0", check=False)

    assert result.returncode == EXIT_DATA_ERR, result.stdout + result.stderr
    assert SYMLINK_ESCAPE in result.stderr, result.stderr
    assert ENTRY_ESCAPE not in result.stderr, result.stderr
    # The finished layer must not exist: a finalized tree carrying the link is
    # the defect, whatever the exit code said.
    layers = ocx.ocx_home / "layers"
    hits = sorted(str(p) for p in layers.rglob("e1")) if layers.exists() else []
    assert not hits, f"a layer carrying an escaping link was finalized: {hits}"


def _no_pwned_under(root: Path) -> None:
    """Nothing named PWNED anywhere at or above the extraction root — the actual
    security property. Swept against `tmp_path`, which contains both the scratch
    root and its siblings, so a link that climbs out lands somewhere this sees."""
    hits = sorted(str(p) for p in root.rglob("PWNED"))
    assert not hits, f"a traversal escaped the extraction root: wrote {hits}"


def test_target_component_ladder_traversal_is_refused(ocx: OcxRunner, tmp_path: Path):
    """Codex finding — the ladder shape. Each hop folds back in-root *lexically*
    (`a/..` -> the root) but climbs one level *physically* through the previous
    planted hop (`a` -> `.`, `e1` -> `a/..`, `e2` -> `e1/..`, …). The old lexical
    fold accepted every rung; the physical predicate resolves each target through
    the on-disk chain, sees it leave the root, and refuses the symlink before it
    is created — so the file authored to be written through the deepest rung
    (`e3/PWNED/x`) never lands outside the tree. `build_archive` writes symlinks
    before files, reproducing the chain order exactly. Asserts the FILESYSTEM
    (nothing created outside the root), not merely the exit code: two guards
    refuse this shape and the code alone cannot tell them apart."""
    archive = build_archive(
        tmp_path / "chain.tar.gz",
        {"e3/PWNED/x": ("pwned", 0o644)},
        symlinks={"a": ".", "e1": "a/..", "e2": "e1/..", "e3": "e2/.."},
    )
    out = tmp_path / "bundle.tar.xz"

    result = _create(ocx, archive, out)

    assert result.returncode == EXIT_DATA_ERR, result.stdout + result.stderr
    assert SYMLINK_ESCAPE in result.stderr, result.stderr
    assert not out.exists(), "a refused extraction must leave no bundle behind"
    _no_pwned_under(tmp_path)


def test_mkdir_only_traversal_through_planted_symlink_is_refused(ocx: OcxRunner, tmp_path: Path):
    """The surviving primitive after the write path is closed is *directory
    creation* through a planted link. `a` -> `.` is an in-root hop the target
    guard accepts (its target does NOT leave the root), so this exercises a
    *different* guard from the ladder above: a later entry whose parent path
    traverses `a` (`a/PWNED/x`) forces `create_dir_all(a/PWNED)` to mkdir THROUGH
    the link, and the extractor's pre-create ancestor-symlink guard must refuse it
    first (ENTRY_ESCAPE, not the target-guard's SYMLINK_ESCAPE).

    No filesystem sweep here, by construction: for THIS shape `a` -> `.` resolves
    to the root itself, so `a/PWNED` is `root/PWNED` — inside the root — and
    nothing can land outside for a sweep to find. (An accepted link can still
    come to escape when a LATER entry plants its hop — the reverse-order ladder
    row below — which is what the post-loop sweep exists for.) The load-bearing
    discriminators are the exit code, the ENTRY_ESCAPE fragment (with
    SYMLINK_ESCAPE absent, pinning *which* guard fired), and `not out.exists()`
    (a produced bundle would mean the refusal never happened)."""
    archive = build_archive(
        tmp_path / "mkdir.tar.gz",
        {"a/PWNED/x": ("pwned", 0o644)},
        symlinks={"a": "."},
    )
    out = tmp_path / "bundle.tar.xz"

    result = _create(ocx, archive, out)

    assert result.returncode == EXIT_DATA_ERR, result.stdout + result.stderr
    # The in-root hop `a` is created legitimately; the escape is the write
    # THROUGH it, caught by the extractor's pre-create ancestor-symlink guard —
    # the entry-escape kind, not the symlink-target kind. Asserting the sibling
    # fragment is absent keeps this from passing on a target-guard refusal.
    assert ENTRY_ESCAPE in result.stderr, result.stderr
    assert SYMLINK_ESCAPE not in result.stderr, result.stderr
    assert not out.exists(), "a refused extraction must leave no bundle behind"
