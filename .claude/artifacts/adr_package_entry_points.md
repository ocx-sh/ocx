# ADR: Package Entry Points — Generated Launchers Wrapping `ocx exec`

## Metadata

**Status:** Proposed
**Date:** 2026-04-21
**Deciders:** worker-architect (opus, Phase 4 of /swarm-plan high #61)
**GitHub Issue:** [max-oberwasserlechner/ocx#61](https://github.com/max-oberwasserlechner/ocx/issues/61)
**Related PRD:** [`prd_package_entry_points.md`](./prd_package_entry_points.md)
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md` (Rust 2024 / Tokio; no new language or framework introduced)
- [x] Deviation from Golden Path? None.
**Domain Tags:** package-manager, cli, file-structure, metadata, api
**Supersedes:** —
**Superseded By:** —
**Reversibility Classification:** **One-Way Door High.** The `ocx exec --install-dir=<path>` CLI contract baked into every generated launcher is locked across every future OCX version because of Tension 4's host-OCX coupling: old launchers on disk outlive every OCX upgrade and continue calling whatever `ocx` the user has on PATH. Changing the contract requires a major-version deprecation cycle *plus* maintaining the old shape indefinitely for installed launchers — a Medium door can be closed with a repair command and one minor version; this cannot. Schema and storage pieces remain Two-Way (additive-optional field, additive-sibling directory).

**Cross-model review recommendation:** Per `.claude/skills/swarm-plan/classify.md`, High-reversibility ADRs auto-trigger Codex cross-model plan review. The user disabled Codex for this `/swarm-plan` run; flagging here so the user can decide whether to enable it before landing. No action from this ADR author.

## Context

OCX installs binaries into a content-addressed store but does not expose them to users by name. Callers must type `ocx exec <identifier> -- <tool>` or manually add `${installPath}/bin` to PATH via the shell-profile machinery. Every comparable tool (Homebrew, Nix `makeWrapper`, npm `cmd-shim`, pipx `console_scripts`, asdf/mise shims, rustup proxies) ships launcher/shim binaries as the primary user surface. Without a comparable feature OCX cannot serve its stated primary audience — GitHub Actions, Bazel rules, devcontainer features — where invocation-by-name is assumed.

Issue #61 specifies:

- Additive `entrypoints` field on bundle metadata
- Launchers generated at install time, invoking `ocx exec` with the package path
- Host-OCX coupling (launcher calls whatever `ocx` is on PATH; no OCX version baked)
- Cross-platform: Unix `.sh` + Windows `.cmd` targets

Four deliberate tensions must be resolved before implementation:

1. `ocx exec` invocation contract (the one-way door)
2. PATH integration selection model
3. Windows shell targets + argument escaping
4. Correlation with #25 (portable OCX home)

Plus confirmation of #61's already-decided tension 4 (OCX version coupling) and a concrete answer to the `DependencyContext` blocker surfaced in Discover §3 Gap 1.

## Decision Drivers

- **Backend-first product principle** — launchers must be machine-invocable, not chatty; overhead must stay <5 ms on Linux
- **Clean-env execution principle** — launchers must preserve `ocx exec`'s env discipline; no ambient PATH leakage
- **Reversibility at the CLI boundary** — every bit of contract baked into launchers is locked forever once a publisher ships packages that depend on it
- **Minimal net-new surface** — issue #61 labels `area/package` + `area/package-manager` + `area/cli`, but each change is an **extension** to an existing pattern, not a new crate or major restructure
- **#25 coordination** — portable OCX home is open; launchers must not force #25 into a specific design, but also must not block #25 by requiring a launcher-level migration for every move
- **Windows reality** — OCX's target audience uses Git Bash + PowerShell + cmd.exe; all three must work; MOTW and MSYS2 path translation are known hazards
- **No new innovation tokens** — Boring Technology principle: generated shell scripts are the industry default across rustup, npm, Nix, pipx

## Industry Context & Research

**Research artifact:** [`research_launcher_patterns.md`](./research_launcher_patterns.md)

**Trending approaches:**

- Rustup + mise moving toward native binary stubs (compiled proxies) for startup speed. Irrelevant here — OCX launchers are one-shot `exec` wrappers with baked paths, not interceptors that re-resolve at runtime. Fork+exec of `/bin/sh` adds 1–3 ms, not 120 ms.
- npm cmd-shim emits three files (`.sh`, `.cmd`, `.ps1`). OCX follows two-file convention (`.sh`, `.cmd`) because PowerShell's `--`-parsing semantics are architectural (not a defect) and no clean `.ps1` launcher shape supports both baked-path interpolation and arbitrary user-arg forwarding — see Tension 3 and research §3.
- Homebrew ships symlinks (no wrapper); Nix `makeWrapper` ships shell wrappers. OCX needs the wrapper shape because the wrapper's job is to call `ocx exec`, not to exec the binary directly — that preserves the clean-env guarantee.

**Key insight:** Bake the absolute install path at generation time; resolve nothing at runtime. Every mature ecosystem does this. The `exec --install-dir` indirection through `ocx` provides the env composition; the launcher itself is dumb.

## Tension 1 — `ocx exec` Invocation Contract (THE one-way door)

### Considered Options

#### Option A — Positional `<path>` overload

**Description:** Launchers invoke `ocx exec /absolute/path/to/packages/.../content -- "$@"`. Requires a new branch in `exec.rs` that inspects `self.packages[0]` with an `is_absolute_content_path` prefix check *before* `Identifier::transform_all` runs, because today's `options::Identifier::from_str` (`crates/ocx_cli/src/options/identifier.rs:45-52`) calls `oci::Identifier::from_str(s)?` which errors on absolute paths — there is no path-mode fallback. The positional is effectively overloaded with a new disambiguation rule.

| Pros | Cons |
|------|------|
| Shell traces read naturally: `ocx exec /path -- cmd` | Implicit disambiguation — a prefix check (`/` on Unix, `<drive>:\` on Windows) decides path vs identifier |
| Positional is the most common `ocx exec` shape today | Non-greppable: launcher invocations are indistinguishable from "user typed a path by hand" in shell history and logs |
| No new flag name to commit to | Future `Identifier` extensions must continue to reject leading `/` and drive-letter prefixes — constrains the identifier grammar forever |
| — | Still adds CLI surface: new code branch, new help-text note, new precedence rule to document |

#### Option B — `--install-dir=<path>` flag (RECOMMENDED)

**Description:** Launchers invoke `ocx exec --install-dir=/absolute/path -- "$@"`. Explicit; no positional overload; mirrors existing precedent (e.g., `--index`, `--format`).

| Pros | Cons |
|------|------|
| Unambiguously reads as "use this path directly, skip resolution" | New CLI flag to design, document, test |
| Greppable in shell history and logs: `--install-dir` is a stable literal marking "this is a launcher call" | Introduces a branch in `Exec::execute` that reads the flag and skips `find_or_install_all` — comparable code surface to Option A's prefix check |
| Clear in error messages (`invalid --install-dir: {path}` is better than `invalid identifier: /path/...`) | Baked-in forever on every launcher; flag name churn would require a compat alias (but see Reversibility — no worse than Option A's positional contract) |
| No constraint on future identifier grammar — identifiers and path-mode are orthogonal | — |
| Escape-clause flexibility: adding a second flag (`--install-metadata=<path>`, `--install-content-digest=<digest>`) is a natural extension | — |

#### Option C — Hybrid: path-or-identifier positional, disambiguated by prefix detection

**Description:** Explicitly document "if the positional starts with `/` (Unix) or `<drive>:\` (Windows), it is a path; otherwise an identifier." Formalizes Option A's runtime rule in help text and tests.

| Pros | Cons |
|------|------|
| Same runtime shape as Option A with documented contract | Formalizes an ambiguity — encourages callers to rely on the disambiguation, which makes escaping harder later |
| Discoverable behavior | Doesn't materially improve on A; inherits A's non-greppability |

#### Option D — New sub-shape: `ocx exec-launcher <path> -- "$@"`

**Description:** A separate sub-command so the path-mode contract lives in isolation.

| Pros | Cons |
|------|------|
| Cleanest separation — `ocx exec` stays identifier-only | Adds a second near-identical command; violates KISS |
| Future-proof against `ocx exec` semantic drift | Launchers now reference a launcher-specific command; breaks the issue requirement "use host OCX" (host may be slightly old and lack `exec-launcher`) — compat window risk |

### Decision Outcome

**Chosen Option:** **Option B (`--install-dir=<path>` flag).**

**Rationale:** Round 1 recommended Option A on a tiebreaker that was factually wrong — the ADR claimed "`options::Identifier::transform_all` falls back to path-mode" and that Option A therefore added "zero new CLI surface." Verification against `crates/ocx_cli/src/options/identifier.rs:45-52` shows `Identifier::from_str` calls `oci::Identifier::from_str(s)?`, which errors on absolute paths. There is no fallback. Both Option A and Option B add new CLI surface — a branch in `exec.rs` that inspects the input and short-circuits identifier resolution.

**Re-weighing on real grounds:**

- **Log greppability** — B's `--install-dir=` literal marks every launcher invocation unambiguously; A's positional is indistinguishable from a user typing a path by hand. For a feature that will produce millions of cross-process invocations in CI logs, this matters.
- **Future-proofing the identifier grammar** — A locks the identifier grammar into "must never start with `/` or drive-letter." B leaves identifier syntax orthogonal to path-mode.
- **Escape-clause flexibility** — B composes: a future `--install-metadata=<path>` (e.g., to direct `ocx exec` at a metadata file outside the content dir) is a sibling flag. A's positional has no such extension point.
- **Implementation cost** — comparable. Both add one input-inspection branch and one code path that skips index resolution.
- **Ergonomics "tiebreaker"** — A reads slightly more naturally in a hand-typed shell command. But launchers are not hand-typed; they are machine-generated. This consideration should not drive the call.

The trade-off reviewer's argument that Option B is materially better stands once the false tiebreaker is removed. The dismissal rationale from Round 1 ("A adds zero new CLI surface") no longer holds; under honest accounting B is the stronger choice.

**Reversibility:** Locked across OCX versions per Tension 4 (host-OCX coupling). Installed launchers calling `--install-dir` will persist across every future OCX release; the flag must remain honored indefinitely. A future shape change (e.g., adding `--install-metadata`) is additive and Two-Way; replacing `--install-dir` with a different name is as One-Way as a positional would be under the same coupling.

## Tension 2 — PATH Integration Selection Model

> **Superseded 2026-04-26 (pre-release branch `feat/package-entry-points`).** The
> sibling `entrypoints-current` symlink was removed before any release shipped
> the design. The replacement: `current` (and `candidates/{tag}`) target the
> **package root** rather than `content/`, and consumers traverse
> `<current>/content`, `<current>/entrypoints`, or `<current>/metadata.json`
> from the same anchor. Shell PATH integration uses `<current>/entrypoints`.
> See "Update 2026-04-26" decision block at the bottom of this ADR for the
> new shape; the option-comparison below is preserved for historical context.

### Considered Options

#### Option A — Per-repo `entrypoints-current` symlink (RECOMMENDED — superseded)

**Description:** Add a new sibling to the existing `symlinks/{registry}/{repo}/current` path: `symlinks/{registry}/{repo}/entrypoints-current` targeting `packages/.../entrypoints/`. `ocx select` flips both `current` and `entrypoints-current` atomically. Shell profile gets one PATH entry per selected package: `$OCX_HOME/symlinks/{registry}/{repo}/entrypoints-current`.

| Pros | Cons |
|------|------|
| Mirrors exactly the existing `current` mental model | Adds one PATH entry per selected package; if 20 tools are installed, the shell's PATH grows by 20 |
| Atomic swap via existing `symlink::update` | — |
| No PATH-entry mutation when users `ocx select` a different version — the symlink flips, not the PATH | — |
| Works in CI without shell-profile reload (user exports `$OCX_HOME/symlinks/.../entrypoints-current` once) | Requires per-package `ocx shell profile add` or an `ocx ci export` convenience |

#### Option B — Per-install PATH entry via shell profile augmentation

**Description:** Each install appends `${installPath}/../entrypoints` (i.e., `$OCX_HOME/packages/.../entrypoints`) to PATH via the existing metadata `env` mechanism.

| Pros | Cons |
|------|------|
| No new storage model needed | Fragile relative traversal in metadata-declared env |
| Reuses today's shell-profile pipeline | Every version switch means a shell-profile edit — breaks the "select flips symlinks, shell doesn't need to reload" guarantee |
| — | Uninstall requires mutating shell profile to remove the entry |

#### Option C — Single global `$OCX_HOME/entrypoints/` directory

**Description:** Maintain `$OCX_HOME/entrypoints/` with symlinks from each selected package's entry points. `ocx select` rewrites symlinks in this dir. One PATH entry total.

| Pros | Cons |
|------|------|
| Single PATH entry for the user's whole OCX toolchain | Atomic swap is hard — need per-symlink updates under a lock; partial failure leaves inconsistent state |
| Matches Homebrew `/opt/homebrew/bin` mental model | Collision resolution must be explicit — when two selected packages declare entry point `ls`, which wins? |
| — | Breaks the per-repo `current` symlink isolation that OCX's GC model relies on |
| — | Requires new GC logic to keep the global dir in sync as packages are uninstalled |

#### Option D — No PATH integration v1

**Description:** Generate launchers only; users invoke via explicit path or add directories manually.

| Pros | Cons |
|------|------|
| Smallest v1 | Defeats the primary goal — "user can invoke `cmake`" is the whole point |
| Defers decision | Competitors do not require users to configure PATH manually; backend-first principle would be violated |

### Decision Outcome

**Chosen Option:** **Option A (per-repo `entrypoints-current` symlink).**

**Rationale:** Option A is the lowest-cost extension of the pattern OCX already runs on. The `current` symlink model is the product's version-switching mental model; `entrypoints-current` is a literal per-repo sibling of it, atomic by reusing `symlink::update`, and GC-safe by inheriting the package-root reachability contract. Option C's appeal — one PATH entry — collides hard with the atomic-swap requirement: `ocx select cmake:3.29` must not leave a half-updated global dir if it fails. Option B's fragility (shell-profile edits on every select) is disqualifying against the `current` semantics.

**Honest close call:** Option C's "one PATH entry" aesthetic is genuinely nice, and if OCX's primary audience were interactive users this would be a harder call. But the primary audience is CI — where the shell-profile flow is already fine with multiple PATH entries (CI runners set them programmatically), and the atomicity of `select` matters more than the aesthetic of PATH length.

**Reversibility:** Two-Way. Adding Option C later on top of Option A is additive — `entrypoints-current` stays as the canonical per-repo anchor; a future aggregator at `$OCX_HOME/entrypoints/` would symlink into the per-repo anchors. Removing Option A later is hard only if shell profiles are already wired to per-repo entries.

## Tension 3 — Windows Shell Targets + Argument Escaping

### Considered Options

#### Option A — `.cmd` only (v1), `.ps1` deferred (RECOMMENDED)

**Description:** Generate Unix `.sh` + Windows `.cmd`. PowerShell users invoke the `.cmd` directly (cmd.exe is the transparent shim). Documented in research §3: PowerShell's `--` token is a parameter-binding boundary, not a Unix-style end-of-options marker, and its `--%` stop-parsing operator disables variable expansion — so a `.ps1` launcher that needs to interpolate the baked install path and forward user-supplied `--flag` arguments has no clean construction.

| Pros | Cons |
|------|------|
| Matches research recommendation | PowerShell users must know to call `.cmd` (ergonomic tax) |
| `.cmd` callable from cmd.exe, PowerShell, Git Bash (via `cmd.exe /C`) | — |
| Smallest test burden | — |

#### Option B — `.cmd` + `.ps1`

**Description:** Ship both; work around PowerShell's `--` semantics.

| Pros | Cons |
|------|------|
| PowerShell parity | PowerShell's `--` + `--%` semantics are architectural; there is no combination that preserves both path interpolation AND arbitrary user-arg forwarding |
| | Extra test surface; duplicate maintenance of two templates |
| | Any v1 shape would either silently drop `--`-prefixed user args, or refuse to interpolate the baked path at generation time |

#### Option C — `.cmd` + `.ps1` + `.sh` bridge for MSYS/Git Bash

**Description:** Maximum compat including a dedicated MSYS bridge.

| Pros | Cons |
|------|------|
| No ambiguity about which launcher fires in which shell | Triples Windows test surface |
| | MSYS2 path translation is already automatic for `.cmd` via cmd.exe interop |

### Decision Outcome

**Chosen Option:** **Option A (`.cmd` only for Windows v1).**

**Rationale:** PowerShell's argument-parsing rules are architectural, not a fixable defect. The `--` token in PowerShell is a parameter-binding boundary (used by the parser to separate cmdlet parameters from positional values), not a Unix-style end-of-options marker. The `--%` stop-parsing operator is available, but prevents variable expansion — which defeats a launcher that needs to interpolate the baked install path. A `.ps1` launcher that wants both "forward arbitrary user args, including `--flag`" and "interpolate the baked path" has no clean construction under PowerShell's by-design rules. `.cmd` is callable from every Windows shell; it is the safe common denominator. Git Bash users (a real and documented segment of the target audience) invoke `.cmd` through MSYS2's binfmt bridge; path translation converts the baked Windows path seamlessly (research §3).

**Close-call honesty:** Option B was not given equal weight in the final recommendation because PowerShell's `--` semantics are not a defect we can wait to be fixed — they are the documented PowerShell parser contract. Adding `.ps1` later would require a dedicated design pass to pick a concrete trade-off (e.g., accept silently-dropped `--`-prefixed user args, or use `--%` and give up path interpolation), not a "flip the switch when upstream fixes the bug" migration.

**Reversibility:** Adding `.ps1` later is additive at the file-generation level (launchers are regenerated on every `ocx install`), but the design decision itself is not a free "Two-Way" door — it requires re-opening the `.cmd` vs `.ps1` trade-off and committing to one of the constrained `.ps1` shapes. Document that dependency explicitly rather than implying `.ps1` can be added at will.

## Tension 4 — OCX Version Coupling (RECONFIRM)

The issue body decided: use host OCX. Launchers contain no embedded `ocx` version, no embedded `ocx` binary path. This ADR reconfirms.

**Implication:** The launcher invocation contract becomes **load-bearing across OCX versions.** If `ocx` v2 changes `exec`'s argument shape, every existing launcher breaks silently. Mitigations:

- Acceptance test asserting the launcher invocation works as documented — becomes a compat contract
- Semver discipline: the contract shape cannot change without a major-version deprecation cycle, AND any prior shape must continue to be honored for already-installed launchers
- Documented in the CLI reference as a stable contract

> **Updated 2026-04-26 (pre-release).** The launcher contract is now
> `ocx exec 'file://<absolute-package-root>' -- <name> <args>` (not
> `ocx exec --install-dir=<absolute-content-path> -- <name> <args>`). The
> `--install-dir` flag was deleted before any release shipped a launcher with
> the old shape. The `file://` URI scheme + positional shape replace the flag
> and become the new load-bearing contract. See "Update 2026-04-26" decision
> block at the bottom of this ADR.

**Rationale:** Any alternative (bake `ocx` version → `ocx-0.42 exec`, or bake the `ocx` absolute path) forces launcher regeneration on OCX upgrade. Regenerating means `ocx install --repair` which complicates the workflow. The host-OCX approach is what every competitor does (mise, rustup, asdf all call the host tool).

## Tension 5 — Correlation with #25 (Portable OCX Home)

### Considered Options

#### Option A — Bake absolute paths now; regen on `$OCX_HOME` move (RECOMMENDED)

**Description:** Launchers contain the absolute package content path. If `$OCX_HOME` moves, #25's mover includes a launcher-regeneration step.

| Pros | Cons |
|------|------|
| Zero startup cost — launcher is `exec ocx exec <literal> -- "$@"` | Couples #25 to this feature — #25 must call a launcher-regen hook |
| Matches research recommendation + industry practice | — |
| Works offline after install (no resolution path at invocation time) | — |

#### Option B — Relative paths from a known anchor

**Description:** Use `$0` / `%~dp0` + relative traversal. Launcher resolves its own path at runtime.

| Pros | Cons |
|------|------|
| #25-friendly: moving `$OCX_HOME` doesn't break launchers | Fragile under symlinks (`pwd -P` vs `pwd`; Windows junctions) |
| No regen step needed | Launcher becomes non-trivial — self-location idioms from research §2 must ship in every launcher |
| — | Breaks when the launcher is called through an external symlink (a user's own `/usr/local/bin/cmake` → OCX launcher) |

#### Option C — Indirect via identifier only

**Description:** Launcher contains `ocx exec <registry>/<name>@<digest> -- "$@"`. OCX resolves path at each invocation.

| Pros | Cons |
|------|------|
| `$OCX_HOME`-relocation immune | Re-introduces resolve cost per invocation (+index lookup + disk walk) |
| — | Fails offline if local index is absent or corrupted; today's `ocx exec <identifier>` reaches the resolver |
| — | Breaks the "launcher works with no network" guarantee that absolute paths give |

### Decision Outcome

**Chosen Option:** **Option A (bake absolute paths; regenerate on move).**

**Rationale:** Startup cost matters (backend-first principle; build scripts invoke these in loops). Offline robustness matters (offline-first principle). Option B's self-location is a research-documented footgun; Option C's per-invocation resolve cost undermines both principles. The #25 coupling cost (mover must regenerate launchers) is a one-time engineering cost on #25's side, not a per-invocation cost on every user.

**Close-call honesty:** Option B has a genuine appeal for #25 alignment — if OCX were an interactive user-facing tool, relative paths might win. For a backend tool where startup is in a hot path, Option A wins cleanly. The review panel should confirm that the #25 coupling cost was correctly estimated (one-time + additive, not invasive).

**Reversibility:** Two-Way. Moving from A to B later is a launcher-regen on every install; moving from A to C is just a schema of the launcher body. Both are one-time migrations.

## Decision Summary

| Tension | Chosen | Rejected alternatives | Close call? |
|---------|--------|-----------------------|-------------|
| 1. `ocx exec` contract | B: `--install-dir=<path>` flag | A (positional), C (hybrid), D (new command) | **Yes** — A vs B re-weighed Round 2 after false "zero-surface" tiebreaker was corrected |
| 2. PATH integration | A: per-repo `entrypoints-current` | B (per-install), C (global), D (none) | Yes — A vs C on aesthetic vs atomicity |
| 3. Windows targets | A: `.cmd` only | B (`.cmd`+`.ps1`), C (three-target) | No — blocked by PS `--` bug |
| 4. OCX version coupling | Reconfirm host-OCX | Bake version / bake `ocx` path | No — per issue decision |
| 5. #25 correlation | A: absolute paths + regen on move | B (relative), C (identifier indirection) | Yes — A vs B on #25 alignment vs startup cost |

## Component Contracts

### 1. `EntryPoint` metadata type

**Location:** `crates/ocx_lib/src/package/metadata/entry_point.rs` (new file, mirrors `dependency.rs` structure).

**Inputs:** JSON object `{ "name": "<slug>", "target": "<template-string>" }`.

**Outputs:** Validated `EntryPoint { name: EntryPointName, target: String }`.

**Invariants:**

- `name` matches `^[a-z0-9][a-z0-9_-]*$` (slug regex reused from `dependency.rs:12-13`). Enforced at construction and deserialization.
- `target` is a non-empty string; template token validation happens at `ValidMetadata::try_from` level (below), not on `EntryPoint` itself — following the same split as dep-token validation.
- No `visibility` field v1 — entry points are implicitly public (if declared, they are meant to be invoked).

**Error cases:**

- `EntryPointError::InvalidName { name: String }` — slug regex failed. Block-tier at deserialization.

**Testing strategy:**

- Round-trip tests mirroring `dependency.rs::alias_invalid_empty_rejected` and `alias_invalid_uppercase_rejected`
- Schema test: regenerate JSON Schema; assert `entrypoints` is an additive-optional property on Bundle

### 2. `EntryPoints` wrapper

**Location:** Same file as `EntryPoint`.

**Inputs:** `Vec<EntryPoint>` from deserialization.

**Outputs:** Validated `EntryPoints { entries: Vec<EntryPoint> }`.

**Invariants:**

- Uniqueness of `name` across all entries — matches `Dependencies::new()` pattern (`dependency.rs:132-154`).
- Empty vec is allowed; `is_empty()` returns true and field is skipped at serialization via `#[serde(skip_serializing_if)]`.

**Error cases:**

- `EntryPointError::DuplicateName { name: String }` — same as `Dependencies::DuplicateAlias`.

**Testing strategy:**

- Serde round-trip + preserve-order test (array position is stable — not used for resolution but documented)
- `duplicate_name_rejected` test mirroring `duplicate_alias_rejected`

### 3. `Metadata::Bundle.entrypoints`

**Location:** `crates/ocx_lib/src/package/metadata/bundle.rs:36`.

**Schema change:**

```rust
pub struct Bundle {
    pub version: Version,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub strip_components: Option<u8>,
    #[serde(skip_serializing_if = "env::Env::is_empty", default)]
    pub env: env::Env,
    #[serde(skip_serializing_if = "Dependencies::is_empty", default)]
    pub dependencies: Dependencies,
    // NEW:
    #[serde(skip_serializing_if = "EntryPoints::is_empty", default)]
    pub entrypoints: EntryPoints,
}
```

**Forward-compat stance:** Additive-optional. Older `ocx` encountering a newer package with `entrypoints` set **parses successfully, ignores the field**, and produces a package install without launchers. This matches existing precedent (`dependencies` was added the same way). Documented in ADR consequences.

### 4. Publish-time template validation

**Location:** `crates/ocx_lib/src/package/metadata.rs:47-143` (`ValidMetadata::try_from`).

**Extend the existing `${deps.NAME.installPath}` validator:**

- For each `entry_point.target`, run the same `DEP_TOKEN` regex check as env values
- Additionally verify the target resolves to a path under `${installPath}` or a declared dep's `${installPath}` — **no absolute-path escape allowed**
- Error: `Error::EntryPointTargetInvalid { name: String, source: TemplateError }`

**Invariants:**

- Same template resolver as env (reuse `Exporter`-style resolution for the structured target string)
- Targets that interpolate to paths outside `${installPath}`/`${deps.*.installPath}` are rejected at publish time; this is the publisher's contract that every launcher points into an OCX-managed tree

### 5. `PackageDir::entrypoints()` accessor + `PackageStore` methods

**Location:** `crates/ocx_lib/src/file_structure/package_store.rs`.

**Additions:**

```rust
impl PackageDir {
    pub fn entrypoints(&self) -> PathBuf { self.dir.join("entrypoints") }
}

impl PackageStore {
    pub fn entrypoints(&self, identifier: &oci::PinnedIdentifier) -> PathBuf {
        self.path(identifier).join("entrypoints")
    }
    pub fn entrypoints_for_content(&self, content_path: &Path) -> Result<PathBuf> {
        Ok(package_dir_for_content(content_path)?.join("entrypoints"))
    }
}
```

**Invariants:**

- `entrypoints/` is a sibling of `content/` and `refs/` inside the package root
- GC reachability flows from the package root; `entrypoints/` is automatically collected when the package is
- No layer-hardlink relationship — launcher files are regular files generated at install time, not content-addressed

**Testing:**

- Unit tests mirroring `package_dir_accessors` + `*_for_content` tests (already 7 such tests; add entrypoints variant)

### 6. Launcher template generator

**Location:** `crates/ocx_lib/src/package_manager/entrypoints.rs` (new file, module-private helpers; no `impl PackageManager` additions).

**Inputs:**

- `content_path: &Path` — absolute path to package `content/` (baked into launcher)
- `entrypoints: &EntryPoints` — validated list
- `dep_contexts: HashMap<String, DependencyContext>` — for `${deps.NAME.installPath}` resolution in `target`
- `dest: &Path` — the package's `entrypoints/` temp dir

**Outputs:** Side effect — writes `<dest>/<name>` (Unix) and `<dest>/<name>.cmd` (Windows) files. `chmod 755` on Unix files.

**Unix launcher body (≤10 lines):**

```sh
#!/bin/sh
# Generated by ocx at install time. Do not edit.
_install_dir='<BAKED-ABSOLUTE-CONTENT-PATH>'
exec ocx exec --install-dir="$_install_dir" -- "$@"
```

Single-quoted literal — no shell expansion of the baked path.

**Windows launcher body (≤15 lines):**

```bat
@ECHO off
SETLOCAL
SET _install_dir=<BAKED-ABSOLUTE-CONTENT-PATH>
ocx exec --install-dir="%_install_dir%" -- %*
```

No `find_dp0` subroutine needed because the path is baked, not self-located.

**Invariants:**

- Both templates always produced on both platforms. Reason: cross-platform installs (Windows targets installing packages that will be mounted into a Linux CI container, or vice-versa) are real. Launcher file count per entry point is 2; OS determines which is invoked.
- The `target` field in metadata is **not** the launcher file; it is the binary the launcher's `ocx exec` resolves via env. The launcher always calls `ocx exec --install-dir=<content-path> -- "$@"` — the `target` shows up in metadata only to assert publish-time that the binary the name refers to actually exists.

**Error cases:**

- `io::Error` on write — bubbled up through `PackageErrorKind::Internal`.
- Template resolution of `target` fails — should have been caught at publish-time validation, but defense-in-depth: error at install time with `PackageErrorKind::Internal(Error::EntryPointTargetInvalid { ... })`.

**Testing:**

- Unit test: generator produces expected file contents with known install path
- Acceptance test: `ocx install <pkg>` → `$OCX_HOME/packages/.../entrypoints/cmake` exists and is executable
- Acceptance test: running the launcher produces the same stdout as `ocx exec <pkg> -- cmake`

### 7. Install-pipeline hook

**Location:** `crates/ocx_lib/src/package_manager/tasks/pull.rs` — between `assemble_from_layers` (line 344) and `post_download_actions` (line 355).

**Pseudocode:**

```rust
// After assemble_from_layers completes successfully:
if let Some(entrypoints) = metadata.bundle_entrypoints() {
    let dest = pkg.entrypoints();
    // Build dep_contexts same way resolve_env does (resolve.rs:220-260)
    let dep_contexts = build_dep_contexts(&metadata, &dependencies, fs);
    entrypoints::generate(&pkg.content(), entrypoints, &dep_contexts, &dest)
        .await
        .map_err(PackageErrorKind::Internal)?;
}
```

**Invariants:**

- Generation runs **before** `post_download_actions` so launchers and `resolve.json`/`install.json` land in the same temp dir and are carried by the same atomic `temp → packages/` move at `pull.rs:376-377`.
- No GC changes — `entrypoints/` is sibling of `content/`; GC already treats the package root as the reachability unit.
- Crash between launcher-write and atomic-move leaves the temp dir — cleanup is handled by existing temp-dir stale-entry logic (`temp_store.rs`).

### 8. `PackageManager::resolve_env` — path-mode variant

**The DependencyContext blocker from Discover Gap 1.**

When `ocx exec --install-dir=<absolute-path>` is invoked, we have a path but not a `PinnedIdentifier`. The launcher's invocation of `ocx exec --install-dir=<content-path> -- "$@"` must resolve env without triggering index resolution.

**Decision:** Add a new helper in `PackageManager`, NOT a variant of `InstallInfo`. Keep `InstallInfo` unchanged (it retains the `resolved: ResolvedChain` field).

**Contract (new method):**

```rust
impl PackageManager {
    /// Resolve env entries for a package known only by its on-disk content path.
    ///
    /// Reads metadata.json + resolve.json next to `content_path`; reconstructs
    /// `InstallInfo`-shaped input for `resolve_env`; returns the same
    /// `Vec<Entry>` shape that `resolve_env(&[InstallInfo])` produces.
    pub async fn resolve_env_from_content_path(
        &self,
        content_path: &Path,
    ) -> Result<Vec<package::metadata::env::exporter::Entry>, Error>;
}
```

**Implementation notes:**

- Uses `PackageStore::metadata_for_content` + `PackageStore::resolve_for_content` (both already exist, `package_store.rs:165-207`).
- `ResolvedChain.chain` (blob chain) is empty/absent — not needed for env resolution. The `resolved.dependencies` field is read from `resolve.json` and carries the platform-manifest digests. This is the field `resolve_env` uses at `resolve.rs:223-234`.
- New `InstallInfo` is constructed from disk data; `identifier` comes from the digest file + metadata (reconstruct `PinnedIdentifier` from sibling `digest` file).

**CLI surface change in `ocx exec`:**

Add a new flag to `Exec`'s clap struct in `crates/ocx_cli/src/command/exec.rs`:

```rust
#[derive(clap::Args, Debug)]
pub struct Exec {
    /// Absolute path to an installed package's content/ directory.
    /// Mutually exclusive with positional <packages...>; skips identifier
    /// resolution and index lookup. Used by generated launchers.
    #[arg(long, value_name = "PATH", conflicts_with = "packages")]
    pub install_dir: Option<PathBuf>,

    // existing fields ...
    pub packages: Vec<options::Identifier>,
    // ...
}
```

Execution branch:

```rust
// crates/ocx_cli/src/command/exec.rs — new branch at top of execute()
if let Some(content) = &self.install_dir {
    let entries = manager.resolve_env_from_content_path(content).await?;
    // ... build env, exec (same as today from resolve.rs:45 onwards)
    return Ok(...);
}
// existing identifier-mode path continues below
```

**Invariants:**

- `--install-dir` requires an absolute path; relative paths are rejected at clap validation or first use with a clear `invalid --install-dir: path must be absolute` error.
- `--install-dir` is mutually exclusive with positional packages (clap `conflicts_with`).
- The flag name is load-bearing across OCX versions (see Tension 4 reconfirmation and §Reversibility).

**Testing:**

- Unit test on `PackageManager`: given a fixture package dir with metadata + resolve.json + content, `resolve_env_from_content_path` returns the same entries as `resolve_env(&[info])` with the reconstructed `InstallInfo`
- Acceptance test: `ocx exec --install-dir=/path/to/content -- echo $PATH` produces the same `PATH` as `ocx exec <identifier> -- echo $PATH` for the same install
- Acceptance test: `ocx exec --install-dir=<path> <identifier>` (with both) is rejected by clap with a `conflicts_with` error
- Acceptance test: `ocx exec --install-dir=relative/path -- cmd` is rejected with a clear "must be absolute" error

### 9. PATH integration — `entrypoints-current` symlink + shell-profile

**Location:**

- `crates/ocx_lib/src/file_structure/symlink_store.rs` — new `entrypoints_current(&self, id) → PathBuf` accessor returning `{root}/symlinks/{registry}/{repo}/entrypoints-current`.
- `crates/ocx_lib/src/package_manager/tasks/install.rs` — on `select: true`, after `rm.link(&current_path, &info.content)`, also `rm.link(&entrypoints_current_path, &info.entrypoints())`.
- Shell-profile machinery (`crates/ocx_lib/src/profile/manager.rs`) — when a profiled package has `entrypoints.is_empty() == false`, emit a PATH entry pointing at `entrypoints-current` in addition to (or instead of — to be decided by the builder phase) the existing `${installPath}/bin` PATH entry driven by metadata `env`.

**Invariants:**

- `entrypoints-current` is a sibling of `current`, updated atomically alongside it via the same `ReferenceManager::link`.
- `ocx deselect` unlinks both `current` and `entrypoints-current`.
- Uninstall (`tasks/uninstall.rs`) removes back-refs via `refs/symlinks/`; existing pattern handles both without additional code.

**Testing:**

- Acceptance test: after `ocx install --select`, `$OCX_HOME/symlinks/.../entrypoints-current/cmake` resolves to the expected launcher
- Acceptance test: `ocx select` with a different version flips the symlink atomically

## Additional Considerations

These trade-offs surfaced during Round 2 review. Each is narrow enough not to warrant a full Tension section, but explicit enough to deserve a decision on record.

### Relative-symlinks interaction with #23

Issue #23 standardized relative-path targets for OCX-owned symlinks (`current` → `../../packages/.../content` instead of `/absolute/path`) to keep `$OCX_HOME` portable. The new `entrypoints-current` symlink is a literal per-repo sibling of `current` and MUST follow the same relative-path stance — it is written with the same `symlink::update` / `ReferenceManager::link` code path that `current` uses today, so relative targeting falls out naturally. Decision: `entrypoints-current` follows #23. Any divergence would silently break #25 (portable OCX home) in exactly the way #23 was designed to prevent. No new storage-layer code needed; the launcher files themselves are regular files with baked absolute string paths (covered by Tension 5).

### Bare `ocx` PATH lookup cost at launcher runtime

Launchers invoke bare `ocx` (not `/absolute/path/to/ocx`), so each call incurs a fork+exec of `/bin/sh`, a PATH search for `ocx`, and an exec of the resolved binary. Trade-off vs baking the absolute `ocx` binary path at generation time: bare-PATH is ~1 PATH lookup (<1 ms on local disk; higher on Windows UNC paths), whereas baked-absolute would break silently if the host OCX binary moves (e.g., `apt upgrade`, `brew upgrade`, `ocx self-update`). Bare-PATH is the correct choice because it IS Tension 4's host-OCX decision: launchers follow whatever `ocx` is on the user's PATH, by design. Document the trade-off explicitly so later "launcher startup is slow" bug reports reach the right trade-off, not a mistaken "cache the absolute path" fix.

### Collision policy: select-time, not install-time

The Collision Policy section below describes "reject at install time." More precisely: launcher *files* are generated under each installing package's `entrypoints/` directory unconditionally at install — there is no collision check at that step because files live in per-package dirs. The collision fires when `entrypoints-current` is about to be wired at `ocx install --select` (or a later `ocx select`) and the chosen package's entry-point names overlap with another *currently-selected* package's entry points. A package installed but not selected cannot collide. Install without `--select` never fires the collision check. Correction: the check is select-time, not install-time; generation is unconditional. Downstream: the error variant is still raised from `tasks/install.rs` (when `--select` is on) and from the select command path, using the same `PackageErrorKind::EntryPointNameCollision` variant.

### `$OCX_HOME` on network drives / SMB / UNC paths

Baked absolute paths work on network-mounted `$OCX_HOME` locations (SMB shares, Windows UNC paths like `\\server\share\...`). Correctness is unaffected. Performance is not: each launcher invocation is a fork+exec of the shell + an exec of `ocx`, and every stat/open under `packages/.../content/` crosses the network. The <5 ms launcher-overhead target (Validation section) may be exceeded on UNC paths; the `ocx exec --install-dir` path-mode branch still reads `metadata.json` + `resolve.json` from disk. Known limitation, not a blocker. Recommendation: include a smoke test or CI job that measures launcher invocation time when `$OCX_HOME` is on a network mount, so regressions surface early. Not part of v1 acceptance criteria.

## Launcher Name Collision Policy

**Decision:** **Reject at select time.** Generation of launcher *files* under each package's `entrypoints/` is unconditional (every install with `entrypoints` declared produces launcher files in the package dir). Collision is checked when `entrypoints-current` is about to be wired — i.e., at `ocx install --select` (the combined install+select path) or at `ocx select` on an already-installed package. Install without `--select` never fires the check. When a selected package's entry-point `name` collides with another *currently-selected* package's entry-point in a different repo, the select step fails with `PackageErrorKind::EntryPointNameCollision { name, existing_package: oci::Identifier }`. See Additional Considerations §3 for the precise trigger.

**Alternatives considered:**

- **First-wins (keep the existing launcher):** silently hides the newly-installed entry point; violates "surprise-free install" principle.
- **Last-wins (overwrite):** silently breaks the previously-installed package's launcher; worse than first-wins.
- **Namespace by package name (`<package>-<entry>`):** defeats the product purpose of short names; users want `cmake`, not `ocx.sh-cmake-cmake`.

**Rejection is loud, unambiguous, and actionable:** error message names both packages and suggests `ocx deselect <other-package>` or asking the publisher to rename. The publisher-side discipline question (how do two publishers coordinate?) is a product question, not a technical one.

**Transitive dep isolation (SOTA-6):** Transitive dep entrypoints are NOT exposed via the parent package's PATH. Only direct dependencies reachable from the consumer's explicit root are visible. This matches the dep-name interpolation scoping in `adr_deps_name_interpolation.md` — `${deps.NAME.installPath}` is only reachable for direct deps declared in the consumer's `metadata.json`; sealed/private transitive deps do not propagate their launchers to grandparent consumers.

**`target` template path-separator assumption (SOTA-7):** The `target` field in `metadata.json` uses forward-slash-only for path components relative to `${installPath}`. Backslash characters in the template string are treated as literals, not path separators. This assumption holds because packages are published from Linux/macOS hosts (where forward slash is the path separator); the template string travels through the OCI registry unchanged and is interpreted at install time on the consuming host (where OCX translates `${installPath}` to the platform-appropriate absolute path). A publisher who hand-authors `target` with backslash separators on a Windows host would produce a template that does not validate correctly under `check_target_contained` on a Linux publish host.

## GC Impact

**Non-Issue.** Reconfirmed from Discover §6:

- `entrypoints/` is a sibling of `content/` and `refs/` under the package root.
- GC (`reachability_graph.rs`) walks from package roots; anything inside a package root is collected when the package is.
- Launcher files contain absolute string paths as text — they are **not** symlinks. They do not create edges in the reachability graph. A launcher file whose baked path points into a layer does not protect that layer — the `refs/layers/` forward-ref is what protects the layer, and that already exists.
- No change to `reachability_graph.rs`, `clean.rs`, or `reference_manager.rs`.

## Schema Evolution Stance

| Scenario | Behavior |
|----------|----------|
| Old `ocx` reads new metadata with `entrypoints` | `#[serde(default)]` + `skip_serializing_if`: field parses to empty, package installs without launchers |
| New `ocx` reads old metadata without `entrypoints` | Same path — field defaults to empty; install proceeds without launchers |
| Publisher renames an entry point | Breaking change for callers. Not detected by OCX; publisher discipline. Documented in user-guide |
| Publisher deletes an entry point | Same — breaking for callers; not enforced |

**Stance:** additive-optional only. No removal, no rename, no compat shim v1. Matches `project_breaking_compat_next_version.md` memory (breaking compat acceptable in next version) but this feature does not introduce breaking compat — only additive growth.

## UX Scenarios

### Happy path — Linux interactive

```
$ ocx install cmake:3.28 --select
 resolving cmake:3.28 ...
 installing ...
 selected as current
$ eval "$(ocx --offline shell profile load)"   # one-time shell wiring
$ cmake --version
cmake version 3.28.1
```

### Happy path — GitHub Actions

```yaml
- run: ocx install cmake:3.28 --select
- run: ocx ci export cmake:3.28 >> "$GITHUB_ENV"     # adds entrypoints-current to PATH
- run: cmake --version
```

### Error — Name collision

```
$ ocx install myorg/cmake-fork:1.0 --select
error: entry point 'cmake' conflicts with installed package ocx.sh/cmake:3.28
hint: ocx deselect ocx.sh/cmake:3.28 first, or ask the publisher to rename the entry point
exit 65 (DataError)
```

### Error — Publisher declares invalid target

```
$ ocx package create ./my-pkg
error: entry point 'cmake' target '/etc/passwd' must resolve under ${installPath} or ${deps.*.installPath}
exit 65 (DataError)
```

### Error — Launcher called but `ocx` not on PATH

```
$ cmake --version
/home/me/.ocx/.../entrypoints/cmake: line 3: ocx: command not found
exit 127
```

### Error — MSYS2/Git Bash with stale Windows install path

```
$ cmake --version
ocx exec: --install-dir '/home/me/.ocx/packages/...' does not resolve to an installed package
exit 79 (NotFound)
```

### Error — Windows `.cmd` with empty argument

```
C:\> cmake "" --version
cmake version 3.28.1            # empty arg silently dropped by cmd.exe %*
```

Documented limitation; matches Windows cmd.exe behavior everywhere.

## Migration Plan

**For existing publishers:** No action. `entrypoints` defaults to empty; existing packages continue to install without launchers. Publishers opt in by adding the field in the next release.

**For existing consumers:** No action. `ocx exec <identifier> -- <cmd>` continues to work unchanged. Users who want launchers run the same `ocx install --select` they run today and get launchers as a side effect.

**For existing installs when upgrading `ocx`:** Installed packages do not retroactively gain launchers. The next `ocx install` generates launchers for newly-installed packages that declare `entrypoints`. For users who want launchers for already-installed packages: `ocx install <pkg> --select` reinstalls and generates launchers.

No `ocx migrate` or `ocx repair` command v1. If post-launch feedback shows demand, add `ocx install --repair` that regenerates launchers for an already-installed package without re-downloading.

## Implementation Plan (high level; detailed plan separate)

1. [ ] Add `EntryPoint` + `EntryPoints` + `EntryPointError` types in `crates/ocx_lib/src/package/metadata/entry_point.rs`
2. [ ] Extend `Bundle` struct + `Metadata` accessor + JSON Schema
3. [ ] Extend `ValidMetadata::try_from` with target-template validation
4. [ ] Add `PackageDir::entrypoints()` + `PackageStore::entrypoints()` + `PackageStore::entrypoints_for_content()`
5. [ ] Add launcher generator in `package_manager/entrypoints.rs`
6. [ ] Wire install-pipeline hook in `tasks/pull.rs` (between line 344 and 355)
7. [ ] Add `PackageManager::resolve_env_from_content_path` and add `--install-dir` flag + path-mode branch in `command/exec.rs`
8. [ ] Add `SymlinkStore::entrypoints_current` + wire `ocx select` / `ocx deselect`
9. [ ] Extend shell-profile PATH emission to include `entrypoints-current`
10. [ ] Add launcher name-collision check in `tasks/install.rs`
11. [ ] Acceptance tests (Unix + Windows + MSYS2 smoke)
12. [ ] Update `website/src/docs/reference/metadata.md` + `command-line.md`
13. [ ] Update `website/src/docs/user-guide.md` three-store section with `entrypoints/` sibling

## Validation

- [ ] Performance: `time <launcher> --version` on Linux <5 ms (launcher script) + `ocx exec` startup
- [ ] Security review: launchers contain no secret interpolation; baked paths are content-addressed + publish-time-validated; no shell injection via unquoted vars (acceptance test with path containing spaces and `"`)
- [ ] Schema check: `task schema:generate` produces valid JSON Schema with `entrypoints` as additive-optional property
- [ ] Backward compat: acceptance test installing an existing package without `entrypoints` succeeds and produces no launchers
- [ ] Cross-platform parity: acceptance tests run on Linux + macOS + Windows runners (existing matrix) and on MSYS2 Git Bash

## Consequences

**Positive:**

- OCX becomes usable as a primary tool-distribution mechanism for its stated primary audience (CI automation)
- Casual invocation (`cmake --version`) joins the `ocx exec` surface with no clean-env compromise
- Existing `current` symlink mental model extends naturally to `entrypoints-current`
- Every change is additive; no breaking compat; no feature flags
- Explicit `--install-dir` flag is greppable in CI logs and orthogonal to identifier grammar — a discoverable stable contract

**Negative:**

- The `ocx exec --install-dir=<absolute-path>` contract becomes load-bearing across OCX versions — cannot be changed without a major-version deprecation cycle AND indefinite honoring of the old flag for already-installed launchers. This is the One-Way Door High classification called out in Metadata.
- Shell-profile PATH grows by one entry per selected package (mitigated by CI use being the primary target, where PATH length is non-issue)
- Windows PowerShell users must invoke `.cmd` launchers, not `.ps1` — a documented ergonomic tax. `.ps1` launchers are deferred because PowerShell's `--` parsing is architectural (not a defect to be fixed) and PowerShell's `--%` stop-parsing operator prevents the variable expansion that launchers need. Adding `.ps1` later would require a dedicated design pass, not a "flip the switch when upstream fixes a bug" migration.
- **Cross-model review gap:** High-reversibility ADRs auto-trigger Codex cross-model plan review per `.claude/skills/swarm-plan/classify.md`. The user disabled Codex for this `/swarm-plan` run; the recommendation is surfaced here and in the plan handoff for the user's decision before landing.

**Risks:**

- **#25 (portable OCX home) landing after this feature** — #25's implementation must include a launcher-regen step. Document in the #25 planning artifact before this feature merges.
- **Name collision friction** — if publishers routinely pick the same entry-point name (`ls`, `go`, `test`), collisions will be frequent and install-time rejection will frustrate users. Mitigation: document the publisher naming contract prominently; consider a namespace escape hatch in a v2 ADR if collision rate exceeds a threshold.
- **Acceptance-test coverage on Windows** — MSYS2 Git Bash path translation is real and hard to test in CI. Smoke test via GitHub Actions Windows runner with Git Bash setup.

## Technical Details

### Architecture

```
metadata.json (publisher-authored)
  └─ entrypoints: [ { name, target } ]             ← new field
       └─ ValidMetadata::try_from validates       ← extend existing validator

↓ publish via ocx package push

package in OCI registry
  └─ metadata.json contains entrypoints

↓ ocx install

pull.rs:344  assemble_from_layers  ──────────┐
pull.rs:NEW  generate launchers              │  new step
pull.rs:355  post_download_actions           │
pull.rs:376  atomic temp → packages/         │  (launchers carried by existing atomic move)

packages/{reg}/sha256/{shard}/
  ├─ content/           ← hardlinked from layers/ (unchanged)
  ├─ refs/              ← unchanged
  ├─ metadata.json      ← unchanged
  ├─ resolve.json       ← unchanged
  └─ entrypoints/       ← NEW sibling
        ├─ cmake         (Unix .sh script, +x)
        ├─ cmake.cmd     (Windows .cmd batch)
        ├─ ctest
        └─ ctest.cmd

↓ ocx select

symlinks/{reg}/{repo}/
  ├─ current                 ← unchanged (flips to packages/.../content/)
  └─ entrypoints-current     ← NEW (flips to packages/.../entrypoints/)

↓ shell profile add

$PATH contains $OCX_HOME/symlinks/{reg}/{repo}/entrypoints-current

↓ user runs

$ cmake --version
  resolves via PATH → $OCX_HOME/symlinks/.../entrypoints-current/cmake
  symlink → packages/.../entrypoints/cmake
  script:  exec ocx exec --install-dir=<baked-content-path> -- "$@"
  ocx exec path-mode: reads metadata.json + resolve.json sibling to content,
     builds env (same code as identifier mode), spawns child with resolved env
  cmake --version  ← child process, receives clean env + PATH including {installPath}/bin
```

### API Contract (CLI surface added)

One new CLI flag is added to `ocx exec`:

```
ocx exec --install-dir=<absolute-path-to-content-dir> -- <command> [args...]
    Path mode: bypasses index resolution; reads metadata.json + resolve.json
    next to the given content path; resolves env; execs command.

    Mutually exclusive with positional <packages...>. Path must be absolute.

    Contract stability: this flag name and shape are load-bearing for all
    installed launchers and cannot change across OCX versions without a
    major-version deprecation cycle PLUS indefinite honoring of the old
    flag on behalf of already-installed launchers.
```

No new CLI command is added. One flag is added.

### Data Model

```
EntryPoint {
    name: EntryPointName,   // newtype, slug regex
    target: String,         // template string, validated at ValidMetadata
}

EntryPoints {
    entries: Vec<EntryPoint>,   // unique by name
}

Bundle {
    version: Version,
    strip_components: Option<u8>,
    env: Env,
    dependencies: Dependencies,
    entrypoints: EntryPoints,   // NEW, #[serde(default, skip_serializing_if = "is_empty")]
}

EntryPointError {
    InvalidName { name },
    DuplicateName { name },
}

Error::EntryPointTargetInvalid {
    name: String,
    source: TemplateError,
}

PackageErrorKind::EntryPointNameCollision {
    name: String,
    existing_package: oci::Identifier,
}
```

## Links

- [PRD](./prd_package_entry_points.md)
- [Research](./research_launcher_patterns.md)
- [Discover](./discover_package_entry_points.md)
- [ADR: three-tier CAS storage](./adr_three_tier_cas_storage.md) — establishes the `packages/.../content/` layout this feature extends with `entrypoints/`
- [ADR: deps name interpolation](./adr_deps_name_interpolation.md) — the `${deps.NAME.installPath}` surface this feature reuses in `target` template validation
- [GitHub issue #61](https://github.com/max-oberwasserlechner/ocx/issues/61)

## Update 2026-04-26 — Pre-Release Reshape

The decisions in Tension 1, Tension 2, and Tension 4 were revisited before any
release shipped a launcher of the original shape. Two structural changes
landed together on `feat/package-entry-points`; both are documented here in
full and supersede the matching sections above.

### Decision 1 — Unified `ocx exec` reference syntax (supersedes Tension 1 / Tension 4)

`ocx exec` now accepts package references in three forms:

| Input | Resolved as |
|---|---|
| `node:20` | bare → default `oci://` → identifier resolution + auto-install |
| `oci://node:20` | explicit `oci://` → identifier resolution + auto-install |
| `file:///abs/path/to/pkg-root` | `file://` → validate, read metadata, resolve env directly (no install, no index lookup) |

The `--install-dir` flag is removed. Generated launchers bake
`'file://<absolute-package-root>'` and call
`ocx exec 'file://<...>' -- "$(basename "$0")" "$@"` (Unix) or
`ocx exec "file://<...>" -- "%~n0" %*` (Windows). The `file://` scheme +
`ocx exec` argument shape are now the load-bearing runtime contract for
launchers, replacing the earlier `--install-dir` flag.

**Why supersede:** the original two-codepath split (positional identifier vs.
`--install-dir` flag) duplicated parsing, validation, and error surfaces;
unifying behind a scheme prefix collapses both into a single positional
shape and removes the contract surface that made the flag a one-way door.

### Decision 2 — Flat per-repo symlink layout (supersedes Tension 2)

`current` (and `candidates/{tag}`) target the package root rather than
`content/`. The sibling `entrypoints-current` symlink is removed. Consumers
traverse from a single anchor:

```
symlinks/{registry}/{repo}/
├── current               → packages/{...}/        (package root)
└── candidates/{tag}      → packages/{...}/        (package root)
```

- Tools that need files: `<current>/content/...`
- Shell PATH for entry points: `<current>/entrypoints`
- Metadata readers: `<current>/metadata.json`

Per-repo `.select.lock` still serializes the (now single) symlink update;
per-registry `.entrypoints-index.json` collision detection is name-keyed so it
is unaffected by the layout change. Shell profile load exports
`<current>/entrypoints` to PATH instead of the deleted `<entrypoints-current>`
path.

**Why supersede:** the sibling-symlink design required every selection to
flip *two* atomic symlinks under `.select.lock`, and would have forced a
parallel `candidates-current` to stay symmetric. Flattening to a single
per-repo anchor removes that growth, keeps `select` atomic via one symlink,
and lets every consumer reach what it needs with a single traversal.

**What survives unchanged:** publish-time `target` semantics, entry-point
name validation, `LauncherSafeString`, the per-registry collision-index
contract, GC reachability, install-time launcher generation lifecycle.

**Reversibility:** Both decisions are pre-release and one-way only in the
sense that any subsequent change must again be propagated to every installed
launcher. The original ADR's "load-bearing across releases" caveat applies
to the new shape.

### Negative consequences — Update 2026-04-26 (Arch-A2) {#update-2026-04-26-negative}

**Synthetic `file-url-mode/<digest>` OCI repository for `file://` exec paths.**

When `ocx exec 'file://<pkg-root>'` is invoked, the package is not identified by a real OCI `PinnedIdentifier` (no registry, no tag). To flow `file://` exec paths through the existing dedup map and `EntrypointNameCollision` error variants — both of which are keyed on `PinnedIdentifier` — the implementation at `crates/ocx_lib/src/package_manager.rs:300-345` synthesises a fake OCI repository component of the form `format!("file-url-mode/{}", &digest.hex()[..16])`. The first 16 hex digits of the content digest make each package root unique. This identifier is used only for dedup tracking inside `resolve_env`; it is never written to disk, never sent to a registry, and is not visible in any user-facing output.

The consequence is that `file://` packages are treated as a special case within the `PinnedIdentifier` type, creating a latent semantic mismatch: `PinnedIdentifier` values with `file-url-mode/…` repositories are not real OCI identifiers. Any future code that inspects the repository component must account for this synthetic prefix or filter `file://` paths upstream.

Flagged refactor target: introduce `enum PackageIdentity { Oci(PinnedIdentifier), PackageRoot(PathBuf) }` to make the distinction type-safe and eliminate the synthetic string. Per max-tier review finding Arch-A2.

## Stable Surfaces {#stable-surfaces}

Three orthogonal contracts are baked into every generated launcher. Changes to any of these break launcher-format compatibility with launchers already installed on user machines.

### (a) `file://` URI scheme passed to `ocx exec` {#stable-file-uri}

Launchers invoke `ocx exec 'file://<absolute-package-root>' -- ...`. The `file://` URI scheme is the scheme-dispatch mechanism that routes `ocx exec` into path-mode (no index lookup, reads `metadata.json` + `resolve.json` from disk). Removing or renaming this scheme requires regenerating every installed launcher.

### (b) `-- "$0" "$@"` / `-- "%~n0" %*` argv shape {#stable-argv-shape}

Both the POSIX `.sh` launcher and the Windows `.cmd` launcher pass `"$0"` / `"%~n0"` (the script's own name, which resolves to the entry-point name) as the first argument after `--`. `ocx exec` uses this to determine which binary to invoke inside the package's content tree. The `-- <name> <user-args>` separator + argv shape is preserved by both launcher formats. Changing the separator or the position of `<name>` requires regenerating every installed launcher.

### (c) `ocx exec` accepting `file://<pkg-root>` as a positional argument {#stable-exec-positional}

The positional `<packages...>` argument of `ocx exec` must continue to accept `file://`-prefixed values as a stable contract. If `ocx exec` ever stops parsing `file://` as a valid positional form — or if the argument is refactored into a flag — every installed launcher breaks without a migration. This constraint flows from the OCX version coupling reconfirmed in Tension 4: launchers call the host `ocx` binary; there is no version lock.

Per max-tier review finding Arch-A6.

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-04-21 | worker-architect (opus, Phase 4 of /swarm-plan high #61) | Initial draft |
| 2026-04-21 | worker-architect (opus, Phase 6 Round 2 of /swarm-plan high #61) | Round 2 corrections: (1) Tension 1 premise corrected (no identifier path-mode fallback exists) and re-weighed, flipping from Option A (positional) to **Option B (`--install-dir=<path>` flag)**; §8, launcher templates, UX scenarios, and data-flow diagram updated. (2) Reversibility upgraded from Medium to **One-Way Door High** with honest rationale; Codex cross-model review recommendation flagged for user decision. (3) Added Additional Considerations section covering #23 relative-symlinks interaction, bare-`ocx` PATH lookup trade-off, select-time (not install-time) collision policy, and SMB/UNC path limitation. (4) `.ps1` framing corrected — deferral is architectural (PowerShell `--` / `--%` semantics by design), not conditional on a fixable upstream bug. |
| 2026-04-26 | Reviewer (pre-release reshape on `feat/package-entry-points`) | Tension 1 + Tension 4: replaced `--install-dir` flag with `file://` URI scheme; `ocx exec` accepts bare/`oci://`/`file://` refs, launchers bake `file://<package-root>`. Tension 2: removed `entrypoints-current` sibling symlink; `current` and `candidates/{tag}` now target the package root, consumers traverse `<current>/content`, `<current>/entrypoints`, `<current>/metadata.json`. Both supersedes documented inline at the affected sections; new "Update 2026-04-26" decision block at the bottom summarizes the new shape. |
| 2026-04-29 | worker-doc-writer (Sonnet 4.6) | Added §Negative consequences — Update 2026-04-26 documenting synthetic `file-url-mode/<digest>` fake OCI repo for `file://` exec paths + flagged `PackageIdentity` refactor (Arch-A2). Added §Stable Surfaces enumerating three load-bearing launcher contracts: `file://` URI scheme, `-- "$0"/"$@"` argv shape, `ocx exec` positional `file://` acceptance (Arch-A6). Added transitive dep isolation clarification + `target` forward-slash-only note in §Collision Policy (SOTA-6, SOTA-7). |
