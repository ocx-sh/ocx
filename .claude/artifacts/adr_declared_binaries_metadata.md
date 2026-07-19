# ADR: Declared Binaries Metadata — Interface-Surface Executable-Name Claims

- **Status:** Accepted (2026-07-19)
- **Related:** `adr_dependency_manifest_pinning.md` (authoring/published split; output-sidecar-as-build-artifact precedent this ADR's persistence mechanism reuses directly), `adr_two_env_composition.md` (composer admission model this rides on), `research_bin_name_conventions.md` (grammar + Windows-extension survey), `subsystem-package.md`, `subsystem-package-manager.md`

## Problem

`ocx env` / `ocx package env` tell a caller what environment a composed set of
packages produces, but not what **executable names** actually resolve on the
resulting `PATH`. Automation (CI matrices, Bazel toolchain rules, devcontainer
features) that needs to answer "does `cmake` exist after installing this set"
today has to install the packages and probe PATH itself. This ADR adds a
**claim**, not a verified fact: a publisher-declared set of bare executable
names exposed on the package's interface `PATH` surface, consumable via a new
`binaries` array in the `ocx env` / `ocx package env` JSON report.

## Decision Summary

| # | Decision |
|---|---|
| 1 | New optional published field `Bundle.binaries: Option<Binaries>` — sorted, unique, **unverified** claim of interface-surface executable names |
| 2 | `ocx package create` tri-state auto-scan (`--bin-scan` / `--no-bin-scan` / neither) fills or verifies the claim against the on-disk content tree |
| 3 | `ocx package push` untouched beyond routine `ValidMetadata` cross-field checks (no new gate) |
| 4 | `ocx env` / `ocx package env` JSON gains `binaries` + `entrypoints` top-level sibling arrays; never in `--shell`/`--ci` sinks |
| 5 | `BinaryName` grammar per `research_bin_name_conventions.md` — looser than `EntrypointName`, bare (no `.exe`) |
| 6 | `ocx package test` default `self_view=false` already exercises the consumer surface — no change |

---

## 1. The `binaries` Field — Claim Semantics

```rust
// crates/ocx_lib/src/package/metadata/bundle.rs
pub struct Bundle {
    // ...existing fields...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binaries: Option<Binaries>,
}
```

**Why a claim, never a verified fact.** Push never decompresses layers
(streams blobs to the registry); multi-layer composition (`package push
sha256:<digest>.tar.xz` layer references) and `LayerRef::Digest` foreign
layers mean no point in the pipeline — create, push, or install — ever has
every layer's content tree materialized at once. Full-tree validation is
structurally impossible without breaking the streaming-push / lazy-pull
architecture (`adr_three_tier_cas_storage.md`). `binaries` is therefore
documentation-grade, not a guarantee — same trust model as a `package.json`
`bin` field or a Cargo `[[bin]]` name (per `research_bin_name_conventions.md`
survey, every peer ecosystem makes the same trade).

**Scope — interface-surface, own-package only:**
- Only names exposed via **`${installPath}`-rooted `Path`-modifier env vars
  with `visibility.has_interface() == true`**. Private/`libexec`-style
  directories are excluded by definition — they were never candidates.
- **Own-package claims only**, never transitive. A package never lists a
  dependency's binaries. Edge visibility defaults to `sealed`; the transitive
  closure of what's *reachable* through a dependency chain is a composition
  query (`ocx env`'s `binaries` array, §4), not a per-package metadata fact.
- **Entrypoints are never listed here.** They are a separate, already-typed
  field (`Bundle.entrypoints`); the runtime synthetic `entrypoints/` PATH
  directory shadows declared `bin/` (`composer.rs::emit_dep_path_block`
  ordering invariant) so a name can appear in both `binaries` (declares the
  underlying binary exists) and `entrypoints` (declares a launcher wraps it)
  without contradiction.

**`None` vs `Some(empty)` — deliberately distinct wire states:**

```json
// undeclared / predates this field — omitted entirely
{ "type": "bundle", "version": 1 }

// declared: verified/asserted zero interface binaries (e.g. env-only package)
{ "type": "bundle", "version": 1, "binaries": [] }
```

This is why the field is `Option<Binaries>` with `skip_serializing_if =
"Option::is_none"` — **not** the `Entrypoints`/`Env`/`Dependencies` pattern
(`#[serde(default)]` + `skip_serializing_if = "X::is_empty"`), which
deliberately collapses `None` and empty into the same wire absence. Here the
two states carry different meaning: "nobody declared" vs "publisher asserts
none." A consumer reading raw `metadata.json` (e.g. a security/SBOM scanner)
can distinguish "declared empty on purpose" from "field predates this ADR."

**Element read-contract v1 — untagged `string | object` union:**

```rust
// crates/ocx_lib/src/package/metadata/binary.rs (new)
pub struct BinaryName(String); // grammar: see §5

pub struct Binaries(std::collections::BTreeSet<BinaryName>); // sorted, unique

// Deserialize path only — never constructed by the writer:
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum BinaryElement {
    Name(BinaryName),
    Object { name: BinaryName, #[serde(flatten)] _reserved: serde_json::Map<String, serde_json::Value> },
}
```

- **Write side always emits bare strings.** `Binaries`'s `Serialize` is a
  plain derive over the sorted `BTreeSet<BinaryName>` — no object form is
  ever produced by `ocx`.
- **Read side accepts both** so a v1 binary reading a v2-published
  `metadata.json` (hypothetically carrying richer per-binary objects) does
  not hard-fail — it extracts `name`, ignores unknown keys via the flattened
  map. This buys forward-compat headroom **without** bumping
  `bundle::Version` to `V2` for what would otherwise be a wire-breaking
  addition. Mirrors the `serde_repr` reject-unknown-version precedent
  (`quality-rust.md` "Version Enum via serde_repr") in spirit: extend the
  *element* grammar, not the *document* version, when the extension is
  additive and ignorable by old readers.

---

## 2. Create-Time Auto-Scan — Tri-State

### CLI surface

```rust
// crates/ocx_cli/src/options/bin_scan.rs (new — mirrors options/pull.rs)
#[derive(clap::Args, Clone, Debug, Default)]
pub struct BinScan {
    #[clap(long = "bin-scan", overrides_with = "no_bin_scan")]
    bin_scan: bool,
    #[clap(long = "no-bin-scan", overrides_with = "bin_scan")]
    no_bin_scan: bool,
}

pub enum BinScanMode { Auto, Verify, Off }
impl BinScan {
    pub fn mode(&self) -> BinScanMode { /* neither → Auto; --bin-scan → Verify; --no-bin-scan → Off */ }
}
```

Same POSIX last-wins paired-flag shape as `options::Pull`, but **tri-state**
(`mode()`, not `enabled(default: bool)`) because "auto" and "off" are not a
default/override pair — they're three distinct behaviors.

### Scan function

```rust
// crates/ocx_lib/src/package/bin_scan.rs (new — sibling to dependency_pinning.rs,
// the other "ocx package create compile step" module)
pub async fn scan_interface_binaries(
    content_root: &std::path::Path,        // the pre-archive source tree (`self.path` in package_create.rs)
    metadata: &AuthoringMetadata,
    platform: &oci::Platform,               // selects Unix exec-bit vs Windows extension-allowlist convention
) -> crate::Result<std::collections::BTreeSet<BinaryName>>
```

**Algorithm:**
1. Collect every `Var` in `metadata`'s `env` where `modifier` is `Path` **and**
   `visibility.has_interface()` **and** the value template is exactly
   `${installPath}/<rel>` (no combination with `${deps.*}`, no extra literal
   segments). Vars not matching this exact shape are excluded from scan scope
   — best-effort, not an error (a combined-path var is rare and the publisher
   can always hand-author `binaries` for it).
2. **`strip_components` mapping (extraction-time semantics).** `content_root`
   is the pre-archive tree; `strip_components` is applied by the *installing*
   client at extraction time, not by `create`. So the on-disk directory
   backing `${installPath}/<rel>` post-install is `content_root/<N wildcard
   path segments>/<rel>` where `N = strip_components.unwrap_or(0)`. Parse
   `rel` through the existing hardened primitive
   `utility::fs::path::RelativePath::parse` (rejects absolute, escaping, and
   drive/UNC forms — same untrusted-relative-under-root class the codebase
   already solved once; per SOTA review this is the exact npm/pnpm
   bin-path-traversal bug family, closed here by reuse, not reinvention).
   Then use `utility::fs::DirWalker` bounded to
   `max_depth(N + rel.components().count())` with a `classify` that descends
   until depth `N` and joins the validated `rel` under each depth-`N` dir via
   `join_under_root`; every matching directory is a scan target
   (`WalkDecision::leaf`). In the common single-top-level-directory
   case (the entire reason `strip_components` exists — website
   `metadata.md` §Extraction) this resolves to exactly one target dir; a
   package with two top-level dirs both containing the same `rel` scans the
   union (harmless — more claims found, never fewer).
3. For each target directory (probed for existence first — a missing
   directory contributes zero candidates, not an error, mirroring `Path.
   required: false` semantics), list immediate entries (not recursive —
   `PATH` only ever puts one literal directory on the search path, never its
   subdirectories):
   - **Unix / `any` platform:** regular file **and** exec bit set
     (`metadata.is_file() && mode & 0o111 != 0` — a bare exec-bit test would
     claim every subdirectory, since directory 0755 carries `x` meaning
     "traversable"; `tokio::fs::metadata` **follows symlinks**, so
     `bin/gcc -> gcc-13` counts as a claim named `gcc`; a dangling symlink's
     metadata error is swallowed, excluded from claims, not a hard error).
   - **Windows platform:** strip the extension against a **new fixed
     allowlist constant** `BIN_SCAN_WINDOWS_EXTENSIONS = [".exe", ".com",
     ".bat", ".cmd"]` — deliberately the same set as
     `env.rs::resolve_command_windows`'s PATHEXT *fallback* (env.rs:409-432),
     frozen as a source constant. **Resolution promise (Codex gate):** the
     claim describes what resolves under the *default* Windows resolution
     set; the runtime resolver additionally honors the composed child
     `PATHEXT`, so a hardened child env may fail to resolve a claimed
     `.bat`/`.cmd`/`.com` name — consistent with claim semantics (existence
     is never guaranteed), stated in the field docs. The constant is never
     read from `%PATHEXT%` (per `research_bin_name_conventions.md` Pitfalls);
     what `adr_windows_exe_shim.md` deleted was the *launcher-side* PATHEXT
     injection machinery, not this resolver.
   - A filename whose (extension-stripped, for Windows) stem fails
     `BinaryName::try_from` is silently excluded (not an error) — e.g. a
     stray `.DS_Store` or `README` in a `bin/` dir is not a binary claim.
4. Return the union as a plain `BTreeSet<BinaryName>` — **not** yet a
   `Binaries` (case-fold-collision validation happens once, at the call site
   that turns a scan or an authored list into the typed collection — see §5).

### Mode semantics

| Mode | Authoring field absent | Authoring field `Some(declared)` (incl. `[]`) |
|---|---|---|
| **Auto** (neither flag) | Scan; bake the scanned set into the **output sidecar** written next to `-o` (see §2.1) via `with_binaries`, however small. The **authored `-m` input is never rewritten** (`create` never writes back to `-m` — see §2.1). | Scan **not** run. Declared list passed through verbatim. |
| **`--bin-scan`** (Verify) | Behaves exactly like Auto: scan + bake into the output sidecar (possibly `[]`). Verification requires a declaration to verify against — first use needs no onboarding dance (revised after One-Way-Door review: the original "any find = error" made `--bin-scan` unusable on an undeclared package without a hand-copy step). | Scan; **one-directional** diff: a scanned name absent from the declared list → error (`UndeclaredBinary`, exit 65); a declared name found on disk but not executable → error (`DeclaredNotExecutable`, exit 65); a declared name simply absent from disk → **legal**, no error (may be platform-conditional, dependency-sourced via a future indirection, or genuinely absent from this build). On success, declared list passes through verbatim. |
| **`--no-bin-scan`** | No scan. Published field stays whatever the authoring field is (`None` stays `None`). | No scan. Declared list passes through verbatim. |

### §2.1 — Auto's fill persists into the *output* sidecar, never the authored input

**Revised from the original draft** (which treated the scan-fill as a
`create`-time-only convenience discarded before `push`). That was wrong: it
contradicted the feature's whole purpose — the config blob is what
downstream tooling (e.g. `rules_ocx`) reads, so a claim that never reaches
the registry payload is not a convenience, it's a no-op.

**The fix rides an existing, unrelated fact about `create`'s I/O, not a new
mechanism.** `ocx package create -o <bundle> -m <input>` **never writes back
to `-m`.** It always resolves + writes a canonical sidecar **next to `-o`**
via `conventions::infer_metadata_file(&output)` (`package_create.rs:118-123`).
This is exactly how dependency pins already survive from `create` to `push`
today (`AuthoringDependency`'s digest, `AuthoringBundle.platform` via
`with_platform` — D5) — `push --metadata` isn't even required to be passed
explicitly for this to work: `conventions::resolve_metadata_path` defaults
`-m` to the sibling of the first file layer, i.e. **exactly** the sidecar
`create` wrote. `binaries` just needs to ride the same rail dependency pins
and the recorded platform already ride.

**Mechanism — a consuming builder, exact `with_platform` precedent, no
`to_published` signature change:**

```rust
// crates/ocx_lib/src/package/metadata/authoring.rs (new, alongside with_platform)
impl AuthoringMetadata {
    #[must_use]
    pub fn with_binaries(self, binaries: Binaries) -> Self {
        match self {
            AuthoringMetadata::Bundle(bundle) => AuthoringMetadata::Bundle(AuthoringBundle {
                binaries: Some(binaries),
                ..bundle
            }),
        }
    }
}
```

`to_published`'s **signature is unchanged** — no `scanned_binaries`
parameter. Its body gains exactly one line when WP1 adds the field: the
`Metadata::Bundle{...}` struct literal grows `binaries:
bundle.binaries.clone()`. That projection is then correct once
`with_binaries` has run, because by the time
`to_published` is called the in-memory `AuthoringMetadata` value already
carries the scanned set as an ordinary declared field. (This supersedes the
original Decision D sketch of threading a scan result through `to_published`
— that mechanism is now unnecessary. See revised Component Contracts.)

**Ordering in `package_create.rs::execute`** (revises the existing flow,
inserting the scan step before the existing validation call):

```
read -m input (AuthoringMetadata)
  → resolve_dependency_pins
  → compute platform (validation_platform / self.platform)
  → scan_interface_binaries(self.path, &metadata, &platform)   [Auto or Verify mode; skipped under Off]
  → branch on metadata.binaries() — mutually exclusive:
      None (Auto or Verify):  metadata = metadata.with_binaries(scanned)   [fill; no check — nothing declared to verify]
      Some(declared), Verify: one-directional check against declared (§2 table); bail on violation; never writes
      Some(declared), Auto:   pass through verbatim; no scan ran
  → ValidMetadata::try_from(metadata.to_published(&platform)?)?   [unchanged call, now sees the filled field]
  → resolved_metadata = Some(metadata.with_platform(platform))
  → ... build archive ...
  → resolved_metadata.write_json(infer_metadata_file(&output))    [output sidecar — carries binaries]
```

`with_binaries` must run **before** the `to_published`/`ValidMetadata` call
so validation exercises the same value that gets written, and **before**
`with_platform` only insofar as both are applied to the same `metadata`
binding prior to the final `write_json` — order between `with_binaries` and
`with_platform` themselves doesn't matter (disjoint fields, both consuming
builders on the same value).

**Why this is staleness-free** (the property the original draft was
protecting, just via the wrong mechanism): the **authored `-m` input is
never mutated** — every `ocx package create` invocation re-reads it fresh,
so a field left absent in `-m` is rescanned from the current content tree on
every run, never cached. The **output sidecar is a build artifact**, exactly
like the resolved dependency pins and recorded platform that already live
only there — regenerated in full on every `create`, never hand-edited, never
a source of drift. This is the identical trust boundary
`adr_dependency_manifest_pinning.md` already established for pins; `binaries`
adds nothing new to that boundary, it just uses it.

---

## 3. `ocx package push` — Untouched

No new gate. `ValidMetadata`'s existing cross-field checks are unaffected;
`binaries` carries no `${deps.*}`/`${installPath}` template to validate (it's
a bare-name array, not a template field). Entrypoint-name / binary-name
collision (e.g. `binaries: ["cmake"]` and `entrypoints: {"cmake": {...}}`) is
**silently allowed** — a launcher legitimately wraps a binary of the same
invocable name, and `entrypoints/`'s synth-PATH shadowing already resolves
the runtime ambiguity (§1, "Entrypoints are never listed here").

---

## 4. Env Report — `ocx env` / `ocx package env`

No new flag. JSON gains two top-level sibling arrays on the existing
`EnvVars` envelope (already future-proofed for exactly this — the current
doc comment on `api/data/env.rs::EnvVars` reads "future top-level fields
(e.g. `entrypoints`) can be added without breaking the wire format"):

```json
{
  "entries": [ /* unchanged */ ],
  "binaries": [ { "name": "cmake", "package": "ocx.sh/cmake:3.28@sha256:..." } ],
  "entrypoints": [ { "name": "fmt", "package": "ocx.sh/cmake:3.28@sha256:..." } ]
}
```

- Structural discriminator (separate top-level keys) — no `kind` tag needed,
  unlike `EntrySource`'s tagged enum (that disambiguates rows *within* one
  array; these are separate arrays).
- `package` is `Option<String>` — **absent means "attribution unknown," never
  "this package has zero binaries."** In practice, with the admission model
  chosen below (Decision A), `package` is populated for every entry; the
  `Option` typing exists so a future no-clean-attribution source (e.g. a
  patch-companion-contributed name — explicitly out of scope here, §7) can be
  added without a breaking schema change.
- **Never surfaced on `--shell`/`--ci` sinks.** `emit_lines`/`export_ci` keep
  their existing `&[Entry]`-only signatures untouched; both command files
  already `return` on the `--shell`/`--ci` branches *before* the point where
  `EnvVars` gets constructed, so this falls out of the existing control flow
  rather than requiring a new guard.
- **Deltas vs GitHub issue #177** (recorded per team-lead instruction, not
  reopened): no `--bins` flag (this is unconditional JSON, not opt-in), no
  `path` field (a claim, not a resolved filesystem fact — `Client::exec` /
  `package which` already answer "where," this answers "what name"), no
  global first-name-wins ordering (every admitted package's claims all
  appear — they are per-package claims, not directory-attributed like a
  literal `PATH` scan would be).

### Decision A — Projection source: complete closure via `ComposeOutput`, not roots-only

**Option 1 (rejected): roots-only.** `command/env.rs:107-116` /
`toolchain_env.rs:273-315` already hold `Vec<Arc<InstallInfo>>` for the
explicit roots. Reading `root.metadata().binaries()` directly needs zero new
plumbing. **Rejected** because it silently omits every transitively admitted,
interface-visible dependency's binaries — e.g. a metapackage `cmake` that
publicly depends on `ninja` would report only `cmake`'s own claims even
though `ninja`'s binaries are equally resolvable on the composed `PATH`. That
is a correctness gap in the exact question this feature exists to answer
("what names resolve here"), not a cosmetic omission.

**Option 2 (chosen): extend `composer::compose`'s existing admission walk.**
`compose()` already loads every admitted TC entry's `Metadata` once (the
`loaded` vec in the per-root loop) purely to emit env vars, and already knows
`root.metadata()` for each explicit root. Reading `.binaries()` /
`.entrypoints()` off values already in scope during that same loop is **zero
additional I/O** — not a second traversal, not a new `PackageManager` method,
not a metadata reload keyed off the (currently internal-only) `admitted:
Vec<PinnedIdentifier>` list. `ComposeOutput` gains two pre-flattened,
visit-ordered fields:

```rust
// crates/ocx_lib/src/package_manager/composer.rs (extends ComposeOutput)
pub struct ComposeOutput {
    pub entries: Vec<Entry>,
    pub admitted: Vec<oci::PinnedIdentifier>,                        // unchanged
    pub admitted_binaries: Vec<(oci::PinnedIdentifier, BinaryName)>,     // new
    pub admitted_entrypoints: Vec<(oci::PinnedIdentifier, EntrypointName)>, // new
}
```

**Admission rule — reuses the existing gate, no third axis invented:** a
package's declared `binaries`/`entrypoints` appear in the report **iff that
package was admitted to this compose call** — root packages unconditionally
(mirrors `entries`' unconditional root emission), dependency packages iff
their TC entry passes the active surface gate (`has_interface()` default /
`has_private()` under `--self`, the same test already gating `entries` and
`admitted`). No separate "is this claim itself interface-scoped" re-check is
needed at the report layer: `binaries`' claim scope (§1, interface-surface
only) is already enforced once, at authoring time — admission to `compose`
governs report inclusion, exactly like every other field this function
emits.

**Delivery mechanism (revised after One-Way-Door review's call-site census):**
`resolve_env_with_patch_boundary`'s signature is **unchanged**. The
tuple-growing sketch was retracted: the function has ~15 call sites across 7
files (incl. `direnv_export.rs`, `run.rs`, `patch_why.rs`,
`tasks/patch_test.rs`, the `resolve_env` wrapper's internal call, and 6+
exact-arity 3-tuple destructures in `resolve.rs`'s own test modules) — every
one would need a mechanical arity fix for data it discards. Instead, since
`ComposeOutput` is a named struct (field growth is non-breaking), the two
consumers that actually need attribution get a **new sibling accessor**:

```rust
// crates/ocx_lib/src/package_manager/tasks/resolve.rs
pub struct AdmittedBinaries {
    pub binaries: Vec<(oci::PinnedIdentifier, BinaryName)>,
    pub entrypoints: Vec<(oci::PinnedIdentifier, EntrypointName)>,
}

/// Like `resolve_env_with_patch_boundary`, additionally surfacing the
/// admitted claim attribution. Consumers: `ocx env`, `ocx package env`.
pub async fn resolve_env_with_attribution(
    &self, packages: &[Arc<InstallInfo>], self_view: bool, scope: PatchScope,
) -> crate::Result<(Vec<Entry>, usize, Vec<PatchProvenance>, AdmittedBinaries)>
```

Both delegate to the same internals; the legacy 3-tuple form simply drops
the attribution field (exactly how `resolve_env` already drops
`patch_start`/`provenance` today — same insulation pattern, one level up).

**Blast radius under the new-accessor mechanism (census by both reviewers,
2026-07-19 — `resolve_env_with_patch_boundary` has ~15 call sites across 7
files incl. `run.rs`, `direnv_export.rs`, `patch_why.rs`,
`tasks/patch_test.rs`, and 6+ exact-arity test destructures in `resolve.rs`
itself, which is what killed the tuple-growth sketch):**
- Existing signatures untouched → **zero existing call sites change**:
  `resolve_env` and `resolve_env_with_patch_boundary` keep their returns;
  every current caller — command files, task files, and the entire
  `resolve.rs` test suite — compiles unmodified.
- New code only: `ComposeOutput` field growth (non-breaking struct
  extension), the `resolve_env_with_attribution` accessor, and its two
  adopters — `command/env.rs` and `command/toolchain_env.rs` (whose internal
  `resolve_global_pinned_env` helper switches to the new accessor and grows
  its own private `Option<(...)>` return + `Ok(Some((...)))` match arm,
  contained within that one file).
- **Performance courtesy, not a correctness requirement:** both command
  files already `return` early on the `--shell`/`--ci` branches before
  touching `entries`; the `AdmittedBinaries` → `BinaryAttribution` JSON
  conversion should likewise happen only in the fallthrough
  structured-report branch, so a pure `--shell` eval caller never pays for
  data it discards. (The metadata itself is already loaded inside `compose`
  regardless of which branch the caller takes — this is about the JSON
  attribution mapping only, which is cheap either way.)
- **Explicitly out of scope:** patch-companion-contributed binaries.
  Companion overlay entries are appended by `build_site_patch_set` *after*
  `compose()` returns, over a structurally different provenance model
  (`PatchProvenance`, not `PinnedIdentifier` admission). Mixing the two would
  require a design decision this ADR does not make; `binaries`/`entrypoints`
  report only `compose()`-admitted packages.

### Decision C — Plain-table rendering: `print_hint`, not a conditional column

**Option 1 (rejected): conditional `Binaries` column**, mirroring
`api/data/env.rs`'s `has_patch_entry` → conditional `Source` column
(`env.rs:110-137`). Rejected because the analogy breaks on row shape: the
`Source` column adds a *cell* to each existing `entries` row (same row axis,
new dimension). `binaries`/`entrypoints` are a **differently shaped**
dataset — `(name, package)` pairs with no natural per-`entries`-row mapping
(a `PATH=/foo/bin:/bar/bin` row has no single "which binary" cell to fill).
Force-fitting them as columns would misrepresent the data.

**Option 2 (rejected): second table.** Violates the Single-Table Rule
(`subsystem-cli-api.md`) outright.

**Option 3 (rejected): JSON-only, silent in plain mode.** Leaves an
interactive `ocx env` user with zero discoverability of the new capability.

**Chosen: `DataInterface::print_hint`** (existing primitive, per
`subsystem-cli-api.md`'s channel table). Plain format's `Key | Type | Value
[| Source]` table is unchanged — CI scripts parsing it today keep working
byte-for-byte. After the table, a short hint line summarizes availability,
e.g. `5 binaries available (cmake, ctest, cpack, ...); use --format json for
the full list`. This respects both the Single-Table Rule and the
backend-first principle (`product-context.md` #1: JSON is the primary
machine-consumption path; plain mode is a human glance, and a hint is the
correct weight for a glance).

---

## 5. `BinaryName` Grammar

Per `research_bin_name_conventions.md` (already-completed survey of npm,
Cargo, Scoop, Chocolatey, nixpkgs, mise/asdf — all converge on bare-name
claims, extension handling pushed to the platform-specific resolve/shim
step, never into cross-platform metadata):

- ASCII printable, no whitespace anywhere.
- Forbidden: `/ \ < > : " | ? *` (Windows-reserved filename chars — OCX
  materializes names as real files on Windows).
- No leading `-` (flag-lookalike shell hazard).
- No leading or trailing `.` (Unix hidden-file ambiguity / Windows silent
  strip → on-disk collision).
- Non-empty, max 64 bytes (same cap as `EntrypointName`/`DependencyName` —
  `slug::SLUG_MAX_LEN`, though `BinaryName` does **not** reuse `slug::
  SLUG_PATTERN`, since the character class is deliberately looser to admit
  `python3.13`, `c++`, `MSBuild`).
- Case-insensitive rejection of reserved Windows device names (`CON PRN AUX
  NUL COM0`–`COM9 LPT0`–`LPT9`) — applied to the **basename before the first
  dot**, so suffixed aliases (`CON.txt`, `NUL.foo`, `LPT1.log`) are rejected
  too (Windows reserves device names with any extension; Codex gate finding).
- **Case-preserving as declared**, but two names differing only by case-fold
  within the same package are rejected — see Decision B.
- **Bare names only, never `.exe`** — matches every surveyed ecosystem;
  extension handling is entirely the scan step's / resolver's job (§2 step
  3), never stored.

**Security note (explicit closure claim, per SOTA review):** the grammar
structurally closes the npm/pnpm bin-field path-traversal CVE family — two
distinct pnpm advisories, plus npm's own precedent:

- **GHSA-xpqm-wm3m-f34h / CVE-2026-23890** — scoped-name `../` bypass in
  pnpm's bin-field handling, leading to arbitrary file creation.
- **GHSA-4gxm-v5v7-fqc4 / CVE-2026-55699** — a bare `..` bin key surviving
  into pnpm's global uninstall path, leading to arbitrary directory
  deletion.
- **CVE-2026-24131** — pnpm's `directories.bin` field joined via unchecked
  `path.join()`, escaping the install root.
- npm pre-6.13.3's leading `/`/`.`/`..` bin-path escapes (the family's
  original precedent, pre-dating GHSA IDs).

Both pnpm CVEs are closed by the same two grammar rules: `/` and `\` are
forbidden outright (no scoped/nested names exist to normalize), and
leading/trailing-`.` rejection kills bare `..` — so neither the `../`
traversal nor the bare-`..` deletion vector can be expressed as a
`BinaryName` in the first place. Spec tests assert these vectors explicitly,
not incidentally.

### Decision B — Duplicate / case-fold-collision encoding: construction-time validation, not a custom `Deserialize` visitor

**Option 1 (rejected): custom `Deserialize` visitor**, mirroring
`Entrypoints`'s `MapAccess` visitor (`entrypoint.rs:254-295`) that rejects
duplicate keys during the deserialize walk itself.

**Option 2 (chosen): construction-time validation via `TryFrom`.** House
precedent, cited verbatim by the calling task: "Entrypoint uniqueness is
enforced at construction time by `Entrypoints::new`... so no publish-time
entrypoint validation step is needed here" (`validation.rs` module doc,
lines 16–18) — i.e. the codebase's own stated preference, even for the
`Entrypoints` map case, is a single construction-time check reused by every
entry path, not a visitor duplicated per format.

`Entrypoints`'s visitor pattern doesn't actually transfer cleanly here
anyway: it exists because a JSON **object**'s `serde_json` default is silent
last-wins on duplicate *keys* — there is no analogous silent-data-loss
hazard for a JSON **array** deserialized into a `BTreeSet` (exact-duplicate
array elements collapse via ordinary `Ord`/`Eq` set semantics with zero
information loss — two claims of the literal same name are the same claim,
not a collision). The only case needing an explicit reject is **case-fold**
collision (`Cmake` and `cmake` — different strings, same on-disk filename
under a case-insensitive target filesystem), which is not a "detect during
the parse walk" problem, it's a "validate the fully-collected set" problem —
squarely construction-time:

```rust
// crates/ocx_lib/src/package/metadata/binary.rs
impl TryFrom<std::collections::BTreeSet<BinaryName>> for Binaries {
    type Error = BinaryError; // BinaryError::CaseFoldCollision { first: BinaryName, second: BinaryName }
}
```

`Binaries`'s custom `Deserialize` impl collects the untagged `BinaryElement`
sequence (§1) into a `BTreeSet<BinaryName>` and calls this `TryFrom`,
mapping the error via `serde::de::Error::custom` — same call graph the
create-time scan uses (§2 step 4 hands its scanned `BTreeSet` to the same
`TryFrom`). **One validation function, two callers** — matches DRY guidance
in `quality-core.md` ("single source of truth for business logic").
Case-fold collision is a real hazard even though the *authoring* filesystem
is usually case-sensitive: a Linux CI host building Windows-targeted content
can produce `Cmake` and `cmake` as two distinct files that only collide once
extracted onto the case-insensitive target.

---

## 6. `ocx package test` — No Change

`self_view` already defaults to `false`, which already exercises the
consumer (interface) surface — the exact surface `binaries` claims describe.
Recorded here as a **fact**, not a decision: no code changes to `package_test.rs`.

---

## Component Contracts Summary

| Component | Signature / Shape |
|---|---|
| `BinaryName` | `TryFrom<String>`/`TryFrom<&str>`, `Display`, `AsRef<str>`; grammar §5 |
| `Binaries` | `BTreeSet<BinaryName>` wrapper; `TryFrom<BTreeSet<BinaryName>>` (case-fold check); custom `Deserialize` (untagged element union → `TryFrom`); derived `Serialize` (bare-string array) |
| `Bundle.binaries` / `AuthoringBundle.binaries` | `Option<Binaries>`, `#[serde(default, skip_serializing_if = "Option::is_none")]` — **not** the `X::is_empty` pattern |
| `Metadata::binaries(&self) -> Option<&Binaries>` | Mirrors `entrypoints()` accessor shape |
| `AuthoringMetadata::binaries(&self) -> Option<&Binaries>` | Mirrors `dependencies()` accessor shape |
| `AuthoringMetadata::to_published(&self, platform: &Platform) -> Result<Metadata, AuthoringError>` | Signature unchanged; struct literal gains `binaries: bundle.binaries.clone()` (correct once `with_binaries` has run) |
| `AuthoringMetadata::with_binaries(self, binaries: Binaries) -> Self` | New consuming builder, exact `with_platform` precedent |
| `options::BinScan` + `BinScanMode` | Paired `--bin-scan`/`--no-bin-scan`, tri-state `.mode()` |
| `package::bin_scan::scan_interface_binaries(content_root, metadata, platform) -> Result<BTreeSet<BinaryName>>` | New module, sibling of `dependency_pinning.rs` |
| `BinScanError` | `thiserror`, `#[non_exhaustive]`, `ClassifyExitCode → DataError (65)` — variants `UndeclaredBinary { name, path }`, `DeclaredNotExecutable { name, path }` |
| `composer::ComposeOutput` | + `admitted_binaries: Vec<(PinnedIdentifier, BinaryName)>`, `admitted_entrypoints: Vec<(PinnedIdentifier, EntrypointName)>` |
| `resolve::AdmittedBinaries` | New struct returned by new `resolve_env_with_attribution` accessor; `resolve_env`/`resolve_env_with_patch_boundary` signatures unchanged |
| `api::data::env::BinaryAttribution` | `{ name: String, package: Option<String> }`, `#[serde(skip_serializing_if = "Option::is_none")]` on `package` |
| `api::data::env::EnvVars` | + `binaries: Vec<BinaryAttribution>`, `entrypoints: Vec<BinaryAttribution>`; `new()` grows two params |

## UX Scenarios

| Scenario | Command | Outcome | Exit |
|---|---|---|---|
| No `--bin-scan`/`--no-bin-scan`, field absent, `bin/` has 3 executables | `ocx package create -o bundle.tar.xz -m input.json` | `input.json` unchanged on disk; output sidecar (`bundle-metadata.json`) carries `binaries` with 3 names | 0 |
| `--bin-scan`, field absent, `bin/` has 3 executables | `ocx package create --bin-scan` | Same as Auto: output sidecar carries the 3 names | 0 |
| `--bin-scan`, field absent, `bin/` empty/missing | `ocx package create --bin-scan` | Same as Auto: output sidecar carries `binaries: []` | 0 |
| `--bin-scan`, `binaries: ["cmake"]` declared, `cmake` present + executable | `ocx package create --bin-scan` | Succeeds; declared list passed through | 0 |
| `--bin-scan`, `binaries: ["cmake"]` declared, `cmake` present but not `+x` | `ocx package create --bin-scan` | `BinScanError::DeclaredNotExecutable` | 65 |
| `--bin-scan`, `binaries: ["cmake"]` declared, `cmake` absent from disk | `ocx package create --bin-scan` | Succeeds (declared-but-missing is legal) | 0 |
| `--no-bin-scan`, any field state | `ocx package create --no-bin-scan` | No scan; field passed through verbatim | 0 |
| Hand-authored `Cmake` + `cmake` in same `binaries` array | any `ocx package create` mode that constructs `Binaries` | `BinaryError::CaseFoldCollision` at JSON parse (or scan-collected set construction) | 65 |
| `ocx env` / `ocx package env`, plain format | — | Unchanged table + new hint line if any binaries admitted | 0 |
| `ocx --format json env` | — | `binaries`/`entrypoints` arrays populated from admitted-set closure | 0 |
| `ocx env --shell=bash` / `--ci=github` | — | Byte-identical to pre-ADR output; no binaries/entrypoints leak into the sink | 0 |

## Error Taxonomy

| Error | Layer | Exit |
|---|---|---|
| `BinaryError` grammar variants (`Empty`, `InvalidCharacter`, `Whitespace`, `LeadingDash`, `LeadingOrTrailingDot`, `TooLong`, `ReservedWindowsDeviceName`) | `serde` parse (metadata.json read) | 65 (existing metadata-parse classification) |
| `BinaryError::CaseFoldCollision` | `serde` parse or scan-collected `TryFrom` | 65 |
| `BinScanError::UndeclaredBinary` | `ocx package create --bin-scan` (declared field present only) | 65 |
| `BinScanError::DeclaredNotExecutable` | `ocx package create --bin-scan` | 65 |
| `BinScanError::UnsupportedHostScan` | `ocx package create --bin-scan` — this host cannot evaluate the target platform's executable-file convention | 65 |
| `--bin-scan` without `-m`/`--metadata` | `ocx package create` CLI validation (`package_create.rs::validate_bin_scan`) | 64 |

The paired `--bin-scan`/`--no-bin-scan` flags themselves have no invalid combination — ordinary
`overrides_with` last-wins, same as every other paired flag in the CLI. The usage-error row above
is a separate precondition check on `--bin-scan` alone, added during the fix pass (see Fix-Pass
Refinements below): Verify mode needs a declaration to verify against, and there is nothing to
check without a metadata sidecar, so an explicit `--bin-scan` given without `--metadata` is
rejected rather than silently no-op'ing.

## Edge Cases

| Case | Resolution |
|---|---|
| Empty or nonexistent PATH-target dir | Existence probed before walk; zero candidates, not an error |
| Symlink in a scanned `bin/` dir | Followed (`tokio::fs::metadata`); dangling symlink silently excluded |
| `--platform any` package | Unix exec-bit convention only; no Windows extension-stripping (no native OS convention for `any`) |
| Multi-platform `create` sequence | Each per-platform invocation scans independently using its own `--platform`; no cross-invocation state (D5 single-platform-per-invocation architecture already enforces this) |
| Zero executables with a `PATH` var declared | Legal in every mode; Auto/Verify with absent field bake `Some([])`; with a declared field, Verify validates only, field passes through as authored |
| Subdirectory inside a scanned PATH dir (e.g. `bin/vendor/`, mode 0755) | Excluded — scan requires `is_file()`, not just the exec bit |
| Filename that fails `BinaryName` grammar (e.g. `.DS_Store`) | Silently excluded from scan results, not an error |
| Combined-path `Path` var (`${installPath}/bin:${deps.x.installPath}/bin`) | Excluded from scan scope entirely (not the simple `${installPath}/<rel>` shape); publisher hand-authors if needed |
| Multiple top-level dirs at the `strip_components` depth, both containing `<rel>` | Scan unions all matches — more claims found, never fewer |
| Foreign / multi-layer composition (layers added at `push` time, not visible to `create`'s scan) | Unsupported for auto-detect by design — authored list is the only way to declare binaries contributed by a foreign layer |

## Out of Scope (this ADR)

- Writing Auto mode's scan result back into the **authored `-m` input file**
  — `create` never writes back to `-m` for any field (deps, platform,
  binaries alike); this is existing, unrelated behavior, not a new
  restriction (§2.1).
- Patch-companion-contributed binaries in the env report (§4 Decision A).
- Any verification at `ocx package push` beyond existing `ValidMetadata`
  checks (§3).
- Non-ASCII binary names (research artifact: no real-world demand surveyed).

## Fix-Pass Refinements (post-acceptance)

Recorded, not redesigned — tightened during implementation review, no decision above reverses:

- **Fail-closed scan I/O.** `collect_directory_candidates` (`bin_scan.rs`) treats only
  `NotFound`/`NotADirectory` on the target directory as "zero candidates" (§2 step 3); any other
  I/O error (permission denied, transient failure) propagates instead of silently baking an
  incomplete `binaries` claim.
- **Cross-host scan gap fails closed, mode-dependent.** A host that cannot evaluate the target
  platform's executable-file convention (Unix exec-bit needs a Unix host; the Windows extension
  allowlist is pure string matching, host-independent) makes `--bin-scan` (Verify) error
  (`UnsupportedHostScan`, exit 65); Auto leaves the field undeclared (`None`, not `Some([])`)
  instead.
- **`--bin-scan` requires `--metadata`.** Verify mode needs a declaration to verify against;
  `--bin-scan` without `-m`/`--metadata` is now a usage error (exit 64), not a silent no-op.

## Documentation & Schema Surfaces to Update

- `website/src/docs/reference/metadata.md` — new `## Executables` (or
  similar) section alongside `## Entry Points`; Schema Changelog entry under
  "Version 1 — Current" (additive, no version bump).
- `website/src/docs/reference/command-line.md` — `ocx package create
  --bin-scan`/`--no-bin-scan` flag docs; `ocx env`/`ocx package env` JSON
  shape (`binaries`/`entrypoints` arrays).
- `crates/ocx_schema` — regenerate `v1.json` (`task schema:generate`) per
  `subsystem-metadata-schema.md`. `Binaries` needs a manual
  `impl schemars::JsonSchema` (custom `Deserialize`, same category as
  `Entrypoints`). The public schema describes the **write contract only**
  (`"type":"array","items":{"type":"string"},"uniqueItems":true`) — the
  read-side string|object leniency is an internal Rust affordance and never
  appears in the published schema. `BinaryElement` therefore likely needs no
  schema at all; verify against schemars 1.2.1 native untagged-derive
  behavior at WP1 before writing a second manual impl.
- `subsystem-package.md` Module Map — add `metadata/binary.rs`,
  `bin_scan.rs` rows.
- `subsystem-metadata-schema.md` — add `Binaries` to its "Custom
  `JsonSchema` Implementations" list (protocol: every manual impl is
  documented there); add `BinaryElement` only if WP1's schemars-1.2.1
  verification finds a manual impl is actually needed.
- `subsystem-package-manager.md` — `composer.rs` row (admitted_binaries/
  admitted_entrypoints), `resolve.rs` row (`AdmittedBinaries`).
- `subsystem-cli-commands.md` — `package create` Key Flags column
  (`--bin-scan`), `env`/`package env` output shape note.
