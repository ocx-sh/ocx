# Analysis: open-issue triage and effort estimation

- **Date**: 2026-08-29
- **Tree**: `origin/main` @ [`24f0a7d6`](https://github.com/ocx-sh/ocx/commit/24f0a7d6)
- **Scope**: all open issues and all pull requests on `ocx-sh/ocx`
- **Method**: two multi-agent sweeps, each with an adversarial second pass
- **Result**: 106 → 70 open issues; 5 → 4 open PRs

This is a **snapshot**, not a plan. It records what was verified, what was
overturned, and — more usefully than either — the ways this repository's issue
tracker misleads a reader who trusts it.

---

## 1. Headline

| | |
|---|---|
| Open issues before | 106 |
| Closed this pass | 36 (25 fixed, 8 not-planned, 3 outdated premise) |
| Open issues after | 70 |
| PRs audited | 20 (5 open, 15 substantive closed-unmerged) |
| Milestones closed | 1 (*AI adoption v1*) |

**The single most useful finding is a process one.** PR
[#339](https://github.com/ocx-sh/ocx/pull/339) closed with a footer reading
`Closes #170, #343, #344, #345, …` for thirteen issues. GitHub parses a closing
keyword against **one** reference — so only `#170` closed, and nine genuinely
fixed issues sat open reading as live work. Every merged PR was swept for the
same pattern; #339 was the only leak, and all 48 issues named in the other 24
PRs' closing keywords were correctly closed.

> Write `closes #a, closes #b`. A comma list after a single keyword silently
> drops everything after the first number.

---

## 2. How much to trust this

Both sweeps ran finders and then had a separate, more capable model try to
**refute** every non-trivial verdict. The refutation rates are the honest
measure of how unreliable single-pass triage is:

| Sweep | Claims challenged | Overturned |
|---|---|---|
| Issue state (78 issues, 49 agents) | 25 | **8** |
| Effort sizing (24 issues, 18 agents) | 10 | **8** |

Almost every overturn ran the same direction: an overstated *"partially fixed"*
or *"that's small"* where nothing had landed, or where a hidden contract made
the change bigger than its diff.

**Four errors in my own first, un-adversarial pass**, recorded so the rate is
not hidden:

| Issue | First call | Truth |
|---|---|---|
| [#315](https://github.com/ocx-sh/ocx/issues/315) | still valid | Fixed. I searched `crates/ocx_lib/src/oci/` — the issue plainly said the defect was in `external/rust-oci-client` |
| [#317](https://github.com/ocx-sh/ocx/issues/317) | still valid | Fixed [`dd5ec1ac`](https://github.com/ocx-sh/ocx/commit/dd5ec1ac), the day it was filed |
| [#103](https://github.com/ocx-sh/ocx/issues/103) | rescoped as half-done | Fully shipped; `enforce_builder_pin` was already wired |
| [#28](https://github.com/ocx-sh/ocx/issues/28) | not examined | Fixed 2026-04-05, six days after filing |

**Rule that would have caught all four:** search where the issue says the defect
is — the vendored fork is part of the tree — and check `.claude/artifacts/` for a
decision record before reading code. A `plan_*_closeout.md` outranks both the
issue body and a grep.

---

## 3. Estimation legend

| Basis | Meaning |
|---|---|
| `code` | An agent read the actual code, produced a concrete change plan, and a second model tried to prove the estimate wrong. 24 issues. |
| `triage` | Tier inferred from the verified issue state, labels and scope. **Not separately costed.** Directionally right, not a commitment. 46 issues. |

| Tier | Meaning |
|---|---|
| **TRIVIAL** | < 30 min, one file |
| **SMALL** | < 2 h, 1–3 files, shape already decided |
| **MEDIUM** | Half a day or more, *or* the design is still open regardless of diff size |
| **LARGE** | Multi-subsystem, greenfield, or a one-way door |
| **DECISION** | Blocked on a human call, not on effort |
| **BLOCKED** | Blocked on something outside this repo |
| **TRACKER** | Not itself work |

⚠ marks an estimate that an adversarial pass raised.

---

## 4. All 70 open issues

| Issue | Effort | Risk | Value | Basis | What gates it |
|---|---|---|---|---|---|
| [#53](https://github.com/ocx-sh/ocx/issues/53) gc: parallelize delete_objects to reduce ocx clean | **SMALL** | MEDIUM | MEDIUM | `code` | unblocked; watch: Four things the plan missed, none tier-moving. (1) "EXISTING PATTERN CLAIMED: NONE" is false and the copied pattern is the wrong one for… |
| [#79](https://github.com/ocx-sh/ocx/issues/79) [entry-points-followup] Move LauncherUnsafeCharact | **SMALL** | LOW | MEDIUM | `code` | unblocked; watch: Four additive misses, none tier-changing. (1) A third call site the FILES list omits: package_manager/launcher.rs:32 in shim_body() also… |
| [#81](https://github.com/ocx-sh/ocx/issues/81) [entry-points-followup] Add completeness assertion | **SMALL** ⚠ | MEDIUM | MEDIUM | `code` | sized up on review — An unmade design decision, debug_assert! vs assert!, with two directly conflicting documented precedents in this repo.… |
| [#102](https://github.com/ocx-sh/ocx/issues/102) SLSA provenance attach: `ocx package push --proven | **SMALL** | — | — | `triage` | capability ships via `attest --type slsaprovenance`; only the `push --provenance` sugar remains, mirroring the shipped `--sbom` path |
| [#361](https://github.com/ocx-sh/ocx/issues/361) find_symlink_all resolves packages one at a time a | **SMALL** ⚠ | MEDIUM | MEDIUM | `code` | sized up on review — Three things the sizing missed. (a) The test fixture: "find_symlink.rs's existing test module already has make_offline_manager + package-seeding… |
| [#42](https://github.com/ocx-sh/ocx/issues/42) feat: unified freshness/update check strategy with | **MEDIUM** | — | — | `triage` | the self-update half largely shipped; what remains is the *unification* across four freshness paths, and one item (`TagLock`-keyed timestamps) has to be redesigned… |
| [#46](https://github.com/ocx-sh/ocx/issues/46) perf(oci): stream layer archives across the OciTra | **MEDIUM** | — | — | `triage` | wire-level streaming landed; the client-level 4x-buffer ceiling this issue exists for did not |
| [#50](https://github.com/ocx-sh/ocx/issues/50) policy-based retention for orphan blobs | **MEDIUM** | — | — | `triage` | prerequisite #35 closed, so unblocked; needs a `[retention]` config surface plus `ocx clean --max-age/--max-size` |
| [#69](https://github.com/ocx-sh/ocx/issues/69) remove identifier requirement for launcher-exec ro | **MEDIUM** | — | — | `triage` | labelled breaking-change: no canonical identity exists for an installed package, so this is a model decision before it is a diff |
| [#71](https://github.com/ocx-sh/ocx/issues/71) feat(cli): ocx install --reinstall <pkg> for in-pl | **MEDIUM** | — | — | `triage` | new flag plus an uninstall/reinstall path through the store; no design blocker, but not a one-file change |
| [#78](https://github.com/ocx-sh/ocx/issues/78) [entry-points-followup] Drop Deref&lt;Target=Metad | **MEDIUM** | MEDIUM | MEDIUM | `code` | Open design question with no existing local pattern to copy (the codebase's other Deref impls — PinnedIdentifier, Style, test-only TestPkg — are ordinary newtype… |
| [#80](https://github.com/ocx-sh/ocx/issues/80) [entry-points-followup] Demote EntrypointError and | **MEDIUM** ⚠ | MEDIUM | LOW | `code` | sized up on review — E0446 hard compile error x3: EntrypointError is the assoc Error/Err type of three public trait impls (TryFrom<String>, TryFrom<&str>, FromStr) on… |
| [#108](https://github.com/ocx-sh/ocx/issues/108) Publisher CI guidance: provenance + SBOM workflows | **MEDIUM** | — | — | `triage` | three guide pages, none of which exist; unblocked now that the attach flags shipped |
| [#109](https://github.com/ocx-sh/ocx/issues/109) Threat model + 2024-2026 incident references | **MEDIUM** | — | — | `triage` | capstone doc; the shipped-defenses table may only claim merged behaviour, so it lands after the rest of the milestone |
| [#167](https://github.com/ocx-sh/ocx/issues/167) perf(oci): bound per-layer spawn_blocking concurre | **MEDIUM** | MEDIUM | HIGH | `code` | Design is genuinely open, not just labeled that way: the issue's own unchecked AC boxes require (1) extending the layer-scaling bench to 8/16 layers to find the… |
| [#178](https://github.com/ocx-sh/ocx/issues/178) docs(cli): declare the `--format json` output shap | **MEDIUM** | — | — | `triage` | a stability declaration is cheap; the snapshot suite that makes it true is not |
| [#191](https://github.com/ocx-sh/ocx/issues/191) support of patches and managed config in rules | **MEDIUM** | — | — | `triage` | Bazel half shipped in rules_ocx v0.2.0; the CMake half in find_ocx is untouched. Satellite repo, not this one |
| [#192](https://github.com/ocx-sh/ocx/issues/192) rules multi-package | **MEDIUM** | — | — | `triage` | neither rule set can declare more than one package per invocation; work lands in two satellite repos |
| [#211](https://github.com/ocx-sh/ocx/issues/211) `ocx package create`: pin dependencies from the pr | **MEDIUM** | — | — | `triage` | the issue lists three open interface questions (lock-binding match, group scoping, tag mismatch) — decide before costing |
| [#265](https://github.com/ocx-sh/ocx/issues/265) feat(env): unset directive in project [env] — remo | **MEDIUM** | MEDIUM | LOW | `code` | Owner ADR decision to defer (not a technical blocker, a scope decision). If ever pulled in: breaking package-metadata wire-format change (new ModifierKind variant,… |
| [#270](https://github.com/ocx-sh/ocx/issues/270) feat(oci): fork — retry a transiently failed chunk | **MEDIUM** | — | — | `triage` | fork change: buffer each chunk into `Bytes` so a PATCH is replayable. Cross-repo landing, own CI |
| [#271](https://github.com/ocx-sh/ocx/issues/271) fix(oci): fork — RegistryError should carry the HT | **MEDIUM** | HIGH | HIGH | `code` | Cross-repo: requires a real commit+push to ocx-sh/rust-oci-client (issues disabled there, so no separate tracking) before the ocx-side submodule pin can move —… |
| [#276](https://github.com/ocx-sh/ocx/issues/276) fix(oci): registry_error classifies mid-upload con | **MEDIUM** | MEDIUM | HIGH | `code` | Open design question, not just a diff-size one: should `registry_error`'s RequestError transient predicate become `!is_builder()` (mirroring transport_policy,… |
| [#283](https://github.com/ocx-sh/ocx/issues/283) ocx_lib: support bzip2 tarballs (.tar.bz2) in Comp | **MEDIUM** ⚠ | MEDIUM | HIGH | `code` | sized up on review — Four things the sizing missed. (a) A sixth exhaustive match at crates/ocx_lib/src/oci/client.rs:1092 that the plan never lists — the streaming pull… |
| [#284](https://github.com/ocx-sh/ocx/issues/284) ocx-mirror: bare single-file compressed assets (.g | **MEDIUM** | — | — | `triage` | spans this repo (bare-compressed extraction path) and ocx-mirror (`asset_type:binary` false-green); verified unchanged in both |
| [#306](https://github.com/ocx-sh/ocx/issues/306) Patch companion overlay re-emits a shared dependen | **MEDIUM** | MEDIUM | MEDIUM | `code` | Issue says outright: 'not yet reproduced by an executed test — reproduce before fixing.' No repro exists yet. Real open design question (Option 1 vs Option 2, and… |
| [#311](https://github.com/ocx-sh/ocx/issues/311) fork — a redirect to an IP literal bypasses the SS | **MEDIUM** | — | — | `triage` | fork + security: make the redirect policy consult the SSRF guard per hop, not only the scheme. Cross-repo landing |
| [#313](https://github.com/ocx-sh/ocx/issues/313) sign↔verify module cycle blocks the planned ocx_li | **MEDIUM** ⚠ | MEDIUM | MEDIUM | `code` | sized up on review — The sign⇄verify cycle SURVIVES the proposed diff, transitively through `oci::attest` — and closing that path reopens ADR decision D-h, which is not… |
| [#314](https://github.com/ocx-sh/ocx/issues/314) verify: cold-cache referrers probe fetches the lis | **MEDIUM** | MEDIUM | MEDIUM | `code` | Fix (1) is shippable alone with no open question. Fix (2) has a real open design question the issue doesn't resolve: where the process-lifetime cache is… |
| [#318](https://github.com/ocx-sh/ocx/issues/318) cli: the JSON error envelope's reserved 'remediati | **MEDIUM** | MEDIUM | MEDIUM | `code` | Explicit open design/product decision, labeled discussion-needed by the filer. Not something to default without an owner call: which direction (populate vs.… |
| [#319](https://github.com/ocx-sh/ocx/issues/319) verify: unpinned Rekor public key is re-fetched pe | **MEDIUM** ⚠ | MEDIUM | MEDIUM | `code` | sized up on review — Exit 83 (TransparencyLogUnavailable, documented RETRYABLE at error.rs:786-813) is hoisted ahead of the 65-family permanent failures that… |
| [#320](https://github.com/ocx-sh/ocx/issues/320) verify: --format json emits certificate identity f | **MEDIUM** | HIGH | MEDIUM | `code` | Real design/policy question, not implementation ambiguity: does 'JSON is a machine channel, verbatim by design' still hold as a blanket rule, or does it get a… |
| [#321](https://github.com/ocx-sh/ocx/issues/321) sign: a Rekor proof with undecodable hex is report | **MEDIUM** ⚠ | HIGH | MEDIUM | `code` | sized up on review — The plan's cheapness rests entirely on reusing SignErrorKind::RekorSetMalformed, which the repo has already refused in a test-pinned rule:… |
| [#322](https://github.com/ocx-sh/ocx/issues/322) test: nothing forces a new error variant to get an | **MEDIUM** | MEDIUM | LOW | `code` | Issue itself lays out 3 undecided options and explicitly says none is decided. strum is not in the dependency tree today (checked Cargo.toml -- absent), so picking… |
| [#323](https://github.com/ocx-sh/ocx/issues/323) Sigstore calls fail under an HTTP proxy configured | **MEDIUM** | — | — | `triage` | the SSRF DNS hook fails closed on the proxy host; no `.no_proxy()` on either builder. Security-adjacent |
| [#326](https://github.com/ocx-sh/ocx/issues/326) CLI write interfaces: ocx env set/unset and ocx co | **MEDIUM** | — | — | `triage` | no set/unset/get verbs exist anywhere in the CLI; needs comment-preserving TOML writes |
| [#328](https://github.com/ocx-sh/ocx/issues/328) config get/set/unset/describe similar to grimoire | **MEDIUM** | — | — | `triage` | same gap as #326, config-tier side. **Duplicate cluster** — see #326/#329 |
| [#329](https://github.com/ocx-sh/ocx/issues/329) ocx project toolchain edit cli command for env ent | **MEDIUM** | — | — | `triage` | same gap as #326, project-toolchain `[env]` side. **Duplicate cluster** — see #326/#328 |
| [#333](https://github.com/ocx-sh/ocx/issues/333) feat(net): make index/registry timeouts, retries a | **MEDIUM** | — | — | `triage` | problem 1 fixed by b8b72e86; A9 (`--jobs`) and A10 (`[registry]` timeout/retry keys) untouched |
| [#358](https://github.com/ocx-sh/ocx/issues/358) The shell edge-case module can't run on Windows be | **MEDIUM** | — | — | `triage` | module-wide win32 skip plus a hardcoded POSIX `BASE_PATH`; both ours, not the platform's |
| [#360](https://github.com/ocx-sh/ocx/issues/360) C-044's per-prompt budget is nominally met and eff | **MEDIUM** | — | — | `triage` | re-derive the budget cross-environment *and* the classifier's abstention rule together |
| [#363](https://github.com/ocx-sh/ocx/issues/363) shell: no way to select which groups/packages load | **MEDIUM** | — | — | `triage` | new CLI **and** config surface; `ShellConfig` carries only `hook`/`completions` today |
| [#365](https://github.com/ocx-sh/ocx/issues/365) flaky: project_lock::a_symlink_planted_during_the_ | **MEDIUM** | MEDIUM | LOW | `code` | Root cause is undetermined and the issue explicitly asks for reproduction/diagnosis first, not a fix — genuinely open design question. Branch (a) is a small,… |
| [#367](https://github.com/ocx-sh/ocx/issues/367) test/ is not linted — no ruff dependency, no [tool | **MEDIUM** | LOW | MEDIUM | `code` | Rule selection and whether the 576 pre-existing findings get fixed now, suppressed, or tracked as follow-up debt is an open decision for a maintainer — the issue… |
| [#368](https://github.com/ocx-sh/ocx/issues/368) fork — dead pull_referrers/pull_referrers_via_tag_ | **MEDIUM** ⚠ | MEDIUM | MEDIUM | `code` | sized up on review — Both functions are upstream oras-project code (pull_referrers_via_tag_schema merged upstream 2026-05-19 as PR #259), so deletion is a permanent… |
| [#25](https://github.com/ocx-sh/ocx/issues/25) feat: portable OCX home export/import for air-gapp | **LARGE** | — | — | `triage` | new export+import verbs, an archive format and relocation rules; prerequisite #23 landed so it is unblocked, but nothing is built |
| [#31](https://github.com/ocx-sh/ocx/issues/31) feat: mount dependencies into parent content at kn | **LARGE** | — | — | `triage` | `Dependency` has no `mount` field; touches metadata schema, layer assembly and GC reachability |
| [#34](https://github.com/ocx-sh/ocx/issues/34) feat: mise backend plugin for OCX | **LARGE** | — | — | `triage` | lives in a plugin repo that does not exist yet; no OCX core change, but all of it is greenfield |
| [#104](https://github.com/ocx-sh/ocx/issues/104) OSV vulnerability scan on install (cargo-auditable | **LARGE** | — | — | `triage` | greenfield: cargo-auditable `.dep-v0` parsing, an OSV client, a new exit code (85) and a local stub for tests |
| [#144](https://github.com/ocx-sh/ocx/issues/144) glibc version floor + libc version differentiation | **LARGE** | — | — | `triage` | `LibcFlavor` is unit-variant-only and `os.version` is warn-dropped; a version-range axis needs a new ADR and a new model field |
| [#189](https://github.com/ocx-sh/ocx/issues/189) ocx select | **LARGE** | — | — | `triage` | `adr_project_toolchain_links.md` is still **Proposed** — a link tree plus three consumer paths plus Windows junctions |
| [#193](https://github.com/ocx-sh/ocx/issues/193) Dockerfile-friendly environment import for tool bo | **LARGE** | — | — | `triage` | every design question the issue lists is still open (aggregated bin dir? `--path` output? frozen index in image?) |
| [#214](https://github.com/ocx-sh/ocx/issues/214) Managed configuration option to always log digest  | **LARGE** | — | — | `triage` | no execution-record code exists; overlaps open PR #238, which adds 17 files for exactly this |
| [#312](https://github.com/ocx-sh/ocx/issues/312) Uncapped response reads: manifests, referrers, and | **LARGE** | HIGH | HIGH | `code` | Cross-repo landing process: subsystem-oci.md is explicit — 'Submodule at external/rust-oci-client/. Changes need upstream PRs' — so this is a fork PR + a separate… |
| [#324](https://github.com/ocx-sh/ocx/issues/324) net: give the HTTP transport layer one owner (ARCH | **LARGE** | — | — | `triage` | ARCH-16 foundation unit — consolidates tls.rs, ssrf.rs and three duplicated client builders. Carries two TLS-root bugs |
| [#357](https://github.com/ocx-sh/ocx/issues/357) The EC register asserts behaviour nothing verifies | **LARGE** | — | — | `triage` | two of 227 EC rows audited; the other ~225 are the actual work. A grind, not a puzzle |
| [#362](https://github.com/ocx-sh/ocx/issues/362) The global tier does a store write per tool on eve | **LARGE** | HIGH | MEDIUM | `code` | The issue is a measurement, explicitly not a judgment, and says so twice. It flags its own open question up front: 'How much of that 2.5ms is the writes rather… |
| [#77](https://github.com/ocx-sh/ocx/issues/77) [entry-points-followup] Policy: should publishers  | **DECISION** | — | — | `triage` | policy question (may publishers declare `git`/`ls` as entrypoint names?) — `EntrypointName` validation is unchanged and no `ReservedName` variant exists |
| [#224](https://github.com/ocx-sh/ocx/issues/224) Recommended OCI annotation set for OCX packages | **DECISION** | — | — | `triage` | discussion-needed: someone must *state* the annotation set. Three constants are unused and `image.documentation` is undefined |
| [#288](https://github.com/ocx-sh/ocx/issues/288) feat(index): explicit whole-source sync / override | **DECISION** | — | — | `triage` | bulk sync shipped; the override verb deletes local tags, which is exactly what the index-is-the-lock invariant exists to prevent |
| [#310](https://github.com/ocx-sh/ocx/issues/310) update notice | **DECISION** | — | — | `triage` | informally worded; two of its asks are already met by unrelated mechanisms. Needs scoping into a real ask or closing |
| [#316](https://github.com/ocx-sh/ocx/issues/316) auto-verify: trust-service fan-out inherits the un | **DECISION** | — | — | `triage` | discussion-needed: the inner fan-out is deliberately unbounded to avoid an ancestor-permit deadlock, so a cap is a policy call |
| [#348](https://github.com/ocx-sh/ocx/issues/348) record_origin mints a namespace-consent marker wit | **DECISION** | — | — | `triage` | deliberately deferred: strengthening the origin marker's write gate is a persisted-format decision |
| [#356](https://github.com/ocx-sh/ocx/issues/356) ocx signatures fallback | **DECISION** | — | — | `triage` | the second ask (canonical tag -> `sha256-<hex>`) directly conflicts with a decision ratified 8 days before filing |
| [#359](https://github.com/ocx-sh/ocx/issues/359) Consider a hookless shims mode as an alternative t | **DECISION** | — | — | `triage` | filed as declined with a named reopening trigger (`RECONCILE_BUDGET_MS` climbing). Close or keep as a watch item |
| [#364](https://github.com/ocx-sh/ocx/issues/364) shell: should a consent stamp cover the project's  | **DECISION** | — | — | `triage` | discussion-needed; a fix was tried on the branch and reverted for breaking 32 tests |
| [#107](https://github.com/ocx-sh/ocx/issues/107) Rekor v2 migration delta (gated on #194 spike) | **BLOCKED** | — | — | `triage` | upstream: sigstore-rs 0.14 ships no Rekor v2 client (Go/Python/cosign do). Rekor v1 parallel-operates with no announced sunset |
| [#200](https://github.com/ocx-sh/ocx/issues/200) Dogfood: attach OCX's own SBOM on release publish | **BLOCKED** | — | — | `triage` | GHCR serves no OCI Referrers API and has no roadmap item for one; the SBOM is already generated, only the attach is blocked |
| [#262](https://github.com/ocx-sh/ocx/issues/262) Dynamic shell completion for identifiers from the  | **BLOCKED** | — | — | `triage` | upstream clap-rs/clap#3166 (dynamic completion) is still open; `unstable-dynamic` is the only route |
| [#199](https://github.com/ocx-sh/ocx/issues/199) Tracking: SBOM, Provenance & Scanning v1 | **TRACKER** | — | — | `triage` | tracking issue — 4 of 9 children now closed; not itself work |

---

## 5. Where to start

Genuine low-hanging fruit is **thin** — 8 of 10 cheap claims were overturned.

### One clean pick

**[#79](https://github.com/ocx-sh/ocx/issues/79)** — move `LauncherUnsafeCharacter`
out of the crate-root error enum. SMALL / LOW, survived the challenge. A verbatim
copy of the `DependencyError` precedent (`package_manager/error.rs:356-368`): new
`LauncherError`, one-arm `ClassifyExitCode` impl, re-export, `#[from]`. The two
existing tests match on the old type, so the compiler supplies the red→green.
Two additions the sizing missed: a third call site at `launcher.rs:32`, and a
`try_downcast!` line in `cli/classify.rs` per its own rule at `:100-103`.

### One decision each, then small

| Issue | Diff | The call |
|---|---|---|
| [#321](https://github.com/ocx-sh/ocx/issues/321) | ~10 lines | Undecodable Rekor hex → 65, not 83. `rekor.rs::parse_upload_response` already draws this line ("the log answered 2xx, so it is malformed, not unavailable") |
| [#361](https://github.com/ocx-sh/ocx/issues/361) | small | Templates in-tree (`find_or_install.rs:52`, `drain_package_tasks`). Real cost is a new ~40-line fixture — `find_symlink` needs `metadata.json` + `resolve.json` + digest + symlink and no helper seeds all four |
| [#367](https://github.com/ocx-sh/ocx/issues/367) | mechanical | **576 pre-existing ruff findings.** Ship a narrow rule set and close it, or budget the cleanup |
| [#314](https://github.com/ocx-sh/ocx/issues/314) fix (1) | small | Pass the probe's already-fetched listing through instead of discarding and re-listing. Fix (2) has an open ownership question — ship (1) alone |

---

## 6. Looks easy, is not

Recorded so nobody re-derives these the hard way.

- **[#81](https://github.com/ocx-sh/ocx/issues/81)** — a two-line `debug_assert!`
  that **can never fire**. `taskfiles/rust.taskfile.yml:146` runs
  `cargo nextest run --workspace --release`. That is the *Unchecked Green*
  anti-pattern `quality-core.md` calls Block-tier. The two "conflicting
  precedents" an agent cited do not conflict: `validation.rs:1566` is inside
  `mod tests`; only `shim_bin_store.rs:52` is production. The real question is
  whether silent dependency loss should panic a release binary or return an error.

- **[#283](https://github.com/ocx-sh/ocx/issues/283)** bzip2 — the identical task
  shipped as zstd ([`0fa616b1`](https://github.com/ocx-sh/ocx/commit/0fa616b1)):
  **16 files, +380/−27**, plus the self-contained-linking tests that ban a dynamic
  `libbz2`.

- **[#313](https://github.com/ocx-sh/ocx/issues/313)** — the ~60-line move does
  **not** break the cycle. `oci.rs:76-84` already documents `attest ↔ verify` and
  `attest ↔ sign` as live, accepted cycles, so the obvious check reports success
  falsely.

- **[#53](https://github.com/ocx-sh/ocx/issues/53)** — the plan copies the wrong
  idiom. [#49](https://github.com/ocx-sh/ocx/issues/49) standardised this repo on
  `futures::stream::buffered`, not `JoinSet`; the correct shape is *fewer* lines.
  The `performance` label also makes a benchmark mandatory under Two Hats, and
  there is no local-filesystem bench row.

- **[#320](https://github.com/ocx-sh/ocx/issues/320)** — reverses a documented,
  test-pinned decision from PR #203 (`--format json` is verbatim by design).

- **The five fork issues** ([#270](https://github.com/ocx-sh/ocx/issues/270),
  [#271](https://github.com/ocx-sh/ocx/issues/271),
  [#311](https://github.com/ocx-sh/ocx/issues/311),
  [#312](https://github.com/ocx-sh/ocx/issues/312),
  [#368](https://github.com/ocx-sh/ocx/issues/368)) share one submodule branch
  (`ocx/cross-host-followups` @ `21ded5e7`) and one 6-binary test suite, so they
  batch well. But `subsystem-oci.md` mandates a cross-repo landing process — a PR
  to `ocx-sh/rust-oci-client` with its own CI, then a pointer bump. Two review
  cycles each. Batching helps; it does not make them cheap. #368's two functions
  are also upstream oras-project code, not ours to delete casually.

---

## 7. Duplicate and cross-repo clusters

- **[#326](https://github.com/ocx-sh/ocx/issues/326) / [#328](https://github.com/ocx-sh/ocx/issues/328) / [#329](https://github.com/ocx-sh/ocx/issues/329)** are three framings of one gap: no CLI write verbs for `ocx.toml` `[env]` or `config.toml`. Merge into one.
- **[#42](https://github.com/ocx-sh/ocx/issues/42) ↔ [#310](https://github.com/ocx-sh/ocx/issues/310)** both claim the locked-tag drift notice. Pick an owner.
- **[#214](https://github.com/ocx-sh/ocx/issues/214) ↔ PR [#238](https://github.com/ocx-sh/ocx/pull/238)** — the open PR adds 17 files implementing exactly this issue.
- **Satellite-repo work tracked here**: [#191](https://github.com/ocx-sh/ocx/issues/191) and [#192](https://github.com/ocx-sh/ocx/issues/192) (rules_ocx / find_ocx), [#284](https://github.com/ocx-sh/ocx/issues/284) (half in ocx-mirror).
- **[#42](https://github.com/ocx-sh/ocx/issues/42) carries a dead reference**: `TagLock` / `TagStore` / `TagGuard` were deleted by `adr_index_indirection.md` with no migration, so "per-tag timestamps alongside `TagLock`" has no home.

---

## 8. Pull requests

All 15 substantive **closed-unmerged** PRs had their work land by another route —
verified by testing every file each PR *added* against `origin/main`, which is the
only reliable test under squash-merge. No orphaned work: #331, #300, #290, #289,
#287, #264, #261, #219, #186, #185, #182, #172, #134, #87 all fully landed.

Open PRs:

| PR | State | Action |
|---|---|---|
| [#217](https://github.com/ocx-sh/ocx/pull/217) | superseded — landed as [`eb3333c8`](https://github.com/ocx-sh/ocx/commit/eb3333c8), which cites it by number | **closed this pass** |
| [#169](https://github.com/ocx-sh/ocx/pull/169) | not landed; 372 commits behind, `CONFLICTING`; **no issue tracks it** | owner: file a tracking issue before closing, or rebase |
| [#238](https://github.com/ocx-sh/ocx/pull/238) | not landed; genuinely pending; implements [#214](https://github.com/ocx-sh/ocx/issues/214) | owner |
| [#153](https://github.com/ocx-sh/ocx/pull/153) | superseded — `octocrab` removed from `Cargo.toml`, the other five already at or past the bump | safe to close |
| [#146](https://github.com/ocx-sh/ocx/pull/146) | not landed; actions bump still pending | owner (merging triggers CI) |

---

## 9. Method notes worth keeping

1. **A PR's `Closes` footer closes only the first number.** Audit each one after a large merge.
2. **A closeout artifact outranks the issue body and the code grep.** This repo writes `.claude/artifacts/plan_<pr>_issue_closeout.md` with per-issue FIX / FIX-DONE / CLOSE-ALREADY-DONE / DEFER decisions and prepared rationales. Read it first.
3. **Search where the issue says the defect is** — including `external/rust-oci-client`.
4. **Pair every negative grep with a positive control on the same file.** The `rtk` proxy reformats output and can return silent negatives.
5. **`gh pr diff --name-status` is not supported by this `gh` build.** Use `gh api repos/ocx-sh/ocx/pulls/N/files` and test added files with `git cat-file -e origin/main:<path>`.
6. **Never close on a single agent's word.** The refutation rate was 32% and 80%.

---

## 10. Limits

- The 46 `triage` rows are **not costed against the code**. Treat them as ordering hints, not commitments.
- Issue state is a snapshot at [`24f0a7d6`](https://github.com/ocx-sh/ocx/commit/24f0a7d6). Line numbers in the gate column drift.
- `crates/ocx_mirror/` exists in the `ocx-sion` worktree as untracked litter — 0 files in git, not a workspace member, absent from `origin/main`. Left in place; deleting it is the owner's call.
