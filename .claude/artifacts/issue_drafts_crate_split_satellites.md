# Issue drafts — crate-split satellite follow-ups

**Status:** specification only. WP-39 (C-076). Nothing here has been executed,
and nothing here may be executed from this repository: both satellites are the
owner's to change, and the submodule pointer is theirs to bump.

**Why this file exists.** `ocx_lib` was deleted at WP-37. `ocx-mirror` links it
by path and cannot build against this workspace until every import is
re-pointed. That break is recorded in the `BREAKING CHANGE` footer of the
deletion commit; this file is the work it implies, written out so the owner can
hand it to someone (or to an agent) without re-deriving it.

**Measured, not transcribed.** Every count below was re-measured against
`ocx-mirror@d8e74b1` and this worktree on the day of writing, not copied from
`discover_crate_split_ai_config_satellites.md` § Part 2. Where the two disagree
the discovery figure is named beside the current one, because the disagreement
is information: the mirror moved.

---

## Part A — `ocx-mirror`

**State at time of writing.** `main`, HEAD `d8e74b1`, submodule pointer
`3538b755` (`external/ocx`, this repo's `v0.6.2`). Clean.

**Surface.** 493 `ocx_lib` references across 138 files (discovery, at an earlier
HEAD: 511 across 140). Heaviest five:

| File | Refs |
|---|---|
| `src/pipeline/target_registry.rs` | 33 |
| `src/command/package/pipeline/prepare.rs` | 25 |
| `src/pipeline/registry_copy.rs` | 24 |
| `crates/ocx_python/src/compose.rs` | 20 |
| `src/http.rs` | 18 |

### A.1 — the mechanical rewrite

Every `ocx_lib::<head>::…` import re-points by its **module head**. This table
is the whole of the mechanical work; the counts are distinct item paths under
each head, by a shallow extractor (138 paths — the discovery's fuller brace
expander found 179, and the difference is nested `use` groups, not new items).

| `ocx_lib::` head | Paths | Becomes | Note |
|---|---|---|---|
| `oci::index::*` | — | `ocx_index::*` | the index protocol and its store |
| `oci::{sign,attest,verify,simplesigning}::*` | 3 | `ocx_sign::*` | `DiscoveryMethod`, `SignerCandidate`, `list_signature_candidates` |
| `oci::*` (the rest) | 54 total under `oci` | `ocx_oci::*` | largest single group |
| `auth::*` | 3 | `ocx_oci::auth::*` | |
| `package::*` | 23 | `ocx_package::*` | |
| `publisher::*` | 2 | `ocx_package::publisher::*` | |
| `file_structure::*` | 3 | `ocx_index::*` **or** `ocx_store::*` | the six re-exports the discovery corrected live in `store.rs` → `ocx_index`; check each by grep, the mapping is file-grained |
| `env::*` | 6 | `ocx_config::env::*` | |
| `Config`, `resolve_mirror_map` | 2 | `ocx_config::{Config, mirror::resolve_mirror_map}` | `resolve_mirror_map` is `crates/ocx_config/src/mirror.rs` |
| `utility::*` | 5 | `ocx_util::*` | |
| `archive::*`, `compression::*` | 5 | `ocx_util::{archive,compression}::*` | |
| `tls::*` | 3 | `ocx_config::tls::*` | **not** `ocx_util` — `tls.rs` landed in `ocx_config` |
| `log::*` | 4 | `ocx_console::*` | |
| `cli::{ExitCode, ErrorCategory}` | — | `ocx_exit::*` | an exit code is a CLI contract; `ocx_exit` carries the tier |
| `cli::*` (the rest) | 16 total under `cli` | `ocx_console::*` | |

### A.2 — `Error` and `Result` have no successor

`ocx_lib::Error` was a crate-wide union that WP-37 dissolved; there is no type
to re-point to. Nine sites, all in `src/pipeline/target_registry.rs`:

| Sites | Spelling | Resolution |
|---|---|---|
| `:178`, `:240`, `:263`, `:341` | `ocx_lib::Error::OciClient` | match `ocx_oci::client::error::ClientError` directly — the variant was a transparent wrapper, so the inner type is what the code already reads |
| `:174`, `:237`, `:250`, `:337` | `ocx_lib::Result<T>` | `anyhow::Result<T>`, or the tier's own `Result` where the function raises only one tier's errors. The mirror is an application, so `anyhow` is the cheaper default |
| `:399` | bare `ocx_lib::Error` | read the site; it is one value, and the successor is whichever tier raises it |

> The deletion commit's `BREAKING CHANGE` footer states "5 `ocx_lib::Result` …
> and `ocx_lib::Error` at 4 more". The measured split is the reverse — **4
> `Result`, 5 `Error`** (4 of them `Error::OciClient`, 1 bare. Total 9 either
> way). The footer is in a landed commit and is not being amended; this line is
> the correction.

### A.3 — three satellite-linking-rule violations, 13 items

These are **not** mechanical. Each links a crate the satellite linking rule
forbids — internal-tier crates carry no stability obligation at all, which is
why linking one is drift rather than a shortcut. Each needs an owner decision,
and the recommendation is stated so the decision is a yes/no.

| Violation | Items | Sites | Recommended resolution |
|---|---|---|---|
| `ClassifyExitCode`, `LogLevel`, `LogSettings` → `ocx_cli` | 3 | `src/main.rs:8` (import), `src/main.rs:76` (`error.classify().unwrap_or(…ExitCode::ConfigError)`) | **A mirror-local classify function**, plus the mirror's own tracing init for `LogLevel`/`LogSettings`. E3 says no library crate defines or implements `ClassifyExitCode`; the mirror is a second binary and owns its own exit-code policy. One `ClassifyExitCode` use site, verified — not "none" |
| `forge::{ForgeCredentials, ForgeKind, RepoCoordinate, WriteTransport::*}` → `ocx_announce` | 8 | 10 `ocx_lib::forge::` refs | **`ocx package announce` as a CLI call.** Announce is workflow orchestration, which the linking rule reserves to the CLI surface; the mirror driving it by argv is the rule working as intended, not a downgrade |
| `ci::{CiFlavor, annotations::for_flavor}` → `ocx_shell` | 2 | 5 `ocx_lib::ci::` refs | **A mirror-local CI-flavor enum.** Two items, no shared state, and the mirror's CI surface is its own |

**If the owner declines a resolution**, the alternative is a documented
exception to the linking rule for that item, written into
`arch-principles.md` § Core vs Plugin Boundary — not a silent link. The rule is
enforced by `task satellite:verify`, so an undocumented exception shows up as a
build failure nobody can explain.

### A.4 — the three mechanical Cargo lines, unchanged

The consumer re-declares all three; none travels across a path dependency.

1. **`[patch.crates-io]`** — verbatim, pointing into its own submodule
   checkout. Dropping it does not error; Cargo silently resolves the unpatched
   crates.io releases.
2. **The two `serde_json` feature lines.** `preserve_order` (without it the
   index-root serializer alphabetizes and every rewritten root's digest
   changes, with no compile error) and `raw_value` (the attestation path
   splices a caller-supplied predicate verbatim). Post-split these unify with
   `ocx_index` and `ocx_sign` rather than `ocx_lib`; **the consumer declaration
   is unchanged and still mandatory.** The mirror declares `preserve_order` on
   the root package only — `raw_value` is absent and `crates/ocx_python` declares
   neither. Check both against what the mirror actually exercises.
3. **Full-tree git-submodule vendoring** — Cargo cannot resolve a transitive
   path dependency across two git repositories
   ([rust-lang/cargo#14946](https://github.com/rust-lang/cargo/issues/14946)).

### A.5 — the manifests

`Cargo.toml` (root) and `crates/ocx_python/Cargo.toml` each replace the single
`ocx_lib` path dependency with the set the code actually uses. From the table
above that is at most eight ecosystem-tier crates:

```toml
ocx_exit    = { path = "external/ocx/crates/ocx_exit" }
ocx_util    = { path = "external/ocx/crates/ocx_util" }
ocx_console = { path = "external/ocx/crates/ocx_console" }
ocx_oci     = { path = "external/ocx/crates/ocx_oci" }
ocx_config  = { path = "external/ocx/crates/ocx_config" }
ocx_index   = { path = "external/ocx/crates/ocx_index" }
ocx_package = { path = "external/ocx/crates/ocx_package" }
ocx_sign    = { path = "external/ocx/crates/ocx_sign" }
```

Add only what compiles — an unused path dependency is a link the linking rule
has to justify later. `crates/ocx_python` needs a strict subset; derive it from
that crate's own imports rather than copying this block.

### A.6 — order of operations

1. Re-point the imports (A.1, A.2) and the manifests (A.5) **against the
   current submodule pointer's crates**, which do not exist there yet — so this
   step and step 2 land in one commit or the repo does not build.
2. Bump `external/ocx` to a commit at or after this repo's `ocx_lib` deletion.
3. Resolve the three violations (A.3), or record the exceptions.
4. Green on the mirror's own gate (`task rust:build`, `rust:test:unit`,
   `rust:format:check`, `rust:clippy:check` — a `task verify`-equivalent under
   a different name) and on its `verify.yml` `smoke` job, which asserts
   `[patch.crates-io]` is bound to the vendored fork paths via
   `cargo tree -i oci-client` / `-i sigstore`.

---

## Part B — the `continue-on-error` flip, in **this** repo

`verify-deep.yml`'s `satellite-verify` job is non-blocking while the mirror
cannot build (DEC-4). The flip is **deleting one line** and nothing else:

```diff
--- a/.github/workflows/verify-deep.yml
@@ satellite-verify:
-    continue-on-error: true
     runs-on: ubuntu-latest
```

Delete the explanatory comment block above it in the same change — a comment
describing a line that is gone is the stale-pointer defect this plan keeps
finding.

**Carry this diff in the ocx-mirror follow-up PR's companion PR here, not
before it.** Flipping early turns every deep run red for a reason no one in
this repo can fix.

> **Never make this job a required check while the line is set.** The job's own
> check run still concludes `failure` — only the workflow run and `needs.*.result`
> read `success` — so a required check on it blocks every merge until the mirror
> re-points. And if GitHub ever read the job as green, the requirement would
> gate nothing. Either way, required + `continue-on-error` is a gate that cannot
> do its job.

---

## Part C — grimoire onboarding

**Not a break, an addition.** grimoire has **no** `ocx_*` dependency today
(0 hits in its `Cargo.toml`), so nothing there is broken by the split. What
follows is the onboarding the ADR anticipated.

### C.1 — fork reconciliation first, and it is not a version bump

| Repo | `external/rust-oci-client` |
|---|---|
| grimoire | `7f3d0b6c` on `ocx/integration` (tag `v0.17.0-40-g7f3d0b6`) |
| ocx | `e5ed433a` on `ocx/drop-dead-referrers-fallback` |

**Different branch and different commit.** Cargo cannot resolve a transitive
path dependency across two repositories, so the two checkouts must converge on
one commit before any `ocx_*` dependency can be added. Reconciling the fork is
step one and is the only step with unknown cost; everything after it is
mechanical.

`external/docker_credential` also differs (grimoire: `8e89cd0e` on
`feat/store-erase-list`) — same reconciliation, smaller surface.

grimoire has **no `sigstore` dependency**, which confirms it carries no signing
code today. If onboarding adds `ocx_sign`, it inherits one.

### C.2 — the dependency block

Six crates, the ADR's stated set for the signing and registry stack:

```toml
ocx_exit   = { path = "external/ocx/crates/ocx_exit" }
ocx_util   = { path = "external/ocx/crates/ocx_util" }
ocx_oci    = { path = "external/ocx/crates/ocx_oci" }
ocx_trust  = { path = "external/ocx/crates/ocx_trust" }
ocx_sign   = { path = "external/ocx/crates/ocx_sign" }
ocx_config = { path = "external/ocx/crates/ocx_config" }
```

### C.3 — the same three mechanical lines

`[patch.crates-io]` re-declared verbatim into grimoire's own submodule paths
(it already has one for `docker_credential` and `oci-client`; `sigstore` joins
it if `ocx_sign` is taken), both `serde_json` feature lines declared in
grimoire's own manifest, and full-tree submodule vendoring.

`grimoire/CLAUDE.md` names "ocx" zero times today; onboarding adds the linking
rule to it, not to this file.

---

## What this file is not

It is not a migration guide for users, and no part of it belongs in
`website/src/docs/**`. It is not a changelog entry — the deletion commit's
subject is that. It is a work order, and it stops being useful the day both
satellites are green.
