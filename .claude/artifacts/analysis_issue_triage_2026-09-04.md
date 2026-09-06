# Analysis: open-issue triage against main (2026-09-04)

- **Date**: 2026-09-04
- **Tree**: `origin/main` @ [`34728edc`](https://github.com/ocx-sh/ocx/commit/34728edc)
- **Scope**: all 82 open issues except [#407](https://github.com/ocx-sh/ocx/issues/407) (excluded by the owner)
- **Method**: 12 sonnet finders (one batch each, read-only) → 12 opus refuters, one per batch, instructed to falsify every verdict and re-derive the remaining scope. Final call by the orchestrator on each closure, with the cited symbols re-checked in the tree.
- **Predecessor**: [`analysis_issue_triage_2026-08-29.md`](./analysis_issue_triage_2026-08-29.md) (main @ `24f0a7d6`)

## 1. Result

| | |
|---|---|
| Open before (excl. #407) | 82 |
| Closed as implemented | 3 — [#314](https://github.com/ocx-sh/ocx/issues/314), [#319](https://github.com/ocx-sh/ocx/issues/319), [#356](https://github.com/ocx-sh/ocx/issues/356) |
| Closed as duplicate of [#326](https://github.com/ocx-sh/ocx/issues/326) | 2 — [#328](https://github.com/ocx-sh/ocx/issues/328), [#329](https://github.com/ocx-sh/ocx/issues/329) |
| Open after (excl. #407) | 77 |
| Scope comment posted | 76 (every open issue except the tracker #199) |
| Body appended with re-verified scope | 12 (#42, #46, #102, #191, #324, #333, #200, #109, #199, #357, #360, #402) |
| `discussion-needed` added | #34, #80, #178, #189, #192, #288, #320, #323, #348, #359, #397 |
| Retitled | #71 → `ocx package install --reinstall` |

**Refutation rate**: 19 of 82 first-pass verdicts overturned (#34, #79, #80, #211, #288, #306, #310, #321, #322, #328, #329, #358, #393, #395, #397, #398, #402, and sizing-only on #392, #405), three of them false closures (#310 IMPLEMENTED, #395 IMPLEMENTED, #397 OBSOLETE — in each case the finder cited code that predates the issue). A single pass is not evidence.

**Incidental findings**:
- `.github/workflows/release.yml:237` reads `steps.cargo-cyclonedx.output.paths` — GitHub Actions requires `outputs`, so the CycloneDX SBOM assets never upload. One-word fix; recorded in #200's scope.
- `ocx status` prints `none for linux/amd64+libc.glibc` and no digest for 6 of 8 tools in this repo: `ToolStatus::plain_annotation` matches the host leaf by string equality. Now the scope of #395.
- The `Demand` attestation path lists the same subject twice (`refuse_unsigned` unfiltered, then `scan` filtered) — the #314 pattern at a newer call site. Not yet filed.

## 2. Legend

Size: XS < 1 h, one file · S ≤ half day, ≤ 3 files · M 1–2 days, one subsystem · L multi-day / multi-subsystem · XL needs an ADR first.
Importance: what breaks or who is blocked if it waits.
Gate: what must happen before an implementer can start; `none` = startable today.

## 3. All 82 issues

| Issue | Title | Verdict | Size | Importance | Gate |
|---|---|---|---|---|---|
| [#25](https://github.com/ocx-sh/ocx/issues/25) | feat: portable OCX home export/import for air-gapped environments | NOT_STARTED | L | medium | decide archive format + verb location |
| [#31](https://github.com/ocx-sh/ocx/issues/31) | feat: mount dependencies into parent content at known subpaths | NOT_STARTED | L | medium→low | confirm still wanted (interpolation covers the example) |
| [#34](https://github.com/ocx-sh/ocx/issues/34) | feat: mise backend plugin for OCX | NEEDS_DECISION | L | low | pursue mise backend or close (mise is now a competitor) |
| [#42](https://github.com/ocx-sh/ocx/issues/42) | feat: unified freshness/update check strategy with TTL caching | PARTIAL | L | high | none |
| [#46](https://github.com/ocx-sh/ocx/issues/46) | perf(oci): stream layer archives across the OciTransport boundary — cl | PARTIAL | S | medium | none |
| [#50](https://github.com/ocx-sh/ocx/issues/50) | policy-based retention for orphan blobs | NOT_STARTED | M | medium | none |
| [#53](https://github.com/ocx-sh/ocx/issues/53) | gc: parallelize delete_objects to reduce ocx clean latency | NOT_STARTED | S | low | none |
| [#69](https://github.com/ocx-sh/ocx/issues/69) | remove identifier requirement for launcher-exec root package | NOT_STARTED | XL | low | ADR: approach A vs B |
| [#71](https://github.com/ocx-sh/ocx/issues/71) | feat(cli): ocx install --reinstall <pkg> for in-place package refresh | NOT_STARTED | S | low | none |
| [#77](https://github.com/ocx-sh/ocx/issues/77) | [entry-points-followup] Policy: should publishers be allowed to declar | NEEDS_DECISION | XS | low | policy: allow / blocklist / warn-at-select |
| [#78](https://github.com/ocx-sh/ocx/issues/78) | [entry-points-followup] Drop Deref&lt;Target=Metadata&gt; + From&lt;Va | NOT_STARTED | S–M | low | none |
| [#79](https://github.com/ocx-sh/ocx/issues/79) | [entry-points-followup] Move LauncherUnsafeCharacter out of crate-root | NOT_STARTED | S | low | none |
| [#80](https://github.com/ocx-sh/ocx/issues/80) | [entry-points-followup] Demote EntrypointError and TemplateResolver fr | NEEDS_DECISION | XS–S | low | keep pub (close) or wrapper error |
| [#81](https://github.com/ocx-sh/ocx/issues/81) | [entry-points-followup] Add completeness assertion before Vec&lt;Optio | NOT_STARTED | XS | low | none |
| [#102](https://github.com/ocx-sh/ocx/issues/102) | SLSA provenance attach: `ocx package push --provenance FILE` | PARTIAL | S | low | none |
| [#104](https://github.com/ocx-sh/ocx/issues/104) | OSV vulnerability scan on install (cargo-auditable + OSV.dev) | NOT_STARTED | L | high | none |
| [#107](https://github.com/ocx-sh/ocx/issues/107) | Rekor v2 migration delta (gated on #194 spike) | NOT_STARTED | L/XL | low | blocked upstream: sigstore-rs has no Rekor v2 client |
| [#108](https://github.com/ocx-sh/ocx/issues/108) | Publisher CI guidance: provenance + SBOM workflows | NOT_STARTED | M | medium | none (docs) |
| [#109](https://github.com/ocx-sh/ocx/issues/109) | Threat model + 2024-2026 incident references | NOT_STARTED | M | medium | body items 1+4 false; fix before writing (docs) |
| [#144](https://github.com/ocx-sh/ocx/issues/144) | glibc version floor + libc version differentiation (os.version / -vers | NOT_STARTED | XL | low | ADR amendment first |
| [#167](https://github.com/ocx-sh/ocx/issues/167) | perf(oci): bound per-layer spawn_blocking concurrency in streaming pul | NOT_STARTED | M | low | bench to 8/16 layers first |
| [#178](https://github.com/ocx-sh/ocx/issues/178) | docs(cli): declare the `--format json` output shapes stable-within-min | NEEDS_DECISION | M | critical | grant pre-1.0 stability carve-out for --format json? |
| [#189](https://github.com/ocx-sh/ocx/issues/189) | ocx select | NEEDS_DECISION | L | medium | approve adr_project_toolchain_links (Option D)? |
| [#191](https://github.com/ocx-sh/ocx/issues/191) | support of patches and managed config in rules | PARTIAL | S | medium | satellite repo find_ocx |
| [#192](https://github.com/ocx-sh/ocx/issues/192) | rules multi-package | NEEDS_DECISION | S–M | medium | (a) list attribute, (b) composing form, or close as satisfied |
| [#193](https://github.com/ocx-sh/ocx/issues/193) | Dockerfile-friendly environment import for tool bootstrap (no project  | NOT_STARTED | L | medium | 3 design axes open (output shape, frozen index, staleness) |
| [#199](https://github.com/ocx-sh/ocx/issues/199) | Tracking: SBOM, Provenance & Scanning v1 | TRACKER | — | — | body edit only |
| [#200](https://github.com/ocx-sh/ocx/issues/200) | Dogfood: attach OCX's own SBOM on release publish | NOT_STARTED | S | medium | signed vs unsigned; index vs platform subject |
| [#211](https://github.com/ocx-sh/ocx/issues/211) | `ocx package create`: pin dependencies from the project `ocx.lock` | NOT_STARTED | M | medium | impl-level questions (match key, any-target gate) |
| [#214](https://github.com/ocx-sh/ocx/issues/214) | Managed configuration option to always log digest when package is invo | NOT_STARTED | L | high | rebase PR #238; answer --exec-log follow-up |
| [#224](https://github.com/ocx-sh/ocx/issues/224) | Recommended OCI annotation set for OCX packages | NEEDS_DECISION | S | medium | upstream-attribution annotation key — before fleet publish |
| [#262](https://github.com/ocx-sh/ocx/issues/262) | Dynamic shell completion for identifiers from the local index | NOT_STARTED | M | low | blocked upstream: clap dynamic completion |
| [#265](https://github.com/ocx-sh/ocx/issues/265) | feat(env): unset directive in project [env] — remove ambient var durin | NOT_STARTED | M | low | deferred by ADR; wire-format change |
| [#270](https://github.com/ocx-sh/ocx/issues/270) | feat(oci): fork — retry a transiently failed chunk PATCH in place (re- | NOT_STARTED | M | medium | fork PR |
| [#271](https://github.com/ocx-sh/ocx/issues/271) | fix(oci): fork — RegistryError should carry the HTTP status alongside  | NOT_STARTED | M | high | fork PR (breaking fork change) |
| [#276](https://github.com/ocx-sh/ocx/issues/276) | fix(oci): registry_error classifies mid-upload connection resets as pe | NOT_STARTED | M | high | predicate shape (impl-level) |
| [#283](https://github.com/ocx-sh/ocx/issues/283) | ocx_lib: support bzip2 tarballs (.tar.bz2) in CompressionAlgorithm | NOT_STARTED | M | medium | read-side only vs full parity (impl-level) |
| [#284](https://github.com/ocx-sh/ocx/issues/284) | ocx-mirror: bare single-file compressed assets (.gz/.xz/.zst without t | NOT_STARTED | M (mirror) / XS (here) | high | ocx-mirror repo |
| [#288](https://github.com/ocx-sh/ocx/issues/288) | feat(index): explicit whole-source sync / override commands | NEEDS_DECISION | XS / L | low | amend index ADR to allow destructive override, or close |
| [#306](https://github.com/ocx-sh/ocx/issues/306) | Patch companion overlay re-emits a shared dependency's env entries | NOT_STARTED | S (opt 1) / XL (opt 2) | low | Option 1 default |
| [#310](https://github.com/ocx-sh/ocx/issues/310) | update notice | NOT_STARTED | S–M | medium | none (owns toolchain drift notice; #42 keeps cache unification) |
| [#311](https://github.com/ocx-sh/ocx/issues/311) | fork — a redirect to an IP literal bypasses the SSRF guard (the DNS ho | NOT_STARTED | S | medium | seam: fork predicate hook vs duplicate (impl-level) |
| [#312](https://github.com/ocx-sh/ocx/issues/312) | Uncapped response reads: manifests, referrers, and every non-2xx error | NOT_STARTED | M | high | fork PR |
| [#313](https://github.com/ocx-sh/ocx/issues/313) | sign↔verify module cycle blocks the planned ocx_lib crate split (ARCH- | NOT_STARTED | L | medium | say whether D-h (attest cycle) reopens |
| [#314](https://github.com/ocx-sh/ocx/issues/314) | verify: cold-cache referrers probe fetches the listing, discards it, t | IMPLEMENTED | — | — | closed |
| [#316](https://github.com/ocx-sh/ocx/issues/316) | auto-verify: trust-service fan-out inherits the unbounded dependency p | NEEDS_DECISION | S | medium | cap width for trust-service fan-out |
| [#318](https://github.com/ocx-sh/ocx/issues/318) | cli: the JSON error envelope's reserved 'remediation' field is never p | NEEDS_DECISION | XS–S | low | populate remediation or delete the field |
| [#319](https://github.com/ocx-sh/ocx/issues/319) | verify: unpinned Rekor public key is re-fetched per candidate instead  | IMPLEMENTED | — | — | closed |
| [#320](https://github.com/ocx-sh/ocx/issues/320) | verify: --format json emits certificate identity fields unsanitized | NEEDS_DECISION | S | medium | exception to verbatim-JSON for certificate fields? |
| [#321](https://github.com/ocx-sh/ocx/issues/321) | sign: a Rekor proof with undecodable hex is reported as retryable (exi | NOT_STARTED | XS | low | none |
| [#322](https://github.com/ocx-sh/ocx/issues/322) | test: nothing forces a new error variant to get an error-slug row | NOT_STARTED | S | low | none |
| [#323](https://github.com/ocx-sh/ocx/issues/323) | Sigstore calls fail under an HTTP proxy configured by hostname | NEEDS_DECISION | M | high | proxy-host exemption vs keep refusal |
| [#324](https://github.com/ocx-sh/ocx/issues/324) | net: give the HTTP transport layer one owner (ARCH-16 foundation unit) | PARTIAL | XS (bug 2) / S (+endpoint fallback) / L (consolidation) | high | consolidate ocx_lib::net? |
| [#326](https://github.com/ocx-sh/ocx/issues/326) | CLI write interfaces: ocx env set/unset and ocx config set | NOT_STARTED | M | medium | none (absorbs #328/#329) |
| [#328](https://github.com/ocx-sh/ocx/issues/328) | config get/set/unset/describe similar to grimoire | DUPLICATE | — | — | closed → #326 |
| [#329](https://github.com/ocx-sh/ocx/issues/329) | ocx project toolchain edit cli command for env entries | DUPLICATE | — | — | closed → #326 |
| [#333](https://github.com/ocx-sh/ocx/issues/333) | feat(net): make index/registry timeouts, retries and fan-out width con | PARTIAL | M | medium | loosely gated on #324 decision |
| [#348](https://github.com/ocx-sh/ocx/issues/348) | record_origin mints a namespace-consent marker without wire contact | NEEDS_DECISION | L | medium | persisted-format for pre-existing origin markers (3 options) |
| [#356](https://github.com/ocx-sh/ocx/issues/356) | ocx signatures fallback | IMPLEMENTED | — | — | closed |
| [#357](https://github.com/ocx-sh/ocx/issues/357) | The EC register asserts behaviour nothing verifies — two rows found fa | PARTIAL | L | medium | none (prose audit of 232 rows) |
| [#358](https://github.com/ocx-sh/ocx/issues/358) | The shell edge-case module can't run on Windows because of our own har | NOT_STARTED | M | low | recommend close as won't-do |
| [#359](https://github.com/ocx-sh/ocx/issues/359) | Consider a hookless shims mode as an alternative to per-prompt reconci | NEEDS_DECISION | XL | medium | close-or-watch; "sub-ms no-op" premise falsified (4.8 ms quiet / 7.6 ms CI) |
| [#360](https://github.com/ocx-sh/ocx/issues/360) | C-044's per-prompt budget is nominally met and effectively undecidable | PARTIAL | M | medium | none |
| [#361](https://github.com/ocx-sh/ocx/issues/361) | find_symlink_all resolves packages one at a time and takes no concurre | NOT_STARTED | S | low | none |
| [#362](https://github.com/ocx-sh/ocx/issues/362) | The global tier does a store write per tool on every prompt | NOT_STARTED | M | medium | measure resolve() vs syscalls split first |
| [#363](https://github.com/ocx-sh/ocx/issues/363) | shell: no way to select which groups/packages load into the per-prompt | NOT_STARTED | L | low | none |
| [#364](https://github.com/ocx-sh/ocx/issues/364) | shell: should a consent stamp cover the project's [env] table, not jus | NEEDS_DECISION | XL | medium | (a) by-design / (b) adopt fb/envdrift + supersede S-005/S-009 / (c) report-only |
| [#365](https://github.com/ocx-sh/ocx/issues/365) | flaky: project_lock::a_symlink_planted_during_the_retry_loop_is_refuse | NOT_STARTED | S | low | diagnose which assertion fires first |
| [#391](https://github.com/ocx-sh/ocx/issues/391) | copy: referrer count reports PUTs issued, not referrers discoverable a | NOT_STARTED | S | low | none |
| [#392](https://github.com/ocx-sh/ocx/issues/392) | copy: promoting a cosign-signed package to a referrers-less registry c | NEEDS_DECISION | XS / S / M | medium | lane 1 document / 2 relax gate / 3 write fallback index |
| [#393](https://github.com/ocx-sh/ocx/issues/393) | plugins: OCX_AUTH_* and OCX_ANNOUNCE_TOKEN reach ocx-<name> processes  | NOT_STARTED | S | high | OCX_AUTH_ half ships now; OCX_ANNOUNCE_TOKEN half is owner call |
| [#395](https://github.com/ocx-sh/ocx/issues/395) | ocx status show pinned digest | NOT_STARTED | XS–S | low | rescoped: host-leaf match in plain_annotation |
| [#396](https://github.com/ocx-sh/ocx/issues/396) | Version build metadata compares as a string: `_10001` sorts below `_80 | NOT_STARTED | S | high | none |
| [#397](https://github.com/ocx-sh/ocx/issues/397) | initializing / allowing ocx.toml does not auto-load | NEEDS_DECISION | S | low | reporter's `ocx shell state --format json` + shell needed |
| [#398](https://github.com/ocx-sh/ocx/issues/398) | smoke.star: ocx.exists/read_file are scratch-only despite docs, and a  | NOT_STARTED | S | medium | none (docs fix first) |
| [#399](https://github.com/ocx-sh/ocx/issues/399) | announce: a diverged branch is read as the committed root, so the inde | NOT_STARTED | L | critical | design note before patch (reverses half of #228) |
| [#400](https://github.com/ocx-sh/ocx/issues/400) | `OCX_NO_CONSENT`: let a non-interactive caller run `pull`/`exec` witho | NOT_STARTED | S | medium | none |
| [#401](https://github.com/ocx-sh/ocx/issues/401) | pull_referrers_native drops the Link header, so callers cannot detect  | NOT_STARTED | M | medium | fork PR chain |
| [#402](https://github.com/ocx-sh/ocx/issues/402) | package push --sign publishes the tag cascade before signing, leaving  | PARTIAL | M | high | reorder (default) vs mark exposure |
| [#403](https://github.com/ocx-sh/ocx/issues/403) | list_signature_candidates truncates to 8 silently, so a client cannot  | NOT_STARTED | S | low | none |
| [#404](https://github.com/ocx-sh/ocx/issues/404) | Export package::tag::SIDECAR_SUFFIXES and sidecar_tag — consumers cann | NOT_STARTED | XS | low | none |
| [#405](https://github.com/ocx-sh/ocx/issues/405) | package push leaves a stale tag→digest pin in the local index: content | NOT_STARTED | S | high | none (review against subsystem-oci invariant 2) |

## 4. Proposed metaplan batches

A batch = one `/hex-plan` + `/hex-execute` cycle of file-disjoint work packages in parallel worktrees. Capacity is bounded by review bandwidth and merge overlap, not by count: **6–8 S/M packages, or 3–4 L packages, per batch.** Fork changes (`external/rust-oci-client`) share one fork PR and one pin bump per batch. 45 issues are startable without an owner decision; they fit in 8 batches.

| Batch | Theme | Issues | Load |
|---|---|---|---|
| 0 | Urgent, alone | #399 (design note first — reverses half of #228) | 1 L·critical |
| 1a | Fork transport (one fork PR + pin bump) | #312, #271, #270, #311, #401 | 4 M + 1 S |
| 1b | Sign / push / referrers, ocx side | #402, #405, #276, #403, #404, #321, #324 (bug 2 + `endpoint.rs` fallback only), #391 | 2 M + 6 S/XS |
| 2 | Small disjoint cleanups | #46, #53, #71, #79, #81, #322, #306 (Option 1), #102 | 6 S + 2 XS |
| 3 | Package-manager / config features | #333, #326, #310, #50, #283, #211 | 6 M |
| 4 | SBOM / provenance milestone (#199) + env scrub | #104, #108, #109, #200 (+ `release.yml` typo), #393 (`OCX_AUTH_` half) | 1 L + 2 M + 2 S |
| 5 | Larger refactors | #42, #313, #214 (rebase PR #238), #78 | 3 L + 1 S–M |
| 6 | Shell / status quick wins | #396, #400, #395, #398, #362, #360, #361, #365 | 2 M + 6 S/XS |
| design-first | ADR / design before code | #25, #193, #31 (confirm wanted), #167 (bench first), #363, #357 (prose audit), #69, #144 | L–XL each |
| blocked / deferred | Upstream or by ADR | #107, #262, #265; #358 (recommend close as won't-do) | — |
| satellite | Other repos | #191 (find_ocx), #284 (ocx-mirror) | S / M |

## 5. Owner decisions that gate work (one line each)

| Issue | Question |
|---|---|
| [#178](https://github.com/ocx-sh/ocx/issues/178) (critical) | Grant a pre-1.0 stability carve-out for `--format json` shapes + sysexits, or tell integrators to pin exact versions? |
| [#323](https://github.com/ocx-sh/ocx/issues/323) (high) | Exempt the `HTTPS_PROXY` host from the SSRF private-range refusal, or keep refusing and document it? |
| [#393](https://github.com/ocx-sh/ocx/issues/393) (high, half) | Does `OCX_ANNOUNCE_TOKEN` join the plugin scrub set (needs an ocx-mirror change alongside)? The `OCX_AUTH_*` half ships without this. |
| [#189](https://github.com/ocx-sh/ocx/issues/189) | Accept or reject `adr_project_toolchain_links.md` (Option D)? Gates #189 and #193's staleness question. |
| [#224](https://github.com/ocx-sh/ocx/issues/224) | Which annotation key records *upstream* provenance for mirrored packages? Decide before the 42-package fleet publishes. |
| [#324](https://github.com/ocx-sh/ocx/issues/324) | Consolidate `tls.rs` + `ssrf.rs` + one client factory under `ocx_lib::net`? (bug 2 and the `endpoint.rs` fallback ship regardless) |
| [#364](https://github.com/ocx-sh/ocx/issues/364) | Consent stamp over `[env]`: (a) by design, (b) adopt `fb/envdrift` and supersede S-005/S-009, or (c) report-only drift? |
| [#348](https://github.com/ocx-sh/ocx/issues/348) | Persisted format for pre-existing origin markers: prefixed payload, sibling `refs/origins-wire/`, or accept the clause-2 drop? |
| [#392](https://github.com/ocx-sh/ocx/issues/392) | Copy to a referrers-less registry: document only, relax the gate, or write the fallback index (extends Amendment 10 to copy)? |
| [#359](https://github.com/ocx-sh/ocx/issues/359) | Hookless shims mode: close as declined or keep as a watch item? The "no-op path is sub-ms" premise is falsified (4.8 / 7.6 ms). |
| [#316](https://github.com/ocx-sh/ocx/issues/316) | Cap trust-service fan-out in auto-verify, and at what width? |
| [#320](https://github.com/ocx-sh/ocx/issues/320) | Carve a bidi/zero-width exception to the verbatim-JSON policy for the two certificate fields? |
| [#318](https://github.com/ocx-sh/ocx/issues/318) | Populate `remediation` or delete the reserved field? |
| [#288](https://github.com/ocx-sh/ocx/issues/288) | Amend the index ADR to allow one explicit destructive override verb, or close? |
| [#192](https://github.com/ocx-sh/ocx/issues/192) | (a) list attribute, (b) composing form, or close as satisfied (repetition already works)? |
| [#397](https://github.com/ocx-sh/ocx/issues/397) | You are the reporter: which shell, and what does `ocx shell state --format json` say in that directory? Three documented configurations produce the symptom. |
| [#34](https://github.com/ocx-sh/ocx/issues/34) | Still want a mise backend plugin, given mise is now listed as a competitor? |
| [#77](https://github.com/ocx-sh/ocx/issues/77) | Publisher may declare `bash`/`git` as entrypoint names: allow, blocklist, or warn at select? |
| [#80](https://github.com/ocx-sh/ocx/issues/80) | Keep `EntrypointError`/`TemplateResolver` `pub` (close as won't-do) or add a wrapper error? |
