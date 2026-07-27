"""Unit tests for the Track D announce-E2E evidence module.

Pure logic, no registry, no network, no `ocx` binary — the whole point of
Key Decision D-2's split. Run narrowly with:

    cd test && OCX_TESTS_NO_REGISTRY=1 uv run pytest tests/test_announce_e2e_evidence.py -v
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from announce_e2e.evidence import (
    _TABLE_HEADER,
    MISSING,
    REDACTION,
    SCENARIOS,
    EvidenceRecord,
    LaneClassification,
    Redacted,
    _placeholder_row,
    assert_pr_union,
    classify_lane,
    classify_report,
    compute_latency_seconds,
    main,
    redact_secrets,
    render_evidence_markdown,
)

FIXTURES = Path(__file__).parent / "fixtures" / "announce_e2e"

#: The index bot's numeric actor id, as `BOT_ACTOR_IDS` carries it at run time.
#: Every fixture's bot events use this id.
BOT_IDS = (198765432,)


def fixture(name: str) -> dict | list:
    return json.loads((FIXTURES / f"{name}.json").read_text())


# ---------------------------------------------------------------------------
# classify_report — interface contract #4 (C6)
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    ("name", "expected"),
    [("report_unchanged", "unchanged"), ("report_updated", "updated")],
)
def test_classify_report_passes_through_contract_values(
    name: str, expected: str
) -> None:
    assert classify_report(fixture(name)) == expected


def test_classify_report_rejects_missing_status() -> None:
    with pytest.raises(ValueError):
        classify_report(fixture("report_missing_status"))


@pytest.mark.parametrize("garbage", ["created", "UNCHANGED", "", None, 0])
def test_classify_report_rejects_off_contract_status(garbage: object) -> None:
    with pytest.raises(ValueError):
        classify_report({"package": "acme/widget", "status": garbage})


# ---------------------------------------------------------------------------
# compute_latency_seconds
# ---------------------------------------------------------------------------


def test_compute_latency_seconds_known_delta() -> None:
    assert (
        compute_latency_seconds("2026-07-24T10:00:00Z", "2026-07-24T10:01:47Z") == 107.0
    )


def test_compute_latency_seconds_accepts_offset_form() -> None:
    assert (
        compute_latency_seconds("2026-07-24T10:00:00+00:00", "2026-07-24T10:00:30Z")
        == 30.0
    )


@pytest.mark.parametrize(
    ("start", "end"),
    [
        ("not-a-timestamp", "2026-07-24T10:00:00Z"),
        ("2026-07-24T10:00:00Z", "yesterday"),
        ("", "2026-07-24T10:00:00Z"),
    ],
)
def test_compute_latency_seconds_rejects_malformed(start: str, end: str) -> None:
    with pytest.raises(ValueError):
        compute_latency_seconds(start, end)


def test_compute_latency_seconds_rejects_end_before_start() -> None:
    with pytest.raises(ValueError):
        compute_latency_seconds("2026-07-24T10:01:00Z", "2026-07-24T10:00:00Z")


# ---------------------------------------------------------------------------
# classify_lane — Key Decision D-3 (numeric actor id, never the login string)
# ---------------------------------------------------------------------------


def test_classify_lane_machine_when_every_actor_id_is_a_bot() -> None:
    result = classify_lane(fixture("pr_events_machine_lane"), BOT_IDS)

    assert isinstance(result, LaneClassification)
    assert result.lane == "machine"
    assert result.human_click_detected is False
    assert result.actor_ids == [198765432]


def test_classify_lane_human_when_any_actor_id_is_not_a_bot() -> None:
    result = classify_lane(fixture("pr_events_human_lane"), BOT_IDS)

    assert result.lane == "human"
    assert result.human_click_detected is True
    assert 1234567 in result.actor_ids


def test_classify_lane_ignores_recycled_bot_login() -> None:
    """A human actor id posting under the bot's *login* must still read human.

    Register X7 names login recycling as a threat class; the client-side
    assertion must not reintroduce it by trusting `actor.login`.
    """
    events = fixture("pr_events_recycled_login")
    assert any(event.get("actor", {}).get("login") == "ocx-bot" for event in events)

    result = classify_lane(events, BOT_IDS)

    assert result.lane == "human"
    assert result.human_click_detected is True
    assert 77000111 in result.actor_ids


def test_classify_lane_treats_an_actorless_event_as_human() -> None:
    """Fail closed: an event with no numeric id proves nothing about the lane."""
    result = classify_lane([{"event": "merged", "actor": None}], BOT_IDS)

    assert result.lane == "human"
    assert result.human_click_detected is True


def test_classify_lane_empty_event_list_is_not_a_machine_merge() -> None:
    """No events = no evidence of a bot merge. Never report `machine` on silence."""
    result = classify_lane([], BOT_IDS)

    assert result.lane == "human"
    assert result.human_click_detected is True


# ---------------------------------------------------------------------------
# assert_pr_union — C4's contract
# ---------------------------------------------------------------------------


def test_assert_pr_union_superset() -> None:
    assert assert_pr_union(["1.0.0", "1.1.0", "latest"], ["1.0.0", "1.1.0"]) == (
        True,
        [],
    )


def test_assert_pr_union_reports_missing_tags() -> None:
    assert assert_pr_union(["1.0.0"], ["1.0.0", "1.1.0"]) == (False, ["1.1.0"])


def test_assert_pr_union_empty_expected_is_trivially_true() -> None:
    assert assert_pr_union([], []) == (True, [])


# ---------------------------------------------------------------------------
# redact_secrets — Key Decision D-4
# ---------------------------------------------------------------------------


def test_redact_secrets_replaces_every_occurrence() -> None:
    token = "ghp_liveTokenValue"
    text = f"Authorization: Bearer {token}\nretrying with {token}"

    out = redact_secrets(text, [token])

    assert token not in out
    assert out.count("***REDACTED***") == 2


def test_redact_secrets_reaches_into_a_url_query_string() -> None:
    token = "ghp_liveTokenValue"
    text = f"GET https://api.github.com/repos/ocx-sh/index/pulls?access_token={token}&per_page=1"

    out = redact_secrets(text, [token])

    assert token not in out
    assert "***REDACTED***&per_page=1" in out


def test_redact_secrets_ignores_an_empty_secret() -> None:
    """An unset `OCX_ANNOUNCE_TOKEN` must not redact the entire log."""
    text = "nothing secret here"

    assert redact_secrets(text, ["", None]) == text  # type: ignore[list-item]


def test_redact_secrets_returns_the_redacted_marker_type() -> None:
    assert isinstance(redact_secrets("x", ["y"]), Redacted)


# ---------------------------------------------------------------------------
# EvidenceRecord — D-4 enforced by type, not by convention
# ---------------------------------------------------------------------------


def _record(**overrides: object) -> EvidenceRecord:
    fields: dict = {
        "scenario": "sequenced",
        "status": "pass",
        "pr_url": Redacted("https://github.com/ocx-sh/index/pull/57"),
        "run_urls": [Redacted("https://github.com/ocx-sh/index/actions/runs/1")],
        "latency_seconds": 107.0,
        "notes": Redacted("validate.yml green"),
        "captured_at": "2026-07-24T10:02:00Z",
    }
    fields.update(overrides)
    return EvidenceRecord(**fields)


def test_evidence_record_accepts_redacted_free_text() -> None:
    assert _record().scenario == "sequenced"


@pytest.mark.parametrize(
    "overrides",
    [
        {"notes": "raw capture with a ghp_ token in it"},
        {"pr_url": "https://github.com/ocx-sh/index/pull/57"},
        {"run_urls": ["https://github.com/ocx-sh/index/actions/runs/1"]},
    ],
)
def test_evidence_record_rejects_unredacted_free_text(overrides: dict) -> None:
    """The render path cannot be handed raw text — the record refuses to hold it."""
    with pytest.raises(TypeError):
        _record(**overrides)


def test_evidence_record_accepts_a_null_pr_url() -> None:
    assert _record(pr_url=None).pr_url is None


# ---------------------------------------------------------------------------
# render_evidence_markdown
# ---------------------------------------------------------------------------


def test_render_includes_scenario_and_status_of_every_record() -> None:
    out = render_evidence_markdown(
        [_record(), _record(scenario="idempotency", status="fail")]
    )

    assert "sequenced" in out
    assert "idempotency" in out
    assert "pass" in out
    assert "fail" in out


def test_render_writes_missing_for_a_none_field() -> None:
    out = render_evidence_markdown([_record(pr_url=None, latency_seconds=None)])

    assert MISSING in out
    assert "None" not in out


def test_render_of_an_empty_set_still_lists_every_scenario() -> None:
    """The bare template: five placeholder rows, every cell MISSING."""
    out = render_evidence_markdown([])

    for scenario in (
        "sequenced",
        "idempotency",
        "machine_lane",
        "update_union",
        "clean_install",
    ):
        assert scenario in out
    assert out.count(MISSING) >= 5


# ---------------------------------------------------------------------------
# CLI exit codes — the machine-lane driver branches on these, not on JSON text
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    ("name", "expected_exit"),
    [
        ("pr_events_machine_lane", 0),
        ("pr_events_human_lane", 1),
        ("pr_events_recycled_login", 1),
    ],
)
def test_classify_lane_require_machine_exit_code(
    name: str, expected_exit: int, capsys: pytest.CaptureFixture[str]
) -> None:
    exit_code = main(
        [
            "classify-lane",
            "--file",
            str(FIXTURES / f"{name}.json"),
            "--bot-actor-ids",
            "198765432",
            "--require-machine",
        ]
    )
    capsys.readouterr()

    assert exit_code == expected_exit


def test_secrets_arrive_by_env_name_never_by_value(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    """A secret on argv is readable from /proc/<pid>/cmdline, so the CLI takes
    the variable NAME and reads the value itself."""
    monkeypatch.setenv("E2E_FAKE_TOKEN", "ghp_notreal")
    out = tmp_path / "x.record.json"

    main(
        [
            "record",
            "--scenario",
            "sequenced",
            "--status",
            "pass",
            "--notes",
            "leaked ghp_notreal into the log",
            "--secrets-env",
            "E2E_FAKE_TOKEN",
            "--out",
            str(out),
        ]
    )
    capsys.readouterr()

    written = out.read_text()
    assert "ghp_notreal" not in written
    assert REDACTION in written


def test_an_unredacted_run_must_be_stated_not_defaulted() -> None:
    """Neither flag is an error: an empty secret list can never happen by
    omission, only by an explicit --no-secrets."""
    with pytest.raises(SystemExit):
        main(["redact"])


def test_the_committed_results_table_is_one_the_renderer_could_have_produced() -> None:
    """The committed artifact's table stays renderer-shaped once it is filled.

    Track E and Track F read that file; if the renderer and the artifact drift,
    a filled-in run stops being comparable to the skeleton it replaced. Real
    records make byte-equality with `render_evidence_markdown([])` impossible,
    so what is pinned is what the renderer decides regardless of run data: the
    header, every scenario present exactly once, seven cells per row, and the
    rule that an uncaptured scenario is a whole MISSING row rather than a
    half-filled one.

    Row *order* is deliberately not pinned. The renderer emits captured records
    in the order they were read and only appends placeholders in `SCENARIOS`
    order, so a filled table is ordered by record filename — pinning that would
    fail the moment a scenario is recaptured under a new run id.
    """
    artifact = (
        Path(__file__).parents[2] / ".claude" / "artifacts" / "e2e_results_announce.md"
    )
    body = artifact.read_text().split("<!-- BEGIN evidence table")[1]
    table = body.split("-->\n", 1)[1].split("<!-- END evidence table -->")[0]

    assert table.startswith(_TABLE_HEADER)
    rows = table[len(_TABLE_HEADER) :].splitlines(keepends=True)
    scenarios = [_cells(row)[0] for row in rows]
    assert sorted(scenarios) == sorted(SCENARIOS), (
        "every scenario appears exactly once, and nothing else does"
    )

    for scenario, row in zip(scenarios, rows, strict=True):
        cells = _cells(row)
        assert len(cells) == 7, f"{scenario} row has {len(cells)} cells, not 7"
        # A captured row may still carry MISSING in a field the run had no
        # value for (idempotency records no pull request), so only the status
        # cell distinguishes a placeholder — and a placeholder is all-MISSING.
        if cells[1] == MISSING:
            assert row == _placeholder_row(scenario)


def _cells(row: str) -> list[str]:
    return row.rstrip("\n").removeprefix("| ").removesuffix(" |").split(" | ")
