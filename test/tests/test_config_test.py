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

from src.helpers import push_managed_config
from src.runner import OcxRunner

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


def test_config_test_previews_merge_of_candidate_onto_machine_tiers(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """The report is the EFFECTIVE config: candidate values appear, and machine
    values the candidate does not override survive."""
    _write_home_config(
        ocx,
        '[registry]\n'
        'default = "machine.example"\n'
        '\n'
        '[mirrors."ghcr.io"]\n'
        'registry = "https://mirror.machine/ghcr"\n',
    )
    candidate = _write_candidate(
        tmp_path,
        '[registries."corp.example.com"]\n'
        'index = "https://index.corp.example.com"\n'
        '\n'
        '[mirrors."quay.io"]\n'
        'registry = "https://mirror.corp/quay"\n'
        '\n'
        '[patches]\n'
        'registry = "corp.example.com/ocx-patches"\n'
        'required = false\n',
    )

    report = ocx.json("config", "test", str(candidate))

    assert report["valid"] is True
    assert Path(report["candidate"]).name == "candidate.toml"
    assert report["registry_default"] == "machine.example", (
        "a machine value the candidate does not set must survive the merge"
    )
    assert "corp.example.com" in report["registries"], "the candidate's registry entry must appear"
    assert set(report["mirrors"]) >= {"ghcr.io", "quay.io"}, (
        "both the machine mirror and the candidate mirror must appear in the effective view"
    )
    assert report["patches"]["registry"] == "corp.example.com/ocx-patches"
    assert report["patches"]["required"] is False
    assert report["patches"]["path_template"] == "{registry}/{repository}", (
        "an omitted path template must be reported as the default that would apply"
    )


def test_config_test_candidate_overrides_machine_value(ocx: OcxRunner, tmp_path: Path) -> None:
    """Where candidate and machine tier both set a value, the candidate wins —
    that is what adopting the payload would do. Base-tier fidelity; the
    companion of the overlay test below."""
    _write_home_config(ocx, '[registry]\ndefault = "machine.example"\n')
    candidate = _write_candidate(tmp_path, '[registry]\ndefault = "corp.example.com"\n')

    report = ocx.json("config", "test", str(candidate))

    assert report["registry_default"] == "corp.example.com"


def test_config_test_config_overlay_outranks_the_candidate(ocx: OcxRunner, tmp_path: Path) -> None:
    """An explicit `--config`/`OCX_CONFIG` overlay outranks the managed tier, so
    it outranks the candidate here too — the preview reproduces the adoption
    fold order (base, then candidate, then overlay) rather than approximating
    it. Both keys are set on both sides so the report cannot pass by accident.
    """
    _write_home_config(ocx, '[registry]\ndefault = "machine.example"\n')
    overlay = tmp_path / "overlay.toml"
    overlay.write_text(
        '[registry]\n'
        'default = "overlay.example"\n'
        '\n'
        '[patches]\n'
        'registry = "overlay.example/patches"\n'
    )
    candidate = _write_candidate(
        tmp_path,
        '[registry]\n'
        'default = "corp.example.com"\n'
        '\n'
        '[patches]\n'
        'registry = "corp.example.com/patches"\n',
    )

    report = ocx.json(
        "config", "test", str(candidate), env_overrides={"OCX_CONFIG": str(overlay)}
    )

    assert report["registry_default"] == "overlay.example", (
        "an explicit overlay outranks an adopted payload, so it must outrank the candidate"
    )
    assert report["patches"]["registry"] == "overlay.example/patches"


def test_config_test_candidate_still_wins_where_the_overlay_is_silent(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """The overlay only outranks keys it actually sets — otherwise the test
    above would pass on a preview that ignored the candidate entirely."""
    overlay = tmp_path / "overlay.toml"
    overlay.write_text('[registry]\ndefault = "overlay.example"\n')
    candidate = _write_candidate(
        tmp_path, '[patches]\nregistry = "corp.example.com/patches"\n'
    )

    report = ocx.json(
        "config", "test", str(candidate), env_overrides={"OCX_CONFIG": str(overlay)}
    )

    assert report["registry_default"] == "overlay.example"
    assert report["patches"]["registry"] == "corp.example.com/patches"


def test_config_test_reports_the_machines_managed_posture(ocx: OcxRunner, tmp_path: Path) -> None:
    """The candidate can never carry `[managed]`, so the reported tier posture is
    the machine's own — which tier would adopt this payload, and whether an
    unsynced snapshot fails commands closed."""
    candidate = _write_candidate(tmp_path, '[registry]\ndefault = "corp.example.com"\n')

    report = ocx.json(
        "config",
        "test",
        str(candidate),
        env_overrides={"OCX_MANAGED_CONFIG": "corp.example.com/ocx-config:user"},
    )

    assert report["managed"]["source"] == "corp.example.com/ocx-config:user"
    assert report["managed"]["required"] is True, "the tier defaults to fail-closed"


def test_config_test_reports_an_opted_out_managed_posture(ocx: OcxRunner, tmp_path: Path) -> None:
    """The `required` value is read from the machine's seed, not assumed — the
    fail-closed default above would pass on a hardcoded report too."""
    _write_home_config(
        ocx,
        '[managed]\nsource = "corp.example.com/ocx-config:user"\nrequired = false\n',
    )
    candidate = _write_candidate(tmp_path, '[registry]\ndefault = "corp.example.com"\n')

    report = ocx.json("config", "test", str(candidate))

    assert report["managed"]["source"] == "corp.example.com/ocx-config:user"
    assert report["managed"]["required"] is False


def test_config_test_reports_no_managed_tier_when_unconfigured(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    candidate = _write_candidate(tmp_path, '[registry]\ndefault = "corp.example.com"\n')

    report = ocx.json("config", "test", str(candidate))

    assert report["managed"] is None


def test_config_test_plain_output_is_a_field_value_table(ocx: OcxRunner, tmp_path: Path) -> None:
    candidate = _write_candidate(tmp_path, '[registry]\ndefault = "corp.example.com"\n')

    result = ocx.plain("config", "test", str(candidate))

    assert result.returncode == 0, result.stderr
    assert "Field" in result.stdout and "Value" in result.stdout
    assert "corp.example.com" in result.stdout
    assert "Valid" not in result.stdout, (
        "reaching plain output at all is the verdict; a constant row is noise"
    )


def test_config_test_plain_repeats_the_field_name_on_every_row(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Multi-valued fields label every row, so a line stays readable when the
    output is grepped or sliced."""
    candidate = _write_candidate(
        tmp_path,
        '[registries."corp.example.com"]\nindex = "https://index.corp.example.com"\n',
    )

    result = ocx.plain("config", "test", str(candidate))

    registry_rows = [line for line in result.stdout.splitlines() if line.startswith("Registries")]
    assert len(registry_rows) >= 2, (
        f"both the candidate entry and the built-in ocx.sh entry must be labelled: {result.stdout}"
    )


# ---------------------------------------------------------------------------
# Rejections (exit 78) — shared with `ocx config push`
# ---------------------------------------------------------------------------


def test_config_test_rejects_managed_section_exit_78(ocx: OcxRunner, tmp_path: Path) -> None:
    candidate = _write_candidate(tmp_path, '[managed]\nsource = "corp.example.com/ocx-config:user"\n')

    result = ocx.run("config", "test", str(candidate), check=False)

    assert result.returncode == 78, result.stderr
    assert "[managed]" in result.stderr


def test_config_test_rejects_invalid_toml_exit_78(ocx: OcxRunner, tmp_path: Path) -> None:
    candidate = _write_candidate(tmp_path, "not = [valid\n")

    result = ocx.run("config", "test", str(candidate), check=False)

    assert result.returncode == 78, result.stderr


def test_config_test_rejects_oversize_payload_exit_78(ocx: OcxRunner, tmp_path: Path) -> None:
    """The 64 KiB cap is the same one the consumer-side fetch enforces — an
    oversize payload could never be adopted, so previewing it is refused."""
    candidate = _write_candidate(tmp_path, "# padding\n" * 7_000)  # ~70 KiB > 64 KiB

    result = ocx.run("config", "test", str(candidate), check=False)

    assert result.returncode == 78, result.stderr


def test_config_test_missing_candidate_exits_79(ocx: OcxRunner, tmp_path: Path) -> None:
    result = ocx.run("config", "test", str(tmp_path / "absent.toml"), check=False)

    assert result.returncode == 79, result.stderr


# ---------------------------------------------------------------------------
# Resolution gates — a payload can parse and still be unusable
# ---------------------------------------------------------------------------


def test_config_test_rejects_plain_http_mirror_exit_78(ocx: OcxRunner, tmp_path: Path) -> None:
    """Valid TOML, valid schema, but no machine can start under it: the mirror
    asks for plain HTTP without the host being allowed. `config test` runs the
    same mirror resolution every ocx invocation runs against its own config."""
    candidate = _write_candidate(
        tmp_path,
        '[mirrors."ghcr.io"]\nregistry = "http://mirror.corp"\n',
    )

    result = ocx.run(
        "config",
        "test",
        str(candidate),
        check=False,
        # The runner allows the test registry by default; the mirror host here
        # is deliberately NOT in that list.
        env_overrides={"OCX_INSECURE_REGISTRIES": ""},
    )

    assert result.returncode == 78, result.stderr
    assert "OCX_INSECURE_REGISTRIES" in result.stderr, (
        f"the error must name the plain-HTTP gate: {result.stderr!r}"
    )


def test_config_test_allowed_plain_http_mirror_passes(ocx: OcxRunner, tmp_path: Path) -> None:
    """The gate's other side: the same payload is fine once the mirror host is
    allowed, so the refusal above is the gate firing and not the command
    rejecting every `[mirrors]` entry."""
    candidate = _write_candidate(
        tmp_path,
        '[mirrors."ghcr.io"]\nregistry = "http://mirror.corp"\n',
    )

    report = ocx.json(
        "config", "test", str(candidate), env_overrides={"OCX_INSECURE_REGISTRIES": "mirror.corp"}
    )

    assert "ghcr.io" in report["mirrors"]


def test_config_test_plain_http_mirror_allowed_by_candidate_registries_entry(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """The candidate's own ``[registries."<host>"] insecure = true`` entry
    licenses its plain-HTTP mirror on its own, with ``OCX_INSECURE_REGISTRIES``
    explicitly emptied so the config half cannot hide behind an inherited env
    grant. The pair above drives only the env half of the union
    (``OCX_INSECURE_REGISTRIES: ""`` vs. ``"mirror.corp"``) — this is the
    config half, exercised end to end including the ``plain_http`` report
    field."""
    candidate = _write_candidate(
        tmp_path,
        '[mirrors."ghcr.io"]\nregistry = "http://mirror.corp"\n'
        '[registries."mirror.corp"]\ninsecure = true\n',
    )

    report = ocx.json(
        "config", "test", str(candidate), env_overrides={"OCX_INSECURE_REGISTRIES": ""}
    )

    assert "ghcr.io" in report["mirrors"]
    assert report["plain_http"] == ["mirror.corp"]


def test_config_test_rejects_empty_patch_registry_exit_78(ocx: OcxRunner, tmp_path: Path) -> None:
    """An empty `[patches] registry` is a no-op tier that would silently skip
    every companion — refused at resolve time, and the command's help promises
    78 for a payload that is not publishable."""
    candidate = _write_candidate(tmp_path, '[patches]\nregistry = ""\n')

    result = ocx.run("config", "test", str(candidate), check=False)

    assert result.returncode == 78, (
        f"a malformed [patches] tier must classify as a config error: {result.stderr}"
    )


# ---------------------------------------------------------------------------
# `[patches]` precedence
# ---------------------------------------------------------------------------


def test_config_test_falls_back_to_the_env_patch_tier(ocx: OcxRunner, tmp_path: Path) -> None:
    """No `[patches]` anywhere in config → the forwarded `OCX_PATCHES` tier is
    what the machine resolves, so it is what the preview reports."""
    candidate = _write_candidate(tmp_path, '[registry]\ndefault = "corp.example.com"\n')

    report = ocx.json(
        "config",
        "test",
        str(candidate),
        env_overrides={
            "OCX_PATCHES": '{"registry":"env.example.com/patches","path_template":"{registry}/{repository}","required":true}'
        },
    )

    assert report["patches"]["registry"] == "env.example.com/patches"


def test_config_test_candidate_patches_outrank_the_env_tier(ocx: OcxRunner, tmp_path: Path) -> None:
    """Config tier beats the env tier — the same precedence an adopted payload
    would get."""
    candidate = _write_candidate(tmp_path, '[patches]\nregistry = "corp.example.com/patches"\n')

    report = ocx.json(
        "config",
        "test",
        str(candidate),
        env_overrides={
            "OCX_PATCHES": '{"registry":"env.example.com/patches","path_template":"{registry}/{repository}","required":true}'
        },
    )

    assert report["patches"]["registry"] == "corp.example.com/patches"


# ---------------------------------------------------------------------------
# Unknown-key warnings (exit 0)
# ---------------------------------------------------------------------------


def test_config_test_reports_unknown_keys_and_still_exits_zero(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A typo'd section or key is silently ignored by the loader (fleet
    forward-compat). `config test` is the one place it is surfaced — as a
    warning, never a failure: an unknown key may equally be a setting a newer
    ocx understands."""
    candidate = _write_candidate(
        tmp_path,
        '[patchs]\n'
        'registry = "corp.example.com/ocx-patches"\n'
        '\n'
        '[registry]\n'
        'defalt = "corp.example.com"\n',
    )

    result = ocx.run("config", "test", str(candidate), check=False)
    assert result.returncode == 0, result.stderr

    report = ocx.json("config", "test", str(candidate))
    assert report["valid"] is True
    assert "patchs" in report["unknown_keys"], f"typo'd section must be listed: {report['unknown_keys']}"
    assert "registry.defalt" in report["unknown_keys"], (
        f"a typo'd key must be listed by its full path: {report['unknown_keys']}"
    )
    assert report["registry_default"] is None, (
        "the typo means nothing was actually set — the preview must not pretend otherwise"
    )


def test_config_test_does_not_check_keys_inside_a_mirrors_entry(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """KNOWN LIMITATION PIN, not a desired behaviour.

    A `[mirrors."<host>"]` entry is parsed value-first from a raw TOML value, so
    the schema never ignores anything inside it and a misspelled role is
    silently dropped. This test pins the gap so a future change to the mirrors
    deserializer shows up here as a deliberate decision rather than a surprise.
    The help text and the `unknown_keys` field doc both carry the caveat.
    """
    candidate = _write_candidate(
        tmp_path,
        '[mirrors."ghcr.io"]\nregistry = "https://mirror.corp/ghcr"\nregsitry = "https://typo.example"\n',
    )

    report = ocx.json("config", "test", str(candidate))

    assert report["unknown_keys"] == [], (
        "if this now reports the typo, the limitation is fixed - update the help text, "
        f"the unknown_keys field doc and this test: {report['unknown_keys']}"
    )
    assert "ghcr.io" in report["mirrors"], "the correctly spelled role must still take effect"


def test_config_test_clean_payload_reports_no_unknown_keys(ocx: OcxRunner, tmp_path: Path) -> None:
    """The negative twin of the test above: the warning list is empty for a
    payload with no typos, so a non-empty list is real signal."""
    candidate = _write_candidate(tmp_path, '[registry]\ndefault = "corp.example.com"\n')

    report = ocx.json("config", "test", str(candidate))

    assert report["unknown_keys"] == []


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


def test_config_test_finds_the_key_the_ordinary_command_ignores(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    candidate = _write_candidate(tmp_path, _TYPO_CONFIG)

    report = ocx.json("config", "test", str(candidate))

    assert "registry.timeuot" in report["unknown_keys"], (
        f"the same key the loader ignores must be surfaced here: {report['unknown_keys']}"
    )
