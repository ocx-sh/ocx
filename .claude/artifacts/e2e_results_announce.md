# Announce E2E Results (Track D, gate G-D)

Durable evidence that the fork-PR announce lane works end to end against the
real `ocx-sh/index`. Consumed by Track E's rollout handover and Track F's
doc-accuracy check.

**Status:** template — no run recorded yet. Every cell reads `MISSING` until an
operator executes the drivers in `test/manual/announce-e2e/` and pastes the
rollup in. An incomplete evidence set is meant to look incomplete.

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

<!-- BEGIN evidence table — byte-identical to `render_evidence_markdown([])`
     while empty; pinned by test_announce_e2e_evidence.py -->
| Scenario | Status | Pull Request | Runs | Latency (s) | Captured At | Notes |
|---|---|---|---|---|---|---|
| sequenced | MISSING | MISSING | MISSING | MISSING | MISSING | MISSING |
| idempotency | MISSING | MISSING | MISSING | MISSING | MISSING | MISSING |
| machine_lane | MISSING | MISSING | MISSING | MISSING | MISSING | MISSING |
| update_union | MISSING | MISSING | MISSING | MISSING | MISSING | MISSING |
| clean_install | MISSING | MISSING | MISSING | MISSING | MISSING | MISSING |
<!-- END evidence table -->

## What each row proves

| Scenario | Design-spec §7 bullet | Driver |
|---|---|---|
| `sequenced` | tag → build → announce → fork PR → validate green → merge → served → clean install | `run_sequence.sh` |
| `idempotency` | second identical run: `status: "unchanged"`, zero PRs, zero commits (C6) | `run_idempotency.sh` |
| `machine_lane` | tag-refresh announce auto-merges via G-19 with no human click | `run_machine_lane.sh` |
| `update_union` | two announces, first PR unmerged → one PR carrying both tag sets (C4) | `run_update_union.sh` |
| `clean_install` | `ocx install` resolves from a machine that has never seen ocx | `clean_install_check.sh` |
