// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx.toml` schema: the developer-editable project-tier declaration.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use super::error::{ProjectError, ProjectErrorKind};
use crate::log;
use crate::oci::Identifier;
use crate::oci::identifier::error::IdentifierErrorKind;

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
/// schema boundary — see [`parse_tool_map`] for the contract. The
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

    /// Named additive groups; `[group.<name>]` in TOML. `default` is
    /// reserved — a literal `[group.default]` declaration is a parse
    /// error (enforced at parse time, not at the serde layer).
    #[serde(default, rename = "group")]
    pub groups: BTreeMap<String, BTreeMap<String, Identifier>>,

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
            groups: self.groups.clone(),
            declaration_hash_cache: OnceLock::new(),
        }
    }
}

impl PartialEq for ProjectConfig {
    fn eq(&self, other: &Self) -> bool {
        // The cache is a derived datum; comparing it would conflate "same
        // declaration" with "both cached" / "neither cached". Equality
        // speaks to the declared content only.
        self.tools == other.tools && self.groups == other.groups
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

    #[serde(default, rename = "group")]
    groups: BTreeMap<String, BTreeMap<String, String>>,
}

impl ProjectConfig {
    /// Constructor for in-test fixtures and programmatic construction sites
    /// that need to bypass the TOML round-trip in `from_toml_str`. Initialises
    /// the private declaration-hash cache as empty so the first call to
    /// [`Self::declaration_hash_cached`] computes the canonical value.
    pub fn from_parts(
        tools: BTreeMap<String, Identifier>,
        groups: BTreeMap<String, BTreeMap<String, Identifier>>,
    ) -> Self {
        Self {
            tools,
            groups,
            declaration_hash_cache: OnceLock::new(),
        }
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

    /// Resolve the project-tier `ocx.toml` and adjacent lock paths.
    ///
    /// Precedence: explicit > `OCX_PROJECT` > CWD walk >
    /// `$OCX_HOME/ocx.toml` home-tier fallback. Returns `None` when no
    /// source produces a path or `OCX_NO_PROJECT=1` is set (kill switch
    /// beats the home fallback per plan §3 step 3).
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
        home: Option<&Path>,
    ) -> std::result::Result<Option<(PathBuf, PathBuf)>, crate::config::error::Error> {
        // Steps 1-4: delegate to ConfigLoader (explicit flag > env > CWD walk).
        let walk_result = crate::config::loader::ConfigLoader::project_path(cwd, explicit).await?;

        if let Some(p) = walk_result {
            let lock = super::lock::lock_path_for(&p);
            return Ok(Some((p, lock)));
        }

        // Step 3 kill switch: OCX_NO_PROJECT=1 returns None even when a home
        // fallback exists (per plan §3 step 3). Explicit flag is NOT gated by
        // this (Amendment G3), so we check only after the walk returned None.
        if crate::env::flag("OCX_NO_PROJECT", false) {
            return Ok(None);
        }

        // Step 5: home-tier fallback.
        let Some(home_dir) = home else {
            return Ok(None);
        };
        let candidate = home_dir.join("ocx.toml");

        // Probe with metadata — follows symlinks intentionally (plan §3
        // "Symlink-at-home policy: symlinks are followed").
        match tokio::fs::metadata(&candidate).await {
            Ok(meta) if meta.file_type().is_file() => {
                let lock = super::lock::lock_path_for(&candidate);
                Ok(Some((candidate, lock)))
            }
            Ok(_) => {
                // Exists but is not a regular file (directory, FIFO, …) — same
                // permissive policy as `discover_paths` in the ambient loader.
                log::warn!(
                    "skipping non-regular-file home-tier candidate {} (expected a regular file)",
                    candidate.display()
                );
                Ok(None)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => {
                // Permission denied, broken symlink, etc. — permissive, same
                // as the ambient discovery tier.
                log::warn!("skipping unreadable home-tier candidate {}: {e}", candidate.display());
                Ok(None)
            }
        }
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
        // `[group.*]` table.
        let tools = parse_tool_map(&raw.tools, &path)?;
        let mut groups: BTreeMap<String, BTreeMap<String, Identifier>> = BTreeMap::new();
        for (group_name, group_tools) in raw.groups {
            let parsed = parse_tool_map(&group_tools, &path)?;
            groups.insert(group_name, parsed);
        }

        Ok(Self {
            tools,
            groups,
            declaration_hash_cache: OnceLock::new(),
        })
    }
}

/// Walk a raw `(name → value)` map and validate every value as a
/// fully-qualified [`Identifier`]. Splits
/// [`IdentifierErrorKind::MissingRegistry`] from other identifier
/// failures so the project-tier diagnostic can name the offending
/// binding without losing the underlying [`crate::oci::identifier::error::IdentifierError`]
/// for non-registry failures.
///
/// Bare identifiers — registry + repository, no tag and no digest
/// (e.g. `"ocx.sh/cmake"`) — get `:latest` injected at this boundary
/// so resolution always has an advisory tag to look up. The default is
/// applied here, not on [`Identifier`] itself, so CLI args without a
/// tag still surface as `tag = None`. Digest-pinned entries
/// (`@sha256:...`) keep `tag = None`; the digest is the canonical pin.
fn parse_tool_map(raw: &BTreeMap<String, String>, path: &Path) -> Result<BTreeMap<String, Identifier>, super::Error> {
    let mut out: BTreeMap<String, Identifier> = BTreeMap::new();
    for (name, value) in raw {
        match Identifier::parse(value) {
            Ok(id) => {
                let id = if id.tag().is_none() && id.digest().is_none() {
                    id.clone_with_tag("latest")
                } else {
                    id
                };
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

[group.ci]
shellcheck = "ocx.sh/shellcheck:0.10"
shfmt = "ocx.sh/shfmt:3.7"

[group.release]
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
            .and_then(|g| g.get("shellcheck"))
            .expect("ci/shellcheck present");
        assert_eq!(sc.to_string(), "ocx.sh/shellcheck:0.10");
        let gr = config
            .groups
            .get("release")
            .and_then(|g| g.get("goreleaser"))
            .expect("release/goreleaser present");
        assert_eq!(gr.to_string(), "ocx.sh/goreleaser:2.0");
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

[group.ci]
cmake = "ocx.sh/cmake:3.29"
"#;
        let config = ProjectConfig::from_toml_str(toml_str)
            .expect("same binding name in [tools] and [group.ci] must parse successfully");
        assert!(config.tools.contains_key("cmake"), "cmake must be present in [tools]");
        assert!(
            config
                .groups
                .get("ci")
                .map(|g| g.contains_key("cmake"))
                .unwrap_or(false),
            "cmake must be present in [group.ci]"
        );
    }

    #[test]
    fn parse_allows_same_tool_in_two_groups() {
        // Same tool name in two groups (NOT in [tools]) is allowed at the
        // schema layer. The cross-group conflict check is exec-time.
        let toml_str = r#"
[group.ci]
shellcheck = "ocx.sh/shellcheck:0.10"

[group.lint]
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

[group.ci]
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
            .and_then(|g| g.get("shellcheck"))
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
        // Same F1 contract applies inside `[group.*]` tables — the
        // first pass walks both maps and validates uniformly.
        let toml = r#"[group.ci]
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

    // ── resolve() contract tests ─────────────────────────────────────────────
    //
    // Each test acquires `crate::test::env::lock()` and clears the three env
    // vars that influence resolution so tests do not bleed state.
    // `OCX_CEILING_PATH` is set to the workspace root in CWD-walk tests so
    // the walk cannot escape into the real filesystem.

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
        let result = ProjectConfig::resolve(Some(tmp.path()), None, None)
            .await
            .expect("resolve ok");
        let (cp, lp) = result.expect("Some expected");
        assert_eq!(cp, config_path);
        assert_eq!(lp, tmp.path().join("ocx.lock"));
    }

    /// Walk miss with a valid `home/ocx.toml` → returns home-tier paths.
    #[tokio::test]
    async fn resolve_walk_miss_falls_back_to_home() {
        let env = crate::test::env::lock();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let home_dir = tempfile::tempdir().expect("home tempdir");
        env.set("OCX_CEILING_PATH", workspace.path().to_str().unwrap());
        // No ocx.toml in workspace — walk miss.
        let home_config = home_dir.path().join("ocx.toml");
        tokio::fs::write(&home_config, "").await.expect("write home");
        let result = ProjectConfig::resolve(Some(workspace.path()), None, Some(home_dir.path()))
            .await
            .expect("resolve ok");
        let (cp, lp) = result.expect("Some expected from home fallback");
        assert_eq!(cp, home_config);
        assert_eq!(lp, home_dir.path().join("ocx.lock"));
    }

    /// Walk miss, no home dir provided → `None`.
    #[tokio::test]
    async fn resolve_walk_miss_no_home_returns_none() {
        let env = crate::test::env::lock();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
        let workspace = tempfile::tempdir().expect("tempdir");
        env.set("OCX_CEILING_PATH", workspace.path().to_str().unwrap());
        let result = ProjectConfig::resolve(Some(workspace.path()), None, None)
            .await
            .expect("resolve ok");
        assert!(result.is_none(), "expected None when no home provided");
    }

    /// Explicit path beats the home fallback.
    #[tokio::test]
    async fn resolve_explicit_beats_home() {
        let env = crate::test::env::lock();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
        let flag_dir = tempfile::tempdir().expect("flag dir");
        let home_dir = tempfile::tempdir().expect("home dir");
        let flag_config = flag_dir.path().join("ocx.toml");
        let home_config = home_dir.path().join("ocx.toml");
        tokio::fs::write(&flag_config, "").await.expect("write flag");
        tokio::fs::write(&home_config, "").await.expect("write home");
        env.set("OCX_CEILING_PATH", flag_dir.path().to_str().unwrap());
        let result = ProjectConfig::resolve(None, Some(&flag_config), Some(home_dir.path()))
            .await
            .expect("resolve ok");
        let (cp, _lp) = result.expect("Some expected");
        assert_eq!(cp, flag_config, "explicit path must beat home fallback");
    }

    /// `OCX_NO_PROJECT=1` suppresses the home fallback.
    #[tokio::test]
    async fn resolve_no_project_kill_switch_returns_none_even_with_home() {
        let env = crate::test::env::lock();
        env.set("OCX_NO_PROJECT", "1");
        env.remove("OCX_PROJECT");
        let workspace = tempfile::tempdir().expect("tempdir");
        let home_dir = tempfile::tempdir().expect("home tempdir");
        env.set("OCX_CEILING_PATH", workspace.path().to_str().unwrap());
        let home_config = home_dir.path().join("ocx.toml");
        tokio::fs::write(&home_config, "").await.expect("write home");
        let result = ProjectConfig::resolve(Some(workspace.path()), None, Some(home_dir.path()))
            .await
            .expect("resolve ok");
        assert!(result.is_none(), "OCX_NO_PROJECT=1 must suppress home fallback");
    }

    /// CWD walk hit beats the home fallback (wholesale replacement, plan §3
    /// line 379).
    #[tokio::test]
    async fn resolve_walk_hit_beats_home() {
        let env = crate::test::env::lock();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
        let walk_dir = tempfile::tempdir().expect("walk dir");
        let home_dir = tempfile::tempdir().expect("home dir");
        let walk_config = walk_dir.path().join("ocx.toml");
        let home_config = home_dir.path().join("ocx.toml");
        tokio::fs::write(&walk_config, "").await.expect("write walk");
        tokio::fs::write(&home_config, "").await.expect("write home");
        env.set("OCX_CEILING_PATH", walk_dir.path().to_str().unwrap());
        let result = ProjectConfig::resolve(Some(walk_dir.path()), None, Some(home_dir.path()))
            .await
            .expect("resolve ok");
        let (cp, _lp) = result.expect("Some expected");
        assert_eq!(cp, walk_config, "walk hit must beat home fallback");
    }

    /// Lock path is `config_path.with_extension("lock")` for both tiers.
    #[tokio::test]
    async fn resolve_lock_path_is_with_extension_lock() {
        let env = crate::test::env::lock();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");

        // Project-tier hit
        let proj_dir = tempfile::tempdir().expect("proj tempdir");
        let proj_config = proj_dir.path().join("ocx.toml");
        tokio::fs::write(&proj_config, "").await.expect("write proj");
        env.set("OCX_CEILING_PATH", proj_dir.path().to_str().unwrap());
        let (_, lp) = ProjectConfig::resolve(Some(proj_dir.path()), None, None)
            .await
            .expect("resolve ok")
            .expect("Some");
        assert_eq!(
            lp.file_name().unwrap(),
            "ocx.lock",
            "project-tier lock must be ocx.lock"
        );

        // Home-tier hit
        let home_dir = tempfile::tempdir().expect("home tempdir");
        let home_config = home_dir.path().join("ocx.toml");
        tokio::fs::write(&home_config, "").await.expect("write home");
        let empty_ws = tempfile::tempdir().expect("empty ws");
        env.set("OCX_CEILING_PATH", empty_ws.path().to_str().unwrap());
        let (_, lp) = ProjectConfig::resolve(Some(empty_ws.path()), None, Some(home_dir.path()))
            .await
            .expect("resolve ok")
            .expect("Some from home");
        assert_eq!(lp.file_name().unwrap(), "ocx.lock", "home-tier lock must be ocx.lock");
    }

    /// Home `ocx.toml` that is a symlink is followed (symlink-at-home policy).
    #[cfg(unix)]
    #[tokio::test]
    async fn resolve_home_follows_symlinks() {
        let env = crate::test::env::lock();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
        let home_dir = tempfile::tempdir().expect("home tempdir");
        let real = home_dir.path().join("real.toml");
        tokio::fs::write(&real, "").await.expect("write real");
        let link = home_dir.path().join("ocx.toml");
        tokio::fs::symlink(&real, &link).await.expect("symlink");
        let workspace = tempfile::tempdir().expect("ws tempdir");
        env.set("OCX_CEILING_PATH", workspace.path().to_str().unwrap());
        let result = ProjectConfig::resolve(Some(workspace.path()), None, Some(home_dir.path()))
            .await
            .expect("resolve ok");
        let (cp, lp) = result.expect("Some expected — symlink followed");
        // The resolver returns the candidate path (`home/ocx.toml`), not the
        // resolved target (`home/real.toml`).
        assert_eq!(cp, link);
        assert_eq!(lp, home_dir.path().join("ocx.lock"));
    }

    /// Home `ocx.toml` that is a directory → `None` (permissive, with warn).
    #[tokio::test]
    async fn resolve_home_directory_returns_none() {
        let env = crate::test::env::lock();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
        let home_dir = tempfile::tempdir().expect("home tempdir");
        // Create `ocx.toml` as a directory instead of a file.
        let dir_candidate = home_dir.path().join("ocx.toml");
        tokio::fs::create_dir(&dir_candidate)
            .await
            .expect("create dir candidate");
        let workspace = tempfile::tempdir().expect("ws tempdir");
        env.set("OCX_CEILING_PATH", workspace.path().to_str().unwrap());
        let result = ProjectConfig::resolve(Some(workspace.path()), None, Some(home_dir.path()))
            .await
            .expect("resolve ok");
        assert!(result.is_none(), "directory candidate must yield None");
    }

    /// Explicit missing path → `Err(FileNotFound)` propagated from Phase 1.
    #[tokio::test]
    async fn resolve_explicit_missing_returns_file_not_found_error() {
        let env = crate::test::env::lock();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
        env.remove("OCX_CEILING_PATH");
        let missing = PathBuf::from("/tmp/ocx-resolve-test-nonexistent-explicit.toml");
        let err = ProjectConfig::resolve(None, Some(&missing), None)
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
}
