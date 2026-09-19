"""Logging oracle for the crate split (plan C-048 / S-013, design addendum A.5(i)).

Library code calls the ``log`` facade; the CLI's tracing subscriber renders
those records on stderr through ``tracing_log::LogTracer``. A crate move can
break either half silently — a record that no longer reaches the subscriber,
or an ``OCX_LOG`` directive that no longer names a target — so both are pinned
here on observable stderr text. The subscriber prints no target
(``with_target(false)``), which is why every assertion is on the message.
"""

from __future__ import annotations

import re

import pytest

from src.helpers import PROJECT_ROOT

# One library ``log::debug!`` (``ocx_package_manager/src/tasks/clean.rs``, the
# project-GC root walk), emitted by ``ocx --offline clean`` on a fresh
# ``OCX_HOME`` — no network, no package fixture, no project. The implicit
# ``$OCX_HOME/ocx.lock`` root (``adr_global_toolchain_tier.md`` D5) is always
# walked and always absent there, so the line is unconditional rather than
# incidental.
#
# This row used to assert ``Listing repositories for registry`` under ``index
# catalog``. WP-28 moved that line into ``ocx_index``, and the row FAILED rather
# than skipping — which is the behaviour the note below promises, observed. It
# then belonged to ``ocx_lib`` until WP-34 moved ``package_manager/**`` into
# ``ocx_package_manager``: the line did not change, its crate target did, and
# the row follows the line.
PACKAGE_MANAGER_LINE = "Skipping project root"
# One CLI ``log::debug!`` (``app/context.rs``), emitted whenever the command
# context is built — ``index catalog`` builds one.
CLI_LINE = "Creating context with options"

# ``ocx_util`` left ``ocx_lib`` in WP-22 with ``log::`` call sites of its own,
# so it is a target the filter must name. Its line is ``env::flag``'s boolean
# refusal: every invocation reads ``OCX_NO_CONFIG`` through that accessor, so a
# value that is not a boolean renders the line with no registry, no package and
# no filesystem state. A bare ``ocx_util`` entry asserting ``LIBRARY_LINE``
# would be a loud red, not coverage — that line is ``oci/index.rs``'s, and
# ``oci`` stays in ``ocx_lib`` until WP-28.
UTIL_LINE = "Environment variable 'OCX_NO_CONFIG' has invalid boolean value"
# ``ocx_console`` left ``ocx_lib`` in WP-23 with the ``log::`` call sites of
# ``UserInterface``, which routes every status/warning/success line to the
# facade whenever the process is quiet or non-interactive — which a piped
# subprocess always is. ``index catalog`` reaches none of them, so this case
# carries its own verb: ``ocx shell revoke`` is idempotent by contract, exits 0
# on a project that was never stamped, and says so through
# ``UserInterface::status``. It needs no registry, no package and no network.
CONSOLE_LINE = "nothing to revoke"
# ``ocx_oci`` left ``ocx_lib`` in WP-24 with the ``log::`` call sites of
# ``auth``, ``client`` and ``client::native_transport``. Every one but the auth
# lookup sits *behind* a wire response, so pinning one would need a live
# registry and a published package. ``get_docker_auth`` (``auth.rs``) resolves
# credentials *before* the request goes out, so it renders whether or not
# anything answers: ``package install`` against a closed loopback port reaches
# it, then fails the connect (exit 75 — see `EXPECT_NONZERO`). No registry
# fixture, no server, no network beyond a refused TCP connect to 127.0.0.1:1.
#
# Assumes the host has no native Docker credential entry for ``127.0.0.1:1`` —
# a port nothing can bind a registry to, which is why it is the address used.
OCI_LINE = "No native Docker credentials found for registry"
# ``ocx_trust`` left ``ocx_lib`` in WP-25 with two production ``log::debug!``
# sites, both in the policy compiler. The cheaper one is
# ``parse_verification_key``'s rejection reason: a ``--key`` is compiled before
# anything is fetched, so a PEM that is not a key renders the line with no
# registry, no package and no network at all. ``env://`` rather than a path so
# the case needs no file — the bad key rides ``extra_env`` the way
# ``ocx_util``'s invalid boolean does. Exits 65 (``DataError``), the code
# ``KeyMalformed { fault: ConfigText }`` already answered before the move.
TRUST_LINE = "rejected by sigstore"
# ``ocx_config`` left ``ocx_lib`` in WP-26 with ``config/**``, ``env.rs`` and
# ``managed_config/**`` — 39 ``log::`` call sites, the largest block the split
# has moved. Almost all of them need a tier file on disk or a managed-config
# snapshot to render; the cheapest does not. ``OCX_PROJECT=""`` takes the
# loader's escape hatch, which it logs before it reads anything at all, so the
# line renders with no registry, no package, no network and no file. The verb
# stays ``CATALOG`` and the run exits 0 — this row is deliberately **not** in
# `EXPECT_NONZERO`.
CONFIG_LINE = "OCX_PROJECT is set to empty string"
# ``ocx_index`` left ``ocx_lib`` in WP-28 with the whole resolution-index tier.
# Unlike ``ocx_store``'s rows it needs no on-disk state and no network: the
# ``index catalog`` verb this table already uses for three other crates enters
# the tier at ``Index::list_repositories`` and logs the registry it is about to
# enumerate *before* the enumeration that ``--offline`` then answers locally.
# Observed rendering on a fresh ``OCX_HOME``, not derived from reading the
# source: it is the only ``ocx_index`` line the verb emits.
INDEX_LINE = "Listing repositories for registry"
# ``ocx_store`` left ``ocx_lib`` in WP-27 with the ``log::`` call sites of the
# nine stores, ``reference_manager``, ``assemble``, ``hardlink`` and
# ``codesign``. Every one of them sits *behind* real on-disk state — an
# assembled package, a rendered toolchain home, a shim tree — so no bare verb
# reaches a single one on a fresh ``OCX_HOME``; that is why this row is the only
# one carrying a package. ``ReferenceManager::link`` is the cheapest of them:
# ``package install`` creates the candidate symlink through it and the line
# names both ends. ``codesign``'s ``OCX_NO_CODESIGN`` line — the one seam an env
# var alone would reach — is ``cfg!(target_os = "macos")``-gated and renders on
# no CI leg this suite runs on.
STORE_LINE = "Linking '"
# ``ocx_package`` left ``ocx_lib`` at WP-30 with twenty-four ``log::`` call
# sites over ten files. Like ``ocx_store``'s they all sit behind real state — a
# push, a pinned dependency, a description artifact — so this row carries a
# package too. ``package cascade repair`` is the cheapest: ``cascade::gather``
# logs the tag census it is about to reason over *before* deciding whether
# anything needs repairing, so the line renders whether or not the cascade is
# already intact, and the verb exits 0 either way. Observed on a fresh
# ``OCX_HOME`` against the suite's registry, not derived from reading the
# source.
PACKAGE_LINE = "Cascade gather for '"
# ``ocx_sign`` left ``ocx_lib`` at WP-31 with the whole signing tier. Only six
# of its call sites reach the ``log`` facade at all — the rest are ``tracing::``
# and invisible to this oracle — and five of the six are in the key backend,
# which is the half that runs *before* anything is fetched. A ``--key`` whose
# PEM claims cosign's ``ENCRYPTED SIGSTORE PRIVATE KEY`` envelope and is not one
# is rejected by sigstore, and the reason is logged rather than flattened into
# the ``MalformedKey`` message; so the line renders with no registry, no
# package, no network and no file. Exits 65 (``DataError``) — the row is in
# `EXPECT_NONZERO`. The PEM rides ``extra_env`` the way ``ocx_trust``'s bad key
# does, and the two lines overlap by the words "rejected by sigstore": that is
# harmless because each row asserts its own line under its own directive, and
# the longer one is the one pinned here.
SIGN_LINE = "encrypted key PEM rejected by sigstore"
#: Header cosign writes and `PemKeyBackend::from_encrypted_pem` dispatches on;
#: the body is deliberately not a key.
BAD_ENCRYPTED_KEY_PEM = (
    "-----BEGIN ENCRYPTED SIGSTORE PRIVATE KEY-----\n"
    "bm90IGEga2V5\n"
    "-----END ENCRYPTED SIGSTORE PRIVATE KEY-----\n"
)
#: Replaced with ``published_package.fq`` before the row runs. A row is a
#: literal argv everywhere else; this is the one value the table cannot know. It
#: is matched by identity, not equality, so it can never rewrite a coincidentally
#: equal argument.
PACKAGE = "<published package>"
#: The same device one column over: replaced with ``fake_forge.base_url`` in
#: ``extra_env`` before the row runs, and matched by identity for the same
#: reason. ``ocx_announce``'s only two ``log::`` lines sit behind a forge
#: round-trip -- ``resolve_author`` asks the token identity before the CI rung,
#: whatever ``--owner`` says -- so the crate has no argv-only route at all.
#: Measured, not assumed: ``package claim --out`` renders both lines and exits
#: 0, but reaches ``api.github.com`` doing it, and pointing the seam at a dead
#: port exits 69 with nothing rendered. A green bought from the public internet
#: is not one, so the row takes the fixture the six announce suites already use.
FORGE = "<fake forge base url>"
#: And the third, for the same reason: ``--out`` needs a directory that exists
#: and is this test's own. Replaced with a ``tmp_path`` child.
OUT_DIR = "<claim out dir>"

# One ``log::`` line per library crate hosting call sites, with the extra
# environment and the verb that make it render. A crate is added here by the
# commit that extracts it (DEC-3: targets follow the module path). A stale name
# renders no line and FAILS below — it never skips.
CATALOG = ("--offline", "index", "catalog")
# Rows whose verb cannot exit 0 here: the line under test renders *before* the
# wire call that then fails. Named one by one rather than relaxing `check` for
# the table — a tolerated exit range is how a row stops noticing that its verb
# broke for an unrelated reason.
# ``ocx_shell`` left ``ocx_lib`` at WP-32 with ``shell/**`` and ``ci/**`` — five
# log-calling files, and not one ``debug!`` among them. Its production levels are
# ``trace`` and ``warn``, and every ``warn`` sits behind composed env entries: the
# reconciler's A-10 admission gate needs a project carrying an installed package
# whose metadata declares an inadmissible entry, and the CI annotation warn sits
# after ``push``'s auth round-trip. The ``trace`` needs nothing — ``about``
# reports the detected shell, so ``Shell::detect``'s process walk runs on a fresh
# ``OCX_HOME`` with no project, no registry and no network, and exits 0. The loop
# entry rather than ``Detecting shell from path``: it is above the ``if let`` that
# the path line sits inside, so it renders whenever the walk runs at all.
# Observed rendering, not derived from reading the source.
SHELL_LINE = "Checking process with PID "
# ``ocx_project`` left ``ocx_lib`` at WP-33 with ``project/**`` flattened to the
# crate root plus ``activate``, ``ladder`` and ``lazy``. Its log calls all sit
# behind project state — a departed registry link, a lock whose clause-2
# corroboration fails — except this one: reading the consent stamp on a project
# that has none. ``shell state`` on a fresh ``OCX_HOME`` with an ``ocx.toml``
# reaches it with no network, no lock and no registry, and exits 0.
# Observed rendering, not derived from reading the source; ``ocx_store=debug``
# was confirmed NOT to select it, so the row proves the directive and not merely
# that the line exists.
PROJECT_LINE = "No usable consent stamp at "
# ``ocx_setup`` left ``ocx_lib`` at WP-36 with ``setup/**`` flattened to the
# crate root — and with every one of ``ocx_lib``'s remaining ``log::`` call
# sites, so this row changed target and nothing else. The line is emitted from
# what is now ``ocx_setup/src/lib.rs``, and it is still the only one reachable
# with no network and no fixture: ``config setup --managed-config ""`` takes
# the clear arm, which warns when ``OCX_MANAGED_CONFIG`` is still exported. A
# ``warn`` is above ``debug``, so the row needs no ``TRACE_TARGETS`` entry, and
# the verb exits 0. ``ocx_lib`` was deleted at WP-37, so there is no row for it
# — ``test_every_crate_that_logs_has_a_row`` reds on a row with no call site
# exactly as it does on a missing one.
# Observed rendering, not derived from reading the source.
LIBRARY_LINE = "OCX_MANAGED_CONFIG is still exported"
# ``ocx_announce`` left ``ocx_lib`` at WP-35 with ``announce/**``, ``claim/**``
# and ``forge/**``. Two ``log::`` call sites, both in the claim tier, and this
# is the one that does not depend on which rung answered: ``claim`` logs what it
# is about to write once the owner set is settled. ``--out`` keeps the run off
# the write path, so it exits 0 against the fake forge with no pull request
# opened. Observed rendering, not derived from reading the source.
ANNOUNCE_LINE = "claiming ocx.sh/acme/widget for"
EXPECT_NONZERO = {"ocx_oci", "ocx_trust", "ocx_sign"}
# Rows whose crate hosts no ``debug!`` at all. ``OCX_LOG=<crate>=debug`` selects
# that level *and above*, so a crate whose cheapest line is a ``warn`` needs no
# entry here — only one whose only state-free line is a ``trace`` does. Named as
# a set rather than by widening the whole table to ``trace``: a directive one
# level looser than the row needs stops the row proving the filter selects
# anything in particular.
TRACE_TARGETS = {"ocx_shell"}
#: Rows whose line needs an account to exist on the fake forge. Named as a set
#: rather than given a per-row setup hook, for the reason `EXPECT_NONZERO` and
#: `TRACE_TARGETS` are sets: the table stays four literal columns, and the one
#: row that departs from "argv and environment are all there is" says so by
#: name where a reader of the table will see it.
#:
#: `FakeForge.users` starts EMPTY by construction (DX-74) — `/user` answers the
#: credential's own identity while `GET /users/<login>` 404s — so a claim run
#: against it exits 79 (`OwnerUnknown`) before it logs anything. Seeding the
#: owner the row names is the whole of what this buys.
#:
#: The login is deliberately one that cannot resolve on real GitHub, and that
#: is load-bearing rather than cosmetic (DEC-96). The first draft named
#: `octocat:583231` — a *real* account — so a run that lost the
#: `__OCX_TESTING_FORGE_BASE_URL` override would have reached `api.github.com`,
#: resolved the owner there and gone green with the fixture doing nothing. A
#: row that passes with and without its fake forge is taking its green from the
#: public internet. With this login the same run 404s, so the fixture is proven
#: load-bearing by the row going red without it.
SEEDED_OWNERS = {"ocx_announce": ("ocx-sh-nonexistent-claim-fixture", 4242424242)}
LIBRARY_TARGETS = [
    (
        "ocx_setup",
        LIBRARY_LINE,
        {"OCX_MANAGED_CONFIG": "ghcr.io/ocx-sh/nothing:0"},
        ("config", "setup", "--managed-config", ""),
    ),
    ("ocx_util", UTIL_LINE, {"OCX_NO_CONFIG": "maybe"}, CATALOG),
    ("ocx_console", CONSOLE_LINE, {}, ("--offline", "shell", "revoke")),
    ("ocx_oci", OCI_LINE, {}, ("package", "install", "127.0.0.1:1/x:1")),
    (
        "ocx_trust",
        TRUST_LINE,
        {"__OCX_TESTING_BAD_KEY_PEM": "-----BEGIN PUBLIC KEY-----\nnot a key\n-----END PUBLIC KEY-----"},
        ("package", "verify", "--key", "env://__OCX_TESTING_BAD_KEY_PEM", "127.0.0.1:1/x:1"),
    ),
    ("ocx_config", CONFIG_LINE, {"OCX_PROJECT": ""}, CATALOG),
    ("ocx_index", INDEX_LINE, {}, CATALOG),
    ("ocx_package", PACKAGE_LINE, {}, ("package", "cascade", "repair", PACKAGE)),
    (
        "ocx_sign",
        SIGN_LINE,
        {"__OCX_TESTING_BAD_ENCRYPTED_KEY_PEM": BAD_ENCRYPTED_KEY_PEM},
        ("package", "sign", "--key", "env://__OCX_TESTING_BAD_ENCRYPTED_KEY_PEM", "127.0.0.1:1/x:1"),
    ),
    ("ocx_shell", SHELL_LINE, {}, ("about",)),
    ("ocx_project", PROJECT_LINE, {}, ("shell", "state")),
    (
        "ocx_announce",
        ANNOUNCE_LINE,
        # `OCX_DEFAULT_REGISTRY` pinned for the reason `test_package_claim.py`
        # pins it: the harness's own default is the compose registry, so the
        # canonical `ocx.sh` name the line renders would otherwise be whatever
        # the fixture happened to set.
        {"__OCX_TESTING_FORGE_BASE_URL": FORGE, "OCX_DEFAULT_REGISTRY": "ocx.sh"},
        (
            "package",
            "claim",
            "--repository",
            "oci://ghcr.io/acme/widget",
            "--owner",
            "ocx-sh-nonexistent-claim-fixture:4242424242",
            "--out",
            OUT_DIR,
            "acme/widget",
        ),
    ),
    ("ocx_package_manager", PACKAGE_MANAGER_LINE, {}, ("--offline", "clean")),
    ("ocx_store", STORE_LINE, {}, ("package", "install", PACKAGE)),
]
# The CLI crate is the ``ocx`` package (``[[bin]] name = "ocx"``), so its
# targets are ``ocx::…``. ``EnvFilter`` matches a target by string prefix, so
# a bare ``ocx=debug`` would enable ``ocx_setup`` too; the ``::`` pins the crate.
# (``ocx_cli`` is the crate's directory, not a target — a directive on it
# selects nothing at all and could not tell a library line from a CLI one.)
CLI_TARGET = "ocx::app"


def _stderr(ocx, argv: tuple[str, ...], check: bool = True, **env: str) -> str:
    return ocx.plain(*argv, env_overrides=env, check=check).stderr


# The crate whose targets are the CLI's own. Excluded from the derived set
# below because it is not a library row: its coverage is ``CLI_TARGET``, which
# every parametrised case asserts as its control.
CLI_CRATE = "ocx_cli"
LOG_MACRO = re.compile(r"\blog::(?:debug|info|warn|error|trace)!")


def _crates_with_log_sites() -> set[str]:
    """Every workspace crate whose ``src/`` calls the ``log`` facade."""
    crates = PROJECT_ROOT / "crates"
    found = set()
    for manifest in crates.glob("*/Cargo.toml"):
        src = manifest.parent / "src"
        if any(LOG_MACRO.search(f.read_text(encoding="utf-8", errors="replace")) for f in src.rglob("*.rs")):
            found.add(manifest.parent.name)
    return found


def test_every_crate_that_logs_has_a_row():
    """``LIBRARY_TARGETS`` is complete against the tree, not against a comment.

    The table said "a crate is added here by the commit that extracts it" and
    nothing checked it, so a crate that gained ``log::`` call sites — or one
    extracted without its row — was invisible. Derived rather than listed: the
    subject is every crate whose ``src/`` reaches the facade.
    """
    rows = {target for target, _, _, _ in LIBRARY_TARGETS}
    logging = _crates_with_log_sites()
    assert CLI_CRATE in logging, (
        f"{CLI_CRATE} calls no log macro, so this test is reading the wrong tree; found={sorted(logging)}"
    )
    assert len(logging) > 3, f"only {len(logging)} crate(s) found to log at all — the scan lost its subject"
    assert rows == logging - {CLI_CRATE}, (
        "LIBRARY_TARGETS and the tree disagree about which crates log: "
        f"missing a row {sorted(logging - {CLI_CRATE} - rows)}, "
        f"row with no call site {sorted(rows - logging)}"
    )


def test_debug_level_reaches_library_events(ocx):
    """``--log-level debug`` renders a library ``debug!`` line on stderr."""
    # ``INDEX_LINE``, not ``LIBRARY_LINE``: this asserts that *a* library is
    # reached, and the library this verb reaches is ``ocx_index``. Before WP-28
    # the two constants named the same line, so the distinction did not exist;
    # the extraction split them, and this test follows the verb it already had
    # rather than acquiring a second one.
    stderr = ocx.plain("--offline", "index", "catalog", log_level="debug").stderr
    assert INDEX_LINE in stderr, (
        f"no library debug line under --log-level debug; stderr={stderr!r}"
    )


@pytest.mark.parametrize(
    ("target", "line", "extra_env", "argv"), LIBRARY_TARGETS, ids=[t for t, _, _, _ in LIBRARY_TARGETS]
)
def test_env_filter_selects_library_target(request, ocx, tmp_path, target, line, extra_env, argv):
    """``OCX_LOG=<crate>=debug`` alone selects that crate's events, and a CLI-only directive leaves them out."""
    check = target not in EXPECT_NONZERO
    # Requested per row rather than taken by every row: building and pushing a
    # package costs a registry round-trip, and standing a fake forge up costs a
    # thread and a port, so only the rows that name the sentinel pay.
    if any(item is PACKAGE for item in argv):
        argv = tuple(request.getfixturevalue("published_package").fq if item is PACKAGE else item for item in argv)
    if any(item is OUT_DIR for item in argv):
        out = tmp_path / "claim-out"
        out.mkdir()
        argv = tuple(str(out) if item is OUT_DIR else item for item in argv)
    if any(value is FORGE for value in extra_env.values()):
        forge = request.getfixturevalue("fake_forge")
        if target in SEEDED_OWNERS:
            forge.seed_user(*SEEDED_OWNERS[target])
        extra_env = {key: (forge.base_url if value is FORGE else value) for key, value in extra_env.items()}
    # ``OcxRunner`` keeps the real ``HOME`` and ``PATH`` and sets no
    # ``DOCKER_CONFIG``, so credential resolution reads the developer's own
    # docker config. The ``ocx_oci`` row asserts the "no native credentials"
    # line, and a contributor with ``credsStore`` set takes the
    # ``HelperCommunicationError`` arm instead and reds on a clean tree. An
    # empty directory answers the same way on every host; set for every row
    # because no row wants the host's credentials.
    docker_config = tmp_path / "docker"
    docker_config.mkdir()
    extra_env = {**extra_env, "DOCKER_CONFIG": str(docker_config)}
    level = "trace" if target in TRACE_TARGETS else "debug"
    selected = _stderr(ocx, argv, check=check, OCX_LOG=f"{target}={level}", **extra_env)
    assert line in selected, (
        f"OCX_LOG={target}={level} rendered no library line — is {target!r} still a crate target?; "
        f"stderr={selected!r}"
    )
    assert CLI_LINE not in selected, (
        f"OCX_LOG={target}={level} also enabled CLI events; stderr={selected!r}"
    )

    # The control leg gets its own ``OCX_HOME``. Sharing one made the leak
    # assertion below unfalsifiable for any row whose work is idempotent:
    # ``ocx_store``'s ``ReferenceManager::link`` emits "Linking '" only when the
    # forward path is not already a symlink, so on the second run it takes the
    # "already up to date, skipping" arm and renders nothing whatever the filter
    # says. ``line not in cli_only`` then held because the line could not be
    # produced, not because the directive excluded it — green in both worlds.
    # A fresh home makes the second run do the same work as the first, so the
    # filter is the only thing that can keep the line out.
    control_home = tmp_path / "control-home"
    control_home.mkdir()
    cli_only = _stderr(
        ocx, argv, check=check,
        OCX_LOG=f"{CLI_TARGET}=debug", OCX_HOME=str(control_home), **extra_env,
    )
    assert CLI_LINE in cli_only, (
        f"OCX_LOG={CLI_TARGET}=debug rendered no CLI line — the control is dead; stderr={cli_only!r}"
    )
    assert line not in cli_only, (
        f"a CLI-only directive rendered the {target} line; stderr={cli_only!r}"
    )
