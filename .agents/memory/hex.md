# hex — swarm memory

Maintained by the hex skills. Small by contract: pointers and preferences,
not copies. Team-shared — commit it.

## Pointers

- Verification: `CLAUDE.md` › "Build & Development" — `task verify:scoped
  --force` per task and review-fix iteration (escalates to the full gate
  when a path demands it); `task verify` (full) at the work-package merge
  and at finalize; `task` = fast check.
- Research artifacts: `.claude/artifacts/research_<topic>.md` (project
  convention, per `.claude/templates/artifacts/adr.template.md`; committed).
  `hex-discuss` lanes default to `.agents/research/` (gitignored) — copy into
  `.claude/artifacts/` before an ADR cites them (re-pointed 2026-09-06).
- Plan / ADR conventions: `CLAUDE.md` › "Workflow" — planning flow
  ADR → Design Spec → Plan; artifacts in `.claude/artifacts/` (patterns
  `adr_<topic>.md`, `design_spec_<comp>.md`, `plan_<task>.md`), templates
  in `.claude/templates/artifacts/`. Plan Status protocol:
  `.claude/rules/meta-ai-config.md` › "Plan Status Protocol" (active plans
  in `.claude/state/plans/`, pointer in `.claude/state/current_plan.md`).
- Product knowledge: `.claude/rules/product-context.md` — canonical
  identity doc (positioning, users, competitors), indexed from `CLAUDE.md`.
- Key rules: catalog `.claude/rules.md` ("By concern" table); architecture
  boundaries + ADR index `.claude/rules/arch-principles.md`.
  Security-sensitive paths: `.github/workflows/**`, `.github/actions/**`
  (`.claude/rules/quality-security.md`), `crates/ocx_oci/**` and
  `crates/ocx_sign/**` (auth/SSRF/wire formats per the CLAUDE.md model
  policy), `crates/ocx_trust/**` (who may sign), `crates/ocx_config/**`
  (which registries may be reached over plain HTTP, which extra CA roots are
  trusted, which `[shell.consent]` grants stand) and `crates/ocx_store/**`
  (the on-disk layout every install writes through).
- Worktrees: default `.agents/worktrees/` (gitignored, `.gitignore:50`).
- Federation: this repo is the **lead**. Satellite keys, for plans carrying a
  `Repo` column:
  - `mirror-asciinema` → `/home/mherwig/dev/mirror-asciinema` (trunk `main`; no
    remote yet — the repository is owner-gated, see `.agents/owner-actions.md`)
  - `server-hetzner1` → `/home/mherwig/dev/server-hetzner1` (trunk `main`)
  - **Satellite worktree deviation.** Neither satellite creates a
    `.agents/worktrees/<wp>` tree: each is a single-writer checkout already on
    the branch its work belongs to, and the owner reviews those branches
    directly. Satellite WPs commit in place on that branch. Consequently the
    C-303 pre-flight's clause (vi) (`.agents/worktrees/` must be ignored) does
    not gate here — no such path is ever written.
- Constitution: `.claude/rules/arch-principles.md` (optional gate; plans
  checked against it when present).
- Discussions: `.agents/discussions/<slug>.md` (hex-discuss artifacts;
  research they spawn lands in `.agents/research/`).

## Preferences

```yaml
# hex config, vocabulary v2. Unknown keys warn once and are ignored.
models:
  fast-balanced: sonnet
  deep-reasoning: opus
  overrides:
    reviewer:quality: deep-reasoning
    reviewer:security: deep-reasoning
    reviewer:performance: deep-reasoning
    reviewer:spec: deep-reasoning
adversary: codex:rescue
perspectives:
  always:
    - role: reviewer:security
      when: "{.github/workflows/**,.github/actions/**,crates/ocx_oci/**,crates/ocx_trust/**,crates/ocx_config/**,crates/ocx_store/**,crates/ocx_sign/**}"
research-axes:
  - registry ecosystems / OCI spec evolution
  - package-manager UX (mise, asdf, volta, proto)
  - shell integration mechanisms
```

- Review is never downgraded to save cost — the reviewer overrides above
  encode the CLAUDE.md "MODEL POLICY — NON-NEGOTIABLE" table.
- Fable/Mythos is the session orchestrator only, never a spawn target
  (CLAUDE.md model policy; matches models.md Rule 4).
- **`adversary: codex:rescue` is the configured name, not the seat that has run.** Codex has been
  quota-exhausted since the B4 batch of the crate split (to 2026-09-19), and the gate has been
  standing up under `copilot`, invoked per-file and read-only from a throwaway worktree, argv-bound
  at the kernel's 128 KiB cap. It has produced findings no Claude seat reached in every batch it ran.
  A gate that runs under a substitute is `ran`; a gate that does not run is **`absent`**, never
  `skipped` — the two words carry different weight in a verdict and the reader cannot recover which
  one was meant.
- **`perspectives.always.when` grows one crate per extraction, in the commit that extracts it.**
  A path-scoped rule whose subject migrates stops firing and nothing reports it — same class as
  the plan's DEC-30 item 1 and the B5 review's B5-22, one level up. WP-24 added `crates/ocx_oci/**`
  and **kept** the monolith's own `src/oci/**`: DEC-34 calls that tree the one WP-24 empties, but it
  does not — `index/`, `sign/`, `attest/`, `verify/` and `simplesigning.rs` stay behind, so a
  re-point that dropped the old glob would have silently un-guarded the signing tier. The rule is
  *add the new crate*, and drop a glob only once its tree is genuinely empty. Still owed, each in
  its own extraction commit. WP-25 added `crates/ocx_trust/**` — trust policy decides *who may
  sign*, so it belongs in an always-on security perspective even though its old home,
  the monolith's `src/trust.rs`, was never matched by any path-scoped rule. WP-26 added
  `crates/ocx_config/**` on the same ground: the config tier decides which registries may be
  reached over plain HTTP (`insecure`), which extra CA roots are trusted (`tls`), and which
  `[shell.consent]` grants stand — and, like the trust tree, its old home was matched by no
  path-scoped rule at all, so this is a property the glob gains rather than one it keeps. WP-27
  added `crates/ocx_store/**`. WP-31 added `crates/ocx_sign/**` and, for the first time, **dropped**
  a glob: the monolith's `src/oci/**` is the one tree the rule above says may go, because the
  extraction took the last four submodules and the `oci.rs` that declared them, so the directory is
  gone rather than merely thinner. Nothing is owed here now; C-075's WP-38 sweep is the backstop,
  not the schedule.

## Memory

- **Active plan (goat, 2026-09-25): `.claude/artifacts/plan_bazel_cargo_port.md`** — port the
  remaining `task verify` cargo compiles (schema, clippy, doc ratchet, doctests) to cached Bazel
  actions on `refactor/bazel-test-binary` (PR [ocx-sh/ocx#527](https://github.com/ocx-sh/ocx/pull/527)).
  `/hex-execute` high, 4 serial WPs run in place on the branch; pointer in `.claude/state/current_plan.md`.
- **Plan `plan_test_speed_tiers.md` executed 2026-09-22…24 (goat checkout, `hex/test-speed-tiers`)** —
  14 WPs merged, `State: review`, `Next: /hex-review`. Lessons: (1) a *resumed* subagent's final
  report is delivered to the session lead, not to the sub-orchestrator that resumed it — ask every
  resumed worker to write its report to a file and poll that; (2) from WP-07 on, every WP merge is
  `git merge --no-ff --no-commit -m "Merge WP-…"` → stage the plan → `task verify --force` → commit
  (`.tmp/hex-tiers/merge.sh` shape); shape-(e) ported-from ranges can only be guarded AFTER the
  merge commit exists (tip must be an ancestor of HEAD, clean tree); (3) any `Cargo.toml` edit
  stales each checkout's gitignored `Cargo.bazel.lock.json` — repin before the merge verify
  (`TMPDIR=/var/tmp/ocx-splice CARGO_BAZEL_REPIN=1 bazel fetch --repo=@crates --repo_env=…`);
  (4) the cross-model adversary found four holes all seven opus leaf seats missed — keep it.
- **Active plan (sion): `.claude/artifacts/plan_bazel_build_adoption.md`** — the Bazel
  adoption ([ADR](../../.claude/artifacts/adr_bazel_build_adoption.md) Accepted 2026-09-21).
  `/hex-plan high`, 2026-09-21. **31 WPs, 12 waves, 4 repos** (ocx, rules_ocx,
  mirror-asciinema, server-hetzner1). Discover wave of 5 + 1 research axis; review panel
  (spec/architect/SOTA, all opus) + `codex:rescue` **ran** — 30 findings, 6 Block, one fix
  round, re-validation converged.
  **Three sibling pre-works landed mid-plan and were folded in (R2):** rules_ocx on Bazel
  9.2.0 done and pushed (branch `bazel-9` @ `9ced5ffb`, `ocx-sh/rules_ocx` PR #15, 80/80) →
  WP-00b **withdrawn**; the `agg` mirror spec built and validated (`~/dev/mirror-asciinema`
  @ `02e5fd1`, 6 platforms) → WP-29 resized to publication-only and **WP-33b split out** so
  the owner gate blocks one S-sized WP instead of the cast stage; the server reader realm
  implemented and tested (`bazel-cache-reader-realm`, `b469ae1`/`8667a34`) → WP-25 resized.
  Their common consequence is **C-029 — no red/green proof may depend on the owner-gated
  remote realm**; the whole corpus runs on `--disk_cache`, so every gate is demonstrable
  before the owner applies anything. Measured tool-path contract worth keeping: the tool
  digest lives in the **launcher text**, so actions re-key on a tool change (ADR ruling
  2(c) is green by construction) but the absolute-path launcher makes **RBE structurally
  impossible**, and only executables are exposed — no filegroups for package data.
  Research: `.claude/artifacts/research_bazel_mechanism_verification.md`.
  `Next: /hex-execute .claude/artifacts/plan_bazel_build_adoption.md`.
  **Lessons.**
  (1) **The ADR's stage-2 design rested on a premise one `sed` of the live taskfile
  refutes.** `rust:test:floor` (`taskfiles/rust.taskfile.yml:591-614`) runs
  `cargo nextest list --message-format json` — a **declaration** check that never reads a
  run, so moving *execution* to Bazel does not touch it. The ADR and this plan's first draft
  both specified a BEP + libtest-output reader, an L-sized WP, and a Block-tier "Don't Own
  Non-Domain Code" deviation, to rebuild a gate that did not need rebuilding. The architect
  seat found it. **Read what a gate actually executes before designing its replacement** —
  the discover report had quoted the exact command and nobody drew the conclusion.
  (2) **A measurement can fill a signal row and still not answer it.** `verify-deep`'s
  largest job is `Build & Unit Test (Windows)` (1250/1456/1487 s), not acceptance — which
  reads like go/no-go clause 4 firing. But the lane is Linux-only and a 3-OS matrix stage
  costs `max(legs)`, so **stage 2's contribution to `verify-deep` is structurally zero**.
  The ADR asked "which job is largest **and how much of it is compile**"; only the first
  half was measured. A verdict written off half a signal leaves the row empty.
  (3) **The plan dropped the ADR's own WP-1c and thereby deleted A2's abort.** Two seats
  caught it independently. An abort with no work package that can fire it is a decorative
  gate — the failure the ADR wrote a subsection against. Re-added as WP-30, gating the swap.
  (4) `--remote_instance_name` is **inert over an HTTP remote cache** (Bazel's HTTP client
  never puts it in the URL), so the ADR's "abandoning a poisoned generation costs one string
  bump" exit did not exist. Needs a URI path prefix + `--enable_ac_key_instance_mangling`
  server-side. A cheap-sounding mitigation nobody priced against the actual deployment.
  (5) `--local_test_jobs=1` is **global, never per-target-pattern** — the ADR scoped it "for
  the `//test/...` package", which would have serialised the Rust tests too and deleted the
  win. The per-target mechanism is the `exclusive` tag.
  (6) **`external/` are mode-160000 submodules**, so `external/*/BUILD.bazel` cannot simply
  be committed in the parent repo. Only the cross-model seat raised it.
  (7) The cross-model gate earned its keep an **eighth** time: 3 of its Blocks were net-new,
  including (6) and a semantic dependency cycle (WP-32 gated WP-34 while needing WP-34's
  rule as its subject) that four opus seats missed.
  (8) Preference hint for the next `/hex-init`: the seat that paid here was **SOTA /
  known-pitfall**, which produced a flag-existence table for a binary the repo does not yet
  have. BZL-FLAG-11's "prove it on the pinned binary's two help surfaces" is unrunnable
  before adoption, so a documentation-sourced table is the only pre-flight available — and
  it found six wrong flag positions.
- **Design record (hex-architect high, 2026-09-21, dossier fast path): `.claude/artifacts/adr_bazel_build_adoption.md`
  (Status Proposed), on `sion`.** From `.agents/discussions/bazel-full-adoption.md`. Bazel 9.2.0 /
  rules_rust 0.74.0 under Taskfile, four staged surfaces, `ocx.lock` as pin authority. Amended
  `adr_crate_split_workspace.md:19` in place (**not** superseded — the crate split stands in full;
  only its Bazel clause reopens). ADR index row added to `arch-principles.md`. Discover:
  `discover_bazel_full_adoption.md` (claim diff: 1 WRONG, 3 contradictions). Research axes
  security&compliance / technology-verification: `research_bazel_cache_trust_boundary.md`,
  `research_bazel_toolchain_verification.md`; operability&cost and design-precedent skipped on
  unexpired dossier citations. Panel (spec, quality, security — all opus) + Codex `nox-review`:
  **10 Block / 16 High**, one fix round, re-validation 30/30 closed.
  **Lessons.**
  (1) **A dossier's own `## Decisions` can rest on a premise a landed sibling record already
  refuted — and the root cause was a stale README.** The steelman duty found it and I verified it:
  `bazel-adoption-timing.md:50` ("Anonymous 403; bazel-cache reads now 401", 2026-09-20, server work
  *landed*) contradicts the next day's dossier asserting anonymous reads — and this file's own
  Memory row agreed with the 401 all along. Settled by measurement on hetzner1:
  `nginx/.../ocx-sh-10-bazel-cache.conf:29-33` puts `auth_basic` on `location /` with **no
  `limit_except`**, so GET is 401; `/srv/sh.ocx/bazel-cache/README.md:7-8` still says "reads are
  anonymous" and **that stale line is what the discussion rested on**. `bazel-cache.ocx.sh` **is
  not anonymously readable** — fix the README before it misleads a third decision.
  **Two-party agreement between an owner and one discussion agent is unexamined.** Read the
  *preceding* decision in the same series, and the live config, not the dossier's own citations.
  (2) **`nox-review` on a path under `.claude/` reviews NOTHING and still returns `status: ok`.**
  Its neutralizer strips agent-config paths by pattern: `counts: neutralized=1 of 1`, empty
  checkout, `verdict: needs-attention` about the missing document. Copy the artifact to a path
  outside `.claude/` first, then delete the copy. A `status: ok` whose `counts:` line says the
  subject was neutralized is the textbook unchecked green.
  (3) The re-run gate earned its keep a **seventh** time — 3 of its 4 Highs were net-new, including
  a hermeticity check that was *inverted* (it demanded a cache miss after touching an **undeclared**
  file, which a correctly isolated action legitimately survives as a hit, so it could never
  discriminate). No opus seat caught it.
  (4) **The fix round introduced a new wrong number beside a correct fix** — `54` (all
  `//crates/...` targets) substituted for `34` (test targets) at four sites, making its *own* floor
  contract unsatisfiable. Budget a re-validation for every correction pass; this is the second
  dataset for that rule.
  (5) A count nobody questioned was off by an order of magnitude: the ADR and all three reviewers
  carried "~15 acceptance modules"; `ls test/tests/test_*.py` is **172**, which moves the stage-4
  target count to ~274 and all but reaches BZL-CI-01's tripwire on day one. Re-derive counts,
  never inherit them.
  (6) **Measured 2026-09-21, supersedes what this file and the dossier carried.** CI medians over
  the last 50 successful runs: **verify-basic 1731 s, verify-deep 3407 s** — the `3347 s` in the
  2026-09-20 row below is stale, and the narrower fix has shown **no demonstrated movement** (the
  controlled before/after is still owed). `ocx.sh/bazelbuild/bazel` publishes 9.0.0…9.2.0 over five
  platforms (no linux/arm64 musl). `agg` has **no mirror sibling at all** — the prerequisite is a
  new `mirror-asciinema` repo on the `../mirror-bazelbuild/bazel/mirror.yml` pattern, not a tag
  bump. **`rules_ocx` is v0.4.0, not the 0.1.0 the dossier claimed** — clean on `main` @ `825f20b`,
  `ocx.project()` already live in `//ocx:extensions.bzl` and dogfooding its own lock; "outdated"
  is the 8.7.0 pin and being unverified on 9.x, nothing missing. Host envelope: 32 GB / 32 cores,
  `/tmp` tmpfs 76 % full.
  `Next: /hex-plan high "Bazel build adoption, per .claude/artifacts/adr_bazel_build_adoption.md"`
  — WP-0 is `bazel-adopt`'s 11 steps. Two orchestrator rulings were taken in autonomous mode and
  are recorded in the ADR **pending owner ratification**: the read-credential branch for the 401
  cache, and the ≥ 4 min / < 150 ms p50 performance threshold with WP-1a/WP-1c budgets.
  Preference hint for the next `/hex-init`: the axes that carried this ADR were
  **security & compliance** (shared-cache trust boundary) and **technology verification**
  (converting a prior lane's `UNVERIFIED` markers into primary-source facts) — the second is a
  *reusable shape*, not a topic: when a dossier cites research that flagged its own gaps, that axis
  is not covered.
- **Active plan: `.claude/artifacts/plan_test_speed_tiers.md`** (hex-plan, tier high,
  2026-09-22), implementing `.claude/artifacts/adr_test_speed_tiers.md` (+ Amendment AM-1…AM-8).
  State `plan-approved`, 14 WPs in 9 waves; pointer in `.claude/state/current_plan.md`.
  Stage 7 porting waves need a follow-up plan after the pilot's GO. Cross-model plan review was
  PARTIAL (Codex quota; thread `01a0ca87-0ca8-7052-b3a9-c054a282d4fa`). Deferred owner calls:
  B2 (diff-guard shapes opt-in via `--tiered-shapes` vs default; DEC-10 untouched); D-2 (one
  Deploy Dev dispatch for WP-04's `--exec` green). Research: `.claude/artifacts/research_cargo_dist_prehost_scan.md`
  (`global-artifacts-jobs` gates `host`; `host-jobs` does not).
  `Next: /hex-execute .claude/artifacts/plan_test_speed_tiers.md`
- **ADR written (hex-architect xhigh, 2026-09-22): `.claude/artifacts/adr_test_speed_tiers.md`
  (Status Proposed) + `.claude/artifacts/system_design_test_tiers.md`, from dossier
  `.agents/discussions/test-suite-speed-tiers.md`.** Option A: test-build placeholder provenance
  (the measured cause of the ~1.5% action-cache hit rate), a lint tier, verb markers + a
  `[security]` escalation list, T2 enforced by a full mark on `--no-ff` WP merges, local-only
  acceptance cache (no remote writer; any writer needs its own ruling), under-declared modules
  tagged `external`, port-down pilot-gated. Research: `.claude/artifacts/research_test_tier_{tooling,patterns,operability}.md`.
  Preference hint for the next `/hex-init`: research axis "operability & cost" mattered most
  (Prometheus/Tempo measurement overturned the dossier's assumptions).
  `Next: /hex-plan high "Tiered, cache-driven verification, per .claude/artifacts/adr_test_speed_tiers.md"`
- **Discussion handed off (hex-discuss, 2026-09-22): `.agents/discussions/test-suite-speed-tiers.md`
  → architect (tier high), on `goat`.** Test architecture for a fast AI loop, with Bazel caching
  as the lever: a lint tier for the 12 structural sweeps; an inner loop of cached `rust_test` +
  lint + smoke; verb-level `SCOPED_ROWS` with a coverage guard; full acceptance at WP merge and
  finalize; a dedicated fan-out port-down to `rust_test` (originals deleted after a mutation
  proof); agent files fixed to name the tiered gate; test builds carry placeholder provenance
  (`build.rs` bakes in describe/SHA/`GITHUB_*`, which invalidates all 181 acceptance targets on
  every commit). cucumber-rs rejected. Findings inlined under the artifact's `## Research`.
  `Next: /hex-architect .agents/discussions/test-suite-speed-tiers.md`
- **Discussion handed off (hex-discuss, 2026-09-21): `.agents/discussions/bazel-full-adoption.md`
  → architect (tier high), on `sion`.** Bazel 9 at all four stages (Rust unit tests per crate,
  casts, website, acceptance), existing bazel-cache.ocx.sh + otel.ocx.sh, no RBE, rules_ocx as
  the sole tool path via worktree/branch/`git_override`. Supersedes the 2026-09-20
  `bazel-adoption-timing` deferral: the narrower fix landed and per-crate test caching is the
  pain cargo cannot address. Research: `.agents/research/bazel_*.md` (six lanes).
  `Next: /hex-architect .agents/discussions/bazel-full-adoption.md` — reopens
  `adr_crate_split_workspace.md` "Bazel parked".
- **Discussion handed off (hex-discuss, 2026-09-20): `.agents/discussions/bazel-adoption-timing.md`
  → plan, applied inline on `evelynn`.** Verdict: Bazel deferred until the narrower fix is
  measured; server half landed (sccache.ocx.sh → Garage, bazel-cache reads closed,
  herwig-systems/server-hetzner1 #1–#3); ocx half = parallel acceptance in verify-deep,
  `CARGO_BUILD_TARGET` so schema-generate reuses the build dir, sccache-action with org
  secrets, the 210 s / 4×30 s unit tests. Research: `.agents/research/research_build_time_recon.md`,
  `research_build_cache_prior_art.md`, `research_graph_tools_middle_ground.md`. Re-measure
  verify-deep median (was 3347 s) before running the bazel-adopt gate.
- **Active plan (evelynn): `.claude/artifacts/plan_crate_split_workspace.md`** — the
  crate split ([ADR](../../.claude/artifacts/adr_crate_split_workspace.md) Accepted
  2026-09-16). **Batch B1 (phase 0 tooling) executed 2026-09-16 by sub-orchestrator
  `exec-b1`: WP-01, WP-03, WP-02, WP-04, WP-06, WP-05, WP-09 merged on `evelynn`; batch-end
  `/hex-review xhigh 3538b755..evelynn` (review-b1) returned **Needs Work** (0 Block / 8 High
  / 28 Warn, cross-model ran) and appended **WP-40**, which sub-orchestrator `exec-b1-fix`
  executed on 2026-09-16: six file-disjoint sub-WPs (rust, scripts, hooks, taskfiles, docs,
  ci) + an L2 aggregate fix pass in two more, every H1–H8 / W1–W28 fixed or recorded, owner
  rulings D1–D11 folded, D12–D18 raised; the cross-model gate was **partial** (Codex usage
  limit; its five leads reproduced and fixed by an opus seat, DX-43). `State: review`; the `B1-review` token is still
  HELD. `Next: /hex-review xhigh 0f54991e..evelynn` (B1 delta); only its Approve releases the
  token and starts B2.** Execution deviations DX-1…DX-42 live in the plan's
  "## Execution deviations" table; § Schedule log carries every merge SHA and gate quote.
  Owner questions open: D12–D18 in § Deferred findings. Cap 3 worktrees (2 under 16 GB free);
  full verifies one at a time; `task verify:scoped --force` escalates to full whenever a root
  manifest, taskfile or `scripts/**` changed, or a crate in `scoped_gate.py`'s
  `TABLE_ESCALATES` (`ocx_test_support`, `ocx`; `ocx_lib`'s row left with the crate at
  WP-37) — ≈ 10 min on this
  host, the scoped path ≈ 2 min.
  - Lessons from WP-40 (2026-09-16): (1) the pre-commit hook reads the Bash command TEXT,
    so a command that merely QUOTES a release-commit example (a heredoc writing docs, a grep
    pattern) is judged as that commit and refused — write such text through a file, never a
    heredoc; (2) a regex that decides whether a gate fires at all must match by SHAPE, never
    by an allow-list of options: `git <any-option> commit` bypassed both commit hooks
    entirely until the L2 seat probed 16 spellings; (3) `.claude/tests/**` ran under no CI
    job — every pin an owner ruling rested on was a check CI never executed; grep `.github/`
    for the task name before believing a structural test guards anything; (4) replacing a
    hand-rolled parser with a real one (tokenizer → `syn`) is only done when a DIFFERENTIAL
    over the live tree reports "old-only: 0" — the first cut silently dropped ~30 reaches on
    a one-token precedence bug; (5) a corpus union hides its members: "> 1 file walked" over
    21 crates passes with 20 of them gone — assert per subtree; (6) a needle list erodes
    silently unless the witness carries every needle; (7) `task claude:tests` run from inside
    `.agents/worktrees/` is half-blind (`test_all_markdown_refs_resolve` skips any path
    containing `worktrees`) — the post-merge run from the main checkout is the real one.
  - Perspective hint for the next `/hex-init`: `perspectives.always.when` should
    be `crates/ocx_{oci,sign,trust,config,store}/**` — the five security-sensitive
    trees, matching the Pointers line above, which WP-38 rewrote to agree with it.
    All five crates exist; `ocx_lib` is gone, so no pre-split glob remains to keep
    beside them.
  - Lessons from B1 (2026-09-16): (1) the L1 seat fires at every leaf join — three
    merges ran before their seat and every one came back "needs work" (a real SSRF
    ratchet bypass, a hook that missed `--message=`/`-F`/`--amend`, a gate that never
    ran `test_hooks.py`); review before merge, not after; (2) a 130-file `use`-line
    rewrite dropped the blank line after the SPDX header in two files and the full gate
    caught it only at hawkeye — audit `-U0 | grep '^-$'` hunks of any sed-shaped WP;
    (3) a guard's per-WP diff range and its release-base range see different things
    (in-series files are status `A` forever over the release base): run both, and name
    the plan-owned structural tests as the only line-check exemption (DX-17); (4) a
    `TMPDIR` under the repo reds `project_path_walk_without_git_or_ceiling_returns_none`
    on any tree — builder scratch lives in `~/.cache/`; (5) the plan's literal
    `OCX_LOG=ocx_cli=debug` could never discriminate — the CLI's target is its package
    name `ocx`; measure the tool before believing the plan about it; (6) `codex:rescue`
    can die on the Codex usage limit mid-review — a skip is not a review, say so in the
    handoff; (7) `task claude:tests` listed one file — the hook and workflow tests were
    guards nobody ran until an L1 seat asked "which gate runs this?" (DX-13, F2).
  - Lessons from the plan run: (1) a cross-model adversary on a *plan* found the
    two defects the three-seat panel missed (an uncompilable two-commit move shape,
    a sealed trait with out-of-crate test impls) — both were code-path facts the
    plan had taken from research without opening the file; (2) `/hex-execute`
    has no batch concept, so a batch boundary must be a non-WP dependency token,
    never a table column; (3) a "scoped" verify default on a migration whose
    every WP touches `.claude/**` or `ocx_lib` is full-cost in practice — count the
    real full runs in a DEC rather than let the label imply savings; (4) a
    `codex:rescue` adversary spawned as a subagent returns its findings in the
    conversation and writes no file — the orchestrator writes the triage artifact.

- **Finalized (goat → `feat/extra-ca-certs`): `.claude/artifacts/plan_extra_ca_certs.md`** — extra CA roots for [ocx#448](https://github.com/ocx-sh/ocx/issues/448); `/hex-finalize` on 2026-09-14 recomposed 20 commits into 5 (tree-equality vs `backup/feat/extra-ca-certs-2dcad4ba`, rebased clean onto `main` @ `2cc0f172`), `task verify` 8027/8027 unit + 3435/3436 acceptance (known push-mount red), PR [ocx-sh/ocx#465](https://github.com/ocx-sh/ocx/pull/465) + Deep Verify dispatched; residual findings filed as #466–#471; installer contract on [ocx-sh/www-setup#23](https://github.com/ocx-sh/www-setup/issues/23). Merge is the owner's. Lessons: (1) a "reaches every client" contract is reviewed by enumerating construction sites, not by opinion — the Codex pass found three the four opus leaf seats missed; (2) recompose on the merge-base first and prove tree equality there, then rebase the short series — 5 commits rebase clean where 20 with merges would not, and `git merge-tree origin/main backup` gives a byte-exact proof of the rebase; (3) a helper used only by a `#[cfg]`-gated test must carry the same gate — local Linux green cannot show the `dead_code` red, and the first non-Linux build a series ever gets is the PR's smoke job (cost: one CI round + a series rewrite); per-commit `cargo check --all-targets` remains the cheap bisectability proof. No active plan on `goat`.
- Review pending: `.claude/artifacts/plan_ci_integration_review_fixes.md` — the
  `/hex-execute` run for #449–#453 finished 2026-09-13 at `sion` @ `df8d33bb`
  (`State: review`, not pushed). Every Block/High from the prior `/hex-review`
  and both cross-model gates is fixed and mutation-proven; three owner
  decisions are listed in its last Schedule entry. `Next: /hex-review` on that
  path. Pointer left beside the toolchain plan below rather than replacing it —
  both live on `sion`.
- Active plan: `.claude/artifacts/plan_toolchain_tree_layout.md` (toolchain tree
  layout — the closed depth-1 set). **Moved out of `.claude/state/plans/` on
  2026-09-08 — that tree is gitignored (`.gitignore:39`) and this plan must ship
  with the branch.** ADR addendum in
  `.claude/artifacts/adr_toolchain_activation.md`; research in
  `research_toolchain_tree_doc_surface.md` and
  `research_toolchain_tree_corrupt_states.md`; plan reviews in
  `review_toolchain_tree_plan_{spec,adversary}.md`. All four owner questions
  answered 2026-09-08: hold 0.6.1, **option B** (`active` + `links/` + `shells/`),
  C-082 carried as WP-0 rather than filed, Windows verification ignored by
  decision. Origin discussion parked at
  `.agents/discussions/shell-multi-activation.md`.
  State: `plan-approved` → executing on `feat/toolchain-tree-layout`.
- **A structural check whose failure message renders the pattern it forbids seeds its own
  corpus (2026-09-06).** C-062's repo-wide `--package` sweep prints the offending command line
  when it fails; that message lands in the verify log, which sat inside its own "repo-wide"
  scan scope. The **first true positive therefore made every later run red**, against its own
  previous output, with no code defect anywhere. Prune every tree the project writes logs and
  scratch to — gitignored ones especially — and prove the prune by **discrimination**, not by a
  green: one byte-identical probe file must red at the repository root and stay green inside
  the pruned tree. A green alone cannot separate "pruned correctly" from "the walker now
  reaches nothing". Sibling trap on the same check: a probe wrapped in backticks does not red,
  because the prose exemption strips backtick spans first — a probe that fails to red means
  the probe was wrong before it means the check is weak.
- **`git log` under the rtk proxy silently drops merge commits (2026-09-05).** `git log --graph`,
  `git log --first-parent` and `git log <range>` all render a history with every merge commit
  removed, so a feature branch integrating work packages reads as if nothing had been merged — and
  a resume that derives state from `git log` concludes the opposite of the truth. `git diff` is
  worse: it returns **empty**, even redirected to a file. Use `git rev-list --parents`,
  `git diff-tree -r --name-status <base> <ref>`, `git show --stat` and `git rev-parse`. Same family
  as the known `grep -c` / `rg` alternation defects; the failure is always a silent negative.
- **A build slot is the only way several agent worktrees share one host (2026-09-05).**
  `<repo>/.tmp/hex/build-slot.sh` is a host-wide `flock` that additionally waits for >= 10 GB free
  before exec'ing, and every `cargo`/`nextest`/`clippy`/`task`/pytest call in every worker goes
  through it. Without it, concurrent work packages OOM-kill the orchestrator, which is how the first
  wave-3 session died. Keep at most **2 build-capable workers** alive; read-only reviewers are free.
  The slot is shared with *other* Claude sessions on the same machine, so multi-minute waits are
  normal and must not be worked around.
- **An orchestrator killed mid-run leaves its workers' commits but loses its own plan edits
  (2026-09-05).** Worker commits then cite `DX-` rows that exist nowhere, and renumbering is
  impossible because the commits cannot be rewritten. Keep the number allocation the commits already
  used, allocate the gap below it, and record the discontinuity as its own row. Corollary: write the
  plan's Status and Schedule-log mutation **per merge**, not per wave — the artifact is the only
  state that survives the session.
- **Discussion handed off (hex-discuss, 2026-09-04): `.agents/discussions/index-claim-command.md`
  → architect** — `ocx package claim` for [ocx#410](https://github.com/ocx-sh/ocx/issues/410) +
  the GitLab git transport of [ocx#411](https://github.com/ocx-sh/ocx/issues/411), owner-ratified.
  Nine decisions: `ocx package claim` beside announce; claim is its own command; owners written
  `login`/`id` only, no `format_version` bump, indexbot gets its own emit-drop ADR; ocx 0.6.1
  ships claim + git transport together; transport = one `GitLabForge`, REST reads / git writes,
  orchestration transport-blind (council 2/3; separate writer rejected); owner detection when
  `--owner` omitted, explicit list replaces, bots refused; credentials: `OCX_ANNOUNCE_TOKEN` =
  API token, `OCX_ANNOUNCE_GIT_TOKEN`/`_USERNAME` override the push, job token picked up under
  `git` only. Research: `.claude/artifacts/research_index_claim_{recon,prior_art,archaeology,council_transport}.md`.
  Fact corrections found: ocx's `forge/gitlab.rs:148-157` and `environment.md:128` overstate the
  job-token block (it reads files/branches/commits/MRs; cannot write); the indexbot owners ADR
  wrongly says the catalog omits `owners` (it renders them with a `github.com` href).
  **Promotion candidate for the next `/hex-init`:** convention "PR/MR author = the credential's
  identity, owners = the explicit list; the two never substitute" — the doc use-case page the
  discussion asked for. **Lane note:** `worker-researcher` has no Write tool (fourth run) — every
  lane returned inline and the orchestrator persisted; brief that role inline-only.
  Next: done 2026-09-05 — design record below.
- **Design complete (hex-architect high, 2026-09-05): `.claude/artifacts/adr_index_claim_command.md`
  (Status Proposed) + `system_design_index_claim_command.md`**, from the dossier
  `.agents/discussions/index-claim-command.md`. Discover: `discover_index_claim_map.md` (claim diff
  7 HOLDS). Research axes security / tooling / operability:
  `research_index_claim_{security,git_tooling,operability}.md`. Panel (spec, quality, security,
  sota — opus) r1: 5 Blocks; spec r2 found the round-1 ADR fixes never reached the design (B1) and
  an exit-86 rule whose second signal had no classifier arm (B2); Codex terra found 2 Blocks the
  panel missed — an unconditional fetch of a branch a first claim does not have, and an `ExitCode`
  variant without its arm in the wildcard-free `ErrorCategory` match — triage in
  `review_adr_index_claim_adversary.md`; reviews `review_adr_index_claim_{spec,spec_r2,spec_r3,quality,security,sota}.md`.
  Three fix rounds → 0 Block / 0 High; open questions zero (live `root.schema.json` read).
  **Lessons:** a fix brief names BOTH artifacts as edit targets and the re-validation checks
  cross-artifact, or the ADR moves while the design stays; a structural fix (moving the version
  gate across a phase boundary) re-opens producer/consumer and step-order checks — re-validate
  after every such fix, not only after the round; `codex-companion task --help` runs a real task.
  Deferred human questions: ADR § "Deferred to handoff".
  Next: `/hex-plan high "Index claim command, forge write transports, and forge-neutral owners, per .claude/artifacts/adr_index_claim_command.md"`.
- **Executing (hex-execute high, 2026-09-05): waves 1-2 of `.claude/artifacts/plan_index_claim_command.md` merged on `feat/index-claim-command`** — WP-1 exit code 86, WP-2 shared announce clock, WP-3 register amendments, WP-4 git-over-HTTP fixture + recording git shim, WP-5 forge write-transport surface. Resume at wave 3 (WP-6 onward); the plan's `## Schedule log` carries each merge SHA and its gate. **Twenty-six execution deviations recorded (DX-1..DX-26)** — the plan is the living design record, so read that table before wave 3: five are rulings the WP-5 edge-case hunt forced, and DX-24's `Redacted` newtype changes a field type WP-12 and WP-13 construct.
  **Lessons.** *A builder that goes silent has usually done the work* — two did (WP-4 stub, WP-5 stub, ~2h each); both trees were nearly complete and the right move was a fresh agent told to assume nothing and verify item by item, not to redo it. *Hand a gate that can discriminate*: I gave WP-4 `pytest --collect-only` as its non-regression check and the builder proved it passes in BOTH polarities — `conftest.py` loads `fake_forge` by path inside the fixture body, while collection does an ordinary import that registers the module in `sys.modules`, which is exactly the state where the fault cannot occur (DX-17). *Measure the tool before believing the plan about it*: three plan-named tests were wrong about git 2.54.0 — it exports `GIT_PUSH_OPTION_COUNT=0` with no push-options negotiation, so the absence assertion was satisfiable only by the fabricating hook it existed to forbid; a chunked push is always two receive-pack POSTs because of the `probe_rpc`; and a `HOME`-side `postBuffer` at or below `LARGE_PACKET_MAX` aborts every protocol-v2 fetch (DX-19). *The undefended-guard shape repeats within one package*: WP-4 round 2 cured it for one knob and round 3 found the same hole in the skip round 2 had just added. *Close a one-way door while it is open* — `Redacted` could only be introduced at WP-5's merge, since WP-5 is the sole writer of `forge/error.rs` while two later packages construct its variants.
  Next: `/hex-execute .claude/artifacts/plan_index_claim_command.md` — wave 3.
- **Waves 5-6 executed (hex-execute high, 2026-09-06): WP-14..WP-17 merged on `feat/index-claim-command`** —
  the `ocx package claim` CLI, the announce `--package` → positional window, and both acceptance suites.
  **Thirty-seven more deviations, DX-51..DX-87.** Two review rounds per panel package; every reviewer opus.
  **The suites are the whole story.** WP-16 ran against an already-implemented command, so it *validated*
  rather than specified — and three of forty-four tests reddened on arrival, including a duplicate-owner
  refusal whose exit code depended on whether the users API happened to be reachable. WP-17 then found two
  defects that make the headline feature dead: `claim --transport git` cannot succeed on **any** input (the
  multi-line request body reaches `render_push_options`, which refuses LF), and C-043's retry does not
  converge under `git`. Every unit suite in waves 3 and 4 was green over all five. The plan's own
  "shippable after wave 5" line was wrong, and only the end-to-end package could say so.
  **Lessons.** *A contract can have no carrier* — C-060 contracted an `author` key that nothing on
  `ClaimOutcome` could hold; three packages and four review rounds missed it because each checked only its
  own half. Ask, per contracted field, *which struct holds this?* *clap renders the **variant** doc comment,
  not the args struct's* — both write commands' long help was dead text, so a round-1 Block was "fixed" into
  text nobody could read; pin help by reading the **built** `Command`. *A required `ArgGroup` renders every
  member regardless of `Arg::hide`*, so a deprecation window's hidden flag reappears in `--help` without
  `override_usage`. *The sweep count was wrong a second time* — 99 across **fourteen** files, not fifteen,
  and one hit was a docstring; reproduce a count, never inherit it. *A stalled worker is a fourth failure
  mode* beside idle, terminated and delivered: 40 minutes with no writes and no reply. The salvage commit is
  what made it recoverable, and the fresh worker's first act — assume nothing, re-verify item by item —
  found the salvaged tests **red on arrival**.
  **Two mechanics worth carrying.** `git merge --squash` re-bases each step against the wrong parent: a
  second squash of a successive commit conflicts, because the commit you just made is not the old one's
  parent. Use `git cherry-pick -n` per commit, group them into the subjects the changelog needs, and prove
  the result by **tree identity** against the branch tip. And the hourly `/tmp` reaper deletes
  `forge::git_workspace`'s shim directory mid-run, producing up to 20 spurious failures — `TMPDIR=~/.cache/<wp>tmp`
  fixes it, while a **repo-local** `TMPDIR` does not (two config-walk tests expect no `ocx.toml` above them).
  Next: WP-18 (docs) and WP-19 (release gates), plus the two production fixes above.
- **A hermetic fixture can red a path the old suite only passed via the public internet
  (proxy-aware SSRF guard, ocx#407/#323).** The stdlib forward-proxy fixture made acceptance A
  fail with exit 75 AFTER the proxied pull had succeeded: `ChainedIndex`'s manifest/blob walks
  fell through from an authoritative miss to the registry-backed `ocx.sh` source, and the twin
  test was green only because that dial reached the public ocx.sh and got a 404. Grep the trace
  for external dials before blaming the fixture; fixing the walk (four sibling walks already
  honoured the authoritative stop) was right, aliasing the fixture would have hidden a contract
  violation. Same run: three reviewers missed that a textual host check must judge the SAME
  normalised form the transport dials (`0x7f000001`, `[::1]` slipped a raw `IpAddr` parse);
  the security perspective caught it, the researcher confirmed the class. Codex's one-shot
  then found the remaining route-blind spot in the resolver hook (destination named like the
  proxy on a direct route) — deferred with a pin-map design, since its fix would reopen #323.
- **The cross-model gate earned its keep a SIXTH time, and this run makes the pattern
  unambiguous.** Eight Claude reviewers on the shell-env branch (spec, test-coverage,
  security, escaping, performance, docs, architect, SOTA) produced 6 Block findings between
  them. Codex then found two more that every one of them had missed, both in the revert
  planner and both requiring the same setup nobody had constructed: *two scopes declaring the
  same key, retiring in the same prompt*. Its stated mechanism was wrong on one of them
  (it blamed restore ordering; the real cause was that the global scope carries no priors at
  all), which is the point — **a cross-model finding is a lead, not a verdict.** Opening
  `reconcile.rs` is what turned a wrong explanation into the right fix. Never treat the
  adversary as the optional last layer, and never relay its diagnosis unopened.
- **File-disjoint decomposition is what made a 26-finding fix round parallelisable, and the
  refusals were the most valuable worker output.** Six workers, one worktree each, disjoint
  file sets. Three of them correctly REFUSED items that crossed their boundary rather than
  reaching into another worker's file: W2 refused R1 with a compile-level argument (every
  carrier shape that could hold a global prior is built by exhaustive struct literal in
  `activate.rs`, so Rust has no partial-literal escape), W3 refused P2 because `Verdict`
  lives in W2's file, and W5 refused three register rows needing Rust tests. Each refusal
  came with the exact patch the next worker should apply. A seventh "cross-file follow-up"
  worker then landed all of them in one commit series. **Brief for the outcome and say a
  reasoned refusal with evidence is an acceptable answer** — otherwise the agent reaches
  across the boundary and the merge conflicts.
- **A worker can die on an API error and report nothing.** W5 (the vacuous-check pass) came
  back as `Agent terminated early due to an API error: 403 Unable to verify organization
  membership` having done zero work. That is a *third* failure mode alongside "idle without
  reporting" and "delivered": **terminated without starting**. The tell is that the result is
  an error string rather than a report or silence. Re-spawn it; do not assume the work
  happened. Its worktree was still at the base commit, which is the cheap check.
- **`task --force` skips `preconditions:` on go-task 3.52** (found by the re-spawned W5 while
  writing a guard against a silent degrade). Any guard that must survive the `--force` this
  repo's own conventions recommend belongs in `cmds:`, never `preconditions:`. A guard that
  evaporates under the flag everyone passes is the same class as an unreachable red state.
- **`task verify` exceeds the 10-minute foreground bash cap on this repo.** Backgrounding it
  from a subagent gets it killed at the turn boundary (the exit-143 lesson above). What works
  from an orchestrator: `nohup … > log 2>&1 &` with `disown`, then a `Monitor` until-loop on
  the PID. Full run here: ~25 min wall (5959 unit + 2664 acceptance), exit 0.
- **A red CI leg can be fixed by a change aimed at something else — check before investigating.**
  `test_a_real_pwsh_prompt_hook_applies_on_cd` was red on the PR before this round. The fix
  round never targeted it; W4's unrelated refactor of `power_shell_registration` (extracting
  `function global:__ocxReconcile` so the wrapper and the prompt share one guarded entry
  point) made it pass. Re-run the failing leg against the new tip before opening an
  investigation into it.

- **A doc comment that states a guarantee the code does not have is this codebase's
  most repeatable defect — caught THREE times in one wave.** A serde paragraph claimed a
  truncated stamp "cannot deserialize into a valid-looking one" while an `Option` field
  read a dropped key as a valid tier; a floor doc claimed "every caller passes one of the
  named constants" when there were zero callers and the file's own test passed arbitrary
  floors; a join doc claimed "no failure mode at all — none of them can contain the
  separator" when Windows `split_paths` reads `"` as a quote and can emit a segment
  containing `;`. Each was written by a careful worker and each survived one review round.
  **Add "read your doc comments back against the code" to every builder brief**, and have
  reviewers treat a doc-asserted invariant as a claim to falsify, not as context.
- **My own instruction caused a Block.** Round 1 told a builder to delete `std::env::join_paths`
  for a manual `PATH_SEPARATOR` join; that tore quoted Windows segments and widened the
  search path in the one function whose subject is narrowing it. The reviewer caught it.
  **Orchestrator instructions are a defect source with no review stage of their own** — when
  a brief tells a worker to replace a stdlib call with a hand-rolled one, that is exactly
  the `quality-core.md` *Don't Own Non-Domain Code* case, and the brief should have to
  justify it the way a diff would.
- **The cross-model gate is not the only adversarial layer that earns its keep — the
  *researcher* perspective did, decisively.** Six reviewers ran on one work package; four
  independently found the same Block (a `Err(_)` arm swallowing a security guard's refusal).
  The researcher was the one that made it un-arguable, with three projects that shipped the
  identical loop: asdf#2166, opencodex#1439 (same PID, `execve` in place, ~31 h CPU over
  3.7 days) and claude-code#47978 (2,171 processes, kernel OOM). It also found that pyenv
  *proposed and rejected* the exact marker-in-file identity check this design adopted, and
  that the directory-list alternative pyenv chose instead is what pyenv#2696 reports as a bug.
  Ecosystem evidence turns "I think this is a defect" into "three projects shipped it".
- **Two concurrent `task rust:verify` runs are not a trustworthy gate on this host.** The
  same single `shell::tests::live_*` test failed in two **file-disjoint** worktrees in one
  concurrent pair, with `pwsh` SIGABRT and a .NET `FileLoadException` naming a *truncated*
  PublicKeyToken — while an earlier concurrent pair of the same two trees was fully green.
  Discriminated, not assumed: passes 3/3 alone on base and in both trees, and both full
  verifies pass **serially** (6826/6826, 6843/6843), as does the merged branch (6890/6890).
  Run verifies one at a time. And note the shape: `task rust:verify --force > log 2>&1; echo $?`
  reports the **`echo`'s** status — the verdict is `Failed to run task` inside the log, the
  same wrapper-exit-code trap the CI lesson already records.
- **A `pub fn` in a library crate that is never called triggers no `dead_code` lint**, so a
  security predicate can ship fully written, fully documented, fully tested — and wired to
  nothing. Two reviewers had to read the call graph to find it. When a contract says
  "X refuses", the review question is *where is X called*, not *does X exist*.

- **Design record (hex-architect, 2026-09-06, tier xhigh, dossier fast path):**
  `.claude/artifacts/adr_crate_split_workspace.md` (Status Proposed) +
  `.claude/artifacts/system_design_crate_workspace.md`. Dissolves `ocx_lib` into
  15 library crates + `ocx_test_support`; Option A (layer in place, then extract,
  weighted 83 vs 66/68/52); `resolve_tiered` stays in `ocx_trust`; exit-code
  classification in `crates/ocx_cli/src/exit/`; `layer_layout` in `ocx_oci`;
  `ocx_sign` extracts ninth after store/index/package. Research this run:
  `research_crate_split_{data_model,performance,operability}.md`; discover
  `discover_crate_split_workspace.md`. Review: 4 seats + Codex terra → 6 Block /
  14 High fixed in one pass, spec re-validation pass. Owner-deferred: `ocx_console`
  below libraries (OQ3), `--cfg ocx_testing` vs feature, deep-suite per-PR trigger
  (OQ1, `merge_group` recommended), `native_transport` escape hatch for grimoire,
  pre-existing opt-in SSRF guard (3/4 builders unguarded; relates to PR #409).
  Follow-ups outside the write surface: ADR index row in `arch-principles.md`;
  CLAUDE.md "Stability tiers" gains the ecosystem tier (named in the ADR).
  Lessons: a single-line grep undercounts multi-line `impl` blocks (37 vs 58) —
  count with a comment-stripped multi-line scan; the Codex job's `status --json`
  nests under `.job.status`, so a flat parse reads "?" and false-stalls.
  Preference hint for the next `/hex-init`: research axes "data model /
  compatibility" and "operability & cost" carried this ADR — worth listing.
- **Discussion hand-off (hex-discuss, 2026-09-06):**
  `.agents/discussions/crate-split-and-verify-tiers.md` → `/hex-architect`
  (tier floor `high`). Decided: dissolve `ocx_lib` into responsibility-derived
  `ocx_*` crates, layer in place first (boundary tests, then extraction);
  satellites link library-shaped ops, CLI for workflows; three stability tiers
  (internal / ecosystem / interface); scoped WP gate ≤ 5 min with full gate at
  `/hex-review` + `/hex-finalize`; Bazel parked. Addendum: grimoire is a
  second lockstep consumer (signing stack incl. trust policy, generic OCI);
  map first, order later, no crate column pre-named. Research index:
  `.agents/research/research_crate_split_{recon,prior_art,archaeology}.md`,
  `research_test_impact_adjacent.md`, `research_crate_decomposition_patterns.md`.
- **Design record (hex-architect, 2026-08-24/25):**
  `.claude/artifacts/adr_shell_env_overhaul.md` — tier high, Status `Proposed`,
  supersedes `adr_live_env_reload.md`. Replaces direnv with a native per-prompt
  reconciler; consent model, `[shell.trust]` whitelist, `__OCX_ENV_STATE` carrier,
  one project key + one per-project state root, `--[no-]hook` symmetric with
  completions. Scope brief `.claude/artifacts/brief_env_overhaul.md` (authoritative);
  Discover map `discover_shell_env_map.md`; research
  `research_{project_state_layout,trust_whitelist_grammar,shell_integration_rollout,private_env_state_vars}.md`;
  panel findings `review_adr_env_{spec,security,quality,sota}.md`.
  Converged in 3 rounds: opus panel (spec/quality/security) + SOTA → 9 Block fixed →
  Codex cross-model gate → 3 net-new (2 Block) fixed.
  **Key reversal from the predecessor**: the phase-1/phase-2 dependency on
  `adr_project_toolchain_links.md` ([#189](https://github.com/ocx-sh/ocx/issues/189))
  is NOT real — the reconciler works against digest paths, links are an optimization
  plus the frozen-process class. The two are independent tracks.
  **Owner gates**: 3 open questions (whitelist key shape, default-on blast radius,
  WinPS 5.1 fidelity) and the flip to `Accepted`.
  Research axes worth keeping as `hex.md › Preferences` hints: per-project state
  layout; trust/whitelist config grammar; generated-shell-integration rollout lag.

- **Active plan (hex-plan high, 2026-08-25):**
  `.claude/artifacts/plan_shell_env_overhaul.md` — the shell env overhaul, State
  `plan-approved`, 19 file-disjoint work packages in six waves. Spine
  `.claude/artifacts/design_spec_shell_env_overhaul.md` (C-001..C-052, S-001..S-045).
  Research `research_{shell_hook_cast_recording,prompt_hook_ci_testing,shell_env_sota_gap_check}.md`.
  Next: `/hex-review .claude/artifacts/plan_shell_env_overhaul.md` — waves 0-5 all
  merged except WP-12b (spike-gated nushell leaf, nothing depends on it).
  **Execution lesson, waves 4-5:** every defect this run found came from running the
  suite somewhere the author could not, never from reading the code. The host has no
  nushell or elvish, so those arms skipped silently and shipped broken; a uid-0
  container ignores the `chmod` three tests staged their premise with, so they were
  green without checking anything; and `coexistence::detect` passed CI for a whole
  wave against a `DIRENV_DIR` spelling real direnv never emits. **Run the fixture
  against the real thing before believing a green.**
  **Plan lives in `.claude/artifacts/`, not `.claude/state/plans/`** — the latter is
  gitignored (`.gitignore:39`) and the plan had to be committable.
  **Review lesson, third dataset:** the panel's single highest-value finding was a
  *false* claim in its own input — the architect worker asserted `project/hook.rs` had
  zero call sites and scheduled it for deletion in the sequential commit gating all 18
  other packages; it is live (`direnv_export.rs:11,94,96,102`). Two other Block-tier
  findings were the same shape: a gate homed at a seam that does not reach the surface
  it defends (`emit_lines` never routes through `Env::apply_entries`), and a security
  property asserted against a shipped mechanism that implements the opposite
  (`ScopeSpec`'s deserializer *drops* unknown keys). **A "zero call sites" or
  "X already does Y" claim is not evidence until you grep excluding the defining file.**
  **Cross-model gate caveat:** `codex:rescue` ran but `--model gpt-5.3-codex` (the
  `terra` mapping) was rejected — *"not supported when using Codex with a ChatGPT
  account"* — so the run used Codex's account-default model. It still found two real
  Blocks the opus panel missed (the `resolve_env*` seam is `tasks/resolve.rs:724`, not
  `composer.rs`; WP-14's DAG was missing two edges). Fix the `terra` model string.

- **Active plan (hex-plan, 2026-08-09):**
  `.claude/state/plans/plan_interpolation_token_grammar.md` — ocx#303, tier high,
  State `plan-approved`. Design record
  `.claude/artifacts/adr_interpolation_token_grammar.md`; research
  `.claude/artifacts/research_interpolation_token_grammar.md`.
  Next: `/hex-execute .claude/state/plans/plan_interpolation_token_grammar.md`.
  Review converged at the round-3 cap: panel (spec/architect/SOTA) → 29 fixes → cross-model
  Codex gate + re-validation → 13 more → final check, one block-tier defect closed.
  **Then the owner reversed the central decision** (2026-08-09, after review): OCX claims
  every `${…}`; no foreign-token pass-through, no reserved-root set, `$${…}` the only escape.
  Reason: pass-through makes OCX's namespace hostage to other tools' vocabularies. Plus D14 —
  refusal scoped to resolve/publish, read-only paths (incl. `pull`/`install`) stay permissive.
  ADR + plan rewritten; #221 amended on GitHub. **D14 and the reversal are unreviewed** — the
  panel ran against the superseded design.
  **`.claude/state/current_plan.md` deliberately NOT repointed** — it still names
  `plan_servable_index_snapshot.md`, which is mid-execution with a dirty worktree at
  `.agents/worktrees/wp8`. Repointing would hijack that run's `/next`. The owner decides
  which plan owns the pointer.
- **A correction pass overshoots into a new false claim — budget a check for the fix, not
  only for the defect.** The 2026-08-31 consent round found that the record's premise about
  git (`safe.directory` "has no glob support") was simply wrong. The fix pass corrected it
  and then asserted *exact* parity at six sites, including a doc comment that feeds the
  published JSON schema. Measured with controls, git's `/*` **refuses** the named directory
  and allows only what is nested under it; ours grants it. A fix-round reviewer caught it —
  the same failure class the round existed to close, one layer later. Two rules fell out:
  when a claim about an external tool is load-bearing, **run the tool** (a five-line probe
  with a positive and a negative control beats any man-page paraphrase), and always spend
  one reviewer on the fix diff itself, never assume a fix round is self-verifying.
- **A codex-companion job's `status` stays `running` after its worker dies.** The
  2026-08-31 consent review's gate job froze at minute 1; its pid was gone while the state
  JSON still read `"status": "running"`, and the agent polling it reported "long reasoning
  turn" for two hours. Liveness is `ps -p <pid>` on the pid in the job's own JSON plus log
  mtime growth — never the status field, and never the shared broker process, which stays
  alive with CPU burn across all jobs and so returns the same answer in every state. Brief
  every Codex-driving agent with this and give it a time budget; a skip with a reason is a
  valid gate result.
- **Note for the next `/hex-init`:** a worker went idle without delivering its report and
  had to be pulled with `SendMessage`; treat "idle" as "not reported" and pull, do not
  assume completion.
- Plan `.claude/state/plans/plan_package_integrations.md` — package
  `integrations`, [ocx-sh/ocx#221](https://github.com/ocx-sh/ocx/issues/221),
  branch `soraka`. **Complete, awaiting merge.** Two review rounds
  (`/swarm-review` max, then `/hex-review` high) plus two fix rounds; Codex `sol`
  returned `approve, no material findings` on the final state. `task verify`
  exit 0 (2011 acceptance). Deferred, filed as
  [ocx-sh/ocx#306](https://github.com/ocx-sh/ocx/issues/306): the patch overlay
  re-emits a shared dependency's *env* entries, because base roots and companions
  are composed by separate `compose` passes that share no `seen` set. The
  `integrations` carrier was fixed surgically (merge-site dedup keyed on the
  **stripped** identifier); unifying the two passes needs its own ADR.
- **The single highest-value review lesson of this feature:** the cross-model
  (Codex) pass found the one defect the entire 8-worker Claude panel missed — a
  duplicate-row regression the fix round had just introduced. Twice now the
  adversary has produced the run's most valuable finding. Never treat it as the
  optional last layer.
- **Corollary, learned the same run:** a reviewer that "confirms" a finding is
  not evidence. Round 2's architect asserted D16 ratified the shipped wire key;
  it ratified the opposite, and only opening the ADR settled it. Round 1's spec
  reviewer marked the merge-site dedup CLOSED while the architect found it broken
  on the advisory-tag axis. Open the file before relaying a claim either way.
- **Dead gate (repo finding, 2026-08-09, unfixed):**
  `.claude/tests/test_ai_config.py::TestPlanStatusBlock` filters candidates to
  git-tracked files, but `.gitignore:39` ignores all of `.claude/state/` — so
  the set is always empty and its three real assertions skip in every worktree,
  reporting "fresh checkout where no plans exist", a cause never observed.
  29 plan files on disk, 0 tracked. Plan Status blocks are effectively
  hand-verified. Out of scope when found; owner notified.
- **Protocol drift:** `meta-ai-config.md` "Plan Status Protocol" enumerates only
  `/swarm-*` values for the `Step` field; hex plans write `/hex-plan → …`.
  Accurate but outside the enumeration `/next` and `/finalize` read.
- **Perspective gap for the next `/hex-init` — reproduced across two rounds:**
  7 review-shaped workers produced 1 full report (researcher) and 1 partial
  (the Codex adversary, which nonetheless found the single most valuable
  defect of the run). reviewer:spec / reviewer:security / architect returned
  nothing in round 1, and re-spawning security + architect in round 2 with
  explicit "your final message IS the deliverable" briefs reproduced the
  failure exactly. Two SendMessage pulls each changed nothing.
  **The pattern:** workers whose output is a *file* deliver through the file
  (all 5 Discover explorers delivered; the ADR author wrote 1287 lines but
  never returned a summary); workers whose only output is a *report* go idle.
  Practical consequence for an orchestrator: verify load-bearing claims
  yourself and treat review-panel delivery as unreliable — in this run the
  necessity of the constitution deviation, the DoS bound, the traceability
  coverage and a CWE-451 finding in C-005 all had to be established directly.
  **FIX CONFIRMED (2026-08-09, execute phase):** give the reviewer a *file* as
  its deliverable — "write findings to `<path>`, append each the moment you
  confirm it, reply with just the path and a one-line verdict". An 8th
  review-shaped worker, same opus model and same task as one that had just
  idled twice, produced a full 16-contract sweep this way: 1 blocking gap and
  4 notes, including five acceptance scenarios that were Unchecked Green
  because the stub threads `Vec::new()` instead of `unimplemented!()`. The
  incremental-append instruction is load-bearing — it makes a partial run still
  worth something. Make this the default shape for every review spawn.
- **Session-outage recovery (2026-08-09, proven):** a killed session loses every
  subagent *process* but keeps their *file edits* — the working tree is the
  durable artifact. Recover by re-running the phase gate and reading what
  actually compiled, never by re-spawning the original brief blind: the stub
  worker died ~90% done, and a blind re-spawn would have redone finished work.
  Checkpoint the instant a gate goes green (`task checkpoint`); this run went
  through two outages with the stub uncommitted.
- **The command proxy fabricates line numbers and collapses grep output.**
  Observed repeatedly this run: `grep -n` returning `"N matches in 1F"` with
  mangled bodies instead of matching lines, and — worse — an `awk` call
  reporting a match at line 2092 of a file that is 1767 lines long. A
  verification step that trusts that output is worse than no verification,
  because it reads as evidence. When a line number or match count is
  load-bearing, cross-check it (`wc -l`, a second tool, or the Read tool,
  which is not proxied) before acting on it.
- **`git grep` is blind to untracked files.** During a stub phase the newest
  file is untracked by definition, so `git grep -c '<symbol>'` reports zero and
  reads as "the worker did nothing". Cost one false accusation and one wasted
  worker spawn here. Use plain `grep -rn` to verify anything a stub phase
  created, and prefer the compiler's own output over a text search.
- Displaced pointer: `.claude/state/current_plan.md` previously named
  `plan_testing_hardening.md` (branch `testing-hardening`, phase 4 complete,
  awaiting PR #287 which was closed unmerged — its fixes are unlanded, so that
  plan is stale-but-unfinished, not done).
- **A silent subagent is not a failed one — and not a finished one either**
  (execution session, 2026-08-11; sharpens the older "treat idle as not
  reported" note above with a second dataset). `wincheck`, `winfix`,
  `pubreview`, `conflict-core` and `conflict-cli` all signalled idle and never
  delivered, including after explicit pulls naming the exact output contract;
  `conflict-docs` delivered a first-rate report unprompted. Same models, same
  session — so delivery is not a model property and cannot be planned around.
- **So audit the refs, not the report.** `winfix` had produced two sound
  commits; reading its diff directly — rather than waiting on a report it never
  sent — is what surfaced a defect *that diff introduced*. `conflict-core`
  resolved its last hunk correctly in the window between one grep and the next.
  The work is observable without the agent's cooperation: `git log`,
  `git status`, marker counts, and a compile. Check those first, pull second.
- **Corollary — the orchestrator's own checks lie the same way.** Three in one
  session returned a passing result while being structurally incapable of
  failing: `${PIPESTATUS[0]}` (zsh uses `$pipestatus`, expands empty),
  `find -newermt '-75 seconds'` (`find` is `bfs` here and rejects it — error to
  stderr, empty stdout read as "settled"), and `for f in $FILES` (zsh does not
  word-split unquoted parameters, so `stat` got one giant filename and the
  sentinel survived as "settled"). Each was caught only because it contradicted
  something directly observed seconds earlier. Make the failure loud — guard
  every watcher with an explicit failure branch rather than inferring success
  from empty output.
- **PARKED (owner decision, 2026-08-11): `/hex-plan high` for ocx-sh/ocx#183**
  (zip layer media type). Discover + Research complete (7 workers); Design never
  started and no plan artifact or ADR exists. Research rejected the issue's stated
  digest-identity rationale, so #183 is parked pending a vendor-checksum metadata
  field rather than reframed as ordinary archive-format support. Do not resume it
  as a media-type feature — a resume starts from the checksum field.
  - Research persisted: `.claude/artifacts/research_zip_layer_oci_precedent.md`,
    `research_zip_artifact_classes.md`, `research_zip_streaming_constraints.md`.
  - Key constraints for whoever resumes: media-type legality is settled and cheap
    (artifact manifests impose no layer-format rule; Sylabs/WASM precedent).
    Streaming zip is disqualified — buffer→verify→parse via central directory
    only, PLUS local-vs-central cross-validation, because CVE-2025-54368 (uv)
    shows one digest can parse two ways. Port `read_entry_capped` from
    ocx-mirror `crates/ocx_python/src/repack.rs` for bomb caps. Zip cannot carry
    the exec bit reliably (protobuf#10301) so a post-extract mode policy is
    mandatory. `adr_layer_layout_config.md` binds: existing tar publishes must
    stay byte-identical.
  - Related landed work: "package test refuses a layer archive it cannot name a
    media type for" made unrecognized layer extensions a hard error in
    `pull_local::stage_layers`; its regression test pins "zip is refused" and
    must be deliberately flipped if the decision is ever revisited.
  - Note for next `/hex-init`: `worker-researcher` has no Write tool, so
    "persist a research artifact" instructions cannot be followed by that role —
    the orchestrator must persist, or the role needs Write.
- **2026-08-19 `/hex-architect` lesson (tier high, `adr_sbom_attestations.md`):** the
  cross-model adversary caught a wire-format directive error the whole panel had
  propagated (rekor `dsse:0.0.1` payloadHash covers the DECODED payload bytes, not the
  PAE) — keep the adversary ON for wire-format ADRs regardless of overlay defaults.
- **SBOM attestations milestone: done.** `.claude/state/plans/plan_sbom_attestations.md`
  finalized; landed as [ocx-sh/ocx#325](https://github.com/ocx-sh/ocx/pull/325)
  (merged 2026-08-20). No active plan.
- 2026-08-20 execution lesson: running a wave-landing or history-writing git operation
  with an inherited working directory landed it on the fixed `soraka` branch (repaired
  same turn; `soraka` moved back to its prior tip). Every git call in an orchestrator
  turn carries `-C <worktree>` or a leading `cd &&` — no exceptions.
- **2026-08-21 `/hex-review high` on `ocx package copy` (branch `evelynn`, 33 files, +3757).**
  Verdict Request Changes. 8-worker panel + Codex gate; 73 severity-tagged findings.
  - **The adversary produced the run's best finding for the THIRD time.** Codex found a
    Block the entire 8-worker Claude panel missed: `fetch_manifest_raw_bytes_addressed`
    (`oci/client.rs:2060-2062`) verifies bytes against the **registry-supplied**
    `Docker-Content-Digest`, never against the **requested** digest, and
    `oci/copy.rs:125` never compares the two. A registry answering `GET /manifests/A`
    with self-consistent bytes for B gets B pushed while `publisher/copy.rs:240` merges
    **A** into the target index — pointing it at a manifest that was never copied. The
    re-run reports `Unchanged` (`:188` compares source-vs-target, both A) over a
    permanently broken index. Never treat the cross-model pass as the optional layer.
  - **A read-site audit table is worth more than a security narrative.** Asking
    reviewer:security for "every read: file:line, addressing, does it decide a write,
    verdict" produced 15 rows and found **4** Invariant #5 violations where Stage 1 had
    found 1 — proving the obvious one-line fix was insufficient. Ask for the enumeration,
    not the opinion.
  - **`worker-researcher` still has no Write tool** (confirmed again). It returned inline;
    the orchestrator persisted `.claude/artifacts/review_r1_sota_package_copy.md`. Its
    outside-in lens found the mount auth-scope gap all seven Claude reviewers missed.
  - **File-first delivery defeats the idle-worker failure.** Every worker was told to write
    its artifact BEFORE returning a summary. Five of eight then went idle without a
    summary — and lost nothing, because the file was on disk. Make this the default brief.
  - **Two workers disagreed on a fix direction and the file settled it.** spec said "amend
    the ADR to match the code" on the canonical-tag phase; architect said "fix the code".
    Reading `client.rs:642-682` showed the merged index is used for one digest lookup the
    copy path already has — architect right. Open the file; do not pick the confident one.
- **2026-08-21/22 `/hex-execute high` on `ocx package copy` (branch `evelynn`).** Applied every
  finding from the round-1 panel, ran a second 5-worker panel plus the Codex `terra` gate, and
  converged. `task verify` exit 0. Plan at `review`; nothing pushed.
  - **The adversary earned its keep a FOURTH time — but differently.** It found nothing blocking
    in the diff and instead surfaced two *pre-existing* defects in a file the diff merely touched:
    an unbounded description-layer ingestion path (`client.rs:1819`, no size pre-check, no stream
    cap, `fs::read` of the whole blob) and a lost-update RMW on the target index (`:531`, no
    conditional PUT). Both verified byte-identical at the base commit. This is exactly the class
    `quality-core.md` calls invisible to diff-scoped review: the file already existed, so nobody
    reviewing the *change* was prompted to question it. Ask the cross-model pass for that class
    explicitly — it is the one thing the Claude panel structurally cannot produce.
  - **"My verification was wrong" happened twice, both times in my own work.** I verified a retry
    *helper* red/green and reported the Block closed; a round-2 reviewer found the warm-primary
    path still emptied the suite green. And I claimed the addressing inversion closed the cascade
    Block "for every caller"; security showed push's blocker *list* was still mirrored. Both times
    the mistake was verifying the mechanism rather than the path that reaches it.
  - **A `#[error(transparent)]` variant forwards `source()` PAST the wrapped error.** A worker's
    `classify()` returned `None` expecting the chain walker to reach the cause; exits 79/80/84 all
    collapsed to 1. Its own test — "the failure kind is reachable by a chain walk" — passed
    throughout, because reachability and classification are different properties. When a test and
    a behaviour disagree, check whether the test asserts the property you actually care about.
  - **`task test:build` does not exist.** It silently no-op'd, I read the following `ls` as proof
    of a rebuild, and re-ran the acceptance suite against a six-minute-stale binary. The real
    command is `cargo build --release -p ocx --features ocx/__testing --locked` then `cp` to
    `test/bin/ocx`. A task runner's unknown target is not an error here.
  - **Two more silently-empty checks, same session as the three already logged above.** A
    tolerance-band grep died on a regex parse error and printed `0 matches`, which reads as a
    clean result; and `find -newermt '-90 minutes'` was rejected by `bfs` with the error on stderr
    and nothing on stdout, so "no Codex artifact was written" was an unfounded conclusion until I
    re-checked with `ls -t`. Both were caught only by deliberately falsifying the check. Make the
    canary explicit: prove the pattern can match before trusting that it did not.
  - **Never edit the tree while `task verify` runs.** I fixed a CLI message mid-gate; the release
    binary the acceptance suite would have used no longer matched HEAD. Killed the run, committed,
    re-ran clean. A gate result against a tree that moved under it is not evidence.
  - **`--theirs` on a test-file conflict silently drops coverage.** Taking one WP's side removed
    another's two assertions. Graft the dropped assertions back and anchor each with an
    asserted-once check rather than trusting the merge.
  - **Idle-worker failure reproduced again, and the file-first fix worked again.** The Codex gate
    went idle three times delivering nothing; the third pull replaced its deliverable with "write
    findings to `<path>`, append each as you confirm it, reply with just the path" and it
    delivered a full report immediately. Make the file the deliverable in the first brief, not the
    third.
- **2026-08-23 `/hex-execute high` on the ocx#272 review set (branch
  `fix/oci-cross-host-upload-auth-272`).** Rebased onto a `main` that had moved 9 commits, then
  applied the `/hex-review` actionable set across three parallel work packages.
  - **`worker-researcher` has no Write tool — THIRD run in a row I briefed it with a file
    deliverable.** It routed around the gap by pasting the whole report into its reply, which
    works but defeats the file-first rule that exists precisely because workers go idle holding
    their output. Stop writing "write your findings to `<path>`" for this role: either brief it
    to reply inline, or spawn `worker-explorer`/`general-purpose` when the deliverable must
    land on disk. The orchestrator persists its output by hand.
  - **The `Cargo.lock` inside `external/rust-oci-client` is gitignored.** A review finding read
    it as a committed second lockfile pinning `reqwest 0.13.2` against the workspace's 0.13.4
    and graded it High. It is a local artefact — but the real consequence survived the regrade:
    every mutation proof ever run *inside* the fork executed against a dependency graph that is
    not what ships. Any fork work package must `cargo update` and assert the resolved version
    before producing evidence. Check whether a lockfile is tracked before grading a lockfile
    finding.
  - **`rtk` rewrites `cargo test` output into its own summary line.** `cargo test | grep
    '^test result'` returns silently empty — a passing-looking gate that never ran. Same class
    as the greps already logged above; the tell was an empty section where a number belonged.
    Run the command unfiltered, or match `rtk`'s own `cargo test: N passed` line.
  - **A mutation that fails to red is a lead, not a weakness — and reading the dependency
    settled it.** A 307-shaped redirect test stayed green when the guard was removed. The cause
    was in `tower-http`'s `follow_redirect`: the body is *moved* into the inner service and
    `reqwest::Body::wrap_stream` is not cloneable, so a 307 is never followed at all. A 303
    zeroes the body before the take and IS followed, so only that shape discriminates. The
    worker root-caused it out of the vendored source rather than concluding the guard was fine.
  - **Ask the research axis "does refusing this break anyone?" before shipping a hard refusal.**
    A behaviour change (any 3xx on an upload `PUT`/`PATCH` is now fatal) looked like a
    compatibility risk worth deferring to review. One sonnet researcher retired it instead, and
    turned up the strongest evidence in the run: `distribution` computes the blob digest
    server-side as bytes stream past, so a registry *structurally cannot* delegate an upload via
    redirect; and oras-go shipped CVE-2026-50151 for this exact function shape. Prior art beat
    a review round.
  - **The cross-model gate earned its keep a FIFTH time, and this was its clearest win yet.** Seven
    Claude reviewers — including a security pass that enumerated all 24 sites where a
    registry-supplied URL reaches a request builder — produced two Blocks between them. Codex
    then found two more that every one of them had missed:
    - **A zero-padded port defeated the system lock.** The round had just fixed the *case* axis
      of the same comparison and nobody asked what else two spellings of one authority could do.
      `OCX_INSECURE_REGISTRIES=registry.corp:05000` misses a lock on `:5000` at every string
      compare, and `url::parse_port` accumulates `port*10+digit`, so the socket lands on 5000
      anyway. Lesson: when you normalise one axis of an identity comparison, enumerate the other
      axes in the same sitting — case, port spelling, default-port elision, trailing dot.
    - **The read-site audit marked the session-opening POST "clean" for the same reason the
      implementer did**: its `Location` is vetted afterwards. Both true, both missing that the
      *request itself* gets relocated before any `Location` exists. Two independent reviewers
      agreeing is not corroboration when they share the premise. Codex proved it pre-existing
      with a `git diff` against the fork base rather than asserting it.
  - **A fix round can staledate its own reviewers.** Two agents fixed the same seam from opposite
    sides in one round: the fork replaced `attempt.stop()` with `attempt.error(...)` while the ocx
    side was documenting `attempt.stop()`. The resulting comment described neither tree. When
    parallel work packages share a seam, re-read the other side before accepting a comment about
    it — and sequence the gitlink bump before, not after, the dependent write-up.
  - **"Not on either branch" does not mean "unlanded".** An auditor reported the hawkeye-7 work
    lost because its commit was on neither branch; `main` had done the same migration
    independently and the effect was live. The decisive test is whether the *effect* is present
    (one `git show` plus one gate line), never commit ancestry alone.
  - **The best worker output of the run refused the task as briefed.** Told to close a finding,
    wp1-fork instead proved the defended branch unreachable and fixed the *comment* that claimed
    otherwise; told to wire `ssrf_guard`, wp2-crates refused with the caller list that would break
    (56 acceptance files on loopback, every air-gapped registry) and the subsystem rule already
    documenting it as design. Brief for the outcome, and say that a reasoned refusal with evidence
    is an acceptable answer — otherwise the agent implements the wrong thing well.

- **Shell-env overhaul, waves 2-3 execution (2026-08-25, branch `feat/shell-env-overhaul`,
  tip `5089ccdf`, `task verify` exit 0 / 2285 acceptance).** WP-10, WP-12a, WP-13, WP-11
  merged. Both plan open questions closed by spike, not reasoning: nushell `hide-env
  --ignore-errors` DOES reach the caller from inside an `env_change.PWD` hook (full
  parity, WP-12b ungated; re-runnable harness `test/manual/nushell-hide-env-spike.sh`),
  and the C-047 dispatcher ceilings are measured per family with an
  `INLINED_LOGIC_FLOOR = 500` assertion so the constants cannot go vacuous by drift.
  - **One Docker registry serves every worktree.** All `.agents/worktrees/*` share compose
    project `test_default` on `localhost:5000`, and `test_patches.py` holds a
    registry-wide, non-UUID-scoped slot — so three concurrent `task verify` runs produced
    three different failures, none of them real. Per-WP gates must be `task rust:verify`;
    exactly one serialized `task verify` on the integration branch, after
    `docker compose down -v` and after pre-building the release binary so the suite run
    cannot be killed mid-module (a kill mid-`test_patches.py` poisons the slot).
  - **`${PIPESTATUS[0]}` expands EMPTY under zsh** (it is `$pipestatus`), so a piped gate
    yields no exit code at all while its output still scrolls past looking green. Never
    pipe `task verify`; redirect to a log and capture `$?` on the next line. Compounding
    trap: a background wrapper that *echoes* the exit code exits 0 itself, so the task
    notification says "completed (exit code 0)" for a red gate — read the logged
    `VERIFY_EXIT=`, never the notification.
  - **Four worker checks were vacuous, and all four were caught only by insisting on a
    red.** A shim-body denylist whose needle was a literal in the file it scanned; a
    `$PWD` grep mis-escaped so it matched identically in both states; a fake-binary
    fixture that emitted no carrier, so the empty-carrier term fired every prompt and the
    named term was never reached; a headline test whose precondition fired before its
    assertion. A worker reporting "red demonstrated" is a claim, not evidence — ask for
    the `grep -c` of the mutated token and the two exit codes.
  - **Rust privacy is module-scoped, so a unit proof-struct proves nothing.** C-028 was
    made compile-time by a `ConsentProof` the consent gate alone can mint — but the first
    attempt used a *unit* struct and the hoist it was meant to forbid still compiled. A
    private `()` field fixed it. The injection caught it; review had not.
  - **Merging a sibling's stale SHA costs a content conflict.** A worker amended after I
    merged; the amended tip then conflicted with my own merge of its predecessor.
    `--theirs` is only defensible because both sides were the same author's drafts, and
    only after proving the result byte-identical to the amended tip
    (`git diff --stat <amended> -- <files>` empty). Re-read a worker's tip immediately
    before merging.
  - **A pty driver named after the stdlib module it imports shadows it.** The nushell
    spike wrote its driver to `${workdir}/pty.py`, so `import pty` found itself,
    `pty.fork` did not exist, no output was produced — and the harness concluded
    "hide-env did not propagate", the opposite of the truth. A harness that can only
    report one outcome is the failure mode; gate on the mutation being present.
  - **The headline use case was broken on 4 of 5 shells and invisible.** The prompt
    guard had no `$PWD` term and the watch set is fixed at shell start, so `cd` alone
    never re-reconciled — nushell was unaffected, which is exactly why nothing caught it.
    `shell/hook.rs` was unowned by any work package. **Assign every file the feature's
    headline scenario traverses**, not only the files the contracts name.

- **`adversarial-review --background` is a no-op in this codex-companion.mjs version** (2026-08-30,
  issue-sweep plan gate). Only `handleTask` checks `options.background`, so the review ran in the
  foreground and was SIGTERM'd by the 2-minute Bash call timeout after ~15 tool calls, before
  emitting a verdict. **Use the Bash tool's own `run_in_background`, never the script's flag.**
  - The tell is a stale job file: `"status":"running"` with a dead pid and a log whose last entry
    is minutes old. That reads as *pending* and is actually *failed* — the same silent-negative
    class as an empty grep. Treat "running + dead pid" as FAIL, and check the pid, not the status
    field.
  - Costs nothing to survive: the adversary's deliverable must be an incrementally-appended FILE.
    A gate killed at 90% leaves a full report on disk instead of nothing. This is the third
    distinct failure mode for review-shaped workers here, alongside "idle without reporting" and
    "terminated on an API error before starting".

- **rustc exempts leading-underscore identifiers from `dead_code`, so an underscore-named probe
  red-proofs nothing** (2026-08-30, WP-7 fork deletion). A worker's first mutation probes were
  named `__wp7_*` and came back green even under `RUSTFLAGS="-D dead_code"` — the canary was
  structurally incapable of firing. Renaming them without the underscore produced the expected
  `function ... is never used` at both lib level and inside `#[cfg(test)] mod test`. Two
  compounding traps in the same check: `cargo build` never compiles `#[cfg(test)]` mods at all,
  and the `rtk` hook replaces compiler output with its own summary, hiding warnings entirely.
  **A zero-warning claim needs `cargo check --all-targets`, run unfiltered, with a probe whose
  name rustc will actually complain about.**
- **A submodule gitlink bump can cross a merge without carrying a foreign change — prove it by
  tree, not by log.** WP-7 branched off `origin/ocx/integration` (`609d3f7`), one merge past the
  pinned `21ded5e`. Commit ancestry says a change rode along; `git rev-parse <sha>^{tree}` on both
  says the trees are identical (`253279661d64c…`), so nothing did. Same lesson shape as
  "not on either branch does not mean unlanded": compare the *effect*, never the graph.

- **Prove the mutation changed the BINARY, not just the file** (2026-08-30, WP-9, issue sweep).
  Gate an acceptance mutation on the release binary's **sha256 changing**, not only on a
  file-level `grep -c`: the grep proves the edit landed on disk, which is *not* the same as
  proving the binary under test contains it.
  - **But do NOT gate on the rebuilt binary hashing back to the baseline** — `vergen-gix` stamps
    build metadata into `ocx`, so a rebuild is not byte-identical and that assertion is luck, not
    a check. It reported three correct proofs as BROKEN when run against a stale baseline. The
    sound gates are: **mutated ≠ baseline, restored ≠ mutated, source byte-identical, green run
    passes.** (WP-9 shipped the wrong form first and corrected it — carry the corrected form.)
  - **Restore on abort.** The same harness aborted mid-mutation and left the mutation on disk when
    a mutation broke the compile under `-D warnings`; only the *next* run's anchor gate
    (`anchor hits=0`) stopped it silently measuring a mutated tree. Write mutations that revert
    behaviour without breaking the build, and restore in a trap.
  Two sibling packages in the same run hit the weaker form: one measured the previous mutation's
  binary after `cargo fmt` silently staledated its needle, another had two mutations come back
  **build-broken rather than red** ("a build break is not a red").
- **Keep a control leg that passes UNDER the mutation.** WP-9's acceptance test carried a
  bare-path leg beside the `file://` leg; the bare leg stayed green while the mutated `file://`
  leg reddened, which is what proves the test discriminates *the spelling* rather than the whole
  rung. A red alone only shows something broke.
- **A guard that cannot red on this host is a platform guard, not a weak test.** WP-9 could not
  red the rooted rows of `anchored_at`: Unix `Path::join` already replaces the base for an
  absolute argument, so the `has_root()` branch is Windows-only. It mutated the branch
  *condition* instead and recorded which rows are load-bearing only on Windows. Correct reading
  of the "a mutation that fails to red means you have not found every guard" corollary.
- **The recurring defect shape of this run: a widening that lands at some doors and not others.**
  Three separate packages found a third or fourth reader of the thing they were fixing —
  `append_to_tags_file` (third unbounded `--tags-file` caller), `read_unverified_referrer`
  (second `.first()` site), `context.rs:1099` + `managed_config/publish.rs:316` (third and fourth
  readers of `OCX_SIGSTORE_TRUSTED_ROOT`). **Every one was found by the implementer, none by
  either review pass.** Brief work packages to enumerate every reader/caller of the seam they
  touch, and grant the scope extension when they do — a contract honoured at two of four doors
  is a worse contract than the one before it.
- **Scenario lists do not get regenerated when a contract is corrected.** S-023 kept specifying
  the probe-only-when-empty optimisation that C-094 had explicitly deleted; a tester working from
  it would have pinned the defect as the spec. Second instance in one plan. The only reader
  forced to reconcile a contract with its scenario is the implementer — so when a review corrects
  a contract, re-read its scenarios in the same edit.
- **Worker idle-notifications race the orchestrator's reply.** Two packages committed "awaiting
  your decision" while the grant was already in their inbox, costing a round trip each. When a
  worker parks on a decision, assume the next message it sends crossed yours and re-state the
  answer in one short paragraph rather than referring back to it.
- **2026-09-04/05 `/hex-architect high` — toolchain activation** (branch `goat`). Artifacts:
  `.claude/artifacts/adr_toolchain_activation.md` (Proposed), `system_design_toolchain_activation.md`
  (Draft), amended `adr_project_toolchain_links.md`, research `research_toolchain_activation_{competitive,shell_session,security}.md`.
  Panel: spec/quality/security (opus) + SOTA (sonnet) → 5 Block, 12 High, ~19 Warn/Suggest, all
  applied; spec re-validation raised 6 precision items, applied without a further pass (disclosed
  in handoff). Cross-model gate: Codex `sol`, plan-artifact scope.
  - **`hex.md › Preferences` names `adversary: codex:rescue`, which is a rescue delegate, not a
    reviewer.** The project's cross-model review skill is `codex-adversary` (`--scope plan-artifact`).
    Propose at the next `/hex-init`: `adversary: codex-adversary`.
  - **Agents addressed the orchestrator as `team-lead` and their full reports never arrived** —
    only the idle summary did. Pull with `SendMessage` asking for a resend to `main` in numbered
    parts; long reviews truncate at ~6k chars in the idle notification.
  - **The federation `Federation lead:` bullet was removed on owner instruction** (mirror-signing
    merged); the lead's plan file still said `landing` — the plan state, not the bullet, was stale.

- **Active plan: `.claude/state/plans/plan_toolchain_activation.md`** (hex-plan, tier high,
  2026-09-05). State `plan-approved`; `Next: /hex-execute` on that path. Pointer also written
  to `.claude/state/current_plan.md` (gitignored, per-worktree). 19 work packages, 6 waves,
  critical path WP-1 → WP-5 → WP-7 → WP-8 → WP-12a → WP-12d. Consumes the Accepted
  `adr_toolchain_activation.md` and `system_design_toolchain_activation.md`; the plan does not
  re-design and records nine divergences (D-V1…D-V9) with reasons.

- **The two structural gates over `.claude/state/plans/` are both vacuous there, and both look
  green.** `TestPlanStatusBlock` enforces only on **git-tracked** plans, and `.claude/state/` is
  gitignored (`.gitignore:39`) — so all three of its tests SKIP, not pass, on every plan in that
  directory. `task claude:lint:links` likewise never scans the directory (675 links checked, zero
  from the plan). A reviewer read the test's assertion and reported "PASS"; the test had not run.
  For any plan artifact, re-run the assertion directly against the file and show it red on a
  mutated copy — the field-removal and the past-line-30 mutations both red cleanly.

- **A same-family review panel found the two defects that would have shipped, and both were in
  the ORCHESTRATOR's own judgment calls, not the ADR's.** hex-plan high on toolchain activation:
  the spec reviewer found an ADR shipping break (`ocx env` emitting link paths) with a commit
  subject but no contract, no test and no file in any work package — `composer.rs` appeared in no
  Expected Files cell, so no package could have implemented it. The architect reviewer refuted the
  orchestrator's own loop-guard deferral with a concrete two-project scenario the guard could not
  see: the exclusion set names only *this* invocation's homes, so two projects each carrying a
  stale trampoline loop A → B → A forever. Both findings were in text the orchestrator wrote after
  the ADR, which is where to look first — the Accepted record had been reviewed four times.
- **Active plan (hex-plan tier `high`, 2026-09-05): `.claude/artifacts/plan_index_claim_command.md`**
  — `ocx package claim`, the `--transport api|git` forge write seam, forge-neutral `owners[]`.
  19 work packages / 7 waves, 75 contracts, 40 scenarios, 7 ADR deviations. `State: plan-approved`,
  shippable after wave 5. Reviews beside it: `review_plan_index_claim_{spec,quality,security,sota,adversary}.md`
  plus `_spec_r2` / `_spec_r3` (each carrying its own applied-fix disposition). Research:
  `research_plan_index_claim_{git_fixture,cli_patterns,gitlab_verification}.md`, `Expires: 2027-03-05`.
  Next: `/hex-execute`.
- **A ratified ADR's premise can be stale against HEAD — check the file it names before planning to
  it.** `adr_index_claim_command.md` put the capturing subprocess helper in
  `utility/child_process.rs`; the spawn primitives had since moved to a private
  `launch/child_process.rs` behind a structural firewall (`SPAWN_TOKENS` / `SPAWN_ALLOWED`, whose own
  module doc says the searches "are not proofs — privacy is what actually holds"). The plan records
  it as a deviation with an allowlist row rather than following the ADR off the cliff. Verify each
  ADR file anchor at plan time; ratified decisions stay ratified, ratified *premises* do not.
- **The defect class that survived three review rounds: a symbol or file the change must touch whose
  owner and whose dependents sit in different work packages.** It reappeared four times in one plan,
  never twice in the same shape — module-declaration hubs no package declared (`command.rs`,
  `api/data.rs`, `forge.rs`, `config.mts`); a widened constructor with one caller three waves
  downstream; a contract's ownership left behind when its implementation moved; a parity test owned
  one wave before the second half it asserts exists. **Same-wave file-set disjointness does not
  catch any of them.** Add a decomposition check that asks, per symbol whose signature changes and
  per file the change requires: *who owns it, and can every dependent still be finished by someone?*
- **The cross-model gate earns its cost on exactly that class, and its own fix is usually too
  heavy.** Codex found the stranded constructor (1 High after the panel had absorbed 8 Blocks / 18
  Highs) and proposed moving a hub file into a later wave plus a new edge. Opening `kind.rs` showed
  `client` is a factory over field-storing constructors, so a two-sentence contract amendment sufficed.
  Take the adversary's *premise*, re-derive the *remedy* from the source.
- **A widened check scope needs the sweep re-run, not just the wording changed.** "89 across six
  acceptance modules" became "repo-wide" without re-sweeping; the real total was 99 across fifteen
  files, four of them **user-facing remediation strings ocx prints at operators** telling them to run
  the deprecated form. Also state the check's *predicate*, not only its scope — a check whose scope is
  contracted and whose predicate is not gets written to whatever makes it green.
- **`~/.cache/claude-hex/` was wiped mid-session**, so a durable-scratch path chosen to dodge the
  hourly `/tmp` wipe still lost the adversary prompt between Write and `cat`. `.tmp/` inside the
  repo (gitignored, `.gitignore:126`) held. Prefer in-repo scratch for anything a background job
  reads back.

### Wave 7 (WP-18, WP-19) — lessons

- **A repo-wide structural check can poison itself.** C-062's `--package` sweep
  prints the offending command line in its failure message; that message lands in
  the verify log, which was inside its "repo-wide" scan scope. First true positive
  → red forever. Exclusion set now prunes `.tmp` (DX-92). Prove such a fix by
  *discrimination* — one byte-identical probe reds at the root, stays green inside
  the pruned tree — never by a bare green.
- **A probe that fails to red may be a bad probe.** The same check exempts prose by
  stripping backtick spans first, so a probe written as `` `ocx package announce
  --package …` `` is correctly ignored. Read the matcher before concluding the check
  is weak.
- **A package's own gate can be red at its merge base.** `task website:build` is
  WP-18's Implement gate and `task verify` does not run it, so three dead relative
  links from 2026-07-26 sat unnoticed. The package that owns the gate is the only
  one that can clear it — accept the out-of-set file and record it (DX-94).
- **"Documented" is half of a doc contract.** `cli-contract.md`'s EXIT-10 needs the
  public row *and* a machine-readable assertion. WP-18 landed the row; all three
  exit-86 acceptance rows asserted only the integer and stderr, with the JSON
  envelope already in hand and never read (DX-93). A gate package that only reports
  would have shipped it red — check both halves at the terminal gate.
- **The cross-model seat found what the panel missed in SIX consecutive batches of the crate split,
  and every time it landed on the INSTRUMENTS rather than the code.** B1 through B5 ran six to eight
  Claude seats apiece — spec, test-coverage, quality, security, performance, docs, architect, SOTA —
  and in each batch the one-shot cross-model pass returned at least one finding none of them reached:
  a `--check` that drops unresolved references and still prints a zero; a scan scope that walks the
  tree the refactor is emptying; a needle list that forbids nothing; an assertion whose expected value
  is also the fallback. **This is not a general-competence result — it is a blind-spot result.** The
  Claude panel reads the diff and asks whether the code is right; it inherits the harness's own frame
  and therefore trusts the harness. A reviewer that did not build the instruments does not inherit that
  trust, and the defect class it keeps finding is exactly the one the panel is structurally worst at:
  a check whose green state is indistinguishable from never having run. **Brief the cross-model seat at
  the instruments explicitly** — guards, ratchets, baselines, gates, allowlists — rather than at the
  diff, and it will out-earn a seventh Claude seat every time.
  Two corollaries the same six batches settled: **a cross-model finding is evidence, not a verdict** —
  B5-28 was reported as a vacuous test and was downgraded after deleting the registration reddened the
  test on a *different* assertion, so verify by property before recording; and **never relay its
  diagnosis unopened**, since its stated mechanism has been wrong while its conclusion was right.
- **Rebase, don't squash, to land a wave on a moved tip.** `git merge --squash`
  re-bases against the branch base and three-way-merges both sides. `git rebase
  --onto <new-tip> <old-base> <branch>` then `merge --ff-only` preserved all three
  wave-6 subjects, which are the changelog entries.

### Cross-model substitute: `copilot` can die producing zero bytes

While Codex is quota-exhausted, `copilot` substitutes. It is argv-bound at 128 KiB, but it can die
**well below that** producing zero bytes on stdout *and* zero on stderr — no banner, no non-zero
message, indistinguishable from a model with nothing to say. Observed dying at 69 KB and succeeding
at 39 KB in the same batch.

**Count invocations against responses.** An uncounted dead seat reports `absent` as `ran`, which is a
finding *against* the report rather than in it. Go per-file and keep each call under ~40 KB.

Strip `CLAUDE.md`, `.claude/`, `AGENTS.md` and `.mcp.json` from the extract, and confirm with
`copilot instruction list` printing `No instruction sources found.` — otherwise the second harness
inherits the first harness's frame, which is the entire reason the seat exists.
