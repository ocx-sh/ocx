"""Pure classification / latency / redaction / render logic for the Track D
announce E2E gate.

Deliberately network-free: every function here takes already-fetched JSON
(`gh api` output, an `ocx package announce --format json` report) so the whole
module is unit-testable. Fetching is the bash layer's job — see
`test/manual/announce-e2e/scripts/`.

Import path: this package lives under `test/src/`, which pytest puts on
`sys.path` via `pythonpath` in `test/pyproject.toml`. Outside pytest, set
`PYTHONPATH=<repo>/test/src` (the scripts do).
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from collections.abc import Collection, Iterable, Sequence
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import Literal

REDACTION = "***REDACTED***"

#: Scenario names the results artifact carries a row for, in render order.
SCENARIOS: tuple[str, ...] = (
    "sequenced",
    "idempotency",
    "machine_lane",
    "update_union",
    "clean_install",
)

#: Rendered in place of any `None` field so an incomplete evidence set is
#: visibly incomplete rather than silently short.
MISSING = "MISSING"

#: The two values `ocx package announce --format json` may report.
_ANNOUNCE_STATUS: tuple[str, ...] = ("unchanged", "updated")

_TABLE_HEADER = (
    "| Scenario | Status | Pull Request | Runs | Latency (s) | Captured At | Notes |\n"
    "|---|---|---|---|---|---|---|\n"
)


class Redacted(str):
    """Text that has been through :func:`redact_secrets`.

    A marker type, not a container: `EvidenceRecord` refuses free-text fields
    that are not `Redacted`, so an unredacted capture cannot reach
    :func:`render_evidence_markdown` by forgetting a call (Key Decision D-4).
    Constructing `Redacted(raw)` by hand defeats that — it is deliberate,
    greppable, and reviewed for in Step 5.1.
    """

    __slots__ = ()


@dataclass(frozen=True, slots=True)
class LaneClassification:
    lane: Literal["human", "machine"]
    human_click_detected: bool
    actor_ids: list[int]  # numeric actor ids only — see Key Decision D-3


@dataclass(frozen=True, slots=True)
class EvidenceRecord:
    scenario: str  # one of SCENARIOS
    status: Literal["pass", "fail"]
    pr_url: Redacted | None
    run_urls: list[Redacted]
    latency_seconds: float | None
    notes: Redacted
    captured_at: str  # ISO-8601 UTC

    def __post_init__(self) -> None:
        """Refuse free text that has not been through :func:`redact_secrets`.

        The results artifact is committed and read later by Tracks E and F, so
        a live credential must not be able to reach it. Enforcing that here —
        at the only door into the render path — beats a convention every
        caller has to remember (Key Decision D-4).
        """
        if not isinstance(self.notes, Redacted):
            raise TypeError(
                "EvidenceRecord.notes must be redacted (see redact_secrets)"
            )
        if self.pr_url is not None and not isinstance(self.pr_url, Redacted):
            raise TypeError(
                "EvidenceRecord.pr_url must be redacted (see redact_secrets)"
            )
        for url in self.run_urls:
            if not isinstance(url, Redacted):
                raise TypeError(
                    "every EvidenceRecord.run_urls entry must be redacted (see redact_secrets)"
                )


def classify_report(report: dict) -> Literal["unchanged", "updated"]:
    """`ocx package announce --format json` report dict in; its `status`
    field out. Raises ValueError if the field is absent or not one of
    the two contract values (interface contract #4, C6)."""
    if "status" not in report:
        raise ValueError(f"announce report carries no `status` field: {sorted(report)}")
    status = report["status"]
    if status not in _ANNOUNCE_STATUS:
        raise ValueError(
            f"announce report `status` is {status!r}, not one of {_ANNOUNCE_STATUS}"
        )
    return status


def compute_latency_seconds(start_iso: str, end_iso: str) -> float:
    """ISO-8601 UTC in, elapsed seconds out. Raises ValueError on
    unparseable input or end < start."""
    start = _parse_iso_utc(start_iso, "start")
    end = _parse_iso_utc(end_iso, "end")
    if end < start:
        raise ValueError(f"end {end_iso!r} precedes start {start_iso!r}")
    return (end - start).total_seconds()


def _parse_iso_utc(value: str, label: str) -> datetime:
    try:
        parsed = datetime.fromisoformat(value)
    except (TypeError, ValueError) as error:
        raise ValueError(f"{label} timestamp {value!r} is not ISO-8601") from error
    # `gh api` emits `...Z`; a naive value can only have come from a UTC field.
    return parsed if parsed.tzinfo else parsed.replace(tzinfo=UTC)


def classify_lane(
    pr_events: list[dict], bot_actor_ids: Collection[int]
) -> LaneClassification:
    """Merged `gh api .../pulls/<n>/reviews` + `.../issues/<n>/events`
    list in. `machine` lane requires every state-changing event's
    `actor.id` (numeric, never `actor.login`) to be in the given
    bot-id allowlist (`BOT_ACTOR_IDS` in `env.sh`).

    Fail-closed on three counts: an event whose actor carries no numeric id
    reads as human, an empty event list reads as human (silence is not proof
    of a bot merge), and *every* event counts — a human comment on a PR the
    bot merged still falsifies "no human touched this".

    ponytail: no event-type taxonomy. If a future lane proof needs to
    distinguish state-changing events from chatter, filter `pr_events` in the
    caller — widening this function's trust is the wrong direction.
    """
    bot_ids = set(bot_actor_ids)
    actor_ids: list[int] = []
    human_click_detected = not pr_events

    for event in pr_events:
        actor_id = _actor_id(event)
        if actor_id is None:
            human_click_detected = True
            continue
        if actor_id not in actor_ids:
            actor_ids.append(actor_id)
        if actor_id not in bot_ids:
            human_click_detected = True

    return LaneClassification(
        lane="human" if human_click_detected else "machine",
        human_click_detected=human_click_detected,
        actor_ids=actor_ids,
    )


def _actor_id(event: dict) -> int | None:
    """The numeric id of whoever produced this event, or None.

    Issue events name them `actor`, review submissions name them `user`.
    `login` is never read — it is renameable and recyclable (D-3, register X7).
    """
    for key in ("actor", "user"):
        who = event.get(key)
        if isinstance(who, dict) and isinstance(who.get("id"), int):
            return who["id"]
    return None


def assert_pr_union(
    committed_tags: Sequence[str], expected_tags: Sequence[str]
) -> tuple[bool, list[str]]:
    """committed_tags = tags currently on the PR branch's root;
    expected_tags = union of every announce run's tag set so far.
    Returns (is_superset, missing_tags) — C4's contract."""
    committed = set(committed_tags)
    missing = [tag for tag in dict.fromkeys(expected_tags) if tag not in committed]
    return not missing, missing


def redact_secrets(text: str, secrets: Iterable[str]) -> Redacted:
    """Replace every occurrence of every non-empty string in `secrets`
    with `***REDACTED***`. Must be called on every captured log/output
    before it reaches render_evidence_markdown (Key Decision D-4)."""
    # Longest first: a secret containing a shorter one is masked whole rather
    # than left as `***REDACTED***…tail`.
    for secret in sorted((s for s in secrets if s), key=len, reverse=True):
        text = text.replace(secret, REDACTION)
    return Redacted(text)


def render_evidence_markdown(records: Sequence[EvidenceRecord]) -> str:
    """Render the frozen results-artifact template (see
    `.claude/artifacts/e2e_results_announce.md`) from a list of
    per-scenario records. A `None` field renders as the literal string
    `MISSING` so an incomplete evidence set is visibly incomplete."""
    rows = [_row(record) for record in records]
    covered = {record.scenario for record in records}
    rows.extend(_placeholder_row(name) for name in SCENARIOS if name not in covered)
    return _TABLE_HEADER + "".join(rows)


def _row(record: EvidenceRecord) -> str:
    cells = (
        record.scenario,
        record.status,
        _cell(record.pr_url),
        ", ".join(record.run_urls) if record.run_urls else MISSING,
        _cell(record.latency_seconds),
        _cell(record.captured_at),
        _cell(record.notes),
    )
    return "| " + " | ".join(cells) + " |\n"


def _placeholder_row(scenario: str) -> str:
    return "| " + " | ".join((scenario, *([MISSING] * 6))) + " |\n"


def _cell(value: object) -> str:
    """A field's markdown cell — `MISSING` for None, never an empty cell."""
    if value is None or value == "":
        return MISSING
    return str(value)


# ---------------------------------------------------------------------------
# CLI dispatcher — lets the bash drivers call this module without a second
# language boundary: `uv run python -m announce_e2e.evidence <subcommand>`.
# ---------------------------------------------------------------------------


def _read_json(path: str | None) -> object:
    raw = sys.stdin.read() if path in (None, "-") else Path(path).read_text()
    return json.loads(raw)


def _build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="announce_e2e.evidence")
    subparsers = parser.add_subparsers(dest="subcommand", required=True)

    report = subparsers.add_parser(
        "classify-report", help="print an announce report's status"
    )
    report.add_argument("--file", help="report JSON path; omit or '-' for stdin")

    latency = subparsers.add_parser("latency", help="print elapsed seconds")
    latency.add_argument("--start", required=True)
    latency.add_argument("--end", required=True)

    lane = subparsers.add_parser("classify-lane", help="print a lane classification")
    lane.add_argument(
        "--file", help="merged reviews+events JSON; omit or '-' for stdin"
    )
    lane.add_argument(
        "--bot-actor-ids",
        required=True,
        help="comma-separated numeric ids (env.sh BOT_ACTOR_IDS) — never logins",
    )
    lane.add_argument(
        "--require-machine",
        action="store_true",
        help="exit 1 unless the lane is machine with no human click, so the "
        "caller branches on an exit code instead of grepping this JSON",
    )

    union = subparsers.add_parser("pr-union", help="check a committed tag superset")
    union.add_argument("--committed", required=True, help="comma-separated tags")
    union.add_argument("--expected", required=True, help="comma-separated tags")

    redact = subparsers.add_parser("redact", help="redact secrets from stdin")
    _add_secret_args(redact)

    record = subparsers.add_parser("record", help="write one EvidenceRecord JSON")
    record.add_argument("--scenario", required=True, choices=SCENARIOS)
    record.add_argument("--status", required=True, choices=("pass", "fail"))
    record.add_argument("--pr-url", default="")
    record.add_argument("--run-urls", default="", help="comma-separated")
    record.add_argument("--latency", default="", help="seconds, or empty for none")
    record.add_argument("--notes", default="")
    record.add_argument("--out", required=True, help="destination path")
    _add_secret_args(record)

    render = subparsers.add_parser("render", help="render the results artifact")
    render.add_argument(
        "--file", help="EvidenceRecord list JSON; omit or '-' for stdin"
    )
    render.add_argument(
        "--records-dir",
        help="directory of `record`-written *.record.json files; replaces --file",
    )
    _add_secret_args(render)
    return parser


def _add_secret_args(sub: argparse.ArgumentParser) -> None:
    """Secrets arrive by environment variable NAME, never by value.

    `--secrets <token>` would put the live PAT on the argv of this process and
    of the `uv` parent, where `/proc/<pid>/cmdline` and every process-monitoring
    agent can read it — the same leak `subsystem-cli-commands.md` refuses for
    `--password`. One of the two flags is required so an unredacted run is
    always a stated choice, never an empty string nobody noticed.
    """
    group = sub.add_mutually_exclusive_group(required=True)
    group.add_argument(
        "--secrets-env",
        default="",
        help="comma-separated env var NAMES whose values are masked (e.g. OCX_ANNOUNCE_TOKEN)",
    )
    group.add_argument(
        "--no-secrets",
        action="store_true",
        help="state that this invocation has nothing to redact",
    )


def _secrets_from_env(names: str) -> list[str]:
    """Read each named variable's whole value. Never split a value on commas —
    a secret is opaque. An unset or empty name is announced, not swallowed."""
    secrets = []
    for name in _split(names):
        value = os.environ.get(name, "")
        if value:
            secrets.append(value)
        else:
            print(
                f"warning: ${name} is unset or empty; nothing to redact",
                file=sys.stderr,
            )
    return secrets


def _split(value: str) -> list[str]:
    return [item for item in (part.strip() for part in value.split(",")) if item]


def _record_from_json(entry: dict, secrets: Sequence[str]) -> EvidenceRecord:
    """Build a record, redacting on the way in so the CLI cannot skip D-4."""
    pr_url = entry.get("pr_url")
    return EvidenceRecord(
        scenario=entry["scenario"],
        status=entry["status"],
        pr_url=None if pr_url is None else redact_secrets(pr_url, secrets),
        run_urls=[redact_secrets(url, secrets) for url in entry.get("run_urls", [])],
        latency_seconds=entry.get("latency_seconds"),
        notes=redact_secrets(entry.get("notes", ""), secrets),
        captured_at=entry["captured_at"],
    )


def _as_json(record: EvidenceRecord) -> dict:
    return {
        "scenario": record.scenario,
        "status": record.status,
        "pr_url": record.pr_url,
        "run_urls": list(record.run_urls),
        "latency_seconds": record.latency_seconds,
        "notes": str(record.notes),
        "captured_at": record.captured_at,
    }


def main(argv: Sequence[str] | None = None) -> int:
    args = _build_parser().parse_args(argv)

    if args.subcommand == "classify-report":
        print(classify_report(_read_json(args.file)))
    elif args.subcommand == "latency":
        print(compute_latency_seconds(args.start, args.end))
    elif args.subcommand == "classify-lane":
        lane = classify_lane(
            _read_json(args.file), [int(i) for i in _split(args.bot_actor_ids)]
        )
        print(
            json.dumps(
                {
                    "lane": lane.lane,
                    "human_click_detected": lane.human_click_detected,
                    "actor_ids": lane.actor_ids,
                }
            )
        )
        if args.require_machine:
            return 0 if lane.lane == "machine" and not lane.human_click_detected else 1
    elif args.subcommand == "pr-union":
        is_superset, missing = assert_pr_union(
            _split(args.committed), _split(args.expected)
        )
        print(json.dumps({"is_superset": is_superset, "missing_tags": missing}))
        return 0 if is_superset else 1
    elif args.subcommand == "redact":
        print(
            redact_secrets(sys.stdin.read(), _secrets_from_env(args.secrets_env)),
            end="",
        )
    elif args.subcommand == "record":
        entry = {
            "scenario": args.scenario,
            "status": args.status,
            "pr_url": args.pr_url or None,
            "run_urls": _split(args.run_urls),
            "latency_seconds": float(args.latency) if args.latency else None,
            "notes": args.notes,
            "captured_at": datetime.now(UTC).isoformat(timespec="seconds"),
        }
        # Round-trip through _record_from_json so the record that lands on disk
        # is one the render path will accept — including its redaction (D-4).
        Path(args.out).write_text(
            json.dumps(
                _as_json(_record_from_json(entry, _secrets_from_env(args.secrets_env))),
                indent=2,
            )
        )
        print(args.out)
    elif args.subcommand == "render":
        secrets = _secrets_from_env(args.secrets_env)
        if args.records_dir:
            entries = [
                json.loads(path.read_text())
                for path in sorted(Path(args.records_dir).glob("*.record.json"))
            ]
        else:
            entries = _read_json(args.file)
        print(
            render_evidence_markdown(
                [_record_from_json(entry, secrets) for entry in entries]
            ),
            end="",
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
