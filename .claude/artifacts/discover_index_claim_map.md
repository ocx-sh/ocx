# Discover: index claim command — claim diff + architecture map

## Metadata

- Date: 2026-09-04 (session date; system clock rolled to 2026-09-05 mid-run, no code changed after the anchor)
- HEAD: `487570fb183b51e824b2004b99640a0c67545ffa` (branch `sion`)
- Anchor: dossier `Updated: 2026-09-04` header, `.agents/discussions/index-claim-command.md` (git-ignored, per `.gitignore:125`; untracked by design, not by accident)
- Working tree at start: `.agents/memory/hex.md` modified (unrelated), four new untracked research artifacts under `.claude/artifacts/research_index_claim_*.md` (the dossier's own research, already accounted for in Task C)

## Task A — Claim diff

**Tally: 7 HOLDS / 0 DRIFTED / 0 WRONG** (against the seven concrete claims enumerated in the assignment; every repo-root-relative path the dossier names was also existence/date-checked — see below).

Several cited files carry a same-day last-commit timestamp (`6feb9c02`, `c897630b`, `4fb16089`, `487570fb`, all 2026-09-04), so per the staleness protocol I re-read each in full rather than trusting the dossier's characterization. All held.

| # | Claim | Path:line | Verdict | Evidence |
|---|---|---|---|---|
| 1 | `require_root` constructs `AnnounceError::UnclaimedNamespace` on a `None` root read | `crates/ocx_lib/src/announce/pipeline.rs:123-134` | **HOLDS** (exact line range) | `pub fn require_root(` at line 123; doc comment naming `UnclaimedNamespace` at line 122; construction at line 129; function closes line 133. Dossier's own `123-134` bracket is precise. |
| 2 | `Forge` trait is the announce↔forge boundary; orchestration never learns which forge | `crates/ocx_lib/src/forge/api.rs` (whole file) | **HOLDS** | Module doc: "`[Forge]` is the whole surface `[crate::announce::announce]` needs... no forge named in any of them." Every method the dossier names by name — `commit_files`, `open_or_update_pull_request`, `compare_branch`, `find_fork`, `ensure_fork`, `sync_fork` — exists with that exact signature. **Adjacent drift, not a dossier claim**: the doc comment says "ten operations" but the trait has **11** async methods (the list above plus `get_file_contents`, `get_ref_sha`, `find_open_pull_request`, `pull_request_mergeability`, `ensure_push_access`). `pull_request_mergeability` was the 11th, added for `adr_announce_diverged_branch_rebuild.md` (landed `6feb9c02`, today) — the doc comment was never bumped. Flagged for Task B/whoever writes the new ADR, not counted against the dossier (which never states a count). |
| 3 | `ForgeKind::from_host` resolves a canonical host to a forge, refusing to guess on a self-hosted one | `crates/ocx_lib/src/forge/kind.rs:34` | **HOLDS** | `from_host` at line 34; used by `resolve` at line 54; referenced in `arch-principles.md`-style doc comments at line 77 (`from_host`). |
| 4 | `OCX_ANNOUNCE_TOKEN` is read exactly once, via `std::env::var`, in this range | `crates/ocx_cli/src/command/package_announce.rs:205-215` | **HOLDS** | Line 205: `let token = std::env::var(OCX_ANNOUNCE_TOKEN)...`. Only one `std::env::var(OCX_ANNOUNCE_TOKEN)` call in the file (a second hit is the `const` declaration at line 21, not a read). |
| 5 | `fake_forge.py` serves both forges' REST surfaces from one process/graph and never speaks a git wire protocol | `test/tests/fake_forge.py:1-25` | **HOLDS** | Module docstring, verbatim: "Serves both forge surfaces from one process over one git object graph: GitHub under `/repos/...`... and GitLab under `/projects/...`" and "minus the git-transport layer OCX does not use (REST-only, design register S1)." `fake_gitlab.py` is imported as a mixin (`GitLabRoutes`), sharing the same in-memory object graph — confirmed via both files' headers. |
| 6 | `forge/gitlab.rs`'s job-token doc comment currently says a CI job token reaches none of files/commits/branches/MRs/forking | `crates/ocx_lib/src/forge/gitlab.rs:148-157` | **HOLDS** (line range exact) | Doc comment spans 148 (`/// An authorized request builder.`) through 157 (`/// credential never enters a URL or a query string...`); the job-token sentence sits at 153-156: "GitLab's job-token access table covers packages, releases, artifacts and environments, and lists none of repository files, commits, branches, merge requests or forking." Unchanged — the dossier's "needs correcting" is prospective, correctly described as not-yet-done. |
| 7 | `environment.md` makes the same claim at this exact line | `website/src/docs/reference/environment.md:128` | **HOLDS** (exact line) | Line 128, verbatim: "A GitLab **CI job token** (`CI_JOB_TOKEN`) does not work here. GitLab's job-token access table covers packages, releases, artifacts and environments; it lists none of repository files, commits, branches, merge requests or forking." |

Two more claims embedded in the Requirements/Threads prose were spot-checked and hold:

- "Publishing tags for a package that has no entry in the index yet is out of scope for `announce`" — confirmed verbatim in `website/src/docs/reference/command-line.md:2679` (the `#package-announce` section), matching the dossier's stated motivation.
- `RepoCoordinate` is `{ host, namespace, project }` with nested-group support and a `full_path()` — confirmed via `forge/kind.rs` usage (`coordinate.host.as_deref()`, `same_host`) and via `plan_announce_gitlab_forge.md` D2, which is where this shape was decided and landed.

### Escaping paths (not read — resolve outside `/home/mherwig/dev/ocx-sion`)

- `../ocx-indexbot/.claude/artifacts/adr_forge_neutral_owners.md` → resolves to `/home/mherwig/dev/ocx-indexbot/.claude/artifacts/adr_forge_neutral_owners.md`. That repo exists on disk as a sibling checkout but is a separate git repository; not read per instructions.

### Other-repo paths (not read — content of a different named repo, whether or not present locally)

- `site/src/docs/how-to/claim-a-namespace.md`, `site/src/docs/reference/namespace-policy.md`, `site/src/docs/reference/entry-schema.md`, `site/src/docs/reference/governance-contracts.md`, `schema/root.schema.json` — all `ocx-sh/index` (the public index repo), addressed in the dossier as `site/src/docs/...` / `schema/...`, never as `crates/`, `test/`, `website/`, or `.claude/` paths in *this* repo.
- `src/ocx_indexbot/cli/validate.py`, `core/validate_entry.py`, `cli/governance_check.py` — `ocx-sh/ocx-indexbot` (the index bot), same reasoning.

No dossier-named path escaped the repo undetected and no repo-root path was misidentified as belonging elsewhere — every `crates/`, `test/`, `website/`, `.claude/`, `.agents/` path the dossier cites exists in `ocx-sion` and was verified in place.

## Task B — Architecture map

### 1. Module map

| Module | Path | Role |
|---|---|---|
| `Forge` trait | `crates/ocx_lib/src/forge/api.rs` | Forge-neutral operation set: **11** async methods (see Task A #2) — `get_file_contents`, `get_ref_sha`, `compare_branch`, `find_open_pull_request`, `pull_request_mergeability`, `find_fork`, `ensure_fork`, `sync_fork`, `ensure_push_access`, `commit_files`, `open_or_update_pull_request`. Also hosts `BranchComparison` (`Identical`/`Ahead`/`Behind`/`Diverged`), `Mergeability` (`Mergeable`/`Conflicting`/`Unknown`), `RefUpdate` (`FastForward`/`Reset`), `CommitBase<'a>` (`repo`, `sha`, `branch`). |
| `ForgeKind` | `crates/ocx_lib/src/forge/kind.rs` | Closed 2-variant enum (`GitHub`/`GitLab`), no `#[non_exhaustive]` (arch-principles convention: internal, exhaustive matches). `from_host` (line 34), `resolve(declared, coordinate)`, `canonical_host()`, `same_host()`, `validate_coordinate()` (GitHub-only flat-namespace check via `github::require_flat_namespace`), `client(token, coordinate) -> Box<dyn Forge>`. `clap_builder::ValueEnum` impl backs `--forge github\|gitlab`. |
| `GitHubForge` | `crates/ocx_lib/src/forge/github.rs` (69K) | REST-only client, "copy-and-own port of grimoire's GitHub forge flow... transport-adjusted to REST-only" (module doc). Multi-file commits via GitHub's git data API (blob→tree→commit→ref), bounded readiness poll for fork provisioning. |
| `GitLabForge` | `crates/ocx_lib/src/forge/gitlab.rs` (45K) | Second `Forge` impl. Module doc states 3 structural differences from GitHub: one-request commit (GitLab's batch commits API), per-file `last_commit_id` concurrency (not compare-and-swap on a ref), and project-id-based addressing (percent-encoded `:id`, nested groups as one segment). Credential today: `PRIVATE-TOKEN` header only (line 166) — the dossier's decision to move to `Authorization: Bearer` for every non-job-token kind is not yet implemented. |
| `identity.rs` | `crates/ocx_lib/src/forge/identity.rs` | Fork-identity verification, forge-agnostic logic in one file "because the guard itself is the same on every forge" (module doc): `fork_identity_from_path`, `verify_github_fork` (case-insensitive path compare), `verify_gitlab_fork` (immutable numeric id compare — "strictly stronger than GitHub's path compare"), `verify_fork_namespace`. |
| `http.rs` | `crates/ocx_lib/src/forge/http.rs` | One shared `build_forge_http_client`: no-redirect policy (prevents cross-host credential replay), embedded Mozilla roots, `ocx/<version>` UA. Has a self-defeating-guard-aware structural test (splits out `#[cfg(test)]` before scanning, asserts exactly one `.redirect(` call) — a documented example of the "structural guard" pattern from `quality-rust.md`. |
| `poll.rs` | `crates/ocx_lib/src/forge/poll.rs` | Bounded exponential-backoff schedule for fork-readiness polling (ported from grimoire, REST-only). |
| `error.rs` (forge) | `crates/ocx_lib/src/forge/error.rs` | `ForgeError` (`#[non_exhaustive]` — public/growable, per the error-enum exemption in arch-principles). Never carries the token in any variant. Implements `ClassifyExitCode` (see §5). |
| `announce.rs` + `announce/{pipeline,request,error}.rs` | `crates/ocx_lib/src/announce.rs` (49K), `.../announce/pipeline.rs` (88K), `.../announce/request.rs` (6K), `.../announce/error.rs` (22K) | Orchestration. `announce.rs` module doc: "self-contained, forge-neutral routine (reused by `ocx-mirror`)"; ADR `adr_announce_publisher_surface.md` D5 places orchestration in `ocx_lib`, CLI a thin wrapper. `pipeline.rs` holds `require_root`/`UnclaimedNamespace`, `current_timestamp`, tag-curation folds. `request.rs` holds the public API types: `TagSelection` (`Replace`/`UnionFile`/`Refresh`/`FromRegistry`), `AnnounceTarget` (`Out`/`Fork`/`Direct`), `AnnounceRequest`, `AnnounceStatus` (`Unchanged`/`Updated`), `AnnounceOutcome`. `error.rs` holds `AnnounceError` (`#[non_exhaustive]`) with an explicit exit-code doc comment (see §5). |
| CLI dispatcher | `crates/ocx_cli/src/command/package.rs` | `Package` enum, one variant per `ocx package <verb>` (21 variants today, `Announce` first), flat `execute()` match — the canonical shape a `Claim(...)` variant would join. |
| `ocx package announce` | `crates/ocx_cli/src/command/package_announce.rs` (522 lines) | `PackageAnnounce` clap `Args` struct; reads `OCX_ANNOUNCE_TOKEN` once (line 205); builds the forge via `kind.client(...)`; calls `announce::announce(&publisher, Some(forge.as_ref()), request)`. |
| `child_process.rs` | `crates/ocx_lib/src/utility/child_process.rs` | Generic subprocess boundary: `exec` (Unix `execvp`, Windows spawn+wait+exit), `spawn_and_wait` (non-diverging, signal-forwarding on Unix, `kill_on_drop`), `propagate_exit_code`/`propagate_exit_status` (128+signum on Unix, full-i32 passthrough on Windows). **Last touched 2026-05-18** — no forge/git awareness at all today; this is the seam a `--transport git` `git` subprocess would extend, not a file that already anticipates it. |
| Exit-code enum + classifier | `crates/ocx_lib/src/cli/exit_code.rs`, `crates/ocx_lib/src/cli/classify.rs` | See §5. |
| `--format json` report type | `crates/ocx_cli/src/api/data/announce.rs` | `AnnounceReport` — see §4 (Reusable code). |

### 2. Dependency graph

- **Forge clients → `http.rs`** for the hardened `reqwest::Client` (one builder, redirect-disabled, shared so "a security control that exists twice is a security control that can drift" — module doc).
- **Forge clients → `identity.rs`** for fork verification; **→ `poll.rs`** for readiness backoff (GitHub only — GitLab's `sync_fork` is a documented no-op per `adr_announce_gitlab_forge.md` D6).
- **`announce.rs`/`pipeline.rs` → `forge::{Forge, BranchComparison, CommitBase, Mergeability, PullRequest, RefUpdate, RepoCoordinate}`**, `oci::index::serialize_root`, `publisher::Publisher`. Orchestration imports the trait and the vocabulary types only — never `GitHubForge`/`GitLabForge` directly, which is the "orchestration never learns which forge" invariant made structural.
- **`ForgeKind::client` → `GitHubForge::new` / `GitLabForge::new`** — the one place a concrete forge type is named outside the forges' own modules and their tests.
- **`package_announce.rs` (CLI) → `announce::{announce, AnnounceRequest, AnnounceTarget, TagSelection}`, `forge::{ForgeKind, ForgeToken, RepoCoordinate}`, `api::data::announce::AnnounceReport`.**
- **`cli::classify::try_classify` → `forge::ForgeError`, `announce::AnnounceError`** (both registered in the ladder; see §5).
- **`ocx-mirror` shells out** (note, not read here): `announce.rs`'s own module doc says the routine is "reused by `ocx-mirror`" as a linked library call, not a subprocess — the dossier's own "ocx-mirror parity (parked)" thread confirms `AnnounceConfig` has no `forge` field yet, so mirror pipelines cannot announce to a GitLab-hosted index today. This is a linkage gap, not a `child_process.rs` concern.
- **Test fixtures → `fake_forge.py` (GitHub REST) + `fake_gitlab.py` (GitLab REST mixin, imported by `fake_forge.py`)**, one shared in-memory git-object graph, wired into pytest via `test/conftest.py::fake_forge` (function-scoped, ephemeral loopback port, pointed at via `__OCX_TESTING_FORGE_BASE_URL`).

### 3. Active patterns

**Adding a new `ocx package <verb>`.** Follow `subsystem-cli.md`'s "Command Module Structure": a new leaf file `command/package_<verb>.rs` with its own `Args` struct (clap derive) + `execute(&self, context)`, added as one more variant on the `Package` enum in `command/package.rs` and one more arm in its `execute()` match (both files: `crates/ocx_cli/src/command/package.rs`). `PackageAnnounce` is the exemplar for a write command needing a credential.

**Env-var credential reading, kept out of subprocesses.** `subsystem-cli.md`'s credential-exemption table:

| Var | Read site | Rationale |
|---|---|---|
| `OCX_IDENTITY_TOKEN` | `command/package_sign.rs` | short-lived OIDC bearer |
| `OCX_KEY_PASSWORD` | `oci/sign/key_backend.rs::key_password` | passphrase, absent/empty must stay indistinguishable |
| `OCX_SIGNING_KEY` | `oci/sign/key_backend.rs`, `trust.rs::compile_key_reference` | raw private key material |
| `OCX_ANNOUNCE_TOKEN` | `command/package_announce.rs` | **"Known-open, cross-repo decision"** — `ocx-mirror` inherits it deliberately via plugin-process ambient env; adding it to the forwarded set would break that inheritance |

Any new credential (the dossier's `OCX_ANNOUNCE_GIT_TOKEN`/`OCX_ANNOUNCE_GIT_USERNAME`) must land in this table, in `ocx_lib::env::keys::CREDENTIAL_KEYS`, and in `environment.md` in the same PR — `subsystem-cli.md` states this as a three-edit checklist, not optional documentation.

**Errors carry forge reasons, redacted.** `adr_announce_gitlab_forge.md` D9 ("errors carry the forge's reason") + D15 ("the credential is redacted out of a forge's error body") — implemented today as `ForgeError::Status { url, status, detail }`, built via `status_detail` (trims/caps/empties the forge's own `{"message": ...}` body). `environment.md`'s `OCX_ANNOUNCE_TOKEN` section states the same guarantee in user-facing terms: "A forge's error body is echoed back in diagnostics, so the token is redacted out of it first."

**Branch-state classification.** `BranchComparison` (`Identical`/`Ahead`/`Behind`/`Diverged`) lives on the `Forge` trait's vocabulary side (`forge/api.rs`), consumed by `pipeline.rs`. `RefUpdate::{FastForward, Reset}` is the write-side counterpart: every ordinary announce uses `FastForward` (surfaces `ForgeError::NonFastForward` on a concurrent write, never silently clobbers); `Reset` is reserved for `adr_announce_diverged_branch_rebuild.md`'s rebuild-on-base path. `CommitBase<'a>` carries `{ repo, sha, branch }` because a fresh/rebuilt branch bases off the **upstream's** default branch, never the fork's potentially-stale one.

**`--out` renders without a forge.** `AnnounceTarget::Out(PathBuf)` — `announce()`'s forge parameter is `Option<&dyn Forge>`; `Out` mode still needs a forge to *read* the committed root (module doc: "a forge is needed for every mode... the token is required by every mode that writes, which is every mode except `--out`" — `package_announce.rs:201-203`), but never writes. This is the pattern the dossier's own `claim --out` design copies verbatim.

### 4. Reusable code (for `claim` + a git transport)

| Need | Reuse verbatim | Location |
|---|---|---|
| Coordinate parsing/validation | `RepoCoordinate` (`{host, namespace, project}`, `full_path()`), `ForgeKind::resolve`/`same_host`/`validate_coordinate`/`client` | `crates/ocx_lib/src/forge/{kind.rs, api.rs}` |
| Fork verification | `verify_github_fork`, `verify_gitlab_fork`, `verify_fork_namespace`, `fork_identity_from_path` | `crates/ocx_lib/src/forge/identity.rs` |
| Hardened HTTP client | `build_forge_http_client` | `crates/ocx_lib/src/forge/http.rs` |
| Root rendering (byte-exact serialization) | `crate::oci::index::serialize_root` (imported in `announce.rs`), the `require_root`/tag-curation folds in `pipeline.rs` | `crates/ocx_lib/src/announce/pipeline.rs` |
| Report scaffolding | `AnnounceReport` — doc-comment convention (plain: one-row table with dashes for absent fields; JSON: explicit null-vs-empty-array rules per field) is the template a `ClaimReport` should follow | `crates/ocx_cli/src/api/data/announce.rs` |
| Exit-code taxonomy | `ExitCode` (`crates/ocx_lib/src/cli/exit_code.rs`) + `ClassifyExitCode`/`ClassifyErrorKind` traits + the `try_classify` downcast ladder | `crates/ocx_lib/src/cli/{exit_code.rs, classify.rs}` |
| Test fixture | `fake_forge.py` (`FakeForge(GitLabRoutes, ThreadingHTTPServer)`), `fake_gitlab.py` (`GitLabRoutes` mixin) — one shared in-memory git object graph (blobs/trees/commits/refs keyed by `owner/repo`), simulating fork lookup/create, branch compare, `commit_files` (incl. GitLab's create-vs-update probe), PR/MR open, reviewer/approval, identity endpoints. **Does not simulate a git-push transport** — the dossier's own open question (git fixture shape) is unanswered by anything in this repo today; extending it is greenfield work. | `test/tests/{fake_forge.py, fake_gitlab.py}`, wired via `test/conftest.py::fake_forge` |
| Acceptance test entry points | `pytest.fixture() fake_forge` (function-scoped, ephemeral port, `__OCX_TESTING_FORGE_BASE_URL` env override) | `test/conftest.py:254-270` |

### 5. Conventions

**Testing.** Acceptance layout per `subsystem-tests.md`: `test/tests/test_*.py`, `test/conftest.py` for session/function fixtures, `__OCX_TESTING_*` env-var seams gated behind the `__testing` Cargo feature (never documented, never forwarded via `apply_ocx_config`). The forge fakes are wired the same way every other fake registry/proxy fixture is (`html_mirror`, `forward_proxy`): function-scoped, ephemeral loopback port, zero real network, torn down via `server.shutdown()`/`server_close()`/`thread.join(timeout=5)`.

**Docs surfaces for a new command.** `website/src/docs/reference/command-line.md` — each command gets an H4 anchor (`#### \`announce\` {#package-announce}`) with prose, a `**Usage**` fenced block, an `**Options**` table (Name/Description/Default), an `**Exit codes**` table (Condition/Exit code), and a `**JSON report**` fenced example — `claim` would follow this exact five-part shape. `website/src/docs/reference/environment.md` — one `###` section per env var with an anchor (`{#ocx-announce-token}`), a table of forge→token-type/scope, and explicit statements of what is *not* supported (the job-token caveat). Both files are auto-scoped by `quality-cli-help.md`'s structural gates (`cli_help_text_has_no_internal_references` etc.) for the clap-facing half; the website prose itself is free of that constraint but is where the ADR-section/ID references the dossier explicitly forbids from `///` doc comments are expected to live instead.

**Changelog = commit subject.** Per `CLAUDE.md`: no `CHANGELOG.md` edits, ever; `cliff.toml` renders the changelog bullet from the commit subject alone, scope as `*(scope)*`, `!` as **BREAKING**.

**Exit-code doctrine (as implemented, not just as documented in `quality-rust-exit_codes.md`).** This repo does *not* use the generic rule's single free-function-with-inline-match shape. Instead: each error type implements `ClassifyExitCode::classify(&self) -> Option<ExitCode>` (or the exhaustive `ClassifyErrorKind::exit_code(&self) -> ExitCode` for pure discriminant "kind" enums), and one shared free function `classify_error` (`crates/ocx_lib/src/cli/classify.rs`) walks the `std::error::Error` source chain via `successors`, downcasting to ~50 registered types via a `try_downcast!` macro, first non-`None` wins, falls through to `ExitCode::Failure`. `AnnounceError` and `ForgeError` are both registered. Two documented erasure traps this pattern exists to avoid: `#[error(transparent)]` variants make `source()` skip past the wrapped error (so `AnnounceError::Ssrf`/`::Forge` must delegate explicitly in their own `classify()`, never rely on the generic walker), and a `Box<ConcreteType>` `#[source]` field erases to `Box<ConcreteType>`'s `TypeId`, not the inner type's (so `AnnounceError::Observe`/`::ObserveDesc` delegate explicitly too). `announce/error.rs`'s module doc states the exact taxonomy for a new `ClaimError` to follow: SSRF refusal → `ConfigError` (78), DNS/registry unreachable → `Unavailable` (69), unresolved tag → `NotFound` (79), unmergeable PR → `DataError` (65), forge 401/403 → `AuthError` (80) via `ForgeError`'s own classification, forge transport failure → `Unavailable` (69). `ExitCode` (79-85) is OCX-specific, `#[non_exhaustive]`, with a numeric-value regression test per variant.

## Task C — Prior decision records

Enumerated every `.claude/artifacts/{adr,design_spec,plan,research}_*.md` whose subject touches forge, announce, index wire format, owners, tokens, or `ocx package *` CLI grammar.

| Path | Status | Binds this design? |
|---|---|---|
| `adr_announce_gitlab_forge.md` | Accepted | **Central precedent.** D1 (trait, orchestration forge-blind) — the exact shape the dossier's Decision 6 extends. D3 (forge declared for self-hosted, never probed) — reused unchanged, dossier says so explicitly. **D0 states "S1 (REST only, no git subprocess) stands unchanged"** — this is the one clause the dossier's new ADR must amend, and the dossier says exactly that ("This amends ADR S1... explicitly"). Not a conflict — a planned, flagged amendment. |
| `adr_announce_publisher_surface.md` | (no `## Metadata`/Status block found — appears never formally closed with an explicit status line, though its content is fully landed) | D5 (orchestration in `ocx_lib`, CLI thin) — cited verbatim in `announce.rs`'s own module doc, landed. D3 ("Token model per forge") is the **historical basis** for the single forge-neutral `OCX_ANNOUNCE_TOKEN`; the dossier's Decision 9 (splitting API vs. push credential) *extends* this, does not conflict — the original D3 explicitly scoped GitLab as "deferred, index-hosted-on-GitLab only" and never addressed a git-push transport at all. |
| `adr_announce_diverged_branch_rebuild.md` | (landed same day, `6feb9c02`) | Directly relevant: added `pull_request_mergeability` to the `Forge` trait (the 11th operation the dossier's Threads section reuses: "the spent-branch case reuses #399's rebuild-on-base"). No conflict — the dossier explicitly builds on it. |
| `plan_announce_gitlab_forge.md` | (execution plan; content fully landed per code inspection) | D1 confirms the trait had **10** operations at GitLab-forge-support time (before `pull_request_mergeability` was added) — corroborates the "ten operations" doc-comment staleness found in Task A #2. D2 is where `RepoCoordinate`'s host/namespace/project shape and the CLI-grammar-widening precedent were decided — directly reused by the dossier's claim command. No conflict. |
| `adr_index_indirection.md` | Accepted (Decisions A-H; F still in flight per its own metadata) | Governs the **local index collection format**, not the git-hosted `ocx-sh/index` repo the dossier's `claim`/`announce` write to — a different "index" in this codebase (local cache vs. published source-of-truth repo). No conflict; different subsystem sharing a name. |
| `adr_oci_index_only_dispatch.md` | Accepted, owner-ratified 2026-07-25 | Same local-index-format subsystem as above. No conflict. |
| `adr_index_routing_semantics.md` | Accepted | Same local-index-format subsystem. No conflict. |
| `adr_index_root_variants.md` | Accepted | Adds a `variants` field to the **published root** wire format (same document the dossier's `claim` writes) — a live precedent for "additive, optional, both writers (ocx `announce`/`claim` and indexbot) must spell it identically." The dossier's owners-field drop is the mirror operation (removal, not addition) of the same wire format; no field-name conflict (`variants` vs `owners`/`login`/`id` are disjoint). No conflict, useful precedent for how a root-schema change is normally staged. |
| `adr_public_index_registry_indirection.md`, `research_sparse_index_formats.md`, `research_index_wire_versioning_trust.md`, `adr_index_sync_performance.md` (Proposed), `decisions_index_sync_perf_autonomous.md` | Various (Proposed / execution-record) | All concern `index.ocx.sh` / `ocx.rs` — the **sparse HTTP resolution index** ocx clients read at install time — a third, distinct "index" concept from both the local cache above and the git-hosted `ocx-sh/index` repo the claim command targets. No conflict; different subsystem. |
| `adr_servable_index_snapshot.md`, `design_spec_servable_index_snapshot.md`, `research_servable_index_snapshot.md` | Accepted / Draft | Same sparse-HTTP-index subsystem as above. No conflict. |
| `research_gitlab_forge_api.md` | (research artifact, no status line) | Background research for the original GitLab-forge-support work; superseded in currency by the dossier's own live-fetched job-token/push-option research (`research_index_claim_prior_art.md` addendum), which corrects it. No conflict — the newer research explicitly supersedes the older on the job-token facts. |
| `research_grimoire_announce_port.md` | (research artifact) | The "grimoire donor" cited by name in `fake_forge.py`'s and `github.rs`'s own doc comments — this is the origin story for the whole `Forge`/fixture pattern, not something the dossier's design touches or could conflict with. |
| `research_index_announce_bots.md` | (research artifact) | Background on indexbot governance/auto-merge patterns (G-04, G-19 come from this lineage) — informs but does not conflict with the dossier's owner-input/authorship-vs-ownership decisions. |
| `feedback_index_routing_semantics.md` | Superseded by `adr_index_routing_semantics.md` | Historical, local-index subsystem, no conflict. |
| `handoff_announce_initiative_state.md`, `handover_index_indirection.md` | Session handoffs (the latter explicitly `PARKED`) | Process artifacts, not design decisions; no conflict. |
| `e2e_results_announce.md` | Evidence artifact ("all five scenarios captured live... 2026-07-27") | Confirms the announce REST path has a live-GitHub e2e proof; the dossier's own Verification section notes the equivalent live check is *missing* for GitLab REST and for any git-transport work (`adr_announce_gitlab_forge.md:262-264`, cited in the council-transport research) — consistent, not conflicting: the gap is real and both the ADR and the dossier already say so. |
| `research_index_claim_{recon,prior_art,archaeology,council_transport}.md` | (this session's own research, untracked) | The dossier's direct supporting research — already the primary source for Task A/B above, not a separate prior-decision record to reconcile against. |
| `research_token_optimization.md` | (research artifact) | **Not relevant** — "token" here means LLM/Claude-Code token-cost optimization (RTK), unrelated to forge credentials. Excluded. |
| `adr_interpolation_token_grammar.md`, `research_interpolation_token_grammar.md`, `review_round1_interpolation_token_grammar.md`, `evidence_prestub_interpolation_token_grammar.md` | (various) | **Not relevant** — "token" here means the `${...}` metadata-interpolation grammar, unrelated to forge/CI credential tokens or the claim command. Excluded. |

**Conflict list**: one, and it is a self-declared, in-progress amendment rather than an oversight — `adr_announce_gitlab_forge.md` D0/S1 ("REST only, no git subprocess" stands unchanged) is superseded by the dossier's Decision 6 (REST reads + git writes under `--transport git`), and the dossier states its own ADR will amend S1 explicitly. No other prior artifact contradicts a decision the dossier records.

## Notes for the downstream architect

- The `Forge` trait's module doc ("ten operations") is stale against the actual 11-method trait; worth a one-line fix in whichever commit next touches `forge/api.rs`, including possibly the claim-command PR itself.
- `GitLabForge::request` (`gitlab.rs:161`) still sends `PRIVATE-TOKEN` unconditionally — none of the dossier's `Authorization: Bearer`-by-default / `JOB-TOKEN`-when-job-token header-selection logic exists yet. This is greenfield implementation work, not a refactor of existing header logic.
- `child_process.rs` has zero forge/git awareness today — a `--transport git` subprocess spawn is new code reusing the exec/spawn_and_wait/exit-code-propagation *pattern*, not an existing seam to extend in place.
- `ocx-mirror`'s `AnnounceConfig` forge gap (no `forge` field) is real and explicitly out of scope per the dossier — confirmed, no code in this repo closes it.
