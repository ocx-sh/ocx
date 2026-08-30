// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx.toml` schema: the developer-editable project-tier declaration.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use super::env::ProjectEnv;
use super::error::{ProjectError, ProjectErrorKind};
use crate::lazy::{LazyMode, LazyModeLadder, LazyReport};
use crate::oci::Identifier;
use crate::oci::identifier::error::{IdentifierError, IdentifierErrorKind};

/// A named group's body: `[group.<name>.tools]` and `[group.<name>.env]`.
///
/// Exactly two optional sub-tables, nothing else. A tool binding written
/// directly under `[group.<name>]` is a parse error
/// ([`ProjectErrorKind::GroupHoldsDirectBinding`]) — one place bindings can
/// live, so nothing merges and nothing can collide across spellings.
///
/// A tool literally named `env` or `tools` needs no special handling: it is
/// just a key inside [`Self::tools`], which is a plain map.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Group {
    /// Tool bindings for this group. Values are fully-qualified
    /// [`Identifier`]s, validated on the second parse pass exactly as
    /// `[tools]` is.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<String, Identifier>,

    /// Environment variables applied when this group is selected.
    ///
    /// Skipped when empty so `ocx add -g ci` never writes a bare
    /// `[group.ci.env]` into a file that declares none.
    #[serde(default, skip_serializing_if = "ProjectEnv::is_empty")]
    pub env: ProjectEnv,

    /// Group-tier override of the `lazy-mode` resolution ladder
    /// (`plan_lazy_package_loading.md` C-006). `None` means inherit from the
    /// toolchain tier, never [`LazyMode::Never`]. Excluded from
    /// [`super::declaration_hash`] — resolve-time policy, not a tool
    /// binding.
    #[serde(default, rename = "lazy-mode", skip_serializing_if = "Option::is_none")]
    pub lazy_mode: Option<LazyMode>,
    // There is deliberately no group-tier `lazy-report`. `lazy-mode` is decided
    // while composing, where the selected group is known; `lazy-report` is
    // decided inside `ocx launcher shim`, a separate process that receives a
    // pinned identifier and a basename and has no way to learn which group
    // composed the tool. A settable-but-unreadable tier is the defect
    // `plan_lazy_package_loading.md` C-006 (i) exists to prevent, so the field
    // is absent rather than ignored. See [`PackageSettings::lazy_report`] and
    // [`ProjectConfig::lazy_report`] for the tiers that do apply.
}

/// Per-package resolve-time settings declared in `ocx.toml` under
/// `[package."<registry/repo[:tag]>"]`.
///
/// Currently carries a single opt-out: `no-patches = true` declines the
/// site-tier companion overlay for that base — EXCEPT a system-required patch
/// still applies (enforcement beats opt-out, C7). The opt-out is
/// version-independent: it keys on canonical `registry/repository` and applies
/// to every installed version.
///
/// `#[serde(deny_unknown_fields)]` so a typo in a `[package.*]` key's body
/// surfaces as a parse error rather than a silent no-op.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PackageSettings {
    /// Decline the site-tier companion overlay for this base.
    ///
    /// A system-required patch still applies (enforcement beats opt-out).
    #[serde(default, rename = "no-patches")]
    pub no_patches: bool,

    /// Package-tier override of the `lazy-mode` resolution ladder
    /// (`plan_lazy_package_loading.md` C-006) — the most specific config
    /// tier, only outranked by the CLI flag. `None` means inherit from the
    /// group/toolchain tiers, never [`LazyMode::Never`]. Excluded from
    /// [`super::declaration_hash`] — resolve-time policy, not a tool
    /// binding.
    #[serde(default, rename = "lazy-mode", skip_serializing_if = "Option::is_none")]
    pub lazy_mode: Option<LazyMode>,

    /// Package-tier override of the `lazy-report` setting. `None` means
    /// inherit from the group/toolchain tiers. Excluded from
    /// [`super::declaration_hash`].
    #[serde(default, rename = "lazy-report", skip_serializing_if = "Option::is_none")]
    pub lazy_report: Option<LazyReport>,
}

/// Project-tier configuration parsed from `ocx.toml`.
///
/// Schema follows ADR "Project-Level Toolchain Config" decision 1A:
/// flat `[tools]` table as the implicit default group, plus additive
/// `[group.<name>]` tables for optional named groups. Values are
/// registry-qualified [`Identifier`] strings of the form
/// `registry/repo[:tag][@digest]`. Bare-tag forms (no registry, e.g.
/// `cmake = "3.28"`) are rejected with
/// [`super::error::ProjectErrorKind::ToolValueMissingRegistry`].
///
/// Bare-repo entries with no tag and no digest (e.g.
/// `cmake = "ocx.sh/cmake"`) parse with `:latest` injected at the
/// schema boundary — see [`parse_tool_value`] for the contract. The
/// default does not apply to digest-pinned entries
/// (`tool = "ghcr.io/acme/tool@sha256:..."`); the digest is the
/// canonical pin.
///
/// `#[serde(deny_unknown_fields)]` is enforced at the struct level so
/// schema drift in consumer `ocx.toml` files surfaces as a parse error
/// rather than silent ignore.
///
/// Phase 2.1 NOTE: the `platforms` field is removed in this revision.
/// The effective platform set is sourced ambient from the project tier
/// (currently the canonical five-platform set) until ADR-driven
/// per-tool platform overrides land.
#[derive(Debug, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    /// Tools in the reserved `default` group (the top-level `[tools]`
    /// table in `ocx.toml`). Values are fully-qualified
    /// [`Identifier`]s.
    #[serde(default)]
    pub tools: BTreeMap<String, Identifier>,

    /// Environment variables for the default group; `[env]` in TOML.
    ///
    /// Standing in the same relation to `[group.<name>.env]` that `[tools]`
    /// does to `[group.<name>.tools]`. Excluded from
    /// [`super::declaration_hash`] — it does not change *which* packages
    /// resolve, so an env edit must not force a re-lock.
    #[serde(default, skip_serializing_if = "ProjectEnv::is_empty")]
    pub env: ProjectEnv,

    /// Named additive groups; `[group.<name>]` in TOML. `default` is
    /// reserved — a literal `[group.default]` declaration is a parse
    /// error (enforced at parse time, not at the serde layer).
    ///
    /// The Rust field is plural, the TOML table singular: `[group.<name>]`
    /// is what users write. Do not rename the wire key.
    #[serde(default, rename = "group")]
    pub groups: BTreeMap<String, Group>,

    /// Per-package resolve-time settings; `[package."<id>"]` in TOML. Keyed by
    /// the canonical author string (registry/repo[:tag]) so Serialize
    /// round-trips byte-faithfully. Currently carries the `no-patches` opt-out.
    ///
    /// This is RESOLVE-TIME POLICY, not a tool-binding declaration: it is
    /// deliberately excluded from [`super::declaration_hash`] (a `no-patches`
    /// edit must not invalidate `ocx.lock`).
    #[serde(default, rename = "package")]
    pub packages: BTreeMap<String, PackageSettings>,

    /// Toolchain-tier `lazy-mode` (`plan_lazy_package_loading.md` C-006) —
    /// the least specific tier of the resolution ladder, only outranked by
    /// `[group.<g>]`, `[package."<id>"]`, and the CLI flag. `None` means
    /// fall through to `OCX_LAZY_MODE`, never [`LazyMode::Never`] — only the
    /// ladder's own floor applies that default.
    ///
    /// The FIRST bare scalar fields on this struct — every other field is a
    /// collection. This is RESOLVE-TIME POLICY, not a tool-binding
    /// declaration: deliberately excluded from [`super::declaration_hash`]
    /// (same rationale as `env` and `packages`).
    #[serde(default, rename = "lazy-mode", skip_serializing_if = "Option::is_none")]
    pub lazy_mode: Option<LazyMode>,

    /// Toolchain-tier `lazy-report`. `None` means fall through to the
    /// built-in default. Excluded from [`super::declaration_hash`].
    #[serde(default, rename = "lazy-report", skip_serializing_if = "Option::is_none")]
    pub lazy_report: Option<LazyReport>,
    /// Identity-pinned verification policies; `[[trust.policy]]` in TOML.
    ///
    /// Like [`Self::packages`], this is resolve-time policy — NOT a tool
    /// binding — so it is excluded from [`super::declaration_hash`] (a policy
    /// edit must not invalidate `ocx.lock`). Consumed by `ocx package verify`,
    /// where it pools (array-append) with the `config.toml` tiers. See
    /// `crate::trust`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust: Option<crate::trust::TrustConfig>,

    /// Lazily-cached canonical declaration hash (RFC 8785 JCS + SHA-256).
    ///
    /// Populated on first call to [`Self::declaration_hash_cached`]. Mutators
    /// in [`crate::project::mutate`] / [`crate::project::mutation`] that
    /// modify `tools` / `groups` in place must call
    /// [`Self::invalidate_declaration_hash_cache`] (or replace the whole
    /// `ProjectConfig` from a fresh disk load) to keep the cache coherent.
    ///
    /// `OnceLock` (not `OnceCell`) so the type stays `Send + Sync` —
    /// `resolve_lock` clones the `Index` but borrows the config from the
    /// surrounding scope; future call sites that move `&ProjectConfig` into
    /// async tasks do not need a manual `Sync` audit.
    ///
    /// Excluded from `PartialEq` / `Eq` / `Serialize` / `Deserialize` /
    /// `JsonSchema` — those traits speak to the on-disk identity of the
    /// config, not its runtime cache state.
    #[serde(skip)]
    #[schemars(skip)]
    declaration_hash_cache: OnceLock<String>,
}

// Manual impls below keep the cache out of equality / cloning semantics.

impl Clone for ProjectConfig {
    fn clone(&self) -> Self {
        // Fresh `OnceLock` on clone: cloning a `ProjectConfig` (e.g. for
        // staging a candidate config in `MutationGuard::stage`) means the
        // clone may be mutated independently. Sharing the cached hash with
        // the original would silently leak the original's hash through to
        // the mutated clone, defeating the whole point of the gate. The
        // recompute on first access against the clone is what the cache is
        // designed to amortise.
        Self {
            tools: self.tools.clone(),
            env: self.env.clone(),
            groups: self.groups.clone(),
            packages: self.packages.clone(),
            lazy_mode: self.lazy_mode,
            lazy_report: self.lazy_report,
            trust: self.trust.clone(),
            declaration_hash_cache: OnceLock::new(),
        }
    }
}

impl PartialEq for ProjectConfig {
    fn eq(&self, other: &Self) -> bool {
        // The cache is a derived datum; comparing it would conflate "same
        // declaration" with "both cached" / "neither cached". Equality
        // speaks to the declared content only.
        self.tools == other.tools
            && self.env == other.env
            && self.groups == other.groups
            && self.packages == other.packages
            && self.lazy_mode == other.lazy_mode
            && self.lazy_report == other.lazy_report
            && self.trust == other.trust
    }
}

impl Eq for ProjectConfig {}

/// Raw on-disk shape used as the first deserialization pass.
///
/// Step 2 walks this and validates each value with [`Identifier::parse`]
/// (strict — no `OCX_DEFAULT_REGISTRY` fallback), mapping
/// [`crate::oci::identifier::error::IdentifierErrorKind::MissingRegistry`]
/// to [`super::error::ProjectErrorKind::ToolValueMissingRegistry`] and
/// other identifier failures to
/// [`super::error::ProjectErrorKind::ToolValueInvalid`]. Two-pass form
/// is required so the diagnostic carries both the binding name (map
/// key) and the offending value (map value); a value-position visitor
/// alone can't access the key.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProjectConfig {
    #[serde(default)]
    tools: BTreeMap<String, String>,

    /// Raw `[env]` table. Held as a [`toml::Table`] rather than a
    /// [`ProjectEnv`] so [`parse_project_env`] can attach the scope string
    /// every env diagnostic needs — a value-position deserializer cannot see
    /// which table it is inside.
    #[serde(default)]
    env: toml::Table,

    /// Raw `[group.*]` bodies, each still an untyped table.
    ///
    /// Value-first, following the [`crate::config::mirror`] precedent: a
    /// typed struct with `deny_unknown_fields` would reject a stray key, but
    /// serde's message cannot name the enclosing group (it is the outer map
    /// key, invisible from inside the value) and so cannot carry the
    /// migration instruction. [`parse_group`] walks the table with the group
    /// name in hand instead.
    #[serde(default, rename = "group")]
    groups: BTreeMap<String, toml::Table>,

    /// Per-package settings. `PackageSettings` deserializes directly — the
    /// map KEY is validated as a strict [`Identifier`] in
    /// [`ProjectConfig::from_str_with_path`], not at the serde layer.
    #[serde(default, rename = "package")]
    package: BTreeMap<String, PackageSettings>,

    /// Toolchain-tier `lazy-mode`. `LazyMode`'s own `Deserialize` already
    /// rejects an unrecognized wire value, so — unlike `tools` — no second
    /// validation pass is needed; [`ProjectConfig::from_str_with_path`]
    /// copies this straight through.
    #[serde(default, rename = "lazy-mode")]
    lazy_mode: Option<LazyMode>,

    /// Toolchain-tier `lazy-report`. Same direct-deserialize rationale as
    /// [`Self::lazy_mode`].
    #[serde(default, rename = "lazy-report")]
    lazy_report: Option<LazyReport>,
    /// Trust policies (`[[trust.policy]]`). Parsed directly (no per-entry
    /// identifier validation — the scope is a prefix pattern, not an
    /// [`Identifier`]).
    #[serde(default)]
    trust: Option<crate::trust::TrustConfig>,

    /// `[shell]`, declared only to be refused by name.
    ///
    /// Held as an untyped [`toml::Table`] so the refusal fires on the section's
    /// presence rather than on its contents — a `[shell]` block that happens to
    /// be well-formed for `config.toml` is no more acceptable here than a
    /// malformed one. Without this field the section would still be rejected,
    /// but by `deny_unknown_fields`, whose message reads as "unknown key" and
    /// names no remedy.
    #[serde(default)]
    shell: Option<toml::Table>,
}

impl ProjectConfig {
    /// Constructor for in-test fixtures and programmatic construction sites
    /// that need to bypass the TOML round-trip in `from_toml_str`. Initialises
    /// the private declaration-hash cache as empty so the first call to
    /// [`Self::declaration_hash_cached`] computes the canonical value.
    ///
    /// Takes groups as plain tool maps rather than [`Group`] values: this
    /// constructor exists for the tool dimension, and wrapping here keeps
    /// every fixture that predates `[group.<name>.env]` compiling unchanged
    /// — which is also what pins the frozen declaration-hash corpus.
    /// Fixtures needing group env parse TOML through [`Self::from_toml_str`].
    pub fn from_parts(
        tools: BTreeMap<String, Identifier>,
        groups: BTreeMap<String, BTreeMap<String, Identifier>>,
    ) -> Self {
        Self {
            tools,
            env: ProjectEnv::default(),
            groups: groups
                .into_iter()
                .map(|(name, tools)| {
                    (
                        name,
                        Group {
                            tools,
                            env: ProjectEnv::default(),
                            lazy_mode: None,
                        },
                    )
                })
                .collect(),
            // `packages` is resolve-time policy, not a tool binding; the
            // programmatic constructor starts with no opt-outs declared.
            packages: BTreeMap::new(),
            // Toolchain-tier lazy config is likewise resolve-time policy;
            // the programmatic constructor starts with nothing declared.
            lazy_mode: None,
            lazy_report: None,
            trust: None,
            declaration_hash_cache: OnceLock::new(),
        }
    }

    /// The trust policies declared in this project's `ocx.toml` (empty when no
    /// `[trust]` section is present).
    #[must_use]
    pub fn trust_policies(&self) -> &[crate::trust::TrustPolicy] {
        self.trust.as_ref().map_or(&[], |trust| trust.policy.as_slice())
    }

    /// Lazily cached canonical declaration hash for this config.
    ///
    /// First call computes the hash via [`super::declaration_hash`] (RFC 8785
    /// JCS canonicalization + SHA-256). Subsequent calls return the cached
    /// `&str` for free. Mutators that change `tools` / `groups` in place must
    /// call [`Self::invalidate_declaration_hash_cache`] to keep the cache
    /// coherent; rebuilding a fresh `ProjectConfig` from disk also resets
    /// the cache (a fresh `Default::default()` `OnceLock` is empty by
    /// construction).
    pub fn declaration_hash_cached(&self) -> &str {
        self.declaration_hash_cache
            .get_or_init(|| super::declaration_hash(self))
    }

    /// Drop any cached declaration hash so the next call to
    /// [`Self::declaration_hash_cached`] recomputes from current state.
    ///
    /// Mutators that modify `tools` / `groups` in place (e.g.
    /// `mutate::add_binding_in_memory`, `mutate::remove_binding_in_memory`,
    /// `MutationGuard::stage`'s closure) MUST call this after the change
    /// or the staleness gate will compare the lock's hash against the
    /// pre-mutation cached hash and silently accept a divergent state.
    ///
    /// `&mut self` is sufficient because `OnceLock` provides interior-
    /// mutability methods (`take`) that move the cached value out without
    /// requiring outer ownership.
    pub fn invalidate_declaration_hash_cache(&mut self) {
        self.declaration_hash_cache.take();
    }

    /// Set of canonical `"registry/repository"` strings for every
    /// `[package."<id>"]` entry whose `no-patches == true`.
    ///
    /// Tag/digest are EXCLUDED: the opt-out is version-independent — opting a
    /// package out applies to every installed version. Keys were validated as
    /// fully-qualified [`Identifier`]s at parse time, so `Identifier::parse`
    /// here cannot fail for a well-formed config; a key that somehow fails to
    /// re-parse is silently skipped (it could not match a base anyway).
    ///
    /// Consumed by the resolver's site-patch boundary
    /// (`build_site_patch_set`) to skip the companion overlay for opted-out
    /// bases — EXCEPT a system-required patch, which still applies.
    pub fn no_patches_repositories(&self) -> std::collections::BTreeSet<String> {
        self.packages
            .iter()
            .filter(|(_, settings)| settings.no_patches)
            .filter_map(|(key, _)| Identifier::parse(key).ok().map(|id| repository_key(&id)))
            .collect()
    }

    /// The `[package."<id>"]` settings for `identifier`, or `None` when no
    /// entry names its repository.
    ///
    /// Matched on `registry/repository` with tag and digest EXCLUDED, the same
    /// version-independent rule [`Self::no_patches_repositories`] follows — and
    /// the only rule the wire permits for a shim: the invoked shim carries one
    /// pinned identifier and the config author cannot know its digest.
    ///
    /// Consumed by `ocx launcher shim` for the `lazy-report` ladder's package
    /// tier. A key that fails to re-parse is skipped; keys were validated as
    /// fully-qualified [`Identifier`]s at parse time, and one that somehow
    /// fails here could not match a package anyway.
    pub fn package_settings(&self, identifier: &Identifier) -> Option<&PackageSettings> {
        let wanted = repository_key(identifier);
        self.packages
            .iter()
            .find(|(key, _)| Identifier::parse(key).is_ok_and(|parsed| repository_key(&parsed) == wanted))
            .map(|(_, settings)| settings)
    }

    /// Resolve the project-tier `ocx.toml` and adjacent lock paths.
    ///
    /// Precedence: `--global`/`OCX_GLOBAL` (exclusive with `--project`) >
    /// explicit `--project` > `OCX_PROJECT` > CWD walk > **None**. There
    /// is no implicit `$OCX_HOME/ocx.toml` fallback — the global toolchain
    /// is reachable *only* via the explicit `global` selector, never
    /// discovered implicitly (adr_global_toolchain_tier.md §Decision 1).
    /// Returns `None` when no source produces a path or `OCX_NO_PROJECT=1`
    /// prunes discovery.
    ///
    /// When `global` is set, the in-effect project file is
    /// `<ocx_home>/ocx.toml` with its sibling `<ocx_home>/ocx.lock`,
    /// bypassing the CWD walk entirely (peer to the explicit `--project`
    /// branch). `ocx_home` is the caller's `$OCX_HOME` root.
    ///
    /// Lock path is derived via [`super::lock::lock_path_for`] as
    /// `<parent>/ocx.lock`, independent of the config file's extension.
    ///
    /// # Errors
    /// Propagates [`crate::config::error::Error`] from the underlying
    /// loader: `FileNotFound` (exit 79) when an explicit source names a
    /// missing file, `Io` (exit 74) for other I/O failures.
    pub async fn resolve(
        cwd: Option<&Path>,
        explicit: Option<&Path>,
        ocx_home: Option<&Path>,
        global: bool,
    ) -> std::result::Result<Option<(PathBuf, PathBuf)>, crate::config::error::Error> {
        // Global selector: explicit, exclusive with `--project` (clap
        // `conflicts_with` enforces the exclusion at parse time). Selects
        // `<ocx_home>/ocx.toml` directly and bypasses the CWD walk. This
        // branch is a peer of the explicit `--project` branch — never an
        // implicit fallback (adr_global_toolchain_tier.md §Decision 1/2).
        if global {
            // Peer of the explicit `--project` branch: select
            // `<ocx_home>/ocx.toml` directly and bypass the CWD walk.
            // `ocx_home` is the caller's `$OCX_HOME` root (plumbed by
            // every project-tier prologue). Absence of an `ocx_home`
            // (no `$OCX_HOME`, no home dir) is a hard config error —
            // `--global` cannot name a file without a root.
            let home = ocx_home.ok_or_else(|| crate::config::error::Error::FileNotFound {
                path: PathBuf::from("ocx.toml"),
                tier: crate::config::error::ConfigSource::Project,
            })?;
            let config_path = home.join("ocx.toml");
            let lock = super::lock::lock_path_for(&config_path);
            return Ok(Some((config_path, lock)));
        }

        // Steps 1-3: delegate to ConfigLoader (explicit flag > env > CWD
        // walk). No home-tier fallback: a CWD-walk miss is a hard `None`.
        let walk_result = crate::config::loader::ConfigLoader::project_path(cwd, explicit).await?;

        if let Some(p) = walk_result {
            let lock = super::lock::lock_path_for(&p);
            return Ok(Some((p, lock)));
        }

        Ok(None)
    }

    /// Parse a [`ProjectConfig`] from a TOML string.
    ///
    /// Validates that `[group.default]` is not declared (reserved name).
    /// Validates that every value parses as a fully-qualified [`Identifier`]
    /// — bare-tag forms are rejected with
    /// [`super::error::ProjectErrorKind::ToolValueMissingRegistry`].
    ///
    /// Same-name bindings across different groups (e.g. `cmake` in both
    /// `[tools]` and `[group.ci]`) are allowed at parse time; the runtime
    /// conflict check fires at compose time via
    /// [`super::error::ProjectErrorKind::DuplicateToolAcrossSelectedGroups`].
    pub fn from_toml_str(s: &str) -> Result<Self, super::Error> {
        Self::from_str_with_path(s, PathBuf::new())
    }

    /// Parse a [`ProjectConfig`] from pre-read bytes attributed to `path`.
    ///
    /// Used by callers that already hold the file open (e.g.
    /// `load_project_for_mutate`, which reads through its exclusive
    /// `LockedFile` handle to avoid the Windows F1 (`ERROR_LOCK_VIOLATION`)
    /// that a second raw open would trigger). Enforces the same 64 KiB size
    /// cap as [`Self::from_path`] and surfaces the same structured errors.
    pub fn from_toml_bytes_with_path(bytes: &[u8], path: PathBuf) -> Result<Self, super::Error> {
        let limit = super::internal::FILE_SIZE_LIMIT_BYTES;
        if bytes.len() as u64 > limit {
            return Err(ProjectError::new(
                path,
                ProjectErrorKind::FileTooLarge {
                    size: bytes.len() as u64,
                    limit,
                },
            )
            .into());
        }
        let content = std::str::from_utf8(bytes).map_err(|e| {
            ProjectError::new(
                path.clone(),
                ProjectErrorKind::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
            )
        })?;
        Self::from_str_with_path(content, path)
    }

    /// Load and parse a [`ProjectConfig`] from a filesystem path.
    ///
    /// Enforces a 64 KiB size cap (`super::internal::FILE_SIZE_LIMIT_BYTES`)
    /// before reading; oversized files surface as a structured
    /// [`super::error::ProjectErrorKind::FileTooLarge`].
    pub async fn from_path(path: &Path) -> Result<Self, super::Error> {
        use tokio::io::AsyncReadExt;
        let limit = super::internal::FILE_SIZE_LIMIT_BYTES;

        let file = tokio::fs::File::open(path)
            .await
            .map_err(|e| ProjectError::new(path.to_path_buf(), ProjectErrorKind::Io(e)))?;
        // `metadata.len()` fast-paths normal oversized files without reading
        // any bytes; the bounded `take(limit + 1)` below guards synthetic
        // files (e.g. procfs, pipes) whose metadata reports 0 but whose read
        // is unbounded. Mirrors the ambient config loader's
        // `ConfigLoader::load_and_merge` pattern.
        let metadata = file
            .metadata()
            .await
            .map_err(|e| ProjectError::new(path.to_path_buf(), ProjectErrorKind::Io(e)))?;
        if metadata.len() > limit {
            return Err(ProjectError::new(
                path.to_path_buf(),
                ProjectErrorKind::FileTooLarge {
                    size: metadata.len(),
                    limit,
                },
            )
            .into());
        }

        let mut content = String::new();
        let mut taken = file.take(limit + 1);
        taken
            .read_to_string(&mut content)
            .await
            .map_err(|e| ProjectError::new(path.to_path_buf(), ProjectErrorKind::Io(e)))?;
        if content.len() as u64 > limit {
            return Err(ProjectError::new(
                path.to_path_buf(),
                ProjectErrorKind::FileTooLarge {
                    size: content.len() as u64,
                    limit,
                },
            )
            .into());
        }
        Self::from_str_with_path(&content, path.to_path_buf())
    }

    fn from_str_with_path(s: &str, path: PathBuf) -> Result<Self, super::Error> {
        // First pass: deserialize the on-disk shape with raw string values.
        // Second pass (below) walks every entry through `Identifier::parse`
        // so the binding name (map key) and offending value (map value) can
        // both reach the diagnostic — a value-position visitor cannot see
        // the key.
        let raw: RawProjectConfig =
            toml::from_str(s).map_err(|e| ProjectError::new(path.clone(), ProjectErrorKind::TomlParse(e)))?;

        // Schema-level: `[shell]` is a `config.toml` section and is refused
        // here by name (C-033). The whitelist it carries is what gates project
        // activation, so reading it from the project tier would let a
        // checked-in file consent to itself.
        if raw.shell.is_some() {
            return Err(ProjectError::new(path, ProjectErrorKind::ShellSectionInProject).into());
        }

        // Schema-level: `[group.default]` is reserved for the implicit
        // top-level `[tools]` table. Reject before identifier validation
        // so the user sees the actionable schema error first.
        if raw.groups.contains_key(super::internal::DEFAULT_GROUP) {
            return Err(ProjectError::new(
                path,
                ProjectErrorKind::ReservedGroupName {
                    name: super::internal::DEFAULT_GROUP.to_owned(),
                    hint: "put tools in the top-level [tools] table",
                },
            )
            .into());
        }

        // Schema-level: `[group.all]` is reserved as the CLI expansion
        // keyword that selects every declared group. Rejected here before
        // identifier validation so the user sees the actionable schema error
        // first.
        if raw.groups.contains_key(super::internal::ALL_GROUP) {
            return Err(ProjectError::new(
                path,
                ProjectErrorKind::ReservedGroupName {
                    name: super::internal::ALL_GROUP.to_owned(),
                    hint: "rename this group; `all` is a reserved keyword that selects every declared group",
                },
            )
            .into());
        }

        // Per-entry identifier validation across `[tools]` and every
        // `[group.*]` table, plus the env key/value grammar per scope.
        let tools = parse_tool_map(&raw.tools, &path)?;
        let env = parse_project_env(super::env::DEFAULT_ENV_SCOPE, &raw.env, &path)?;
        let mut groups: BTreeMap<String, Group> = BTreeMap::new();
        for (group_name, group_body) in raw.groups {
            let parsed = parse_group(&group_name, &group_body, &path)?;
            groups.insert(group_name, parsed);
        }

        // Validate every `[package."<key>"]` key as a strict, fully-qualified
        // [`Identifier`] (same path as `[tools]` values: no default-registry
        // fallback). A bare key without a registry is an error. The validated
        // map is keyed by the ORIGINAL author string so Serialize round-trips
        // byte-faithfully; validation here is for early, actionable errors only.
        let packages = validate_package_keys(raw.package, &path)?;

        Ok(Self {
            tools,
            env,
            groups,
            packages,
            // `LazyMode`/`LazyReport` reject an unrecognized wire value in
            // their own `Deserialize`, so `raw`'s first pass already
            // validated these — no second pass needed, unlike `tools`.
            lazy_mode: raw.lazy_mode,
            lazy_report: raw.lazy_report,
            trust: raw.trust,
            declaration_hash_cache: OnceLock::new(),
        })
    }
}

/// Parse one `[group.<name>]` body into a [`Group`].
///
/// Walks the raw table key by key: `tools`, `env` and `lazy-mode` are the three
/// recognized keys. Among unrecognized keys, a
/// **string**-valued one is the removed flat binding form and raises
/// [`ProjectErrorKind::GroupHoldsDirectBinding`] naming the group and
/// pointing at `[group.<name>.tools]`; anything else raises
/// [`ProjectErrorKind::UnknownGroupSection`] naming the offending key.
///
/// Key-first, not value-first: `lazy-mode` is itself
/// string-valued (`lazy-mode = "always"`), so a value-shape check run before
/// the key match would misclassify it as the removed flat binding form.
/// Recognizing the key first, and reserving the string-shape check for the
/// fallback arm, keeps both diagnoses correct.
///
/// Value-first rather than a `deny_unknown_fields` derive: the group name is
/// the outer map key and is unreachable from inside a value deserializer, so
/// a derive cannot produce either diagnostic. Same reason
/// [`crate::config::mirror::parse_mirror_value`] is hand-rolled.
///
/// # Errors
///
/// [`ProjectErrorKind::GroupHoldsDirectBinding`],
/// [`ProjectErrorKind::UnknownGroupSection`], or any error from
/// [`parse_tool_map`] / [`parse_project_env`] on the recognized sub-tables.
fn parse_group(name: &str, raw: &toml::Table, path: &Path) -> Result<Group, super::Error> {
    // `[group.<name>]` with neither sub-table is a declared-but-empty group.
    if raw.is_empty() {
        return Ok(Group::default());
    }

    let mut group = Group::default();
    for (key, value) in raw {
        // Value shape decides before key name, except for the scalar
        // settings: a string under a key that names a *sub-table* is a tool
        // binding in the removed flat form whatever it is called, so
        // `tools = "ocx.sh/tools:1"` and `bar = "ocx.sh/bar:1"` both get the
        // migration message rather than a TOML type error or a bogus
        // "unknown section". `lazy-mode` is exempt because its value is
        // legitimately a string. `lazy-report` is exempt for a different
        // reason: it is a *removed* group-tier key, and routing it here would
        // tell the user to move it under `[group.<name>.tools]`, where it is
        // not a valid tool binding at all. Falling through to the `_` arm
        // reports it as the unknown key it is.
        if value.is_str() && !matches!(key.as_str(), "lazy-mode" | "lazy-report") {
            return Err(ProjectError::new(
                path.to_path_buf(),
                ProjectErrorKind::GroupHoldsDirectBinding {
                    group: name.to_string(),
                    binding: key.clone(),
                },
            )
            .into());
        }
        match key.as_str() {
            "tools" => {
                let raw_tools: BTreeMap<String, String> = value
                    .clone()
                    .try_into()
                    .map_err(|e| ProjectError::new(path.to_path_buf(), ProjectErrorKind::TomlParse(e)))?;
                group.tools = parse_tool_map(&raw_tools, path)?;
            }
            "env" => {
                let raw_env: toml::Table = value
                    .clone()
                    .try_into()
                    .map_err(|e| ProjectError::new(path.to_path_buf(), ProjectErrorKind::TomlParse(e)))?;
                group.env = parse_project_env(&format!("group.{name}.env"), &raw_env, path)?;
            }
            "lazy-mode" => {
                let mode: LazyMode = value
                    .clone()
                    .try_into()
                    .map_err(|e| ProjectError::new(path.to_path_buf(), ProjectErrorKind::TomlParse(e)))?;
                group.lazy_mode = Some(mode);
            }
            _ => {
                return Err(ProjectError::new(
                    path.to_path_buf(),
                    ProjectErrorKind::UnknownGroupSection {
                        group: name.to_string(),
                        key: key.clone(),
                    },
                )
                .into());
            }
        }
    }
    Ok(group)
}

/// Parse a raw `[env]` / `[group.<name>.env]` table, attaching `scope` to
/// every diagnostic and `path` for file context.
///
/// Thin adapter over [`super::env::ProjectEnv::from_table`] — it exists to
/// wrap the returned [`ProjectErrorKind`] in a path-bearing
/// [`ProjectError`], which is the shape every other parse helper here
/// returns.
///
/// # Errors
///
/// Propagates the key-policy and value-grammar errors documented on
/// [`super::env::ProjectEnv::from_table`].
fn parse_project_env(scope: &str, raw: &toml::Table, path: &Path) -> Result<ProjectEnv, super::Error> {
    // Every `ocx.toml` reaches here, the overwhelming majority declaring no
    // `[env]` at all. Short-circuit the absent case so the unimplemented
    // grammar below is reachable only from a file that actually declares one.
    if raw.is_empty() {
        return Ok(ProjectEnv::default());
    }
    ProjectEnv::from_table(scope, raw).map_err(|kind| ProjectError::new(path.to_path_buf(), kind).into())
}

/// Parse one `[tools]` value into the [`Identifier`] the schema boundary
/// promises.
///
/// Bare identifiers — registry + repository, no tag and no digest
/// (e.g. `"ocx.sh/cmake"`) — get `:latest` injected here so resolution always
/// has an advisory tag to look up. The default is applied at this boundary,
/// not on [`Identifier`] itself, so CLI args without a tag still surface as
/// `tag = None`. Digest-pinned entries (`@sha256:...`) keep `tag = None`; the
/// digest is the canonical pin.
///
/// Named rather than inlined into [`parse_tool_map`] because it is the only
/// definition of "what this text means": the format-preserving renderer
/// ([`super::document`]) asks it whether a line on disk already says what the
/// candidate says, and a text comparison would call the injected `:latest` a
/// change and rewrite a line the mutation never targeted.
///
/// # Errors
///
/// Propagates [`Identifier::parse`] verbatim — callers map the kinds onto
/// their own diagnostics.
pub(super) fn parse_tool_value(value: &str) -> Result<Identifier, IdentifierError> {
    let identifier = Identifier::parse(value)?;
    if identifier.tag().is_none() && identifier.digest().is_none() {
        return Ok(identifier.clone_with_tag("latest"));
    }
    Ok(identifier)
}

/// Walk a raw `(name → value)` map and validate every value as a
/// fully-qualified [`Identifier`] via [`parse_tool_value`]. Splits
/// [`IdentifierErrorKind::MissingRegistry`] from other identifier
/// failures so the project-tier diagnostic can name the offending
/// binding without losing the underlying [`IdentifierError`]
/// for non-registry failures.
fn parse_tool_map(raw: &BTreeMap<String, String>, path: &Path) -> Result<BTreeMap<String, Identifier>, super::Error> {
    let mut out: BTreeMap<String, Identifier> = BTreeMap::new();
    for (name, value) in raw {
        match parse_tool_value(value) {
            Ok(id) => {
                out.insert(name.clone(), id);
            }
            Err(e) if matches!(e.kind, IdentifierErrorKind::MissingRegistry) => {
                return Err(ProjectError::new(
                    path.to_path_buf(),
                    ProjectErrorKind::ToolValueMissingRegistry {
                        name: name.clone(),
                        value: value.clone(),
                    },
                )
                .into());
            }
            Err(e) => {
                return Err(ProjectError::new(
                    path.to_path_buf(),
                    ProjectErrorKind::ToolValueInvalid {
                        name: name.clone(),
                        value: value.clone(),
                        source: e,
                    },
                )
                .into());
            }
        }
    }
    Ok(out)
}

/// Validate every `[package."<key>"]` key as a strict, fully-qualified
/// [`Identifier`], returning the map re-keyed by the ORIGINAL author string so
/// Serialize round-trips byte-faithfully.
///
/// Mirrors [`parse_tool_map`]'s error-mapping style: a key missing a registry
/// maps to [`ProjectErrorKind::PackageKeyMissingRegistry`]; any other identifier
/// failure maps to [`ProjectErrorKind::PackageKeyInvalid`] (carrying the
/// underlying [`crate::oci::identifier::error::IdentifierError`] via `#[source]`).
/// Validation is for early, actionable errors only — the parsed identifier is
/// discarded; the original key string is retained as the map key.
fn validate_package_keys(
    raw: BTreeMap<String, PackageSettings>,
    path: &Path,
) -> Result<BTreeMap<String, PackageSettings>, super::Error> {
    for key in raw.keys() {
        match Identifier::parse(key) {
            Ok(_) => {}
            Err(e) if matches!(e.kind, IdentifierErrorKind::MissingRegistry) => {
                return Err(ProjectError::new(
                    path.to_path_buf(),
                    ProjectErrorKind::PackageKeyMissingRegistry { key: key.clone() },
                )
                .into());
            }
            Err(e) => {
                return Err(ProjectError::new(
                    path.to_path_buf(),
                    ProjectErrorKind::PackageKeyInvalid {
                        key: key.clone(),
                        source: e,
                    },
                )
                .into());
            }
        }
    }
    Ok(raw)
}

/// The version-independent key a `[package."<id>"]` entry is matched on:
/// `registry/repository`, tag and digest excluded.
///
/// One function rather than two `format!`s so the `no-patches` opt-out and the
/// `lazy-report` package tier cannot drift into matching on different things.
fn repository_key(identifier: &Identifier) -> String {
    format!("{}/{}", identifier.registry(), identifier.repository())
}

/// Fill the **project-tier** `lazy-mode` ladder for one tool (plan contract
/// C-006, [#302](https://github.com/ocx-sh/ocx/issues/302)) — the five tiers
/// `--lazy-mode` ▸ `[package."<id>"]` ▸ `[group.<g>]` ▸ toolchain ▸
/// `OCX_LAZY_MODE`, unresolved.
///
/// Every tier is an `Option`, and `None` means *inherit*, never `Never`: the
/// floor lives in [`LazyModeLadder::resolve`] and nowhere else.
///
/// `group` is the selected group the binding came from (`Origin::Group(name)`);
/// `None` for a positional package, which has no group tier. `cli` is the
/// parsed `--lazy-mode`, which no library type can see for itself — hence a
/// caller-supplied argument rather than another config read.
///
/// The package tier is matched on `registry/repository` with tag and digest
/// excluded ([`ProjectConfig::package_settings`]) — the only rule the wire
/// permits, since a config author cannot know the digest a lock pins.
///
/// Split from [`lazy_mode_for_tool`] so which-config-tier-feeds-which-slot is
/// assertable independently of the host's shim-support floor: on Windows the
/// resolved answer is `Never` for every input, so a test driving the resolving
/// form there proves nothing about this wiring — the same reason
/// [`LazyModeLadder::resolve`] is split from
/// [`LazyModeLadder::resolve_for_host`] one layer down.
pub fn lazy_mode_ladder_for_tool(
    config: &ProjectConfig,
    identifier: &Identifier,
    group: Option<&str>,
    cli: Option<LazyMode>,
) -> LazyModeLadder {
    LazyModeLadder {
        cli,
        package: config.package_settings(identifier).and_then(|s| s.lazy_mode),
        group: group.and_then(|name| config.groups.get(name)).and_then(|g| g.lazy_mode),
        toolchain: config.lazy_mode,
        environment: LazyMode::from_env(),
    }
}

/// Resolve the `lazy-mode` ladder for one **project-tier** tool — the form
/// every production caller uses.
///
/// [`lazy_mode_ladder_for_tool`] fills the tiers; resolution goes through
/// [`LazyModeLadder::resolve_for_host`], so on Windows the answer is
/// [`LazyMode::Never`] whatever the tiers say — scenario S-010.
///
/// Lives here, with the config it reads, because this contract makes
/// `ProjectConfig` the owner of tiers 2-4. The OCI-tier sibling —
/// [`lazy_mode_for_package`](crate::package_manager::composer::lazy_mode_for_package)
/// — reads no `ocx.toml` at any tier and therefore stays with its caller.
pub fn lazy_mode_for_tool(
    config: &ProjectConfig,
    identifier: &Identifier,
    group: Option<&str>,
    cli: Option<LazyMode>,
) -> LazyMode {
    lazy_mode_ladder_for_tool(config, identifier, group, cli).resolve_for_host()
}

#[cfg(test)]
mod tests {
    //! Contract-first tests for [`ProjectConfig`] parsing and resolution.
    //!
    //! These tests encode the Phase 2.1 plan contract
    //! (plan_project_toolchain.md §1 and §3). They assert on typed
    //! [`super::super::error::ProjectErrorKind`] variants, never on
    //! `Display` strings, except where message shape is the
    //! user-facing contract (the `display_chain_*` tests).
    use super::*;
    use crate::project::error::ProjectErrorKind;

    // ── declaration_hash cache coherence ────────────────────────────────────

    /// First call computes the hash; the cached `&str` is byte-identical to the
    /// stand-alone `super::super::declaration_hash(&config)` reference value so
    /// the cache cannot drift from the canonical algorithm.
    #[test]
    fn declaration_hash_cache_matches_free_function() {
        let toml = r#"[tools]
cmake = "ocx.sh/cmake:3.28"
"#;
        let config = ProjectConfig::from_toml_str(toml).expect("parse");
        let cached = config.declaration_hash_cached().to_string();
        let standalone = crate::project::declaration_hash(&config);
        assert_eq!(cached, standalone, "cached must equal the free-function output");
        // Second call returns the same cached value (cheap path).
        assert_eq!(cached, config.declaration_hash_cached());
    }

    /// C-033: `[shell]` in an `ocx.toml` is refused by its own named arm at
    /// exit 78, with a message that says where the section belongs.
    ///
    /// Asserting the **variant**, not the exit code, is what makes this test
    /// discriminating: delete the explicit arm and `deny_unknown_fields` still
    /// refuses the file at the same 78 — but as `TomlParse`, whose message
    /// reads "unknown field `shell`" and names no remedy.
    #[test]
    fn shell_section_in_ocx_toml_is_refused_by_name() {
        let err = ProjectConfig::from_toml_str(
            r#"[tools]
cmake = "ocx.sh/cmake:3.28"

[shell]
hook = true

[shell.consent]
paths = ["/anything"]
"#,
        )
        .expect_err("[shell] must not parse in ocx.toml");

        let super::super::Error::Project(pe) = &err;
        assert!(
            matches!(pe.kind, ProjectErrorKind::ShellSectionInProject),
            "expected ShellSectionInProject; got {err:?}"
        );
        assert_eq!(
            crate::cli::ClassifyExitCode::classify(&err),
            Some(crate::cli::ExitCode::ConfigError),
            "a [shell] block in a project file exits 78"
        );
        let rendered = pe.kind.to_string();
        assert!(
            rendered.contains("config.toml"),
            "the message must name where the section belongs; got {rendered:?}"
        );
    }

    /// `[[trust.policy]]` parses in an `ocx.toml` (the `deny_unknown_fields`
    /// guard must not reject it) and, like `[package]`, is resolve-time policy
    /// EXCLUDED from the declaration hash — so adding a policy never
    /// invalidates `ocx.lock`.
    #[test]
    fn trust_policy_parses_and_does_not_affect_declaration_hash() {
        let base = r#"[tools]
cmake = "ocx.sh/cmake:3.28"
"#;
        let with_policy = r#"[tools]
cmake = "ocx.sh/cmake:3.28"

[[trust.policy]]
scope = "ocx.sh/*"
signers = [
  { kind = "keyless", identity = "https://github.com/ocx-sh/ocx/.github/workflows/release.yml@refs/tags/v1", oidc_issuer = "https://token.actions.githubusercontent.com" },
]
"#;
        let base_config = ProjectConfig::from_toml_str(base).expect("parse base");
        let policy_config = ProjectConfig::from_toml_str(with_policy).expect("parse with policy");
        assert_eq!(policy_config.trust_policies().len(), 1);
        assert!(
            !policy_config.trust_policies()[0].signers.is_empty(),
            "the `signers` array must parse through the deny_unknown_fields ocx.toml root",
        );
        assert!(base_config.trust_policies().is_empty());
        assert_eq!(
            crate::project::declaration_hash(&base_config),
            crate::project::declaration_hash(&policy_config),
            "trust policy must not perturb the lock-invalidating declaration hash",
        );
    }

    // ── [package] per-package settings ──────────────────────────────────────

    /// A `[package."<id>"]` block with `no-patches = true` parses, and the
    /// accessor returns the canonical `registry/repository` (tag excluded).
    #[test]
    fn parse_package_no_patches_and_accessor_strips_tag() {
        let toml = r#"[tools]
cmake = "ocx.sh/cmake:3.28"

[package."ghcr.io/acme/cli:v1"]
no-patches = true
"#;
        let config = ProjectConfig::from_toml_str(toml).expect("[package] block parses");
        let opted_out = config.no_patches_repositories();
        assert!(
            opted_out.contains("ghcr.io/acme/cli"),
            "accessor must yield canonical registry/repository (tag stripped); got: {opted_out:?}"
        );
        assert_eq!(opted_out.len(), 1);
    }

    /// `[package."<id>"]` with `no-patches = false` (or absent) does NOT add the
    /// base to the opt-out set.
    #[test]
    fn parse_package_no_patches_false_is_not_opted_out() {
        let toml = r#"[package."ghcr.io/acme/cli:v1"]
no-patches = false
"#;
        let config = ProjectConfig::from_toml_str(toml).expect("parses");
        assert!(
            config.no_patches_repositories().is_empty(),
            "no-patches=false must not opt the base out"
        );
    }

    /// A `[package."<key>"]` key without a registry is rejected with
    /// `PackageKeyMissingRegistry` (same rule as `[tools]` values).
    #[test]
    fn parse_package_bare_key_rejected_missing_registry() {
        let toml = r#"[package."cmake"]
no-patches = true
"#;
        let err = ProjectConfig::from_toml_str(toml).expect_err("bare package key must reject");
        #[allow(irrefutable_let_patterns)]
        let crate::project::Error::Project(pe) = err else {
            panic!("expected Error::Project");
        };
        assert!(
            matches!(&pe.kind, ProjectErrorKind::PackageKeyMissingRegistry { key } if key == "cmake"),
            "expected PackageKeyMissingRegistry {{ key: \"cmake\" }}; got {:?}",
            pe.kind
        );
    }

    /// An unknown field inside a `[package."<id>"]` body is rejected
    /// (`deny_unknown_fields` on `PackageSettings`).
    #[test]
    fn parse_package_unknown_field_rejected() {
        let toml = r#"[package."ghcr.io/acme/cli:v1"]
no-patches = true
bogus = "x"
"#;
        let err = ProjectConfig::from_toml_str(toml).expect_err("unknown [package] field must reject");
        #[allow(irrefutable_let_patterns)]
        let crate::project::Error::Project(pe) = err else {
            panic!("expected Error::Project");
        };
        assert!(
            matches!(&pe.kind, ProjectErrorKind::TomlParse(_)),
            "expected TomlParse; got {:?}",
            pe.kind
        );
    }

    /// CHARACTERIZATION: adding a `[package]` opt-out must NOT change the
    /// declaration hash. The `packages` field is resolve-time policy, not a
    /// tool-binding declaration — a `no-patches` edit must not invalidate
    /// `ocx.lock` via the staleness gate.
    #[test]
    fn declaration_hash_unchanged_by_no_patches() {
        let without = r#"[tools]
cmake = "ocx.sh/cmake:3.28"
"#;
        let with = r#"[tools]
cmake = "ocx.sh/cmake:3.28"

[package."ghcr.io/acme/cli:v1"]
no-patches = true
"#;
        let config_without = ProjectConfig::from_toml_str(without).expect("parse");
        let config_with = ProjectConfig::from_toml_str(with).expect("parse");
        assert_eq!(
            crate::project::declaration_hash(&config_without),
            crate::project::declaration_hash(&config_with),
            "declaration hash must be invariant to [package] no-patches edits"
        );
    }

    /// H1: adding an `[env]` block must NOT change the declaration hash.
    /// Mirrors [`declaration_hash_unchanged_by_no_patches`] exactly in shape
    /// — deliberately: [`super::super::hash::declaration_hash`] reads only
    /// `config.tools` / `config.groups`, so `[env]` is excluded from the
    /// canonical JSON input by construction today. This test exists to stop
    /// a future refactor from wiring `[env]` into the hash by accident, not
    /// because the current algorithm could plausibly include it — an `[env]`
    /// edit must never force a re-lock.
    #[test]
    fn declaration_hash_unchanged_by_env() {
        let without = r#"[tools]
cmake = "ocx.sh/cmake:3.28"
"#;
        let with = r#"[tools]
cmake = "ocx.sh/cmake:3.28"

[env]
CI = "1"
"#;
        let config_without = ProjectConfig::from_toml_str(without).expect("parse");
        let config_with = ProjectConfig::from_toml_str(with).expect("parse");
        assert_eq!(
            crate::project::declaration_hash(&config_without),
            crate::project::declaration_hash(&config_with),
            "declaration hash must be invariant to [env] edits"
        );
    }

    /// The group-scoped counterpart of the above. `hash.rs` walks `group.tools`
    /// specifically so a `[group.<name>.env]` edit cannot stale `ocx.lock` — env
    /// does not change WHICH packages resolve. Pinned separately because the
    /// top-level `[env]` case would still pass if a refactor wired only the
    /// group arm into the hash.
    #[test]
    fn declaration_hash_unchanged_by_group_env() {
        let without = r#"[group.ci.tools]
cmake = "ocx.sh/cmake:3.28"
"#;
        let with = r#"[group.ci.tools]
cmake = "ocx.sh/cmake:3.28"

[group.ci.env]
SOURCE_DATE_EPOCH = "0"
"#;
        let config_without = ProjectConfig::from_toml_str(without).expect("parse");
        let config_with = ProjectConfig::from_toml_str(with).expect("parse");
        assert_eq!(
            crate::project::declaration_hash(&config_without),
            crate::project::declaration_hash(&config_with),
            "declaration hash must be invariant to [group.<name>.env] edits"
        );
    }

    /// C-006: `lazy-mode` is excluded from the declaration hash at EVERY tier.
    ///
    /// The exclusion is by omission — [`super::super::hash::declaration_hash`]
    /// reads only `config.tools` / `group.tools`, and nothing annotates these
    /// fields as skipped. This test is therefore the *only* thing standing
    /// between a future edit of `hash.rs` and a silent re-lock storm: laziness
    /// is resolve-time policy, so toggling it must never invalidate
    /// `ocx.lock`.
    ///
    /// The two configs differ only in the three `lazy-mode` declarations, and
    /// the parse assertions below keep the green honest — without them a
    /// regression that dropped the keys on the floor would satisfy the hash
    /// assertion trivially.
    #[test]
    fn declaration_hash_unchanged_by_lazy_mode() {
        let without = r#"[tools]
cmake = "ocx.sh/cmake:3.28"

[group.ci.tools]
shellcheck = "ocx.sh/shellcheck:0.10"

[package."ghcr.io/acme/cli:v1"]
no-patches = true
"#;
        let with = r#"lazy-mode = "always"

[tools]
cmake = "ocx.sh/cmake:3.28"

[group.ci]
lazy-mode = "always"

[group.ci.tools]
shellcheck = "ocx.sh/shellcheck:0.10"

[package."ghcr.io/acme/cli:v1"]
no-patches = true
lazy-mode = "always"
"#;
        let config_without = ProjectConfig::from_toml_str(without).expect("parse");
        let config_with = ProjectConfig::from_toml_str(with).expect("parse");

        assert_eq!(config_with.lazy_mode, Some(LazyMode::Always), "toolchain tier declared");
        assert_eq!(
            config_with.groups.get("ci").expect("group ci").lazy_mode,
            Some(LazyMode::Always),
            "group tier declared"
        );
        assert_eq!(
            config_with
                .packages
                .get("ghcr.io/acme/cli:v1")
                .expect("package entry")
                .lazy_mode,
            Some(LazyMode::Always),
            "package tier declared"
        );

        assert_eq!(
            crate::project::declaration_hash(&config_without),
            crate::project::declaration_hash(&config_with),
            "declaration hash must be invariant to lazy-mode at every tier"
        );
    }

    /// The `lazy-report` half of [`declaration_hash_unchanged_by_lazy_mode`].
    /// Pinned separately: a refactor that wired only one of the two keys into
    /// the hash would leave the other test green.
    #[test]
    fn declaration_hash_unchanged_by_lazy_report() {
        let without = r#"[tools]
cmake = "ocx.sh/cmake:3.28"

[group.ci.tools]
shellcheck = "ocx.sh/shellcheck:0.10"

[package."ghcr.io/acme/cli:v1"]
no-patches = true
"#;
        let with = r#"lazy-report = "progress"

[tools]
cmake = "ocx.sh/cmake:3.28"

[group.ci.tools]
shellcheck = "ocx.sh/shellcheck:0.10"

[package."ghcr.io/acme/cli:v1"]
no-patches = true
lazy-report = "progress"
"#;
        let config_without = ProjectConfig::from_toml_str(without).expect("parse");
        let config_with = ProjectConfig::from_toml_str(with).expect("parse");

        assert_eq!(
            config_with.lazy_report,
            Some(LazyReport::Progress),
            "toolchain tier declared"
        );
        assert_eq!(
            config_with
                .packages
                .get("ghcr.io/acme/cli:v1")
                .expect("package entry")
                .lazy_report,
            Some(LazyReport::Progress),
            "package tier declared"
        );

        assert_eq!(
            crate::project::declaration_hash(&config_without),
            crate::project::declaration_hash(&config_with),
            "declaration hash must be invariant to lazy-report at both its tiers"
        );
    }

    /// Mutating the config in place after caching MUST invalidate the cache —
    /// otherwise the staleness gate would silently accept a divergent state.
    #[test]
    fn declaration_hash_cache_invalidated_on_mutation() {
        let toml = r#"[tools]
cmake = "ocx.sh/cmake:3.28"
"#;
        let mut config = ProjectConfig::from_toml_str(toml).expect("parse");
        let before = config.declaration_hash_cached().to_string();

        // Mutate the config in place AND drop the cache. This is the contract
        // every in-place mutator must honour (`add_binding_in_memory`,
        // `remove_binding_in_memory`).
        config.tools.insert(
            "ninja".to_string(),
            Identifier::parse("ocx.sh/ninja:1.11").expect("valid"),
        );
        config.invalidate_declaration_hash_cache();

        let after = config.declaration_hash_cached();
        assert_ne!(before, after, "cache must reflect mutation after invalidation");
    }

    /// `Clone` must produce a fresh empty cache: the clone may be mutated
    /// independently of the original, and sharing the cached hash would leak
    /// the original's hash through to the divergent clone.
    #[test]
    fn declaration_hash_cache_resets_on_clone() {
        let toml = r#"[tools]
cmake = "ocx.sh/cmake:3.28"
"#;
        let config = ProjectConfig::from_toml_str(toml).expect("parse");
        let original_hash = config.declaration_hash_cached().to_string();

        let mut cloned = config.clone();
        cloned.tools.insert(
            "ninja".to_string(),
            Identifier::parse("ocx.sh/ninja:1.11").expect("valid"),
        );
        // No invalidate call — the clone's cache started empty so the next
        // `declaration_hash_cached` must see the mutated state.
        let cloned_hash = cloned.declaration_hash_cached();
        assert_ne!(
            original_hash, cloned_hash,
            "cloned config's first cache fill must reflect mutation"
        );
    }

    /// Helper: assert the error is a `Project` variant whose kind matches
    /// the provided pattern. Uses `let else` (not exhaustive `match`)
    /// because [`ProjectErrorKind`] is `#[non_exhaustive]`: an
    /// exhaustive match breaks the moment a new variant lands, producing
    /// a confusing "non-exhaustive patterns" error from the test macro
    /// rather than the actual test failure. `let else` surfaces the real
    /// mismatch directly.
    #[allow(irrefutable_let_patterns)]
    macro_rules! assert_kind {
        ($err:expr, $pat:pat) => {{
            let err = $err;
            #[allow(irrefutable_let_patterns)]
            let crate::project::Error::Project(pe) = err else {
                panic!("expected Error::Project, got {err:?}");
            };
            let kind = &pe.kind;
            let $pat = kind else {
                panic!("unexpected error kind: {kind:?}");
            };
        }};
    }

    #[test]
    fn parse_minimal_config_ok() {
        let toml_str = r#"
[tools]
cmake = "ocx.sh/cmake:3.28"
"#;
        let config = ProjectConfig::from_toml_str(toml_str).expect("minimal config parses");
        assert_eq!(config.tools.len(), 1);
        let cmake = config.tools.get("cmake").expect("cmake binding present");
        assert_eq!(cmake.to_string(), "ocx.sh/cmake:3.28");
        assert!(config.groups.is_empty());
    }

    #[test]
    fn parse_empty_config_ok() {
        let config = ProjectConfig::from_toml_str("").expect("empty config parses");
        assert!(config.tools.is_empty());
        assert!(config.groups.is_empty());
    }

    #[test]
    fn parse_full_config_ok() {
        let toml_str = r#"
[tools]
cmake = "ocx.sh/cmake:3.28"
ninja = "ocx.sh/ninja:1.11"

[group.ci.tools]
shellcheck = "ocx.sh/shellcheck:0.10"
shfmt = "ocx.sh/shfmt:3.7"

[group.release.tools]
goreleaser = "ocx.sh/goreleaser:2.0"
"#;
        let config = ProjectConfig::from_toml_str(toml_str).expect("full config parses");
        assert_eq!(config.tools.len(), 2);
        let cmake = config.tools.get("cmake").expect("cmake present");
        assert_eq!(cmake.to_string(), "ocx.sh/cmake:3.28");
        assert_eq!(config.groups.len(), 2);
        let sc = config
            .groups
            .get("ci")
            .and_then(|g| g.tools.get("shellcheck"))
            .expect("ci/shellcheck present");
        assert_eq!(sc.to_string(), "ocx.sh/shellcheck:0.10");
        let gr = config
            .groups
            .get("release")
            .and_then(|g| g.tools.get("goreleaser"))
            .expect("release/goreleaser present");
        assert_eq!(gr.to_string(), "ocx.sh/goreleaser:2.0");
    }

    // ── Group restructure: [group.<name>.tools] / [group.<name>.env] (S2/S8) ──

    /// `[group.ci.tools]` and `[group.ci.env]` both populate their
    /// respective halves of the restructured `Group`.
    #[test]
    fn group_tools_and_env_both_populate() {
        let toml_str = r#"
[group.ci.tools]
shellcheck = "ocx.sh/shellcheck:0.10"

[group.ci.env]
CI = "1"
"#;
        let config = ProjectConfig::from_toml_str(toml_str).expect("nested group body must parse");
        let group = config.groups.get("ci").expect("group 'ci' present");
        assert_eq!(
            group.tools.get("shellcheck").map(ToString::to_string),
            Some("ocx.sh/shellcheck:0.10".to_string())
        );
        assert_eq!(group.env.get("CI"), Some(&crate::project::EnvValue::constant("1")));
    }

    /// `[group.ci]` with only `env` (no `tools`) parses; `tools` stays empty.
    #[test]
    fn group_env_only_leaves_tools_empty() {
        let toml_str = r#"
[group.ci.env]
CI = "1"
"#;
        let config = ProjectConfig::from_toml_str(toml_str).expect("env-only group body must parse");
        let group = config.groups.get("ci").expect("group 'ci' present");
        assert!(
            group.tools.is_empty(),
            "tools must stay empty when only [env] is declared"
        );
        assert_eq!(group.env.get("CI"), Some(&crate::project::EnvValue::constant("1")));
    }

    /// `[group.ci]` with neither `tools` nor `env` parses as a
    /// declared-but-empty group.
    #[test]
    fn group_with_neither_subtable_parses_empty() {
        let toml_str = "[group.ci]\n";
        let config = ProjectConfig::from_toml_str(toml_str).expect("empty group body must parse");
        let group = config.groups.get("ci").expect("group 'ci' present");
        assert!(group.tools.is_empty());
        assert!(group.env.is_empty());
    }

    /// The flat-form migration message must survive when the offending key
    /// is `tools` or `env` itself, not just an arbitrary name like `bar`.
    ///
    /// Regression guard for the `lazy-mode`/`lazy-report` wiring: those two
    /// settings are legitimately string-valued, so admitting them by
    /// dispatching on key name *before* value shape would route
    /// `tools = "..."` into the `[group.<name>.tools]` sub-table parse and
    /// surface a raw TOML type error instead. `bar` alone cannot catch that
    /// — it falls through to the same arm either way.
    #[test]
    fn group_direct_binding_under_subtable_key_still_reports_flat_form() {
        for key in ["tools", "env"] {
            let toml_str = format!("[group.ci]\n{key} = \"ocx.sh/bar:1\"\n");
            let err = ProjectConfig::from_toml_str(&toml_str)
                .expect_err("string-valued '{key}' under [group.ci] must reject");

            #[allow(irrefutable_let_patterns)]
            let crate::project::Error::Project(pe) = err else {
                panic!("expected Error::Project for key {key:?}");
            };
            let ProjectErrorKind::GroupHoldsDirectBinding { group, binding } = &pe.kind else {
                panic!("key {key:?} must report the flat binding form, got {:?}", pe.kind);
            };
            assert_eq!(group, "ci");
            assert_eq!(binding, key);
        }
    }

    /// S8: a tool binding declared directly under `[group.<name>]` (the
    /// removed flat form) is a parse error naming the group and pointing at
    /// `[group.<name>.tools]`, classified `ExitCode::ConfigError` (78) — the
    /// error message IS the migration story for the handful of files
    /// written against the old shape.
    #[test]
    fn group_direct_binding_is_parse_error_naming_group_and_tools() {
        let toml_str = r#"
[group.ci]
bar = "ocx.sh/bar:1"
"#;
        let err = ProjectConfig::from_toml_str(toml_str).expect_err("direct binding under [group.ci] must reject");

        let code = <crate::project::Error as crate::cli::ClassifyExitCode>::classify(&err);
        assert_eq!(
            code,
            Some(crate::cli::ExitCode::ConfigError),
            "S8 break must classify as ConfigError (exit 78); got {code:?}"
        );

        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("[group.ci.tools]"),
            "message must point at [group.ci.tools]; got {rendered:?}"
        );

        #[allow(irrefutable_let_patterns)]
        let crate::project::Error::Project(pe) = err else {
            panic!("expected Error::Project");
        };
        let ProjectErrorKind::GroupHoldsDirectBinding { group, binding } = &pe.kind else {
            panic!("expected GroupHoldsDirectBinding, got {:?}", pe.kind);
        };
        assert_eq!(group, "ci");
        assert_eq!(binding, "bar");
    }

    /// A `[group.ci.tolos]` typo (an unrecognized sub-table — neither
    /// `tools` nor `env`) is rejected naming both the group and the
    /// offending key.
    #[test]
    fn group_unknown_subsection_rejected_naming_group_and_key() {
        let toml_str = r#"
[group.ci.tolos]
shellcheck = "ocx.sh/shellcheck:0.10"
"#;
        let err = ProjectConfig::from_toml_str(toml_str).expect_err("unknown [group.ci.tolos] must reject");
        #[allow(irrefutable_let_patterns)]
        let crate::project::Error::Project(pe) = err else {
            panic!("expected Error::Project");
        };
        let ProjectErrorKind::UnknownGroupSection { group, key } = &pe.kind else {
            panic!("expected UnknownGroupSection, got {:?}", pe.kind);
        };
        assert_eq!(group, "ci");
        assert_eq!(key, "tolos");
    }

    /// A tool literally named `env` or `tools` inside `[group.ci.tools]`
    /// needs no special handling — it is just a key inside a plain map
    /// (S2).
    #[test]
    fn group_tools_named_env_or_tools_parse() {
        let toml_str = r#"
[group.ci.tools]
env = "ocx.sh/env:1"
tools = "ocx.sh/tools:1"
"#;
        let config = ProjectConfig::from_toml_str(toml_str).expect("tools named 'env'/'tools' must parse");
        let group = config.groups.get("ci").expect("group 'ci' present");
        assert_eq!(
            group.tools.get("env").map(ToString::to_string),
            Some("ocx.sh/env:1".to_string())
        );
        assert_eq!(
            group.tools.get("tools").map(ToString::to_string),
            Some("ocx.sh/tools:1".to_string())
        );
    }

    /// `[group.default]` / `[group.all]` stay rejected before any
    /// group-body inspection — even now that a populated body is the
    /// nested `.tools`/`.env` shape rather than the old flat one.
    #[test]
    fn reserved_default_group_rejected_even_with_nested_tools_body() {
        let toml_str = r#"
[group.default.tools]
foo = "ocx.sh/foo:1"
"#;
        let err = ProjectConfig::from_toml_str(toml_str).expect_err("[group.default.*] must still reject");
        let crate::project::Error::Project(ref pe) = err;
        assert!(
            matches!(&pe.kind, ProjectErrorKind::ReservedGroupName { name, .. } if name == "default"),
            "expected ReservedGroupName {{ name: \"default\", .. }}; got {err:?}"
        );
    }

    #[test]
    fn parse_unknown_top_level_field_rejects() {
        // `deny_unknown_fields` on `ProjectConfig` (and on
        // `RawProjectConfig` during the first pass) must trip on
        // unknown keys.
        let toml_str = r#"
foo = "bar"

[tools]
cmake = "ocx.sh/cmake:3.28"
"#;
        let err = ProjectConfig::from_toml_str(toml_str).expect_err("unknown field must reject");
        assert_kind!(err, ProjectErrorKind::TomlParse(_));
    }

    #[test]
    fn parse_rejects_reserved_default_group() {
        // `[group.default]` is the one literal string that must be a parse
        // error even though serde would accept it. Enforced post-parse.
        let toml_str = r#"
[group.default]
foo = "ocx.sh/foo:1"
"#;
        let err = ProjectConfig::from_toml_str(toml_str).expect_err("[group.default] must reject");
        // Updated to match the new parameterized variant shape (Phase 1.2).
        let crate::project::Error::Project(ref pe) = err;
        assert!(
            matches!(&pe.kind, ProjectErrorKind::ReservedGroupName { name, .. } if name == "default"),
            "expected ReservedGroupName {{ name: \"default\", .. }}; got {err:?}"
        );
    }

    #[test]
    fn from_toml_str_accepts_same_name_across_groups() {
        // Same name in [tools] AND [group.ci] is now allowed at the schema
        // layer. The runtime conflict check happens at compose time via
        // `DuplicateToolAcrossSelectedGroups`, not at parse time.
        let toml_str = r#"
[tools]
cmake = "ocx.sh/cmake:3.28"

[group.ci.tools]
cmake = "ocx.sh/cmake:3.29"
"#;
        let config = ProjectConfig::from_toml_str(toml_str)
            .expect("same binding name in [tools] and [group.ci] must parse successfully");
        assert!(config.tools.contains_key("cmake"), "cmake must be present in [tools]");
        assert!(
            config
                .groups
                .get("ci")
                .map(|g| g.tools.contains_key("cmake"))
                .unwrap_or(false),
            "cmake must be present in [group.ci]"
        );
    }

    #[test]
    fn parse_allows_same_tool_in_two_groups() {
        // Same tool name in two groups (NOT in [tools]) is allowed at the
        // schema layer. The cross-group conflict check is exec-time.
        let toml_str = r#"
[group.ci.tools]
shellcheck = "ocx.sh/shellcheck:0.10"

[group.lint.tools]
shellcheck = "ocx.sh/shellcheck:0.10"
"#;
        let config =
            ProjectConfig::from_toml_str(toml_str).expect("same tool in two groups should parse at schema layer");
        assert!(config.tools.is_empty());
        assert_eq!(config.groups.len(), 2);
    }

    #[test]
    fn parse_accepts_full_identifier_forms() {
        // Cover all four canonical Identifier forms accepted by
        // `Identifier::parse`. F1: the binding name (map key) is
        // independent of the repository path.
        //
        // Every value here carries an explicit tag, digest, or both, so
        // `parse_tool_map` does not inject the bare-identifier `:latest`
        // default — see `parse_defaults_bare_identifier_to_latest_tag`
        // for the bare-repo case. Each value round-trips through
        // `Identifier::Display` verbatim.
        let toml_str = r#"
[tools]
cmake = "ocx.sh/cmake:3.28"
mytool = "ghcr.io/acme/mytool:1.0"
pinned = "ghcr.io/acme/tool@sha256:abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
digest_and_tag = "ghcr.io/acme/tool:v1@sha256:abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
"#;
        let config = ProjectConfig::from_toml_str(toml_str).expect("full identifier forms parse");
        let cases = &[
            ("cmake", "ocx.sh/cmake:3.28"),
            ("mytool", "ghcr.io/acme/mytool:1.0"),
            (
                "pinned",
                "ghcr.io/acme/tool@sha256:abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789",
            ),
            (
                "digest_and_tag",
                "ghcr.io/acme/tool:v1@sha256:abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789",
            ),
        ];
        for (binding, expected) in cases {
            let id = config
                .tools
                .get(*binding)
                .unwrap_or_else(|| panic!("binding {binding} present"));
            assert_eq!(
                id.to_string(),
                *expected,
                "binding {binding} must round-trip Display verbatim",
            );
        }
    }

    #[test]
    fn parse_defaults_bare_identifier_to_latest_tag() {
        // Unit 3 contract: `[tools]` and `[group.*]` entries with a registry
        // and repository but no tag and no digest get `:latest` injected at
        // parse time. The default lives at the project-config boundary, not
        // on `Identifier` — `Identifier::tag()` still returns `None` for
        // CLI args without a tag.
        let toml_str = r#"
[tools]
cmake = "ocx.sh/cmake"

[group.ci.tools]
shellcheck = "ocx.sh/shellcheck"
"#;
        let config = ProjectConfig::from_toml_str(toml_str).expect("bare repo entries parse");

        let cmake = config.tools.get("cmake").expect("cmake binding present");
        assert_eq!(
            cmake.tag(),
            Some("latest"),
            "bare [tools] entry must default to ':latest'",
        );
        assert_eq!(cmake.to_string(), "ocx.sh/cmake:latest");

        let shellcheck = config
            .groups
            .get("ci")
            .and_then(|g| g.tools.get("shellcheck"))
            .expect("ci/shellcheck binding present");
        assert_eq!(
            shellcheck.tag(),
            Some("latest"),
            "bare [group.*] entry must default to ':latest'",
        );
        assert_eq!(shellcheck.to_string(), "ocx.sh/shellcheck:latest");
    }

    #[test]
    fn parse_preserves_tag_and_digest_identifier() {
        // Counter-case: explicit tag + digest is the most-pinned form. The
        // bare-identifier guard inspects both fields; an explicit tag must
        // suppress the `:latest` default even when the digest is also set.
        let hex = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
        let toml_str = format!(
            r#"[tools]
both = "ghcr.io/acme/tool:v1@sha256:{hex}"
"#
        );
        let config = ProjectConfig::from_toml_str(&toml_str).expect("tag+digest entry parses");
        let both = config.tools.get("both").expect("both binding present");
        assert_eq!(both.tag(), Some("v1"), "explicit tag preserved alongside digest");
        assert!(both.digest().is_some(), "digest preserved");
    }

    #[test]
    fn parse_preserves_digest_only_identifier_without_injecting_latest() {
        // Counter-case: digest-pinned entry has no tag, but the digest is
        // canonical pin — auto-injecting `:latest` would silently override a
        // deliberate immutable reference.
        let hex = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
        let toml_str = format!(
            r#"[tools]
pinned = "ghcr.io/acme/tool@sha256:{hex}"
"#
        );
        let config = ProjectConfig::from_toml_str(&toml_str).expect("digest-only entry parses");
        let pinned = config.tools.get("pinned").expect("pinned binding present");
        assert_eq!(pinned.tag(), None, "digest-only entry must not get a default tag");
        assert!(pinned.digest().is_some(), "digest preserved");
    }

    #[test]
    fn parse_preserves_explicit_tag() {
        // Counter-case: explicit tag is preserved verbatim — no normalization,
        // no override.
        let toml_str = r#"
[tools]
cmake = "ocx.sh/cmake:3.28"
"#;
        let config = ProjectConfig::from_toml_str(toml_str).expect("tagged entry parses");
        let cmake = config.tools.get("cmake").expect("cmake binding present");
        assert_eq!(cmake.tag(), Some("3.28"), "explicit tag preserved");
    }

    #[test]
    fn parse_rejects_bare_tag_value_with_missing_registry_diagnostic() {
        // F1 contract: bare-tag values (no explicit registry) are
        // rejected. The diagnostic must carry both the binding name
        // and the offending value so the user can locate the line.
        let toml = r#"[tools]
cmake = "3.28"
"#;
        let err = ProjectConfig::from_toml_str(toml).expect_err("bare tag must be rejected");
        #[allow(irrefutable_let_patterns)]
        let crate::project::Error::Project(pe) = err else {
            panic!("expected Error::Project");
        };
        let ProjectErrorKind::ToolValueMissingRegistry { name, value } = &pe.kind else {
            panic!("expected ToolValueMissingRegistry, got {:?}", pe.kind);
        };
        assert_eq!(name, "cmake");
        assert_eq!(value, "3.28");
    }

    #[test]
    fn parse_rejects_bare_tag_value_in_group_with_missing_registry_diagnostic() {
        // Same F1 contract applies inside `[group.*.tools]` tables — the
        // first pass walks both maps and validates uniformly.
        let toml = r#"[group.ci.tools]
shellcheck = "0.10"
"#;
        let err = ProjectConfig::from_toml_str(toml).expect_err("bare tag must be rejected in groups");
        #[allow(irrefutable_let_patterns)]
        let crate::project::Error::Project(pe) = err else {
            panic!("expected Error::Project");
        };
        let ProjectErrorKind::ToolValueMissingRegistry { name, value } = &pe.kind else {
            panic!("expected ToolValueMissingRegistry, got {:?}", pe.kind);
        };
        assert_eq!(name, "shellcheck");
        assert_eq!(value, "0.10");
    }

    #[test]
    fn display_chain_for_missing_registry_is_load_bearing() {
        // Message shape IS the contract here: users see this and need
        // to know which binding failed, why, and how to fix it.
        let toml = r#"[tools]
cmake = "3.28"
"#;
        let err = ProjectConfig::from_toml_str(toml).expect_err("rejected");
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("tool 'cmake'"),
            "diagnostic must name the binding: {rendered}"
        );
        assert!(
            rendered.contains("missing a registry"),
            "diagnostic must explain the failure: {rendered}"
        );
        assert!(
            rendered.contains("ocx.sh/cmake:3.28") || rendered.contains("registry/repo:tag"),
            "diagnostic must include an example: {rendered}"
        );
    }

    #[test]
    fn parse_rejects_malformed_identifier_with_tool_value_invalid() {
        // Invalid characters (uppercase repo) — `Identifier::parse`
        // rejects with a non-MissingRegistry kind. The two-pass parser
        // must surface this as `ToolValueInvalid` so the underlying
        // `IdentifierError` reaches the diagnostic chain via `#[source]`.
        let toml = r#"[tools]
bad = "ocx.sh/CMAKE:3.28"
"#;
        let err = ProjectConfig::from_toml_str(toml).expect_err("malformed identifier rejected");
        #[allow(irrefutable_let_patterns)]
        let crate::project::Error::Project(pe) = err else {
            panic!("expected Error::Project");
        };
        let ProjectErrorKind::ToolValueInvalid { name, value, .. } = &pe.kind else {
            panic!("expected ToolValueInvalid, got {:?}", pe.kind);
        };
        assert_eq!(name, "bad");
        assert_eq!(value, "ocx.sh/CMAKE:3.28");
    }

    #[test]
    fn display_chain_for_reserved_group_is_load_bearing() {
        // One case where message shape IS the contract: users see this
        // and need to know what to rename. Uses `{:#}` to render the
        // full message.
        let toml_str = "[group.default]\nfoo = \"ocx.sh/foo:1\"\n";
        let err = ProjectConfig::from_toml_str(toml_str).expect_err("reserved must reject");
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("[group.default] is reserved"),
            "chain display should name the reserved-group error, got: {rendered}"
        );
    }

    #[tokio::test]
    async fn load_rejects_oversized_file() {
        // Size-cap contract: `ocx.toml` larger than 64 KiB is a sanity
        // failure, surfaced as a structured `FileTooLarge` error rather
        // than proceeding into a pathological TOML parse. Matches the
        // ambient config loader's cap and the `ocx.lock` guard in the
        // sibling module.
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("ocx.toml");

        let padding: String = "# padding comment line to exceed the size cap\n".repeat(2200);
        let oversized = format!("{padding}\n[tools]\ncmake = \"ocx.sh/cmake:3.28\"\n");
        assert!(
            oversized.len() > 64 * 1024,
            "fixture must exceed 64 KiB cap, got {}",
            oversized.len()
        );
        tokio::fs::write(&path, &oversized).await.expect("write oversized");

        let err = ProjectConfig::from_path(&path)
            .await
            .expect_err("oversized config must reject");
        assert_kind!(err, ProjectErrorKind::FileTooLarge { .. });
    }

    /// A checked-in `ocx.toml` cannot declare the trust-sensitive `OCX_*`
    /// variables — not in `[env]`, not in a group's — because both sit in the
    /// namespace `crate::env::is_reserved_ocx_key` reserves.
    ///
    /// `OCX_NO_VERIFY` turns off the policy-gated auto-verify on install/pull
    /// and is forwarded to every child ocx; `OCX_IDENTITY_TOKEN` is a bearer
    /// credential. Either one declarable from a project file would let a
    /// repository silently disable signature verification for everyone who runs
    /// a tool out of it.
    ///
    /// Built from the `keys::` constants rather than string literals: the gate
    /// matches on the `OCX_` prefix, so respelling either variable outside that
    /// prefix moves it out of the gate's reach without touching the gate. That
    /// is the failure this test exists to catch, alongside the gate itself
    /// being weakened.
    #[test]
    fn project_env_cannot_declare_trust_sensitive_ocx_keys() {
        for key in [crate::env::keys::OCX_NO_VERIFY, crate::env::keys::OCX_IDENTITY_TOKEN] {
            for scope in ["env", "group.ci.env"] {
                let toml_str = format!("[{scope}]\n{key} = \"1\"\n");
                let Err(err) = ProjectConfig::from_toml_str(&toml_str) else {
                    panic!("[{scope}] must reject the reserved key {key}");
                };
                assert_eq!(
                    <crate::project::Error as crate::cli::ClassifyExitCode>::classify(&err),
                    Some(crate::cli::ExitCode::ConfigError),
                    "a reserved-key declaration is a config fault (exit 78)"
                );

                #[allow(irrefutable_let_patterns)]
                let crate::project::Error::Project(project_error) = err else {
                    panic!("expected Error::Project");
                };
                let ProjectErrorKind::EnvReservedKey {
                    scope: reported_scope,
                    key: reported_key,
                } = &project_error.kind
                else {
                    panic!("expected EnvReservedKey, got {:?}", project_error.kind);
                };
                assert_eq!(reported_scope, scope, "the error must name the table to edit");
                assert_eq!(reported_key, key);
            }
        }
    }

    // ── resolve() contract tests ─────────────────────────────────────────────
    //
    // Each test acquires `crate::test::env::lock()` and clears the three env
    // vars that influence resolution so tests do not bleed state.
    // `OCX_CEILING_PATH` is set to the workspace root in CWD-walk tests so
    // the walk cannot escape into the real filesystem.
    //
    // W2-P3 (adr_global_toolchain_tier.md §Decision 1): the implicit
    // `$OCX_HOME/ocx.toml` home-tier fallback was removed. The home-fallback
    // tests (`resolve_walk_miss_falls_back_to_home`, `resolve_explicit_beats_home`,
    // `resolve_no_project_kill_switch_returns_none_even_with_home`,
    // `resolve_walk_hit_beats_home`, the home-tier half of
    // `resolve_lock_path_is_with_extension_lock`, `resolve_home_follows_symlinks`,
    // `resolve_home_directory_returns_none`) were deleted with the behaviour
    // they pinned. The surviving non-home tests below are updated to the new
    // 4-arg `resolve(cwd, explicit, ocx_home, global)` signature.

    /// CWD walk finds `ocx.toml` → returns `(config_path, lock_path)`.
    #[tokio::test]
    async fn resolve_walk_hit_returns_project_paths() {
        let env = crate::test::env::lock();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
        let tmp = tempfile::tempdir().expect("tempdir");
        env.set("OCX_CEILING_PATH", tmp.path().to_str().unwrap());
        let config_path = tmp.path().join("ocx.toml");
        tokio::fs::write(&config_path, "").await.expect("write");
        let result = ProjectConfig::resolve(Some(tmp.path()), None, None, false)
            .await
            .expect("resolve ok");
        let (cp, lp) = result.expect("Some expected");
        assert_eq!(cp, config_path);
        assert_eq!(lp, tmp.path().join("ocx.lock"));
    }

    /// W2-P3 (adr_global_toolchain_tier.md §Decision 1): regression for the
    /// deleted home fallback. With no explicit `--project`, no `OCX_PROJECT`,
    /// no `global`, and a CWD-walk miss, `resolve` returns `None` even when an
    /// `$OCX_HOME/ocx.toml` exists — there is no implicit home discovery. The
    /// `ocx_home` argument names a dir that DOES contain an `ocx.toml`, so a
    /// regression that re-adds the fallback would make this `Some` and fail.
    #[tokio::test]
    async fn project_path_returns_none_without_global_or_project() {
        let env = crate::test::env::lock();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let home_dir = tempfile::tempdir().expect("home tempdir");
        env.set("OCX_CEILING_PATH", workspace.path().to_str().unwrap());
        // A home `ocx.toml` exists but must NOT be discovered implicitly.
        tokio::fs::write(home_dir.path().join("ocx.toml"), "")
            .await
            .expect("write home ocx.toml");
        let result = ProjectConfig::resolve(Some(workspace.path()), None, Some(home_dir.path()), false)
            .await
            .expect("resolve ok");
        assert!(
            result.is_none(),
            "walk-miss with no --global/--project must be None — no implicit \
             $OCX_HOME/ocx.toml fallback (adr_global_toolchain_tier.md §Decision 1)"
        );
    }

    /// W2-P3 (adr_global_toolchain_tier.md §Decision 1/2): `global = true`
    /// selects `<ocx_home>/ocx.toml` directly with its sibling
    /// `<ocx_home>/ocx.lock`, bypassing the CWD walk entirely (peer to the
    /// explicit `--project` branch). A project `ocx.toml` placed in the CWD
    /// must be ignored under `global` — the global selector never consults the
    /// walk.
    #[tokio::test]
    async fn resolve_selects_ocx_home_under_global() {
        let env = crate::test::env::lock();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
        let ocx_home = tempfile::tempdir().expect("ocx_home tempdir");
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        env.set("OCX_CEILING_PATH", workspace.path().to_str().unwrap());
        // A competing project file in the CWD must be bypassed by `global`.
        tokio::fs::write(workspace.path().join("ocx.toml"), "")
            .await
            .expect("write cwd ocx.toml");

        let result = ProjectConfig::resolve(Some(workspace.path()), None, Some(ocx_home.path()), true)
            .await
            .expect("resolve ok");
        let (cp, lp) = result.expect("Some expected under --global");
        assert_eq!(
            cp,
            ocx_home.path().join("ocx.toml"),
            "--global must select <ocx_home>/ocx.toml, not the CWD project file"
        );
        assert_eq!(
            lp,
            ocx_home.path().join("ocx.lock"),
            "--global lock must be <ocx_home>/ocx.lock"
        );
    }

    /// Explicit missing path → `Err(FileNotFound)` propagated from Phase 1.
    #[tokio::test]
    async fn resolve_explicit_missing_returns_file_not_found_error() {
        let env = crate::test::env::lock();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
        env.remove("OCX_CEILING_PATH");
        let missing = PathBuf::from("/tmp/ocx-resolve-test-nonexistent-explicit.toml");
        let err = ProjectConfig::resolve(None, Some(&missing), None, false)
            .await
            .expect_err("missing explicit path must be FileNotFound");
        assert!(
            matches!(
                err,
                crate::config::error::Error::FileNotFound {
                    path: ref p,
                    tier: crate::config::error::ConfigSource::Project,
                } if *p == missing,
            ),
            "expected FileNotFound(Project) for missing explicit path, got: {err:?}"
        );
    }

    // ── reserved `all` group name ────────────────────────────────────────

    /// Plan §Phase 3.1: `parse_rejects_reserved_all_group`
    ///
    /// `[group.all]` must be rejected at parse time with
    /// `ReservedGroupName { name: "all", .. }`.
    #[test]
    fn parse_rejects_reserved_all_group() {
        let toml_str = r#"
[group.all]
foo = "ocx.sh/foo:1"
"#;
        let err = ProjectConfig::from_toml_str(toml_str).expect_err("[group.all] must reject");
        let crate::project::Error::Project(ref pe) = err;
        assert!(
            matches!(&pe.kind, ProjectErrorKind::ReservedGroupName { name, .. } if name == "all"),
            "expected ReservedGroupName {{ name: \"all\", .. }}; got {err:?}"
        );
    }

    /// Plan §Phase 3.1: `display_chain_for_reserved_all_is_load_bearing`
    ///
    /// The formatted error for `[group.all]` must contain the load-bearing
    /// substrings `"[group.all] is reserved"` and `"reserved keyword"`.
    #[test]
    fn display_chain_for_reserved_all_is_load_bearing() {
        use crate::project::ProjectErrorKind;
        use crate::project::error::ProjectError;
        let kind = ProjectErrorKind::ReservedGroupName {
            name: "all".into(),
            hint: "rename this group; `all` is a reserved keyword that selects every declared group",
        };
        let err = crate::project::Error::Project(ProjectError::new(std::path::PathBuf::new(), kind));
        let formatted = format!("{err:#}");
        assert!(
            formatted.contains("[group.all] is reserved"),
            "formatted error must contain '[group.all] is reserved'; got: {formatted:?}"
        );
        assert!(
            formatted.contains("reserved keyword"),
            "formatted error must contain 'reserved keyword'; got: {formatted:?}"
        );
    }

    // ── C-005 / C-006: lazy-mode + lazy-report at the three config tiers ────

    /// `lazy-mode` parses at the toolchain (top-level scalar), `[group.<g>]`
    /// and `[package."<id>"]` tiers, and each declaration lands in its own
    /// struct field. Three tiers in one file so a parser that routed one
    /// tier's value into another's field cannot pass.
    #[test]
    fn lazy_mode_parses_at_toolchain_group_and_package_tiers() {
        let toml = r#"lazy-mode = "never"

[tools]
cmake = "ocx.sh/cmake:3.28"

[group.ci]
lazy-mode = "always"

[group.ci.tools]
shellcheck = "ocx.sh/shellcheck:0.10"

[package."ghcr.io/acme/cli:v1"]
lazy-mode = "always"
"#;
        let config = ProjectConfig::from_toml_str(toml).expect("lazy-mode parses at all three tiers");
        assert_eq!(config.lazy_mode, Some(LazyMode::Never), "toolchain tier");
        let group = config.groups.get("ci").expect("group 'ci' present");
        assert_eq!(group.lazy_mode, Some(LazyMode::Always), "group tier");
        assert_eq!(
            group.tools.get("shellcheck").map(ToString::to_string),
            Some("ocx.sh/shellcheck:0.10".to_string()),
            "a group carrying lazy-mode still parses its tools sub-table"
        );
        assert_eq!(
            config
                .packages
                .get("ghcr.io/acme/cli:v1")
                .expect("package entry present")
                .lazy_mode,
            Some(LazyMode::Always),
            "package tier"
        );
    }

    /// The `lazy-report` mirror of the above, one tier shorter: there is no
    /// `[group.<g>].lazy-report`.
    #[test]
    fn lazy_report_parses_at_toolchain_and_package_tiers() {
        let toml = r#"lazy-report = "silent"

[tools]
cmake = "ocx.sh/cmake:3.28"

[package."ghcr.io/acme/cli:v1"]
no-patches = true
lazy-report = "progress"
"#;
        let config = ProjectConfig::from_toml_str(toml).expect("lazy-report parses at both its tiers");
        assert_eq!(config.lazy_report, Some(LazyReport::Silent), "toolchain tier");
        let settings = config
            .packages
            .get("ghcr.io/acme/cli:v1")
            .expect("package entry present");
        assert_eq!(settings.lazy_report, Some(LazyReport::Progress), "package tier");
        assert!(settings.no_patches, "the pre-existing package setting still parses");
    }

    /// `[group.<g>].lazy-report` is **not** a key — it is rejected, not
    /// ignored.
    ///
    /// The shim process that resolves `lazy-report` never learns which group
    /// composed the tool, so a group-tier value could only ever be written and
    /// never read. Accepting and dropping it is exactly the settable-and-
    /// unreadable defect the removal exists to close, so the parser has to
    /// say so. `lazy-mode` in the same table is the positive control: it is
    /// still a group-tier key, so the rejection below cannot come from a
    /// parser that stopped accepting scalars in `[group.<g>]` altogether.
    #[test]
    fn group_tier_lazy_report_is_rejected_while_lazy_mode_is_accepted() {
        let accepted = ProjectConfig::from_toml_str("[group.ci]\nlazy-mode = \"always\"\n")
            .expect("[group.<g>].lazy-mode is still a recognized key");
        assert_eq!(
            accepted.groups.get("ci").expect("group 'ci' present").lazy_mode,
            Some(LazyMode::Always)
        );

        let error = ProjectConfig::from_toml_str("[group.ci]\nlazy-report = \"progress\"\n")
            .expect_err("[group.<g>].lazy-report must be refused, never silently ignored");
        let rendered = error.to_string();
        assert!(
            rendered.contains("lazy-report"),
            "the diagnostic must name the offending key; got {rendered}"
        );
    }

    /// C-006: an undeclared tier is `None` — "inherit", never
    /// [`LazyMode::Never`]. A `#[serde(default)]` that resolved to a floor
    /// value at parse time would make every config file an explicit `never`
    /// declaration and strand the tiers below it.
    #[test]
    fn absent_lazy_settings_leave_every_tier_none() {
        let toml = r#"[tools]
cmake = "ocx.sh/cmake:3.28"

[group.ci.tools]
shellcheck = "ocx.sh/shellcheck:0.10"

[package."ghcr.io/acme/cli:v1"]
no-patches = true
"#;
        let config = ProjectConfig::from_toml_str(toml).expect("parse");
        assert_eq!(config.lazy_mode, None);
        assert_eq!(config.lazy_report, None);
        let group = config.groups.get("ci").expect("group 'ci' present");
        assert_eq!(group.lazy_mode, None);
        let settings = config.packages.get("ghcr.io/acme/cli:v1").expect("package entry");
        assert_eq!(settings.lazy_mode, None);
        assert_eq!(settings.lazy_report, None);
    }

    /// C-005: an unrecognized wire value is a parse error at every tier, not a
    /// silent default.
    #[test]
    fn lazy_mode_unknown_value_rejected_at_every_tier() {
        let cases = [
            ("toolchain", "lazy-mode = \"sometimes\"\n".to_string()),
            ("group", "[group.ci]\nlazy-mode = \"sometimes\"\n".to_string()),
            (
                "package",
                "[package.\"ghcr.io/acme/cli:v1\"]\nlazy-mode = \"sometimes\"\n".to_string(),
            ),
        ];
        for (tier, toml) in cases {
            let err = ProjectConfig::from_toml_str(&toml)
                .err()
                .unwrap_or_else(|| panic!("`sometimes` must be rejected at the {tier} tier"));
            assert_kind!(err, ProjectErrorKind::TomlParse(_));
        }
    }

    /// The `lazy-report` mirror: an unrecognized value is rejected at both of
    /// its tiers. No group row — the key does not exist there, and its
    /// rejection is `group_tier_lazy_report_is_rejected_while_lazy_mode_is_accepted`'s
    /// subject rather than this test's.
    #[test]
    fn lazy_report_unknown_value_rejected_at_every_tier() {
        let cases = [
            ("toolchain", "lazy-report = \"loud\"\n".to_string()),
            (
                "package",
                "[package.\"ghcr.io/acme/cli:v1\"]\nlazy-report = \"loud\"\n".to_string(),
            ),
        ];
        for (tier, toml) in cases {
            let err = ProjectConfig::from_toml_str(&toml)
                .err()
                .unwrap_or_else(|| panic!("`loud` must be rejected at the {tier} tier"));
            assert_kind!(err, ProjectErrorKind::TomlParse(_));
        }
    }

    /// Admitting two new scalar keys must not have loosened
    /// `deny_unknown_fields`: a typo beside a valid `lazy-mode` is still a
    /// parse error at the toolchain and package tiers.
    #[test]
    fn unknown_key_beside_lazy_mode_still_rejected() {
        let toolchain = r#"lazy-mode = "always"
lazy-mod = "always"
"#;
        let err = ProjectConfig::from_toml_str(toolchain).expect_err("unknown top-level key must reject");
        assert_kind!(err, ProjectErrorKind::TomlParse(_));

        let package = r#"[package."ghcr.io/acme/cli:v1"]
lazy-mode = "always"
bogus = true
"#;
        let err = ProjectConfig::from_toml_str(package).expect_err("unknown [package] key must reject");
        assert_kind!(err, ProjectErrorKind::TomlParse(_));
    }

    /// The group tier's equivalent. `parse_group` walks the table by hand, so
    /// its unknown-key rejection is separate code from the two
    /// `deny_unknown_fields` derives above and needs its own proof.
    ///
    /// A non-string value is used deliberately: a *string*-valued unknown key
    /// takes the `GroupHoldsDirectBinding` arm (any string under an
    /// unrecognized key is the removed flat binding form), which is the
    /// pre-existing contract pinned by
    /// [`group_direct_binding_under_subtable_key_still_reports_flat_form`].
    #[test]
    fn unknown_non_string_key_beside_lazy_mode_in_group_still_rejected() {
        let toml = r#"[group.ci]
lazy-mode = "always"
bogus = 1
"#;
        let err = ProjectConfig::from_toml_str(toml).expect_err("unknown [group.ci] key must reject");
        #[allow(irrefutable_let_patterns)]
        let crate::project::Error::Project(pe) = err else {
            panic!("expected Error::Project");
        };
        let ProjectErrorKind::UnknownGroupSection { group, key } = &pe.kind else {
            panic!("expected UnknownGroupSection, got {:?}", pe.kind);
        };
        assert_eq!(group, "ci");
        assert_eq!(key, "bogus");
    }
}
