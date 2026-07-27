# Announce E2E Results (Track D, gate G-D)

Durable evidence that the fork-PR announce lane works end to end against the
real `ocx-sh/index`. Consumed by Track E's rollout handover and Track F's
doc-accuracy check.

**Status:** all five scenarios captured live on 2026-07-27 against
`ocx-sh/index`, `michael-herwig/ocx-e2e-publisher` and
`ghcr.io/michael-herwig/ocx-e2e-hello`. Every pull request, run and timestamp
below is clickable. Re-running any driver overwrites its own row only — a
scenario nobody captured stays a full `MISSING` row, because an incomplete
evidence set is meant to look incomplete.

## How to fill this in

```sh
cd test && PYTHONPATH=src uv run python -m announce_e2e.evidence render \
    --records-dir manual/announce-e2e/results
```

Replace the table below with that output, then commit. The renderer redacts
`OCX_ANNOUNCE_TOKEN` out of every free-text field on the way in, and
`EvidenceRecord` refuses text that has not been through redaction — this file
is committed and durable, so it must never carry a live credential.

## Results

<!-- BEGIN evidence table — verbatim `evidence render` output; its shape is
     pinned by test_announce_e2e_evidence.py -->
| Scenario | Status | Pull Request | Runs | Latency (s) | Captured At | Notes |
|---|---|---|---|---|---|---|
| idempotency | pass | MISSING | MISSING | MISSING | 2026-07-27T07:29:10+00:00 | status=unchanged; 3 PR(s) unchanged; branch head 392b039 unchanged |
| machine_lane | pass | https://github.com/ocx-sh/index/pull/87 | MISSING | 146.0 | 2026-07-27T07:28:40+00:00 | tag 1.0.8; {"lane": "machine", "human_click_detected": false, "actor_ids": [41898282]}; tag-push to merge 146.0s, tag-push to served 222.0s |
| clean_install | pass | MISSING | MISSING | MISSING | 2026-07-27T07:24:17+00:00 | resolved michael-herwig/ocx-e2e-hello from https://index.ocx.sh in a clean debian container |
| sequenced | pass | https://github.com/ocx-sh/index/pull/86 | https://github.com/michael-herwig/ocx-e2e-publisher/actions/runs/30245751807, https://github.com/ocx-sh/index/actions/runs/30245921245, https://index.ocx.sh/p/michael-herwig/ocx-e2e-hello.json | 160.0 | 2026-07-27T07:24:17+00:00 | tag 1.0.7; PR #86 merged at 2026-07-27T07:22:23Z; tag-push to merge 160.0s; (g2) 9 tag→object bindings over 2 distinct o/ objects, byte-identical to ghcr.io/michael-herwig/ocx-e2e-hello |
| update_union | pass | https://github.com/ocx-sh/index/pull/88 | MISSING | MISSING | 2026-07-27T07:32:10+00:00 | single PR #88; committed [1,1.0,1.0.1,1.0.2,1.0.3,1.0.4,1.0.5,1.0.6,1.0.7,1.0.8,1.0.9,latest] superset of [1.0.6,1.0.9] |
<!-- END evidence table -->

### Reading the MISSING cells

A `MISSING` cell in a `pass` row is a field that driver never had a value for,
not a gap in the evidence: `run_idempotency.sh` and `clean_install_check.sh`
open no pull request, and `run_machine_lane.sh` records its two latency legs in
Notes rather than as run URLs. Only a row whose **Status** reads `MISSING` is an
uncaptured scenario, and such a row is `MISSING` in every column.

## How this run was captured

Tags `1.0.6`–`1.0.9` on `michael-herwig/ocx-e2e-publisher` were spent here
(`1.0.1`–`1.0.5` were already consumed):

| Tag | What it did | Disposition |
|---|---|---|
| `1.0.6` | first sequenced attempt; its pull request (#83) opened `CONFLICTING` off a spent branch | **closed unmerged**, tag left in place |
| `1.0.7` | sequenced — pushed three times, retracted twice (see the two driver defects below) | merged as #86 |
| `1.0.8` | machine lane | auto-merged as #87 — that *is* the proof |
| `1.0.9` | pushed with the fork's announce branch deleted, so its announce could not commit | no pull request; tag left on the registry |

`1.0.6` and `1.0.9` therefore sat on `ghcr.io` uncommitted, which is exactly the
pair `run_update_union.sh` needs — two announceable tags without a sixth CI
cycle.

Three things had to be true before any driver ran, and the first of them is why
the `1.0.6` attempt failed:

- **The fork's announce branch must point at the index's `main` before each
  run.** The index squash-merges, so the branch is spent after every merge and
  the next announce opens `CONFLICTING` (that is what closed #83). Reset it,
  never delete it — the dev-channel `ocx` the publisher CI resolves predates
  `60c8b391` and cannot create the branch from scratch, which is how the `1.0.9`
  announce was made to fail on purpose. See README.md "The spent branch".
- **`OCX_ANNOUNCE_TOKEN` for the two local-announce drivers** —
  `run_idempotency.sh` and `run_update_union.sh` announce from this machine.
- **A release-shaped `ocx`** for the clean-machine image, staged from
  `target/release/ocx`, never `test/bin/ocx` (D-7).

`update_union`'s pull request (#88) auto-merged at `2026-07-27T07:32:58Z`,
**after** the driver had already asserted the union at `07:32:10Z` against the
still-open pull request — the C4 assertion was made on an unmerged branch, which
is what the scenario requires.

Two driver defects were found and fixed by this run, both in
`scripts/env.sh`: `_checks_settled` treated a `QUEUED` GitHub Actions check as
settled and failed a pull request whose checks had not started, and `poll_run`
matched a retracted tag's stale workflow run and reported the previous attempt's
verdict as this one's.

## What each row proves

| Scenario | Design-spec §7 bullet | Driver |
|---|---|---|
| `sequenced` | tag → build → announce → fork PR → validate green → merge → served → clean install | `run_sequence.sh` |
| `idempotency` | second identical run: `status: "unchanged"`, zero PRs, zero commits (C6) | `run_idempotency.sh` |
| `machine_lane` | tag-refresh announce auto-merges via G-19 with no human click | `run_machine_lane.sh` |
| `update_union` | two announces, first PR unmerged → one PR carrying both tag sets (C4) | `run_update_union.sh` |
| `clean_install` | `ocx install` resolves from a machine that has never seen ocx | `clean_install_check.sh` |
