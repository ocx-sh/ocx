// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

pub mod error;
pub mod insecure;
pub mod loader;
pub mod managed;
pub mod mirror;
pub mod patch;
pub mod registry;
pub mod shell;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::cli::ClassifyExitCode;
use crate::cli::ExitCode;

pub use self::managed::ManagedConfig;
pub use self::mirror::MirrorConfig;
pub use self::patch::PatchConfig;
pub use self::registry::RegistryConfig;
pub use self::shell::ShellConfig;

/// Which `config.toml` tier a value came from (C-032).
///
/// Runtime provenance only — never parsed from a file, never serialized. The
/// loader stamps it per file; [`ShellConfig::merge`] carries it alongside the
/// scalar it describes, so a consumer can report the tier that **actually**
/// decided a setting rather than asserting one (A-32).
///
/// Ordered lowest- to highest-precedence, which is also the fold order:
/// `System` → `User` → `Home` → `Managed` → `Explicit`.
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
    /// Renders the tier as the name a user would recognise it by — the flag or
    /// the path, never the Rust variant.
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

/// Root configuration struct.
///
/// No `deny_unknown_fields` anywhere in this tree — unknown sections AND
/// unknown keys inside a known section are silently ignored. The reason is
/// fleet forward-compat, not convenience: one `config.toml` is read by many
/// ocx versions at once (the `[managed]` tier ships an operator's payload to
/// every binary in a fleet), so a file written for a newer ocx must degrade to
/// "the parts I understand" on an older one. The alternative — rejecting the
/// file — takes the WHOLE payload out of service on every older binary, which
/// is how a central rollout bricks a fleet.
///
/// The cost is that a typo'd key silently no-ops. When a change genuinely
/// cannot degrade (a key whose meaning or value shape changed), the escape
/// hatch is the tier's own OCI tag: publish the new payload under a new tag
/// and move fleets onto it as they upgrade — see the 2026-07-31 amendment in
/// `adr_managed_config_tier.md` and `website/src/docs/user-guide.md`
/// ("Rolling out an incompatible change").
#[derive(Debug, Default, Clone, Deserialize, schemars::JsonSchema)]
pub struct Config {
    /// Global registry-subsystem settings (`[registry]` section).
    ///
    /// In v1 contains only `default`, but reserved for future global settings
    /// (timeout, retry policy, default-credential-provider, etc.).
    pub registry: Option<RegistryDefaults>,

    /// Named per-registry configuration tables (`[registries.<name>]`).
    ///
    /// The plural name is deliberate: it matches Cargo's convention and avoids
    /// a TOML collision with the singular `[registry]` global-settings section.
    ///
    /// Every key is an identifier prefix, always — `[registry] default`
    /// never dereferences through this table (§6 ratified simplification).
    /// Future extensions (location rewrite, timeout, auth) drop into the same
    /// entry struct without breaking existing configs.
    pub registries: Option<HashMap<String, RegistryConfig>>,

    /// Per-traffic-host mirrors (`[mirrors."<host>"]`).
    ///
    /// Maps a canonical upstream traffic host (e.g. `"ghcr.io"`,
    /// `"index.ocx.sh"`) to replacement endpoint(s) so OCX routes read
    /// traffic to a corporate mirror instead of the firewall-blocked origin.
    /// Each value is a union (`adr_index_indirection.md` F5b): a bare string
    /// rewrites both traffic roles for that host; a `{registry?, index?}`
    /// table splits per role — `registry` rewrites OCI distribution traffic
    /// only, `index` rewrites index-tree traffic only. Replace semantics: no
    /// origin fallback. The canonical identifier and content-addressed digest
    /// stay unchanged, so an `ocx.lock` produced behind the mirror remains
    /// valid with direct egress and vice versa.
    ///
    /// Deserialized via [`mirror::deserialize_mirrors_table`] (hand-rolled,
    /// value-first) rather than a plain derive, so a malformed entry raises a
    /// named per-host error instead of an opaque `#[serde(untagged)]`
    /// variant-mismatch.
    #[serde(default, deserialize_with = "mirror::deserialize_mirrors_table")]
    pub mirrors: Option<HashMap<String, MirrorConfig>>,

    /// Site-tier patch configuration (`[patches]`).
    ///
    /// Points at an operator-controlled patch registry that provides companion
    /// packages (CA bundles, proxy env vars, license-server endpoints) layered
    /// onto unmodified upstream packages at compose time. The patch tier is the
    /// execution-env twin of `[mirrors]`: `[mirrors]` adapts transport, patches
    /// adapt the execution environment.
    ///
    /// Absent → no patch tier configured (opt-in, not required).
    pub patches: Option<PatchConfig>,

    /// Corporate managed-configuration tier (`[managed]`).
    ///
    /// Points at an operator-controlled OCI artifact that supplies a plain
    /// `config.toml` payload — mirrors, patches pointer, default registry —
    /// synced into local state and merged above the user config every
    /// invocation. See `crate::managed_config` for the fetch/persist domain
    /// layer and `adr_managed_config_tier.md`.
    ///
    /// Absent → no managed tier configured (opt-in, seeded by
    /// `ocx self setup --managed-config`).
    pub managed: Option<ManagedConfig>,

    /// Identity-pinned verification policies (`[[trust.policy]]`).
    ///
    /// Unlike every other section, trust policies **array-append** across the
    /// `config.toml` tiers rather than replace (see [`Config::merge`]) — the
    /// operator (config.toml) trust set is the union of system/user/`$OCX_HOME`.
    /// At verify time this operator set takes precedence over the project
    /// `ocx.toml` (`crate::trust::resolve_tiered`). Consumed by
    /// `ocx package verify`. See `crate::trust` and `adr_trust_policy.md`.
    pub trust: Option<crate::trust::TrustConfig>,

    /// Shell-integration settings (`[shell]`) — the per-prompt hook and
    /// completions toggles, plus the activation consent whitelist.
    ///
    /// **Never contributed by the project tier.** `ConfigLoader` strips
    /// `shell` from any project-tier contribution explicitly (C-033), because
    /// consent material read from a repo's own `ocx.toml` would let a clone
    /// consent to itself. Consumed by `ocx self activate`, `ocx self setup`
    /// and `ocx shell state`. See `adr_shell_env_overhaul.md` Decisions 4, 5
    /// and 7.
    pub shell: Option<ShellConfig>,

    /// Execution-record sink (`[records]`).
    ///
    /// Designates where OCX writes one JSON resolution record per tool launch —
    /// the resolved package closure with digests, plus the resolved executable.
    /// Declared at SYSTEM scope it clamps: no lower tier can redirect or
    /// disable it, which is what lets an operator make recording a fleet
    /// property instead of a wrapper-script convention.
    ///
    /// Absent → no records written (opt-in).
    pub records: Option<crate::record::RecordsOptions>,

    // C-016, S-004 — and every Rust cross-reference this field has. They live
    // in this `//` comment rather than in the `///` block below because
    // schemars copies that block verbatim into the `description` of the
    // published `config.toml` schema (RUL-30): an intra-doc link renders there
    // as literal brackets, and a contract identifier means nothing to the
    // person the schema is written for.
    //
    // The neighbours a Rust reader wants: `ToolchainRoot::resolve` is the only
    // path from this declared value to a root a home may be built on — reading
    // this field and handing it straight to
    // `crate::project::resolve_toolchain_home` is the bypass R-W2 exists to
    // close. That function performs the `<root>/<project-key>/` join;
    // `crate::reference_manager::ReferenceManager::name_for_path` derives the
    // key. The global home is `crate::file_structure::FileStructure::toolchain`,
    // built under `$OCX_HOME` and unaffected by this key.
    /// Root directory under which each project's toolchain tree is rendered.
    ///
    /// A project's tree lands at `<root>/<project-key>/toolchain/`, so this
    /// names one directory holding many projects, not a single project's
    /// toolchain home. The global toolchain home ignores this key entirely: it
    /// is always `$OCX_HOME/toolchain`.
    ///
    /// A leading `~` expands against the home directory, and nothing else is
    /// expanded — a `%VAR%` reference is taken literally on every platform,
    /// Windows included. The value must be absolute and must not contain a
    /// `..` component.
    ///
    /// The directory it resolves to must sit inside the home directory or
    /// inside `$OCX_HOME`, must not be either of those two directories itself,
    /// and must not be inside `$OCX_HOME/toolchain`. It must not be a
    /// filesystem root or a system location such as `/usr`, `/etc`, `/var/lib`
    /// or `C:\Program Files`. On Linux and macOS it must additionally be owned
    /// by the invoking user and writable by neither group nor world; Windows
    /// checks no ownership. ocx exits 78 when any of that fails. It does not
    /// have to exist yet — the directory is created when a toolchain is first
    /// rendered.
    ///
    /// Available in every configuration tier, a managed-configuration payload
    /// included, so one fleet rollout can place every host's project trees on a
    /// chosen volume.
    #[serde(default)]
    pub toolchain_dir: Option<PathBuf>,
}

/// Global registry-subsystem settings (`[registry]` section).
///
/// No `deny_unknown_fields` — unknown keys are ignored, like every other
/// config table (see [`Config`]).
#[derive(Debug, Default, Clone, Deserialize, schemars::JsonSchema)]
pub struct RegistryDefaults {
    /// Default registry for bare identifiers (e.g. `cmake:3.28` expands to
    /// `<default>/cmake:3.28`). Overridden by the `OCX_DEFAULT_REGISTRY`
    /// environment variable.
    ///
    /// Always a literal prefix (e.g. `"ghcr.io"`, `"ocx.sh"`) — never
    /// resolved through the `[registries.<name>]` table (§6 ratified
    /// simplification).
    pub default: Option<String>,

    /// Runtime provenance marker: this tier was declared at the SYSTEM config
    /// scope (`/etc/ocx/config.toml`), so it is NON-OVERRIDABLE by any lower
    /// tier. Mirrors [`PatchConfig`]'s C7 lock, but unconditional — unlike
    /// `[patches].required`, `[registry]` has no opt-out field, so any
    /// system-scope declaration is authoritative by itself.
    ///
    /// Never serialized — set by the loader via [`Self::lock_as_system`]
    /// after parsing the system-scope file, not read from disk.
    #[serde(skip)]
    #[schemars(skip)]
    pub system_locked: bool,
}

impl Config {
    /// Merge `other` into `self`. `other` has higher precedence — its set
    /// fields override `self`'s.
    ///
    /// Scalars: `other` wins when present (`Some`).
    /// Tables: merged key-by-key.
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
        // Trust policies APPEND across tiers at storage level (union): this
        // extends the vec, it never drops or replaces an earlier tier's entry
        // outright. Masking happens at resolution time instead, in
        // `trust::resolve`, which keeps only the matches at the winning
        // specificity level — so a later tier's MORE SPECIFIC scope displaces
        // an earlier tier's broader pin, and only entries at *equal*
        // specificity combine as ANY-of (rotation).
        //
        // The system tier is exempt: `apply_system_locks` marks its entries
        // `system_locked`, and a locked policy that matches the target FIXES
        // the specificity level, so a `ghcr.io/acme/tool` entry from the user
        // tier (or the untrusted managed payload) can no longer mask a
        // system-tier `ghcr.io/acme/*` pin — it may only join that pin's
        // ANY-of set by declaring the same scope. Non-system tiers still mask
        // each other by specificity as above.
        //
        // `[trust.sigstore]` is the opposite case and merges by the opposite
        // rule — replace-and-lock, per `TrustConfig::merge`. Two Fulcio CAs is
        // not a pooled ANY-of set, it is an ambiguity.
        if let Some(other_trust) = other.trust {
            match self.trust.as_mut() {
                Some(self_trust) => self_trust.merge(other_trust),
                None => self.trust = Some(other_trust),
            }
        }
        // `[shell]` is neither a plain scalar-wins section nor a plain append:
        // `hook`/`completions` are scalars, `consent.paths` appends and
        // `consent.namespaces` accumulates into one spec. `ShellConfig::merge`
        // owns that split (C-032); this arm only routes to it.
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
        // A plain scalar (C-016): the higher tier wins when it declares one, and
        // its `None` never clobbers a lower tier's value — the same rule
        // `RegistryDefaults::merge` applies to `[registry] default`. An **empty**
        // value is absent rather than a declaration, the rule `declared_root`
        // applies at the tier ladder: without this filter `toolchain_dir = ""`
        // in a higher tier silently erases a lower tier's real value, which is
        // the one thing "a `None` never clobbers" was written to prevent.
        //
        // No system lock, and that is a plain gap rather than a design: the ADR
        // states the key exists so one fleet rollout can place every host's
        // trees on a chosen volume, which is operator policy by definition. It
        // is unlocked because no ruling in this work package granted a lock,
        // and every tier's value faces the identical [`ToolchainRoot::resolve`]
        // refusals whichever tier won.
        if other
            .toolchain_dir
            .as_deref()
            .is_some_and(|value| !value.as_os_str().is_empty())
        {
            self.toolchain_dir = other.toolchain_dir;
        }
    }

    /// The declared trust policies (empty when no `[trust]` section is set).
    #[must_use]
    pub fn trust_policies(&self) -> &[crate::trust::TrustPolicy] {
        self.trust.as_ref().map_or(&[], |trust| trust.policy.as_slice())
    }

    /// Return [`RegistryDefaults::default`] as a literal prefix.
    ///
    /// `[registry] default` is always a literal identifier prefix (e.g.
    /// `"ghcr.io"`, `"ocx.sh"`) — it never dereferences through the
    /// `[registries.<name>]` table (§6 ratified simplification). Returns
    /// `None` only when no default is configured at all.
    #[must_use]
    pub fn resolved_default_registry(&self) -> Option<&str> {
        self.registry.as_ref()?.default.as_deref()
    }

    /// The `toolchain_dir` root **as the merged tiers declared it** (C-016), or
    /// `None` when no tier set one.
    ///
    /// Deliberately unvalidated: this is what a file said, and a file may say
    /// `../x`, `/usr`, or a directory anyone can write to. Use it to *report*
    /// configuration; use [`ToolchainRoot::resolve`] to obtain a root a
    /// toolchain home may be built on. Passing this value to
    /// [`resolve_toolchain_home`](crate::project::resolve_toolchain_home) is the
    /// bypass R-W2 exists to close.
    #[must_use]
    pub fn toolchain_dir(&self) -> Option<&Path> {
        self.toolchain_dir.as_deref()
    }
}

/// Which tier declared a `toolchain_dir` value (R-W2).
///
/// Reached only through [`ToolchainRootError`]: a refusal has to name the input
/// whoever hit it can actually change, or a message blames a `config.toml` for a
/// value an exported environment variable supplied — the exact confusion S-011
/// tests for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolchainRootTier {
    /// The merged `config.toml` chain's root-level `toolchain_dir` key. The
    /// `[managed]` payload folds into that same chain, so a fleet-pushed value
    /// reports as this tier too — the operator's remedy is to edit the payload,
    /// which is still a `config.toml`.
    ConfigFile,
    /// The `OCX_TOOLCHAIN_DIR` environment variable
    /// ([`env::keys::OCX_TOOLCHAIN_DIR`](crate::env::keys::OCX_TOOLCHAIN_DIR)).
    Environment,
}

impl std::fmt::Display for ToolchainRootTier {
    /// Renders the tier as the thing a user would edit — the key or the
    /// variable, never the Rust variant. Mirrors [`ConfigTier`]'s `Display`.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::ConfigFile => "config.toml `toolchain_dir`",
            Self::Environment => "OCX_TOOLCHAIN_DIR",
        })
    }
}

/// Why a declared `toolchain_dir` was refused (C-017, C-018, C-019, R-W1, R-W2,
/// RUL-4, RUL-5).
///
/// Every variant classifies as [`ExitCode::ConfigError`] (78), and every variant
/// names the tier that declared the value alongside the path that failed — the
/// failing property alone does not tell an operator which of two inputs to
/// change.
///
/// **A root that does not exist is not in here.** RUL-5 accepts it: the ADR's own
/// worked example is `toolchain_dir = "~/.cache/ocx/toolchain"`, which does not
/// exist on a fresh machine, and the renderer (WP-7) creates the directory. What
/// C-019 checks in that case is the nearest existing ancestor, which is why the
/// two ownership variants carry `checked` beside `resolved`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ToolchainRootError {
    /// A leading `~` that could not be expanded — `~user` (unsupported) or a
    /// machine with no resolvable home directory.
    ///
    /// `defect` is the shipped [`EntryDefect`](crate::config::shell::EntryDefect)
    /// that `config::shell`'s expander returned, not a second vocabulary for the
    /// same two conditions. Only `UnsupportedTildeUser` and `UnresolvableHome`
    /// are reachable through this path; the wildcard shapes belong to consent
    /// entries, which run more checks than expansion.
    #[error("{tier} declares {}, whose leading '~' cannot be expanded: {defect}", declared.display())]
    Unexpandable {
        tier: ToolchainRootTier,
        declared: PathBuf,
        defect: crate::config::shell::EntryDefect,
    },

    /// R-W2 — a relative value, tested **after** `~` expansion. Canonicalising
    /// one uses the process working directory, so the same project would resolve
    /// a different home from every directory ocx is invoked from.
    #[error(
        "{tier} is the relative path {}; a toolchain_dir root must be absolute, or one project resolves a different home from every working directory",
        declared.display()
    )]
    Relative { tier: ToolchainRootTier, declared: PathBuf },

    /// A `..` component anywhere in the value.
    ///
    /// Refused outright rather than normalised away, following the shipped
    /// consent-entry rule
    /// ([`EntryDefect::ParentDirComponent`](crate::config::shell::EntryDefect)):
    /// lexical `..` removal and POSIX resolution disagree the moment a preceding
    /// component is a symlink (`$HOME/link/../x` is lexically `$HOME/x` and
    /// actually `/x`), so a normalising containment check can admit a root
    /// outside `$HOME` that the config never named. No ADR example uses `..`.
    #[error(
        "{tier} declares {}, which contains a '..' component; write the directory the root actually names",
        declared.display()
    )]
    ParentDirComponent { tier: ToolchainRootTier, declared: PathBuf },

    /// Neither `$HOME` / `%USERPROFILE%` nor `$OCX_HOME` could be resolved, so
    /// C-017 has no anchor to compare against and every value is refused.
    ///
    /// Its own variant rather than [`Self::OutsideHome`]: "outside both anchors"
    /// would name two directories that do not exist, sending an operator to look
    /// for the wrong fault.
    #[error(
        "{tier} declares {}, but neither a home directory nor $OCX_HOME could be resolved to contain it",
        declared.display()
    )]
    NoContainmentAnchor { tier: ToolchainRootTier, declared: PathBuf },

    /// The root resolves to a containment anchor **itself** rather than to a
    /// directory beneath one — `$OCX_HOME` (RUL-4) or the home directory.
    ///
    /// `$OCX_HOME` exactly would put every project's 16-hex key directly beside
    /// `packages/`, `blobs/` and `toolchain/` in the ocx root; the home directory
    /// exactly litters `$HOME` with the same opaque keys. C-017 says *descendant*,
    /// and neither anchor is a descendant of itself.
    ///
    /// `anchor` can name a directory that is not literally `resolved`: the
    /// comparison folds ASCII case (see [`eq_ignoring_ascii_case`]), so on a
    /// case-**sensitive** host `$HOME=/home/u` with `toolchain_dir = /home/U`
    /// reports two genuinely different directories as one. The refusal is still
    /// the right answer — `/home/U` fails C-017 anyway — but the diagnosis is
    /// the cost of folding a refusal rather than probing the filesystem for the
    /// volume's case behaviour, which RUL-5 forbids here.
    #[error(
        "{tier} resolves to {}, which is the containment anchor {} itself; name a directory beneath it",
        resolved.display(),
        anchor.display()
    )]
    IsContainmentAnchor {
        tier: ToolchainRootTier,
        resolved: PathBuf,
        anchor: PathBuf,
    },

    /// C-017 — the load-bearing containment refusal. After expansion and
    /// canonicalisation the root must be a descendant of `$HOME` /
    /// `%USERPROFILE%` or of `$OCX_HOME`, compared component-wise.
    #[error(
        "{tier} resolves to {}, which is outside both the home directory and $OCX_HOME",
        resolved.display()
    )]
    OutsideHome { tier: ToolchainRootTier, resolved: PathBuf },

    /// C-018 — a filesystem root or a system prefix. Defence in depth for an
    /// absurd `$HOME`, which would otherwise satisfy C-017 on its own — so this
    /// check runs even when containment passed, never only as its fallback.
    #[error("{tier} resolves to the system location {}", resolved.display())]
    SystemPrefix { tier: ToolchainRootTier, resolved: PathBuf },

    /// R-W1 — a root at or under `$OCX_HOME/toolchain`. It passes C-017 (it *is*
    /// under `$OCX_HOME`) and is a data-loss path: project homes would land at
    /// `$OCX_HOME/toolchain/<project-key>/toolchain`, where `<project-key>` is
    /// indistinguishable from a group directory, so a bare global `ocx pull`'s
    /// whole-directory reconcile prunes other projects' trees as orphan groups.
    #[error(
        "{tier} resolves to {}, inside the global toolchain home {}; a global `ocx pull` would prune other projects' trees there",
        resolved.display(),
        global_home.display()
    )]
    InsideGlobalToolchainHome {
        tier: ToolchainRootTier,
        resolved: PathBuf,
        global_home: PathBuf,
    },

    /// An I/O failure while inspecting the path chain — canonicalising the
    /// nearest existing ancestor, or reading its metadata. `EACCES`, `ELOOP` and
    /// their kin.
    ///
    /// **Not** "the root does not exist", nor "a component is not a directory":
    /// RUL-5 accepts an absent root, and an ancestor always exists to walk up
    /// to, so a failure here is a real I/O fault rather than the ordinary
    /// fresh-machine case; `ENOTDIR` is walked past to the ancestor that does
    /// exist, and lands on [`Self::NotADirectory`].
    #[error(
        "{tier} resolves to {}, whose nearest existing directory {} cannot be inspected",
        resolved.display(),
        checked.display()
    )]
    Inaccessible {
        tier: ToolchainRootTier,
        resolved: PathBuf,
        checked: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The checked path exists and is **not a directory** — a `toolchain_dir`
    /// naming a regular file, a socket, a FIFO or a device node.
    ///
    /// `checked` follows [`Self::NotOwnerOwned`]'s rule, so this fires both for
    /// a root that *is* the file (`~/notes.txt`) and for one whose nearest
    /// existing ancestor is (`~/notes.txt/sub`). Neither reaches C-019's
    /// ownership half: a regular file owned by the effective user at mode
    /// `0600` satisfies both of its clauses, so without this variant the root
    /// resolves and the renderer's `create_dir_all` fails later, naming a
    /// directory the operator never wrote.
    ///
    /// Unlike C-019's two, this refusal runs on **every** platform — nothing
    /// about `is_dir` is Unix-specific.
    #[error(
        "{tier} resolves to {}, whose nearest existing path {} is not a directory",
        resolved.display(),
        checked.display()
    )]
    NotADirectory {
        tier: ToolchainRootTier,
        resolved: PathBuf,
        checked: PathBuf,
    },

    /// C-019 — the checked directory is not owned by the effective user.
    ///
    /// `checked` is the root when it exists and its nearest existing ancestor
    /// when it does not (RUL-5): naming only `resolved` would report a property
    /// of a directory that was never inspected.
    ///
    /// `owner` and `effective_user` are rendered forms, not a numeric type,
    /// because the two platforms answer with different things — a decimal uid on
    /// Unix (`st_uid` against `geteuid`), a security identifier on Windows.
    ///
    /// **Unix only today.** The Windows arm of C-019 is not implemented: reading
    /// a directory's owner SID needs `GetNamedSecurityInfoW`, whose `windows-sys`
    /// feature this work package does not own. See
    /// [`ToolchainRoot::resolve`]'s *Windows* section for the exact gap.
    #[error(
        "{tier} resolves to {}, whose nearest existing directory {} is owned by {owner} rather than by the effective user {effective_user}",
        resolved.display(),
        checked.display()
    )]
    NotOwnerOwned {
        tier: ToolchainRootTier,
        resolved: PathBuf,
        checked: PathBuf,
        owner: String,
        effective_user: String,
    },

    /// C-019 — the checked directory's mode carries `0o020` or `0o002`, so
    /// another account can plant a trampoline in a tree that lands on a PATH.
    ///
    /// `checked` follows [`Self::NotOwnerOwned`]'s rule. Unix only, and
    /// deliberately so: a Windows directory's permissions are an ACL rather than
    /// three mode triples, so there is no mode to read and this variant is never
    /// constructed there.
    #[error(
        "{tier} resolves to {}, whose nearest existing directory {} has mode {mode:04o}, granting write to group or world",
        resolved.display(),
        checked.display()
    )]
    GroupOrWorldWritable {
        tier: ToolchainRootTier,
        resolved: PathBuf,
        checked: PathBuf,
        mode: u32,
    },
}

impl ClassifyExitCode for ToolchainRootError {
    /// Exhaustive on purpose, with no wildcard: a refusal added later cannot
    /// reach a user as an unclassified exit code without a decision here. Every
    /// variant is 78 today — the operator's remedy is always to edit a
    /// `toolchain_dir` value — but the match, not a blanket `Some`, is what
    /// makes that a statement rather than an accident (D-V15(e)).
    ///
    /// Reachable from the CLI only through
    /// [`config::error::Error::Toolchain`](crate::config::error::Error), which
    /// `cli::classify` already downcasts. Deleting that variant leaves this impl
    /// intact and flips the process exit from 78 to 1 — the mutation that
    /// separates "classified" from "reachable".
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::Unexpandable { .. }
            | Self::Relative { .. }
            | Self::ParentDirComponent { .. }
            | Self::NoContainmentAnchor { .. }
            | Self::IsContainmentAnchor { .. }
            | Self::OutsideHome { .. }
            | Self::SystemPrefix { .. }
            | Self::InsideGlobalToolchainHome { .. }
            | Self::Inaccessible { .. }
            | Self::NotADirectory { .. }
            | Self::NotOwnerOwned { .. }
            | Self::GroupOrWorldWritable { .. } => ExitCode::ConfigError,
        })
    }
}

/// A `toolchain_dir` root that has passed every C-016–C-019 refusal.
///
/// **The funnel, enforced by the type and not by memory (R-W2).** The inner path
/// is private and [`Self::resolve`] is the only constructor — there is no
/// `new`, no `From<PathBuf>`, no public field and no `Deref`. So, **to any
/// module outside `crate::config`**, a value of this type cannot exist without
/// having been expanded, canonicalised, contained inside `$HOME` or `$OCX_HOME`
/// component-wise, checked against the system-prefix set, checked against
/// `$OCX_HOME/toolchain`, confirmed to be a directory as far as it exists, and
/// checked for owner-ownership and group/world writability. Whatever tier
/// supplied it.
///
/// The qualifier is exact, not defensive: `root` is module-private, so
/// `config/loader.rs` and every other `config/*.rs` descendant could write the
/// struct literal directly and skip every refusal. Nothing does. Moving the type
/// to its own `config/toolchain_root.rs` would make the module boundary the
/// funnel and remove even that; it costs one file and is the upgrade path if a
/// second `config` module ever needs to hold one of these.
///
/// [`Self::resolve`] reads the `OCX_TOOLCHAIN_DIR` environment variable itself
/// rather than taking it as a parameter, and resolves `$HOME` / `$OCX_HOME`
/// itself through the shipped
/// [`home_directory`](crate::file_structure::home_directory) and
/// [`default_ocx_root`](crate::file_structure::default_ocx_root). So a caller
/// **outside `crate::config`** has no way to hand in an unchecked value for
/// either the input or the containment anchors — which is what makes
/// `OCX_TOOLCHAIN_DIR=/tmp/x` refuse exactly like the identical `config.toml`
/// value (S-011) instead of bypassing the check.
///
/// The qualifier carries the same weight as the struct-literal one above, and
/// for the same reason: RUL-26 granted [`Self::resolve_with_anchors`], which is
/// module-private, so `config/loader.rs` and every other `config/*.rs`
/// descendant can name its own anchors. Nothing outside this file does — the
/// seam exists so a test can produce the two anchor states no process
/// environment can (no home directory at all; a symlinked one).
///
/// The one bypass this type cannot close is
/// [`Config::toolchain_dir`], which by C-016's mandated signature hands back a
/// bare `Option<&Path>` that
/// [`resolve_toolchain_home`](crate::project::resolve_toolchain_home) would
/// accept. That accessor's contract says so in as many words; feeding it to a
/// home resolver is a review finding, not a compile error.
///
/// # What "checked for owner-ownership" means per platform
///
/// On Unix, both halves of C-019 run: `st_uid` against `geteuid`, and the mode
/// against `0o020` / `0o002`. **On Windows neither runs.** The mode half has
/// nothing to read — permissions are an ACL — and the owner half needs
/// `GetNamedSecurityInfoW` from `windows-sys`'s `Win32_Security_Authorization`
/// feature, which is not enabled in this workspace and which this work package's
/// file set does not include. So on Windows the guarantee above is containment,
/// the system-location set, directory-ness and a `checked` path that can be
/// stat'd — the one metadata read runs on every platform, so
/// [`ToolchainRootError::Inaccessible`] is reachable there too — and nothing
/// about who owns the directory.
///
/// The directory-ness refusal is on the other side of that platform line:
/// `metadata.is_dir()` needs no platform API, so
/// [`ToolchainRootError::NotADirectory`] fires everywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolchainRoot {
    /// Absolute, past every refusal, and canonical **as far as it exists**:
    /// the longest ancestor present on disk is canonicalised and the absent
    /// tail is re-joined verbatim, so a root that does not exist yet is not a
    /// canonical path and cannot be — there is nothing to resolve. Private to
    /// `crate::config`: reachable from outside only through
    /// [`ToolchainRoot::as_path`], written only by [`ToolchainRoot::resolve`].
    root: PathBuf,
}

impl ToolchainRoot {
    /// Resolve the effective `toolchain_dir` root, or `None` when no tier
    /// declared one (the in-project `<project>/.ocx/toolchain` default).
    ///
    /// # Tiers
    ///
    /// `config.toml` first, `OCX_TOOLCHAIN_DIR` second, matching C-007's rule for
    /// the sibling toolchain keys: **the environment variable is the weakest
    /// tier, not an override** (RUL-3). The variable is written by
    /// [`Env::apply_ocx_config`](crate::env::Env::apply_ocx_config) from
    /// `OcxConfigView.toolchain_dir` — intended as a parent ocx's already-resolved
    /// root, though that field is a plain `Option<PathBuf>` and nothing in the
    /// type system holds the producer to it — so a child that can read its own
    /// configuration prefers that, and the variable supplies continuity exactly
    /// where the child cannot (`OCX_NO_CONFIG=1`, a different `--config`).
    ///
    /// Read through [`crate::env::var`], never `std::env::var`, so the
    /// `#[cfg(test)]` override seam applies and unit tests of this ladder are not
    /// order-dependent inside one nextest process. An **empty** value in either
    /// tier reads as absent, not as invalid — the rule
    /// [`ActivateMode::from_env`](crate::activate::ActivateMode) states and
    /// `OCX_CONFIG=""` already follows.
    ///
    /// # Resolution order — no filesystem side effect at any step
    ///
    /// 1. Expand a **leading** `~` through `config::shell`'s shipped expander,
    ///    the one seam the consent path already uses. `~user` and an unresolvable
    ///    home are [`ToolchainRootError::Unexpandable`]. Expansion comes first
    ///    because `Path::new("~/.cache/ocx/toolchain").is_absolute()` is `false` —
    ///    testing absoluteness before expanding would refuse the ADR's own worked
    ///    example.
    /// 2. **No `%VAR%` expansion, on any platform.** The ADR's Windows line is
    ///    commented out, and a literal `%LOCALAPPDATA%\ocx\toolchain` simply fails
    ///    the checks below and exits 78 naming the path, which is diagnosable. Do
    ///    not add an expander for it here or anywhere.
    /// 3. Refuse a **relative** value ([`ToolchainRootError::Relative`], R-W2),
    ///    then a `..` component ([`ToolchainRootError::ParentDirComponent`]). In
    ///    that order, so `../tc` — which is both — has one answer on record
    ///    (RUL-29).
    /// 4. Normalise lexically, then apply C-018's system locations, RUL-4's
    ///    anchor-itself refusal, R-W1's `$OCX_HOME/toolchain` exclusion,
    ///    RUL-25's "no anchor resolved" and C-017 containment — in that order,
    ///    most specific diagnosis first (RUL-28), and **component-wise**
    ///    ([`Path::starts_with`], which is already component-wise; never a
    ///    string prefix). C-018 runs even when C-017 passed, and ahead of it: an
    ///    absurd `$HOME` of `/usr` is exactly the case it is defence in depth
    ///    for. The two refusing comparisons run ahead of the fail-closed arm
    ///    because they hold whether or not the anchor resolves — see
    ///    [`Containment`].
    /// 5. Canonicalise the **nearest existing ancestor**, re-join the tail, and
    ///    re-run step 4 on the result — that re-run is what catches a root whose
    ///    own last component is a symlink out of `$HOME`. The anchors are
    ///    canonicalised too, or a symlinked `$HOME` mis-refuses a legitimate
    ///    root. `dunce::canonicalize`, never `std::fs::canonicalize`.
    /// 6. Refuse a checked path that exists and is **not a directory**
    ///    ([`ToolchainRootError::NotADirectory`]), then apply C-019 — to the
    ///    root when it exists, and to the **nearest existing ancestor** when it
    ///    does not (RUL-5). That ancestor is the directory the renderer will
    ///    create under, so it is the one whose permissions decide whether
    ///    another account could plant a trampoline; and a regular file there is
    ///    owner-owned at mode `0600`, so C-019 alone would admit it.
    ///
    /// # An absent root is accepted
    ///
    /// RUL-5: `~/.cache/ocx/toolchain` on a fresh machine resolves, and the
    /// renderer (WP-7) creates the directory. `resolve` itself creates nothing —
    /// no `create_dir_all`, no probe file, nothing observable on disk.
    ///
    /// The **global** home never consults any of this: it is
    /// [`FileStructure::toolchain`](crate::file_structure::FileStructure) under
    /// `$OCX_HOME`, whatever this resolves to (C-016).
    ///
    /// # Windows
    ///
    /// Step 4 is complete there. C-018's `%SystemRoot%`, `C:\Program Files`,
    /// `C:\Program Files (x86)` and `C:\ProgramData` are checked, through
    /// [`windows_system_prefixes`] and the injectable
    /// [`is_system_location_among`] — which is what lets a Linux CI runner
    /// exercise the clause, since those directories are a host property no
    /// POSIX machine has. A bare drive root and a UNC share root are covered by
    /// the parentless test, as `C:\` cannot be spelled as a constant.
    ///
    /// Step 6's directory-ness refusal runs there; its **C-019 half** does
    /// not — see this type's *What "checked for owner-ownership" means per
    /// platform*. That remains the one gap, and it is one change away:
    /// `windows-sys`'s `Win32_Security_Authorization` feature, in a workspace
    /// `Cargo.toml` outside this work package's file set.
    ///
    /// Blocking: canonicalisation and one metadata read. Async callers wrap it in
    /// `spawn_blocking`, the same note and the same reason as
    /// [`resolve_toolchain_home`](crate::project::resolve_toolchain_home).
    ///
    /// # Errors
    ///
    /// A [`ToolchainRootError`] — exit 78 — for each refusal above.
    pub fn resolve(config: &Config) -> Result<Option<Self>, ToolchainRootError> {
        Self::resolve_with_anchors(config, &ContainmentAnchors::from_environment())
    }

    /// [`Self::resolve`] against an explicit anchor pair.
    ///
    /// `anchors` is a parameter for the reason `config::shell`'s
    /// [`expand_against`](crate::config::shell) states for its own `home`
    /// parameter: the interesting branches are the machine with no home
    /// directory at all and the machine whose home is a symlink, and a test
    /// cannot produce either without mutating the process environment out from
    /// under every concurrent test. `$OCX_HOME` alone is injectable through
    /// [`crate::env::var`]; the home directory is not.
    ///
    /// The tier ladder still reads `OCX_TOOLCHAIN_DIR` from the environment —
    /// that variable *is* injectable, so leaving it here keeps the one input a
    /// caller must not be able to forge (S-011) out of the signature.
    ///
    /// # Errors
    ///
    /// As [`Self::resolve`].
    fn resolve_with_anchors(config: &Config, anchors: &ContainmentAnchors) -> Result<Option<Self>, ToolchainRootError> {
        let Some((tier, declared)) = declared_root(config) else {
            return Ok(None);
        };

        // Step 1 — a leading `~`, through the one shipped expander. Before the
        // absoluteness test, because `~/.cache/ocx/toolchain` is not absolute
        // until it has expanded and that value is the ADR's worked example.
        let expanded = shell::expand_against(&declared, anchors.home.as_deref()).map_err(|defect| {
            ToolchainRootError::Unexpandable {
                tier,
                declared: declared.clone(),
                defect,
            }
        })?;

        // Step 2 is a non-step: nothing expands `%VAR%`, on any platform.

        // Step 3 — relative first (RUL-29), so `../tc`, which is both relative
        // and `..`-bearing, reports `Relative` rather than depending on which
        // check happens to run first.
        if !expanded.is_absolute() {
            return Err(ToolchainRootError::Relative { tier, declared });
        }
        if expanded
            .components()
            .any(|component| component == std::path::Component::ParentDir)
        {
            return Err(ToolchainRootError::ParentDirComponent { tier, declared });
        }

        // Step 4, first pass — lexical. `..` is already refused and `.` is
        // dropped by `Path::components`, so normalising is re-collecting the
        // components; it exists so the value carried into the refusals is the
        // one a reader would write, not `/a/./b`.
        let lexical: PathBuf = expanded.components().collect();
        let containment = Containment::around(anchors);
        containment.refuse_uncontained(tier, &declared, &lexical)?;

        // Step 5 — canonicalise what exists, re-join what does not, re-run the
        // whole of step 4. Only this pass sees through a symlink, and it is the
        // one that decides.
        let nearest = NearestExisting::of(&lexical).map_err(|(checked, source)| ToolchainRootError::Inaccessible {
            tier,
            resolved: lexical.clone(),
            checked,
            source,
        })?;
        containment.refuse_uncontained(tier, &declared, &nearest.resolved)?;

        // Step 6 — against the directory that actually exists.
        refuse_unsound_root(tier, &nearest.resolved, &nearest.existing)?;

        Ok(Some(Self { root: nearest.resolved }))
    }

    /// The validated root, for
    /// [`resolve_toolchain_home`](crate::project::resolve_toolchain_home)'s
    /// `toolchain_dir` parameter.
    ///
    /// The only way out of the funnel: absolute, canonical below its nearest
    /// existing ancestor, and past every refusal [`Self::resolve`] performs.
    ///
    /// **The directory may not exist** (RUL-5), and creating it is the caller's
    /// obligation, not this type's — `resolve` writes nothing. Create it mode
    /// `0o700` on Unix: C-019 was checked against the nearest *existing*
    /// ancestor, so a new directory inheriting a permissive umask would hand
    /// away exactly the group- or world-write that check refused one level up.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.root
    }

    /// A root taken as already validated — **test builds only**.
    ///
    /// Forced by R-W20: `resolve_toolchain_home` now takes `Option<&Self>` so an
    /// unvalidated path cannot be spelled at the one call site that builds a
    /// home, and [`Self::resolve`] — the only other writer — refuses anything
    /// outside `$HOME`/`$OCX_HOME`, which no `tempfile` scratch directory
    /// satisfies. Its own anchor-injecting sibling is private to this module, so
    /// a unit test in another module has no other way to hold one.
    ///
    /// `#[cfg(test)]`, never `feature = "__testing"`: this bypasses C-017–C-019
    /// wholesale, so it must not exist in any build an acceptance test can
    /// invoke.
    #[cfg(test)]
    pub fn from_validated(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

/// The tier that declared a `toolchain_dir`, and what it declared.
///
/// `config.toml` beats `OCX_TOOLCHAIN_DIR` (RUL-3): the environment variable is
/// the weakest tier, so a child that can read its own configuration prefers
/// that and the variable supplies continuity only where the child cannot.
///
/// An **empty** value is absent rather than invalid, at both tiers (RUL-24) —
/// the rule `ActivateMode::from_env` states and `OCX_CONFIG=""` already follows.
/// So an empty `config.toml` value falls through to the environment exactly as a
/// missing key does; "absent" is one condition, not two.
fn declared_root(config: &Config) -> Option<(ToolchainRootTier, PathBuf)> {
    if let Some(declared) = config
        .toolchain_dir
        .as_ref()
        .filter(|value| !value.as_os_str().is_empty())
    {
        return Some((ToolchainRootTier::ConfigFile, declared.clone()));
    }
    // `crate::env::var`, never `std::env::var`: the `#[cfg(test)]` override seam
    // lives there, and a direct process read would make every unit test of this
    // ladder order-dependent inside one nextest process.
    crate::env::var(crate::env::keys::OCX_TOOLCHAIN_DIR)
        .filter(|value| !value.is_empty())
        .map(|value| (ToolchainRootTier::Environment, PathBuf::from(value)))
}

/// The two directories C-017 admits a `toolchain_dir` root beneath, as the
/// environment spells them.
///
/// Declared spellings, not canonical ones — [`Containment::around`] canonicalises.
/// Keeping the raw form here is what lets a test inject a *symlinked* anchor and
/// observe that its descendants are still admitted.
#[derive(Debug, Clone)]
struct ContainmentAnchors {
    /// `$HOME`, or `%USERPROFILE%` on Windows. Also the directory a leading `~`
    /// expands against, so one value answers both questions.
    home: Option<PathBuf>,
    /// `$OCX_HOME`, else `~/.ocx`.
    ocx_home: Option<PathBuf>,
}

impl ContainmentAnchors {
    /// The anchors this process resolves.
    ///
    /// Read here rather than taken from a caller: a caller able to name its own
    /// anchor could admit any root at all, which is the bypass the whole type
    /// exists to close (S-011).
    fn from_environment() -> Self {
        Self {
            home: crate::file_structure::home_directory(),
            ocx_home: crate::file_structure::default_ocx_root(),
        }
    }
}

/// The anchor set a candidate is compared against, in every spelling that
/// counts — split by the **role** each spelling plays.
///
/// The two roles pull in opposite directions, which is why they are separate
/// fields rather than one list consulted twice. An anchor that cannot be
/// canonicalised must stop *admitting* — RUL-25, since a directory that does
/// not resolve contains nothing — and must keep on *refusing*, because a
/// refusal that evaporates when its target does not exist is backwards.
/// `$OCX_HOME` is `~/.ocx`, absent on precisely the fresh machine the ADR's
/// worked example targets; dropping it from both roles would admit
/// `$OCX_HOME/toolchain` itself there, which is the one location R-W1 exists
/// to keep a project tree out of.
struct Containment {
    /// **Admitting** (C-017). Each resolvable anchor as declared *and* as
    /// canonicalised — both, because step 4 runs twice, once on the value as
    /// written and once on the canonicalised value, and a symlinked anchor
    /// spells those differently. An anchor with no canonical form contributes
    /// nothing at all (RUL-25), failing closed.
    anchors: Vec<PathBuf>,
    /// **Refusing** (RUL-4). The anchors themselves, present whether or not
    /// they exist.
    anchor_identities: Vec<PathBuf>,
    /// **Refusing** (R-W1). `$OCX_HOME/toolchain` in each of `$OCX_HOME`'s
    /// spellings, present whether or not `$OCX_HOME` exists.
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

    /// C-018, RUL-4, R-W1, RUL-25 and C-017, applied to one candidate in the
    /// order RUL-28 fixes: the most specific diagnosis wins. The system-location
    /// refusal beats "that is the anchor itself", which beats "that is the
    /// global toolchain home", which beats the fail-closed "nothing anchors",
    /// which beats plain containment.
    ///
    /// The two refusing comparisons fold ASCII case and the admitting one does
    /// not — see [`starts_with_ignoring_ascii_case`] for why that asymmetry is
    /// the safe direction.
    ///
    /// `declared` appears only in the refusals that describe an *input* rather
    /// than a resolved location — there is no resolved location to name when no
    /// anchor resolved.
    ///
    /// # Errors
    ///
    /// One [`ToolchainRootError`] per contract above.
    fn refuse_uncontained(
        &self,
        tier: ToolchainRootTier,
        declared: &Path,
        candidate: &Path,
    ) -> Result<(), ToolchainRootError> {
        // C-018 — first, and unconditionally. Gating it behind a C-017 failure
        // would admit `/usr/tc` on a host whose `$HOME` is `/usr`, which is the
        // one case C-018 exists for.
        if is_system_location(candidate) {
            return Err(ToolchainRootError::SystemPrefix {
                tier,
                resolved: candidate.to_path_buf(),
            });
        }
        // RUL-4 — C-017 admits *descendants*, and no directory is a descendant
        // of itself. Ahead of RUL-25's fail-closed arm because this refusal
        // holds whether or not the anchor resolves, and naming the anchor is
        // the more useful answer than "nothing anchors".
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
        // R-W1 — passes C-017 by construction, so it needs its own check, and
        // for the same reason as RUL-4 it runs ahead of the fail-closed arm.
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
        // RUL-25 — nothing anchors, so nothing is contained.
        if self.anchors.is_empty() {
            return Err(ToolchainRootError::NoContainmentAnchor {
                tier,
                declared: declared.to_path_buf(),
            });
        }
        // C-017 — the load-bearing one. `Path::starts_with` compares one path
        // component at a time, which is the entire contract: `$HOME` of
        // `/home/u` must NOT contain `/home/ufoo`, and a `str::starts_with` or
        // `to_string_lossy().starts_with(..)` on the same two values says it
        // does.
        if !self.anchors.iter().any(|anchor| candidate.starts_with(anchor)) {
            return Err(ToolchainRootError::OutsideHome {
                tier,
                resolved: candidate.to_path_buf(),
            });
        }
        Ok(())
    }
}

/// An anchor as declared and as canonicalised, deduplicated — or nothing at all
/// when it cannot be canonicalised (RUL-25).
///
/// The **admitting** spelling set. [`refusal_spellings_of`] is its refusing
/// counterpart, and never returns nothing.
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

/// The same anchor for the **refusing** comparisons, which never drop it.
///
/// [`NearestExisting::of`] resolves whatever prefix does exist and re-joins the
/// rest, so an absent `$OCX_HOME` still yields a spelling that sees through a
/// symlinked ancestor; when even that fails — an unreadable ancestor — the
/// lexically normalised value stands alone. Never empty for an anchor the
/// environment named, which is the whole difference from [`spellings_of`].
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

/// Whether `path` sits at or under `prefix`, comparing one component at a time
/// and folding ASCII case.
///
/// The refusing comparisons use this; the admitting one deliberately does not.
/// macOS's default volume and every NTFS volume are case-**insensitive**: there
/// `$OCX_HOME/TOOLCHAIN` and `$OCX_HOME/toolchain` name one directory, and a
/// byte-wise compare walks an attacker-chosen spelling straight past R-W1 and
/// RUL-4 (CWE-178). Folding widens a *refusal*, which stays sound on a
/// case-sensitive filesystem — the extra match names a directory that is merely
/// a different directory, and refusing it costs a diagnosable exit 78. Folding
/// the *admitting* compare would widen what is accepted, the one direction this
/// must not take, so C-017 stays byte-wise.
///
/// ASCII-only, and no filesystem probe: RUL-5 forbids a side effect here, and a
/// `cfg!(windows)` branch would be a rule no test on this host could reach
/// (RUL-26).
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

/// C-018's system locations matched as **subtrees** — a candidate at or under
/// any of these is refused.
///
/// A constant rather than an inline literal so the specification test asserts
/// parity against it instead of transcribing the same list a second time
/// (RUL-31): a prefix deleted here then reds a row rather than silently running
/// one iteration fewer.
///
/// `/` and `/var` are **not** here; they are matched by identity, in
/// [`SYSTEM_LOCATIONS_MATCHED_EXACTLY`]. `/var` because an ostree-composed
/// Fedora — Silverblue, Kinoite, CoreOS, Bazzite — puts every user's home at
/// `/var/home/<user>`: `/home` is a symlink to `var/home` and the passwd entry
/// canonicalises there. A bare `/var` subtree prefix therefore refuses *every*
/// `toolchain_dir` on those hosts, `~/.cache/ocx/toolchain` included — the
/// ADR's own worked example, on a shipping desktop distribution. The
/// directories C-018 is actually defending are listed one by one below; there
/// is no exemption mechanism and no configuration key that opts out of any of
/// them.
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

/// C-018's system locations matched by **identity** — the directory itself is
/// refused, its descendants are not.
///
/// `/` for RUL-27's reason: `Path::new("/x").starts_with("/")` is `true`, as it
/// must be since `/` is every absolute path's first component, so matching it
/// as a prefix would refuse every absolute path there is. `/var` for the ostree
/// reason [`SYSTEM_PREFIXES`] states — the directory itself is no place for a
/// toolchain tree, but `/var/home/alice` is somebody's home.
const SYSTEM_LOCATIONS_MATCHED_EXACTLY: [&str; 2] = ["/", "/var"];

/// C-018's Windows half, read from the environment with the shipped defaults.
///
/// Through [`crate::env::var`], never `std::env::var`, for the reason the tier
/// ladder states: that is the seam a test can inject through. The fallbacks are
/// the paths Windows installs to, so a host that unset `%SystemRoot%` is still
/// covered and a host that relocated it is covered by the variable.
fn windows_system_prefixes() -> Vec<PathBuf> {
    [
        ("SystemRoot", r"C:\Windows"),
        ("ProgramFiles", r"C:\Program Files"),
        ("ProgramFiles(x86)", r"C:\Program Files (x86)"),
        ("ProgramData", r"C:\ProgramData"),
    ]
    .into_iter()
    .map(|(key, fallback)| {
        crate::env::var(key)
            // Empty is absent, the rule the tier ladder applies. POSIX-absolute
            // is absent too, and that one is load-bearing: `crate::env::var`'s
            // override arm is `#[cfg(test)]`, so on a POSIX host this is a live
            // `std::env::var` read, and a cross-compilation shell
            // (`cargo-xwin`, which `verify-deep.yml` runs; `msvc-wine`) can
            // export `ProgramFiles=/home/u`. Without the filter that lands in
            // the prefix list and refuses the operator's own home directory,
            // naming it "the system location" — the same wrong message the
            // bare `/var` entry used to produce.
            .filter(|value| !value.is_empty() && !value.starts_with('/'))
            .map_or_else(|| PathBuf::from(fallback), PathBuf::from)
    })
    .collect()
}

/// Whether `path` is a filesystem root or sits at or under a C-018 system
/// location.
///
/// Component-wise throughout — never a string prefix.
fn is_system_location(path: &Path) -> bool {
    is_system_location_among(path, &windows_system_prefixes())
}

/// [`is_system_location`] against an explicit Windows prefix set.
///
/// `windows_prefixes` is a parameter for the reason
/// [`ToolchainRoot::resolve_with_anchors`] takes its anchors (RUL-26): the
/// POSIX half is spelled by literal constants and reads the same on every host,
/// but `%SystemRoot%` and the three `Program*` directories are a *host*
/// property, and the machine that compiles and runs these rows on every pull
/// request is a Linux runner with none of them. Injecting the list is what
/// makes C-018's Windows clause testable at all.
fn is_system_location_among(path: &Path, windows_prefixes: &[PathBuf]) -> bool {
    // A filesystem root has no parent: `/` on Unix, and on Windows a bare drive
    // root such as `C:\` or a UNC share root, neither of which is spellable as
    // a constant because the drive or server name is a host property.
    if path.parent().is_none() {
        return true;
    }
    // Folded, like every other refusal: macOS's default volume is
    // case-insensitive, so `/USR`, `/library` and `/VAR` name the directories
    // C-018 lists. Pass 2 does canonicalise a *resolvable* spelling back to the
    // on-disk one, but pass 1 runs on the lexical value and C-018 is defence in
    // depth precisely for the state where the other checks are satisfied
    // (CWE-178).
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
    // The Windows arm runs on every platform and is inert off Windows: no POSIX
    // path's first component can equal `C:`. Both sides go through the shipped
    // `lexical_normalize`, which rewrites `\` to `/` — without it
    // `C:\Windows\tc` is a *single* component on a Linux host and no
    // component-wise compare can see inside it. The compare then folds ASCII
    // case, because every NTFS volume is case-insensitive.
    let normalized = crate::utility::fs::path::lexical_normalize(path);
    windows_prefixes.iter().any(|prefix| {
        starts_with_ignoring_ascii_case(&normalized, &crate::utility::fs::path::lexical_normalize(prefix))
    })
}

/// A fresh temporary directory usable as a `toolchain_dir` containment anchor,
/// or the reason this host cannot supply one.
///
/// The temp root has to be a *usable* anchor. On macOS `std::env::temp_dir()` is
/// `$TMPDIR` under `/var/folders/…`, and `/var` is a symlink to `private/var`,
/// so pass 2 canonicalises anything beneath it to `/private/var/folders/…` —
/// and `/private` is a live [`SYSTEM_PREFIXES`] entry. An "accepted because it
/// is under `$OCX_HOME`" assertion built there passes or fails for the wrong
/// reason, and on `verify-deep.yml`'s `macos-latest` leg it fails.
///
/// The guard asks [`is_system_location`] itself rather than transcribing a
/// prefix list, so it cannot drift from what `resolve` actually refuses — a
/// second transcription would be a second thing to keep true.
///
/// At `crate::config` scope rather than inside `toolchain_root_tests`, and
/// `pub(crate)` rather than private, because `config::loader`'s tests mint
/// anchors too and a sibling module cannot see another module's private items.
/// That is the whole reason this reasoning shipped at one site and not both.
///
/// # Errors
///
/// The path it observed, never a cause it merely assumed.
#[cfg(test)]
pub(crate) fn anchor_sandbox() -> Result<tempfile::TempDir, String> {
    let sandbox =
        tempfile::TempDir::new().map_err(|error| format!("could not create a temporary directory: {error}"))?;
    let canonical = dunce::canonicalize(sandbox.path())
        .map_err(|error| format!("could not canonicalise {}: {error}", sandbox.path().display()))?;
    if is_system_location(&canonical) {
        return Err(format!(
            "this host's temporary root canonicalises to {}, which C-018 refuses, so it cannot serve as a containment anchor",
            canonical.display()
        ));
    }
    Ok(sandbox)
}

/// [`anchor_sandbox`], or `None` having reported on stderr why the row is being
/// skipped.
#[cfg(test)]
pub(crate) fn sandbox_or_skip() -> Option<tempfile::TempDir> {
    match anchor_sandbox() {
        Ok(sandbox) => Some(sandbox),
        Err(reason) => {
            eprintln!("skipped: {reason}");
            None
        }
    }
}

/// A candidate root split at the boundary between what exists and what does not
/// (RUL-5 step 5).
struct NearestExisting {
    /// The longest ancestor that exists, canonicalised — which is `path`
    /// itself when `path` exists. C-019's subject: an absent root has no owner
    /// or mode, and this is then the directory the renderer will create it
    /// under.
    ///
    /// Existence, not directory-ness: a `toolchain_dir` naming an existing
    /// **regular file** canonicalises here and is carried out intact.
    /// Refusing it is [`refuse_unsound_root`]'s job, at the one stat this
    /// resolution performs — [`ToolchainRootError::NotADirectory`].
    existing: PathBuf,
    /// [`Self::existing`] with the absent tail re-joined — the value the caller
    /// asked about, expressed through resolved symlinks.
    resolved: PathBuf,
}

impl NearestExisting {
    /// Split `path`, following symlinks in its existing prefix.
    ///
    /// # Errors
    ///
    /// The directory whose canonicalisation failed, and why, for any failure
    /// that is neither "no such file or directory" nor "not a directory". Both
    /// of those mean the cursor is not the longest existing ancestor yet, so
    /// they walk up; `EACCES` and `ELOOP` are real faults and stop here.
    ///
    /// `ENOTDIR` walks up because `~/notes.txt/sub` has a longest existing
    /// ancestor — `~/notes.txt` — and this type's job is to find it. Stopping
    /// there instead would report a structural fact about the path as an I/O
    /// fault, and would hide it from [`refuse_unsound_root`], which is what
    /// says the useful thing about it. Both spellings are in the set because a
    /// host answers that shape with either — `ENOTDIR` here, and one of
    /// `ERROR_PATH_NOT_FOUND` / `ERROR_DIRECTORY` on Windows, which map to
    /// `NotFound` and `NotADirectory` respectively.
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
                    // Walk up one component. `file_name` and `parent` are both
                    // `None` only at a filesystem root, and a root that reports
                    // `NotFound` is a host we cannot say anything more useful
                    // about than the error it just gave us.
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

/// Step 6 — `checked` must be a directory (every platform), owned by the
/// effective user and granting write to neither group nor world (Unix, C-019).
///
/// One `metadata` read serves both halves, which is why the directory-ness
/// check lives here rather than at a second stat: [`NearestExisting::of`]
/// canonicalises but never inspects a file type, and this is the only place in
/// the resolution path that reads a path's metadata. Every caller routes through
/// [`ToolchainRoot::resolve_with_anchors`], so guarding here covers a root that
/// *is* a regular file and a root whose nearest existing ancestor is one.
///
/// `resolved` is carried only so the refusal can report the root the tier
/// declared alongside the path actually inspected; naming one without the other
/// reports a property of a directory nobody asked about.
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

/// C-019 — `checked` must be owned by the effective user and grant write to
/// neither group nor world.
///
/// Takes the `metadata` [`refuse_unsound_root`] already read, so the resolution
/// path stats `checked` exactly once.
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

    // SAFETY: `geteuid` reads the calling process's own credentials. It takes no
    // arguments, touches no memory, and is documented as always succeeding.
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
    // The two write bits C-019 names, and only those: `0o750` and `0o755` are
    // admitted, so the check is not "anything but 0700".
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

/// C-019 is **not implemented** off Unix.
///
/// The mode half has nothing to read — Windows permissions are an ACL, not three
/// mode triples. The owner half needs `GetNamedSecurityInfoW`, which lives
/// behind `windows-sys`'s `Win32_Security_Authorization` feature; that feature
/// is not enabled in this workspace, and the workspace `Cargo.toml` is not in
/// this work package's file set. Returning `Ok` states the truth: this platform
/// performs no ownership check.
///
/// [`refuse_unsound_root`]'s directory-ness refusal is **not** part of this gap
/// — it reads `metadata.is_dir()` and runs everywhere.
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
    /// Mark this tier as system-locked — non-overridable by lower tiers.
    ///
    /// Called by the config loader on the system-scope file
    /// (`/etc/ocx/config.toml`) after parsing and before folding higher tiers
    /// in. Unconditional: unlike [`PatchConfig::lock_as_system`], `[registry]`
    /// has no opt-out field to gate on — a system-scope declaration of
    /// `[registry]` is always authoritative.
    pub fn lock_as_system(&mut self) {
        self.system_locked = true;
    }

    /// Merge `other` into `self` field-by-field. `other`'s `Some` values
    /// override `self`'s; `other`'s `None` values do not clobber `self`.
    ///
    /// A system-locked tier (`self.system_locked`) ignores ALL lower-tier
    /// overrides. The locked flag stays on `self` (sticky). The loader folds
    /// the system tier in FIRST as the accumulator base, so `self` is the
    /// system tier when locked.
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
    use crate::record::RecordsOptions;

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

// ── WP-4 · `toolchain_dir` specification tests ──────────────────────────────

/// Specification tests for the `toolchain_dir` root.
///
/// Written from `plan_toolchain_activation.md` (C-016–C-019, S-004, S-011,
/// R-W1, R-W2), `adr_toolchain_activation.md` (§ *`config.toml` placement key*
/// and validation item 18) and rulings RUL-3/RUL-4/RUL-5 — never from an
/// implementation. At the commit these were written [`ToolchainRoot::resolve`]
/// and [`Config::toolchain_dir`] are `unimplemented!()`, so every test calling
/// either fails with that panic. Each test names the identifier it covers.
///
/// # Anchors — what this module can inject, and what it can only read
///
/// `$OCX_HOME` resolves through [`crate::env::var`]
/// ([`default_ocx_root`](crate::file_structure::default_ocx_root)), so
/// [`crate::test::env`] injects it and every `$OCX_HOME`-anchored row is
/// hermetic.
///
/// `$HOME` does **not**. It is
/// [`home_directory`](crate::file_structure::home_directory), which is
/// `std::env::home_dir()`, and no in-process seam redirects it — the only
/// alternative, `std::env::set_var`, is precisely what `crate::test::env`
/// exists to avoid. The home-anchored rows therefore *read* the real home:
/// refusal rows touch no filesystem and are unaffected, and the rows that must
/// be **accepted** first check that this host can host them, skipping with the
/// uid and mode they actually observed otherwise.
///
/// Rows needing an anchor the home cannot supply — a symlinked anchor (RUL-5
/// step 5), an anchor that is itself a system prefix (C-018 winning over
/// C-017) — run against `$OCX_HOME` instead, which C-017 treats identically.
/// Every such substitution is stated on the row.
#[cfg(test)]
mod toolchain_root_tests {
    use super::*;
    use crate::cli::classify_error;
    use crate::env::keys::OCX_TOOLCHAIN_DIR;
    use crate::test::env::EnvLock;

    /// C-018's prefix set, written out here rather than read from the
    /// implementation.
    ///
    /// A table driven by the production constant is vacuous the moment a
    /// prefix is deleted from it — the loop just runs one row fewer. This list
    /// is transcribed from the contract (`plan_toolchain_activation.md`, C-018)
    /// and from the ADR's § *`config.toml` placement key* check 2, so dropping
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

    /// C-018's locations matched by identity rather than as subtrees,
    /// transcribed for the same reason as [`C018_PREFIXES`].
    const C018_EXACT_LOCATIONS: [&str; 2] = ["/", "/var"];

    /// C-018's Windows locations, and the environment variables that name them.
    ///
    /// Transcribed from the contract, like the two lists above. The value
    /// column is what the implementation falls back to when the variable is
    /// unset, which is the only branch a POSIX host can reach.
    const C018_WINDOWS_LOCATIONS: [(&str, &str); 4] = [
        ("SystemRoot", r"C:\Windows"),
        ("ProgramFiles", r"C:\Program Files"),
        ("ProgramFiles(x86)", r"C:\Program Files (x86)"),
        ("ProgramData", r"C:\ProgramData"),
    ];

    /// The environment lock with both `toolchain_dir` inputs pinned:
    /// `$OCX_HOME` at `anchor`, and no ambient `OCX_TOOLCHAIN_DIR` leaking in
    /// from the developer's shell.
    ///
    /// Every value below is injected through this seam, which only
    /// [`crate::env::var`] consults — so a `resolve` reading `std::env::var`
    /// directly would fail these rows rather than pass them by accident
    /// (C-007's idiom, as `ActivateMode::from_env` states it).
    fn anchored_env(anchor: &Path) -> EnvLock {
        let env = crate::test::env::lock();
        env.set("OCX_HOME", anchor.to_str().expect("anchor path is utf-8"));
        env.remove(OCX_TOOLCHAIN_DIR);
        env
    }

    /// A merged `Config` whose `config.toml` tier declares `value`.
    ///
    /// The `[managed]` tier folds into the same field, so this fixture is both
    /// tiers as far as [`ToolchainRoot::resolve`] can see; the `[managed]`
    /// payload's own journey into this field is pinned in `config/loader.rs`
    /// (S-004).
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

    /// The first ancestor of `path` that exists — the directory RUL-5 step 6
    /// checks when the root itself does not exist.
    // Only `host_can_accept` calls this, and that is `#[cfg(unix)]`, so off
    // Unix the function is dead under the workspace's denied warnings.
    #[cfg(unix)]
    fn nearest_existing_ancestor(path: &Path) -> Option<&Path> {
        path.ancestors().find(|candidate| candidate.exists())
    }

    /// `Ok(())` when `path`'s nearest existing ancestor satisfies C-019 on this
    /// host; otherwise the uid and mode actually observed.
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

    // ── C-017 · containment, component-wise ─────────────────────────────────

    /// C-017 · RUL-4 — an anchor is not a descendant of itself. `$HOME`
    /// exactly would litter the home with opaque project keys; `$OCX_HOME`
    /// exactly would land them beside `packages/` and `blobs/` inside the tree
    /// `ocx clean` and the GC walk own.
    #[test]
    fn refuses_a_root_that_is_a_containment_anchor_itself() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let _env = anchored_env(sandbox.path());
        let home = crate::file_structure::home_directory().expect("this host resolves a home directory");

        for anchor in [home.clone(), sandbox.path().to_path_buf()] {
            let error = refusal(ToolchainRoot::resolve(&config_tier(anchor.clone())));
            assert!(
                matches!(error, ToolchainRootError::IsContainmentAnchor { .. }),
                "the anchor {} itself must be refused as an anchor, not by some later check; got {error}",
                anchor.display()
            );
        }
    }

    /// C-017 — a direct child of either anchor is the ordinary accepted shape.
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

        let home = crate::file_structure::home_directory().expect("this host resolves a home directory");
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

    /// C-017 — **the string-prefix trap.** `$HOME` is `/home/u` and the value
    /// is `/home/ufoo`: a `to_string_lossy().starts_with(..)` containment check
    /// admits it, a component-wise one refuses it. The single row this contract
    /// exists for, asserted against both anchors.
    ///
    /// Both anchors are **injected** through RUL-26's seam rather than read from
    /// the environment. The ambient form carried an unstated premise — that the
    /// temp root lies outside the real home directory — which `TMPDIR` decides:
    /// this repository's own gate points it at `$HOME/.cache/ocx-test-tmp`, and
    /// there `<sandbox>foo` is a legitimate descendant of the home anchor, so
    /// the row inverted and reported the implementation's guard as broken. The
    /// property asserted is unchanged; only where the anchors come from is.
    #[test]
    fn refuses_a_sibling_whose_path_bytes_merely_start_with_an_anchor() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let env = crate::test::env::lock();
        env.remove(OCX_TOOLCHAIN_DIR);

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

    /// C-017 — a `..` component is refused outright rather than normalised
    /// away. Lexical removal and POSIX resolution disagree whenever a preceding
    /// component is a symlink (`$HOME/link/../x` is lexically `$HOME/x` and
    /// actually `/x`), which would admit a root outside `$HOME` that the config
    /// never named. Same rule the shipped consent path applies
    /// (`EntryDefect::ParentDirComponent`).
    #[test]
    fn refuses_a_parent_dir_component_rather_than_normalising_it_away() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let _env = anchored_env(sandbox.path());
        let home = crate::file_structure::home_directory().expect("this host resolves a home directory");

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

    /// R-W1 — a root at or under `$OCX_HOME/toolchain` passes C-017 (it *is*
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

    /// C-017 — a sibling of `$OCX_HOME/toolchain` whose name merely starts with
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

    /// RUL-5 step 5 — the root's own last component is a symlink out of the
    /// anchor. Only the canonicalise-and-re-check step catches it: every
    /// lexical check above passes, because the declared path *is* under the
    /// anchor.
    ///
    /// The anchor is **injected** through RUL-26's seam, for the reason
    /// [`refuses_a_sibling_whose_path_bytes_merely_start_with_an_anchor`] gives:
    /// "outside the anchor" has to mean outside a directory this row controls,
    /// not outside wherever `TMPDIR` happens to place the temp root relative to
    /// the real home. The link target sits beside the anchor rather than in a
    /// second temp root, so one sandbox holds both halves.
    #[cfg(unix)]
    #[test]
    fn refuses_a_root_whose_final_component_symlinks_out_of_the_anchor() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let env = crate::test::env::lock();
        env.remove(OCX_TOOLCHAIN_DIR);

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

    /// RUL-5 step 5 — the positive control for the row above: a symlink that
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

    /// RUL-5 step 5 — **the anchors are canonicalised too.** `$OCX_HOME` is a
    /// symlink; a root beneath it must still be accepted. Canonicalising only
    /// the candidate mis-refuses a legitimate root, because the canonical
    /// candidate is under the anchor's *target* and the anchor is still spelled
    /// as the link.
    ///
    /// Stands in for the symlinked `$HOME` the ADR's containment paragraph
    /// implies: C-017 treats the two anchors identically and only `$OCX_HOME`
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

    /// S-011 — `/tmp/ocx-tc`, owner-owned and mode 0700, still exits 78.
    /// Containment, not permissions, is what refuses it: the root satisfies
    /// C-019 exactly. The ADR's decisive red is this row — drop the containment
    /// check and watch it pass.
    ///
    /// On macOS `/tmp` canonicalises to `/private/tmp` and C-018 refuses it
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
    /// `../tc` is both relative and `..`-bearing; RUL-29 settled which answer
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

    /// RUL-29 — `../tc` reports **`Relative`**, not `ParentDirComponent`.
    ///
    /// RUL-5 step 3 names both refusals in one breath; RUL-29 settled it
    /// afterwards. One answer on record is worth a row of its own: the two
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
        env.set(OCX_TOOLCHAIN_DIR, "./tc");

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

        env.set(OCX_TOOLCHAIN_DIR, "");
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

    // ── RUL-5 steps 1-2 · `~` expands, `%VAR%` never does ───────────────────

    /// RUL-5 step 1 and ADR validation item 18 — `~/.cache/ocx/toolchain` is
    /// **accepted**. It is the ADR's own worked example, and
    /// `Path::new("~/.cache/ocx/toolchain").is_absolute()` is `false`, so an
    /// implementation testing absoluteness before expanding refuses the
    /// documented value.
    #[test]
    fn expands_a_leading_tilde_before_testing_absoluteness() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let _env = anchored_env(sandbox.path());
        let home = crate::file_structure::home_directory().expect("this host resolves a home directory");

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

    /// RUL-5 step 1 — only a **leading** `~` expands. `<anchor>/~/tc` names a
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

    /// RUL-5 step 1 — `~user` is unsupported, surfaced as the shipped
    /// [`EntryDefect`](crate::config::shell::EntryDefect) rather than a second
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
                    defect: crate::config::shell::EntryDefect::UnsupportedTildeUser,
                    ..
                }
            ),
            "'~someone/tc' is refused as an unsupported tilde-user form, carrying the shipped defect; got {error}"
        );
    }

    /// RUL-5 step 2 — **no `%VAR%` expansion on any platform.** A literal
    /// `%LOCALAPPDATA%` value falls through to the checks above and exits 78,
    /// which is diagnosable.
    ///
    /// The discriminating half: `LOCALAPPDATA` is set to a value that *would*
    /// be accepted if it were expanded. The refusal must be unaffected.
    #[test]
    fn performs_no_percent_variable_expansion_on_any_platform() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let env = anchored_env(sandbox.path());
        env.set("LOCALAPPDATA", sandbox.path().to_str().expect("anchor path is utf-8"));

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

    // ── C-018 · system prefixes ─────────────────────────────────────────────

    /// C-018 — every listed prefix is refused **even when containment admits
    /// it**. `$OCX_HOME` is `/` for this row, so C-017 passes on every value
    /// and only C-018 can produce the refusal: the check runs in its own right,
    /// never as containment's fallback.
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

    /// C-018 — the subtree, not just the prefix itself. `/usr/local/tc` is the
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

    /// C-018 — **the order-pinning row.** The containment anchor is itself
    /// `/usr`, so C-017 admits `/usr/tc` outright; C-018 is the only thing left
    /// to refuse it. This is the "absurd `$HOME`" case the ADR names as C-018's
    /// entire reason, and it reds the moment C-018 is gated behind a C-017
    /// failure.
    ///
    /// `$OCX_HOME` stands in for the absurd `$HOME` the ADR describes: C-017
    /// treats the two anchors identically and only this one is injectable.
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

    /// C-018 · BLOCK 3 — **`/var` is refused by identity, not as a subtree.**
    ///
    /// An ostree-composed Fedora (Silverblue, Kinoite, CoreOS, Bazzite) makes
    /// `/home` a symlink to `var/home`, so every passwd entry canonicalises
    /// under `/var`. A bare `/var` subtree prefix therefore refuses *every*
    /// `toolchain_dir` on those hosts — `~/.cache/ocx/toolchain` included,
    /// which is the ADR's own worked example. The directories C-018 defends are
    /// named one by one instead.
    ///
    /// Red against `SYSTEM_PREFIXES` carrying a bare `"/var"`: the first
    /// assertion fires. Red against dropping `/var` altogether: the third does.
    /// The predicate is called directly because no containment anchor a test
    /// can inject would put a candidate under `/var` on this host.
    #[test]
    fn treats_var_as_a_system_location_by_identity_and_not_as_a_subtree() {
        let _env = crate::test::env::lock();

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

    /// C-018 — **`/private` stays a subtree prefix.** Recorded decision, not an
    /// open question.
    ///
    /// It names the macOS system firmlink and is what keeps `/etc` refused
    /// there after canonicalisation (`/etc` → `/private/etc`). Unlike bare
    /// `/var` it swallows no ordinary user home — a macOS home is
    /// `/Users/<name>` — so the ADR's worked example resolves. The two things it
    /// does refuse are macOS's *root* home (`/var/root` → `/private/var/root`,
    /// and `ocx` under `sudo` is not a documented flow) and a `$TMPDIR`-rooted
    /// `$OCX_HOME`, which is a fixture concern that [`anchor_sandbox`] handles
    /// by skipping rather than a contract concern.
    ///
    /// Red in both directions: drop `/private` and the first two assertions
    /// fire; the third is the control that pins what `/private` must not reach.
    #[test]
    fn keeps_private_as_a_subtree_prefix_for_the_macos_firmlink() {
        let _env = crate::test::env::lock();

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

    /// C-018 — the POSIX arms fold ASCII case, like every other refusal.
    ///
    /// macOS's default volume is case-insensitive, so `/USR`, `/library` and
    /// `/VAR` name the directories C-018 lists. Pass 2 canonicalises a spelling
    /// that *resolves*, but pass 1 runs on the lexical value and C-018 is
    /// defence in depth for exactly the state where every other check passed
    /// (CWE-178).
    ///
    /// Red against `Path::eq` / `Path::starts_with` on either arm.
    #[test]
    fn refuses_a_system_location_in_any_ascii_case() {
        let _env = crate::test::env::lock();

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

    /// C-018 · BLOCK 4 — **the Windows clause, exercised on this host.**
    ///
    /// `%SystemRoot%` and the three `Program*` directories exist on no POSIX
    /// machine, and the runner that compiles and runs these rows on every pull
    /// request (`.github/workflows/verify-basic.yml`,
    /// `.github/workflows/verify-deep.yml`) is Linux. So the prefix list is a
    /// parameter — RUL-26's shape, the same reason `resolve_with_anchors` takes
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

    /// C-018 · BLOCK 4 — the prefix list itself comes from the environment,
    /// with the shipped paths as fallbacks.
    ///
    /// A Windows host that relocated `%ProgramFiles%` is covered by the
    /// variable; one that unset it is covered by the fallback. Both arms are
    /// asserted here because only [`crate::env::var`]'s injection seam can
    /// reach the first one on a POSIX host.
    #[test]
    fn reads_the_windows_system_locations_from_the_environment() {
        let env = crate::test::env::lock();

        for (key, fallback) in C018_WINDOWS_LOCATIONS {
            env.remove(key);
            assert!(
                windows_system_prefixes().contains(&PathBuf::from(fallback)),
                "with {key} unset the shipped {fallback} stands in"
            );
            env.set(key, r"D:\Relocated");
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
            env.set(key, "");
            assert!(
                windows_system_prefixes().contains(&PathBuf::from(fallback)),
                "an empty {key} is absent rather than a directory named by the empty string"
            );

            // H-R2-1 — a POSIX-absolute value is absent too. A cross-compilation
            // shell on Linux exports these, and `crate::env::var`'s override arm
            // is `#[cfg(test)]`-only, so in a release build the read is live.
            // Without the filter `/home/u` joins the prefix list and every root
            // under the operator's own home is refused as "the system location".
            env.set(key, "/home/u");
            assert!(
                windows_system_prefixes().contains(&PathBuf::from(fallback)),
                "a POSIX-absolute {key} is not a Windows system location"
            );
            assert!(
                !is_system_location(Path::new("/home/u/.cache/ocx/toolchain")),
                "a POSIX-absolute {key} must not turn the operator's home into a C-018 location"
            );
            env.remove(key);
        }
    }

    // ── The checked path must be a directory ────────────────────────────────

    /// A `toolchain_dir` naming an existing **regular file** is refused, and so
    /// is one whose nearest existing ancestor is that file.
    ///
    /// C-019 alone admits both: `tempfile`'s file is owner-owned and its mode
    /// carries neither `0o020` nor `0o002`, so the ownership half passes and
    /// the renderer's `create_dir_all` would become the diagnosis. Asserts the
    /// exit code through the route a binary takes (`ToolchainRootError` →
    /// [`crate::config::error::Error`] → `classify_error`), not through
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

            let rendered = error.to_string();
            let routed = crate::config::error::Error::from(error);
            assert_eq!(
                classify_error(&routed),
                ExitCode::ConfigError,
                "a toolchain_dir naming a non-directory must exit 78, not 1: {rendered}"
            );
        }
    }

    // ── C-019 · owner and mode ──────────────────────────────────────────────

    /// C-019 — an existing owner-owned root that grants write to neither group
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

    /// C-019 — group- or world-writable is refused, naming the mode. Another
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

    /// RUL-5 step 6 — an **absent** root is accepted, and C-019 falls to the
    /// nearest existing ancestor: the directory the renderer will create under.
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

    /// RUL-5 step 6 — an absent root whose nearest existing ancestor is
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

    /// RUL-5 — a genuine I/O failure while walking the path chain is its own
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

    /// C-019 — a checked directory owned by another account is refused.
    ///
    /// Creating a directory owned by a second uid needs privileges this suite
    /// does not have, so the row borrows an existing one. When the host has
    /// none, it skips reporting what it observed for every candidate — never a
    /// cause it did not see.
    ///
    /// "Other-owned" is necessary but **not sufficient**, and assuming it was
    /// is what made this row red on macOS. C-018 runs before C-019, so a
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

    /// RUL-5 — **`resolve` has no filesystem side effect.** Creating the root
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

    // ── RUL-3 · tier precedence ─────────────────────────────────────────────

    /// RUL-3 — `config.toml` beats `OCX_TOOLCHAIN_DIR`. The environment
    /// variable is the **weakest** tier, matching C-007's rule for the sibling
    /// toolchain keys, not an override.
    #[test]
    fn prefers_the_config_file_over_the_environment_variable() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let env = anchored_env(sandbox.path());
        env.set(
            OCX_TOOLCHAIN_DIR,
            sandbox.path().join("from-env").to_str().expect("path is utf-8"),
        );

        let root = accepted(ToolchainRoot::resolve(&config_tier(sandbox.path().join("from-file"))));
        assert_eq!(
            root.as_path(),
            canonical(sandbox.path()).join("from-file"),
            "the config.toml tier decides; the environment variable is the weakest tier"
        );
    }

    /// RUL-3 — an **invalid** environment value is never reached when the
    /// config file declares one, so it never refuses. Non-obvious, and pinned
    /// so nobody turns it into an eager validation of a tier that lost.
    #[test]
    fn never_reaches_an_invalid_environment_value_when_the_config_file_declares_one() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let env = anchored_env(sandbox.path());
        env.set(OCX_TOOLCHAIN_DIR, "/usr");

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

        env.set(OCX_TOOLCHAIN_DIR, value.as_str());
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

    // ── C-016 · the key, its merge, and what ignores it ─────────────────────

    /// C-016 — the higher tier wins and a `None` never clobbers a lower tier's
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

        // RUL-24 — an empty value is absent at the tier ladder, so it has to be
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

    /// C-016 — the accessor reports the merged value, unvalidated. It is what a
    /// file said; `/usr` reaches a caller unchanged, which is why passing it to
    /// a home resolver is the bypass R-W2 exists to close.
    #[test]
    fn the_accessor_reports_the_merged_value_without_validating_it() {
        assert_eq!(Config::default().toolchain_dir(), None);
        assert_eq!(config_tier("/usr").toolchain_dir(), Some(Path::new("/usr")));
    }

    /// C-016 — **the global home ignores `toolchain_dir` entirely.**
    /// `$OCX_HOME/toolchain/` never moves, whatever any tier declares.
    #[test]
    fn the_global_toolchain_home_ignores_every_declared_root() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let env = anchored_env(sandbox.path());
        env.set(
            OCX_TOOLCHAIN_DIR,
            sandbox.path().join("elsewhere").to_str().expect("path is utf-8"),
        );

        let structure = crate::file_structure::FileStructure::new();
        assert_eq!(
            structure.toolchain.root(),
            sandbox.path().join("toolchain"),
            "the global home is $OCX_HOME/toolchain and no toolchain_dir tier moves it"
        );
    }

    // ── RUL-7 · the refusal has to reach the process exit code ──────────────

    /// Every refusal classifies as 78. On its own this proves **nothing** about
    /// what a user sees — the impl it exercises is reached only through
    /// [`crate::config::error::Error`] — so it is the control for the row
    /// below, which follows the route a binary actually takes.
    #[test]
    fn every_refusal_variant_classifies_as_a_config_error() {
        for refusal in every_refusal_variant() {
            assert_eq!(
                refusal.classify(),
                Some(ExitCode::ConfigError),
                "every toolchain_dir refusal is a config error (78); {refusal} was not"
            );
        }
    }

    /// RUL-7 — **the refusal is reachable as exit 78 from a binary.** The route
    /// is `ToolchainRootError` → [`crate::config::error::Error`] →
    /// `cli::classify_error`, which is what `main` runs; the row above stays
    /// green when that route is broken, so this one is what separates
    /// "classified" from "reachable".
    #[test]
    fn every_refusal_reaches_the_binary_exit_code_as_78() {
        for refusal in every_refusal_variant() {
            let rendered = refusal.to_string();
            let routed = crate::config::error::Error::from(refusal);
            assert_eq!(
                classify_error(&routed),
                ExitCode::ConfigError,
                "a refusal reaching main must exit 78, not 1: {rendered}"
            );
        }
    }

    // ── RUL-26 · the anchors as a parameter ─────────────────────────────────
    //
    // Every row below reaches `resolve_with_anchors`, the testable core, and
    // supplies an anchor pair no in-process seam can produce. They are the rows
    // the module doc names as unreachable while the home directory was read
    // rather than passed.

    /// RUL-25 — **fail closed when nothing anchors.** With neither a home
    /// directory nor `$OCX_HOME` resolvable, C-017 has nothing to compare
    /// against, and the refusal says that rather than claiming the value is
    /// outside two directories that do not exist.
    #[test]
    fn refuses_every_root_when_no_containment_anchor_resolves() {
        let env = crate::test::env::lock();
        env.remove(OCX_TOOLCHAIN_DIR);
        let anchors = ContainmentAnchors {
            home: None,
            ocx_home: None,
        };

        // Absolute *to this host's parser*, and not a C-018 system location —
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

    /// RUL-25 — an anchor that cannot be canonicalised is **dropped**, and the
    /// other one still admits its own descendants. `$OCX_HOME` is `~/.ocx`,
    /// which does not exist on a fresh machine, so treating an unresolvable
    /// anchor as fatal would refuse every root on exactly the hosts the ADR's
    /// worked example targets.
    ///
    /// The non-vacuity half is the row above: dropping *both* refuses.
    #[test]
    fn drops_an_anchor_that_cannot_be_canonicalised_and_keeps_the_other() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let env = crate::test::env::lock();
        env.remove(OCX_TOOLCHAIN_DIR);
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

    /// RUL-25 as corrected · BLOCK 1 — **the fail-closed drop applies only to
    /// the admitting anchor set.**
    ///
    /// `$OCX_HOME` is `~/.ocx`, which does not exist on a fresh machine. An
    /// anchor dropped from the *refusing* roles there stops refusing exactly
    /// what RUL-4 and R-W1 exist to refuse — and the home directory, which does
    /// exist, then admits both values through C-017.
    ///
    /// Red against one dropped-when-absent spelling set: `resolve` returns
    /// `Ok(Some(..))` for both candidates.
    #[test]
    fn refuses_an_absent_ocx_home_and_the_toolchain_directory_under_it() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let env = crate::test::env::lock();
        env.remove(OCX_TOOLCHAIN_DIR);
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

    /// RUL-4 · R-W1 · BLOCK 2 — **the refusing comparisons fold ASCII case.**
    ///
    /// macOS's default volume and every NTFS volume are case-**insensitive**:
    /// there `$OCX_HOME/TOOLCHAIN` and `$OCX_HOME/toolchain` are one directory,
    /// and a byte-wise compare walks the second spelling straight past both
    /// refusals (CWE-178). The home directory is the surrounding anchor, so
    /// C-017 admits every candidate below and only these two refusals can
    /// produce the answer.
    ///
    /// Red against `Path::eq` / `Path::starts_with`: every spelling but the
    /// literal `ocx-home`/`toolchain` pair is accepted. The literal pair is the
    /// non-vacuity control — it must keep refusing either way, so the row
    /// cannot pass by refusing everything.
    #[test]
    fn refuses_the_anchor_and_the_global_toolchain_home_in_any_ascii_case() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let env = crate::test::env::lock();
        env.remove(OCX_TOOLCHAIN_DIR);
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

    /// RUL-5 step 5 — **a symlinked home directory contains its own
    /// descendants.** The shipped row proves this against `$OCX_HOME` because
    /// that is the only anchor `crate::test::env` can redirect; with the anchors
    /// passed in, the home directory itself can carry the symlink.
    ///
    /// Red: canonicalise only the candidate and not the anchors, and this
    /// legitimate root is refused as `OutsideHome`.
    #[cfg(unix)]
    #[test]
    fn accepts_a_root_under_a_symlinked_home_directory() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let env = crate::test::env::lock();
        env.remove(OCX_TOOLCHAIN_DIR);

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

    /// RUL-5 step 1 — a machine with **no** home directory cannot expand a
    /// leading `~`, and reports the shipped
    /// [`EntryDefect`](crate::config::shell::EntryDefect) rather than inventing a
    /// second vocabulary for it. `$OCX_HOME` still resolves, so the refusal is
    /// expansion's and not containment's.
    #[test]
    fn refuses_a_tilde_root_on_a_machine_with_no_home_directory() {
        let Some(sandbox) = sandbox_or_skip() else { return };
        let env = crate::test::env::lock();
        env.remove(OCX_TOOLCHAIN_DIR);
        let anchors = ContainmentAnchors {
            home: None,
            ocx_home: Some(sandbox.path().to_path_buf()),
        };

        let error = refusal(ToolchainRoot::resolve_with_anchors(&config_tier("~/tc"), &anchors));
        assert!(
            matches!(
                error,
                ToolchainRootError::Unexpandable {
                    defect: crate::config::shell::EntryDefect::UnresolvableHome,
                    ..
                }
            ),
            "a leading '~' with no home directory to expand against is an expansion failure; got {error}"
        );
    }

    /// RUL-31 — the list transcribed at the top of this module and the one the
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
        let _env = crate::test::env::lock();

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

    /// One value of each [`ToolchainRootError`] variant.
    ///
    /// Listed by hand rather than derived: the enum is `#[non_exhaustive]` and
    /// has no iterator, so a variant added without a row here is invisible to
    /// both exit-code rows. The compile-time guard against that is
    /// `classify`'s own wildcard-free `match`.
    fn every_refusal_variant() -> Vec<ToolchainRootError> {
        let tier = ToolchainRootTier::ConfigFile;
        let path = || PathBuf::from("/tmp/ocx-tc");
        vec![
            ToolchainRootError::Unexpandable {
                tier,
                declared: path(),
                defect: crate::config::shell::EntryDefect::UnsupportedTildeUser,
            },
            ToolchainRootError::Relative { tier, declared: path() },
            ToolchainRootError::ParentDirComponent { tier, declared: path() },
            ToolchainRootError::NoContainmentAnchor { tier, declared: path() },
            ToolchainRootError::IsContainmentAnchor {
                tier,
                resolved: path(),
                anchor: path(),
            },
            ToolchainRootError::OutsideHome { tier, resolved: path() },
            ToolchainRootError::SystemPrefix { tier, resolved: path() },
            ToolchainRootError::InsideGlobalToolchainHome {
                tier,
                resolved: path(),
                global_home: path(),
            },
            ToolchainRootError::Inaccessible {
                tier,
                resolved: path(),
                checked: path(),
                source: std::io::Error::from_raw_os_error(13),
            },
            ToolchainRootError::NotADirectory {
                tier,
                resolved: path(),
                checked: path(),
            },
            ToolchainRootError::NotOwnerOwned {
                tier,
                resolved: path(),
                checked: path(),
                owner: "0".to_owned(),
                effective_user: "1000".to_owned(),
            },
            ToolchainRootError::GroupOrWorldWritable {
                tier,
                resolved: path(),
                checked: path(),
                mode: 0o777,
            },
        ]
    }
}
