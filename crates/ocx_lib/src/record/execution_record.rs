// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The record payload, and the inputs a launching frame supplies to build it.
//!
//! **The key set here is frozen wire format.** It is deliberately *not* uniform:
//! the envelope (`schemaVersion`, `recordedAt`, `identityNote`, `cleanEnv`,
//! `projectRoot`, `declarationDigest`, `requestedPlatform`, `managedConfig`,
//! `autoInstalled`, `noVerify`, `insecureRegistries`, `patchSnapshot`) is
//! camelCase — the record is an in-toto-style document, and in-toto/SLSA render
//! their own envelopes lowerCamelCase — while the `process`, `host` and `os`
//! blocks keep the flat lowercase spelling of the vocabularies they were lifted
//! from, including `process.working_directory`, which is snake.
//!
//! The dotted `sh.ocx.*` keys are neither: they are namespaced annotation keys
//! in the OCI sense — `sh.ocx.role`, `sh.ocx.provenance` — not field names, and
//! `kind` carries the same dotted namespace in its value
//! (`sh.ocx.execution-record`).
//!
//! For that reason no container here carries a blanket `rename_all`: a single
//! `rename_all = "camelCase"` would silently rewrite `working_directory` to
//! `workingDirectory` and break every consumer, with nothing in the type system
//! objecting. Each key that differs from its Rust field name carries an explicit
//! `#[serde(rename = "…")]`, and a golden test asserts the exact key set.
//!
//! Two field classes, and only one can fail the invocation. Load-bearing fields
//! (`packages[]`, `digest`, `process.executable`, `frame`) are in hand from
//! resolution — absence would be a bug, not a runtime condition. Best-effort
//! fields are `Option` and **omit their key** when undeterminable: an absent key
//! means "not determinable here", a present key is always true, and a
//! `"unknown"` sentinel would be indistinguishable from a host genuinely named
//! `unknown`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Serialize, Serializer};
use serde_json::Value;

use super::error::RecordsError;
use super::purl::{has_logical_identity, package_url};
use crate::config::insecure::allows_plain_http;
use crate::config::mirror::MirrorConfig;
use crate::env::OcxConfigView;
use crate::oci::{Architecture, Digest, Identifier, OperatingSystem, PinnedIdentifier, Platform};
use crate::package::install_info::InstallInfo;
use crate::package::metadata::visibility::Visibility;
use crate::package_manager::tasks::resolve::{AdmittedClaims, PatchProvenance};

/// The in-band schema version, a string, bumped **only** for a
/// backward-incompatible change.
///
/// Additive fields never bump it; consumers must tolerate unknown keys. This is
/// pip's installation-report discipline, adopted verbatim.
pub const SCHEMA_VERSION: &str = "1";

/// The `kind` discriminator carried by every record.
pub const RECORD_KIND: &str = "sh.ocx.execution-record";

/// Why a launcher frame's identity is degraded, stated in-band so a consumer
/// reading one record in isolation learns the limitation from the record itself
/// rather than from documentation it may not have.
const DEGRADED_IDENTITY_NOTE: &str = "package directories are content-shared and carry no registry/repository, so \
     logical identity is not recoverable in this frame and no purl can be emitted";

/// One pre-exec resolution record: the full resolved closure plus the resolved
/// executable, written immediately before the child starts.
///
/// Serialized **compact, one document per file, one line**. Every mainstream log
/// shipper is line-oriented, so on one line "read whole file" degenerates to
/// "read one line" and all of them cope. See [`Self::to_json`].
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct ExecutionRecord {
    /// Format version; always [`SCHEMA_VERSION`].
    #[serde(rename = "schemaVersion")]
    pub schema_version: String,

    /// Format discriminator; always [`RECORD_KIND`].
    pub kind: String,

    /// When the record was written — RFC 3339, UTC, millisecond precision.
    ///
    /// Schema'd as a string rather than pulling in schemars' chrono feature: the
    /// wire form *is* a string, so the feature would buy nothing.
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
    /// package purl it came from.
    ///
    /// A flat string map, deliberately **not** nested under
    /// `process.executable`: that field is typed as a flat keyword string by the
    /// vocabulary it was borrowed from, so hanging sub-fields off it would be an
    /// ingest type conflict rather than a naming quibble.
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

    /// Why identity is [`FrameIdentity::Degraded`]. Omitted when it is complete.
    #[schemars(extend("x-ocx-absent-when-none" = true))]
    #[serde(rename = "identityNote", skip_serializing_if = "Option::is_none")]
    pub identity_note: Option<String>,
}

/// The command that opened a launching frame.
///
/// The wire values are the CLI's own canonical command names
/// (`ocx_cli::app::canonical_command_name`), space-separated exactly as that
/// function spells them: one grep joins a record to the error envelope of the
/// same invocation, instead of two ocx-authored JSON documents naming one
/// command two ways.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub enum FrameCommand {
    /// `ocx exec` — the project tier.
    ///
    /// Also what the hidden deprecated `ocx run` spelling records. The record
    /// states what executed, and both spellings execute the project tier; the
    /// deprecation warning is the CLI's business, not the audit trail's.
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
    /// cannot recover logical identity and emits no purl. A truthful partial
    /// record beats a fabricated complete one.
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
    /// true of the process that ran. The machine's native architecture is
    /// deliberately absent — recovering it needs a per-OS native probe plus a
    /// `uname`-name-to-OCI mapping table this module would then own and have to
    /// keep correct, and a wrong answer in that field is worse than no answer.
    ///
    /// Typed rather than stringly because the controlled vocabulary is the OCI
    /// architecture vocabulary OCX already carries. An architecture outside that
    /// closed set is undeterminable here and omits the key.
    #[schemars(extend("x-ocx-absent-when-none" = true))]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub arch: Option<Architecture>,

    /// The resolved absolute executable — the record's highest-value field.
    pub executable: PathBuf,

    /// The working directory the child inherits.
    ///
    /// Snake-cased on the wire, unlike its camelCase envelope siblings, because
    /// that is the published spelling of the vocabulary this block borrows.
    /// Best-effort; omitted when undeterminable.
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
/// [`Self::id`] comes from the kernel and cannot be forged by the caller's
/// environment, while [`Self::name`] is read from the environment and can be.
/// An audit sink correlating *who ran this* must key on `id`.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct User {
    /// The effective user id the kernel reports, as a string.
    ///
    /// A string rather than a number because the platforms disagree on the
    /// type: a POSIX uid is numeric, a Windows SID is `S-1-5-…`. One key with
    /// one type means a consumer never branches on OS to read it.
    ///
    /// Omitted when undeterminable — currently always on Windows, which has no
    /// uid and whose SID needs a token lookup this module does not do.
    #[schemars(extend("x-ocx-absent-when-none" = true))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    /// Account name, best-effort and **not trustworthy**: it is read from
    /// `$USER`/`$LOGNAME` (`%USERNAME%` on Windows), all of which the caller
    /// controls. Present for readability; [`Self::id`] is the field to key on.
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
    /// Values are JSON, not strings: an in-toto descriptor's `annotations` is
    /// an object with arbitrary values, and the name lists genuinely are lists.
    /// Joining them into one string would be lossy — no separator is forbidden
    /// in a binary name, so `["a,b"]` and `["a","b"]` would arrive
    /// indistinguishable.
    pub annotations: BTreeMap<String, Value>,
}

/// The record's `scope` block — the serialized projection of [`Scope`].
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
    /// Named for what it hashes: the `ocx.toml` **declarations** the lock was
    /// generated from, which is the value the project tier already defines and
    /// carries in the lock's own metadata. It is deliberately **not** a digest
    /// of the lock's contents — two runs whose declarations agree share this
    /// value even when they resolved different closures, so a consumer must
    /// never use it as a closure identity. The resolved closure is recorded
    /// package-by-package with its digests in `packages[]`; that is the
    /// authoritative answer to "what actually ran".
    #[serde(rename = "declarationDigest")]
    pub declaration_digest: BTreeMap<String, String>,
}

/// The resolution policy in force for this invocation.
///
/// The four frame-varying fields below distinguish two states a plain empty
/// collection cannot: `None` means the frame has no such context at all (the
/// launcher frame), `Some(empty)` means the frame has the context and it is
/// empty. The published shape shows both — a package-tier record carries
/// `"mirrors": {}` while a launcher record omits the key entirely.
///
/// `autoInstalled` is load-bearing, not decoration: together with a
/// `sh.ocx.resolved-from: tag` annotation it is what shows an invocation
/// resolved a floating tag and materialized the package on the spot — the one
/// state no pull-time record can capture.
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
    /// The patch tier's answer to [`Self::frozen`], which scopes to the package
    /// tier alone. A snapshot is selected by `OCX_PATCH_SNAPSHOT` naming a
    /// `patches.snapshot.json` that `ocx patch freeze` wrote; under one, every
    /// companion composes at the digest the snapshot pins rather than at
    /// whatever its tag resolves to today. Recording the content digest rather
    /// than the path is what makes that auditable: a path is a name an operator
    /// can point anywhere, while this value changes the moment the pins do.
    ///
    /// Omitted when no snapshot is in force — the common case, and a different
    /// statement from "a snapshot with no pins".
    #[schemars(extend("x-ocx-absent-when-none" = true))]
    #[serde(rename = "patchSnapshot", skip_serializing_if = "BTreeMap::is_empty")]
    pub patch_snapshot: BTreeMap<String, String>,

    /// Whether the policy-gated auto-verify was opted out of (`OCX_NO_VERIFY`).
    ///
    /// Beside `offline` / `remote` / `frozen` because it belongs to the same
    /// class: an operator-set policy that changes what resolution was allowed to
    /// do. An auditor reading a record from a signing-enforced fleet needs to
    /// see that this invocation ran with verification disabled — the record is
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
    /// resolved to is its own `sh.ocx.platform` annotation, and the two differ
    /// legitimately — a flat single-image package selects `any` under any
    /// request. Naming this field for the request is what keeps the pair
    /// readable.
    ///
    /// A **string**, not a serialized [`Platform`]: `Platform`'s own
    /// serialization is the OCI descriptor object, which is a different shape.
    ///
    /// Emitted as an explicit `null` when the frame has no platform context —
    /// never omitted, and never fabricated from the host, which would make the
    /// record lie in exactly the audit that matters.
    #[serde(rename = "requestedPlatform")]
    pub requested_platform: Option<String>,

    /// Registries the frame's packages were fetched from.
    ///
    /// The **content** registries: the physical hosts the transport addressed,
    /// not the index endpoints a version choice was looked up through, and not
    /// the logical namespaces the identifiers name. All three are routinely
    /// different — a package named `ocx.sh/acme/tool` can be resolved through
    /// `index.ocx.sh` and fetched from `ghcr.io` — and reporting one under
    /// another's name is the kind of quiet mislabelling an audit trail cannot
    /// afford. The logical namespace is reported too, in each `packages[]`
    /// purl's `repository_url`, so a divergence between the two is visible
    /// rather than flattened.
    ///
    /// Reported before any `[mirrors]` rewrite of that host — the rewrites in
    /// force are their own field, so the pair composes. A root whose resolution
    /// named no content registry is omitted rather than guessed at.
    #[schemars(extend("x-ocx-absent-when-none" = true))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registries: Option<Vec<String>>,

    /// Which of [`Self::registries`] this invocation was allowed to reach over
    /// plain HTTP.
    ///
    /// The subset declared plaintext-eligible by `[registries."<name>"]
    /// insecure = true` or `OCX_INSECURE_REGISTRIES`, computed by the one
    /// predicate every other plaintext gate uses
    /// ([`crate::allows_plain_http`]) so the record cannot disagree with the
    /// transport about which hosts were exempt.
    ///
    /// Intersected with the registries this frame actually fetched from rather
    /// than reporting the whole configured allowance: a fleet-wide list of
    /// hosts nobody contacted says nothing about this invocation, while a host
    /// that both served content *and* was exempt from TLS is exactly the
    /// finding an auditor is looking for.
    ///
    /// Empty is absent, not `[]`: the overwhelmingly common case is that no
    /// registry was exempt, and a key present on every record would train
    /// readers to skip it.
    #[schemars(extend("x-ocx-absent-when-none" = true))]
    #[serde(rename = "insecureRegistries", skip_serializing_if = "Vec::is_empty")]
    pub insecure_registries: Vec<String>,

    /// Active mirror rewrites, upstream traffic host to its replacement
    /// endpoints.
    ///
    /// A per-host object rather than one endpoint string: a mirror entry
    /// declares the `registry` role, the `index` role, or both, and the roles
    /// address path-disjoint traffic. Collapsing them to one value would drop
    /// the second endpoint of every entry that configures both.
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
    /// launching frame. The source alone still names which tier was in force,
    /// and an empty map on the wire would read as "no digest algorithm applies"
    /// rather than "not determinable here".
    #[schemars(extend("x-ocx-absent-when-none" = true))]
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub digest: BTreeMap<String, String>,
}

/// Everything a launching frame hands the record builder.
///
/// Eight fields are uniform across frames; only [`Self::scope`] diverges, which
/// is what lets [`Frame`] be derived here rather than passed in by each caller.
#[derive(Debug)]
pub struct RecordInputs<'a> {
    /// Root packages; the transitive closure rides inside each resolved package.
    pub packages: &'a [Arc<InstallInfo>],

    /// Claimed executable names per package — which binaries and entry points
    /// each package put on `PATH`.
    ///
    /// The third claim class the composition attributes, `integrations`, is
    /// deliberately not projected: it composes vendor-namespaced configuration,
    /// not an executable the record's subject could have run.
    pub admitted: &'a AdmittedClaims,

    /// The patch companions this frame's site tier overlaid onto its packages,
    /// as `PackageManager::resolve_env_with_attribution` reported them.
    ///
    /// One entry per companion-contributed environment variable, so a companion
    /// contributing three variables appears three times; the record dedups by
    /// content identity. Empty when no `[patches]` tier is configured, which is
    /// the common case.
    pub patch_companions: &'a [PatchProvenance],

    /// The resolved executable, as `Env::resolve_command` produced it.
    pub executable: &'a Path,

    /// Root of the package store — `PackageStore::root`.
    ///
    /// Containment against this, rather than against the frame's root package
    /// directories, is what decides `sh.ocx.provenance`: every package lives
    /// under it, dependencies and launcher-frame packages included, and those
    /// are exactly the ones a frame never enumerates.
    pub store_root: &'a Path,

    /// Root of the shim store — `ShimStore::root`.
    ///
    /// The second half of the same containment test. A deferred tool reaches
    /// `PATH` as a generated launcher under `$OCX_HOME/shims/`, a sibling
    /// namespace to the three CAS tiers rather than a subtree of `packages/`,
    /// so a store-only test would libel every lazily composed tool as
    /// `external` — the record stating the binary did not come from an ocx
    /// package, which is exactly false.
    pub shim_root: &'a Path,

    /// `argv[0]` plus arguments, as invoked.
    ///
    /// Drives the launch — [`crate::launch`] takes the child's arguments from
    /// its tail — and is deliberately **not** recorded: a command line carries
    /// access tokens and passwords often enough that a record shipped to a
    /// central sink must not contain one.
    pub argv: &'a [String],

    /// Resolution policy, feeding the `resolution` block.
    pub config: &'a OcxConfigView,

    /// Every host this process may contact over plain HTTP — the process-wide
    /// union `Context` resolved once from `[registries]` and
    /// `OCX_INSECURE_REGISTRIES`.
    ///
    /// Handed in whole rather than pre-intersected with this frame's registries
    /// so the record does the intersection through the same
    /// [`crate::allows_plain_http`] predicate the transport uses; a caller
    /// filtering it first would be a second implementation of that comparison.
    pub insecure_registries: &'a [String],

    /// Digest of the applied managed-config snapshot.
    ///
    /// A separate input from [`Self::config`] because [`OcxConfigView`] carries
    /// the managed-config *source* — the resolution-affecting value it forwards
    /// to child processes — while the digest of the snapshot that source
    /// resolved to lives in the state store. Reading it here would be I/O on the
    /// exec path; `None` omits the key rather than delaying the launch.
    pub managed_config_digest: Option<&'a Digest>,

    /// Digest of the active patch snapshot's own file bytes.
    ///
    /// Read once at `Context::try_init`, in the same pass that parses the
    /// snapshot, so the digest and the pins it describes come from one read of
    /// one file. `None` when `OCX_PATCH_SNAPSHOT` designated nothing, which
    /// omits the key.
    pub patch_snapshot_digest: Option<&'a Digest>,

    /// The platform OCX resolved to. `None` at the launcher frame, which has no
    /// platform context and must emit `"platform": null` rather than guess.
    pub platform: Option<&'a Platform>,

    /// Whether the child env was built clean.
    pub clean_env: bool,

    /// Packages materialized during this invocation.
    pub auto_installed: &'a [Identifier],

    /// The only frame-divergent input.
    pub scope: Scope,
}

/// What the launching frame was scoped to.
///
/// The input twin of [`ScopeBlock`]: this carries the lock's declaration hash as
/// a parsed digest, the serialized block carries it as the wire map.
#[derive(Debug)]
pub enum Scope {
    /// `ocx exec` — a project toolchain.
    Project {
        /// Directory holding `ocx.toml`.
        root: PathBuf,
        /// The sibling `ocx.lock`.
        lock: PathBuf,
        /// The lock's own `metadata.declaration_hash` — the hash of the
        /// `ocx.toml` declarations, not of the lock's contents. Recorded under
        /// that name; see [`LockReference::declaration_digest`].
        ///
        /// Taken from the already-loaded lock rather than hashed afresh here:
        /// the value is the RFC 8785 canonicalization the project tier defines,
        /// and recomputing it would need the project config plus file I/O on the
        /// exec path.
        declaration_digest: Digest,
        /// Selected groups, in selection order.
        groups: Vec<String>,
        /// Which `ocx.toml` binding each root package was selected under.
        ///
        /// Only the project tier has bindings; the OCI tier names identifiers
        /// directly and the launcher frame names neither.
        bindings: Vec<PackageBinding>,
    },

    /// `ocx package exec` — identifiers named on the command line.
    Package {
        /// Identifiers as requested.
        requested: Vec<Identifier>,
    },

    /// `ocx launcher exec` — a generated entrypoint re-entry, which has no
    /// project context and a synthetic identifier by construction.
    Launcher,

    /// `ocx launcher shim` — a deferred tool's first invocation.
    ///
    /// Its own variant rather than [`Self::Package`] because [`frame_for`]
    /// derives the command from the scope, and this frame is neither
    /// `package exec` nor the identity-degraded `launcher exec`: it composes
    /// the package tier the way `package exec` does, but from an identifier
    /// ocx itself baked into the shim rather than one a user typed.
    LauncherShim {
        /// The tool the shim named, exactly as baked.
        requested: PinnedIdentifier,
    },
}

/// The `ocx.toml` binding a project-tier root package was selected under.
///
/// Carried on [`Scope::Project`] rather than beside the packages because it is
/// project-tier-only data, and [`Scope`] is where the frame-divergent inputs
/// live. Matched to a package by content identity, so the advisory tag cannot
/// split a binding from its package.
#[derive(Debug, Clone)]
pub struct PackageBinding {
    /// Binding name from `ocx.toml`.
    pub binding: String,
    /// The group the binding was selected from.
    pub group: String,
    /// The root package the binding resolved to.
    pub package: PinnedIdentifier,
}

impl ExecutionRecord {
    /// Build a record from a launching frame's inputs.
    ///
    /// `pid` is a parameter rather than read from the current process because
    /// the two platforms learn it at different moments: on Unix it is ocx's own
    /// pid, known up front; on Windows it is the spawned child's, which does not
    /// exist until after the spawn.
    ///
    /// Infallible: every load-bearing field is already in hand from resolution,
    /// and every environmental field omits its key instead of failing. There is
    /// no I/O here — the record is assembled from what the frame already
    /// resolved, so it adds nothing to the exec path but one serialization.
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

    /// Serialize to the published on-disk form: one JSON document, compact, on a
    /// single line, with no trailing newline handling implied.
    ///
    /// # Errors
    ///
    /// Returns [`RecordsError::Serialize`] when serialization fails.
    pub fn to_json(&self) -> Result<String, RecordsError> {
        serde_json::to_string(self).map_err(RecordsError::Serialize)
    }
}

/// Which command opened the frame, and whether it could name what it resolved.
///
/// Derived from [`Scope`] rather than passed in, so a frame cannot claim
/// complete identity while carrying a scope that structurally cannot have it.
fn frame_for(scope: &Scope) -> Frame {
    let (command, identity) = match scope {
        Scope::Project { .. } => (FrameCommand::Exec, FrameIdentity::Complete),
        Scope::Package { .. } => (FrameCommand::PackageExec, FrameIdentity::Complete),
        Scope::Launcher => (FrameCommand::LauncherExec, FrameIdentity::Degraded),
        // Complete, unlike its `launcher exec` sibling: a shim is baked with the
        // tool's pinned identifier, so this frame resolves logical identity
        // rather than being handed an anonymous package directory.
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

/// Project the frame's scope into its serialized block.
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
        // The package tier, because that is the tier a shim composes
        // (`EnvScope::package_tier`) and the identifier is the whole scope. The
        // command that opened the frame is `frame.command`'s job, not `tier`'s.
        Scope::LauncherShim { requested } => ScopeBlock::Package {
            clean_env: inputs.clean_env,
            requested: vec![requested.to_string()],
        },
    }
}

/// Project the resolution policy in force.
///
/// The launcher frame omits the four context fields rather than emitting empty
/// collections: it did not compose the environment and has no index, mirror or
/// managed-config context of its own, which is a different statement from
/// "composed with none".
fn resolution_block(inputs: &RecordInputs<'_>) -> Resolution {
    let composed = !matches!(inputs.scope, Scope::Launcher);
    let registries = composed.then(|| registries(inputs.packages));
    // Derived from `registries` rather than from the configured allowance, and
    // through the same predicate the transport consults: a host is reported here
    // only if this frame both fetched from it and was licensed to do so in the
    // clear.
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
        auto_installed: (!inputs.auto_installed.is_empty())
            .then(|| inputs.auto_installed.iter().map(ToString::to_string).collect()),
    }
}

/// The content registries the frame's root packages were fetched from, sorted
/// and deduplicated.
///
/// Each root's **physical** transport host, taken from
/// [`InstallInfo::transport_registry`] — never the logical namespace its
/// identifier names. Index indirection separates the two: an `ocx.sh` index root
/// can point at `ghcr.io/acme/tool`, and a field named for content registries
/// that reported `ocx.sh` would name a host nothing was fetched from. The
/// logical identity is not lost by this — it is what every `packages[]` purl
/// carries, in `repository_url`.
///
/// Reported before any `[mirrors]` rewrite of that host; the rewrites in force
/// are their own field.
///
/// A root with no transport provenance contributes nothing: a placeholder
/// identifier (whose registry is whichever default happened to be configured,
/// not a source anything was fetched from), or any path that resolved nothing
/// through the index. A frame whose every root is like that reports an empty
/// list — it fetched nothing it can name.
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

/// Project the resolved mirror entries to the frozen `host → {registry?, index?}`
/// shape, with any embedded credential stripped from both endpoints.
///
/// Both roles are reported because both rewrite this invocation's traffic. An
/// entry declaring neither is not a rewrite and is skipped.
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

/// A mirror endpoint with any `user:password@` userinfo removed.
///
/// `[mirrors]` values reach the record as the operator typed them, and userinfo
/// in a remote-repository URL is a mainstream Artifactory/Nexus idiom that OCX
/// accepts silently: [`parse_url`](crate::config::mirror::parse_url) keeps it
/// inside the host, and the index transport interpolates that host back into
/// every request, so the credential is *functional* rather than decorative. This
/// record's sink is operator-collected and routinely fleet-aggregated — the same
/// reason `process.args` is not carried. One credential in one config file must
/// not become one copy per invocation in a log store.
///
/// The authority rule is `parse_url`'s, deliberately, not the `url` crate's: the
/// span redacted here has to be the span the config layer treats as the host, or
/// the two would disagree about what was hidden. Scheme split on `://`, the
/// authority runs to the first `/`, and userinfo ends at the last `@` within it.
/// A value carrying no userinfo is returned byte-for-byte — the audit value of
/// this field is which host traffic was rewritten to, and normalizing an
/// untouched endpoint would cost that for nothing.
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
/// `sh.ocx.provenance` is the field an auditor reads first: `ocx-package` when
/// the resolved path lands inside the package store, `external` when it does
/// not. `ocx exec -- bash` picking up the system bash lands in the second case,
/// which is the fact the record exists to make visible.
///
/// The test is store containment rather than a walk of the frame's root
/// packages: a dependency's binary is on `PATH` too, and the launcher frame runs
/// a binary from a package it never enumerated. Both are inside the store and
/// both would be libelled `external` by a root-only test.
fn executable_block(inputs: &RecordInputs<'_>) -> BTreeMap<String, String> {
    let mut block = BTreeMap::new();
    let in_shim_store = inputs.executable.starts_with(inputs.shim_root);
    if !in_shim_store && !inputs.executable.starts_with(inputs.store_root) {
        block.insert("sh.ocx.provenance".to_string(), "external".to_string());
        return block;
    }

    block.insert("sh.ocx.provenance".to_string(), "ocx-package".to_string());
    // A shim tree holds neither `entrypoints/` nor `content/`, so the
    // store-relative scan has nothing to find there; the third kind is the
    // containment answer itself.
    let kind = if in_shim_store {
        Some("shim")
    } else {
        executable_kind(inputs.executable, inputs.store_root)
    };
    if let Some(kind) = kind {
        block.insert("sh.ocx.kind".to_string(), kind.to_string());
    }
    // The purl needs the owning package's identity, which only a root carries —
    // a dependency is reachable here as an identifier but not as a directory. A
    // dependency-owned executable therefore records its provenance and kind
    // truthfully and omits the package purl rather than guessing at one.
    if let Some(info) = owning_root(inputs)
        && let Some(purl) = package_url(info.identifier(), info.platform())
    {
        block.insert("sh.ocx.package".to_string(), purl);
    }
    block
}

/// Which of a package's two executable trees the resolved path sits in.
///
/// The store's two; the third kind, `shim`, is not a tree inside a package at
/// all but the separate shim store, so [`executable_block`] decides it by
/// containment before calling this.
///
/// `entrypoints/` holds the generated launcher shims, `content/` the payload the
/// package shipped. That distinction is what makes the two records an entrypoint
/// invocation produces readable as a pair: the outer frame resolves the launcher,
/// the launcher re-entry resolves the leaf binary, and they join on the digest.
///
/// Scanned forward from the store root, so a package that ships its own
/// `content/entrypoints/` directory still reports the package-level tree.
fn executable_kind(executable: &Path, store_root: &Path) -> Option<&'static str> {
    let relative = executable.strip_prefix(store_root).ok()?;
    relative
        .components()
        .find_map(|component| match component.as_os_str().to_str()? {
            "entrypoints" => Some("launcher"),
            "content" => Some("binary"),
            _ => None,
        })
}

/// The frame's root package that owns the resolved executable, if one does.
///
/// Two directories per root, because a deferred root has two: the package
/// directory it *will* materialize into, which is pure path arithmetic over the
/// pinned identifier and does not exist yet, and the generated shim tree its
/// launchers actually run from. Matching only the first would drop
/// `sh.ocx.package` from every lazily composed tool.
fn owning_root<'a>(inputs: &'a RecordInputs<'_>) -> Option<&'a Arc<InstallInfo>> {
    inputs.packages.iter().find(|info| {
        inputs.executable.starts_with(info.dir().root())
            || info
                .deferred()
                .is_some_and(|deferred| inputs.executable.starts_with(deferred.shim().root()))
    })
}

/// Project the resolved closure: roots in composition order, then the
/// dependencies each root carries in the topological order they were resolved
/// in, then the patch companions the site tier overlaid. Deduplicated by content
/// identity, so a package reachable twice appears once and keeps its first-seen
/// role.
///
/// Companions come last and as their own pass, not interleaved with the roots
/// they were admitted for: the overlay is site policy rather than anything the
/// invocation asked for, and a reader scanning `packages[]` top-down should
/// reach everything the caller requested before anything the site added.
fn descriptors(inputs: &RecordInputs<'_>) -> Vec<ResourceDescriptor> {
    let mut seen: HashSet<PinnedIdentifier> = HashSet::new();
    let mut descriptors = Vec::new();
    let admitted = AdmittedIndex::build(inputs.admitted);

    for info in inputs.packages {
        // A root has no incoming dependency edge, so it carries no edge
        // visibility of its own; from the composition's perspective it is fully
        // visible, which is what `public` records.
        //
        // The platform is the one the resolution *selected* for this package,
        // never the frame's requested platform: a root reached from the store
        // and a root pulled on the spot must not describe the same artefact
        // differently.
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
            // A deferred root composes as an `InstallInfo` with no package
            // directory and no content on disk. Without this key its descriptor
            // is indistinguishable from a package that exists — same digest,
            // same purl — which is a claim the record cannot support.
            // Only a root can be deferred; a dependency is reached through one.
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
            // A dependency is reachable here as an identifier, not as an
            // install, so its selected platform is not in hand — and the
            // frame's requested platform is a different fact. Omit rather than
            // guess; the digest still names the exact bits.
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

    for provenance in inputs.patch_companions {
        // `interface`, and not because a companion declares it: the overlay
        // composes a companion through `composer::compose_companion` with
        // `self_view = false`, so its interface surface is definitionally the
        // only thing that reached this environment.
        //
        // The platform is omitted for the same reason a dependency's is — the
        // provenance names the companion, not the install that satisfied it, so
        // the purl carries no `arch` qualifier either.
        if let Some(descriptor) = project(
            &provenance.pinned,
            Placement {
                role: "companion",
                visibility: Visibility::INTERFACE,
                platform: None,
            },
            inputs,
            &admitted,
            &mut seen,
        ) {
            descriptors.push(descriptor);
        }
    }

    descriptors
}

/// How one package sits in this closure — the three facts that vary by the pass
/// [`descriptors`] found it in, as opposed to the facts carried by the package
/// itself.
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
    identifier: &PinnedIdentifier,
    placement: Placement<'_>,
    inputs: &RecordInputs<'_>,
    admitted: &AdmittedIndex,
    seen: &mut HashSet<PinnedIdentifier>,
) -> Option<ResourceDescriptor> {
    if !seen.insert(identifier.strip_advisory()) {
        return None;
    }

    let mut annotations = BTreeMap::from([("sh.ocx.role".to_string(), Value::from(placement.role))]);
    let digest = digest_map(&identifier.digest());

    let Some(uri) = package_url(identifier, placement.platform) else {
        // A content-addressed placeholder carries no logical facts to annotate:
        // its name, registry and repository are all local artefacts of how the
        // frame reached the package, so the descriptor stays digest-only and
        // says so.
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
    if identifier.tag().is_some() {
        // A tag survived into the pinned identifier, so this package was reached
        // by resolving a floating tag rather than by a digest already pinned. It
        // is half of the drift signal; `resolution.autoInstalled` is the other.
        annotations.insert("sh.ocx.resolved-from".to_string(), Value::from("tag"));
    }

    Some(ResourceDescriptor {
        name: identifier.name().to_string(),
        uri: Some(uri),
        digest,
        annotations,
    })
}

/// The binding a project-tier package was selected under, if the frame has any.
fn binding_for<'a>(identifier: &PinnedIdentifier, scope: &'a Scope) -> Option<&'a PackageBinding> {
    match scope {
        Scope::Project { bindings, .. } => bindings.iter().find(|binding| binding.package.eq_content(identifier)),
        Scope::Package { .. } | Scope::Launcher | Scope::LauncherShim { .. } => None,
    }
}

/// The executable names each package contributed to `PATH`, in composition
/// order, keyed by the owning package's content identity.
///
/// Built once per record rather than rescanned per descriptor: the claim lists
/// hold every name of every package in the closure, and the naive form scanned
/// both of them twice for each descriptor.
struct AdmittedIndex {
    binaries: HashMap<PinnedIdentifier, Vec<Value>>,
    entrypoints: HashMap<PinnedIdentifier, Vec<Value>>,
}

impl AdmittedIndex {
    fn build(admitted: &AdmittedClaims) -> Self {
        Self {
            binaries: claims_by_owner(&admitted.binaries),
            entrypoints: claims_by_owner(&admitted.entrypoints),
        }
    }
}

/// Group one claim list by owner.
///
/// Keyed on [`PinnedIdentifier::strip_advisory`] because attribution is content
/// identity: the advisory tag must not split a package's claims from the
/// package. That is the hash-lookup form of the `eq_content` comparison this
/// replaces.
fn claims_by_owner<T: std::fmt::Display>(claims: &[(PinnedIdentifier, T)]) -> HashMap<PinnedIdentifier, Vec<Value>> {
    let mut index: HashMap<PinnedIdentifier, Vec<Value>> = HashMap::new();
    for (owner, name) in claims {
        index
            .entry(owner.strip_advisory())
            .or_default()
            .push(Value::from(name.to_string()));
    }
    index
}

/// Render a digest as the frozen `algorithm → bare lowercase hex` map.
///
/// The algorithm is the key, which is the point of the map: the value never
/// carries `sha256:`, never a transport prefix, never uppercase. The prefixed
/// form survives only inside the purl.
fn digest_map(digest: &Digest) -> BTreeMap<String, String> {
    let (algorithm, hex) = digest.parts();
    BTreeMap::from([(algorithm.to_string(), hex.to_ascii_lowercase())])
}

/// Serialize a timestamp as RFC 3339, UTC, millisecond precision — the frozen
/// `recordedAt` form (`2026-07-26T14:03:11.482Z`).
///
/// chrono's derived serializer picks its subsecond width from the value, which
/// would emit `…:11Z` for a timestamp that happens to land on a whole second.
/// The published form is fixed-width, so it is pinned here.
fn serialize_rfc3339_millis<S: Serializer>(value: &DateTime<Utc>, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::str::FromStr;

    use serde_json::Value;

    use super::*;
    use crate::config::mirror::MirrorConfig;
    use crate::oci::Digest;
    use crate::package::metadata::{BinaryName, EntrypointName, Metadata};
    use crate::package::resolved_package::{ResolvedDependency, ResolvedPackage};

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

    fn pinned(repository: &str, registry: &str, tag: Option<&str>, hex: &str) -> PinnedIdentifier {
        let mut identifier = Identifier::new_registry(repository, registry);
        if let Some(tag) = tag {
            identifier = identifier.clone_with_tag(tag);
        }
        PinnedIdentifier::try_from(identifier.clone_with_digest(Digest::Sha256(hex.to_string())))
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
    fn install(identifier: PinnedIdentifier, dir: &str, dependencies: Vec<ResolvedDependency>) -> Arc<InstallInfo> {
        let registry = identifier.registry().to_string();
        Arc::new(
            InstallInfo::new(
                identifier,
                metadata(),
                ResolvedPackage { dependencies },
                crate::file_structure::PackageDir::with_root(PathBuf::from(dir)),
            )
            .with_platform(linux_amd64())
            .with_transport_registry(registry),
        )
    }

    /// Owned fixture data, so a [`RecordInputs`] can borrow from one place.
    struct Frame {
        packages: Vec<Arc<InstallInfo>>,
        admitted: AdmittedClaims,
        patch_companions: Vec<PatchProvenance>,
        executable: PathBuf,
        store_root: PathBuf,
        shim_root: PathBuf,
        argv: Vec<String>,
        config: OcxConfigView,
        insecure_registries: Vec<String>,
        platform: Option<Platform>,
        auto_installed: Vec<Identifier>,
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
                // The default frame composes no patch tier; the tests that assert
                // companion projection install one explicitly.
                patch_companions: Vec::new(),
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
                patch_companions: Vec::new(),
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
                patch_companions: &self.patch_companions,
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
    fn companion_provenance() -> PatchProvenance {
        PatchProvenance {
            rule_match: "*".to_string(),
            companion: Identifier::parse("internal.corp.example/corp-ca:2024").expect("identifier"),
            pinned: pinned("corp-ca", "internal.corp.example", Some("2024"), COMPANION_HEX),
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
            crate::file_structure::PackageDir::with_root(PathBuf::from("/home/ci/.ocx/packages/cmake")),
        ));

        let descriptors = descriptors(&frame.inputs(project_scope()));
        assert!(
            !annotations_of(&descriptors, "cmake").contains_key("sh.ocx.platform"),
            "the frame's requested platform must not stand in for an unknown one",
        );
    }

    // ── R2 — the full closure, roles and order ──────────────────────────

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
            requested: vec![Identifier::parse("index.ocx.sh/ocx/cmake:3.28").expect("identifier")],
        }));
        assert_eq!(
            annotations_of(&descriptors, "cmake")
                .get("sh.ocx.resolved-from")
                .and_then(Value::as_str),
            Some("tag"),
        );
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
                crate::file_structure::PackageDir::with_root(PathBuf::from("/home/ci/.ocx/packages/cmake")),
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
            let parsed = crate::config::mirror::parse_url(endpoint).expect("a redacted endpoint still parses");
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
        frame.auto_installed = vec![Identifier::parse("internal.corp.example/solver").expect("identifier")];
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
        frame.patch_companions = vec![companion_provenance()];

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
        assert_eq!(
            companion.annotations.get("sh.ocx.visibility").and_then(Value::as_str),
            Some("interface"),
            "the overlay composes a companion's interface surface and nothing else",
        );
    }

    /// One companion contributing several environment variables arrives as
    /// several provenance rows; the closure names it once, like every other
    /// package reachable twice.
    #[test]
    fn a_companion_contributing_several_variables_is_recorded_once() {
        let mut frame = Frame::project();
        frame.patch_companions = vec![companion_provenance(), companion_provenance(), companion_provenance()];

        let descriptors = descriptors(&frame.inputs(project_scope()));
        assert_eq!(
            descriptors.iter().filter(|d| d.name == "corp-ca").count(),
            1,
            "three contributed variables, one package",
        );
    }

    /// A companion that is also a root keeps the role it was first seen under.
    /// Reporting it twice would double-count the closure, and reporting it only
    /// as a companion would hide that the caller asked for it directly.
    #[test]
    fn a_companion_that_is_also_a_root_keeps_the_root_role() {
        let mut frame = Frame::project();
        frame.patch_companions = vec![PatchProvenance {
            rule_match: "*".to_string(),
            companion: Identifier::parse("index.ocx.sh/ocx/ninja").expect("identifier"),
            pinned: pinned("ocx/ninja", "index.ocx.sh", None, NINJA_HEX),
        }];

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

        let requested = Identifier::parse("internal.corp.example/solver:2024.3").expect("identifier");
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
