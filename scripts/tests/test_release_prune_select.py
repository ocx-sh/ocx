"""`release_prune_select.py`'s proofs: what each mode selects, and what it must never touch."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest
import release_prune_select as sel

NOW = datetime(2026, 11, 13, 12, 0, 0, tzinfo=UTC)
TAGS = [
    "0.7.0-dev",
    "0.7.0-dev_20260101000000",
    "0.7.0-dev_20261112000000",
    "0.6.0",
    "0.6.0_20260901000000",
    "0.6.0-rc1",
    "0.6.0-rc1_20260801000000",
    "0.6.0-rc1_20260820000000",
    "0.6.0-rc2_20260825000000",
    "0.7.0-rc1_20261110000000",
    "latest",
]


def test_dev_selects_only_old_build_tags_never_the_rolling_tag() -> None:
    assert sel.select_dev(TAGS, NOW - timedelta(days=30)) == [
        "0.7.0-dev_20260101000000"
    ]


def test_dev_with_zero_days_keeps_the_newest_build_of_each_line() -> None:
    # The rolling `X.Y.Z-dev` tag carries the newest build's digest; selecting that build makes
    # `ocx package prune` refuse with prune_shared_version.
    assert sel.select_dev(TAGS, NOW) == ["0.7.0-dev_20260101000000"]


def test_dev_never_selects_the_only_build_of_a_quiet_line() -> None:
    assert sel.select_dev(["0.7.0-dev", "0.7.0-dev_20200101000000"], NOW) == []


def test_dev_keeps_the_newest_build_per_version_line() -> None:
    tags = [
        "0.6.0-dev_20200101000000",
        "0.6.0-dev_20200201000000",
        "0.7.0-dev_20200101000000",
    ]
    assert sel.select_dev(tags, NOW) == ["0.6.0-dev_20200101000000"]


def test_candidates_need_their_final_release_to_be_old_enough() -> None:
    every_candidate_of_0_6_0 = [
        "0.6.0-rc1",
        "0.6.0-rc1_20260801000000",
        "0.6.0-rc1_20260820000000",
        "0.6.0-rc2_20260825000000",
    ]
    assert (
        sel.select_candidates(TAGS, NOW - timedelta(days=30))
        == every_candidate_of_0_6_0
    )
    assert sel.select_candidates(TAGS, NOW - timedelta(days=90)) == []


def test_every_candidate_of_an_old_enough_release_is_selected_with_its_rolling_tag() -> (
    None
):
    chosen = sel.select_candidates(TAGS, NOW)
    assert "0.6.0-rc1_20260820000000" in chosen, "the final is newer than every build"
    assert "0.6.0-rc1" in chosen, (
        "selected with its builds, so prune sees no shared version"
    )
    assert "0.6.0-rc2_20260825000000" in chosen, "a line of one build is selected too"
    assert "0.6.0-rc1_20260801000000" in chosen


def test_a_single_candidate_build_of_an_old_enough_release_is_selected() -> None:
    assert sel.select_candidates(
        ["1.0.0-rc1_20260101000000", "1.0.0_20260201000000"], NOW
    ) == ["1.0.0-rc1_20260101000000"]


def test_a_rolling_candidate_tag_with_no_build_is_selected() -> None:
    assert sel.select_candidates(
        ["0.6.0-rc3", "0.6.0_20260901000000"], NOW - timedelta(days=30)
    ) == ["0.6.0-rc3"]


def test_candidates_of_an_unreleased_version_are_never_selected() -> None:
    assert "0.7.0-rc1_20261110000000" not in sel.select_candidates(TAGS, NOW)


def test_candidates_never_include_the_stable_tags() -> None:
    assert {"0.6.0", "0.6.0_20260901000000", "latest"}.isdisjoint(
        sel.select_candidates(TAGS, NOW)
    )


def test_a_stamp_that_is_not_a_date_selects_nothing() -> None:
    assert sel.select_dev(["0.7.0-dev_20269999999999"], NOW) == []


def test_main_reads_stdin_and_rejects_negative_days(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    import io

    monkeypatch.setattr(
        "sys.stdin", io.StringIO("0.7.0-dev_20260101000000\n0.7.0-dev_20260102000000\n")
    )
    assert sel.main(["dev", "--days", "0", "--now", "2026-11-13T12:00:00+00:00"]) == 0
    assert capsys.readouterr().out.strip() == "0.7.0-dev_20260101000000"
    assert sel.main(["dev", "--days", "-1"]) == 64


def _run(
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
    kind: str,
    tags: list[str],
) -> str:
    import io

    monkeypatch.setattr("sys.stdin", io.StringIO("".join(f"{t}\n" for t in tags)))
    assert sel.main([kind, "--days", "30", "--now", NOW.isoformat()]) == 0
    return capsys.readouterr().out


@pytest.mark.parametrize("kind", ["dev", "candidates"])
def test_an_empty_selection_writes_zero_bytes(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str], kind: str
) -> None:
    # A blank line would reach `mapfile -t` in release.yml as one empty tag argument to `ocx package prune`.
    assert _run(monkeypatch, capsys, kind, ["0.7.0-dev_20261112000000", "latest"]) == ""


def test_dev_selection_is_newline_terminated_without_a_blank_line(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    assert _run(monkeypatch, capsys, "dev", TAGS) == "0.7.0-dev_20260101000000\n"


def test_candidate_selection_is_newline_terminated_without_a_blank_line(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    out = _run(
        monkeypatch,
        capsys,
        "candidates",
        ["0.6.0-rc1_20260801000000", "0.6.0_20260901000000"],
    )
    assert out == "0.6.0-rc1_20260801000000\n"


def test_a_garbage_now_is_a_usage_error_with_one_line(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    import io

    monkeypatch.setattr("sys.stdin", io.StringIO(""))
    assert sel.main(["dev", "--days", "1", "--now", "garbage"]) == 64
    err = capsys.readouterr().err
    assert err.startswith("release_prune_select:") and len(err.splitlines()) == 1


def test_a_naive_now_is_read_as_utc_whatever_the_local_zone(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    import io
    import time

    monkeypatch.setenv("TZ", "Pacific/Kiritimati")  # UTC+14
    time.tzset()
    try:
        # Cutoff 2026-11-13T00:00Z: both 2026-11-12 builds are older.
        tags = "0.7.0-dev_20261112000000\n0.7.0-dev_20261112120000\n0.7.0-dev_20261114000000\n"
        monkeypatch.setattr("sys.stdin", io.StringIO(tags))
        assert sel.main(["dev", "--days", "0", "--now", "2026-11-13T00:00:00"]) == 0
        assert (
            capsys.readouterr().out
            == "0.7.0-dev_20261112000000\n0.7.0-dev_20261112120000\n"
        )
    finally:
        monkeypatch.delenv("TZ")
        time.tzset()


def test_non_ascii_digits_are_not_a_build_stamp() -> None:
    arabic = "٠.٦.٠"
    assert sel.DEV_BUILD.match("0.7.0-dev_٢٠٢٦١١١٢٠٠٠٠٠٠") is None
    assert sel.DEV_BUILD.match(f"{arabic}-dev_20200101000000") is None
    assert sel.RELEASE_BUILD.match(f"{arabic}_20200101000000") is None
    assert sel.CANDIDATE.match(f"{arabic}-rc1") is None


@pytest.mark.parametrize(("age_days", "selected"), [(29, False), (30, True)])
def test_a_final_release_is_old_enough_at_exactly_prune_after_days(
    age_days: int, selected: bool
) -> None:
    built = (NOW - timedelta(days=age_days)).strftime("%Y%m%d%H%M%S")
    tags = ["1.0.0-rc1_20260101000000", f"1.0.0_{built}"]
    cutoff = NOW - timedelta(days=30)  # what `main` computes for `--days 30`
    assert sel.select_candidates(tags, cutoff) == (
        ["1.0.0-rc1_20260101000000"] if selected else []
    )


# --- canary and pr ---------------------------------------------------------------------------


def _stamp(age: timedelta) -> str:
    return (NOW - age).strftime("%Y%m%d%H%M%S")


def test_canary_keeps_the_newest_n_across_version_lines_and_closes_emptied_families() -> (
    None
):
    tags = [
        "0.6.0-canary",
        "0.6.0-canary_20260101000000",
        "0.6.0-canary_20260102000000",
        "0.7.0-canary",
        "0.7.0-canary_20260301000000",
        "0.7.0-canary_20260302000000",
        "0.7.0-canary_20260303000000",
    ]
    assert sel.select_canary(tags, 3) == [
        "0.6.0-canary",
        "0.6.0-canary_20260101000000",
        "0.6.0-canary_20260102000000",
    ]


@pytest.mark.parametrize("keep", [2, 3])
def test_canary_with_at_most_n_builds_selects_nothing(keep: int) -> None:
    tags = ["0.7.0-canary", "0.7.0-canary_20260301000000", "0.7.0-canary_20260302000000"]
    assert sel.select_canary(tags, keep) == []


def test_canary_family_without_a_rolling_tag_selects_only_builds() -> None:
    tags = ["0.6.0-canary_20260101000000", "0.7.0-canary_20260301000000"]
    assert sel.select_canary(tags, 1) == ["0.6.0-canary_20260101000000"]


def test_canary_never_selects_the_rolling_tag_of_a_family_that_keeps_a_build() -> None:
    tags = ["0.7.0-canary", "0.7.0-canary_20260101000000", "0.7.0-canary_20260301000000"]
    assert sel.select_canary(tags, 1) == ["0.7.0-canary_20260101000000"]


def test_canary_ignores_other_tracks_and_other_kinds_ignore_canary() -> None:
    other = ["0.6.0-dev_20200101000000", "0.6.0-pr12_20200101000000", "0.6.0-pr12"]
    assert sel.select_canary(other + ["0.6.0-canary_20260101000000"], 1) == []
    canary = ["0.6.0-canary_20200101000000", "0.6.0-canary_20200102000000", "0.6.0-canary"]
    assert sel.select_dev(canary + ["0.6.0-pr12_20200101000000"], NOW) == []
    assert sel.select_pr(canary, NOW, {12}) == []


def test_canary_ignores_a_stamp_that_is_not_a_date() -> None:
    assert sel.select_canary(["0.6.0-canary_20269999999999"], 1) == []


OPEN = {12}


def test_pr_closed_goes_whole() -> None:
    tags = ["0.7.0-pr7", f"0.7.0-pr7_{_stamp(timedelta(hours=1))}", "0.7.0-pr7_20261112000000"]
    assert sel.select_pr(tags, NOW - timedelta(days=30), OPEN) == sorted(tags)


def test_pr_open_with_young_builds_keeps_everything() -> None:
    tags = ["0.7.0-pr12", "0.7.0-pr12_20261112000000"]
    assert sel.select_pr(tags, NOW - timedelta(days=30), OPEN) == []


def test_pr_open_with_one_old_build_selects_only_that_build() -> None:
    tags = ["0.7.0-pr12", "0.7.0-pr12_20260101000000", "0.7.0-pr12_20261112000000"]
    assert sel.select_pr(tags, NOW - timedelta(days=30), OPEN) == [
        "0.7.0-pr12_20260101000000"
    ]


def test_pr_open_with_every_build_old_goes_whole_with_its_rolling_tag() -> None:
    tags = ["0.7.0-pr12", "0.7.0-pr12_20260101000000", "0.7.0-pr12_20260102000000"]
    assert sel.select_pr(tags, NOW - timedelta(days=30), OPEN) == sorted(tags)


@pytest.mark.parametrize(("age", "selected"), [(timedelta(days=30), True), (timedelta(days=30) - timedelta(seconds=1), False)])
def test_pr_build_is_old_enough_at_exactly_days(age: timedelta, selected: bool) -> None:
    tag = f"0.7.0-pr12_{_stamp(age)}"
    assert sel.select_pr([tag], NOW - timedelta(days=30), OPEN) == ([tag] if selected else [])


def test_pr_orphan_rolling_tag_is_selected_only_for_a_closed_pr() -> None:
    assert sel.select_pr(["0.7.0-pr7", "0.7.0-pr12"], NOW, OPEN) == ["0.7.0-pr7"]


def test_pr_with_no_open_prs_selects_every_pr_build() -> None:
    tags = ["0.7.0-pr12_20261112000000", "0.7.0-pr12"]
    assert sel.select_pr(tags, NOW - timedelta(days=30), set()) == sorted(tags)


def test_pr_ignores_what_is_not_a_pr_track() -> None:
    tags = [
        "0.7.0-pr0_20200101000000",
        "0.7.0-pr01_20200101000000",
        "0.7.0-pr0",
        "0.7.0-pr01",
        "0.7.0-pr١٢_20200101000000",
        "0.7.0-pr12_20269999999999",
        "0.7.0-dev_20200101000000",
    ]
    assert sel.select_pr(tags, NOW, set()) == []


def _run_args(
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
    args: list[str],
    tags: list[str],
) -> tuple[int, str, str]:
    import io

    monkeypatch.setattr("sys.stdin", io.StringIO("".join(f"{t}\n" for t in tags)))
    code = sel.main(args)
    out = capsys.readouterr()
    return code, out.out, out.err


def test_new_kinds_write_sorted_newline_terminated_output_or_zero_bytes(
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
    tmp_path: Path,
) -> None:
    open_file = tmp_path / "open.txt"
    open_file.write_text("\n12\n\n")
    tags = ["0.6.0-canary_20260102000000", "0.6.0-canary_20260101000000", "0.6.0-canary"]
    now = ["--now", NOW.isoformat()]
    assert _run_args(monkeypatch, capsys, ["canary", "--keep", "1"], tags) == (
        0,
        "0.6.0-canary_20260101000000\n",
        "",
    )
    assert _run_args(monkeypatch, capsys, ["canary", "--keep", "5"], tags) == (0, "", "")
    pr = ["pr", "--days", "30", "--open", str(open_file), *now]
    assert _run_args(monkeypatch, capsys, pr, ["0.7.0-pr7", "0.7.0-pr7_20260101000000"]) == (
        0,
        "0.7.0-pr7\n0.7.0-pr7_20260101000000\n",
        "",
    )
    assert _run_args(monkeypatch, capsys, pr, ["0.7.0-pr12_20261112000000", "latest"]) == (0, "", "")


def test_new_kinds_reject_bad_arguments_with_one_stderr_line(
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
    tmp_path: Path,
) -> None:
    garbage = tmp_path / "garbage.txt"
    garbage.write_text("12\nabc\n")
    zero = tmp_path / "zero.txt"
    zero.write_text("0\n")
    good = tmp_path / "good.txt"
    good.write_text("")
    bad = [
        ["canary"],
        ["canary", "--keep", "0"],
        ["canary", "--keep", "-1"],
        ["pr", "--days", "30"],
        ["pr", "--days", "30", "--open", str(tmp_path / "missing.txt")],
        ["pr", "--days", "30", "--open", str(garbage)],
        ["pr", "--days", "30", "--open", str(zero)],
        ["pr", "--days", "-1", "--open", str(good)],
    ]
    for args in bad:
        code, out, err = _run_args(monkeypatch, capsys, args, [])
        assert (code, out) == (64, ""), args
        assert err.startswith("release_prune_select:") and len(err.splitlines()) == 1, args
    assert _run_args(monkeypatch, capsys, ["pr", "--days", "30", "--open", str(good)], [])[0] == 0
