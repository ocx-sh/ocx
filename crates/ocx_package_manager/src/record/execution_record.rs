// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The record payload, and the inputs a launching frame supplies to build it.
//!
//! The key set is frozen wire format and deliberately mixed-case; a blanket
//! `rename_all` would rewrite `process.working_directory` and break every consumer.
//! Best-effort fields omit their key when undeterminable, never an `"unknown"` sentinel.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Serialize, Serializer};
use serde_json::Value;

use super::error::RecordsError;
use super::purl::{has_logical_identity, package_url};
use crate::tasks::resolve::AdmittedClaims;
use ocx_config::env::OcxConfigView;
use ocx_config::mirror::MirrorConfig;
use ocx_oci::ssrf::allows_plain_http;
use ocx_oci::{Architecture, Digest, OperatingSystem, PackageRef, PinnedPackageRef, Platform};
use ocx_package::install_info::InstallInfo;
use ocx_package::metadata::visibility::Visibility;

/// The in-band schema version, bumped only for a backward-incompatible change;
/// additive fields never bump it.
pub const SCHEMA_VERSION: &str = "1";

/// The `kind` discriminator carried by every record.
pub const RECORD_KIND: &str = "sh.ocx.execution-record";

/// The in-band `identityNote` of a degraded launcher frame.
const DEGRADED_IDENTITY_NOTE: &str = "package directories are content-shared and carry no registry/repository, so \
     logical identity is not recoverable in this frame and no purl can be emitted";

/// One pre-exec resolution record: the full resolved closure plus the resolved
/// executable, written immediately before the child starts.
///
/// Serialized **compact, one document per file, one line**.
// One line: every mainstream log shipper is line-oriented, so a pretty-printed record breaks ingestion.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct ExecutionRecord {
    /// Format version, a string bumped only for a backward-incompatible change.
    #[serde(rename = "schemaVersion")]
    pub schema_version: String,

    /// Format discriminator; always `sh.ocx.execution-record`.
    pub kind: String,

    /// When the record was written — RFC 3339, UTC, millisecond precision.
    #[serde(rename = "recordedAt", serialize_with = "serialize_rfc3339_millis")]
    #[schemars(with = "String")]
    pub recorded_at: DateTime<Utc>,

    /// The ocx build that produced the record.
    pub ocx: OcxBuild,

    /// Which command frame launched, and how complete its identity data is.
    pub frame: Frame,

    /// The process that runs the tool.
    pub process: Process,

    /// The machine, best-effort.
    pub host: Host,

    /// The operating system, best-effort.
    pub os: Os,

    /// `sh.ocx.*` facts about the resolved executable: provenance, kind, and the
    /// package purl it came from, as a flat string map.
    ///
    /// `sh.ocx.provenance` is the field an auditor reads first — `ocx-package`
    /// when the resolved path lands inside the store, `external` when it does
    /// not. `ocx exec -- bash …` picking up the *system* bash is a fact this
    /// record states out loud.
    pub executable: BTreeMap<String, String>,

    /// What the launching frame was scoped to.
    pub scope: ScopeBlock,

    /// The resolution policy in force — what makes drift auditable.
    pub resolution: Resolution,

    /// The resolved package closure in topological order: roots tagged
    /// `sh.ocx.role: root`, the transitive closure `dependency`, and the patch
    /// companions the site tier overlaid onto them `companion`.
    pub packages: Vec<ResourceDescriptor>,
}

/// The ocx build that produced a record.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct OcxBuild {
    /// Crate version of the running binary.
    pub version: String,
    /// Absolute path to the running binary.
    pub binary: PathBuf,
}

/// Which frame launched, and whether it could name everything it resolved.
///
/// Not a bare enum: every record carries `command` *and* `identity`, and a
/// degraded frame adds `identityNote` explaining what it could not determine.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct Frame {
    /// The launching command.
    pub command: FrameCommand,

    /// Whether the frame could name the packages it resolved.
    pub identity: FrameIdentity,

    /// Why `identity` is `degraded`. Omitted when it is complete.
    #[schemars(extend("x-ocx-absent-when-none" = true))]
    #[serde(rename = "identityNote", skip_serializing_if = "Option::is_none")]
    pub identity_note: Option<String>,
}

/// The command that opened a launching frame.
///
/// The wire values are the CLI's own canonical command names, space-separated:
/// one grep joins a record to the error envelope of the same invocation.
// Must match `ocx_cli::app::canonical_command_name`, or two ocx JSON documents name one command two ways.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub enum FrameCommand {
    /// `ocx exec` — the project tier.
    ///
    /// Also what the hidden deprecated `ocx run` spelling records.
    // The record states what executed; the deprecation is the CLI's business, not the audit trail's.
    #[serde(rename = "exec")]
    Exec,
    /// `ocx package exec` — the OCI tier.
    #[serde(rename = "package exec")]
    PackageExec,
    /// `ocx launcher exec` — a generated entrypoint re-entry.
    #[serde(rename = "launcher exec")]
    LauncherExec,
    /// `ocx launcher shim` — a deferred tool's first invocation, the one frame
    /// that downloads the content it is about to run.
    #[serde(rename = "launcher shim")]
    LauncherShim,
}

/// How completely a frame could name what it resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub enum FrameIdentity {
    /// Full logical identity: registry, repository, tag and digest.
    #[serde(rename = "complete")]
    Complete,
    /// Digest-complete but name-degraded. Package directories are
    /// content-shared and carry no registry/repository, so a launcher frame
    /// cannot recover logical identity and emits no purl.
    // Never fabricate identity here: a truthful partial record beats a fabricated complete one.
    #[serde(rename = "degraded")]
    Degraded,
}

/// The process that runs the tool.
///
/// `pid` means the same thing on both platforms so a consumer never branches on
/// OS: on Unix it is ocx's own pid, which `execvp` turns *into* the tool; on
/// Windows it is the spawned child's.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct Process {
    /// The process that runs the tool.
    pub pid: u32,

    /// The launching process. Best-effort; omitted when undeterminable.
    #[schemars(extend("x-ocx-absent-when-none" = true))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<ParentProcess>,

    /// The invoking user. Best-effort; omitted when undeterminable — a scratch
    /// container with no passwd entry is the common case.
    #[schemars(extend("x-ocx-absent-when-none" = true))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<User>,

    /// Architecture of the running ocx binary, e.g. `amd64`.
    ///
    /// The **process's** architecture, not the machine's: an amd64 ocx under
    /// Rosetta or qemu on an arm64 host reports `amd64` here, which is exactly
    /// true of the process that ran. The machine's native architecture is not
    /// recorded. An architecture outside the OCI architecture vocabulary is
    /// undeterminable here and omits the key.
    #[schemars(extend("x-ocx-absent-when-none" = true))]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub arch: Option<Architecture>,

    /// The resolved absolute executable — the record's highest-value field.
    pub executable: PathBuf,

    /// The working directory the child inherits.
    ///
    /// Best-effort; omitted when undeterminable.
    // Snake-cased, unlike its camelCase siblings: the published spelling of the vocabulary this block borrows.
    #[schemars(extend("x-ocx-absent-when-none" = true))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<PathBuf>,
}

/// The launching process.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct ParentProcess {
    /// The launching process id.
    pub pid: u32,
}

/// The invoking user.
///
/// Two fields with different trust, which is the whole point of carrying both:
/// `id` comes from the kernel and cannot be forged by the caller's
/// environment, while `name` is read from the environment and can be.
/// An audit sink correlating *who ran this* must key on `id`.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct User {
    /// The effective user id the kernel reports, as a string.
    ///
    /// Omitted when undeterminable — currently always on Windows.
    // A string, not a number, so a Windows SID (`S-1-5-…`) fits the same key once a token lookup reads it.
    #[schemars(extend("x-ocx-absent-when-none" = true))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    /// Account name, best-effort and **not trustworthy**: it is read from
    /// `$USER`/`$LOGNAME` (`%USERNAME%` on Windows), all of which the caller
    /// controls. Present for readability; `id` is the field to key on.
    ///
    /// Omitted when undeterminable — a scratch container sets none of them.
    #[schemars(extend("x-ocx-absent-when-none" = true))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// The machine, best-effort — every field omits rather than guesses.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct Host {
    /// Hostname. Omitted when undeterminable.
    #[schemars(extend("x-ocx-absent-when-none" = true))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// The operating system, best-effort.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct Os {
    /// OS family, e.g. `linux`. Omitted when undeterminable.
    #[schemars(extend("x-ocx-absent-when-none" = true))]
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub os_type: Option<OperatingSystem>,
}

/// One resolved package, shaped field-for-field like an in-toto resource
/// descriptor so a consumer can lift `packages` straight into an attestation's
/// resolved-dependency list without a translator.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct ResourceDescriptor {
    /// The last repository segment, e.g. `cmake`.
    ///
    /// **Not unique on its own** — `ocx.sh/a/cli` and `ocx.sh/b/cli` both yield
    /// `cli`. Identity is name + `repository_url` + digest; a consumer joining
    /// on `name` alone is wrong.
    pub name: String,

    /// A `pkg:oci` package URL carrying the digest as its version.
    ///
    /// Omitted — never fabricated — when no logical identity exists, which is
    /// the launcher frame's synthetic content-addressed identifier. The digest
    /// alone still identifies the resource.
    #[schemars(extend("x-ocx-absent-when-none" = true))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,

    /// Content digest as `algorithm → bare lowercase hex`.
    ///
    /// Never `sha256:<hex>` inside the value, never a transport prefix, never
    /// uppercase: the algorithm is the key, which is the point of the map. The
    /// value is the **platform leaf** manifest digest, never the multi-arch
    /// index digest — it names the exact bits that ran.
    pub digest: BTreeMap<String, String>,

    /// `sh.ocx.*` annotations: role, binding, group, platform, visibility,
    /// declared binaries and entry points, and the resolved-from marker that
    /// makes tag drift visible.
    ///
    /// Values are JSON, not strings: the name lists are lists.
    // Never join a list into one string: no separator is forbidden in a binary name.
    pub annotations: BTreeMap<String, Value>,
}

/// The record's `scope` block.
///
/// Internally tagged on `tier`, matching the three-way union of the published
/// shape. The launcher variant carries nothing else: a launcher re-entry has no
/// project context and did not compose the environment itself.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(tag = "tier")]
pub enum ScopeBlock {
    /// `ocx exec` — a project toolchain.
    #[serde(rename = "project")]
    Project {
        /// Whether the child env was built clean rather than inherited.
        #[serde(rename = "cleanEnv")]
        clean_env: bool,
        /// Directory holding `ocx.toml`.
        #[serde(rename = "projectRoot")]
        project_root: PathBuf,
        /// The lock the closure was resolved through.
        lock: LockReference,
        /// Selected groups, in selection order.
        groups: Vec<String>,
    },

    /// `ocx package exec` — identifiers named on the command line.
    #[serde(rename = "package")]
    Package {
        /// Whether the child env was built clean rather than inherited.
        #[serde(rename = "cleanEnv")]
        clean_env: bool,
        /// Identifiers exactly as requested.
        requested: Vec<String>,
    },

    /// `ocx launcher exec` — a generated entrypoint re-entry.
    #[serde(rename = "launcher")]
    Launcher,
}

/// The lock file a project-tier closure was resolved through.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct LockReference {
    /// Absolute path to `ocx.lock`.
    pub path: PathBuf,

    /// The lock's declaration hash, as `algorithm → bare lowercase hex`.
    ///
    /// Hashes the `ocx.toml` **declarations** the lock was generated from, as
    /// carried in the lock's own metadata; it is **not** a digest of the lock's
    /// contents. Two runs whose declarations agree share this value even when
    /// they resolved different closures, so never use it as a closure identity:
    /// `packages[]` records what actually ran.
    #[serde(rename = "declarationDigest")]
    pub declaration_digest: BTreeMap<String, String>,
}

/// The resolution policy in force for this invocation.
///
/// Frame-varying fields distinguish two states an empty collection cannot: a
/// missing key (or `null`) means the frame has no such context at all (the
/// launcher frame), an empty value means it has the context and it is empty —
/// a package-tier record carries `"mirrors": {}`, a launcher record omits it.
///
/// `autoInstalled` together with a `sh.ocx.resolved-from: tag` annotation is
/// what shows an invocation resolved a floating tag and materialized the
/// package on the spot — the one state no pull-time record can capture.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct Resolution {
    /// Whether `--offline` was in force.
    pub offline: bool,

    /// Whether `--remote` was in force.
    pub remote: bool,

    /// Whether `--frozen` was in force — the **package tier** only.
    ///
    /// The flag freezes package resolution; it does not freeze the patch tier.
    /// A patch companion resolves live under it and pins in
    /// `$OCX_HOME/state/patch-companions/`, and freezing that tier is
    /// `ocx patch freeze` plus `OCX_PATCH_SNAPSHOT`. So this field says "the
    /// package tier could not move", never "this invocation was frozen".
    pub frozen: bool,

    /// The patch snapshot in force, as `algorithm → bare lowercase hex` over
    /// the snapshot **file's own bytes**.
    ///
    /// The patch tier's answer to `frozen`, which scopes to the package tier
    /// alone. A snapshot is selected by `OCX_PATCH_SNAPSHOT` naming a
    /// `patches.snapshot.json` that `ocx patch freeze` wrote; under one, every
    /// companion composes at the digest the snapshot pins. Omitted when no
    /// snapshot is in force — a different statement from "a snapshot with no pins".
    #[schemars(extend("x-ocx-absent-when-none" = true))]
    #[serde(rename = "patchSnapshot", skip_serializing_if = "BTreeMap::is_empty")]
    pub patch_snapshot: BTreeMap<String, String>,

    /// Whether the policy-gated auto-verify was opted out of (`OCX_NO_VERIFY`).
    ///
    /// An auditor reading a record from a signing-enforced fleet needs to see
    /// that this invocation ran with verification disabled — the record is
    /// otherwise silent about it and the packages look identically resolved.
    ///
    /// The env-tier opt-out only. The per-command `--no-verify` flag is a
    /// one-shot user choice that never reaches a launching frame, so no
    /// launching frame can report it.
    #[serde(rename = "noVerify")]
    pub no_verify: bool,

    /// The platform OCX **asked** resolution for, in the canonical platform
    /// grammar — `linux/amd64+libc.glibc`.
    ///
    /// The request, not the outcome: what each package's manifest leaf actually
    /// resolved to is its own `sh.ocx.platform` annotation; a flat single-image
    /// package legitimately selects `any` under any request.
    ///
    /// Emitted as an explicit `null` when the frame has no platform context —
    /// never omitted, and never fabricated from the host, which would make the
    /// record lie in exactly the audit that matters.
    // A string, not `Platform`, which serializes as the OCI descriptor object: a different wire shape.
    #[serde(rename = "requestedPlatform")]
    pub requested_platform: Option<String>,

    /// Registries the frame's packages were fetched from.
    ///
    /// The **content** registries: the physical hosts the transport addressed,
    /// not the index endpoints a version choice was looked up through, and not
    /// the logical namespaces the identifiers name — a package named
    /// `ocx.sh/acme/tool` can be resolved through `index.ocx.sh` and fetched
    /// from `ghcr.io`. The logical namespace is each `packages[]` purl's
    /// `repository_url`, so a divergence between the two is visible. Reported
    /// before any `[mirrors]` rewrite of that host; a root whose resolution
    /// named no content registry is omitted rather than guessed at.
    #[schemars(extend("x-ocx-absent-when-none" = true))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registries: Option<Vec<String>>,

    /// Which of `registries` this invocation was allowed to reach over plain HTTP.
    ///
    /// The subset declared plaintext-eligible by `[registries."<name>"]
    /// insecure = true` or `OCX_INSECURE_REGISTRIES`, intersected with the
    /// registries this frame actually fetched from: a host that both served
    /// content *and* was exempt from TLS is exactly the finding an auditor is
    /// looking for. Omitted when empty, never `[]`.
    #[schemars(extend("x-ocx-absent-when-none" = true))]
    #[serde(rename = "insecureRegistries", skip_serializing_if = "Vec::is_empty")]
    pub insecure_registries: Vec<String>,

    /// Active mirror rewrites, upstream traffic host to its replacement
    /// endpoints, one per declared role (`registry`, `index`).
    // Per-host object, not one string: collapsing the roles drops the second endpoint of an entry declaring both.
    #[schemars(extend("x-ocx-absent-when-none" = true))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mirrors: Option<BTreeMap<String, MirrorEndpoints>>,

    /// The managed-config tier in force.
    #[schemars(extend("x-ocx-absent-when-none" = true))]
    #[serde(rename = "managedConfig", skip_serializing_if = "Option::is_none")]
    pub managed_config: Option<ManagedConfigReference>,

    /// Packages materialized during this invocation rather than already
    /// present — the drift signal.
    #[schemars(extend("x-ocx-absent-when-none" = true))]
    #[serde(rename = "autoInstalled", skip_serializing_if = "Option::is_none")]
    pub auto_installed: Option<Vec<String>>,
}

/// The replacement endpoints one mirror entry declares.
///
/// Both fields are optional and at least one is always present — an entry
/// declaring neither role is not a rewrite and never reaches the record.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct MirrorEndpoints {
    /// Endpoint OCI distribution traffic (`/v2`) is routed to.
    #[schemars(extend("x-ocx-absent-when-none" = true))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registry: Option<String>,

    /// Endpoint index-tree traffic is routed to.
    #[schemars(extend("x-ocx-absent-when-none" = true))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<String>,
}

/// The managed-config tier in force.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct ManagedConfigReference {
    /// The configured managed-config source reference.
    pub source: String,
    /// Digest of the applied snapshot, as `algorithm → bare lowercase hex`.
    ///
    /// Omitted when the applied snapshot's digest is not in hand at the
    /// launching frame; the source alone still names which tier was in force.
    // Omitted, never an empty map: that would read as "no digest algorithm applies".
    #[schemars(extend("x-ocx-absent-when-none" = true))]
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub digest: BTreeMap<String, String>,
}

/// Everything a launching frame hands the record builder.
#[derive(Debug)]
pub struct RecordInputs<'a> {
    /// Root packages; the transitive closure rides inside each resolved package.
    pub packages: &'a [Arc<InstallInfo>],

    /// Claimed executable names per package, and the patch companions the site tier overlaid.
    pub admitted: &'a AdmittedClaims,

    /// The resolved executable, as `Env::resolve_command` produced it.
    pub executable: &'a Path,

    /// Root of the package store; containment under it decides `sh.ocx.provenance`.
    pub store_root: &'a Path,

    /// Root of the shim store; without it every deferred tool's launcher records as `external`.
    pub shim_root: &'a Path,

    /// `argv[0]` plus arguments, as invoked; never recorded, since a command line
    /// can carry tokens a central sink must not hold.
    pub argv: &'a [String],

    /// Resolution policy, feeding the `resolution` block.
    pub config: &'a OcxConfigView,

    /// Every host this process may contact over plain HTTP, unfiltered: the record
    /// intersects it through `allows_plain_http` itself.
    pub insecure_registries: &'a [String],

    /// Digest of the applied managed-config snapshot, passed in so the builder
    /// stays I/O-free on the exec path; `None` omits the key.
    pub managed_config_digest: Option<&'a Digest>,

    /// Digest of the active patch snapshot's file bytes; `None` omits the key.
    pub patch_snapshot_digest: Option<&'a Digest>,

    /// The platform resolution was asked for; `None` at the launcher frame emits
    /// `null` rather than a guess from the host.
    pub platform: Option<&'a Platform>,

    /// Whether the child env was built clean.
    pub clean_env: bool,

    /// Packages materialized during this invocation.
    pub auto_installed: &'a [PackageRef],

    /// The only frame-divergent input.
    pub scope: Scope,
}

/// What the launching frame was scoped to; the input twin of [`ScopeBlock`].
#[derive(Debug)]
pub enum Scope {
    /// `ocx exec` — a project toolchain.
    Project {
        /// Directory holding `ocx.toml`.
        root: PathBuf,
        /// The sibling `ocx.lock`.
        lock: PathBuf,
        /// The loaded lock's `metadata.declaration_hash` (of the `ocx.toml`
        /// declarations, not the lock); recomputing it would need I/O on the exec path.
        declaration_digest: Digest,
        /// Selected groups, in selection order.
        groups: Vec<String>,
        /// Which `ocx.toml` binding each root package was selected under.
        bindings: Vec<PackageBinding>,
    },

    /// `ocx package exec` — identifiers named on the command line.
    Package {
        /// Identifiers as requested.
        requested: Vec<PackageRef>,
    },

    /// `ocx launcher exec` — an entrypoint re-entry: no project context, a synthetic identifier.
    Launcher,

    /// `ocx launcher shim` — a deferred tool's first invocation; not folded into
    /// [`Self::Package`], because [`frame_for`] derives `frame.command` from the scope.
    LauncherShim {
        /// The tool the shim named, exactly as baked.
        requested: PinnedPackageRef,
    },
}

/// The `ocx.toml` binding a project-tier root package was selected under.
#[derive(Debug, Clone)]
pub struct PackageBinding {
    /// Binding name from `ocx.toml`.
    pub binding: String,
    /// The group the binding was selected from.
    pub group: String,
    /// The root package the binding resolved to.
    pub package: PinnedPackageRef,
}

impl ExecutionRecord {
    /// Build a record from a launching frame's inputs; infallible and I/O-free.
    ///
    /// `pid` is the process that runs the tool: ocx's own on Unix, the spawned child's on Windows.
    pub fn build(inputs: &RecordInputs<'_>, recorded_at: DateTime<Utc>, pid: u32) -> Self {
        Self {
            schema_version: SCHEMA_VERSION.to_string(),
            kind: RECORD_KIND.to_string(),
            recorded_at,
            ocx: OcxBuild {
                version: env!("CARGO_PKG_VERSION").to_string(),
                binary: inputs.config.self_exe.clone(),
            },
            frame: frame_for(&inputs.scope),
            process: super::environment::process(pid, inputs.executable),
            host: super::environment::host(),
            os: super::environment::operating_system(),
            executable: executable_block(inputs),
            scope: scope_block(inputs),
            resolution: resolution_block(inputs),
            packages: descriptors(inputs),
        }
    }

    /// Serialize to the published form: one compact JSON document on a single line.
    ///
    /// # Errors
    ///
    /// Returns [`RecordsError::Serialize`] when serialization fails.
    pub fn to_json(&self) -> Result<String, RecordsError> {
        serde_json::to_string(self).map_err(RecordsError::Serialize)
    }
}

/// Which command opened the frame, derived from [`Scope`] so a frame cannot claim
/// identity its scope cannot have.
fn frame_for(scope: &Scope) -> Frame {
    let (command, identity) = match scope {
        Scope::Project { .. } => (FrameCommand::Exec, FrameIdentity::Complete),
        Scope::Package { .. } => (FrameCommand::PackageExec, FrameIdentity::Complete),
        Scope::Launcher => (FrameCommand::LauncherExec, FrameIdentity::Degraded),
        // Complete, unlike `launcher exec`: a shim is baked with the tool's pinned identifier.
        Scope::LauncherShim { .. } => (FrameCommand::LauncherShim, FrameIdentity::Complete),
    };
    Frame {
        command,
        identity,
        identity_note: match identity {
            FrameIdentity::Complete => None,
            FrameIdentity::Degraded => Some(DEGRADED_IDENTITY_NOTE.to_string()),
        },
    }
}

fn scope_block(inputs: &RecordInputs<'_>) -> ScopeBlock {
    match &inputs.scope {
        Scope::Project {
            root,
            lock,
            declaration_digest,
            groups,
            ..
        } => ScopeBlock::Project {
            clean_env: inputs.clean_env,
            project_root: root.clone(),
            lock: LockReference {
                path: lock.clone(),
                declaration_digest: digest_map(declaration_digest),
            },
            groups: groups.clone(),
        },
        Scope::Package { requested } => ScopeBlock::Package {
            clean_env: inputs.clean_env,
            requested: requested.iter().map(ToString::to_string).collect(),
        },
        Scope::Launcher => ScopeBlock::Launcher,
        // A shim composes the package tier (`EnvScope::package_tier`); `frame.command` names the shim.
        // The baked tag is first-writer-wins per shim digest, so it is not what was requested.
        Scope::LauncherShim { requested } => ScopeBlock::Package {
            clean_env: inputs.clean_env,
            requested: vec![requested.strip_advisory().to_string()],
        },
    }
}

/// Project the resolution policy in force.
///
/// The launcher frame omits the context fields: it composed nothing, which is not
/// "composed with none".
fn resolution_block(inputs: &RecordInputs<'_>) -> Resolution {
    let composed = !matches!(inputs.scope, Scope::Launcher);
    let registries = composed.then(|| registries(inputs.packages));
    // Only fetched hosts, through the transport's own predicate, or the record disagrees with the transport.
    let insecure = registries
        .iter()
        .flatten()
        .filter(|host| allows_plain_http(inputs.insecure_registries, host))
        .cloned()
        .collect();
    Resolution {
        offline: inputs.config.offline,
        remote: inputs.config.remote,
        frozen: inputs.config.frozen,
        patch_snapshot: inputs.patch_snapshot_digest.map(digest_map).unwrap_or_default(),
        no_verify: inputs.config.no_verify,
        requested_platform: inputs.platform.map(ToString::to_string),
        registries,
        insecure_registries: insecure,
        mirrors: composed.then(|| mirror_endpoints(&inputs.config.mirrors)),
        managed_config: composed.then(|| managed_config(inputs)).flatten(),
        auto_installed: (!inputs.auto_installed.is_empty()).then(|| {
            inputs
                .auto_installed
                .iter()
                .map(|identifier| match PinnedPackageRef::try_from(identifier.clone()) {
                    Ok(pinned) => recorded_identity(&pinned, &inputs.scope).to_string(),
                    Err(_) => identifier.to_string(),
                })
                .collect()
        }),
    }
}

/// The roots' physical transport hosts ([`InstallInfo::transport_registry`]), sorted
/// and deduplicated; never the logical namespace, which may name a host nothing
/// was fetched from.
fn registries(packages: &[Arc<InstallInfo>]) -> Vec<String> {
    let mut sources: Vec<String> = packages
        .iter()
        .filter(|info| has_logical_identity(info.identifier()))
        .filter_map(|info| info.transport_registry())
        .map(ToString::to_string)
        .collect();
    sources.sort();
    sources.dedup();
    sources
}

/// Project the mirror entries to the frozen `host → {registry?, index?}` shape,
/// credentials stripped.
fn mirror_endpoints(mirrors: &[(String, MirrorConfig)]) -> BTreeMap<String, MirrorEndpoints> {
    mirrors
        .iter()
        .filter(|(_, config)| config.registry.is_some() || config.index.is_some())
        .map(|(host, config)| {
            (
                host.clone(),
                MirrorEndpoints {
                    registry: config.registry.as_deref().map(without_userinfo),
                    index: config.index.as_deref().map(without_userinfo),
                },
            )
        })
        .collect()
}

/// A mirror endpoint with any `user:password@` userinfo removed, else returned byte-for-byte.
///
/// The sink is fleet-aggregated, so a kept credential is copied into every record.
/// The authority ends at the first `/` as in `parse_url`, so the redacted span is
/// the host the config layer sees.
fn without_userinfo(endpoint: &str) -> String {
    let (scheme, rest) = match endpoint.split_once("://") {
        Some((scheme, rest)) => (Some(scheme), rest),
        None => (None, endpoint),
    };
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let Some(at) = rest[..authority_end].rfind('@') else {
        return endpoint.to_string();
    };
    match scheme {
        Some(scheme) => format!("{scheme}://{}", &rest[at + 1..]),
        None => rest[at + 1..].to_string(),
    }
}

/// The managed-config tier in force, when one is.
fn managed_config(inputs: &RecordInputs<'_>) -> Option<ManagedConfigReference> {
    Some(ManagedConfigReference {
        source: inputs.config.managed_config_source.clone()?,
        digest: inputs.managed_config_digest.map(digest_map).unwrap_or_default(),
    })
}

/// `sh.ocx.*` facts about the resolved executable.
///
/// Provenance is store containment, not a walk of the frame's roots, or dependency
/// and launcher-frame binaries record as `external`.
fn executable_block(inputs: &RecordInputs<'_>) -> BTreeMap<String, String> {
    let mut block = BTreeMap::new();
    let in_shim_store = relative_to(inputs.executable, inputs.shim_root).is_some();
    let store_relative = relative_to(inputs.executable, inputs.store_root);
    if !in_shim_store && store_relative.is_none() {
        block.insert("sh.ocx.provenance".to_string(), "external".to_string());
        return block;
    }

    block.insert("sh.ocx.provenance".to_string(), "ocx-package".to_string());
    let kind = if in_shim_store {
        Some("shim")
    } else {
        store_relative.as_deref().and_then(executable_kind)
    };
    if let Some(kind) = kind {
        block.insert("sh.ocx.kind".to_string(), kind.to_string());
    }
    // Only a root is reachable as a directory, so a dependency-owned executable omits the purl.
    if let Some(info) = owning_root(inputs)
        && let Some(purl) = package_url(&recorded_identity(info.identifier(), &inputs.scope), info.platform())
    {
        block.insert("sh.ocx.package".to_string(), purl);
    }
    block
}

/// `executable` relative to `root`: raw spelling first, canonical form only on a miss.
///
/// Raw-only would record every following-lane `ocx exec` (a `toolchain/links/` path) as `external`.
/// The retry canonicalises both operands, or a symlinked `store_root` (macOS `/tmp`) answers `external`.
/// A path that fails to canonicalise (a deferred root not yet on disk) is not contained.
fn relative_to(executable: &Path, root: &Path) -> Option<PathBuf> {
    if let Ok(relative) = executable.strip_prefix(root) {
        return Some(relative.to_path_buf());
    }
    let executable = dunce::canonicalize(executable).ok()?;
    let root = dunce::canonicalize(root).ok()?;
    executable.strip_prefix(root).ok().map(Path::to_path_buf)
}

/// `launcher` under a package's `entrypoints/`, `binary` under its `content/`.
///
/// Scans forward, so a package's own `content/entrypoints/` still reports `binary`.
/// Takes the store-relative path from [`relative_to`], so containment and kind read one spelling.
fn executable_kind(store_relative: &Path) -> Option<&'static str> {
    store_relative
        .components()
        .find_map(|component| match component.as_os_str().to_str()? {
            "entrypoints" => Some("launcher"),
            "content" => Some("binary"),
            _ => None,
        })
}

/// The frame's root package that owns the resolved executable, if one does.
///
/// Checks the shim tree too, or every deferred tool loses `sh.ocx.package`; both via
/// [`relative_to`], or every following-lane project frame loses it.
fn owning_root<'a>(inputs: &'a RecordInputs<'_>) -> Option<&'a Arc<InstallInfo>> {
    inputs.packages.iter().find(|info| {
        relative_to(inputs.executable, info.dir().root()).is_some()
            || info
                .deferred()
                .is_some_and(|deferred| relative_to(inputs.executable, deferred.shim().root()).is_some())
    })
}

/// Project the closure: roots, then each root's dependencies in topological order,
/// then patch companions, then theirs; deduplicated by content identity, first-seen role wins.
fn descriptors(inputs: &RecordInputs<'_>) -> Vec<ResourceDescriptor> {
    let mut seen: HashSet<PinnedPackageRef> = HashSet::new();
    let mut descriptors = Vec::new();
    let admitted = AdmittedIndex::build(inputs.admitted);

    for info in inputs.packages {
        // The selected platform, never the frame's request, or a stored and a freshly pulled root differ.
        if let Some(mut descriptor) = project(
            info.identifier(),
            Placement {
                role: "root",
                visibility: Visibility::PUBLIC,
                platform: info.platform(),
            },
            inputs,
            &admitted,
            &mut seen,
        ) {
            // Without this key a deferred root reads as a package present on disk.
            if info.deferred().is_some() {
                descriptor
                    .annotations
                    .insert("sh.ocx.composition".to_string(), Value::from("deferred"));
            }
            descriptors.push(descriptor);
        }
    }

    for info in inputs.packages {
        for dependency in &info.resolved().dependencies {
            // Its selected platform is not in hand, and the frame's request is a different fact.
            if let Some(descriptor) = project(
                &dependency.identifier,
                Placement {
                    role: "dependency",
                    visibility: dependency.visibility,
                    platform: None,
                },
                inputs,
                &admitted,
                &mut seen,
            ) {
                descriptors.push(descriptor);
            }
        }
    }

    for companion in &inputs.admitted.companions {
        if let Some(descriptor) = project(
            &companion.pinned,
            Placement {
                role: "companion",
                visibility: companion.surface,
                platform: None,
            },
            inputs,
            &admitted,
            &mut seen,
        ) {
            descriptors.push(descriptor);
        }
    }

    for companion in &inputs.admitted.companions {
        for dependency in &companion.dependencies {
            if let Some(descriptor) = project(
                &dependency.identifier,
                Placement {
                    role: "dependency",
                    visibility: dependency.visibility,
                    platform: None,
                },
                inputs,
                &admitted,
                &mut seen,
            ) {
                descriptors.push(descriptor);
            }
        }
    }

    descriptors
}

/// The facts that vary by the [`descriptors`] pass a package was found in.
struct Placement<'a> {
    /// `sh.ocx.role`: `root`, `dependency` or `companion`.
    role: &'a str,
    /// Visibility from the composition's perspective.
    visibility: Visibility,
    /// The platform resolution selected, where an install is in hand to say.
    platform: Option<&'a Platform>,
}

/// Build one descriptor, or `None` when this package was already emitted.
fn project(
    identifier: &PinnedPackageRef,
    placement: Placement<'_>,
    inputs: &RecordInputs<'_>,
    admitted: &AdmittedIndex,
    seen: &mut HashSet<PinnedPackageRef>,
) -> Option<ResourceDescriptor> {
    if !seen.insert(identifier.strip_advisory()) {
        return None;
    }

    let mut annotations = BTreeMap::from([("sh.ocx.role".to_string(), Value::from(placement.role))]);
    let digest = digest_map(&identifier.digest());

    let recorded = recorded_identity(identifier, &inputs.scope);
    let Some(uri) = package_url(&recorded, placement.platform) else {
        // A placeholder's name, registry and repository are local artefacts: stay digest-only.
        annotations.insert("sh.ocx.identity".to_string(), Value::from("synthetic"));
        return Some(ResourceDescriptor {
            name: identifier.repository().to_string(),
            uri: None,
            digest,
            annotations,
        });
    };

    if let Some(binding) = binding_for(identifier, &inputs.scope) {
        annotations.insert("sh.ocx.binding".to_string(), Value::from(binding.binding.clone()));
        annotations.insert("sh.ocx.group".to_string(), Value::from(binding.group.clone()));
    }
    if let Some(platform) = placement.platform {
        annotations.insert("sh.ocx.platform".to_string(), Value::from(platform.to_string()));
    }
    annotations.insert(
        "sh.ocx.visibility".to_string(),
        Value::from(placement.visibility.to_string()),
    );
    if let Some(names) = admitted.binaries.get(&identifier.strip_advisory()) {
        annotations.insert("sh.ocx.binaries".to_string(), Value::from(names.clone()));
    }
    if let Some(names) = admitted.entrypoints.get(&identifier.strip_advisory()) {
        annotations.insert("sh.ocx.entrypoints".to_string(), Value::from(names.clone()));
    }
    if recorded.tag().is_some() {
        // A surviving tag means a floating tag was resolved; with `resolution.autoInstalled`, the drift signal.
        annotations.insert("sh.ocx.resolved-from".to_string(), Value::from("tag"));
    }

    Some(ResourceDescriptor {
        name: identifier.name().to_string(),
        uri: Some(uri),
        digest,
        annotations,
    })
}

/// `identifier` as the record reports it. The tag a lock-bound root or a baked shim
/// carries is advisory, for patch matching: the digest was not reached through it.
fn recorded_identity(identifier: &PinnedPackageRef, scope: &Scope) -> PinnedPackageRef {
    if matches!(scope, Scope::LauncherShim { .. }) || binding_for(identifier, scope).is_some() {
        identifier.strip_advisory()
    } else {
        identifier.clone()
    }
}

/// The binding a project-tier package was selected under, if the frame has any.
fn binding_for<'a>(identifier: &PinnedPackageRef, scope: &'a Scope) -> Option<&'a PackageBinding> {
    match scope {
        Scope::Project { bindings, .. } => bindings.iter().find(|binding| binding.package.eq_content(identifier)),
        Scope::Package { .. } | Scope::Launcher | Scope::LauncherShim { .. } => None,
    }
}

/// The executable names each package contributed to `PATH`, in composition
/// order, keyed by the owning package's content identity.
struct AdmittedIndex {
    binaries: HashMap<PinnedPackageRef, Vec<Value>>,
    entrypoints: HashMap<PinnedPackageRef, Vec<Value>>,
}

impl AdmittedIndex {
    fn build(admitted: &AdmittedClaims) -> Self {
        Self {
            binaries: claims_by_owner(&admitted.binaries),
            entrypoints: claims_by_owner(&admitted.entrypoints),
        }
    }
}

/// Group one claim list by owner, keyed on [`PinnedPackageRef::strip_advisory`] so
/// an advisory tag cannot split a package's claims from the package.
fn claims_by_owner<T: std::fmt::Display>(claims: &[(PinnedPackageRef, T)]) -> HashMap<PinnedPackageRef, Vec<Value>> {
    let mut index: HashMap<PinnedPackageRef, Vec<Value>> = HashMap::new();
    for (owner, name) in claims {
        index
            .entry(owner.strip_advisory())
            .or_default()
            .push(Value::from(name.to_string()));
    }
    index
}

/// Render a digest as the frozen `algorithm → bare lowercase hex` map.
fn digest_map(digest: &Digest) -> BTreeMap<String, String> {
    let (algorithm, hex) = digest.parts();
    BTreeMap::from([(algorithm.to_string(), hex.to_ascii_lowercase())])
}

/// Serialize as the frozen fixed-width `recordedAt` form (`2026-07-26T14:03:11.482Z`);
/// chrono's default drops the subsecond on a whole-second value.
fn serialize_rfc3339_millis<S: Serializer>(value: &DateTime<Utc>, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::str::FromStr;

    use serde_json::Value;

    use super::*;
    use ocx_config::mirror::MirrorConfig;
    use ocx_oci::Digest;
    use ocx_package::metadata::{BinaryName, EntrypointName, Metadata};
    use ocx_package::resolved_package::{ResolvedDependency, ResolvedPackage};

    use crate::tasks::resolve::CompanionProjection;

    /// The platform *leaf* manifest digest — the bits that actually ran.
    const LEAF_HEX: &str = "3f7a2b9c5d1e8f04a6b3c7d2e9f1a5b8c4d6e0f2a3b7c9d1e5f8a0b2c4d6e8f0";
    /// The multi-arch image index that merely *pointed at* the leaf above.
    const INDEX_HEX: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const NINJA_HEX: &str = "8b1c4d7e0f3a6b9c2d5e8f1a4b7c0d3e6f9a2b5c8d1e4f7a0b3c6d9e2f5a8b1c";
    const DEPENDENCY_HEX: &str = "c0d3e6f9a2b5c8d1e4f7a0b3c6d9e2f5a8b1c4d7e0f3a6b9c2d5e8f1a4b7c0d3";
    const LOCK_HEX: &str = "9c1f0b3a77d2e4518ab6c0f92d3e7a41b8c5d6e0f1a2b3c4d5e6f708192a3b4c";
    const MANAGED_HEX: &str = "4d2c8e1f5a90b7c36e4d1928f0a5b3c7d9e2f4a6b8c0d1e3f5a7b9c1d3e5f709";
    const COMPANION_HEX: &str = "7e5a1c3f9b2d4e6a8c0f1b3d5e7a9c1f3b5d7e9a1c3f5b7d9e1a3c5f7b9d1e3a";
    const SNAPSHOT_HEX: &str = "2b4d6f8a0c2e4a6c8e0b2d4f6a8c0e2b4d6f8a0c2e4a6c8e0b2d4f6a8c0e2b4d";

    fn pinned(repository: &str, registry: &str, tag: Option<&str>, hex: &str) -> PinnedPackageRef {
        let mut identifier = PackageRef::new_registry(repository, registry);
        if let Some(tag) = tag {
            identifier = identifier.clone_with_tag(tag);
        }
        PinnedPackageRef::try_from(identifier.clone_with_digest(Digest::Sha256(hex.to_string())))
            .expect("digest present")
    }

    fn linux_amd64() -> Platform {
        Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: None,
            os_features: vec!["libc.glibc".to_string()],
        }
    }

    fn metadata() -> Metadata {
        serde_json::from_str(r#"{"type":"bundle","version":1,"env":[]}"#).expect("bundle metadata")
    }

    /// An install stamped the way the resolution paths stamp one: the platform
    /// it selected, and the registry its content came from. The default here is
    /// the registry-backed case, where that host and the identifier's own
    /// registry coincide — [`registries_name_the_content_host_not_the_logical_namespace`]
    /// is where they are made to diverge.
    fn install(identifier: PinnedPackageRef, dir: &str, dependencies: Vec<ResolvedDependency>) -> Arc<InstallInfo> {
        let registry = identifier.registry().to_string();
        Arc::new(
            InstallInfo::new(
                identifier,
                metadata(),
                ResolvedPackage { dependencies },
                ocx_store::file_structure::PackageDir::with_root(PathBuf::from(dir)),
            )
            .with_platform(linux_amd64())
            .with_transport_registry(registry),
        )
    }

    /// Owned fixture data, so a [`RecordInputs`] can borrow from one place.
    struct Frame {
        packages: Vec<Arc<InstallInfo>>,
        admitted: AdmittedClaims,
        executable: PathBuf,
        store_root: PathBuf,
        shim_root: PathBuf,
        argv: Vec<String>,
        config: OcxConfigView,
        insecure_registries: Vec<String>,
        platform: Option<Platform>,
        auto_installed: Vec<PackageRef>,
        managed_config_digest: Option<Digest>,
        patch_snapshot_digest: Option<Digest>,
        clean_env: bool,
    }

    impl Frame {
        /// The `ocx exec` frame of the design record's first exemplary record:
        /// two roots, one shared dependency, an entrypoint launcher resolved.
        fn project() -> Self {
            let cmake = pinned("ocx/cmake", "index.ocx.sh", None, LEAF_HEX);
            let ninja = pinned("ocx/ninja", "index.ocx.sh", None, NINJA_HEX);
            let runtime = pinned("ocx/libstdcxx-runtime", "index.ocx.sh", None, DEPENDENCY_HEX);

            let mut config = OcxConfigView::new("/home/ci/.ocx/bin/ocx");
            config.frozen = true;
            config.mirrors = vec![(
                "ghcr.io".to_string(),
                MirrorConfig {
                    registry: Some("https://artifactory.corp.example/ghcr-remote".to_string()),
                    index: None,
                    registry_system_locked: false,
                    index_system_locked: false,
                },
            )];
            config.managed_config_source = Some("internal.corp.example/ocx-config:user".to_string());

            Self {
                packages: vec![
                    install(
                        cmake.clone(),
                        "/home/ci/.ocx/packages/cmake",
                        vec![ResolvedDependency {
                            identifier: runtime,
                            visibility: Visibility::INTERFACE,
                        }],
                    ),
                    install(ninja.clone(), "/home/ci/.ocx/packages/ninja", Vec::new()),
                ],
                admitted: AdmittedClaims {
                    binaries: vec![(ninja, BinaryName::try_from("ninja").expect("binary name"))],
                    entrypoints: ["cmake", "ctest", "cpack"]
                        .into_iter()
                        .map(|name| (cmake.clone(), EntrypointName::try_from(name).expect("entrypoint name")))
                        .collect(),
                    ..AdmittedClaims::default()
                },
                executable: PathBuf::from("/home/ci/.ocx/packages/cmake/entrypoints/cmake"),
                store_root: PathBuf::from("/home/ci/.ocx/packages"),
                shim_root: PathBuf::from("/home/ci/.ocx/shims"),
                argv: ["cmake", "--build", "build"].map(str::to_string).to_vec(),
                config,
                // The frame's own content registry is declared plain-HTTP, so
                // the intersection is non-empty and the key is present — the
                // exemplary record populates every field it can.
                insecure_registries: vec!["index.ocx.sh".to_string()],
                platform: Some(linux_amd64()),
                auto_installed: Vec::new(),
                managed_config_digest: Some(Digest::Sha256(MANAGED_HEX.to_string())),
                patch_snapshot_digest: Some(Digest::Sha256(SNAPSHOT_HEX.to_string())),
                clean_env: false,
            }
        }

        /// The `ocx launcher exec` frame: a content-addressed placeholder
        /// identity, no platform context, the leaf binary resolved.
        fn launcher() -> Self {
            let placeholder = pinned(&format!("file-url-mode/{LEAF_HEX}"), "ocx.sh", None, LEAF_HEX);
            Self {
                packages: vec![install(placeholder, "/home/ci/.ocx/packages/cmake", Vec::new())],
                admitted: AdmittedClaims::default(),
                executable: PathBuf::from("/home/ci/.ocx/packages/cmake/content/bin/cmake"),
                store_root: PathBuf::from("/home/ci/.ocx/packages"),
                shim_root: PathBuf::from("/home/ci/.ocx/shims"),
                argv: vec!["cmake".to_string()],
                config: OcxConfigView::new("/opt/ocx/bin/ocx"),
                insecure_registries: Vec::new(),
                platform: None,
                auto_installed: Vec::new(),
                managed_config_digest: None,
                patch_snapshot_digest: None,
                clean_env: false,
            }
        }

        fn inputs(&self, scope: Scope) -> RecordInputs<'_> {
            RecordInputs {
                packages: &self.packages,
                admitted: &self.admitted,
                executable: &self.executable,
                store_root: &self.store_root,
                shim_root: &self.shim_root,
                argv: &self.argv,
                config: &self.config,
                insecure_registries: &self.insecure_registries,
                managed_config_digest: self.managed_config_digest.as_ref(),
                patch_snapshot_digest: self.patch_snapshot_digest.as_ref(),
                platform: self.platform.as_ref(),
                clean_env: self.clean_env,
                auto_installed: &self.auto_installed,
                scope,
            }
        }
    }

    fn project_scope() -> Scope {
        Scope::Project {
            root: PathBuf::from("/scratch/job-88213"),
            lock: PathBuf::from("/scratch/job-88213/ocx.lock"),
            declaration_digest: Digest::Sha256(LOCK_HEX.to_string()),
            groups: vec!["default".to_string()],
            bindings: vec![PackageBinding {
                binding: "cmake".to_string(),
                group: "default".to_string(),
                package: pinned("ocx/cmake", "index.ocx.sh", None, LEAF_HEX),
            }],
        }
    }

    /// One companion projection, as `resolve_env_with_attribution` reports it: a
    /// descriptor rule that named a tag, and the digest that tag resolved to.
    fn companion_projection(surface: Visibility, dependencies: Vec<ResolvedDependency>) -> CompanionProjection {
        CompanionProjection {
            pinned: pinned("corp-ca", "internal.corp.example", Some("2024"), COMPANION_HEX),
            surface,
            dependencies,
        }
    }

    fn annotations_of<'a>(descriptors: &'a [ResourceDescriptor], name: &str) -> &'a BTreeMap<String, Value> {
        &descriptors
            .iter()
            .find(|descriptor| descriptor.name == name)
            .unwrap_or_else(|| panic!("no descriptor named {name}"))
            .annotations
    }

    // ── Format rule 5 — the platform leaf, never the index ──────────────

    #[test]
    fn recorded_digest_is_the_platform_leaf_not_the_image_index() {
        let frame = Frame::project();
        let descriptors = descriptors(&frame.inputs(project_scope()));

        let cmake = descriptors.iter().find(|d| d.name == "cmake").expect("cmake");
        assert_eq!(
            cmake.digest.get("sha256").map(String::as_str),
            Some(LEAF_HEX),
            "the recorded digest must name the exact bits that ran"
        );
        assert_ne!(
            cmake.digest.get("sha256").map(String::as_str),
            Some(INDEX_HEX),
            "the multi-arch index digest must never be substituted for the leaf"
        );
    }

    #[test]
    fn purl_arch_agrees_with_the_recorded_leaf() {
        let frame = Frame::project();
        let descriptors = descriptors(&frame.inputs(project_scope()));
        let cmake = descriptors.iter().find(|d| d.name == "cmake").expect("cmake");
        let uri = cmake.uri.as_deref().expect("uri");

        assert!(uri.contains(&format!("@sha256:{LEAF_HEX}")), "{uri}");
        assert!(
            uri.contains("arch=amd64"),
            "arch must describe the recorded leaf: {uri}"
        );
        assert_eq!(
            annotations_of(&descriptors, "cmake")
                .get("sh.ocx.platform")
                .and_then(Value::as_str),
            Some("linux/amd64+libc.glibc"),
        );
    }

    /// The requested platform and the selected one are different facts, and the
    /// frame here disagrees with the install on purpose: a record built from the
    /// request would report `arm64` for an artefact whose manifest leaf is
    /// `amd64`. Only reading the install's own platform gets this right.
    #[test]
    fn a_package_reports_the_platform_it_resolved_to_not_the_one_requested() {
        let mut frame = Frame::project();
        frame.platform = Some(Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Arm64,
            variant: None,
            os_features: Vec::new(),
        });

        let inputs = frame.inputs(project_scope());
        let descriptors = descriptors(&inputs);

        assert_eq!(
            annotations_of(&descriptors, "cmake")
                .get("sh.ocx.platform")
                .and_then(Value::as_str),
            Some("linux/amd64+libc.glibc"),
            "the selected platform comes from the install, never from the request",
        );
        assert!(
            descriptors
                .iter()
                .find(|descriptor| descriptor.name == "cmake")
                .and_then(|descriptor| descriptor.uri.as_deref())
                .is_some_and(|uri| uri.contains("arch=amd64")),
            "the purl's arch describes the recorded leaf too",
        );
        assert_eq!(
            resolution_block(&inputs).requested_platform.as_deref(),
            Some("linux/arm64"),
            "the request is reported separately, under its own name",
        );
        assert!(
            !annotations_of(&descriptors, "libstdcxx-runtime").contains_key("sh.ocx.platform"),
            "a dependency's selected platform is not in hand, so it is omitted rather than guessed",
        );
    }

    /// An install that never learned its platform — a path that builds an
    /// `InstallInfo` without resolution context — omits the key rather than
    /// borrowing the frame's request.
    #[test]
    fn a_package_with_no_recorded_platform_omits_the_annotation() {
        let mut frame = Frame::project();
        frame.packages[0] = Arc::new(InstallInfo::new(
            pinned("ocx/cmake", "index.ocx.sh", None, LEAF_HEX),
            metadata(),
            ResolvedPackage { dependencies: vec![] },
            ocx_store::file_structure::PackageDir::with_root(PathBuf::from("/home/ci/.ocx/packages/cmake")),
        ));

        let descriptors = descriptors(&frame.inputs(project_scope()));
        assert!(
            !annotations_of(&descriptors, "cmake").contains_key("sh.ocx.platform"),
            "the frame's requested platform must not stand in for an unknown one",
        );
    }

    // ── The full closure, roles and order ──────────────────────────

    #[test]
    fn closure_records_roots_then_dependencies_in_topological_order() {
        let frame = Frame::project();
        let descriptors = descriptors(&frame.inputs(project_scope()));

        let names: Vec<&str> = descriptors.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["cmake", "ninja", "libstdcxx-runtime"]);

        let roles: Vec<&str> = descriptors
            .iter()
            .map(|d| d.annotations["sh.ocx.role"].as_str().expect("role is a string"))
            .collect();
        assert_eq!(roles, vec!["root", "root", "dependency"]);
    }

    #[test]
    fn a_package_reachable_twice_is_recorded_once() {
        let runtime = pinned("ocx/libstdcxx-runtime", "index.ocx.sh", None, DEPENDENCY_HEX);
        let dependency = ResolvedDependency {
            identifier: runtime,
            visibility: Visibility::INTERFACE,
        };
        let mut frame = Frame::project();
        // Both roots now depend on the same package — the diamond case.
        frame.packages[1] = install(
            pinned("ocx/ninja", "index.ocx.sh", None, NINJA_HEX),
            "/home/ci/.ocx/packages/ninja",
            vec![dependency],
        );

        let descriptors = descriptors(&frame.inputs(project_scope()));
        let occurrences = descriptors.iter().filter(|d| d.name == "libstdcxx-runtime").count();
        assert_eq!(occurrences, 1);
    }

    #[test]
    fn dependency_visibility_is_recorded_from_its_resolved_edge() {
        let frame = Frame::project();
        let descriptors = descriptors(&frame.inputs(project_scope()));
        assert_eq!(
            annotations_of(&descriptors, "libstdcxx-runtime")
                .get("sh.ocx.visibility")
                .and_then(Value::as_str),
            Some("interface"),
        );
        assert_eq!(
            annotations_of(&descriptors, "cmake")
                .get("sh.ocx.visibility")
                .and_then(Value::as_str),
            Some("public"),
            "a root carries no incoming edge, so it is fully visible",
        );
    }

    #[test]
    fn every_visibility_maps_to_its_named_wire_value() {
        for (visibility, expected) in [
            (Visibility::SEALED, "sealed"),
            (Visibility::PRIVATE, "private"),
            (Visibility::INTERFACE, "interface"),
            (Visibility::PUBLIC, "public"),
        ] {
            let mut frame = Frame::project();
            frame.packages[0] = install(
                pinned("ocx/cmake", "index.ocx.sh", None, LEAF_HEX),
                "/home/ci/.ocx/packages/cmake",
                vec![ResolvedDependency {
                    identifier: pinned("ocx/libstdcxx-runtime", "index.ocx.sh", None, DEPENDENCY_HEX),
                    visibility,
                }],
            );
            let descriptors = descriptors(&frame.inputs(project_scope()));
            assert_eq!(
                annotations_of(&descriptors, "libstdcxx-runtime")
                    .get("sh.ocx.visibility")
                    .and_then(Value::as_str),
                Some(expected),
            );
        }
    }

    #[test]
    fn admitted_names_and_bindings_are_attributed_to_their_package() {
        let frame = Frame::project();
        let descriptors = descriptors(&frame.inputs(project_scope()));

        let cmake = annotations_of(&descriptors, "cmake");
        assert_eq!(
            cmake.get("sh.ocx.entrypoints"),
            Some(&serde_json::json!(["cmake", "ctest", "cpack"])),
            "the claim is a list, in composition order",
        );
        assert_eq!(cmake.get("sh.ocx.binding").and_then(Value::as_str), Some("cmake"));
        assert_eq!(cmake.get("sh.ocx.group").and_then(Value::as_str), Some("default"));
        assert!(!cmake.contains_key("sh.ocx.binaries"));

        let ninja = annotations_of(&descriptors, "ninja");
        assert_eq!(ninja.get("sh.ocx.binaries"), Some(&serde_json::json!(["ninja"])));
        assert!(!ninja.contains_key("sh.ocx.entrypoints"));
    }

    /// A comma is legal inside a binary name (`binary.rs` forbids it nowhere),
    /// so a joined string cannot express the claim: `["a,b"]` and `["a","b"]`
    /// would arrive identical. This is the discriminator for the array shape —
    /// it fails against any separator-joined encoding, whatever the separator.
    #[test]
    fn a_name_containing_the_join_separator_stays_its_own_element() {
        let cmake = pinned("ocx/cmake", "index.ocx.sh", None, LEAF_HEX);
        let mut frame = Frame::project();
        frame.admitted = AdmittedClaims {
            binaries: ["a,b", "c"]
                .into_iter()
                .map(|name| (cmake.clone(), BinaryName::try_from(name).expect("binary name")))
                .collect(),
            entrypoints: Vec::new(),
            ..AdmittedClaims::default()
        };

        let descriptors = descriptors(&frame.inputs(project_scope()));
        assert_eq!(
            annotations_of(&descriptors, "cmake").get("sh.ocx.binaries"),
            Some(&serde_json::json!(["a,b", "c"])),
            "two names, one of which contains a comma — never three",
        );
    }

    /// Attribution is content identity: the advisory tag must not split a
    /// package's claims from the package. The index keys on the stripped form,
    /// which is the hash-lookup twin of the `eq_content` scan it replaced.
    #[test]
    fn claims_reach_their_package_across_an_advisory_tag() {
        let tagged = pinned("ocx/cmake", "index.ocx.sh", Some("3.28"), LEAF_HEX);
        let untagged = pinned("ocx/cmake", "index.ocx.sh", None, LEAF_HEX);
        let mut frame = Frame::project();
        frame.packages[0] = install(tagged, "/home/ci/.ocx/packages/cmake", Vec::new());
        frame.admitted = AdmittedClaims {
            // The claim was attributed to the untagged form; the descriptor is
            // built from the tagged one.
            binaries: vec![(untagged, BinaryName::try_from("cmake").expect("binary name"))],
            entrypoints: Vec::new(),
            ..AdmittedClaims::default()
        };

        let descriptors = descriptors(&frame.inputs(project_scope()));
        assert_eq!(
            annotations_of(&descriptors, "cmake").get("sh.ocx.binaries"),
            Some(&serde_json::json!(["cmake"])),
        );
    }

    #[test]
    fn a_resolved_tag_is_marked_and_an_absent_one_is_not() {
        let frame = Frame::project();
        let untagged = descriptors(&frame.inputs(project_scope()));
        assert!(
            !annotations_of(&untagged, "cmake").contains_key("sh.ocx.resolved-from"),
            "the lock stores no tag, so none may be synthesised",
        );

        let mut tagged = Frame::project();
        tagged.packages[0] = install(
            pinned("ocx/cmake", "index.ocx.sh", Some("3.28"), LEAF_HEX),
            "/home/ci/.ocx/packages/cmake",
            Vec::new(),
        );
        let descriptors = descriptors(&tagged.inputs(Scope::Package {
            requested: vec![PackageRef::parse("index.ocx.sh/ocx/cmake:3.28").expect("identifier")],
        }));
        assert_eq!(
            annotations_of(&descriptors, "cmake")
                .get("sh.ocx.resolved-from")
                .and_then(Value::as_str),
            Some("tag"),
        );
    }

    /// The `tag` qualifier of a rendered purl, parsed rather than substring-matched.
    fn purl_tag(purl: &str) -> Option<String> {
        packageurl::PackageUrl::from_str(purl)
            .expect("rendered purl must parse")
            .qualifiers()
            .iter()
            .find(|(key, _)| *key == "tag")
            .map(|(_, value)| value.to_string())
    }

    /// A project frame whose `cmake` root came from the lock bound to its
    /// `ocx.toml` tag, and whose `ninja` root is a positional `ninja:1.12`,
    /// outside `Scope::Project.bindings`. Both were materialised this run.
    fn lock_bound_and_positional_frame() -> Frame {
        let mut frame = Frame::project();
        let cmake = pinned("ocx/cmake", "index.ocx.sh", Some("3.28"), LEAF_HEX);
        let ninja = pinned("ocx/ninja", "index.ocx.sh", Some("1.12"), NINJA_HEX);
        frame.packages = vec![
            install(cmake.clone(), "/home/ci/.ocx/packages/cmake", Vec::new()),
            install(ninja.clone(), "/home/ci/.ocx/packages/ninja", Vec::new()),
        ];
        frame.auto_installed = vec![PackageRef::from(cmake), PackageRef::from(ninja)];
        frame
    }

    /// A lock-resolved digest was not reached through a tag, so the advisory tag
    /// a binding carries for patch matching must not surface as provenance: the
    /// record stays byte-identical to a tagless lock pin.
    #[test]
    fn c008_a_lock_bound_tag_is_not_recorded_as_resolved_from_a_tag() {
        let frame = lock_bound_and_positional_frame();
        let inputs = frame.inputs(project_scope());
        let descriptors = descriptors(&inputs);
        let cmake = descriptors
            .iter()
            .find(|descriptor| descriptor.name == "cmake")
            .expect("cmake descriptor");

        assert!(
            !cmake.annotations.contains_key("sh.ocx.resolved-from"),
            "a lock digest is not resolved from a tag: {:?}",
            cmake.annotations
        );
        assert_eq!(
            purl_tag(cmake.uri.as_deref().expect("purl")),
            None,
            "no purl tag for a lock-bound root"
        );
        assert_eq!(
            resolution_block(&inputs)
                .auto_installed
                .expect("materialised")
                .first()
                .map(String::as_str),
            Some(format!("index.ocx.sh/ocx/cmake@sha256:{LEAF_HEX}").as_str()),
            "autoInstalled names the lock-bound root without its advisory tag"
        );
    }

    /// The executable block's package purl follows the descriptor: the lock-bound
    /// root that owns the resolved launcher carries no `tag` qualifier either.
    #[test]
    fn c008_the_executable_purl_of_a_lock_bound_root_carries_no_tag() {
        let frame = lock_bound_and_positional_frame();
        let block = executable_block(&frame.inputs(project_scope()));
        let purl = block.get("sh.ocx.package").expect("cmake owns the executable");
        assert_eq!(purl_tag(purl), None, "{purl}");
    }

    /// A positional `name=repo:tag` was resolved through its tag, so it keeps
    /// the marker, the purl qualifier and its tagged `autoInstalled` entry.
    #[test]
    fn c008_a_positional_tag_keeps_its_resolved_from_provenance() {
        let frame = lock_bound_and_positional_frame();
        let inputs = frame.inputs(project_scope());
        let descriptors = descriptors(&inputs);
        let ninja = descriptors
            .iter()
            .find(|descriptor| descriptor.name == "ninja")
            .expect("ninja descriptor");

        assert_eq!(
            ninja.annotations.get("sh.ocx.resolved-from").and_then(Value::as_str),
            Some("tag")
        );
        assert_eq!(purl_tag(ninja.uri.as_deref().expect("purl")).as_deref(), Some("1.12"));
        assert_eq!(
            resolution_block(&inputs)
                .auto_installed
                .expect("materialised")
                .get(1)
                .map(String::as_str),
            Some(format!("index.ocx.sh/ocx/ninja:1.12@sha256:{NINJA_HEX}").as_str()),
        );
    }

    /// A shim pulls by its baked digest, never by the tag baked beside it, so the
    /// shim frame records no tag provenance, whether the shim was baked from a
    /// lock-bound tool or from a package-tier tagged positional.
    #[test]
    fn c008_a_baked_shim_tag_is_not_recorded_as_resolved_from_a_tag() {
        let frame = lock_bound_and_positional_frame();
        for (index, name, tag, hex) in [(0, "cmake", "3.28", LEAF_HEX), (1, "ninja", "1.12", NINJA_HEX)] {
            let inputs = frame.inputs(Scope::LauncherShim {
                requested: pinned(&format!("ocx/{name}"), "index.ocx.sh", Some(tag), hex),
            });
            let descriptors = descriptors(&inputs);
            let descriptor = descriptors
                .iter()
                .find(|descriptor| descriptor.name == name)
                .expect("descriptor");
            let untagged = format!("index.ocx.sh/ocx/{name}@sha256:{hex}");

            assert!(
                !descriptor.annotations.contains_key("sh.ocx.resolved-from"),
                "{name}: {:?}",
                descriptor.annotations
            );
            assert_eq!(purl_tag(descriptor.uri.as_deref().expect("purl")), None, "{name}");
            assert_eq!(
                resolution_block(&inputs)
                    .auto_installed
                    .expect("materialised")
                    .get(index)
                    .map(String::as_str),
                Some(untagged.as_str()),
                "{name}"
            );
            match scope_block(&inputs) {
                ScopeBlock::Package { requested, .. } => assert_eq!(requested, vec![untagged], "{name}"),
                other => panic!("a shim frame records the package scope, got {other:?}"),
            }
        }
    }

    // ── sh.ocx.kind — launcher versus leaf binary ───────────────────────

    #[test]
    fn kind_is_launcher_when_the_resolved_path_is_an_entrypoint_shim() {
        let frame = Frame::project();
        let block = executable_block(&frame.inputs(project_scope()));
        assert_eq!(block.get("sh.ocx.kind").map(String::as_str), Some("launcher"));
        assert_eq!(block.get("sh.ocx.provenance").map(String::as_str), Some("ocx-package"));
        assert!(
            block
                .get("sh.ocx.package")
                .is_some_and(|purl| purl.starts_with("pkg:oci/cmake@")),
            "{block:?}",
        );
    }

    #[test]
    fn kind_is_binary_when_the_resolved_path_is_package_content() {
        let mut frame = Frame::project();
        frame.executable = PathBuf::from("/home/ci/.ocx/packages/cmake/content/bin/cmake");
        let block = executable_block(&frame.inputs(project_scope()));
        assert_eq!(block.get("sh.ocx.kind").map(String::as_str), Some("binary"));
    }

    #[test]
    fn an_executable_owned_by_a_dependency_is_still_a_package_executable() {
        let mut frame = Frame::project();
        // A package the frame resolved but never enumerated as a root: a
        // dependency's binary is on `PATH` too, and it lives in the store.
        frame.executable = PathBuf::from("/home/ci/.ocx/packages/libstdcxx-runtime/content/bin/gcov");

        let block = executable_block(&frame.inputs(project_scope()));
        assert_eq!(
            block.get("sh.ocx.provenance").map(String::as_str),
            Some("ocx-package"),
            "store containment, not root enumeration, decides provenance",
        );
        assert_eq!(block.get("sh.ocx.kind").map(String::as_str), Some("binary"));
        assert!(
            !block.contains_key("sh.ocx.package"),
            "only a root carries a directory to match an identity against: {block:?}",
        );
    }

    /// A following-lane executable, spelled as the
    /// `<home>/toolchain/links/<group>/<entry>` link the following lane puts on `PATH`, still
    /// records as an ocx package.
    ///
    /// `which::which_in` keeps whichever spelling it resolved through, so from
    /// `executable_block`'s side every project-tier `ocx exec` now arrives
    /// holding a link path. A raw `starts_with` against the store root answers
    /// "not contained" for all of them — `sh.ocx.provenance = "external"`, no
    /// `sh.ocx.kind`, and no `sh.ocx.package` purl, the field the record's own
    /// doc calls the one an auditor reads first.
    ///
    /// Real directories and a real symlink, because the assertion is about what
    /// `dunce::canonicalize` resolves; the sibling fixtures use synthetic
    /// absolute paths and cannot reach the retry at all.
    ///
    /// RED: reverting either containment test in `executable_block` or
    /// `owning_root` to a raw `starts_with`.
    #[test]
    #[cfg(unix)]
    fn a_following_lane_link_spelling_still_records_as_an_ocx_package() {
        let tmp = tempfile::tempdir().expect("a tempdir is creatable");
        let store_root = tmp.path().join("packages");
        let package_root = store_root.join("cmake");
        std::fs::create_dir_all(package_root.join("entrypoints")).expect("the package tree is creatable");
        std::fs::write(package_root.join("entrypoints").join("cmake"), b"#!/bin/sh\n")
            .expect("the launcher is writable");

        let group = tmp.path().join("toolchain").join("default");
        std::fs::create_dir_all(&group).expect("the group directory is creatable");
        ocx_util::fs::symlink::create(&package_root, group.join("cmake")).expect("the rendered link is creatable");

        let mut frame = Frame::project();
        frame.store_root = store_root;
        frame.shim_root = tmp.path().join("shims");
        // Exactly what a following-lane `PATH` resolves to.
        frame.executable = group.join("cmake").join("entrypoints").join("cmake");
        frame.packages[0] = install(
            pinned("ocx/cmake", "index.ocx.sh", None, LEAF_HEX),
            package_root.to_str().expect("a UTF-8 fixture path"),
            Vec::new(),
        );

        let block = executable_block(&frame.inputs(project_scope()));

        assert_eq!(
            block.get("sh.ocx.provenance").map(String::as_str),
            Some("ocx-package"),
            "RUL-82 — the record resolves the link even though the launch does not: {block:?}",
        );
        assert_eq!(
            block.get("sh.ocx.kind").map(String::as_str),
            Some("launcher"),
            "the kind scan runs on the same resolved spelling as the containment test: {block:?}",
        );
        assert!(
            block
                .get("sh.ocx.package")
                .is_some_and(|purl| purl.starts_with("pkg:oci/cmake@")),
            "`owning_root` must see through the link too, or the purl is dropped: {block:?}",
        );
    }

    #[test]
    fn an_executable_outside_the_store_is_recorded_as_external() {
        let mut frame = Frame::project();
        frame.executable = PathBuf::from("/usr/bin/bash");
        let block = executable_block(&frame.inputs(project_scope()));
        assert_eq!(block.get("sh.ocx.provenance").map(String::as_str), Some("external"));
        assert!(!block.contains_key("sh.ocx.kind"), "{block:?}");
        assert!(!block.contains_key("sh.ocx.package"), "{block:?}");
    }

    #[test]
    fn the_launcher_frames_leaf_binary_is_a_package_executable() {
        let frame = Frame::launcher();
        let block = executable_block(&frame.inputs(Scope::Launcher));
        assert_eq!(block.get("sh.ocx.provenance").map(String::as_str), Some("ocx-package"));
        assert_eq!(block.get("sh.ocx.kind").map(String::as_str), Some("binary"));
        assert!(
            !block.contains_key("sh.ocx.package"),
            "a placeholder identity emits no purl: {block:?}",
        );
    }

    // ── Frame, scope and resolution ─────────────────────────────────────

    #[test]
    fn frame_is_derived_from_the_scope() {
        assert_eq!(frame_for(&project_scope()).command, FrameCommand::Exec);
        assert_eq!(frame_for(&project_scope()).identity, FrameIdentity::Complete);
        assert!(frame_for(&project_scope()).identity_note.is_none());

        let package = Scope::Package { requested: Vec::new() };
        assert_eq!(frame_for(&package).command, FrameCommand::PackageExec);
        assert_eq!(frame_for(&package).identity, FrameIdentity::Complete);

        let launcher = frame_for(&Scope::Launcher);
        assert_eq!(launcher.command, FrameCommand::LauncherExec);
        assert_eq!(launcher.identity, FrameIdentity::Degraded);
        assert!(
            launcher
                .identity_note
                .is_some_and(|note| note.contains("content-shared")),
            "a degraded frame states its limitation in-band",
        );
    }

    #[test]
    fn resolution_reports_the_frames_registries_mirrors_and_managed_tier() {
        let frame = Frame::project();
        let resolution = resolution_block(&frame.inputs(project_scope()));

        assert!(resolution.frozen);
        assert_eq!(resolution.requested_platform.as_deref(), Some("linux/amd64+libc.glibc"));
        assert_eq!(
            resolution.registries.as_deref(),
            Some(["index.ocx.sh".to_string()].as_slice())
        );
        let mirrors = resolution.mirrors.expect("mirrors");
        let ghcr = mirrors.get("ghcr.io").expect("the configured mirror host");
        assert_eq!(
            ghcr.registry.as_deref(),
            Some("https://artifactory.corp.example/ghcr-remote")
        );
        assert_eq!(ghcr.index, None, "this entry declares no index role");
        let managed = resolution.managed_config.expect("managed config");
        assert_eq!(managed.source, "internal.corp.example/ocx-config:user");
        assert_eq!(managed.digest.get("sha256").map(String::as_str), Some(MANAGED_HEX));
        assert!(resolution.auto_installed.is_none(), "nothing was materialised here");
    }

    /// `resolution.registries` names where the bytes came from, and under index
    /// indirection that is not the registry the identifier names: an `ocx.sh`
    /// index root can point at `ghcr.io/acme/tool`. A record built from the
    /// identifier would report `index.ocx.sh` for a package fetched from
    /// `ghcr.io` — the field's name would be true of nothing. The two facts are
    /// recorded side by side, so the assertion is both halves: the content host
    /// under `registries`, the logical namespace still under the purl.
    #[test]
    fn registries_name_the_content_host_not_the_logical_namespace() {
        let mut frame = Frame::project();
        frame.packages[0] = Arc::new(
            InstallInfo::new(
                pinned("ocx/cmake", "index.ocx.sh", None, LEAF_HEX),
                metadata(),
                ResolvedPackage { dependencies: vec![] },
                ocx_store::file_structure::PackageDir::with_root(PathBuf::from("/home/ci/.ocx/packages/cmake")),
            )
            .with_platform(linux_amd64())
            .with_transport_registry("ghcr.io"),
        );

        let inputs = frame.inputs(project_scope());
        assert_eq!(
            resolution_block(&inputs).registries.as_deref(),
            Some(["ghcr.io".to_string(), "index.ocx.sh".to_string()].as_slice()),
            "the indirected root reports the host it was fetched from, the plain one its own",
        );
        let uri = descriptors(&inputs)
            .iter()
            .find(|descriptor| descriptor.name == "cmake")
            .and_then(|descriptor| descriptor.uri.clone())
            .expect("uri");
        // Parsed, not substring-matched: the crate percent-encodes qualifier
        // values and emits them alphabetically (see the `purl` module docs).
        let repository_url = packageurl::PackageUrl::from_str(&uri)
            .expect("rendered purl must parse")
            .qualifiers()
            .get("repository_url")
            .map(ToString::to_string);
        assert_eq!(
            repository_url.as_deref(),
            Some("index.ocx.sh/ocx/cmake"),
            "the logical namespace is not overwritten by the content host — it stays in the purl",
        );
    }

    /// Both mirror roles reach the record. The discriminator is an entry
    /// declaring *both*: a single-endpoint shape can only report one of them,
    /// and would silently drop whichever it did not pick.
    #[test]
    fn a_mirror_entry_reports_every_role_it_declares() {
        let mut frame = Frame::project();
        frame.config.mirrors = vec![
            (
                "ghcr.io".to_string(),
                MirrorConfig {
                    registry: Some("https://artifactory.corp.example/ghcr-remote".to_string()),
                    index: Some("https://artifactory.corp.example/ghcr-index".to_string()),
                    registry_system_locked: false,
                    index_system_locked: false,
                },
            ),
            (
                "index.ocx.sh".to_string(),
                MirrorConfig {
                    registry: None,
                    index: Some("https://artifactory.corp.example/ocx-index".to_string()),
                    registry_system_locked: false,
                    index_system_locked: false,
                },
            ),
            // Declares neither role: not a rewrite, so not a record entry.
            ("docker.io".to_string(), MirrorConfig::default()),
        ];

        let mirrors = resolution_block(&frame.inputs(project_scope()))
            .mirrors
            .expect("mirrors");

        let both = mirrors.get("ghcr.io").expect("the dual-role host");
        assert_eq!(
            both.registry.as_deref(),
            Some("https://artifactory.corp.example/ghcr-remote")
        );
        assert_eq!(
            both.index.as_deref(),
            Some("https://artifactory.corp.example/ghcr-index"),
            "the index endpoint must survive alongside the registry one",
        );

        let index_only = mirrors.get("index.ocx.sh").expect("the index-only host");
        assert_eq!(index_only.registry, None);
        assert_eq!(
            index_only.index.as_deref(),
            Some("https://artifactory.corp.example/ocx-index"),
            "an index-only rewrite must not be reported as a registry rewrite",
        );

        assert!(!mirrors.contains_key("docker.io"), "{mirrors:?}");
    }

    /// A `[mirrors]` endpoint may carry a working credential — `parse_url` keeps
    /// `user:token@` inside the host and the index transport sends it — and this
    /// record's sink is fleet-aggregated, which is exactly why `process.args` is
    /// not carried. The token must not survive into the document at all.
    #[test]
    fn a_credential_in_a_mirror_endpoint_never_reaches_the_record() {
        let mut frame = Frame::project();
        frame.config.mirrors = vec![(
            "ghcr.io".to_string(),
            MirrorConfig {
                registry: Some("https://user:t0k3n@mirror.example/idx".to_string()),
                // Scheme-less is an accepted spelling too — `parse_url` defaults
                // it to https — so the redaction cannot rely on a `://`.
                index: Some("user:t0k3n@index-mirror.example/idx".to_string()),
                registry_system_locked: false,
                index_system_locked: false,
            },
        )];

        let inputs = frame.inputs(project_scope());
        let entry = resolution_block(&inputs)
            .mirrors
            .expect("mirrors")
            .remove("ghcr.io")
            .expect("the configured mirror host");

        assert_eq!(entry.registry.as_deref(), Some("https://mirror.example/idx"));
        assert_eq!(entry.index.as_deref(), Some("index-mirror.example/idx"));

        // The endpoint keeps its audit value — which host traffic was rewritten
        // to — and the authority the config layer would parse out of the redacted
        // form carries no userinfo left to send.
        for endpoint in [entry.registry.as_deref(), entry.index.as_deref()]
            .into_iter()
            .flatten()
        {
            let parsed = ocx_config::mirror::parse_url(endpoint).expect("a redacted endpoint still parses");
            assert!(
                !parsed.host.contains('@'),
                "the config layer must see no userinfo in {endpoint:?}, got host {:?}",
                parsed.host,
            );
        }

        let recorded_at = "2026-07-26T14:03:11.482Z".parse().expect("timestamp");
        let json = ExecutionRecord::build(&inputs, recorded_at, 48123)
            .to_json()
            .expect("serializes");
        assert!(!json.contains("t0k3n"), "the credential reached the record: {json}");
        assert!(
            json.contains("mirror.example/idx"),
            "the rewritten host must still be recorded: {json}",
        );
    }

    /// The discriminator for the redaction above: an ordinary endpoint — and one
    /// whose *path* contains an `@` after the authority — survives byte-for-byte.
    /// A rule that normalized or over-trimmed would pass the credential test and
    /// still destroy the field.
    #[test]
    fn an_ordinary_mirror_endpoint_survives_byte_for_byte() {
        let mut frame = Frame::project();
        frame.config.mirrors = vec![(
            "ghcr.io".to_string(),
            MirrorConfig {
                registry: Some("https://artifactory.corp.example/ghcr-remote".to_string()),
                index: Some("https://artifactory.corp.example/idx/team@corp/index".to_string()),
                registry_system_locked: false,
                index_system_locked: false,
            },
        )];

        let entry = resolution_block(&frame.inputs(project_scope()))
            .mirrors
            .expect("mirrors")
            .remove("ghcr.io")
            .expect("the configured mirror host");

        assert_eq!(
            entry.registry.as_deref(),
            Some("https://artifactory.corp.example/ghcr-remote")
        );
        assert_eq!(
            entry.index.as_deref(),
            Some("https://artifactory.corp.example/idx/team@corp/index"),
            "an `@` past the authority is path, not userinfo",
        );
    }

    #[test]
    fn auto_installed_packages_are_named_when_any_were_materialised() {
        let mut frame = Frame::project();
        frame.auto_installed = vec![PackageRef::parse("internal.corp.example/solver").expect("identifier")];
        let resolution = resolution_block(&frame.inputs(project_scope()));
        assert_eq!(
            resolution.auto_installed.as_deref(),
            Some(["internal.corp.example/solver".to_string()].as_slice()),
        );
    }

    // ── insecure_registries — the plaintext intersection ────────────────

    /// The field is the INTERSECTION, not the configured allowance: a host
    /// nobody contacted says nothing about this invocation, and reporting the
    /// whole list would bury the one host that matters.
    #[test]
    fn insecure_registries_names_only_hosts_this_frame_actually_fetched_from() {
        let mut frame = Frame::project();
        frame.insecure_registries = vec![
            "index.ocx.sh".to_string(),
            // Configured, but nothing was fetched from it in this frame.
            "unused.corp.example:5000".to_string(),
        ];

        assert_eq!(
            resolution_block(&frame.inputs(project_scope())).insecure_registries,
            vec!["index.ocx.sh".to_string()],
            "only the host that both served content and was licensed plaintext is reported",
        );
    }

    /// The comparison is the transport's own: byte-exact on `host[:port]`, so a
    /// mis-cased or differently-ported allowance licenses nothing and the key
    /// disappears rather than reporting a host that was in fact reached over
    /// TLS.
    #[test]
    fn a_registry_with_no_plaintext_allowance_is_absent_not_empty() {
        let mut frame = Frame::project();
        frame.insecure_registries = vec!["Index.OCX.sh".to_string()];

        let resolution = resolution_block(&frame.inputs(project_scope()));
        assert!(
            resolution.insecure_registries.is_empty(),
            "a mis-cased allowance licenses nothing: {:?}",
            resolution.insecure_registries,
        );
        let value = serde_json::to_value(resolution).expect("serializes");
        assert!(
            !value.as_object().expect("object").contains_key("insecureRegistries"),
            "empty is absent on the wire, never `[]`: {value}",
        );
    }

    // ── patch_snapshot — the patch tier's own freeze ────────────────────

    #[test]
    fn patch_snapshot_records_the_digest_of_the_snapshot_in_force() {
        let frame = Frame::project();
        let resolution = resolution_block(&frame.inputs(project_scope()));

        assert_eq!(
            resolution.patch_snapshot.get("sha256").map(String::as_str),
            Some(SNAPSHOT_HEX),
            "the patch tier's freeze is recorded beside the package tier's `frozen`",
        );
        assert!(
            resolution.frozen,
            "the two are independent fields, and this frame sets both",
        );
    }

    #[test]
    fn no_patch_snapshot_omits_the_key_rather_than_emitting_an_empty_object() {
        let mut frame = Frame::project();
        frame.patch_snapshot_digest = None;

        let value = serde_json::to_value(resolution_block(&frame.inputs(project_scope()))).expect("serializes");
        assert!(
            !value.as_object().expect("object").contains_key("patchSnapshot"),
            "no snapshot in force is an absent key, not `{{}}`: {value}",
        );
        assert!(
            value.as_object().expect("object").contains_key("frozen"),
            "the package-tier flag is unconditional and must not vanish with it: {value}",
        );
    }

    // ── companions — the site tier's own packages ───────────────────────

    #[test]
    fn a_patch_companion_is_recorded_after_the_roots_and_dependencies() {
        let mut frame = Frame::project();
        frame.admitted.companions = vec![companion_projection(Visibility::INTERFACE, Vec::new())];

        let descriptors = descriptors(&frame.inputs(project_scope()));
        let names: Vec<&str> = descriptors.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["cmake", "ninja", "libstdcxx-runtime", "corp-ca"],
            "site policy lands after everything the caller asked for",
        );

        let companion = descriptors.last().expect("the companion descriptor");
        assert_eq!(
            companion.annotations.get("sh.ocx.role").and_then(Value::as_str),
            Some("companion"),
        );
        let uri = companion.uri.as_deref().expect("a companion has a logical identity");
        assert!(
            uri.starts_with("pkg:oci/corp-ca@") && uri.contains(&format!("@sha256:{COMPANION_HEX}")),
            "the companion's purl names the digest the descriptor's tag resolved to: {uri}",
        );
        assert_eq!(
            packageurl::PackageUrl::from_str(uri)
                .expect("rendered purl must parse")
                .qualifiers()
                .get("tag")
                .map(ToString::to_string)
                .as_deref(),
            Some("2024"),
            "and the tag it was named under survives as the purl's own qualifier",
        );
        assert_eq!(companion.digest.get("sha256").map(String::as_str), Some(COMPANION_HEX),);
    }

    /// A companion's visibility is the surface it composed on, as a root's is its own.
    #[test]
    fn a_companion_is_recorded_with_the_surface_it_composed_on() {
        for (surface, expected) in [
            (Visibility::PRIVATE, "private"),
            (Visibility::INTERFACE, "interface"),
            (Visibility::PUBLIC, "public"),
        ] {
            let mut frame = Frame::project();
            frame.admitted.companions = vec![companion_projection(surface, Vec::new())];

            let descriptors = descriptors(&frame.inputs(project_scope()));
            assert_eq!(
                annotations_of(&descriptors, "corp-ca")
                    .get("sh.ocx.visibility")
                    .and_then(Value::as_str),
                Some(expected),
            );
        }
    }

    /// A companion's dependencies are part of the closure: recorded after the companions, as
    /// dependencies with their own visibility, and once when a root already reached them.
    #[test]
    fn a_companions_dependencies_are_recorded_after_the_companions() {
        let mut frame = Frame::project();
        let trust_store = pinned("corp/trust-store", "internal.corp.example", None, INDEX_HEX);
        let runtime = pinned("ocx/libstdcxx-runtime", "index.ocx.sh", None, DEPENDENCY_HEX);
        frame.admitted.companions = vec![companion_projection(
            Visibility::INTERFACE,
            vec![
                ResolvedDependency {
                    identifier: trust_store,
                    visibility: Visibility::PRIVATE,
                },
                ResolvedDependency {
                    identifier: runtime,
                    visibility: Visibility::PUBLIC,
                },
            ],
        )];

        let descriptors = descriptors(&frame.inputs(project_scope()));
        let names: Vec<&str> = descriptors.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["cmake", "ninja", "libstdcxx-runtime", "corp-ca", "trust-store"],
            "a companion's dependency follows the companions; one a root reached stays where it was",
        );
        let trust_store = annotations_of(&descriptors, "trust-store");
        assert_eq!(
            trust_store.get("sh.ocx.role").and_then(Value::as_str),
            Some("dependency")
        );
        assert_eq!(
            trust_store.get("sh.ocx.visibility").and_then(Value::as_str),
            Some("private")
        );
        assert_eq!(
            annotations_of(&descriptors, "libstdcxx-runtime")
                .get("sh.ocx.visibility")
                .and_then(Value::as_str),
            Some("interface"),
            "the first-seen placement wins",
        );
    }

    /// One companion named under two tags at one digest is one package in the closure.
    #[test]
    fn a_companion_named_twice_at_one_digest_is_recorded_once() {
        let mut frame = Frame::project();
        let mut retagged = companion_projection(Visibility::INTERFACE, Vec::new());
        retagged.pinned = pinned("corp-ca", "internal.corp.example", Some("latest"), COMPANION_HEX);
        frame.admitted.companions = vec![companion_projection(Visibility::INTERFACE, Vec::new()), retagged];

        let descriptors = descriptors(&frame.inputs(project_scope()));
        assert_eq!(
            descriptors.iter().filter(|d| d.name == "corp-ca").count(),
            1,
            "two tags, one digest, one package",
        );
    }

    /// A companion that is also a root keeps the role it was first seen under.
    /// Reporting it twice would double-count the closure, and reporting it only
    /// as a companion would hide that the caller asked for it directly.
    #[test]
    fn a_companion_that_is_also_a_root_keeps_the_root_role() {
        let mut frame = Frame::project();
        let mut ninja = companion_projection(Visibility::INTERFACE, Vec::new());
        ninja.pinned = pinned("ocx/ninja", "index.ocx.sh", None, NINJA_HEX);
        frame.admitted.companions = vec![ninja];

        let descriptors = descriptors(&frame.inputs(project_scope()));
        assert_eq!(descriptors.iter().filter(|d| d.name == "ninja").count(), 1);
        assert_eq!(
            annotations_of(&descriptors, "ninja")
                .get("sh.ocx.role")
                .and_then(Value::as_str),
            Some("root"),
        );
    }

    #[test]
    fn the_launcher_frame_omits_context_it_never_had() {
        let frame = Frame::launcher();
        let resolution = resolution_block(&frame.inputs(Scope::Launcher));
        assert!(resolution.requested_platform.is_none());
        assert!(resolution.registries.is_none());
        assert!(resolution.mirrors.is_none());
        assert!(resolution.managed_config.is_none());
    }

    /// A frame that fetched nothing it can name reports an empty list, not a
    /// registry it never fetched from: a placeholder identifier's registry is
    /// whichever default happened to be configured. The fixture's install is
    /// stamped with a content registry like any other, so what is asserted here
    /// is the identity gate, not an accidental absence.
    #[test]
    fn a_frame_with_no_nameable_source_reports_no_registries() {
        let mut frame = Frame::launcher();
        // Package scope, so the frame *did* compose — the launcher frame omits
        // the key entirely and would pass this trivially.
        frame.config.mirrors = Vec::new();
        let resolution = resolution_block(&frame.inputs(Scope::Package { requested: Vec::new() }));
        assert_eq!(
            resolution.registries.as_deref(),
            Some([].as_slice()),
            "a composed frame states an empty list; only a launcher frame omits the key",
        );
    }

    #[test]
    fn a_placeholder_root_is_recorded_digest_only() {
        let frame = Frame::launcher();
        let descriptors = descriptors(&frame.inputs(Scope::Launcher));
        assert_eq!(descriptors.len(), 1);

        let root = &descriptors[0];
        assert_eq!(root.name, format!("file-url-mode/{LEAF_HEX}"));
        assert_eq!(root.uri, None, "no purl may be fabricated without a repository");
        assert_eq!(root.digest.get("sha256").map(String::as_str), Some(LEAF_HEX));
        assert_eq!(
            root.annotations,
            BTreeMap::from([
                ("sh.ocx.role".to_string(), Value::from("root")),
                ("sh.ocx.identity".to_string(), Value::from("synthetic")),
            ]),
        );
    }

    #[test]
    fn scope_projects_its_tier_specific_block() {
        let frame = Frame::project();
        match scope_block(&frame.inputs(project_scope())) {
            ScopeBlock::Project {
                clean_env,
                project_root,
                lock,
                groups,
            } => {
                assert!(!clean_env);
                assert_eq!(project_root, PathBuf::from("/scratch/job-88213"));
                assert_eq!(lock.path, PathBuf::from("/scratch/job-88213/ocx.lock"));
                assert_eq!(
                    lock.declaration_digest.get("sha256").map(String::as_str),
                    Some(LOCK_HEX)
                );
                assert_eq!(groups, vec!["default".to_string()]);
            }
            other => panic!("expected a project block, got {other:?}"),
        }

        let requested = PackageRef::parse("internal.corp.example/solver:2024.3").expect("identifier");
        match scope_block(&frame.inputs(Scope::Package {
            requested: vec![requested],
        })) {
            ScopeBlock::Package { requested, .. } => {
                assert_eq!(requested, vec!["internal.corp.example/solver:2024.3".to_string()]);
            }
            other => panic!("expected a package block, got {other:?}"),
        }

        assert!(matches!(
            scope_block(&frame.inputs(Scope::Launcher)),
            ScopeBlock::Launcher
        ));
    }

    // ── Serialization — the frozen wire form ────────────────────────────

    /// A record populated in every field, mirroring the design record's first
    /// exemplary record. Built directly rather than through [`ExecutionRecord::build`]
    /// so the golden key set below tests serialization alone.
    fn populated_record() -> ExecutionRecord {
        let frame = Frame::project();
        let inputs = frame.inputs(project_scope());
        ExecutionRecord {
            schema_version: SCHEMA_VERSION.to_string(),
            kind: RECORD_KIND.to_string(),
            recorded_at: "2026-07-26T14:03:11.482Z".parse().expect("timestamp"),
            ocx: OcxBuild {
                version: "0.4.1".to_string(),
                binary: PathBuf::from("/home/ci/.ocx/bin/ocx"),
            },
            frame: frame_for(&inputs.scope),
            process: Process {
                pid: 48123,
                parent: Some(ParentProcess { pid: 47990 }),
                user: Some(User {
                    id: Some("1000".to_string()),
                    name: Some("ci".to_string()),
                }),
                arch: Some(Architecture::Amd64),
                executable: inputs.executable.to_path_buf(),
                working_directory: Some(PathBuf::from("/scratch/job-88213")),
            },
            host: Host {
                name: Some("batch-node-17".to_string()),
            },
            os: Os {
                os_type: Some(OperatingSystem::Linux),
            },
            executable: executable_block(&inputs),
            scope: scope_block(&inputs),
            resolution: resolution_block(&inputs),
            packages: descriptors(&inputs),
        }
    }

    /// Collect every key path in `value`, `/`-separated, with `[]` marking a
    /// step through an array of objects. No record key contains a `/`.
    fn key_paths(value: &Value, prefix: &str, paths: &mut BTreeSet<String>) {
        match value {
            Value::Object(map) => {
                for (key, nested) in map {
                    let path = if prefix.is_empty() {
                        key.clone()
                    } else {
                        format!("{prefix}/{key}")
                    };
                    key_paths(nested, &path, paths);
                }
            }
            Value::Array(items) => {
                let nested: Vec<&Value> = items
                    .iter()
                    .filter(|item| item.is_object() || item.is_array())
                    .collect();
                if nested.is_empty() {
                    paths.insert(prefix.to_string());
                } else {
                    for item in nested {
                        key_paths(item, &format!("{prefix}[]"), paths);
                    }
                }
            }
            _ => {
                paths.insert(prefix.to_string());
            }
        }
    }

    /// Format rule 9 — the exact key set of a fully-populated record.
    ///
    /// The envelope is camelCase and the borrowed blocks are flat lowercase, so
    /// the one change that would break every consumer at once is a blanket
    /// `rename_all = "camelCase"` rewriting `process.working_directory`. Nothing
    /// in the type system objects to that; this does.
    #[test]
    fn serialized_key_set_matches_the_frozen_record_shape() {
        let value = serde_json::to_value(populated_record()).expect("serializes");
        let mut paths = BTreeSet::new();
        key_paths(&value, "", &mut paths);

        let expected: BTreeSet<String> = [
            "schemaVersion",
            "kind",
            "recordedAt",
            "ocx/version",
            "ocx/binary",
            "frame/command",
            "frame/identity",
            "process/pid",
            "process/parent/pid",
            "process/user/id",
            "process/user/name",
            "process/arch",
            "process/executable",
            "process/working_directory",
            "host/name",
            "os/type",
            "executable/sh.ocx.provenance",
            "executable/sh.ocx.kind",
            "executable/sh.ocx.package",
            "scope/tier",
            "scope/cleanEnv",
            "scope/projectRoot",
            "scope/lock/path",
            "scope/lock/declarationDigest/sha256",
            "scope/groups",
            "resolution/offline",
            "resolution/remote",
            "resolution/frozen",
            "resolution/patchSnapshot/sha256",
            "resolution/noVerify",
            "resolution/requestedPlatform",
            "resolution/registries",
            "resolution/insecureRegistries",
            "resolution/mirrors/ghcr.io/registry",
            "resolution/managedConfig/source",
            "resolution/managedConfig/digest/sha256",
            "packages[]/name",
            "packages[]/uri",
            "packages[]/digest/sha256",
            "packages[]/annotations/sh.ocx.role",
            "packages[]/annotations/sh.ocx.binding",
            "packages[]/annotations/sh.ocx.group",
            "packages[]/annotations/sh.ocx.platform",
            "packages[]/annotations/sh.ocx.visibility",
            "packages[]/annotations/sh.ocx.entrypoints",
            "packages[]/annotations/sh.ocx.binaries",
        ]
        .map(str::to_string)
        .into_iter()
        .collect();

        assert_eq!(paths, expected);
    }

    /// The borrowed blocks keep the spelling of the vocabulary they were lifted
    /// from, whatever the envelope does. The golden set above pins the keys a
    /// populated record happens to carry; this holds the *rule* — a blanket
    /// `rename_all = "camelCase"` on [`Process`], [`Host`] or [`Os`] would
    /// rewrite `working_directory` to `workingDirectory` and nothing in the type
    /// system would object.
    #[test]
    fn the_borrowed_blocks_are_never_camel_cased() {
        let value = serde_json::to_value(populated_record()).expect("serializes");
        let mut paths = BTreeSet::new();
        key_paths(&value, "", &mut paths);

        let borrowed: Vec<&String> = paths
            .iter()
            .filter(|path| ["process/", "host/", "os/"].iter().any(|block| path.starts_with(block)))
            .collect();
        assert!(
            borrowed.iter().any(|path| path.as_str() == "process/working_directory"),
            "the needle the rule exists for must be in the scanned set: {paths:?}",
        );

        for path in borrowed {
            assert!(
                !path.chars().any(char::is_uppercase),
                "`{path}` is camelCase; the borrowed blocks keep their own spec's spelling",
            );
        }
    }

    #[test]
    fn serializes_to_one_compact_line() {
        let json = populated_record().to_json().expect("serializes");
        assert!(!json.contains('\n'), "every mainstream log shipper is line-oriented");
        assert!(!json.contains(": "), "compact form has no pretty-printing padding");
        serde_json::from_str::<Value>(&json).expect("one valid JSON document");
    }

    #[test]
    fn schema_version_is_a_string_and_kind_is_present() {
        let value = serde_json::to_value(populated_record()).expect("serializes");
        assert_eq!(value["schemaVersion"], Value::String("1".to_string()));
        assert_eq!(value["kind"], Value::String(RECORD_KIND.to_string()));
    }

    #[test]
    fn recorded_at_is_fixed_width_millisecond_utc() {
        let mut record = populated_record();
        record.recorded_at = "2026-07-26T14:03:11Z".parse().expect("whole-second timestamp");
        let value = serde_json::to_value(record).expect("serializes");
        assert_eq!(
            value["recordedAt"],
            Value::String("2026-07-26T14:03:11.000Z".to_string()),
            "the width must not vary with the value",
        );
    }

    #[test]
    fn digests_are_bare_lowercase_hex_and_the_prefixed_form_lives_only_in_the_purl() {
        let value = serde_json::to_value(populated_record()).expect("serializes");
        for descriptor in value["packages"].as_array().expect("packages") {
            let hex = descriptor["digest"]["sha256"].as_str().expect("digest hex");
            assert!(!hex.contains(':'), "no transport or algorithm prefix: {hex}");
            assert_eq!(hex, hex.to_ascii_lowercase(), "digests are lowercase: {hex}");
            assert!(
                descriptor["uri"]
                    .as_str()
                    .expect("uri")
                    .contains(&format!("@sha256:{hex}")),
                "the prefixed form survives inside the purl, over the same hex",
            );
        }
        let lock = value["scope"]["lock"]["declarationDigest"]["sha256"]
            .as_str()
            .expect("lock declaration hex");
        assert!(!lock.contains(':'), "{lock}");
        let snapshot = value["resolution"]["patchSnapshot"]["sha256"]
            .as_str()
            .expect("patch snapshot hex");
        assert!(!snapshot.contains(':'), "{snapshot}");
    }

    #[test]
    fn build_assembles_every_block_of_a_launching_frame() {
        let frame = Frame::project();
        let inputs = frame.inputs(project_scope());
        let recorded_at = "2026-07-26T14:03:11.482Z".parse().expect("timestamp");
        let record = ExecutionRecord::build(&inputs, recorded_at, 48123);

        assert_eq!(record.schema_version, SCHEMA_VERSION);
        assert_eq!(record.kind, RECORD_KIND);
        assert_eq!(record.ocx.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(record.ocx.binary, PathBuf::from("/home/ci/.ocx/bin/ocx"));
        assert_eq!(record.frame.command, FrameCommand::Exec);
        assert_eq!(record.process.pid, 48123);
        assert_eq!(record.process.executable, frame.executable);
        assert_eq!(record.executable["sh.ocx.kind"], "launcher");
        assert!(matches!(record.scope, ScopeBlock::Project { .. }));
        assert_eq!(record.packages.len(), 3);
        serde_json::from_str::<Value>(&record.to_json().expect("serializes")).expect("one JSON document");
    }

    /// The child's command line never reaches the record. It routinely carries
    /// an access token or a password, and a record is written to a sink an
    /// operator may not control — so the check is on the serialized bytes, not
    /// on the absence of a field a future edit could re-add.
    #[test]
    fn the_invoked_command_line_is_never_serialized() {
        let mut frame = Frame::project();
        frame.argv = ["curl", "--header", "Authorization: Bearer s3cr3t-token"]
            .map(str::to_string)
            .to_vec();

        let inputs = frame.inputs(project_scope());
        let recorded_at = "2026-07-26T14:03:11.482Z".parse().expect("timestamp");
        let json = ExecutionRecord::build(&inputs, recorded_at, 48123)
            .to_json()
            .expect("serializes");

        assert!(!json.contains("s3cr3t-token"), "an argument value leaked: {json}");
        assert!(!json.contains("--header"), "an argument leaked: {json}");
        assert!(
            !serde_json::from_str::<Value>(&json).expect("one JSON document")["process"]
                .as_object()
                .expect("process block")
                .contains_key("args"),
            "{json}",
        );
        assert!(
            json.contains("\"executable\""),
            "the resolved executable is still recorded — it is the point of the record",
        );
    }

    #[test]
    fn digest_map_normalizes_case_and_keys_on_the_algorithm() {
        let map = digest_map(&Digest::Sha256(LEAF_HEX.to_ascii_uppercase()));
        assert_eq!(map, BTreeMap::from([("sha256".to_string(), LEAF_HEX.to_string())]));
    }

    #[test]
    fn an_unknown_platform_is_an_explicit_null_never_an_omitted_key() {
        let frame = Frame::launcher();
        let value = serde_json::to_value(resolution_block(&frame.inputs(Scope::Launcher))).expect("serializes");
        let map = value.as_object().expect("object");
        assert!(
            map.contains_key("requestedPlatform"),
            "the requested platform is not best-effort: absent context is stated, not omitted",
        );
        assert_eq!(map["requestedPlatform"], Value::Null);
        assert!(!map.contains_key("registries"), "{map:?}");
        assert!(!map.contains_key("insecureRegistries"), "{map:?}");
        assert!(!map.contains_key("mirrors"), "{map:?}");
        assert!(!map.contains_key("managedConfig"), "{map:?}");
        assert!(!map.contains_key("patchSnapshot"), "{map:?}");
    }

    #[test]
    fn best_effort_environment_keys_are_omitted_rather_than_filled() {
        let mut record = populated_record();
        record.host = Host { name: None };
        record.os = Os { os_type: None };
        record.process.parent = None;
        record.process.user = None;
        record.process.working_directory = None;

        let value = serde_json::to_value(record).expect("serializes");
        assert_eq!(value["host"], serde_json::json!({}));
        assert_eq!(value["os"], serde_json::json!({}));
        let process = value["process"].as_object().expect("object");
        for key in ["parent", "user", "working_directory"] {
            assert!(!process.contains_key(key), "{key} must be absent, never a placeholder");
        }
        assert!(process.contains_key("pid"), "load-bearing fields cannot go missing");
        assert!(process.contains_key("executable"));
    }

    #[test]
    fn a_managed_tier_without_a_known_snapshot_digest_still_names_its_source() {
        let mut frame = Frame::project();
        frame.managed_config_digest = None;
        let value = serde_json::to_value(resolution_block(&frame.inputs(project_scope()))).expect("serializes");
        let managed = value["managedConfig"].as_object().expect("managed config");
        assert!(managed.contains_key("source"));
        assert!(
            !managed.contains_key("digest"),
            "an absent key means not determinable; an empty map would read as no algorithm applies",
        );
    }
}
