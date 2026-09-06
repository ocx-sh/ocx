# Spec re-validation (round 3, final) — `plan_index_claim_command.md`

**Reviewer:** reviewer (focus: spec), Opus 5 — **bounded final re-validation**
**Target:** `.claude/artifacts/plan_index_claim_command.md` (1109 lines; 19 WPs, 75 contracts, 40 scenarios)
**Scope, deliberately narrow:** the 14 findings of `review_plan_index_claim_spec_r2.md`, the one
actionable finding of `review_plan_index_claim_adversary.md`, and three mechanical re-checks. The
plan was **not** re-reviewed as a whole.
**Date:** 2026-09-05

Every verdict below was ruled by opening the plan and, where the claim is about code, the source
file. No LSP, no serena, no cargo, no pytest.

---

## Set A — the round-2 findings

| Finding | Verdict | Evidence |
|---|---|---|
| **F-01 · Block** · `crates/ocx_cli/src/command.rs` unowned | **closed** | The path is now in WP-14's Expected files. `S-035` moved out of WP-16's Scope (now `S-001…S-012, S-036…S-038`) into WP-14's (`C-057…C-060, C-063, C-065, C-066; S-035`). WP-14's per-package note: "Its edits to `crates/ocx_cli/src/command.rs` are **two distinct lines a reviewer must tell apart**: the `pub mod package_claim;` declaration row, and the `Index` variant's doc comment, which today reads 'Operations related to the package index' and becomes the S-035 sentence…" **Verified in source:** `command.rs` is a hand-written block, `pub mod about; pub mod add; pub mod clean; …` (73 `pub mod` rows). |
| **F-02 · Block** · `crates/ocx_cli/src/api/data.rs` unowned | **closed** | The path is in WP-14's Expected files; the note adds "Its edit to `crates/ocx_cli/src/api/data.rs` is the one `pub mod claim;` row the new report module needs." **Verified in source:** `api/data.rs` is a hand-written block, `pub mod about; pub mod announce; pub mod attestation; …` (42 rows). |
| **F-03 · Block** · four new `forge/` submodules need `mod` rows in `forge.rs`; "exactly three" false | **closed** | The preamble now reads "**Seven** files are written twice **across** waves" and carries a seven-row `File / Created by / Filled by` table covering `github.rs`, `gitlab.rs`, `git_command.rs`, `credentials.rs`, `git_push_options.rs`, `git_stderr.rs`, `git_workspace.rs`. WP-5's Expected files list all five new stubs plus `forge.rs`; a new paragraph states "**`crates/ocx_lib/src/forge.rs` stays single-writer** … WP-5 therefore pre-declares and pre-creates every submodule a later wave fills, including the `pub use` re-exports … A `mod` row for a file that does not exist fails WP-5's own `cargo check` gate, which is why the row and the stub file are inseparable." WP-6/11/12 annotations are now `(implementation)`. **Verified in source:** `forge.rs:28-35` is the hand-written `mod` block, `:37-41` the `pub use` block; `ls crates/ocx_lib/src/forge/` returns exactly `api.rs error.rs github.rs gitlab.rs http.rs identity.rs kind.rs poll.rs`, so the five `git_*`/`credentials` files are genuinely new. |
| **F-04 · Block** · new user-guide page has no sidebar entry; `config.mts` unowned | **closed** | `website/.vitepress/config.mts` is in WP-18's Expected files, and Documentation surfaces gains its own row: "The sidebar is hand-maintained, not filesystem-globbed, so the new use-case page needs an explicit entry beside `Attestations` and `Promoting`. Without it the page builds green and is unreachable — `task website:build` accepts an orphan page." |
| **F-05 · High** · C-062's "every in-repository invocation" misses the CI workflow | **partial** | `.github/workflows/oci-publish.yml` is in WP-15's Expected files and named in its note; C-062 now says "a **repo-wide** structural check (excluding `.claude/artifacts/**` and `.claude/state/**`, which are historical records) asserts no other `--package` invocation survives." **What remains:** the fix generalised the *scope* without sweeping what repo-wide actually reaches. I swept it: six further files carry `--package`, none of them in any package's Expected files, and two more sit in files WP-18 owns one wave *after* the check lands. Four of the unowned hits are user-facing remediation strings. See **F-03** below. |
| **F-06 · High** · docs sweep never names the announce grammar rewrite | **closed** | The `command-line.md` row now reads "**Plus the announce grammar rewrite the `!` commit makes**: the `#package-announce` Usage line, its Options table row and its five example invocations move to the positional form, and the prose reference to `announce --package` follows; the hidden `--package` is recorded once as deprecated until 0.7 and nowhere presented as required. Eight sites in total." The `subsystem-cli-commands.md` row: "**and the `package announce` row's flag column moved to the positional form** — it lists `--package` first today." WP-15's note records the ownership split: "**The reference-page grammar rewrite lands in WP-18, not here** — `command-line.md` is a 5,866-line file WP-18 already owns … WP-18 depends on WP-15, so the grammar is final before it is documented." **Verified:** `website/src/docs/reference/command-line.md` carries 8 `--package` hits; `.claude/rules/subsystem-cli-commands.md:82` lists `--package` first in the announce flag column. |
| **F-07 · Warn** · C-011's public `checks` field permits the empty vector C-069 forbids | **closed, with a new defect in the fix** | C-011 now: "`PushAccess` holds a **private** `Vec<CapabilityCheck>` — the only ways to build one are `skipped_all()` and the row-upgrade methods (C-069), and reads go through an accessor, so `PushAccess { checks: Vec::new() }` does not compile from anywhere in the crate". C-069 cites it: "and — because C-011 makes the field private — the struct exposes no constructor that can produce an empty `checks`." The property is closed. The **test** the fix added to prove it is not buildable here — see **F-04** below. |
| **F-08 · Warn** · DV-2 still says "19 file-disjoint work packages" | **closed** | DV-2 now: "The work is decomposed into 19 work packages across 7 waves — file-disjoint **within each wave**, with the named cross-wave exceptions listed in § Parallelization — not the system design's five sequential phases." |
| **F-09 · Warn** · S-002, S-011, S-035, S-036 in WP-16's Scope with no WP-16 test | **closed** | `S-035` moved to WP-14, whose list already held `index_claim_suggests_package_claim` and `index_group_help_states_no_forge_write`. WP-16 gains `::test_claim_json_report_key_set` ("C-060's whole sixteen-key set, with `capability_checks` in declaration order — no unit test covers the assembled report") for S-002, and `::test_out_without_credential_reports_push_access_skipped` for S-011. Declaration-order stability is now stated in three agreeing places: C-060 ("ordered by `CapabilityName`'s declaration order so the array is stable across runs"), S-036 (same words), and WP-5's renamed test `push_access_skipped_all_seeds_every_name_in_declaration_order`. That also closes r2's F-14 as a side effect. |
| **F-10 · Warn** · `git` shim has a file but no PATH-placement constraint | **closed** | New WP-4 per-package note: "writes the recording shim's executable into a **per-test `tmp_path` directory and prepends only that directory to the child `PATH`**. It is never written under `test/bin/`, under `~/.ocx/`, or to any path that outlives the test … `::test_shim_records_argv_and_env_then_delegates` asserts the placement as well as the capture." |
| **F-11 · Warn** · self-managed-GitLab partial-clone proof not routed | **closed** | Release gate 4 now: "**The same run also carries gate 2's filter proof against that instance** — `git rev-list --objects --missing=print` plus a `GIT_TRACE_PACKET=1` capability trace, each against a full-clone negative control on the same host. This is the ADR's 'partial clone against a self-managed GitLab, not only gitlab.com' item; gate 2 alone measures against a GitHub-hosted repository and cannot answer it." |
| **F-12 · Warn** · canonical-login override (C-048 / S-007) has no named test | **not closed** | Not in the intended fix set. WP-9's list is unchanged — `owner_ladder_explicit_replaces`, `owner_ladder_ci_environment`, `owner_ladder_token_identity`, `owner_id_mismatch_is_refused`, … — and WP-16 still has only `::test_owner_ci_environment`. Both exercise the *unconfirmed* branch. C-048 still promises the override ("resolve every login and take the server's `id`, `bot` and **canonical login spelling**") and S-007 still makes it the expected outcome ("the users API confirms and overrides with the canonical login spelling; source `resolved`"), with nothing asserting it. A build that skips the override and reports `ci-environment` still passes. |
| **F-13 · Suggest** · WP-15's title claims `--transport`, Scope cites no contract for it | **not closed** | Not in the intended fix set. WP-15's Scope is still `C-061, C-062, C-064; S-027, S-034`; `C-059` (the flattened `ForgeWriteOptions`) is cited only by WP-14. |
| **F-14 · Suggest** · C-069's seeding order and S-036's "execution order" differ | **closed** | Both now say declaration order — see F-09's row. |

**Set A tally: 11 closed / 1 partial / 2 not closed / 0 regressed.**

---

## Set B — the cross-model adversary finding

**Finding:** `ForgeKind::client` stubbed in WP-5 with no later package able to implement it.
**Applied fix:** C-017 gains "It ships implemented, not stubbed, in the package that owns
`kind.rs`", and WP-5's per-package note gains "**'Stub bodies' means trait-method bodies only.**
Four things in WP-5 ship implemented: `ForgeKind::validate_transport`, `ForgeKind::client`,
`GitHubForge::new` and `GitLabForge::new` (C-017)."
**Verdict: partial — the reasoning is right, the fix is half-applied, and it opens a second hole.**

### The load-bearing question, ruled from source

*Can WP-5 genuinely implement `validate_transport`, `client`, `GitHubForge::new` and
`GitLabForge::new` while every trait method body stays `unimplemented!()`?*

**On the code: yes.** I opened all three files.

- `ForgeKind::client` (`forge/kind.rs`, `pub fn client`) is today
  `client(self, token: ForgeToken, coordinate: &RepoCoordinate) -> Result<Box<dyn Forge>, ForgeError>`,
  and its whole body is `let host = coordinate.host.as_deref();` plus one `match` returning
  `Box::new(GitHubForge::new(token, host)?)` / `Box::new(GitLabForge::new(token, host)?)`. Nothing
  in it is behaviour.
- `GitHubForge::new` (`forge/github.rs`) and `GitLabForge::new` (`forge/gitlab.rs`) are both
  `new(token, host)` → `testing_base_url_override().unwrap_or_else(|| api_base_url(host))` →
  `Self::build(token, base_url)`, and `build` only fills fields (`client`, `token`, `base_url`,
  and GitLab's `project_ids` cache). Field-storing functions, exactly as the triage said.
- `validate_transport` does not exist yet; C-016 defines it as pure with no network call. A match
  over `ForgeKind` × `WriteTransport` returning `ForgeError::TransportUnsupported` needs only types
  WP-5 declares.
- `impl Forge for` appears exactly twice in the crate — `github.rs:405` and `gitlab.rs:541`, both
  in files WP-5 owns. There is no third implementor or test double that adding trait methods would
  break.

So the triage's rejection of Codex's heavier remedy (add `kind.rs` to WP-13 plus a new edge) was
correct, and its premise about the constructors holds.

**On the plan: no, not as written.** Two things block it, and the second is the one the adversary's
own analysis stopped short of.

1. **The ownership was never moved.** WP-6's Scope cell still reads `C-015…C-017, …` while WP-6's
   Expected files are `forge/git_command.rs`, `forge/credentials.rs`, `launch.rs` — `kind.rs` is not
   among them. WP-5's Scope cell cites neither C-016 nor C-017. The test the inventory names for
   C-017, `client_requires_git_binary_under_git`, is listed under **WP-6**, and the plan's own
   convention is "inline `#[cfg(test)]` in the module under test". See **F-02**.

2. **`client` has a caller, and it is four waves downstream.** I swept the tree for callers.
   `ForgeKind::client` has exactly one outside `kind.rs`:
   `crates/ocx_cli/src/command/package_announce.rs:215`, which is in **WP-15's** Expected files —
   wave 5, against WP-5's wave 2. The constructors are clean (`GitHubForge::new` /
   `GitLabForge::new` are called only from `kind.rs:120-121`, and `with_base_url`'s callers are all
   inside `github.rs`, which WP-5 owns), so `client` is the single symbol whose widening escapes
   WP-5's file set. See **F-01**.

The finding is therefore genuinely closed on the axis Codex raised — the constructor is no longer
stranded — and reopened one step outward, on the axis nobody has walked yet: *who calls the symbol
whose signature just changed*.

---

## Mechanical re-checks

### 1 · File-set disjointness — **(a) PASS, (b) PASS on the declared sets**

All 19 `Expected files` cells normalised and intersected pairwise. Computed result, not asserted:

**(a) Same-wave: PASS.** Wave 1 {WP-1, WP-2, WP-3, WP-4}; wave 2 {WP-5}; wave 3 {WP-6, WP-7, WP-8,
WP-9, WP-10, WP-11, WP-12}; wave 4 {WP-13, WP-14}; wave 5 {WP-15, WP-16}; wave 6 {WP-17, WP-18};
wave 7 {WP-19}. Every intersection within a wave is empty. The two near-misses are genuinely
distinct paths: WP-2's `announce/pipeline.rs` vs WP-10's `announce.rs` + `announce/request.rs`
(different wave anyway), and WP-14's `crates/ocx_cli/src/command.rs` + `api/data.rs` vs WP-15's
`command/package_announce.rs` + `command/deprecated.rs` + `api/data/announce.rs` — parent module
vs children, no overlap.

**(b) Cross-wave: exactly seven collisions, all seven declared.**

| File | Pair | In the preamble table? | Dependency edge? |
|---|---|---|---|
| `forge/github.rs` | WP-5 ∩ WP-7 | row 1 | WP-5 → WP-7 |
| `forge/gitlab.rs` | WP-5 ∩ WP-8 | row 2 | WP-5 → WP-8 |
| `forge/git_command.rs` | WP-5 ∩ WP-6 | row 3 | WP-5 → WP-6 |
| `forge/credentials.rs` | WP-5 ∩ WP-6 | row 4 | WP-5 → WP-6 |
| `forge/git_push_options.rs` | WP-5 ∩ WP-11 | row 5 | WP-5 → WP-11 |
| `forge/git_stderr.rs` | WP-5 ∩ WP-12 | row 6 | WP-5 → WP-12 |
| `forge/git_workspace.rs` | WP-5 ∩ WP-13 | row 7 | WP-5 → WP-6/11/12 → WP-13 (transitive ancestor) |

No eighth collision exists among the declared sets. Both halves of the merge predicate hold on
the plan as written.

**Caveat, and it is what F-01 and F-02 are about:** the check is only as good as the declared sets.
Three edits the plan *requires* have no declared file — `package_announce.rs:215`'s call-site
update (F-01), C-016/C-017 landing in `kind.rs` under WP-6's Scope (F-02), and the `--package`
migration outside `test/tests/` (F-03). Each becomes an undeclared collision or an undeclared file
at execution time, which the predicate rejects at merge.

### 2 · Traceability — **PASS**

**Scope cells.** All 75 `C-001…C-075` and all 40 `S-001…S-040` appear in at least one `Scope` cell.
Recomputed union, contiguous with no gap: C-001–004 (WP-1), C-005–007 (WP-2), C-008–014 + C-018 +
C-031 + C-069–070 + C-073 (WP-5), C-015–017 + C-019–022 + C-035 + C-075 (WP-6), C-023–025 (WP-7),
C-026–032 (WP-8), C-030 + C-033–045 + C-074 (WP-4), C-046–054 + C-067 + C-071–072 (WP-9),
C-055–056 (WP-10), C-039 (WP-11), C-044 (WP-12), C-033–034 + C-036–038 + C-040–043 + C-045 +
C-068 (WP-13), C-057–060 + C-063 + C-065–066 (WP-14), C-061–062 + C-064 (WP-15). Scenarios:
S-001–012 + S-036–038 (WP-16), S-013–033 + S-039–040 (WP-4), S-013–034 + S-039–040 (WP-17),
S-005 + S-024–025 (WP-10), S-013 (WP-11), S-017–018 + S-032 (WP-12), S-022–023 + S-026 + S-030 +
S-039–040 (WP-13), S-035 (WP-14), S-027 + S-034 (WP-15), S-037 (WP-1). **115/115.**

**Named tests.** Nothing fell out of the move. The four IDs the fix round touched all land:

- `S-035` → moved with its two tests (`index_claim_suggests_package_claim`,
  `index_group_help_states_no_forge_write`) from a package that had neither to WP-14, which has both.
- `S-002` → `::test_claim_json_report_key_set` (WP-16, new this round).
- `S-011` → `::test_out_without_credential_reports_push_access_skipped` (WP-16, new this round).
- `S-036` / `C-060` → `push_access_skipped_all_seeds_every_name_in_declaration_order` (WP-5, renamed)
  plus `::test_claim_json_report_key_set`'s declaration-order clause.

The three doc-only markers survive: C-003 (WP-1's checklist), C-032 (WP-8's), WP-3's amendments.
`C-016` → `validate_transport_refuses_github_git` (WP-5) and `C-017` →
`client_requires_git_binary_under_git` (WP-6) both exist as names — but the second sits in a package
that cannot write the file it must live in (F-02).

### 3 · Internal consistency of the touched numbers — **PASS, one stale count**

| Stated | Where | Counted | Verdict |
|---|---|---|---|
| **seven** cross-wave doubled files | Parallelization preamble prose + table | 7 table rows; 7 computed collisions | agree |
| **six** cross-repository issues | WP-19 Scope, closeout header, closeout table, closing condition, release gate 6 | 6 numbered rows + one `—` row for the two posts | agree |
| **four** `Verify: full` packages | prose after the WP table | WP-5, WP-15, WP-17, WP-19 = 4 | agree |
| **19** work packages | DV-2, § In scope, merge plan | WP-1…WP-19; merge plan lists 19 | agree |
| **7** waves | DV-2, WP table, mermaid subgraphs | waves 1–7, all populated | agree |
| critical path **seven packages, five of them large** | § Critical path | `WP-1(S) → WP-5(L) → WP-9(L) → WP-14(L) → WP-15(L) → WP-17(L) → WP-19(M)` = 7 nodes, 5 large. Longest chain recomputed independently: 7 (tied with `WP-1 → WP-5 → WP-6 → WP-14 → WP-15 → WP-17 → WP-19`, also 5 large); next longest 6 | agree |
| **23** dependency edges | (not stated, but table ↔ graph parity) | `Depends on` column sums to 23; mermaid carries 23; no edge in one and not the other | agree |
| **ten** `ForgeError` variants | C-018, `forge_error_exit_code_table` | 10 named | agree |
| **sixteen**-key `ClaimReport` | C-060, `::test_claim_json_report_key_set` | 16 named | agree |
| **six** new announce report keys | C-061, `announce_report_gains_six_keys` | 6 named | agree |
| **four** push-option keys | C-039 | 4 named | agree |
| **eight** sites on `command-line.md` | Documentation surfaces | Usage 1 + Options row 1 + examples 5 + prose 1 = 8; tree grep returns 8 `--package` hits in that file | agree |
| **89** / **90** `--package` invocations | C-062 (89 + 1), WP-15 note (90), **Risks row (89)** | tree sweep: 89 across the six pytest modules + 1 workflow = 90 | C-062 and the note agree; the **Risks row is stale at 89** — see F-05 |


---

## New findings

### F-01 · Block · `ForgeKind::client`'s only caller lives in WP-15's file, three waves after the wave that now widens its signature

**Where:** `C-017`; § Per-package notes, WP-5; Work packages WP-5 and WP-15;
`crates/ocx_cli/src/command/package_announce.rs`

**Problem:** The adversary fix moved `ForgeKind::client`'s real implementation into WP-5 (wave 2)
and gave it C-017's four-argument shape, `client(transport, credentials, coordinate, git)`. Today
the signature is two arguments — verified at `forge/kind.rs`, `pub fn client(self, token:
ForgeToken, coordinate: &RepoCoordinate)`. Widening it is a breaking change to every caller.

I swept the crate for callers. There is exactly one outside `kind.rs`:

```
crates/ocx_cli/src/command/package_announce.rs:215
    let forge = kind.client(ForgeToken::new(token.unwrap_or_default()), &self.index_repo)?;
```

`crates/ocx_cli/src/command/package_announce.rs` appears in **WP-15's** Expected files and in no
other package's. WP-15 is wave 5; WP-5 is wave 2. Both outcomes are bad, and they are the same pair
the F-03 fix already worked through for `forge.rs`:

- **WP-5 updates the call site.** The file is absent from WP-5's declared set, so the merge
  predicate's first half — "every file in a WP's actual diff must appear in that WP's declared set"
  — rejects WP-5's merge.
- **WP-5 does not.** The workspace stops compiling at wave 2 and stays broken until wave 5. WP-5
  carries `Verify: full`, its Implement gate is `cargo check -p <crate> --all-targets --locked`, and
  its stated package gate is "every wave-3 package's stubs compile against it" — all three red, and
  every wave-3 and wave-4 package inherits a broken tree.

The two constructors are genuinely safe and the triage was right about them: `GitHubForge::new` and
`GitLabForge::new` have no caller outside `kind.rs:120-121`, and `with_base_url`'s four callers are
all inside `github.rs`, which WP-5 owns. `impl Forge for` appears only at `github.rs:405` and
`gitlab.rs:541`, so adding trait methods breaks no third implementor either. `client` is the single
symbol whose widening escapes WP-5's file set — which is exactly the class of defect the adversary
gate exists to catch, one hop further out than it looked.

**Fix:** Add `crates/ocx_cli/src/command/package_announce.rs` to WP-5's Expected files and an eighth
row to the Parallelization table — `File: crates/ocx_cli/src/command/package_announce.rs |
Created by: WP-5 (call-site update only) | Filled by: WP-15`. WP-5 is a declared ancestor of WP-15
(`WP-5 → WP-10 → WP-15`), so the ancestor rule already covers the pair and no new edge is needed.
State in WP-5's per-package note that the edit is mechanical — thread the resolved
`ForgeCredentials`, `WriteTransport::Api` and `None` git binary through the one call — and that
WP-15 is where the flag that makes the transport non-default arrives.

---

### F-02 · Block · C-016 and C-017 remain in WP-6's Scope, and WP-6 may not write `kind.rs`

**Where:** Work packages, WP-5 Scope and WP-6 Scope/Expected files; `C-016`, `C-017`; Test
inventory, WP-6 (`client_requires_git_binary_under_git`)

**Problem:** The adversary fix wrote the narrative and left the machinery pointing the old way.

- `C-017` now says the constructor "ships implemented, not stubbed, **in the package that owns
  `kind.rs`**", and WP-5's note names all four symbols as WP-5 deliverables.
- But **WP-6's Scope cell** is still `C-015…C-017, C-019…C-022, C-035, C-075` — it claims C-016 and
  C-017 — while **WP-6's Expected files** are `crates/ocx_lib/src/forge/git_command.rs`,
  `crates/ocx_lib/src/forge/credentials.rs`, `crates/ocx_lib/src/launch.rs`. `kind.rs` is not there.
- **WP-5's Scope cell** is `C-008…C-014, C-018, C-031, C-069, C-070, C-073` — it claims neither.
- The test the inventory names for C-017, `client_requires_git_binary_under_git`, is listed under
  **WP-6**, and the plan's stated convention is "inline `#[cfg(test)]` in the module under test",
  i.e. `kind.rs`.

So WP-6's reviewer is handed two contracts with no diff to check, WP-5's reviewer implements two
contracts its Scope cell does not name, and if WP-6 writes either the contract or its test the merge
predicate rejects it — `kind.rs` is not in WP-6's declared set. This is Codex's stranded-symbol
defect displaced one level: the code found the right package, the ownership did not follow.

`C-015` (`ForgeCredentials`) is correctly WP-6's — WP-6 owns `credentials.rs` and C-015's
`api_is_job_token` derivation is real behaviour. Only C-016 and C-017 are misplaced.

**Fix:** Move `C-016, C-017` from WP-6's Scope cell to WP-5's, and move
`client_requires_git_binary_under_git` from WP-6's test list to WP-5's, beside
`validate_transport_refuses_github_git`, which is already there. Leave C-015 with WP-6. WP-6's
Scope then reads `C-015, C-019…C-022, C-035, C-075`.

---

### F-03 · Block · C-062's repo-wide check reds on eight files: six no package owns, two land a wave after the check

**Where:** `C-062`; Work packages, WP-15 Expected files and per-package note; Test inventory, WP-15
(`no_package_flag_invocation_survives_outside_the_deprecation_test`)

**Claim:** "**Every in-repository invocation moves to the positional form in the same change** — 89
across six acceptance modules and one in `.github/workflows/oci-publish.yml`; … a **repo-wide**
structural check (excluding `.claude/artifacts/**` and `.claude/state/**`, which are historical
records) asserts no other `--package` invocation survives."

**Problem:** The r2 F-05 fix widened the check's *scope* from `test/tests/` to repo-wide, and swept
only `test/tests/` plus the workflow it had already found. Sweeping the way the contract now reads
(excluding `.git`, `target`, `external`, `.agents`, `.claude/artifacts`, `.claude/state`):

| File | Hits | Declared by |
|---|---|---|
| `test/tests/test_announce.py` | 48 | WP-15 |
| `test/tests/test_announce_gitlab.py` | 33 | WP-15 |
| `crates/ocx_cli/src/command/package_announce.rs` | 15 | WP-15 (the declaration itself) |
| `website/src/docs/reference/command-line.md` | 8 | **WP-18 — wave 6, after the check** |
| `test/tests/test_tag_reserved.py` | 4 | WP-15 |
| `crates/ocx_cli/src/api/data/package_cascade_repair.rs` | 3 | **nobody** |
| `test/tests/test_exit_codes.py` | 2 | WP-15 |
| `test/manual/announce-e2e/CLAIM.md` | 2 | **nobody** |
| `test/tests/test_package_cascade.py` | 1 | WP-15 |
| `test/tests/test_announce_push_file.py` | 1 | WP-15 |
| `test/manual/announce-gitlab-e2e/scripts/run_gitlab_e2e.sh` | 1 | **nobody** |
| `test/manual/announce-e2e/scripts/run_update_union.sh` | 1 | **nobody** |
| `test/manual/announce-e2e/scripts/run_idempotency.sh` | 1 | **nobody** |
| `.github/workflows/oci-publish.yml` | 1 | WP-15 |
| `crates/ocx_cli/src/command/package_cascade_repair.rs` | 1 | **nobody** (a comment) |
| `crates/ocx_cli/src/api/data/package_cascade_check.rs` | 1 | **nobody** |
| `.claude/rules/subsystem-cli-commands.md` | 1 | **WP-18 — wave 6** |

Three of the unowned hits are live invocations that will warn on every run and hard-fail at 0.7:

```
test/manual/announce-e2e/scripts/run_update_union.sh:33          --package "$E2E_NAMESPACE/$E2E_PACKAGE" \
test/manual/announce-e2e/scripts/run_idempotency.sh:44           --package "$E2E_NAMESPACE/$E2E_PACKAGE" \
test/manual/announce-gitlab-e2e/scripts/run_gitlab_e2e.sh:98     --forge gitlab --index-repo "$INDEX_COORDINATE" --package "$PACKAGE" "$@"
```

Four more are worse than a lint failure — they are **user-facing remediation strings that hand the
operator the deprecated form**, emitted by `ocx package cascade check` and `cascade repair`:

```
crates/ocx_cli/src/api/data/package_cascade_repair.rs:163  "publish the moved tags - run: ocx package announce --package {package} --tags-file {}"
crates/ocx_cli/src/api/data/package_cascade_repair.rs:167  "... re-run with --announce-tags <PATH>, then: ocx package announce --package {package} --tags-file <PATH>"
crates/ocx_cli/src/api/data/package_cascade_repair.rs:170  "index behind the registry - run: ocx package announce --package {package} --refresh"
crates/ocx_cli/src/api/data/package_cascade_check.rs:151   "index behind the registry - run: ocx package announce --package {package} --refresh"
```

That gap exists whether or not the structural check ever runs: after WP-15 lands, ocx's own output
instructs users to run a form ocx warns about, and at 0.7 it instructs them to run a form that does
not exist. The plan's own argument for migrating the 89 — "would turn the 0.7 removal into 90
simultaneous failures in a repository that has forgotten why" — applies with more force to a string
the product prints at a user.

Finally, two hits sit in files **WP-18 owns in wave 6**, one wave after WP-15's wave-5 check. The
check is red at its own merge gate by construction, and WP-15's note explicitly refuses to move
those surfaces ("The reference-page grammar rewrite lands in WP-18, not here").

**Fix:** Three edits.

1. Add `crates/ocx_cli/src/api/data/package_cascade_repair.rs`,
   `crates/ocx_cli/src/api/data/package_cascade_check.rs`,
   `crates/ocx_cli/src/command/package_cascade_repair.rs`, the three `test/manual/**` scripts and
   `test/manual/announce-e2e/CLAIM.md` to WP-15's Expected files, and name the four remediation
   strings in WP-15's note — they are the reason this is a correctness item, not housekeeping. If
   the cascade strings are covered by snapshot assertions, that is where the red lands.
2. Move the structural check to **WP-18** (wave 6), where every surface it scans is already final,
   or move `command-line.md`'s grammar rewrite and the `subsystem-cli-commands.md` row into WP-15.
   The first is cheaper and keeps WP-15's single-writer argument intact.
3. State the predicate, not only the scope: "invocation" must be defined as a rendered command line
   (which exempts `CLAIM.md`'s prose and `package_cascade_repair.rs:56`'s comment) or as every
   occurrence. A check whose scope is contracted and whose predicate is not will be written to
   whatever makes it green — the exact `quality-core.md` § Unchecked Green shape the rest of this
   plan is careful about.

---

### F-04 · Warn · `push_access_checks_field_is_private` needs a dev-dependency the workspace does not carry, and the plan's own precedent says it needs no test at all

**Where:** `C-011`; `C-069`; Test inventory, WP-5

**Problem:** The F-07 fix made `PushAccess::checks` private — correct, and it closes the property —
and added `push_access_checks_field_is_private`, described as "a compile-fail or trybuild case for
`PushAccess { checks: Vec::new() }`". Two problems with the test, none with the contract.

`trybuild` is not in this workspace. `crates/ocx_lib/Cargo.toml`'s `[dev-dependencies]` are exactly
`anyhow`, `tokio` (`test-util` only) and `h2`, each carrying a paragraph justifying its presence,
and `subsystem-deps.md` gates additions. There is no other compile-fail harness in the tree, so "a
compile-fail case" names no mechanism that exists.

More to the point, the plan already ruled this class the other way. `C-002` says of a structurally
identical compiler-enforced property: "Totality is the **compiler's** job — the match is
wildcard-free by design … **No source-text guard is written for it**." Field privacy is the same
shape: the property holds because `rustc` refuses the literal, and a test asserting "this does not
compile" restates the compiler more weakly.

Secondary, and it is why the test looked necessary: C-011's wording overstates Rust's rule. "…so
`PushAccess { checks: Vec::new() }` **does not compile from anywhere in the crate**" — a private
field is private to its *defining module*, not the crate. `skipped_all()` and the row-upgrade
methods live in that module, and so would an inline `#[cfg(test)] mod tests`, both of which can
write the literal.

**Fix:** Drop `push_access_checks_field_is_private` from WP-5's list and record C-011's privacy the
way C-002 is recorded — the compiler is the control, WP-5's `panel` review confirms the field
carries no `pub`. Reword C-011 to "does not compile from outside the module that declares it", and
have C-069 cite that spelling.

---

### F-05 · Suggest · The Risks table still counts 89 `--package` invocations

**Where:** § Risks, the "The 89 existing `--package` invocations are left behind and all red at 0.7"
row

**Problem:** The r2 F-05 fix updated C-062 (89 + the workflow) and WP-15's note ("90 simultaneous
failures … 90 call sites") and left the Risks row at 89, with a mitigation naming only "the six
modules". F-03 above raises the real total again. Three places now have to agree on a number that
keeps moving.

**Fix:** Drop the count from the row title — "Existing `--package` invocations are left behind and
all red at 0.7" — and let C-062 be the single place the number lives.

---

### F-06 · Suggest · The Parallelization table labels two pre-existing files "Created by WP-5"

**Where:** § Parallelization, the seven-row `File / Created by / Filled by` table

**Problem:** Two of the seven rows name files that already exist and are fully implemented —
`crates/ocx_lib/src/forge/github.rs` (69 KB) and `crates/ocx_lib/src/forge/gitlab.rs` (45 KB); today's
announce ships on both. WP-5 adds trait methods to them and creates nothing. Only the five
`git_*` / `credentials.rs` rows are real creations (verified: `crates/ocx_lib/src/forge/` contains
exactly `api.rs error.rs github.rs gitlab.rs http.rs identity.rs kind.rs poll.rs`). A reader taking
"Created by" literally will expect WP-5's diff to add two files and will read a 69 KB unchanged file
as an ownership violation.

**Fix:** Rename the columns "First writer / Second writer", or annotate the two rows "(existing
file — gains stub methods)".

---

## Summary

| ID | Severity | Title |
|---|---|---|
| F-01 | Block | `ForgeKind::client`'s only caller is in WP-15's `package_announce.rs`, three waves after WP-5 widens the signature |
| F-02 | Block | C-016/C-017 still scoped to WP-6, which cannot write `kind.rs` — the adversary fix moved the code, not the ownership |
| F-03 | Block | C-062's repo-wide check reds on eight files: six unowned (four of them user-facing remediation strings), two owned a wave later |
| F-04 | Warn | `push_access_checks_field_is_private` needs `trybuild`, absent here; C-002's precedent says the compiler is the control |
| F-05 | Suggest | Risks row still says 89 `--package` invocations; C-062 says 90 |
| F-06 | Suggest | "Created by WP-5" applied to two 45–69 KB files that already exist |

**3 Block · 0 High · 1 Warn · 2 Suggest.**

All three Blocks come from the last two fix rounds, and two of the three are one defect class:
**a symbol or file the change must touch whose owner and whose dependents sit in different work
packages.** F-01 and F-02 are the adversary finding's own class, one hop further out — the fix
relocated the implementation and did not follow the symbol outward to its caller, nor inward to the
Scope cell that claims it. F-03 is the r2 F-05 class: a contract generalised from "these six files"
to "repo-wide" without re-running the sweep the new wording implies.

---

## Orchestrator disposition (round 3 of 3, applied)

Every premise below was re-verified against the tree before the edit, not taken from the report.

| ID | Verified how | Applied |
|---|---|---|
| F-01 · Block | `grep -rn '\.client('` over `crates/ocx_cli/src crates/ocx_lib/src` returns exactly one `kind.client(` outside `kind.rs`, at `crates/ocx_cli/src/command/package_announce.rs` | `package_announce.rs` added to WP-5's Expected files as a **call-site-only** edit; the Parallelization table gains an **eighth** row and the preamble says "Eight", with a paragraph stating why this row is not a stub pair; WP-5's per-package note records the mechanical edit and the ancestor rule (`WP-5 → WP-10 → WP-15`) that already covers the pair |
| F-02 · Block | Report's reading of the two Scope cells confirmed in the plan | `C-016, C-017` moved from WP-6's Scope to WP-5's; `client_requires_git_binary_under_git` moved from WP-6's test list to WP-5's; C-015 left with WP-6; WP-5's note states the split and its reason (inline `#[cfg(test)]` in the module under test) |
| F-03 · Block | Independent repo-wide sweep reproduced the file/hit table exactly. `website/.vitepress/dist` confirmed gitignored (`website/.gitignore:9`), so its 2 hits are build output, not surfaces | C-062 rewritten: the swept total is stated as **99 across fifteen files**, the four cascade remediation strings are named as the correctness item, the **predicate** is stated ("a rendered `ocx package announce` command line carrying `--package`", exempting prose and the one comment), the exclusion set is enumerated, and a red-proof is mandated. The six unowned files added to WP-15's Expected files. The structural check **moved to WP-18** (wave 6), which owns the two surfaces the check scans and already depends on WP-15; WP-15 keeps a unit test over the four message builders |
| F-04 · Warn | `crates/ocx_lib/Cargo.toml` `[dev-dependencies]` are `anyhow`, `tokio`, `h2`; `grep -rn trybuild --include=Cargo.toml` returns nothing tree-wide | `push_access_checks_field_is_private` dropped from WP-5's list. C-011 reworded to "does not compile **from outside the module that declares it**" and now states the compiler is the control, citing C-002's precedent; C-069 cites the same spelling |
| F-05 · Suggest | — | Count dropped from the Risks row title; C-062 is named as the single place the number lives |
| F-06 · Suggest | `ls crates/ocx_lib/src/forge/` confirms `github.rs` and `gitlab.rs` pre-exist | Columns renamed **First writer / Second writer**; the two pre-existing files annotated "(existing … file — gains stub trait methods)", the five new ones "(new: stub + `mod` row)" |
| F-12 · Warn (Set A, was not closed) | C-048 and S-007 both promise the override; neither WP-9 nor WP-16 asserted it | WP-9 gains `owner_canonical_login_replaces_supplied_spelling` with its red state stated; WP-16 gains `::test_owner_canonical_login_overrides_supplied_case` |
| F-13 · Suggest (Set A, was not closed) | — | `C-059` added to WP-15's Scope — see the finding below, which the fix surfaced |

### Found while closing F-13 — C-059's parity test was red at its own gate

Not in any review artifact. WP-14 (wave 4) owned `shared_forge_write_options_derived_set_matches_both_commands`,
but the **announce** flatten is an edit to `crates/ocx_cli/src/command/package_announce.rs`, which
is WP-15's file in wave 5. A both-commands assertion owned by WP-14 could not have passed at WP-14's
own merge gate.

Split: WP-14 keeps `shared_forge_write_options_derived_set_matches_claim` (derivation via
`augment_args`, asserted non-empty, over the command it owns); WP-15 — which depends on WP-14 —
adds `shared_forge_write_options_parity_across_both_commands`. C-059 now states the two-wave split
and why. This is the same defect class as F-01 and F-02: a symbol whose owner and whose dependents
sit in different packages.

### Disjointness re-verified after the edits

Eight cross-wave collisions, all eight declared and all covered by an existing ancestor edge. No new
same-wave collision: WP-5 is alone in wave 2; WP-15's six added files appear in no other package;
WP-18's new pytest module collides with nothing in wave 6. No dependency edge added, no wave moved,
19 packages and 7 waves unchanged.

**Round-3 outcome: 0 Block / 0 High open. All three Blocks, the Warn, both Suggests and both
not-closed Set A findings applied. No deferred findings.**
