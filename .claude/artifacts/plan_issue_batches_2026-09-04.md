# Plan: open-issue batches — handoff (2026-09-04)

- **Tree**: `origin/main` @ [`34728edc`](https://github.com/ocx-sh/ocx/commit/34728edc); `sion` carries this file only.
- **Source**: two-pass triage of all 82 open issues except #407 (sonnet finder → opus refuter per batch), recorded in [`analysis_issue_triage_2026-09-04.md`](./analysis_issue_triage_2026-09-04.md). Every scope below was also posted to the issue as a comment on 2026-09-04, so the issue and this file agree.
- **Purpose**: restore the planning context without the conversation. §1 sizes, §2 batches, §3 owner decisions, §4 one story per issue with everything noted, §5 incidental findings, §6 method notes.
- **Vocabulary**: Size XS < 1 h · S ≤ half day · M 1–2 days · L multi-day / multi-subsystem · XL needs an ADR first. "Gate" = what must happen before an implementer starts; `none` = startable today. Rough builder-days used for the sums: XS 0.1 · S 0.5 · M 1.5 · L 4 · XL 6 — planning weights, not commitments.

## 1. Size and importance overview

### 1.1 Open issues by size × importance

| Size \ Importance | critical | high | medium | low | total |
|---|---|---|---|---|---|
| XS | 0 | 1 | 1 | 8 | 10 |
| S | 0 | 3 | 11 | 12 | 26 |
| M | 1 | 6 | 11 | 4 | 22 |
| L | 1 | 3 | 7 | 3 | 14 |
| XL | 0 | 0 | 2 | 2 | 4 |
| **total** | 2 | 13 | 32 | 29 | 76 |

### 1.2 Load per batch

| Batch | Theme | Issues | XS/S/M/L/XL | ≈ builder-days | Startable now |
|---|---|---|---|---|---|
| 0 | Urgent, alone | 1 | 0/0/0/1/0 | 4.0 | yes |
| 1a | Fork transport (one fork PR + one pin bump) | 5 | 0/1/4/0/0 | 6.5 | yes |
| 1b | Sign / push / referrers, ocx side | 8 | 3/3/2/0/0 | 4.8 | yes |
| 2 | Small disjoint cleanups | 8 | 1/7/0/0/0 | 3.6 | yes |
| 3 | Package-manager / config features | 6 | 0/1/5/0/0 | 8.0 | yes |
| 4 | SBOM / provenance milestone (#199) + plugin env scrub | 5 | 0/2/2/1/0 | 8.0 | yes |
| 5 | Larger refactors | 4 | 0/1/0/3/0 | 12.5 | yes |
| 6 | Shell / status quick wins | 8 | 1/5/2/0/0 | 5.6 | yes |
| design | Design-first (ADR or design note before code) | 8 | 0/0/1/5/2 | 33.5 | after design |
| decision | Decision-gated (owner answers first) | 17 | 5/5/2/3/2 | 30.0 | no |
| blocked | Blocked upstream or deferred by ADR | 4 | 0/0/3/1/0 | 8.5 | no |
| satellite | Other repositories | 2 | 0/1/1/0/0 | 2.0 | no |

A batch is one `/hex-plan` + `/hex-execute` cycle of file-disjoint work packages in parallel worktrees. Capacity is bounded by review bandwidth and merge overlap, not by count: **6–8 S/M packages or 3–4 L packages per batch** has held so far. Batches 0–6 (45 issues) are startable without an owner decision. Suggested order: 0 → 1a → 1b → 2 → 4 → 3 → 6 → 5 (1a before 1b so the pin bump lands first; 4 before 3 because #199 is a milestone; 5 last because #42 and #313 have the widest blast radius).

## 2. Batches

### Batch 0 — Urgent, alone

One issue, on its own, because it is critical, self-reinforcing and reverses half of an earlier decision (#228). `ocx announce` reads a diverged announce branch as the committed root, so once a package's root changes shape on the index base, the branch is read verbatim forever and the package freezes (34 packages, up to 21 days at filing). Needs a short design note before the patch: the naive fix (reclassify `Diverged` as `Spent`) is only lossless under two of the four tag-selection modes. Touches `forge/api.rs` and both forge backends plus the pytest fake forge, so it should not share a batch with #324/#333 net work.

| Issue | Title | Verdict | Size | Importance | Gate |
|---|---|---|---|---|---|
| [#399](https://github.com/ocx-sh/ocx/issues/399) | announce: a diverged branch is read as the committed root, so the inde | NOT_STARTED | L | critical | design note before patch (reverses half of #228) |

### Batch 1a — Fork transport (one fork PR + one pin bump)

Everything that changes `external/rust-oci-client`. Grouped so the fork gets **one** PR and ocx gets **one** submodule pin bump, instead of five cross-repo landings. All five touch `client.rs` request/response handling (body caps, error struct, chunk PATCH retry, redirect policy, Link header) and will conflict if done in separate worktrees — plan them as one fork work package with sub-tasks, then one ocx-side package that adopts the new `RegistryError { code, .. }` shape (#271), the `truncated` signal (#401) and the bounded reads (#312). Sequencing: #312 and #271 first (they define the new types), #401 and #270 next, #311 last (needs the SSRF seam decision — caller-supplied predicate on `ClientConfig` vs fork-side duplicate; pick the predicate hook). The fork has issues disabled, so any follow-up is filed here prefixed `fork —`.

| Issue | Title | Verdict | Size | Importance | Gate |
|---|---|---|---|---|---|
| [#312](https://github.com/ocx-sh/ocx/issues/312) | Uncapped response reads: manifests, referrers, and every non-2xx error | NOT_STARTED | M | high | fork PR |
| [#271](https://github.com/ocx-sh/ocx/issues/271) | fix(oci): fork — RegistryError should carry the HTTP status alongside  | NOT_STARTED | M | high | fork PR (breaking fork change) |
| [#270](https://github.com/ocx-sh/ocx/issues/270) | feat(oci): fork — retry a transiently failed chunk PATCH in place (re- | NOT_STARTED | M | medium | fork PR |
| [#311](https://github.com/ocx-sh/ocx/issues/311) | fork — a redirect to an IP literal bypasses the SSRF guard (the DNS ho | NOT_STARTED | S | medium | seam: fork predicate hook vs duplicate (impl-level) |
| [#401](https://github.com/ocx-sh/ocx/issues/401) | pull_referrers_native drops the Link header, so callers cannot detect  | NOT_STARTED | M | medium | fork PR chain |

### Batch 1b — Sign / push / referrers, ocx side

OCI-side correctness fixes that do not need the fork. #402 (unsigned cascade tags after a red sign) and #405 (stale local-index pin after push) are the two user-visible high-importance ones and both live in the push pipeline — same reviewer, adjacent files, do them as one work package. #276 changes `registry_error`'s transient predicate (mirrors `transport_policy::is_retryable_transport_error`; the `io::ErrorKind` walk the issue proposes must not ship — documented inert under h2). #324 here means **bug 2 and the `endpoint.rs` fallback only** (zero-root TLS seed, `unwrap_or_else(|_| reqwest::Client::new())` arm); the `ocx_lib::net` consolidation stays decision-gated. #321, #403, #404, #391 are XS/S and file-disjoint. Review at opus: exit-code semantics and a security-adjacent client factory are in scope.

| Issue | Title | Verdict | Size | Importance | Gate |
|---|---|---|---|---|---|
| [#402](https://github.com/ocx-sh/ocx/issues/402) | package push --sign publishes the tag cascade before signing, leaving  | PARTIAL | M | high | reorder (default) vs mark exposure |
| [#405](https://github.com/ocx-sh/ocx/issues/405) | package push leaves a stale tag→digest pin in the local index: content | NOT_STARTED | S | high | none (review against subsystem-oci invariant 2) |
| [#276](https://github.com/ocx-sh/ocx/issues/276) | fix(oci): registry_error classifies mid-upload connection resets as pe | NOT_STARTED | M | high | predicate shape (impl-level) |
| [#403](https://github.com/ocx-sh/ocx/issues/403) | list_signature_candidates truncates to 8 silently, so a client cannot  | NOT_STARTED | S | low | none |
| [#404](https://github.com/ocx-sh/ocx/issues/404) | Export package::tag::SIDECAR_SUFFIXES and sidecar_tag — consumers cann | NOT_STARTED | XS | low | none |
| [#321](https://github.com/ocx-sh/ocx/issues/321) | sign: a Rekor proof with undecodable hex is reported as retryable (exi | NOT_STARTED | XS | low | none |
| [#324](https://github.com/ocx-sh/ocx/issues/324) | net: give the HTTP transport layer one owner (ARCH-16 foundation unit) | PARTIAL | XS (bug 2) / S (+endpoint fallback) / L (consolidation) | high | consolidate ocx_lib::net? |
| [#391](https://github.com/ocx-sh/ocx/issues/391) | copy: referrer count reports PUTs issued, not referrers discoverable a | NOT_STARTED | S | low | none |

### Batch 2 — Small disjoint cleanups

Eight S/XS packages with no shared files and no open design question — the batch to run when reviewer bandwidth is the constraint. Each is one function or one flag against an existing pattern: #46 one arm in `push_multi_layer_manifest`; #53 `buffer_unordered` in gc (not `JoinSet`); #71 one flag on `ocx package install`; #79 move one error variant; #81 one helper with a `debug_assert!`; #322 `EnumDiscriminants`-based row enumeration at three sites; #306 Option 1 dedup at `resolve.rs:922` (reproduce first — the issue says so); #102 `--provenance` sugar mirroring `--sbom`. Sonnet builders are fine here except #322 and #81 (test-hardening whose red state must be demonstrated).

| Issue | Title | Verdict | Size | Importance | Gate |
|---|---|---|---|---|---|
| [#46](https://github.com/ocx-sh/ocx/issues/46) | perf(oci): stream layer archives across the OciTransport boundary — cl | PARTIAL | S | medium | none |
| [#53](https://github.com/ocx-sh/ocx/issues/53) | gc: parallelize delete_objects to reduce ocx clean latency | NOT_STARTED | S | low | none |
| [#71](https://github.com/ocx-sh/ocx/issues/71) | feat(cli): ocx install --reinstall <pkg> for in-place package refresh | NOT_STARTED | S | low | none |
| [#79](https://github.com/ocx-sh/ocx/issues/79) | [entry-points-followup] Move LauncherUnsafeCharacter out of crate-root | NOT_STARTED | S | low | none |
| [#81](https://github.com/ocx-sh/ocx/issues/81) | [entry-points-followup] Add completeness assertion before Vec&lt;Optio | NOT_STARTED | XS | low | none |
| [#322](https://github.com/ocx-sh/ocx/issues/322) | test: nothing forces a new error variant to get an error-slug row | NOT_STARTED | S | low | none |
| [#306](https://github.com/ocx-sh/ocx/issues/306) | Patch companion overlay re-emits a shared dependency's env entries | NOT_STARTED | S (opt 1) / XL (opt 2) | low | Option 1 default |
| [#102](https://github.com/ocx-sh/ocx/issues/102) | SLSA provenance attach: `ocx package push --provenance FILE` | PARTIAL | S | low | none |

### Batch 3 — Package-manager / config features

Six M-sized features in `package_manager`, `project` and `config`. Grouped by subsystem, not by dependency — they are file-disjoint enough to run in parallel: #326 in `project/mutate.rs` + a new `config.toml` write path; #310 in `tasks/update_check.rs` (owns the toolchain drift notice; #42 keeps cache unification); #50 in gc + `state_store` + a `[retention]` table; #283 in `compression.rs` + four exhaustive matches; #333 in `config.rs` + `index_common.rs` (A9/A10 only, binds to `TransportHardening`/`RetryPolicy`); #211 in `dependency_pinning.rs`. Two of them touch the JSON schema (`crates/ocx_schema`): #333 and #283-if-media-type — serialise those two through one worktree or expect a trivial merge. #211 carries implementation-level decisions (match key, `any`-target gate) the builder can default; they are listed in its story.

| Issue | Title | Verdict | Size | Importance | Gate |
|---|---|---|---|---|---|
| [#333](https://github.com/ocx-sh/ocx/issues/333) | feat(net): make index/registry timeouts, retries and fan-out width con | PARTIAL | M | medium | loosely gated on #324 decision |
| [#326](https://github.com/ocx-sh/ocx/issues/326) | CLI write interfaces: ocx env set/unset and ocx config set | NOT_STARTED | M | medium | none (absorbs #328/#329) |
| [#310](https://github.com/ocx-sh/ocx/issues/310) | update notice | NOT_STARTED | S–M | medium | none (owns toolchain drift notice; #42 keeps cache unification) |
| [#50](https://github.com/ocx-sh/ocx/issues/50) | policy-based retention for orphan blobs | NOT_STARTED | M | medium | none |
| [#283](https://github.com/ocx-sh/ocx/issues/283) | ocx_lib: support bzip2 tarballs (.tar.bz2) in CompressionAlgorithm | NOT_STARTED | M | medium | read-side only vs full parity (impl-level) |
| [#211](https://github.com/ocx-sh/ocx/issues/211) | `ocx package create`: pin dependencies from the project `ocx.lock` | NOT_STARTED | M | medium | impl-level questions (match key, any-target gate) |

### Batch 4 — SBOM / provenance milestone (#199) + plugin env scrub

Closes out tracker #199. #104 (OSV scan on install) is the only L and is greenfield — give it opus and its own worktree; exit code is **86**, not 85. #108 and #109 are docs-only pages under `website/src/docs/in-depth/` and `reference/threat-model.md`; #109's body items 1 and 4 are false since ADR Amendment 10 and must be rewritten first. #200 is now unblocked (fallback tag on GHCR) and is workflow-only; fix the `release.yml:237` `output`→`outputs` typo in the same commit. #393 rides along because it is S, high and security-labelled: the `OCX_AUTH_*` prefix scrub ships now; only the `OCX_ANNOUNCE_TOKEN` checkbox waits on the owner.

| Issue | Title | Verdict | Size | Importance | Gate |
|---|---|---|---|---|---|
| [#104](https://github.com/ocx-sh/ocx/issues/104) | OSV vulnerability scan on install (cargo-auditable + OSV.dev) | NOT_STARTED | L | high | none |
| [#108](https://github.com/ocx-sh/ocx/issues/108) | Publisher CI guidance: provenance + SBOM workflows | NOT_STARTED | M | medium | none (docs) |
| [#109](https://github.com/ocx-sh/ocx/issues/109) | Threat model + 2024-2026 incident references | NOT_STARTED | M | medium | body items 1+4 false; fix before writing (docs) |
| [#200](https://github.com/ocx-sh/ocx/issues/200) | Dogfood: attach OCX's own SBOM on release publish | NOT_STARTED | S | medium | signed vs unsigned; index vs platform subject |
| [#393](https://github.com/ocx-sh/ocx/issues/393) | plugins: OCX_AUTH_* and OCX_ANNOUNCE_TOKEN reach ocx-<name> processes  | NOT_STARTED | S | high | OCX_AUTH_ half ships now; OCX_ANNOUNCE_TOKEN half is owner call |

### Batch 5 — Larger refactors

Three L packages plus one S–M. #42 extracts a shared TTL-cache primitive over four hand-rolled caches (grown from two since the issue was rescoped) and swaps the update-check tag scan for a HEAD — high value, wide blast radius, opus. #313 breaks the sign↔verify module cycle; note the issue's own two-function move does not break it (sidecar naming lives on the verify side; seven verify files import sign) and the ADR-accepted `attest` cycle (D-h) survives — the story lists what actually has to move. #214 is mostly integration: rebase PR #238 (17 files, docs and tests ride along) and answer the `--exec-log` follow-up; a named corporate user is waiting. #78 is the `ValidMetadata` typestate-escape removal — mechanical once the accessors exist. Run these as four worktrees; #42 and #313 do not overlap, #214 is CLI+launcher, #78 is `package/metadata`.

| Issue | Title | Verdict | Size | Importance | Gate |
|---|---|---|---|---|---|
| [#42](https://github.com/ocx-sh/ocx/issues/42) | feat: unified freshness/update check strategy with TTL caching | PARTIAL | L | high | none |
| [#313](https://github.com/ocx-sh/ocx/issues/313) | sign↔verify module cycle blocks the planned ocx_lib crate split (ARCH- | NOT_STARTED | L | medium | say whether D-h (attest cycle) reopens |
| [#214](https://github.com/ocx-sh/ocx/issues/214) | Managed configuration option to always log digest when package is invo | NOT_STARTED | L | high | rebase PR #238; answer --exec-log follow-up |
| [#78](https://github.com/ocx-sh/ocx/issues/78) | [entry-points-followup] Drop Deref&lt;Target=Metadata&gt; + From&lt;Va | NOT_STARTED | S–M | low | none |

### Batch 6 — Shell / status quick wins

Post-0.6.0 dogfood findings, mostly S. #396 is the one high-importance item: build-metadata compares as a string so `_10001 < _8001` and a rolling tag silently resolves to an older build — numeric compare with a raw-string tiebreak (needed for `BTreeSet<Version>` in cascade), plus an ADR amendment. #400 `OCX_NO_CONSENT` at the `record_activation_consent` seam (not in `consent::record`, or `ocx shell allow` breaks). #395 is the `plain_annotation` host-leaf string match. #398 is a docs fix first (`content/` layout), then two small script-host decisions. #362 and #360 are the two M items on the per-prompt path — measure the `resolve()` vs syscall split before touching #362, and redesign `_budget_gate`'s abstention rule for #360 with both colours shown. #361 and #365 are test/harness items. C-044's 10 ms per-prompt ceiling must be re-checked after #362/#400 land.

| Issue | Title | Verdict | Size | Importance | Gate |
|---|---|---|---|---|---|
| [#396](https://github.com/ocx-sh/ocx/issues/396) | Version build metadata compares as a string: `_10001` sorts below `_80 | NOT_STARTED | S | high | none |
| [#400](https://github.com/ocx-sh/ocx/issues/400) | `OCX_NO_CONSENT`: let a non-interactive caller run `pull`/`exec` witho | NOT_STARTED | S | medium | none |
| [#395](https://github.com/ocx-sh/ocx/issues/395) | ocx status show pinned digest | NOT_STARTED | XS–S | low | rescoped: host-leaf match in plain_annotation |
| [#398](https://github.com/ocx-sh/ocx/issues/398) | smoke.star: ocx.exists/read_file are scratch-only despite docs, and a  | NOT_STARTED | S | medium | none (docs fix first) |
| [#362](https://github.com/ocx-sh/ocx/issues/362) | The global tier does a store write per tool on every prompt | NOT_STARTED | M | medium | measure resolve() vs syscalls split first |
| [#360](https://github.com/ocx-sh/ocx/issues/360) | C-044's per-prompt budget is nominally met and effectively undecidable | PARTIAL | M | medium | none |
| [#361](https://github.com/ocx-sh/ocx/issues/361) | find_symlink_all resolves packages one at a time and takes no concurre | NOT_STARTED | S | low | none |
| [#365](https://github.com/ocx-sh/ocx/issues/365) | flaky: project_lock::a_symlink_planted_during_the_retry_loop_is_refuse | NOT_STARTED | S | low | diagnose which assertion fires first |

### Batch design — Design-first (ADR or design note before code)

Not batchable as build work yet. Each needs a design artifact first: #25 archive format + verb location; #193 three open axes (output shape, frozen index in image, staleness); #31 a 'still wanted?' check (interpolation now covers its example) then the multi-layer mount conflict rule; #167 a bench extension to 8/16 layers to find the cap; #363 a config grammar for group selection; #357 a 232-row prose audit (L, but tedious rather than hard); #69 an ADR-class identity decision with a stale consumer map and a live patch-opt-out dependency; #144 an ADR amendment for a libc version-floor axis. Run `/hex-plan` per item, not `/hex-execute`.

| Issue | Title | Verdict | Size | Importance | Gate |
|---|---|---|---|---|---|
| [#25](https://github.com/ocx-sh/ocx/issues/25) | feat: portable OCX home export/import for air-gapped environments | NOT_STARTED | L | medium | decide archive format + verb location |
| [#193](https://github.com/ocx-sh/ocx/issues/193) | Dockerfile-friendly environment import for tool bootstrap (no project  | NOT_STARTED | L | medium | 3 design axes open (output shape, frozen index, staleness) |
| [#31](https://github.com/ocx-sh/ocx/issues/31) | feat: mount dependencies into parent content at known subpaths | NOT_STARTED | L | medium→low | confirm still wanted (interpolation covers the example) |
| [#167](https://github.com/ocx-sh/ocx/issues/167) | perf(oci): bound per-layer spawn_blocking concurrency in streaming pul | NOT_STARTED | M | low | bench to 8/16 layers first |
| [#363](https://github.com/ocx-sh/ocx/issues/363) | shell: no way to select which groups/packages load into the per-prompt | NOT_STARTED | L | low | none |
| [#357](https://github.com/ocx-sh/ocx/issues/357) | The EC register asserts behaviour nothing verifies — two rows found fa | PARTIAL | L | medium | none (prose audit of 232 rows) |
| [#69](https://github.com/ocx-sh/ocx/issues/69) | remove identifier requirement for launcher-exec root package | NOT_STARTED | XL | low | ADR: approach A vs B |
| [#144](https://github.com/ocx-sh/ocx/issues/144) | glibc version floor + libc version differentiation (os.version / -vers | NOT_STARTED | XL | low | ADR amendment first |

### Batch decision — Decision-gated (owner answers first)

Nothing to build until the one-line question in §3 is answered. Several are cheap once decided (#178 declaration is a paragraph, the CI diff gate is the work; #316 is a `Semaphore` on `AutoVerify`; #318 is three lines either way; #320 is a sanitizer already written for the text path). #178 is `priority/critical` and blocks every downstream integrator; #224 must be decided before the 42-package mirror fleet publishes because annotations live in the index bytes; #323 is a total failure of `ocx package sign` behind a hostname proxy.

| Issue | Title | Verdict | Size | Importance | Gate |
|---|---|---|---|---|---|
| [#178](https://github.com/ocx-sh/ocx/issues/178) | docs(cli): declare the `--format json` output shapes stable-within-min | NEEDS_DECISION | M | critical | grant pre-1.0 stability carve-out for --format json? |
| [#323](https://github.com/ocx-sh/ocx/issues/323) | Sigstore calls fail under an HTTP proxy configured by hostname | NEEDS_DECISION | M | high | proxy-host exemption vs keep refusal |
| [#189](https://github.com/ocx-sh/ocx/issues/189) | ocx select | NEEDS_DECISION | L | medium | approve adr_project_toolchain_links (Option D)? |
| [#224](https://github.com/ocx-sh/ocx/issues/224) | Recommended OCI annotation set for OCX packages | NEEDS_DECISION | S | medium | upstream-attribution annotation key — before fleet publish |
| [#364](https://github.com/ocx-sh/ocx/issues/364) | shell: should a consent stamp cover the project's [env] table, not jus | NEEDS_DECISION | XL | medium | (a) by-design / (b) adopt fb/envdrift + supersede S-005/S-009 / (c) report-only |
| [#348](https://github.com/ocx-sh/ocx/issues/348) | record_origin mints a namespace-consent marker without wire contact | NEEDS_DECISION | L | medium | persisted-format for pre-existing origin markers (3 options) |
| [#392](https://github.com/ocx-sh/ocx/issues/392) | copy: promoting a cosign-signed package to a referrers-less registry c | NEEDS_DECISION | XS / S / M | medium | lane 1 document / 2 relax gate / 3 write fallback index |
| [#359](https://github.com/ocx-sh/ocx/issues/359) | Consider a hookless shims mode as an alternative to per-prompt reconci | NEEDS_DECISION | XL | medium | close-or-watch; "sub-ms no-op" premise falsified (4.8 ms quiet / 7.6 ms CI) |
| [#316](https://github.com/ocx-sh/ocx/issues/316) | auto-verify: trust-service fan-out inherits the unbounded dependency p | NEEDS_DECISION | S | medium | cap width for trust-service fan-out |
| [#320](https://github.com/ocx-sh/ocx/issues/320) | verify: --format json emits certificate identity fields unsanitized | NEEDS_DECISION | S | medium | exception to verbatim-JSON for certificate fields? |
| [#318](https://github.com/ocx-sh/ocx/issues/318) | cli: the JSON error envelope's reserved 'remediation' field is never p | NEEDS_DECISION | XS–S | low | populate remediation or delete the field |
| [#288](https://github.com/ocx-sh/ocx/issues/288) | feat(index): explicit whole-source sync / override commands | NEEDS_DECISION | XS / L | low | amend index ADR to allow destructive override, or close |
| [#192](https://github.com/ocx-sh/ocx/issues/192) | rules multi-package | NEEDS_DECISION | S–M | medium | (a) list attribute, (b) composing form, or close as satisfied |
| [#397](https://github.com/ocx-sh/ocx/issues/397) | initializing / allowing ocx.toml does not auto-load | NEEDS_DECISION | S | low | reporter's `ocx shell state --format json` + shell needed |
| [#34](https://github.com/ocx-sh/ocx/issues/34) | feat: mise backend plugin for OCX | NEEDS_DECISION | L | low | pursue mise backend or close (mise is now a competitor) |
| [#77](https://github.com/ocx-sh/ocx/issues/77) | [entry-points-followup] Policy: should publishers be allowed to declar | NEEDS_DECISION | XS | low | policy: allow / blocklist / warn-at-select |
| [#80](https://github.com/ocx-sh/ocx/issues/80) | [entry-points-followup] Demote EntrypointError and TemplateResolver fr | NEEDS_DECISION | XS–S | low | keep pub (close) or wrapper error |

### Batch blocked — Blocked upstream or deferred by ADR

#107 waits on sigstore-rs shipping a Rekor v2 client (Rekor v1 has no announced sunset). #262 waits on clap's native dynamic completion (clap-rs/clap#3166) — adopting `unstable-dynamic` early means re-cutting the CLI-surface output twice. #265 is deferred by an owner ADR decision and is a package-metadata wire-format change when it comes. #358: the refuter recommends closing as won't-do — all three rows it wanted unblocked were automated by another route (#353) and the Windows CI job has no Python toolchain, so the harness fix would run nowhere.

| Issue | Title | Verdict | Size | Importance | Gate |
|---|---|---|---|---|---|
| [#107](https://github.com/ocx-sh/ocx/issues/107) | Rekor v2 migration delta (gated on #194 spike) | NOT_STARTED | L/XL | low | blocked upstream: sigstore-rs has no Rekor v2 client |
| [#262](https://github.com/ocx-sh/ocx/issues/262) | Dynamic shell completion for identifiers from the local index | NOT_STARTED | M | low | blocked upstream: clap dynamic completion |
| [#265](https://github.com/ocx-sh/ocx/issues/265) | feat(env): unset directive in project [env] — remove ambient var durin | NOT_STARTED | M | low | deferred by ADR; wire-format change |
| [#358](https://github.com/ocx-sh/ocx/issues/358) | The shell edge-case module can't run on Windows because of our own har | NOT_STARTED | M | low | recommend close as won't-do |

### Batch satellite — Other repositories

Tracked here by convention. #191 is `find_ocx` (CMake) parity with `rules_ocx` v0.4.0 — one file, `ocx.cmake`. #284 is `ocx-sh/ocx-mirror` (`place_binary` has no magic check, so a bare `.gz` binary ships non-executable with exit 0); the ocx-side half is an optional XS 'not a tar archive' message shared with #283.

| Issue | Title | Verdict | Size | Importance | Gate |
|---|---|---|---|---|---|
| [#191](https://github.com/ocx-sh/ocx/issues/191) | support of patches and managed config in rules | PARTIAL | S | medium | satellite repo find_ocx |
| [#284](https://github.com/ocx-sh/ocx/issues/284) | ocx-mirror: bare single-file compressed assets (.gz/.xz/.zst without t | NOT_STARTED | M (mirror) / XS (here) | high | ocx-mirror repo |

### Batch tracker — Tracker

Not itself work. Body updated 2026-09-04 with the current child status.

| Issue | Title | Verdict | Size | Importance | Gate |
|---|---|---|---|---|---|
| [#199](https://github.com/ocx-sh/ocx/issues/199) | Tracking: SBOM, Provenance & Scanning v1 | TRACKER | — | — | body edit only |

### Batch closed — Closed this pass

Recorded for completeness; nothing to do.

| Issue | Title | Verdict | Size | Importance | Gate |
|---|---|---|---|---|---|
| [#314](https://github.com/ocx-sh/ocx/issues/314) | verify: cold-cache referrers probe fetches the listing, discards it, t | IMPLEMENTED | — | — | closed |
| [#319](https://github.com/ocx-sh/ocx/issues/319) | verify: unpinned Rekor public key is re-fetched per candidate instead  | IMPLEMENTED | — | — | closed |
| [#356](https://github.com/ocx-sh/ocx/issues/356) | ocx signatures fallback | IMPLEMENTED | — | — | closed |
| [#328](https://github.com/ocx-sh/ocx/issues/328) | config get/set/unset/describe similar to grimoire | DUPLICATE | — | — | closed → #326 |
| [#329](https://github.com/ocx-sh/ocx/issues/329) | ocx project toolchain edit cli command for env entries | DUPLICATE | — | — | closed → #326 |

## 3. Owner decisions that gate work

One line each. The story in §4 carries the options and their costs.

| Issue | Importance | Question |
|---|---|---|
| [#178](https://github.com/ocx-sh/ocx/issues/178) | critical | Grant a pre-1.0 stability carve-out for `--format json` shapes + sysexits, or tell integrators to pin exact versions? |
| [#323](https://github.com/ocx-sh/ocx/issues/323) | high | Exempt the `HTTPS_PROXY` host from the SSRF private-range refusal, or keep refusing and document it? |
| [#393](https://github.com/ocx-sh/ocx/issues/393) | high (half) | Does `OCX_ANNOUNCE_TOKEN` join the plugin scrub set (needs an ocx-mirror change alongside)? The `OCX_AUTH_*` half ships without this. |
| [#189](https://github.com/ocx-sh/ocx/issues/189) | medium | Accept or reject `adr_project_toolchain_links.md` (Option D)? Gates #189 and #193's staleness question. |
| [#224](https://github.com/ocx-sh/ocx/issues/224) | medium | Which annotation key records *upstream* provenance for mirrored packages (`sh.ocx.*` vs `image.base.name`)? Decide before the 42-package fleet publishes. |
| [#324](https://github.com/ocx-sh/ocx/issues/324) | high | Consolidate `tls.rs` + `ssrf.rs` + one client factory under `ocx_lib::net`? Bug 2 and the `endpoint.rs` fallback ship regardless (batch 1b). |
| [#364](https://github.com/ocx-sh/ocx/issues/364) | medium | Consent stamp over `[env]`: (a) by design, (b) adopt `fb/envdrift` and supersede S-005/S-009, or (c) report-only drift? |
| [#348](https://github.com/ocx-sh/ocx/issues/348) | medium | Persisted format for pre-existing origin markers: prefixed payload, sibling `refs/origins-wire/`, or accept the clause-2 drop? |
| [#392](https://github.com/ocx-sh/ocx/issues/392) | medium | Copy to a referrers-less registry: document only, relax the gate, or write the fallback index (extends Amendment 10 to copy)? |
| [#359](https://github.com/ocx-sh/ocx/issues/359) | medium | Hookless shims mode: close as declined or keep as a watch item? The "no-op path is sub-ms" premise is falsified (4.8 ms quiet / 7.6 ms CI). |
| [#316](https://github.com/ocx-sh/ocx/issues/316) | medium | Cap trust-service fan-out in auto-verify, and at what width? |
| [#320](https://github.com/ocx-sh/ocx/issues/320) | medium | Carve a bidi/zero-width exception to the verbatim-JSON policy for the two certificate fields? |
| [#318](https://github.com/ocx-sh/ocx/issues/318) | low | Populate `remediation` or delete the reserved field? |
| [#288](https://github.com/ocx-sh/ocx/issues/288) | low | Amend the index ADR to allow one explicit destructive override verb, or close? |
| [#192](https://github.com/ocx-sh/ocx/issues/192) | medium | (a) list attribute, (b) composing form, or close as satisfied (repetition already works)? |
| [#397](https://github.com/ocx-sh/ocx/issues/397) | low | You are the reporter: which shell, and what does `ocx shell state --format json` say in that directory? Three documented configurations produce the symptom. |
| [#34](https://github.com/ocx-sh/ocx/issues/34) | low | Still want a mise backend plugin, given mise is now listed as a competitor and coexistence shipped? |
| [#77](https://github.com/ocx-sh/ocx/issues/77) | low | Publisher may declare `bash`/`git` as entrypoint names: allow, blocklist, or warn at select? |
| [#80](https://github.com/ocx-sh/ocx/issues/80) | low | Keep `EntrypointError`/`TemplateResolver` `pub` (close as won't-do) or add a wrapper error? |
| [#358](https://github.com/ocx-sh/ocx/issues/358) | low | Close as won't-do? (refuter recommendation — see story) |

## 4. Stories

One section per issue, grouped by batch. "Evidence" and "Remaining scope" are the opus refuter's verified text (2026-09-04); "Pass-1 → final" records what the sonnet finder said and whether it was overturned. Line numbers are against `34728edc` and will drift — anchor on the symbol names.

### Batch 0 — Urgent, alone

#### [#399](https://github.com/ocx-sh/ocx/issues/399) announce: a diverged branch is read as the committed root, so the index freezes permanently (34 packages, up to 21 days)

- **Labels**: —
- **Verdict**: NOT_STARTED · **Gate**: design note before patch (reverses half of #228)
- **Size**: L — the branch-state machine plus a new `Forge` trait method across two
  backends and the test double, with a regression test on each side of the #228 invariant.
  Fix (1) alone, done safely, is M.
- **Importance**: critical — self-reinforcing, reports as a benign `WARN`, froze 34
  packages for up to 21 days on `ocx-sh/index`, and recurs on any future index-wide root
  migration or root-serialization change.
- **Depends on / blocks**: must not regress
  `test_announce_keeps_accumulating_while_its_pull_request_is_open` (the #228 invariant).
- **Pass-1 → final**: NOT_STARTED → **AGREE**. Every citation re-verified. One correction
  to the issue's own proposed fix, which pass 1 carried forward unexamined and which would
  make a naive implementation lose published tags.

**Evidence**

- `crates/ocx_lib/src/announce.rs:551-557` — `BranchComparison::Diverged` with an open
 pull request classifies as `BranchState::Live`. Pass 1 cited `545-557`; the enclosing
 `match` starts at `545`, the arm at `551`.
- `announce.rs:112-119` — `read_committed_root` is handed
 `branch_repo.as_ref().filter(|_| branch_state.is_live())`, so a diverged-but-open
 branch is read from the fork branch head (`:570-575`), not from the index base.
- `announce.rs:145` — `let unchanged = new_root_bytes == committed_bytes && …` is a byte
 comparison against those branch-head bytes.
- **The freeze mechanism, verified in code rather than taken from the issue.**
 `announce/pipeline.rs:586` — `regenerate` does `let mut new_root = committed.clone();`
 and edits only `tags`, `desc`, `variants`. Every other key, including a pre-migration
 `owners[]`, is carried through verbatim. So the rebuild reproduces the branch's own
 stale shape and `unchanged` fires on it, exactly as reported.
- **No mergeability signal exists.** `git grep "mergeable|mergeStateStatus|CONFLICTING"`
 over `crates/ocx_lib/src`, `crates/ocx_cli/src` and `test/` returns only prose: three
 comments (`announce.rs:185`, `:490`, `forge/api.rs:25`), one manual-test README line,
 and one pytest docstring (`test_announce.py:820`). Nothing is plumbed.
- **The `Diverged → Live` classification is deliberate and defended by a test.**
 `test/tests/test_announce.py:845`
 `test_announce_keeps_accumulating_while_its_pull_request_is_open` is explicit:
 "Resetting the branch whenever it does not fast-forward would silently drop announce
 #1's still-unmerged tag". Its base-divergence is simulated by seeding an **unrelated**
 package's root (`p/other/package.json`, `:872`), so the #399 case — this package's own
 root changing shape underneath an open, permanently-conflicting PR — is untested.
- **Correction to the issue's fix (1).** The issue argues dropping to `Spent` is
 "lossless — the curated tag set is re-derived from the registry, not from the branch".
 That holds for two of the four selections only.
 `announce/pipeline.rs:182-187` + `union_onto_committed` (`:200-208`):
 `FromRegistry` = union(committed, registry listing) and `Replace` = the given set, both
 safe; but `UnionFile` (`--tags-file`) and `Refresh` union onto or copy *committed*,
 where `committed` is the branch head's tag set. Resetting the branch under either of
 those drops any tag the branch accumulated and the caller did not re-supply — which is
 precisely what the #228 accumulate test guards. The manual remedy worked because it
 re-dispatched `announce-from-registry`; the in-tree fix cannot assume that mode.
- No commit since the issue was filed touches announce or forge — the last is
 `6a1f23cd` (2026-08-30). `resolve_branch_state`'s classification is unchanged since
 `60c8b391` closed #228.

**Remaining scope**

- Stop reading the committed root from a `Diverged` branch. Note the naive form —
 reclassify `Diverged` as `Spent` — is only safe under `--tags-from-registry` and
 `--tags`; under `--tags-file` and `--refresh` it drops branch-accumulated tags and
 regresses `test_announce_keeps_accumulating_while_its_pull_request_is_open`. Preserve
 the accumulated tag set while re-basing the root's *shape* on the index base, or gate
 the reset on the tag selection mode.
- Plumb a pull-request mergeability signal through the `Forge` trait
 (`crates/ocx_lib/src/forge/api.rs`) and both backends (`forge/github.rs`,
 `forge/gitlab.rs`), plus the pytest fake forge.
- Refuse `unchanged` and log at ERROR — not WARN — when an open pull request is
 unmergeable, saying the branch needs a reset.
- New acceptance test: this package's own root changes shape on the base while its
 announce branch has an open, now-unmergeable pull request; assert the branch is rebuilt
 rather than read verbatim, and that no tag is lost.
- Because this reverses half of the `60c8b391` / #228 decision, write the design note
 before the patch — the two failure modes are mirror images and the fix has to hold both.

### Batch 1a — Fork transport (one fork PR + one pin bump)

#### [#312](https://github.com/ocx-sh/ocx/issues/312) Uncapped response reads: manifests, referrers, and every non-2xx error body

- **Labels**: —
- **Verdict**: NOT_STARTED · **Gate**: fork PR
- **Size**: M — the code is mechanical at each site, but seven sites plus cross-repo landing (fork PR, own CI, submodule bump, ocx-side adoption). Pass-1's S understated it because it counted two sites.
- **Importance**: high — resource exhaustion reachable from any registry `ocx-mirror` copies from, which is squarely inside its threat model.
- **Depends on / blocks**: none. Batches with #270/#271/#311 on one submodule branch.
- **Pass-1 → final**: NOT_STARTED → **AGREE on the verdict, OVERTURNED on scope.** Pass-1 says "2 of the originally-cited 3 sites remain". There are **seven** uncapped `bytes().await` reads at the current pin, and five of them are inside the issue's own stated scope.

**Evidence**

2182` region) is the already-bounded referrers path. Enclosing function for each of the rest, verified by opening every site:
 | Line | Function | What is read | In the issue's stated scope? |
 |---|---|---|---|
 | 617 | `list_tags` | whole body, success **and** error | yes — "every non-2xx error body" |
 | 1160 | `fetch_manifest_digest` (HEAD arm) | whole body | yes — "manifests" |
 | 1196 | `fetch_manifest_digest` (GET arm) | whole manifest body | yes — "manifests" |
 | 1396 | `_pull_manifest_raw` | whole manifest body | yes — named in the issue's table |
 | 1583 | `pull_blob` non-2xx | error body | yes — named, the sharpest site |
 | 1698 | `pull_blob_stream_partial` non-2xx | error body | yes — the issue names this copy explicitly |
 | 2235 | `catalog` | whole body, success **and** error | yes — "every non-2xx error body" |
- The issue's body says "`pull_blob_stream` routes through `stream_from_response` … and `pull_blob_stream_partial` has its own copy — every variant reads the error body the same uncapped way", and its suggested fix is a helper that "would fix all three error-branch copies at once". Pass-1 counted one of those three. `stream_from_response`'s copy is at `client.rs:2733`.
- All four of `list_tags`, `catalog`, `fetch_manifest_digest` and `pull_manifest_raw` are reached from ocx — `crates/ocx_lib/src/oci/client/native_transport.rs:421`, `:436`, `:442-447`, `:456-460`. None is dead code.
- `list_tags` matters at scale specifically: the index sync fan-out is `INDEX_REFRESH_CONCURRENCY` (8) × `TAG_REFRESH_CONCURRENCY` (64) = 512 in flight (`crates/ocx_cli/src/command/index_common.rs:31`, `crates/ocx_lib/src/oci/index/local_index.rs:34`), and `INDEX_OUTER_CAP`'s doc (`oci/index/ocx_index.rs:117-131`) reasons about exactly that product as a resident-memory bound. The tag-listing leg has no equivalent byte cap.
- Confirmed dead and correctly excluded: `pull_referrers` and `pull_referrers_via_tag_schema` no longer exist (only `pull_referrers_native` at `client.rs:2165`, bounded at `:2182`). That was [#368](https://github.com/ocx-sh/ocx/issues/368), unrelated to this issue.
- `read_body_bounded` (`client.rs:2556-2580`) is a drop-in: it refuses on a declared `Content-Length` over the limit, clamps the allocation hint, and counts actual streamed bytes. Applying it is mechanical.

**Remaining scope**

- Add a shared `read_error_body(response, url)` at a small cap (64 KiB is generous for an OCI error envelope) and use it in all three non-2xx branches: `pull_blob` (`client.rs:1583`), `pull_blob_stream_partial` (`:1698`), `stream_from_response` (`:2733`).
- Bound the manifest reads with `read_body_bounded` at the existing manifest ceiling: `_pull_manifest_raw` (`:1396`) and both arms of `fetch_manifest_digest` (`:1160`, `:1196`).
- Bound `list_tags` (`:617`) and `catalog` (`:2235`). These are JSON listings with no protocol length bound; `list_tags` is the one that multiplies by the 512-wide index fan-out.
- Once the transport bound exists, drop the post-hoc length check in `ocx_lib`'s `fetch_manifest_raw_bytes_capped` — its own doc records that it refuses the body only after buffering it.
- Confirm `ResponseTooLargeError`'s classification is right for these paths: it currently lands on `ClientError::Registry` → exit 69 via the enumerated arm at `native_transport.rs:213-217`.

#### [#271](https://github.com/ocx-sh/ocx/issues/271) fix(oci): fork — RegistryError should carry the HTTP status alongside the OCI envelope

- **Labels**: area/oci, tech-debt
- **Verdict**: NOT_STARTED · **Gate**: fork PR (breaking fork change)
- **Size**: M — the adoption is mechanical, but it is a breaking fork change landing across two repos and two review cycles.
- **Importance**: high — flagged independently by the security reviewer and the Codex gate on [PR #269](https://github.com/ocx-sh/ocx/pull/269); a mis-typed 429 loses automated retry and a mis-typed 403 loses credential re-send.
- **Depends on / blocks**: none. Same function as #276, independent fix; batches with #270/#311/#312.
- **Pass-1 → final**: NOT_STARTED → **AGREE**

**Evidence**

- `OciDistributionError::RegistryError { envelope, url }` — no status field (`external/rust-oci-client/src/errors.rs:73-78`).
- `validate_registry_response` constructs it on any client-error status whose body parses as an `OciEnvelope`, discarding the `s` it has in scope (`client.rs:2696-2702`).
- Consumer side matches envelope codes only (`crates/ocx_lib/src/oci/client/native_transport.rs:177-186`): `Unauthorized`/`Denied` → `Authentication`, `Toomanyrequests` → `RegistryTransient`, everything else → `ClientError::Registry`.
- The loss is exactly what the #266 contract promises: `Authentication` → 80, `RegistryTransient` → 75, `Registry` → 69 (`crates/ocx_lib/src/oci/client/error.rs:348-349`; `crates/ocx_lib/src/cli/exit_code.rs:36,46,58`). So a 403 or 429 carrying any other envelope code lands on 69, "rerunning will not change the outcome".
- Note the status is **not** always lost: a 4xx whose body does *not* parse as an envelope becomes `ServerError { code, .. }`, which is classified status-first (`native_transport.rs:154-175`). The defect is confined to the envelope-parses branch.

**Remaining scope**

- Fork: change to `RegistryError { code: u16, envelope: OciEnvelope, url: String }` and populate `code` in `validate_registry_response`.
- ocx: reshape `registry_error`'s `RegistryError` arm to classify status-first (401/403 → `Authentication`, 429/502/503/504 → `RegistryTransient`) with the envelope code as a refinement, not the only signal.
- Tests: a 403 carrying a `TOOMANYREQUESTS` envelope must exit 80, and a 429 carrying an exotic envelope code must exit 75.

#### [#270](https://github.com/ocx-sh/ocx/issues/270) feat(oci): fork — retry a transiently failed chunk PATCH in place (re-send 3 MiB, not the whole blob)

- **Labels**: area/oci, tech-debt
- **Verdict**: NOT_STARTED · **Gate**: fork PR
- **Size**: M — small logic change, but cross-repo: a fork PR with its own CI, then a submodule pointer bump plus the ocx-side adoption.
- **Importance**: medium — efficiency, not correctness. One failed 3 MiB chunk currently re-sends up to a whole 350 MB layer.
- **Depends on / blocks**: none. Batches with #271, #311 and #312 on one submodule branch.
- **Pass-1 → final**: NOT_STARTED → **AGREE**

**Evidence**

- `push_chunk_body` is the single call site both chunk variants route through, and it issues exactly one `.send().await?` with no retry loop (`external/rust-oci-client/src/client.rs:1951-1959`).
- `push_chunk_streamed` still hands reqwest `reqwest::Body::wrap_stream(body)` (`client.rs:2029-2035`), so the body is one-shot and cannot be replayed.
- No `ClientConfig` retry knob exists; `const MAX` in the fork is only `MAX_REDIRECTS`, `MAX_REFERRERS_INDEX_BYTES`, `MAX_REFERRERS_DESCRIPTORS`, and two tag-listing caps.
- ocx's whole-blob restart is still the mitigation, gated on `RegistryTransient` and `PUSH_RETRY_ATTEMPTS = 2` (`crates/ocx_lib/src/oci/client/native_transport.rs:787`, `:913-925`), and the `ponytail:` upgrade marker is present verbatim at `native_transport.rs:835`.
- The `Range` cross-check the issue wants kept as arbiter is in place (`client.rs:1974-1981`), returning `SpecViolationError` on disagreement.

**Remaining scope**

- Buffer each chunk into `bytes::Bytes` before sending so the `PATCH` body is `Reusable`.
- Retry a transiently failed `PATCH` a bounded number of times against the same upload session, resuming from the session's reported `range_start` / `location`.
- Transient set: request timeout and connect failures, HTTP 429/502/503/504. Never 401/403.
- Expose attempts and backoff as a `ClientConfig` knob so callers own the budget.
- Keep the existing `Range` cross-check as the arbiter of what the registry stored before any resend.
- After the submodule pin moves, demote ocx's whole-blob restart to a fallback and delete the `ponytail:` marker at `native_transport.rs:835`.

#### [#311](https://github.com/ocx-sh/ocx/issues/311) fork — a redirect to an IP literal bypasses the SSRF guard (the DNS hook never runs)

- **Labels**: —
- **Verdict**: NOT_STARTED · **Gate**: seam: fork predicate hook vs duplicate (impl-level)
- **Size**: S — narrow logic change, but cross-repo landing plus a new redirecting fixture.
- **Importance**: medium. Pass-1 said high; downgraded because the issue's own scope section, which I verified, holds: reqwest strips `Authorization` across a host or scheme change and the digest check rejects the body, so this is a **blind** SSRF with no exfiltration channel back to the hostile registry. What remains is that one request reaches an address the guard exists to refuse — real for GET-actionable internal endpoints, and defence-in-depth worth restoring, but not credential theft.
- **Depends on / blocks**: none. Batches with #270/#271/#312 on one submodule branch; related to #324.
- **Pass-1 → final**: NOT_STARTED → **AGREE**

**Evidence**

//github.com/ocx-sh/ocx/issues/407) proves exists on main. It does not close the hole:
- The guard has exactly two layers, both DNS-shaped: `resolve_and_validate` (`crates/ocx_lib/src/oci/ssrf.rs:234`), a pre-flight on the *physical registry host* only, and `GuardedResolver` (`ssrf.rs:262-289`), a `reqwest::dns::Resolve` hook. The module's own header states this (`ssrf.rs:16-24`). A redirect to an IP literal performs no DNS lookup, so neither layer sees it.
- `git grep redirect -- crates/ocx_lib/src/oci/ssrf.rs` → zero hits. Confirmed by reading the whole 467-line file, not only by the grep.
- The fork's `no_scheme_downgrade_policy` (`external/rust-oci-client/src/client.rs:369-388`) inspects **only** `is_scheme_downgrade(attempt.previous().last(), attempt.url())` and `attempt.previous().len() > MAX_REDIRECTS`. Nothing reads the destination host or IP.
- The guard is installed on the physical-fetch client via `ClientBuilder::ssrf_guard` → `config.dns_resolver` (`crates/ocx_lib/src/oci/client/builder.rs:155-159`), called from `crates/ocx_cli/src/app/context.rs:834` and `command/package_announce.rs:262`. So the premise "the guard is a DNS hook" is true of the shipped configuration, not just the fork default.
- Confirmed landed and correctly out of scope: the fork installs the policy itself (`client.rs:321-322`, `:483`), and the upload path uses `Policy::none()` (`:485-490`). That is the half [#336](https://github.com/ocx-sh/ocx/issues/336) closed.

**Remaining scope**

- Fork: make the followed-redirect policy consult an SSRF check on **every hop** — `is_forbidden_ip` / `host_is_trusted` against `attempt.url()`'s host, including the IP-literal case, not only the scheme.
- Decide the seam. The predicate lives in `ocx_lib` (`oci/ssrf.rs`) and the policy lives in the fork, so either the fork gains a caller-supplied per-hop host predicate on `ClientConfig`, or the check is duplicated fork-side. Pick one before writing the diff; this is the only open design point.
- Regression test needs a redirecting fixture: a test registry answering a blob `GET` with `302 Location: https://169.254.169.254/…` must fail the hop rather than issue it. Red-proof it by reverting the policy check.
- Leave the upload path alone — it is already `Policy::none()`.

#### [#401](https://github.com/ocx-sh/ocx/issues/401) pull_referrers_native drops the Link header, so callers cannot detect a truncated referrer listing

- **Labels**: —
- **Verdict**: NOT_STARTED · **Gate**: fork PR chain
- **Size**: M — S in isolation (one return shape in the fork, one field threaded
  one level up), but the cross-repo submodule-bump chain is what makes it real
  work. The reporter's L is defensible; M is my read.
- **Importance**: medium — silent data loss (referrers past page 1 neither
  copied nor rejected, run reports success), not a crash and not a forged
  signature.
- **Depends on / blocks**: none. Same "truncation must be observable" theme as
  #403; a single design decision could cover both.
- **Pass-1 → final**: NOT_STARTED → **AGREE**. Every citation exact, including
  the line number.

**Evidence**

- `external/rust-oci-client/src/client.rs:2165` — `pub async fn
 pull_referrers_native(&self, image, artifact_type) -> Result<Option<OciImageIndex>>`.
 The response is consumed by `read_body_bounded(res, &url, MAX_REFERRERS_INDEX_BYTES)`
 at line 2182 and never inspected for headers. No `truncated` field on the
 return, and `OciImageIndex` has none.
- `git -C external/rust-oci-client grep -n "Link" -- src/` returns **nothing**.
 A case-insensitive search finds only three unrelated `paginat` hits — the
 `catalog` doc at 2203, a test comment at 4374 and a tags comment at 5117 —
 all of which paginate via `n`/`last` query parameters, not `Link`.
- `pull_referrers` (the pre-`e5ed433` fallback-carrying sibling) is gone:
 `pub async fn pull_referrers` matches only `pull_referrers_native`.
- ocx side: `native_transport::list_referrers`
 (`crates/ocx_lib/src/oci/client/native_transport.rs:680-705`) delegates
 straight to `pull_referrers_native` and returns `Vec<oci::Descriptor>` —
 no truncation signal is threaded up, and neither does
 `list_referrers_with_fallback` (`transport.rs:578`) or `ReferrersListing`.

**Remaining scope**

- **Fork** — capture the `Link` response header before `read_body_bounded`
 consumes the response, and widen `pull_referrers_native`'s return so the
 caller sees either the header or a synthesized `truncated: bool`. Full
 pagination is explicitly not required.
- **ocx_lib** — carry the signal on `ReferrersListing` and thread it through
 `list_referrers` / `list_referrers_with_fallback` to the three consumers:
 signature candidate listing, `oci::copy`, and the capability probe.
- **Chain** — fork commit, then the `external/rust-oci-client` submodule pin
 bump in ocx, then any downstream pinning ocx (`ocx-sh/ocx-mirror`). File the
 fork half as an ocx issue prefixed "fork — …", since the fork has issues
 disabled.
- Decide what a consumer does with the signal (refuse the subject, or carry
 truncation into its report) — that is `ocx-mirror`'s call, not this issue's.

### Batch 1b — Sign / push / referrers, ocx side

#### [#402](https://github.com/ocx-sh/ocx/issues/402) package push --sign publishes the tag cascade before signing, leaving unsigned tags after a red run

- **Labels**: —
- **Verdict**: PARTIAL · **Gate**: reorder (default) vs mark exposure
- **Size**: M — the reorder touches `Publisher`'s cascade API and the push
  command's failure handling; the "label the unsigned tags" fallback alone is S.
- **Importance**: high — `latest` and every cascade tag serve unsigned content
  after a run the operator believes failed closed. Tempered by the fact that the
  run does exit non-zero, does log the signing error to stderr, and does list the
  tags: the exposure is real, but it is not silent.
- **Depends on / blocks**: none.
- **Pass-1 → final**: NOT_STARTED → **OVERTURNED.** The issue makes two claims.
  The ordering claim is true. The "the error message is false — it says *no
  package published*" claim is **not true of ocx on main**: that string does not
  exist in the repository, the report always says `status: "pushed"`, the tags
  are named on both output paths, and the no-rollback contract is documented in
  the reference. So the issue's own fallback ask ("at minimum stop claiming
  nothing was published and name the tags left behind") is already satisfied.

**Evidence**

- **Ordering confirmed.** `crates/ocx_cli/src/command/package_push.rs:341-361`
 runs `publisher.push_cascade(...)` / `publisher.push(...)`; inline signing
 starts at 394 (`context.manager().sign_platforms(...)`). Nothing between them
 or after them re-points or deletes a tag; `failures` is a
 `Vec<ExitCode>` that only feeds `sweep_exit_code` at 478.
- **The repro reaches the post-push failure as described.** `resolve_signing`
 (502-541) runs *before* the push and is documented as the place a malformed
 `--key` is caught, but `self.key.reference()` at 517 only parses the
 reference. `env://` resolution is a separate call — `read_key_env`
 (`crates/ocx_lib/src/oci/sign/key_ref.rs:116`) returns
 `KeyEnvError::Unset` — and happens at signing time. So `--key
 env://DOES_NOT_EXIST` passes the pre-push gate, the cascade lands, then
 signing fails. Confirmed.
- **"no package published" is not ocx's.** `git grep` for `no package
 published`, `all platforms failed`, `nothing published` and `not published`
 across `crates/`, `website/` and `test/` returns nothing matching. The
 string is presumably `ocx-mirror`'s own summary derived from the child's
 exit code.
- **The report is honest and names the tags.** `PushReport.status` is the
 constant `"pushed"` (`crates/ocx_cli/src/api/data/push.rs:35-37,221`), pinned
 by `a_failed_signature_does_not_change_the_push_status` (721-731) and
 `a_failed_attestation_does_not_change_the_push_status` (550-558). JSON
 carries `cascade_tags_written` plus a per-platform `signatures[]` array with
 `status: "failed"` and the failure detail. Plain output prints a `Tags`
 column listing the cascade (`Printable` impl, 259-289). The signing failure
 is additionally logged to stderr at `package_push.rs:427`.
- **The behaviour is a documented contract, not an oversight.**
 `website/src/docs/reference/command-line.md:3262`: *"A push that lands and
 then fails to sign is not rolled back: the push report is still emitted,
 with the per-platform signing outcome recorded, and the signing failure
 decides the exit code."* Matching in-code rationale at `package_push.rs:385-388`.
- **What is genuinely missing.** Nothing labels the live tags as *unsigned* —
 `cascade_tags_written` is a neutral list, and a caller branching on the exit
 code alone still cannot distinguish "nothing published" from "published,
 unsigned". No acceptance test covers a post-push signing failure: the twelve
 tests in `test/tests/test_push.py` include `test_an_unimplemented_key_backend_exits_85_from_sign_attest_and_push`,
 which is refused *before* the push, so the ordering is untested.
- **A reorder is feasible.** The signature covers the platform manifests,
 whose digests are final on push; the cascade tags point at the image index.
 So "push platform manifests → sign them → then write the cascade tags" is
 available, and does not require signing something that does not yet exist.
 It does require splitting `Publisher::push_cascade`, which today does both in
 one call.

**Remaining scope**

- Split `Publisher::push_cascade` so the platform-manifest push and the cascade
 tag write are separate steps, and move inline signing between them, so a
 failed sign leaves no rolling tag (`latest`, `3`, `3.7`) pointing at unsigned
 content.
- If the reorder is rejected, keep the current order but mark the exposure
 explicitly: a stderr line naming the tags that are live and unsigned, and a
 JSON field distinguishing them from `cascade_tags_written` (which today also
 covers the fully-successful case).
- Update `website/src/docs/reference/command-line.md:3262` to match whichever
 is chosen — the current sentence documents the behaviour this issue wants
 changed.
- Add the missing acceptance test: `push --sign --key env://<unset>`, assert
 non-zero exit **and** assert which tags exist at the destination afterwards.
- Do **not** pursue tag rollback: OCI has no un-push, and the codebase already
 states that as its reason.

#### [#405](https://github.com/ocx-sh/ocx/issues/405) package push leaves a stale tag→digest pin in the local index: content is not refreshed when a tag moves

- **Labels**: —
- **Verdict**: NOT_STARTED · **Gate**: none (review against subsystem-oci invariant 2)
- **Size**: S — one refresh call at one site plus a failure-policy decision and
  one acceptance test, reusing an existing public entry point. Revised down from
  pass 1's M.
- **Importance**: high — exit 79 (`manifest not found`) on an ordinary "push
  again, then sign by tag" pipeline on any warm developer machine or
  persistent-cache runner. The `--remote` workaround only routes sign's own
  resolve around the stale pin; every other consumer of that tag still gets the
  old digest.
- **Depends on / blocks**: blocks removing `ocx-sh/ocx-mirror`'s `--remote` sign
  workaround. No hard dependency in either direction.
- **Pass-1 → final**: NOT_STARTED → **AGREE on the verdict, OVERTURNED on the
  size (M → S)**, and pass 1's "unverified" caveat is now resolved: the
  `observed`-refresh half of the issue's description **cannot** arise from the
  code, and the fix is smaller than pass 1 assumed because it does not belong in
  `publisher.rs` at all.

**Evidence**

- **Push never touches the local index.** `crates/ocx_lib/src/publisher.rs` and
 `crates/ocx_cli/src/command/package_push.rs` contain no reference to
 `oci::Index` / `LocalIndex`; every `index` hit in both files is the OCI
 *image* index. So nothing invalidates a pre-existing pin for a tag the push
 just moved.
- **A warm tag-addressed resolve short-circuits before any network.**
 `ChainedIndex::fetch_manifest` gates the local read at
 `crates/ocx_lib/src/oci/index/chained_index.rs:1039`
 (`if is_digest_addressed || self.mode != ChainMode::Remote`) — the issue's
 cited line, exact — and returns straight out of a local
 `DispatchResolution::Dispatch` hit at line 1077, with zero registry contact.
 That is why `--remote` works around it and why a cold `OCX_HOME` hides it.
- **The "observed refreshed while content stale" symptom cannot come from a
 single write.** There is exactly one construction of a derived tag entry in
 the whole crate — `DerivedTag { … }` at
 `crates/ocx_lib/src/oci/index/local_index.rs:668` — and exactly one non-test
 caller of `IndexStore::write_root_document` (`local_index.rs:676`). That
 writer, `commit_root_tags` (601-678), always writes `content` and `observed`
 together in the same upsert. `refresh_derived` (367-473), the only path that
 batches them, sources every `content` from `persist_dispatch(source, …)`,
 i.e. from the remote, so it cannot commit a stale digest with a fresh stamp
 either. The reporter's dump most likely shows a stamp written by whatever
 resolve *first* populated the pin, not one bumped by the second push.
- **The load-bearing half stands regardless**, and is what makes the exit-79
 repro real: nothing on main refreshes or invalidates a locally-pinned
 tag→digest entry as a *result of* a push that moves that tag.
- **The fix is consistent with the sacred invariant, and the rule says so.**
 `.claude/rules/subsystem-oci.md:20-27`, invariant 2: *"A pin moves only under
 a command the user invoked naming what to move… The one resolve that does
 move a pin is an explicit `--remote` one — it re-fetches and rewrites the tag
 it touches, the same write an `ocx index update` scoped to that tag would
 make."* `ocx package push <ref>` names the tags it writes, so refreshing
 exactly those is the same class of act, not a silent reaction to remote
 drift. It should still be reviewed against that rule rather than treated as
 a pure bug fix.
- **Why S, not M.** Pass 1 sized it as threading an index handle into
 `crates/ocx_lib/src/publisher.rs`, where none exists. That is not needed:
 `LocalIndex::refresh_tags` is already `pub` (`local_index.rs:193`) and the CLI
 already has the exact orchestration shape in
 `crates/ocx_cli/src/command/index_common.rs:120-146` (`refresh_packages`),
 which picks the right source by jurisdiction and calls `refresh_tags`
 per identifier. The push path has the same `Context`.

**Remaining scope**

- After a successful `ocx package push`, refresh the local index for exactly
 the tags the push wrote — the primary tag plus `outcome.cascade_tags`, the
 same set `pushed_tags` already collects for `--tags-file`
 (`package_push.rs:363-366`). Exclude `__ocx.keep.*`, as that list already
 does.
- Reuse `LocalIndex::refresh_tags` through the `index_common::refresh_packages`
 shape rather than widening `commit_root_tag` (`pub(super)`, deliberately
 single-caller). Host the orchestration in `ocx_lib` and keep the CLI a thin
 wrapper.
- Decide the failure policy: a refresh failure after a successful push must not
 fail the push. It belongs in the same "post-push work is never rolled back"
 band as signing (`package_push.rs:385-388`) — a report row and a stderr line.
- Add an acceptance test with a **warm** `OCX_HOME`: push, move the tag with a
 second push, then resolve that tag and assert the new digest. A cold-home
 test cannot red.
- Confirm against `.claude/rules/subsystem-oci.md` invariant 2 in review, and
 record the outcome — this is the boundary of a ratified invariant even though
 it reads as consistent with it.
- Independently of the fix, `ocx-sh/ocx-mirror` can drop its `--remote` sign
 workaround afterwards.

#### [#276](https://github.com/ocx-sh/ocx/issues/276) fix(oci): registry_error classifies mid-upload connection resets as permanent (69), not transient (75)

- **Labels**: —
- **Verdict**: NOT_STARTED · **Gate**: predicate shape (impl-level)
- **Size**: M — the diff is a few lines; the predicate decision plus an h2 wire test is the real cost. A `h2_wire_tests`-shaped fixture already exists to copy.
- **Importance**: high — [ocx-sh/ocx-mirror#50](https://github.com/ocx-sh/ocx-mirror/issues/50) narrowed its retry to 75-only on the explicit assumption this is fixed here, so today it under-retries the most likely failure for its 180-350 MB pushes.
- **Depends on / blocks**: none. Same function as #271; land together to avoid two rewrites of the same match.
- **Pass-1 → final**: NOT_STARTED → **AGREE on the verdict; remaining scope corrected** (the fix the issue literally suggests is documented in this repo as inert against the transport actually negotiated).

**Evidence**

- The transient predicates are still exactly `request.is_connect()` (`native_transport.rs:133`) and `request.is_timeout()` (`:139`). No `is_body()`, no `is_request()`, and no `io::ErrorKind` source-chain walk anywhere in the file.
- A post-connect reset therefore falls to the enumerated catch-all `RequestError(_)` → `ClientError::Registry` (`native_transport.rs:213-217`) → `ExitCode::Unavailable` = 69 (`error.rs:348`, `exit_code.rs:36`).
- ocx's own upload retry is gated on `matches!(mapped, ClientError::RegistryTransient(_))` (`native_transport.rs:915`), so the misclassification suppresses the internal retry too, exactly as the issue says.
- **New, and pass-1 missed it**: `crates/ocx_lib/src/oci/transport_policy.rs:54-92` already answers this question for the index path, and rules out the issue's suggested implementation. Its doc states the narrow rule — `is_connect() || is_timeout()` plus an `io::ErrorKind` source-chain walk — is "inert against the transport this path actually negotiates": with reqwest's `http2` feature on and no `http1_only()`, ALPN settles on h2, and a non-io-backed h2 failure becomes `Kind::Http2` carrying an `h2::Error` whose `source()` chain terminates with nothing for the walk to find. A `RST_STREAM` (RFC 9113 §8.7, what a CDN emits when it sheds a stream) was classified terminal under that rule and failed a whole run. The adopted answer there is `is_retryable_transport_error(error) = !error.is_builder()` (`transport_policy.rs:90-92`), guarded by `h2_wire_tests`.
- `.claude/artifacts/analysis_issue_triage_2026-08-29.md` records the same open question in the same terms, naming `transport_policy` as the precedent.

**Remaining scope**

- Decide the predicate shape. The `io::ErrorKind::{ConnectionReset, BrokenPipe, UnexpectedEof}` source-chain walk the issue suggests must **not** be shipped as written — `transport_policy.rs:54-72` documents why it cannot fire for an h2 stream reset, so it would be a check whose red state is unreachable for the main case.
- Preferred shape, mirroring `transport_policy::is_retryable_transport_error`: treat every `RequestError` as transient except a builder error. Unlike the index path, the push path is **not** a bodyless idempotent GET, so the safety argument at `transport_policy.rs:77-88` does not transfer — the justification here must rest on the upload session's `Range` cross-check (`external/rust-oci-client/src/client.rs:1974-1981`) plus the existing `PUSH_RETRY_ATTEMPTS` bound.
- Implement it in `registry_error` (`native_transport.rs:133-139`) and add a regression test that an h2 `RST_STREAM` mid-upload maps to `RegistryTransient` / exit 75, red before the change.
- Optional, same site, stated in the issue: `RegistryTransient` currently collapses 429, 5xx, timeout and connect failures with no preserved status or `Retry-After`. `transport_policy::parse_retry_after` (`transport_policy.rs:108`) already exists if that is picked up.

#### [#403](https://github.com/ocx-sh/ocx/issues/403) list_signature_candidates truncates to 8 silently, so a client cannot tell a complete listing from a clipped one

- **Labels**: —
- **Verdict**: NOT_STARTED · startable now
- **Size**: S — a return-shape change plus a visibility change plus one
  `sort_by`, all in two files; XS if only the `pub` constant is done.
- **Importance**: low — the reporter states it is not a security issue; the
  failure mode is `ocx-mirror` redundantly re-signing, which compounds but does
  not forge or drop a signature.
- **Depends on / blocks**: none; shares the "truncation must be observable"
  design question with #401.
- **Pass-1 → final**: NOT_STARTED → **AGREE**. All four citations verified.

**Evidence**

- `list_signature_candidates` (`crates/ocx_lib/src/oci/verify/candidates.rs:173-177`)
 returns `Result<Vec<SignerCandidate>, ClientError>`. No truncation on the
 return type; it is `pub use`-exported at `crates/ocx_lib/src/oci/verify.rs:66`.
- Truncation is reported only via `tracing::debug!` at `candidates.rs:184-189`,
 and the doc at 156-161 states the intent explicitly ("it is logged at debug
 rather than being silent").
- `MAX_SIGNATURE_CANDIDATES` is `pub(super) const … = 8` at
 `crates/ocx_lib/src/oci/verify/pipeline.rs:91` — unreachable from outside the
 crate, so a consumer must hardcode `8`.
- `.take(MAX_SIGNATURE_CANDIDATES)` at `candidates.rs:192` runs over the
 registry's listing order with no prior sort. **New finding pass 1 missed:**
 the determinism fix already exists in-crate as `order_candidates`
 (`pipeline.rs:3274`, `candidates.sort_by(|a, b| a.digest.cmp(&b.digest))`
 then a stable demotion sort), but its only caller is the verify pipeline at
 `pipeline.rs:1572`. It is never applied to `list_signature_candidates`, so
 option 3 is a one-call reuse rather than new work.
- `more_signature_referrers_than_the_ceiling_list_only_the_ceiling`
 (`crates/ocx_lib/src/oci/verify/candidates/tests.rs:596-611`) pins the cap
 but asserts nothing about which eight, which is the gap.

**Remaining scope**

- Return whether the listing was truncated — a `truncated: bool` alongside the
 vector, or a small result struct. Preferred by the reporter.
- Make `MAX_SIGNATURE_CANDIDATES` `pub` so a consumer can compare
 `candidates.len()` against it without duplicating `8`.
- Sort by digest before `.take()` so repeated passes see a stable window —
 reuse `order_candidates`' first sort rather than writing a second one.
- If the first option is taken, check whether the same signal should come from
 #401's fork-side truncation, so a consumer sees one notion of "clipped".

#### [#404](https://github.com/ocx-sh/ocx/issues/404) Export package::tag::SIDECAR_SUFFIXES and sidecar_tag — consumers cannot enumerate the sidecar tags they must carry

- **Labels**: —
- **Verdict**: NOT_STARTED · startable now
- **Size**: XS — a visibility change in one file, no behaviour change.
- **Importance**: low — the reporter calls it not urgent and partly
  self-mitigating; the list and its classifier sit six lines apart. The latent
  hazard is real but requires someone to add a fourth suffix.
- **Depends on / blocks**: none.
- **Pass-1 → final**: NOT_STARTED → **AGREE**. All four line citations are exact.

**Evidence**

- `crates/ocx_lib/src/package/tag.rs:205-207` — `SIG_SIDECAR_SUFFIX`,
 `ATT_SIDECAR_SUFFIX`, `SBOM_SIDECAR_SUFFIX` all `pub(crate) const`.
- `tag.rs:220` — `pub(crate) const SIDECAR_SUFFIXES: [&str; 3]`.
- `tag.rs:230` — `pub(crate) fn sidecar_tag(subject, suffix)`.
- `tag.rs:247` — `pub fn sbom_sidecar_tag` is the only public one of the four,
 matching the issue's "covers one of the three".
- No alternative enumerator was built under another name: `git grep
 sidecar_tags_for` across `crates/` returns nothing.
- The nearest sibling work landed **after** the issue and did not include it:
 `9b4b991b` "feat(oci): expose the transport and candidate APIs signing needs"
 (2026-09-03 10:57) exported OCX-C-1 `native_transport`, OCX-C-2
 `list_signature_candidates` and OCX-C-5's push endpoint flags. The issue was
 filed 2026-09-03 03:21, and `tag.rs` has not changed since 2026-08-30.

**Remaining scope**

- Widen `SIDECAR_SUFFIXES` and `sidecar_tag` from `pub(crate)` to `pub`, or add
 an enumerator such as `Tag::sidecar_tags_for(subject) -> impl Iterator<Item = String>`
 beside the existing `sbom_sidecar_tag`.
- Keep the "bare suffix list, deliberately not a fourth `SidecarKind` variant"
 rationale at `tag.rs:213-219` intact — widening visibility must not turn the
 list into a claim that a reader exists for every suffix.
- Follow the `9b4b991b` precedent for the export shape, so this joins the same
 consumer-API family rather than inventing a new one.
- Once exported, `ocx-sh/ocx-mirror` deletes its own copy at
 `src/pipeline/registry_copy.rs:171`.

#### [#321](https://github.com/ocx-sh/ocx/issues/321) sign: a Rekor proof with undecodable hex is reported as retryable (exit 83)

- **Labels**: —
- **Verdict**: NOT_STARTED · startable now
- **Size**: XS — two files, one new or widened arm, one test flip plus one added test.
- **Importance**: low — misleads the operator's remediation ("wait" instead of "report a bug"); no security or data-integrity consequence, and the sign still fails closed either way.
- **Depends on / blocks**: none.
- **Pass-1 → final**: NEEDS_DECISION → **OVERTURNED**: the codebase already states the deciding rule and already applies it to the neighbouring case, so this is a classification bug with an obvious fix, not an open policy question.

**Evidence**

- `crates/ocx_lib/src/oci/sign/error.rs:62-65` — `SignErrorKind`'s own doc states the rule: "Each variant is justified by a distinct user-facing remediation AND a distinct exit code... Variants that would map to identical remediation + exit code are merged." Retrying an undecodable proof returns the same bytes forever, so the remediation is provably not "retry".
- `crates/ocx_lib/src/oci/sign/error.rs:97-102` — `RekorSetMalformed` already exists for exactly this class and is already classified the other way: "Distinct from `TransparencyLogUnavailable` because the remediation is 'file a bug,' not 'retry.' Exit 65 (`DataError`)." Confirmed in the mapping at `error.rs:414` and asserted at `error.rs:535`. The precedent is not hypothetical; it is one variant away in the same enum.
- `crates/ocx_lib/src/oci/sign/bundle.rs:107-118` — `proto_inclusion_proof` returns `None` on any hex-decode failure of `root_hash` or `hashes`, indistinguishable from "no proof at all".
- `crates/ocx_lib/src/oci/sign/bundle.rs:155-160` — `assemble` maps that `None` to `SignErrorKind::TransparencyLogUnavailable` with the comment "Exit 83 — retrying may help" unchanged.
- Neither branch of the issue has been taken: there is no new arm, and the variant's doc at `error.rs:91-95` still reads only "Rekor unavailable at time of signing... Remediation: retry later" — it does not state that the undecodable-hex collapse is intentional. So "document the collapse" is also unimplemented.
- `build_refuses_a_proof_whose_hex_fields_are_malformed` (`crates/ocx_lib/src/oci/sign/bundle.rs:510-528`) still asserts `TransparencyLogUnavailable`, exactly as the issue predicted it would until this moves.
- The issue's stated objection to splitting — "one code, one remedy, no new exit number" — does not survive contact with the code: exit 65 already exists and is already the answer for malformed Rekor data, so splitting costs no new exit number.

**Remaining scope**

- Distinguish "the log returned no inclusion proof" from "the log returned a proof whose hex does not decode" in `proto_inclusion_proof` / `assemble` (`crates/ocx_lib/src/oci/sign/bundle.rs`). Returning `Result` rather than `Option` is the natural shape.
- Classify the undecodable case as a data error at exit 65, alongside `RekorSetMalformed`. Either widen that variant to cover a malformed proof and reword its message, or add one arm; both need the `exit_code` and slug tables in `crates/ocx_lib/src/oci/sign/error.rs` updated.
- Keep the no-proof case on `TransparencyLogUnavailable` (83) — that one is genuinely transient.
- Flip `build_refuses_a_proof_whose_hex_fields_are_malformed` to assert the data-error kind, and add the sibling case (proof absent) asserting 83, so the two cannot collapse again unobserved.

#### [#324](https://github.com/ocx-sh/ocx/issues/324) net: give the HTTP transport layer one owner (ARCH-16 foundation unit) + two TLS-root bugs

- **Labels**: —
- **Verdict**: PARTIAL · **Gate**: consolidate ocx_lib::net?
- **Size**: XS for bug 2 alone; S for bug 2 plus the `endpoint.rs` fallback deletion; L for the full consolidation. Pass-1's XS/L split is right but omits the middle item.
- **Importance**: high for bug 2 (a silent zero-root seed defeats the whole point of embedded roots, exactly on hosts with no OS trust store); medium for the `endpoint.rs` fallback (a decided policy applied at one site only, on the SSRF-sensitive Sigstore path); medium for the architectural half.
- **Depends on / blocks**: blocks #333 by that issue's own text. Related to #311, #312, #313, #323.
- **Pass-1 → final**: PARTIAL → **AGREE on the verdict, OVERTURNED on evidence.** Three of pass-1's claims are wrong or incomplete: one duplication it says persists has been removed, a third client factory it does not mention exists, and bug 1's exact shape survives at a site it never opened.

**Evidence**

- **Bug 1 — fixed at the cited site.** `build_index_http_client` (`crates/ocx_lib/src/oci/index/ocx_index.rs:223-258`) no longer falls back to `reqwest::Client::new()`. Its fallback is `harden(reqwest::Client::builder()).build().expect(...)`, with the rationale recorded inline as D-011b (`ocx_index.rs:245-253`) and a structural guard asserting the source does not contain `reqwest::Client::new()` (`ocx_index.rs:4755`). Nothing was ever posted back to #324.
- **Bug 2 — NOT fixed.** `seed_embedded_roots` (`crates/ocx_lib/src/utility/tls.rs:20-28`) still skips any root whose DER fails to parse via `if let Ok(...)`, with no count and no zero-root assertion. Its doc still argues the skip is safe because a parse failure would mean upstream bundle corruption — which is the case worth failing on.
- **Bug 2 is sharper than the issue states.** If every root were skipped, the "fixed" fallback above becomes root-less, and its `.expect("a client with no custom roots and only timeout and redirect settings always builds")` is precisely the empty-store panic on the host `build_index_http_client` exists to protect. D-011b's premise ("roots are non-empty") is the thing bug 2 can falsify. Narrow, since the root set is vendored — but it is the argument for failing loudly rather than skipping.
- **Pass-1 is wrong that `forge/github.rs` hand-rolls its own hardened client.** `crates/ocx_lib/src/forge/http.rs` now owns `build_forge_http_client` (`http.rs:33-41`), used by both `forge/github.rs:103` and `forge/gitlab.rs:126`, with a mutation-tested structural guard on the redirect policy (`http.rs:57-70`). That half of the consolidation landed.
- **Pass-1 missed a third client factory**: `sigstore_client_builder` / `sigstore_http_client` in `crates/ocx_lib/src/oci/endpoint.rs:76-123`. So the count today is three in-repo hardened builders — `forge/http.rs`, `oci/index/ocx_index.rs`, `oci/endpoint.rs` — plus the fork's `configured_builder`, all four sharing only `utility::tls::seed_embedded_roots`.
- **Bug 1's shape is alive at that third site.** `sigstore_http_client` ends in `.build().unwrap_or_else(|_| reqwest::Client::new())` (`endpoint.rs:119`) — the exact construct D-011b deleted from the index client. Here it is worse: the bare client drops the SSRF `PinnedResolver` (`endpoint.rs:243`) and `refuse_redirects()` (`endpoint.rs:139`), which the same function's own comment says no path may hand out ("so no path can hand out a client missing the timeouts, the redirect refusal or the pinned resolver", `endpoint.rs:113-116`). The existing guard test (`endpoint.rs:502`) exercises the primary builder only, so the fallback arm is untested.
- **"One retry ladder" partially landed, unmentioned by pass-1.** `crates/ocx_lib/src/oci/transport_policy.rs` now owns `RetryPolicy`, `RetryBudget`, `TransportHardening`, `is_retryable_status`, `is_retryable_transport_error`, `parse_retry_after` and the `run` driver. Only `oci/index/ocx_index.rs` uses it (`ocx_index.rs:74`, `:309-429`). Three hand-rolled doubling backoffs remain: `native_transport.rs:913-925`, `forge/poll.rs:53`, `project/resolve.rs:712`.
- **Consolidation itself NOT built.** No `crates/ocx_lib/src/net` module exists (directory listing confirmed). `ssrf.rs` still lives under `oci/`, and still carries the `impl ClassifyExitCode for SsrfError` (`ssrf.rs:56-68`) that the issue names as the one structural cost. [#313](https://github.com/ocx-sh/ocx/issues/313), the cycle precedent, is still open.

**Remaining scope**

- Fix bug 2: count roots actually seeded in `utility/tls.rs:20-28` and fail loudly on zero. Red-proof it by feeding a deliberately unparseable root set.
- Apply D-011b to `oci/endpoint.rs:119`: delete the `unwrap_or_else(|_| reqwest::Client::new())` arm so no path can return a Sigstore client without the pinned resolver and the redirect refusal, and extend the guard test to cover the fallback arm. Small, independent of the refactor, and the same decision already made once.
- Decide, explicitly, whether the `ocx_lib::net` consolidation happens. This is the NEEDS_DECISION half. Two of the four pieces have landed on their own (`forge/http.rs`, `oci/transport_policy.rs`), so the remaining question is narrower than when the issue was filed: whether `utility/tls.rs`, `oci/ssrf.rs` and one client factory move under one module.
- If consolidating: move `impl ClassifyExitCode for SsrfError` (`ssrf.rs:56-68`) into `cli/classify.rs` first to avoid the crate-split cycle, and route `native_transport.rs`, `forge/poll.rs` and `project/resolve.rs` onto `transport_policy::run` so the ladder is genuinely one copy.

#### [#391](https://github.com/ocx-sh/ocx/issues/391) copy: referrer count reports PUTs issued, not referrers discoverable at the destination

- **Labels**: area/oci, priority/low
- **Verdict**: NOT_STARTED · startable now
- **Size**: S — one function in `copy.rs`, no new types, existing transport call.
- **Importance**: low — false evidence in a report field. The bytes still
  transfer correctly and nothing unsigned is accepted anywhere.
- **Depends on / blocks**: none. Same false-evidence shape as the mirror run
  summary reporting "announced" from an exit code.
- **Pass-1 → final**: NOT_STARTED → **AGREE**. Both citations verified verbatim.

**Evidence**

- `crates/ocx_lib/src/oci/copy.rs:689-696`: `push_referrer_manifest(...)` then
 `copied = copied.saturating_add(1);` then the recursive climb adds its own
 count. No read-back anywhere between the PUT and the `Ok(copied)` at 698.
- `ReferrersApiCapability::probe` maps `Ok(_) => ReferrersSupport::Supported`
 at `crates/ocx_lib/src/oci/referrer/capability.rs:94` — an empty `200` is
 still proof of support, exactly as the issue says.
- The read-back primitive the fix needs already exists on the same transport
 the copier holds: `list_referrers` / `list_referrers_with_fallback`
 (`crates/ocx_lib/src/oci/client/transport.rs:578`), so this is a call, not a
 new capability.

**Remaining scope**

- After `copy_referrers` finishes its climb for a subject, issue one
 `list_referrers` against the target and report that count as the referrer
 count, instead of the PUT accumulator.
- Keep it a single call for the whole climb, not one per manifest.
- Add no gate and no refusal — copy stays verbatim, digest-preserving
 transport with no trust root.
- Cover it with a test whose destination accepts the PUT and lists nothing, so
 the reported count can go to zero while the PUTs all succeed.

### Batch 2 — Small disjoint cleanups

#### [#46](https://github.com/ocx-sh/ocx/issues/46) perf(oci): stream layer archives across the OciTransport boundary — client still buffers 4 × archive size

- **Labels**: performance, area/oci
- **Verdict**: PARTIAL · startable now
- **Size**: S — one arm in one file plus its tests; both primitives (`hash_file`, `push_blob_from_path`) already exist and `copy.rs:505` is a worked template. (Pass-1 said M; adjusted down because no new primitive is needed.)
- **Importance**: medium — real OOM ceiling (4 × layer size) on the package-publish path only; pull and registry-to-registry copy are already streamed.
- **Depends on / blocks**: none. Sibling to #167.
- **Pass-1 → final**: PARTIAL → **AGREE**

**Evidence**

- `OciTransport::push_blob_from_path` exists at `crates/ocx_lib/src/oci/client/transport.rs:493`, and is **required with no default** — its doc says the obvious default would reintroduce the allocation this method exists to avoid. Test doubles opt in explicitly via `push_blob_buffered`.
- `OciTransport::push_blob` still takes `data: Vec<u8>` (`transport.rs:465`).
- `push_multi_layer_manifest`'s `LayerRef::File` arm still does `Algorithm::Sha256.hash_file_read(path)` → `transport.push_blob(&image, package_data, …)` (`crates/ocx_lib/src/oci/client.rs:1481` and `:1509`). The `BOUNDED:` note naming the 4 × ceiling is directly above it (`client.rs:1470-1474`).
- `const LAYER_PUSH_CONCURRENCY: usize = 4` (`client.rs:21`), whose own doc says "Each `LayerRef::File` reads the full archive into memory before uploading".
- Only caller of the streaming push is the registry-to-registry copy (`crates/ocx_lib/src/oci/copy.rs:505`).

**Remaining scope**

- In `push_multi_layer_manifest`'s `LayerRef::File` arm, replace `hash_file_read(path)` with `Algorithm::hash_file(path)` and `transport.push_blob(...)` with `transport.push_blob_from_path(&image, path, &digest, on_progress)`.
- Take the descriptor `size` and the progress-bar total from `tokio::fs::metadata(path)` instead of `package_data.len()`.
- Delete the `BOUNDED:` comment block at `client.rs:1470-1474` and the memory clause in `LAYER_PUSH_CONCURRENCY`'s doc (`client.rs:17-20`) in the same commit — leaving them makes the constant read as memory-bounded when it no longer is.
- Re-tune `LAYER_PUSH_CONCURRENCY` against registry throughput. This half needs bench data and can ship separately; do not block the memory fix on it.

#### [#53](https://github.com/ocx-sh/ocx/issues/53) gc: parallelize delete_objects to reduce ocx clean latency

- **Labels**: good first issue, performance, area/package-manager, priority/low
- **Verdict**: NOT_STARTED · startable now
- **Size**: S — confirmed. The code change is ~10 lines using an idiom already in
  the tree; the bench row is the bulk of the work. The owner's 2026-08-29 snapshot
  also says SMALL and explicitly records these gotchas as "none tier-moving".
- **Importance**: low — `priority/low`, user-invoked maintenance command, bounded win.
- **Depends on / blocks**: depends on #35 (landed). Do before #50, which rewrites
  the surrounding code.
- **Pass-1 → final**: NOT_STARTED → **AGREE on verdict and size, OVERTURNED on the
  remaining scope** (pass 1 prescribes `JoinSet`, which is the wrong idiom for this
  repo, and asks for a stable sort that the code already does)

**Evidence**

- `crates/ocx_lib/src/package_manager/tasks/garbage_collection.rs:175-217`
 `delete_objects` — I read the whole function. Still a plain sequential
 `for target in sorted_targets` with `tokio::fs::remove_dir_all(target).await`
 at `:200`. Confirmed unparallelised.
- **The issue's own line numbers are stale.** It cites `garbage_collection.rs:100-119`;
 the function is now at `:175-217`. Anyone opening the cited range lands in the
 wrong place.
- **Two-thirds of the issue's proposal is already satisfied.** `:188-189` already
 does `let mut sorted_targets: Vec<&PathBuf> = targets.iter().collect();
 sorted_targets.sort();` — deterministic ordering exists. And `:203-205` already
 treats `ErrorKind::NotFound` as non-fatal. What is missing is only the
 concurrency, and the "keep errors per-target so one failure doesn't abort the
 sweep" half: `:206` still does `return Err(...)` on any other error, which does
 abort the sweep.
- **Wrong idiom in pass 1.** Pass 1 says "replace the loop with a bounded
 `JoinSet` … per `quality-rust.md`'s JoinSet rule". This repo standardised on
 `futures::stream::buffer_unordered` for bounded I/O fan-out — PR
 [#59](https://github.com/ocx-sh/ocx/pull/59) "refactor: unify bounded-concurrency
 fan-out on stream::buffered", and seven `ocx_lib` files use it today
 (`announce/pipeline.rs`, `oci/copy.rs`, `oci/index/local_index.rs`,
 `package/cascade/apply.rs`, `package/cascade/gather.rs`,
 `package_manager/composer.rs`, `publisher/publish_gate.rs`). Independently
 flagged at `analysis_issue_triage_2026-08-29.md:215`: "the plan copies the wrong
 idiom … the correct shape is *fewer* lines." `quality-rust.md:136-138` describes
 `JoinSet` semantics but does not mandate it here.
- **The benchmark gap is real and I verified it directly.** The issue carries the
 `performance` label, and `quality-core.md` "Two Hats" makes benchmarks mandatory
 for optimisation work. `test/bench/baseline.json` has 17 rows and I listed every
 one: `baseline_curl_*`, `ocx_install_*`, `ocx_parallel_*`, `ocx_layers_*`. All
 network, install or layer scenarios. **No `ocx clean` row, no local-filesystem
 row.** One has to be added or the speedup claim is unmeasurable.

**Remaining scope**

- Replace the sequential loop at `garbage_collection.rs:191-208` with
 `futures::stream::iter(...).map(...).buffer_unordered(N)` — the repo's
 established bounded fan-out idiom. **Do not use `JoinSet` here**; seven
 `ocx_lib` sites use `buffer_unordered` and it is the shorter shape.
- Make per-target errors non-aborting. `:206` currently returns on the first
 non-`NotFound` error; collect them instead so one failure does not end the sweep.
- Leave the ordering alone. `:188-189` already sorts targets before the loop, so
 deterministic reporting only needs the results re-sorted after the fan-out, not
 a new sort key.
- Add a local-filesystem `ocx clean` scenario to `test/bench/scenarios.py` and a
 baseline row to `test/bench/baseline.json`. None of the existing 17 rows exercises
 this path, so without one the `performance` label has nothing behind it.
- Fix the issue body's stale citation: the function is at
 `garbage_collection.rs:175-217`, not `:100-119`.

#### [#71](https://github.com/ocx-sh/ocx/issues/71) feat(cli): ocx install --reinstall <pkg> for in-place package refresh

- **Labels**: area/package-manager, area/cli, priority/low
- **Verdict**: NOT_STARTED · startable now
- **Size**: S — confirmed. One flag, semantics fully specified, no open design
  question, roughly three files. (The owner's 2026-08-29 snapshot says MEDIUM with
  the note "not a one-file change"; S here means ≤ half a day and ≤ 3 files, which
  still holds — the two scales differ, the judgement does not.)
- **Importance**: low — confirmed. Self-labelled a tracking issue with
  "Implementation timing: when a real consumer appears", `priority/low`, and no
  consumer has appeared: the migration tooling that would have used it was deleted.
- **Depends on / blocks**: none. Shares the install-flow surface with #69.
- **Pass-1 → final**: NOT_STARTED → **AGREE on verdict and size, OVERTURNED on
  remaining scope** (the issue's CLI grammar no longer exists and one of its
  acceptance criteria is now forbidden by project policy; pass 1 reproduces both
  uncritically)

**Evidence**

- `crates/ocx_cli/src/command/install.rs` — I read the whole file. `Install` has
 four fields: `select`, `platform`, `verify`, `packages`. No `reinstall`.
- `git grep -i reinstall` over `crates/`, `test/`, `website/`, `.claude/` returns
 no flag anywhere. The hits are doc-comment prose
 (`command/deps.rs:319`, `oci/host_capabilities.rs:115,118,1205,2298`), pytest
 scenarios that do uninstall-then-install by hand
 (`test/tests/test_dependencies.py:548,968`,
 `test/scenarios/offline/reinstall-after-purge*.sh`), and ADR prose. No plumbing.
- No standalone launcher-regeneration or repair verb exists either:
 `git grep -i -e "regenerate.*launcher" -e "fn repair" -e "fn heal"` finds only
 `ocx package cascade repair` (rolling tags on a registry) and
 `shell/reconcile/plan.rs:627 repair_owned_segments` (PATH segments). Neither is
 a package refresh.
- **The CLI grammar in the issue title no longer exists.** `ocx install` is now
 `ocx package install` — `command/package.rs:60` declares
 `Install(super::install::Install)`, and `install.rs:13-15` records the move in
 its own header ("`install` itself is moved from root `Command` to
 `Package::Install` (C1)"), together with the removal of `--global`. The root
 `Command` enum (`command.rs:94-171`) has no `Install` variant. The issue's flag
 must be spelled `ocx package install --reinstall`.
- **One acceptance criterion is now forbidden.** The issue asks for a "CHANGELOG
 entry under `### Added` *(cli)*". `CLAUDE.md` now states `CHANGELOG.md` is
 generated by `git-cliff` at release time and that editing it is always wrong —
 the changelog entry *is* the commit subject. That checkbox must be struck, not
 satisfied.
- The design rationale is still current: `.claude/artifacts/adr_package_entry_points.md:720`
 reads "No `ocx migrate` or `ocx repair` command v1. A first-class
 `ocx install --reinstall <pkg>` flag … is tracked at ocx-sh/ocx#71." The ADR
 still points here, so this is not superseded.

**Remaining scope**

- Spell the flag `ocx package install --reinstall <pkg>`. The root `ocx install`
 command no longer exists (`command/package.rs:60`); update the issue title too.
- Add the `reinstall` field to `Install` in
 `crates/ocx_cli/src/command/install.rs` and thread it into `manager.install_all`.
- Semantics as written in the issue and unchanged: drop the existing candidate
 symlink, re-pull manifest and content (a CAS no-op on an unchanged digest),
 re-emit the candidate symlink, preserve `current` if the package was selected,
 regenerate the `entrypoints/` launcher set. Idempotent; combinable with `--select`.
- Preserve dependency forward-refs — no GC cascade. `test/tests/test_dependencies.py:548`
 `test_reinstall_restores_dependency_refs` already pins the uninstall-then-install
 behaviour and is the natural comparison point.
- Errors surface as `PackageErrorKind` variants, consistent with other install paths.
- Acceptance test in `test/tests/test_install.py`.
- **Strike the "CHANGELOG entry under `### Added`" criterion.** `CHANGELOG.md` is
 generated by `git-cliff`; per `CLAUDE.md` the changelog entry is the commit
 subject and the file is never hand-edited.

#### [#79](https://github.com/ocx-sh/ocx/issues/79) [entry-points-followup] Move LauncherUnsafeCharacter out of crate-root error.rs

- **Labels**: tech-debt, entry-points-followup
- **Verdict**: NOT_STARTED · startable now
- **Size**: S — five files, mechanical, and the compiler supplies the red-to-green because both existing tests match on the old type. Same as pass 1.
- **Importance**: low — code-location tech debt, no behaviour change.
- **Depends on / blocks**: none.
- **Pass-1 → final**: PARTIAL → **OVERTURNED**. The maintainer's 2026-08-29 revision rewrote the title and body to the surviving scope, "Remaining scope is that one variant", and declared the `EntrypointInstallFailed` half dead. Under the issue as it now reads, nothing has landed; counting an explicitly de-scoped deletion as partial progress is wrong.

**Evidence**

- `git grep -n "EntrypointInstallFailed" -- crates/ external/` returns nothing. Confirmed gone, and confirmed out of scope by the revision.
- `crates/ocx_lib/src/error.rs:153` — `LauncherUnsafeCharacter { value: String, character: char }` still in the crate-root enum. `:352` — `Self::LauncherUnsafeCharacter { .. } => Some(ExitCode::DataError)`. `:170` — a `launcher_unsafe_hint` helper that exists only for this variant's `#[error]` string.
- Sole raise site `crates/ocx_lib/src/package_manager/launcher/safety.rs:67`, inside `LauncherSafeString::new`, which returns `Result<Self, crate::Error>`. Match sites: `safety.rs:90` and `crates/ocx_lib/src/package_manager/launcher/generate.rs:325`, both in tests.
- **Correction to pass 1's remaining scope, item 1.** Pass 1 relayed from the older triage artifact that a `try_downcast!` match on this variant exists in "CLI `cli/classify.rs`". There is no such match. `crates/ocx_lib/src/cli/classify.rs` is in `ocx_lib`, not the CLI, and `try_downcast!(PackageManagerError)` / `try_downcast!(PackageErrorKind)` are already registered at `:180-181`. Moving the variant into `PackageErrorKind` needs **no** edit to `classify.rs`.
- **Correction to pass 1's remaining scope, item 2.** The extra site at `crates/ocx_lib/src/package_manager/launcher.rs:32` is real but is a `?` propagation inside `shim_body`, not a match arm. Both of its callers already wrap into `PackageErrorKind::Internal` (`tasks/prepare_lazy.rs:347`, `tasks/pull.rs:536`), so the move simplifies them rather than complicating them.
- **Destination correction.** `package_manager::error::Error` is a batch enum (`FindFailed(Vec<PackageError>)` and siblings, `error.rs:31-62`) and is the wrong home for a single-value variant. `PackageErrorKind` (`:132`) is the right one — `EntrypointCollision` already lives there at `:180`.
- **Dead doc found while reading.** `safety.rs:11-13` claims the publish-time validator calls into this module. It does not: `git grep -n "LauncherSafeString"` finds callers only in `launcher.rs`, `launcher/body.rs` and `launcher/generate.rs`. Worth deleting in the same commit.

**Remaining scope**

- Add `LauncherUnsafeCharacter { value, character }` to `PackageErrorKind` in `crates/ocx_lib/src/package_manager/error.rs`, with its `ClassifyExitCode` arm returning `ExitCode::DataError` in the `impl ClassifyExitCode for PackageErrorKind` block at `:394`.
- Change `LauncherSafeString::new` (`launcher/safety.rs:67`) to return the new error type, and follow the `?` through `launcher.rs:32` (`shim_body`) and `launcher/generate.rs:67` (`generate`).
- Simplify the two callers that currently wrap into `PackageErrorKind::Internal`: `tasks/prepare_lazy.rs:347` and `tasks/pull.rs:536`.
- Update the two test match arms: `launcher/safety.rs:90` and `launcher/generate.rs:325`.
- Delete the variant, its `ClassifyExitCode` arm and the `launcher_unsafe_hint` helper from `crates/ocx_lib/src/error.rs` (`:153`, `:352`, `:170`).
- Delete the stale sentence at `launcher/safety.rs:11-13` claiming a publish-time caller.
- No change to `crates/ocx_lib/src/cli/classify.rs` — `PackageErrorKind` is already in the downcast ladder.
- Run `task rust:verify`.

#### [#81](https://github.com/ocx-sh/ocx/issues/81) [entry-points-followup] Add completeness assertion before Vec&lt;Option&gt;::flatten in pull.rs dep setup

- **Labels**: tech-debt, entry-points-followup
- **Verdict**: NOT_STARTED · startable now
- **Size**: XS — one small helper, two call sites, one gated test. Down from pass 1's flagged S, because the decision it flagged does not exist.
- **Importance**: low, down from pass 1's medium. The invariant is enforced by control flow today: every spawned task writes its slot and the `?` returns before `.flatten()` on any error. This is a tripwire against a future refactor, not a live defect.
- **Depends on / blocks**: none.
- **Pass-1 → final**: NOT_STARTED → **AGREE on the verdict, OVERTURNED on the blocker**. Pass 1's headline "looks-easy-is-not" flag — that a `debug_assert!` here "could never fire in the test suite" — is false. It fires on every pull request.

**Evidence**

- `crates/ocx_lib/src/package_manager/tasks/pull.rs:650` `setup_dependencies` — `results: Vec<Option<Arc<InstallInfo>>>` at `:681`, `Ok(results.into_iter().flatten().collect())` at `:694`, no assertion.
- `crates/ocx_lib/src/package_manager/tasks/pull.rs:778` `extract_layers` — `results: Vec<Option<oci::Digest>>` at `:819`, bare `.flatten()` at `:831`, no assertion.
- **The overturn.** Pass 1 (and the older triage artifact it inherited from) checked only `taskfiles/rust.taskfile.yml:146`, which is indeed `cargo nextest run --workspace --release --locked`. CI is different. `.github/workflows/verify-basic.yml:228` runs `cargo nextest run --workspace --locked --profile ci` — **no `--release`** — and that workflow triggers on every `pull_request` to `main`. `.github/workflows/verify-deep.yml:87` does the same across the Linux, macOS and Windows matrix. `--profile ci` is the *nextest* profile from `.config/nextest.toml`, not a cargo profile, so the cargo profile there is `dev` and `debug_assertions` is on.
- The workflow comment at `verify-basic.yml:212-221` says so itself: it runs raw cargo deliberately because "that task hardcodes `--release`, which the same rule file lists as an anti-pattern for unit tests". `.claude/rules/subsystem-ci.md:237` lists `--release` for unit tests as an anti-pattern with the remedy "Debug; reserve release for published binaries".
- There is no `[profile.release]` section in `Cargo.toml` (only `dist` and `shim`, both `inherits = "release"`), so `--release` does use the cargo default `debug-assertions = false`. That half of pass 1's reasoning is sound; its conclusion is not, because the local task is not the only runner.
- **The real gotcha, which nobody has named.** A test that forces a partial `Vec<Option<_>>` and expects a panic passes in debug and fails on the release legs, so it needs a `#[cfg(debug_assertions)]` gate. And neither function exposes a seam: `results` is a local in a private `async fn`, so the "optional but recommended" unit test in the acceptance criteria is not writable without first extracting the flatten step.

**Remaining scope**

- Extract one shared helper both sites call, so the guard exists once and is directly testable, for example `fn all_present<T>(results: Vec<Option<T>>) -> Vec<T>` carrying the `debug_assert!` and the `.into_iter().flatten().collect()`.
- Call it from `setup_dependencies` (`pull.rs:694`) and `extract_layers` (`pull.rs:831`).
- Assertion message names both the count of `None` slots and the total expected, per the issue.
- Add a unit test on the helper passing a partial vector. Gate it `#[cfg(debug_assertions)]` — `task rust:test:unit` runs `--release`, where the assertion is compiled out, so an ungated `#[should_panic]` test fails locally and on the Linux CI leg while passing on the Windows leg.
- `debug_assert!` is the right mechanism, not `assert!`: it is exercised on every pull request by `verify-basic.yml`'s Windows leg and across the `verify-deep.yml` matrix. No design decision is needed here.

#### [#322](https://github.com/ocx-sh/ocx/issues/322) test: nothing forces a new error variant to get an error-slug row

- **Labels**: —
- **Verdict**: NOT_STARTED · startable now
- **Size**: S — three test sites, one shared pattern, and the dependency question is already settled by the existing lockfile.
- **Importance**: low — all rows are correct today, so this is guard hardening, not a live gap.
- **Depends on / blocks**: none.
- **Pass-1 → final**: PARTIAL → **OVERTURNED**. The issue's stated closing condition is "Variant enumeration, so a new variant fails to compile until it has a row." That is met at zero of the three sites, including the one pass 1 called fixed. Pass 1 also contradicted itself, first noting the exhaustive-match approach "doesn't directly transfer" and then listing `ffa19b0d` as "the intended pattern for the remaining two tables".

**Evidence**

- The `error_envelope.rs` table did move. `ffa19b0d refactor(cli)!: classify exit codes in a match the compiler keeps total` touched exactly three files and relocated the logic to `crates/ocx_lib/src/cli/error_category.rs`. `ErrorCategory::from_exit_code` at `:55-81` is an exhaustive match with no wildcard, and its doc at `:42-49` records that the former cross-crate form needed a `_ => Internal` arm under which a new exit code "compiled clean, passed clippy, and silently serialized as `internal`". Removing that wildcard was a real fix.
- **But the gap #322 names survives there too.** The test at `error_category.rs:120` still holds a hand-written `cases` array with `assert_eq!(cases.len(), 16, ...)` at `:155-158`, and its own comment concedes "It cannot force a row for a *new* `ExitCode` variant — `cases` is an array literal, so `len()` is a compile-time constant." A new `ExitCode` variant must get an arm, but may get a *wrong* arm and no test row, and the suite stays green. That is structurally the same failure the issue describes for the slug tables. Note the count is 16 now, not the 15 the issue and pass 1 both cite.
- The other two tables are exactly as the issue describes. `crates/ocx_lib/src/oci/sign/error.rs:884-888` asserts `pairs.len() == 25`; `crates/ocx_lib/src/oci/verify/error.rs:1613-1617` asserts `pairs.len() == 46`. Both comments now correctly state the count pins only row deletion. Both counts have roughly doubled since filing (12 and 23), so the unforced surface has grown, not shrunk.
- `kind_detail` is confirmed wildcard-free: 25 match arms in `sign/error.rs`, no `_ =>`, against a 25-row table. `SignErrorKind` is `#[non_exhaustive]` at `:66`, which binds downstream crates only, so in-crate the match is total. The arm is forced; the row is not.
- **The overturn that changes the cost.** Pass 1 and the older triage artifact both state `strum` is absent and that adopting it "needs a dependency review and third-party-notice regeneration". Both checked only the manifests. `strum` v0.27.2 and `strum_macros` v0.27.2 are already in `Cargo.lock:5528,5534`, reach the workspace via `oci-spec` → `oci-client` → `ocx_lib` (`cargo tree -i strum`), and are already listed in `LICENSE-THIRD-PARTY.md:10825-10826`. Adding a direct dev-dependency at the same version introduces zero new third-party surface and needs no notice regeneration.
- One real constraint the issue anticipates: a plain `#[derive(EnumIter)]` will not compile on these enums, because `EnumIter` requires every variant field to implement `Default` and these carry `Box<std::io::Error>` and `KeyBackendError`. The issue's phrase "the iter has to be over unit-constructible sample values" is pointing at `EnumDiscriminants` plus `EnumIter` over the generated fieldless discriminant enum.

**Remaining scope**

- Add variant enumeration so a new variant fails to compile, or fails the test, until it has a table row. Three sites, not two: `SignErrorKind` (`crates/ocx_lib/src/oci/sign/error.rs`), `VerifyErrorKind` (`crates/ocx_lib/src/oci/verify/error.rs`), and `ExitCode` → `ErrorCategory` (`crates/ocx_lib/src/cli/error_category.rs`). The third is often assumed solved by `ffa19b0d`; that commit removed a wildcard and forces the *arm*, not the *row*, so its mapping is still unpinned.
- The `strum` option is cheaper than the issue estimates: `strum` and `strum_macros` v0.27.2 are already in `Cargo.lock` via `oci-spec`, already in `LICENSE-THIRD-PARTY.md`, and already pass the license audit. A direct dev-dependency at the same version adds no new crate and needs no notice regeneration.
- Plain `EnumIter` will not derive on `SignErrorKind` or `VerifyErrorKind` — it requires every variant field to be `Default`, and these carry `Box<std::io::Error>` and `KeyBackendError`. Use `EnumDiscriminants` with `EnumIter` on the generated fieldless discriminant enum and assert every discriminant appears in the table.
- Whichever mechanism is chosen, demonstrate it red: add a variant without a row and show the test fails, then restore. The current `len()` assertion cannot go red for that mutation, which is the whole point of the issue.

#### [#306](https://github.com/ocx-sh/ocx/issues/306) Patch companion overlay re-emits a shared dependency's env entries

- **Labels**: —
- **Verdict**: NOT_STARTED · **Gate**: Option 1 default
- **Size**: S for Option 1 plus its two tests, single file. XL for Option 2 — it needs an ADR and
  reworks the provenance region model.
- **Importance**: low — the issue itself records that duplicate `path`, `constant` and `list`
  contributions fold idempotently today, so there is no user-visible bug; the value is closing a
  latent contract hole that already produced a real violation on the `integrations` carrier, and
  cleaning up `--show-patches` attribution.
- **Depends on / blocks**: none.
- **Pass-1 → final**: PARTIAL → **OVERTURNED**. Pass 1 scored the `integrations` dedup as "the
  integrations half of this bug is fixed" — but the issue's subject is `entries`, and it names the
  sibling-carrier fix in its own body as work done *elsewhere*, explicitly "**not** by unifying the
  two composition passes, which is the larger change this issue tracks". Nothing of this issue's own
  ask exists on main. The remaining-scope list is unchanged; the label change matters because
  PARTIAL reads as half-delivered.

**Evidence**

- **The defect is structurally live, confirmed end to end, not just by grep.** In
 `crates/ocx_lib/src/package_manager/tasks/resolve.rs`, the overlay merge loop pushes every
 companion-composed entry unconditionally — `entries.push(entry)` at `:922`, with only the
 reserved-key filter above it — while the `integrations` branch three lines below (`:928`) is
 guarded by `seen_integrations` (`:907-912`).
- **The companion overlay genuinely carries the shared dep's entries**, so the duplicate is not
 hypothetical: `build_site_patch_set` extends the overlay with the whole composition output —
 `companion_overlay.entries.extend(out.entries…)` at `resolve.rs:1388-1390` — after
 `composer::compose_companion` (`crates/ocx_lib/src/package_manager/composer.rs:275-287`)
 composes the companion **and its transitive closure** through `compose_gated` (`:295`), whose
 `seen` set is local to that call. No filter reduces the overlay to the companion's own rows.
- The code says so in its own words at `resolve.rs:890-896`: *"The base roots and each companion
 are composed by SEPARATE `compose` calls, and each dedups only within itself — so a dependency
 reachable from both a base root and a companion, or from two companions, arrives here twice."*
 That comment justifies the dedup that was applied to `integrations` only.
- **The sibling fix post-dates the issue but is not this issue's ask.**
 `git log -S seen_integrations` returns exactly one commit,
 [`5a5828af`](https://github.com/ocx-sh/ocx/commit/5a5828af) *"feat(env): compose
 vendor-namespaced package integrations with per-package attribution"*
 ([PR #307](https://github.com/ocx-sh/ocx/pull/307), merged 2026-08-11) — the carrier the issue
 calls `customizations` and describes as already handled. There has never been a
 `seen_customizations` on main.
- **No test covers the entries case.** The two regression tests that exist —
 `a_dep_reachable_from_a_base_and_a_companion_contributes_one_row` (`resolve.rs:6020`) and
 `two_companions_reaching_one_dep_contribute_one_row` (`:6074`) — bind the composed entries as
 `_entries` and assert only on `attribution.integrations` via the `integration_rows` helper
 (`:5997`). Their fixture is one edit away from being the repro this issue asks for: give
 `seed_shared_customizing_dep` (`:5893`) an interface env var and count its contributions.
- Acceptance side is clear too: `test/tests/test_patches.py` has no shared-dependency duplication
 test. `test_global_companion_appears_once_when_it_matches_several_bases` (`:296`) covers a
 different axis — one companion matching several bases, handled by the projection cache at
 `resolve.rs:1082` — and `test_patch_companion_integrations_appear_once_across_several_bases`
 (`:1179`) is the integrations carrier again.

**Remaining scope**

- Reproduce first, as the issue instructs: extend the fixture at
 `crates/ocx_lib/src/package_manager/tasks/resolve.rs:5893` so the shared dependency declares an
 interface env var, and assert the composed `entries` currently contain it twice.
- **Decide Option 1 vs Option 2 before building.** Option 1 is a merge-site
 `(PinnedIdentifier, key)` dedup at `resolve.rs:922`, mirroring the `seen_integrations` pattern
 ten lines above, including its `strip_advisory()` keying — small, but a second dedup rule rather
 than a removed cause. Option 2 is single-pass composition of base roots and companions, which
 fixes both carriers at the root but breaks the contiguous-overlay assumption `--show-patches`
 and `ocx patch why` rely on, and the issue says it needs its own ADR.
- Land the issue's stated acceptance test: a base and a companion sharing one dependency that
 declares an interface env var yields exactly one `entries` contribution, attributed under
 `--show-patches` to the base rather than the companion.

#### [#102](https://github.com/ocx-sh/ocx/issues/102) SLSA provenance attach: `ocx package push --provenance FILE`

- **Labels**: security, area/oci, area/cli
- **Verdict**: PARTIAL · startable now
- **Size**: S — one CLI file plus two doc pages; the engine, the floor and the tests already exist. The only non-mechanical bit is the signed-only decision above.
- **Importance**: low — the capability ships today via `attest --type slsaprovenance1`; this is ergonomics only, and the issue's own 2026-08-29 revision says it gates nothing.
- **Depends on / blocks**: blocks nothing. #108 can be written against the general verb instead of waiting for it.
- **Pass-1 → final**: PARTIAL → **AGREE on the verdict, OVERTURNED on the remaining scope.** Pass 1
  lists "predicate-type version validation (issue asks to reject < v1.0)" as open work and asks
  "check whether the aliases should be excluded". That work is shipped, wired and tested on three
  levels. The only thing left is the sugar flag and its doc line.

**Evidence**

- The `>= v1.0` floor is enforced in the attest pipeline, not deferred: `crates/ocx_lib/src/oci/attest/pipeline.rs:277-281` returns `SignErrorKind::ProvenanceVersionUnsupported` when `predicate::is_provenance_below_v1(ctx.predicate_type)`. The refusal lands before any network call and before any token is resolved.
- `crates/ocx_lib/src/oci/attest/predicate.rs:6-12` states the split deliberately: the alias table is cosign's verbatim (bare `slsaprovenance` → v0.2), and "the `>= v1.0` attach-side floor is enforced by the attest pipeline instead". So the aliases must stay — excluding them would diverge from cosign.
- Unit test dispatches on the resolved URI, not the variant: `attest_refuses_every_spelling_of_provenance_below_v1` (`attest/pipeline.rs:1207-1245`), covering `SlsaProvenance`, `SlsaProvenance02` and the full v0.2 URI; asserts exit 64, an empty transport tape, zero token acquisitions, and that the message names `--type slsaprovenance1`. Comment tags it "S-003: the SLSA provenance >= v1.0 attach floor (#102, row 21)".
- Acceptance test `test_attest_refuses_slsa_provenance_below_v1_and_names_the_flag_to_use` (`test/tests/test_attest.py:286-311`) asserts the JSON envelope detail `provenance_version_unsupported`.
- The "DSSE payload subject digest == pushed manifest digest" criterion is also already covered: `test/tests/test_attest.py:186-191` fetches the platform manifest digest from the registry and asserts `data["subject_digest"] == platform_digest`, with the referrer's `artifactType` and both `dev.sigstore.bundle.*` annotations checked at `:193-198`.
- Documented for users: `website/src/docs/user-guide/attestations.md:100-117` carries the alias table and states that `slsaprovenance` / `slsaprovenance02` are refused at exit 64 with `slsaprovenance1` as the remedy.
- What is genuinely absent: `grep -c provenance crates/ocx_cli/src/command/package_push.rs` → 0. `--sbom` exists at `package_push.rs:112-123` with the `ArgGroup` at `:32` and the `attest_sbom` helper at `:560`. No sibling for provenance.

**Remaining scope**

- Add `--provenance PATH` to `PackagePush` mirroring `--sbom` (`package_push.rs:112-123`), extend the `signing_target` `ArgGroup` at `:32`, and add an `attest_provenance` sibling of `attest_sbom` (`:560`) that passes `PredicateType::SlsaProvenance1`.
- Decide one design point: provenance has no SBOM media type, so an unsigned attach is refused outright (`UnsignedTypeUnsupported`, exit 64 — `attest/pipeline.rs:265-267`, test at `:1723-1755`). `--provenance` therefore requires signing material, unlike `--sbom` which degrades to an unsigned attach. Either require it explicitly or let the existing refusal surface.
- Do **not** touch predicate-version validation: shipped and tested.
- Add the `--provenance` entry to `website/src/docs/reference/command-line.md` (`#package-push`) and a line to `website/src/docs/user-guide/attestations.md`.
- The "publisher handoff example using `actions/attest-build-provenance`" criterion belongs to #108, not here.

### Batch 3 — Package-manager / config features

#### [#333](https://github.com/ocx-sh/ocx/issues/333) feat(net): make index/registry timeouts, retries and fan-out width configurable

- **Labels**: performance, area/oci, discussion-needed
- **Verdict**: PARTIAL · **Gate**: loosely gated on #324 decision
- **Size**: M for A9 + A10 as scoped, and gated on the #324 decision by the issue's own text — though less strictly than when filed, since `transport_policy.rs` already provides the index-side owner.
- **Importance**: medium — real pain behind a slow or throttling corporate proxy, but the acute failure (one timeout killing a whole `index sync`) is already fixed.
- **Depends on / blocks**: depends on #324 (open). Refs #330 (closed), #167, #316.
- **Pass-1 → final**: PARTIAL → **AGREE**

**Evidence**

- **Problem 1 fixed.** The hard 60 s total-request deadline is gone, replaced by three composed bounds: `INDEX_CONNECT_TIMEOUT` 30 s (`crates/ocx_lib/src/oci/index/ocx_index.rs:97`), `INDEX_IDLE_BOUND` 30 s (`:108`) and `INDEX_OUTER_CAP` 300 s (`:135`), each with the D-011 rationale inline. `INDEX_REQUEST_TIMEOUT` no longer exists.
- **A9 not built.** `INDEX_REFRESH_CONCURRENCY: usize = 8` (`crates/ocx_cli/src/command/index_common.rs:31`) and `TAG_REFRESH_CONCURRENCY: usize = 64` (`crates/ocx_lib/src/oci/index/local_index.rs:34`) are still compile-time constants. `OCX_JOBS` resolves only through `crates/ocx_cli/src/app/context.rs:1163-1176` into the package-manager fan-out, and `website/src/docs/reference/environment.md:383-397` documents it as applying to `install`, `pull`, `package pull`, `exec` and `env` — the index verbs are not listed.
- **A10 not built, and worse than "absent".** `RegistryDefaults` (`crates/ocx_lib/src/config.rs:166-187`) carries only `default` and `system_locked`. The struct has no `deny_unknown_fields`, and three tests already write `[registry] timeout = 30` as a *deliberately ignored* forward-compat key (`config.rs:434`, `config/loader.rs:3671`, `config/managed.rs:1089`). So an operator who sets `[registry] timeout` today gets silence, not an error — A10 has to turn an already-accepted-and-discarded key into a real one.
- **The registry-side timeouts are still constants too**: `REGISTRY_READ_TIMEOUT` / `REGISTRY_CONNECT_TIMEOUT` (`crates/ocx_lib/src/oci/client/builder.rs:70`, wired at `:102-103`).
- **What the issue could not know**: `oci/transport_policy.rs` now exists, so A10's "retry" key has a `RetryPolicy` struct to bind to and the index client already reads its hardening from an injectable `TransportHardening`. That materially lowers A10's cost on the index leg.
- Structural guard tests still pin the `8 × 64 == 512` ceiling as a contract (`index_common.rs`, `local_index.rs`), so widening it is a contract amendment.

**Remaining scope**

- **A9**: thread `--jobs` / `OCX_JOBS` into the index refresh fan-out, replacing the constant at `index_common.rs:31` (and deciding whether `TAG_REFRESH_CONCURRENCY` follows or stays fixed). Amend the `8 × 64 == 512` guard tests and ADR `adr_servable_index_snapshot.md` C-024 in the same commit — they assert the product as a contract. Update `website/src/docs/reference/environment.md:383` so `OCX_JOBS`'s stated command list includes the index verbs.
- **A10**: add `timeout` / `retry` / `jobs` to `RegistryDefaults` (`config.rs:166`). Because the key is currently parsed-and-ignored, add a test that it now takes effect, not merely that it parses. Regenerate the JSON schema (`crates/ocx_schema`) and check the managed-config forward-compat posture per `adr_managed_config_tier.md`.
- Bind the new keys to the structures that already exist rather than new ones: `TransportHardening` and `RetryPolicy` in `oci/transport_policy.rs` for the index leg, `REGISTRY_*` in `oci/client/builder.rs:70` for the registry leg.
- Document the new keys in the config reference alongside `[registry] default`.

#### [#326](https://github.com/ocx-sh/ocx/issues/326) CLI write interfaces: ocx env set/unset and ocx config set

- **Labels**: —
- **Verdict**: NOT_STARTED · **Gate**: none (absorbs #328/#329)
- **Size**: M — the `[env]` half reuses proven infrastructure; the `config.toml` half is new but narrow.
- **Importance**: medium — [ocx-sh/ocx-sdk-python](https://github.com/ocx-sh/ocx-sdk-python) blocks its entire write-side API on this by design rule, and rules_ocx / find_ocx share the constraint. No end-user complaint.
- **Depends on / blocks**: blocks the ocx-sdk-python write API. Overlaps #328 (config half) and #329 (env half).
- **Pass-1 → final**: NOT_STARTED → **AGREE**. Citations check out, with one correction to which file is which.

**Evidence**

- `crates/ocx_cli/src/command/config.rs:27-72` — `pub enum ConfigGroup` has exactly four variants: `Setup`, `Update`, `Test`, `Push`. No `get`, `set`, `unset`, or `describe`, for any tier.
- Both env commands are read-only: `crates/ocx_cli/src/command/toolchain_env.rs:135` (`pub struct ToolchainEnv`, the top-level `ocx env`) and `crates/ocx_cli/src/command/env.rs:26` (`pub struct Env`, the package-tier `ocx package env`) are plain structs with no `Subcommand` enum. Pass-1 cited `env.rs` as "the `Env` struct" for the top-level command; the top-level variant is actually `Env(toolchain_env::ToolchainEnv)` per `crates/ocx_cli/src/command.rs:100`. Both are read-only, so the conclusion stands.
- The style-preserving write machinery pass-1 points at is real and does what it claims: `crates/ocx_lib/src/project/mutate.rs:287` `add_binding` and `:361` `remove_binding` both route through `super::document::render_preserving(&original, &config, config_path)` (`:313` and `:368`), and both are covered by ~14 unit tests in the same file. It mutates `[tools]` bindings only — no `[env]` path exists.
- `ocx add` offers no escape hatch: `git grep env crates/ocx_cli/src/command/add.rs` returns only two comment hits, no `--env` flag.

**Remaining scope**

- `ocx env set KEY[:TYPE[:SEP]]=VALUE [--group NAME]` and `ocx env unset KEY [--group NAME]`, writing `[env]` / `[group.<name>.env]` in `ocx.toml`. Extend the proven `add_binding` / `remove_binding` + `render_preserving` pattern in `crates/ocx_lib/src/project/mutate.rs`, under the same exclusive project lock.
- `ocx config set KEY VALUE [--global|--system]`. No equivalent mutation path exists for `config.toml` — this half is new, though small: single-key read-modify-write.
- Validate at write time with ocx's exit codes: separator agreement for `list`, the `OCX_`/`__OCX_` key rejection that `[env]` parsing already enforces, and tier gates.
- Settle the three-issue overlap first — see #328 and #329, which the on-main triage already records as one cluster.

#### [#310](https://github.com/ocx-sh/ocx/issues/310) update notice

- **Labels**: —
- **Verdict**: NOT_STARTED · **Gate**: none (owns toolchain drift notice; #42 keeps cache unification)
- **Size**: S-M once scoped — the throttle, state-file and probe machinery all exist and are identifier-generic; the work is wiring them over the lock's bindings plus a report shape.
- **Importance**: medium — genuine UX value, nobody blocked, no labels on the issue.
- **Depends on / blocks**: overlaps [#42](https://github.com/ocx-sh/ocx/issues/42) (unified freshness/update strategy); one must be closed in favor of the other before either is implemented.
- **Pass-1 → final**: IMPLEMENTED (close candidate) → **OVERTURNED**. Pass-1 answered a different question: every citation it gives is about updates to **the ocx binary itself**, while the issue asks about advisory tags in **toolchains** (the project's tools).

**Evidence**

- The issue's one sentence reads *"query for updates on **advisory tags in toolchains** automatically (in intervals)"*. "Advisory tag" is established ocx vocabulary for a floating tag on a toolchain binding, not for the CLI's own release stream: `crates/ocx_cli/src/command/update.rs` documents `--check` exit 65 as *"an advisory tag moved upstream"*, and `.claude/rules/subsystem-cli-commands.md:71` describes `ocx update` as *"Re-resolve advisory tags in lock against the LIVE registry"*.
- Everything pass-1 cited is self-scoped. `crates/ocx_cli/src/app/update_check.rs::check_for_update` is documented *"Checks the remote registry for a newer **OCX version**"*; it calls `self_check_update`, whose own doc says it *"Resolves the well-known OCX CLI identifier (`ocx.sh/ocx/cli`) and compares the latest tag against the version reported by the currently installed `ocx` binary"*. `OCX_UPDATE_CHECK_INTERVAL` and `OCX_NO_UPDATE_CHECK` throttle that one check. `ocx self update --check` checks that one package. None of it looks at `ocx.toml`.
- Confirmed the generic primitive is not wired to toolchains: `PackageManager::check_update` takes an arbitrary identifier, but `git grep check_update` shows only two production callers — `self_check_update` and `crates/ocx_lib/src/setup/bootstrap.rs:134`, both on the ocx-CLI identifier.
- No toolchain-side notice exists. `crates/ocx_cli/src/command/status.rs` is explicitly offline: *"No network, no advisory lock, no staleness gate, no object-store probe."* `git grep -in "update available\|newer version"` over the project module, `status.rs` and `lock.rs` returns nothing.
- **Partially covered by an unrelated mechanism.** `ocx update --check` is a real dry-run over advisory tags: `update.rs` re-resolves the selected scope and exits 0 (unchanged) or 65 (a pin would move), writing nothing. That answers the issue's *"cli command to check for it"* and *"maybe update --dry-run?"*.
- **The data-fields half is not covered.** The `--check` branch returns `Ok(ExitCode::SUCCESS)` before reaching `context.api().report(&report)`, so the check path emits an exit code and no payload. There is no way to learn *which* bindings would move, which is the issue's *"extra data fields indicating whether an update is available"*.
- **The owner already triaged this the same way, on main.** `.claude/artifacts/analysis_issue_triage_2026-08-29.md:155` records #310 as **DECISION** — *"informally worded; two of its asks are already met by unrelated mechanisms. Needs scoping into a real ask or closing"* — and `:240` flags *"[#42] ↔ [#310] both claim the locked-tag drift notice. Pick an owner."* [#42](https://github.com/ocx-sh/ocx/issues/42) is still OPEN.

**Remaining scope**

- Decide first whether this issue or [#42](https://github.com/ocx-sh/ocx/issues/42) owns the locked-tag drift notice, and close the loser. Both currently claim it.
- Then scope what remains, given `ocx update --check` already provides the manual dry-run:
- An automatic, interval-throttled check that advisory tags in `ocx.toml` have moved, printing a notice. Nothing like this exists for toolchains; the existing interval machinery in `crates/ocx_lib/src/package_manager/tasks/update_check.rs` is self-only but its `check_update` primitive already takes an arbitrary identifier.
- Machine-readable output on the `ocx update --check` path naming which bindings would move. The check branch currently returns before emitting any report, so `--format json` yields nothing there.
- An opt-in/out control for the toolchain check, matching `OCX_NO_UPDATE_CHECK` / `OCX_UPDATE_CHECK_INTERVAL`.
- Do **not** close this as done against `ocx self update --check`; that command checks the ocx binary, not the project's tools.

#### [#50](https://github.com/ocx-sh/ocx/issues/50) policy-based retention for orphan blobs

- **Labels**: area/file-structure
- **Verdict**: NOT_STARTED · startable now
- **Size**: M — down from pass 1's L. The prior-art survey is already done
  (`.claude/artifacts/research_blob_retention_policy.md`, with a stated
  recommendation), the sidecar convention exists, and the change is one subsystem
  (`file_structure` + `tasks/garbage_collection.rs`) plus two flags and a config
  table. Matches the owner's 2026-08-29 snapshot (MEDIUM), which pass 1's L
  contradicted without saying why.
- **Importance**: medium — real shared-CI-runner cost saving, not correctness.
- **Depends on / blocks**: depends on #35 (landed, verified above). Same code area
  as #53; if both are scheduled, do #53 first — it is far smaller and touches the
  function #50 will rewrite around.
- **Pass-1 → final**: NOT_STARTED → **AGREE on verdict, OVERTURNED on size**
  (L → M: the design is pre-surveyed, the sidecar's home is an established
  convention, and the surface is two flags plus one config table)

**Evidence**

- `crates/ocx_cli/src/command/clean.rs:25-40` — I read the whole file. `Clean`
 has exactly two fields, `dry_run` and `force`. No `--max-age`, no `--max-size`.
- `crates/ocx_lib/src/config.rs:78-159` — I read the whole `Config` struct.
 Seven fields: `registry`, `registries`, `mirrors`, `patches`, `managed`,
 `trust`, `shell`. No `retention`.
- **Strongest corroboration, which pass 1 did not cite**:
 `crates/ocx_lib/src/package_manager/tasks/garbage_collection.rs:121-122` says in
 a doc comment "Follow-up #50 tracks policy-based retention for users who want
 stricter retention semantics in shared `$OCX_HOME` scenarios." The code itself
 names this issue as unbuilt follow-up work.
- No last-used tracking and no auto-GC anywhere:
 `git grep -i -e auto.gc -e auto_gc -e last_used -e last-used -- crates` is empty.
- Prerequisite #35 landed — `garbage_collection.rs` tests at `:285, 300, 328, 355,
 369, 382, 468` construct `CasTier::Blob` orphans and expect them collected, and
 no tier-skip guard remains. Independently corroborated by
 `analysis_issue_triage_2026-08-29.md:102` ("prerequisite #35 closed, so unblocked").
- **Trap an implementer must not fall into**: `ocx clean` has *already grown a
 different retention mechanism* since this issue was filed — the per-user project
 registry (`$OCX_HOME/projects.json`), which retains packages pinned by any
 registered project's `ocx.lock` unless `--force` is passed
 (`command/clean.rs:20-24` doc comment, and `adr_clean_project_backlinks.md`).
 That is project-backlink retention, not blob TTL retention. Two policies, one
 command; the interaction between `--force` and a future `--max-age` needs stating.
- The sidecar has an obvious established home: `state_store.rs` already hosts
 `update_check_dir` (`:73`), `managed_config_dir` (`:113`),
 `host_capabilities_file` (`:209`) and `tuf_cache_dir` (`:226`). A last-used index
 belongs there, not in a new tree.

**Remaining scope**

- Last-used timestamp tracking for orphan blobs via a sidecar index under
 `$OCX_HOME/state/` (follow the four existing `state_store` accessors). Not
 filesystem atime — unreliable under `relatime`, Docker volumes and NFS.
- `ocx clean --max-age <duration>` and `--max-size <bytes>`, plus a `[retention]`
 table in `config.toml` for the defaults.
- Hybrid eviction: TTL first, then size-cap oldest-first on what survives.
- State how `--max-age`/`--max-size` compose with the existing `--force`
 project-registry bypass. Two retention policies now share this command.
- End-of-command auto-GC with a daily throttle, skipped under `--offline`. The
 throttle can reuse whatever #42 extracts; if #42 has not landed, follow the
 `state_store` mtime-file pattern already used by the update check.
- Acceptance criteria are already written in the issue and are testable as-is —
 notably `--max-age 0s --max-size 0` must reproduce the post-#35 baseline exactly.

#### [#283](https://github.com/ocx-sh/ocx/issues/283) ocx_lib: support bzip2 tarballs (.tar.bz2) in CompressionAlgorithm

- **Labels**: —
- **Verdict**: NOT_STARTED · **Gate**: read-side only vs full parity (impl-level)
- **Size**: M — four files minimum plus a vetted new dependency and a static-linking constraint;
  the zstd precedent is the ceiling at 16 files if layer-format parity is chosen.
- **Importance**: medium — the only concrete consumer is the `mozilla/grcov` mirror spec, which is
  authored and waiting; no core path is affected.
- **Depends on / blocks**: independent of #284 and #288. Blocks the grcov mirror spec in
  `ocx-sh/ocx-mirror`.
- **Pass-1 → final**: NOT_STARTED → **AGREE on the verdict, OVERTURNED on size** (S → M): pass 1
  costed it as "one file plus a Cargo.toml bump", missing four exhaustive matches outside
  `compression.rs`, a static-linking constraint pinned by an existing test, and an interface
  question about whether a bzip2 **layer** media type is in scope.

**Evidence**

- `crates/ocx_lib/src/compression.rs:10-14` — the enum is still `{None, Lzma, Gzip, Zstd}`.
 `from_file` (`:19-27`) and `from_media_type` (`:30-37`) have no bzip2 arm; `Cargo.lock` contains
 no `bzip2` package.
- **Adding a variant ripples into four exhaustive matches, not one**: `Display`
 (`compression.rs:43-46`), the compress path `write_file` (`:239-275`), the decompress path
 `read_file` (`:304-334`), and the streaming-pull pipeline
 `crates/ocx_lib/src/oci/client.rs:1173-1233`, which matches every variant and returns
 `InvalidManifest` for `None`.
- **A static-linking rule already names bzip2 and would red on a naive dependency add.**
 `crates/ocx_cli/tests/linux_self_contained.rs:43-49` bans `libbz2` (alongside `liblzma`,
 `libzstd`, `libz.so`) from the shipped binary's `NEEDED` list. The `bzip2` crate must therefore
 be taken with a vendored/pure-Rust backend, which is a `deps`-skill decision, not a line in
 `Cargo.toml`.
- **Precedent measured, not estimated.** The identical task for zstd shipped as
 [`0fa616b1`](https://github.com/ocx-sh/ocx/commit/0fa616b1) *"feat: support tar+zstd layer
 format (#58)"* — **16 files, +380/−27**, spanning `compression.rs`, `media_type.rs`,
 `oci/client.rs`, `publisher/layer_ref.rs`, `package/bundle.rs`, two CLI commands, an acceptance
 test (`test/tests/test_multi_layer.py`) and three docs pages. This was already flagged in
 `.claude/artifacts/analysis_issue_triage_2026-08-29.md:205`.
- **Scope question the issue does not settle:** zstd's 16 files are mostly the *publish* side.
 `crates/ocx_lib/src/media_type.rs:15-19` defines only gz/xz/zstd layer media types, so a
 `from_media_type` arm for bzip2 implies a new `MEDIA_TYPE_TAR_BZ2` — a wire-format addition
 (CLAUDE.md "interfaces" tier), whereas the grcov blocker needs only the *read* side.
- The misleading-error half is unaddressed: `from_file` returns `None` for an unknown extension
 and `Archive::extract_with_options` (`crates/ocx_lib/src/archive.rs:113-128`) then hands the raw
 stream to `tar::extract`, which is exactly the `cksum` error the issue quotes.

**Remaining scope**

- Decide first: read-side only (unblocks the mirror; no new media type, and the `client.rs` match
 arm errors like `None` does) versus full parity with zstd including a `MEDIA_TYPE_TAR_BZ2` layer
 format. Only the second is a wire-format change.
- Add the `bzip2` dependency through the `deps` skill, choosing a vendored or pure-Rust backend
 so `crates/ocx_cli/tests/linux_self_contained.rs` stays green with no `libbz2` NEEDED entry.
- Add the `Bzip2` variant plus `from_file` extension arms (`bz2`, `tbz2`, `tbz`) and, if the
 read-side-only decision is reversed, a `from_media_type` arm and the matching constant in
 `crates/ocx_lib/src/media_type.rs`.
- Fill the four exhaustive matches: `Display`, `write_file`, `read_file`, and
 `crates/ocx_lib/src/oci/client.rs:1173`.
- Unit test mirroring the zstd round-trip at `crates/ocx_lib/src/compression.rs:379-411`
 (`round_trip_zstd`, driven by `round_trip_zstd_single_thread` / `_multi_thread`).
- Optional and shared with #284: magic-sniff a known-but-unsupported compressor at the tar
 boundary and fail with "unsupported compression: bzip2" instead of the cksum error.

#### [#211](https://github.com/ocx-sh/ocx/issues/211) `ocx package create`: pin dependencies from the project `ocx.lock`

- **Labels**: —
- **Verdict**: NOT_STARTED · **Gate**: impl-level questions (match key, any-target gate)
- **Size**: M — one new pin-source function, one CLI flag, project resolution on a command that has none, plus the ADR amendment. One subsystem.
- **Importance**: medium — publisher convenience and consistency (inherit the project's tested pins instead of re-resolving); blocks nothing.
- **Depends on / blocks**: none.
- **Pass-1 → final**: NEEDS_DECISION → **OVERTURNED**. Pass 1's central evidence bullet is false — `AuthoringDependency.platforms` no longer exists, it is a rejection sentinel that hard-errors — so the issue's stated premise is stale, and the open questions it lists are implementation design, not owner-only policy.

**Evidence**

- **The false citation.** Pass 1 wrote: "`LockedTool.platforms` ... is structurally the same per-platform digest map as `AuthoringDependency.platforms` — confirms the issue's technical premise". `crates/ocx_lib/src/package/metadata/authoring/dependency.rs:46-57` shows the field is now `retired_platforms: ()`, serde-renamed to `platforms`, `skip_serializing`, with a custom deserializer whose only job is to fail: "the dependency `platforms` pin map is no longer supported; a dependency now carries its manifest digest directly on the identifier". Retired by `8e6eb3ea` (2026-08-05, `feat(package)!: build receipt replaces the recorded-platform metadata`), which postdates the issue (2026-07-10).
- The lock side of the comparison is real: `LockedTool.platforms: BTreeMap<String, Digest>` at `crates/ocx_lib/src/project/lock.rs:162-176`. The issue's own name for it, `LockedTool.resolution: PerPlatform`, is also stale.
- So the shapes no longer match. A dependency now holds one digest on `AuthoringDependency.identifier` (`dependency.rs:34`), and a bundle targets exactly one platform per `create` invocation — `dependency_pinning.rs:18-20`, citing `adr_platform_model_unification.md` D5. The feature is a single-key lookup into the lock's map, not a map copy.
- Current pinning is index-only, confirmed end to end: `crates/ocx_lib/src/package/dependency_pinning.rs:1-16` and `pin_dependencies` at `:59` resolve every unpinned dependency through `Index::fetch_candidates` with `IndexOperation::Resolve`. Tested by `test/tests/test_package_create_pinning.py`.
- `ocx package create` has no project awareness whatsoever. Its full flag set (`crates/ocx_cli/src/command/package_create.rs:14-89`) is `--identifier`, `--platform`, `--output`, `--force`, `--metadata`, `--compression-level`, `--threads`, `--bin-scan`, `--no-libc-lint`. No `--project`, no lock read, and `git grep` for `pin-from-lock` / `pin_from_lock` / `from_lock` returns nothing outside an unrelated test name in `toolchain_env.rs:798`.
- **A constraint pass 1 missed.** `reject_digest_pins_in_any_target` (`dependency_pinning.rs:108`, called at `:66`) refuses an `any`-targeted bundle carrying any pre-existing digest pin, because a digest create did not write itself carries no evidence of being `any`-offered. A lock-sourced pin is by definition a digest create did not write, so `--pin-from-lock` collides with this gate head-on for `any` targets. Same rule is referenced from the push gate at `crates/ocx_lib/src/publisher/publish_gate.rs:341`.

**Remaining scope**

- Give `create` a way to find a project: a `--project` / `OCX_PROJECT`-style resolution, since it currently takes only a content-tree path.
- Add a lock-sourced pin path beside `pin_dependencies` in `dependency_pinning.rs` that, for the invocation's single `--platform`, looks up `LockedTool.platforms[<canonical platform string>]` and attaches that one digest to the dependency's identifier.
- Settle the match key between a metadata dependency and a lock binding: `(registry, repository)` equality, and what `-g` group scoping means here.
- Settle duplicate-repository-across-groups ambiguity.
- Settle tag mismatch: metadata says `java:21`, lock pinned `java:22` — error or pin.
- Settle whether the `--global` lock is a valid source.
- **New, from the gate above**: settle what `--pin-from-lock` does under `--platform any`, where `reject_digest_pins_in_any_target` currently refuses exactly this class of digest. Either the flag is refused for `any` targets, or the gate needs a documented exemption for lock-sourced pins.
- Settle what happens when a dependency is absent from the lock, and when the lock has no leaf for the target platform.
- Amend `adr_dependency_manifest_pinning.md` with the second pin source, add pytest coverage beside `test/tests/test_package_create_pinning.py`, and document the flag in `website/src/docs/reference/command-line.md`.
- Correct the issue body: the `platforms` map it proposes reusing was retired in `8e6eb3ea`.

### Batch 4 — SBOM / provenance milestone (#199) + plugin env scrub

#### [#104](https://github.com/ocx-sh/ocx/issues/104) OSV vulnerability scan on install (cargo-auditable + OSV.dev)

- **Labels**: security, area/package-manager
- **Verdict**: NOT_STARTED · startable now
- **Size**: **L** — adjusted up from pass 1's M. It is not one subsystem: two new crate dependencies, a new outbound network client, a new exit-code slot with envelope plumbing, an install-pipeline hook, two reference pages, and an acceptance suite with a stub server.
- **Importance**: high — security-relevant, dependency-free, and the only day-1-startable item in #199.
- **Depends on / blocks**: none. Coordinate the post-extract hook with the existing pre-download verify seam.
- **Pass-1 → final**: NOT_STARTED → **AGREE**, and the exit-code correction is right.

**Evidence**

- Searched `crates/`, `test/`, `website/src/docs/` and every `Cargo.toml` for `osv.dev`, `querybatch`, `no-vuln-check`, `NO_VULN_CHECK`, `dep-v0`, `auditable-info`. Exactly one hit repo-wide: `Cargo.toml:34-36`, a comment on `[profile.dist]` noting that `strip = "symbols"` leaves cargo-auditable's `.dep-v0` section intact. That is a forward-looking note, not an implementation and not a decision record.
- `git log origin/main -S"querybatch"` and `-S"OCX_NO_VULN_CHECK"` return only `chore(claude):` AI-config commits (`d91cbfa3`, `72a69781`) — the plan artifacts, no code.
- Exit code 85 is taken: `crates/ocx_lib/src/cli/exit_code.rs:93` declares `UnsupportedKeyBackend = 85`. Occupied slots run 74–85 contiguously from 74 (`IoError`) except 76; the next free slot is **86**. The issue's 2026-08-20 revision naming 85 is stale.
- No `test/tests/test_osv_scan.py`; no vuln- or scan-named test module in `test/tests/`.
- Note for the implementer: OCX's own release profile does not build with cargo-auditable, so the fixture binary must be produced deliberately.

**Remaining scope**

- Post-extract `.dep-v0` parse via the `auditable-info` crate (add through the deps workflow).
- Native OSV.dev `/v1/querybatch` client in `ocx_lib` — no `osv-scanner` shell-out.
- Client-side CVSS scoring with the `cvss` crate; prefer the v4 vector when both v3.1 and v4 are present; CRITICAL at base score >= 9.0; no severity vector → UNKNOWN, never blocks.
- Policy: CRITICAL fails the install with a dedicated exit code, HIGH and below warn only. Never prompt.
- **Allocate exit code 86, not 85** — 85 is `UnsupportedKeyBackend`. Add the variant, its `--format json` slug, and an `error_envelope` round-trip case.
- `--no-vuln-check` flag plus `OCX_NO_VULN_CHECK`; `OCX_OFFLINE=1` skips with one WARN; a binary with no `.dep-v0` emits exactly one INFO line.
- Hook placement: post-extract, distinct from the pre-download signature-verify seam.
- Docs: `website/src/docs/reference/environment.md` (`OCX_NO_VULN_CHECK`), `website/src/docs/reference/command-line.md` (the flag and the new exit code).
- `test/tests/test_osv_scan.py` against a local querybatch stub, covering every acceptance row.

#### [#108](https://github.com/ocx-sh/ocx/issues/108) Publisher CI guidance: provenance + SBOM workflows

- **Labels**: security, area/website
- **Verdict**: NOT_STARTED · **Gate**: none (docs)
- **Size**: M — three docs pages, one subsystem, no code; the reusable examples already exist in-repo.
- **Importance**: medium — publisher onboarding, blocks no code.
- **Depends on / blocks**: **nothing.** #100 shipped; #102 is not a dependency once the guide targets the general verb.
- **Pass-1 → final**: NOT_STARTED → **AGREE on the verdict, OVERTURNED on the blocking claim.** Pass 1
  says the tracker's "unblocked" sweep "overstated readiness" and that the guide "can't be
  copy-paste-runnable for provenance until #102 lands". The tracker is right and pass 1 is wrong:
  the guide's own 2026-08-20 revision already fixes the spelling as `ocx package attest --predicate
  FILE --type TYPE` being the general verb, and that verb ships. Nothing blocks this.

**Evidence**

- `website/src/docs/guides/` does not exist — `ls` errors, and `git ls-files website/src/docs/guides` is empty. None of the three named pages exist under any other directory.
- `website/src/docs/in-depth/ci.md` is consumer-side, not publisher-side: its sections are "Toolchain-tier example" (`:29`) and "OCI-tier example" (`:71`), both about installing OCX and running `ocx package env` in CI. Same for the GitLab half at `:98-152`. No publish, no attest, no SBOM.
- `grep -rn attest-build-provenance website/` → zero. The action appears only in the repo's own workflows.
- The general verb is fully documented already: `website/src/docs/user-guide/attestations.md` (154 lines, with an alias table at `:100-117` and a verification-floor section at `:120`), plus `website/src/docs/in-depth/signing.md:264` on. What is missing is specifically the GitHub Actions walkthrough.
- **Material the guide can reuse, already in-repo and already SHA-pinned**: `.github/workflows/docker-publish.yml:205-210` runs `actions/attest-build-provenance@a2bbfa25375fe432b6a289bc6b6cd05ecd0c4c32 # v2.2.3` with `push-to-registry: true` against GHCR; `.github/workflows/build-windows-shims.yml:237-240` uses the same pinned SHA for a file subject. `.github/workflows/deploy-website.yml:122` already carries the `cargo cyclonedx --format json` recipe the SBOM guide needs.
- **New finding — an existing docs page violates the criterion this issue sets**: `website/src/docs/in-depth/ci.md:79` uses `actions/checkout@v4`, unpinned, and the GitLab examples follow the same style. #108's rule ("every example pins all `uses:` refs to commit SHAs") should be applied to `ci.md` in the same pass, or the docs contradict themselves page to page.

**Remaining scope**

- Write the three pages under `website/src/docs/in-depth/` (not `guides/` — that directory was never created and the shipped precedent is `signing.md` / `self-hosted-sigstore.md`): publishing with provenance, publishing with SLSA L3, publishing with an SBOM.
- Write the provenance guide against `ocx package attest --predicate FILE --type slsaprovenance1` — the shipped spelling. Do not wait for #102's `push --provenance` sugar. Note that `slsaprovenance` and `slsaprovenance02` are refused at exit 64.
- Cover `actions/attest-build-provenance` with `push-to-registry: true`, reusing the pinned SHA already in `docker-publish.yml:206`.
- Present the GitHub artifact-attestations reusable-workflow path for SLSA L3; reference `slsa-github-generator` only as superseded.
- SBOM guide: `cargo cyclonedx --format json` → `ocx package push --sbom`. Do not promise CycloneDX 1.6+ generation; cargo-cyclonedx emits <= 1.5.
- SHA-pin every `uses:` ref, and fix the unpinned refs already in `in-depth/ci.md`.
- Link the new pages from the user-guide index; run `task website:build` and the lychee check; review against `docs-style.md`.

#### [#109](https://github.com/ocx-sh/ocx/issues/109) Threat model + 2024-2026 incident references

- **Labels**: security, area/website
- **Verdict**: NOT_STARTED · **Gate**: body items 1+4 false; fix before writing (docs)
- **Size**: M — adjusted up from pass 1's S/M. The page itself is small, but it needs an issue-body correction first, then primary-source date verification for five incidents.
- **Importance**: medium — capstone documentation; blocks no code, but the two false premises will propagate into the page if nobody corrects them first.
- **Depends on / blocks**: unblocked. All prerequisites (#100, #101, #103, #198, the trust-policy schema) are shipped.
- **Pass-1 → final**: NOT_STARTED → **AGREE on the verdict, OVERTURNED on completeness.** Pass 1
  reproduced the issue's task list verbatim as remaining scope without checking whether its premises
  still hold. Two do not. Pass 1 also mis-stated the `reference/` listing (it names a
  `reference/dependencies.md`; the tracked reference pages are `command-line.md`,
  `configuration.md`, `env-composition.md`, `environment.md`, `metadata.md`, `platforms.md`,
  `script-host-api.md` — `dependencies.md` there is generated at deploy time, not committed).

**Evidence**

- `website/src/docs/reference/threat-model.md` does not exist; `git ls-files website/src/docs` lists 40 files and none is a threat-model page. The only "threat" occurrence in the whole docs tree is `in-depth/signing.md:71`.
- **Item 1 is now wrong.** The issue asks the shipped-defense table to claim "referrers-only verify fails hard on non-referrers registries — no tag fallback". Reversed: `.claude/artifacts/adr_oci_referrers_signing_v1.md`, "Amendment 10 — S1-F reversed: OCX reads and writes the referrers fallback tag (2026-08-29)" states S1-F "is reversed", that OCX "now reads the OCI referrers tag-schema index … and appends its own referrer descriptor to it", and names GHCR and Docker Hub explicitly.
- **Item 4 is now wrong.** The issue asks the page to document that "no tag fallback will be added". The fallback is shipped and documented: `website/src/docs/in-depth/signing.md:58-62` describes writing through whichever shape the registry offers, `:69` lists GHCR and Docker Hub under "Fallback index", and `:71` already frames it as "a threat-model difference rather than a compatibility one: the fallback index is an ordinary mutable tag that anyone with push access to the repository can rewrite". That sentence is the real threat row and should be lifted into the page.
- **Item 4's other half is already done elsewhere.** The cosign >= 3.0 verification floor is documented at `website/src/docs/in-depth/signing.md:92` ("cosign 3.0 or newer is required, and pre-3.0 compatibility is deliberately not offered") and cross-linked at `:329`. The new page should link rather than restate.
- **Item 5's prerequisite is met, with a different spelling than the issue assumes.** The `[[trust.policy]]` schema is frozen and documented at `website/src/docs/reference/configuration.md:583-650`: `scope`, `builder` and `signers` sit directly on `[[trust.policy]]`, and keyless matchers live in `signers` entries tagged `kind = "keyless"` — not under a `[trust.policy.keyless]` table as #199's 2026-08-20 revision projected.
- No incident is cited anywhere in the docs tree. The only in-repo mentions are `.claude/artifacts/plan_milestone_split_supply_chain.md:81` (recording the GhostAction = Sep 2025 criterion) and `research_publish_to_bcr_anatomy.md:466` (CVE-2024-3094 in an unrelated context).

**Remaining scope**

- **First, correct the issue body**: item 1 and item 4 both assert "no tag fallback", which ADR Amendment 10 reversed on 2026-08-29. The page must describe the fallback index as shipped and describe its weaker threat model, not claim it was refused.
- Write `website/src/docs/reference/threat-model.md`, ~800 words, one page.
- Threat table separating shipped from planned defenses. Shipped rows available today: digest pinning in `ocx.lock`; keyless signature verify with identity pinning; SLSA builder pinning (`[[trust.policy]].builder`); SBOM attach and discovery; DSSE attestation verify. Planned: OSV scan (#104).
- A row for the fallback-index trade-off, sourced from `in-depth/signing.md:71`: a registry-served Referrers response cannot be rewritten by a pusher; a `sha256-<digest>` fallback tag can.
- Out-of-scope threats: maintainer social engineering, registry-side denial of service, and the freshness gap already stated at `user-guide/attestations.md:126` (no rollback protection against an older, still-validly-signed attestation).
- Incident citations with dates verified against primary sources: xz CVE-2024-3094, Ultralytics Dec 2024, Solana web3.js Dec 2024, Shai-Hulud npm worm Sep 2025, GhostAction Sep 2025 (not Jan).
- Link the cosign >= 3.0 floor at `in-depth/signing.md:92` rather than restating it.
- Write the `[trust.policy]` section against the shipped `signers` / `kind` schema at `reference/configuration.md:583-650`.
- Link from the user-guide index; lychee link check green.

#### [#200](https://github.com/ocx-sh/ocx/issues/200) Dogfood: attach OCX's own SBOM on release publish

- **Labels**: security, area/oci
- **Verdict**: NOT_STARTED · **Gate**: signed vs unsigned; index vs platform subject
- **Size**: **S** — adjusted up from pass 1's XS. Still workflow-only, but it is three workflow files (SBOM-format change, attach step, permissions grant) plus two decisions, not a one-line addition.
- **Importance**: **medium** — adjusted up from pass 1's low. It is now the cheapest remaining item in #199 and the only one that exercises the milestone end to end in a real pipeline; it also carries a live workflow bug.
- **Depends on / blocks**: depends on #100 (shipped). **No longer depends on any registry migration.** Blocks nothing.
- **Pass-1 → final**: "NOT_STARTED (blocked on GHCR)" → **OVERTURNED on the blocker.** Pass 1 called
  the blocker "real and independently documented in-repo" and cited
  `.claude/artifacts/research_oci_referrers_2026.md:9` and `review_r1_slice2_researcher.md:9,56`.
  Both predate the design reversal, and neither is the shipped code. The blocker no longer holds.

**Evidence**

- **The decision that removes the blocker**: `.claude/artifacts/adr_oci_referrers_signing_v1.md`, "Amendment 10 — S1-F reversed: OCX reads and writes the referrers fallback tag (2026-08-29)". Its reasoning names the exact registries: "GHCR and Docker Hub — the two adoption targets S1-F named — still do not serve it, and cosign interoperates with them through this exact tag. Refusing it no longer pressures anyone." Dated nine days after the issue's "Blocked (2026-08-20)" note.
- **The shipped write path**: `crates/ocx_lib/src/oci/verify/pipeline.rs:2503-2545` — "The Unsupported verdict no longer refuses the operation: the OCI referrers tag-schema fallback (`list_referrers_with_fallback` / `append_referrer_fallback_index`) serves a registry without the Referrers API. See `adr_oci_referrers_signing_v1.md`, Amendment 10."
- **Proven end to end against a registry with no Referrers API**: `test_attest_lands_in_the_fallback_index_on_a_registry_without_the_referrers_api` (`test/tests/test_attest.py:965-1010`) attests on a `registry:2` fixture, asserts the attest exits 0, asserts the Referrers API returns **404** so the fallback write is the only thing that could have carried it, then fetches the `sha256-<digest>` fallback tag and finds the referrer descriptor in it.
- **The read path is the same one `ocx package sbom` uses**: `sbom_one` (`package_manager/tasks/sbom.rs:114-166`) builds a `VerifyContext` with `VerifyContentMode::Attestation` and calls `VerifyPipeline::run_attestations`, whose listing goes through `list_referrers_with_fallback` (`verify/pipeline.rs:2540`, also `verify/candidates.rs:178`).
- **Documented as supported, with GHCR named**: `website/src/docs/in-depth/signing.md:58-62` and `:69` — GHCR and Docker Hub listed under "Fallback index"; "Signing succeeds either way, and `ocx package verify` reads both shapes with no flag."
- Publish target confirmed unchanged: `.github/workflows/post-release-oci-publish.yml:44-56` — `registry: ghcr.io`, `physical_namespace: ocx-sh`, logical name `ocx.sh/ocx/cli`. The push itself is a per-platform loop at `.github/workflows/oci-publish.yml:344-355`.
- **Two concrete gaps pass 1 missed in the workflow it cited.** (a) The release pipeline generates CycloneDX **XML** only — `.github/workflows/release.yml:218-229` runs bare `cargo cyclonedx -v` and collects `*.cdx.xml`. `attest` requires a JSON predicate (`test/tests/test_attest.py:382`, "refuses a predicate that is not JSON"), so an XML SBOM cannot be attached as-is. The JSON recipe already exists at `.github/workflows/deploy-website.yml:122` (`cargo cyclonedx --format json --manifest-path crates/ocx_cli/Cargo.toml`). (b) `.github/workflows/release.yml:237` reads `${{ steps.cargo-cyclonedx.output.paths }}` — singular `output`. The GitHub Actions context is `steps.<id>.outputs.<name>`, so this expression is empty and the `.cdx.xml` files are not uploaded at all. Pass 1's claim that release.yml produces "GitHub Release `.cdx.xml` assets" does not hold in effect.

**Remaining scope**

- Generate a JSON CycloneDX SBOM in the publish path — reuse `cargo cyclonedx --format json --manifest-path crates/ocx_cli/Cargo.toml` from `deploy-website.yml:122`. The existing `.cdx.xml` output cannot be attested.
- Attach it in `.github/workflows/oci-publish.yml` alongside the per-platform `ocx package push` at `:349-355`, via `--sbom`.
- Decide signed vs unsigned. `post-release-oci-publish.yml:15-20` grants only `contents: read` and `packages: write`; a signed attach needs `id-token: write` added there and forwarded through `release.yml`'s grant. An unsigned CycloneDX attach works without it but is labeled `verified: false` and is refused under Demand mode (`user-guide/attestations.md:120`), which would undercut the point of the dogfood.
- Decide index vs platform subject. The push loop attaches per platform manifest; `ocx package sbom ocx.sh/ocx/cli` without `--platform` acts on whatever the tag resolves to, which is the index (`test_attest.py:1060`). Pick one and make the acceptance criterion match.
- Fix `release.yml:237` (`output` → `outputs`) while in the file, or the SBOM assets keep silently not uploading.
- Verify the read-back on GHCR through the fallback tag after the first release that carries it.
- Delete the "Blocked (2026-08-20)" section from the issue body.

#### [#393](https://github.com/ocx-sh/ocx/issues/393) plugins: OCX_AUTH_* and OCX_ANNOUNCE_TOKEN reach ocx-<name> processes unscrubbed

- **Labels**: security, area/cli, discussion-needed
- **Verdict**: NOT_STARTED · **Gate**: OCX_AUTH_ half ships now; OCX_ANNOUNCE_TOKEN half is owner call
- **Size**: S for everything except the owner call — two source files, one test, one rule file, one docs page, and the mechanism already exists and is being reused rather than invented.
- **Importance**: high — a registry bearer token and its basic-auth username reach every dispatched `ocx-<name>` process today, with no scrub and no test that could notice. Calibration: exploitation needs the user to have installed a third-party plugin, and the only plugin in the fleet today is first-party `ocx-mirror`, so this is exposure surface rather than an active incident.
- **Depends on / blocks**: the `OCX_ANNOUNCE_TOKEN` checkbox depends on an owner decision and then on `ocx-sh/ocx-mirror`. The `OCX_AUTH_` checkbox depends on nothing and should not wait for it.
- **Pass-1 → final**: NEEDS_DECISION → **OVERTURNED**. The larger, security-relevant half is fully specified by the issue itself and blocked on nothing; filing the whole issue as a decision would park an unblocked credential fix behind a question that only governs one of its two checkboxes.

**Evidence**

- `crates/ocx_lib/src/env.rs:238` — `pub const CREDENTIAL_KEYS: &[&str] = &[OCX_IDENTITY_TOKEN, OCX_KEY_PASSWORD, OCX_SIGNING_KEY];`. Neither `OCX_ANNOUNCE_TOKEN` nor any `OCX_AUTH_` form is a member. Confirmed by reading the constant, not by grep.
- `crates/ocx_lib/src/env.rs:213-227` — the "Known non-members" doc block records both gaps as open, verbatim as the issue describes: `OCX_ANNOUNCE_TOKEN` marked *"Open: a cross-repo decision, not an oversight"*, `OCX_AUTH_<slug>_TOKEN` marked *"Open: a gap in the mechanism, not a missing row"*.
- `crates/ocx_cli/src/app/plugin_dispatch.rs:192-194` — the scrub is `for credential in ocx_lib::env::keys::CREDENTIAL_KEYS { cmd.env_remove(credential); }`. There is no `env_clear()` anywhere in the file, and `cmd.envs(env)` only adds and overrides. **So every ambient variable reaches `ocx-<name>` except those exactly three names.** That is the precise answer to "which variables reach the plugin today".
- `crates/ocx_lib/src/script/ocx_module.rs:82` — `const CREDENTIAL_ENV_PREFIX: &str = "OCX_AUTH_"`, consumed by `is_reserved_env_key` at `:92-102` with a byte-boundary-safe, ASCII-case-insensitive prefix compare. The mechanism the issue points at exists and is correct; it is simply not reachable from `plugin_dispatch.rs`.
- The prefix covers the whole family, which matters: `crates/ocx_lib/src/auth.rs:123-125` builds `OCX_AUTH_{slug}_TYPE`, `_TOKEN` and `_USER`. `_USER` is the basic-auth username and leaks alongside the password today.
- `.claude/rules/subsystem-cli.md:322-324` — the second table ("Two variables meet the bar and are deliberately not on the list") carries both rows with the same known-open wording. The same file's closing paragraph mandates the three-edit checklist: this table, `CREDENTIAL_KEYS`, **and** `website/src/docs/reference/environment.md`.
- Docs are silent rather than wrong, which is worth knowing before writing the fix. `environment.md` carries an explicit **"Never forwarded to child processes … plugins (`ocx-<name>`) included"** paragraph for `OCX_IDENTITY_TOKEN` (~`:279`) and for `OCX_SIGNING_KEY` (~`:318`). The `OCX_ANNOUNCE_TOKEN` section (`:117-137`) and the three `OCX_AUTH_<REGISTRY>_*` sections (`:139-161`) make no forwarding claim at all. No published statement is falsified by the current behaviour; the fix has to *add* the statement, not correct one.
- **The existing test cannot catch this class.** `plugin_dispatch.rs::plugin_command_unsets_every_credential_key` (`:258-283`) loops over `CREDENTIAL_KEYS` itself and asserts each is removed. A prefix rule that regressed would leave that test green, because the loop's subject is the list. The fix needs a test naming a concrete variable, e.g. `OCX_AUTH_ghcr_io_TOKEN`.

**Remaining scope**

- **Ships now, blocked on nothing.** Move `CREDENTIAL_ENV_PREFIX = "OCX_AUTH_"` from `crates/ocx_lib/src/script/ocx_module.rs` to `crates/ocx_lib/src/env.rs`, beside `CREDENTIAL_KEYS`, and have `ocx_module.rs` consume it from there so there is one prefix constant, not two.
- Change `crates/ocx_cli/src/app/plugin_dispatch.rs` to scrub list ∪ prefix. `Command::env_remove` needs concrete names, so the prefix half must enumerate the parent's own environment and remove every key whose first eight bytes match `OCX_AUTH_` case-insensitively. This covers `_TYPE`, `_USER` and `_TOKEN`, all three of which `auth.rs::get_env_auth` reads.
- Add a test that names a literal `OCX_AUTH_<slug>_TOKEN` and a literal `OCX_AUTH_<slug>_USER` and asserts both are removed. Do not extend the existing `CREDENTIAL_KEYS` loop — it is structurally incapable of covering a prefix rule.
- Update `.claude/rules/subsystem-cli.md`: move the `OCX_AUTH_<slug>_TOKEN` row out of the "deliberately not on the list" table and document the list-plus-prefix convention in the main table.
- Add the "Never forwarded to child processes" paragraph to the `OCX_AUTH_<REGISTRY>_TYPE` / `_USER` / `_TOKEN` sections of `website/src/docs/reference/environment.md`, matching the wording already used for `OCX_IDENTITY_TOKEN`.
- **Owner call, and only this.** Whether `OCX_ANNOUNCE_TOKEN` joins the scrub set. Concretely: does `ocx-mirror`'s plugin-dispatched announce keep inheriting the parent's token, or does it read its own / require an explicit forwarding opt-in? If it is scrubbed, a matching change in `ocx-sh/ocx-mirror` has to land first or alongside. Nothing in the `OCX_AUTH_` work waits on this answer.

### Batch 5 — Larger refactors

#### [#42](https://github.com/ocx-sh/ocx/issues/42) feat: unified freshness/update check strategy with TTL caching

- **Labels**: area/package-manager
- **Verdict**: PARTIAL · startable now
- **Size**: L — up from pass 1's M. Four sub-features plus a migration of four
  existing call sites, two of which are security-adjacent (trust roots, referrer
  capability) and one of which is on the measured per-prompt hot path
  (`host_capabilities.rs:98` cites 15.4 ms of a per-prompt shell reconcile). That
  is not one subsystem and not one to two days.
- **Importance**: high — confirmed. It is the only issue in this batch with an
  owner-authored rescope, it is tied to registry rate-limit cost, and the
  duplication it targets is actively spreading.
- **Depends on / blocks**: relates to #41 (negative caching trigger); subsumes or
  is subsumed by #310 (owner picks).
- **Pass-1 → final**: PARTIAL → **AGREE on verdict, OVERTURNED on scope and size**
  (M → L: pass 1 counts the unification as four sub-features across the paths the
  issue named; three more hand-rolled TTL caches have landed since the issue was
  written, and the owner's 2026-08-29 revision predates two of them)

**Remaining scope**

- Extract one shared TTL-cache primitive and migrate the four existing hand-rolled
 instances onto it: `oci/host_capabilities.rs`, `oci/referrer/capability.rs`,
 `oci/verify/trust_cache.rs`, and the `state_store` update-check throttle. They
 already share a documented shape (atomic write, TTL-gated fail-open read,
 clamp-on-read, unusable-means-miss) — this is extraction of a real duplication,
 not a speculative abstraction.
- Swap `list_tags` + `find_latest_version` in
 `package_manager/tasks/update_check.rs:227-251` for a `fetch_manifest_digest`
 against a pinned tag. Single HEAD instead of full tag pagination.
- `--max-age` on the read paths for cached tags, and `--stale-only` on
 `ocx index update`.
- Negative cache for a tag confirmed absent, short TTL. Relates to #41.
- Locked-tag drift notice — **overlaps [#310](https://github.com/ocx-sh/ocx/issues/310)
 entirely**. #310 is a two-sentence stub; #42 has the worked framing. Fold #310
 into #42 or close it, but one owner, not two.
- Drop the dead reference in the issue body: `TagLock`/`TagStore`/`TagGuard` were
 deleted by `adr_index_indirection.md`. Only historical doc-comment analogies
 survive (`file_structure/locked_file.rs:285,368`;
 `package_manager/tasks/patch_discovery.rs:1307-1394`). Per-tag timestamps need a
 home under the local index collection (`index/{source}/`) instead.

#### [#313](https://github.com/ocx-sh/ocx/issues/313) sign↔verify module cycle blocks the planned ocx_lib crate split (ARCH-16)

- **Labels**: —
- **Verdict**: NOT_STARTED · **Gate**: say whether D-h (attest cycle) reopens
- **Size**: L, up from pass 1's M. Breaking sign↔verify requires relocating three independent shared vocabularies, not moving two functions; XL if the crate-split goal is scoped in, since that reopens D-h.
- **Importance**: medium — no runtime effect, purely a structural blocker for a stated architectural direction, with the "which module owns the bundle format" ambiguity accruing now.
- **Depends on / blocks**: touches the same modules as any future `ocx_lib` split. `.claude/artifacts/plan_pr339_issue_closeout.md:72,412` records a shell/project module move made specifically to avoid adding another such cycle, and names #313 and #324 as the reason — so #324 is the sibling to check before scoping this.
- **Pass-1 → final**: NOT_STARTED → **AGREE on the verdict, OVERTURNED on scope and size**. Pass 1 sized the narrow fix as "M for the sign↔verify edge alone (new `certificate.rs`, move two functions, update both import sites)". The two-function move does not break the sign↔verify cycle at all, independent of the attest question pass 1 raised.

**Evidence**

- Both edges the issue names are unchanged: `crates/ocx_lib/src/oci/sign/bundle.rs:33` imports `crate::oci::verify::identity::{oidc_issuer, subject_identity}`; `crates/ocx_lib/src/oci/verify/pipeline.rs:65-66` imports `crate::oci::sign::bundle::{MAX_BUNDLE_SIZE_BYTES, parse_bundle}` and `crate::oci::sign::{KeyBackendKind, SignatureFormat}`.
- No `oci/certificate.rs` exists — `git ls-files | grep -i certificate` returns nothing, and `ls crates/ocx_lib/src/oci/` confirms. The proposed seam was never built.
- **The overturn.** The cycle is far denser than the two edges the issue names. Production `sign → verify` references, all outside any `#[cfg(test)]` block (`sign/pipeline.rs` has one at `:728`, `simplesigning_write.rs` at `:484`): `sign/bundle.rs:33`, `sign/pipeline.rs:193`, `sign/pipeline.rs:198`, `sign/simplesigning_write.rs:50` (`use crate::oci::verify::SidecarKind`), `sign/simplesigning_write.rs:219` (`crate::oci::verify::sidecar_tag(subject, layer.kind)`). Production `verify → sign` references span seven files: `attestation_sidecar.rs`, `candidates.rs`, `dsse.rs`, `error.rs` (`VerifyErrorKind` embeds `sign::KeyRefError` at `:12`), `pipeline.rs`, `simplesigning_read.rs`, `tlog.rs`.
- The issue's proposed move covers only three of those five `sign → verify` references (`bundle.rs:33`, `pipeline.rs:193`, `pipeline.rs:198`). **The simplesigning sidecar-naming vocabulary is a second, entirely separate shared seam the issue does not mention**: `SidecarKind` and `sidecar_tag` both live in `crates/ocx_lib/src/oci/verify/simplesigning_read.rs` and are called from the sign writer.
- Pass 1's attest point is independently correct: `crates/ocx_lib/src/oci.rs:86-91` documents `attest ↔ verify` and `attest ↔ sign` as accepted, ADR-ratified cycles (D-h, `adr_sbom_attestations.md`), "not a design lapse".
- `ARCH-16` is confirmed a MUST rule in `.claude/rules/rust-quality/architecture.md`: "No module-level `use` cycle, even though one crate tolerates them — a crate split does not compile until every cycle is broken."

**Remaining scope**

- Move `subject_identity` and `oidc_issuer` to a neutral `crate::oci::certificate` that imports neither side, and update `sign/bundle.rs:33`, `sign/pipeline.rs:193`, `sign/pipeline.rs:198`. This is the part the issue describes, and it removes three of the five production `sign → verify` references.
- Decide where the simplesigning sidecar naming belongs. `SidecarKind` and `sidecar_tag` live on the verify side (`verify/simplesigning_read.rs`) but are called by the sign writer at `sign/simplesigning_write.rs:50,219`. Until they move to a neutral peer, `sign → verify` survives whatever happens to the identity functions.
- Decide who owns `TargetNotAnIndex`, which both `SignErrorKind` and `VerifyErrorKind` declare and whose slug equality is pinned by a cross-module test.
- Then the reverse direction, which the issue does not scope at all: seven `verify` files import `sign`, including `VerifyErrorKind` embedding `sign::KeyRefError`.
- Separately decide whether the crate-split precondition reopens D-h, since `attest` cycles with both `sign` and `verify` by ratified decision (`oci.rs:86-91`). If it does not, say so in the issue so nobody re-derives it.

#### [#214](https://github.com/ocx-sh/ocx/issues/214) Managed configuration option to always log digest when package is invoked

- **Labels**: —
- **Verdict**: NOT_STARTED · **Gate**: rebase PR #238; answer --exec-log follow-up
- **Size**: L — multi-subsystem (CLI flags, managed-config schema, launcher two-record semantics, docs), but de-risked: a working branch and reporter sign-off already exist, so landing is mostly integration.
- **Importance**: high — a named corporate user is blocked on it for a compliance workflow and has actively engaged three times.
- **Depends on / blocks**: nothing blocking. Independent of #326/#328 despite both being "config surfaces".
- **Pass-1 → final**: NOT_STARTED → **AGREE**

**Evidence**

- `git grep -l "OCX_RECORDS_DIR\|records-dir\|execution-record\|ExecutionRecord"` over `crates`, `website`, `test` returns only `test/manual/announce-e2e/README.md` and `test/src/announce_e2e/evidence.py` — both unrelated uses of the word "evidence/records" in the announce end-to-end harness. No records subsystem on `main`.
- The branch exists locally and on the remote: `feat/exec-resolution-record` / `origin/feat/exec-resolution-record`. It is carried by PR [#238](https://github.com/ocx-sh/ocx/pull/238), still OPEN.
- Re-derived the ask from the thread: the reporter's opening request was the managed-tier policy; the owner's 2026-07-16 comment split it into mechanism (per-invocation flag) then policy, and the 2026-07-28 comment describes both as built on that branch (`[records] dir/required`, `OCX_RECORDS_DIR`, `OCX_RECORDS_NAME`, `sh.ocx.execution-record`).
- The 2026-08-06 comment adds a **new** ask the branch does not answer: a user-facing single-file `--exec-log` / `--audit-log`. The owner's 2026-08-13 reply declines to decide it yet, naming the reason (one invocation writes several records because of nested environments and deferred tools, so a single file means either a lock or losing on-demand loads) and deferring 1-2 weeks.
- Latest comment (2026-09-03, the reporter): *"the design looks good from our end"* — that endorses the design as presented, and does not withdraw the `--exec-log` ask.

**Remaining scope**

- Land PR [#238](https://github.com/ocx-sh/ocx/pull/238) (or a rebased successor) on `main`; 17 files, docs (`website/src/docs/reference/execution-records.md`) and tests ride along on the branch.
- Answer the still-open follow-up from 2026-08-06: a single user-chosen `--exec-log FILE`. The owner's stated obstacle is that one invocation can emit several records, so the design must pick between file locking and recording only the initial call.
- The owner promised the reporter a reply "in 1-2 weeks" on 2026-08-13; that is three weeks overdue as of 2026-09-04, and the reporter pinged on 2026-09-03.

#### [#78](https://github.com/ocx-sh/ocx/issues/78) [entry-points-followup] Drop Deref&lt;Target=Metadata&gt; + From&lt;ValidMetadata&gt; on ValidMetadata — typestate escape

- **Labels**: tech-debt, entry-points-followup
- **Verdict**: NOT_STARTED · startable now
- **Size**: S–M, down from pass 1's M. The forwarding-accessor shape makes it mechanical once started; the only judgment call is which accessors to expose.
- **Importance**: low, down from pass 1's medium. `crates/ocx_lib/Cargo.toml:6` is `publish = false` and `CLAUDE.md` states internal structure has no stability, so no semver cost accrues. Every ingress path already calls `ValidMetadata::try_from`, so the escape hatch costs type-level expressiveness, not correctness, and no reported defect traces to it.
- **Depends on / blocks**: none.
- **Pass-1 → final**: NOT_STARTED → **AGREE** (evidence confirmed; size and importance adjusted)

**Evidence**

- `crates/ocx_lib/src/package/metadata/validation.rs:133` — `impl From<ValidMetadata> for Metadata` present and unchanged. `:139` — `impl std::ops::Deref for ValidMetadata`, unchanged. Doc claim at `:75` ("Derefs to `Metadata` for read access without unwrapping") still there.
- `git log origin/main -S'impl std::ops::Deref for ValidMetadata'` returns exactly one commit, `795920cb feat(package)!: package entry points` — the PR the issue was deferred from. Nothing has touched it since.
- The three decay sites the issue targets are live production code: `crates/ocx_lib/src/package_manager.rs:574`, `crates/ocx_lib/src/package_manager/tasks/pull_local.rs:165`, `crates/ocx_lib/src/package_manager/tasks/common.rs:127`, each `ValidMetadata::try_from(..)?.into()`.
- **Not in pass 1, and it sets the real size**: `ValidMetadata` has **no `impl ValidMetadata` block at all** — zero inherent accessors. Every read on the type goes through `Deref`, including four calls inside `validate_for_publish` itself (`validation.rs:301,337,454,585` all take `&Metadata` and are called as `f(&valid)`). Seven external `&ValidMetadata` parameters exist (`tasks/common.rs:898,1190,1216,1321,1349`, `tasks/inspect.rs:393,436`) and all of them read through `Deref` too — `.dependencies()`, `.env()`, `.integrations()`.
- The re-export the issue cites as `metadata.rs:18` is now `crates/ocx_lib/src/package/metadata.rs:28`.

**Remaining scope**

- Add inherent forwarding accessors on `ValidMetadata` for the fields consumers actually read (`dependencies`, `env`, `integrations`, `entrypoints`, plus whatever `task rust:verify` surfaces).
- Migrate the four in-module `validate_*` helpers off `&Metadata` — they are `pub(super)` and can take `&ValidMetadata` or the private inner field directly.
- Migrate the three `.into()` decay sites to keep the `ValidMetadata` and use its accessors.
- Remove the `Deref` impl and the `:75` doc sentence that advertises it.
- Remove `From<ValidMetadata> for Metadata`, or demote it to `pub(crate)` if a serialization path still needs it.
- Run `task rust:verify`; every type error is a remaining consumer.

### Batch 6 — Shell / status quick wins

#### [#396](https://github.com/ocx-sh/ocx/issues/396) Version build metadata compares as a string: `_10001` sorts below `_8001` (live on amazon/corretto:17)

- **Labels**: —
- **Verdict**: NOT_STARTED · startable now
- **Size**: S — one comparator in one file plus unit tests and an ADR amendment; the
  `Eq`-consistency tiebreak keeps it from being XS but does not spread it across files.
- **Importance**: high — a rolling tag silently resolves to an older binary for any
  published package whose build width straddles a digit boundary, live on
  `amazon/corretto` majors 17 and 11 today. Not critical: `cascade repair` re-points the
  aliases with no re-download once the comparator is fixed, and no persisted format
  changes (only which digest a rolling tag resolves to).
- **Depends on / blocks**: none.
- **Pass-1 → final**: NOT_STARTED → **AGREE** (citations hold; two constraints added that
  change how the fix must be written, and one adjacent defect pass 1 did not look for)

**Evidence**

- `crates/ocx_lib/src/package/version.rs:412-417` — the build arm is
 `(Some(lhs_build), Some(rhs_build)) => lhs_build.cmp(rhs_build)`, i.e. `String::cmp`.
 Pass 1 cited `411-416`; off by one, otherwise exact.
- `git log origin/main -S "lhs_build.cmp" -- crates/ocx_lib/src/package/version.rs`
 returns exactly one commit, `2189dc38 feat: initial commit`. The comparator has never
 been touched. `git log origin/main -- <that file>` lists 10 commits, none about ordering.
- No test covers it. `test_version_ordering` (`version.rs:671-690`) exercises only
 major/minor/patch/prerelease shapes and never constructs a build token;
 `test_build_separator_normalization` (`:725-748`) asserts `_`/`+` normalization and a
 round-trip, not ordering. `git grep "10001"` across `crates`, `test`, `website` returns
 only an unrelated Unicode fixture and a benchmark float.
- **New — the fix cannot be a bare numeric compare.** `Version` derives
 `PartialEq, Eq, Hash` structurally (`version.rs:14`) while `Ord` is hand-written
 (`:335`). A comparator that returns `Equal` for `_007` vs `_7` breaks the
 `Ord`/`Eq` agreement that `BTreeSet<Version>` depends on — and cascade blocking is
 exactly `BTreeSet` range scans (`cascade.rs:108`, `:211`, `:293`;
 `cascade/graph.rs:150`, `:389`, `:401`, `:830`). The numeric compare has to fall
 through to the raw-string compare on a numeric tie.
- **New — an adjacent instance of the same defect.** The prerelease arm one block up
 (`version.rs:400-410`) is also a plain `String::cmp`, so `-rc10` sorts below `-rc8`.
 SemVer orders all-numeric prerelease identifiers numerically. Not what #396 reports,
 but it is the same bug in the same function and a fix touching one should decide about
 the other.
- Ordering is consumed beyond cascade: `package_manager/tasks/update_check.rs:543-545`
 picks the newest version with `.max()` over parsed `Version`s, so the update checker
 reports the same wrong winner.
- `.claude/artifacts/adr_version_build_separator.md:118-119` carries the
 `build = "_" 1*alphanumeric` grammar the issue quotes. `git grep "numeric"` over
 `version.rs` and that ADR returns nothing else — no numeric-token handling is designed
 or documented anywhere.

**Remaining scope**

- Compare all-digit build tokens numerically in `Version::cmp`, falling back to the
 string compare when either side is not all digits.
- Break a numeric tie on the raw string (`_007` vs `_7`) so `cmp` still returns `Equal`
 only for values that are `Eq`. Without this, `BTreeSet<Version>` in cascade can treat
 two distinct published tags as one node.
- Decide whether the all-numeric prerelease identifier gets the same treatment
 (`-rc10` vs `-rc8`, `version.rs:400-410`), or is explicitly left string-ordered.
- Add unit tests: `_10001 > _8001`; `_007` and `_7` are ordered and not `Equal`; both
 survive a `BTreeSet` round-trip.
- Amend `adr_version_build_separator.md` with the ordering rule — it currently states the
 grammar and says nothing about how two build tokens compare.
- Not in this repo: sweep the mirror fleet for generators emitting an unpadded numeric
 build token.

#### [#400](https://github.com/ocx-sh/ocx/issues/400) `OCX_NO_CONSENT`: let a non-interactive caller run `pull`/`exec` without recording an activation stamp

- **Labels**: —
- **Verdict**: NOT_STARTED · startable now
- **Size**: S — one env key, one check at an existing choke point, one doc entry, one test; the pattern is fully established.
- **Importance**: medium — blocking a real downstream adopter (rules_ocx / Bazel), which documents the stamp as an unavoidable side effect until this lands. It is a security control being granted without a human gesture, which argues for higher, but the fail-safe direction and the narrow blast radius keep it at medium.
- **Depends on / blocks**: none; independent.
- **Pass-1 → final**: NOT_STARTED → **AGREE on the verdict, corrected on the remaining scope** — pass-1's fix location would break `ocx shell allow`.

**Evidence**

- No bypass of any kind exists. `grep -rn "OCX_NO_CONSENT"` over `crates/`, `website/` and `test/` returns exit 1 (no match). `Recorded` (`crates/ocx_lib/src/project/consent.rs:489-495`) has exactly two variants, `Stamped` and `OcxHomeNeedsNoStamp`.
- `consent::record` (`consent.rs:540`) and its callee `record_in` are unconditional except for the one A-44 ocx-home carve-out at `consent.rs:800-802` (a device+inode `same_dir` probe against `$OCX_HOME`). No TTY check, no env gate, no `--yes` flag anywhere on the path.
- I checked the adjacent vocabulary too: `OCX_CONSENT_PATHS` and `OCX_CONSENT_NAMESPACES` exist (`crates/ocx_lib/src/config/shell.rs:33`) but they *grant* consent, they do not suppress recording — the opposite direction. The `OCX_NO_*` family in `crates/ocx_lib/src/env.rs:35-143` is `OCX_NO_CONFIG`, `OCX_NO_PROJECT`, `OCX_NO_MODIFY_PATH`, `OCX_NO_CONFIG_REFRESH`, `OCX_NO_VERIFY`, plus `OCX_NO_CODESIGN` and `OCX_NO_HOOK`. No consent member. No `__OCX_TESTING_*` seam either.
- The two call sites the issue names are exact and are the only ones: `crates/ocx_cli/src/command/pull.rs:80` and `crates/ocx_cli/src/command/toolchain_exec.rs:169`, both `load_project_with_lock_consenting(&context)`.
- **Correction to pass-1's remaining scope.** All six A-29 writers funnel through one function: `record_activation_consent` (`crates/ocx_cli/src/app/project_context.rs:374`), called by `add.rs:185`, `remove.rs:170`, `lock.rs:133`, `update.rs:166` and — via `load_project_with_lock_consenting` at `project_context.rs:349` — by `pull` and `exec`. So no threading through call sites is needed. But the gate must **not** go in `consent::record`, because `ocx shell allow` calls it directly (`crates/ocx_cli/src/command/shell_allow.rs:69`) and an env var must never silently no-op an explicit human grant.
- No commit or PR on main mentions #400; the four PRs the search surfaced are numeric coincidences.

**Remaining scope**

- Add an `OCX_NO_CONSENT` boolean key to `crates/ocx_lib/src/env.rs` keys and read it with the existing `crate::env::flag` helper, following `OCX_NO_HOOK` / `OCX_NO_PROJECT`.
- Check it in `record_activation_consent` (`crates/ocx_cli/src/app/project_context.rs:374`) — the six-writer seam — and **not** in `consent::record`, so `ocx shell allow` (`shell_allow.rs:69`) keeps working as the explicit opt-in.
- If a third `Recorded` variant is wanted for reporting symmetry with `OcxHomeNeedsNoStamp`, add it to `consent.rs:489`; it is not required for the behaviour.
- Document it in `website/src/docs/reference/environment.md` beside `OCX_NO_HOOK` (`:620`) and `OCX_NO_PROJECT` (`:647`), per the three-step checklist at `crates/ocx_lib/src/env.rs:196-200`.
- Add an acceptance test asserting `state/projects/<key>/consent.json` is absent after `OCX_NO_CONSENT=1 ocx --project <abs> pull`, and present without it. Check the structural seam test at `project_context.rs:766` (`SEAM_CALLS`) still passes.

#### [#395](https://github.com/ocx-sh/ocx/issues/395) ocx status show pinned digest

- **Labels**: —
- **Verdict**: NOT_STARTED · **Gate**: rescoped: host-leaf match in plain_annotation
- **Size**: XS to S — if (1), one function plus one unit test in one file. Unsizable until the ask is confirmed.
- **Importance**: low — cosmetic on the default view of a read-only reporting command, no wrong data emitted (the JSON is complete and correct), no resolve-path impact.
- **Depends on / blocks**: none.
- **Pass-1 → final**: IMPLEMENTED (close candidate) → **OVERTURNED**. The behaviour pass-1 cites has shipped since **2026-07-31**, a full month *before* the owner filed this on 2026-09-01, so "already shipped, closing" cannot be the answer the issue is asking for.

**Evidence**

- The issue body is **empty**, not "one line" as pass-1 states. `gh issue list --json body` gives `body_len=0`; the only text is the title. Its neighbour `#397` ("initializing / allowing ocx.toml does not auto-load") is also empty-bodied, so both are one-line idea captures from the same window.
- The cited behaviour exists. `crates/ocx_cli/src/api/data/status.rs:56-65` declares `ToolStatus.platforms: Option<BTreeMap<String, String>>`, populated at `:225-231` from `locked.platforms` as `(platform_key, digest.to_string())`. Verified by running the binary: `ocx --format json status` in this repo emits a full `platforms` map of `sha256:…` values per tool. Note the flag is the **global** `ocx --format json status`; `ocx status --format json` is rejected with *"unexpected argument '--format' found"*.
- **The decisive falsifier.** `git log origin/main -- crates/ocx_cli/src/api/data/status.rs` returns three commits, the oldest being `387ecf05` (**2026-07-31**, *"feat(cli): add ocx status and ocx inspect for toolchain i…"*), and `git log -S'plain_annotation'` on that file returns `387ecf05` alone. Both the JSON map and the plain-text digest annotation shipped in the command's first commit. The issue was filed 2026-09-01. Closing it as already-shipped would tell the owner something they had a month to observe.
- **A concrete live gap that fits the title.** `ToolStatus::plain_annotation` (`status.rs:390-399`) looks the host up by exact string equality: `platforms.get(&host.to_string())`. `Platform::current()` (`crates/ocx_lib/src/oci/platform.rs:308-317`) always folds in `cached_os_features()`, which on a glibc Linux host renders `linux/amd64+libc.glibc`. A lock leaf published without libc variance is keyed plain `linux/amd64`, so the two never match. Running `./target/release/ocx status` in this repo right now: **6 of 8 tools print `none for linux/amd64+libc.glibc` and no digest at all**; only `bun` and `uv`, whose locks happen to carry the `+libc.glibc` key, show one. In the text view — the default view — `ocx status` does not show the pinned digest for most tools.
- That lookup is the only host-leaf-from-lock lookup in the tree (`grep -rn 'platforms\.get' crates/` outside tests hits `status.rs:397` alone), so the resolve path is unaffected. It is purely a display defect, and the fix is to match with `Platform::is_compatible` subset semantics rather than string equality.
- Coverage is JSON-only. `test/tests/test_status.py::test_status_with_lock_reports_every_platform` (`:72-103`) exists and asserts what pass-1 says it does — the full platform set, and `digest.startswith("sha256:")` for each. But `_status()` reads JSON, and the `#[cfg(test)] mod tests` in `status.rs` (`:442-518`) covers only the `EnvValueOut` separator field. **Nothing anywhere tests `plain_annotation`**, which is exactly why the host-key mismatch is unnoticed.
- A third reading exists and is cheap to rule in or out: `ocx shell state`, added in the same shell initiative that dominated this window, prints home, project, active and consent-stamp lines and **no tool or digest information at all**. Run and confirmed.

**Remaining scope**

- Ask the owner which of these the title meant, since the plain reading has been shipped since 2026-07-31:
 1. `ocx status`'s **text** view should show the host's pinned digest for every tool, not just those whose lock key string-matches `Platform::current().to_string()`. Today it prints `none for linux/amd64+libc.glibc` for 6 of the 8 tools in this repo.
 2. `ocx shell state` should report the active toolchain's pinned digests.
 3. Something else entirely — in which case the issue needs a body.
- If (1): change `ToolStatus::plain_annotation` in `crates/ocx_cli/src/api/data/status.rs` to select the host leaf by `Platform` compatibility rather than by `String` equality, and keep the `N platform(s)` count and the `none for <host>` fallback for the genuinely-absent case.
- Either way, add the first test for `plain_annotation`. It has none, which is why this survived from the command's first commit. A fixture with one lock leaf keyed `linux/amd64` against a `linux/amd64+libc.glibc` host reds today.

#### [#398](https://github.com/ocx-sh/ocx/issues/398) smoke.star: ocx.exists/read_file are scratch-only despite docs, and a missing binary is an uncatchable spawn error

- **Labels**: —
- **Verdict**: NOT_STARTED · **Gate**: none (docs fix first)
- **Size**: S — the load-bearing fix is three doc paragraphs plus one acceptance test; the
  two API decisions are small and confined to `ocx_module.rs` + `script-host-api.md`.
- **Importance**: medium — no longer a blocker (the check is achievable today), but the
  reference page is misleading enough to have produced a confident, wrong smoke-test
  verdict in production, and `ocx.exists` returning `False` for a file that is present is
  the failure mode that does not announce itself.
- **Depends on / blocks**: none.
- **Pass-1 → final**: PARTIAL → **OVERTURNED**, in both directions. Pass 1 was right that
  the sandbox falls back to the package root, but wrong to conclude "the worse half does not
  reproduce against current code" — it reproduces, for a cause pass 1 did not look for. And
  nothing of the issue's ask has landed, so there is no partial progress to record.

**Evidence**

- The fallback is real and pass 1's citation holds: `script/guard.rs:92-105`
 `resolve_read` probes the scratch candidate, then `package_root.join(normalized)`.
 The two unit tests pass 1 named exist —
 `resolve_read_falls_back_to_package_root_when_only_there` (`guard.rs:231`) and
 `resolve_read_prefers_scratch_when_present_in_both` (`:251`). It shipped in
 `71f184ba` (**2026-06-07**), long before the 0.6.0 the issue measured, so it was
 never the cause.
- The callers pick the right root for the symlink re-check, so the fallback is not
 undone downstream: `script/ocx_module.rs:509-513` (`read_file`) and `:556-560`
 (`exists`) select `scratch_root` or `package_root` by `p.starts_with`.
- **The actual cause of `ocx.exists("bin/javac") -> False`.** A package root is the
 *parent* of the extracted tree, not the tree. `file_structure/package_store.rs:25-26`
 — "The root directory of this package (parent of `content/`, `metadata.json`, etc.)";
 `PackageDir::content()` at `:49-51` is `self.dir.join("content")`. The same page of the
 user docs says it twice more: `website/src/docs/reference/command-line.md:897` and
 `:1893-1895` ("the **package root** (parent of `content/` and `entrypoints/`), not the
 `content/` subdirectory … Consumers traverse into `<path>/content/` for files"), and
 `:3317` says a layer's post-strip tree "is placed in the assembled `content/`
 directory instead of at the package root".
- `ocx package test` anchors exactly that shape: `command/package_test.rs:227` calls
 `manager.pull_local(info, &self.layers, Some(&dest_path))` and `:230-233` then reads
 `install_info_from_package_root(&dest_path)`, which resolves `metadata_for_content`
 against a directory documented as accepting "either a `content/` path or a package
 root" (`package_manager.rs:561-565`). `package_store.rs:32-37` names `package test` as
 a caller of `PackageDir::with_root`. `ocx.package_root` is that same `dest_path`
 (`script_runner.rs:64` → `ocx_module.rs:628,635`), which matches the
 `/home/…/.ocx/temp/test/test-pepsE8` the issue printed.
 **So the file is at `content/bin/javac`, and `ocx.exists("content/bin/javac")` is the
 spelling that works today.** The variant-integrity check the issue says is blocked is
 not blocked; it is undiscoverable.
- `website/src/docs/reference/script-host-api.md` never says this. It describes
 `ocx.package_root` as "the materialized package directory (read-only)" (`#ocx-roots`)
 and `read_file`/`exists` as scoped to `{scratch_root, package_root}` (`#ocx-read-file`,
 `#ocx-exists`) — all true, all useless to someone looking for `bin/javac`.
- The absolute-path workaround is still rejected, as reported: `guard.rs:50-56` refuses
 `raw.is_absolute()` in `resolve_scratch`, which `resolve_read` calls first at `:93`, so
 an `ocx.package_root`-prefixed path never reaches the package-root branch.
- The missing-binary spawn error is still uncatchable, as reported:
 `ocx_module.rs:150` `spawn_res.map_err(|e| format!("failed to spawn '{}': {e}", …))?`
 propagates as a hard script error. `script-host-api.md` `#ocx-run` still says only
 "A non-zero exit code does **not** raise. The script decides whether to fail."
- No positive read test at any level. `git grep "ocx.exists"` over
 `test/tests/test_package_test_script.py` returns one hit (`:542`, an escape case);
 `git grep "ocx.read_file"` returns three (`:362`, `:540` escapes; `:357` a docstring).
 `test_package_and_scratch_roots_are_path_attributes` (`:988`) asserts only that the two
 roots are non-empty strings and differ.
- No commit since the issue was filed touches `crates/ocx_lib/src/script/` — the last is
 `1f94857f` (2026-08-21).

**Remaining scope**

- Correct `website/src/docs/reference/script-host-api.md`: state that the extracted
 bundle lives under `<package_root>/content/`, and show the working spelling
 (`ocx.exists("content/bin/javac")`) in the `#ocx-roots` / `#ocx-exists` /
 `#ocx-read-file` entries. This is the fix for the reported symptom — the sandbox
 itself is correct.
- Decide whether to also expose the content directory directly, e.g. an
 `ocx.content_root` attribute, so a smoke test does not have to know the store layout.
 That is the shape that makes the variant-integrity check natural.
- Decide and implement one of: accept an `ocx.package_root`-prefixed absolute path as an
 explicit sandbox-relative escape hatch, or document at `#ocx-read-file` that absolute
 paths are rejected even when they name a path inside the sandbox.
- Decide and implement one of: return a catchable `RunResult` for a spawn failure
 (sentinel exit code), or document at `#ocx-run` that a missing `argv[0]` raises a
 script error distinct from a non-zero exit, and name the supported way to probe bundle
 contents.
- Add an acceptance test that reads a real file placed under the package root — every
 existing `exists`/`read_file` test is an escape case, so the fallback has no
 end-to-end coverage and the `content/` layout is unpinned.
- Correct the issue body: "scratch-only" is not what the code does, and a reader acting
 on it would change the guard rather than the docs.

#### [#362](https://github.com/ocx-sh/ocx/issues/362) The global tier does a store write per tool on every prompt

- **Labels**: —
- **Verdict**: NOT_STARTED · **Gate**: measure resolve() vs syscalls split first
- **Size**: M — the edit itself is small and local, but it is a behaviour change on a hot path with the content store as blast radius, and the issue requires its own measurement.
- **Importance**: medium — a real per-prompt cost that scales with global-toolchain size and is a named contributor to the budget growth in [#360](https://github.com/ocx-sh/ocx/issues/360); pass-1's "medium-high" overstates it now that the per-tool cost is known to be a resolve plus stats rather than writes.
- **Depends on / blocks**: relates to [#339](https://github.com/ocx-sh/ocx/pull/339), [#360](https://github.com/ocx-sh/ocx/issues/360) (shares the latency-gate instrument), and is evidence in [#359](https://github.com/ocx-sh/ocx/issues/359)'s decision.
- **Pass-1 → final**: NOT_STARTED → **AGREE on the verdict, OVERTURNED on the evidence.** Pass-1's primary citation names the wrong function: `composer.rs:1407` is the **project**-tier probe's tag-only fallback, not the global-tier path. An implementer sent there would fix nothing.

**Evidence**

- The real per-prompt path, traced end to end: `crates/ocx_cli/src/command/self_group/activate.rs:315` calls `resolve_global_pinned_env(&context, &target, &[], &[])` unconditionally on every non-stat-only prompt (past the `is_stat_only` early return at `:290`). Inside it, `crates/ocx_cli/src/command/toolchain_env.rs:627` calls `manager.find(&identifier, target.clone())` **once per locked global tool, in a loop**. That is the A-44 unconditional path the issue names.
- `crates/ocx_lib/src/package_manager/tasks/find.rs:36,50` — `find()` unconditionally calls `self.resolve(package, platform)` and then `link_blobs(...)`. No short-circuit for "chain already present and unchanged" was added.
- **Pass-1's citation is a misattribution.** `composer.rs:1400-1408` is `local_root`, the project tier's probe. Its `Err(_) => self.find(...)` arm fires only for a **tag-only** identifier; every lock-pinned tool takes the `Ok(pinned) => self.find_plain(...)` arm at `:1406`. The global tier does not route through `local_root` at all — `toolchain_env.rs:626` builds a digest-pinned identifier with `clone_with_digest(leaf)` and calls `find()` directly.
- **The issue's own title overstates what happens today.** `link_blobs` (`crates/ocx_lib/src/reference_manager.rs:329-370`) skips the symlink write when the existing target already matches (`continue` at `:357`). What a steady prompt actually pays per tool is one `create_dir_all` plus an `is_link` + `read_link` per chain entry, plus the full `resolve()` round-trip — syscalls and a resolve, not fsyncs. That skip landed in [`ca4553de`](https://github.com/ocx-sh/ocx/commit/ca4553de) on 2026-04-17, four months before the issue was filed, so the "7 store writes per prompt" framing was already inaccurate at filing time.
- Nothing on main addresses it. `git log origin/main --since=2026-08-27` over `toolchain_env.rs`, `find.rs` and `reference_manager.rs` returns only `9309125f` (the #339 merge that surfaced this) and `fed200c6` (the `run` → `exec` rename). No commit or PR mentions #362; the one PR the search surfaced ([#135](https://github.com/ocx-sh/ocx/pull/135), a Dependabot bump) is a numeric coincidence.

**Remaining scope**

- Fix it at `crates/ocx_cli/src/command/toolchain_env.rs:626-631`, not in the composer. The identifier there is already digest-pinned, so the lazy fix is the pattern `local_root` already uses at `composer.rs:1406`: build a `PinnedIdentifier` and call `find_plain`, skipping the `resolve()` round-trip and the `link_blobs` upsert entirely.
- Decide the one behavioural consequence: `find()`'s `link_blobs` upsert exists to repair legacy installs and alt-tag resolves that walked a different image index (`find.rs:47-49`). Skipping it on the prompt path means that repair happens only on `pull`/`add`/`lock`/`update`. Confirm that is acceptable, or gate the upsert on a cheap staleness check.
- Measure the split first, per the issue's own open question: how much of the +2.532 ms across 7 tools is `resolve()` versus the `create_dir_all`/`read_link` syscalls. The `link_blobs` write-skip finding above means the answer is probably "mostly `resolve()`", which changes which fix is worth doing.
- Land it with its own red/green pair on `test/bench/shell_latency.py`, which now seeds the global tier and asserts a non-zero global composed-key count.

#### [#360](https://github.com/ocx-sh/ocx/issues/360) C-044's per-prompt budget is nominally met and effectively undecidable on a slower runner

- **Labels**: —
- **Verdict**: PARTIAL · startable now
- **Size**: M — one function in one file, but it is a considered redesign of a gate's decision rule with a red/green proof obligation, not a mechanical edit.
- **Importance**: medium — the acute risk (a breach reading as PASSED) is fixed and was worth four days of false green; what remains is that the gate's most probable failure state on a contended runner is still "no verdict".
- **Depends on / blocks**: [#339](https://github.com/ocx-sh/ocx/pull/339), [#408](https://github.com/ocx-sh/ocx/pull/408) (merged, carries the budget half); watched by [#359](https://github.com/ocx-sh/ocx/issues/359).
- **Pass-1 → final**: PARTIAL → **AGREE**. I read all three commits in full and tried to promote this to IMPLEMENTED; the classifier half is unambiguously still open, in the owner's own words.

**Evidence**

- **Item 1 (the budget) — DONE.** `RECONCILE_BUDGET_MS = 10.0` at `test/bench/shell_latency.py:545`, a hard product ceiling rather than a derived number, with the full five-observation GitHub-runner series recorded at `:499-509`. `_WORST_KNOWN_GOOD_RECONCILE_MS = 7.590` (`:2498`) now tracks the machine separately, so the two constants no longer move together. Commit [`f58e6531`](https://github.com/ocx-sh/ocx/commit/f58e6531783a03035e1618dbc7e281a8a277dd05).
- **Red-reachable, and now runner-sized.** `test/taskfile.yml:239-254` injects 11 ms and requires both budget needles to go red; `unmatched_gate_needles` (`shell_latency.py:678-693`) refuses to let an abstention satisfy a needle. Commit [`34728edc`](https://github.com/ocx-sh/ocx/commit/34728edc254ef18dbeebe4bd6109b608a0871a3a) re-derived the sizing note from the GitHub runner (17.340 ms) instead of the quiet dev box (16.05 ms), correcting a claim that was false on the only machine it described.
- **The reporting bug — DONE.** `overall_verdict` (`shell_latency.py:651-675`) is now a pure function that leads with `NO VERDICT` and never contains the string `PASSED` when abstaining. The self-check at `:2891-2907` asserts all three colours, including `assert "PASSED" not in verdict`. Commit [`4fcdc272`](https://github.com/ocx-sh/ocx/commit/4fcdc2721fe64795f196475b4b40ffd5b4fa4f35).
- **Item 2 (the classifier's margin rule) — NOT DONE.** I read `4fcdc272`'s diff line by line: it adds `overall_verdict` plus its self-check cases and rewrites one f-string in `format_report`. `_budget_gate` is untouched, and its rule 3 is still literally `inconclusive = missed and spread > margin` at `shell_latency.py:803`. The commit message says so itself: *"What this does NOT fix is the classifier itself — a breach smaller than the floor's scatter still yields no verdict. That is the other half of #360, and it is a design question, not a wording one."*
- No floor-quality precondition exists. `measurement_admissible` (`shell_latency.py:1201`) is derived *from* the gate abstentions (`not any(gate.inconclusive ...)`); it is a record of what happened, not a precondition that can fail a run.
- The undecidability is narrowed but not closed, and `f58e6531`'s own message states the residual: *"a breach between 7.6 and 10 ms is now invisible"*. On a runner whose floor scatters more than the margin, a breach above 10 ms still abstains.

**Remaining scope**

- Redesign `_budget_gate`'s margin rule (`test/bench/shell_latency.py:739-821`), choosing one of the two options the issue names: a floor-quality precondition that **fails the run** rather than abstaining the gate, or a rule requiring N quiet samples before a verdict may be withheld.
- Ship it with both colours demonstrated in `self_check`, the way `overall_verdict` was — the existing abstention cases at `:2771-2820` are the pattern to extend.

#### [#361](https://github.com/ocx-sh/ocx/issues/361) find_symlink_all resolves packages one at a time and takes no concurrency argument

- **Labels**: —
- **Verdict**: NOT_STARTED · startable now
- **Size**: S — one function, two call-site lines, one new test; the shape is already decided by the `composer.rs` template.
- **Importance**: low — reached only by explicit `--candidate`/`--current` invocations of `ocx package env` and `ocx package which`. Verified not on the per-prompt path.
- **Depends on / blocks**: none. `ocx-sh/ocx#339` and `fe228947` are landed precedent, not blockers.
- **Pass-1 → final**: NOT_STARTED → **AGREE**. Evidence checks out; two small factual corrections and one contract correction below.

**Evidence**

- `crates/ocx_lib/src/package_manager/tasks/find_symlink.rs:125-146` — `find_symlink_all(&self, packages: Vec<oci::Identifier>, kind: SymlinkKind)`. Body is a plain `for package in &packages { … self.find_symlink(package, kind).await … }`. No `concurrency` parameter, no `join_all`, no semaphore. Confirmed by reading the function.
- Both call sites confirmed by `grep -rn find_symlink_all crates/`: `crates/ocx_cli/src/command/which.rs:79` and `crates/ocx_lib/src/package_manager/composer.rs:1296`. There are exactly two.
- The template is in the same file as one call site: `composer.rs`'s `Materialization::LocalOnly` arm uses `concurrency.semaphore()` + `acquire_permit` + `join_all`, with a comment explaining `join_all` over `buffer_unordered` because it yields in input order. The `Materialization::Symlink(kind)` arm two branches above still calls `find_symlink_all` with no concurrency, even though `compose_roots` already has a `concurrency: Concurrency` parameter in scope — so wiring at that call site is a one-line change.
- The latency premise holds. `composer.rs`'s `LocalOnly` arm carries the comment *"Every shell prompt takes this branch"*; `Materialization::Symlink` is produced only at `crates/ocx_cli/src/command/env.rs:137`, from `--candidate`/`--current`. So `find_symlink_all` is reached only by `ocx package env --candidate/--current` and `ocx package which --candidate/--current`, never per prompt. Pass-1 named only `env`; `which` is the second command.
- **Pass-1 error (test count).** Pass-1 says the `#[cfg(test)] mod tests` holds "two tests for `installed_current_digest`". There are **four**: `installed_current_digest_absent_symlink_returns_none` (`:180`), `_healthy_install_returns_digest` (`:195`), `_dangling_symlink_returns_none` (`:229`), `_malformed_digest_file_returns_none` (`:251`). The substantive claim is right: **zero** tests exercise `find_symlink_all`.
- No acceptance coverage of the ordering either. `test/tests/test_which.py::test_find_candidate_returns_candidate_symlink` and `test/tests/test_env.py::test_env_candidate_uses_symlink_path` are the only `--candidate` tests, and both pass a single package, so neither can discriminate result-to-request pairing.
- **Contract correction the issue itself gets wrong.** The issue says to "keep first-error-by-index". The current code does not do that: it accumulates *every* failure into `errors: Vec<PackageError>` in request order and returns `Error::FindFailed(errors)` with all of them. A rewrite must preserve the full error list in request order, not collapse to the first.

**Remaining scope**

- Add a `concurrency: Concurrency` parameter to `find_symlink_all` and drive the loop with `futures::future::join_all` plus `concurrency.semaphore()` / `acquire_permit`, mirroring the `Materialization::LocalOnly` arm in `composer.rs`. Not `try_join_all` — it aborts on first completion.
- Preserve the existing error contract exactly: every failing package still reports, and the `Vec<PackageError>` stays ordered by request index, not by completion. `join_all` yields in input order, so no re-sort is needed.
- Wire the parameter through both call sites: `crates/ocx_cli/src/command/which.rs:79` and `crates/ocx_lib/src/package_manager/composer.rs:1296` (the latter already has `concurrency` in scope from `compose_roots`).
- Add the discriminating regression test: three packages with the **middle one absent**, asserting each `InstallInfo` lands on its own request and the error names the middle package. Show it red under a results-reversal mutation before trusting the change — `find_symlink.rs` currently has zero tests for this function, so there is no existing green to lean on.

#### [#365](https://github.com/ocx-sh/ocx/issues/365) flaky: project_lock::a_symlink_planted_during_the_retry_loop_is_refused fails under parallel load

- **Labels**: area/tests, priority/low, flaky-test
- **Verdict**: NOT_STARTED · **Gate**: diagnose which assertion fires first
- **Size**: S for the diagnostic pass (one test, one file). M if a genuine retry-loop window is confirmed.
- **Importance**: low — `priority/low`, intermittent under parallel load only, passes 3/3 in isolation, no confirmed regression. Raise it if the diagnosis lands on (b), since the guard is a security refusal.
- **Depends on / blocks**: none.
- **Pass-1 → final**: NOT_STARTED → **AGREE**. All four evidence claims re-verified independently; one citation is imprecise.

**Evidence**

- The test is present and unchanged at `crates/ocx_lib/src/project/project_lock.rs:364-397`. Still `#[cfg(unix)] #[tokio::test(flavor = "multi_thread")]`, still `tokio::time::sleep(Duration::from_millis(40))` in the planting task, with the comment *"One tick in, leaving ~460 ms of the 500 ms budget to be caught in."*
- Whole-file history, not just the `-S` needle: `git log origin/main -- crates/ocx_lib/src/project/project_lock.rs` returns exactly three commits — `bef7428b` (2026-08-22, authored the test), `9ae4a844` (2026-05-28), `33bd9fa4` (2026-05-09). Nothing has touched the file since the issue was filed on 2026-08-27.
- **Citation correction.** Pass-1 cites `CONTENTION_BUDGET` at `project_lock.rs:106`. Line 106 is the *use* (`Instant::now() + CONTENTION_BUDGET`); the constant is defined at `:52` as `Duration::from_millis(500)`. The 500 ms figure is right.
- `.config/nextest.toml` read in full: the only test-group and the only two overrides are `live-shell` / `filter = 'test(shell::tests::live_)'`, for concurrent `pwsh` assembly-loader corruption. No retry policy, no serialization, no override for `project_lock`. The mitigation applied to the other flaky-under-load class was not extended here. `git ls-files | grep -i nextest` confirms this is the only nextest config in the tree.
- PR #408 confirmed irrelevant, by files rather than by body text: `gh pr view 408 --json files` returns `analysis_shell_env_edge_cases.md`, `shell_state.rs`, `config/shell.rs`, `project/consent.rs`, `test/bench/README.md`, `test/bench/shell_latency.py`, `test/taskfile.yml`. It does not touch `project_lock.rs`, and `body | test("#365")` is `false`.

**Remaining scope**

- Reproduce under load and capture the assertion that actually fires. The two observed failures were truncated by nextest fail-fast, so no one has yet seen whether it is the `expect_err` (the acquire unexpectedly succeeded) or the `matches!` on the error kind (it failed for a different reason).
- Decide between the two causes: fixture timing (the 40 ms sleep racing the 500 ms budget on a loaded box) or a genuine narrow window in the retry loop's symlink re-check.
- If fixture timing, make it deterministic rather than widening the sleep — a barrier or a channel handshake between the planting task and the acquire loop removes the timing assumption entirely.
- If a real window, the fix belongs in `acquire_project_lock_for_file`'s retry loop in `crates/ocx_lib/src/project/project_lock.rs`, and needs its own regression test.
- Do not paper over it with a nextest retry: the row exists to prove a CWE-59/CWE-367 refusal, and a retried flaky security assertion is an unchecked green.

### Batch design — Design-first (ADR or design note before code)

#### [#25](https://github.com/ocx-sh/ocx/issues/25) feat: portable OCX home export/import for air-gapped environments

- **Labels**: area/file-structure, area/cli
- **Verdict**: NOT_STARTED · **Gate**: decide archive format + verb location
- **Size**: L — new verbs, a format decision, and a walk across all four store
  tiers plus the index. Unchanged from pass 1, and matches the owner's own
  2026-08-29 snapshot (LARGE).
- **Importance**: medium — directly serves differentiator #9 (air-gapped/corporate
  mirror support) and the offline-first principle, but the manual zip path works
  today and no user report is on file.
- **Depends on / blocks**: depends on #23 (landed — unblocked). Shares the archive
  module with `ocx package create`.
- **Pass-1 → final**: NOT_STARTED → **AGREE** (verdict right; the remaining scope
  misses a landed ADR that already settles part of the archive design, and leaves
  the #23 prerequisite "unverified" when it is resolvable)

**Evidence**

- `crates/ocx_cli/src/command.rs:94-171` root `Command` enum and
 `crates/ocx_cli/src/command/package.rs:20-84` `Package` enum — I read both in
 full. No `Export`/`Import` verb at either tier. The nearest neighbours are
 `Package::Copy` (registry→registry promotion) and `Package::Create` (single
 package archive), neither of which is a home export.
- `crates/ocx_lib/src/archive/` holds `tar.rs` **and** `zip.rs` plus
 `backend.rs`/`extract_options.rs`. Pass 1 cited only `tar.rs`. Both backends
 exist and are the reuse target — an implementer should not add a third.
- **Pass 1 missed this**: `.claude/artifacts/adr_three_tier_cas_storage.md`
 (Status **Accepted**, 2026-04-06) carries an "Import/Export Consideration"
 section that already decides part of #25. Verbatim at `:562`: hardlinks
 survive `tar`/`cp -a`/`rsync -H`, "the `ocx export` command design does not
 need special handling for symlink dereferencing inside package content,
 because there are no cross-tier symlinks in `packages/.../content/`", while
 cross-tier forward-refs (`refs/layers/`, `refs/blobs/`, `refs/deps/`) "still
 need `-L` dereferencing or separate archival when exporting a single package
 in isolation". That is a hard constraint on the archive walk, and it is
 already ratified.
- The three-tier store the ADR describes is real:
 `crates/ocx_lib/src/file_structure/` contains `blob_store.rs`,
 `layer_store.rs`, `package_store.rs`, `index_store.rs`, `state_store.rs`.
- The manual form of this feature is already a tested contract:
 `test/tests/test_managed_config.py:701`
 `test_zip_and_move_warm_home_offline_identical`, and
 `.claude/artifacts/adr_infrastructure_patches.md:60` states the offline
 contract as "zip `OCX_HOME` → unzip elsewhere → reinstall → identical patched
 env". #25 formalises a path users are already told to take by hand.
- Prerequisite #23 (relative symlinks): pass 1 left this "unverified". Resolved
 — `analysis_issue_triage_2026-08-29.md:140` records "prerequisite #23 landed
 so it is unblocked". #25 is not blocked.

**Remaining scope**

- Decide the archive format: OCX-native, OCI image-layout, or native-by-default
 with `--format oci-layout`. Still the one genuinely open question.
- `ocx export [packages...] --output <path>` — resolve packages (or the whole
 home) to their blob/layer/package tier entries plus the matching `index/`
 entries, and write the archive through the existing
 `crates/ocx_lib/src/archive/` backends. Do not add a new archive backend.
- Honour the ratified constraint from `adr_three_tier_cas_storage.md`: content
 inside `packages/.../content/` needs no symlink dereferencing (hardlinks
 survive tar), but `refs/layers/`, `refs/blobs/` and `refs/deps/` are cross-tier
 symlinks and need `-L` dereferencing or separate archival.
- `ocx import <path> [--home <target>]` — extract, content-address merge
 (identical digests cannot conflict), recreate install symlinks.
- Decide where the verb lives. Every neighbouring noun-scoped verb sits under
 `ocx package`; a whole-home verb has no existing group. This is a CLI-grammar
 call, not an implementation detail.
- Acceptance test, and a docs page. The behaviour it replaces is currently
 documented as a manual zip.

#### [#193](https://github.com/ocx-sh/ocx/issues/193) Dockerfile-friendly environment import for tool bootstrap (no project toolchain)

- **Labels**: —
- **Verdict**: NOT_STARTED · **Gate**: 3 design axes open (output shape, frozen index, staleness)
- **Size**: L — three open design axes before code, then CLI plus docs.
- **Importance**: medium — a dogfood and CI pain point the shipped docs already admit to; blocks nothing.
- **Depends on / blocks**: the staleness sub-question depends on #189's ADR approval, and only for the global tier. The aggregated-bin-dir question is independent.
- **Pass-1 → final**: NOT_STARTED → **AGREE**. I checked the four alternative surfaces named in my task prompt; none of them closes it.

**Evidence**

- `website/src/docs/docker.md:110`, closing the "Bootstrap Single Tools" section, states in the shipped docs: "A more direct global-install story for Dockerfiles (persistent `PATH` without a project) is planned." The section still recommends the mini-project workaround the issue calls insufficient. Confirmed by reading the section; `docker.md` has no other bootstrap heading.
- **`ocx env` (OCI tier)** — `crates/ocx_cli/src/command/env.rs:26-118`: the only output modes are `--shell[=NAME]` (eval-safe), `--ci=github|gitlab` with `--export-file`, and the context-level `--format`. No `--path`, no aggregated bin dir.
- **`ocx env` (toolchain tier, incl. `--global`)** — `crates/ocx_cli/src/command/toolchain_env.rs`, clap attributes at `:136-208`: the same four (`--shell`, `--ci`, `--export-file`, `--show-patches`). Its doc at `:533` confirms the global form is eval-only — `eval "$(ocx --global env --shell=sh)"`. A Dockerfile `ENV` cannot consume any of these.
- **`ocx direnv export`** — `crates/ocx_cli/src/command/direnv_export.rs:19-37`: project tier only, output always bash export lines, explicitly no `--shell` flag, designed for `eval "$(ocx direnv export)"` from `.envrc`. Not layer-persistent, and it is not a global-tier command.
- **No aggregated bin dir.** `crates/ocx_lib/src/file_structure.rs:124-128`: the only flat directories are `$OCX_HOME/.bin/ocx-shim` (the shim *binary*, one file) and `$OCX_HOME/shims/<key>/bin` (per-deferred-tool). Entrypoints remain per-repo under `symlinks/{registry}/{repo}/current/entrypoints`, exactly as the issue describes.
- **setup-ocx is not the answer.** `ocx-sh/setup-ocx@main` contains `action.yml` and TypeScript only — no Dockerfile, no image, no docs on baking `ENV`. It installs the CLI in GitHub Actions; it does not import a tool environment.
- `adr_project_toolchain_links.md` metadata lists this issue as "Dockerfile env staleness — global tier only; its aggregated-bin-dir question stays open", and that ADR is itself unapproved.

**Remaining scope**

- Decide the output shape: an `ocx env --path` / plain-`ENV` mode consumable by a Dockerfile `ENV` line, versus a stable aggregated bin dir the image can simply prepend to `PATH`. The two are alternatives, not both.
- Decide the frozen-index-in-image story so a bootstrap layer resolves reproducibly without a project.
- Decide env staleness: how a baked `ENV` block refreshes when a package update introduces new variables. `adr_project_toolchain_links.md`, once approved, answers this for the global tier only.
- Implement in whichever of `env.rs` / `toolchain_env.rs` the shape lands, with pytest coverage beside `test/tests/test_env.py` and `test_toolchain_env.py`.
- Rewrite `website/src/docs/docker.md` "Bootstrap Single Tools" and delete the "is planned" sentence at `:110`.

#### [#31](https://github.com/ocx-sh/ocx/issues/31) feat: mount dependencies into parent content at known subpaths

- **Labels**: area/package-manager
- **Verdict**: NOT_STARTED · **Gate**: confirm still wanted (interpolation covers the example)
- **Size**: L — down from pass 1's XL. The content-immutability decision that
  forced an ADR is already made and Accepted; what is left is a metadata field, a
  schema bump, a pull-time symlink, validation, and one narrower layer-interaction
  question. Matches the owner's 2026-08-29 snapshot (LARGE).
- **Importance**: medium, trending low — the headline use case is now served by
  landed interpolation, which weakens the motivation the issue was filed on.
- **Depends on / blocks**: depends on PR #13 (landed). Interacts with #22
  (multi-layer packages) via the non-overlapping-subtree rule.
- **Pass-1 → final**: NOT_STARTED → **AGREE on verdict, OVERTURNED on size**
  (XL → L: pass 1 says the design questions "are unresolved and explicitly flagged
  for an ADR"; an Accepted ADR already answers the hardest of the three)

**Evidence**

- `crates/ocx_lib/src/package/metadata/dependency.rs:121-141` — I read the whole
 `Dependency` struct. Three fields: `identifier`, `visibility`, `name`. No
 `mount`. Confirmed.
- `crates/ocx_lib/src/oci/client.rs:1423` — `mount_from` is the OCI
 cross-repository *blob* mount (`POST …mount=<digest>&from=<repo>`, "mounting is
 a pure optimization"). Pass 1's call of "grep noise, not partial credit" is
 correct; I verified it rather than inheriting it.
- **Pass 1 missed this**: `.claude/artifacts/adr_three_tier_cas_storage.md`
 (Status **Accepted**, names #31 in its Related Issues header) decides the
 content-immutability question at two places. `:226`: "Future mount points (#31)
 still use symlinks from package content into dependency content directories,
 because mounts cross package boundaries and need to be resolvable independently
 of the hardlink graph." `:557`: "Mount symlinks are still created inside the
 package's `content/` directory … and must remain symlinks even in the hardlink
 assembly model." That closes design question 3 of 3 — symlink-in-`content/`
 wins over sibling `mounts/` and over hardlink assembly. It is decided, not open.
- `.claude/artifacts/adr_package_dependencies.md:163` and `:360` already define
 `sealed` visibility as "Structural dependency — content accessed by mount point,
 symlink, or direct path". The visibility model was designed expecting mounts.
- **Premise check**: the issue's motivating example is "the parent's scripts can
 then reference `${installPath}/python/bin/python`". `${deps.NAME.installPath}`
 interpolation has since **landed** — `dependency.rs:134`,
 `metadata/env/constant.rs:14`, `metadata/authoring/dependency.rs:41`, and it is
 documented in `website/src/docs/reference/env-composition.md`. A parent's env
 and entrypoint templates can already name a dependency's install path. The
 issue lists that interpolation as out-of-scope/separate; it is now shipped, and
 it covers the stated example.

**Remaining scope**

- **First, confirm the feature is still wanted.** `${deps.NAME.installPath}`
 shipped and serves the issue's own worked example. Mount's remaining unique
 value is content that cannot be templated — a binary or config with a hardcoded
 relative path expecting a sibling at `./python/`. If that case is not real,
 close this as superseded rather than build it.
- If kept: add `mount: Option<String>` to `Dependency`
 (`crates/ocx_lib/src/package/metadata/dependency.rs`) and to the authoring
 mirror in `metadata/authoring/dependency.rs`; regenerate the JSON schema
 (`crates/ocx_schema`).
- Create the symlink at pull time at `${content}/<mount>` → dependency `content/`.
 The shape is already ratified: a symlink inside `content/`, never a sibling
 `mounts/` dir and never hardlink assembly
 (`adr_three_tier_cas_storage.md:226,557`). Do not re-open that in an ADR.
- Validate mount paths: relative only, no `..`, no absolute, no reserved names,
 no two dependencies claiming the same subpath — rejected at `ocx package create`.
- Decide the one question the ADR does **not** answer: how a mount interacts with
 a multi-layer package, given layers are validated as non-overlapping subtrees at
 extraction time (`adr_three_tier_cas_storage.md:554`). A mount point landing
 inside a layer's subtree is the conflict case.
- GC: the mount symlink must not make the dependency collectable or
 uncollectable by accident. Forward-refs under `refs/deps/` already exist.
- `ocx package deps` tree view shows mount paths.

#### [#167](https://github.com/ocx-sh/ocx/issues/167) perf(oci): bound per-layer spawn_blocking concurrency in streaming pull

- **Labels**: performance, area/oci, priority/low, tech-debt, discussion-needed
- **Verdict**: NOT_STARTED · **Gate**: bench to 8/16 layers first
- **Size**: M — the code change is small; the bench extension needed to justify the number is the size driver.
- **Importance**: low — carries the owner's own `priority/low` label and the issue's own "latent, not active" reading. (`.claude/artifacts/analysis_issue_triage_2026-08-29.md` rates its *value* HIGH; the discrepancy is worth an owner glance, but the label is the owner's.)
- **Depends on / blocks**: none. Related to #46 and #58.
- **Pass-1 → final**: NOT_STARTED → **AGREE**

**Evidence**

- `extract_layers` still spawns one `JoinSet` task per layer with no permit (`crates/ocx_lib/src/package_manager/tasks/pull.rs:805-816`). The only shared state added since is an SSRF `OnceCell` dial guard (`pull.rs:800`), not a semaphore.
- The whole download+decompress+extract of a layer is still inside one `spawn_blocking`, and the code's own comment still says the mitigation is deferred: "If install parallelism ever grows unbounded, add a semaphore at this boundary (deferred)" (`crates/ocx_lib/src/oci/client.rs:1088-1090`).
- Semaphores in `package_manager/` exist only for the package-level fan-out (`concurrency.rs:44-47`), `clean.rs:258`, and `garbage_collection/reachability_graph.rs:91`. None is at the layer boundary.
- Bench still tops out at 4 layers: `ocx_layers_4` and `ocx_layers_large_4` (`test/bench/README.md:370` and `:379`). No 8/16-layer rows exist.

**Remaining scope**

- Extend the layer-scaling bench to 8 and 16 layers to locate the parallelism plateau; that plateau is the minimum safe cap.
- Decide the cap value and the permit class. It must be a **separate** class from the package-level semaphore — `concurrency.rs:8-10` records that reusing one class deadlocks when an ancestor holds a permit while waiting on children.
- Add an adversarial test: many tiny high-expansion layers installed concurrently, asserting a bounded blocking-thread count and bounded transient memory.
- Implement the semaphore (option 1) or restructure so only decompress+tar runs blocking and network I/O stays async (option 2).

#### [#363](https://github.com/ocx-sh/ocx/issues/363) shell: no way to select which groups/packages load into the per-prompt env

- **Labels**: area/cli, priority/low, area/config
- **Verdict**: NOT_STARTED · startable now
- **Size**: L — new CLI and config surface across config, activation, shell-state reporting and the fingerprint; the issue sizes it past the ~1200 LOC branch-scope ceiling.
- **Importance**: low — labelled `priority/low`; a real gap with no reported pain beyond the owner's own manual-test note.
- **Depends on / blocks**: none. Independent of [#339](https://github.com/ocx-sh/ocx/pull/339).
- **Pass-1 → final**: NOT_STARTED → **AGREE**. The issue states the wanted behaviour in a four-item acceptance sketch and says "blocked on nothing", so this is an implementation gap, not a decision.

**Evidence**

- `crates/ocx_lib/src/config/shell.rs:42-83` — `ShellConfig` carries exactly `hook`, `completions`, `consent`, plus four `#[serde(skip)]` provenance fields. No `groups` key at either tier.
- The shell path hardcodes the default group at **both** tiers. Global: `crates/ocx_cli/src/command/self_group/activate.rs:315` passes `&[]` for `groups` into `resolve_global_pinned_env`. Project: `crates/ocx_lib/src/activation.rs:569` is a literal `let groups = vec![DEFAULT_GROUP.to_owned()];`. Neither has a channel to carry a selection.
- The vocabulary exists but is wired elsewhere. `options::GroupSelection` (`crates/ocx_cli/src/options/group_selection.rs:19`) reaches `toolchain_exec.rs:63`, `toolchain_env.rs:137`, `inspect.rs:99` and `direnv_export.rs:52` — not the reconcile path.
- No commit or PR on main mentions #363. The one PR the search surfaced, [#339](https://github.com/ocx-sh/ocx/pull/339), is the branch this was deferred from.

**Remaining scope**

- Config grammar for group selection at both the ocx-home and project tiers, with the override-versus-intersect rule written down. This is the implementer's design call, not an owner gate.
- `ocx shell state` reports the effective selection and which tier decided it — follow the existing `hook_tier` / `completions_tier` provenance pattern in `ShellConfig`.
- Fold the selection into the reconcile fingerprint (`crates/ocx_lib/src/shell/reconcile/fingerprint.rs`) so an edit re-composes at the next prompt.
- Re-verify the C-044 budget holds with a selection present (`test/bench/shell_latency.py`).
- Keep it distinct from A-44: the ocx home toolchain is always consented, and a selection is not consent.

#### [#357](https://github.com/ocx-sh/ocx/issues/357) The EC register asserts behaviour nothing verifies — two rows found false from a sample of eight

- **Labels**: —
- **Verdict**: PARTIAL · **Gate**: none (prose audit of 232 rows)
- **Size**: L — 232 rows of prose-versus-code reading across the whole shell-env subsystem. Unchanged from pass-1, but the per-row cost is lower than pass-1 assumed: each row already names an asserting test to read against.
- **Importance**: medium — not user-facing; the register is consulted as evidence and one wrong cell already argued for reinstating a form removed for corrupting values. No blocker.
- **Depends on / blocks**: none. Same defect class as `ocx-sh/ocx#352` (different document, not a dependency).
- **Pass-1 → final**: PARTIAL → **AGREE** on the verdict, but two of its evidence bullets are wrong and it missed the single most important fact about this register.

**Evidence**

- Row count confirmed: `grep -cE '^\| EC-[A-Z]+-[0-9]+ \|' .claude/artifacts/analysis_shell_env_edge_cases.md` → **234**. There are no `G-` rows; pass-1's "`EC-`/`G-`" phrasing is harmless but the set is EC-only.
- Code-verified corrections confirmed: three cells carry `**Register error, corrected against the code:**` — `:131` (EC-CONST-010 row), `:145` (EC-LIST-009 row), `:479` (EC-LIST-009's entry in the recap table). That is **two distinct rows**, as pass-1 said. The other 13 `Register error, corrected by A-NN` cells are addenda-process corrections, confirmed by reading `:107`, `:108`, `:129`.
- The document's self-statement is verbatim present: *"Cells outside this one and `EC-LIST-009` are unaudited — ocx-sh/ocx#357."*
- **Pass-1 error (line-range count).** Pass-1 claims "30 line-range citations remain, down from ~36". Actual: `grep -oE '[a-z_]+\.rs:[0-9]+-[0-9]+' … | wc -l` → **36**, exactly the owner's 2026-08-29 figure. **Nothing has been dropped.** `shell.rs:427-441` — the stale range the issue named — is still present twice.
- **Pass-1 omission (the register is now test-traced).** `test/tests/test_shell_reconcile_edge_cases.py` (4657 lines) carries a mechanized traceability gate: `_parse_register()` parses every EC row out of the register, `_tests_citing_each_row()` AST-scans `test/tests/test_shell*.py` and every `crates/**/*.rs` `#[test]`/`#[tokio::test]`, and `test_traceability_every_register_row_is_cited_by_a_real_test` fails if any row is uncited or if a Coverage cell names a vanished test. `test_traceability_the_summary_counts_match_the_register` recomputes six counts and the row total from the parsed rows. `test_traceability_no_row_is_covered_only_by_an_assertion_free_placeholder` rejects placeholder coverage.
- Consequence: the register today self-reports **`**234 rows.**`** and **`**Coverage: 234 / 234 rows cited, 231 by a test that asserts**`**, and both numbers are gated. Only three rows are exempt (`_UNCOVERED_ROWS` = `EC-FP-005`, `EC-VER-003`, `EC-VER-007`). The issue's headline — "asserts behaviour nothing verifies" — is now **false for the Coverage column** and true only for the free-text analysis/recommendation prose, which is what the two found-false cells actually were.
- That gate landed in `9309125f` (2026-08-27 21:49 +0200), i.e. inside PR #339, the same closeout this issue was filed from at 00:52 the same day. The owner's 2026-08-29 comment post-dates it and still says the audit is outstanding, so the gate does not close the issue.
- The issue's fourth suggestion ("consider whether a cell contradicting its own named test can be caught by a test") is **answered by an explicit decision in the code**, not by omission: `test_traceability_the_summary_counts_match_the_register`'s docstring states *"Deliberately scoped to the counts. The surrounding prose is not validated and should not be: gating prose is how a check becomes something people delete."*
- File history since the owner's comment confirmed: only `61f5ba12` (2026-09-01) and `0e7dba8f` (2026-09-04) touched the register, both adding/adjusting rows for the `[shell.consent]` subtree-grant feature. No audit correction landed.

**Remaining scope**

- Audit the analysis and recommendation prose of the 232 still-unaudited rows against current code. Per row the cheap oracle now exists: 231 of 234 rows name a test that actually asserts, and the traceability gate guarantees that test exists and cites back — so the check is "does the prose agree with the cited test", not a from-scratch re-derivation.
- Where a claim is wrong, use the in-cell `**Register error, corrected against the code:**` convention established at `:131` and `:145`. Do not rewrite the original claim.
- Drop all 36 remaining `<file>.rs:NNN-NNN` line-range citations, keep symbol names. None have been removed yet; `shell.rs:427-441` (the instance the issue named) is still there twice.
- Close out the "mechanize it" suggestion as **decided, not pending**: prose is deliberately ungated per the traceability test's own docstring. Record that in the issue rather than leaving it as open scope.
- Update the register's self-statement at `:131` once the audit completes, so the "cells outside this one are unaudited" sentence does not outlive its truth.

#### [#69](https://github.com/ocx-sh/ocx/issues/69) remove identifier requirement for launcher-exec root package

- **Labels**: area/package, area/package-manager, priority/low, breaking-change
- **Verdict**: NOT_STARTED · **Gate**: ADR: approach A vs B
- **Size**: XL — confirmed. ADR-class decision, a call-site map that must be rebuilt
  from scratch, a second caller the issue does not know about, and a load-bearing
  behaviour to preserve. Owner's 2026-08-29 snapshot agrees ("a model decision
  before it is a diff").
- **Importance**: low — confirmed `priority/low`. The synthetic identifier is
  internal-only and never persisted in OCI manifests
  (`package_manager.rs:579-584`), so the wrong-registry defect is latent, not
  user-visible. Note the code comment at `:579-584` argues the *collision* risk is
  negligible; it does not address the wrong-registry complaint, which stands.
- **Depends on / blocks**: none. Shares the `InstallInfo`/composer surface with #31
  and #71.
- **Pass-1 → final**: NOT_STARTED → **AGREE on verdict, OVERTURNED on remaining
  scope** (pass 1 copied the issue's call-site map verbatim; two of the files it
  names no longer exist, every line number in it is dead, and it misses that the
  synthetic identifier has since become load-bearing)

**Evidence**

- `crates/ocx_lib/src/package/install_info.rs:76-79` — `InstallInfo` declares
 `identifier: oci::PinnedIdentifier`, non-`Option`. Neither approach A nor B has
 landed. Confirmed.
- `crates/ocx_lib/src/package_manager.rs:549-599` `install_info_from_package_root`
 — I read the whole function. `:585-589` still reads the digest sidecar, builds
 `let repo_name = format!("file-url-mode/{}", digest.hex());`, and calls
 `Identifier::new_registry(repo_name, &self.default_registry)`. Both defects the
 issue names are present: the `file-url-mode/` sentinel and the
 `OCX_DEFAULT_REGISTRY`-derived registry.
- **The synthetic identifier is now load-bearing — this is the finding that
 changes the work.** `crates/ocx_cli/src/command/launcher/exec.rs:71-77` carries
 an "AF1" comment stating that because the minted `file-url-mode/<content-digest>`
 identifier means "a repo-key never matches this base", the patch resolver's
 opt-out check (which matches on repo-key **or** digest) relies on the digest leg
 to suppress a re-injected companion. Removing the synthesis must preserve that
 behaviour. The issue was written before this coupling existed and does not
 mention it. `package_manager/tasks/resolve.rs:1111` refers to the same synthetic
 mint.
- **The issue's premise sentence is out of date.** It says the single-shape
 `InstallInfo` "forces synthesis at the only caller that has no identity to give".
 There are now **two** such callers: `command/launcher/exec.rs:61` and
 `command/package_test.rs:229-231`, where `ocx package test` materialises a
 package locally and bridges to env composition through the same function.
- **Two cited consumer files do not exist.** `ls` over
 `crates/ocx_cli/src/command/` shows no `shell_profile_add.rs`, and `ls` over
 `crates/ocx_lib/src/package_manager/tasks/` shows no `profile_resolve.rs`. The
 issue cites `command/shell_profile_add.rs:75` and `tasks/profile_resolve.rs:235`.
- **Every cited line number is dead.** The issue cites `composer.rs:93,189,247,254,520`;
 `crates/ocx_lib/src/package_manager/composer.rs` is now 6578 lines. The call-site
 map has to be re-derived from scratch.
- `command/install.rs` is now reached as `ocx package install`, and
 `command/select.rs` and `command/deps.rs` still exist — so three of the five
 cited CLI report sites survive in some form.

**Remaining scope**

- Decide the shape first, as the issue says: `Option<PinnedIdentifier>` on
 `InstallInfo` (approach A) versus a `RootedInstall`/`RootlessInstall` split
 (approach B). Still ADR-class and still undecided.
- **Re-derive the consumer map before estimating.** The issue's list is stale:
 `command/shell_profile_add.rs` and `tasks/profile_resolve.rs` no longer exist,
 and `composer.rs` has grown to 6578 lines, so all five cited line numbers there
 are meaningless.
- Handle **two** identity-less callers, not one: `command/launcher/exec.rs:61` and
 `command/package_test.rs:231` (`ocx package test`).
- **Preserve the patch opt-out behaviour documented at `launcher/exec.rs:71-77`.**
 The synthetic `file-url-mode/<digest>` repo name is relied upon never to match a
 real repo-key, so the digest leg of the resolver's opt-out check is what
 suppresses a re-injected companion. Removing the synthesis without an equivalent
 is a live regression, and it needs a test.
- Then remove `install_info_from_package_root`'s identity synthesis, the digest
 sidecar read, the `self.default_registry` reference and the `file-url-mode/`
 sentinel (`package_manager.rs:585-589`).
- Composer collision reporters must render the package-root path when the owner is
 a rootless install.
- `test_cross_repo_dedup_preserves_query_repository` must pass unchanged.

#### [#144](https://github.com/ocx-sh/ocx/issues/144) glibc version floor + libc version differentiation (os.version / -version suffix)

- **Labels**: area/oci, priority/low
- **Verdict**: NOT_STARTED · **Gate**: ADR amendment first
- **Size**: XL — needs the ADR amendment before code; then platform model, wire read path, host detection, resolution and the create-time lint.
- **Importance**: low — the issue labels itself `priority/low` and "not a v1 blocker"; the failure mode is real (silent runtime break) but documented as a known escape rather than a regression.
- **Depends on / blocks**: none.
- **Pass-1 → final**: NOT_STARTED → **AGREE**. Every pass-1 citation opened and confirmed; I add one stronger in-code citation pass 1 missed.

**Evidence**

- `LibcFlavor` at `crates/ocx_lib/src/oci/host_capabilities.rs:188` is unit-variant-only (`Glibc`, `Musl`, `Unknown(String)`). Confirmed by reading the enum and its doc comment; no version field, no floor.
- `crates/ocx_lib/src/oci/platform.rs:832-836` warns and drops `os.version` on inbound conversion, with the literal message "OCX platform model has no os_version concept". Round-trip tests `serde_os_version_in_input_json_is_dropped` (`platform.rs:2190`) and `native_os_version_dropped_in_conversion` (`:2252`) pin the drop.
- **Stronger than pass 1**: `crates/ocx_lib/src/package/libc_lint.rs:99-101`, in `check_declared_libc`'s "What this does not catch" list, names exactly this issue: "**glibc symbol versions.** A binary built against glibc 2.38 and run on glibc 2.28 satisfies `libc.glibc` and still fails. `os.features` has no version vocabulary (`LibcFlavor` is deliberately unit-variant)." The gap is documented in the shipping code as a known escape.
- No host glibc-version detection: `getconf GNU_LIBC_VERSION` appears only in the two research artifacts (`research_libc_detection_methods.md:41,107`, `research_libc_detection_robustness.md:68`) and in vendored `packaging/_manylinux.py` inside the two pytest virtualenvs. Zero hits under `crates/`.
- `adr_platform_libc_os_features.md:185` records the deferral ("Versions (e.g. glibc ≥ 2.17) | **Deferred**"). No amendment has been written since.
- There is a create-time libc **family** lint (`libc_lint.rs`, PT_INTERP based, tested by `test/tests/test_package_create_libc_lint.py`) — family half shipped, version half untouched, exactly as the issue frames it.

**Remaining scope**

- Amend `adr_platform_libc_os_features.md`: version-suffix build-identifier syntax, `os.version` floor semantics, and the resolution rule "prefer the highest floor ≤ host version".
- Add a version/floor axis to the platform model without breaking the atomic-tag invariant — floors live in `os.version`, never as dotted `os.features` atoms.
- Stop dropping `os.version` in `Platform::try_from` (`platform.rs:829-836`) and persist it; update the two round-trip tests that currently assert the drop.
- Host glibc-version detection (`getconf GNU_LIBC_VERSION`) beside the existing family probes in `host_capabilities.rs`.
- Floor comparison at candidate selection time in `oci::select_best` / `is_compatible`.
- Extend `libc_lint.rs` to read ELF symbol versions (it already parses ELF via the `elf` crate) so a create-time claim can be checked, and delete the "does not catch" bullet at `libc_lint.rs:99`.

### Batch decision — Decision-gated (owner answers first)

#### [#178](https://github.com/ocx-sh/ocx/issues/178) docs(cli): declare the `--format json` output shapes stable-within-minor

- **Labels**: area/cli, priority/critical
- **Verdict**: NEEDS_DECISION · **Gate**: grant pre-1.0 stability carve-out for --format json?
- **Size**: M — the declaration is a paragraph, but the committed-schema diff gate plus the exit-code test is the real work; matches the prior on-main triage ("a stability declaration is cheap; the snapshot suite that makes it true is not"). Pass-1's S understates the CI half.
- **Importance**: critical — carries `priority/critical`, and downstream integrators (rules_ocx, vscode-ocx, ocx-sdk-python) cannot float on a minor range without it.
- **Depends on / blocks**: no code dependency. The reports schema already gives it something concrete to declare stability *over*, which lowers the cost considerably.
- **Pass-1 → final**: NEEDS_DECISION → **AGREE** on the verdict, but one load-bearing citation is wrong (below).

**Evidence**

- **Pass-1's claim "nowhere marked as a machine interface" is false.** `crates/ocx_schema/src/reports.rs` generates a published JSON Schema covering *every* `--format json` root, from the same Rust types the CLI serializes. Module doc: *"The published `--format json` report contract … so an SDK can pin its parsers against the wire format instead of against a fixture somebody typed by hand."* Canonical id `REPORTS_ID = "https://ocx.sh/schemas/reports/v1.json"` (`reports.rs:39`).
- It is documented for users: `website/src/docs/reference/configuration.md:1530` lists `--format json` output (every command) against that URL, under a heading that says OCX publishes these *at stable URLs*, followed by a paragraph explaining that the schema's `required` sets tell a parser exactly which keys are always written. That is proposal bullet 1 ("document the shapes … marked as the machine interface"), and in a stronger form than the issue asked for.
- **The stability declaration itself does not exist.** No occurrence of "stable-within-minor", "within a minor", or "minor series" in `website/src/docs`. A stable *URL* is not a stable *shape*.
- **The docs assert the opposite for one of the three named shapes.** `website/src/docs/reference/command-line.md:920`: *"**JSON shape (breaking, pre-1.0):** the value under each requested identifier is now an object, `{"path": "...", "kind": "package"|"shim"}`, rather than a bare path string."* The issue body names shape 2 as `{"<raw>":"<store-root>"}` — that shape has **already been broken** since the issue was filed. Any declaration now has to be written against today's shapes, not the issue's.
- **Proposal bullet 2 (CI compat gate) is absent.** The schemas are generated, never committed: `git ls-files website/src/public/schemas` returns nothing, and `website/schema.taskfile.yml:68-75` (`generate-reports`) writes `src/public/schemas/reports/v1.json` at build time behind a `test -f` status guard. With no committed artifact there is no diff to fail on, so a report-type change cannot red any gate. `test/tests/test_schema.py` covers the *metadata* schema only; `test_schema_generation.py` asserts `$id` URLs, not shapes. The two Rust guards that do exist (`reports_roots_are_complete`, `every_skip_serializing_if_field_is_marked`) keep the root list and the nullability markers honest — they do not detect a renamed or removed field.
- **Proposal bullet 3 (release checklist) is absent** — no release checklist file exists; `.claude/rules/workflow-release.md` has no shape-change rule.
- Project policy cuts against the ask: `CLAUDE.md` states interfaces break pre-1.0, announced in the changelog and nowhere else. `website/src/docs/roadmap.md:88` carries "Stable CLI semver" as `status="planned"`.

**Remaining scope**

- Owner decision first: grant a pre-1.0 stability carve-out for the `--format json` report shapes and the sysexit contract, or decline and tell integrators to keep pinning exact versions.
- If granted, and only then:
- Write the declaration against the *current* shapes, not the ones in this issue body — `ocx package which` already returns `{"path","kind"}`, not a bare path string.
- Commit the generated `website/src/public/schemas/reports/v1.json` and add a CI step that regenerates and diffs it, so a renamed or removed field fails the build. Today the file is build-time-only and nothing can go red.
- Pin the sysexit contract (`lock --check` → 0 / 65 / 78) in an acceptance test; `test_exit_codes.py` does not cover it.
- Add the "shape change ⇒ minor bump" rule to `.claude/rules/workflow-release.md`.

#### [#323](https://github.com/ocx-sh/ocx/issues/323) Sigstore calls fail under an HTTP proxy configured by hostname

- **Labels**: —
- **Verdict**: NEEDS_DECISION · **Gate**: proxy-host exemption vs keep refusal
- **Size**: M — the change is contained to `endpoint.rs`, but it modifies a security control and needs the policy settled plus tests that exercise a proxy.
- **Importance**: high — a total, un-workaroundable failure of `ocx package sign`, `ocx package verify` and policy-gated auto-verify for any operator behind a hostname-configured proxy, which is the ordinary corporate-network shape.
- **Depends on / blocks**: none. Same symptom family as [#407](https://github.com/ocx-sh/ocx/issues/407), different code path (the registry-pull pre-flight, not this resolver) per the maintainer's own comment.
- **Pass-1 → final**: NEEDS_DECISION → **AGREE**. Every citation checked out, including the code's self-reference to the issue.

**Evidence**

- `crates/ocx_lib/src/oci/endpoint.rs:76-90` (`.dns_resolver` at :90) — `sigstore_client_builder` installs `.dns_resolver(Arc::new(PinnedResolver))` and calls no `.no_proxy()`. reqwest's `auto_sys_proxy` therefore stays on, so `HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY` route Sigstore traffic to a proxy whose hostname the resolver then refuses. Both halves of the issue's mechanism confirmed directly, not inferred.
- `crates/ocx_lib/src/oci/endpoint.rs:235-240` — the resolver's own doc names this failure and links the issue: "with an HTTP proxy configured by hostname, the connector dials the *proxy*, which no guard ever named, so every Sigstore call fails here... tracked as https://github.com/ocx-sh/ocx/issues/323". The code documents the issue as open.
- `crates/ocx_lib/src/oci/endpoint.rs:242-262` — `PinnedResolver::resolve` returns an error for any unpinned host, and the message names the proxy possibility and links the issue. That is the shipped mitigation the issue describes, and nothing beyond it has landed.
- `git grep -F no_proxy -- crates/` returns **no hits at all** (exit 1) across the whole workspace: there is no bypass, no allowlist and no proxy-aware branch anywhere.
- The fail-closed behaviour is pinned by `the_shared_client_refuses_a_host_no_guard_approved` (`crates/ocx_lib/src/oci/endpoint.rs:501-510`), whose doc states its own discriminator ("drop `.dns_resolver(...)` and the failure becomes reqwest's own DNS error"). That test moves with whichever option is chosen.
- The 2026-09-04 maintainer comment on the issue keeps it open and separates it from [#407](https://github.com/ocx-sh/ocx/issues/407), which is the registry-pull SSRF pre-flight — a different code path with the same symptom.

**Remaining scope**

- Decide among the three options the issue names. Guarding the proxy host under the existing rules is ruled out by the issue itself: the SSRF guard refuses RFC1918, which is where every corporate proxy lives.
- If the exemption is chosen: resolve the proxy host named by `HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY` with the system resolver and add it to the pin table at client construction, exempt from the private-range refusal, and leave `resolve_sigstore_url`'s up-front guard untouched so a metadata-service URL is still rejected. Document in `PinnedResolver`'s doc comment that the rebinding half of the defence is inert under a proxy, and why.
- If the refusal is kept: say so in `PinnedResolver`'s doc as a decision rather than as a tracked defect, and drop the issue link from the runtime message.
- Either way, add a test that pins the chosen behaviour under a set `HTTPS_PROXY` (the issue's own repro command is the starting point), and update `the_shared_client_refuses_a_host_no_guard_approved` if the exemption path is taken.
- Note for whoever implements: reqwest reads proxy settings from environment variables and the Windows registry only. PAC/WPAD is not consulted, so a PAC-only deployment is unaffected by either fix.

#### [#189](https://github.com/ocx-sh/ocx/issues/189) ocx select

- **Labels**: —
- **Verdict**: NEEDS_DECISION · **Gate**: approve adr_project_toolchain_links (Option D)?
- **Size**: L once approved — multi-day across file-structure, package-manager, CLI, Windows and docs. Zero until then.
- **Importance**: medium — real pain (a running IDE's `JAVA_HOME` never refreshes; the shipped per-prompt reconciler cannot reach a non-shell process), but a working fallback exists today.
- **Depends on / blocks**: shares its ADR with #193, which the ADR covers only for the global tier.
- **Pass-1 → final**: NEEDS_DECISION → **AGREE on the verdict**, but one pass-1 citation is wrong and I correct it below.

**Evidence**

- **Pass-1 citation error**: pass 1 wrote "`ocx select` / `ocx deselect` already ship". The command is not at the root. `crates/ocx_cli/src/command/package.rs:68,70` registers `Select`/`Deselect` under the `Package` subcommand enum, so the shipped surface is `ocx package select` / `ocx package deselect`. This matters: the issue is titled `ocx select`, and a root-level toolchain selector is precisely what does *not* exist.
- The shipped command is per-package `current`-symlink switching: `crates/ocx_cli/src/command/select.rs` doc comment — "updates the per-repo `current` symlink to point at the package root". Landed in `c050ce62` (2026-02-28), four months **before** this issue was filed (2026-07-07), so it cannot be what the issue asks for. Acceptance coverage: `test/tests/test_select.py:14` `test_select_switches_current_symlink`.
- `.claude/artifacts/adr_project_toolchain_links.md` names this issue in its metadata: "[#189] (stable links from a toolchain — delivered by this ADR)". Its Context section states the per-package links are user-owned and structurally insufficient — "lock-pinned scopes must not consult `current`", "two projects pinning different digests of one repo cannot share one link".
- **Status: Proposed**, dated 2026-08-03 and rewritten 2026-09-02 against the post-reconciler tree, review rounds 2–3. Its Implementation Plan section reads in full: "Plan via `/swarm-plan` after ADR approval."
- Nothing implemented: zero hits under `crates/`, `website/`, `test/` for `OCX_TOOLCHAIN_DIR`, `OCX_NO_TOOLCHAIN_LINKS`, `ToolchainStore`, or a `.ocx/toolchain/` tree. The only `[toolchain]` config strings on main are forward-compat *test fixtures* (`crates/ocx_lib/src/config.rs:443`, `config/loader.rs:3671`) asserting that a section from a newer ocx parses without dropping known settings — not this ADR's section.

**Remaining scope**

- Owner: accept or reject the ADR. Nothing else can start; the ADR itself defers planning to after approval.
- On acceptance, `/swarm-plan` the work packages the ADR already enumerates: link facility extracted from `SymlinkStore` and parameterized on root + containment policy; `ToolchainStore` plus registration invariants; per-site `install_path` override in `composer.rs` and the `EnvScope::Project` lane (largest package); materializer with heal-before-emit on the four composing emit paths; Windows junction backend; `[toolchain]` config, the two env keys, schema regen; `options::Pinned`; docs across storage-layout, `configuration.md`, `environment.md`, `env-composition.md`, `subsystem-file-structure.md`, `subsystem-package-manager.md`.

#### [#224](https://github.com/ocx-sh/ocx/issues/224) Recommended OCI annotation set for OCX packages

- **Labels**: area/oci, area/mirror, discussion-needed
- **Verdict**: NEEDS_DECISION · **Gate**: upstream-attribution annotation key — before fleet publish
- **Size**: S — one constants file, one docs page, and the constant cull; nothing in the write path
  changes because `parse_annotation` (`crates/ocx_cli/src/command/package_push.rs:609-612`) stays
  unpoliced.
- **Importance**: medium — nothing breaks by waiting, but the upstream-key half is a cheap-now /
  expensive-later gate on the mirror fleet, so it should not ride the same indefinite deferral as
  the docs table.
- **Depends on / blocks**: blocks the `ocx-sh/ocx-mirror` fleet publish (Track E). Loosely coupled
  to `ocx-sh/ocx-mirror#19`, which already shipped its half.
- **Pass-1 → final**: NEEDS_DECISION → **AGREE** (the recommended-set table is still unstated and
  `org.opencontainers.image.documentation` is still undefined), but the remaining scope was wrong in
  three places — see Evidence.

**Evidence**

- `crates/ocx_lib/src/oci/annotations.rs:7-25` — the ten OCI constants and `sh.ocx.keywords` are
 unchanged, and there is **no** `documentation` constant. The module has since gained two more
 OCX keys the issue never saw: `LAYER_STRIP_COMPONENTS` and `LAYER_PREFIX` (`:24-25`), which is
 precedent that new `sh.ocx.*` keys get minted here without ceremony.
- **The G-2 sub-decision is SETTLED, and pass 1 under-reported it.**
 `.claude/state/announce_pointer_state_archive_2026-07-27.md:257-259`, under "Owner decisions
 still genuinely open": *"G-2 settled that `org.opencontainers.image.source` names the mirror;
 what records the upstream is unsettled. Needed before the 42-package fleet publishes, not
 before the pilot."* The open gate is therefore the **upstream-attribution key**, not the
 mirror-vs-upstream question. `.claude/state/plans/meta-plan_oci_index_alignment.md:1021` carries
 the same gate row, and `.claude/artifacts/handoff_announce_initiative_state.md:60-75` records
 that `ocx-sh/ocx-mirror#19` shipped the mirror answer as a config-overridable default.
- **Five constants are dead, not four.** `git grep -F annotations::<NAME>` over `crates/` and
 `external/` returns zero production hits for `VERSION`, `URL`, `REVISION`, `AUTHORS` **and**
 `LICENSES`. The issue's own accounting missed `URL` and `LICENSES`; pass 1 repeated the miss.
 `CREATED` is genuinely live — `crates/ocx_lib/src/oci/referrer/manifest.rs:19,123`
 (`bundle_annotations` writes it onto every signature/attestation bundle).
- `SOURCE` and `VENDOR` confirmed test-only: every hit
 (`crates/ocx_lib/src/oci/client.rs:4927,4953,5010,5024,5048,6127`) sits after the
 `#[cfg(test)] mod tests` boundary at `client.rs:2401`.
- **A production consumer of the catalog half now exists**, which the issue predates:
 `crates/ocx_lib/src/announce/pipeline.rs:471,475,521` reads `TITLE`, `DESCRIPTION` and
 `KEYWORDS` off the manifest to build the index's `desc` object, with `title` carrying a
 schema-required `minLength: 1` fallback chain. The "catalog metadata, owned by describe" row of
 the issue's table is therefore load-bearing for index rendering, not just display.
- **The issue's command citations are stale.** `ocx package describe` /
 `package_describe.rs:105-118` no longer exist; the surface is `ocx package description push|pull`
 (`crates/ocx_cli/src/command/package.rs:46`, `package_description_push.rs:120-133`).
- Docs unchanged: `website/src/docs/authoring/building-pushing.md:88-102` documents `source`,
 `revision` and `licenses` in prose only. Neither that page nor
 `website/src/docs/reference/command-line.md` mentions `image.version`, `image.vendor` or
 `image.documentation` — no recommended-set table anywhere.

**Remaining scope**

- **Owner decision (the only blocking item):** pick the key that records **upstream** provenance
 for a mirrored package. `org.opencontainers.image.source` is already settled as naming the
 mirror repo (G-2), so upstream needs its own key — an `sh.ocx.*` extension (precedent:
 `sh.ocx.keywords`, `sh.ocx.layer.*`) or a reuse of `org.opencontainers.image.base.name`.
 Decide it **before** the 42-package fleet publishes: annotations live in the image index bytes,
 so changing one afterwards moves every digest and forces a re-push plus a re-announce of every
 tag.
- **Owner decision:** ratify (or amend) the recommended-set table in the issue body as stated
 vocabulary, and confirm `licenses` stays a verbatim pass-through with no SPDX dependency.
- Then implement: add a `DOCUMENTATION` constant to `crates/ocx_lib/src/oci/annotations.rs`, and
 add the decided upstream-attribution key beside it.
- Then decide per constant: wire up or delete `VERSION`, `URL`, `REVISION`, `AUTHORS`, `LICENSES`
 (five, not four — `CREATED` is now used by the referrer bundle path).
- Then document: add the recommended-set section to
 `website/src/docs/authoring/building-pushing.md` beside `#source-annotation`, naming
 `ocx package description push` (not the retired `describe`) as the owner of the
 title/description/keywords row.

#### [#364](https://github.com/ocx-sh/ocx/issues/364) shell: should a consent stamp cover the project's [env] table, not just its lock sources?

- **Labels**: area/cli, priority/low, discussion-needed
- **Verdict**: NEEDS_DECISION · **Gate**: (a) by-design / (b) adopt fb/envdrift + supersede S-005/S-009 / (c) report-only
- **Size**: XL — options (b) and (c) both need an ADR supersede before any code can land, which is the brief's XL bar. Option (a) is free.
- **Importance**: medium — a real consent-model gap, deliberately specified rather than accidental, with an explicit `discussion-needed` label and `priority/low` on GitHub.
- **Depends on / blocks**: [#339](https://github.com/ocx-sh/ocx/pull/339); `.claude/artifacts/adr_shell_env_overhaul.md` must change before (b) or (c).
- **Pass-1 → final**: NEEDS_DECISION → **AGREE**. Labelled `discussion-needed`; the issue records a tension the owner deliberately left open with three options, one of which is uncosted.

**Evidence**

- `crates/ocx_lib/src/project/consent.rs:661` — clause 1 is still `stamp.project_dir == project_dir && sources.is_subset(&stamp.sources)`. An `[env]`-only edit is not covered.
- `crates/ocx_lib/src/project/consent.rs:155-160` — `authorizes_project_env()` returns `true` for `Grant::Stamp` and `Grant::Path`, `false` only for `Grant::Namespace`. So the gap stands exactly as reported: the package channel refuses on drift, the `[env]` channel does not.
- The current behaviour is pinned as the specification. `test/tests/test_shell_reconcile.py:639` (`test_a_changed_env_value_resolves_at_the_next_prompt`, parameterized across shells) and `test/tests/test_shell_reconcile_edge_cases.py:869` (`test_ec_fp_003_env_only_edit_with_lock_untouched_recomposes`) both assert that an `[env]` edit applies at the next prompt.
- The reverted implementation is real and recoverable. Local branch `fb/envdrift` exists; `bb203305` is `fix(shell)!: refuse a project [env] the consent stamp never saw`, 10 files, 629 insertions / 77 deletions, touching `consent.rs`, `activation.rs`, a new `project/hash.rs`, `shell_allow`, `shell_state` and one pytest module. `git merge-base --is-ancestor bb203305 origin/main` returns false — not landed, not pushed.
- No commit or PR on main mentions #364.

**Remaining scope**

this is the decision, not a code gap. Costs per option:
- **(a) Leave as specified.** Zero cost. S-005 / S-009 stand; close as by-design. The exposure needs a persisted `$OCX_HOME/state/` *and* an `[env]` edit arriving with none of the six stamp writers running.
- **(b) Adopt `fb/envdrift`.** The code exists on the local branch. Requires superseding S-005 / S-009 in `.claude/artifacts/adr_shell_env_overhaul.md` **first**, then rewriting the 32 acceptance tests that assert the current behaviour. C-044 is not an obstacle — the reverted state measured 4.535 ms against the then-6.1 ms budget with the injected red still failing as required.
- **(c) The uncosted middle.** Keep applying the edit but *report* the drift in `ocx shell state` and on the prompt line. Nobody has scoped this; it would still need the `env_hash` machinery from (b) minus the refusal, so most of `bb203305` is reusable.

#### [#348](https://github.com/ocx-sh/ocx/issues/348) record_origin mints a namespace-consent marker without wire contact

- **Labels**: —
- **Verdict**: NEEDS_DECISION · **Gate**: persisted-format for pre-existing origin markers (3 options)
- **Size**: L — the diff is ~55 lines, but it changes the OCI resolve path and cannot land until a persisted-format decision is recorded; pass-1's M understates the gating. (Not XL only because the owner has already reduced the design space to three named options.)
- **Importance**: medium — a real weakening of `Grant::Namespace`'s evidentiary strength with a concrete chain, but step 2 of that chain is not attacker-controlled and the owner graded it below the #339 blocking bar.
- **Depends on / blocks**: relates to [#339](https://github.com/ocx-sh/ocx/pull/339) (merged, deferred this deliberately), [#344](https://github.com/ocx-sh/ocx/issues/344) (comparison unaffected).
- **Pass-1 → final**: NEEDS_DECISION → **AGREE** (verdict correct; size raised, see below)

**Evidence**

- `crates/ocx_lib/src/package_manager/tasks/pull.rs:334` — `let from_registry = provided_metadata.is_none();`, still the sole write gate. Consumed at `pull.rs:451` (`if from_registry { file_structure::record_origin(...) }`).
- The two store-hit early returns at `pull.rs:344` and `pull.rs:381` are both conditional on `check_install_status`, so an absent-or-not-OK package dir falls through to the fetching branch exactly as the issue describes. Unchanged.
- `crates/ocx_lib/src/file_structure/package_store.rs` last commit is `9309125f` (the PR [#339](https://github.com/ocx-sh/ocx/pull/339) merge). No follow-up.
- The "also in scope" half is already done: `package_store.rs:405-420`'s `record_origin` doc states the weak claim verbatim ("A marker is evidence that **this host** ran a fetching pull … It is **not** evidence that a registry vouched for that binding"), and `refs_origins_dir` at `:98-107` matches. So nothing on main overstates the gate while this stays open.
- No commit or PR on main mentions #348.

**Remaining scope**

- Decide the persisted-format question for pre-existing origin markers. The owner named three options: a prefixed payload, a sibling `refs/origins-wire/` directory, or accepting that tightening permanently drops clause-2 grants for already-materialized packages (a warm package never re-reaches `record_origin` past the two store-hit early returns, so the drop is permanent, not until-next-pull).
- Then: record a manifest-provenance signal where `ChainedIndex` already knows a configured source answered, surface it through a defaulted `IndexImpl` method, and consume it at `pull.rs:334` in place of `provided_metadata.is_none()`. The owner sized this at ~55 production lines across four files.
- Do **not** use a "did any layer fetch touch the wire" boolean — the issue rules it out explicitly, because a legitimate fully-cached re-pull would then stop recording and silently deny consent.
- After it lands, restore `package_store.rs`'s strong wording (the doc comments at `:98-107` and `:405-420`).

#### [#392](https://github.com/ocx-sh/ocx/issues/392) copy: promoting a cosign-signed package to a referrers-less registry can never exit 0

- **Labels**: area/oci, priority/low, discussion-needed
- **Verdict**: NEEDS_DECISION · **Gate**: lane 1 document / 2 relax gate / 3 write fallback index
- **Size**: lane 1 XS; lane 2 S; lane 3 **M** — revised down from the issue's XL
  because the writer, its caps, its retry policy and its tests already exist and
  the D4 reversal is already ratified.
- **Importance**: medium — blocks promotion of any signed package to plain
  `registry:2`, older Artifactory/Nexus and local dev registries; reporter
  labeled it `priority/low`.
- **Depends on / blocks**: reuses the #356 / Amendment 10 write path. Not blocked
  by #391.
- **Pass-1 → final**: NEEDS_DECISION → **AGREE on the verdict, OVERTURNED on the
  sizing.** Pass 1 kept the issue's own "lane 3 = XL, reverses ratified decision
  D4, needs an ADR" and called Amendment 10 merely "relevant context". That
  understates it: D4 was already reversed, on 2026-08-29, and the writer lane 3
  needs already exists and is tested.

**Evidence**

- Ordering confirmed: the sidecar sweep runs at `crates/ocx_lib/src/oci/copy.rs:257-261`,
 the gate at 275, with the comment at 250-256 stating the position *is* the
 contract (C-091). Both are gated on the same `include_referrers`, so
 `--no-referrers` still skips the sweep as well as the gate.
- `ensure_target_serves_referrers` (`copy.rs:310-325`) returns
 `Err(ClientError::ReferrersUnsupported)` unconditionally on
 `ReferrersSupport::Unsupported`. No carve-out for "every referrer already
 landed as a sidecar".
- **Why lane 3 is no longer XL:** `append_referrer_fallback_index` is a
 **default method on the `OciTransport` trait**
 (`crates/ocx_lib/src/oci/client/transport.rs:698`), built on
 `pull_referrer_fallback_index` and `push_manifest_raw`, with optimistic
 read-back and bounded retry, the descriptor-ceiling refusal before the PUT
 (712-720), and `RegistryTransient` (75) rather than 84 on exhaustion. The
 copier already holds that transport (`self.client.transport()`, used at
 `copy.rs:316`). Tests exist:
 `two_writers_racing_one_fallback_index_both_land` (transport.rs:1386) and
 `the_fallback_write_lands_at_the_spec_tag_with_artifact_type_and_annotations`
 (1471).
- The D4 lost-update race the issue cites as the blocker is the same race
 Amendment 10 accepted and mitigated for the sign path. The project-level
 policy call has been made; what is left is whether *copy specifically* may
 author a manifest, given `copy.rs:335`'s "same bytes, same digest, same tag,
 or nothing" rule.

**Remaining scope**

- **Lane 1 (document only)** — state the limitation in `ocx package copy
 --help` and `website/src/docs/reference/command-line.md`. Say plainly that a
 signed package cannot be promoted to a registry without the Referrers API
 and exit 0, and that `--no-referrers` also drops the sidecar sweep.
- **Lane 2 (relax the gate)** — list the source's referrers before the gate
 decision; skip the refusal when every one of them is already representable
 as a cosign sidecar tag that the sweep carried. Needs careful contract
 wording for the mixed case.
- **Lane 3 (write the fallback index)** — call the existing
 `OciTransport::append_referrer_fallback_index` for each referrer the copy
 pushed, instead of refusing. Record the decision as an amendment to
 `adr_oci_referrers_signing_v1.md` extending Amendment 10 from the sign path
 to the copy path; no new ADR is needed for D4 itself.
- Whichever lane is taken, split `--no-referrers` so an operator can keep the
 sidecar sweep while skipping the referrers gate, or document that they are
 deliberately one switch.

#### [#359](https://github.com/ocx-sh/ocx/issues/359) Consider a hookless shims mode as an alternative to per-prompt reconciliation

- **Labels**: —
- **Verdict**: NEEDS_DECISION · **Gate**: close-or-watch; "sub-ms no-op" premise falsified (4.8 ms quiet / 7.6 ms CI)
- **Size**: XL — the reviewer sized a shims mode at ~150+ LOC of new surface with its own staleness, ordering and Windows-shim questions; it would need a design pass before any of it is real.
- **Importance**: medium — raised from pass-1's low. The grading that made it LOW rested on a sub-millisecond claim that measurement has since falsified, so the decision itself now deserves a fresh look even if the answer stays no.
- **Depends on / blocks**: watches [#360](https://github.com/ocx-sh/ocx/issues/360) (the budget it uses as its instrument) and [#362](https://github.com/ocx-sh/ocx/issues/362) (a named contributor to the per-prompt cost); relates to [#339](https://github.com/ocx-sh/ocx/pull/339).
- **Pass-1 → final**: NEEDS_DECISION → **AGREE** on the verdict, but pass-1 missed the decisive evidence and understated the case. It framed trigger 1 as "technically" fired via budget movement; the stronger fact is that the issue's *own argument against building* is now falsified by measurement.

**Evidence**

- The issue's argument against building is "ocx's no-op path is already sub-millisecond, which is cheaper than the cost shims mode exists to avoid in mise". That is no longer true. `test/bench/shell_latency.py:498-531` records the measured steady-state per-prompt reconcile Δ: **4.82 and 4.96 ms** median on a quiet dev box across eight interleaved pairs, and **7.590 ms** worst on a GitHub runner (`_WORST_KNOWN_GOOD_RECONCILE_MS = 7.590`, `shell_latency.py:2498`).
- That Δ is the *converged, no-op* prompt, not a first entry. `measure_wall_clock` (`shell_latency.py:1450-1477`) runs a warm-up pass of every command before sampling, and `_fixed_point_gates` (`:824-866`) asserts `steady_applies == 0` — a steady-state prompt emits no PATH line at all and still costs ~5 ms above a bare `ocx version`.
- `RECONCILE_BUDGET_MS = 10.0` (`shell_latency.py:545`). Four moves: 3.0 → 3.5 → 6.1 → 10.0, two of them since this issue was filed on 2026-08-27. The fourth is explicitly no longer derived from measurement — `shell_latency.py:531-537` calls 10 ms "a chosen ceiling … the product answer to how long a prompt may take".
- Counter-evidence, which is why this is a decision and not a finding: commit [`f58e6531`](https://github.com/ocx-sh/ocx/commit/f58e6531783a03035e1618dbc7e281a8a277dd05) attributes the 6.1 → 10.0 move to the CI runner being slower, not to the reconciler degrading — the same binary benched alternately at two commits on one quiet box gave medians inside each other's spread.
- No shims-mode code exists. `crates/ocx_shim` is the Windows launcher shim, unrelated to tool resolution; no PATH-shim resolution path exists anywhere in `crates/`.

**Remaining scope**

none to implement. This is a close-or-reopen decision.

#### [#316](https://github.com/ocx-sh/ocx/issues/316) auto-verify: trust-service fan-out inherits the unbounded dependency pull, with no cap of its own

- **Labels**: performance, area/package-manager, discussion-needed
- **Verdict**: NEEDS_DECISION · **Gate**: cap width for trust-service fan-out
- **Size**: S — one contained entry point, one shared struct that already holds cross-task state.
- **Importance**: medium — a dependency-heavy `[[trust.policy]]` bursts a public Fulcio/Rekor deployment proportional to the transitive graph from a single `ocx install`; blast radius is limited today because auto-verify is opt-in via trust policy.
- **Depends on / blocks**: none.
- **Pass-1 → final**: NEEDS_DECISION → **AGREE** on the verdict; **size corrected L → S**.

**Evidence**

- `crates/ocx_lib/src/package_manager/tasks/pull.rs:259` — `setup_impl` calls `mgr.maybe_auto_verify(pinned.as_identifier())` unconditionally at the metadata-first seam, and its own comment states it "fires for the root and every transitive dependency".
- `crates/ocx_lib/src/package_manager/tasks/pull.rs:671-679` — `setup_dependencies` spawns one task per dependency into a bare `JoinSet`, no permit acquisition of any kind. Unbounded exactly as described.
- `crates/ocx_lib/src/package_manager/tasks/pull.rs:159-176` — the only `acquire_permit` in the file guards the **outer** root dispatch inside `pull_all`; the doc comment at `pull.rs:135-138` states the inner fan-out is deliberately unbounded to avoid an ancestor-permit deadlock.
- `grep -rn "Semaphore\|buffer_unordered\|max_concurrent"` across `package_manager/tasks/auto_verify.rs`, `oci/verify/` and `oci/endpoint.rs` returns exactly one hit, and it is a **connection-pool idle cap**, not a concurrency cap: `endpoint.rs:88` `.pool_max_idle_per_host(SIGSTORE_MAX_IDLE_PER_HOST)`. Nothing bounds in-flight Sigstore requests.
- **Size correction**: the fix is contained, not multi-subsystem. `maybe_auto_verify` (`auto_verify.rs:166`) is a single entry point that early-returns before any network work when no policy covers the target (`auto_verify.rs:177-182`), and the `AutoVerify` struct it reads already carries shared cross-task state (`warned: AtomicBool`, `auto_verify.rs:187`). An `Arc<Semaphore>` field acquired after the policy check is a few lines in one file. The decision is the gate here, not the code.

**Remaining scope**

- Decide whether trust-service fan-out gets its own cap, and at what width.
- If yes: add a `Semaphore` to the `AutoVerify` struct and acquire a permit inside `maybe_auto_verify`, after the `policies.is_empty()` early return so uncovered packages pay nothing.
- Scope the permit so it is never held while awaiting a descendant's pull permit — the ancestor-deadlock argument that justifies leaving the inner pull unbounded does not apply to a verify-scoped semaphore.
- Add a test that a dependency-heavy install with a covering `[[trust.policy]]` never exceeds the cap in concurrent trust-service dials.

#### [#320](https://github.com/ocx-sh/ocx/issues/320) verify: --format json emits certificate identity fields unsanitized

- **Labels**: —
- **Verdict**: NEEDS_DECISION · **Gate**: exception to verbatim-JSON for certificate fields?
- **Size**: S — the sanitizer, `is_bidi_control` and `is_zero_width` all exist; this is a call-site change plus a test row.
- **Importance**: medium — Trojan-Source-class reordering of the exact string an operator reads to decide whether to trust a signature; requires an attacker-controlled Fulcio SAN and an operator rendering raw JSON to a terminal.
- **Depends on / blocks**: none. Any ruling here should be written back into `data.rs:129-133` so the policy states whether it is field-blind.
- **Pass-1 → final**: NEEDS_DECISION → **AGREE**. The decision is genuine: sanitizing breaks a documented wire guarantee, so it is a trade-off, not a defect with one right answer.

**Evidence**

- `crates/ocx_cli/src/api/data/verification.rs:119` and `:121` — `certificate_identity` and `certificate_oidc_issuer` are plain `String` fields on a `#[derive(Serialize)]` struct, no field-level sanitizer.
- `crates/ocx_cli/src/api/data/verification.rs:178-197` — `plain_fields` routes both through `sanitize_for_terminal`, but only for the plain table.
- `crates/ocx_cli/src/api/data/verification.rs:217-224` — `print_json` hands `self` to `render_success_envelope` and prints, with no sanitization step. `DataInterface::print_json` (`crates/ocx_lib/src/cli/data_interface.rs:187-196`) uses `serde_json::to_string_pretty` / `colored_json` with default escaping, so nothing escapes non-ASCII. The issue's finding is exact.
- `crates/ocx_cli/src/api/data.rs:129-133` documents the opposite policy: "**stdout, `--format json`** — deliberately **not** covered. That is a machine channel and carries the key verbatim by design... raw C1 and raw bidi *do* reach a terminal that renders JSON directly; that is the accepted cost of the verbatim guarantee."
- **Provenance of that policy, verified**: `git log -S'accepted cost of the verbatim guarantee' -- crates/ocx_cli/src/api/data.rs` returns exactly one commit, `7aa00f9f` (2026-08-10), subject `feat(index): snapshot a whole registry into a servable lo…`. It was written about index-key payloads, eight days before #320 was filed, and does not weigh the Fulcio-certificate threat model. It is a policy that happens to cover this field, not a ruling on it.
- `crates/ocx_cli/src/api/data/verification.rs:40` — `SignatureEntry`'s doc asserts "Nothing here is exposed today: this array is JSON-only, and `serde_json` escapes C0 controls." That is the exact reasoning #320 refutes: C0 escaping neutralizes the OSC/CSI half, not the bidi half. The doc is honest about escape injection and wrong about display reordering.
- No test covers it: `verification.rs`'s test module contains no bidi or control-character fixture on the JSON path.
- **Doc-drift, adjacent, worth one line when this is touched**: `data.rs:135-137` says zero-width characters are "Out of scope by choice", but `sanitize_for_terminal` filters `is_zero_width` (`data.rs:203-205`, covering U+200B/C/D and U+FEFF) and the implementation comment at `data.rs:171-181` argues at length that dropping that filter would re-open half the finding. The paragraph is stale for those four codepoints; U+2060, U+00AD and the tag block genuinely remain out of scope.

**Remaining scope**

- Decide: keep the verbatim-JSON policy and close as won't-fix, citing `crates/ocx_cli/src/api/data.rs:129-133`; or carve an exception for the two registry-supplied certificate fields.
- If the exception is chosen: strip the bidi and zero-width set from `certificate_identity` and `certificate_oidc_issuer` on the JSON path, and decide whether `SignatureEntry`'s per-row copies of the same two fields get the same treatment (they are the same attacker input).
- Either way, correct `SignatureEntry`'s doc claim that "nothing here is exposed today" — C0 escaping does not cover the bidi class.
- If the exception is chosen: pin it with the existing per-attack-class corpus in `data.rs`'s test module, which today has no JSON-path row.

#### [#318](https://github.com/ocx-sh/ocx/issues/318) cli: the JSON error envelope's reserved 'remediation' field is never populated

- **Labels**: area/cli, discussion-needed
- **Verdict**: NEEDS_DECISION · **Gate**: populate remediation or delete the field
- **Size**: XS–S once decided — roughly three lines plus doc to delete, or a remediation string per covered kind plus one pinning test.
- **Importance**: low — no consumer is currently misled, because the key is absent rather than empty. Correctly labelled `discussion-needed`.
- **Depends on / blocks**: none. Shares the envelope contract-test discipline with #322 but is independently fixable.
- **Pass-1 → final**: NEEDS_DECISION → **AGREE** (evidence confirmed; the issue's own premise needs correcting before the owner decides)

**Evidence**

- `crates/ocx_cli/src/error_envelope.rs:174` — `render_error_envelope` hard-codes `remediation: None`. The test module starts at `:279`, so `:174` is production.
- `crates/ocx_cli/src/error_envelope.rs:82-86` — the field's doc still calls it reserved. The only non-`None` values in the tree are test fixtures at `:315` and `:326`.
- `collect_context` (`error_envelope.rs:189-219`) collects `identifier` from `SignError` / `VerifyError` and `source` / `target` from `CopyError`. Nothing remediation-shaped is gathered anywhere.
- **Correction to the issue's premise, and it lowers the stakes.** The field carries `#[serde(skip_serializing_if = "Option::is_none")]` at `:85`, and `error_envelope_omits_none_detail_and_remediation` (`:377`) pins that. The key never appears in `--format json` output. The issue says "Shipping a permanently-empty field in a documented envelope is the one option that is wrong" — nothing empty is shipped; the key is simply absent.
- **Correction on "the docs".** The user-facing reference at `website/src/docs/reference/command-line.md:33-39` documents the envelope as `schema_version`, `command`, `exit_code` and an `error` object, and never mentions `remediation`. The only place it is promised is the frozen-v1 stability contract in `.claude/artifacts/adr_oci_referrers_signing_v1.md:850`, which lists `error.detail` and `error.remediation` as *optional* — and whose worked example at `:836` shows it populated with the very string that survives only as a test fixture.

**Remaining scope**

- Decide: populate `remediation` for the error kinds with a one-line fix, or delete the field and the reserved-field language in `error_envelope.rs:32-34` and `:82-86`.
- Note before deciding that the key is already omitted from output by `skip_serializing_if`, so no consumer sees an empty field today. The gap is between the ADR's frozen-shape contract, which lists `error.remediation` and shows it populated, and an implementation that never sets it.
- If populated: pin the covered-kinds set with a test, the same way the `detail` slug tables are pinned. That set becomes part of the JSON contract.
- If deleted: `ENVELOPE_SCHEMA_VERSION` does not need a bump, since the key has never been emitted, but the ADR's frozen-shape section needs the field struck.

#### [#288](https://github.com/ocx-sh/ocx/issues/288) feat(index): explicit whole-source sync / override commands

- **Labels**: —
- **Verdict**: NEEDS_DECISION · **Gate**: amend index ADR to allow destructive override, or close
- **Size**: XS for the decision itself; L for the implementation if approved (own grammar,
  confirmation semantics, exit codes, plus an ADR amendment).
- **Importance**: low — no reported pain since bulk sync landed, and the missing verb is a
  deliberately deferred destructive operation, not a gap anything is waiting on.
- **Depends on / blocks**: nothing depends on it; it is blocked on the owner ruling above.
- **Pass-1 → final**: PARTIAL → **OVERTURNED**. Nothing here is an open implementation task. Bulk
  sync shipped; the override verb is unbuilt because it collides head-on with a ratified invariant,
  so it needs an owner ruling rather than a planning slot — and if the ruling is "no", the issue
  closes instead of waiting for a builder. The 2026-08-29 sweep reached the same label independently
  (`.claude/artifacts/analysis_issue_triage_2026-08-29.md:154`, classified `DECISION`).

**Evidence**

- Bulk sync shipped and is live: `crates/ocx_cli/src/command/index.rs:76` declares
 `Index::Sync` (its contract stated in the doc comment at `:41-75`), implemented at
 `crates/ocx_cli/src/command/index_sync.rs:25-36`. Both cited
 commits exist — [`7aa00f9f`](https://github.com/ocx-sh/ocx/commit/7aa00f9f) *"feat(index):
 snapshot a whole registry into a servable local index"* and
 [`b8b72e86`](https://github.com/ocx-sh/ocx/commit/b8b72e86) *"perf(index): make `ocx index sync`
 fast and survive a transient failure"*.
- Acceptance-tested: `test/tests/test_index_servable_snapshot.py` drives `ocx index sync` at
 lines 335, 376, 553, 614, 677, 713 and asserts the `--frozen` refusal at 718 and 735. (Pass 1's
 line numbers for this file were all wrong — 282/518/634/695 — though its claim about coverage
 holds.)
- The override verb is absent: `git grep -F force-snapshot` over `crates/`, `.claude/` and
 `website/` returns nothing, and the whole subcommand tree is
 `{Catalog, List, Update, Sync, Regenerate}` (`crates/ocx_cli/src/command/index.rs:9-106`).
- **One pass-1 claim is wrong**: "no destructive command exists anywhere in
 `command/index*.rs`". `ocx index regenerate` does delete — `index.rs:56` and `:78-105` document it as
 "the only command that drops anything", namely a catalog entry whose root document is already
 gone. It is not the override verb (it re-derives from local disk and consults no source), but a
 reviewer should not be told nothing deletes.
- **The invariant conflict is real and written down.**
 `.claude/artifacts/adr_index_indirection.md:1029-1103` is the owner-doctrine amendment: the
 local copy **is** the package-tier lock; "**Neither scope deletes:** a tag the remote stopped
 listing survives locally with its pinned digest … otherwise a publisher retiring a version would
 silently break every machine still pinned to it". The same amendment says "There is no
 whole-index sync, and none is added" and "do not re-propose" the `--all` flag — a clause the
 shipped `ocx index sync` already supersedes, which is itself evidence that this ADR's
 prohibitions are the owner's to lift, not an implementer's.

**Remaining scope**

- **Owner decision, and nothing else until it lands:** may a command exist that adopts a source's
 current tag list verbatim and drops locally-retained tags the source deleted? That is precisely
 what the package-tier-lock invariant
 (`.claude/artifacts/adr_index_indirection.md:1029-1103`) forbids, so approving it means
 amending that ADR — stating that the invariant binds *implicit* writes and that one explicit,
 named, confirmation-gated verb may break it.
- If approved, the design must then answer: the command's grammar and naming unit (the issue
 fixes it at `(package, tag)`), whether the destructive set is enumerated and confirmed before
 any write, its exit-code story, its behaviour under `--frozen` and `--offline` (both should
 refuse, matching `sync`), and how the authored local catalog is rewritten publish-style.
- If declined, close the issue: bulk sync already covers the non-destructive half, and the
 decision record belongs in the ADR rather than in an open ticket.

#### [#192](https://github.com/ocx-sh/ocx/issues/192) rules multi-package

- **Labels**: —
- **Verdict**: NEEDS_DECISION · **Gate**: (a) list attribute, (b) composing form, or close as satisfied
- **Size**: S for (a) — additive attribute in two satellite files. M for (b) — a new composition form plus a collision policy in both repos.
- **Importance**: medium — a build declaring many tools without an `ocx.toml` writes one block per tool; annoying, not blocking.
- **Depends on / blocks**: none. Same satellite-repo pair as #191.
- **Pass-1 → final**: NEEDS_DECISION → **AGREE on the verdict**; pass 1's key citation does not support its claim, and it missed an in-repo triage record that settles the intended reading.

**Evidence**

- **Pass-1 citation error**: pass 1 cites "the worked example at `ocx.cmake:21`" as confirming the repeat-call pattern. That line is a single call, `ocx_package(NAME jq PACKAGE ocx.sh/jq:latest)`, inside the module's header comment. It demonstrates nothing about repetition.
- The claim is nevertheless true, on better evidence I verified myself. CMake: `find_ocx` `examples/package/CMakeLists.txt` makes three `ocx_package()` calls in one file (`jq`, `jq_pinned`, `jq_frozen`), each exporting its own `OCX_<NAME>_RUN_<BIN>`. Bazel: `rules_ocx` `examples/package/MODULE.bazel` declares four `ocx.package()` tags and ends with `use_repo(ocx, "jq", "jq_frozen", "jq_lazy", "jq_pinned")`; `extensions.bzl:277` iterates `for tag in mod.tags.package:`.
- Neither toolchain accepts a list in one call. `_package` tag_class is documented "Provisions a **single** OCX package from an OCI registry" with `name` and `package` both `mandatory = True` (`extensions.bzl:122-124`, `:180`). `ocx_package`'s RST block says "Provisions a single OCX package from an OCI registry" and its parse takes singular `NAME;PACKAGE` (`ocx.cmake:1063`).
- **Pass 1 missed this**: `.claude/artifacts/analysis_issue_triage_2026-08-29.md:112` already characterizes the issue in this repo — "neither rule set can declare more than one package per invocation; work lands in two satellite repos". So the batch-API reading is the recorded one, not a guess.
- Repetition already worked on the day the issue was filed. I read `extensions.bzl` at `ea9005d6` (v0.1.2, released 2026-07-07, the filing date): the identical `for tag in mod.tags.package:` loop is at `:163`. The issue therefore cannot be asking for repetition support.
- There is no in-between form: `ocx_project` / `ocx.project` compose N tools but require an `ocx.toml` + `ocx.lock`; `ocx_package` / `ocx.package` take one registry-direct package and give it its own isolated repo/env. Nothing composes several registry-direct packages into one environment.

**Remaining scope**

- Owner picks (a), (b) or "close as satisfied". Nothing can be scoped before that; the one-line body admits all three readings and the issue has no comments.
- If (a): add a repeated/list attribute to `_package` in `rules_ocx` and a multi-value `PACKAGES` keyword to `ocx_package()` in `find_ocx`, each entry still needing its own repo name; keep the singular form working.
- If (b): a new composing form in both repos that resolves N identifiers into one env — larger, and it needs a decision on entrypoint-collision policy across the composed set first.

#### [#397](https://github.com/ocx-sh/ocx/issues/397) initializing / allowing ocx.toml does not auto-load

- **Labels**: —
- **Verdict**: NEEDS_DECISION · **Gate**: reporter's `ocx shell state --format json` + shell needed
- **Size**: S — each candidate resolution is a single-file change plus one test; the
  blocker is the decision, not the work.
- **Importance**: low — the core flow is acceptance-proven working, every designed
  non-activation path prints a hint naming the fix, and there is no reproducer to act on.
  Raise it the moment a transcript arrives.
- **Depends on / blocks**: none.
- **Pass-1 → final**: OBSOLETE → **OVERTURNED**. Every mechanism pass 1 cites as proof the
  issue is moot shipped **in the release the reporter measured**, four days before the
  issue was filed. An issue cannot be obsoleted by code that was already running when it
  was reported.

**Evidence**

- **The chronology is decisive.** `9309125f feat(shell)!: reconcile the environment on
 every prompt` is dated **2026-08-27** and is the commit that *added*
 `crates/ocx_lib/src/shell/hook.rs`, `crates/ocx_lib/src/activation.rs`,
 `shell_allow.rs` and `shell_state.rs` (`git log --diff-filter=A` over all four returns
 that one commit). `e48ef73c release: v0.6.0` is **2026-08-31**. #397 was filed
 **2026-09-02**, and its sibling #398 states "measured on ocx 0.6.0". So the per-prompt
 reconciler, the consent stamp, the `ocx shell allow` gesture and the "open a new shell
 prompt" message were all in the binary that produced the report.
- **Nothing since changes activation.** `git log origin/main --since=2026-09-02` over
 `shell/`, `activation.rs`, `shell_state.rs` and `project/` returns two commits:
 `0e7dba8f` (a test) and `84085eb8`, whose own message scopes it to *reporting* a
 malformed `[shell.consent] paths` entry, not to whether a project activates. No commit
 anywhere mentions #397.
- **The happy path is acceptance-proven, so a blanket "it does not auto-load" is false.**
 `test/tests/test_shell_reconcile.py:1684`
 `test_shell_allow_consents_a_clone_and_revoke_takes_it_back` asserts the clone is inert
 (`inert_reason == "no_stamp_no_grant"`) before `ocx shell allow`, that the stamp file
 lands, and that the **very next** reconcile emits the project's own env
 (`"export WP14_CONST='alpha'" in emitted.stdout`) — then that revoke reverses all three.
 It uses `_locked_project`, i.e. a project that has an `ocx.lock`.
- **`ocx init` provably cannot activate anything, for two independent reasons.**
 (a) It writes no consent stamp: the only two callers of `consent::record` are
 `crates/ocx_cli/src/app/project_context.rs:383` (the six mutating commands) and
 `crates/ocx_cli/src/command/shell_allow.rs:69`; `command/init.rs:31` calls
 `init_project_at_default` and nothing else. (b) It writes no `ocx.lock`, and
 `activation.rs:551-557` refuses composition with `LockCurrency::Missing` when the lock
 is absent. So `ocx init` alone leaves a project inert by design, and the title's first
 word is literally accurate.
- **Neither refusal is silent.** An unconsented project prints
 `"ocx: {dir} is not activated; run \`ocx shell allow\` to consent, or \`ocx shell state\`
 to see why"` (`activation.rs:666-672`); a missing lock is caught at
 `crates/ocx_cli/src/command/self_group/activate.rs:331-345` and rendered through
 `refusal_lines` (`:441-449`), deduped by `messages_fp` so it prints once, not every
 prompt. Both landed in `9309125f` too.
- **Three designed configurations do produce exactly the reported symptom**, and any of
 them could be what was observed:
 1. **direnv or mise live for that directory.** `shell/coexistence.rs:29-39, 74+`
 yields on `DIRENV_DIR` naming the resolved project dir, or on `MISE_SHELL` /
 `__MISE_ORIG_PATH`; `activation.rs:647-650` then narrows to the global scope and
 reverts the project scope entirely. This repo's own `.envrc` makes that the owner's
 default shell state.
 2. **A shell with no per-prompt hook.** `website/src/docs/in-depth/shell-integration.md`
 ("Per-shell coverage" table) records ash, dash, ksh and Batch as shell-start only,
 and nushell as global-scope only — "no project reconcile, no revert, no consent
 gate, today".
 3. **No lock**, as above.
- unverified: which of the three (or a fourth, genuine defect) the reporter hit. The
 issue body is empty — no shell named, no `ocx shell state` output, no command
 transcript. Nothing in the repo records the observation.

**Remaining scope**

- **Decide what was actually observed.** Ask for the shell, `ocx shell state --format json`
 in the affected directory, and whether `DIRENV_DIR` / `MISE_SHELL` were set. If it is
 configuration 1, 2 or 3 above, the answer is documented behaviour and the issue closes.
- If the ask is the title read literally — **`ocx init` should consent** — implement it:
 call `consent::record` from `command/init.rs` alongside `init_project_at_default`, the
 same call `project_context.rs:383` makes. Creating `ocx.toml` in a directory is at
 least as strong a gesture as the `ocx add` that already consents. Add the paired
 acceptance assertion (stamp absent before `ocx init`, present after).
- If the complaint is the wording, replace `"consented to {dir} - open a new shell prompt"`
 (`shell_allow.rs:75`) with text that says the next prompt in *this* terminal, and say so
 in `shell-integration.md` too — "open a new shell prompt" reads as "open a new terminal".
- If it is the direnv yield, decide whether `ocx shell allow` should warn when
 `DIRENV_DIR` names the directory being consented, since the stamp it writes will have
 no visible effect there.

#### [#34](https://github.com/ocx-sh/ocx/issues/34) feat: mise backend plugin for OCX

- **Labels**: area/package-manager
- **Verdict**: NEEDS_DECISION · **Gate**: pursue mise backend or close (mise is now a competitor)
- **Size**: L if pursued — entirely greenfield, in a repository that does not exist,
  in a language (Lua) this project ships nowhere else.
- **Importance**: low — a speculative reach play whose stated rationale the product
  positioning now contradicts. Pass 1 said medium; I lower it on the positioning
  evidence above.
- **Depends on / blocks**: none. Zero OCX-core work either way.
- **Pass-1 → final**: NOT_STARTED → **OVERTURNED** (the code search is right and I
  reproduce it, but "not started" invites planning a feature whose premise the
  product has since contradicted; the owner has to settle that before any slot is
  spent)

**Evidence**

- Nothing is built. `git grep -w -i mise` over `crates/`, `website/`, `.claude/`
 returns only prior-art prose and the coexistence code below. No Lua anywhere:
 no `backend_list_versions.lua`, `backend_install.lua`, `backend_exec_env.lua`,
 or `metadata.lua`. Corroborated by `analysis_issue_triage_2026-08-29.md:142`
 ("lives in a plugin repo that does not exist yet … all of it is greenfield").
- **Premise contradiction 1** — `.claude/rules/product-context.md:36` lists
 "**Version managers** (mise, asdf)" under competitors. Lines 79 and 81 make
 "a gap mise/asdf/ORAS do not close" the stated wording of differentiators #9
 and #11. The issue's framing, "mise is the natural frontend partner for OCX's
 backend", is the opposite posture.
- **Premise contradiction 2** — `.claude/artifacts/adr_global_toolchain_tier.md`
 `:99-102` and `:141` reject mise's model by name: "mise/asdf's additive merge is
 precisely the reproducibility [hole]" and "This is the mise leakage model the
 research explicitly tells a backend tool to avoid. Rejected."
- **Premise contradiction 3** — when #34 was filed (2026-04-05) OCX was a pure
 backend needing a frontend partner. It now ships its own frontend tier:
 `ocx.toml`/`ocx.lock`, and `Add`, `Lock`, `Update`, `Remove`, `Status`,
 `Inspect`, `Env`, `Exec`, `Shell`, `Direnv` as root commands
 (`crates/ocx_cli/src/command.rs:94-171`).
- **What did ship instead**: coexistence. `Tool::Mise` is a real variant —
 `crates/ocx_cli/src/api/data/shell_state.rs:1008` maps it to `"mise"`,
 `:1469-1482` build an `Observation { tool: Tool::Mise, signal: "MISE_SHELL=bash" }`
 with `Reason::YieldedTo`, asserted at `:1808,1814`. `.claude/rules/arch-principles.md:167`
 documents `shell/coexistence.rs` as detecting "a live direnv/mise session to
 yield to". OCX detects mise and steps aside; it does not delegate to it.

**Remaining scope**

- Owner decides first: ship a mise backend plugin, or close as not-planned on the
 grounds that mise is now a competitor and coexistence is the shipped posture.
- If shipped: create the plugin repository. `subsystem-*` rules and this repo's
 CI do not cover it, and the issue's own "Out of scope: changes to OCX core"
 means no work lands in `ocx-sh/ocx`. Consider closing here and re-filing there.
- Three Lua hooks over `ocx package install`, `ocx index list`, and `ocx package env --format json`.
 Note the CLI grammar moved since the issue was written: `ocx install` is now
 `ocx package install` and `ocx env` is the project-tier command, not the
 package-tier one. The issue's hook mapping is written against a grammar that no
 longer exists.
- Resolve the issue's own open spike: mise expects a predictable `install_path` it
 controls; OCX's store is content-addressed. Symlink into mise's tree, or answer
 with OCX's path from `BackendExecEnv`.

#### [#77](https://github.com/ocx-sh/ocx/issues/77) [entry-points-followup] Policy: should publishers be allowed to declare system-command entry point names?

- **Labels**: entry-points-followup, discussion-needed
- **Verdict**: NEEDS_DECISION · **Gate**: policy: allow / blocklist / warn-at-select
- **Size**: XS once decided — one file, a constant list plus one branch. The issue is entirely the undecided half.
- **Importance**: low — no reported incident, and the audience (CI and backend tooling) narrows the blast radius. It is a standing posture gap, not a live defect.
- **Depends on / blocks**: none.
- **Pass-1 → final**: NEEDS_DECISION → **AGREE**

**Evidence**

- `crates/ocx_lib/src/package/metadata/entrypoint.rs:37-49` — `TryFrom<String> for EntrypointName` runs exactly two checks, `value.len() > Self::MAX_LEN` and `SLUG_PATTERN.is_match(&value)`. No name policy of any kind.
- `crates/ocx_lib/src/package/metadata/entrypoint.rs:335` — `EntrypointError` carries only `InvalidName`-class variants; no `ReservedName`.
- `git grep -n -i -E "reserved_name|ReservedName|reserved name|shadow" -- crates/` returns nothing entrypoint-adjacent. Every `shadow` hit belongs to the SBOM referrer-supersession feature (`crates/ocx_cli/src/api/data/sbom.rs`) or to the launcher-exec PATH rule at `crates/ocx_cli/src/command/launcher/exec.rs:223`, which is about resolving *through* a package's own launcher, not about system-name reservation.
- `.claude/artifacts/adr_package_entry_points.md`, section "Launcher Name Collision Policy" — the only decided policy is select-time collision between two *currently-selected packages*, raising `PackageErrorKind::EntryPointNameCollision`. It says nothing about reserved system names. `PackageErrorKind::EntrypointCollision` at `crates/ocx_lib/src/package_manager/error.rs:180` is that variant's current shape.

**Remaining scope**

- Decide the policy: allow unconditionally, reject at publish time against a blocklist, or allow and warn at `ocx install --select` / `ocx select`.
- If a blocklist is adopted: decide who owns it, how it is versioned, and whether it is global or per-registry.
- If warn-at-select is adopted: decide the canonical system-name list and whether it is platform-specific.
- Only then the mechanical part: the validator in `entrypoint.rs`, a new `EntrypointError` variant if rejecting, and an acceptance test for a publisher declaring `bash`.

#### [#80](https://github.com/ocx-sh/ocx/issues/80) [entry-points-followup] Demote EntrypointError and TemplateResolver from pub to pub(crate)

- **Labels**: tech-debt, entry-points-followup
- **Verdict**: NEEDS_DECISION · **Gate**: keep pub (close) or wrapper error
- **Size**: XS–S. Zero code if both decisions land on the status quo; S if a wrapper error is introduced.
- **Importance**: low — API-surface hygiene on a crate that is never published, no functional impact either way.
- **Depends on / blocks**: none.
- **Pass-1 → final**: NOT_STARTED → **OVERTURNED**. Pass 1's own remaining-scope paragraph says "this is not the mechanical demotion the issue describes. It's now a design question", which is the definition of NEEDS_DECISION; the verdict field contradicted its own body. The maintainer's 2026-08-29 comment retracts all four prescribed steps and says "Retaining the issue for that [design question]" — the latest maintainer word rules.

**Evidence**

- `crates/ocx_lib/src/package/metadata/entrypoint.rs:335` — `pub enum EntrypointError`, unchanged. `crates/ocx_lib/src/package/metadata.rs:23` — the `pub use entrypoint::{Entrypoint, EntrypointError, EntrypointName, Entrypoints};` re-export, unchanged.
- `crates/ocx_lib/src/package/metadata/template.rs:122` — `pub struct TemplateResolver<'a>`, unchanged.
- The `TemplateResolver` premise is falsified, confirmed independently: `crates/ocx_cli/src/command/launcher/exec.rs:22` imports it and `:161` constructs it. `mod tests` in that file starts at `:345`, so `:161` is production code.
- **The E0446 claim is verified by compilation, not by reading.** I reproduced the exact shape (public `EntrypointName`, `pub(crate)` `EntrypointError`, `impl TryFrom<String>` with `type Error = EntrypointError`) with `rustc --edition 2024` and got `error[E0446]: private type EntrypointError in public interface ... can't leak private type`. It is a hard error, not a warn-level `private_interfaces` lint. There are three such impls (`entrypoint.rs:37`, `:52`, `:60`).
- **Not in pass 1, and it deflates the issue's whole rationale.** The issue justifies the demotion as "requiring semver-major bumps if they ever need to change". `crates/ocx_lib/Cargo.toml:6` is `publish = false`, and `CLAUDE.md` states plainly that internal code structure has no stability at all and `ocx_lib` is not a published library. There is no semver contract to protect, so the stated cost of leaving both `pub` is zero.

**Remaining scope**

- Decide `EntrypointError`: leave it `pub` (zero work — its visibility is fixed by the three public trait impls on `EntrypointName`, not by call sites), or introduce a public wrapper error so the crate-private type stops leaking. Note that `ocx_lib` is `publish = false`, so neither arm has a semver consequence.
- Decide `TemplateResolver`: leave it `pub` and treat the CLI's use at `crates/ocx_cli/src/command/launcher/exec.rs:161` as a legitimate consumer, or formally promote it with doc coverage. Demotion is not an option while that caller exists.
- Closing this as won't-do is a legitimate outcome of both decisions and costs nothing.

### Batch blocked — Blocked upstream or deferred by ADR

#### [#107](https://github.com/ocx-sh/ocx/issues/107) Rekor v2 migration delta (gated on #194 spike)

- **Labels**: security, area/oci
- **Verdict**: NOT_STARTED · **Gate**: blocked upstream: sigstore-rs has no Rekor v2 client
- **Size**: L/XL — a sigstore bump drags the pinned crypto stack at `Cargo.toml:136-142`; if upstream never ships a v2 client this becomes a hand-rolled client and needs a design pass first.
- **Importance**: **low** — adjusted down from pass 1's medium. Nothing can start, no user impact today: v1 parallel-operates and sunset requires a year's notice that has not been given. It should carry a blocked-upstream label rather than a planning slot.
- **Depends on / blocks**: blocked on upstream sigstore-rs. No in-repo tracking issue for the upstream dependency. Blocks nothing.
- **Pass-1 → final**: NOT_STARTED → **AGREE**. Every citation checked out.

**Evidence**

- Gate resolved: #194 is CLOSED (closed 2026-08-19T17:12:14Z, "Implement sign/verify pipeline via sigstore-rs (slice 2)").
- The spike gives the explicit instruction: `.claude/artifacts/research_sigstore_rs_spike.md:44` — "**Rekor v2** | **NO** | No v2 client surface in 0.14 (v2 GA'd 2025-10; Go/cosign have it, Rust does not). **#107 stays as the Rekor-v2 delta** … **Do NOT close #107 into #194.**" Reinforced at `:49-51`.
- Deferral recorded exactly where the spike said to put it: `crates/ocx_lib/src/oci/sign/rekor.rs:4-9` — "Rekor v2 (RFC 3161 TSA) is not supported — deferred to #107 pending a sigstore-rs v2 client".
- The "no hardcoded endpoint" criterion is unmet by construction: `crates/ocx_lib/src/oci/endpoint.rs:41` holds `DEFAULT_REKOR_URL = "https://rekor.sigstore.dev"`, overridable only by `--rekor-url` / `[trust.sigstore]`. No `SigningConfig` / `TrustedRoot`-driven discovery exists.
- `sigstore = "0.14.0"` at `Cargo.toml:139`, with `Cargo.toml:136-138` explaining that the surrounding crypto pins are held to 0.14's versions — a bump is not a one-line change.

**Remaining scope**

- Re-check sigstore-rs releases for a Rekor v2 client before any work starts; nothing here is actionable until one exists.
- Sign against a Rekor v2 (Tessera-backed) log; verify v2 entries.
- Replace the hardcoded `DEFAULT_REKOR_URL` (`oci/endpoint.rs:41`) with dynamic discovery from `SigningConfig` / `TrustedRoot` — the endpoint rotates roughly every 6 months.
- Keep Rekor v1 entry verification working for already-published packages.
- Remove the "v2 deferred" note at `oci/sign/rekor.rs:4-9`.

#### [#262](https://github.com/ocx-sh/ocx/issues/262) Dynamic shell completion for identifiers from the local index

- **Labels**: area/cli, priority/low
- **Verdict**: NOT_STARTED · **Gate**: blocked upstream: clap dynamic completion
- **Size**: M once unblocked — one command surface, no wire format. Pass-1's L conflates elapsed time with effort; "blocked" is a state, not a size.
- **Importance**: low — carries `priority/low`, explicitly a nice-to-have.
- **Depends on / blocks**: blocked on clap-rs/clap#3166 (external). Blocks nothing.
- **Pass-1 → final**: NOT_STARTED → **AGREE**

**Evidence**

- `crates/ocx_cli/src/command/shell_completion.rs::render_completion_script` calls `clap_complete::generate` and nothing else; its only extra work is prepending a `compinit` self-load guard for zsh. Static script generation only.
- `git grep "ArgValueCandidates\|ArgValueCompleter\|CompleteEnv\|unstable-dynamic"` over `crates` and `Cargo.toml` returns zero hits. `crates/ocx_cli/Cargo.toml:47` is a bare `clap_complete.workspace = true` with no feature list.
- The issue body itself states the non-start is deliberate: *"Blocked on upstream: clap-rs/clap#3166 … Not starting before that lands."* The prior on-main triage independently classified it BLOCKED for the same reason.
- Citation nit on pass-1: the command is `ocx shell completion` (per the function's own doc comment, shared with `ocx self activate`), not `ocx completion`. Does not change the verdict.

**Remaining scope**

- Blocked: wait for clap-rs/clap#3166 to stabilize the native completion engine. Nothing to do before then — adopting `unstable-dynamic` early means re-cutting CLI-surface output twice.
- When unblocked, smallest first slice: package-name candidates for `ocx install`, read from `~/.ocx/index/<source>/c/index.json`.
- Constraints to honor: local file reads only, never network (a TAB that stalls on DNS is worse than no completion); the bash `COMP_WORDBREAKS` workaround for `:` before version completion; no nushell support (clap-rs/clap#5840).
- Switching entry points changes what `ocx shell completion` and `ocx self activate` emit — that is a CLI-surface contract change and needs a changelog commit subject.

#### [#265](https://github.com/ocx-sh/ocx/issues/265) feat(env): unset directive in project [env] — remove ambient var during activation

- **Labels**: area/package-manager, priority/low, area/config
- **Verdict**: NOT_STARTED · **Gate**: deferred by ADR; wire-format change
- **Size**: M — three surfaces, but the expensive one (reconciler revert) is already built and documented as reusable.
- **Importance**: low — carries `priority/low` and is deferred by an explicit design decision, not by neglect.
- **Depends on / blocks**: not blocked. Natural companion to the #326/#328/#329 config-write cluster, since both change the project `[env]` surface.
- **Pass-1 → final**: NOT_STARTED → **AGREE**. Pass-1 leaned on the ADR's prose; I confirmed each claim against source.

**Evidence**

- `crates/ocx_lib/src/package/metadata/env/modifier.rs:82-86` — `pub enum ModifierKind { Path, Constant, List }`. Exactly three variants, no `Unset`. (The ADR cites `:77-81`; the symbol has drifted five lines, the fact holds.)
- `crates/ocx_lib/src/shell/reconcile/ledger.rs:163-168` — `pub enum Prior { Unset, Value(String) }`, with `Unset` documented as *"The variable did not exist before ocx set it; reverting removes it."* This is ledger-internal bookkeeping, a different concept from a project asking for an ambient variable to be removed.
- `git grep -in unset` over `crates/ocx_lib/src/project` and `crates/ocx_schema/src` returns only unrelated hits (a consent test string, two fault-injection env-var comments). No `unset` directive in the project config surface or the schema.
- `.claude/artifacts/adr_shell_env_overhaul.md` Decision 3 separates the two senses explicitly and records the deferral: the desired-unset sense *"does not exist anywhere in the product today"*, is *"a package-metadata wire-format change — a fourth `ModifierKind` variant — plus a project-config-syntax change"*, and *"This ADR leaves #265 deferred and does not pull it in."*
- Latest maintainer comment (2026-08-26) re-scopes where it should be reopened: *"Reopen against the config-schema work, not against the reconciler."* The forward-compat story is already shipped — an unknown `type` deserializes to `Modifier::Unknown` and `ValidMetadata::try_from` refuses the package by name, so an older ocx refuses a newer package rather than misreading it.

**Remaining scope**

- Add a fourth variant to `ModifierKind` in `crates/ocx_lib/src/package/metadata/env/modifier.rs`. This is a package-metadata wire-format change and needs the read-path compatibility treatment.
- Add `unset = ["VAR", ...]` syntax to the project `[env]` / `[group.<name>.env]` parser and the `ocx.toml` JSON schema.
- Add one composer apply arm. The revert machinery already exists — `Prior::Unset` handles removal on exit — so this half is cheap by the ADR's own account.
- Land it against the config-schema work, per the maintainer's 2026-08-26 direction, not against the reconciler.

#### [#358](https://github.com/ocx-sh/ocx/issues/358) The shell edge-case module can't run on Windows because of our own harness assumptions, not the platform

- **Labels**: —
- **Verdict**: NOT_STARTED · **Gate**: recommend close as won't-do
- **Size**: M — the two harness edits are S, but nothing is gained without also standing up a Python toolchain and a pytest step on the Windows job, which pass-1's S omitted.
- **Importance**: low — test-harness hygiene, no user-facing behaviour; the issue itself says it is not a blocker, and its value has since been absorbed by #353.
- **Depends on / blocks**: overlapped by [#353](https://github.com/ocx-sh/ocx/issues/353) (its retier did the practical work by another route); relates to [#339](https://github.com/ocx-sh/ocx/pull/339).
- **Pass-1 → final**: PARTIAL → **OVERTURNED** — none of the issue's three scope bullets landed. What landed is [#353](https://github.com/ocx-sh/ocx/issues/353)'s retier, a *different* route that removes most of the need; that is not "some of it landed".

**Evidence**

- `test/src/shell_matrix.py:62` — `BASE_PATH` is still the hardcoded POSIX list `["/usr/local/sbin", "/usr/local/bin", "/usr/sbin", "/usr/bin", "/sbin", "/bin"]`. Not parameterized.
- `test/tests/test_shell_reconcile_edge_cases.py:54-66` — the module-wide `pytest.mark.skipif(sys.platform == "win32", ...)` is still there. Its reason text was rewritten (it now cites `ocx#353` and states that no CI leg invokes the module on Windows), but the skip itself is unchanged.
- No Windows pytest surface exists to reach. `.github/workflows/verify-deep.yml:150-163` — the acceptance matrix is `ubuntu-latest` only, with a comment refusing a Windows entry because `registry:2` does not run reliably there. `.github/workflows/shell-activation-deep.yml:136-155` — the `Activation (Windows x64)` job runs three `env.ps1` gates and installs no `uv` or Python; the macOS job at `:53-110` is the only leg that runs pytest, and it runs `test_shell_activation.py test_shell_reconcile.py`, not the edge-case module.
- The issue's premise is falsified. `test/tests/test_shell_reconcile_edge_cases.py:4044-4074` records, per row, that #353's retier moved EC-QUOTE-004/010 and EC-PATH-013 to fully automated and EC-QUOTE-011's delayed-expansion-OFF half to automated, via native Rust tests on `verify-deep.yml`'s `windows-latest` `nextest` leg. The `live_batch_*` tests exist at `crates/ocx_lib/src/shell.rs:2239`, `:2267`, `:2303`, `:2319`, `:2673` and are not `#[cfg]`-gated off that leg. The three rows still manual are attributed there to non-harness obstacles: file ownership (`hook.rs`/`shims.rs` for EC-HOOK-009, `shell/reconcile.rs` for EC-SIZE-003) and the absence of a Windows host to observe cmd's `!...!` pairing (EC-QUOTE-011 delayed-expansion-ON).
- No commit or PR on main mentions #358.

**Remaining scope**

- **Recommended: close as won't-do.** Nothing currently depends on this. The three rows the issue wanted unblocked are automated via the Rust route, and the rows still manual are blocked by causes the harness fix does not touch. The issue's stated purpose — keeping "manual because our harness assumes POSIX" from being read as "manual because Windows" — is now served by the per-row attribution at `test_shell_reconcile_edge_cases.py:4044-4074`.
- If it is done anyway, the full job is larger than the issue states:
- Parameterize `BASE_PATH` per platform in `test/src/shell_matrix.py` so `clean_env` can build a Windows child environment.
- Replace the module-wide `win32` skip at `test_shell_reconcile_edge_cases.py:54-66` with per-row skips.
- Add a pytest invocation to the `Activation (Windows x64)` job in `.github/workflows/shell-activation-deep.yml`, including a `uv` install — that job has no Python toolchain today, so without this step the harness fix runs nowhere and is an unfalsifiable green.
- Re-tier whatever becomes automatable, and update the register's tier counts (the traceability tests at `test_shell_reconcile_edge_cases.py:4264-4285` assert claimed tiers against computed ones).

### Batch satellite — Other repositories

#### [#191](https://github.com/ocx-sh/ocx/issues/191) support of patches and managed config in rules

- **Labels**: —
- **Verdict**: PARTIAL · **Gate**: satellite repo find_ocx
- **Size**: S — one file, one repo, with the Bazel implementation as the executable spec. The per-call vs global-passthrough wrinkle is the only non-mechanical part.
- **Importance**: medium — CMake users cannot reach corporate managed config or pinned patch companions at all, while Bazel users can.
- **Depends on / blocks**: none. Same satellite-repo pair as #192; work lands outside this repo.
- **Pass-1 → final**: PARTIAL → **AGREE**. Independently re-verified against both satellite repos' current tips; one factual correction on the release number.

**Evidence**

- **Bazel done, at today's tip.** `ocx-sh/rules_ocx@main` `ocx/extensions.bzl`: `_package` tag_class (`:122`) declares `config` (`:44`), `no_config` (`:77`) and `patch_snapshot` (`:95`); `_project` tag_class (`:38`) declares the same three. The extension implementation threads all three into both repo rules — `no_config = tag.no_config, patch_snapshot = tag.patch_snapshot` at `:272-273` (project), `:304-305` (multi-platform package), `:323-324` (host-only package).
- **Correction to pass 1**: pass 1 (quoting the owner's comment) says this shipped in "rules_ocx v0.2.0 (2026-08-03)" and stops there. The feature commit is `41d1652b feat(config): adopt ocx managed config and patch freeze` (2026-08-02), released in v0.2.0 — but rules_ocx has since shipped v0.3.0 (2026-08-13) and **v0.4.0** (`fcd056ae`, 2026-09-02). I confirmed the three attributes survive to `main` today rather than assuming v0.2.0 is the tip.
- **CMake untouched.** `ocx-sh/find_ocx@main` `ocx.cmake` is 1519 lines; its six `cmake_parse_arguments` calls are at `:441`, `:691`, `:871` (`ocx_project`: `PULL` / `NAME;TOML;LOCK;PLATFORM` / `GROUPS;BINS`), `:1063` (`ocx_package`: `PULL;NO_ROOT;NO_INDEX` / `NAME;PACKAGE;INDEX;PLATFORM` / `PINS;BINS`), `:1309`, `:1340`. No `CONFIG`, `NO_CONFIG` or `PATCH_SNAPSHOT` keyword anywhere; the only three `CONFIG` matches in the file are `CMAKE_CONFIGURE_DEPENDS` at `:920`, `:922`, `:1153`.
- find_ocx has had no functional commit since `00bf9a4b` (2026-07-03, v0.3.0); everything after is CI/dist plumbing (`ac2a759c`, `870f1094`, `97e9131e`, all 2026-07-07). The CMake half is genuinely untouched, not merely unreleased.
- Threading mechanism already exists on the CMake side: `__ocx_env_prefix` (`ocx.cmake:406`) builds a `cmake -E env` prefix from a `__OCX_PASSTHROUGH_VARS` global property, and is called at each provisioning site (`:442`, `:932`, `:1168`, `:1384`).

**Remaining scope**

- Add `CONFIG <file>`, `NO_CONFIG` and `PATCH_SNAPSHOT <file>` to `ocx_project()` (`ocx.cmake:871`) and `ocx_package()` (`:1063`), mirroring the semantics already shipped in `rules_ocx`.
- `CONFIG` sets `OCX_CONFIG`; `NO_CONFIG` sets `OCX_NO_CONFIG=1` and additionally blanks `OCX_CONFIG`, `OCX_PATCHES` and `OCX_PATCH_SNAPSHOT`; `PATCH_SNAPSHOT` sets `OCX_PATCH_SNAPSHOT`.
- Extend the env threading: `__ocx_env_prefix` currently reads a *global* property, while these attributes are per-call — it needs to accept per-invocation additions.
- Add both files to `CMAKE_CONFIGURE_DEPENDS` (the `:920`/`:922` pattern) and fold them into the per-call memo fingerprint so a config edit re-configures.
- Document the keywords in the RST block above each command, and add an example under `examples/`.

#### [#284](https://github.com/ocx-sh/ocx/issues/284) ocx-mirror: bare single-file compressed assets (.gz/.xz/.zst without tar) — unsupported, and asset_type:binary silently false-greens them

- **Labels**: —
- **Verdict**: NOT_STARTED · **Gate**: ocx-mirror repo
- **Size**: M in `ocx-sh/ocx-mirror` (spec-schema field, `place_binary` magic guard, tests). XS in
  this repo for the diagnosis slice alone.
- **Importance**: high — the `binary` path ships a non-executable payload with an all-green
  pipeline, and a spec author "fixing" the tar error by switching asset type walks straight into it.
  That is a correctness bug independent of the `taplo` use case.
- **Depends on / blocks**: none. Would benefit from #283 only if bzip2-compressed bare binaries are
  also in scope; the cited `taplo` case is gzip and needs nothing from #283.
- **Pass-1 → final**: NOT_STARTED → **AGREE**, and its two substantive claims were falsifiable and
  survived. Correcting one framing: pass 1 wrote "there is no ocx_lib-side gap", which is right
  about the **fix** and wrong about the **diagnosis** — the misleading tar error is this repo's, and
  is the same slice #283 lists as a bonus.

**Evidence**

- **The mirror-side defect is verified still live on `ocx-sh/ocx-mirror`'s own `origin/main`**
 (`1ca8570`, `release: v0.6.0`), which pass 1 could not reach: `place_binary`
 (`/home/mherwig/dev/ocx-mirror/src/pipeline/package.rs:59-77`) is still
 `tokio::fs::copy` + `set_permissions(0o755)` with no magic check, and
 `/home/mherwig/dev/ocx-mirror/src/spec/asset_type.rs:110` still reads
 `Binary { name: String }` with no decompress knob.
- **Nothing downstream catches it.** The `bin_scan: verify` mode that guards binaries
 (`crates/ocx_lib/src/package/bin_scan.rs:361,432-442`) checks *names, exec bits and PATH
 reachability*, never file magic — so a gzip stream chmod'ed 0755 under the declared binary name
 passes. The silent false green the issue calls the dangerous half is intact.
- `crates/ocx_lib/src/archive.rs:98-128` (`extract_with_options`) still infers compression, then
 unconditionally `tar::extract`s — reproducing the `archive` asset-type failure verbatim.
- **The primitive the mirror needs already exists and is public**:
 `crates/ocx_lib/src/compression.rs:294-340` (`pub async fn read_file`, exported via
 `pub mod compression` at `crates/ocx_lib/src/lib.rs:46`) returns a decompressing
 `Box<dyn Read + Send>` over a file with no tar layer involved. No new ocx_lib API is required
 to implement the fix.
- **On where the issue should live**: keep it open here. This repo already tracks satellite work
 in its own tracker by convention — `.claude/artifacts/analysis_issue_triage_2026-08-29.md:242`
 lists #284 explicitly under "satellite-repo work tracked here", beside #191/#192 (rules_ocx /
 find_ocx), and the same file's row 119 records "spans this repo … and ocx-mirror … verified
 unchanged in both". Moving it would split a two-repo defect across two trackers for no gain.

**Remaining scope**

- In `ocx-sh/ocx-mirror`, `src/spec/asset_type.rs`: give `Binary` a decompress knob (an explicit
 `decompress`/`compressed` field, or magic auto-detection after download).
- In `ocx-sh/ocx-mirror`, `src/pipeline/package.rs::place_binary`: reject a payload whose leading
 bytes match a known compressor (`1f8b` gzip, `fd377a585a00` xz, `28b52ffd` zstd) unless
 decompression was requested — turning today's exit-0 into a loud failure.
- Use `ocx_lib::compression::read_file` as the decompression primitive; do not add a new ocx_lib
 API for this.
- In this repo, optional and shared with
 [#283](https://github.com/ocx-sh/ocx/issues/283): make the tar boundary report "not a tar
 archive / unsupported compression" instead of the `numeric field … cksum` error, so the
 `asset_type: archive` path stops misdiagnosing itself.

### Batch tracker — Tracker

#### [#199](https://github.com/ocx-sh/ocx/issues/199) Tracking: SBOM, Provenance & Scanning v1

- **Labels**: security, area/oci
- **Verdict**: TRACKER · **Gate**: body edit only
- **Size**: —
- **Importance**: —
- **Depends on / blocks**: parent of the five open children above.
- **Pass-1 → final**: NEEDS_DECISION → **AGREE on the shape, OVERTURNED on one row (#200).**

**Remaining scope**

- Edit the tracker body: #200 is no longer blocked — ADR Amendment 10 (2026-08-29) reversed S1-F and OCX now writes and reads the referrers fallback tag on GHCR.
- Label #107 blocked-upstream and drop it from milestone planning until sigstore-rs ships a Rekor v2 client. (#107 is a child of #24, not of this tracker, but it is the only remaining Sigstore-side delta.)
- Correct #109's body items 1 and 4 before that page is written.
- Remaining work, in dependency order: #104 (L, day-1 startable), #108 and #109 (both unblocked, docs-only), #200 (S, now unblocked), #102 (S, optional sugar).
- Note that the tracker's "Test fixtures" section still points at `test/tests/fixtures/fake_sigstore.py` while the 2026-08-20 revision says that file is deleted in favour of the real compose stack. `test_attest.py` uses a `sigstore_stack` fixture, consistent with the revision, so the body section is stale.

### Batch closed — Closed this pass

#### [#314](https://github.com/ocx-sh/ocx/issues/314) verify: cold-cache referrers probe fetches the listing, discards it, then re-lists the same subject

- **Labels**: performance, area/oci
- **Verdict**: IMPLEMENTED · **Gate**: closed
- **Size**: S — already landed.
- **Importance**: medium — real N+1 listing cost on the cold path, now eliminated for the signature scan.
- **Depends on / blocks**: none.
- **Pass-1 → final**: IMPLEMENTED → **AGREE** on the verdict, but its close comment cites the wrong PR (see Evidence) and it missed a live sibling instance of the same defect class (see New finding).

**Evidence**

- `git grep ReferrersApiCapability -- crates/ocx_lib/src/oci/verify/` returns exactly one hit, and it is a comment inside a test (`crates/ocx_lib/src/oci/verify/pipeline.rs:4648`). There is no probe call left anywhere in the verify path.
- The signature scan issues exactly one listing: `crates/ocx_lib/src/oci/verify/pipeline.rs:1541` calls `Self::list_signature_referrers(...)` once, and that helper (`pipeline.rs:2502-2543`) makes one `list_referrers_with_fallback` call. The client-side re-filter is at `pipeline.rs:1549-1560`.
- `server_filter` is computed at `pipeline.rs:1537` as `(!discover_simplesigning).then_some(SIGSTORE_BUNDLE_V03)` — so when both shapes are wanted the listing is already unfiltered, exactly the "one unfiltered listing beats one request per artifact type" reasoning the issue asked for.
- The capability cache is untouched by verify, so the issue's "N redundant atomic writes to one file" cost is gone by construction, not by a memo. `ReferrersApiCapability::from_cache`/`probe`/`write_cache` survive only on the write/copy paths (`crates/ocx_lib/src/oci/sign/referrers.rs:43-56`, `crates/ocx_lib/src/oci/copy.rs:316`), neither of which the issue names.
- **Pass-1 citation error**: the removal landed in `c96b23dd93c19f382bbc7ca126684788463f725a`, which merged via **PR #369** on 2026-08-30. PR #203 merged 2026-08-19 (merge commit `ac46dda2`), eleven days earlier, so it cannot have carried this. Verified with `gh pr view 203` and `gh pr list --search c96b23dd`.
- **No regression test.** Confirmed independently: no assertion anywhere under `crates/ocx_lib/src/oci/verify/` counts `list_referrers` calls. `verify_stays_on_the_mirror_and_never_writes` (`pipeline.rs:4640`) asserts call *membership* and host, never call count. A refactor could reintroduce a second listing silently.

**Closed**: The double-fetch this issue describes is gone. Verify no longer probes the Referrers API at all: `ReferrersApiCapability::probe` has no call site left under `crates/ocx_lib/src/oci/verify/`, and the signature scan now issues a single `list_referrers_with_fallback` through `list_signature_referrers` and re-filters client-side, keeping the server-side `artifactType` hint only while the bundle shape is the one thing being looked for. That landed in [`c96b23dd`](https://github.com/ocx-sh/ocx/commit/c96b23dd93c19f382bbc7ca126684788463f725a) via [#369](https://github.com/ocx-sh/ocx/pull/369). Two caveats for the record: no test pins the call count, so the pattern could return silently; and the `Demand` attestation path still lists the same subject twice (`refuse_unsigned` unfiltered, then `scan` filtered), which is the same redundancy at a call site that did not exist when this issue was written — filed separately.

#### [#319](https://github.com/ocx-sh/ocx/issues/319) verify: unpinned Rekor public key is re-fetched per candidate instead of per run

- **Labels**: performance, area/oci
- **Verdict**: IMPLEMENTED · **Gate**: closed
- **Size**: S — landed.
- **Importance**: low — bounded at 8 fetches, reachable only from standalone online-unpinned `ocx package verify`.
- **Depends on / blocks**: none.
- **Pass-1 → final**: IMPLEMENTED → **AGREE**. Every citation checked out.

**Evidence**

- `crates/ocx_lib/src/oci/verify/pipeline.rs:1637` — `let rekor_keys = RekorKeyMemo::default();` sits above the candidate loop, with the comment "One memo for every door this scan opens (#374, #319)".
- `crates/ocx_lib/src/oci/verify/pipeline.rs:2820-2837` — `RekorKeyMemo::resolve` consults the `HashMap<log_id_hex, PEM>` first and only reaches `fetch_rekor_public_key_pem` on a miss. Failures are deliberately not memoized, preserving ANY-of semantics.
- `fetch_rekor_public_key_pem` has exactly two call sites on main: inside the memo (`pipeline.rs:2833`) and the auto-verify pre-pin (`auto_verify.rs:234`). The pipeline cannot fetch around the memo.
- `one_log_id_is_fetched_once_however_many_candidates_ask` (`pipeline.rs:8306-8355`) counts real TCP connections against a stub Rekor with `Connection: close`, asserts `hits == 1` for four `resolve()` calls, and its own comment documents why red is reachable: the trust root pins no key at all, so every unmemoized resolution must hit the network.
- The over-collapse guard is separately pinned by `the_rekor_memo_answers_each_log_id_with_its_own_key` (`pipeline.rs:8258-8292`), which is offline throughout so no network round trip can satisfy it.
- `76fd1a8db78fcfb81f9a5d74eaa81bd75b58cc42` is on `origin/main` (`git merge-base --is-ancestor` confirmed), landed via PR #390.

**Closed**: Fixed in [`76fd1a8d`](https://github.com/ocx-sh/ocx/commit/76fd1a8db78fcfb81f9a5d74eaa81bd75b58cc42), merged via [#390](https://github.com/ocx-sh/ocx/pull/390). `RekorKeyMemo` in `crates/ocx_lib/src/oci/verify/pipeline.rs` memoizes the Rekor public-key resolution by `logId` for the life of one scan and is shared by both the bundle and the cosign-sidecar door, so N candidates now cost one fetch instead of N; failures are deliberately not memoized so a transient Rekor fault cannot decide the whole ANY-of scan. It is pinned by `one_log_id_is_fetched_once_however_many_candidates_ask`, which counts real connections against a stub log, and by a sibling test proving two distinct log ids still resolve to distinct keys. One residue worth knowing: the memo is per scan pass rather than per run, so a run that also scans an enclosing index can resolve the same log twice.

#### [#356](https://github.com/ocx-sh/ocx/issues/356) ocx signatures fallback

- **Labels**: —
- **Verdict**: IMPLEMENTED · **Gate**: closed
- **Size**: n/a — shipped.
- **Importance**: high, and already paid — cosign interop with GHCR and Docker
  Hub, neither of which serves the Referrers API.
- **Depends on / blocks**: it is the write-side precedent #392 lane 3 would
  reuse.
- **Pass-1 → final**: IMPLEMENTED → **AGREE**. Falsification attempt failed on
  every axis I tried: both halves of the one-line ask exist, on both the write
  and the read side, with unit tests, acceptance tests in both interop
  directions, and a dedicated docs page.

**Evidence**

- **Timeline rules out the "filed after it shipped" hypothesis.** Issue filed
 2026-08-26T22:14Z. At that point `crates/ocx_lib/src/package/tag.rs` carried
 `is_referrer_fallback_tag` as a *classifier only* — I read the file at
 `git rev-list -1 --until=2026-08-26T23:59 origin/main`: it strips `.sig` to
 refuse the string as a package version, and there is no constructor. The
 work landed after: `c96b23dd` (2026-08-30 11:32) and `bf24416a` (2026-08-30
 20:44). ADR Amendment 10 is dated 2026-08-29 and names the issue at
 `.claude/artifacts/adr_oci_referrers_signing_v1.md:1080` (pass 1 said 1063 —
 the amendment *heading* is near there, the `#356` link is at 1080).
- **Ask half 2, "change canonical tag to `sha256-xxx`":** `referrer_fallback_tag`
 (`crates/ocx_lib/src/package/tag.rs:187`) emits `<algorithm>-<hex truncated
 to 64>`. It is read by `OciTransport::pull_referrer_fallback_index`
 (`crates/ocx_lib/src/oci/client/transport.rs:627`) and written by
 `append_referrer_fallback_index` (same file, 698) — both **default trait
 methods**, so every transport including test doubles gets a real one.
 `list_referrers_with_fallback` (578) routes the read.
- **Ask half 1, "support `sha256-xxx.sig`":** write side is
 `oci::sign::simplesigning_write::append_layer`
 (`crates/ocx_lib/src/oci/sign/simplesigning_write.rs:212`), which targets
 `crate::oci::verify::sidecar_tag(subject, layer.kind)` at line 219. It is
 reached from the sign pipeline at
 `crates/ocx_lib/src/oci/sign/pipeline.rs:687` (`write_simplesigning_leg`),
 gated on `ctx.format.writes_simplesigning()` at line 512, and from the
 attest pipeline at `crates/ocx_lib/src/oci/attest/pipeline.rs:464`. Read side
 is `sidecar_tag` at `crates/ocx_lib/src/oci/verify/simplesigning_read.rs:204`
 (pass-1 line exact).
- **Discovery needs no flag on the read side, only the write side is opt-in.**
 `SignatureFormat` (`crates/ocx_lib/src/oci/sign/format.rs`) defaults to
 `Bundle`; `simplesigning` / `both` opt into *also* writing the sidecar.
 `DiscoveryMethod` (`crates/ocx_lib/src/oci/verify/discovery.rs:29`) has all
 three arms — `ReferrersApi`, `FallbackTag`, `SidecarTag` — and is reported
 verbatim on `signatures[].discovery_method`.
- **Tests.** Unit: `the_fallback_write_lands_at_the_spec_tag_with_artifact_type_and_annotations`
 (`transport.rs:1471`), `two_writers_racing_one_fallback_index_both_land`
 (`transport.rs:1386`), `a_registry_without_the_referrers_api_gets_the_fallback_index_written`
 (`sign/pipeline.rs:2037`), `a_simplesigning_sidecar_carries_the_claim_and_its_cosign_annotations`
 (1698), `the_default_format_writes_no_simplesigning_sidecar` (1939) — the
 last is the negative control that keeps the default honest. Acceptance:
 `test/tests/test_cosign_interop.py` plus a four-file matrix covering **both**
 directions (`test_cosign_matrix_cosign_signs.py`, `test_cosign_matrix_ocx_signs.py`,
 `test_cosign_matrix_attest.py`, `test_cosign_matrix_extras.py`).
- **Docs.** `website/src/docs/in-depth/cosign-parity.md` (whole page),
 `website/src/docs/in-depth/signing.md:74,94`, and the frozen flag tables at
 `website/src/docs/reference/command-line.md:3932`, `4181`, `4432`.
- Checked and rejected the alternative reading that "canonical tag" meant the
 GC keep tag: `__ocx.keep.<algorithm>-<hex>` is a separate, unrelated
 mechanism (`crates/ocx_cli/src/options/keep_tag.rs`), and `tag.rs:184-186`
 documents that it is deliberately *not* the bare spec-reserved form.

**Closed**: Landed by [#369](https://github.com/ocx-sh/ocx/pull/369) (commit
 [c96b23d](https://github.com/ocx-sh/ocx/commit/c96b23dd93c19f382bbc7ca126684788463f725a)),
 with the copy-side sweep following in
 [#390](https://github.com/ocx-sh/ocx/pull/390) (commit
 [bf24416](https://github.com/ocx-sh/ocx/commit/bf24416a)). Both halves of the
 ask are on main: the canonical referrers tag is now the spec form
 `sha256-<hex>` (`package::tag::referrer_fallback_tag`), read and written
 through `OciTransport::pull_referrer_fallback_index` /
 `append_referrer_fallback_index` in `crates/ocx_lib/src/oci/client/transport.rs`;
 and the cosign `sha256-<hex>.sig` sidecar is written by
 `oci::sign::simplesigning_write::append_layer` under `--signature-format
 simplesigning|both` and discovered on the read side with no flag at all. The
 decision record is Amendment 10 of
 `.claude/artifacts/adr_oci_referrers_signing_v1.md`, which reverses the old
 S1-F "never write the fallback tag" position and cites this issue as the
 reason. Covered by `the_fallback_write_lands_at_the_spec_tag_with_artifact_type_and_annotations`
 and `two_writers_racing_one_fallback_index_both_land` in `transport.rs`, and
 end to end by the four-file cosign interop matrix under `test/tests/`.

#### [#328](https://github.com/ocx-sh/ocx/issues/328) config get/set/unset/describe similar to grimoire

- **Labels**: —
- **Verdict**: DUPLICATE · **Gate**: closed → #326
- **Size**: XS to decide (a merge/close call). M-L to implement if it stays and spans every config tier.
- **Importance**: medium — same underlying gap as #326, which has the concrete downstream blocker. On its own, this issue carries no evidence of urgency.
- **Depends on / blocks**: duplicate cluster with #326 and #329. Blocks nothing until the cluster is resolved.
- **Pass-1 → final**: NOT_STARTED → **OVERTURNED** on usefulness, not on facts. Nothing is implemented, but the issue is a title with an empty body inside a duplicate cluster the owner has already flagged for merging — an implementer cannot act on "NOT_STARTED, scope: everything".

**Evidence**

- Issue body is empty; no labels, no comments, no acceptance criteria. The only content is the title's reference to grim's `config` verb shape.
- `crates/ocx_cli/src/command/config.rs:27-72` — `ConfigGroup` has only `Setup`, `Update`, `Test`, `Push`. No `get`/`set`/`unset`/`describe` verb exists for any tier. Searched for landed-under-another-name work: the top-level `Command` enum in `crates/ocx_cli/src/command.rs:100-171` has no `Project` or `Toolchain` group either.
- The duplication is already recorded on `main`: `.claude/artifacts/analysis_issue_triage_2026-08-29.md:239` — *"[#326] / [#328] / [#329] are three framings of one gap: no CLI write verbs for `ocx.toml` `[env]` or `config.toml`. **Merge into one.**"* The per-issue row at `:131` reads *"same gap as #326, config-tier side. **Duplicate cluster**"*.

**Closed**: duplicate of #326, folded into its scope

#### [#329](https://github.com/ocx-sh/ocx/issues/329) ocx project toolchain edit cli command for env entries

- **Labels**: —
- **Verdict**: DUPLICATE · **Gate**: closed → #326
- **Size**: XS to decide. M to implement, identical to #326's `[env]` half.
- **Importance**: low — no body, no labels, and the capability is already argued for with evidence in #326.
- **Depends on / blocks**: duplicate cluster with #326 and #328.
- **Pass-1 → final**: NOT_STARTED → **OVERTURNED** on the same grounds as #328: factually unstarted, but it is an empty-bodied third framing of #326's `[env]` half and needs a merge decision, not an implementer.

**Evidence**

- Issue body is empty; no labels, no comments.
- No such command exists. `crates/ocx_cli/src/command.rs:100-171` enumerates the whole top-level `Command` enum — `Env`, `Add`, `Clean`, `Config`, `Direnv`, `Index`, `About`, `Init`, `Lock`, `Login`, `Logout`, `Update`, `Launcher`, `Package`, `Patch`, `Pull`, `Remove`, `Exec`, `DeprecatedRun`, `Shell`, `Self_`, `Status`, `Inspect`, `Version`, `External` — with no `Project` or `Toolchain` group.
- `toolchain_env.rs` and `toolchain_exec.rs` back `ocx env` and `ocx exec`; both are read/spawn commands, neither edits anything.
- Pass-1's false-positive check is correct: PR [#1](https://github.com/ocx-sh/ocx/pull/1) (merged 2026-03-03) is a GitHub Actions dependency bump, a text match on the number only.
- The on-main triage records it in the same cluster: `.claude/artifacts/analysis_issue_triage_2026-08-29.md:132` — *"same gap as #326, project-toolchain `[env]` side. **Duplicate cluster**"*.

**Closed**: duplicate of #326, folded into its scope

## 5. Incidental findings (not yet issues)

- `.github/workflows/release.yml:237` reads `steps.cargo-cyclonedx.output.paths`; GitHub Actions requires `outputs`, so the expression is empty and the CycloneDX SBOM assets never upload. One-word fix, folded into #200's scope; can ship alone.
- `ocx status` prints `none for linux/amd64+libc.glibc` and no digest for 6 of 8 tools in this repo: `ToolStatus::plain_annotation` matches the host leaf by string equality. Now the scope of #395.
- The `Demand` attestation path lists the same subject twice (`refuse_unsigned` unfiltered, then `scan` filtered — `verify/pipeline.rs:594-597`). Same redundancy #314 removed elsewhere, at a call site that landed after it. Not filed.
- No test pins the referrers call count on the verify path, so the #314 double-fetch could return unobserved. Not filed.
- `RekorKeyMemo` (#319) is per scan pass, not per run: a run that also scans an enclosing index resolves the same log twice. Noted in the close comment, not filed.
- The C-044 steady-state prompt measures 4.82 ms on a quiet box and 7.59 ms on a CI runner (from #359's refutation) — relevant to #362 and #400, and to any future "the no-op path is free" argument.
- #104's proposed exit code 85 is already `UnsupportedKeyBackend`; the next free slot is 86.
- `strum` / `strum_macros` 0.27.2 are already in `Cargo.lock` via `oci-spec` and in `LICENSE-THIRD-PARTY.md`; a direct dev-dependency for #322 adds no new crate.
- `test_shell_reconcile_edge_cases.py` is skipped wholesale on `win32` and the Windows CI job has no Python toolchain (#358) — any "runs on Windows" claim about the pytest shell suite is currently an unfalsifiable green.
- Issue bodies found factually wrong and corrected via scope comment: #53 (function line range), #69 (two files no longer exist, composer line numbers stale, second identity-less caller), #71 (root `ocx install` removed; changelog criterion forbidden by CLAUDE.md), #109 (items 1 and 4 reversed by Amendment 10), #199 (blocker + fixtures section), #200 (blocker), #211 (`platforms` map retired in `8e6eb3ea`), #361 (error-contract wording), #398 ("scratch-only" is not what the code does), #402 ("no package published" premise), #42 (dead `TagLock` references).

## 6. Method notes

- 12 sonnet finder batches → 12 opus refuter batches, each refuter told to falsify every verdict and re-derive the remaining scope. 19 of 82 first-pass verdicts overturned; three were false closures (#310, #395, #397) where the finder cited code that predates the issue. Rule: check the issue's creation date against the commit date of the cited fix before believing IMPLEMENTED.
- Search where the issue says the defect is (the fork submodule is part of the tree), and check `.claude/artifacts/` for a decision record before reading code — ADR Amendment 10 (2026-08-29) alone changed the status of #200, #109, #356 and #392.
- Sizes were adjusted against the code on 14 issues; the usual direction was up, and the usual cause was a hidden contract (exhaustive matches, a wire format, a test that asserts the old behaviour). The sizes here are post-refutation.
- Working files (issue dumps, both pass reports, posted comment bodies): `~/.cache/ocx-triage-333/` — durable, outside the repo.
