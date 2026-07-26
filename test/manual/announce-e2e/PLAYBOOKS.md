# Announce E2E Failure Playbooks

What to do when a driver in [README.md](./README.md) exits non-zero. Each
playbook names the diagnosis, the fix, and the thing not to do.

## Playbook: Floating Tag Not Advanced

**Symptom.** The publisher's CI runs against a stale ocx: a fix you deployed is
not in the announce it produces, or `ocx update` in the publisher keeps
resolving the same old digest.

**Diagnosis.** The publisher's `ocx.toml` pins the floating tag
`dev.ocx.sh/ocx/cli:<next>-dev` — never a `_<TS>` build segment. Compare what
that tag resolves to now against what the last Deploy Dev run published:

```sh
source ./scripts/env.sh
check_floating_tag dev.ocx.sh/ocx/cli:0.5.0-dev
```

That prints the currently-resolved digest and the `gh run list` command for the
Deploy Dev history. Digests differ → the deploy did not advance the floating
tag.

**Fix.** Re-run Deploy Dev:

```sh
gh workflow run "Deploy Dev" --repo ocx-sh/ocx --ref <branch>
```

`workflow_dispatch` only, and only for a branch that exists on `origin` — the
branch has to be pushed first.

**Do not** hand-pin a timestamped `<next>-dev_<TS>` tag to work around it, and
do not hand-edit `ocx.lock`. Refresh with `ocx update` only. A stuck floating
tag is a broken deploy; pinning around it hides the break and every later run
inherits it (register §7 step 5, verbatim rule).

## Playbook: validate.yml Red

**Symptom.** `run_sequence.sh` exits at phase (d): "validate.yml is not green".

**Diagnosis.** The index bot surfaces a structured reason for every
publisher-visible failure — never a bare exit (register §5, the observability
floor). Read it, in this order:

1. The failing check run's **step summary** on the pull request's Checks tab —
   this is where the bot writes the reason.
2. The bot's **comment on the pull request**, if it posted one.
3. Only then the raw job log:
   `gh run view <run-id> --repo ocx-sh/index --log-failed`.

The reason text is free-form by design, so there is no script here — read it
and route on what it says.

| Reason names | Route |
|---|---|
| a schema or field error in the rebuilt root | Track A — the client produced it |
| an unresolvable tag or digest | check the tag still exists on the registry; announce observes, it does not create |
| an SSRF-forbidden physical host | the root's `oci://<host>` pointer; `trusted_hosts` on the namespace's `[registries]` entry |
| a governance rule (owners, upstream, lane) | Track B — the index bot owns it |

**Do not** re-run the announce hoping it turns green. A red `validate.yml` on a
deterministic input stays red.

## Playbook: Registry Byte Identity

**Symptom.** `run_sequence.sh` exits at phase (g2): a committed
`o/sha256/<hex>.json` did not match what the registry serves under that digest.

**Read the taxonomy before concluding anything.** These four outcomes look
alike in a log and mean entirely different things. Reading a transport artefact
as a format regression sends the next hour into the wrong repository, which is
why (g2) exits with a distinct message per row.

| Observation | Verdict | Next step |
|---|---|---|
| **1.** 404 / `MANIFEST_UNKNOWN` | **The bug class** — the index locked a digest no registry serves | Pre-change: this is the reproduction, capture it (that is the whole point of the `1.0.3` run). Post-change: **this is also where a format regression lands** — a re-serialization in the announce path commits an object that hashes to its own filename and then 404s, because no registry ever stored those bytes. Look for a parse-then-write path; announce must copy `fetch_manifest_raw_bytes` verbatim |
| **2.** 200, `Docker-Content-Digest` ≠ requested | The registry content-negotiated | A **registry-behaviour** finding, not a mapping defect. **Do not** conclude a format regression. Re-request with a single `Accept` and compare |
| **3.** 200, header matches, bytes ≠ committed | **Registry integrity failure** — the registry advertised digest H and served bytes that do not hash to H | Not the announce path: the hash anchor below already proved `sha256(committed) == H`, and step 3 proved the registry claims H, so only the registry can be lying. Re-fetch; if it persists, escalate to the registry operator |
| **4.** 200, header matches, bytes == committed | **Proof** | Nothing to do; the digest lands in the evidence notes |

Row 3 is the rare one, and rows 1 and 3 are **not** two flavours of the same
defect: a re-serialization can only produce row 1, because the whole point is
that the committed digest exists nowhere but the index. Read the routing in row
1, not row 3, when you suspect the client.

A status that is neither 200 nor 404 — 401, 403, 429, 5xx — is a transport or
credential failure and gets its own message. It is **not** a row above. Re-run
after fixing the credential; a rate-limited GET says nothing about the format.

**Two earlier anchors fire before the registry is contacted**, so a failure
there is not a registry problem at all:

- *"nothing is committed at `o/sha256/<hex>.json`"* — a D2/D3 violation: the
  root names a `tags[].content` with no object behind it. Track B (the index
  bot renders `o/`), not the client.
- *"does not hash to its own filename"* — the CAS disagrees with itself.
  Everything downstream of that is meaningless; stop and escalate.

**And four refusals fire before even those**, on the served root itself. Each
says (g2) declined to judge, not that the mapping is wrong:

- *"malformed content digest"*, *"is not a host"*, *"is not an OCI repository
  name"*, *"does not name a usable physical repository"* — the root carries
  something that would have gone into a URL unvalidated. The root is
  publisher-controlled; a `?` in the repository path silently truncates the
  request into one that never asked for a manifest, and its 404 would read as
  row 1. Fix the root, or find out who wrote it.
- *"failed to parse"* / *"would pass vacuously"* / *"would attest D7
  vacuously"* — the served root did not parse, or committed no tags. A render
  regression or a CDN error page served with 200 both land here. Check what
  `/p/<ns>/<pkg>.json` actually returns before suspecting anything else.

**Do not** re-run (g2) hoping it turns green. Every input is a committed digest
and a content-addressed GET; a red (g2) on the same root stays red.

## Playbook: Lane Misclassification

Checklist only — no diagnostic script. This playbook fires when the automation
disagreed with what you expected, which is a human judgment call by
construction; the corrective action is an issue against the classifier, not a
script that "fixes" the lane (Key Decision D-6).

**What looks wrong.**

- A routine tag refresh sat waiting for human review when you expected G-19
  auto-merge.
- A first-claim or `owners[]`-changing pull request auto-merged when you
  expected human review. **This is the serious direction** — treat it as a
  governance incident, not a convenience bug.
- `run_machine_lane.sh` failed with `human_click_detected: true` on a pull
  request nobody touched.

**Check the benign causes first.**

1. **Path allowlist (most common, and new since this plan was written).** The
   hardened bot forces `human-review-required` on any pull request whose
   changed paths leave the touched roots plus those roots' own CAS objects at
   `p/<ns>/<pkg>/o/sha256/<64hex>.{json,md,svg,png}`. This is fail-closed and
   correct. A machine-lane scenario whose diff strays outside that scope — a
   stray file, a second package, anything under a different root — will
   correctly *not* auto-merge. Check the pull request's Files tab before
   suspecting the classifier.
2. **`owners[]` membership.** G-19 auto-merge needs the announcer's numeric
   `github_id` already in the claimed root's `owners[]`. Absent → human lane,
   correctly.
3. **`BOT_ACTOR_IDS` is wrong.** If the id in your environment is not the id
   that actually merged, `classify_lane` reports `human` for a genuine bot
   merge. Confirm with:
   ```sh
   gh api repos/ocx-sh/index/issues/<pr>/events --jq '.[] | {event, id: .actor.id, login: .actor.login}'
   ```
   Compare the **numeric id**, never the login. A matching login with a
   different id is exactly the recycled-login case the numeric check exists to
   catch — that is a correct `human` verdict, not a false positive.

**Escalate** when none of those explain it. Open an issue against the index
repository with:

- the pull request URL and the numeric actor ids off its events,
- the `evidence classify-lane` output,
- the lane you expected and why (`owners[]` state, the diff's paths).

**Non-goal.** Nothing in Track D auto-corrects a lane. Do not merge a pull
request by hand to "unblock" a machine-lane run — that destroys the evidence
the run exists to produce, and the recorded proof would be a lie.
