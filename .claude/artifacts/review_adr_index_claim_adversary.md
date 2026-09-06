# Adversary: cross-model review of the index-claim ADR and system design

## Metadata

- Date: 2026-09-05
- Scope: plan-artifact (`adr_index_claim_command.md`, `system_design_index_claim_command.md`, post fix round 1)
- Reviewer: Codex `gpt-5.6-terra` via codex-companion `task` (read-only), job `task-mtnkbqvv-a2ydyn`, Codex session `01a06eab-fd4c-7392-bb3f-504e127c8bcc`
- Raw result: `~/.cache/claude-hex/adversary_result_index_claim.txt`
- Triage: orchestrator (Fable), each finding re-read against the artifacts and the cited code before ruling

Codex totals: B=2 H=3 W=0 S=0.

## Triage (4-way: fix / defer / reject / already addressed)

| ID | Sev | Anchor | Defect (Codex) | Ruling | Basis |
|---|---|---|---|---|---|
| A-1 | Block | ADR "The git recipe" step 2 | Initial fetch names `<branch>:refs/remotes/o/<branch>` unconditionally; a first claim has no branch and `git fetch` fails on the missing remote ref | **fix** | Confirmed at ADR recipe step 2; design's claim state machine has an `absent` state. Remedy: branch existence via the REST Branches API (job-token readable; `get_ref_sha` already does this), fetch only refs that exist. |
| A-2 | Block | design "Phase 1 — Corrections and the exit code" | `ExitCode::ForgeCapabilityUnavailable = 86` added without an `ErrorCategory` arm; `from_exit_code` is exhaustive with no wildcard | **fix** | Confirmed at `crates/ocx_lib/src/cli/error_category.rs:40-79`; neither artifact mentions `ErrorCategory`. Remedy: `ErrorCategory::ForgeCapabilityUnavailable` (own category, same genus as 84/85), mapping, frozen-category test, all in Phase 1. |
| A-3 | High | ADR D-T7 / env precedence step 3 vs "Credential injection" | "git's own credential helpers apply" contradicts "`-c credential.helper=` on every invocation" (ADR line 1217, design line 475 "Never a credential helper") | **fix** | Confirmed. The helper fallback is owner-ratified (dossier decision 9). Remedy: the helper reset applies only to invocations that inject an ocx credential; the `git-helper` posture leaves helpers in charge, still with `http.followRedirects=false`, and the design table row says so. |
| A-4 | High | design Executive Summary "REST for every read"; ADR D-T5 | `compare_branch` under `git` is a fetch plus local graph read, so a second read transport exists unacknowledged | **fix wording, reject remedy** | The wording is wrong: council synthesis and dossier decision 6 carve out `compare_branch` because a job token has no REST compare endpoint (GitLab docs, 2026-09-04). Codex's "keep it on REST" would make the job-token path impossible. Remedy: every "every read" claim carves out `compare_branch` and names why. |
| A-5 | High | ADR D-T4 "atomicity is stronger under git" | Ref update plus push-option merge-request creation treated as one atomic operation; no bounded confirmation, no recovery for push-succeeded/MR-absent | **fix** | Confirmed at ADR lines 527-528; recipe step 6 queries once. GitLab creates push-option merge requests in the asynchronous post-receive worker, so a single immediate read can miss it. Remedy: drop the atomicity claim; bounded confirmation poll on `GET /merge_requests?source_branch=` with backoff; unconfirmed → exit 75 with a message naming the rerun path, which is D-T4's refresh commit; fixture delays the MR record to prove the poll. |

Deferred: none. Rejected outright: none (A-4's remedy only).
