# E1-pre — live failing-first capture (pre-change stack)

**Verdict: taxonomy row 1 — the bug class is present.** The index committed a
digest no registry serves. Captured 2026-07-25, against the real
`ocx-sh/index` and the real `ghcr.io`, with owner authorisation (gate G-1).

This reproduction is unrepeatable: the producing code is being deleted, and
once `Deploy Dev` publishes the new binary the pre-change announce path no
longer exists to be observed.

---

## Run identity

| | |
|---|---|
| Publisher tag | `1.0.3` (fresh; `push_publisher_tag` hard-fails on an existing tag) |
| Publisher commit | `b622cd10e640b155ea86efe619ab22f780e6d41e` (`origin/main`) |
| Publisher run | <https://github.com/michael-herwig/ocx-e2e-publisher/actions/runs/30174480620> — concluded **success** |
| Announce PR | **#61** — <https://github.com/ocx-sh/index/pull/61> |
| PR head | `ff8e8b49ed31fdaa9d4ac202b47dc63661c1a253` on `michael-herwig/index:indexbot-announce-michael-herwig-ocx-e2e-hello` |
| Package | `michael-herwig/ocx-e2e-hello`, physical repo read off the root: `oci://ghcr.io/michael-herwig/ocx-e2e-hello` |
| Driver | `test/manual/announce-e2e/scripts/run_sequence.sh 1.0.3`, `POLL_DEADLINE_SECONDS=900` |

### The binary under test was the pre-change one

The publisher resolves `ocx.toml`'s floating pin `dev.ocx.sh/ocx/cli:0.5.0-dev`.
Its `Resolve dev-channel ocx` step recorded:

```
Binding  Group    Digest
ocx      default  sha256:fb58dc14f605abf2c0daac259e633a1afba1f332ed8904eb4a11d19b61e56726
```

`ocx-sh/ocx@main` is `0b070e1e` (2026-07-25T11:27:41Z); the most recent
`Deploy Dev` (run `30156347972`, 11:30:13Z) built that commit. The
OCI-index-only dispatch change is **not** on `main`, so nothing newer could
have been deployed. Confirmed pre-change.

---

## What the announce committed

PR #61 changes exactly two paths:

```
modified p/michael-herwig/ocx-e2e-hello.json                                                     +22 -1
added    p/michael-herwig/ocx-e2e-hello/o/sha256/4ee19e66016380ec603fee8f6b8d8fda85768d779944a942f3d42befe451fe91.json  +1 -0
```

All five tags bind to **one** content digest:

| tag | `tags[].content` | observed |
|---|---|---|
| `1.0.2` | `sha256:4ee19e66016380ec603fee8f6b8d8fda85768d779944a942f3d42befe451fe91` | 2026-07-25T11:42:13Z |
| `1.0` | same | 2026-07-25T11:42:13Z |
| `1` | same | 2026-07-25T11:42:13Z |
| `latest` | same | 2026-07-25T11:42:13Z |
| `1.0.3` | same | 2026-07-25T20:55:22Z |

The committed object at that digest is **not an OCI image index**. It is a
document the announcer minted:

```json
{"platforms":[{"digest":"sha256:edc2a8dee0126febc8e96bdc0b88f5a8c64215460afaa34547423044f5cc3ef5","platform":{"architecture":"amd64","os":"linux"}},{"digest":"sha256:f3cb723572ffe070458cccc294d27329ee6fa3b0ccb3c58ac3652ec416da7641","platform":{"architecture":"arm64","os":"linux"}}]}
```

It is internally self-consistent — `sha256(bytes)` equals its own filename
`4ee19e66…` — which is precisely why the defect is invisible to any check that
stops at the CAS anchor. The platform→digest mapping is an **assertion the bot
authored**, not a **copy of bytes a registry served**.

---

## The reproduction — verbatim registry response

Requested **by digest**, so `Docker-Content-Digest` can separate a
content-negotiating registry (row 2) from a byte divergence (row 3). Identical
result for all five tag→object bindings; `1.0.3` shown verbatim:

```
GET https://ghcr.io/v2/michael-herwig/ocx-e2e-hello/manifests/sha256:4ee19e66016380ec603fee8f6b8d8fda85768d779944a942f3d42befe451fe91
  Accept: application/vnd.oci.image.index.v1+json, application/vnd.oci.image.manifest.v1+json

HTTP/2 404
content-type: application/json
docker-distribution-api-version: registry/2.0
strict-transport-security: max-age=63072000; includeSubDomains; preload
date: Sat, 25 Jul 2026 20:57:33 GMT
content-length: 70
x-github-request-id: 4F2E:3B3BC7:3132154:338F14E:6A65233D

{"errors":[{"code":"MANIFEST_UNKNOWN","message":"manifest unknown"}]}
```

`Docker-Content-Digest` is **absent** (no 200, nothing served).

### Control — the 404 is about the digest, not a missing package

A 404 by digest would look identical if the repository or the tag simply did
not exist, which would make the reading worthless. Same host, same repository,
same `Accept`, same anonymous pull token, asking for the tag **by name**:

```
GET https://ghcr.io/v2/michael-herwig/ocx-e2e-hello/manifests/1.0.3
HTTP 200
Docker-Content-Digest: sha256:50e02438d1d8e4968ad9a663d29185638931b2771e7e4f68cc9923926ccb5ee1
sha256(body)         = 50e02438d1d8e4968ad9a663d29185638931b2771e7e4f68cc9923926ccb5ee1
{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"sha256:edc2a8dee0126febc8e96bdc0b88f5a8c64215460afaa34547423044f5cc3ef5","size":451,"platform":{"architecture":"amd64","os":"linux"}},{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"sha256:f3cb723572ffe070458cccc294d27329ee6fa3b0ccb3c58ac3652ec416da7641","size":451,"platform":{"architecture":"arm64","os":"linux"}}],"artifactType":"application/vnd.sh.ocx.package.v1","annotations":{"org.opencontainers.image.revision":"b622cd10e640b155ea86efe619ab22f780e6d41e","org.opencontainers.image.source":"https://github.com/michael-herwig/ocx-e2e-publisher"}}
```

The repository exists, the tag exists, anonymous pull works, and the registry's
own digest for it is `sha256:50e02438…`. The index recorded `sha256:4ee19e66…`.
The two per-platform digests inside the real image index are the same ones the
minted object lists, which is what makes the invented document *look* right.

(`1.0.2` also serves `sha256:50e02438…`: both tags were built from
`b622cd10`, so their image indices are byte-identical. That is content
addressing behaving correctly, not a second defect.)

---

## Taxonomy row and why

| Row | Observation | Hit? |
|---|---|---|
| **1 — bug class present** | 404 / `MANIFEST_UNKNOWN` | **YES** |
| 2 — registry content-negotiated | 200, `Docker-Content-Digest` ≠ requested | no — no 200 |
| 3 — format regression | 200, header matches, bytes differ | no — no 200 |
| 4 — proof | 200, header matches, bytes identical | no |

**Row 1.** The status is 404 with `MANIFEST_UNKNOWN`, so rows 2–4 (all of
which require a 200) are unreachable by construction. It is not a transport or
auth failure: the identical request shape against the same repository returned
200 for the tag by name, using the same token. It is not a missing package: see
the control. What is missing is specifically the digest the index minted —
which is the claim under test, executed: *the index committed a digest no
registry can serve, so the platform→digest mapping was an assertion, not a copy.*

Bindings checked: **5 tag→object bindings over 1 distinct `o/` object.**

---

## Deviations from the runbook

Two, neither of which affects the verdict. Both are recorded rather than
worked around.

### 1. The driver stopped at phase (d), not at (f)

The README predicts the `1.0.3` run blocks in `poll_merge` until the deadline.
It did not get that far:

```
→ (d) waiting for validate.yml on PR #61
warning: PR #61 checks not green: bot-test, governance-gate
error: validate.yml is not green on PR #61 — see PLAYBOOKS.md playbook 2
```

`governance-gate` failed. It classified #61 as machine lane (label `refresh`)
and tried to arm auto-merge:

```
gh pr merge "$PR_NUMBER" --repo "$REPO" --auto --squash
GraphQL: Resource not accessible by integration (enablePullRequestAutoMerge)
```

Every other check was green (`bot-test`, `bot-lint`, `bot-audit`,
`schema-validate`, `schema-validate-pr`, `render-check`, `site-build`,
`workflows-lint`, `dependency-review`, `governance/review-required`).
`bot-test` appears in the driver's not-green list only because `poll_check`
re-reads the rollup after settling and both `governance-gate` entries collapse
into it; `gh pr checks` shows `bot-test` as `SUCCESS`.

Two consequences worth carrying forward:

- The run reaching (f) at all was never on the table, so no `poll_merge` wait
  was spent. Phases (g)/(g2)/(h) were not observed, as designed for E1-pre.
- **The auto-merge permission failure is the only reason #61 did not merge
  itself.** Had `enablePullRequestAutoMerge` been permitted, a machine-lane
  auto-merge would have written the old-format CAS object into `main`
  unattended. That is a Track-E/G-19 item to settle before `1.0.4`, and it is
  also a latent hazard: the same gate will arm auto-merge on the post-change
  run.

### 2. The README's E1-pre capture command cannot complete for an unmerged PR

The prescribed one-liner exits before it reaches the registry:

```
curl: (22) The requested URL returned error: 404
error: tag 1.0 records sha256:4ee19e66… but nothing is committed at
       o/sha256/4ee19e66….json — D2/D3 says every tags[].content has an object
```

Cause: `_assert_tag_identical` reads anchor 1 (the committed CAS object) from
`$INDEX_SITE` — `https://index.ocx.sh`, which only ever serves **rendered
`main`**. An unmerged pull request's object exists only on the fork branch, so
the anchor can never be satisfied for E1-pre. That message is *not* one of the
four taxonomy rows; it is a harness limitation, not a finding.

The capture therefore reused the gate's own logic, sourced rather than
reimplemented — `MANIFEST_ACCEPT`, `_root_repository`, `_root_tag_names`,
`_root_tag_content`, `_pull_token`, `_content_digest_header`, and the same
by-digest URL and status taxonomy — changing exactly one thing: **anchor 1
reads the object out of the pull request's own tree on the fork** instead of
the served site. Anchor 1 still passed (the object hashes to its own filename)
and anchor 2 is byte-for-byte the request `_assert_tag_identical` builds.

Suggested fix for the runbook, before someone repeats this: teach
`_assert_tag_identical` an object source (served site by default, a git ref for
the unmerged case), or drop the E1-pre one-liner from README in favour of a
`--ref` flag on the gate.

---

## Disposition — closed unmerged, `main` untouched

`1.0.3` must not merge: merging writes an old-format CAS object into
`ocx-sh/index@main`, the exact artefact this change exists to remove.

```
$ gh pr close 61 --repo ocx-sh/index --comment "…E1-pre failing-first capture, closed by design…"
✓ Closed pull request ocx-sh/index#61
```

Verified after closing:

| Check | Result |
|---|---|
| PR #61 state | `CLOSED` |
| `mergedAt` / `mergeCommit` / `autoMergeRequest` | `null` / `null` / `null` — **closed unmerged** |
| closedAt | 2026-07-25T20:58:24Z |
| Open PRs on `ocx-sh/index` | `[]` — the per-package announce branch is free for `1.0.4` |
| `p/` on `main` | exactly one root: `p/michael-herwig/ocx-e2e-hello.json` |
| `o/sha256/` objects under `p/` on `main` | **0** |
| Root's `tags` on `main` | `{}` — unchanged |
| `main` HEAD | `efcb234c` (2026-07-25T19:37:38Z), i.e. not advanced by this run |

(The repository does contain 22 paths matching `o/sha256/` elsewhere — all
under `bot/tests/golden/…` and `scripts/demo-fixtures/…`. Those are test and
demo fixtures with placeholder hex, not index objects; none are under `p/`.)

Publisher tag `1.0.3` remains pushed and consumed. Per the README budget,
`1.0.4` is the post-change proof and `1.0.5` the machine-lane proof.

---

## Reproduction inputs (no secrets)

```sh
export GH_REPO_PUBLISHER=michael-herwig/ocx-e2e-publisher
export GH_REPO_INDEX=ocx-sh/index
export INDEX_FORK=michael-herwig/index
export E2E_NAMESPACE=michael-herwig
export E2E_PACKAGE=ocx-e2e-hello
export BOT_ACTOR_IDS=41898282
export POLL_DEADLINE_SECONDS=900
export PUBLISHER_WORKTREE=<clone of michael-herwig/ocx-e2e-publisher>
```

`OCX_ANNOUNCE_TOKEN` was never in scope on this machine and is not needed:
`run_sequence.sh` pushes a tag and then observes; the announce ran inside the
publisher's CI, which holds the secret as a repo secret. No token appears in
any command line, file, or artifact here.
