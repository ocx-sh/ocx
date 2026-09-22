# Security review — `adr_bazel_build_adoption.md`

Reviewer seat: `reviewer` (focus **security**), Round 1 design panel, `/hex-architect high`.
Subject: `/home/mherwig/dev/ocx-sion/.claude/artifacts/adr_bazel_build_adoption.md` (1279 lines, Status Proposed).
Date: 2026-09-21 · Model: opus · Read-only pass. The ADR was not edited.

**Verdict:** the cache-write design is argued from a premise that is false for this
repository's own PR shape, and it sanctions a second, ungated writer in its file-set
table. Both are `BZL-CACHE-01`/`-02` MUST violations that would ship as written.
Everything else is good-to-excellent: all eight of the research artifact's
`## Decisions this changes` bullets are adopted, three of them more strongly than
the research asked.

**Counts:** Block 2 · High 5 · Warn 5 · Note 1.
**Research bullets neither adopted nor refused: 0 of 8.** (Two are adopted as prose
obligations with no gate — see F11; one is adopted on a rationale that does not
hold — see F1.)

---

## 1. Enumeration — every credential and trust boundary this ADR introduces or touches

| # | Credential or trust boundary | Where it lives | Who can reach it | What decides a write | Verdict |
|---|---|---|---|---|---|
| 1 | **bazel-cache write credential** (nginx htpasswd, via `--credential_helper`) | ADR § Cache staging ruling 4, L868-874; ruling 5, L875-880 | The `smoke` job on `verify-basic.yml`. **Also every same-repo PR run of that job if wired like the precedent the ADR names** (`verify-basic.yml:92-100`) | `github.event_name == 'push' && github.ref == 'refs/heads/main'`, plus `--remote_upload_local_results=false` everywhere else | **Block — F1** |
| 2 | **Developer write credential** — "personal `--credential_helper`" in `.bazelrc.user` | ADR § The file set, L351 | Every developer who configures one. Reads need no credential, so a helper here can only be a write helper | Nothing. No gate, no review, no rotation | **Block — F2** |
| 3 | **The credential-helper executable** in `$RUNNER_TEMP` | ADR L872-874 | Every process in the job; Bazel spawns it pre-sandbox with the full client environment (the ADR's own BZL-CACHE-04 citation, L889-892) | An unspecified "written at job setup" step — no interpolation rule, no file mode, no cleanup | **High — F3** |
| 4 | **The Action Cache** (action-digest → output set) as distinct from the **CAS** | ADR L321 (`AC/CAS` in the C4 box) and nowhere else | Anonymous read: the entire internet. Write: boundary 1 | Nothing distinguishes the two halves anywhere in the ADR | **High — F4** |
| 5 | **`git_override` on `rules_ocx`** — analysis-time code execution, no `integrity` | ADR L985-990, ruling 9 L908-914, Open Question 3 L1213 | Whoever can push that branch. Executed by *anyone* running any `bazel` command in a fresh clone, including read-only `bazel query` | A commit-pin bump, reviewed as a supply-chain event (stated). Nothing gates the execution itself | **High — F5** |
| 6 | **Anonymous cache read** (`GET https://bazel-cache.ocx.sh/{ac,cas}/<key>`) | ADR L50-52, NFR Security L1003 | The entire internet; keys are computable from the public tree | n/a — read side | **High — F6** |
| 7 | **Tag-derived cache exclusion** on casts + acceptance (`local` ⇒ `no-remote-cache`) | ADR ruling 1, L797-806 | Any future edit to a `tags = [...]` list in `test/BUILD.bazel` or `test/doc_scripts/BUILD.bazel` | Prose only: "Any future tag trim is a BZL-CORE-01 violation" | **High — F7** |
| 8 | **`ocx.sh/bazelbuild/bazel:9.2.0`** — the build engine itself, via `ocx.lock` digests | ADR L976-983; `ocx.toml`, `ocx.lock:1-22` | The mirror author and the registry operator | Digest pin in `ocx.lock`; no signature, no `[[trust.policy]]` | **Warn — F11** |
| 9 | **`ocx.sh/asciinema/agg:<v>`** — a mirror package that **does not exist yet** | ADR L969, L980-983 (confirmed absent from the live catalogue) | Same as 8, plus: its first publication is authored during this initiative | Nothing stated | **Warn — F11** |
| 10 | **BEP JSON + `--execution_log_compact_file`** → `scripts/bep_to_otlp.py` → `otel.ocx.sh` | ADR § Observability, L916-950 | Anything on the runner; `verify-basic.yml:178-186` already uploads artifacts from this job | No field allowlist; no statement that `structured_command_line` is excluded | **Warn — F8** |
| 11 | **GitHub job `permissions:` on the two edited workflows** | **Absent from the ADR.** The file-set table (L344-361) lists no `.github/` path at all | n/a | n/a | **Warn — F9** |
| 12 | **bazel-remote `/status` + `--enable_endpoint_metrics`** | ADR ruling 8, L902-907 (UNVERIFIED, deferred to the server repo) | Unknown — possibly the same public listener as anonymous reads | n/a | **Warn — F10** |
| 13 | **`--action_env` / workspace-status stamping** as a cache-key and output channel | ADR ruling 3, L817-849; `crates/ocx_cli/build.rs:40-96` | Every cacheable Rust action | A prose ban on `CI`/`GITHUB_*`, argued from cache-splitting rather than from output nondeterminism | **Warn — F12** |
| 14 | **nginx read/write split** (auth on non-GET) | ADR ruling 7, L896-901 | Server operator | Kept as-is; native `--allow_unauthenticated_reads` explicitly refused pending buchgr/bazel-remote#468 | **OK** — research bullet 6 adopted verbatim |
| 15 | **Nerd Font `http_archive`** | ADR L675-676; A6 L1182-1188 | Upstream host | `integrity` attribute, asserted by the A6 grep with both red halves named | **OK** |
| 16 | **Website rule hermeticity** — the only sandboxed, shared-cacheable stage | ADR ruling 2 L807-816; A3 L1136-1152 | Every cache reader, if the check is skipped | A two-halves gate contract (cross-path hit, then mutated-undeclared-input miss); carries `no-remote-cache` until green | **OK** — research bullet 2 adopted in full |
| 17 | **`sccache` S3 write credential** (context, not this ADR's) | `verify-basic.yml:99-100` | Every `smoke` run, **including same-repo PRs** | `secrets.X != ''` in a job-level `env:` block | **Note — F1's evidence** |
| 18 | **`OTEL_OTLP_AUTH`** (context, unchanged) | `verify-basic.yml:105`, `:290` | Same as 17 | Step-level `if: env.OTEL_OTLP_AUTH != ''` | **Note** — pre-existing, untouched |

---

## 2. Findings

### F1 · **Block** · The PR lanes' credential-absence is asserted from a fork-only premise; this repository's PRs are same-repo

**ADR anchor:** § Cache staging, "Rulings the research demands" ruling 5 (L875-880) —
> "PR lanes stay on `pull_request` (**never** `pull_request_target`) with **no credential flag at all** — GitHub documents that for fork PRs 'no secrets are passed to the workflow', so **the write secret is never in scope for a PR-triggered job**."

**Threat.** The conclusion does not follow from the premise. GitHub withholds secrets
from **fork** PRs only. A pull request opened from a branch *in the same repository* —
the only PR shape this project actually uses (`goat`, `evelynn`, `sion`, `soraka`,
`feat/*`) — receives the full `secrets` context. If the bazel-cache write credential is
wired the way the ADR itself names as the precedent, it is present in the environment of
every same-repo PR run of the very job that holds it, and the only thing standing between
an untrusted contributor and the shared Action Cache is a flag value, not the absence of
a key.

**Evidence.**
- `/home/mherwig/dev/ocx-sion/.claude/rules/bazel-quality/caching.md:69`, BZL-CACHE-01, **MUST**, verbatim: *"Every CI lane an untrusted contributor can trigger — **any pull request, fork or same-repo** — runs the cache read-only, with the write credential **absent from that lane's environment**, not merely unused."* Its portable shape: *"a composite action that appends the authorization header only when its secret input is non-empty … **with the workflow supplying that input only on a trusted event**."*
- The ADR's own non-negotiable, L83-84: *"No secret reachable from a lane an untrusted contributor can trigger (BZL-CACHE-01, MUST)."* — the requirement is stated, then satisfied by an argument that does not reach same-repo PRs.
- The precedent the ADR points implementers at, `/home/mherwig/dev/ocx-sion/.github/workflows/verify-basic.yml:92-100` — a **job-level** `env:` block: `SCCACHE_ENABLED: ${{ secrets.SCCACHE_AWS_ACCESS_KEY_ID != '' }}` … `AWS_SECRET_ACCESS_KEY: ${{ secrets.SCCACHE_AWS_SECRET_ACCESS_KEY }}`. Its own comment (`verify-basic.yml:90-91`) says *"a **fork** PR has no secrets"* — i.e. a same-repo PR does, and gets a live write credential for the existing shared compile cache today.
- ADR L887 names that pattern as *"the closest structural precedent (`secrets.X != ''`)"* without saying that its secret **placement** must not be copied.
- Compounding, § Acceptance A2 (L1127-1131): *"**Write isolation:** run the PR-lane workflow with the write credential deliberately present in the environment and confirm `--remote_upload_local_results=false` still produces zero `PUT` lines."* This makes the state BZL-CACHE-01 forbids into the *test fixture*, and treats passing it as the isolation proof. It is a good defence-in-depth check; it is not the control.

*(The ADR's other clause on this — L880-884, "BZL-CACHE-01's verification treats the absence of that flag in an untrusted lane as the finding" — is a **correct** quotation of the rule's verification column, `caching.md:69`. The defect is in the normative half, not that sentence.)*

**Concrete fix.** Replace ruling 5's rationale and add the mechanism to the ADR:
the write secret is supplied to the job **only** through a trusted-event-conditioned
expression, so it is absent — not merely unused — everywhere else:

```yaml
# verify-basic.yml, job `smoke`
env:
  BAZEL_CACHE_WRITE: ${{ (github.event_name == 'push' && github.ref == 'refs/heads/main')
                         && secrets.BAZEL_CACHE_WRITE || '' }}
```
State that the credential is never placed in an unconditional job-level `env:` block,
and that `--remote_upload_local_results=false` is the *corroborating* check (per the
rule's verification column), not the control. Keep A2's write-isolation test, relabelled
as defence in depth, and add its true counterpart: on a same-repo PR run, assert the
credential env var is empty.

---

### F2 · **Block** · `.bazelrc.user` sanctions a second, ungated cache writer — BZL-CACHE-02's exact named shape

**ADR anchor:** § The file set this ADR creates, L351 —
> "| `.bazelrc.user` | Local opt-in: `--disk_cache`, **personal `--credential_helper`**. | **gitignored** |"

**Threat.** In this design reads are anonymous (ADR L50-52), so a credential helper on a
developer machine can only be a **write** helper. The table therefore blesses, as
ordinary local configuration, an unreviewed writer to the shared Action Cache running
unsandboxed builds of whatever branch the developer has checked out — which is the
Stripe/`#4276` threat model the ADR accepts elsewhere on the strength of "writes on one
main-only lane".

**Evidence.**
- `/home/mherwig/dev/ocx-sion/.claude/rules/bazel-quality/caching.md:70`, BZL-CACHE-02, **MUST**: *"Keep write access to the Action Cache strictly narrower than read access — never symmetric, and never held by a developer machine's default configuration. … **A developer rc file holding the write token is the second un-gated actor that a CI-only review never sees.**"* Its reading heuristic: *"enumerate every actor that could plausibly hold the write credential … More than one un-gated actor"* is the finding.
- The ADR's own NFR Security row, L1003: *"writes on one main-only lane via `--credential_helper` from a path outside the workspace"* — false as specified, because L351 creates a second one.
- ADR L788: *"**remote cache read-only, everywhere** … → **writes on the main lane only**"* — same contradiction.

**Concrete fix.** Change the L351 row to read: *"Local opt-in: `--disk_cache`, and a
`--credential_helper` **only** for a read-scoped credential; a cache-**write** helper on
a developer machine is a BZL-CACHE-02 violation."* Since reads are anonymous, the honest
form is simply to strike `--credential_helper` from that row. Back it with the A6-shaped
grep the ADR already uses for `http_archive`: a gate asserting no `--remote_upload_local_results=true`
and no write-scoped helper reaches the cache host outside the main lane, plus an nginx-side
control — bazel-remote's htpasswd realm should hold exactly one account, rotated on a
named schedule, so "who can write" is answerable from the server, not from trust.

---

### F3 · **High** · The credential helper's own provisioning is unspecified — the mechanism BZL-CACHE-03 exists to make safe

**ADR anchor:** § Cache staging ruling 4 (L872-874) —
> "On CI the helper is written to `$RUNNER_TEMP` at job setup and passed as `--credential_helper=bazel-cache.ocx.sh=$RUNNER_TEMP/...` on the command line of that lane only."

**Threat.** "Written at job setup" is the whole attack surface and it is one clause long.
Unspecified: (a) whether the secret is interpolated into the `run:` script body as
`${{ secrets.X }}` — GitHub materialises step scripts to a file under
`/home/runner/work/_temp/`, so that spelling writes the credential to a second on-disk
location and into the expression-expansion path; (b) the helper's file mode — `$RUNNER_TEMP`
is not private by construction, and the helper's whole job is to `stdout` a JSON object
containing the credential; (c) that the helper must be removed at job end; (d) that
`bep_to_otlp.py`'s input files and `actions/upload-artifact` must not sweep `$RUNNER_TEMP`.
Bazel spawns this program **before the sandbox, with the full client environment**, on
every invocation on that lane — the ADR cites exactly that property at L889-892 and then
does not carry it into the provisioning spec.

**Evidence.**
- `caching.md:71`, BZL-CACHE-03, **MUST, pinned**: the helper is mandated precisely because *"a static token is long-lived, unscoped, **readable by anything that reads the file**, and visible in process argv."* A world-readable helper script in `$RUNNER_TEMP` reintroduces the first half.
- `caching.md:68`, BZL-CACHE-04: *"Bazel resolves and spawns the helper with the full client environment, before the sandbox, with no check on where the flag came from."*
- The project already knows the right shape and the ADR quotes it two sections later — § Observability L920-924 cites `~/.config/ocx-telemetry/env` **mode 600** as the existing pattern. It is not applied to the helper.
- `/home/mherwig/dev/ocx-sion/.github/workflows/verify-basic.yml:178-186` — this job already runs `actions/upload-artifact`; nothing in the ADR scopes what may be uploaded.

**Concrete fix.** Write the four lines into ruling 4: the secret reaches the step through
`env:`, never through `${{ }}` inside `run:`; the helper is created with `umask 077` (or
`install -m 600`, then `chmod 700`); it is deleted in an `if: always()` step; and no
upload step may take a path under `$RUNNER_TEMP`. Add the red half the ADR demands of
every other gate: a job that asserts `ls -l` on the helper shows `0700` and that the
credential appears in no file under `$GITHUB_WORKSPACE`.

---

### F4 · **High** · The AC/CAS asymmetry — the poisoning-relevant half — is never stated, and there is no detection, recovery or rotation

**ADR anchor:** § Cache staging ruling 6 (L893-895) —
> "That is the 'CI is the trusted uploader' model, and [bazel#4276] is the production incident where it failed with **no adversary** … **This is accepted, not mitigated.**"

**Threat.** The ADR accepts the residual risk without ever writing down what the risk
*operates on*. `AC/CAS` appears exactly once in 1279 lines — inside an ASCII box at L321.
A reader of this ADR cannot learn that the CAS is self-verifying (Bazel rehashes the blob
against the requested digest) while the Action Cache is input-addressed and **not**
verifiable from its key; that bazel-remote's `--disable_http_ac_validation` checks proto
well-formedness only, never whether the referenced blobs are the honest output; or that
the attack is "upload a blob to CAS, then repoint one AC entry at it". Consequently the
ADR also has no: poisoned-entry **detection**, **recovery** plan, or credential
**rotation** schedule. Bazel's own documentation, quoted in the research, says
*"wiping the entire remote cache is not a feasible solution"* — so the accepted risk has
no exit.

**Evidence.**
- Grep over the ADR for `Action Cache|action cache|AC/CAS|CAS|content-address` returns two hits: `L321` (the diagram) and `L946` (a sentence about the *local* action cache in the execution log). No hits for `detection`, `rotate`, `rotation`, `blast radius`, `audit log`.
- `research_bazel_cache_trust_boundary.md` findings 1, 2, 3 and 6 are exactly this material; finding 6 quotes *"there is no way to verify the validity of an AC entry based on its key."* None of it reaches the ADR.
- ADR L1003 (NFR Security) summarises the posture in one row and lists no detective or corrective control.

**Concrete fix.** Add a short paragraph to § Cache staging stating the asymmetry
(CAS self-verifying on download; AC extrinsically trusted — *who was allowed to PUT* is
the entire control), and three operational lines that cost nothing now and are
unrecoverable later:
1. **A cache generation salt** — run the remote cache under a namespaced prefix or
   `--remote_instance_name=v1`. Abandoning a suspected-poisoned generation then costs one
   string bump in `.bazelrc`, not a 50 GB wipe.
2. **Detection** — alert on any `PUT` to the cache host outside a main-lane run window
   (bazel-remote already exports the counters, and Prometheus already scrapes it per L51-53).
3. **Rotation** — name the htpasswd rotation cadence and the owner.

---

### F5 · **High** · `git_override` on an out-of-tree branch is analysis-time code execution on a fresh clone — the ADR cites this property for helpers and not for the module extension

**ADR anchor:** § rules_ocx as the sole tool path (L985-990) and ruling 9 (L908-914) —
> "consumed here via `git_override(module_name = "rules_ocx", remote = …, commit = <sha>)`" … "verification is delegated entirely to git's commit hash, with no independent second attestation and no registry-level review."

**Threat.** The ADR's supply-chain analysis of `git_override` stops at *content
integrity* (git's hash-linked object model — correctly reasoned, and the commit-pin-bump-
as-reviewable-event obligation is right). It never states the larger property: a Bazel
**module extension runs arbitrary code at analysis time, outside the sandbox, with the
full client environment**, and `ocx.project()` (L967-969) additionally shells out to a
binary that performs network pulls. After this lands, `git clone && bazel query //...`
on this repository executes an unreviewed branch of a second repository plus network
fetches — before any `test`, `build` or human read. The ADR establishes exactly this
reasoning **four rulings earlier**, for credential helpers, citing the vendor's own
verdict that it is intended behaviour, and does not transfer the premise.

**Evidence.**
- ADR L889-892: *"a read-only `bazel query //...` on a fresh clone is enough to execute it (reproduced on 9.2.0, closed by the vendor as intended behaviour, [bazel#30439])"* — said of `--credential_helper`; the same pre-sandbox, full-environment execution applies to repository rules and module extensions.
- `caching.md:68` (BZL-CACHE-04) rationale: *"Bazel resolves and spawns the helper with the full client environment, before the sandbox, with no check on where the flag came from."*
- ADR L1213, Open Question 3, reaches the adjacent question (does the `ocx` pin enter the action key?) but frames it as a **cache-correctness** issue — *"a tool-provisioning change is invisible to the cache"* — not as a trust one.
- ADR L985-990: rules_ocx's state is *"owner-attested, not independently verified"*, and the branch is unreleased.

**Concrete fix.** Add to ruling 9: (a) name the analysis-time-execution property
explicitly, so the commit-pin-bump review is understood to be reviewing *code that will
run on every contributor's machine*, not just a version string; (b) require the pinned
SHA to be on a **protected** branch of `rules_ocx` with force-push disabled, so the pin
target cannot be made unreachable and history cannot be rewritten under it; (c) state the
availability consequence the NFR table omits — with rules_ocx as the sole tool path, an
unreachable git remote fails the build closed, unlike the cache outage which degrades to
a slow green (L1002); (d) state whether `rules_ocx` is public, because a private remote
makes a git credential a prerequisite for `task verify`.

---

### F6 · **High** · Anonymous read is never analysed as a content channel — only as a metrics surface

**ADR anchor:** § Non-functional requirements, Security row (L1003) —
> "writes on one main-only lane … **reads anonymous everywhere**; no `pull_request_target`; no remote execution"

**Threat.** The ADR's entire treatment of the read side is one clause asserting it and
one ruling (8, L902-907) about the Prometheus/`/status` surface. What the cache will
actually hold on the anonymous listener is never enumerated. Under stage 1, every
`rust_test` target's outputs — including **`test.log`**, which the floor reader depends on
(L604-606) — are uploaded to the CAS from the credential-holding main lane and are
readable by anyone who computes the key. Keys are not guessed: they are deterministic
functions of a public tree, a public `BUILD.bazel` and a pinned toolchain, so any reader
with the same checkout can derive them. For an open-source repository this is mostly
benign, and saying so in one sentence is all that is needed — but the ADR never gets far
enough to say it, and therefore never states the boundary it implies: **no cacheable
action on the writing lane may read an ambient secret**, because if one ever does, the
world-readable CAS is the recovery channel.

**Evidence.**
- `research_bazel_cache_trust_boundary.md` finding 19: *"The real exfiltration path is **prediction, not brute force** … **except** where an action's inputs (or its output) embed a value that is not meant to be public."*
- ADR grep: no occurrence of `exfil`; `anonymous` appears at L51, L902, L1003, L1016, L1116 — each an assertion or the metrics ruling, never an analysis of content.
- The writing lane is `verify-basic.yml`'s `smoke` job, whose job-level `env:` today carries `AWS_SECRET_ACCESS_KEY` (`:100`) and `OTEL_OTLP_AUTH` (`:105`). `--incompatible_strict_action_env` keeps those out of actions today; nothing in the ADR states that as a **standing invariant** for the lane that writes.

**Concrete fix.** Two sentences in § Cache staging: (1) the repository is public, so the
compiled form of readable code plus test logs is not a disclosure — state it and move on;
(2) the standing invariant — *no target on the writing lane may carry `--action_env` for a
secret-bearing variable, or be tagged `no-sandbox` without `no-remote-cache`* — with the
same A6-shaped grep the ADR already trusts for `http_archive`. One line also confirming
that `--remote_cache` traffic is TLS to the explicit `https://` scheme (the ADR pins the
scheme at L350 for a different reason — it is also what stops an on-path reader seeing
the CAS contents in clear).

---

### F7 · **High** · The tag corollary the research most insisted on is the one invariant in this ADR with no gate

**ADR anchor:** § Cache staging ruling 1 (L802-806) —
> "**Corollary, load-bearing:** a later 'let's cache acceptance tests too' must not drop the `no-remote-cache`/`local` half while keeping `no-sandbox` … **Any future tag trim on these targets is a BZL-CORE-01 violation**."

**Threat.** The reasoning is exactly right and matches the primary source. The
enforcement is a sentence in a document. This ADR's own non-negotiable (L85-86) is
*"Every gate this ADR creates has a demonstrable **red** state"*, and it honours that for
the pin check (L421), the BUILD drift check (L556), the floor (L609), the ceiling
(L617-618), the hermeticity check (L810-816), the telemetry parity check (L1157-1159) and
the tool-path grep (L1186-1188). The one invariant whose violation is *silent and
cross-machine* — a `no-sandbox` target that lost its `no-remote-cache` half — is the one
left to prose. Nothing reds when someone "optimises cache reach" in 2027.

**Evidence.**
- `research_bazel_cache_trust_boundary.md`, `## Decisions this changes` bullet 1: *"do not let a later 'let's cache acceptance tests too' change drop the `no-remote-cache`/`local` half while keeping only `no-sandbox`."* — the only bullet the research phrases as a *standing* obligation rather than a one-time decision.
- `caching.md:103`, BZL-CACHE-34: *"any action that reads ambient run-time state ships that state to every consumer of the cache"*, measured on 8.7.0 and 9.2.0 with a second checkout's output literally containing the first checkout's `output_base` hash.
- ADR L85-86 vs. L802-806: the standard the ADR sets for itself, and the one place it is not met.

**Concrete fix.** One query, in `.verify:lint` beside the two drift gates the ADR already
specifies, with the red half the ADR's own format demands:

```
bazel query 'attr(tags, "no-sandbox", //...) except attr(tags, "(no-remote-cache|no-remote|local)", //...)'
# non-empty output = exit 1, naming each target
# red state: add `no-sandbox` to one cast target's tags without `local` -> exit 1 naming it;
#            point the query at an empty universe -> exit 1 on the reader floor.
```

---

### F8 · **Warn** · No field allowlist for the BEP→OTLP pusher, so a future credential regression is silently exported

**ADR anchor:** § Cache staging ruling 4 (L862-866) —
> "`--announce_rc` and BEP's `structured_command_line` are a designed-in, fully-expanded transcript of every flag value. Whether `--remote_header` values are masked there is **UNVERIFIED** … Since this ADR's own BEP→OTLP script reads `--build_event_json_file` on the writing lane, `--remote_header` would put the credential directly in the script's input."

**Threat.** The ADR carries the UNVERIFIED honestly (also restated at L1023-1026) and
resolves it the strong way — `--credential_helper` from day one, so no secret is a flag
value. That closes the *current* exposure. It does not close the *class*: the research's
second half of that bullet — scrub `structured_command_line` before the JSON is read — is
dropped as unnecessary, which is defensible, but the ADR then never states what
`bep_to_otlp.py` **may** read. Since the script runs on the one lane that holds a
credential, and its input is a designed-in transcript of every flag and (via the
execution log) every action environment, a field allowlist is the cheap structural
version of the personnel obligation.

**Evidence.**
- Research bullet 4: *"Prefer `--credential_helper` … **otherwise scrub `structured_command_line`** from that JSON before it's read by the OTLP pusher."*
- ADR § Observability (L916-950) specifies the script's *sources* (`TestResult.test_attempt_duration`, `SpawnMetrics.total_time`, `ExecLogEntry.Spawn.runner`/`cache_hit`) but never states an exclusion.
- Ordering, as asked: safe today. The credential never enters the BEP at all under `--credential_helper`, so there is no window in which an unscrubbed JSON exists on disk. The residual exposure is the helper *path* under `$RUNNER_TEMP` — not a secret (see F3 for the file itself).

**Concrete fix.** One sentence in § Observability: `bep_to_otlp.py` reads only
`TestResult`, `TargetComplete` and `ExecLogEntry.Spawn`, and **never**
`structured_command_line`, `unstructured_command_line`, `OptionsParsed` or
`WorkspaceStatus`. Assert it with a unit test over a fixture BEP containing a planted
`--remote_header=Authorization=secret` line: the emitted spans must not contain it. That
is a red state the ADR's own standard would demand.

---

### F9 · **Warn** · The workflow edits are not specified to the precision `subsystem-ci.md` requires — no path, no `permissions:` block

**ADR anchor:** § The file set this ADR creates (L344-361) — the table lists
`.bazelversion`, `MODULE.bazel`, `.bazelrc`, twenty `BUILD.bazel` files and three scripts,
and **no `.github/` path at all**, while § Stage 2 ruling 1 (L578-582) changes
`verify-basic.yml`'s `smoke` job and `verify-deep.yml`'s Linux matrix leg.

**Threat.** The lane swap, the credential-helper setup step, the
`--remote_upload_local_results=false` addition on every other lane and the `if:` gate are
all workflow edits, and none of them is in the file set a reviewer would diff against.
`permissions:` is never mentioned in 1279 lines. Two specific traps go unnamed:
- **`workflow_call` inverts the gate's context.** `verify-deep.yml:5-10` is a reusable
  workflow composed into `release-readiness.yml`, and the file's own comment at `:77-79`
  says *"Under `workflow_call` the `github` context is the CALLER's"*. The ADR's gate
  expression (`github.event_name == 'push' && github.ref == 'refs/heads/main'`, L878-880)
  therefore evaluates against the caller wherever it is placed in a reusable workflow or a
  composite action under `.github/actions/**`. It is safe today only because the write
  lives in `verify-basic.yml`, which `verify-basic.yml:18-20` records as neither reusable
  nor composed. That safety is an accident of placement, unstated.
- **`merge_group` and `workflow_dispatch`.** Both workflows carry `merge_group: {}`
  (`verify-basic.yml:11`, `verify-deep.yml:28`) and `verify-deep.yml` carries
  `workflow_dispatch:` (`:4`). Neither satisfies `event_name == 'push'`, so neither writes —
  correct, but it means the queue run that immediately precedes a merge never warms the
  cache, and the ADR does not say that is intended.

**Evidence.**
- `/home/mherwig/dev/ocx-sion/.claude/rules/subsystem-ci.md:100`: *"**Minimal permissions** — declare at workflow level, elevate per-job"*; `:215`: *"Permissions — explicit, minimal, at workflow and/or job level"*.
- `subsystem-ci.md:98`: *"**SHA-pin every action** … Includes first-party actions (`ocx-sh/setup-ocx`): no floating-major carve-out."*
- Current blocks: `verify-basic.yml:13-16` (`contents: read`, `checks: write`, `pull-requests: write`) and `verify-deep.yml:36-39` (identical). The `smoke` job has **no** job-level `permissions:` override, so the Bazel lane would inherit `checks: write` + `pull-requests: write` alongside a cache-write credential.

**Concrete fix.** Add `.github/workflows/verify-basic.yml` and `verify-deep.yml` to the
file-set table with a one-line contract each, and state: the `smoke` job gets an explicit
job-level `permissions: { contents: read }`; the ref gate is placed only in a
non-reusable workflow, never in a composite action or a `workflow_call` target (naming
the caller-context inversion); and no new third-party GitHub Action is introduced —
which is true and worth recording, since Bazel arrives through `ocx.toml`/`ocx.lock` and
`ocx-sh/setup-ocx@25fa771…` is already SHA-pinned at `verify-basic.yml:58`.

---

### F10 · **Warn** · A2's acceptance evidence is read from the one surface the ADR has not confirmed is access-controlled

**ADR anchor:** § Acceptance A2, Green (L1116-1118) —
> "a `main` run increments bazel-remote's `http_cache` write metric."

and ruling 8 (L902-907) —
> "Whether hetzner1's nginx already separates them is **UNVERIFIED**; it is a config-review item for the server repo, not a code change here."

**Threat.** Deferring the metrics-surface question to the server repo is the right call
(research bullet 8 adopted). But A2's green then depends on reading that same surface,
and the ADR does not say with what credential or from where. If the answer is "it is
public", the acceptance check is easy and the disclosure question is answered the wrong
way; if it is "it is behind auth", the check needs a credential nobody has specified.

**Evidence.** Research finding 20: *"worth confirming this metrics/status port is not the
same public listener as the anonymous-read cache port, or is itself behind auth, since
it's a (minor) information-disclosure and DoS-targeting surface."* ADR L51-53 records
Prometheus already scraping it and Grafana already provisioning `cache.json`.

**Concrete fix.** Make A2's green read the metric **through Grafana** (already
provisioned, already authenticated — `project_monitoring_stack_and_telemetry`), not
through the cache host, and sequence ruling 8's config review **before** A2 rather than
beside it.

---

### F11 · **Warn** · New mirror packages on the build's critical path carry no provenance statement

**ADR anchor:** § rules_ocx as the sole tool path (L980-983) —
> "| `agg = "ocx.sh/asciinema/agg:<v>"` | **Does NOT exist.** Confirmed absent from the same catalogue. | The mirror package … is a **hard prerequisite on the critical path** |"

**Threat.** This ADR makes two new OCX packages build-critical — one of them the
**build engine itself** (`ocx.sh/bazelbuild/bazel:9.2.0`), the highest-value
supply-chain target in the whole design — and says nothing about how their provenance is
established. The digest pin in `ocx.lock` gives integrity (the ADR argues this well at
L396-400, and the `:9.2.0`-not-`:9` correction is a genuine improvement over the dossier),
but a digest attests only *"the same bytes as last time"*, not *"the bytes upstream
published"*. The `agg` package does not exist yet, so its very first publication happens
inside this initiative — the one moment when pinning provenance is free.

**Evidence.**
- Grep over the ADR for `cosign|sigstore|signed|signing|trust.policy|attest`: every hit is incidental (a tool name in `ocx.toml`, a submodule path, an `http_archive` `integrity` attribute). No hit discusses signing a package.
- `/home/mherwig/dev/ocx-sion/ocx.toml` declares no `[[trust.policy]]`; `ocx.lock` carries per-platform digests only (`:13-19`). So this is an inherited posture, not a regression — but it is widened.
- Keyless Sigstore signing with identity-pinned `[[trust.policy]]` is this product's differentiator #12 (`product-context.md`).

**Concrete fix.** One row in § rules_ocx: state the posture explicitly (digest-pinned,
unsigned, consistent with the nine existing tools), and for the two new entries require
the mirror's publication to record the **upstream release checksum** it was built from —
upstream publishes SHA-256 for every Bazel binary — so the chain terminates at an upstream
attestation rather than at the mirror author. If `ocx package sign` is applied to the two
new packages, say so; it is the cheapest possible dogfood of differentiator #12 on the
exact packages that most warrant it.

---

### F12 · **Warn** · The `--action_env` ban is argued from the weaker hazard, and "keep the SHA in the volatile half" is safe only under a premise stated three bullets away

**ADR anchor:** § Cache staging ruling 3 (L826-849) —
> "Every `--action_env` var enters the cache key: `GITHUB_RUN_ID` changes every run and would make the action permanently uncacheable; bare `CI` splits the cache into two disjoint universes that never share a hit."
> … "**Route build metadata through `--workspace_status_command`**, and keep the git SHA in the **volatile** half."
> … "**Therefore: stages 1 and 2 build no `rust_binary` at all.**"

**Threat — two parts, both in a correct conclusion.**

*(a) The stated reason for banning `CI` is cache economics; the real reason is output
nondeterminism.* `crates/ocx_cli/build.rs:40-42` gates `build_timestamp(in_ci)` on the
presence of `CI`, and its own comment says the timestamp *"emits the current UTC time
every invocation"*. Under `--action_env=CI`, that build-script action becomes a
**remote-cacheable action with a wall-clock-varying output** on the lane that writes the
shared cache — a `#4276`-class poisoner, not a cache-split inefficiency. A future reader
weighing the ADR's stated rationale ("`CI` is constant within CI, so the split costs us
one universe") can reverse the ban on its own terms. The hazard should be named where the
ban is.

*(b) "Keep the git SHA in the volatile half" is the unsafe configuration in isolation.*
The ADR states the trap in both directions (L836-842) and then removes it by building no
`rust_binary` — which is stronger than any of the three options the research offered, and
is the right call. But the standing instruction and the premise that makes it safe sit in
different bullets. Whoever adds the first `rust_binary` must remember both; the ADR's
`--nostamp` escape is one clause at L847-849 with no gate behind it.

**Evidence.**
- `/home/mherwig/dev/ocx-sion/crates/ocx_cli/build.rs:36-42` — *"build_timestamp(true) emits the current UTC time every invocation … Only enable under CI"*; `let in_ci = std::env::var_os("CI").is_some();`
- `caching.md:101`, BZL-CACHE-20, and research finding 24 (`bazel#5573`) — the volatile/stable trap, both directions.
- ADR L830-834 also slightly overstates its own verification: *"The six `GITHUB_*` vars **only populate `ci.run_url`**."* Verified at `crates/ocx_cli/src/app/build_info.rs:146-157` — they populate `ci.run_url` **and** `ci.workflow`, `ci.git_ref`, `ci.sha`. The material claim ("no bearing on build correctness") is **true**: all four are optional fields of the same `ci` block, every one reached through `option_env!()`, and the module's own test doc-comment (`build_info.rs:161-172`) states each block *"is present when its backing env vars happen to be exported at test-binary build time … absent locally"* — so their absence under Bazel breaks no test. Filed as a Note below, not as a defect in the ruling.

**Concrete fix.** In ruling 3, add the real reason beside the stated one — *"`CI` also
flips `build.rs` into emitting a wall-clock timestamp (`build.rs:36-42`), which makes the
action's **output** nondeterministic on the lane that writes the shared cache; that, not
the cache split, is why the ban is unconditional"* — and move the `--nostamp` escape into
the standing rule: *any* `rust_binary` added to the graph carries `no-remote-cache`
**or** `--nostamp`, asserted by the same query gate proposed in F7.

---

### N1 · **Note** · Two small precision items

1. **L830-831**, *"The six `GITHUB_*` vars only populate `ci.run_url`"* — they populate the
   whole `ci` block (`build_info.rs:146-157`: `run_url`, `workflow`, `git_ref`, `sha`).
   The conclusion is unaffected; the sentence is narrower than the code.
2. **L1116**, A2's green includes *"a PR lane with no secret still reads the cache
   anonymously"*. Under this repository's PR shape (F1), a PR lane **does** have secrets;
   the property being tested is "reads without presenting a credential". Rewording it
   removes the same false premise from the acceptance criteria that F1 removes from the
   ruling.

---

## 3. Research artifact adoption — item by item

`research_bazel_cache_trust_boundary.md` § `## Decisions this changes`, all eight bullets.

| # | Research bullet | ADR disposition | Where | Assessment |
|---|---|---|---|---|
| 1 | Tag casts + acceptance `local`; don't let a later change drop `no-remote-cache` while keeping `no-sandbox` | **Adopted**, with the corollary elevated to "load-bearing" | ruling 1, L797-806 | Reasoning correct; **no gate** → F7 |
| 2 | Explicit hermeticity check for the website rule before it touches the shared cache | **Adopted**, as a gate contract with both red halves | ruling 2, L807-816; A3, L1145-1152 | Full adoption, stronger than asked |
| 3 | No `CI`/`GITHUB_*` in `--action_env`; route metadata through workspace status; volatile half, or `no-remote-cache`, or split `--nostamp`/`dist` | **Adopted and superseded** — stages 1-2 build no `rust_binary`, removing the conflict rather than managing it | ruling 3, L817-849 | Strongest of the four available options; rationale/premise placement → F12 |
| 4 | Prefer `--credential_helper` over `--remote_header`; otherwise scrub `structured_command_line` | **Adopted**, and hardened: BZL-CACHE-03 is a MUST for new setups regardless of BEP | ruling 4, L850-892 | Correct; the dropped second half is defensible → F8 |
| 5 | PR lanes on `pull_request`, never `pull_request_target`, no `--remote_header` | **Adopted** | ruling 5, L875-887 | Adopted on a **false premise** (fork-only) → **F1** |
| 6 | Keep the nginx read/write split; do not migrate to `--allow_unauthenticated_reads` before ruling out #468 | **Adopted** verbatim | ruling 7, L896-901 | Full adoption |
| 7 | Treat every `rules_ocx` commit-pin bump as a reviewable supply-chain event | **Adopted**, plus a named exit (drop the override at the first BCR release) | ruling 9, L908-914 | Full adoption; scope stops at content integrity → F5 |
| 8 | Confirm `/status` + `--enable_endpoint_metrics` are not on the public anonymous listener | **Adopted** as a config-review item, marked UNVERIFIED | ruling 8, L902-907 | Adopted; sequencing against A2 → F10 |

**Failed to adopt or explicitly refuse: 0 of 8.**

---

## 4. Answers to the seven questions, in one line each

1. **Cache poisoning / integrity.** Write credential: held by `verify-basic.yml`'s `smoke` job, gated on `push` + `refs/heads/main`, carried by `--credential_helper` from `$RUNNER_TEMP` — but its *absence* from PR lanes rests on a fork-only premise that does not hold here (**F1**), a second ungated writer is sanctioned in the file-set table (**F2**), and the helper's provisioning is one clause (**F3**). Blast radius and detection: the AC/CAS asymmetry, poisoning detection, recovery and rotation are absent entirely (**F4**).
2. **Credential exposure paths.** Resolved well: `--credential_helper` from day one means no secret is a flag value, so `.bazelrc`, `--announce_rc`, BEP JSON and the execution log are all clean, the UNVERIFIED masking question is carried honestly (L862-866, L1023-1026) and the ordering question is moot. Remaining: the helper file itself (**F3**) and no BEP field allowlist (**F8**).
3. **Non-hermetic actions.** Correctly ruled: `local` ⇒ `no-remote-cache` is cited from the primary source, casts and acceptance are safe by construction, and the website rule — the only sandboxed cacheable stage — is gated on a two-halves hermeticity check before it may touch the shared cache. Not pinned: the tag corollary has no gate (**F7**).
4. **Supply chain of the tool path.** Threat model is stated for content integrity, the commit-pin bump is a reviewable event, the BCR exit is named, and the `MODULE.bazel.lock` question is explicitly *not* claimed as a security control. Missing: analysis-time execution on a fresh clone, force-push/unreachable-remote consequences (**F5**), and any provenance statement for the two new mirror packages (**F11**).
5. **Anonymous read as an exfiltration channel.** Reasoned about only as a metrics surface (ruling 8); the content question is never asked (**F6**), and A2 reads the very surface ruling 8 has not cleared (**F10**).
6. **Stamping and strict action env.** The conclusion is sound and the chosen alternative — no `rust_binary` in stages 1-2 — removes the volatile-SHA hazard rather than managing it. `build.rs` verified: the six `GITHUB_*` vars feed only the optional `ci` block (`build_info.rs:146-157`), so the "no bearing on build correctness" claim holds. The stated rationale names the weaker hazard (**F12**).
7. **CI workflow changes.** Not specified to `subsystem-ci.md`'s precision: the workflow files are absent from the file-set table, `permissions:` is never mentioned, and the `workflow_call` caller-context inversion goes unnamed (**F9**). One positive worth recording: **no new third-party GitHub Action is proposed** — Bazel arrives through `ocx.toml`/`ocx.lock`, and every existing action in both workflows is already SHA-pinned.
