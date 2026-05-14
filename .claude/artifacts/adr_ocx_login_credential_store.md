# ADR: Credential Helper Write-Path Implementation for `ocx login` / `ocx logout`

- **Status:** Proposed (recommends Option A — fork + upstream)
- **Date:** 2026-05-14
- **Deciders:** Architect (drafting), user (final call — reads ADR, decides direction)
- **Related issue:** [ocx-sh/ocx#89](https://github.com/ocx-sh/ocx/issues/89)
- **Related research:**
  - `.claude/artifacts/research_docker_credential_helper_protocol.md`
  - `.claude/artifacts/research_cli_login_patterns.md`
  - `.claude/artifacts/research_credential_storage_security.md`
  - `.claude/artifacts/research_oras_go_credentials_alignment.md` — semantic alignment with `oras.land/oras-go/v2/registry/remote/credentials`
- **Touches:** `crates/ocx_lib/src/auth/credential_helper.rs` (new), `crates/ocx_lib/src/auth/store.rs` (new), `crates/ocx_lib/src/auth/registry_url.rs` (new — shared canonicalization used by read + write paths), `crates/ocx_lib/src/auth.rs` (modify: route read through `auth/registry_url::canonicalize_registry`), `external/` (only if Option A is chosen)

---

## Context

Issue #89 proposes adding `ocx login REGISTRY` and `ocx logout REGISTRY` to OCX. Today, `crates/ocx_lib/src/auth.rs` implements a read-only auth chain:

1. `OCX_AUTH_{slug}_{TYPE,USER,TOKEN}` env vars (`auth.rs:110-146`)
2. `docker_credential::get_credential(registry)` shelling out to `docker-credential-*` helpers (`auth.rs:81-108`)
3. `oci::native::Auth::Anonymous` (`auth.rs:42`)

The read path is satisfied entirely by the `docker_credential` crate v1.3.3 re-exported at `crates/ocx_lib/src/oci.rs:26-29`. The crate's public API is read-only — it exposes `get_credential`, `get_credential_from_reader`, `get_podman_credential`, and the `CredentialRetrievalError` taxonomy, but **no `store`, `erase`, or `list`** counterparts.

To implement `ocx login` we need three new operations against the Docker credential-helper protocol:

| Op | Stdin | Stdout (success) | Used by |
|---|---|---|---|
| `store` | `{"ServerURL","Username","Secret"}` JSON | empty | `ocx login` (write) |
| `erase` | raw server URL bytes | empty | `ocx logout` |
| `list` | empty | `{serverURL: username}` JSON | future: `ocx auth status` |

Issue body currently states the implementation approach as "fork `keirlawson/docker_credential` and submit upstream PR". Research (`research_docker_credential_helper_protocol.md` §4) disagrees and recommends an in-house module. Both paths are technically viable. This is a **one-way door** in the sense that once we commit code investment (fork branch with months of patches; or in-house module rooted across OCX) the cost of reversal grows fast.

---

## Decision

**Fork `keirlawson/docker_credential` under the `ocx-sh` GitHub organization, add the write API (~120 LOC).** OCX rides the fork via `[patch.crates-io]` at `external/docker_credential/` (analog to `external/rust-oci-client/`). **Upstream PR is deferred** — open it once the fork API has stabilized through OCX use, not as part of `ocx login` v1. When/if upstream PR eventually merges + releases, drop the patch entry, keep upstream as the dep.

**Rationale ranked, most important first**

1. **Symmetric API on one crate.** Every credential operation (read AND write) goes through `docker_credential::*` — cognitive parsimony for OCX maintainers and any future Rust project consuming the crate. No "two implementations of the same protocol in one process" drift surface.
2. **Lock-step versioning of read + write protocol.** Read path (`get_credential`) and write path (`store_credential` / `erase_credential` / `list_credentials`) share the same `run_helper` spawn primitive after the refactor — URL canonicalization, error parsing, sentinel detection, timeout, byte-cap all live in one place inside the fork. Eliminates the symmetry-violation risk a parallel in-house module would carry.
3. **Community contribution.** Other Rust projects needing helper writes (`containers-rs` ecosystem, future `oras-rs`, `helm-rs` wrappers) benefit. OCX has a moral obligation to share when the change is genuinely additive and well-scoped.
4. **Genuinely ~120 LOC, not a hidden complexity sink.** Research established that the upstream crate already implements the protocol cleanly for `get`. The refactor extracts the private `response_from_helper` into a reusable `run_helper(name, action, stdin)` primitive (~30 LOC change), and the three new public functions (`store_credential`, `erase_credential`, `list_credentials`) are each ~15 LOC thin wrappers. Plus error variants + tests. Total: ~120 LOC for the protocol-level work.
5. **Same pattern OCX already uses for `oci-client`.** `external/rust-oci-client/` proves the submodule + `[patch.crates-io]` pattern is operationally fine: contributors clone with `--recurse-submodules`, CI handles it, `cargo update` works. No infrastructure inventing required — copy the playbook.
6. **Block-tier security knobs are designed INTO the fork, not bolted on.** 30s subprocess timeout, 64 KiB stdout cap, sentinel-string `NotFound` detection, helper-path validation — all land in the fork as defaults (not tunables — the same knobs are correct for ANY consumer of this protocol, not OCX-specific). If upstream pushes back on the security defaults during review, OCX still ships against the fork; pull these into a smaller upstream PR scope later.
7. **Reversibility.** If upstream rejects or stalls forever, OCX keeps the fork — same risk profile as `rust-oci-client` today, which OCX has carried with no issue. If upstream merges, drop the `[patch.crates-io]` line and we are done.

The user's preference (recorded in issue #89 refinement) is Option A. This ADR aligns with that preference because the research's "no extractable primitive" objection is misframed: the refactor to extract the primitive IS the contribution — it's a normal Rust crate evolution PR that any maintained crate accepts, and even if rejected we ship against the fork.

---

## Options Considered

### Option A — Fork `keirlawson/docker_credential` under `ocx-sh` org, add write API, upstream PR  [SELECTED]

**Shape**

- Create fork via `gh repo fork keirlawson/docker_credential --org ocx-sh --clone=false --remote=false`. Result: `https://github.com/ocx-sh/docker_credential`. (See `## Fork Setup Playbook` below for full step list.)
- Add submodule at `external/docker_credential/` pointing at `https://github.com/ocx-sh/docker_credential.git` (analog to `external/rust-oci-client/`).
- Add `[patch.crates-io] docker_credential = { path = "external/docker_credential" }` to root `Cargo.toml`. Add `external/docker_credential` to workspace `exclude = [...]`.
- Inside fork: extend the **existing module layout** to add the write API. The fork's structure takes precedence — do NOT impose a new module decomposition (`src/store.rs`, `src/erase.rs`, `src/list.rs`) just because it looked clean on a blank slate. Read what's already there first; add new public functions to the modules that already host the analogous read-side code (e.g. if `get_credential` lives in `src/lib.rs`, add `store_credential` alongside; if `helper.rs` already holds subprocess logic, extend it). Extend `CredentialRetrievalError` with `NotFound`, `Timeout`, `OutputTooLarge`, `NotOnPath`, `UnsafePath`, `InvalidJson` variants.
- **Strict adherence to upstream style**: read `CONTRIBUTING.md` if present; match the existing repo conventions (rustfmt config, error naming, doc-comment style, test layout, async stance). No formatting churn in the diff. No new modules unless upstream's structure naturally calls for them.
- **Upstream PR deferred.** OCX ships against the fork via `[patch.crates-io]`. Once the fork API has stabilized through OCX use (post `ocx login` v1 release + a few iterations of real-world feedback), open the upstream PR. Until then, OCX carries the fork as a normal vendored dep — same posture as `external/rust-oci-client/` today.

**Pros**

- Community contribution. Other Rust projects (e.g., `containers`-rs ecosystem, future `oras-rs`) that need the same write API benefit.
- Symmetric API on one crate — every credential operation goes through `docker_credential::*`. Cognitive parsimony.
- Lock-step versioning — read and write paths can never drift in protocol assumptions because they share the same `Command::spawn` code path.
- If accepted, long-term maintenance burden is the upstream maintainer's, not OCX's.

**Cons**

- **No extractable primitive in v1.3.3.** The spawn logic in `helper.rs::response_from_helper` is private + hard-coded to action="get". To add `store`/`erase`/`list` cleanly, the spawn primitive must first be extracted and made public. That refactor is itself a PR upstream is free to request changes on, delay, or reject.
- **External maintainer responsiveness is an unknown.** `keirlawson/docker_credential` last release v1.3.3 was 2026-05-01; activity cadence and review responsiveness are not part of OCX's risk model. The feature ships at whatever rate the upstream maintainer moves.
- **Carrying a submodule for an indefinite window.** Until PR merges + new crate version is published, every `cargo update` / dependabot run touches the patched dep through OCX's submodule. Same pattern as `external/rust-oci-client/` — non-zero ongoing cost.
- **Security knobs become a negotiation.** Block-tier requirements (30s timeout, 64 KiB cap, sentinel parsing as a typed enum) need to be either upstream defaults or tunable. Upstream may push back on opinionated defaults; tunables expand the public API.
- **CI infrastructure for fork.** The fork needs its own CI for the four credential-helper actions across platforms — most of that infra needs to exist in OCX regardless (acceptance tests with mock helpers), but the fork demands its own crate-level test suite to be a credible PR.
- **The same write functionality must be wired into OCX's `auth::store` either way.** Fork or no fork, OCX still owns the config.json read-modify-write + flock + atomic rename. The fork's contribution is one Rust function per protocol verb — useful, but not the bulk of the work.

### Option B — In-house module at `crates/ocx_lib/src/auth/credential_helper.rs`  [REJECTED]

**Shape**

- New module file with `run_helper` primitive + `store_credential` / `erase_credential` / `list_credentials` / `detect_default_helper` public functions.
- `HelperError` enum derived with `thiserror::Error`, `#[non_exhaustive]`.
- Continue using `docker_credential::get_credential` for the read path (`auth.rs:81-108` unchanged).
- Re-export from `crates/ocx_lib/src/oci.rs` next to existing `docker_credential` re-exports.

**Pros**

- **Pure addition.** No `[patch.crates-io]`, no submodule, no fork, no external maintainer dependency.
- **~80 LOC, self-contained, testable.** The sketch in `research_docker_credential_helper_protocol.md` §4 demonstrates the full surface in 4 functions + 1 primitive.
- **Block-tier security requirements live next to call site.** 30s subprocess timeout via `tokio::time::timeout`. 64 KiB stdout cap via `tokio::io::AsyncReadExt::take(64 << 10)`. Sentinel-string `"credentials not found in native keychain"` detection as a typed `HelperError::NotFound` variant. Helper-path validation via `which::which` (already a workspace dep at `crates/ocx_lib/Cargo.toml:47`).
- **Faster to ship `ocx login` v1.** No external review cycle gating the merge.
- **Reversible.** If `docker_credential` upstream adds a write API in a v2 release, migration is module-replacement scoped to one file plus the few call sites in `auth/store.rs`.
- **Two-tier consistency:** the `docker_credential` crate handles the *only* op we read (`get_credential` for `~/.docker/config.json` lookups in `auth.rs:86`); the OCX module handles the three ops we mutate (`store`/`erase`/`list` driven by `ocx login`/`ocx logout`). Each path is narrow and complete.

**Cons**

- **No community contribution.** Other Rust projects that need the same write API will re-invent it. (Mitigation: open a tracking issue upstream pointing at OCX's implementation; if/when upstream decides to add a write API, our module is a reference impl.)
- **Two subprocess code paths in OCX's compile graph.** `docker_credential::get_credential` (for reads) and `auth::credential_helper::run_helper` (for writes) both shell out to `docker-credential-*`. The protocol invariants (server URL via stdin, errors via stdout, sentinel string for not-found) must be honored by both. Drift risk is low (the wire protocol is stable, documented at `github.com/docker/docker-credential-helpers`) but non-zero.
- **OCX owns the maintenance.** When a new credential helper ships with quirks (e.g., `docker-credential-ecr-login` already exhibits the "hang on stalled IMDS" failure mode), OCX's module is where the workaround lives.

### Option C — Skip Docker config entirely, use `keyring-core` for direct OS keychain access  [REJECTED]

Listed for completeness; rejected.

- `keyring-core` (v1.0.0, 2026-04-21, 29 stars). The "not for production" warning has shifted as of April 2026 — `keyring-core` v1.0 is the new production target after `keyring-rs` v4.0.0 repositioned itself as a sample app. Updated assessment: `keyring-core` is likely production-ready. Still defer for v1: bypasses Docker config.json compatibility users expect, and adds 3+ new platform-store deps; revisit in v2 as fallback when no `docker-credential-*` helper is on PATH.
- Bypasses the Docker config.json compatibility users expect (`docker login` and `ocx login` should be interchangeable for the same registry).
- Different problem domain — direct keychain access from process memory, not subprocess to vetted helper binaries.
- Defer to v2 evaluation once `keyring-core` matures.

---

## Trade-off Matrix

| Axis | Option A — Fork + Upstream | Option B — In-house | Option C — `keyring-core` |
|---|---|---|---|
| **Maintenance burden** | High — submodule + upstream PR cycle + ongoing fork sync until merge | Low — ~80 LOC self-contained + tests | Medium — direct platform keychain API surface, immature lib |
| **Time to ship v1** | Slow — bounded by upstream review pace | Fast — single feature branch | Medium — but blocked by lib maturity |
| **Community value** | High — usable by every Rust project needing helper writes | None — OCX-internal | None |
| **OCX-specific security guarantees** | Harder — must land in upstream or expose as tunables | Easy — knobs live at call site | Different — bypasses Docker proto entirely |
| **Docker `~/.docker/config.json` compat** | Yes | Yes | No — breaks tool interop |
| **Reversibility** | High — delete fork + submodule when upstream catches up | High — replace module file when upstream ships write API | Low — recommits user data to OS keychain only |
| **External dependency on third party** | High — upstream maintainer pace | None | High — `keyring-core` maturity |
| **Workspace dep additions** | None (already have `docker_credential`) | None (already have `which`) | High (`keyring-core` + transitive) |
| **One-way-door risk** | Submodule entanglement grows with time spent on fork | Module is one file scoped to `auth/credential_helper.rs` | Lock-in to OS-keychain semantics, no Docker config |

---

## Consequences

### Direct consequences of Option A

- New GitHub repo `ocx-sh/docker_credential` (fork of `keirlawson/docker_credential`). See `## Fork Setup Playbook` below.
- New submodule at `external/docker_credential/` (mirrors `external/rust-oci-client/` pattern).
- New `[patch.crates-io] docker_credential = { path = "external/docker_credential" }` line in workspace `Cargo.toml`.
- Workspace `exclude` extended with `external/docker_credential`.
- Inside fork (~120 LOC) — module landings are illustrative; **actual placement follows upstream's existing structure**, decided after reading the repo:
  - Refactor wherever the existing `response_from_helper`-equivalent subprocess primitive lives: extract a public `run_helper(name, action, stdin)` reusable across all four protocol verbs.
  - Add `store_credential`, `erase_credential`, `list_credentials` public functions in whichever module already hosts the analogous `get_credential` code. Do NOT introduce new top-level modules unless upstream's conventions support it.
  - Extend `CredentialRetrievalError` enum (in its existing location): `NotFound`, `Timeout { seconds: u64 }`, `OutputTooLarge { cap_bytes: usize }`, `NotOnPath { name: String }`, `UnsafePath { name, path }`, `InvalidJson(#[source] serde_json::Error)`.
  - 30s subprocess timeout — implementation chosen by upstream's async stance (`tokio::time::timeout` if upstream already uses tokio; otherwise sync `std::process::Command` + thread-based timeout via `recv_timeout`). **Do not pull tokio into a sync upstream crate.**
  - 64 KiB stdout cap via `Read::take(64 << 10)` or `AsyncReadExt::take`, matching the chosen sync/async stance.
  - Tests in upstream's existing test layout (likely `tests/` integration directory OR `#[cfg(test)] mod tests` inline blocks — match what's there).
- New module `crates/ocx_lib/src/auth/store.rs` (~250 LOC) wrapping `~/.docker/config.json` read-modify-write under a `FileLock` (existing primitive at `crates/ocx_lib/src/file_lock.rs`) + atomic rename. Resolution order: `credHelpers[reg]` → `credsStore` → `auths[reg]` plaintext fallback. Calls `docker_credential::{store_credential, erase_credential, list_credentials}` from the patched fork.
- `AuthError` (`crates/ocx_lib/src/auth/error.rs:11-21`) gains variants: `WriteConfigFailed { path, source }`, `Helper(docker_credential::CredentialRetrievalError)`, `NoCredentialStoreAvailable`, `LoginRejected { registry }` (produced by `auth::login()` Ping-then-Put when the registry rejects the credential — see oras-go alignment).
- **Semantic alignment with `oras.land/oras-go/v2/registry/remote/credentials`** baked into the design (see `research_oras_go_credentials_alignment.md` §10):
  - `Credential` is a **flat struct** `{username, password, refresh_token, access_token}` (secrecy-wrapped), NOT an enum. Matches oras-go `auth.Credential` and the docker-helper wire format 1:1.
  - `CredentialStore` trait has exactly three async methods — `get`, `put`, `delete` — mirroring oras-go `Store` interface verbs.
  - `get()` returns `Result<Option<Credential>, AuthError>` (idiomatic Rust) instead of oras-go's zero-value sentinel `EmptyCredential`.
  - `put()` returns `Result<(), AuthError>` — no `StoreLocation`. Tier is implementation detail.
  - `delete()` returns `Result<(), AuthError>` — no `EraseResult::Removed/Noop`. UI distinction unwanted.
  - `login()` / `logout()` are **module-level functions**, not methods on `CredentialStore`. `login()` performs `Ping(ctx)` (`GET /v2/`) BEFORE `store.put()` — bad credentials never reach the store. Single most load-bearing security invariant. Matches oras-go `credentials.Login` exactly.
  - `StoreOptions::allow_plaintext_put: false` default — `ocx login --allow-insecure-store` flag opts in. Matches oras-go safe default.
  - Registry canonicalization (`auth::registry_url::canonicalize_registry`) mirrors oras-go `ServerAddressFromRegistry` (incl. `docker.io` → `https://index.docker.io/v1/` alias).
- `ClassifyExitCode` impl on `AuthError` (`auth/error.rs:23-30`) extends to map new variants to `IoError(74)`, `ConfigError(78)`, `AuthError(80)`, `TempFail(75)`, `DataError(65)`.
- New deps: `rpassword = "7"` in **`crates/ocx_cli/Cargo.toml`** (TTY-masked prompts live in the CLI crate, not the lib — interactive prompting is a presentation concern). `secrecy = "0.10"` in `crates/ocx_lib/Cargo.toml` (`SecretString` with zeroize-on-drop + `Debug` redaction; `Credential` type lives in `auth/store.rs`). `which` already at workspace `v8` line 47. Both new deps satisfy `subsystem-deps.md` policy (Apache-2.0 / MIT, no advisories, well-maintained).

### Consequences accepted by choosing Option A over B

- Fork lifecycle management: `cargo update` of the patched dep depends on submodule HEAD; bumping requires submodule `git pull` + verifying upstream changes don't conflict. Same operational pattern as `external/rust-oci-client/`.
- Upstream PR review tail is non-blocking but ongoing — issue tracker thread, occasional review responses. Mitigation: ride the fork until merge; design the PR to be self-contained so review is bounded.
- Sentinel-string + timeout + cap defaults must be defended in upstream review. Mitigation: defaults match what `oras-go`/`docker-cli` already use; cite during review.

### Compatibility & invariants preserved

- `~/.docker/config.json` schema fully preserved — unknown fields round-tripped via `#[serde(flatten)] other: serde_json::Map<String, Value>` (matches `docker` and `oras` behavior; verified against `research_docker_credential_helper_protocol.md` §3).
- Existing read path (`auth.rs:81-108` calling `oci::native::get_docker_credential`) untouched. `ocx pull` and `ocx install` continue to resolve credentials via the unchanged chain.
- Env-var auth (`auth.rs:110-146`) remains highest-priority. `ocx login` writes to the store; env vars still override at read time for CI scenarios.
- Sentinel-string detection: in-house module's `HelperError::NotFound` enum variant matches the exact byte-trim semantics documented at `research_docker_credential_helper_protocol.md` §1.

---

## Block-tier Security Requirements (apply regardless of option)

From `research_credential_storage_security.md` §5. All eight must be satisfied before merge — in-house module makes each enforceable next to the call site:

| # | Requirement | Lives in (Option A) |
|---|---|---|
| 1 | `--password VALUE` literal refused at parse — exit 64 + stderr message | `crates/ocx_cli/src/command/login.rs` clap struct (flag omitted; documented in `--help`) |
| 2 | `--password-stdin` mandatory in non-TTY context — strip exactly one trailing `\n`, no echo | `command/login.rs::execute` (IsTerminal + Read::read_to_string) |
| 3 | Helper subprocess via stdin only, never argv — argv carries action verb | `external/docker_credential/src/helper.rs::run_helper` (fork) |
| 4 | Helper wrapped in `tokio::time::timeout(30s)` | `external/docker_credential/src/helper.rs::run_helper` (fork) |
| 5 | Atomic config write under exclusive flock with write-to-temp-then-rename | `auth/store.rs::CredentialStore::store` using `file_lock::FileLock::lock_exclusive_with_timeout` |
| 6 | No `log::*` / `tracing::*` references credential-bearing value — enforced structurally via `secrecy::SecretString` | `Credential` enum in `auth/store.rs` holds `SecretString`; `Debug` impl redacts |
| 7 | Helper-binary path resolved via `which::which`, validated against `/usr/bin/`, `/usr/local/bin/`, `~/.docker/`, or platform-equivalent allowlist | `external/docker_credential/src/helper.rs::resolve_helper_path` (fork) |
| 8 | `~/.docker/config.json` created with mode 0600 (Unix) — `OpenOptionsExt::mode(0o600)` | `auth/store.rs::ensure_config_file` |

---

## Fork Setup Playbook

Concrete commands the implementing developer runs in order. Mirrors the `external/rust-oci-client/` pattern step-for-step.

### 1. Create the fork under `ocx-sh`

```sh
# Authenticate as a user with push rights to ocx-sh org.
gh auth status

# Fork keirlawson/docker_credential into ocx-sh.
gh repo fork keirlawson/docker_credential --org ocx-sh --clone=false --remote=false

# Verify fork exists at https://github.com/ocx-sh/docker_credential.
gh repo view ocx-sh/docker_credential --json url,parent
```

### 2. Add as a submodule + workspace patch

```sh
# From repo root.
git submodule add https://github.com/ocx-sh/docker_credential.git external/docker_credential
cd external/docker_credential
# Pin to a known-good upstream commit before adding patches.
git checkout v1.3.3
cd -

# Edit root Cargo.toml:
#   [patch.crates-io]
#   docker_credential = { path = "external/docker_credential" }
#   oci-client = { path = "external/rust-oci-client" }
#
# And extend workspace exclude:
#   exclude = ["external/rust-oci-client", "external/docker_credential"]

# Commit the submodule + Cargo.toml change.
git add .gitmodules external/docker_credential Cargo.toml
git commit -m "chore(deps): vendor docker_credential fork under external/"
```

### 3. Read upstream conventions BEFORE writing any code

```sh
cd external/docker_credential
# Required reading:
[ -f CONTRIBUTING.md ] && cat CONTRIBUTING.md
[ -f rustfmt.toml ] && cat rustfmt.toml
[ -f .editorconfig ] && cat .editorconfig
cat Cargo.toml          # check edition, MSRV, async stance (tokio? sync?)
ls tests/               # match existing test layout
git log --oneline -20   # observe commit message style
```

**Strict adherence rules for the fork branch:**

- Match the upstream `rustfmt.toml` byte-for-byte (or use defaults if file absent). No reformat of unrelated files.
- Match the existing error-variant naming, doc-comment style, and module decomposition. New variants follow the same `#[error("...")]` prose register as existing ones.
- If upstream is **sync** (no tokio dep), the new write API stays sync. Use `std::process::Command` + a thread-based 30s timeout (`std::thread::spawn` + `recv_timeout`), not `tokio::time::timeout`. **The fork's async stance is decided by upstream, not OCX.**
- If upstream lacks `Cargo.toml` features for optional deps, do not introduce them.
- New tests use the existing test harness — no `cargo nextest` overrides, no new dev-deps unless strictly required.
- Commit messages follow the upstream style observed via `git log` (likely Conventional Commits if present; otherwise match what the maintainer uses).

### 4. Implement the write API on a feature branch

```sh
cd external/docker_credential
git checkout -b feat/store-erase-list
# Implement run_helper extraction + store_credential + erase_credential + list_credentials.
# All ~120 LOC of additions per the Consequences section above.
cargo test
cargo fmt --check
cargo clippy -- -D warnings
git commit -m "<style-matching-upstream>: add store/erase/list credential helper API"
git push origin feat/store-erase-list
```

### 5. Upstream PR — DEFERRED

**Do not open the upstream PR as part of `ocx login` v1 work.** Ship against the fork first; let the fork API stabilize through OCX use; gather real-world feedback (CI-leg flakes, edge cases, etc.); then open a focused upstream PR with battle-tested code.

Sync upstream `master` into fork periodically (monthly is fine — no PR pending means no rebase pressure):

```sh
cd external/docker_credential
git fetch upstream
git checkout main   # or master, whatever upstream uses
git merge upstream/main  # or rebase
git push origin main
```

OCX's feature branch in the fork tracks the upstream sync.

### 6. When OCX is ready to upstream (future, not v1)

Open PR via `gh pr create --repo keirlawson/docker_credential --base <upstream-default> --head ocx-sh:<feature-branch>` with a body summarizing changes, citing this ADR and issue #89. Respond to review; squash + rebase per maintainer request.

### 7. If/when the upstream PR eventually merges + releases

- Bump `docker_credential` version in `crates/ocx_lib/Cargo.toml`.
- Drop the `[patch.crates-io] docker_credential = ...` line.
- `git submodule deinit -f external/docker_credential && git rm -rf external/docker_credential && rm -rf .git/modules/external/docker_credential`.
- Commit: `chore(deps): drop docker_credential fork; upstream merged write API`.

---

## Future Work (re-evaluate)

Conditions that would flip the recommendation back to Option A or trigger migration off Option B:

1. **`docker_credential` v2.x ships a write API.** When (if) upstream adds `store_credential` / `erase_credential` / `list_credentials` with comparable security guarantees, migrate OCX's `auth::credential_helper` module to a thin shim over the crate and delete the bulk of `run_helper`. Tracking issue should be opened against `keirlawson/docker_credential` after Option B lands, with a link to OCX's module as a reference implementation.
2. **Cargo-style credential-provider plugin protocol (cargo 1.74+) gains adoption beyond cargo.** If a multi-tool plugin protocol emerges (JSON stdin/stdout, similar to Docker helpers but generalized), evaluate migrating both read and write paths to that protocol.
3. **`keyring-core` v1.0 reaches broader adoption.** Production-readiness inflection already underway (April 2026 — `keyring-rs` v4.0 repositioned the lib as sample; `keyring-core` v1.0 is the new target). Re-examine as a Layer-0 backend Docker helpers fall through to when no `docker-credential-*` binary is on PATH.
4. **OIDC / device-code flows for `ocx.sh`.** Separate ticket; would land alongside `ocx login --device-code` and possibly fold into a `CredentialProvider` trait that abstracts Basic/Bearer/OIDC. The `auth::credential_helper` primitive stays useful as the Docker-compat backend.

---

## References

- `.claude/artifacts/research_docker_credential_helper_protocol.md` — Wire protocol authoritative spec, in-house sketch (§4), edge-case table (§6)
- `.claude/artifacts/research_cli_login_patterns.md` — UX convergence across `oras`, `docker`, `gh`, `cargo`, `helm`, `crane`; recommended OCX CLI shape
- `.claude/artifacts/research_credential_storage_security.md` — Threat model, secret lifecycle hazards, eight block-tier security requirements
- `crates/ocx_lib/src/auth.rs` — Current read-only auth chain
- `crates/ocx_lib/src/auth/error.rs` — `AuthError` enum to extend
- `crates/ocx_lib/src/oci.rs:26-29` — Current `docker_credential` re-exports
- `crates/ocx_lib/src/cli/exit_code.rs` — `ExitCode` taxonomy (AuthError=80, UsageError=64, ConfigError=78, IoError=74)
- `crates/ocx_lib/src/cli/classify.rs:121` — Existing `AuthError` downcast in classifier
- `crates/ocx_lib/src/file_lock.rs` — `FileLock::lock_exclusive_with_timeout` primitive used for atomic config write
- `crates/ocx_lib/Cargo.toml:35,47` — Existing `docker_credential = "1.3.2"` and `which = "8"` deps
- `.claude/rules/quality-rust-errors.md` — Error message conventions (lowercase, no period, `#[source]`, `#[non_exhaustive]`)
- `.claude/rules/quality-rust-exit_codes.md` — `ExitCode` design, classification protocol
- `.claude/rules/subsystem-cli.md` / `subsystem-cli-commands.md` / `subsystem-cli-api.md` — CLI surface and Printable layer conventions
- `.claude/rules/product-context.md` — Backend-first, offline-first, content-addressed positioning informing "no surprise downloads" + "CI-friendly exit codes"
- [Rust API Guidelines `C-GOOD-ERR`](https://rust-lang.github.io/api-guidelines/interoperability.html#error-types-are-meaningful-and-well-behaved-c-good-err) — Error message style
- [FreeBSD `sysexits.h` manpage](https://man.freebsd.org/cgi/man.cgi?sysexits) — Exit code numeric anchors
- [`docker/docker-credential-helpers`](https://github.com/docker/docker-credential-helpers) — Canonical helper protocol Go source
- [`keirlawson/docker_credential`](https://github.com/keirlawson/docker_credential) — Upstream crate examined for fork viability

---

## Approval

Recommendation: **Option A — fork under `ocx-sh` org, upstream PR**. Aligns with the user's stated preference in the issue #89 refinement. Plan in `.claude/state/plans/plan_ocx_login.md` follows this option — `external/docker_credential/` submodule + `[patch.crates-io]` line + ~120 LOC fork changes. The rest of the design (CredentialStore, login/logout commands, AuthError extensions, security requirements, UX, exit-code taxonomy) is unchanged from the earlier Option-B draft.
