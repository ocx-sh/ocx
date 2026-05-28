# ADR: Build provenance baking for `ocx version`

## Metadata

**Status:** Accepted (post-hoc — implementation already merged on `sion`)
**Date:** 2026-05-28
**Date Accepted:** 2026-05-28
**Deciders:** mherwig
**Beads Issue:** N/A
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md` — Rust 2024, pure-Rust git via `gix` (no native libgit2), build-script pattern is idiomatic Cargo.
**Domain Tags:** api | devops | observability
**Supersedes:** N/A
**Superseded By:** N/A

## Context

`ocx` is a backend tool (`product-context.md` #1). Bug reports come from
CI logs, automation scripts, third-party tools — never from a human in
front of a terminal. The pre-enrichment `ocx version` printed only the
`Cargo.toml` value, which collapsed every kind of binary to the same
string: a tarball-built `0.3.1`, a dev-deploy `0.3.1` cut from a dirty
working tree, and a stable cargo-dist `0.3.1` were indistinguishable.

Three concrete pain points forced the change:

1. **Bug-report fidelity.** Without commit SHA, dirty flag, build
   timestamp, target triple, and rustc version, an issue filed against
   `ocx 0.3.1` could match any of a dozen binaries on disk across the
   release-channel matrix. The maintainer's first round-trip was always
   "which exact build?".
2. **`ocx self update` reliability.** The self-update flow
   (`update_check.rs::query_installed_version`) spawns the
   currently-installed binary with `env_clear()` and parses
   `ocx --format json version` to learn what's on disk. That JSON
   contract was a single key (`version`). Any non-additive change to the
   payload would silently route updates to bootstrap mode.
3. **Dev-deploy binaries lying about identity.** Dev-deploy CI publishes
   artifacts whose OCI tag is `0.3.2-dev+20260528143045` while the
   workspace `Cargo.toml` still says `0.3.1`. A pre-enrichment dev
   binary self-reported `0.3.1`, breaking every "which version is
   actually running?" diagnostic for the dev channel.

The implementation already merged on `sion` (commits `17c45451`,
`27548fdd`, `de27008b`) addresses all three. This ADR records the
decisions retroactively so future contributors do not re-litigate
them.

## Decision Drivers

- **Backend-first diagnostics.** Machine-consumable provenance must be
  in `ocx --format json version` by default; humans get the enriched
  view only behind `--verbose`.
- **Wire-contract stability.** `query_installed_version` parses the JSON
  payload; any additive change must keep the `version` key intact and
  in the same shape.
- **Hermetic-subprocess safety.** `query_installed_version` calls
  `env_clear()` before spawning the installed binary. Build provenance
  must survive a stripped env — that rules out reading anything at
  runtime from `std::env`.
- **Tarball builds must still compile.** A `cargo build` from a release
  tarball with no `.git/` directory must produce a working `ocx`, just
  with degraded provenance.
- **Supply-chain discipline.** Build-deps still ship in `Cargo.lock`;
  every transitive crate is a review surface.
- **Incremental-build friendliness.** Local `cargo build` must not
  re-link `ocx_cli` on every invocation just to bake a timestamp.

## Considered Options

### Option 1: `vergen-gix` build script + `option_env!` consumer (chosen)

**Description:** A `build.rs` in `ocx_cli` calls `vergen-gix` to emit
`cargo:rustc-env=VERGEN_*` instructions for git, build, cargo, and
rustc metadata. `crate::app::build_info` reads each value via
`option_env!()` — every field is `Option`. CI also pass-throughs
`__OCX_BUILD_VERSION`, `__OCX_BUILD_CHANNEL`, and the `GITHUB_*`
context variables through the same `cargo:rustc-env` mechanism.

| Pros | Cons |
|------|------|
| Pure-Rust git (`gix`); no libgit2 / native dep | Build-dep tree grows by ~31 transitive `gix-*` crates |
| Every field optional → tarball builds compile cleanly | `vergen-gix` is pinned exactly (`=9.1.0`) to mitigate floating-version churn across a wide transitive surface |
| `option_env!` resolves at compile time → values baked as `&'static str`; `env_clear()` cannot strip them | `option_env!` callers must remember the `Option`-ness — every accessor returns `Option<T>` |
| Build script is idiomatic Cargo; no new tool in the build pipeline | Build script must opt out of `build_timestamp` outside CI or every local `cargo build` re-links `ocx_cli` |

### Option 2: Shell out to `git` at runtime

**Description:** `ocx version` invokes `git rev-parse HEAD`,
`git describe`, etc. at runtime to learn provenance.

| Pros | Cons |
|------|------|
| Zero build-time machinery | Adds a runtime dep on `git` for a diagnostic command |
| Always reflects the current working copy | Wrong answer in production — published binaries are not in a git checkout |
| | Fails inside `env_clear()` (no `PATH`) — exactly the path self-update uses |
| | Slow (forks per field), and a hung `git` would stall `ocx version` |

### Option 3: The `built` crate

**Description:** Use the `built` crate, which generates a Rust source
module with build-time constants.

| Pros | Cons |
|------|------|
| Simple consumer ergonomics (everything is a `const`) | Less feature parity with `vergen-gix` for git data (no `describe --dirty --tags` out of the box) |
| Smaller transitive tree | Generates a source file in `OUT_DIR` — extra `include!` ceremony |
| | Same `&'static str` semantics; no real win over `option_env!` |

### Option 4: Hand-rolled `cargo:rustc-env` from a Bash build script

**Description:** Replace `vergen-gix` with a `build.rs` that `Command`s
out to `git` and `date` and emits its own `cargo:rustc-env` lines.

| Pros | Cons |
|------|------|
| Smallest build-dep tree (none) | Re-implements `vergen-gix` poorly; cross-platform `git`/`date` handling is precisely what `gix` exists to remove |
| | Tarball-without-`.git/` handling has to be re-derived |
| | Newline-injection / quoting bugs (the very class `pass_through_env` guards against) become our problem |

## Decision Outcome

**Chosen Option:** Option 1 — `vergen-gix` + `option_env!`.

**Rationale:** The chosen shape satisfies all four hard constraints
(backend-first JSON, additive wire contract, hermetic-subprocess
safety, tarball compiles) with the smallest amount of
custom code. The cost is concentrated in one place (the build-dep
graph) and bounded by exact pinning. Options 2 and 4 fail the
hermetic-subprocess constraint outright; Option 3 has worse git
ergonomics and no offsetting upside.

### Decisions (binding)

1. **`vergen-gix` is the build-time provenance source.** Pinned
   exactly (`=9.1.0`) in `crates/ocx_cli/Cargo.toml`. Floating ranges
   are forbidden — the build-dep tree is wide (~31 transitive `gix-*`
   crates) and a silent `cargo update` bump is a supply-chain risk
   review cannot see.
2. **Consumer side uses `option_env!()` exclusively; every field is
   `Option`.** A tarball checkout without `.git/` compiles and runs.
   Missing fields are omitted from JSON via
   `#[serde(skip_serializing_if = "Option::is_none")]` and suppressed
   in the human-readable verbose view.
3. **The JSON wire contract is "the `version` key is required;
   everything else is additive."** `query_installed_version`
   (`crates/ocx_lib/src/package_manager/tasks/update_check.rs`) parses
   only `version`; the rest of the payload is best-effort enrichment.
   Old self-update parsers keep working when the schema grows. New
   top-level keys are allowed; renaming or removing `version` is a
   breaking change requiring its own ADR.
4. **`__OCX_*` double-underscore prefix marks an implementation-detail
   seam, not a public CLI/env contract.** `__OCX_BUILD_VERSION` and
   `__OCX_BUILD_CHANNEL` are set by dev-deploy CI to override the
   embedded version string and label the release line; user-facing env
   vars use the single-underscore `OCX_*` prefix and are listed in
   `website/src/docs/reference/environment.md`. The double-underscore
   variants MUST NOT appear in user docs and MUST NOT be relied on by
   downstream tools.
5. **Hermetic-subprocess invariant.** Every provenance accessor in
   `crate::app::build_info` resolves through `option_env!`, never
   `std::env`. The values are baked into the binary as `&'static str`
   constants and survive `env_clear()` in
   `query_installed_version`. Adding a runtime `env::var` read to any
   field on this path is a regression — the self-update bootstrap
   branch will swallow the failure silently. The `Version::execute`
   doc-comment carries the same warning at the consumer site.
6. **`channel` carries release-line identity.** `__OCX_BUILD_CHANNEL`
   (`dev` for dev-deploy artifacts, eventually `stable` for cargo-dist
   releases) lets bug reports distinguish a dev binary from a stable
   one with identical SHA. Absent on local `cargo build`; JSON omits
   the field, verbose plain-text suppresses the row.
7. **`build_timestamp` is gated on `CI`.** `BuildBuilder::default()
   .build_timestamp(in_ci)` opts local builds out of the
   `VERGEN_BUILD_TIMESTAMP` emit so incremental rebuilds stay
   incremental. The `cargo:rerun-if-env-changed=CI` line keeps the
   gate observable.

### Consequences

**Positive:**
- Bug reports get rich provenance (commit + dirty + timestamp +
  target + rustc + CI run URL) for free in `ocx --format json version`.
- `ocx self update` keeps working unchanged — the JSON wire contract
  is preserved by construction.
- Dev-deploy binaries now self-identify as
  `0.3.2-dev+20260528143045` with `channel: dev`, closing the "which
  binary is this?" diagnostic gap.
- Verbose plain-text view (`ocx version --verbose`) gives humans a
  scannable labelled-value list reusing theme entity paints.
- Tarball builds (`.git/`-less) still compile; missing fields just
  drop out of the payload.

**Negative:**
- Build-dep tree grows by ~31 transitive `gix-*` crates. Mitigated by
  exact pinning of `vergen-gix` — any version bump is a reviewable
  diff in `Cargo.lock`.
- Local builds without `CI=1` get no `build_timestamp` field in
  `ocx version` output. Acceptable: local builds are not artifacts
  anyone files bugs against.
- Consumer code carries `Option`-handling boilerplate for every field.
  The cost is contained to `build_info.rs` and the `Printable` impl
  in `api/data/version.rs`.

**Risks:**
- `vergen-gix` upstream churn: an unreviewed major-version bump could
  silently change which `VERGEN_*` keys are emitted, breaking the
  `option_env!` consumers. Mitigation: exact pin + the JSON
  round-trip tests in `api/data/version.rs::tests` and
  `build_info.rs::tests` fail at compile/test time when a field
  disappears.
- Future addition of a runtime `env::var` lookup to `build_info`
  silently re-introduces the hermetic-subprocess hazard. Mitigation:
  the `Version::execute` doc-comment and `subsystem-package-manager.md`
  "Hermetic subprocess pattern" section both call this out; a
  follow-up acceptance test (see Followups) would lock it in.

### How would we reverse this?

Reversible. Drop `vergen-gix` from `[build-dependencies]`, delete
`build.rs`, delete `app/build_info.rs`, revert
`api/data/version.rs::VersionData` to its pre-enrichment shape (just
`version: String`). The JSON wire contract survives the reversal by
construction — `version` is the only required key. Bounded surface:
four files in `crates/ocx_cli/`, plus a CHANGELOG line.

## Technical Details

### Build-script architecture

```
crates/ocx_cli/build.rs
  ├── vergen-gix:
  │     BuildBuilder(timestamp=in_ci) ──┐
  │     CargoBuilder(target, debug)   ──┤── Emitter ──► cargo:rustc-env=VERGEN_*
  │     RustcBuilder(semver)          ──┤
  │     GixBuilder(sha/describe/dirty/ts) ─┘   (failure → cargo:warning, build continues)
  │
  ├── pass_through_env("__OCX_BUILD_VERSION")  ┐
  ├── pass_through_env("__OCX_BUILD_CHANNEL")  ├─► cargo:rustc-env=…
  └── pass_through_env("GITHUB_*")             ┘   (newline-guarded)
```

`pass_through_env` re-emits `cargo:rerun-if-env-changed=<NAME>` so a
change to any source env triggers a relink, and guards against
`\n`/`\r` injection into the `cargo:rustc-env` instruction stream.

### Consumer wire shape

`ocx version --format json` (full schema, dev-deploy build):

```json
{
  "version":            "0.3.2-dev+20260528143045",
  "cargo_pkg_version":  "0.3.1",
  "channel":            "dev",
  "commit": {
    "sha":       "…",
    "short":     "…",
    "describe":  "…",
    "dirty":     false,
    "timestamp": "…"
  },
  "build": {
    "timestamp": "…",
    "profile":   "release",
    "target":    "…",
    "rustc":     "…"
  },
  "ci": {
    "provider": "github-actions",
    "run_url":  "…",
    "workflow": "…",
    "ref":      "…",
    "sha":      "…"
  }
}
```

Locally-built (no `.git/`, no `CI=1`, no `__OCX_BUILD_*`) payload
collapses to:

```json
{ "version": "0.3.1", "build": { … target + rustc only … } }
```

`cargo_pkg_version` is suppressed when identical to `version`. The
`version` key is the sole required field; `query_installed_version`
parses only it.

### Field provenance

| Field | Source env | Notes |
|---|---|---|
| `version` | `__OCX_BUILD_VERSION` ?? `CARGO_PKG_VERSION` | `app::version()` |
| `cargo_pkg_version` | `CARGO_PKG_VERSION` | Suppressed when == `version` |
| `channel` | `__OCX_BUILD_CHANNEL` | `dev` from dev-deploy CI |
| `commit.sha` | `VERGEN_GIT_SHA` | Long SHA; short is `[..8]` |
| `commit.describe` | `VERGEN_GIT_DESCRIBE` | `--tags --dirty` |
| `commit.dirty` | `VERGEN_GIT_DIRTY` | `"true"` → `true` |
| `commit.timestamp` | `VERGEN_GIT_COMMIT_TIMESTAMP` | ISO-8601 author date |
| `build.timestamp` | `VERGEN_BUILD_TIMESTAMP` | Gated on `CI` |
| `build.profile` | `VERGEN_CARGO_DEBUG` | `"true"` → `debug`, else `release` |
| `build.target` | `VERGEN_CARGO_TARGET_TRIPLE` | — |
| `build.rustc` | `VERGEN_RUSTC_SEMVER` | — |
| `ci.run_url` | `GITHUB_SERVER_URL` + `GITHUB_REPOSITORY` + `GITHUB_RUN_ID` | Composed at runtime from baked `&'static str`s |
| `ci.workflow` / `ref` / `sha` | `GITHUB_WORKFLOW` / `GITHUB_REF` / `GITHUB_SHA` | — |

### `__OCX_*` naming convention

User-facing env vars use a single-underscore `OCX_` prefix
(`OCX_HOME`, `OCX_OFFLINE`, `OCX_GLOBAL`, …) and are documented in
`website/src/docs/reference/environment.md`. Build-time
implementation-detail seams use the double-underscore prefix
(`__OCX_BUILD_VERSION`, `__OCX_BUILD_CHANNEL`) to signal:

- not part of the public CLI/env contract;
- not stable across releases;
- not to be referenced from user docs or third-party tooling.

The same convention extends to any future build-time-only override
(e.g. `__OCX_BUILD_PROVENANCE_SOURCE` in the deferred Followups list
below).

## Validation

- [x] `ocx version` prints bare `version` string by default (pre-enrichment contract preserved).
- [x] `ocx version --verbose` prints multi-line labelled-value summary; absent fields suppress their rows.
- [x] `ocx version --format json` always emits `version`; all other top-level keys are optional.
- [x] `cargo_pkg_version` suppressed when identical to `version`.
- [x] An empty provenance payload serialises to `{}` (no `null` rows).
- [x] `CiInfo` renames the `git_ref` Rust field to JSON `ref`.
- [x] `update_check.rs::query_installed_version` continues to parse the `version` key under `env_clear()` (existing test surface).
- [ ] Optional: tarball-build CI matrix leg that asserts the JSON payload contains only `version` + `build.{target,rustc}`. *(Followup.)*

## Links

- `crates/ocx_cli/build.rs` — vergen-gix wiring + `pass_through_env`
- `crates/ocx_cli/src/app/build_info.rs` — `Provenance`/`CommitInfo`/`BuildInfo`/`CiInfo` + `channel` field on `Provenance`
- `crates/ocx_cli/src/app/version.rs` — `version()` accessor (raw `CARGO_PKG_VERSION` inlined at the single `command/version.rs` call site)
- `crates/ocx_cli/src/api/data/version.rs` — `VersionData::enriched` + verbose plain-text renderer
- `crates/ocx_cli/src/command/version.rs` — `Version::execute` (hermetic-subprocess doc-comment)
- `crates/ocx_lib/src/package_manager/tasks/update_check.rs` — `query_installed_version` (the subprocess consumer)
- `.github/workflows/deploy-dev.yml` — sets `__OCX_BUILD_VERSION` + `__OCX_BUILD_CHANNEL=dev`
- [vergen-gix on crates.io](https://crates.io/crates/vergen-gix)
- [`gix` (gitoxide) — pure-Rust git](https://github.com/Byron/gitoxide)

## Followups

These are deliberately out of scope of this ADR; recorded so they are
not lost:

1. **Add this ADR to the `arch-principles.md` ADR Index.** Separate
   commit; not the scope of the build-provenance change.
2. **Tarball-build CI matrix leg.** Spin a job that builds from a
   `cargo package`-generated tarball (no `.git/`) and asserts the JSON
   payload degrades gracefully (only `version` + the unconditionally
   emitted `build.{target,rustc}` keys present). Locks in Decision 2.
3. **`provenance_source` field.** Optional `"ci" | "tarball" | "local"`
   discriminant to distinguish CI builds from tarball rebuilds without
   inferring it from field presence. Deferred design decision — wait
   for a concrete bug report that needs it before adding the field.
4. **Hermetic-subprocess regression test.** An acceptance test that
   invokes `ocx --format json version` under `env_clear()` and asserts
   the `version` key parses, locking in Decision 5 against future
   refactors that sneak a runtime `env::var` read into `build_info`.

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-05-28 | Architect (Opus) | Initial — post-hoc record of build-provenance enrichment merged on `sion` (commits `17c45451`, `27548fdd`, `de27008b`). |
