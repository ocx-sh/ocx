# Research: Grimoire Announce Implementation — Copy-and-Own Inventory for OCX

<!--
Persisted from the 2026-07-22 deep-read of /home/mherwig/dev/grimoire (grim CLI,
same owner) and grimoire-rs/index. Grimoire is a PARTS DONOR only (owner ruling):
copy code/patterns and own them; never copy strategy, config model, or index design.
Companion to design_spec_announce_initiative.md (the decision register).
-->

**Date:** 2026-07-22 · **Sources:** local checkout `../grimoire` (HEAD at read time), shallow clone of `grimoire-rs/index`

## What grimoire ships (context)

`grim publish --announce` writes pointer files into a separate index repo
(`grimoire-rs/index`) and opens a fork PR/MR — GitHub **and** GitLab, pure
`reqwest` REST for forge APIs (no `gh`/`glab`/octocrab), git subprocess for
clone/commit/push. Design record: `../grimoire/.claude/artifacts/adr_announce_fork.md`
(Accepted 2026-07-19, D1–D10). Key sources: `src/catalog/forge.rs` (~2150 lines),
`src/catalog/index_announce.rs` (~860 lines), tests `test/tests/test_publish_announce.py`.

## PORT — copy and own into `ocx_lib` (transport-adjusted to REST-only)

| Item | Grimoire source | Notes for the OCX port |
|---|---|---|
| No-redirect forge HTTP client | `forge.rs:237-266` `build_client` | `redirect::Policy::none()` — reqwest otherwise replays Authorization on cross-host 3xx (token leak). Embedded-roots merge. One client per run. ~15-line unit, verbatim. |
| Fork-parent verification | `forge.rs:979-1022` `github_fork_target` | Reject a fork whose `parent.full_name` ≠ upstream (same-named stranger repo). Mandatory before any write. |
| Response-body-only fork identity | ADR D5 | `full_name`/endpoints read from the fork API response, never composed as `{login}/{basename}` — renamed forks break the naive form. OCX REST twist: rebuild every subsequent API endpoint from the verified identity; never follow an API-returned URL blindly. |
| Bounded fork-readiness poll | `forge.rs:626-653, 1213-1266` | 2s initial → 30s cap doubling, 300s wall deadline, 8s per-request timeout (one black-holed request can't eat the deadline). Classifier-parameterized. |
| Fork-push 404 race retry | `index_announce.rs:551-586` | GitHub fork *metadata* reads ready before its *git objects*; first write can 404. One retry after fixed 3s, only for fresh forks. With REST commits the same race applies to the first contents/commits call. |
| PR open-or-update on 422/409 | `forge.rs:467-613` | 422 → look up + reuse existing open PR. Update-in-place, never duplicate. |
| Token-leak test assertions | `test_publish_announce.py:609,748,776,824,958,1006` | `assert TOKEN not in stdout+stderr` as a first-class acceptance pattern; one test shims the transport to capture argv/requests and asserts the literal never appears. |
| Fake-forge test harness | `test_publish_announce.py:182-384` `_ForgeApi` | Stdlib `ThreadingHTTPServer` implementing exactly the REST surface the client calls: fork 202/201/409, PR 422-reuse, scripted readiness sequences (pending→ready, fail-fast), renamed-fork path, parent-mismatch rejection. ~200 lines, per-test instance, zero real network. OCX drops grimoire's third layer (git `insteadOf` redirection) — REST-only client doesn't need it. |
| Numeric-ID anti-spoof pattern | `validate_pr.py:71-90,185-187` (index repo) | Authorization compares immutable numeric account id from a live forge lookup, never the recyclable login. OCX's `owners[].github_id` already does this — keep the *live re-derivation on the validator side* discipline. |
| Config-file schema pattern | `src/command/publish.rs:41-306` | `deny_unknown_fields` + size-capped read + JSON-Schema from the same struct. Matches existing OCX conventions; confirmation, not new. |

## REJECT — explicitly not copied (owner rulings + technical mismatch)

| Item | Why rejected |
|---|---|
| Git-subprocess transport (clone/commit/push, `protocol.ext.allow=never`, `verify_fork_push_url`) | S1: REST-only. The two git-hardening guards become moot; their *intent* survives as "rebuild endpoints from verified identity". |
| Tri-state push-permission probe (`Option<bool>`, degrade to upstream push) | S3: always-fork. No probe, no direct-push path. |
| Content-hash branch naming (`announce/<ns>-<sha8>`) | C9: stable branch matching the Python reference tool (FP-9 parity, cross-tool dedupe). |
| GitHub App token minting (`create-github-app-token`, index-scoped App) | S4: PAT-only v1. Pattern recorded as future option; grimoire proves the index-scoped App shape works (opposite of BCR's retired publisher-repo-scoped App). |
| Enrich-tree split (nightly bot commits presentation data to main) | S8: desc stays in the root per ADR-6. |
| Index/config model (`[[registries]]` array, `oci` XOR `index`, browse-only index, TTL-only freshness, no digest verification, no yank, reachability-only validation) | OCX's landed model is strictly stronger: namespace-keyed authority, index in the resolve path, CAS + byte-exact wire, verify-claims re-derivation, yank markers, governance lanes. See gap list below. |
| GitLab CI job-token credential helper | Git-transport-specific; OCX is REST + GitLab is future track. Revisit at GitLab time. |

## Grimoire index-side gap list (why OCX's bot stays the heavier design)

grimoire-rs/index validates: path shape, schema, namespace==author (or public org
membership), owner.id == live numeric id, ref reachability (≥1 tag). It does NOT:
re-derive claims from the registry (no digest/manifest comparison), pin content
(tag-less/digest-less refs, silent retag invisible), verify signatures/provenance,
hold an index-side yank authority (deprecation mirrors the publisher's own registry),
have multi-owner/maintainer fallback, or rate-limit namespace creation. OCX's
indexbot (ADR-6, G-01..G-20, verify-claims byte-compare, CAS, verify-only reconcile)
already covers exactly these — the owner's ruling "don't copy grimoire's index
design; this is the next iteration" is grounded in this table.

## Incidental confirmations

- `publish-catalog.yml` degrade pattern: missing announce credential ⇒ `::notice::` skip, publish itself never fails — adopt for the CI snippet (Track F).
- Always-run dry-run smoke before real publish (validate+pack+plan, zero writes) — adopt in publisher harness CI (Track C).
- Deferred-lint-failure CI pattern and `secrets-via-env, never `${{ }}` in run bodies` — already OCX conventions (`subsystem-ci.md`).
- grim's own OCI/digest modules are headers-say "Adapted from OCX" — copying between these repos is established practice in this direction too.
