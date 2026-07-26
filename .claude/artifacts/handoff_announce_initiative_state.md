# Handoff — Announce Initiative, session state 2026-07-25

Written because the session that produced it is about to be compacted. Everything a
successor needs to resume without re-deriving. Facts carry evidence; anything unverified
says so.

Canonical decision register: [`design_spec_announce_initiative.md`](./design_spec_announce_initiative.md).
This file is the *state*, not the decisions.

---

## 1. Where the initiative actually stands

Track D — the live end-to-end gate against the real `ocx-sh/index` — **ran and reached the
index**. A git tag became a pull request against the production index, with the announce
token never leaving CI. That is the proof the whole initiative was built toward, and it
is now demonstrated, not argued.

It found **four real defects** on the way. Three are fixed and merged; one is open and
needs an owner decision.

| # | Defect | Where | State |
|---|---|---|---|
| 1 | Publisher published only on `workflow_dispatch`; a git-tag push ran green without publishing | `michael-herwig/ocx-e2e-publisher` | fixed, merged `12530c5` (PR #4) |
| 2 | **First announce for any package failed.** GitHub returns 422 (not 404) for a missing ref on `PATCH .../git/refs/heads/<branch>`, so `upsert_branch`'s create path was unreachable | `ocx-sh/ocx` `forge/github.rs` | fixed, merged `0b070e1e` (PR #225) |
| 3 | **Every announce PR failed `schema-validate-pr`.** Git pathspec `*` matches `/`, so `p/*/*.json` also selected CAS objects and fed them to the package-root validator | `ocx-sh/index` `validate.yml` | fixed, merged (PR #59) |
| 4 | **Machine-lane auto-merge cannot arm.** `gh pr merge --auto` → `GraphQL: Resource not accessible by integration (enablePullRequestAutoMerge)` | `ocx-sh/index` `validate.yml:208` | **OPEN — owner decision** |

Defect 2 is the one worth remembering: **1684 tests certified a broken lane** because
`test/tests/fake_forge.py` modelled the same wrong 404 the client did. A test double that
encodes the same assumption as the code under test will confirm it indefinitely. The fix
corrected the fake, and against the *pre-fix* binary the corrected fake reproduces the
production error verbatim.

---

## 2. Open decisions blocking progress

### 2.1 Bug 4 — machine-lane merge identity (blocks Track D's last mile)

The governance *verdict* passes (`governance/review-required` green:
*"refresh: PR author owns every touched package, no review required"*). Only the
auto-merge call fails. Repo has `allow_auto_merge: true`; the job has
`pull-requests: write`. The default `GITHUB_TOKEN` cannot perform that GraphQL mutation.

Any fix changes *who merges*, which collides with the machine-lane proof:
`BOT_ACTOR_IDS=41898282` assumes `github-actions[bot]` is the merging actor, and the
proof is "no human clicked" (D-3 / register X7 — numeric actor ids, never logins).

| Option | Effect on the proof |
|---|---|
| GitHub App | Own bot actor id — proof intact. But register S4 records App auth as future-only, PAT day one |
| Dedicated `ocx-bot` machine account | Its own numeric id; `BOT_ACTOR_IDS` becomes that. Already an owner gate for Track E |
| Owner PAT | Works mechanically; merging actor becomes a human id. Turns the proof from *unproven* into *unprovable* |
| Merge directly instead of arming auto-merge | Governance job runs before checks finish, so it would have to wait — changes the gate's shape |

### 2.2 `ocx-sh/ocx#224` — recommended annotation set

Filed and revised. Table is a *vocabulary*, not a conformance ladder — every annotation
is optional and the issue does not propose changing that. Most of it is documentation and
can wait indefinitely.

**One narrow sub-decision is time-sensitive: for a mirror, does
`org.opencontainers.image.source` name the mirror repo or upstream?**
`ocx-sh/ocx-mirror#19` ships the **mirror-repo answer as a config-overridable default**, so
nothing is locked in code.

**Why the timing changed (owner Q, 2026-07-25).** Annotations live on the image index, and
`adr_oci_index_only_dispatch.md` makes `o/` hold that index **verbatim**. So changing an
annotation changes the index bytes → its digest → the CAS object → `tags[].content` → and
requires **re-announcing every affected tag**. This is the churn `ocx-sh/index#58`
deliberately avoided by keeping `source` out of the content-addressed object; the ADR
reverses that by construction, and correctly so — the published artifact changed, so the
lock should change.

Consequence: deciding before Track E publishes is free. Deciding after 42 packages ship
means re-push + re-announce across the fleet. Everything else in #224 stays deferrable.

### 2.3 ADR OQ2 — `artifactType` strictness — **CLOSED, not blocking**

**Closed by owner ruling 2026-07-25, recorded as O-5 in
`.claude/state/plans/meta-plan_oci_index_alignment.md:1039-1044`, and carried into
`.claude/artifacts/adr_oci_index_only_dispatch.md` OQ2.** No `artifactType` check — not as a
refusal and not as a warning. Document **kind** ("is an image index") is the right
granularity. Listed here only so the closure is visible from this file; nothing is pending.

The decisive ground is checkable in a minute: **nothing in either repo reads an image
index's `artifactType`.** ocx's enforcement sites are all image *manifests* — `pull.rs:398`
(the resolved leaf), `client.rs:1290-1299` (`pull_description`), `client.rs:1411-1418`
(`fetch_single_layer_artifact`) — never a document ocx merges into. The other three grounds
(single invariant impossible; it refuses indices ocx itself maintains; it stops no
adversary) are in the ADR in full.

Correction to an earlier reading recorded here: `merge_platform_into_index` does **not**
leave `artifact_type` unstamped. It fills the field when absent and leaves a *declared*
foreign value alone (`client.rs:335-341`) — overwriting one would relabel someone else's
artifact. That is what makes the strict check refuse ocx-maintained indices, so the
conclusion is unchanged; only the mechanism was stated wrongly.

---

## 3. The side quest, and how to get back

Mid-session the owner reopened the index dispatch-object format. It is now an
owner-ratified ADR: **`.claude/artifacts/adr_oci_index_only_dispatch.md`**.

**The decision.** `o/<algo>/<hex>.json` holds **raw OCI image index bytes only**. The
bot-synthesized observation object is deleted. A tag resolving to a bare manifest is
refused, not recorded. Plus D7: reserved tags (`__ocx*`, case-insensitive, no dot; and
canonical `sha256.<hex>`) are never versions.

**Why** (the owner's rationale, which is the ADR's Context and the thing to preserve):
an index is a catalog of OCI artifacts; tags are floating pointers; the index's job is to
*lock* one. So `o/` holds a verbatim copy of the registry's image index at that instant —
not a noun we invented. A manifest is immutable by digest and its reachability is the
publisher's concern; an *index* changes whenever a platform is added and the old one
becomes unreferenced in the ordinary course of publishing. **Snapshot exactly the thing
that can disappear.** And a snapshot must be *the bytes* — a re-serialized projection can
never be verified against what it replaced, because the original is gone by the time you
would check.

**Sequencing.** This must land before Track E publishes its fleet. `p/` currently holds
**one** package and **one** CAS object, so the migration is as cheap as it will ever be
and gets more expensive with every announce.

**Getting back after it lands:** resume at §4, in that order. Nothing in Track D/E/F was
invalidated by the side quest — only deferred.

---

## 4. Resume order after the ADR is implemented

1. **Decide bug 4** (§2.1) — Track D cannot finish without it.
2. **[`ocx-sh/index#57`](https://github.com/ocx-sh/index/pull/57) — CLOSED unmerged by the
   owner, 2026-07-25.** No old-format CAS object exists anywhere. Track D's re-announce
   (fresh publisher tag, post-change binary, post-change index) produces the first
   new-format object. Retained below: why it was a free choice either way.

   **There is no backwards-compatibility constraint here at all.** The entire `index.ocx.sh`
   client is unreleased — verified: `git cat-file -e v0.4.3:crates/ocx_lib/src/oci/index/ocx_index.rs`
   and `…/wire.rs` are both **ABSENT** in `v0.4.3`, the latest release. `wire::Observation`
   has never existed in a released binary, so no deployed client reads `o/` at all. This work
   is **finishing an unshipped feature before it ships**, not migrating a live format.

   (An earlier version of this handoff argued `#57` *must* be closed, on the grounds that an
   old client reading a new-format object fails silently — `#[serde(default)] platforms` with
   no `deny_unknown_fields` (`wire.rs:105-109`) parses image-index JSON as `platforms: []` →
   `NotFound`, no diagnostic. The code behaviour is real; the scenario has no victim. That
   argument is withdrawn.)

   What remains is tidiness: merging writes an old-format CAS object into `main` that would
   be re-announced anyway, and the re-announce is Track D's next step regardless since tags
   `1.0.1`/`1.0.2` are consumed.
3. **Track D remaining stages**, executed against the new format. Tags `1.0.1`/`1.0.2` are
   consumed, so the next run needs a fresh tag — which the re-announce needs anyway, making
   it Track D's next step rather than extra work. Steps (f) merge + render-deploy,
   (g) `index.ocx.sh` serves the root, (h) clean-machine install. Then `run_machine_lane.sh`,
   `run_idempotency.sh`, `run_update_union.sh`.
4. **Track F** — `docs/announce-user-guide` @ `2c7e2851` verified green, held pending Track D
   proof. Re-check against the ADR's doc changes before opening.
5. **Track E** — mirror fleet rollout. Gated on E-P4 (physical GHCR path convention, in the
   register) and on the ADR landing.

---

## 5. Track D operational facts (do not re-derive)

Drivers: `.agents/worktrees/ocx-d-e2e/test/manual/announce-e2e/scripts/` on branch
`announce/d` — **not on `origin/main`**.

Environment, every value derived from checked state:

```sh
GH_REPO_PUBLISHER=michael-herwig/ocx-e2e-publisher
GH_REPO_INDEX=ocx-sh/index
INDEX_FORK=michael-herwig/index
E2E_NAMESPACE=michael-herwig
E2E_PACKAGE=ocx-e2e-hello
BOT_ACTOR_IDS=41898282      # index validate.yml:208 merges with ${{ github.token }} => github-actions[bot]
PUBLISHER_WORKTREE=.agents/worktrees/ocx-e2e-publisher
OCX_BINARY=<ocx-sion>/target/release/ocx
POLL_DEADLINE_SECONDS=900
```

- Tags `1.0.1` and `1.0.2` are **consumed** — next run needs a fresh tag.
- The publisher publishes on **tag push** and `workflow_dispatch` only; `pull_request` and
  branch pushes are excluded (verified live: `Push + announce = skipping` on the PR run).
- Dev channel: `dev.ocx.sh/ocx/cli:0.5.0-dev` (floating). Publisher `ocx.toml` pins exactly
  that. Re-deploy via `gh workflow run "Deploy Dev" --ref main` after any ocx change the
  E2E needs — it is `workflow_dispatch`-only and does **not** fire on merge.

### KNOWN UNFIXED BUG — Track D driver

`poll_run` in `scripts/env.sh:180-199` identifies a workflow run by **SHA alone**. A git
tag pushed at `origin/main`'s HEAD produces **two** runs at the same SHA (the branch push
and the tag push), and `_run_field` takes `first`. Worse, `poll_run` re-queries between the
concluded-check (`:183`) and the conclusion read (`:185`), so the two `gh` calls can observe
different runs — this produced an **empty conclusion** and a false failure on the `1.0.2`
run whose publisher CI actually succeeded.

Fix needed: select on SHA **and** a discriminator (`headBranch` = the tag), and read all
fields in **one** query. Diagnosed, not implemented.

---

## 6. Landed this session

| Repo | Commit / PR | What |
|---|---|---|
| `ocx-sh/ocx` | `f40be710` (#223) | `ocx package push --annotation KEY=VALUE` |
| `ocx-sh/ocx` | `0b070e1e` (#225) | 422-vs-404 ref classification (defect 2) |
| `ocx-sh/ocx` | `bf24cec7` | E-P4 physical GHCR path convention → register |
| `ocx-sh/ocx-mirror` | `607d376`, `21d8ed4` (#19) | Submodule bump + `annotations:` config + CI allowlist |
| `ocx-sh/index` | `a779112` (#56) | Per-deployment registry-host allowlist |
| `ocx-sh/index` | (#58) | Root `source` field + `MetaRail` display |
| `ocx-sh/index` | (#59) | `:(glob)` pathspec fix (defect 3) |
| `michael-herwig/ocx-e2e-publisher` | `12530c5` (#4) | Publish on tag push |
| `michael-herwig/ocx-e2e-publisher` | `b622cd1` (#5) | Emit `image.source` / `image.revision` |

---

## 7. Standing constraints (unchanged, still in force)

- `OCX_ANNOUNCE_TOKEN` read from env only; never enters `auth/store.rs`, never logged,
  never on argv. Classic PAT, `public_repo` scope only.
- No `--trusted-host` CLI flag — the SSRF escape hatch lives only on
  `[registries."<ns>"]` so `system_locked` applies.
- Set secrets via `gh secret set …` **without** `--body` (token from stdin).
- Never `git stash` in agent worktrees. Never commit on `main`. Never push without the
  owner's word.
- Model policy: opus for security/review/non-mechanical implementation and ADRs; sonnet
  for exploration/docs/mechanical edits. Set `model` explicitly on every spawn.

---

## 8. Session lessons worth keeping

- **Read the exit line, not the wrapper.** A `tee`'d pipeline reports `tee`'s status. This
  has produced a false "verify passed" in this project before.
- **A test double that encodes the code's assumption cannot fail.** Defect 2 survived 32
  acceptance tests for exactly that reason. When fixing a wire-level bug, fix the fake in
  the same change and prove the fake now fails against the old code.
- **Verify agent deliverables against real state.** Several agents signalled idle without
  reporting; some had delivered, some had not. The idle signal is not a report.
- **`rg` is emulated here and false-negatives.** Use `git grep` in verification gates —
  a `grep -rn` returned "0 matches" for a string that was present twice in the file.
