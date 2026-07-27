# Announce E2E Gate (Track D)

Five drivers that prove the fork-PR announce lane works end to end against the
**real** `ocx-sh/index` — not a sandbox. They are run by hand against live
infrastructure and verified by checklist; nothing here is pytest-collected. The
surfaces that do run unattended are the pure logic in `test/src/announce_e2e/`
(`cd test && uv run pytest tests/test_announce_e2e_evidence.py`), the (g2)
gate's own decision logic (`./scripts/selfcheck_g2.sh`) and the run /
pull-request matchers (`./scripts/selfcheck_identity.sh`) — neither self-check
touches a network or a credential.

Each run leaves per-scenario evidence under `results/` (gitignored — real pull
request URLs and live timestamps are scratch). The curated rollup lives in
`.claude/artifacts/e2e_results_announce.md`.

## Prerequisites

**Tools.** `gh` authenticated (`gh auth status`) with a classic PAT scoped
`public_repo`; Docker running; `uv` on `PATH`; an `ocx` binary carrying
`package announce` (a dev-channel build — set `OCX_BIN` to point at it, or put
it on `PATH`).

**A claimed namespace.** Announce refuses an unclaimed namespace and never
creates a root itself (ruling R3). Before the first run, open a
claim-a-namespace pull request against `ocx-sh/index` committing
`p/<ns>/<pkg>.json` with your numeric `github_id` in `owners[]`, and get it
merged through the human lane (G-04). `run_sequence.sh` checks for this and
waits rather than failing — see "this step waits on you" below.

**Environment.** No script carries a default namespace or package: announcing
into the real index under a wrong default is not recoverable.

| Variable | Meaning |
|---|---|
| `GH_REPO_PUBLISHER` | `michael-herwig/ocx-e2e-publisher` |
| `GH_REPO_INDEX` | `ocx-sh/index` |
| `INDEX_FORK` | the fork announce opens its pull request from, `<owner>/index` |
| `E2E_NAMESPACE` / `E2E_PACKAGE` | the claimed pilot package |
| `BOT_ACTOR_IDS` | comma-separated **numeric** actor ids of the index bot |
| `PUBLISHER_WORKTREE` | optional; a publisher checkout to push tags from |
| `OCX_ANNOUNCE_TOKEN` | see below — needed only by the two local-announce drivers |
| `POLL_DEADLINE_SECONDS` | optional, default 600 |
| `CLAIM_DEADLINE_SECONDS` | optional, default 3600 |
| `OCX_BINARY` | optional; the ocx staged into the clean-machine image, default `target/release/ocx` |
| `E2E_INDEX_PREFIX` | optional; the identifier prefix the clean-machine config keys on, default `ocx.sh` |

`BOT_ACTOR_IDS` is numeric on purpose. A login is renameable and recyclable, so
proving "no human clicked" from a login string would reintroduce exactly the
threat the index bot is hardened against (Key Decision D-3, register X7). Read
the ids off the claimed root's `owners[].github_id`, or
`gh api users/<bot-login> --jq .id`.

**Where the announce credential lives.** `OCX_ANNOUNCE_TOKEN` is a repo secret
on `michael-herwig/ocx-e2e-publisher`; it is **not** on this machine. That
splits the drivers in two:

- `run_sequence.sh` and `run_machine_lane.sh` push a tag and then *observe*.
  The announce runs inside the publisher's CI, which holds the secret. They
  need no local token.
- `run_idempotency.sh` and `run_update_union.sh` run `ocx package announce`
  locally, so they need `OCX_ANNOUNCE_TOKEN` exported in your shell. Use **your
  own** classic PAT scoped `public_repo` — Track D does not need the shared
  `ocx-bot` PAT (that is Track E's). Without it they exit immediately with a
  pointer back here; they never run half a scenario.

Every captured artifact — announce reports, evidence records, the clean-machine
container log — goes through `redact_secrets` before it touches disk, and
`EvidenceRecord` refuses free text that has not been redacted, so a token
cannot reach the committed results artifact.

The secret reaches the evidence module by variable **name**
(`--secrets-env OCX_ANNOUNCE_TOKEN`), never by value. A value on argv is
readable from `/proc/<pid>/cmdline` by any local process for the life of the
call — the same leak the ocx CLI refuses for `--password`. An invocation with
no credential in scope passes `--no-secrets`, so an unredacted artifact is
always a stated choice rather than an empty string nobody noticed.

**Cost.** Every rehearsal spends real GitHub API budget and real CI minutes on
two repositories, and leaves real pull requests behind. Run them deliberately;
do not loop them.

## Before each run — the tag budget and the branch trap

**Each run needs a fresh tag.** `push_publisher_tag` in `scripts/run_sequence.sh`
hard-fails on a tag that already exists rather than silently re-pointing one.
`1.0.1` and `1.0.2` are consumed. The budget for the OCI-index alignment:

| Tag | Purpose | Pull request disposition |
|---|---|---|
| `1.0.3` | E1-pre — the live failing-first run against the **pre-change** stack, up to phase (f) | **close unmerged** |
| `1.0.4` | E1 / E2a — the post-change proof, the full `run_sequence.sh` | merge |
| `1.0.5` | machine-lane proof — `run_machine_lane.sh` needs a tag distinct from the sequenced run | auto-merge (that *is* the proof) |

`1.0.3` must not be merged: merging would write an old-format CAS object into
`main`, which is the thing the change exists to remove. If E1-pre is skipped,
shift the other two down one.

**The `1.0.3` run never reaches (g2), and that is not a defect.** (g2) runs
after (f), which needs the merge and the render-deploy; a pull request that by
design will never merge leaves the driver blocked in `poll_merge` until the
deadline. So (g2) will not have been observed red before the `1.0.4` run —
capture the E1-pre 404 by hand instead, off the unmerged pull request's own
root, using the gate's own taxonomy rather than a hand-rolled `curl`:

```sh
bash -c 'source ./scripts/run_sequence.sh   # sourcing never pushes a tag
    assert_bytes_identical "$(gh api \
        "repos/$INDEX_FORK/contents/p/$E2E_NAMESPACE/$E2E_PACKAGE.json?ref=$ANNOUNCE_BRANCH" \
        --header "Accept: application/vnd.github.raw")"'
```

Against the pre-change stack that exits with the row 1 message. **That output is
the reproduction** — paste it into the evidence artifact, then close the pull
request.

**The branch trap — check this before `1.0.4` and before `1.0.5`.** The announce
branch is **per-package, not per-tag** (`ANNOUNCE_BRANCH` in `scripts/env.sh`
is `indexbot-announce-<ns>-<pkg>`). A fresh tag therefore reuses the same branch
and lands on the **same open pull request** if one is still open. The drivers
no longer *mis-measure* that: every poll carries a freshness floor — the highest
pull-request number and workflow-run id GitHub had already issued, read before
the push — so a pull request or workflow run from an earlier rehearsal is never
mistaken for this run's. It shows up as a **timeout** instead — the driver waits
for a number above the floor, and an updated older pull request never qualifies.
The floor is a server-side counter and never a timestamp, so a local clock
running behind GitHub's cannot make stale data look fresh. Confirm no announce
pull request is open for the package first:

```sh
gh pr list --repo ocx-sh/index --state open \
    --json number,headRefName --jq '.[] | select(.headRefName | startswith("indexbot-announce-"))'
```

Empty output, or nothing naming your package, means the run measures itself.

**The spent branch — reset it before every run.** The index squash-merges, so the
announce branch's own commits never become ancestors of `main`. The next
announce builds on that spent branch and its pull request opens `CONFLICTING`,
which auto-merge cannot act on. `ocx` fixes this on its side (rebuild from the
base when the branch is spent), but the publisher's CI resolves a dev-channel
`ocx` and only picks the fix up once it is deployed. Until then, point the
branch back at the index's `main` before each run — **reset it, never delete
it**: a dev-channel `ocx` that predates the fix cannot create the branch from
scratch and exits with `forge returned HTTP status 404 for .../git/refs`.

```sh
gh api -X PATCH "repos/$INDEX_FORK/git/refs/heads/$ANNOUNCE_BRANCH" \
    -f sha="$(gh api "repos/$GH_REPO_INDEX/git/ref/heads/main" --jq .object.sha)" \
    -F force=true --jq .object.sha
```

## Sequenced Scenario

`./scripts/run_sequence.sh <tag>` — design-spec §7's exit gate, in order.

Proves: tag release → build → dev-ocx push + announce → fork PR on the real
index → `validate.yml` green → lane classification → merge → rendered
`index.ocx.sh` serves the root → `ocx package install` resolves from a clean
machine.

Phases, each printing a numbered banner:

- **(a0)** the namespace is claimed. If it is not, the script prints what to
  open and polls for up to `CLAIM_DEADLINE_SECONDS`. **This step waits on
  you** — a paused driver here is the design, not an error state.
- **(a)** tags `origin/main` in the publisher checkout and pushes the tag.
- **(b)** waits for the publisher's build + push + announce run to conclude.
- **(c)** waits for the pull request on `indexbot-announce-<ns>-<pkg>`. This is
  a *tag refresh* against an already-claimed namespace, not a first claim.
- **(d)** waits for every check on that pull request to settle, then requires
  them all green.
- **(e)** prints the governance checklist. The lane outcome is read off live
  state: a tag refresh auto-merges via G-19 when your `github_id` is already in
  the root's `owners[]`, and routes to the human lane otherwise. Both are
  legitimate outcomes here.
- **(f)** waits for the merge, then for render-deploy at the merge commit.
- **(g)** `curl`s `/p/<ns>/<pkg>.json` and `/c/index.json`, requiring the root
  to name **this run's tag** — asserting on a `sha256:` digest instead would
  pass against a root an earlier rehearsal populated, so a no-op render would
  still report a served render.
- **(g2)** the acceptance gate — see below.
- **(h)** calls `clean_install_check.sh`.

Writes `EvidenceRecord(scenario="sequenced")` with the pull request URL, three
run URLs, tag-push-to-merge latency, and what (g2) proved: the number of
tag→object bindings it checked **and** the number of distinct `o/` objects
those bindings cover. Under `--cascade` the first is several times the second,
and only the second says how much of `o/` was actually read.

### (g2) The committed objects are the registry's own bytes

The claim under test, as a falsifiable assertion:

> The index committed a digest **no registry can serve**, so the
> platform→digest mapping was an assertion, not a copy.

Before this change `tags[].content` was `sha256(serialize_observation(...))` —
a digest the announcer minted, not one that exists on any registry. So
`GET /v2/<physical-repo>/manifests/sha256:<content-hex>` returns **404
`MANIFEST_UNKNOWN`**, and that 404 is the reproduction. After it, the same GET
returns 200 and its body is byte-identical to the committed
`o/sha256/<hex>.json`. This is the ADR's Validation bullet 2, executed.

**E1 — for every tag in the served root, not only this run's.** `--cascade`
aliases `1.0.4` / `1.0` / `1` / `latest` onto one image index, so a per-tag
divergence is exactly what a single-tag check would let through. Per tag:

1. the committed CAS object hashes to its own filename;
2. the registry serves it **by digest**;
3. the response's `Docker-Content-Digest` **equals the requested digest**;
4. `cmp` against the committed object is byte-identical.

Step 3 is not redundant with step 4. It is what separates a content-negotiating
registry — which answered a question nobody asked — from a registry whose bytes
disagree with the digest it just advertised. Each outcome exits with its own
message; the taxonomy is in [PLAYBOOKS.md](./PLAYBOOKS.md) "Registry Byte
Identity".

The registry is read from the root's own `repository` field, never assumed from
`<ns>/<pkg>`: the ADR names that field as the thing to check against, and the
physical path is a convention a publisher is free not to follow. That field is
publisher-controlled, so its host and path are validated before either reaches
a URL — the same boundary treatment `tags[].content` gets. A root naming a host
nobody constrained could otherwise serve the committed bytes straight back and
turn the gate green.

**E2a — reserved tags never reach the index.** The served root's `tags` carry
no key matching `__ocx*` (case-insensitive) or `sha256.<64hex>` (D7). Free,
because the pilot repository genuinely carries both classes:
`push_canonical_tag` is default-on, and the publisher's `Describe package` step
writes `__ocx.desc`.

**What E2a does not prove.** The publisher announces via `--announce-file`,
which under D7 never carries a canonical tag — so a clean root is consistent
with **no filter at all**. E2a is a cheap regression tripwire. E2b (the index
repo's reconcile sweep) and the local acceptance cases are the proof that the
filter fired. Do not read a green E2a as more than it is.

**No new tools.** `curl`, `python3`, `sha256sum`, `cmp`, `mktemp` — `python3`
is already a hard prerequisite. Deliberately not `jq`: it is not in `ocx.toml`'s
toolchain list, and nothing here transforms JSON, it only reads fields.

**The gate's own logic has a test.** `./scripts/selfcheck_g2.sh` sources
`run_sequence.sh` (the guard at its foot keeps sourcing from pushing a tag) and
drives every (g2) verdict against fixture roots and a stubbed `curl`: each
taxonomy row, both pre-network anchors, the boundary guards on `tags[].content`
and `repository`, the parse and vacuity refusals, and the binding-vs-object
counting. No network, no credentials, no live state — run it after any edit to
(g2). It does **not** test TLS, real registry auth, or whether the announce path
writes a registry digest at all; the `1.0.4` run is the only thing that can
prove that.

**So does artifact identity.** `./scripts/selfcheck_identity.sh` sources
`env.sh` with `gh` stubbed on `PATH` and drives `run_floor`, `pr_floor`,
`_run_concluded` and `pr_number` against recorded `gh` JSON: how the floor is
read, the floor on both matchers, newest-wins when several observations survive
it, the polling gate that keeps an unconcluded run from reading as a verdict,
and the head-repository and branch guards. Two cases give a stale artifact the
fresh-looking creation time a backward-skewed local clock would have accepted
and require it to stay excluded — they pin that the matchers read the counter
and nothing else; they do not replay the old timestamp comparison, which the
counter signature no longer admits. Both matchers used to be able to answer with an
earlier rehearsal's run or pull request, which is how a driver reports green off
stale data — run this after any edit to either. It needs `jq` (test-only: it
replays the drivers' own `--jq` programs offline, and `gh` embeds its own engine
at run time).

## Idempotency Proof

`./scripts/run_idempotency.sh` — requirement (2), design register C6.

Re-announces the identical inputs (`--refresh`: re-observe every
already-committed tag) and asserts all three of: the report says
`status: "unchanged"`, the pull request count did not move, and the announce
branch head did not move. Any one failing exits 1 with the diff printed — an
`"updated"` here means the C6 contract broke, which is a Track A escalation,
not something to retry.

Needs `OCX_ANNOUNCE_TOKEN`.

## Machine-Lane Proof

`./scripts/run_machine_lane.sh <tag>` — requirement (3), G-19.

Push a tag **distinct** from the sequenced scenario's, so this is independent
evidence rather than a re-read of the same pull request. The script waits for
auto-merge, then classifies the lane from the merged pull request's reviews and
issue events by **numeric actor id**, and fails if `lane != "machine"` or any
human click is detected. A human acting on the pull request falsifies the proof
this driver exists to make; it does not "pass anyway".

Records both latency legs: tag-push → merge, and tag-push → served by
`index.ocx.sh`.

Needs no local token — the publisher's CI announces.

## Update-Union Proof

`./scripts/run_update_union.sh <tag-a> <tag-b>` — requirement (4), design
register C4.

Both tags must already exist on the registry; announce observes tags, it does
not create them. Runs announce twice with the first pull request deliberately
left unmerged (the script refuses to continue if someone merged it mid-run),
then asserts the second announce **updated** the same pull request rather than
opening a second one, and that its committed tag set is a superset of both
runs' tags.

Both runs use `--tags-from-file`, which adds to the committed curated set. `--tags`
*replaces* it, so run #2 would drop tag-a and the scenario would prove nothing.

Needs `OCX_ANNOUNCE_TOKEN`.

## Clean-Install Proof

`./scripts/clean_install_check.sh` — the last phase of the sequenced scenario,
also runnable alone.

Builds `docker/clean-machine.Dockerfile` (Debian trixie-slim, CA roots and
nothing else) with `$OCX_BINARY` staged into the build context, and runs
`ocx --format json package install <ns>/<pkg>` inside it. On failure the script
dumps the container's `config.toml`; the per-run image and staged context are
removed on exit.

Two things about this image are load-bearing and easy to get wrong:

**It stages a binary rather than downloading a release.** Index-kind resolution
is not in a release yet: ocx 0.4.3 rejects the config below with
`unknown field 'index', expected 'url'`, and `ocx self setup` would pull that
same released copy over anything staged. Point `OCX_BINARY` at a
dev-channel-equivalent build (`target/release/ocx` by default) — **not**
`test/bin/ocx`, whose `__testing` feature unlocks internal seams and would stop
the proof from reflecting a real user's machine (Key Decision D-7). A plain
`--release` build is a real user's artifact; that is what D-7 rules out
`__testing` in favour of. Base image is trixie because a locally- or CI-built
binary links against the builder's glibc, which bookworm's 2.36 cannot load.

**The config keys on the registry prefix, not the namespace.** The image
writes:

```toml
[registries."ocx.sh"]
index = "https://index.ocx.sh"
```

`ocx.sh` is not index-kind by default yet (register §6), so this says so
explicitly. Override the key with `E2E_INDEX_PREFIX` if the pilot package resolves
under a different registry. Keying it on `<ns>` instead parses fine and then
never matches — a `[registries]` key is compared against the registry component
of the resolved identifier, and `<ns>/<pkg>` resolves to `ocx.sh/<ns>/<pkg>`.
The failure is silent: ocx falls back to plain-OCI against `ocx.sh/v2/`, which
reads like "the index did not serve the root".

## Evidence Artifact

Each driver writes one `results/<run-id>-<scenario>.record.json` through
`evidence record`, which redacts on the way in. Roll them up with:

```sh
cd test && PYTHONPATH=src uv run python -m announce_e2e.evidence render \
    --records-dir manual/announce-e2e/results
```

That prints the same table as `.claude/artifacts/e2e_results_announce.md`, with
a `MISSING` row for every scenario no record covers — an incomplete evidence
set stays visibly incomplete. Paste the output into that artifact and commit it
as part of closing G-D; Track E's handover and Track F's doc-accuracy check
both read it.

`results/*.json` is gitignored. The rollup is the durable record.

## Cleaning up a rehearsal

A rehearsal leaves real state behind, and `git revert` undoes none of it.

```sh
gh pr close <n> --repo ocx-sh/index --delete-branch     # unwanted pull request
git tag -d <tag> && git push origin :refs/tags/<tag>    # retract a rehearsal tag
```

The tag lives in a Track C repository — coordinate before deleting anything
that repository's own CI may depend on.
