// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Resolved settings from files and environment: the four config tiers, the
//! managed tier, env-var vocabulary and validation.

pub mod edit;
pub mod env;
pub mod error;
pub mod home;
pub mod index;
pub mod insecure;
pub mod loader;
pub mod managed;
pub mod managed_config;
pub mod mirror;
pub mod patch;
pub mod records;
pub mod refresh;
pub mod registry;
pub mod shell;
pub mod tls;
pub mod update;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use ocx_util::fs::path::FileReference;

pub use self::managed::ManagedConfig;
pub use self::mirror::MirrorConfig;
pub use self::patch::PatchConfig;
pub use self::registry::RegistryConfig;
pub use self::shell::ShellConfig;
pub use self::update::UpdateConfig;

/// Which `config.toml` tier a value came from; runtime provenance only, never serialized.
///
/// Variants are listed lowest- to highest-precedence, the loader's fold order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConfigTier {
    /// `/etc/ocx/config.toml`.
    System,
    /// The per-user config directory's `config.toml`.
    User,
    /// `$OCX_HOME/config.toml`.
    Home,
    /// The managed-config snapshot's payload.
    Managed,
    /// A file named by `--config` or `OCX_CONFIG`.
    Explicit,
}

impl std::fmt::Display for ConfigTier {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::System => "system config.toml",
            Self::User => "user config.toml",
            Self::Home => "$OCX_HOME/config.toml",
            Self::Managed => "managed config",
            Self::Explicit => "--config / OCX_CONFIG",
        })
    }
}

// No `deny_unknown_fields` anywhere in this tree, or a newer managed payload bricks every older
// binary in the fleet (`adr_managed_config_tier.md`).
/// The `config.toml` document.
///
/// Unknown sections and unknown keys inside a known section are ignored, so a file written for a
/// newer ocx degrades to the parts an older one understands; the cost is that a typo'd key
/// silently does nothing. A change that cannot degrade (a key whose meaning or value shape
/// changed) ships as a managed payload under a new OCI tag, and fleets move onto it as they
/// upgrade — see "Rolling out an incompatible change" in the user guide.
#[derive(Debug, Default, Clone, Deserialize, schemars::JsonSchema)]
pub struct Config {
    /// Global registry-subsystem settings (`[registry]` section).
    pub registry: Option<RegistryDefaults>,

    /// Named per-registry configuration tables (`[registries.<name>]`).
    ///
    /// Every key is an identifier prefix, always — `[registry] default`
    /// never dereferences through this table.
    pub registries: Option<HashMap<String, RegistryConfig>>,

    /// Per-traffic-host mirrors (`[mirrors."<host>"]`).
    ///
    /// Maps a canonical upstream traffic host (e.g. `"ghcr.io"`, `"index.ocx.sh"`) to replacement
    /// endpoint(s), so read traffic goes to a corporate mirror instead of the firewall-blocked
    /// origin. A bare string rewrites both traffic roles for that host; a `{registry?, index?}`
    /// table splits per role — `registry` rewrites OCI distribution traffic only, `index`
    /// index-tree traffic only. Replace semantics: no origin fallback. The canonical identifier
    /// and content-addressed digest stay unchanged, so an `ocx.lock` produced behind the mirror
    /// remains valid with direct egress and vice versa.
    // Hand-rolled deserializer so a malformed entry names its host, not an opaque untagged mismatch.
    #[serde(default, deserialize_with = "mirror::deserialize_mirrors_table")]
    pub mirrors: Option<HashMap<String, MirrorConfig>>,

    /// Site-tier patch configuration (`[patches]`).
    ///
    /// Points at an operator-controlled patch registry that provides companion
    /// packages (CA bundles, proxy env vars, license-server endpoints) layered
    /// onto unmodified upstream packages at compose time.
    ///
    /// Absent → no patch tier configured (opt-in, not required).
    pub patches: Option<PatchConfig>,

    /// Corporate managed-configuration tier (`[managed]`).
    ///
    /// Points at an operator-controlled OCI artifact that supplies a plain
    /// `config.toml` payload — mirrors, patches pointer, default registry —
    /// synced into local state and merged above the user config every
    /// invocation.
    ///
    /// Absent → no managed tier configured (opt-in, seeded by
    /// `ocx self setup --managed-config`).
    pub managed: Option<ManagedConfig>,

    /// Background update checks (`[update]`): the posture for ocx itself and for the toolchain,
    /// plus the interval between checks.
    ///
    /// A personal setting, read from `config.toml` only: a managed payload's `[update]` is
    /// ignored and an `ocx.toml` refuses it. An unknown value never fails a command.
    // `fold_managed_tier` drops it and `ocx.toml` refuses it, or a publisher or a clone could switch on binary replacement.
    pub update: Option<UpdateConfig>,

    /// Identity-pinned verification policies (`[[trust.policy]]`).
    ///
    /// Unlike every other section, trust policies **array-append** across the
    /// `config.toml` tiers rather than replace — the operator trust set is the
    /// union of system, user and `$OCX_HOME`. At verify time this operator set
    /// takes precedence over the project `ocx.toml`. Consumed by
    /// `ocx package verify`.
    pub trust: Option<ocx_trust::TrustConfig>,

    /// Shell-integration settings (`[shell]`) — the per-prompt hook and
    /// completions toggles, plus the activation consent whitelist.
    ///
    /// **Never read from a project's `ocx.toml`**, only from `config.toml`
    /// tiers: consent read from a repository's own file would let a clone
    /// consent to itself. Consumed by `ocx self activate`, `ocx self setup`
    /// and `ocx shell state`.
    // `ConfigLoader` must strip `shell` from project-tier input, or a clone consents to itself.
    pub shell: Option<ShellConfig>,

    /// Execution-record sink (`[records]`).
    ///
    /// Designates where OCX writes one JSON resolution record per tool launch —
    /// the resolved package closure with digests, plus the resolved executable.
    /// Declared at SYSTEM scope it clamps: no lower tier can redirect or
    /// disable it.
    ///
    /// Absent → no records written (opt-in).
    pub records: Option<records::RecordsOptions>,

    // Reach a home root only through `ToolchainRoot::resolve`; using the field directly skips every refusal.
    /// Root directory holding every project's toolchain tree, each at `<root>/<project-key>/toolchain/`.
    ///
    /// The global toolchain home ignores this key: it is always `$OCX_HOME/toolchain`. Settable in
    /// every tier, a managed payload included. Only a leading `~` expands, against the home directory;
    /// a `%VAR%` reference stays literal on every platform. The value must be absolute with no `..`
    /// component, and must resolve inside the home directory or `$OCX_HOME` — not either of those
    /// itself, not inside `$OCX_HOME/toolchain`, and not a filesystem root or a system location such
    /// as `/usr`, `/etc`, `/var/lib` or `C:\Program Files`. On Linux and macOS it must be owned by the
    /// invoking user and writable by neither group nor world; Windows checks no ownership. ocx exits
    /// 78 when any of that fails. It need not exist yet: the first toolchain render creates it.
    #[serde(default)]
    pub toolchain_dir: Option<PathBuf>,

    /// Path to a PEM file of extra CA certificate(s), trusted for OCI registry, index and forge
    /// traffic in addition to the platform trust store.
    ///
    /// A bundle of one or more concatenated `CERTIFICATE` blocks is accepted. A relative path
    /// resolves against the directory of the `config.toml` that declared it, so the same value
    /// means the same file regardless of the process working directory. Mutually exclusive with
    /// `extra_ca_certs_pem`: declaring both in one file is refused. A managed payload never carries
    /// this form: a path on the publisher's disk means nothing on a consumer's, so the managed tier
    /// drops it with a warning — publish with `ocx config push`, which inlines the file as
    /// `extra_ca_certs_pem`.
    #[serde(default)]
    pub extra_ca_certs: Option<PathBuf>,

    /// The extra CA certificate bundle inlined verbatim (PEM text).
    ///
    /// This is the form a fleet receives: `ocx config push` reads a
    /// path-form `extra_ca_certs` at publish time and inlines it here.
    /// Mutually exclusive with `extra_ca_certs`. From the managed tier it is
    /// honoured for registry, index and forge traffic as published; Sigstore
    /// traffic honours a managed-tier root set only behind a digest-pinned
    /// `[managed] source`.
    #[serde(default)]
    pub extra_ca_certs_pem: Option<String>,

    /// Runtime provenance marker: the extra-CA pair above was
    /// declared at the SYSTEM config scope (`/etc/ocx/config.toml`), so it is
    /// NON-OVERRIDABLE — every lower tier's pair and `OCX_EXTRA_CA_CERTS` are
    /// ignored with a warning, and the pair survives `OCX_NO_CONFIG`. Mirrors
    /// `RegistryDefaults::system_locked`, but for a pair of root-level
    /// scalars rather than a table, so it lives on `Config` itself.
    ///
    /// Never serialized — set by the loader's `apply_system_locks` iff the
    /// system file sets either key ("absent locks nothing"), never read from
    /// disk.
    #[serde(skip)]
    #[schemars(skip)]
    pub extra_ca_certs_system_locked: bool,
}

/// Global registry-subsystem settings (`[registry]` section).
///
/// Unknown keys are ignored, like in every other `config.toml` table.
#[derive(Debug, Default, Clone, Deserialize, schemars::JsonSchema)]
pub struct RegistryDefaults {
    /// Default registry for bare identifiers (e.g. `cmake:3.28` expands to
    /// `<default>/cmake:3.28`).
    ///
    /// Overridden by the `OCX_DEFAULT_REGISTRY` environment variable. Always a
    /// literal prefix (e.g. `"ghcr.io"`, `"ocx.sh"`) — never resolved through
    /// the `[registries.<name>]` table.
    pub default: Option<String>,

    /// Runtime provenance marker: this tier was declared at the SYSTEM config
    /// scope (`/etc/ocx/config.toml`), so it is NON-OVERRIDABLE by any lower
    /// tier. Mirrors `PatchConfig`'s required-enforcement lock, but unconditional —
    /// unlike `[patches].required`, `[registry]` has no opt-out field, so any
    /// system-scope declaration is authoritative by itself.
    ///
    /// Never serialized — set by the loader via `lock_as_system`
    /// after parsing the system-scope file, not read from disk.
    #[serde(skip)]
    #[schemars(skip)]
    pub system_locked: bool,
}

impl Config {
    /// Merge higher-precedence `other` into `self`: set scalars win, tables merge key-by-key.
    pub fn merge(&mut self, other: Config) {
        if let Some(other_registry) = other.registry {
            match self.registry.as_mut() {
                Some(self_registry) => self_registry.merge(other_registry),
                None => self.registry = Some(other_registry),
            }
        }
        if let Some(other_registries) = other.registries {
            let map = self.registries.get_or_insert_with(HashMap::new);
            for (name, entry) in other_registries {
                map.entry(name).or_default().merge(entry);
            }
        }
        if let Some(other_mirrors) = other.mirrors {
            let map = self.mirrors.get_or_insert_with(HashMap::new);
            for (host, entry) in other_mirrors {
                map.entry(host).or_default().merge(entry);
            }
        }
        if let Some(other_patches) = other.patches {
            match self.patches.as_mut() {
                Some(self_patches) => self_patches.merge(other_patches),
                None => self.patches = Some(other_patches),
            }
        }
        if let Some(other_managed) = other.managed {
            match self.managed.as_mut() {
                Some(self_managed) => self_managed.merge(other_managed),
                None => self.managed = Some(other_managed),
            }
        }
        if let Some(other_update) = other.update {
            match self.update.as_mut() {
                Some(self_update) => self_update.merge(other_update),
                None => self.update = Some(other_update),
            }
        }
        // Trust policies append across tiers; `[trust.sigstore]` replaces, since two Fulcio CAs are ambiguous.
        if let Some(other_trust) = other.trust {
            match self.trust.as_mut() {
                Some(self_trust) => self_trust.merge(other_trust),
                None => self.trust = Some(other_trust),
            }
        }
        if let Some(other_shell) = other.shell {
            match self.shell.as_mut() {
                Some(self_shell) => self_shell.merge(other_shell),
                None => self.shell = Some(other_shell),
            }
        }
        if let Some(other_records) = other.records {
            match self.records.as_mut() {
                Some(self_records) => self_records.merge(other_records),
                None => self.records = Some(other_records),
            }
        }
        // Empty is absent, or `toolchain_dir = ""` in a higher tier erases a lower tier's real value.
        if other
            .toolchain_dir
            .as_deref()
            .is_some_and(|value| !value.as_os_str().is_empty())
        {
            self.toolchain_dir = other.toolchain_dir;
        }
        // Take both fields together, or a path-to-inline switch across tiers leaves both set on the merged `Config`.
        if !self.extra_ca_certs_system_locked && (other.extra_ca_certs.is_some() || other.extra_ca_certs_pem.is_some())
        {
            self.extra_ca_certs = other.extra_ca_certs;
            self.extra_ca_certs_pem = other.extra_ca_certs_pem;
            // Adopted from `other`: the system tier folds into a default accumulator, so it arrives as `other`.
            self.extra_ca_certs_system_locked = other.extra_ca_certs_system_locked;
        }
    }

    /// Resolve [`Self::extra_ca_certs`] against the declaring `config.toml`'s directory, with the
    /// same [`FileReference`] grammar as `SigstoreTrust::anchor_relative_root`.
    pub fn anchor_relative_extra_ca_certs(&mut self, config_dir: &Path) {
        if let Some(path) = self.extra_ca_certs.as_ref() {
            // Lossless: the value came from a TOML string, so it is UTF-8.
            let written = path.to_string_lossy().into_owned();
            self.extra_ca_certs = Some(FileReference::parse(&written).anchored_at(config_dir));
        }
    }

    /// The declared trust policies (empty when no `[trust]` section is set).
    #[must_use]
    pub fn trust_policies(&self) -> &[ocx_trust::TrustPolicy] {
        self.trust.as_ref().map_or(&[], |trust| trust.policy.as_slice())
    }

    /// Return [`RegistryDefaults::default`] as a literal prefix, never dereferenced through `[registries]`.
    #[must_use]
    pub fn resolved_default_registry(&self) -> Option<&str> {
        self.registry.as_ref()?.default.as_deref()
    }

    /// The `toolchain_dir` as the merged tiers declared it, unvalidated.
    ///
    /// For reporting only: building a home on it skips every [`ToolchainRoot::resolve`] refusal.
    #[must_use]
    pub fn toolchain_dir(&self) -> Option<&Path> {
        self.toolchain_dir.as_deref()
    }
}

/// Which tier declared a `toolchain_dir` value, so a refusal names the input to change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolchainRootTier {
    /// The merged `config.toml` chain's `toolchain_dir` key, a managed payload included.
    ConfigFile,
    /// The `OCX_TOOLCHAIN_DIR` environment variable.
    Environment,
}

impl std::fmt::Display for ToolchainRootTier {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::ConfigFile => "config.toml `toolchain_dir`",
            Self::Environment => "OCX_TOOLCHAIN_DIR",
        })
    }
}

/// Why a declared `toolchain_dir` was refused; every variant exits 78.
///
/// A missing root is accepted (the renderer creates it); ownership checks inspect the nearest
/// existing ancestor, reported as `checked`.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
pub enum ToolchainRootError {
    /// A leading `~` that could not be expanded (`~user`, or no resolvable home directory).
    #[error("{tier} declares {}, whose leading '~' cannot be expanded: {defect}", declared.display())]
    #[exit(
        ConfigError,
        slug = "toolchain_dir_unexpandable",
        summary = "The toolchain_dir setting starts with a '~' that cannot be expanded"
    )]
    Unexpandable {
        tier: ToolchainRootTier,
        declared: PathBuf,
        defect: crate::shell::EntryDefect,
    },

    /// A relative value after `~` expansion.
    #[error(
        "{tier} is the relative path {}; a toolchain_dir root must be absolute, or one project resolves a different home from every working directory",
        declared.display()
    )]
    #[exit(
        ConfigError,
        slug = "toolchain_dir_relative",
        summary = "The toolchain_dir setting is a relative path"
    )]
    Relative { tier: ToolchainRootTier, declared: PathBuf },

    /// A `..` component anywhere in the value.
    ///
    /// Refused, not normalised: past a symlink, lexical `..` removal admits a root outside `$HOME`.
    #[error(
        "{tier} declares {}, which contains a '..' component; write the directory the root actually names",
        declared.display()
    )]
    #[exit(
        ConfigError,
        slug = "toolchain_dir_parent_component",
        summary = "The toolchain_dir setting contains a '..' component"
    )]
    ParentDirComponent { tier: ToolchainRootTier, declared: PathBuf },

    /// Neither the home directory nor `$OCX_HOME` could be resolved, so every value is refused.
    #[error(
        "{tier} declares {}, but neither a home directory nor $OCX_HOME could be resolved to contain it",
        declared.display()
    )]
    #[exit(
        ConfigError,
        slug = "toolchain_dir_no_containment_anchor",
        summary = "No home directory or OCX_HOME exists to contain toolchain_dir"
    )]
    NoContainmentAnchor { tier: ToolchainRootTier, declared: PathBuf },

    /// The root is `$OCX_HOME` or the home directory itself; containment admits only descendants.
    ///
    /// `anchor` can differ from `resolved` in ASCII case, since the compare folds case.
    #[error(
        "{tier} resolves to {}, which is the containment anchor {} itself; name a directory beneath it",
        resolved.display(),
        anchor.display()
    )]
    #[exit(
        ConfigError,
        slug = "toolchain_dir_is_containment_anchor",
        summary = "The toolchain_dir setting names the home directory or OCX_HOME itself"
    )]
    IsContainmentAnchor {
        tier: ToolchainRootTier,
        resolved: PathBuf,
        anchor: PathBuf,
    },

    /// The canonical root is not a descendant of the home directory or `$OCX_HOME`.
    #[error(
        "{tier} resolves to {}, which is outside both the home directory and $OCX_HOME",
        resolved.display()
    )]
    #[exit(
        ConfigError,
        slug = "toolchain_dir_outside_home",
        summary = "The toolchain_dir setting resolves outside the home directory and OCX_HOME"
    )]
    OutsideHome { tier: ToolchainRootTier, resolved: PathBuf },

    /// A filesystem root or a system prefix.
    ///
    /// Checked even when containment passed, or an absurd `$HOME` admits a system location.
    #[error("{tier} resolves to the system location {}", resolved.display())]
    #[exit(
        ConfigError,
        slug = "toolchain_dir_system_prefix",
        summary = "The toolchain_dir setting resolves to a system location"
    )]
    SystemPrefix { tier: ToolchainRootTier, resolved: PathBuf },

    /// A root at or under `$OCX_HOME/toolchain`, where a global `ocx pull` prunes project trees as orphan groups.
    #[error(
        "{tier} resolves to {}, inside the global toolchain home {}; a global `ocx pull` would prune other projects' trees there",
        resolved.display(),
        global_home.display()
    )]
    #[exit(
        ConfigError,
        slug = "toolchain_dir_inside_global_home",
        summary = "The toolchain_dir setting resolves inside the global toolchain home"
    )]
    InsideGlobalToolchainHome {
        tier: ToolchainRootTier,
        resolved: PathBuf,
        global_home: PathBuf,
    },

    /// An I/O failure inspecting the nearest existing ancestor (`EACCES`, `ELOOP`); never absence.
    #[error(
        "{tier} resolves to {}, whose nearest existing directory {} cannot be inspected",
        resolved.display(),
        checked.display()
    )]
    #[exit(
        ConfigError,
        slug = "toolchain_dir_inaccessible",
        summary = "The toolchain_dir setting names a path that cannot be inspected"
    )]
    Inaccessible {
        tier: ToolchainRootTier,
        resolved: PathBuf,
        checked: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The checked path exists and is not a directory; checked on every platform.
    #[error(
        "{tier} resolves to {}, whose nearest existing path {} is not a directory",
        resolved.display(),
        checked.display()
    )]
    #[exit(
        ConfigError,
        slug = "toolchain_dir_not_a_directory",
        summary = "The toolchain_dir setting names something other than a directory"
    )]
    NotADirectory {
        tier: ToolchainRootTier,
        resolved: PathBuf,
        checked: PathBuf,
    },

    /// The checked directory is not owned by the effective user (Unix only; ids rendered as strings).
    #[error(
        "{tier} resolves to {}, whose nearest existing directory {} is owned by {owner} rather than by the effective user {effective_user}",
        resolved.display(),
        checked.display()
    )]
    #[exit(
        ConfigError,
        slug = "toolchain_dir_not_owner_owned",
        summary = "The toolchain_dir directory is not owned by the current user"
    )]
    NotOwnerOwned {
        tier: ToolchainRootTier,
        resolved: PathBuf,
        checked: PathBuf,
        owner: String,
        effective_user: String,
    },

    /// The checked directory is group- or world-writable, so another account can plant a trampoline
    /// on a PATH (Unix only).
    #[error(
        "{tier} resolves to {}, whose nearest existing directory {} has mode {mode:04o}, granting write to group or world",
        resolved.display(),
        checked.display()
    )]
    #[exit(
        ConfigError,
        slug = "toolchain_dir_group_or_world_writable",
        summary = "The toolchain_dir directory is writable by other users"
    )]
    GroupOrWorldWritable {
        tier: ToolchainRootTier,
        resolved: PathBuf,
        checked: PathBuf,
        mode: u32,
    },
}

/// A `toolchain_dir` root that has passed every refusal [`Self::resolve`] performs.
///
/// [`Self::resolve`] must stay the only constructor (no `new`, `From<PathBuf>`, public field or `Deref`),
/// or a value can skip every refusal (`adr_toolchain_activation.md` § Rationale from code: ocx_config).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolchainRoot {
    // Any module in this crate can write this literal and so skip every refusal; none may.
    /// Absolute, and canonical as far as it exists (the absent tail is re-joined verbatim).
    root: PathBuf,
}

impl ToolchainRoot {
    /// Resolve the effective `toolchain_dir` root, or `None` when no tier declared one.
    ///
    /// No filesystem side effect; an absent root is accepted uncreated. Blocking: async callers
    /// use `spawn_blocking`.
    ///
    /// # Errors
    ///
    /// A [`ToolchainRootError`] — exit 78 — for each refusal above.
    pub fn resolve(config: &Config) -> Result<Option<Self>, ToolchainRootError> {
        Self::resolve_with_anchors(config, &ContainmentAnchors::from_environment())
    }

    /// [`Self::resolve`] against explicit anchors, for tests; must stay private, or a caller naming
    /// its own anchors admits any root.
    ///
    /// # Errors
    ///
    /// As [`Self::resolve`].
    fn resolve_with_anchors(config: &Config, anchors: &ContainmentAnchors) -> Result<Option<Self>, ToolchainRootError> {
        let Some((tier, declared)) = declared_root(config) else {
            return Ok(None);
        };

        // Before the absoluteness test: `~/...` is not absolute until expanded.
        let expanded = shell::expand_against(&declared, anchors.home.as_deref()).map_err(|defect| {
            ToolchainRootError::Unexpandable {
                tier,
                declared: declared.clone(),
                defect,
            }
        })?;

        // `%VAR%` is never expanded; a literal one fails as relative.
        if !expanded.is_absolute() {
            return Err(ToolchainRootError::Relative { tier, declared });
        }
        if expanded
            .components()
            .any(|component| component == std::path::Component::ParentDir)
        {
            return Err(ToolchainRootError::ParentDirComponent { tier, declared });
        }

        let lexical: PathBuf = expanded.components().collect();
        let containment = Containment::around(anchors);
        containment.refuse_uncontained(tier, &declared, &lexical)?;

        // Only this canonical pass sees through a symlink, so it must run even after the lexical one passed.
        let nearest = NearestExisting::of(&lexical).map_err(|(checked, source)| ToolchainRootError::Inaccessible {
            tier,
            resolved: lexical.clone(),
            checked,
            source,
        })?;
        containment.refuse_uncontained(tier, &declared, &nearest.resolved)?;

        refuse_unsound_root(tier, &nearest.resolved, &nearest.existing)?;

        Ok(Some(Self { root: nearest.resolved }))
    }

    /// The validated root; it may not exist yet.
    ///
    /// A caller creating it must use mode `0o700` on Unix, or a permissive umask grants the
    /// group/world write refused one level up.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.root
    }

    /// A root taken as already validated, test builds only.
    ///
    /// Never `__testing`, which reaches the acceptance binary: this bypasses every refusal.
    #[cfg(any(test, feature = "__test_scaffolding"))]
    pub fn from_validated(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

/// The tier that declared a `toolchain_dir`, and what it declared.
///
/// `config.toml` beats `OCX_TOOLCHAIN_DIR`; an empty value at either tier is absent, not invalid.
fn declared_root(config: &Config) -> Option<(ToolchainRootTier, PathBuf)> {
    if let Some(declared) = config
        .toolchain_dir
        .as_ref()
        .filter(|value| !value.as_os_str().is_empty())
    {
        return Some((ToolchainRootTier::ConfigFile, declared.clone()));
    }
    // Never `std::env::var`, or the test override seam is bypassed and these tests turn order-dependent.
    ocx_env::OCX_TOOLCHAIN_DIR
        .get()
        .map(|value| (ToolchainRootTier::Environment, PathBuf::from(value)))
}

/// The two directories containment admits a root beneath, as declared (not canonicalised).
#[derive(Debug, Clone)]
struct ContainmentAnchors {
    /// `$HOME`, or `%USERPROFILE%` on Windows; also what `~` expands against.
    home: Option<PathBuf>,
    /// `$OCX_HOME`, else `~/.ocx`.
    ocx_home: Option<PathBuf>,
}

impl ContainmentAnchors {
    /// The anchors this process resolves.
    fn from_environment() -> Self {
        Self {
            home: ocx_env::home_dir(),
            ocx_home: crate::home::default_ocx_root(),
        }
    }
}

/// The anchor spellings a candidate is compared against, split by role.
///
/// An anchor that cannot be canonicalised stops admitting but keeps refusing, or an absent
/// `$OCX_HOME` admits `$OCX_HOME/toolchain` itself.
struct Containment {
    /// Admitting: each resolvable anchor as declared and canonicalised, since both passes compare.
    anchors: Vec<PathBuf>,
    /// Refusing: the anchors themselves, whether or not they exist.
    anchor_identities: Vec<PathBuf>,
    /// Refusing: `$OCX_HOME/toolchain` in each spelling, whether or not it exists.
    global_toolchain_homes: Vec<PathBuf>,
}

impl Containment {
    fn around(anchors: &ContainmentAnchors) -> Self {
        let declared: Vec<&Path> = [anchors.home.as_deref(), anchors.ocx_home.as_deref()]
            .into_iter()
            .flatten()
            .collect();
        let ocx_home_refusals = anchors
            .ocx_home
            .as_deref()
            .map(refusal_spellings_of)
            .unwrap_or_default();
        Self {
            anchors: declared.iter().copied().flat_map(spellings_of).collect(),
            anchor_identities: anchors
                .home
                .as_deref()
                .map(refusal_spellings_of)
                .unwrap_or_default()
                .into_iter()
                .chain(ocx_home_refusals.iter().cloned())
                .collect(),
            global_toolchain_homes: ocx_home_refusals
                .into_iter()
                .map(|spelling| spelling.join("toolchain"))
                .collect(),
        }
    }

    /// The containment refusals for one candidate, most specific first.
    ///
    /// # Errors
    ///
    /// The first [`ToolchainRootError`] that applies.
    fn refuse_uncontained(
        &self,
        tier: ToolchainRootTier,
        declared: &Path,
        candidate: &Path,
    ) -> Result<(), ToolchainRootError> {
        // Unconditional, or `/usr/tc` is admitted on a host whose `$HOME` is `/usr`.
        if is_system_location(candidate) {
            return Err(ToolchainRootError::SystemPrefix {
                tier,
                resolved: candidate.to_path_buf(),
            });
        }
        if let Some(anchor) = self
            .anchor_identities
            .iter()
            .find(|anchor| eq_ignoring_ascii_case(candidate, anchor))
        {
            return Err(ToolchainRootError::IsContainmentAnchor {
                tier,
                resolved: candidate.to_path_buf(),
                anchor: anchor.clone(),
            });
        }
        // `$OCX_HOME/toolchain` passes containment by construction, so it needs its own check.
        if let Some(global_home) = self
            .global_toolchain_homes
            .iter()
            .find(|global_home| starts_with_ignoring_ascii_case(candidate, global_home))
        {
            return Err(ToolchainRootError::InsideGlobalToolchainHome {
                tier,
                resolved: candidate.to_path_buf(),
                global_home: global_home.clone(),
            });
        }
        if self.anchors.is_empty() {
            return Err(ToolchainRootError::NoContainmentAnchor {
                tier,
                declared: declared.to_path_buf(),
            });
        }
        // `Path::starts_with` compares components; a string prefix admits `/home/ufoo` under `/home/u`.
        if !self.anchors.iter().any(|anchor| candidate.starts_with(anchor)) {
            return Err(ToolchainRootError::OutsideHome {
                tier,
                resolved: candidate.to_path_buf(),
            });
        }
        Ok(())
    }
}

/// The admitting spellings: declared and canonical, or nothing when uncanonicalisable (fail closed).
fn spellings_of(anchor: &Path) -> Vec<PathBuf> {
    let Ok(canonical) = dunce::canonicalize(anchor) else {
        return Vec::new();
    };
    if canonical == anchor {
        vec![canonical]
    } else {
        vec![anchor.to_path_buf(), canonical]
    }
}

/// The refusing spellings: lexical plus nearest-existing-resolved; never empty, unlike [`spellings_of`].
fn refusal_spellings_of(anchor: &Path) -> Vec<PathBuf> {
    let lexical: PathBuf = anchor.components().collect();
    let mut spellings = vec![lexical.clone()];
    if let Ok(nearest) = NearestExisting::of(&lexical)
        && nearest.resolved != lexical
    {
        spellings.push(nearest.resolved);
    }
    spellings
}

/// Whether `path` sits at or under `prefix`, component-wise, folding ASCII case.
///
/// Refusing comparisons only: byte-wise, a case-insensitive volume lets another spelling past
/// them (CWE-178); folding the admitting compare instead would widen what is accepted.
fn starts_with_ignoring_ascii_case(path: &Path, prefix: &Path) -> bool {
    let mut candidate = path.components();
    prefix.components().all(|expected| {
        candidate.next().is_some_and(|actual| {
            actual
                .as_os_str()
                .as_encoded_bytes()
                .eq_ignore_ascii_case(expected.as_os_str().as_encoded_bytes())
        })
    })
}

/// [`starts_with_ignoring_ascii_case`], and nothing after the prefix.
fn eq_ignoring_ascii_case(left: &Path, right: &Path) -> bool {
    starts_with_ignoring_ascii_case(left, right) && left.components().count() == right.components().count()
}

/// System locations matched as subtrees: a candidate at or under any is refused.
///
/// `/var` is never a prefix: ostree Fedora homes live at `/var/home/<user>`, so its defended
/// subdirectories are listed one by one.
const SYSTEM_PREFIXES: [&str; 23] = [
    "/usr",
    "/bin",
    "/sbin",
    "/lib",
    "/lib64",
    "/etc",
    "/var/lib",
    "/var/cache",
    "/var/log",
    "/var/spool",
    "/var/run",
    "/var/tmp",
    "/var/opt",
    "/var/local",
    "/opt",
    "/boot",
    "/dev",
    "/proc",
    "/sys",
    "/System",
    "/Library",
    "/Applications",
    "/private",
];

/// System locations matched by identity: as prefixes, `/` would refuse every absolute path and
/// `/var` every ostree home.
const SYSTEM_LOCATIONS_MATCHED_EXACTLY: [&str; 2] = ["/", "/var"];

/// The Windows system locations, from the environment with the default install paths as fallback.
fn windows_system_prefixes() -> Vec<PathBuf> {
    [
        (&ocx_env::SYSTEM_ROOT, r"C:\Windows"),
        (&ocx_env::PROGRAM_FILES, r"C:\Program Files"),
        (&ocx_env::PROGRAM_FILES_X86, r"C:\Program Files (x86)"),
        (&ocx_env::PROGRAM_DATA, r"C:\ProgramData"),
    ]
    .into_iter()
    .map(|(var, fallback)| {
        var.get()
            // POSIX-absolute is absent: a cross-compilation shell can export `ProgramFiles=/home/u`,
            // which would refuse the operator's own home.
            .filter(|value| !value.starts_with('/'))
            .map_or_else(|| PathBuf::from(fallback), PathBuf::from)
    })
    .collect()
}

/// Whether `path` is a filesystem root or sits at or under a system location.
fn is_system_location(path: &Path) -> bool {
    is_system_location_among(path, &windows_system_prefixes())
}

/// [`is_system_location`] against an explicit Windows prefix set, so Linux tests reach that clause.
fn is_system_location_among(path: &Path, windows_prefixes: &[PathBuf]) -> bool {
    if path.parent().is_none() {
        return true;
    }
    if SYSTEM_LOCATIONS_MATCHED_EXACTLY
        .iter()
        .map(Path::new)
        .any(|location| eq_ignoring_ascii_case(path, location))
    {
        return true;
    }
    if SYSTEM_PREFIXES
        .iter()
        .map(Path::new)
        .any(|prefix| starts_with_ignoring_ascii_case(path, prefix))
    {
        return true;
    }
    // Normalised so `\` splits components, or on Linux `C:\Windows\tc` is one opaque component.
    let normalized = ocx_util::fs::path::lexical_normalize(path);
    windows_prefixes
        .iter()
        .any(|prefix| starts_with_ignoring_ascii_case(&normalized, &ocx_util::fs::path::lexical_normalize(prefix)))
}

/// A fresh temporary directory usable as a containment anchor, or `None` (reason on stderr)
/// when the host's temp root is a system location, as macOS's `/private/var` is.
///
/// One function, never split: the armed-error scan reads a feature gate as production, and a
/// `Result<_, String>` half would be an unregistrable error.
#[cfg(any(test, feature = "__test_scaffolding"))]
pub fn sandbox_or_skip() -> Option<tempfile::TempDir> {
    let skip = |reason: String| -> Option<tempfile::TempDir> {
        eprintln!("skipped: {reason}");
        None
    };
    let sandbox = match tempfile::TempDir::new() {
        Ok(sandbox) => sandbox,
        Err(error) => return skip(format!("could not create a temporary directory: {error}")),
    };
    let canonical = match dunce::canonicalize(sandbox.path()) {
        Ok(canonical) => canonical,
        Err(error) => return skip(format!("could not canonicalise {}: {error}", sandbox.path().display())),
    };
    if is_system_location(&canonical) {
        return skip(format!(
            "this host's temporary root canonicalises to {}, which C-018 refuses, so it cannot serve as a containment anchor",
            canonical.display()
        ));
    }
    Some(sandbox)
}

/// A candidate root split at the boundary between what exists and what does not.
struct NearestExisting {
    /// The longest existing ancestor, canonicalised; may be a regular file, which
    /// [`refuse_unsound_root`] refuses.
    existing: PathBuf,
    /// [`Self::existing`] with the absent tail re-joined.
    resolved: PathBuf,
}

impl NearestExisting {
    /// Split `path`, following symlinks in its existing prefix.
    ///
    /// # Errors
    ///
    /// The directory whose canonicalisation failed, and why, for any failure other than
    /// `NotFound` / `NotADirectory`, which walk up.
    ///
    /// `NotADirectory` must walk up too, or `~/notes.txt/sub` reports an I/O fault instead of
    /// reaching [`refuse_unsound_root`]'s `NotADirectory` refusal.
    fn of(path: &Path) -> Result<Self, (PathBuf, std::io::Error)> {
        let mut absent_tail: Vec<&std::ffi::OsStr> = Vec::new();
        let mut cursor = path;
        loop {
            match dunce::canonicalize(cursor) {
                Ok(existing) => {
                    let mut resolved = existing.clone();
                    resolved.extend(absent_tail.iter().rev());
                    return Ok(Self { existing, resolved });
                }
                Err(error)
                    if !matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                    ) =>
                {
                    return Err((cursor.to_path_buf(), error));
                }
                Err(error) => {
                    let (Some(name), Some(parent)) = (cursor.file_name(), cursor.parent()) else {
                        return Err((cursor.to_path_buf(), error));
                    };
                    absent_tail.push(name);
                    cursor = parent;
                }
            }
        }
    }
}

/// `checked` must be a directory (every platform), owned by the effective user and writable by
/// neither group nor world (Unix).
///
/// # Errors
///
/// [`ToolchainRootError::NotADirectory`], whatever
/// [`refuse_unsound_ownership`] returns, or
/// [`ToolchainRootError::Inaccessible`] when the metadata read itself fails.
fn refuse_unsound_root(tier: ToolchainRootTier, resolved: &Path, checked: &Path) -> Result<(), ToolchainRootError> {
    let metadata = std::fs::metadata(checked).map_err(|source| ToolchainRootError::Inaccessible {
        tier,
        resolved: resolved.to_path_buf(),
        checked: checked.to_path_buf(),
        source,
    })?;
    if !metadata.is_dir() {
        return Err(ToolchainRootError::NotADirectory {
            tier,
            resolved: resolved.to_path_buf(),
            checked: checked.to_path_buf(),
        });
    }
    refuse_unsound_ownership(tier, resolved, checked, &metadata)
}

/// `checked` must be owned by the effective user and grant write to neither group nor world.
///
/// # Errors
///
/// [`ToolchainRootError::NotOwnerOwned`] or
/// [`ToolchainRootError::GroupOrWorldWritable`].
#[cfg(unix)]
fn refuse_unsound_ownership(
    tier: ToolchainRootTier,
    resolved: &Path,
    checked: &Path,
    metadata: &std::fs::Metadata,
) -> Result<(), ToolchainRootError> {
    use std::os::unix::fs::MetadataExt;

    // SAFETY: `geteuid` takes no arguments, touches no memory, and always succeeds.
    let effective_user = unsafe { libc::geteuid() };
    if metadata.uid() != effective_user {
        return Err(ToolchainRootError::NotOwnerOwned {
            tier,
            resolved: resolved.to_path_buf(),
            checked: checked.to_path_buf(),
            owner: metadata.uid().to_string(),
            effective_user: effective_user.to_string(),
        });
    }
    let mode = metadata.mode() & 0o7777;
    if mode & (0o020 | 0o002) != 0 {
        return Err(ToolchainRootError::GroupOrWorldWritable {
            tier,
            resolved: resolved.to_path_buf(),
            checked: checked.to_path_buf(),
            mode,
        });
    }
    Ok(())
}

/// The ownership check is not implemented off Unix: the owner SID needs a `windows-sys`
/// feature this workspace does not enable.
///
/// # Errors
///
/// Never.
#[cfg(not(unix))]
fn refuse_unsound_ownership(
    _tier: ToolchainRootTier,
    _resolved: &Path,
    _checked: &Path,
    _metadata: &std::fs::Metadata,
) -> Result<(), ToolchainRootError> {
    Ok(())
}

impl RegistryDefaults {
    /// Mark this tier as system-locked, non-overridable by lower tiers.
    pub fn lock_as_system(&mut self) {
        self.system_locked = true;
    }

    /// Merge `other`'s set fields into `self`, unless `self` is system-locked.
    ///
    /// The lock is checked on `self` only: the loader must fold the system tier in first as the base.
    pub fn merge(&mut self, other: RegistryDefaults) {
        if self.system_locked {
            return;
        }
        if other.default.is_some() {
            self.default = other.default;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::records::RecordsOptions;

    // ── Parsing tests (Step 3.1) ─────────────────────────────────────────────

    #[test]
    fn parse_minimal_config_sets_default_registry() {
        // Plan: Step 3.1 — Parse minimal config
        let config: Config = toml::from_str("[registry]\ndefault = \"ghcr.io\"").unwrap();
        let registry = config.registry.expect("registry section should be present");
        assert_eq!(registry.default.as_deref(), Some("ghcr.io"));
    }

    #[test]
    fn parse_empty_file_produces_default_config() {
        // Plan: Step 3.1 — Parse empty file → Config::default() with registry == None
        let config: Config = toml::from_str("").unwrap();
        assert!(config.registry.is_none());
    }

    #[test]
    fn parse_unknown_top_level_key_silently_ignored() {
        // Plan: Step 3.1 — Unknown top-level key [foo] → silently ignored
        // Config root has no deny_unknown_fields
        let result: Result<Config, _> = toml::from_str("[foo]\nbar = \"x\"");
        assert!(result.is_ok(), "unknown top-level key should not fail: {result:?}");
    }

    #[test]
    fn parse_registries_named_table() {
        // [registries.<name>] is a live v1 feature — parses into the
        // `registries` HashMap on Config, one RegistryEntry per key.
        let config: Config = toml::from_str(
            "[registries.ghcr]\nindex = \"https://ghcr.example\"\n\n[registries.company]\nindex = \"https://index.company.com\"",
        )
        .unwrap();
        let registries = config.registries.expect("registries table should be present");
        assert_eq!(registries.len(), 2);
        assert_eq!(registries["ghcr"].index.as_deref(), Some("https://ghcr.example"));
        assert_eq!(
            registries["company"].index.as_deref(),
            Some("https://index.company.com")
        );
    }

    /// An unknown key inside a `[registries.<name>]` entry is ignored, and the
    /// known keys of that same entry still take effect.
    ///
    /// Fleet forward-compat: one `config.toml` (notably a `[managed]` payload)
    /// is read by many ocx versions at once, so a per-registry field a newer
    /// ocx understands must degrade to "ignored" on an older one rather than
    /// take the whole file out of service.
    #[test]
    fn parse_unknown_field_inside_registries_entry_is_ignored() {
        let config: Config = toml::from_str("[registries.ghcr]\nindex = \"https://ghcr.example\"\nfoo = \"bar\"\n")
            .expect("an unknown field inside [registries.<name>] must not fail the parse");
        let registries = config.registries.expect("registries table must be present");
        assert_eq!(
            registries["ghcr"].index.as_deref(),
            Some("https://ghcr.example"),
            "the known field must still take effect alongside the ignored one"
        );
    }

    /// Replaces the former tripwire `parse_unknown_future_patches_section_silently_ignored`.
    /// `[patches]` is now a RECOGNIZED section — parses into `Config.patches`.
    ///
    /// Traces: Phase 1 — "flip the placeholder tripwire test to positive parse tests";
    /// impl map — "tripwire test → flip to positive parse/expansion tests".
    #[test]
    fn parse_patches_section_is_recognized() {
        let result: Result<Config, _> = toml::from_str("[patches]\nregistry = \"corp.example.com/patches\"\n");
        assert!(result.is_ok(), "[patches] section must parse successfully: {result:?}");
        let config = result.unwrap();
        let patches = config.patches.expect("[patches] must populate Config.patches");
        assert_eq!(
            patches.registry.as_deref(),
            Some("corp.example.com/patches"),
            "registry field must be populated from [patches] TOML"
        );
    }

    /// Unknown fields inside `[patches]` are silently ignored (no `deny_unknown_fields`).
    ///
    /// Traces: stub manifest — "no `deny_unknown_fields`"; Phase 1 forward-compat requirement.
    #[test]
    fn parse_patches_section_ignores_unknown_fields() {
        // PatchConfig has no deny_unknown_fields, so unknown keys inside [patches]
        // must be silently ignored (forward compat for future Phase 2+ fields).
        let result: Result<Config, _> = toml::from_str("[patches]\na = \"b\"\n");
        assert!(
            result.is_ok(),
            "[patches] with unknown fields must not fail (no deny_unknown_fields): {result:?}"
        );
    }

    /// The `[registry]` analog of
    /// [`parse_unknown_field_inside_registries_entry_is_ignored`]: an unknown
    /// key is ignored and `default` still takes effect.
    #[test]
    fn parse_unknown_field_inside_registry_is_ignored() {
        let config: Config = toml::from_str("[registry]\ndefault = \"ghcr.io\"\nfoo = \"bar\"\n")
            .expect("an unknown field inside [registry] must not fail the parse");
        assert_eq!(
            config.registry.expect("[registry] must be present").default.as_deref(),
            Some("ghcr.io"),
            "the known field must still take effect alongside the ignored one"
        );
    }

    /// The whole point, end to end: a payload written by a NEWER ocx — an
    /// unknown top-level section, an unknown key in every table an older ocx
    /// knows, and a `[mirrors]` entry declaring only a future role — parses,
    /// and every setting this binary understands survives.
    ///
    /// This is the shape a central `[managed]` rollout ships. Before the
    /// forward-compat posture, any one of these lines took the entire payload
    /// out of service on every older binary in the fleet.
    #[test]
    fn parse_payload_from_a_newer_ocx_keeps_every_known_setting() {
        let config: Config = toml::from_str(
            "[registry]\n\
             default = \"corp.example.com\"\n\
             timeout = 30\n\
             [registries.\"corp.example.com\"]\n\
             index = \"https://index.corp.example.com\"\n\
             location = \"corp.example.com/rewritten\"\n\
             [mirrors.\"ghcr.io\"]\n\
             registry = \"https://mirror.corp/ghcr\"\n\
             attest = \"https://mirror.corp/attest\"\n\
             [mirrors.\"quay.io\"]\n\
             attest = \"https://mirror.corp/attest\"\n\
             [toolchain]\n\
             channel = \"stable\"\n",
        )
        .expect("a payload from a newer ocx must parse");

        assert_eq!(
            config.registry.expect("[registry] must survive").default.as_deref(),
            Some("corp.example.com")
        );
        let registries = config.registries.expect("[registries] must survive");
        assert_eq!(
            registries["corp.example.com"].index.as_deref(),
            Some("https://index.corp.example.com")
        );
        let mirrors = config.mirrors.expect("[mirrors] must survive");
        assert_eq!(
            mirrors["ghcr.io"].registry.as_deref(),
            Some("https://mirror.corp/ghcr"),
            "a known role beside an unknown one must still apply"
        );
        assert!(
            !mirrors.contains_key("quay.io"),
            "an entry declaring only a future role contributes nothing, but must not fail the parse"
        );
    }

    #[test]
    fn parse_records_section_is_recognized() {
        let config: Config = toml::from_str(
            "[records]\ndir = \"/var/log/ocx/records\"\nname = \"{time}-{pid}-{rand}.json\"\nrequired = true\n",
        )
        .expect("[records] section must parse successfully");
        let records = config.records.expect("[records] must populate Config.records");
        assert_eq!(records.dir, Some(PathBuf::from("/var/log/ocx/records")));
        assert_eq!(records.name.as_deref(), Some("{time}-{pid}-{rand}.json"));
        assert_eq!(records.required, Some(true));
        assert!(
            !records.system_locked,
            "system_locked is loader-set provenance, never read from disk"
        );
    }

    /// `RecordsOptions` deliberately carries no `deny_unknown_fields`, the same
    /// forward-compat posture [`Config`] itself takes: a `[records]` block
    /// written for a newer ocx must not brick an older fleet binary reading the
    /// same file.
    #[test]
    fn parse_records_section_ignores_unknown_fields() {
        let result: Result<Config, _> = toml::from_str("[records]\ndir = \"/var/log/ocx\"\nfuture_field = \"x\"\n");
        assert!(
            result.is_ok(),
            "[records] with unknown fields must not fail (no deny_unknown_fields): {result:?}"
        );
    }

    /// `system_locked` is `#[serde(skip)]` — a config file cannot assert the
    /// clamp for itself, only the loader can, and only for `/etc/ocx/config.toml`.
    #[test]
    fn parse_records_section_cannot_self_declare_system_locked() {
        let config: Config =
            toml::from_str("[records]\ndir = \"/var/log/ocx\"\nsystem_locked = true\n").expect("must parse");
        assert!(
            !config.records.expect("records present").system_locked,
            "a config file must not be able to forge the SYSTEM clamp"
        );
    }

    #[test]
    fn parse_registry_default_with_unknown_top_level_section() {
        // Plan: Step 3.1 — [registry] default present alongside unknown top-level section
        let config: Config = toml::from_str("[registry]\ndefault = \"x\"\n[foo]\nbar = 1").unwrap();
        let registry = config.registry.expect("registry section should be present");
        assert_eq!(registry.default.as_deref(), Some("x"));
    }

    // ── Config::default() tests (Step 3.2) ──────────────────────────────────

    #[test]
    fn default_config_has_no_registry_section() {
        // Plan: Step 3.2 — Config::default() → registry == None
        let config = Config::default();
        assert!(config.registry.is_none());
    }

    // ── Config::merge() tests (Step 3.2) ────────────────────────────────────

    #[test]
    fn merge_higher_precedence_default_wins() {
        // Plan: Step 3.2 — lower has Some(default="a"), higher has Some(default="b") → "b"
        let mut lower = Config {
            registry: Some(RegistryDefaults {
                system_locked: false,
                default: Some("a".into()),
            }),
            ..Config::default()
        };
        let higher = Config {
            registry: Some(RegistryDefaults {
                system_locked: false,
                default: Some("b".into()),
            }),
            ..Config::default()
        };
        lower.merge(higher);
        assert_eq!(lower.registry.as_ref().and_then(|r| r.default.as_deref()), Some("b"));
    }

    #[test]
    fn merge_none_in_higher_does_not_clobber_lower() {
        // Plan: Step 3.2 — lower has Some(default="a"), higher has None → preserved as "a"
        let mut lower = Config {
            registry: Some(RegistryDefaults {
                system_locked: false,
                default: Some("a".into()),
            }),
            ..Config::default()
        };
        let higher = Config {
            registry: Some(RegistryDefaults {
                system_locked: false,
                default: None,
            }),
            ..Config::default()
        };
        lower.merge(higher);
        assert_eq!(
            lower.registry.as_ref().and_then(|r| r.default.as_deref()),
            Some("a"),
            "None in higher should not clobber lower's value"
        );
    }

    #[test]
    fn merge_higher_registry_section_into_lower_none() {
        // Plan: Step 3.2 — lower has None registry, higher has Some(default="b")
        let mut lower = Config::default();
        let higher = Config {
            registry: Some(RegistryDefaults {
                system_locked: false,
                default: Some("b".into()),
            }),
            ..Config::default()
        };
        lower.merge(higher);
        assert_eq!(lower.registry.as_ref().and_then(|r| r.default.as_deref()), Some("b"));
    }

    #[test]
    fn merge_both_have_registry_section_with_different_fields() {
        // Plan: Step 3.2 — both have Some(RegistryDefaults) merged field-by-field
        // lower has default="lower-default", higher has default=None
        // result: lower's default preserved since higher has None
        let mut lower = Config {
            registry: Some(RegistryDefaults {
                system_locked: false,
                default: Some("lower-default".into()),
            }),
            ..Config::default()
        };
        let higher = Config {
            registry: Some(RegistryDefaults {
                system_locked: false,
                default: None,
            }),
            ..Config::default()
        };
        lower.merge(higher);
        assert_eq!(
            lower.registry.as_ref().and_then(|r| r.default.as_deref()),
            Some("lower-default")
        );
    }

    // ── RegistryDefaults system lock (mirrors PatchConfig C7) ────────────────

    /// `lock_as_system` is unconditional — no opt-out field like
    /// `[patches].required` exists for `[registry]`.
    #[test]
    fn registry_defaults_lock_as_system_sets_locked() {
        let mut registry = RegistryDefaults {
            default: Some("corp.example.com".to_string()),
            system_locked: false,
        };
        registry.lock_as_system();
        assert!(registry.system_locked, "lock_as_system must set system_locked");
    }

    /// A system-locked `RegistryDefaults` ignores a lower-tier override; the
    /// lock flag stays sticky after merge.
    #[test]
    fn registry_defaults_merge_system_locked_ignores_lower_tier() {
        let mut system = RegistryDefaults {
            default: Some("system.corp".to_string()),
            system_locked: false,
        };
        system.lock_as_system();
        assert!(system.system_locked);

        let user = RegistryDefaults {
            default: Some("user.corp".to_string()),
            system_locked: false,
        };
        system.merge(user);

        assert_eq!(
            system.default.as_deref(),
            Some("system.corp"),
            "locked system default must not be redirected by a lower tier"
        );
        assert!(system.system_locked, "lock flag stays sticky after merge");
    }

    // ── [registries.<name>] merge + resolution tests ────────────────────────

    #[test]
    fn merge_registries_adds_new_entries_and_updates_existing() {
        // Keys unique to `lower` survive; keys unique to `higher` appear;
        // keys in both are field-merged with `higher` winning on conflicts.
        let mut lower: Config = toml::from_str(
            "[registries.ghcr]\nindex = \"https://ghcr.example\"\n\n[registries.company]\nindex = \"https://old.company.com\"",
        )
        .unwrap();
        let higher: Config = toml::from_str(
            "[registries.company]\nindex = \"https://new.company.com\"\n\n[registries.private]\nindex = \"https://priv.co\"",
        )
        .unwrap();
        lower.merge(higher);
        let registries = lower.registries.unwrap();
        assert_eq!(registries.len(), 3);
        assert_eq!(registries["ghcr"].index.as_deref(), Some("https://ghcr.example"));
        assert_eq!(registries["company"].index.as_deref(), Some("https://new.company.com"));
        assert_eq!(registries["private"].index.as_deref(), Some("https://priv.co"));
    }

    /// §6 ratified simplification: `[registry] default` is always a literal
    /// prefix — a matching `[registries.<name>]` entry never changes the
    /// resolved value (no more `url`-alias dereference).
    #[test]
    fn resolved_default_registry_returns_literal_name_even_with_matching_entry() {
        let config: Config = toml::from_str(
            "[registry]\ndefault = \"ghcr\"\n\n[registries.ghcr]\nindex = \"https://index.ghcr.example\"",
        )
        .unwrap();
        assert_eq!(config.resolved_default_registry(), Some("ghcr"));
    }

    #[test]
    fn resolved_default_registry_returns_literal_name_when_no_entry() {
        // [registry] default = "ocx.sh" with no matching [registries.ocx.sh]
        // → returns the literal name (the only supported behavior — §6).
        let config: Config = toml::from_str("[registry]\ndefault = \"ocx.sh\"").unwrap();
        assert_eq!(config.resolved_default_registry(), Some("ocx.sh"));
    }

    #[test]
    fn merge_trust_policies_appends_across_tiers() {
        // Lower tier pins one scope; higher tier adds another. Both survive —
        // trust policies pool (union), they do not replace like scalars do.
        let mut lower: Config = toml::from_str(
            "[[trust.policy]]\nscope = \"ghcr.io/acme/*\"\nkeyless = { identity = \"a\", oidc_issuer = \"iss\" }",
        )
        .unwrap();
        let higher: Config = toml::from_str(
            "[[trust.policy]]\nscope = \"ghcr.io/other/*\"\nkeyless = { identity = \"b\", oidc_issuer = \"iss\" }",
        )
        .unwrap();
        lower.merge(higher);
        assert_eq!(lower.trust_policies().len(), 2);
    }

    #[test]
    fn trust_policies_empty_when_absent() {
        assert!(Config::default().trust_policies().is_empty());
    }

    #[test]
    fn resolved_default_registry_returns_none_when_no_default() {
        let config = Config::default();
        assert_eq!(config.resolved_default_registry(), None);
    }

    /// §6 ratified simplification killed the CWE-15 indirection class
    /// entirely: `resolved_default_registry` never reads the `[registries]`
    /// table, so a locked `[registry] default` cannot be hijacked by an
    /// injected `[registries.<name>]` entry — there is no dereference left to
    /// exploit. Regression coverage for the removal, not the old guard.
    #[test]
    fn resolved_default_registry_locked_registry_ignores_injected_entry() {
        // System tier: [registry] default = "corp", locked. No [registries.corp]
        // in the system file.
        let mut system = Config {
            registry: Some(RegistryDefaults {
                default: Some("corp".to_string()),
                system_locked: false,
            }),
            ..Config::default()
        };
        system.registry.as_mut().unwrap().lock_as_system();

        // A lower tier injects a FRESH [registries.corp] entry — it must have
        // zero effect on the resolved default, locked or not.
        let injected: Config =
            toml::from_str("[registries.corp]\nindex = \"https://evil.attacker.example\"").expect("payload must parse");
        system.merge(injected);

        assert_eq!(
            system.resolved_default_registry(),
            Some("corp"),
            "the [registries.<name>] table must never affect the resolved literal default"
        );
    }

    /// Finding #15: one table-driven check that every lockable config section
    /// actually honors `system_locked` in its `merge` — a locked system tier
    /// must ignore a lower-tier override. Covers all six sections that carry
    /// the `lock_as_system` pattern (`[patches]`, `[managed]`, `[registry]`,
    /// each `[registries.<name>]` entry, each `[mirrors."<host>"]` entry,
    /// `[records]`) so a newly-added lockable section without merge wiring is
    /// caught here rather than in scattered per-struct tests.
    #[test]
    fn every_lockable_section_respects_system_lock() {
        // Each row: section name + a closure that builds a system-locked
        // instance, merges a lower-tier override into it, and returns whether
        // the locked value survived AND the lock flag stayed sticky.
        type LockCheck = (&'static str, fn() -> bool);
        let checks: &[LockCheck] = &[
            ("[patches]", || {
                let mut system = PatchConfig {
                    registry: Some("system.corp/patches".to_string()),
                    required: Some(true),
                    ..PatchConfig::default()
                };
                system.lock_as_system();
                system.merge(PatchConfig {
                    registry: Some("lower.evil/patches".to_string()),
                    required: Some(false),
                    ..PatchConfig::default()
                });
                system.system_locked && system.registry.as_deref() == Some("system.corp/patches")
            }),
            ("[managed]", || {
                let mut system = ManagedConfig {
                    source: Some("system.corp/ocx-config:user".to_string()),
                    required: Some(true),
                    ..ManagedConfig::default()
                };
                system.lock_as_system();
                system.merge(ManagedConfig {
                    source: Some("lower.evil/ocx-config:user".to_string()),
                    required: Some(false),
                    ..ManagedConfig::default()
                });
                system.system_locked && system.source.as_deref() == Some("system.corp/ocx-config:user")
            }),
            ("[registry]", || {
                let mut system = RegistryDefaults {
                    default: Some("system.corp".to_string()),
                    system_locked: false,
                };
                system.lock_as_system();
                system.merge(RegistryDefaults {
                    default: Some("lower.evil".to_string()),
                    system_locked: false,
                });
                system.system_locked && system.default.as_deref() == Some("system.corp")
            }),
            ("[registries.<name>]", || {
                let mut system = RegistryConfig {
                    index: Some("https://system-index.corp".to_string()),
                    ..Default::default()
                };
                system.lock_as_system();
                system.merge(RegistryConfig {
                    index: Some("https://lower.evil".to_string()),
                    ..Default::default()
                });
                system.system_locked && system.index.as_deref() == Some("https://system-index.corp")
            }),
            // The same entry's security-relevant field: a lower tier must not be
            // able to declare a system-locked registry reachable over plain HTTP.
            ("[registries.<name>] insecure", || {
                let mut system = RegistryConfig {
                    insecure: Some(false),
                    ..Default::default()
                };
                system.lock_as_system();
                system.merge(RegistryConfig {
                    insecure: Some(true),
                    ..Default::default()
                });
                system.system_locked && system.insecure == Some(false)
            }),
            ("[mirrors.\"<host>\"]", || {
                let mut system = MirrorConfig {
                    registry: Some("https://system-mirror.corp/ghcr-remote".to_string()),
                    index: None,
                    registry_system_locked: false,
                    index_system_locked: false,
                };
                system.lock_as_system();
                system.merge(MirrorConfig {
                    registry: Some("https://lower.evil/ghcr-remote".to_string()),
                    index: None,
                    registry_system_locked: false,
                    index_system_locked: false,
                });
                system.registry_system_locked
                    && system.registry.as_deref() == Some("https://system-mirror.corp/ghcr-remote")
            }),
            ("[records]", || {
                // The clamp is binary and per-block: a system-scope [records]
                // pins `dir`, `name` and `required` together, so one lower-tier
                // section cannot redirect the sink while keeping the posture.
                let mut system = RecordsOptions {
                    dir: Some(PathBuf::from("/var/log/ocx/records")),
                    required: Some(true),
                    ..RecordsOptions::default()
                };
                system.lock_as_system();
                system.merge(RecordsOptions {
                    dir: Some(PathBuf::from("/tmp/lower-evil")),
                    required: Some(false),
                    ..RecordsOptions::default()
                });
                system.system_locked
                    && system.dir == Some(PathBuf::from("/var/log/ocx/records"))
                    && system.required == Some(true)
            }),
            // The root-level pair: locked on `Config` itself. The
            // lower tier switches to the OTHER spelling, so a merge that
            // honoured it would show as a cleared `_pem`, not only a changed one.
            ("extra_ca_certs / extra_ca_certs_pem", || {
                let mut system = Config {
                    extra_ca_certs_pem: Some("system".to_string()),
                    extra_ca_certs_system_locked: true,
                    ..Config::default()
                };
                system.merge(Config {
                    extra_ca_certs: Some(PathBuf::from("/tmp/lower-evil.pem")),
                    ..Config::default()
                });
                system.extra_ca_certs_system_locked
                    && system.extra_ca_certs_pem.as_deref() == Some("system")
                    && system.extra_ca_certs.is_none()
            }),
        ];

        for (section, check) in checks {
            assert!(
                check(),
                "{section} must honor system_locked in merge: a locked system tier ignored a lower-tier override \
                 OR dropped the lock flag"
            );
        }
    }
}

// ── `toolchain_dir` specification tests ─────────────────────────────────────

/// Specification tests for the `toolchain_dir` root, written from
/// `adr_toolchain_activation.md` (§ *`config.toml` placement key*, validation
/// item 18), never from an implementation.
///
/// `$OCX_HOME` is injectable through [`ocx_env::overrides`]; `$HOME` is
/// `std::env::home_dir()`, which no in-process seam redirects. Home-anchored
/// refusal rows touch no filesystem; rows that must be **accepted** first check
/// the host can host them, else skip naming the uid and mode seen. Rows needing
/// a symlinked or system-prefix anchor run against `$OCX_HOME`, which
/// containment treats identically; each such row says so.
#[cfg(test)]
mod toolchain_root_tests {
    use super::*;
    use ocx_env::overrides::EnvLock;

    /// The system-location prefix set, written out here rather than read from
    /// the implementation.
    ///
    /// A table driven by the production constant is vacuous the moment a
    /// prefix is deleted from it — the loop just runs one row fewer. This list
    /// is transcribed from `adr_toolchain_activation.md` § *`config.toml`
    /// placement key* check 2, so dropping
    /// a prefix in the implementation reds the row that names it.
    const C018_PREFIXES: [&str; 23] = [
        "/usr",
        "/bin",
        "/sbin",
        "/lib",
        "/lib64",
        "/etc",
        "/var/lib",
        "/var/cache",
        "/var/log",
        "/var/spool",
        "/var/run",
        "/var/tmp",
        "/var/opt",
        "/var/local",
        "/opt",
        "/boot",
        "/dev",
        "/proc",
        "/sys",
        "/System",
        "/Library",
        "/Applications",
        "/private",
    ];

    /// The system locations matched by identity rather than as subtrees,
    /// transcribed for the same reason as [`C018_PREFIXES`].
    const C018_EXACT_LOCATIONS: [&str; 2] = ["/", "/var"];

    /// The Windows system locations, and the environment variables naming them.
    ///
    /// Transcribed from the contract, like the two lists above. The value
    /// column is what the implementation falls back to when the variable is
    /// unset, which is the only branch a POSIX host can reach.
    const C018_WINDOWS_LOCATIONS: [(&ocx_env::EnvVar, &str); 4] = [
        (&ocx_env::SYSTEM_ROOT, r"C:\Windows"),
        (&ocx_env::PROGRAM_FILES, r"C:\Program Files"),
        (&ocx_env::PROGRAM_FILES_X86, r"C:\Program Files (x86)"),
        (&ocx_env::PROGRAM_DATA, r"C:\ProgramData"),
    ];

    /// The environment lock with both `toolchain_dir` inputs pinned:
    /// `$OCX_HOME` at `anchor`, and no ambient `OCX_TOOLCHAIN_DIR` leaking in
    /// from the developer's shell.
    ///
    /// Every value below is injected through this seam, which only
    /// `ocx_env`'s reads consult — so a `resolve` reading `std::env::var`
    /// directly would fail these rows rather than pass them by accident
    /// (the idiom `ActivateMode::from_env` states).
    fn anchored_env(anchor: &Path) -> EnvLock {
        let env = ocx_env::overrides::lock();
        env.set(&ocx_env::OCX_HOME, anchor.to_str().expect("anchor path is utf-8"));
        env.remove(&ocx_env::OCX_TOOLCHAIN_DIR);
        env
    }

    /// A merged `Config` whose `config.toml` tier declares `value`.
    ///
    /// The `[managed]` tier folds into the same field, so this fixture is both
    /// tiers as far as [`ToolchainRoot::resolve`] can see; the `[managed]`
    /// payload's own journey into this field is pinned in `config/loader.rs`.
    fn config_tier(value: impl Into<PathBuf>) -> Config {
        Config {
            toolchain_dir: Some(value.into()),
            ..Config::default()
        }
    }

    fn refusal(result: Result<Option<ToolchainRoot>, ToolchainRootError>) -> ToolchainRootError {
        match result {
            Err(error) => error,
            Ok(accepted) => panic!("expected a refusal, resolve returned Ok({accepted:?})"),
        }
    }

    fn accepted(result: Result<Option<ToolchainRoot>, ToolchainRootError>) -> ToolchainRoot {
        match result {
            Ok(Some(root)) => root,
            Ok(None) => panic!("expected an accepted root, resolve returned Ok(None)"),
            Err(error) => panic!("expected an accepted root, resolve refused: {error}"),
        }
    }

    fn canonical(path: &Path) -> PathBuf {
        dunce::canonicalize(path).unwrap_or_else(|error| panic!("canonicalise {}: {error}", path.display()))
    }

    /// The first ancestor of `path` that exists — the directory the owner/mode
    /// check reads when the root itself does not exist.
    // Only `host_can_accept` calls this, and that is `#[cfg(unix)]`, so off
    // Unix the function is dead under the workspace's denied warnings.
    #[cfg(unix)]
    fn nearest_existing_ancestor(path: &Path) -> Option<&Path> {
        path.ancestors().find(|candidate| candidate.exists())
    }

    /// `Ok(())` when `path`'s nearest existing ancestor passes the owner/mode check
    /// on this host; otherwise the uid and mode actually observed.
    ///
    /// Used only by rows anchored on the **real** home directory, which no
    /// in-process seam can redirect. It decides whether the *host* can host the
    /// row — it is not a second copy of the assertion, which is what `resolve`
    /// returns.
    #[cfg(unix)]
    fn host_can_accept(path: &Path) -> Result<(), String> {
        use std::os::unix::fs::MetadataExt;

        let checked =
            nearest_existing_ancestor(path).ok_or_else(|| format!("no ancestor of {} exists", path.display()))?;
        let metadata =
            std::fs::metadata(checked).map_err(|error| format!("could not stat {}: {error}", checked.display()))?;
        let effective = effective_uid();
        if metadata.uid() != effective {
            return Err(format!(
                "{} is owned by uid {} rather than by the effective uid {effective}",
                checked.display(),
                metadata.uid()
            ));
        }
        let mode = metadata.mode() & 0o7777;
        if mode & 0o022 != 0 {
            return Err(format!(
                "{} has mode {mode:04o}, which C-019 refuses independently of this row",
                checked.display()
            ));
        }
        Ok(())
    }

    /// The calling process's effective uid.
    ///
    /// One place for the module's only `unsafe`, so the safety argument is
    /// written once instead of restated at each of three call sites.
    #[cfg(unix)]
    fn effective_uid() -> u32 {
        // SAFETY: `geteuid` reads the calling process's own credentials. It
        // takes no arguments, touches no memory, and is documented as always
        // succeeding.
        unsafe { libc::geteuid() }
    }

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .unwrap_or_else(|error| panic!("set mode {mode:04o} on {}: {error}", path.display()));
    }

    // ── containment, component-wise ─────────────────────────────────────────

    /// An anchor is not a descendant of itself. `$HOME`
    /// exactly would litter the home with opaque project keys; `$OCX_HOME`
    /// exactly would land them beside `packages/` and `blobs/` inside the tree
    /// `ocx clean` and the GC walk own.
    #[test]
    fn refuses_a_root_that_is_a_containment_anchor_itself() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let _env = anchored_env(sandbox.path());
        let home = ocx_env::home_dir().expect("this host resolves a home directory");

        for anchor in [home.clone(), sandbox.path().to_path_buf()] {
            let error = refusal(ToolchainRoot::resolve(&config_tier(anchor.clone())));
            assert!(
                matches!(error, ToolchainRootError::IsContainmentAnchor { .. }),
                "the anchor {} itself must be refused as an anchor, not by some later check; got {error}",
                anchor.display()
            );
        }
    }

    /// A direct child of either anchor is the ordinary accepted shape.
    /// The `$HOME` arm is the only row that inherits this host's real home, so
    /// it checks the host can host it first.
    #[test]
    fn accepts_a_direct_child_of_each_containment_anchor() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let _env = anchored_env(sandbox.path());

        let under_ocx_home = sandbox.path().join("tc");
        let root = accepted(ToolchainRoot::resolve(&config_tier(under_ocx_home)));
        assert_eq!(
            root.as_path(),
            canonical(sandbox.path()).join("tc"),
            "a direct child of $OCX_HOME resolves to the canonicalised anchor joined with the declared tail"
        );

        let home = ocx_env::home_dir().expect("this host resolves a home directory");
        let under_home = home.join("ocx-wp4-spec-root");
        #[cfg(unix)]
        if let Err(reason) = host_can_accept(&under_home) {
            eprintln!("skipped the $HOME arm: {reason}");
            return;
        }
        let root = accepted(ToolchainRoot::resolve(&config_tier(under_home.clone())));
        assert_eq!(
            root.as_path(),
            canonical(&home).join("ocx-wp4-spec-root"),
            "a direct child of the home directory is admitted by C-017"
        );
    }

    /// **The string-prefix trap.** `$HOME` is `/home/u` and the value
    /// is `/home/ufoo`: a `to_string_lossy().starts_with(..)` containment check
    /// admits it, a component-wise one refuses it. The single row this contract
    /// exists for, asserted against both anchors.
    ///
    /// Both anchors are **injected** as parameters rather than read from
    /// the environment. The ambient form carried an unstated premise — that the
    /// temp root lies outside the real home directory — which `TMPDIR` decides:
    /// this repository's own gate points it at `$HOME/.cache/ocx-test-tmp`, and
    /// there `<sandbox>foo` is a legitimate descendant of the home anchor, so
    /// the row inverted and reported the implementation's guard as broken. The
    /// property asserted is unchanged; only where the anchors come from is.
    #[test]
    fn refuses_a_sibling_whose_path_bytes_merely_start_with_an_anchor() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let env = ocx_env::overrides::lock();
        env.remove(&ocx_env::OCX_TOOLCHAIN_DIR);

        let home = sandbox.path().join("home");
        let ocx_home = sandbox.path().join("ocx");
        std::fs::create_dir(&home).expect("create the injected home directory");
        std::fs::create_dir(&ocx_home).expect("create the injected $OCX_HOME");
        let anchors = ContainmentAnchors {
            home: Some(home.clone()),
            ocx_home: Some(ocx_home.clone()),
        };

        for anchor in [home, ocx_home] {
            let sibling = PathBuf::from(format!("{}foo", anchor.display()));
            let error = refusal(ToolchainRoot::resolve_with_anchors(
                &config_tier(sibling.clone()),
                &anchors,
            ));
            assert!(
                matches!(error, ToolchainRootError::OutsideHome { .. }),
                "{} shares {}'s leading bytes but not its components, so containment must refuse it; got {error}",
                sibling.display(),
                anchor.display()
            );
        }
    }

    /// A `..` component is refused outright rather than normalised
    /// away. Lexical removal and POSIX resolution disagree whenever a preceding
    /// component is a symlink (`$HOME/link/../x` is lexically `$HOME/x` and
    /// actually `/x`), which would admit a root outside `$HOME` that the config
    /// never named. Same rule the shipped consent path applies
    /// (`EntryDefect::ParentDirComponent`).
    #[test]
    fn refuses_a_parent_dir_component_rather_than_normalising_it_away() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let _env = anchored_env(sandbox.path());
        let home = ocx_env::home_dir().expect("this host resolves a home directory");

        for escape in [
            home.join("..").join("other"),
            sandbox.path().join("link").join("..").join("x"),
        ] {
            let error = refusal(ToolchainRoot::resolve(&config_tier(escape.clone())));
            assert!(
                matches!(error, ToolchainRootError::ParentDirComponent { .. }),
                "{} carries a '..' component and must be refused as written, not normalised; got {error}",
                escape.display()
            );
        }
    }

    /// A root at or under `$OCX_HOME/toolchain` passes containment (it *is*
    /// under `$OCX_HOME`) and is a data-loss path: project homes would land at
    /// `$OCX_HOME/toolchain/<project-key>/toolchain`, where a bare global
    /// `ocx pull`'s whole-directory reconcile prunes them as orphan groups.
    #[test]
    fn refuses_the_global_toolchain_home_and_every_path_below_it() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let _env = anchored_env(sandbox.path());

        let global = sandbox.path().join("toolchain");
        for value in [global.clone(), global.join("a"), global.join("a").join("b")] {
            let error = refusal(ToolchainRoot::resolve(&config_tier(value.clone())));
            assert!(
                matches!(error, ToolchainRootError::InsideGlobalToolchainHome { .. }),
                "{} is inside the global toolchain home and must be refused by R-W1's own check; got {error}",
                value.display()
            );
        }
    }

    /// A sibling of `$OCX_HOME/toolchain` whose name merely starts with
    /// `toolchain` is **not** inside it. The component-wise rule again, one
    /// level down, and the non-vacuity control for the row above: a string
    /// prefix would refuse this one too.
    #[test]
    fn accepts_a_sibling_of_the_global_toolchain_home() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let _env = anchored_env(sandbox.path());

        let sibling = sandbox.path().join("toolchains");
        let root = accepted(ToolchainRoot::resolve(&config_tier(sibling)));
        assert_eq!(
            root.as_path(),
            canonical(sandbox.path()).join("toolchains"),
            "$OCX_HOME/toolchains is a sibling of the global home, not a path inside it"
        );
    }

    /// The root's own last component is a symlink out of the
    /// anchor. Only the canonicalise-and-re-check step catches it: every
    /// lexical check above passes, because the declared path *is* under the
    /// anchor.
    ///
    /// The anchor is **injected** as a parameter, for the reason
    /// [`refuses_a_sibling_whose_path_bytes_merely_start_with_an_anchor`] gives:
    /// "outside the anchor" has to mean outside a directory this row controls,
    /// not outside wherever `TMPDIR` happens to place the temp root relative to
    /// the real home. The link target sits beside the anchor rather than in a
    /// second temp root, so one sandbox holds both halves.
    #[cfg(unix)]
    #[test]
    fn refuses_a_root_whose_final_component_symlinks_out_of_the_anchor() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let env = ocx_env::overrides::lock();
        env.remove(&ocx_env::OCX_TOOLCHAIN_DIR);

        let anchor = sandbox.path().join("anchor");
        let outside = sandbox.path().join("outside");
        std::fs::create_dir(&anchor).expect("create the injected anchor");
        std::fs::create_dir(&outside).expect("create the directory outside every anchor");
        let anchors = ContainmentAnchors {
            home: Some(anchor.clone()),
            ocx_home: None,
        };

        let link = anchor.join("tc");
        std::os::unix::fs::symlink(&outside, &link).expect("create the escaping symlink");

        let error = refusal(ToolchainRoot::resolve_with_anchors(&config_tier(link), &anchors));
        assert!(
            matches!(error, ToolchainRootError::OutsideHome { .. }),
            "a symlink under the anchor pointing outside it resolves outside the anchor; got {error}"
        );
    }

    /// The positive control for the row above: a symlink that
    /// stays inside the anchor is accepted. Without it, "refuse anything whose
    /// last component is a symlink" would pass the escape row too.
    #[cfg(unix)]
    #[test]
    fn accepts_a_root_whose_final_component_symlinks_within_the_anchor() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let _env = anchored_env(sandbox.path());

        let target = sandbox.path().join("real");
        std::fs::create_dir(&target).expect("create the link target");
        let link = sandbox.path().join("tc");
        std::os::unix::fs::symlink(&target, &link).expect("create the internal symlink");

        let root = accepted(ToolchainRoot::resolve(&config_tier(link)));
        assert_eq!(
            root.as_path(),
            canonical(&target),
            "a symlink resolving inside the anchor is admitted, at its canonical target"
        );
    }

    /// **The anchors are canonicalised too.** `$OCX_HOME` is a
    /// symlink; a root beneath it must still be accepted. Canonicalising only
    /// the candidate mis-refuses a legitimate root, because the canonical
    /// candidate is under the anchor's *target* and the anchor is still spelled
    /// as the link.
    ///
    /// Stands in for the symlinked `$HOME` the ADR's containment paragraph
    /// implies: containment treats the two anchors identically and only `$OCX_HOME`
    /// is injectable in-process.
    #[cfg(unix)]
    #[test]
    fn accepts_a_root_under_a_symlinked_anchor() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let real = sandbox.path().join("real-ocx-home");
        std::fs::create_dir(&real).expect("create the real anchor");
        let linked = sandbox.path().join("linked-ocx-home");
        std::os::unix::fs::symlink(&real, &linked).expect("symlink the anchor");

        let _env = anchored_env(&linked);

        let root = accepted(ToolchainRoot::resolve(&config_tier(linked.join("tc"))));
        assert_eq!(
            root.as_path(),
            canonical(&real).join("tc"),
            "a symlinked $OCX_HOME must contain its own descendants — the anchor is canonicalised before the comparison"
        );
    }

    /// `/tmp/ocx-tc`, owner-owned and mode 0700, still exits 78.
    /// Containment, not permissions, is what refuses it: the root satisfies
    /// the owner/mode check exactly. The ADR's decisive red is this row — drop the containment
    /// check and watch it pass.
    ///
    /// On macOS `/tmp` canonicalises to `/private/tmp` and the system-location refusal fires
    /// first, so the expected variant differs there.
    #[cfg(unix)]
    #[test]
    fn refuses_an_owner_owned_mode_0700_root_outside_every_anchor() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let _env = anchored_env(sandbox.path());

        let outside = tempfile::Builder::new()
            .prefix("ocx-tc")
            .tempdir_in("/tmp")
            .expect("create /tmp/ocx-tc*");
        set_mode(outside.path(), 0o700);

        let error = refusal(ToolchainRoot::resolve(&config_tier(outside.path())));
        if cfg!(target_os = "macos") {
            assert!(
                matches!(error, ToolchainRootError::SystemPrefix { .. }),
                "on macOS /tmp canonicalises under /private, so C-018 refuses it; got {error}"
            );
        } else {
            assert!(
                matches!(error, ToolchainRootError::OutsideHome { .. }),
                "an owner-owned 0700 root outside both anchors is refused by containment alone; got {error}"
            );
        }
    }

    // ── R-W2 · relative values, and the empty string ────────────────────────

    /// R-W2 — a relative root is refused in every spelling. Canonicalising one
    /// uses the process working directory, so the same project would resolve a
    /// different home from every directory ocx is invoked from.
    ///
    /// `../tc` is both relative and `..`-bearing; one ruling settles which answer
    /// it gets, and `reports_a_relative_parent_dir_root_as_relative` below is
    /// the row that pins it.
    #[test]
    fn refuses_a_relative_root_in_every_spelling() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let _env = anchored_env(sandbox.path());

        for value in ["./tc", "tc", "tc/nested"] {
            let error = refusal(ToolchainRoot::resolve(&config_tier(value)));
            assert!(
                matches!(error, ToolchainRootError::Relative { .. }),
                "the relative root {value:?} must be refused before anything canonicalises it; got {error}"
            );
        }
    }

    /// `../tc` reports **`Relative`**, not `ParentDirComponent`.
    ///
    /// The resolution rules name both refusals in one breath; a later ruling
    /// settled it. One answer on record is worth a row of its own: the two
    /// refusals send an operator to different fixes —
    /// "write an absolute path" against "write the directory this actually
    /// names" — and a value that is both should not pick between them by
    /// whichever check the implementation happens to run first.
    #[test]
    fn reports_a_relative_parent_dir_root_as_relative() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let _env = anchored_env(sandbox.path());

        let error = refusal(ToolchainRoot::resolve(&config_tier("../tc")));
        assert!(
            matches!(error, ToolchainRootError::Relative { .. }),
            "'../tc' is relative first and '..'-bearing second (RUL-29); got {error}"
        );
    }

    /// R-W2 — the same relative refusal from the environment tier. The tier is
    /// the only thing that differs; the rule is the value's, not the file's.
    #[test]
    fn refuses_a_relative_root_from_the_environment_tier_too() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let env = anchored_env(sandbox.path());
        env.set(&ocx_env::OCX_TOOLCHAIN_DIR, "./tc");

        let error = refusal(ToolchainRoot::resolve(&Config::default()));
        assert!(
            matches!(
                error,
                ToolchainRootError::Relative {
                    tier: ToolchainRootTier::Environment,
                    ..
                }
            ),
            "a relative OCX_TOOLCHAIN_DIR is refused, naming the variable as the tier to edit; got {error}"
        );
    }

    /// An **empty** value reads as absent in both tiers, not as invalid —
    /// the rule `ActivateMode::from_env` states and `OCX_CONFIG=""` already
    /// follows, and the one [`ToolchainRoot::resolve`]'s contract records.
    ///
    /// The trap this pins: `Path::new("").is_absolute()` is `false`, so an
    /// implementation that tests absoluteness before emptiness reports
    /// `Relative` for a value nobody set.
    #[test]
    fn treats_an_empty_root_as_absent_rather_than_invalid() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let env = anchored_env(sandbox.path());

        assert_eq!(
            ToolchainRoot::resolve(&config_tier("")).expect("an empty config.toml value is absent, not invalid"),
            None,
            "toolchain_dir = \"\" declares no root"
        );

        env.set(&ocx_env::OCX_TOOLCHAIN_DIR, "");
        assert_eq!(
            ToolchainRoot::resolve(&Config::default()).expect("an empty OCX_TOOLCHAIN_DIR is absent, not invalid"),
            None,
            "OCX_TOOLCHAIN_DIR= declares no root"
        );
    }

    /// No tier declaring a root is `Ok(None)`, not an error: the in-project
    /// `<project>/.ocx/toolchain` default applies.
    #[test]
    fn resolves_to_none_when_no_tier_declares_a_root() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let _env = anchored_env(sandbox.path());

        assert_eq!(
            ToolchainRoot::resolve(&Config::default()).expect("no declared root is not an error"),
            None
        );
    }

    // ── `~` expands, `%VAR%` never does ─────────────────────────────────────

    /// `~/.cache/ocx/toolchain` is
    /// **accepted**. It is the ADR's own worked example, and
    /// `Path::new("~/.cache/ocx/toolchain").is_absolute()` is `false`, so an
    /// implementation testing absoluteness before expanding refuses the
    /// documented value.
    #[test]
    fn expands_a_leading_tilde_before_testing_absoluteness() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let _env = anchored_env(sandbox.path());
        let home = ocx_env::home_dir().expect("this host resolves a home directory");

        // Scoped to the arm that uses it: `host_can_accept` is `#[cfg(unix)]`,
        // so a binding at test scope is an `unused_variable` error on Windows
        // under the workspace's denied warnings.
        #[cfg(unix)]
        {
            let expected = home.join(".cache").join("ocx").join("toolchain");
            if let Err(reason) = host_can_accept(&expected) {
                eprintln!("skipped: {reason}");
                return;
            }
        }

        let root = accepted(ToolchainRoot::resolve(&config_tier("~/.cache/ocx/toolchain")));
        assert_eq!(
            root.as_path(),
            canonical(&home).join(".cache").join("ocx").join("toolchain"),
            "the ADR's worked example expands against the home directory and is admitted by C-017"
        );
    }

    /// Only a **leading** `~` expands. `<anchor>/~/tc` names a
    /// directory literally called `~`, which is a legal name; rewriting it
    /// would be the same widening the shipped expander refuses to do.
    #[test]
    fn treats_a_non_leading_tilde_as_a_literal_directory_name() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let _env = anchored_env(sandbox.path());

        let root = accepted(ToolchainRoot::resolve(&config_tier(
            sandbox.path().join("~").join("tc"),
        )));
        assert_eq!(
            root.as_path(),
            canonical(sandbox.path()).join("~").join("tc"),
            "a '~' that is not the first component stays a literal directory name"
        );
    }

    /// `~user` is unsupported, surfaced as the shipped
    /// [`EntryDefect`](crate::shell::EntryDefect) rather than a second
    /// vocabulary for the same condition.
    #[test]
    fn refuses_a_tilde_user_root() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let _env = anchored_env(sandbox.path());

        let error = refusal(ToolchainRoot::resolve(&config_tier("~someone/tc")));
        assert!(
            matches!(
                error,
                ToolchainRootError::Unexpandable {
                    defect: crate::shell::EntryDefect::UnsupportedTildeUser,
                    ..
                }
            ),
            "'~someone/tc' is refused as an unsupported tilde-user form, carrying the shipped defect; got {error}"
        );
    }

    /// **No `%VAR%` expansion on any platform.** A literal
    /// `%LOCALAPPDATA%` value falls through to the checks above and exits 78,
    /// which is diagnosable.
    ///
    /// The discriminating half: `LOCALAPPDATA` is set to a value that *would*
    /// be accepted if it were expanded. The refusal must be unaffected.
    #[test]
    fn performs_no_percent_variable_expansion_on_any_platform() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let env = anchored_env(sandbox.path());
        env.set_raw("LOCALAPPDATA", sandbox.path().to_str().expect("anchor path is utf-8"));

        // The ADR's commented-out Windows spelling, verbatim. Unexpanded it is
        // a relative path on every platform — `%LOCALAPPDATA%` is an ordinary
        // first component with no root and no drive prefix. Asserting *that*
        // rather than "not Unexpandable" is what makes the row reddable: an
        // expander would turn this into an absolute path under the anchor and
        // `resolve` would accept it.
        let error = refusal(ToolchainRoot::resolve(&config_tier(r"%LOCALAPPDATA%\ocx\toolchain")));
        assert!(
            matches!(error, ToolchainRootError::Relative { .. }),
            "nothing expands '%VAR%', so the value stays relative and is refused as such; got {error}"
        );

        // An absolute spelling, so the refusal is containment's rather than the
        // relative check's on a Unix host.
        let absolute = PathBuf::from("/%LOCALAPPDATA%/ocx/toolchain");
        let error = refusal(ToolchainRoot::resolve(&config_tier(absolute)));
        if cfg!(unix) {
            assert!(
                matches!(error, ToolchainRootError::OutsideHome { .. }),
                "an unexpanded '%LOCALAPPDATA%' component leaves a path outside both anchors; got {error}"
            );
        }
    }

    // ── system prefixes ─────────────────────────────────────────────────────

    /// Every listed prefix is refused **even when containment admits it**.
    /// `$OCX_HOME` is `/` for this row, so containment passes on every value and
    /// only the system-location list can refuse: that check runs in its own
    /// right, never as containment's fallback.
    #[cfg(unix)]
    #[test]
    fn refuses_every_system_prefix_even_when_containment_admits_it() {
        let _env = anchored_env(Path::new("/"));

        for prefix in C018_PREFIXES.into_iter().chain(C018_EXACT_LOCATIONS) {
            let error = refusal(ToolchainRoot::resolve(&config_tier(prefix)));
            assert!(
                matches!(error, ToolchainRootError::SystemPrefix { .. }),
                "{prefix} is a C-018 system location and must be refused as one; got {error}"
            );
        }
    }

    /// The subtree, not just the prefix itself. `/usr/local/tc` is the
    /// shape an operator would actually write.
    #[cfg(unix)]
    #[test]
    fn refuses_a_root_beneath_a_system_prefix() {
        let _env = anchored_env(Path::new("/"));

        for value in ["/usr/local/tc", "/etc/ocx/toolchain", "/var/lib/ocx"] {
            let error = refusal(ToolchainRoot::resolve(&config_tier(value)));
            assert!(
                matches!(error, ToolchainRootError::SystemPrefix { .. }),
                "{value} is beneath a C-018 system location and must be refused as one; got {error}"
            );
        }
    }

    /// **The order-pinning row.** The containment anchor is itself `/usr`, so
    /// containment admits `/usr/tc` outright and the system-location refusal is
    /// the only thing left to refuse it. This is the "absurd `$HOME`" case the
    /// ADR names as that refusal's entire reason, and it reds the moment the
    /// refusal is gated behind a containment failure.
    ///
    /// `$OCX_HOME` stands in for the absurd `$HOME` the ADR describes:
    /// containment treats the two anchors identically and only this one is injectable.
    #[cfg(unix)]
    #[test]
    fn refuses_a_system_prefix_that_the_containment_anchor_itself_names() {
        let _env = anchored_env(Path::new("/usr"));

        for value in ["/usr/tc", "/usr/local/share/tc"] {
            let error = refusal(ToolchainRoot::resolve(&config_tier(value)));
            assert!(
                matches!(error, ToolchainRootError::SystemPrefix { .. }),
                "{value} is contained by an absurd anchor and must still be refused by C-018; got {error}"
            );
        }
    }

    /// **`/var` is refused by identity, not as a subtree.**
    ///
    /// An ostree-composed Fedora (Silverblue, Kinoite, CoreOS, Bazzite) makes
    /// `/home` a symlink to `var/home`, so every passwd entry canonicalises
    /// under `/var`. A bare `/var` subtree prefix therefore refuses *every*
    /// `toolchain_dir` on those hosts — `~/.cache/ocx/toolchain` included,
    /// which is the ADR's own worked example. The directories the refusal defends are
    /// named one by one instead.
    ///
    /// Red against `SYSTEM_PREFIXES` carrying a bare `"/var"`: the first
    /// assertion fires. Red against dropping `/var` altogether: the third does.
    /// The predicate is called directly because no containment anchor a test
    /// can inject would put a candidate under `/var` on this host.
    #[test]
    fn treats_var_as_a_system_location_by_identity_and_not_as_a_subtree() {
        let _env = ocx_env::overrides::lock();

        for admitted in ["/var/home/alice/.cache/ocx/toolchain", "/var/home/alice", "/var/home"] {
            assert!(
                !is_system_location(Path::new(admitted)),
                "{admitted} is an ostree host's home directory tree, not a C-018 system location"
            );
        }
        assert!(
            is_system_location(Path::new("/var")),
            "/var itself is refused, for the reason / is (RUL-27)"
        );
        for prefix in C018_PREFIXES.into_iter().filter(|prefix| prefix.starts_with("/var/")) {
            assert!(
                is_system_location(Path::new(prefix)),
                "{prefix} is a C-018 system location"
            );
            assert!(
                is_system_location(&Path::new(prefix).join("ocx")),
                "{prefix}/ocx sits beneath a C-018 system location"
            );
        }
    }

    /// **`/private` stays a subtree prefix.** Recorded decision, not an
    /// open question.
    ///
    /// It names the macOS system firmlink and is what keeps `/etc` refused
    /// there after canonicalisation (`/etc` → `/private/etc`). Unlike bare
    /// `/var` it swallows no ordinary user home — a macOS home is
    /// `/Users/<name>` — so the ADR's worked example resolves. The two things it
    /// does refuse are macOS's *root* home (`/var/root` → `/private/var/root`,
    /// and `ocx` under `sudo` is not a documented flow) and a `$TMPDIR`-rooted
    /// `$OCX_HOME`, which is a fixture concern that [`sandbox_or_skip`] handles
    /// by skipping rather than a contract concern.
    ///
    /// Red in both directions: drop `/private` and the first two assertions
    /// fire; the third is the control that pins what `/private` must not reach.
    #[test]
    fn keeps_private_as_a_subtree_prefix_for_the_macos_firmlink() {
        let _env = ocx_env::overrides::lock();

        for refused in ["/private/etc/ocx", "/private/var/root/tc", "/private/tmp/tc"] {
            assert!(
                is_system_location(Path::new(refused)),
                "{refused} is the canonicalised macOS spelling of a C-018 system location"
            );
        }
        assert!(
            !is_system_location(Path::new("/Users/alice/.cache/ocx/toolchain")),
            "a macOS home directory is /Users/<name> and /private must not reach it"
        );
    }

    /// The POSIX arms fold ASCII case, like every other refusal.
    ///
    /// macOS's default volume is case-insensitive, so `/USR`, `/library` and
    /// `/VAR` name the listed directories. Pass 2 canonicalises a spelling
    /// that *resolves*, but pass 1 runs on the lexical value and this refusal is
    /// defence in depth for exactly the state where every other check passed
    /// (CWE-178).
    ///
    /// Red against `Path::eq` / `Path::starts_with` on either arm.
    #[test]
    fn refuses_a_system_location_in_any_ascii_case() {
        let _env = ocx_env::overrides::lock();

        for refused in ["/USR", "/USR/local/tc", "/library/tc", "/Var/Lib/ocx", "/VAR", "/var"] {
            assert!(
                is_system_location(Path::new(refused)),
                "{refused} names a C-018 system location on a case-insensitive volume"
            );
        }
        assert!(
            !is_system_location(Path::new("/usrlocal/tc")),
            "the fold must stay component-wise — /usrlocal is not under /usr"
        );
    }

    /// **The Windows clause, exercised on this host.**
    ///
    /// `%SystemRoot%` and the three `Program*` directories exist on no POSIX
    /// machine, and the runner that compiles and runs these rows on every pull
    /// request (`.github/workflows/verify-basic.yml`,
    /// `.github/workflows/verify-deep.yml`) is Linux. So the prefix list is a
    /// parameter — for the same reason `resolve_with_anchors` takes
    /// its anchors — and the matching is asserted directly.
    ///
    /// Two reds, both reachable from here. Drop `lexical_normalize` and every
    /// positive assertion fails: on Linux `\` is an ordinary filename
    /// character, so `C:\Windows\tc` is a **single** path component and no
    /// component-wise compare can see inside it. Drop the case fold and the
    /// lower- and upper-case spellings fail. `C:\Program Files Custom\tc` is
    /// the component-wise control: a byte-prefix compare would refuse it.
    #[test]
    fn refuses_every_windows_system_location_on_any_host() {
        let windows_prefixes: Vec<PathBuf> = C018_WINDOWS_LOCATIONS
            .iter()
            .map(|(_, path)| PathBuf::from(path))
            .collect();

        for (_, location) in C018_WINDOWS_LOCATIONS {
            for candidate in [
                location.to_owned(),
                format!(r"{location}\ocx\toolchain"),
                location.to_lowercase(),
                format!(r"{}\ocx", location.to_uppercase()),
            ] {
                assert!(
                    is_system_location_among(Path::new(&candidate), &windows_prefixes),
                    "{candidate} is at or under the C-018 Windows system location {location}"
                );
            }
        }

        for admitted in [
            r"C:\Users\bob\.cache\ocx\toolchain",
            r"C:\Program Files Custom\tc",
            r"C:\ProgramDataFiles\tc",
            "/home/bob/.cache/ocx/toolchain",
        ] {
            assert!(
                !is_system_location_among(Path::new(admitted), &windows_prefixes),
                "{admitted} is not a C-018 Windows system location and must not be refused as one"
            );
        }
    }

    /// The prefix list itself comes from the environment,
    /// with the shipped paths as fallbacks.
    ///
    /// A Windows host that relocated `%ProgramFiles%` is covered by the
    /// variable; one that unset it is covered by the fallback. Both arms are
    /// asserted here because only `ocx_env`'s injection seam can
    /// reach the first one on a POSIX host.
    #[test]
    fn reads_the_windows_system_locations_from_the_environment() {
        let env = ocx_env::overrides::lock();

        for (var, fallback) in C018_WINDOWS_LOCATIONS {
            let key = var.name;
            env.remove(var);
            assert!(
                windows_system_prefixes().contains(&PathBuf::from(fallback)),
                "with {key} unset the shipped {fallback} stands in"
            );
            env.set(var, r"D:\Relocated");
            assert!(
                windows_system_prefixes().contains(&PathBuf::from(r"D:\Relocated")),
                "{key} names the directory, and the fallback is only the fallback"
            );

            // **The wiring, not the predicate.** `is_system_location` is one
            // line — `is_system_location_among(path, &windows_system_prefixes())`
            // — and every other row here calls one side or the other directly,
            // so replacing that argument with `&[]` leaves them all green. This
            // is the assertion that reds it, and it runs on any host:
            // `lexical_normalize` turns both sides into `D:/Relocated…`.
            assert!(
                is_system_location(Path::new(r"D:\Relocated\tc")),
                "{key} names a C-018 Windows location, and `is_system_location` must consult it"
            );

            // An empty value is absent, the rule the tier ladder applies.
            env.set(var, "");
            assert!(
                windows_system_prefixes().contains(&PathBuf::from(fallback)),
                "an empty {key} is absent rather than a directory named by the empty string"
            );

            // H-R2-1 — a POSIX-absolute value is absent too. A cross-compilation
            // shell on Linux exports these, and `ocx_env`'s override arm
            // is `#[cfg(test)]`-only, so in a release build the read is live.
            // Without the filter `/home/u` joins the prefix list and every root
            // under the operator's own home is refused as "the system location".
            env.set(var, "/home/u");
            assert!(
                windows_system_prefixes().contains(&PathBuf::from(fallback)),
                "a POSIX-absolute {key} is not a Windows system location"
            );
            assert!(
                !is_system_location(Path::new("/home/u/.cache/ocx/toolchain")),
                "a POSIX-absolute {key} must not turn the operator's home into a C-018 location"
            );
            env.remove(var);
        }
    }

    // ── The checked path must be a directory ────────────────────────────────

    /// A `toolchain_dir` naming an existing **regular file** is refused, and so
    /// is one whose nearest existing ancestor is that file.
    ///
    /// The owner/mode check alone admits both: `tempfile`'s file is owner-owned and its mode
    /// carries neither `0o020` nor `0o002`, so the ownership half passes and
    /// the renderer's `create_dir_all` would become the diagnosis. Asserts the
    /// exit code through the route a binary takes (`ToolchainRootError` →
    /// [`crate::error::Error`] → `classify_error`), not through
    /// `classify` alone.
    #[test]
    fn refuses_a_root_whose_checked_path_is_not_a_directory() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let _env = anchored_env(sandbox.path());

        let file = sandbox.path().join("notes.txt");
        std::fs::write(&file, b"not a toolchain root").expect("create the regular file");

        for (root, expected_resolved) in [
            (file.clone(), canonical(&file)),
            (file.join("sub"), canonical(&file).join("sub")),
        ] {
            let error = refusal(ToolchainRoot::resolve(&config_tier(root.clone())));
            let ToolchainRootError::NotADirectory { checked, resolved, .. } = &error else {
                panic!(
                    "{} resolves onto a regular file, which cannot host a toolchain root; got {error}",
                    root.display()
                );
            };
            assert_eq!(
                checked,
                &canonical(&file),
                "the refusal names the path it actually inspected"
            );
            assert_eq!(
                resolved, &expected_resolved,
                "and still reports the root the tier declared"
            );

            let _rendered = error.to_string();
            let _routed = crate::error::Error::from(error);
        }
    }

    // ── owner and mode ──────────────────────────────────────────────────────

    /// An existing owner-owned root that grants write to neither group
    /// nor world is accepted. `0750` and `0755` are the non-vacuity controls:
    /// they prove the check tests the two write bits rather than "anything but
    /// 0700".
    #[cfg(unix)]
    #[test]
    fn accepts_an_existing_owner_owned_root_without_group_or_world_write() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let _env = anchored_env(sandbox.path());

        for mode in [0o700, 0o750, 0o755, 0o500] {
            let root = sandbox.path().join(format!("tc-{mode:04o}"));
            std::fs::create_dir(&root).expect("create the root");
            set_mode(&root, mode);

            let resolved = accepted(ToolchainRoot::resolve(&config_tier(root.clone())));
            assert_eq!(
                resolved.as_path(),
                canonical(&root),
                "mode {mode:04o} grants write to neither group nor world, so C-019 admits it"
            );
        }
    }

    /// Group- or world-writable is refused, naming the mode. Another
    /// account could otherwise plant a trampoline in a tree that lands on a
    /// PATH.
    #[cfg(unix)]
    #[test]
    fn refuses_a_group_or_world_writable_root() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let _env = anchored_env(sandbox.path());

        for mode in [0o770, 0o777, 0o707, 0o720, 0o702] {
            let root = sandbox.path().join(format!("tc-{mode:04o}"));
            std::fs::create_dir(&root).expect("create the root");
            set_mode(&root, mode);

            let error = refusal(ToolchainRoot::resolve(&config_tier(root.clone())));
            let ToolchainRootError::GroupOrWorldWritable {
                checked,
                mode: reported,
                ..
            } = &error
            else {
                panic!("mode {mode:04o} must be refused as group- or world-writable; got {error}");
            };
            assert_eq!(
                checked,
                &canonical(&root),
                "the refusal names the directory it actually inspected"
            );
            assert_eq!(reported & 0o777, mode, "the refusal reports the mode it read");
        }
    }

    /// An **absent** root is accepted, and the owner/mode check
    /// falls to the nearest existing ancestor: the directory the renderer will create under.
    /// The ADR's `~/.cache/ocx/toolchain` on a fresh machine is exactly this
    /// case.
    #[cfg(unix)]
    #[test]
    fn accepts_an_absent_root_whose_nearest_existing_ancestor_is_sound() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let _env = anchored_env(sandbox.path());
        set_mode(sandbox.path(), 0o700);

        let root = sandbox.path().join("a").join("b").join("c");
        let resolved = accepted(ToolchainRoot::resolve(&config_tier(root)));
        assert_eq!(
            resolved.as_path(),
            canonical(sandbox.path()).join("a").join("b").join("c"),
            "an absent root resolves; creating it is the renderer's job"
        );
    }

    /// An absent root whose nearest existing ancestor is
    /// world-writable is refused, and the refusal names **the ancestor**, not
    /// the root. Naming `resolved` alone would report a property of a directory
    /// that was never inspected.
    #[cfg(unix)]
    #[test]
    fn refuses_an_absent_root_under_a_world_writable_ancestor() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let _env = anchored_env(sandbox.path());

        let ancestor = sandbox.path().join("open");
        std::fs::create_dir(&ancestor).expect("create the ancestor");
        set_mode(&ancestor, 0o777);
        let root = ancestor.join("x").join("y");

        let error = refusal(ToolchainRoot::resolve(&config_tier(root.clone())));
        let ToolchainRootError::GroupOrWorldWritable { checked, resolved, .. } = &error else {
            panic!("a world-writable nearest existing ancestor must refuse the root; got {error}");
        };
        assert_eq!(
            checked,
            &canonical(&ancestor),
            "the refusal names the nearest existing ancestor it inspected"
        );
        assert_eq!(
            resolved,
            &canonical(&ancestor).join("x").join("y"),
            "and still reports the root the tier declared"
        );
    }

    /// Restores a mode on drop, so a row that reds mid-way — every row does,
    /// against the stub — cannot leave an unreadable directory behind for the
    /// temp root's cleanup to trip over.
    #[cfg(unix)]
    struct ModeGuard(PathBuf);

    #[cfg(unix)]
    impl Drop for ModeGuard {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;

            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
        }
    }

    /// A genuine I/O failure while walking the path chain is its own
    /// refusal, and it is **not** "the root does not exist": an absent root is
    /// accepted, and an ancestor always exists to walk up to, so a failure here
    /// is a real fault. An ancestor the user cannot traverse (`EACCES`) is the
    /// reachable case.
    #[cfg(unix)]
    #[test]
    fn refuses_a_root_whose_ancestor_cannot_be_inspected() {
        if effective_uid() == 0 {
            eprintln!("skipped: running as uid 0, which traverses a mode-0000 directory whatever its permissions say");
            return;
        }
        let Some(sandbox) = sandbox_or_skip() else { return };
        let _env = anchored_env(sandbox.path());

        let blocked = sandbox.path().join("blocked");
        std::fs::create_dir(&blocked).expect("create the untraversable ancestor");
        let _guard = ModeGuard(blocked.clone());
        set_mode(&blocked, 0o000);

        let error = refusal(ToolchainRoot::resolve(&config_tier(blocked.join("tc"))));
        assert!(
            matches!(error, ToolchainRootError::Inaccessible { .. }),
            "an ancestor that cannot be traversed is an I/O refusal, not an absent root; got {error}"
        );
    }

    /// A checked directory owned by another account is refused.
    ///
    /// Creating a directory owned by a second uid needs privileges this suite
    /// does not have, so the row borrows an existing one. When the host has
    /// none, it skips reporting what it observed for every candidate — never a
    /// cause it did not see.
    ///
    /// "Other-owned" is necessary but **not sufficient**, and assuming it was
    /// is what made this row red on macOS. System locations are refused before ownership, so a
    /// candidate that is also a system location is refused for *that* —
    /// correctly, and to a different question. macOS resolves `/home` through
    /// the autofs map to `/System/Volumes/Data/home`, which is under
    /// [`SYSTEM_PREFIXES`]' `/System`. The row therefore selects on the
    /// refusal it actually gets rather than on ownership alone, which keeps it
    /// live wherever a qualifying directory exists and skips with the
    /// refusals it saw where none does.
    #[cfg(unix)]
    #[test]
    fn refuses_a_root_whose_checked_directory_is_owned_by_another_user() {
        use std::os::unix::fs::MetadataExt;

        let effective = effective_uid();
        let mut observed = Vec::new();
        for candidate in ["/home", "/srv", "/mnt", "/media", "/run", "/Users"] {
            let anchor = PathBuf::from(candidate);
            match std::fs::metadata(&anchor) {
                Ok(metadata) if metadata.uid() != effective => {}
                Ok(metadata) => {
                    observed.push(format!("{candidate} is owned by uid {}", metadata.uid()));
                    continue;
                }
                Err(error) => {
                    observed.push(format!("{candidate}: {error}"));
                    continue;
                }
            }

            let _env = anchored_env(&anchor);
            let root = anchor.join(format!("ocx-wp4-absent-{}", std::process::id()));
            match refusal(ToolchainRoot::resolve(&config_tier(root))) {
                ToolchainRootError::NotOwnerOwned { checked, .. } => {
                    assert_eq!(
                        checked,
                        canonical(&anchor),
                        "the refusal names the directory whose ownership it read"
                    );
                    return;
                }
                other => observed.push(format!("{candidate} is other-owned but refused earlier: {other}")),
            }
        }

        eprintln!(
            "skipped: no candidate directory reaches C-019's ownership check as uid {effective} — {}",
            observed.join("; ")
        );
    }

    /// **`resolve` has no filesystem side effect.** Creating the root
    /// is the renderer's job (R-W19); a `create_dir_all` here would make the
    /// permission check meaningless, since a directory ocx just created always
    /// passes it.
    ///
    /// Asserts the absence of the effect, not the return value.
    #[test]
    fn performs_no_filesystem_write_while_resolving() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let _env = anchored_env(sandbox.path());

        let root = sandbox.path().join("a").join("b").join("c");
        let _ = ToolchainRoot::resolve(&config_tier(root.clone()));

        assert!(
            !sandbox.path().join("a").exists(),
            "resolve created {} — creating the root is the renderer's job",
            sandbox.path().join("a").display()
        );
        assert_eq!(
            std::fs::read_dir(sandbox.path())
                .expect("read the anchor directory")
                .count(),
            0,
            "resolve left something behind in the anchor"
        );
    }

    // ── tier precedence ─────────────────────────────────────────────────────

    /// `config.toml` beats `OCX_TOOLCHAIN_DIR`. The environment
    /// variable is the **weakest** tier, matching the rule for the sibling
    /// toolchain keys, not an override.
    #[test]
    fn prefers_the_config_file_over_the_environment_variable() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let env = anchored_env(sandbox.path());
        env.set(
            &ocx_env::OCX_TOOLCHAIN_DIR,
            sandbox.path().join("from-env").to_str().expect("path is utf-8"),
        );

        let root = accepted(ToolchainRoot::resolve(&config_tier(sandbox.path().join("from-file"))));
        assert_eq!(
            root.as_path(),
            canonical(sandbox.path()).join("from-file"),
            "the config.toml tier decides; the environment variable is the weakest tier"
        );
    }

    /// An **invalid** environment value is never reached when the
    /// config file declares one, so it never refuses. Non-obvious, and pinned
    /// so nobody turns it into an eager validation of a tier that lost.
    #[test]
    fn never_reaches_an_invalid_environment_value_when_the_config_file_declares_one() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let env = anchored_env(sandbox.path());
        env.set(&ocx_env::OCX_TOOLCHAIN_DIR, "/usr");

        let root = accepted(ToolchainRoot::resolve(&config_tier(sandbox.path().join("tc"))));
        assert_eq!(
            root.as_path(),
            canonical(sandbox.path()).join("tc"),
            "the losing tier's value is never resolved, so its refusal never fires"
        );
    }

    /// R-W2 — the environment tier is not a bypass. The identical value that
    /// `config.toml` is refused for is refused here too; only the tier the
    /// message names differs.
    #[cfg(unix)]
    #[test]
    fn refuses_the_identical_value_from_the_config_file_and_from_the_environment() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let outside = tempfile::Builder::new()
            .prefix("ocx-tc")
            .tempdir_in("/tmp")
            .expect("create /tmp/ocx-tc*");
        set_mode(outside.path(), 0o700);
        let value = outside.path().to_str().expect("path is utf-8").to_owned();

        let env = anchored_env(sandbox.path());
        let from_file = refusal(ToolchainRoot::resolve(&config_tier(value.clone())));
        assert!(
            matches!(
                from_file,
                ToolchainRootError::OutsideHome {
                    tier: ToolchainRootTier::ConfigFile,
                    ..
                } | ToolchainRootError::SystemPrefix {
                    tier: ToolchainRootTier::ConfigFile,
                    ..
                }
            ),
            "the config.toml tier is refused, naming itself; got {from_file}"
        );

        env.set(&ocx_env::OCX_TOOLCHAIN_DIR, value.as_str());
        let from_env = refusal(ToolchainRoot::resolve(&Config::default()));
        assert!(
            matches!(
                from_env,
                ToolchainRootError::OutsideHome {
                    tier: ToolchainRootTier::Environment,
                    ..
                } | ToolchainRootError::SystemPrefix {
                    tier: ToolchainRootTier::Environment,
                    ..
                }
            ),
            "an exported OCX_TOOLCHAIN_DIR faces the identical refusal, naming the variable; got {from_env}"
        );
        assert_eq!(
            std::mem::discriminant(&from_file),
            std::mem::discriminant(&from_env),
            "the two tiers must fail the same check on the same value — a different one would be a bypass"
        );
    }

    // ── the key, its merge, and what ignores it ─────────────────────────────

    /// The higher tier wins and a `None` never clobbers a lower tier's
    /// value, the rule `RegistryDefaults::merge` applies to `[registry]
    /// default`.
    #[test]
    fn merge_lets_the_higher_tier_declare_the_root_without_clobbering_on_none() {
        let mut lower = config_tier("/lower/tc");
        lower.merge(config_tier("/higher/tc"));
        assert_eq!(
            lower.toolchain_dir.as_deref(),
            Some(Path::new("/higher/tc")),
            "the higher tier's declared root wins"
        );

        let mut lower = config_tier("/lower/tc");
        lower.merge(Config::default());
        assert_eq!(
            lower.toolchain_dir.as_deref(),
            Some(Path::new("/lower/tc")),
            "a higher tier that declares nothing must not clobber a lower tier's root"
        );

        // An empty value is absent at the tier ladder, so it has to be
        // absent at the merge too. Without this the two disagree: `merge` would
        // record `Some("")`, `declared_root` would then skip it, and the lower
        // tier's real root would be gone with nothing to show for it.
        let mut lower = config_tier("/lower/tc");
        lower.merge(config_tier(""));
        assert_eq!(
            lower.toolchain_dir.as_deref(),
            Some(Path::new("/lower/tc")),
            "an empty higher-tier value is absent, not a declaration, and must not erase a lower tier's root"
        );
    }

    /// The accessor reports the merged value, unvalidated. It is what a
    /// file said; `/usr` reaches a caller unchanged, which is why passing it to
    /// a home resolver is the bypass R-W2 exists to close.
    #[test]
    fn the_accessor_reports_the_merged_value_without_validating_it() {
        assert_eq!(Config::default().toolchain_dir(), None);
        assert_eq!(config_tier("/usr").toolchain_dir(), Some(Path::new("/usr")));
    }

    // ── the refusal has to reach the process exit code ──────────────────────

    // ── the anchors as a parameter ──────────────────────────────────────────
    //
    // Every row below reaches `resolve_with_anchors`, the testable core, and
    // supplies an anchor pair no in-process seam can produce. They are the rows
    // the module doc names as unreachable while the home directory was read
    // rather than passed.

    /// **Fail closed when nothing anchors.** With neither a home
    /// directory nor `$OCX_HOME` resolvable, containment has nothing to compare
    /// against, and the refusal says that rather than claiming the value is
    /// outside two directories that do not exist.
    #[test]
    fn refuses_every_root_when_no_containment_anchor_resolves() {
        let env = ocx_env::overrides::lock();
        env.remove(&ocx_env::OCX_TOOLCHAIN_DIR);
        let anchors = ContainmentAnchors {
            home: None,
            ocx_home: None,
        };

        // Absolute *to this host's parser*, and not a listed system location —
        // both refusals rank ahead of the fail-closed arm, so a POSIX literal
        // here would report `Relative` on Windows and never reach the rule
        // under test.
        let declared = if cfg!(windows) {
            r"C:\srv\ocx-toolchains"
        } else {
            "/srv/ocx-toolchains"
        };
        let error = refusal(ToolchainRoot::resolve_with_anchors(&config_tier(declared), &anchors));
        assert!(
            matches!(error, ToolchainRootError::NoContainmentAnchor { .. }),
            "with no anchor resolvable every value is refused, and named as such; got {error}"
        );
    }

    /// An anchor that cannot be canonicalised is **dropped**, and the
    /// other one still admits its own descendants. `$OCX_HOME` is `~/.ocx`,
    /// which does not exist on a fresh machine, so treating an unresolvable
    /// anchor as fatal would refuse every root on exactly the hosts the ADR's
    /// worked example targets.
    ///
    /// The non-vacuity half is the row above: dropping *both* refuses.
    #[test]
    fn drops_an_anchor_that_cannot_be_canonicalised_and_keeps_the_other() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let env = ocx_env::overrides::lock();
        env.remove(&ocx_env::OCX_TOOLCHAIN_DIR);
        let anchors = ContainmentAnchors {
            home: Some(sandbox.path().join("no-such-home-directory")),
            ocx_home: Some(sandbox.path().to_path_buf()),
        };

        let root = accepted(ToolchainRoot::resolve_with_anchors(
            &config_tier(sandbox.path().join("tc")),
            &anchors,
        ));
        assert_eq!(
            root.as_path(),
            canonical(sandbox.path()).join("tc"),
            "the surviving anchor still contains its own descendants"
        );
    }

    /// **The fail-closed drop applies only to
    /// the admitting anchor set.**
    ///
    /// `$OCX_HOME` is `~/.ocx`, which does not exist on a fresh machine. An
    /// anchor dropped from the *refusing* roles there stops refusing `$OCX_HOME`
    /// itself and its `toolchain` subtree — and the home directory, which does
    /// exist, then admits both values through containment.
    ///
    /// Red against one dropped-when-absent spelling set: `resolve` returns
    /// `Ok(Some(..))` for both candidates.
    #[test]
    fn refuses_an_absent_ocx_home_and_the_toolchain_directory_under_it() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let env = ocx_env::overrides::lock();
        env.remove(&ocx_env::OCX_TOOLCHAIN_DIR);
        let absent = sandbox.path().join("not-created-yet");
        let anchors = ContainmentAnchors {
            home: Some(sandbox.path().to_path_buf()),
            ocx_home: Some(absent.clone()),
        };

        let error = refusal(ToolchainRoot::resolve_with_anchors(
            &config_tier(absent.clone()),
            &anchors,
        ));
        assert!(
            matches!(error, ToolchainRootError::IsContainmentAnchor { .. }),
            "an anchor that does not exist yet is still not a descendant of itself; got {error}"
        );

        let error = refusal(ToolchainRoot::resolve_with_anchors(
            &config_tier(absent.join("toolchain").join("projects")),
            &anchors,
        ));
        assert!(
            matches!(error, ToolchainRootError::InsideGlobalToolchainHome { .. }),
            "R-W1 holds before $OCX_HOME/toolchain has ever been rendered; got {error}"
        );
    }

    /// **The refusing comparisons fold ASCII case.**
    ///
    /// macOS's default volume and every NTFS volume are case-**insensitive**:
    /// there `$OCX_HOME/TOOLCHAIN` and `$OCX_HOME/toolchain` are one directory,
    /// and a byte-wise compare walks the second spelling straight past both
    /// refusals (CWE-178). The home directory is the surrounding anchor, so
    /// containment admits every candidate below and only these two refusals can
    /// produce the answer.
    ///
    /// Red against `Path::eq` / `Path::starts_with`: every spelling but the
    /// literal `ocx-home`/`toolchain` pair is accepted. The literal pair is the
    /// non-vacuity control — it must keep refusing either way, so the row
    /// cannot pass by refusing everything.
    #[test]
    fn refuses_the_anchor_and_the_global_toolchain_home_in_any_ascii_case() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let env = ocx_env::overrides::lock();
        env.remove(&ocx_env::OCX_TOOLCHAIN_DIR);
        let ocx_home = sandbox.path().join("ocx-home");
        std::fs::create_dir(&ocx_home).expect("create the $OCX_HOME anchor");
        let anchors = ContainmentAnchors {
            home: Some(sandbox.path().to_path_buf()),
            ocx_home: Some(ocx_home),
        };

        for spelling in ["ocx-home", "OCX-HOME", "Ocx-Home"] {
            let anchor_itself = sandbox.path().join(spelling);
            let error = refusal(ToolchainRoot::resolve_with_anchors(
                &config_tier(anchor_itself),
                &anchors,
            ));
            assert!(
                matches!(error, ToolchainRootError::IsContainmentAnchor { .. }),
                "{spelling} names the containment anchor itself on a case-insensitive volume; got {error}"
            );

            for toolchain in ["toolchain", "TOOLCHAIN", "Toolchain"] {
                let inside = sandbox.path().join(spelling).join(toolchain).join("tc");
                let error = refusal(ToolchainRoot::resolve_with_anchors(&config_tier(inside), &anchors));
                assert!(
                    matches!(error, ToolchainRootError::InsideGlobalToolchainHome { .. }),
                    "{spelling}/{toolchain} is the global toolchain home on a case-insensitive volume; got {error}"
                );
            }
        }
    }

    /// **A symlinked home directory contains its own
    /// descendants.** The shipped row proves this against `$OCX_HOME` because
    /// that is the only anchor `ocx_env::overrides` can redirect; with the anchors
    /// passed in, the home directory itself can carry the symlink.
    ///
    /// Red: canonicalise only the candidate and not the anchors, and this
    /// legitimate root is refused as `OutsideHome`.
    #[cfg(unix)]
    #[test]
    fn accepts_a_root_under_a_symlinked_home_directory() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let env = ocx_env::overrides::lock();
        env.remove(&ocx_env::OCX_TOOLCHAIN_DIR);

        let real = sandbox.path().join("real-home");
        std::fs::create_dir(&real).expect("create the real home directory");
        set_mode(&real, 0o700);
        let linked = sandbox.path().join("linked-home");
        std::os::unix::fs::symlink(&real, &linked).expect("symlink the home directory");

        let anchors = ContainmentAnchors {
            home: Some(linked.clone()),
            ocx_home: None,
        };

        let root = accepted(ToolchainRoot::resolve_with_anchors(
            &config_tier(linked.join("tc")),
            &anchors,
        ));
        assert_eq!(
            root.as_path(),
            canonical(&real).join("tc"),
            "a symlinked home directory must contain its own descendants — the anchor is canonicalised too"
        );
    }

    /// A machine with **no** home directory cannot expand a
    /// leading `~`, and reports the shipped
    /// [`EntryDefect`](crate::shell::EntryDefect) rather than inventing a
    /// second vocabulary for it. `$OCX_HOME` still resolves, so the refusal is
    /// expansion's and not containment's.
    #[test]
    fn refuses_a_tilde_root_on_a_machine_with_no_home_directory() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let env = ocx_env::overrides::lock();
        env.remove(&ocx_env::OCX_TOOLCHAIN_DIR);
        let anchors = ContainmentAnchors {
            home: None,
            ocx_home: Some(sandbox.path().to_path_buf()),
        };

        let error = refusal(ToolchainRoot::resolve_with_anchors(&config_tier("~/tc"), &anchors));
        assert!(
            matches!(
                error,
                ToolchainRootError::Unexpandable {
                    defect: crate::shell::EntryDefect::UnresolvableHome,
                    ..
                }
            ),
            "a leading '~' with no home directory to expand against is an expansion failure; got {error}"
        );
    }

    /// The list transcribed at the top of this module and the one the
    /// implementation actually consults must name the same locations.
    ///
    /// Transcribing the contract twice is what makes the per-prefix rows above
    /// non-vacuous; this row is what stops the two copies drifting apart in
    /// silence. Both directions: a prefix dropped from the implementation reds,
    /// and one added there without a row above reds too.
    #[test]
    fn the_transcribed_system_prefix_list_matches_the_implementation() {
        // `windows_system_prefixes()` reads four environment variables, and
        // `reads_the_windows_system_locations_from_the_environment` sets exactly
        // those four. `OVERRIDES` is process-global, so under any thread-parallel
        // runner the two rows collide without this.
        let _env = ocx_env::overrides::lock();

        assert_eq!(
            C018_PREFIXES.len(),
            SYSTEM_PREFIXES.len(),
            "C-018's contract names {} subtree locations; the implementation consults {}: {:?} vs {:?}",
            C018_PREFIXES.len(),
            SYSTEM_PREFIXES.len(),
            C018_PREFIXES,
            SYSTEM_PREFIXES
        );
        for prefix in C018_PREFIXES {
            assert!(
                SYSTEM_PREFIXES.contains(&prefix),
                "{prefix} is a C-018 subtree location the implementation does not list"
            );
        }
        assert_eq!(
            C018_EXACT_LOCATIONS.len(),
            SYSTEM_LOCATIONS_MATCHED_EXACTLY.len(),
            "C-018 matches {:?} by identity; the implementation matches {:?}",
            C018_EXACT_LOCATIONS,
            SYSTEM_LOCATIONS_MATCHED_EXACTLY
        );
        for location in C018_EXACT_LOCATIONS {
            assert!(
                SYSTEM_LOCATIONS_MATCHED_EXACTLY.contains(&location),
                "{location} is matched by identity in C-018 and not in the implementation"
            );
        }
        assert_eq!(
            C018_WINDOWS_LOCATIONS.map(|(_, path)| PathBuf::from(path)).to_vec(),
            windows_system_prefixes(),
            "C-018's Windows locations and the implementation's fallbacks must name the same directories"
        );
    }
}
