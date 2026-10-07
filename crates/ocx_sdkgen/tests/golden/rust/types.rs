// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The types of the machine documents, decoded as `ocx` prints them.

use std::collections::BTreeMap;

use serde::de::Error as _;
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use super::wire::{Unknowns, field, payload};

/// System information about the ocx installation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct About {
    /// The version ocx reports for itself.
    pub version: String,
    /// The default registry a bare identifier resolves against.
    pub registry: RegistryHost,
    /// The host platform as ocx matches it, `os.features` included.
    pub platforms: Vec<Platform>,
    /// The host's full `os.features` (a superset of `libc`); a package is runnable when its offered
    /// features are a subset of these.
    pub features: Vec<String>,
    /// Detected host libc `os.features` tags (e.g. `\["libc.glibc"\]`,
    /// `\["libc.glibc","libc.musl"\]`), empty when none detected (non-Linux, NixOS, failed probe).
    /// Reflects the same host detection the index-resolution path uses; a host may advertise
    /// multiple families.
    pub libc: Vec<String>,
    /// The shell ocx detected; absent when it detected none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    /// The ocx home directory.
    pub home: AbsolutePath,
    /// The release channel the binary was built for (`dev`, `stable`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// The git commit the binary was built from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<CommitInfo>,
    /// The build environment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<BuildInfo>,
    /// The CI run that built the binary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci: Option<CiInfo>,
}

impl Unknowns for About {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "version", &self.version);
        field(pointer, out, "registry", &self.registry);
        field(pointer, out, "platforms", &self.platforms);
        field(pointer, out, "features", &self.features);
        field(pointer, out, "libc", &self.libc);
        field(pointer, out, "shell", &self.shell);
        field(pointer, out, "home", &self.home);
        field(pointer, out, "channel", &self.channel);
        field(pointer, out, "commit", &self.commit);
        field(pointer, out, "build", &self.build);
        field(pointer, out, "ci", &self.ci);
    }
}

/// Absolute path in the native spelling of the host that wrote it.
pub type AbsolutePath = String;

/// How a rendered toolchain home reaches a shell's environment.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ActivateMode {
    /// Compose the toolchain environment on every prompt. The default.
    Env,
    /// Put `<home>/toolchain/active/bin` on `PATH` and compose nothing else, so a tool is resolved
    /// by its launcher trampoline at invocation time.
    Bin,
    /// Neither. The reconciler withdraws whatever it owns and adds nothing.
    None,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl ActivateMode {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Env => "env",
            Self::Bin => "bin",
            Self::None => "none",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "env" => Self::Env,
            "bin" => Self::Bin,
            "none" => Self::None,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for ActivateMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for ActivateMode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ActivateMode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for ActivateMode {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// What the registry holds at an alias tag, taken as a whole.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum AliasState {
    /// An image index — the shape every alias is supposed to have.
    Present,
    /// No such tag at the registry: the alias was never created.
    Absent,
    /// The tag exists but resolves to a bare image manifest, so it has no per-platform slots at all
    /// and every expected platform reads as missing.
    NotAnIndex {
        /// The bare manifest the tag resolves to.
        digest: Digest,
    },
    /// A variant this SDK was not generated for: the whole object, as sent.
    Unknown(Value),
}

impl Serialize for AliasState {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Present => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "present")?;
                object.end()
            }
            Self::Absent => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "absent")?;
                object.end()
            }
            Self::NotAnIndex { digest: field_0 } => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "not_an_index")?;
                object.serialize_entry("digest", field_0)?;
                object.end()
            }
            Self::Unknown(value) => value.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for AliasState {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let Some(tag) = value.get("type").and_then(Value::as_str) else {
            return Err(D::Error::custom("a union object needs a string `type`"));
        };
        match tag {
            "present" => Ok(Self::Present),
            "absent" => Ok(Self::Absent),
            "not_an_index" => {
                #[derive(Deserialize)]
                struct Fields {
                    digest: Digest,
                }
                let fields = payload::<Fields, D::Error>("not_an_index", value)?;
                Ok(Self::NotAnIndex { digest: fields.digest })
            }
            _ => Ok(Self::Unknown(value)),
        }
    }
}

impl Unknowns for AliasState {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        match self {
            Self::Present => {}
            Self::Absent => {}
            Self::NotAnIndex { digest: field_0 } => {
                field(pointer, out, "digest", field_0);
            }
            Self::Unknown(_) => out.push(pointer.clone()),
        }
    }
}

/// The registry tag itself, e.g. latest, 3, 3.28, 3.28.1, or a variant name.
pub type AliasTag = String;

/// Result of a successful `ocx package announce`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnnounceReport {
    /// The announced `<namespace>/<package>` identifier.
    pub package: String,
    /// `unchanged` when the rebuilt root was byte-identical to the committed one, so nothing was
    /// committed; `updated` otherwise. An unchanged `--fork` run still ensures a pull request when
    /// its announce branch is ahead of the index base, and still reports one when the branch has
    /// diverged from the index base but its open pull request can still merge.
    pub status: WriteStatus,
    /// `updated` when the package's `__ocx.desc` artifact moved, so the root's `desc` object was
    /// rebuilt and its readme (and logo) written as new content-addressed objects; `unchanged` when
    /// the description sits where the committed root already recorded it, or there is none.
    pub desc_status: WriteStatus,
    /// The resolved forge kind, after `--forge`.
    pub forge: Forge,
    /// The selected write transport.
    pub transport: Transport,
    /// The API credential's kind. `job_token` only when the credential is this environment's own
    /// `CI_JOB_TOKEN` — ocx cannot tell a personal from a project, group or OAuth token, so it
    /// reports no kind it cannot observe.
    pub credential_kind: CredentialKind,
    /// The push credential's kind; absent under the `api` transport, which pushes nothing.
    /// `git_helper` means nothing was injected and git's own credential helpers authenticated the
    /// push.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push_credential_kind: Option<PushCredentialKind>,
    /// The announce branch; absent under `--output`, which opens no request and so has no branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The opened or updated pull request's web URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request_url: Option<String>,
    /// The opened or updated pull request's number; absent under the same conditions as
    /// `pull_request_url`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request_number: Option<i64>,
    /// The verified fork, as `owner/repo`; absent for `--output`, and in `--fork` mode only when
    /// the run made no pull request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork: Option<String>,
    /// The relative paths written under the `--output` directory; empty outside `--output`.
    pub written_paths: Vec<String>,
    /// Every preflight row, including the ones that did not apply.
    pub capability_checks: Vec<CapabilityCheckEntry>,
    /// Tags dropped from the curated set because they are reserved.
    pub reserved_tags_dropped: Vec<String>,
    /// Tags whose rows this run removed from the index because the registry no longer has them.
    /// Always an array, empty rather than absent.
    pub removed: Vec<String>,
    /// Durable tags the registry no longer has, found by `--refresh` or `--tags-from-registry` and
    /// kept in the index; name them with `--tags` or `--tags-file` to remove them. Always an array,
    /// empty rather than absent.
    pub durable_missing: Vec<String>,
}

impl Unknowns for AnnounceReport {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "package", &self.package);
        field(pointer, out, "status", &self.status);
        field(pointer, out, "desc_status", &self.desc_status);
        field(pointer, out, "forge", &self.forge);
        field(pointer, out, "transport", &self.transport);
        field(pointer, out, "credential_kind", &self.credential_kind);
        field(pointer, out, "push_credential_kind", &self.push_credential_kind);
        field(pointer, out, "branch", &self.branch);
        field(pointer, out, "pull_request_url", &self.pull_request_url);
        field(pointer, out, "pull_request_number", &self.pull_request_number);
        field(pointer, out, "fork", &self.fork);
        field(pointer, out, "written_paths", &self.written_paths);
        field(pointer, out, "capability_checks", &self.capability_checks);
        field(pointer, out, "reserved_tags_dropped", &self.reserved_tags_dropped);
        field(pointer, out, "removed", &self.removed);
        field(pointer, out, "durable_missing", &self.durable_missing);
    }
}

/// The terminating assertion record (when the run failed on an assertion).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AssertionRecord {
    /// Assertion kind (e.g. `ok`, `eq`, `contains`).
    pub kind: String,
    /// Failure detail. For `expect.ok`, auto-embeds the child stderr.
    pub message: String,
    /// Where in the script the failure happened. Additive-optional: absent for an outcome the
    /// engine could not locate (a timeout, a pre-engine host fault), never emitted as `null`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<SourceLocation>,
}

impl Unknowns for AssertionRecord {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "kind", &self.kind);
        field(pointer, out, "message", &self.message);
        field(pointer, out, "location", &self.location);
    }
}

/// What `--sbom` did after the push landed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AttestationOutcome {
    /// Whether the attestation was published.
    pub status: AttestationStatus,
    /// Digest of the published OCI referrer manifest. Absent on failure, and under
    /// `--signature-format simplesigning`, which publishes the `sha256-<hex>.att` sidecar alone and
    /// no referrer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referrer_digest: Option<Digest>,
    /// Digest of the `sha256-<hex>.att` sidecar manifest, when `--signature-format` asked for one.
    /// Spelled as in the `ocx package attest` report, so one vocabulary names both addresses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidecar_digest: Option<Digest>,
    /// The resolved `predicateType` URI written into the Statement. Present exactly when `status`
    /// is `succeeded`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predicate_type: Option<String>,
    /// Whether the referrer carries a signature. `false` means the SBOM was attached raw because
    /// the run had no signing identity available — the push still succeeded, and nothing vouches
    /// for the document. Present exactly when `status` is `succeeded`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed: Option<bool>,
    /// The error document's `error.detail` slug, falling back to its `error.kind` category. Present
    /// exactly when `status` is `failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Human-readable cause, sanitized for the terminal (CWE-150). Present exactly when `status` is
    /// `failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl Unknowns for AttestationOutcome {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "status", &self.status);
        field(pointer, out, "referrer_digest", &self.referrer_digest);
        field(pointer, out, "sidecar_digest", &self.sidecar_digest);
        field(pointer, out, "predicate_type", &self.predicate_type);
        field(pointer, out, "signed", &self.signed);
        field(pointer, out, "kind", &self.kind);
        field(pointer, out, "message", &self.message);
    }
}

/// Summary of a successful keyless attestation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AttestationReport {
    /// The package that was attested, resolved against the default registry.
    pub identifier: PackageRef,
    /// The `--platform` the run narrowed into; absent when none was given and the run attested
    /// whatever the identifier resolved to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<Platform>,
    /// Digest of the subject manifest the Statement names.
    pub subject_digest: Digest,
    /// The resolved `predicateType` URI written into the Statement.
    pub predicate_type: String,
    /// Digest of the referrer's layer content: the Sigstore bundle blob on a signed attach, the
    /// SBOM document itself on an unsigned one. The JSON key keeps its shipped name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_digest: Option<Digest>,
    /// Digest of the published OCI referrer manifest wrapping the payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referrer_digest: Option<Digest>,
    /// Digest of the `sha256-<hex>.att` sidecar manifest, when `--signature-format` asked for one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidecar_digest: Option<Digest>,
    /// Whether the referrer carries a signature. `false` means the document was attached as-is,
    /// with no identity behind it — the two certificate fields below are then absent rather than
    /// empty.
    pub signed: bool,
    /// Certificate SAN (identity) embedded in the Fulcio cert.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate_identity: Option<String>,
    /// Certificate OIDC issuer URL embedded in the Fulcio cert.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate_oidc_issuer: Option<String>,
    /// Which key model produced this attestation (`keyless`, `file`, and — once they exist —
    /// `aws_kms` and friends). Absent on an unsigned attach, where no key model was involved at
    /// all. Same vocabulary as the `ocx package sign` report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_backend: Option<KeyBackendKind>,
    /// The signing key's cosign hint, in key mode only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_key_hint: Option<String>,
    /// The Rekor log index of the transparency record this run created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transparency_log_index: Option<i64>,
}

impl Unknowns for AttestationReport {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "identifier", &self.identifier);
        field(pointer, out, "platform", &self.platform);
        field(pointer, out, "subject_digest", &self.subject_digest);
        field(pointer, out, "predicate_type", &self.predicate_type);
        field(pointer, out, "bundle_digest", &self.bundle_digest);
        field(pointer, out, "referrer_digest", &self.referrer_digest);
        field(pointer, out, "sidecar_digest", &self.sidecar_digest);
        field(pointer, out, "signed", &self.signed);
        field(pointer, out, "certificate_identity", &self.certificate_identity);
        field(pointer, out, "certificate_oidc_issuer", &self.certificate_oidc_issuer);
        field(pointer, out, "key_backend", &self.key_backend);
        field(pointer, out, "public_key_hint", &self.public_key_hint);
        field(pointer, out, "transparency_log_index", &self.transparency_log_index);
    }
}

/// What a `--tags` / `--tags-file` sweep did, one row per swept tag.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AttestationReportSweep {
    /// One row per swept tag, in the order the tags were given.
    pub items: Vec<AttestationReportSweptTag>,
}

impl Unknowns for AttestationReportSweep {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "items", &self.items);
    }
}

/// One swept tag's row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AttestationReportSweptTag {
    /// The tag as the caller spelled it, so the report names what was asked for rather than what it
    /// resolved to.
    pub tag: String,
    /// What the sweep did to this tag.
    pub status: SweptStatus,
    /// The per-reference report, verbatim. Present for every tag whose run produced one, which
    /// includes a `failed` row carrying a partial report: a `--signature-format both` tag where one
    /// leg landed and one did not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<AttestationReport>,
    /// The error's slug, else its category: the `error.detail`, else the `error.kind`, its error
    /// document would carry. Present exactly when `status` is `failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Human-readable cause, sanitized for the terminal (CWE-150). Present exactly when `status` is
    /// `failed` or `covered` — for a `covered` row it names the tag whose run wrote the referrer,
    /// not a failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl Unknowns for AttestationReportSweptTag {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "tag", &self.tag);
        field(pointer, out, "status", &self.status);
        field(pointer, out, "report", &self.report);
        field(pointer, out, "kind", &self.kind);
        field(pointer, out, "message", &self.message);
    }
}

/// Whether the `--sbom` attestation was published.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AttestationStatus {
    /// The attestation was published on the pushed manifest.
    Succeeded,
    /// The push landed and was NOT rolled back; the attestation did not.
    Failed,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl AttestationStatus {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for AttestationStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for AttestationStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AttestationStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for AttestationStatus {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// A newer release outside the current major, which only `--major` moves to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BeyondMajor {
    /// Local binding name (the `ocx.toml` key).
    pub name: String,
    /// Owning group — `default` for the top-level `\[tools\]` table.
    pub group: String,
    /// The tag declared after this run.
    pub tag: String,
    /// The newest tag at the same precision in a higher major.
    pub newest_tag: String,
}

impl Unknowns for BeyondMajor {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "name", &self.name);
        field(pointer, out, "group", &self.group);
        field(pointer, out, "tag", &self.tag);
        field(pointer, out, "newest_tag", &self.newest_tag);
    }
}

/// Publisher-declared, unverified claim of interface-surface executable names exposed on PATH by
/// this package. Absent means undeclared; an empty array means the publisher asserts zero interface
/// binaries. On Windows the claim reflects the default executable-resolution set
/// (.exe/.com/.bat/.cmd); a customized child PATHEXT may resolve fewer.
pub type Binaries = Vec<String>;

/// A single admitted `binaries`/`entrypoints` claim, attributed to the package that declared it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BinaryAttribution {
    /// The claimed executable or entrypoint name.
    pub name: String,
    /// The declaring package; absent when attribution is unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
}

impl Unknowns for BinaryAttribution {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "name", &self.name);
        field(pointer, out, "package", &self.package);
    }
}

/// One `(group, binding, platform)` pin that moved between the predecessor lock and the candidate.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BindingChange {
    /// Local binding name (the `ocx.toml` key).
    pub name: String,
    /// Owning group — `default` for the top-level `\[tools\]` table.
    pub group: String,
    /// The platform the pin belongs to.
    pub platform: Platform,
    /// The tag the declaration spells; absent when it is digest-pinned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// Pull identifier before the update; absent when newly pinned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<PackageRef>,
    /// Pull identifier after the update; absent when dropped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<PackageRef>,
    /// The release `tag` named before the update (`3` -> `3.28.3`); absent when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_version: Option<String>,
    /// The release `tag` names after the update; absent when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_version: Option<String>,
}

impl Unknowns for BindingChange {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "name", &self.name);
        field(pointer, out, "group", &self.group);
        field(pointer, out, "platform", &self.platform);
        field(pointer, out, "tag", &self.tag);
        field(pointer, out, "from", &self.from);
        field(pointer, out, "to", &self.to);
        field(pointer, out, "from_version", &self.from_version);
        field(pointer, out, "to_version", &self.to_version);
    }
}

/// One `(group, binding, platform)` pin the update left where it was.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BindingState {
    /// Local binding name (the `ocx.toml` key).
    pub name: String,
    /// Owning group — `default` for the top-level `\[tools\]` table.
    pub group: String,
    /// The platform the pin belongs to.
    pub platform: Platform,
    /// The tag the declaration spells; absent when it is digest-pinned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// The unchanged pull identifier — the same `registry/repository@sha256:<hex>` form
    /// `changes\[\].from` / `to` carry, so the two arrays are directly comparable.
    pub identifier: PackageRef,
    /// The release `tag` names (`3` -> `3.28.4`); absent when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl Unknowns for BindingState {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "name", &self.name);
        field(pointer, out, "group", &self.group);
        field(pointer, out, "platform", &self.platform);
        field(pointer, out, "tag", &self.tag);
        field(pointer, out, "identifier", &self.identifier);
        field(pointer, out, "version", &self.version);
    }
}

/// Blob traffic, summed over every platform.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BlobSummary {
    /// Already at the target — nothing transferred.
    pub present: i64,
    /// Mounted from another repository in the same registry.
    pub mounted: i64,
    /// Downloaded from the source and uploaded to the target.
    pub uploaded: i64,
}

impl Unknowns for BlobSummary {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "present", &self.present);
        field(pointer, out, "mounted", &self.mounted);
        field(pointer, out, "uploaded", &self.uploaded);
    }
}

/// Whether this run installed ocx into its own content store.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BootstrapEntry {
    /// What the bootstrap did.
    pub status: BootstrapStatus,
    /// The version pulled, or that a dry run would pull.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Resolved content digest; present when a pinned version produced one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<Digest>,
}

impl Unknowns for BootstrapEntry {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "status", &self.status);
        field(pointer, out, "version", &self.version);
        field(pointer, out, "digest", &self.digest);
    }
}

/// What a bootstrap did.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BootstrapStatus {
    /// The requested ocx was already installed.
    AlreadyPresent,
    /// The requested ocx was pulled.
    Pulled,
    /// Dry run: the requested ocx would be pulled.
    WouldPull,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl BootstrapStatus {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::AlreadyPresent => "already_present",
            Self::Pulled => "pulled",
            Self::WouldPull => "would_pull",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "already_present" => Self::AlreadyPresent,
            "pulled" => Self::Pulled,
            "would_pull" => Self::WouldPull,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for BootstrapStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for BootstrapStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for BootstrapStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for BootstrapStatus {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// Build environment metadata (timestamp, profile, target, rustc).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BuildInfo {
    /// ISO-8601 UTC build timestamp.
    pub timestamp: String,
    /// `"release"` or `"debug"` — derived from the `CARGO_DEBUG` flag.
    pub profile: String,
    /// Target triple the binary was compiled for.
    pub target: String,
    /// `rustc` version that compiled the binary.
    pub rustc: String,
}

impl Unknowns for BuildInfo {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "timestamp", &self.timestamp);
        field(pointer, out, "profile", &self.profile);
        field(pointer, out, "target", &self.target);
        field(pointer, out, "rustc", &self.rustc);
    }
}

/// Bundle metadata format version.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BundleMetadataVersion {
    /// The first bundle metadata format.
    Code1,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(i64),
}

impl BundleMetadataVersion {
    /// The wire value.
    pub fn value(&self) -> i64 {
        match self {
            Self::Code1 => 1,
            Self::Unknown(value) => *value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: i64) -> Self {
        match value {
            1 => Self::Code1,
            _ => Self::Unknown(value),
        }
    }
}

impl Serialize for BundleMetadataVersion {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i64(self.value())
    }
}

impl<'de> Deserialize<'de> for BundleMetadataVersion {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(i64::deserialize(deserializer)?))
    }
}

impl Unknowns for BundleMetadataVersion {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// Size in bytes.
pub type ByteSize = i64;

/// One platform child of an image index, or one locked platform leaf.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CandidateOut {
    /// The candidate manifest's digest.
    pub digest: Digest,
    /// This candidate as a pullable reference — the entry's identifier with this child's digest
    /// attached. Emitted for the same reason the entry carries `pinned_identifier`: splicing one by
    /// hand means knowing where the tag goes relative to the digest.
    pub pinned: PinnedPackageRef,
    /// The platform the candidate serves.
    pub platform: Platform,
    /// Absent for a lock-projected candidate: `ocx.lock` records the leaf digest per platform, not
    /// the descriptor that pointed at it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    /// The candidate manifest's size; absent for a lock-projected candidate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<ByteSize>,
}

impl Unknowns for CandidateOut {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "digest", &self.digest);
        field(pointer, out, "pinned", &self.pinned);
        field(pointer, out, "platform", &self.platform);
        field(pointer, out, "media_type", &self.media_type);
        field(pointer, out, "size", &self.size);
    }
}

/// A write-preflight capability.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Capability {
    /// The local `git` version against the floor the git transport needs.
    GitVersion,
    /// The credential's push permission on the repository being written.
    PushAccess,
    /// Whether the project lets a CI job token push to its repository.
    JobTokenPush,
    /// Whether the index project's job-token allowlist admits the publishing project.
    JobTokenAllowlist,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl Capability {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::GitVersion => "git_version",
            Self::PushAccess => "push_access",
            Self::JobTokenPush => "job_token_push",
            Self::JobTokenAllowlist => "job_token_allowlist",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "git_version" => Self::GitVersion,
            "push_access" => Self::PushAccess,
            "job_token_push" => Self::JobTokenPush,
            "job_token_allowlist" => Self::JobTokenAllowlist,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for Capability {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for Capability {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Capability {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for Capability {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// One row of the write preflight.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CapabilityCheckEntry {
    /// The capability this row reports on.
    pub name: Capability,
    /// How the check came out.
    pub status: CapabilityStatus,
    /// A human-readable qualifier where the forge exposes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Unknowns for CapabilityCheckEntry {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "name", &self.name);
        field(pointer, out, "status", &self.status);
        field(pointer, out, "detail", &self.detail);
    }
}

/// How one capability check came out. There is no `failed`: a check that fails raises an error and
/// no report is rendered.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CapabilityStatus {
    /// The capability was read and is present.
    Passed,
    /// The forge did not let the credential read the capability; never fails the run.
    UnknownValue,
    /// The check does not apply to this run's forge, transport or credential.
    Skipped,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl CapabilityStatus {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Passed => "passed",
            Self::UnknownValue => "unknown",
            Self::Skipped => "skipped",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "passed" => Self::Passed,
            "unknown" => Self::UnknownValue,
            "skipped" => Self::Skipped,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for CapabilityStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for CapabilityStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for CapabilityStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for CapabilityStatus {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// The whole finding set for one package — the value both `check` and `repair` report.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CascadeReport {
    /// The physical repository the graph was read from.
    pub identifier: PackageRef,
    /// The logical name the user asked for, when it differed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logical: Option<PackageRef>,
    /// Per alias, what the registry holds as a whole.
    pub aliases: BTreeMap<String, AliasState>,
    /// Per-slot rows, sorted by (tag, platform). Includes `Ok` rows so a reader sees the whole
    /// graph, not only its damage.
    pub rows: Vec<SlotRow>,
    /// Registry-versus-index findings; always empty for a physical identifier.
    pub index_findings: Vec<IndexFinding>,
    /// Tags excluded from the graph, verbatim.
    pub ignored_tags: Vec<String>,
    /// Findings that need new content published before they can be fixed.
    pub unrepairable: Vec<Unrepairable>,
}

impl Unknowns for CascadeReport {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "identifier", &self.identifier);
        field(pointer, out, "logical", &self.logical);
        field(pointer, out, "aliases", &self.aliases);
        field(pointer, out, "rows", &self.rows);
        field(pointer, out, "index_findings", &self.index_findings);
        field(pointer, out, "ignored_tags", &self.ignored_tags);
        field(pointer, out, "unrepairable", &self.unrepairable);
    }
}

/// Repository catalog listing, optionally including tags per repository.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    /// One entry per repository: in registry order without `--with-tags`, sorted by name with it.
    pub items: Vec<CatalogEntry>,
}

impl Unknowns for Catalog {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "items", &self.items);
    }
}

/// One repository of the catalog.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CatalogEntry {
    /// The repository name.
    pub repository: String,
    /// The repository's tags, sorted; present only when tags were requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

impl Unknowns for CatalogEntry {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "repository", &self.repository);
        field(pointer, out, "tags", &self.tags);
    }
}

/// What `ocx index sync --dry-run` would refresh, per registry, in argument order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CatalogPreview {
    /// One entry per registry, in argument order.
    pub items: Vec<CatalogPreviewEntry>,
}

impl Unknowns for CatalogPreview {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "items", &self.items);
    }
}

/// One registry's enumerated package set.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CatalogPreviewEntry {
    /// The registry the packages were enumerated from.
    pub registry: RegistryHost,
    /// The packages a sync would refresh, sorted.
    pub packages: Vec<String>,
}

impl Unknowns for CatalogPreviewEntry {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "registry", &self.registry);
        field(pointer, out, "packages", &self.packages);
    }
}

/// One blob in the resolution chain. Same descriptor surface as a layer (digest, media type, size)
/// plus the OCI `role` so a consumer can tell the index from the manifest from the config without
/// decoding digests.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChainOut {
    /// The blob's digest.
    pub digest: Digest,
    /// What the blob is in the walk: `index`, `manifest` or `config`.
    pub role: String,
    /// The blob's media type.
    pub media_type: String,
    /// The blob's size; absent when it is unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<ByteSize>,
}

impl Unknowns for ChainOut {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "digest", &self.digest);
        field(pointer, out, "role", &self.role);
        field(pointer, out, "media_type", &self.media_type);
        field(pointer, out, "size", &self.size);
    }
}

/// GitHub Actions context. Present only when built under a GitHub workflow that exported the
/// standard `GITHUB_*` env vars.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CiInfo {
    /// The CI system (`github-actions`).
    pub provider: String,
    /// Direct link to the run that produced this binary. Composed as
    /// `{server_url}/{repository}/actions/runs/{run_id}`.
    pub run_url: String,
    /// The workflow name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    /// The git ref the run built (`refs/tags/v1.2.3`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<String>,
    /// The commit SHA the run built.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
}

impl Unknowns for CiInfo {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "provider", &self.provider);
        field(pointer, out, "run_url", &self.run_url);
        field(pointer, out, "workflow", &self.workflow);
        field(pointer, out, "ref", &self.r#ref);
        field(pointer, out, "sha", &self.sha);
    }
}

/// Result of a successful `ocx package claim`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClaimReport {
    /// The claimed `<namespace>/<package>` identifier, as given.
    pub package: String,
    /// The logical name written into the root.
    pub name: String,
    /// `unchanged` when the open claim branch already carries a byte-identical root; `updated`
    /// otherwise, which `--output` always is.
    pub status: WriteStatus,
    /// The resolved forge kind, after `--forge`.
    pub forge: Forge,
    /// The selected write transport.
    pub transport: Transport,
    /// The API credential's kind. `job_token` only when the credential is this environment's own
    /// `CI_JOB_TOKEN` — ocx cannot tell a personal from a project, group or OAuth token, so it
    /// reports no kind it cannot observe.
    pub credential_kind: CredentialKind,
    /// The push credential's kind; absent under the `api` transport, which pushes nothing.
    /// `git_helper` means nothing was injected and git's own credential helpers authenticated the
    /// push.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push_credential_kind: Option<PushCredentialKind>,
    /// The identity that authored the request; absent when none is available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<OwnerEntry>,
    /// Which rule produced `author`: `resolved` or `ci_environment`; absent exactly when `author`
    /// is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_identity_source: Option<IdentitySource>,
    /// The resolved owner list written into the root, in the order given.
    pub owners: Vec<OwnerEntry>,
    /// Which rule produced `owners`: `resolved`, `asserted` or `ci_environment`.
    pub owner_identity_source: IdentitySource,
    /// The claim branch, so a script need not re-derive the naming convention. Always present,
    /// `--output` included: the name is derived from the package, not read from the forge, so a run
    /// that opens no request still reports the branch a later run would use.
    pub branch: String,
    /// The opened or updated request's web URL; absent when the run opened none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request_url: Option<String>,
    /// The opened or updated request's number; absent when the run opened none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request_number: Option<i64>,
    /// The verified fork, as `namespace/project`; absent on the direct path and under the `git`
    /// transport.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork: Option<String>,
    /// The relative paths written under the `--output` directory; empty otherwise.
    pub written_paths: Vec<String>,
    /// Every preflight row, including the ones that did not apply.
    pub capability_checks: Vec<CapabilityCheckEntry>,
}

impl Unknowns for ClaimReport {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "package", &self.package);
        field(pointer, out, "name", &self.name);
        field(pointer, out, "status", &self.status);
        field(pointer, out, "forge", &self.forge);
        field(pointer, out, "transport", &self.transport);
        field(pointer, out, "credential_kind", &self.credential_kind);
        field(pointer, out, "push_credential_kind", &self.push_credential_kind);
        field(pointer, out, "author", &self.author);
        field(pointer, out, "author_identity_source", &self.author_identity_source);
        field(pointer, out, "owners", &self.owners);
        field(pointer, out, "owner_identity_source", &self.owner_identity_source);
        field(pointer, out, "branch", &self.branch);
        field(pointer, out, "pull_request_url", &self.pull_request_url);
        field(pointer, out, "pull_request_number", &self.pull_request_number);
        field(pointer, out, "fork", &self.fork);
        field(pointer, out, "written_paths", &self.written_paths);
        field(pointer, out, "capability_checks", &self.capability_checks);
    }
}

/// Objects, temp directories and consent stamps a clean removed, or would remove in a dry run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Clean {
    /// One entry per resource: objects, then temp directories, then consent stamps.
    pub items: Vec<CleanEntry>,
}

impl Unknowns for Clean {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "items", &self.items);
    }
}

/// A single cleaned-up resource entry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CleanEntry {
    /// What kind of resource this is.
    pub kind: CleanKind,
    /// Whether the run was a dry run, so nothing was removed.
    pub dry_run: bool,
    /// Where the resource is, or was.
    pub path: String,
    /// Project `ocx.lock` paths holding this entry. Empty when the entry is not protected by any
    /// registered project, or when `--force` was specified.
    pub held_by: Vec<String>,
}

impl Unknowns for CleanEntry {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "kind", &self.kind);
        field(pointer, out, "dry_run", &self.dry_run);
        field(pointer, out, "path", &self.path);
        field(pointer, out, "held_by", &self.held_by);
    }
}

/// The kind of resource cleaned up.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CleanKind {
    /// An unreferenced package in the object store.
    Object,
    /// A leftover temporary directory.
    Temp,
    /// A `state/projects/<key>/` directory whose consent stamp was swept.
    Consent,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl CleanKind {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Object => "object",
            Self::Temp => "temp",
            Self::Consent => "consent",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "object" => Self::Object,
            "temp" => Self::Temp,
            "consent" => Self::Consent,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for CleanKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for CleanKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for CleanKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for CleanKind {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// One transitive dependency in the `--closure` object, in transitive-closure order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClosureDepOut {
    /// Short display name — the repository's final path segment (e.g. `deps-mid`). The flat plain
    /// tree labels each dep by this.
    pub name: String,
    /// Always digest-pinned — a closure node is a resolved artifact, never a tag. There is no
    /// separate `digest` key because this one already ends in it.
    pub identifier: PinnedPackageRef,
    /// The visibility composed from the root down to this dependency.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_visibility: Option<Visibility>,
    /// Tri-state, mirrors `Bundle.binaries`: key absent = undeclared, `Some(empty)` = publisher
    /// asserts zero interface executables.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binaries: Option<Vec<String>>,
    /// The dep's declared entrypoint names.
    pub entrypoints: Vec<String>,
    /// The dep's own declared integration namespace keys, lexicographically ordered. Keys only —
    /// a closure node is not installed, so `${installPath}` has no value and an interpolated
    /// payload would be a half-truth. Always present, `\[\]` when the dep declares none: absent and
    /// empty are the same state here (unlike `binaries`' tri-state).
    pub integrations: Vec<String>,
    /// The dep's own declared dependency edges (as authored) — lets a consumer rebuild the DAG
    /// from the flat list.
    pub dependencies: Vec<ClosureEdgeOut>,
}

impl Unknowns for ClosureDepOut {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "name", &self.name);
        field(pointer, out, "identifier", &self.identifier);
        field(pointer, out, "effective_visibility", &self.effective_visibility);
        field(pointer, out, "binaries", &self.binaries);
        field(pointer, out, "entrypoints", &self.entrypoints);
        field(pointer, out, "integrations", &self.integrations);
        field(pointer, out, "dependencies", &self.dependencies);
    }
}

/// A declared dependency edge (as authored) of one closure dependency.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClosureEdgeOut {
    /// The dependency, digest-pinned.
    pub identifier: PinnedPackageRef,
    /// The visibility the edge declares.
    pub visibility: Visibility,
    /// The dependency's name as the edge declares it.
    pub name: String,
}

impl Unknowns for ClosureEdgeOut {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "identifier", &self.identifier);
        field(pointer, out, "visibility", &self.visibility);
        field(pointer, out, "name", &self.name);
    }
}

/// The dependency closure emitted with `--closure`. Everything nests under one object: `deps` (the
/// transitive dependencies in transitive-closure order), `surface` (the interface + private
/// projections), and interface-projection `conflicts`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClosureOut {
    /// Transitive dependencies in transitive-closure order (deps before dependents). The inspected
    /// root is NOT listed here — it is named by the top-level `identifier` and appears in each
    /// surface's attributions.
    pub deps: Vec<ClosureDepOut>,
    /// What would land on each axis if the root were installed.
    pub surface: SurfacesOut,
    /// Conditions on the interface projection that install would refuse.
    pub conflicts: ConflictsOut,
}

impl Unknowns for ClosureOut {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "deps", &self.deps);
        field(pointer, out, "surface", &self.surface);
        field(pointer, out, "conflicts", &self.conflicts);
    }
}

/// Git commit metadata baked at build time.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CommitInfo {
    /// Full 40-character SHA-1.
    pub sha: String,
    /// 8-character abbreviated SHA — convenience for humans.
    pub short: String,
    /// `git describe --tags --dirty` output, including any `-dirty` suffix.
    pub describe: String,
    /// `true` if the working tree had uncommitted changes when built.
    pub dirty: bool,
    /// ISO-8601 commit timestamp (author date).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

impl Unknowns for CommitInfo {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "sha", &self.sha);
        field(pointer, out, "short", &self.short);
        field(pointer, out, "describe", &self.describe);
        field(pointer, out, "dirty", &self.dirty);
        field(pointer, out, "timestamp", &self.timestamp);
    }
}

/// Report of `ocx config setup`: the outcome of adopting the managed config.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConfigSetupData {
    /// The adoption outcome, in the entry shape `ocx self setup` reports.
    pub managed_config: ManagedConfigEntry,
}

impl Unknowns for ConfigSetupData {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "managed_config", &self.managed_config);
    }
}

/// CLI report for `ocx config test` — a candidate managed-config payload validated locally, plus
/// the configuration adopting it would produce.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConfigTestData {
    /// The candidate file this report describes.
    pub candidate: String,
    /// Always `true`: an invalid payload fails the command (exit 78) and produces no report.
    /// Carried so a JSON consumer reads the verdict from the document rather than inferring it from
    /// the exit code.
    pub valid: bool,
    /// Effective `\[registry\] default`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry_default: Option<String>,
    /// Effective `\[registries.<name>\]` keys, sorted.
    pub registries: Vec<String>,
    /// Effective `\[mirrors."<host>"\]` keys, sorted.
    pub mirrors: Vec<String>,
    /// Hosts this machine would contact over plain HTTP once the candidate is adopted, sorted —
    /// the candidate's own `\[registries.<name>\] insecure` entries unioned with this machine's
    /// `OCX_INSECURE_REGISTRIES`, less anything the system scope locked shut.
    pub plain_http: Vec<String>,
    /// Effective `\[patches\]` tier, defaults applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patches: Option<PatchesView>,
    /// The machine's `\[managed\]` tier posture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed: Option<ManagedView>,
    /// Dotted paths of keys the config schema ignores, sorted. Advisory: the loader ignores unknown
    /// keys by design, so these are equally typos and settings a newer ocx understands.
    pub unknown_keys: Vec<String>,
}

impl Unknowns for ConfigTestData {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "candidate", &self.candidate);
        field(pointer, out, "valid", &self.valid);
        field(pointer, out, "registry_default", &self.registry_default);
        field(pointer, out, "registries", &self.registries);
        field(pointer, out, "mirrors", &self.mirrors);
        field(pointer, out, "plain_http", &self.plain_http);
        field(pointer, out, "patches", &self.patches);
        field(pointer, out, "managed", &self.managed);
        field(pointer, out, "unknown_keys", &self.unknown_keys);
    }
}

/// CLI report for `ocx config update` — both the full-update path and the `--check` probe-only
/// path (mirrors `ocx self update --check`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConfigUpdateData {
    /// What the run found or did.
    pub status: ConfigUpdateStatus,
    /// The effective managed-config source (flag > env > seed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The local snapshot's manifest digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<Digest>,
    /// When the snapshot was last fetched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<Timestamp>,
    /// The tier's refresh policy (`apply` / `notify` / `manual`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<String>,
    /// Active kill switches (e.g. `OCX_NO_CONFIG_REFRESH`), by env-var name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kill_switches: Option<Vec<String>>,
    /// Whether the registry's current digest differs from the local snapshot; present only when
    /// reachable (`--check`, online).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drift: Option<bool>,
    /// The tag the local snapshot was fetched under (snapshot v2 bookkeeping — shows which
    /// floating/pinned tag the tier tracks).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// The instant until which the background tick is paused (`ocx config update --pause`); absent
    /// when no pause is in force.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_ends_at: Option<Timestamp>,
    /// The version spec pinned alongside an in-force pause (`--pause <d> <VERSION>`); absent when
    /// the pause carries no pin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<String>,
}

impl Unknowns for ConfigUpdateData {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "status", &self.status);
        field(pointer, out, "source", &self.source);
        field(pointer, out, "digest", &self.digest);
        field(pointer, out, "fetched_at", &self.fetched_at);
        field(pointer, out, "policy", &self.policy);
        field(pointer, out, "kill_switches", &self.kill_switches);
        field(pointer, out, "drift", &self.drift);
        field(pointer, out, "tag", &self.tag);
        field(pointer, out, "pause_ends_at", &self.pause_ends_at);
        field(pointer, out, "pinned", &self.pinned);
    }
}

/// Top-level `status` of the `ocx config update` report.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ConfigUpdateStatus {
    /// No managed-config tier is configured.
    NotConfigured,
    /// The registry was probed and its digest matches the local snapshot — verified current.
    /// Reported only when the probe actually ran.
    AlreadyCurrent,
    /// A new snapshot was fetched and persisted (full-update path only).
    Updated,
    /// A probe-only report (`--check`) that ran and detected drift — no swap was attempted.
    Checked,
    /// A probe-only report (`--check`) whose registry probe could NOT run — offline, no client,
    /// source absent, auth failure, or a registry error. Distinct from `already_current` so
    /// operators can tell "verified current" apart from "couldn't check" — the report degrades to
    /// local-state only (source/digest/fetched-at, no live drift).
    CheckUnavailable,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl ConfigUpdateStatus {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::NotConfigured => "not_configured",
            Self::AlreadyCurrent => "already_current",
            Self::Updated => "updated",
            Self::Checked => "checked",
            Self::CheckUnavailable => "check_unavailable",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "not_configured" => Self::NotConfigured,
            "already_current" => Self::AlreadyCurrent,
            "updated" => Self::Updated,
            "checked" => Self::Checked,
            "check_unavailable" => Self::CheckUnavailable,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for ConfigUpdateStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for ConfigUpdateStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ConfigUpdateStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for ConfigUpdateStatus {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// Install/compose-gate conditions detected over the interface projection. Both arrays always
/// present; empty means the surface is realizable. Inspect stays a view, not a gate — exit 0
/// either way.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConflictsOut {
    /// Entrypoint names claimed by more than one package.
    pub entrypoints: Vec<EntrypointConflictOut>,
    /// Repositories resolved to more than one digest.
    pub repositories: Vec<RepositoryConflictOut>,
}

impl Unknowns for ConflictsOut {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "entrypoints", &self.entrypoints);
        field(pointer, out, "repositories", &self.repositories);
    }
}

/// The version of every gated machine document, so a caller can refuse a command before running it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContractVersions {
    /// The error document's `schema_version`.
    pub errors: i64,
    /// Each command's contract version, keyed by its path below `ocx`, space-separated.
    pub commands: BTreeMap<String, i64>,
    /// Each `--format json` root's `schema_version`, keyed by the root name the command grammar
    /// uses.
    pub reports: BTreeMap<String, i64>,
}

impl Unknowns for ContractVersions {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "errors", &self.errors);
        field(pointer, out, "commands", &self.commands);
        field(pointer, out, "reports", &self.reports);
    }
}

/// What became of one platform.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CopiedPlatformRow {
    /// The platform this manifest serves.
    pub platform: Platform,
    /// The leaf digest the target serves for this platform. For a `kept_not_in_source` row this is
    /// the digest it already had.
    pub digest: Digest,
    /// Typed, so JSON carries `added` / `unchanged` / `replaced` / `kept_not_in_source` while the
    /// table renders the prose.
    pub disposition: Disposition,
}

impl Unknowns for CopiedPlatformRow {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "platform", &self.platform);
        field(pointer, out, "digest", &self.digest);
        field(pointer, out, "disposition", &self.disposition);
    }
}

/// Result of `ocx package copy`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CopyReport {
    /// The source reference as given.
    pub source: String,
    /// The resolved target reference.
    pub target: String,
    /// `copied`, or `planned` under `--dry-run`.
    pub status: CopyStatus,
    /// One row per platform manifest the target offers after this copy, including any it already
    /// had that the source does not ship.
    pub manifests: Vec<CopiedPlatformRow>,
    /// Rolling tags written in addition to the target's own tag.
    pub cascade_tags_written: Vec<String>,
    /// Digest-named `__ocx.keep.<algorithm>-<hex>` tags written, one per distinct manifest.
    pub keep_tags_written: Vec<String>,
    /// Referrer manifests carried over — signatures, SBOMs, attestations.
    pub referrers_copied: i64,
    /// cosign `<algorithm>-<hex>.{sig,att,sbom}` sidecar tags carried over.
    pub sidecars_copied: i64,
    /// Sidecar tags the target already held under a different manifest, and this copy therefore
    /// refused to overwrite. Non-empty means exit 65.
    pub sidecar_conflicts: Vec<String>,
    /// Blob traffic, summed over every platform.
    pub blobs: BlobSummary,
    /// What became of the repository description; absent when `--with-description` was not passed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<DescriptionOutcome>,
}

impl Unknowns for CopyReport {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "source", &self.source);
        field(pointer, out, "target", &self.target);
        field(pointer, out, "status", &self.status);
        field(pointer, out, "manifests", &self.manifests);
        field(pointer, out, "cascade_tags_written", &self.cascade_tags_written);
        field(pointer, out, "keep_tags_written", &self.keep_tags_written);
        field(pointer, out, "referrers_copied", &self.referrers_copied);
        field(pointer, out, "sidecars_copied", &self.sidecars_copied);
        field(pointer, out, "sidecar_conflicts", &self.sidecar_conflicts);
        field(pointer, out, "blobs", &self.blobs);
        field(pointer, out, "description", &self.description);
    }
}

/// Whether this run copied or only planned.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CopyStatus {
    /// The copy was written to the target.
    Copied,
    /// `--dry-run`: the copy was planned and nothing was written.
    Planned,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl CopyStatus {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Copied => "copied",
            Self::Planned => "planned",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "copied" => Self::Copied,
            "planned" => Self::Planned,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for CopyStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for CopyStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for CopyStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for CopyStatus {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// The API credential's kind.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CredentialKind {
    /// The credential is this environment's own `CI_JOB_TOKEN`.
    JobToken,
    /// A credential ocx holds but cannot classify further.
    Token,
    /// The ladder resolved nothing — the unauthenticated `--output` path.
    None,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl CredentialKind {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::JobToken => "job_token",
            Self::Token => "token",
            Self::None => "none",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "job_token" => Self::JobToken,
            "token" => Self::Token,
            "none" => Self::None,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for CredentialKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for CredentialKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for CredentialKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for CredentialKind {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// Ordered list of package dependencies; array position is the environment import order.
pub type Dependencies = Vec<Dependency>;

/// Why view — all paths from roots to a target dependency.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DependenciesTrace {
    /// Every path from a requested root to the target, root first.
    pub paths: Vec<Vec<PackageRef>>,
    /// Why no path was found; present only when `paths` is empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl Unknowns for DependenciesTrace {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "paths", &self.paths);
        field(pointer, out, "message", &self.message);
    }
}

/// A pinned dependency descriptor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Dependency {
    /// Fully qualified pinned OCX identifier with required explicit registry and digest. The tag
    /// portion is advisory (for update tooling) — only the digest is used for resolution.
    pub identifier: PinnedPackageRef,
    /// Controls how this dependency's environment variables propagate. Default: `sealed` — no env
    /// contribution. One of four levels: `sealed`, `private`, `public`, `interface`.
    pub visibility: Visibility,
    /// Optional name for this dependency used in `${deps.NAME.installPath}` interpolation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<DependencyName>,
}

impl Unknowns for Dependency {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "identifier", &self.identifier);
        field(pointer, out, "visibility", &self.visibility);
        field(pointer, out, "name", &self.name);
    }
}

/// Interpolation name for this dependency. Must match ^\[a-z0-9\]\[a-z0-9_-\]*$ (max 64 chars).
pub type DependencyName = String;

/// A node in the dependency tree (for tree view output).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DependencyNode {
    /// The package, digest-pinned.
    pub identifier: PackageRef,
    /// Whether this package already appeared earlier in the tree; its dependencies are listed
    /// there.
    pub repeated: bool,
    /// The visibility the parent declared for this edge, not the propagated result; absent on a
    /// root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<Visibility>,
    /// This package's own dependencies.
    pub dependencies: Vec<DependencyNode>,
}

impl Unknowns for DependencyNode {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "identifier", &self.identifier);
        field(pointer, out, "repeated", &self.repeated);
        field(pointer, out, "visibility", &self.visibility);
        field(pointer, out, "dependencies", &self.dependencies);
    }
}

/// Tree view of the dependency graph (default output).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DependencyTree {
    /// One tree per requested package, in request order.
    pub items: Vec<DependencyNode>,
}

impl Unknowns for DependencyTree {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "items", &self.items);
    }
}

/// What became of the repository description.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DescriptionOutcome {
    /// Pulled from the source and pushed to the target.
    Copied,
    /// The source publishes none, so there was nothing to copy. Not a failure: a description is
    /// repository-level prose and legitimately absent.
    Absent,
    /// `--dry-run` was in force, so the description was not read or written.
    SkippedDryRun,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl DescriptionOutcome {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Copied => "copied",
            Self::Absent => "absent",
            Self::SkippedDryRun => "skipped_dry_run",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "copied" => Self::Copied,
            "absent" => Self::Absent,
            "skipped_dry_run" => Self::SkippedDryRun,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for DescriptionOutcome {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for DescriptionOutcome {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for DescriptionOutcome {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for DescriptionOutcome {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// OCI content-addressed digest (e.g. 'sha256:abcdef...').
pub type Digest = String;

/// Where a signature referrer was discovered.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DiscoveryMethod {
    /// `GET /v2/<name>/referrers/<digest>` — the OCI 1.1 Referrers API.
    ReferrersApi,
    /// The `sha256-<hex>` fallback referrers index a registry without the Referrers API gets
    /// instead.
    FallbackTag,
    /// The cosign `sha256-<hex>.sig` / `.att` / `.sbom` sidecar tag.
    SidecarTag,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl DiscoveryMethod {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::ReferrersApi => "referrers_api",
            Self::FallbackTag => "fallback_tag",
            Self::SidecarTag => "sidecar_tag",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "referrers_api" => Self::ReferrersApi,
            "fallback_tag" => Self::FallbackTag,
            "sidecar_tag" => Self::SidecarTag,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for DiscoveryMethod {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for DiscoveryMethod {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for DiscoveryMethod {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for DiscoveryMethod {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// What became of one platform at the target.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Disposition {
    /// The target's index had no entry for this platform.
    Added,
    /// The target already pointed at this exact digest — nothing to do.
    Unchanged,
    /// The target pointed at a different digest for this platform.
    Replaced,
    /// The target offers this platform and the source does not, so the merge leaves it alone.
    KeptNotInSource,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl Disposition {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Added => "added",
            Self::Unchanged => "unchanged",
            Self::Replaced => "replaced",
            Self::KeptNotInSource => "kept_not_in_source",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "added" => Self::Added,
            "unchanged" => Self::Unchanged,
            "replaced" => Self::Replaced,
            "kept_not_in_source" => Self::KeptNotInSource,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for Disposition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for Disposition {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Disposition {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for Disposition {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// A single dry-run preview row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DryRunEntry {
    /// The locked tool's pinned identifier.
    pub package: PinnedPackageRef,
    /// Whether the tool is cached or would be fetched.
    pub status: PullStatus,
    /// The package root; absent when nothing is materialised yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl Unknowns for DryRunEntry {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "package", &self.package);
        field(pointer, out, "status", &self.status);
        field(pointer, out, "path", &self.path);
    }
}

/// Origin of a resolved environment variable entry, shown under `--show-patches`.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum EntrySource {
    /// Declared by the package itself.
    Package,
    /// Contributed by a companion patch overlay.
    Patch {
        /// The descriptor rule glob that admitted the companion for the base.
        rule: String,
        /// The identifier of the companion that produced the entry.
        companion: String,
    },
    /// A variant this SDK was not generated for: the whole object, as sent.
    Unknown(Value),
}

impl Serialize for EntrySource {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Package => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "package")?;
                object.end()
            }
            Self::Patch {
                rule: field_0,
                companion: field_1,
            } => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "patch")?;
                object.serialize_entry("rule", field_0)?;
                object.serialize_entry("companion", field_1)?;
                object.end()
            }
            Self::Unknown(value) => value.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for EntrySource {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let Some(tag) = value.get("type").and_then(Value::as_str) else {
            return Err(D::Error::custom("a union object needs a string `type`"));
        };
        match tag {
            "package" => Ok(Self::Package),
            "patch" => {
                #[derive(Deserialize)]
                struct Fields {
                    rule: String,
                    companion: String,
                }
                let fields = payload::<Fields, D::Error>("patch", value)?;
                Ok(Self::Patch {
                    rule: fields.rule,
                    companion: fields.companion,
                })
            }
            _ => Ok(Self::Unknown(value)),
        }
    }
}

impl Unknowns for EntrySource {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        match self {
            Self::Package => {}
            Self::Patch {
                rule: field_0,
                companion: field_1,
            } => {
                field(pointer, out, "rule", field_0);
                field(pointer, out, "companion", field_1);
            }
            Self::Unknown(_) => out.push(pointer.clone()),
        }
    }
}

/// A single named entrypoint for a package.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entrypoint {
    /// Dispatch target resolved on the composed `PATH`, when it differs from the invocable name.
    /// Absent means the entrypoint name *is* the command (the common case): a package may expose
    /// `hello` while dispatching a differently named binary such as `hello-bin`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<EntrypointName>,
    /// Fixed leading arguments the generated launcher prepends before the user's own arguments.
    /// Each element may carry `${installPath}` — or its alias `${self.installPath}` —
    /// optionally suffixed `:native` or `:posix`; `${deps.*}` and `${self.env.*}` are NOT permitted
    /// here, and every other `${...}` is rejected (write `$${` for a literal `${`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
}

impl Unknowns for Entrypoint {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "command", &self.command);
        field(pointer, out, "args", &self.args);
    }
}

/// Two or more interface-admitted closure nodes declare the same entrypoint name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EntrypointConflictOut {
    /// The contested entrypoint name.
    pub name: String,
    /// Every package claiming it.
    pub packages: Vec<String>,
}

impl Unknowns for EntrypointConflictOut {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "name", &self.name);
        field(pointer, out, "packages", &self.packages);
    }
}

/// Entrypoint name for invocation by users. Must match ^\[a-z0-9\]\[a-z0-9_-\]*$ and be at most 64
/// characters.
pub type EntrypointName = String;

/// Map of entrypoint names to entrypoint definitions. Each key is the user-invokable command name;
/// the value object carries an optional `command` field naming the binary the generated launcher
/// dispatches to when it differs from the invokable name (omit it and the name is dispatched
/// directly).
pub type Entrypoints = BTreeMap<String, Entrypoint>;

/// A package's declared environment variables, in declaration order.
pub type Env = Vec<Var>;

/// A single resolved environment variable entry, tagged with its modifier kind.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EnvEntry {
    /// The environment variable name.
    pub key: String,
    /// The resolved value this entry contributes.
    pub value: String,
    /// How the value combines with the variable's existing value.
    pub kind: ModifierKind,
    /// The separator a `list` entry folds with; absent on every other kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub separator: Option<String>,
    /// Origin annotation under `--show-patches`: absent for a package-native entry, the patch
    /// provenance object for a companion overlay entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<EntrySource>,
}

impl Unknowns for EnvEntry {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "key", &self.key);
        field(pointer, out, "value", &self.value);
        field(pointer, out, "kind", &self.kind);
        field(pointer, out, "separator", &self.separator);
        field(pointer, out, "source", &self.source);
    }
}

/// One declared environment value, normalized: `kind` and `value` are always emitted.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EnvValueOut {
    /// How the value combines with the variable's existing value.
    pub kind: ModifierKind,
    /// The declared separator for a `list`-typed value. `None` for every other kind, and for a
    /// `list` that declared none — the project surface may omit it, and what the omission
    /// inherits is decided at compose time, which status does not do. Skipped in JSON when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub separator: Option<String>,
    /// Verbatim as written: a relative `path` value stays relative. `ocx inspect` and `ocx env`
    /// resolve it against the project root.
    pub value: String,
}

impl Unknowns for EnvValueOut {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "kind", &self.kind);
        field(pointer, out, "separator", &self.separator);
        field(pointer, out, "value", &self.value);
    }
}

/// One env key exposed on the interface surface, attributed to the package that declares it. No
/// value: values are `${installPath}`-templated and only concrete after install.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EnvVarAttribution {
    /// The environment variable name.
    pub key: String,
    /// How the value combines with the variable's existing value.
    pub kind: ModifierKind,
    /// The declared separator for a `list`-kind entry; `None` for every other kind. Skipped in JSON
    /// when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub separator: Option<String>,
    /// The declaring package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
}

impl Unknowns for EnvVarAttribution {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "key", &self.key);
        field(pointer, out, "kind", &self.kind);
        field(pointer, out, "separator", &self.separator);
        field(pointer, out, "package", &self.package);
    }
}

/// Resolved environment variables for one or more packages, in declaration order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EnvVars {
    /// The resolved entries, in application order.
    pub items: Vec<EnvEntry>,
    /// Admitted `binaries` claims, attributed to their packages.
    pub binaries: Vec<BinaryAttribution>,
    /// Admitted `entrypoints` claims, attributed to their packages.
    pub entrypoints: Vec<BinaryAttribution>,
    /// Admitted integration payloads, one per (package, namespace) pair.
    pub integrations: Vec<IntegrationAttribution>,
    /// Advisories raised for the **deferred** tools in this composition — always present, empty
    /// whenever nothing was deferred. Warning-only.
    pub advisories: Vec<LazyAdvisoryReport>,
}

impl Unknowns for EnvVars {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "items", &self.items);
        field(pointer, out, "binaries", &self.binaries);
        field(pointer, out, "entrypoints", &self.entrypoints);
        field(pointer, out, "integrations", &self.integrations);
        field(pointer, out, "advisories", &self.advisories);
    }
}

/// The `error` object of an error document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ErrorBody {
    /// Coarse error category.
    pub kind: ErrorCategory,
    /// The specific error within its category (e.g., `"oidc_token_rejected"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<ErrorDetail>,
    /// Full user-facing message.
    pub message: String,
    /// The packages the failure is about; always emitted, possibly empty.
    pub context: ErrorContext,
}

impl Unknowns for ErrorBody {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "kind", &self.kind);
        field(pointer, out, "detail", &self.detail);
        field(pointer, out, "message", &self.message);
        field(pointer, out, "context", &self.context);
    }
}

/// The coarse class of an error.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorCategory {
    /// The command line is invalid
    UsageError,
    /// Configuration is invalid or incomplete
    ConfigError,
    /// Input data is malformed or fails verification
    DataError,
    /// Authentication failed or credentials are missing
    AuthError,
    /// The operation was refused: a permission or a local policy
    PermissionDenied,
    /// A named package, tag, file or resource does not exist
    NotFound,
    /// A required service is unavailable and rerunning will not help
    Unavailable,
    /// A transient failure; the same command may succeed on retry
    TempFail,
    /// The operation as requested is not supported or not enabled by this registry, forge or build;
    /// retrying will not help
    Unsupported,
    /// A filesystem read or write failed
    IoError,
    /// A failure with no more specific category
    Internal,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl ErrorCategory {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::UsageError => "usage_error",
            Self::ConfigError => "config_error",
            Self::DataError => "data_error",
            Self::AuthError => "auth_error",
            Self::PermissionDenied => "permission_denied",
            Self::NotFound => "not_found",
            Self::Unavailable => "unavailable",
            Self::TempFail => "temp_fail",
            Self::Unsupported => "unsupported",
            Self::IoError => "io_error",
            Self::Internal => "internal",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "usage_error" => Self::UsageError,
            "config_error" => Self::ConfigError,
            "data_error" => Self::DataError,
            "auth_error" => Self::AuthError,
            "permission_denied" => Self::PermissionDenied,
            "not_found" => Self::NotFound,
            "unavailable" => Self::Unavailable,
            "temp_fail" => Self::TempFail,
            "unsupported" => Self::Unsupported,
            "io_error" => Self::IoError,
            "internal" => Self::Internal,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for ErrorCategory {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for ErrorCategory {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ErrorCategory {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for ErrorCategory {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// The packages an error is about.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ErrorContext {
    /// The package a sign or verify failure is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identifier: Option<PackageRef>,
    /// The package a copy failure reads from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<PackageRef>,
    /// The repository a copy failure writes to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<PackageRef>,
}

impl Unknowns for ErrorContext {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "identifier", &self.identifier);
        field(pointer, out, "source", &self.source);
        field(pointer, out, "target", &self.target);
    }
}

/// The specific error within its class.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorDetail {
    /// Shell activation failed with an unclassified cause
    ActivationFailed,
    /// A libc-agnostic platform ships a binary that needs a libc
    AgnosticPlatformLibcClaim,
    /// Both extra_ca_certs and extra_ca_certs_pem are declared
    AmbiguousExtraCaCerts,
    /// ocx.lock pins several platforms this host could run, none preferred
    AmbiguousHostLeaf,
    /// The payload declares both trusted_root and trusted_root_json
    AmbiguousTrustRoot,
    /// Announcing failed with an unclassified cause
    AnnounceFailed,
    /// A dependency pinned as `any` is not published for any platform
    AnyPinNotAdvertisedAsAny,
    /// Verifying an `any` pin's provenance failed with an unclassified cause
    AnyPinProvenanceUnavailable,
    /// An archive entry path escapes the extraction root
    ArchiveEntryEscape,
    /// Extraction exceeded the decompressed-size cap
    ArchiveExtractionCapExceeded,
    /// An archive entry uses the unsupported GNU sparse format
    ArchiveGnuSparseUnsupported,
    /// An archive hard link does not resolve inside the extraction root
    ArchiveHardLinkEscape,
    /// An internal archive operation failed
    ArchiveInternal,
    /// Reading or writing an archive entry failed
    ArchiveIo,
    /// An archive symlink points outside the extraction root
    ArchiveSymlinkEscape,
    /// The tar stream is malformed
    ArchiveTarInvalid,
    /// The archive format is not supported
    ArchiveUnsupportedFormat,
    /// The zip archive is malformed
    ArchiveZipInvalid,
    /// The attestations together exceed the total size limit
    AttestationBudgetExhausted,
    /// No verified attestation matches the requested predicate type
    AttestationNotFound,
    /// The attestation statement exceeds its size limit
    AttestationPayloadTooLarge,
    /// The attestation envelope exceeds its size limit
    AttestationTooLarge,
    /// An environment variable the authentication type needs is not set
    AuthEnvMissing,
    /// Scanning for interface binaries failed with an unclassified cause
    BinScanFailed,
    /// This host cannot scan for the target platform's executables
    BinScanUnsupportedHost,
    /// A binding with this name already exists
    BindingAlreadyExists,
    /// A binding exists in several groups and no group was named
    BindingAmbiguous,
    /// The named binding is not declared in ocx.toml
    BindingNotDeclared,
    /// The named binding does not exist in ocx.toml
    BindingNotFound,
    /// A binding's value is not a valid identifier
    BindingValueInvalid,
    /// A binding's identifier names no registry
    BindingValueMissingRegistry,
    /// The registry has no such blob
    BlobNotFound,
    /// The acting identity is a bot account
    BotIdentity,
    /// The provenance builder does not match the pinned builder
    BuilderMismatch,
    /// The signature bundle could not be parsed
    BundleParseFailed,
    /// The examination cap was reached with candidates unexamined and none passing
    CandidateLimitExhausted,
    /// The index source serves no catalog document
    CatalogDocumentAbsent,
    /// The certificate chain does not verify against the trust root
    CertChainInvalid,
    /// The log entry time falls outside the certificate validity window
    CertificateValidityWindow,
    /// Writing the CI export stream failed
    CiExportWrite,
    /// Writing a CI export file failed
    CiFileWrite,
    /// A CI environment variable the export needs is not set
    CiMissingEnv,
    /// A registry operation failed with an unclassified cause
    ClientInternal,
    /// Reading or writing a local file during a registry operation failed
    ClientIo,
    /// A registry document could not be serialized or parsed
    ClientSerialization,
    /// The command resolves to a file that is not executable
    CommandNotExecutable,
    /// The command does not resolve in the composed environment
    CommandNotFound,
    /// The command resolves to a toolchain launcher that would re-enter itself
    CommandTrampolineRefused,
    /// The announce would drop tags the index already committed
    CommittedTagsDropped,
    /// Creating compressed output failed
    CompressionCreate,
    /// The format can be extracted but not written
    CompressionDecodeOnly,
    /// The compression engine failed to initialize
    CompressionEngineInit,
    /// A compression stream failed to read or write
    CompressionIo,
    /// Opening a file for decompression failed
    CompressionOpen,
    /// The compression algorithm could not be determined
    CompressionUnknownFormat,
    /// Reading or writing the config file being edited failed
    ConfigEditIo,
    /// Another process holds the lock on the config file being edited
    ConfigEditLocked,
    /// The config file being edited has a shape the edit cannot apply to
    ConfigEditMalformed,
    /// The config file being edited does not parse as TOML
    ConfigEditParse,
    /// The config file being edited exceeds the allowed size
    ConfigEditTooLarge,
    /// An explicitly named config or project file does not exist
    ConfigFileNotFound,
    /// A config file exceeds the allowed size
    ConfigFileTooLarge,
    /// Reading a config file failed
    ConfigIo,
    /// A config file does not parse
    ConfigParse,
    /// The credential helper failed to supply credentials
    CredentialHelperFailed,
    /// The credential helper answered with invalid JSON
    CredentialHelperInvalidJson,
    /// The configured credential helper is not on PATH
    CredentialHelperNotOnPath,
    /// The credential helper did not answer in time
    CredentialHelperTimeout,
    /// The credential helper resolves to an unsafe path
    CredentialHelperUnsafePath,
    /// Writing the credential store failed
    CredentialStoreWrite,
    /// A declared binary is not executable
    DeclaredBinaryNotExecutable,
    /// A layer decompressed past the size cap
    DecompressionCapExceeded,
    /// A tag delete names no single tag, or also names a digest
    DeleteNeedsTag,
    /// Several of a dependency's platforms match the target equally
    DependencyAmbiguousPlatform,
    /// Dependencies pin conflicting versions of one repository
    DependencyConflict,
    /// A package declares its dependencies invalidly
    DependencyDeclarationInvalid,
    /// A pinned dependency manifest is not in the registry
    DependencyManifestNotFound,
    /// A dependency is published for no platform compatible with the target
    DependencyNoCompatiblePlatform,
    /// A dependency is not in the selected index
    DependencyNotFound,
    /// Resolving a dependency pin failed with an unclassified cause
    DependencyPinResolutionFailed,
    /// Verifying a dependency pin failed with an unclassified cause
    DependencyPinVerificationFailed,
    /// A dependency is pinned to an image index rather than a platform manifest
    DependencyPinnedToIndex,
    /// Routing a dependency through the index failed with an unclassified cause
    DependencyRoutingFailed,
    /// Setting up a dependency failed with an unclassified cause
    DependencySetupFailed,
    /// The package description vanished between two reads
    DescDisappeared,
    /// Inspecting the destination directory failed
    DestinationCheckIo,
    /// The destination exists and is not a directory
    DestinationNotADirectory,
    /// The destination directory is not empty
    DestinationNotEmpty,
    /// A destination path resolves through a symlink
    DestinationThroughSymlink,
    /// Downloaded content does not hash to its expected digest
    DigestMismatch,
    /// The identifier carries no digest after resolution
    DigestMissing,
    /// An `any` package pins a dependency by a platform-specific digest
    DirectDigestPinInAnyTarget,
    /// A stored dispatch object does not hash to its claimed digest
    DispatchObjectDigestMismatch,
    /// Reading Docker credentials failed
    DockerCredentialRetrieval,
    /// A binding is defined in more than one selected group
    DuplicateBindingAcrossGroups,
    /// The same owner was supplied twice
    DuplicateOwner,
    /// ocx.lock lists the same platform twice for one binding
    DuplicatePlatformKey,
    /// A group filter names an empty group
    EmptyGroupFilter,
    /// A push names no target platform
    EmptyPushSet,
    /// An endpoint host does not resolve
    EndpointUnresolvable,
    /// Two packages declare the same entrypoint name
    EntrypointCollision,
    /// The extra CA bundle the payload names is not usable
    ExtraCaCertsInvalid,
    /// The extra CA bundle is not UTF-8 and cannot be persisted
    ExtraCaCertsNotUtf8,
    /// The extra CA bundle inlined in the payload is not usable
    ExtraCaCertsPemInvalid,
    /// Reading the extra CA bundle the payload names failed
    ExtraCaCertsReadFailed,
    /// The extra CA bundle file holds no usable certificate
    ExtraCaFileInvalid,
    /// The extra CA bundle file exceeds the allowed size
    ExtraCaFileTooLarge,
    /// The extra CA bundle cannot be read
    ExtraCaUnreadable,
    /// The configured extra CA bundle value holds no usable certificate
    ExtraCaValueInvalid,
    /// The configured extra CA bundle value exceeds the allowed size
    ExtraCaValueTooLarge,
    /// The package needs a platform feature this host lacks
    FeatureMismatch,
    /// Reading or writing an internal file failed
    FileIo,
    /// The rewritten registry resolves into a forbidden address range
    ForbiddenRegistryTarget,
    /// The forge rejected the request's credentials
    ForgeAuthFailed,
    /// The forge or project lacks a capability the transport needs
    ForgeCapabilityUnavailable,
    /// The forge HTTP client could not be built
    ForgeClientBuild,
    /// A forge response could not be decoded
    ForgeDecode,
    /// A forge operation failed with an unclassified cause
    ForgeFailed,
    /// The forge kind of a self-hosted host is unknown; name it explicitly
    ForgeKindUnknown,
    /// A forge response lacks a required field
    ForgeMissingField,
    /// The push was not a fast-forward; another writer moved the branch
    ForgeNonFastForward,
    /// The publishing project is not on the index project's job-token allowlist
    ForgePublisherNotAllowlisted,
    /// The credential may not push to the repository
    ForgePushAccessDenied,
    /// The forge refused the push
    ForgePushRefused,
    /// A forge request body could not be encoded
    ForgeRequestEncode,
    /// No forge was supplied to read the committed index root through
    ForgeRequired,
    /// The branch moved since it was read
    ForgeStaleLease,
    /// The forge answered with an unexpected HTTP status
    ForgeStatus,
    /// The forge answered with a status a retry may clear
    ForgeTransient,
    /// The forge could not be reached
    ForgeTransportFailed,
    /// The selected transport does not support this operation
    ForgeTransportOperationUnsupported,
    /// Reaching the forge failed in a way a retry may clear
    ForgeTransportTransient,
    /// The forge does not support the selected transport
    ForgeTransportUnsupported,
    /// The forge answered with a server error
    ForgeUnavailable,
    /// The fork's branch cannot reach the upstream base
    ForkBaseUnreachable,
    /// A fork response lacks a required field
    ForkFieldMissing,
    /// The fork lives on another host than the index
    ForkHostMismatch,
    /// The fork did not become ready in time
    ForkNotReady,
    /// The fork is owned by another account than expected
    ForkOwnerMismatch,
    /// The fork reports no parent
    ForkParentAbsent,
    /// The fork's parent is not the expected upstream
    ForkParentMismatch,
    /// The OCX_ENV value a parent ocx forwarded is malformed
    ForwardedEnvInvalid,
    /// The certificate authority rejected the signing request
    FulcioBadRequest,
    /// The certificate authority is unreachable or overloaded
    FulcioUnavailable,
    /// A git command failed
    GitCommandFailed,
    /// A git push failed for an unrecognised reason
    GitPushFailed,
    /// The git executable the transport needs is unavailable
    GitUnavailable,
    /// A group table holds a binding outside its tools table
    GroupHoldsDirectBinding,
    /// A registry host could not be resolved
    HostResolutionFailed,
    /// The certificate identity does not match the required signer
    IdentityMismatch,
    /// The identity token file is readable by group or other
    IdentityTokenFilePermissive,
    /// A dispatch object does not hash to the digest its root claims
    IndexDispatchDigestMismatch,
    /// An index request failed
    IndexHttpFailed,
    /// An index request failed in a way a retry may clear
    IndexHttpTransient,
    /// The registry has no manifest for a tag the index update walked
    IndexManifestNotFound,
    /// The source names an image index by digest, which cannot merge into the target
    IndexNamedByDigest,
    /// A local index path is invalid
    IndexPathInvalid,
    /// A local policy refused resolving through the index
    IndexResolutionBlocked,
    /// The committed index root lacks a required field
    IndexRootMissingField,
    /// The committed index root is not a JSON object
    IndexRootNotObject,
    /// The committed index root is not valid JSON
    IndexRootParse,
    /// An index document could not be serialized
    IndexSerialization,
    /// A shared index operation failed with an unclassified cause
    IndexSingleflightFailed,
    /// An index source failed with an unclassified cause
    IndexSourceFailed,
    /// An integrations namespace is invalid
    IntegrationNamespaceInvalid,
    /// An integrations payload exceeds its size limit
    IntegrationTooLarge,
    /// A package's integrations exceed the per-package size limit
    IntegrationsTooLarge,
    /// A failure no classifier recognises
    Internal,
    /// An internal store path has an unexpected structure
    InternalPathInvalid,
    /// The configured authentication type is not recognized
    InvalidAuthType,
    /// A binary name is not valid
    InvalidBinaryName,
    /// A binding name carries a character outside the allowed set
    InvalidBindingName,
    /// A value is not one of the accepted boolean spellings
    InvalidBooleanString,
    /// A version's build metadata is invalid
    InvalidBuildMetadata,
    /// A digest is not a valid algorithm-prefixed hash
    InvalidDigest,
    /// Registry content is not valid UTF-8
    InvalidEncoding,
    /// A Sigstore endpoint URL failed validation
    InvalidEndpointUrl,
    /// An OCX environment variable holds a value its declaration refuses
    InvalidEnv,
    /// A group name carries a character outside the allowed set
    InvalidGroupName,
    /// A package identifier does not parse
    InvalidIdentifier,
    /// An image index document is invalid
    InvalidImageIndex,
    /// A configured index URL is invalid
    InvalidIndexUrl,
    /// A layer's strip-components or prefix annotation is invalid
    InvalidLayerLayout,
    /// A layer reference or its layout suffix does not parse
    InvalidLayerRef,
    /// A package env entry declares an unusable list separator
    InvalidListSeparator,
    /// The logo file's content does not match its format
    InvalidLogoContent,
    /// The managed-config source is not a valid OCI identifier
    InvalidManagedConfigSource,
    /// A manifest is invalid
    InvalidManifest,
    /// An owner login carries a character outside letters, digits, dot, underscore and hyphen
    InvalidOwnerLogin,
    /// A platform string does not follow the platform grammar
    InvalidPlatform,
    /// A repository coordinate is not a valid owner/name
    InvalidRepoCoordinate,
    /// A tag is malformed, or not one the operation may act on
    InvalidTag,
    /// A group or binding name carries a character toolchain paths refuse
    InvalidToolchainNameCharset,
    /// The payload declares an unusable trust-policy entry
    InvalidTrustPolicy,
    /// The version argument is not a tag, a digest, or a tag pinned to a digest
    InvalidVersionSpec,
    /// The certificate OIDC issuer does not match the required issuer
    IssuerMismatch,
    /// JSON could not be serialized or deserialized
    JsonSerialization,
    /// The signing key backend is temporarily unavailable
    KeyBackendUnavailable,
    /// A signing or verification key holds no usable key
    KeyMalformed,
    /// A key reference could not be parsed
    KeyReferenceInvalid,
    /// A signing or verification key could not be read from its location
    KeyUnreadable,
    /// The OCX_LAUNCH_IDENTITIES value a parent ocx forwarded is malformed
    LaunchIdentitiesInvalid,
    /// The resolved command could not be started
    LaunchSpawnFailed,
    /// A path baked into a generated launcher is not valid UTF-8
    LauncherPathNotUtf8,
    /// A value baked into a generated launcher contains an unsafe character
    LauncherUnsafeCharacter,
    /// Resolving a layer layout failed with an unclassified cause
    LayerLayoutFailed,
    /// A layer the package needs was not staged
    LayerNotStaged,
    /// A layer declares a size above the allowed maximum
    LayerSizeExceeded,
    /// Reading a file while checking its libc requirement failed
    LibcLintRead,
    /// Walking the content tree for the libc check failed with an unclassified cause
    LibcLintScanFailed,
    /// The libc scan scope carries a modifier it cannot honour
    LibcScanScopeModifier,
    /// The libc scan scope cannot be resolved
    LibcScanScopeUnresolvable,
    /// The install link path holds something other than an ocx package link
    LinkPathOccupied,
    /// A list-valued environment entry conflicts with or edges on its separator
    ListSeparatorInvalid,
    /// ocx.lock does not exist
    LockMissing,
    /// ocx.lock records a repository carrying a tag or digest
    LockRepositoryNotBare,
    /// ocx.lock does not match ocx.toml
    LockStale,
    /// The registry rejected the supplied credentials
    LoginRejected,
    /// An index source served a malformed catalog key
    MalformedCatalogKey,
    /// A fork's full name is malformed
    MalformedForkFullName,
    /// An index document is not valid JSON of the expected shape
    MalformedIndexDocument,
    /// An index root carries a malformed physical repository reference
    MalformedPhysicalRef,
    /// The index root names a malformed registry repository
    MalformedPhysicalRepository,
    /// The repository is not a well-formed oci:// pointer
    MalformedRepository,
    /// A local index root document is not valid JSON of the expected shape
    MalformedRootDocument,
    /// The managed-config payload contains a \[managed\] section
    ManagedConfigContainsManagedSection,
    /// The managed-config config.toml entry exceeds the allowed size
    ManagedConfigEntryTooLarge,
    /// A file the managed-config payload names does not exist
    ManagedConfigInputNotFound,
    /// The managed-config settings are invalid, or a system lock refuses the change
    ManagedConfigInvalid,
    /// The managed-config layer is not a readable archive
    ManagedConfigInvalidArchive,
    /// The fetched managed-config payload is not valid TOML
    ManagedConfigInvalidToml,
    /// The payload declares a trust-policy key signer by path instead of inline
    ManagedConfigKeyByPath,
    /// The managed-config layer does not hash to its declared digest
    ManagedConfigLayerDigestMismatch,
    /// The managed-config layer declares a size above the allowed maximum
    ManagedConfigLayerSizeExceeded,
    /// The managed-config layer contains no config.toml
    ManagedConfigMissingConfigToml,
    /// The managed-config package has no any/any platform entry
    ManagedConfigNoAnyPlatform,
    /// The managed-config package has no tar+gzip layer
    ManagedConfigNoGzipLayer,
    /// The managed-config payload is not a valid config file
    ManagedConfigPayloadInvalidToml,
    /// Reading the managed-config payload failed
    ManagedConfigPayloadReadFailed,
    /// The managed-config payload exceeds the allowed size
    ManagedConfigPayloadTooLarge,
    /// Writing the managed-config snapshot failed
    ManagedConfigSnapshotWrite,
    /// The registry has no managed-config package at the configured source
    ManagedConfigSourceNotFound,
    /// Staging the managed-config payload for publishing failed
    ManagedConfigStageFailed,
    /// The managed-config package has an unexpected manifest shape
    ManagedConfigUnexpectedManifest,
    /// Syncing the managed-config snapshot failed with an unclassified cause
    ManagedConfigUpdateFailed,
    /// The registry has no such manifest
    ManifestNotFound,
    /// The forge did not confirm the merge request in time
    MergeRequestUnconfirmed,
    /// More than one candidate package metadata file was found
    MetadataAmbiguous,
    /// A package metadata blob exceeds the size cap
    MetadataBlobTooLarge,
    /// A layer path cannot anchor a metadata file search
    MetadataInvalidLayerPath,
    /// No package metadata file was given or found
    MetadataRequired,
    /// A registry mirror setting is invalid
    MirrorConfigInvalid,
    /// The index base branch does not exist
    MissingBaseRef,
    /// An identifier the store needs pinned carries no digest
    MissingDigest,
    /// A concurrent claim's or announce's head carries no index root
    MissingHeadRoot,
    /// A list-valued package env entry omits its separator
    MissingListSeparator,
    /// More than one verified attestation matched
    MultipleAttestations,
    /// The envelope carries other than exactly one signature
    MultipleSignatures,
    /// An image index nests another image index, which OCI does not support
    NestedImageIndex,
    /// The forge does not support nested namespaces
    NestedNamespaceUnsupported,
    /// No owner could be determined to write into the claim
    NoActingIdentity,
    /// No credential helper or store is available to save credentials in
    NoCredentialStore,
    /// No tag is left to announce once reserved tags are dropped
    NoCuratedTags,
    /// ocx.lock pins no platform this host can run
    NoHostLeaf,
    /// No signer identity was given by flags or a matching trust policy
    NoIdentityProvided,
    /// The repository carries no tag the index can record
    NoIndexableTag,
    /// The source index offers no requested platform
    NoMatchingPlatform,
    /// No ocx.toml exists in the working directory or any parent
    NoProject,
    /// The explicitly selected project directory holds no ocx.toml
    NoProjectIn,
    /// The target has no signature referrers
    NoSignaturesFound,
    /// Referrers exist but none is a recognised Sigstore bundle
    NoUsableBundle,
    /// A local index path is not valid UTF-8
    NonUtf8WireName,
    /// ocx.lock records a platform key not in canonical form
    NoncanonicalPlatformKey,
    /// The registry did not answer with a manifest
    NotAManifest,
    /// The package is not listed in the index
    NotInIndex,
    /// A tag appeared between the two reads of one announce
    ObserveRaced,
    /// Attaching was refused because offline mode is on
    OfflineAttestRefused,
    /// Offline mode and the manifest is not stored locally
    OfflineManifestMissing,
    /// A network operation was attempted in offline mode
    OfflineMode,
    /// Signing was refused because offline mode is on
    OfflineSignRefused,
    /// The OIDC token failed a client-side check before reaching the certificate authority
    OidcPreCheckFailed,
    /// The certificate authority rejected the OIDC token
    OidcTokenRejected,
    /// Writing the output tree failed
    OutputWrite,
    /// A supplied owner id disagrees with the forge account
    OwnerIdMismatch,
    /// The forge has no account with this login
    OwnerUnknown,
    /// Package metadata cannot be authored as given
    PackageAuthoringInvalid,
    /// Reading or writing a package file failed
    PackageFileIo,
    /// A package operation failed with an unclassified cause
    PackageInternal,
    /// A package key is not a valid identifier
    PackageKeyInvalid,
    /// A package key names no registry
    PackageKeyMissingRegistry,
    /// Reading or writing an internal package file failed
    PackageManagerInternalFile,
    /// A package document could not be serialized
    PackageManagerSerialization,
    /// The package does not exist
    PackageNotFound,
    /// A package document could not be serialized
    PackageSerialization,
    /// A per-package operation failed with an unclassified cause
    PackageTaskFailed,
    /// Persisting a patch descriptor blob failed
    PatchBlobWriteFailed,
    /// A patch-tier setting is invalid
    PatchConfigInvalid,
    /// A patch descriptor is not valid JSON of the expected shape
    PatchDescriptorInvalidJson,
    /// A patch descriptor exceeds its structural limits
    PatchDescriptorTooLarge,
    /// A patch descriptor has a version this build does not understand
    PatchDescriptorUnsupportedVersion,
    /// A pinned patch descriptor is no longer in the registry
    PatchDescriptorVanished,
    /// Discovering patches failed with an unclassified cause
    PatchDiscoveryFailed,
    /// A patch descriptor layer does not hash to its declared digest
    PatchLayerDigestMismatch,
    /// A patch descriptor layer declares a size above the allowed maximum
    PatchLayerSizeExceeded,
    /// A patch descriptor manifest does not hash to its declared digest
    PatchManifestDigestMismatch,
    /// A local policy refused fetching a patch descriptor
    PatchPolicyBlocked,
    /// The project config patches would freeze into is unreadable
    PatchProjectConfigUnreadable,
    /// Patch pins cannot advance while OCX_PATCH_SNAPSHOT is set
    PatchSnapshotActive,
    /// The patch snapshot names a descriptor that is not stored locally
    PatchSnapshotDescriptorMissing,
    /// The patch snapshot has a format version this build does not understand
    PatchSnapshotUnsupportedVersion,
    /// A patch descriptor manifest carries an unexpected artifact type
    PatchUnexpectedArtifactType,
    /// A patch descriptor layer carries an unexpected media type
    PatchUnexpectedLayerMediaType,
    /// A patch descriptor manifest has an unexpected shape
    PatchUnexpectedManifest,
    /// A patch descriptor manifest carries other than exactly one layer
    PatchWrongLayerCount,
    /// A relative path is absolute, escapes its root, or is otherwise unsafe to join
    PathEscape,
    /// The envelope payload type is not in-toto JSON
    PayloadTypeUnsupported,
    /// The operating system refused access to a file or directory
    PermissionDenied,
    /// The registry resolved the pinned tag to a different digest
    PinDigestMismatch,
    /// An identifier that must be pinned carries no digest
    PinnedIdentifierMissingDigest,
    /// An index URL uses plain HTTP for a host not allowed to
    PlainHttpIndexNotAllowed,
    /// The source is a single-platform manifest and more than one platform was named
    PlatformAmbiguous,
    /// The source is a single-platform manifest and no platform was named
    PlatformRequired,
    /// The predicate file is not JSON
    PredicateNotJson,
    /// The predicate or statement exceeds its size limit
    PredicateTooLarge,
    /// The signed predicate type is not the requested one
    PredicateTypeMismatch,
    /// ocx.toml already exists at the target path
    ProjectAlreadyExists,
    /// ocx.toml sets an invalid environment variable name
    ProjectEnvInvalidKey,
    /// An ocx.toml environment entry has an unusable list separator
    ProjectEnvInvalidSeparator,
    /// An ocx.toml environment entry has a value of the wrong shape
    ProjectEnvInvalidValue,
    /// An ocx.toml path value contains the platform path separator
    ProjectEnvPathSeparatorInValue,
    /// ocx.toml sets a reserved OCX environment key
    ProjectEnvReservedKey,
    /// An ocx.toml list value starts or ends with its separator
    ProjectEnvSeparatorEdgedValue,
    /// An ocx.toml environment entry sets a separator on a non-list value
    ProjectEnvSeparatorOnNonList,
    /// An ocx.toml environment entry names an unknown modifier type
    ProjectEnvUnknownModifier,
    /// An ocx.toml environment entry table holds an unknown field
    ProjectEnvUnknownValueField,
    /// A project file exceeds the allowed size
    ProjectFileTooLarge,
    /// Reading or writing an internal project file failed
    ProjectInternalFile,
    /// Reading or writing a project file failed
    ProjectIo,
    /// Loading the project failed with an unclassified cause
    ProjectLoadFailed,
    /// Another process holds the lock on ocx.toml
    ProjectLocked,
    /// The edited ocx.toml no longer matches the staged configuration
    ProjectManifestEditDiverged,
    /// ocx.toml is not an editable TOML document
    ProjectManifestNotEditable,
    /// Reading or writing the project registry failed
    ProjectRegistryIo,
    /// A local policy refused resolving a binding
    ProjectResolutionBlocked,
    /// ocx.toml or ocx.lock is not valid TOML of the expected shape
    ProjectTomlInvalid,
    /// A project file could not be serialized
    ProjectTomlSerialize,
    /// The provenance predicate type is older than SLSA v1.0
    ProvenanceVersionUnsupported,
    /// A digest was given where prune takes a tag
    PruneDigestTag,
    /// No index is configured to check the tags against
    PruneNoIndex,
    /// The --prerelease value is not a pre-release without a build
    PruneNotAPrerelease,
    /// The package argument carries a tag or digest
    PrunePackageNotBare,
    /// The index still lists a tag prune would delete, and the tag is durable
    PruneRefusedDurable,
    /// The index still lists a tag prune would delete, pending an announce
    PruneRefusedPending,
    /// A deleted tag is still present in the registry
    PruneTagStillPresent,
    /// The open announce pull request conflicts with the index
    PullRequestUnmergeable,
    /// The forge refused a git push option
    PushOptionRefused,
    /// The execution-record exemption was refused
    RecordExemptionRefused,
    /// An execution record frame carries no argv
    RecordInputsIncomplete,
    /// The record name is not a plain filename
    RecordNameNotAFilename,
    /// Serializing an execution record failed
    RecordSerializeFailed,
    /// The execution-record sink is a symlink
    RecordSinkSymlink,
    /// The record name template has no varying component
    RecordTemplateNotUnique,
    /// The record name template names an unknown placeholder
    RecordTemplateUnknownPlaceholder,
    /// Writing an execution record failed
    RecordWriteFailed,
    /// Execution records are required but no sink is configured
    RecordsRequiredWithoutSink,
    /// The registry supports neither the OCI Referrers API nor a referrers fallback tag
    ReferrersUnsupported,
    /// The registry rejected the request's authentication
    RegistryAuthFailed,
    /// The registry does not delete tags
    RegistryDeleteUnsupported,
    /// A registry operation failed in a way a retry may clear
    RegistryTransient,
    /// A registry operation failed in a way a retry does not clear
    RegistryUnavailable,
    /// The registry could not be reached
    RegistryUnreachable,
    /// The bundle carries no transparency log inclusion proof
    RekorInclusionProofAbsent,
    /// The bundle has a timestamp authority stamp but no transparency log timestamp, which this
    /// build cannot verify
    RekorSetAbsentTsaPresent,
    /// The transparency log timestamp does not verify against the log key
    RekorSetInvalid,
    /// The transparency log answered with an unusable timestamp or body
    RekorSetMalformed,
    /// Skipping the transparency log upload requires a signing key
    RekorUploadRequiredForKeyless,
    /// Persisting the extra CA bundle would exceed the config size limit
    RenderedConfigTooLarge,
    /// An index repository path escapes its source directory
    RepositoryEscapesIndexHome,
    /// A re-claim supplied a repository other than the committed one
    RepositoryMismatch,
    /// The registry has no such repository
    RepositoryNotFound,
    /// A required path does not exist
    RequiredPathMissing,
    /// A package env entry sets a reserved OCX key
    ReservedEnvKey,
    /// ocx.toml declares a reserved group name
    ReservedGroupName,
    /// Resolving a binding timed out
    ResolveTimeout,
    /// A retired OCX environment variable is set
    RetiredEnv,
    /// The committed index root names a different package
    RootNameMismatch,
    /// A derived index root points at another repository
    RootRepositoryMismatch,
    /// Probing whether two paths share a filesystem failed
    SameFilesystemCheckFailed,
    /// An unsigned SBOM referrer declares a media type outside the SBOM set
    SbomMediaTypeUnsupported,
    /// The selection matches more than one package
    SelectionAmbiguous,
    /// The fork namespace is the upstream's own
    SelfForkRefused,
    /// A package list value starts or ends with its separator
    SeparatorEdgedListValue,
    /// A session PATH entry is not an absolute path
    SessionPathNotAbsolute,
    /// A session PATH entry is not valid UTF-8
    SessionPathNotUtf8,
    /// A session PATH entry contains a character the host format cannot store
    SessionPathUnencodable,
    /// Installing ocx into the content store failed with an unclassified cause
    SetupBootstrapFailed,
    /// Reading or writing a setup file failed
    SetupIo,
    /// A shell subprocess setup needs could not run
    SetupProfileSubprocess,
    /// ocx.toml declares a \[shell\] section, which only config.toml may hold
    ShellSectionInProject,
    /// A declared interface name does not exist in the package
    ShimClaimUnfulfilled,
    /// A shim name is not a valid binary name
    ShimNameInvalid,
    /// A shim name is not an interface name the package declares
    ShimNameNotClaimed,
    /// The package's interface names cannot be enumerated
    ShimNamesNotEnumerable,
    /// The registry ended a blob download early
    ShortBlobRead,
    /// A sidecar signature format was requested without a signing identity
    SidecarRequiresSignature,
    /// The signature does not verify over the subject digest
    SignatureInvalid,
    /// The simple signing payload declares an unsupported claim type
    SimpleSigningClaimUnsupported,
    /// The leader of shared in-flight work stopped without a result
    SingleflightAbandoned,
    /// Too many distinct operations were in flight at once
    SingleflightCapacityExceeded,
    /// Shared in-flight work failed with an unclassified cause
    SingleflightFailed,
    /// Waiting for shared in-flight work timed out
    SingleflightTimeout,
    /// A registry host resolves to an address outside the trusted set
    SsrfForbiddenTarget,
    /// The signed statement carries no subjects
    StatementSubjectAbsent,
    /// No subject in the signed statement binds the target digest
    StatementSubjectMismatch,
    /// A statement subject carries no sha256 digest
    StatementSubjectWeakAlgorithm,
    /// The statement type is not an accepted in-toto statement type
    StatementTypeUnsupported,
    /// The registry served subject bytes that do not hash to the resolved digest
    SubjectDigestMismatch,
    /// The subject digest is not sha256
    SubjectDigestUnsupported,
    /// The package has no candidate or current symlink to resolve
    SymlinkNotFound,
    /// Symlink resolution needs a tag, not a digest
    SymlinkRequiresTag,
    /// Inspecting a destination path component failed
    SymlinkWalkIo,
    /// The system-wide config file is unusable
    SystemConfigInvalid,
    /// A tag to announce does not point at an image index
    TagNotAnImageIndex,
    /// The registry has no such tag
    TagNotFound,
    /// A platform was requested but the reference names a single manifest
    TargetNotAnIndex,
    /// The reference resolves to no manifest for the requested platform
    TargetNotFound,
    /// A package task panicked
    TaskPanicked,
    /// An interpolation token names an ambiguous dependency
    TemplateAmbiguousDependencyRef,
    /// An interpolation token names an env var declared more than once
    TemplateAmbiguousSelfEnvRef,
    /// An interpolation token names a dependency that is not installed
    TemplateDependencyNotInstalled,
    /// An interpolation token is not permitted in this field
    TemplateDisallowedToken,
    /// An interpolation modifier does not apply to its token
    TemplateModifierNotApplicable,
    /// An interpolation token names an env var not declared before it
    TemplateUndefinedSelfEnvRef,
    /// An interpolation token names an unknown dependency
    TemplateUnknownDependencyRef,
    /// An interpolation token names an unknown field
    TemplateUnknownField,
    /// An interpolation token carries an unknown modifier
    TemplateUnknownModifier,
    /// An interpolation token is not recognised
    TemplateUnknownToken,
    /// An interpolated value exceeds its size budget
    TemplateValueTooLarge,
    /// The transparency log entry does not match the received envelope
    TlogBindingMismatch,
    /// The target has more attestation referrers than the candidate limit
    TooManyAttestations,
    /// The toolchain_dir directory is writable by other users
    ToolchainDirGroupOrWorldWritable,
    /// The toolchain_dir setting names a path that cannot be inspected
    ToolchainDirInaccessible,
    /// The toolchain_dir setting resolves inside the global toolchain home
    ToolchainDirInsideGlobalHome,
    /// The toolchain_dir setting names the home directory or OCX_HOME itself
    ToolchainDirIsContainmentAnchor,
    /// No home directory or OCX_HOME exists to contain toolchain_dir
    ToolchainDirNoContainmentAnchor,
    /// The toolchain_dir setting names something other than a directory
    ToolchainDirNotADirectory,
    /// The toolchain_dir directory is not owned by the current user
    ToolchainDirNotOwnerOwned,
    /// The toolchain_dir setting resolves outside the home directory and OCX_HOME
    ToolchainDirOutsideHome,
    /// The toolchain_dir setting contains a '..' component
    ToolchainDirParentComponent,
    /// The toolchain_dir setting is a relative path
    ToolchainDirRelative,
    /// The toolchain_dir setting resolves to a system location
    ToolchainDirSystemPrefix,
    /// The toolchain_dir setting starts with a '~' that cannot be expanded
    ToolchainDirUnexpandable,
    /// The toolchain home is not an absolute path
    ToolchainHomeNotAbsolute,
    /// A toolchain group or entry name contains a control character
    ToolchainNameControlCharacter,
    /// A toolchain group or entry name is empty
    ToolchainNameEmpty,
    /// A toolchain group or entry name carries a path prefix
    ToolchainNamePathPrefix,
    /// A toolchain group or entry name is a relative path component
    ToolchainNameRelative,
    /// A toolchain group or entry name contains a path separator
    ToolchainNameSeparator,
    /// A toolchain group or entry name ends with a dot or a space
    ToolchainNameTrailingDotOrSpace,
    /// The transparency log entry belongs to another subject
    TransparencyBodyMismatch,
    /// The transparency log answered its public-key request with a client error
    TransparencyLogKeyUnavailable,
    /// The transparency log answered with a public-key body that is oversize or not text
    TransparencyLogResponseInvalid,
    /// The transparency log is unavailable
    TransparencyLogUnavailable,
    /// The transparency log cannot be reached and a rerun will not change that
    TransparencyLogUnreachable,
    /// Copying a manifest graph exceeded a traversal limit
    TraversalLimitExceeded,
    /// A trust policy entry is malformed
    TrustPolicyInvalid,
    /// Trust material failed to load
    TrustRootLoad,
    /// The trust root could not be loaded
    TrustRootUnavailable,
    /// A trust root file named by the operator could not be read
    TrustRootUnreadable,
    /// The named file is not a usable Sigstore trusted root
    TrustedRootInvalid,
    /// Reading the trusted root the payload names failed
    TrustedRootReadFailed,
    /// The package has no index entry to announce into
    UnclaimedPackage,
    /// A scanned executable is not declared in binaries
    UndeclaredBinary,
    /// A binary needs a libc the package's platform does not declare
    UndeclaredLibc,
    /// An artifact carries an unexpected artifact type
    UnexpectedArtifactType,
    /// A layer carries an unexpected media type
    UnexpectedLayerMediaType,
    /// An image index arrived where an image manifest was expected
    UnexpectedManifestType,
    /// The registry answered with a redirect the transport did not follow
    UnfollowedRedirect,
    /// A forge compare returned a status the client does not model
    UnknownCompareStatus,
    /// A package env entry declares a type this build does not know
    UnknownEnvModifier,
    /// A group filter names a group ocx.toml does not declare
    UnknownGroup,
    /// A group table holds an unknown key
    UnknownGroupSection,
    /// A file carries an ELF header but cannot be parsed
    UnparseableElf,
    /// A binary names an ELF interpreter no libc family claims
    UnrecognizedInterpreter,
    /// A tag to announce does not exist in the registry
    UnresolvedTag,
    /// The registry named a destination the transport refuses
    UnsafeDestination,
    /// An unsigned document was refused because a signature is required
    UnsignedRejectedByPolicy,
    /// An unsigned attach names a predicate type with no SBOM media type
    UnsignedTypeUnsupported,
    /// ocx.lock records a declaration hash version this build does not understand
    UnsupportedDeclarationHashVersion,
    /// The index format version is not supported
    UnsupportedIndexFormat,
    /// A key reference names a recognised key backend this build does not implement
    UnsupportedKeyBackend,
    /// ocx.lock has a version this build does not understand
    UnsupportedLockVersion,
    /// The logo file has an unsupported format
    UnsupportedLogoFormat,
    /// A manifest carries an unsupported media type
    UnsupportedMediaType,
    /// The transparency log entry kind is not accepted
    UnsupportedTlogEntryKind,
    /// A tag to unyank is not a curated tag
    UnyankTagNotCurated,
    /// ocx.toml declares an \[update\] section, which only config.toml may hold
    UpdateSectionInProject,
    /// The command was invoked with arguments it cannot act on
    Usage,
    /// The forge offers no users API to resolve owners through
    UsersApiUnavailable,
    /// A package version does not parse
    VersionInvalid,
    /// A source answered a digest request with different content
    WalkedDigestMismatch,
    /// An artifact carries other than exactly one layer
    WrongLayerCount,
    /// A tag to yank is not a curated tag
    YankTagNotCurated,
    /// The same tag is both yanked and unyanked
    YankUnyankOverlap,
    /// The resolved version is yanked
    YankedRefused,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl ErrorDetail {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::ActivationFailed => "activation_failed",
            Self::AgnosticPlatformLibcClaim => "agnostic_platform_libc_claim",
            Self::AmbiguousExtraCaCerts => "ambiguous_extra_ca_certs",
            Self::AmbiguousHostLeaf => "ambiguous_host_leaf",
            Self::AmbiguousTrustRoot => "ambiguous_trust_root",
            Self::AnnounceFailed => "announce_failed",
            Self::AnyPinNotAdvertisedAsAny => "any_pin_not_advertised_as_any",
            Self::AnyPinProvenanceUnavailable => "any_pin_provenance_unavailable",
            Self::ArchiveEntryEscape => "archive_entry_escape",
            Self::ArchiveExtractionCapExceeded => "archive_extraction_cap_exceeded",
            Self::ArchiveGnuSparseUnsupported => "archive_gnu_sparse_unsupported",
            Self::ArchiveHardLinkEscape => "archive_hard_link_escape",
            Self::ArchiveInternal => "archive_internal",
            Self::ArchiveIo => "archive_io",
            Self::ArchiveSymlinkEscape => "archive_symlink_escape",
            Self::ArchiveTarInvalid => "archive_tar_invalid",
            Self::ArchiveUnsupportedFormat => "archive_unsupported_format",
            Self::ArchiveZipInvalid => "archive_zip_invalid",
            Self::AttestationBudgetExhausted => "attestation_budget_exhausted",
            Self::AttestationNotFound => "attestation_not_found",
            Self::AttestationPayloadTooLarge => "attestation_payload_too_large",
            Self::AttestationTooLarge => "attestation_too_large",
            Self::AuthEnvMissing => "auth_env_missing",
            Self::BinScanFailed => "bin_scan_failed",
            Self::BinScanUnsupportedHost => "bin_scan_unsupported_host",
            Self::BindingAlreadyExists => "binding_already_exists",
            Self::BindingAmbiguous => "binding_ambiguous",
            Self::BindingNotDeclared => "binding_not_declared",
            Self::BindingNotFound => "binding_not_found",
            Self::BindingValueInvalid => "binding_value_invalid",
            Self::BindingValueMissingRegistry => "binding_value_missing_registry",
            Self::BlobNotFound => "blob_not_found",
            Self::BotIdentity => "bot_identity",
            Self::BuilderMismatch => "builder_mismatch",
            Self::BundleParseFailed => "bundle_parse_failed",
            Self::CandidateLimitExhausted => "candidate_limit_exhausted",
            Self::CatalogDocumentAbsent => "catalog_document_absent",
            Self::CertChainInvalid => "cert_chain_invalid",
            Self::CertificateValidityWindow => "certificate_validity_window",
            Self::CiExportWrite => "ci_export_write",
            Self::CiFileWrite => "ci_file_write",
            Self::CiMissingEnv => "ci_missing_env",
            Self::ClientInternal => "client_internal",
            Self::ClientIo => "client_io",
            Self::ClientSerialization => "client_serialization",
            Self::CommandNotExecutable => "command_not_executable",
            Self::CommandNotFound => "command_not_found",
            Self::CommandTrampolineRefused => "command_trampoline_refused",
            Self::CommittedTagsDropped => "committed_tags_dropped",
            Self::CompressionCreate => "compression_create",
            Self::CompressionDecodeOnly => "compression_decode_only",
            Self::CompressionEngineInit => "compression_engine_init",
            Self::CompressionIo => "compression_io",
            Self::CompressionOpen => "compression_open",
            Self::CompressionUnknownFormat => "compression_unknown_format",
            Self::ConfigEditIo => "config_edit_io",
            Self::ConfigEditLocked => "config_edit_locked",
            Self::ConfigEditMalformed => "config_edit_malformed",
            Self::ConfigEditParse => "config_edit_parse",
            Self::ConfigEditTooLarge => "config_edit_too_large",
            Self::ConfigFileNotFound => "config_file_not_found",
            Self::ConfigFileTooLarge => "config_file_too_large",
            Self::ConfigIo => "config_io",
            Self::ConfigParse => "config_parse",
            Self::CredentialHelperFailed => "credential_helper_failed",
            Self::CredentialHelperInvalidJson => "credential_helper_invalid_json",
            Self::CredentialHelperNotOnPath => "credential_helper_not_on_path",
            Self::CredentialHelperTimeout => "credential_helper_timeout",
            Self::CredentialHelperUnsafePath => "credential_helper_unsafe_path",
            Self::CredentialStoreWrite => "credential_store_write",
            Self::DeclaredBinaryNotExecutable => "declared_binary_not_executable",
            Self::DecompressionCapExceeded => "decompression_cap_exceeded",
            Self::DeleteNeedsTag => "delete_needs_tag",
            Self::DependencyAmbiguousPlatform => "dependency_ambiguous_platform",
            Self::DependencyConflict => "dependency_conflict",
            Self::DependencyDeclarationInvalid => "dependency_declaration_invalid",
            Self::DependencyManifestNotFound => "dependency_manifest_not_found",
            Self::DependencyNoCompatiblePlatform => "dependency_no_compatible_platform",
            Self::DependencyNotFound => "dependency_not_found",
            Self::DependencyPinResolutionFailed => "dependency_pin_resolution_failed",
            Self::DependencyPinVerificationFailed => "dependency_pin_verification_failed",
            Self::DependencyPinnedToIndex => "dependency_pinned_to_index",
            Self::DependencyRoutingFailed => "dependency_routing_failed",
            Self::DependencySetupFailed => "dependency_setup_failed",
            Self::DescDisappeared => "desc_disappeared",
            Self::DestinationCheckIo => "destination_check_io",
            Self::DestinationNotADirectory => "destination_not_a_directory",
            Self::DestinationNotEmpty => "destination_not_empty",
            Self::DestinationThroughSymlink => "destination_through_symlink",
            Self::DigestMismatch => "digest_mismatch",
            Self::DigestMissing => "digest_missing",
            Self::DirectDigestPinInAnyTarget => "direct_digest_pin_in_any_target",
            Self::DispatchObjectDigestMismatch => "dispatch_object_digest_mismatch",
            Self::DockerCredentialRetrieval => "docker_credential_retrieval",
            Self::DuplicateBindingAcrossGroups => "duplicate_binding_across_groups",
            Self::DuplicateOwner => "duplicate_owner",
            Self::DuplicatePlatformKey => "duplicate_platform_key",
            Self::EmptyGroupFilter => "empty_group_filter",
            Self::EmptyPushSet => "empty_push_set",
            Self::EndpointUnresolvable => "endpoint_unresolvable",
            Self::EntrypointCollision => "entrypoint_collision",
            Self::ExtraCaCertsInvalid => "extra_ca_certs_invalid",
            Self::ExtraCaCertsNotUtf8 => "extra_ca_certs_not_utf8",
            Self::ExtraCaCertsPemInvalid => "extra_ca_certs_pem_invalid",
            Self::ExtraCaCertsReadFailed => "extra_ca_certs_read_failed",
            Self::ExtraCaFileInvalid => "extra_ca_file_invalid",
            Self::ExtraCaFileTooLarge => "extra_ca_file_too_large",
            Self::ExtraCaUnreadable => "extra_ca_unreadable",
            Self::ExtraCaValueInvalid => "extra_ca_value_invalid",
            Self::ExtraCaValueTooLarge => "extra_ca_value_too_large",
            Self::FeatureMismatch => "feature_mismatch",
            Self::FileIo => "file_io",
            Self::ForbiddenRegistryTarget => "forbidden_registry_target",
            Self::ForgeAuthFailed => "forge_auth_failed",
            Self::ForgeCapabilityUnavailable => "forge_capability_unavailable",
            Self::ForgeClientBuild => "forge_client_build",
            Self::ForgeDecode => "forge_decode",
            Self::ForgeFailed => "forge_failed",
            Self::ForgeKindUnknown => "forge_kind_unknown",
            Self::ForgeMissingField => "forge_missing_field",
            Self::ForgeNonFastForward => "forge_non_fast_forward",
            Self::ForgePublisherNotAllowlisted => "forge_publisher_not_allowlisted",
            Self::ForgePushAccessDenied => "forge_push_access_denied",
            Self::ForgePushRefused => "forge_push_refused",
            Self::ForgeRequestEncode => "forge_request_encode",
            Self::ForgeRequired => "forge_required",
            Self::ForgeStaleLease => "forge_stale_lease",
            Self::ForgeStatus => "forge_status",
            Self::ForgeTransient => "forge_transient",
            Self::ForgeTransportFailed => "forge_transport_failed",
            Self::ForgeTransportOperationUnsupported => "forge_transport_operation_unsupported",
            Self::ForgeTransportTransient => "forge_transport_transient",
            Self::ForgeTransportUnsupported => "forge_transport_unsupported",
            Self::ForgeUnavailable => "forge_unavailable",
            Self::ForkBaseUnreachable => "fork_base_unreachable",
            Self::ForkFieldMissing => "fork_field_missing",
            Self::ForkHostMismatch => "fork_host_mismatch",
            Self::ForkNotReady => "fork_not_ready",
            Self::ForkOwnerMismatch => "fork_owner_mismatch",
            Self::ForkParentAbsent => "fork_parent_absent",
            Self::ForkParentMismatch => "fork_parent_mismatch",
            Self::ForwardedEnvInvalid => "forwarded_env_invalid",
            Self::FulcioBadRequest => "fulcio_bad_request",
            Self::FulcioUnavailable => "fulcio_unavailable",
            Self::GitCommandFailed => "git_command_failed",
            Self::GitPushFailed => "git_push_failed",
            Self::GitUnavailable => "git_unavailable",
            Self::GroupHoldsDirectBinding => "group_holds_direct_binding",
            Self::HostResolutionFailed => "host_resolution_failed",
            Self::IdentityMismatch => "identity_mismatch",
            Self::IdentityTokenFilePermissive => "identity_token_file_permissive",
            Self::IndexDispatchDigestMismatch => "index_dispatch_digest_mismatch",
            Self::IndexHttpFailed => "index_http_failed",
            Self::IndexHttpTransient => "index_http_transient",
            Self::IndexManifestNotFound => "index_manifest_not_found",
            Self::IndexNamedByDigest => "index_named_by_digest",
            Self::IndexPathInvalid => "index_path_invalid",
            Self::IndexResolutionBlocked => "index_resolution_blocked",
            Self::IndexRootMissingField => "index_root_missing_field",
            Self::IndexRootNotObject => "index_root_not_object",
            Self::IndexRootParse => "index_root_parse",
            Self::IndexSerialization => "index_serialization",
            Self::IndexSingleflightFailed => "index_singleflight_failed",
            Self::IndexSourceFailed => "index_source_failed",
            Self::IntegrationNamespaceInvalid => "integration_namespace_invalid",
            Self::IntegrationTooLarge => "integration_too_large",
            Self::IntegrationsTooLarge => "integrations_too_large",
            Self::Internal => "internal",
            Self::InternalPathInvalid => "internal_path_invalid",
            Self::InvalidAuthType => "invalid_auth_type",
            Self::InvalidBinaryName => "invalid_binary_name",
            Self::InvalidBindingName => "invalid_binding_name",
            Self::InvalidBooleanString => "invalid_boolean_string",
            Self::InvalidBuildMetadata => "invalid_build_metadata",
            Self::InvalidDigest => "invalid_digest",
            Self::InvalidEncoding => "invalid_encoding",
            Self::InvalidEndpointUrl => "invalid_endpoint_url",
            Self::InvalidEnv => "invalid_env",
            Self::InvalidGroupName => "invalid_group_name",
            Self::InvalidIdentifier => "invalid_identifier",
            Self::InvalidImageIndex => "invalid_image_index",
            Self::InvalidIndexUrl => "invalid_index_url",
            Self::InvalidLayerLayout => "invalid_layer_layout",
            Self::InvalidLayerRef => "invalid_layer_ref",
            Self::InvalidListSeparator => "invalid_list_separator",
            Self::InvalidLogoContent => "invalid_logo_content",
            Self::InvalidManagedConfigSource => "invalid_managed_config_source",
            Self::InvalidManifest => "invalid_manifest",
            Self::InvalidOwnerLogin => "invalid_owner_login",
            Self::InvalidPlatform => "invalid_platform",
            Self::InvalidRepoCoordinate => "invalid_repo_coordinate",
            Self::InvalidTag => "invalid_tag",
            Self::InvalidToolchainNameCharset => "invalid_toolchain_name_charset",
            Self::InvalidTrustPolicy => "invalid_trust_policy",
            Self::InvalidVersionSpec => "invalid_version_spec",
            Self::IssuerMismatch => "issuer_mismatch",
            Self::JsonSerialization => "json_serialization",
            Self::KeyBackendUnavailable => "key_backend_unavailable",
            Self::KeyMalformed => "key_malformed",
            Self::KeyReferenceInvalid => "key_reference_invalid",
            Self::KeyUnreadable => "key_unreadable",
            Self::LaunchIdentitiesInvalid => "launch_identities_invalid",
            Self::LaunchSpawnFailed => "launch_spawn_failed",
            Self::LauncherPathNotUtf8 => "launcher_path_not_utf8",
            Self::LauncherUnsafeCharacter => "launcher_unsafe_character",
            Self::LayerLayoutFailed => "layer_layout_failed",
            Self::LayerNotStaged => "layer_not_staged",
            Self::LayerSizeExceeded => "layer_size_exceeded",
            Self::LibcLintRead => "libc_lint_read",
            Self::LibcLintScanFailed => "libc_lint_scan_failed",
            Self::LibcScanScopeModifier => "libc_scan_scope_modifier",
            Self::LibcScanScopeUnresolvable => "libc_scan_scope_unresolvable",
            Self::LinkPathOccupied => "link_path_occupied",
            Self::ListSeparatorInvalid => "list_separator_invalid",
            Self::LockMissing => "lock_missing",
            Self::LockRepositoryNotBare => "lock_repository_not_bare",
            Self::LockStale => "lock_stale",
            Self::LoginRejected => "login_rejected",
            Self::MalformedCatalogKey => "malformed_catalog_key",
            Self::MalformedForkFullName => "malformed_fork_full_name",
            Self::MalformedIndexDocument => "malformed_index_document",
            Self::MalformedPhysicalRef => "malformed_physical_ref",
            Self::MalformedPhysicalRepository => "malformed_physical_repository",
            Self::MalformedRepository => "malformed_repository",
            Self::MalformedRootDocument => "malformed_root_document",
            Self::ManagedConfigContainsManagedSection => "managed_config_contains_managed_section",
            Self::ManagedConfigEntryTooLarge => "managed_config_entry_too_large",
            Self::ManagedConfigInputNotFound => "managed_config_input_not_found",
            Self::ManagedConfigInvalid => "managed_config_invalid",
            Self::ManagedConfigInvalidArchive => "managed_config_invalid_archive",
            Self::ManagedConfigInvalidToml => "managed_config_invalid_toml",
            Self::ManagedConfigKeyByPath => "managed_config_key_by_path",
            Self::ManagedConfigLayerDigestMismatch => "managed_config_layer_digest_mismatch",
            Self::ManagedConfigLayerSizeExceeded => "managed_config_layer_size_exceeded",
            Self::ManagedConfigMissingConfigToml => "managed_config_missing_config_toml",
            Self::ManagedConfigNoAnyPlatform => "managed_config_no_any_platform",
            Self::ManagedConfigNoGzipLayer => "managed_config_no_gzip_layer",
            Self::ManagedConfigPayloadInvalidToml => "managed_config_payload_invalid_toml",
            Self::ManagedConfigPayloadReadFailed => "managed_config_payload_read_failed",
            Self::ManagedConfigPayloadTooLarge => "managed_config_payload_too_large",
            Self::ManagedConfigSnapshotWrite => "managed_config_snapshot_write",
            Self::ManagedConfigSourceNotFound => "managed_config_source_not_found",
            Self::ManagedConfigStageFailed => "managed_config_stage_failed",
            Self::ManagedConfigUnexpectedManifest => "managed_config_unexpected_manifest",
            Self::ManagedConfigUpdateFailed => "managed_config_update_failed",
            Self::ManifestNotFound => "manifest_not_found",
            Self::MergeRequestUnconfirmed => "merge_request_unconfirmed",
            Self::MetadataAmbiguous => "metadata_ambiguous",
            Self::MetadataBlobTooLarge => "metadata_blob_too_large",
            Self::MetadataInvalidLayerPath => "metadata_invalid_layer_path",
            Self::MetadataRequired => "metadata_required",
            Self::MirrorConfigInvalid => "mirror_config_invalid",
            Self::MissingBaseRef => "missing_base_ref",
            Self::MissingDigest => "missing_digest",
            Self::MissingHeadRoot => "missing_head_root",
            Self::MissingListSeparator => "missing_list_separator",
            Self::MultipleAttestations => "multiple_attestations",
            Self::MultipleSignatures => "multiple_signatures",
            Self::NestedImageIndex => "nested_image_index",
            Self::NestedNamespaceUnsupported => "nested_namespace_unsupported",
            Self::NoActingIdentity => "no_acting_identity",
            Self::NoCredentialStore => "no_credential_store",
            Self::NoCuratedTags => "no_curated_tags",
            Self::NoHostLeaf => "no_host_leaf",
            Self::NoIdentityProvided => "no_identity_provided",
            Self::NoIndexableTag => "no_indexable_tag",
            Self::NoMatchingPlatform => "no_matching_platform",
            Self::NoProject => "no_project",
            Self::NoProjectIn => "no_project_in",
            Self::NoSignaturesFound => "no_signatures_found",
            Self::NoUsableBundle => "no_usable_bundle",
            Self::NonUtf8WireName => "non_utf8_wire_name",
            Self::NoncanonicalPlatformKey => "noncanonical_platform_key",
            Self::NotAManifest => "not_a_manifest",
            Self::NotInIndex => "not_in_index",
            Self::ObserveRaced => "observe_raced",
            Self::OfflineAttestRefused => "offline_attest_refused",
            Self::OfflineManifestMissing => "offline_manifest_missing",
            Self::OfflineMode => "offline_mode",
            Self::OfflineSignRefused => "offline_sign_refused",
            Self::OidcPreCheckFailed => "oidc_pre_check_failed",
            Self::OidcTokenRejected => "oidc_token_rejected",
            Self::OutputWrite => "output_write",
            Self::OwnerIdMismatch => "owner_id_mismatch",
            Self::OwnerUnknown => "owner_unknown",
            Self::PackageAuthoringInvalid => "package_authoring_invalid",
            Self::PackageFileIo => "package_file_io",
            Self::PackageInternal => "package_internal",
            Self::PackageKeyInvalid => "package_key_invalid",
            Self::PackageKeyMissingRegistry => "package_key_missing_registry",
            Self::PackageManagerInternalFile => "package_manager_internal_file",
            Self::PackageManagerSerialization => "package_manager_serialization",
            Self::PackageNotFound => "package_not_found",
            Self::PackageSerialization => "package_serialization",
            Self::PackageTaskFailed => "package_task_failed",
            Self::PatchBlobWriteFailed => "patch_blob_write_failed",
            Self::PatchConfigInvalid => "patch_config_invalid",
            Self::PatchDescriptorInvalidJson => "patch_descriptor_invalid_json",
            Self::PatchDescriptorTooLarge => "patch_descriptor_too_large",
            Self::PatchDescriptorUnsupportedVersion => "patch_descriptor_unsupported_version",
            Self::PatchDescriptorVanished => "patch_descriptor_vanished",
            Self::PatchDiscoveryFailed => "patch_discovery_failed",
            Self::PatchLayerDigestMismatch => "patch_layer_digest_mismatch",
            Self::PatchLayerSizeExceeded => "patch_layer_size_exceeded",
            Self::PatchManifestDigestMismatch => "patch_manifest_digest_mismatch",
            Self::PatchPolicyBlocked => "patch_policy_blocked",
            Self::PatchProjectConfigUnreadable => "patch_project_config_unreadable",
            Self::PatchSnapshotActive => "patch_snapshot_active",
            Self::PatchSnapshotDescriptorMissing => "patch_snapshot_descriptor_missing",
            Self::PatchSnapshotUnsupportedVersion => "patch_snapshot_unsupported_version",
            Self::PatchUnexpectedArtifactType => "patch_unexpected_artifact_type",
            Self::PatchUnexpectedLayerMediaType => "patch_unexpected_layer_media_type",
            Self::PatchUnexpectedManifest => "patch_unexpected_manifest",
            Self::PatchWrongLayerCount => "patch_wrong_layer_count",
            Self::PathEscape => "path_escape",
            Self::PayloadTypeUnsupported => "payload_type_unsupported",
            Self::PermissionDenied => "permission_denied",
            Self::PinDigestMismatch => "pin_digest_mismatch",
            Self::PinnedIdentifierMissingDigest => "pinned_identifier_missing_digest",
            Self::PlainHttpIndexNotAllowed => "plain_http_index_not_allowed",
            Self::PlatformAmbiguous => "platform_ambiguous",
            Self::PlatformRequired => "platform_required",
            Self::PredicateNotJson => "predicate_not_json",
            Self::PredicateTooLarge => "predicate_too_large",
            Self::PredicateTypeMismatch => "predicate_type_mismatch",
            Self::ProjectAlreadyExists => "project_already_exists",
            Self::ProjectEnvInvalidKey => "project_env_invalid_key",
            Self::ProjectEnvInvalidSeparator => "project_env_invalid_separator",
            Self::ProjectEnvInvalidValue => "project_env_invalid_value",
            Self::ProjectEnvPathSeparatorInValue => "project_env_path_separator_in_value",
            Self::ProjectEnvReservedKey => "project_env_reserved_key",
            Self::ProjectEnvSeparatorEdgedValue => "project_env_separator_edged_value",
            Self::ProjectEnvSeparatorOnNonList => "project_env_separator_on_non_list",
            Self::ProjectEnvUnknownModifier => "project_env_unknown_modifier",
            Self::ProjectEnvUnknownValueField => "project_env_unknown_value_field",
            Self::ProjectFileTooLarge => "project_file_too_large",
            Self::ProjectInternalFile => "project_internal_file",
            Self::ProjectIo => "project_io",
            Self::ProjectLoadFailed => "project_load_failed",
            Self::ProjectLocked => "project_locked",
            Self::ProjectManifestEditDiverged => "project_manifest_edit_diverged",
            Self::ProjectManifestNotEditable => "project_manifest_not_editable",
            Self::ProjectRegistryIo => "project_registry_io",
            Self::ProjectResolutionBlocked => "project_resolution_blocked",
            Self::ProjectTomlInvalid => "project_toml_invalid",
            Self::ProjectTomlSerialize => "project_toml_serialize",
            Self::ProvenanceVersionUnsupported => "provenance_version_unsupported",
            Self::PruneDigestTag => "prune_digest_tag",
            Self::PruneNoIndex => "prune_no_index",
            Self::PruneNotAPrerelease => "prune_not_a_prerelease",
            Self::PrunePackageNotBare => "prune_package_not_bare",
            Self::PruneRefusedDurable => "prune_refused_durable",
            Self::PruneRefusedPending => "prune_refused_pending",
            Self::PruneTagStillPresent => "prune_tag_still_present",
            Self::PullRequestUnmergeable => "pull_request_unmergeable",
            Self::PushOptionRefused => "push_option_refused",
            Self::RecordExemptionRefused => "record_exemption_refused",
            Self::RecordInputsIncomplete => "record_inputs_incomplete",
            Self::RecordNameNotAFilename => "record_name_not_a_filename",
            Self::RecordSerializeFailed => "record_serialize_failed",
            Self::RecordSinkSymlink => "record_sink_symlink",
            Self::RecordTemplateNotUnique => "record_template_not_unique",
            Self::RecordTemplateUnknownPlaceholder => "record_template_unknown_placeholder",
            Self::RecordWriteFailed => "record_write_failed",
            Self::RecordsRequiredWithoutSink => "records_required_without_sink",
            Self::ReferrersUnsupported => "referrers_unsupported",
            Self::RegistryAuthFailed => "registry_auth_failed",
            Self::RegistryDeleteUnsupported => "registry_delete_unsupported",
            Self::RegistryTransient => "registry_transient",
            Self::RegistryUnavailable => "registry_unavailable",
            Self::RegistryUnreachable => "registry_unreachable",
            Self::RekorInclusionProofAbsent => "rekor_inclusion_proof_absent",
            Self::RekorSetAbsentTsaPresent => "rekor_set_absent_tsa_present",
            Self::RekorSetInvalid => "rekor_set_invalid",
            Self::RekorSetMalformed => "rekor_set_malformed",
            Self::RekorUploadRequiredForKeyless => "rekor_upload_required_for_keyless",
            Self::RenderedConfigTooLarge => "rendered_config_too_large",
            Self::RepositoryEscapesIndexHome => "repository_escapes_index_home",
            Self::RepositoryMismatch => "repository_mismatch",
            Self::RepositoryNotFound => "repository_not_found",
            Self::RequiredPathMissing => "required_path_missing",
            Self::ReservedEnvKey => "reserved_env_key",
            Self::ReservedGroupName => "reserved_group_name",
            Self::ResolveTimeout => "resolve_timeout",
            Self::RetiredEnv => "retired_env",
            Self::RootNameMismatch => "root_name_mismatch",
            Self::RootRepositoryMismatch => "root_repository_mismatch",
            Self::SameFilesystemCheckFailed => "same_filesystem_check_failed",
            Self::SbomMediaTypeUnsupported => "sbom_media_type_unsupported",
            Self::SelectionAmbiguous => "selection_ambiguous",
            Self::SelfForkRefused => "self_fork_refused",
            Self::SeparatorEdgedListValue => "separator_edged_list_value",
            Self::SessionPathNotAbsolute => "session_path_not_absolute",
            Self::SessionPathNotUtf8 => "session_path_not_utf8",
            Self::SessionPathUnencodable => "session_path_unencodable",
            Self::SetupBootstrapFailed => "setup_bootstrap_failed",
            Self::SetupIo => "setup_io",
            Self::SetupProfileSubprocess => "setup_profile_subprocess",
            Self::ShellSectionInProject => "shell_section_in_project",
            Self::ShimClaimUnfulfilled => "shim_claim_unfulfilled",
            Self::ShimNameInvalid => "shim_name_invalid",
            Self::ShimNameNotClaimed => "shim_name_not_claimed",
            Self::ShimNamesNotEnumerable => "shim_names_not_enumerable",
            Self::ShortBlobRead => "short_blob_read",
            Self::SidecarRequiresSignature => "sidecar_requires_signature",
            Self::SignatureInvalid => "signature_invalid",
            Self::SimpleSigningClaimUnsupported => "simple_signing_claim_unsupported",
            Self::SingleflightAbandoned => "singleflight_abandoned",
            Self::SingleflightCapacityExceeded => "singleflight_capacity_exceeded",
            Self::SingleflightFailed => "singleflight_failed",
            Self::SingleflightTimeout => "singleflight_timeout",
            Self::SsrfForbiddenTarget => "ssrf_forbidden_target",
            Self::StatementSubjectAbsent => "statement_subject_absent",
            Self::StatementSubjectMismatch => "statement_subject_mismatch",
            Self::StatementSubjectWeakAlgorithm => "statement_subject_weak_algorithm",
            Self::StatementTypeUnsupported => "statement_type_unsupported",
            Self::SubjectDigestMismatch => "subject_digest_mismatch",
            Self::SubjectDigestUnsupported => "subject_digest_unsupported",
            Self::SymlinkNotFound => "symlink_not_found",
            Self::SymlinkRequiresTag => "symlink_requires_tag",
            Self::SymlinkWalkIo => "symlink_walk_io",
            Self::SystemConfigInvalid => "system_config_invalid",
            Self::TagNotAnImageIndex => "tag_not_an_image_index",
            Self::TagNotFound => "tag_not_found",
            Self::TargetNotAnIndex => "target_not_an_index",
            Self::TargetNotFound => "target_not_found",
            Self::TaskPanicked => "task_panicked",
            Self::TemplateAmbiguousDependencyRef => "template_ambiguous_dependency_ref",
            Self::TemplateAmbiguousSelfEnvRef => "template_ambiguous_self_env_ref",
            Self::TemplateDependencyNotInstalled => "template_dependency_not_installed",
            Self::TemplateDisallowedToken => "template_disallowed_token",
            Self::TemplateModifierNotApplicable => "template_modifier_not_applicable",
            Self::TemplateUndefinedSelfEnvRef => "template_undefined_self_env_ref",
            Self::TemplateUnknownDependencyRef => "template_unknown_dependency_ref",
            Self::TemplateUnknownField => "template_unknown_field",
            Self::TemplateUnknownModifier => "template_unknown_modifier",
            Self::TemplateUnknownToken => "template_unknown_token",
            Self::TemplateValueTooLarge => "template_value_too_large",
            Self::TlogBindingMismatch => "tlog_binding_mismatch",
            Self::TooManyAttestations => "too_many_attestations",
            Self::ToolchainDirGroupOrWorldWritable => "toolchain_dir_group_or_world_writable",
            Self::ToolchainDirInaccessible => "toolchain_dir_inaccessible",
            Self::ToolchainDirInsideGlobalHome => "toolchain_dir_inside_global_home",
            Self::ToolchainDirIsContainmentAnchor => "toolchain_dir_is_containment_anchor",
            Self::ToolchainDirNoContainmentAnchor => "toolchain_dir_no_containment_anchor",
            Self::ToolchainDirNotADirectory => "toolchain_dir_not_a_directory",
            Self::ToolchainDirNotOwnerOwned => "toolchain_dir_not_owner_owned",
            Self::ToolchainDirOutsideHome => "toolchain_dir_outside_home",
            Self::ToolchainDirParentComponent => "toolchain_dir_parent_component",
            Self::ToolchainDirRelative => "toolchain_dir_relative",
            Self::ToolchainDirSystemPrefix => "toolchain_dir_system_prefix",
            Self::ToolchainDirUnexpandable => "toolchain_dir_unexpandable",
            Self::ToolchainHomeNotAbsolute => "toolchain_home_not_absolute",
            Self::ToolchainNameControlCharacter => "toolchain_name_control_character",
            Self::ToolchainNameEmpty => "toolchain_name_empty",
            Self::ToolchainNamePathPrefix => "toolchain_name_path_prefix",
            Self::ToolchainNameRelative => "toolchain_name_relative",
            Self::ToolchainNameSeparator => "toolchain_name_separator",
            Self::ToolchainNameTrailingDotOrSpace => "toolchain_name_trailing_dot_or_space",
            Self::TransparencyBodyMismatch => "transparency_body_mismatch",
            Self::TransparencyLogKeyUnavailable => "transparency_log_key_unavailable",
            Self::TransparencyLogResponseInvalid => "transparency_log_response_invalid",
            Self::TransparencyLogUnavailable => "transparency_log_unavailable",
            Self::TransparencyLogUnreachable => "transparency_log_unreachable",
            Self::TraversalLimitExceeded => "traversal_limit_exceeded",
            Self::TrustPolicyInvalid => "trust_policy_invalid",
            Self::TrustRootLoad => "trust_root_load",
            Self::TrustRootUnavailable => "trust_root_unavailable",
            Self::TrustRootUnreadable => "trust_root_unreadable",
            Self::TrustedRootInvalid => "trusted_root_invalid",
            Self::TrustedRootReadFailed => "trusted_root_read_failed",
            Self::UnclaimedPackage => "unclaimed_package",
            Self::UndeclaredBinary => "undeclared_binary",
            Self::UndeclaredLibc => "undeclared_libc",
            Self::UnexpectedArtifactType => "unexpected_artifact_type",
            Self::UnexpectedLayerMediaType => "unexpected_layer_media_type",
            Self::UnexpectedManifestType => "unexpected_manifest_type",
            Self::UnfollowedRedirect => "unfollowed_redirect",
            Self::UnknownCompareStatus => "unknown_compare_status",
            Self::UnknownEnvModifier => "unknown_env_modifier",
            Self::UnknownGroup => "unknown_group",
            Self::UnknownGroupSection => "unknown_group_section",
            Self::UnparseableElf => "unparseable_elf",
            Self::UnrecognizedInterpreter => "unrecognized_interpreter",
            Self::UnresolvedTag => "unresolved_tag",
            Self::UnsafeDestination => "unsafe_destination",
            Self::UnsignedRejectedByPolicy => "unsigned_rejected_by_policy",
            Self::UnsignedTypeUnsupported => "unsigned_type_unsupported",
            Self::UnsupportedDeclarationHashVersion => "unsupported_declaration_hash_version",
            Self::UnsupportedIndexFormat => "unsupported_index_format",
            Self::UnsupportedKeyBackend => "unsupported_key_backend",
            Self::UnsupportedLockVersion => "unsupported_lock_version",
            Self::UnsupportedLogoFormat => "unsupported_logo_format",
            Self::UnsupportedMediaType => "unsupported_media_type",
            Self::UnsupportedTlogEntryKind => "unsupported_tlog_entry_kind",
            Self::UnyankTagNotCurated => "unyank_tag_not_curated",
            Self::UpdateSectionInProject => "update_section_in_project",
            Self::Usage => "usage",
            Self::UsersApiUnavailable => "users_api_unavailable",
            Self::VersionInvalid => "version_invalid",
            Self::WalkedDigestMismatch => "walked_digest_mismatch",
            Self::WrongLayerCount => "wrong_layer_count",
            Self::YankTagNotCurated => "yank_tag_not_curated",
            Self::YankUnyankOverlap => "yank_unyank_overlap",
            Self::YankedRefused => "yanked_refused",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "activation_failed" => Self::ActivationFailed,
            "agnostic_platform_libc_claim" => Self::AgnosticPlatformLibcClaim,
            "ambiguous_extra_ca_certs" => Self::AmbiguousExtraCaCerts,
            "ambiguous_host_leaf" => Self::AmbiguousHostLeaf,
            "ambiguous_trust_root" => Self::AmbiguousTrustRoot,
            "announce_failed" => Self::AnnounceFailed,
            "any_pin_not_advertised_as_any" => Self::AnyPinNotAdvertisedAsAny,
            "any_pin_provenance_unavailable" => Self::AnyPinProvenanceUnavailable,
            "archive_entry_escape" => Self::ArchiveEntryEscape,
            "archive_extraction_cap_exceeded" => Self::ArchiveExtractionCapExceeded,
            "archive_gnu_sparse_unsupported" => Self::ArchiveGnuSparseUnsupported,
            "archive_hard_link_escape" => Self::ArchiveHardLinkEscape,
            "archive_internal" => Self::ArchiveInternal,
            "archive_io" => Self::ArchiveIo,
            "archive_symlink_escape" => Self::ArchiveSymlinkEscape,
            "archive_tar_invalid" => Self::ArchiveTarInvalid,
            "archive_unsupported_format" => Self::ArchiveUnsupportedFormat,
            "archive_zip_invalid" => Self::ArchiveZipInvalid,
            "attestation_budget_exhausted" => Self::AttestationBudgetExhausted,
            "attestation_not_found" => Self::AttestationNotFound,
            "attestation_payload_too_large" => Self::AttestationPayloadTooLarge,
            "attestation_too_large" => Self::AttestationTooLarge,
            "auth_env_missing" => Self::AuthEnvMissing,
            "bin_scan_failed" => Self::BinScanFailed,
            "bin_scan_unsupported_host" => Self::BinScanUnsupportedHost,
            "binding_already_exists" => Self::BindingAlreadyExists,
            "binding_ambiguous" => Self::BindingAmbiguous,
            "binding_not_declared" => Self::BindingNotDeclared,
            "binding_not_found" => Self::BindingNotFound,
            "binding_value_invalid" => Self::BindingValueInvalid,
            "binding_value_missing_registry" => Self::BindingValueMissingRegistry,
            "blob_not_found" => Self::BlobNotFound,
            "bot_identity" => Self::BotIdentity,
            "builder_mismatch" => Self::BuilderMismatch,
            "bundle_parse_failed" => Self::BundleParseFailed,
            "candidate_limit_exhausted" => Self::CandidateLimitExhausted,
            "catalog_document_absent" => Self::CatalogDocumentAbsent,
            "cert_chain_invalid" => Self::CertChainInvalid,
            "certificate_validity_window" => Self::CertificateValidityWindow,
            "ci_export_write" => Self::CiExportWrite,
            "ci_file_write" => Self::CiFileWrite,
            "ci_missing_env" => Self::CiMissingEnv,
            "client_internal" => Self::ClientInternal,
            "client_io" => Self::ClientIo,
            "client_serialization" => Self::ClientSerialization,
            "command_not_executable" => Self::CommandNotExecutable,
            "command_not_found" => Self::CommandNotFound,
            "command_trampoline_refused" => Self::CommandTrampolineRefused,
            "committed_tags_dropped" => Self::CommittedTagsDropped,
            "compression_create" => Self::CompressionCreate,
            "compression_decode_only" => Self::CompressionDecodeOnly,
            "compression_engine_init" => Self::CompressionEngineInit,
            "compression_io" => Self::CompressionIo,
            "compression_open" => Self::CompressionOpen,
            "compression_unknown_format" => Self::CompressionUnknownFormat,
            "config_edit_io" => Self::ConfigEditIo,
            "config_edit_locked" => Self::ConfigEditLocked,
            "config_edit_malformed" => Self::ConfigEditMalformed,
            "config_edit_parse" => Self::ConfigEditParse,
            "config_edit_too_large" => Self::ConfigEditTooLarge,
            "config_file_not_found" => Self::ConfigFileNotFound,
            "config_file_too_large" => Self::ConfigFileTooLarge,
            "config_io" => Self::ConfigIo,
            "config_parse" => Self::ConfigParse,
            "credential_helper_failed" => Self::CredentialHelperFailed,
            "credential_helper_invalid_json" => Self::CredentialHelperInvalidJson,
            "credential_helper_not_on_path" => Self::CredentialHelperNotOnPath,
            "credential_helper_timeout" => Self::CredentialHelperTimeout,
            "credential_helper_unsafe_path" => Self::CredentialHelperUnsafePath,
            "credential_store_write" => Self::CredentialStoreWrite,
            "declared_binary_not_executable" => Self::DeclaredBinaryNotExecutable,
            "decompression_cap_exceeded" => Self::DecompressionCapExceeded,
            "delete_needs_tag" => Self::DeleteNeedsTag,
            "dependency_ambiguous_platform" => Self::DependencyAmbiguousPlatform,
            "dependency_conflict" => Self::DependencyConflict,
            "dependency_declaration_invalid" => Self::DependencyDeclarationInvalid,
            "dependency_manifest_not_found" => Self::DependencyManifestNotFound,
            "dependency_no_compatible_platform" => Self::DependencyNoCompatiblePlatform,
            "dependency_not_found" => Self::DependencyNotFound,
            "dependency_pin_resolution_failed" => Self::DependencyPinResolutionFailed,
            "dependency_pin_verification_failed" => Self::DependencyPinVerificationFailed,
            "dependency_pinned_to_index" => Self::DependencyPinnedToIndex,
            "dependency_routing_failed" => Self::DependencyRoutingFailed,
            "dependency_setup_failed" => Self::DependencySetupFailed,
            "desc_disappeared" => Self::DescDisappeared,
            "destination_check_io" => Self::DestinationCheckIo,
            "destination_not_a_directory" => Self::DestinationNotADirectory,
            "destination_not_empty" => Self::DestinationNotEmpty,
            "destination_through_symlink" => Self::DestinationThroughSymlink,
            "digest_mismatch" => Self::DigestMismatch,
            "digest_missing" => Self::DigestMissing,
            "direct_digest_pin_in_any_target" => Self::DirectDigestPinInAnyTarget,
            "dispatch_object_digest_mismatch" => Self::DispatchObjectDigestMismatch,
            "docker_credential_retrieval" => Self::DockerCredentialRetrieval,
            "duplicate_binding_across_groups" => Self::DuplicateBindingAcrossGroups,
            "duplicate_owner" => Self::DuplicateOwner,
            "duplicate_platform_key" => Self::DuplicatePlatformKey,
            "empty_group_filter" => Self::EmptyGroupFilter,
            "empty_push_set" => Self::EmptyPushSet,
            "endpoint_unresolvable" => Self::EndpointUnresolvable,
            "entrypoint_collision" => Self::EntrypointCollision,
            "extra_ca_certs_invalid" => Self::ExtraCaCertsInvalid,
            "extra_ca_certs_not_utf8" => Self::ExtraCaCertsNotUtf8,
            "extra_ca_certs_pem_invalid" => Self::ExtraCaCertsPemInvalid,
            "extra_ca_certs_read_failed" => Self::ExtraCaCertsReadFailed,
            "extra_ca_file_invalid" => Self::ExtraCaFileInvalid,
            "extra_ca_file_too_large" => Self::ExtraCaFileTooLarge,
            "extra_ca_unreadable" => Self::ExtraCaUnreadable,
            "extra_ca_value_invalid" => Self::ExtraCaValueInvalid,
            "extra_ca_value_too_large" => Self::ExtraCaValueTooLarge,
            "feature_mismatch" => Self::FeatureMismatch,
            "file_io" => Self::FileIo,
            "forbidden_registry_target" => Self::ForbiddenRegistryTarget,
            "forge_auth_failed" => Self::ForgeAuthFailed,
            "forge_capability_unavailable" => Self::ForgeCapabilityUnavailable,
            "forge_client_build" => Self::ForgeClientBuild,
            "forge_decode" => Self::ForgeDecode,
            "forge_failed" => Self::ForgeFailed,
            "forge_kind_unknown" => Self::ForgeKindUnknown,
            "forge_missing_field" => Self::ForgeMissingField,
            "forge_non_fast_forward" => Self::ForgeNonFastForward,
            "forge_publisher_not_allowlisted" => Self::ForgePublisherNotAllowlisted,
            "forge_push_access_denied" => Self::ForgePushAccessDenied,
            "forge_push_refused" => Self::ForgePushRefused,
            "forge_request_encode" => Self::ForgeRequestEncode,
            "forge_required" => Self::ForgeRequired,
            "forge_stale_lease" => Self::ForgeStaleLease,
            "forge_status" => Self::ForgeStatus,
            "forge_transient" => Self::ForgeTransient,
            "forge_transport_failed" => Self::ForgeTransportFailed,
            "forge_transport_operation_unsupported" => Self::ForgeTransportOperationUnsupported,
            "forge_transport_transient" => Self::ForgeTransportTransient,
            "forge_transport_unsupported" => Self::ForgeTransportUnsupported,
            "forge_unavailable" => Self::ForgeUnavailable,
            "fork_base_unreachable" => Self::ForkBaseUnreachable,
            "fork_field_missing" => Self::ForkFieldMissing,
            "fork_host_mismatch" => Self::ForkHostMismatch,
            "fork_not_ready" => Self::ForkNotReady,
            "fork_owner_mismatch" => Self::ForkOwnerMismatch,
            "fork_parent_absent" => Self::ForkParentAbsent,
            "fork_parent_mismatch" => Self::ForkParentMismatch,
            "forwarded_env_invalid" => Self::ForwardedEnvInvalid,
            "fulcio_bad_request" => Self::FulcioBadRequest,
            "fulcio_unavailable" => Self::FulcioUnavailable,
            "git_command_failed" => Self::GitCommandFailed,
            "git_push_failed" => Self::GitPushFailed,
            "git_unavailable" => Self::GitUnavailable,
            "group_holds_direct_binding" => Self::GroupHoldsDirectBinding,
            "host_resolution_failed" => Self::HostResolutionFailed,
            "identity_mismatch" => Self::IdentityMismatch,
            "identity_token_file_permissive" => Self::IdentityTokenFilePermissive,
            "index_dispatch_digest_mismatch" => Self::IndexDispatchDigestMismatch,
            "index_http_failed" => Self::IndexHttpFailed,
            "index_http_transient" => Self::IndexHttpTransient,
            "index_manifest_not_found" => Self::IndexManifestNotFound,
            "index_named_by_digest" => Self::IndexNamedByDigest,
            "index_path_invalid" => Self::IndexPathInvalid,
            "index_resolution_blocked" => Self::IndexResolutionBlocked,
            "index_root_missing_field" => Self::IndexRootMissingField,
            "index_root_not_object" => Self::IndexRootNotObject,
            "index_root_parse" => Self::IndexRootParse,
            "index_serialization" => Self::IndexSerialization,
            "index_singleflight_failed" => Self::IndexSingleflightFailed,
            "index_source_failed" => Self::IndexSourceFailed,
            "integration_namespace_invalid" => Self::IntegrationNamespaceInvalid,
            "integration_too_large" => Self::IntegrationTooLarge,
            "integrations_too_large" => Self::IntegrationsTooLarge,
            "internal" => Self::Internal,
            "internal_path_invalid" => Self::InternalPathInvalid,
            "invalid_auth_type" => Self::InvalidAuthType,
            "invalid_binary_name" => Self::InvalidBinaryName,
            "invalid_binding_name" => Self::InvalidBindingName,
            "invalid_boolean_string" => Self::InvalidBooleanString,
            "invalid_build_metadata" => Self::InvalidBuildMetadata,
            "invalid_digest" => Self::InvalidDigest,
            "invalid_encoding" => Self::InvalidEncoding,
            "invalid_endpoint_url" => Self::InvalidEndpointUrl,
            "invalid_env" => Self::InvalidEnv,
            "invalid_group_name" => Self::InvalidGroupName,
            "invalid_identifier" => Self::InvalidIdentifier,
            "invalid_image_index" => Self::InvalidImageIndex,
            "invalid_index_url" => Self::InvalidIndexUrl,
            "invalid_layer_layout" => Self::InvalidLayerLayout,
            "invalid_layer_ref" => Self::InvalidLayerRef,
            "invalid_list_separator" => Self::InvalidListSeparator,
            "invalid_logo_content" => Self::InvalidLogoContent,
            "invalid_managed_config_source" => Self::InvalidManagedConfigSource,
            "invalid_manifest" => Self::InvalidManifest,
            "invalid_owner_login" => Self::InvalidOwnerLogin,
            "invalid_platform" => Self::InvalidPlatform,
            "invalid_repo_coordinate" => Self::InvalidRepoCoordinate,
            "invalid_tag" => Self::InvalidTag,
            "invalid_toolchain_name_charset" => Self::InvalidToolchainNameCharset,
            "invalid_trust_policy" => Self::InvalidTrustPolicy,
            "invalid_version_spec" => Self::InvalidVersionSpec,
            "issuer_mismatch" => Self::IssuerMismatch,
            "json_serialization" => Self::JsonSerialization,
            "key_backend_unavailable" => Self::KeyBackendUnavailable,
            "key_malformed" => Self::KeyMalformed,
            "key_reference_invalid" => Self::KeyReferenceInvalid,
            "key_unreadable" => Self::KeyUnreadable,
            "launch_identities_invalid" => Self::LaunchIdentitiesInvalid,
            "launch_spawn_failed" => Self::LaunchSpawnFailed,
            "launcher_path_not_utf8" => Self::LauncherPathNotUtf8,
            "launcher_unsafe_character" => Self::LauncherUnsafeCharacter,
            "layer_layout_failed" => Self::LayerLayoutFailed,
            "layer_not_staged" => Self::LayerNotStaged,
            "layer_size_exceeded" => Self::LayerSizeExceeded,
            "libc_lint_read" => Self::LibcLintRead,
            "libc_lint_scan_failed" => Self::LibcLintScanFailed,
            "libc_scan_scope_modifier" => Self::LibcScanScopeModifier,
            "libc_scan_scope_unresolvable" => Self::LibcScanScopeUnresolvable,
            "link_path_occupied" => Self::LinkPathOccupied,
            "list_separator_invalid" => Self::ListSeparatorInvalid,
            "lock_missing" => Self::LockMissing,
            "lock_repository_not_bare" => Self::LockRepositoryNotBare,
            "lock_stale" => Self::LockStale,
            "login_rejected" => Self::LoginRejected,
            "malformed_catalog_key" => Self::MalformedCatalogKey,
            "malformed_fork_full_name" => Self::MalformedForkFullName,
            "malformed_index_document" => Self::MalformedIndexDocument,
            "malformed_physical_ref" => Self::MalformedPhysicalRef,
            "malformed_physical_repository" => Self::MalformedPhysicalRepository,
            "malformed_repository" => Self::MalformedRepository,
            "malformed_root_document" => Self::MalformedRootDocument,
            "managed_config_contains_managed_section" => Self::ManagedConfigContainsManagedSection,
            "managed_config_entry_too_large" => Self::ManagedConfigEntryTooLarge,
            "managed_config_input_not_found" => Self::ManagedConfigInputNotFound,
            "managed_config_invalid" => Self::ManagedConfigInvalid,
            "managed_config_invalid_archive" => Self::ManagedConfigInvalidArchive,
            "managed_config_invalid_toml" => Self::ManagedConfigInvalidToml,
            "managed_config_key_by_path" => Self::ManagedConfigKeyByPath,
            "managed_config_layer_digest_mismatch" => Self::ManagedConfigLayerDigestMismatch,
            "managed_config_layer_size_exceeded" => Self::ManagedConfigLayerSizeExceeded,
            "managed_config_missing_config_toml" => Self::ManagedConfigMissingConfigToml,
            "managed_config_no_any_platform" => Self::ManagedConfigNoAnyPlatform,
            "managed_config_no_gzip_layer" => Self::ManagedConfigNoGzipLayer,
            "managed_config_payload_invalid_toml" => Self::ManagedConfigPayloadInvalidToml,
            "managed_config_payload_read_failed" => Self::ManagedConfigPayloadReadFailed,
            "managed_config_payload_too_large" => Self::ManagedConfigPayloadTooLarge,
            "managed_config_snapshot_write" => Self::ManagedConfigSnapshotWrite,
            "managed_config_source_not_found" => Self::ManagedConfigSourceNotFound,
            "managed_config_stage_failed" => Self::ManagedConfigStageFailed,
            "managed_config_unexpected_manifest" => Self::ManagedConfigUnexpectedManifest,
            "managed_config_update_failed" => Self::ManagedConfigUpdateFailed,
            "manifest_not_found" => Self::ManifestNotFound,
            "merge_request_unconfirmed" => Self::MergeRequestUnconfirmed,
            "metadata_ambiguous" => Self::MetadataAmbiguous,
            "metadata_blob_too_large" => Self::MetadataBlobTooLarge,
            "metadata_invalid_layer_path" => Self::MetadataInvalidLayerPath,
            "metadata_required" => Self::MetadataRequired,
            "mirror_config_invalid" => Self::MirrorConfigInvalid,
            "missing_base_ref" => Self::MissingBaseRef,
            "missing_digest" => Self::MissingDigest,
            "missing_head_root" => Self::MissingHeadRoot,
            "missing_list_separator" => Self::MissingListSeparator,
            "multiple_attestations" => Self::MultipleAttestations,
            "multiple_signatures" => Self::MultipleSignatures,
            "nested_image_index" => Self::NestedImageIndex,
            "nested_namespace_unsupported" => Self::NestedNamespaceUnsupported,
            "no_acting_identity" => Self::NoActingIdentity,
            "no_credential_store" => Self::NoCredentialStore,
            "no_curated_tags" => Self::NoCuratedTags,
            "no_host_leaf" => Self::NoHostLeaf,
            "no_identity_provided" => Self::NoIdentityProvided,
            "no_indexable_tag" => Self::NoIndexableTag,
            "no_matching_platform" => Self::NoMatchingPlatform,
            "no_project" => Self::NoProject,
            "no_project_in" => Self::NoProjectIn,
            "no_signatures_found" => Self::NoSignaturesFound,
            "no_usable_bundle" => Self::NoUsableBundle,
            "non_utf8_wire_name" => Self::NonUtf8WireName,
            "noncanonical_platform_key" => Self::NoncanonicalPlatformKey,
            "not_a_manifest" => Self::NotAManifest,
            "not_in_index" => Self::NotInIndex,
            "observe_raced" => Self::ObserveRaced,
            "offline_attest_refused" => Self::OfflineAttestRefused,
            "offline_manifest_missing" => Self::OfflineManifestMissing,
            "offline_mode" => Self::OfflineMode,
            "offline_sign_refused" => Self::OfflineSignRefused,
            "oidc_pre_check_failed" => Self::OidcPreCheckFailed,
            "oidc_token_rejected" => Self::OidcTokenRejected,
            "output_write" => Self::OutputWrite,
            "owner_id_mismatch" => Self::OwnerIdMismatch,
            "owner_unknown" => Self::OwnerUnknown,
            "package_authoring_invalid" => Self::PackageAuthoringInvalid,
            "package_file_io" => Self::PackageFileIo,
            "package_internal" => Self::PackageInternal,
            "package_key_invalid" => Self::PackageKeyInvalid,
            "package_key_missing_registry" => Self::PackageKeyMissingRegistry,
            "package_manager_internal_file" => Self::PackageManagerInternalFile,
            "package_manager_serialization" => Self::PackageManagerSerialization,
            "package_not_found" => Self::PackageNotFound,
            "package_serialization" => Self::PackageSerialization,
            "package_task_failed" => Self::PackageTaskFailed,
            "patch_blob_write_failed" => Self::PatchBlobWriteFailed,
            "patch_config_invalid" => Self::PatchConfigInvalid,
            "patch_descriptor_invalid_json" => Self::PatchDescriptorInvalidJson,
            "patch_descriptor_too_large" => Self::PatchDescriptorTooLarge,
            "patch_descriptor_unsupported_version" => Self::PatchDescriptorUnsupportedVersion,
            "patch_descriptor_vanished" => Self::PatchDescriptorVanished,
            "patch_discovery_failed" => Self::PatchDiscoveryFailed,
            "patch_layer_digest_mismatch" => Self::PatchLayerDigestMismatch,
            "patch_layer_size_exceeded" => Self::PatchLayerSizeExceeded,
            "patch_manifest_digest_mismatch" => Self::PatchManifestDigestMismatch,
            "patch_policy_blocked" => Self::PatchPolicyBlocked,
            "patch_project_config_unreadable" => Self::PatchProjectConfigUnreadable,
            "patch_snapshot_active" => Self::PatchSnapshotActive,
            "patch_snapshot_descriptor_missing" => Self::PatchSnapshotDescriptorMissing,
            "patch_snapshot_unsupported_version" => Self::PatchSnapshotUnsupportedVersion,
            "patch_unexpected_artifact_type" => Self::PatchUnexpectedArtifactType,
            "patch_unexpected_layer_media_type" => Self::PatchUnexpectedLayerMediaType,
            "patch_unexpected_manifest" => Self::PatchUnexpectedManifest,
            "patch_wrong_layer_count" => Self::PatchWrongLayerCount,
            "path_escape" => Self::PathEscape,
            "payload_type_unsupported" => Self::PayloadTypeUnsupported,
            "permission_denied" => Self::PermissionDenied,
            "pin_digest_mismatch" => Self::PinDigestMismatch,
            "pinned_identifier_missing_digest" => Self::PinnedIdentifierMissingDigest,
            "plain_http_index_not_allowed" => Self::PlainHttpIndexNotAllowed,
            "platform_ambiguous" => Self::PlatformAmbiguous,
            "platform_required" => Self::PlatformRequired,
            "predicate_not_json" => Self::PredicateNotJson,
            "predicate_too_large" => Self::PredicateTooLarge,
            "predicate_type_mismatch" => Self::PredicateTypeMismatch,
            "project_already_exists" => Self::ProjectAlreadyExists,
            "project_env_invalid_key" => Self::ProjectEnvInvalidKey,
            "project_env_invalid_separator" => Self::ProjectEnvInvalidSeparator,
            "project_env_invalid_value" => Self::ProjectEnvInvalidValue,
            "project_env_path_separator_in_value" => Self::ProjectEnvPathSeparatorInValue,
            "project_env_reserved_key" => Self::ProjectEnvReservedKey,
            "project_env_separator_edged_value" => Self::ProjectEnvSeparatorEdgedValue,
            "project_env_separator_on_non_list" => Self::ProjectEnvSeparatorOnNonList,
            "project_env_unknown_modifier" => Self::ProjectEnvUnknownModifier,
            "project_env_unknown_value_field" => Self::ProjectEnvUnknownValueField,
            "project_file_too_large" => Self::ProjectFileTooLarge,
            "project_internal_file" => Self::ProjectInternalFile,
            "project_io" => Self::ProjectIo,
            "project_load_failed" => Self::ProjectLoadFailed,
            "project_locked" => Self::ProjectLocked,
            "project_manifest_edit_diverged" => Self::ProjectManifestEditDiverged,
            "project_manifest_not_editable" => Self::ProjectManifestNotEditable,
            "project_registry_io" => Self::ProjectRegistryIo,
            "project_resolution_blocked" => Self::ProjectResolutionBlocked,
            "project_toml_invalid" => Self::ProjectTomlInvalid,
            "project_toml_serialize" => Self::ProjectTomlSerialize,
            "provenance_version_unsupported" => Self::ProvenanceVersionUnsupported,
            "prune_digest_tag" => Self::PruneDigestTag,
            "prune_no_index" => Self::PruneNoIndex,
            "prune_not_a_prerelease" => Self::PruneNotAPrerelease,
            "prune_package_not_bare" => Self::PrunePackageNotBare,
            "prune_refused_durable" => Self::PruneRefusedDurable,
            "prune_refused_pending" => Self::PruneRefusedPending,
            "prune_tag_still_present" => Self::PruneTagStillPresent,
            "pull_request_unmergeable" => Self::PullRequestUnmergeable,
            "push_option_refused" => Self::PushOptionRefused,
            "record_exemption_refused" => Self::RecordExemptionRefused,
            "record_inputs_incomplete" => Self::RecordInputsIncomplete,
            "record_name_not_a_filename" => Self::RecordNameNotAFilename,
            "record_serialize_failed" => Self::RecordSerializeFailed,
            "record_sink_symlink" => Self::RecordSinkSymlink,
            "record_template_not_unique" => Self::RecordTemplateNotUnique,
            "record_template_unknown_placeholder" => Self::RecordTemplateUnknownPlaceholder,
            "record_write_failed" => Self::RecordWriteFailed,
            "records_required_without_sink" => Self::RecordsRequiredWithoutSink,
            "referrers_unsupported" => Self::ReferrersUnsupported,
            "registry_auth_failed" => Self::RegistryAuthFailed,
            "registry_delete_unsupported" => Self::RegistryDeleteUnsupported,
            "registry_transient" => Self::RegistryTransient,
            "registry_unavailable" => Self::RegistryUnavailable,
            "registry_unreachable" => Self::RegistryUnreachable,
            "rekor_inclusion_proof_absent" => Self::RekorInclusionProofAbsent,
            "rekor_set_absent_tsa_present" => Self::RekorSetAbsentTsaPresent,
            "rekor_set_invalid" => Self::RekorSetInvalid,
            "rekor_set_malformed" => Self::RekorSetMalformed,
            "rekor_upload_required_for_keyless" => Self::RekorUploadRequiredForKeyless,
            "rendered_config_too_large" => Self::RenderedConfigTooLarge,
            "repository_escapes_index_home" => Self::RepositoryEscapesIndexHome,
            "repository_mismatch" => Self::RepositoryMismatch,
            "repository_not_found" => Self::RepositoryNotFound,
            "required_path_missing" => Self::RequiredPathMissing,
            "reserved_env_key" => Self::ReservedEnvKey,
            "reserved_group_name" => Self::ReservedGroupName,
            "resolve_timeout" => Self::ResolveTimeout,
            "retired_env" => Self::RetiredEnv,
            "root_name_mismatch" => Self::RootNameMismatch,
            "root_repository_mismatch" => Self::RootRepositoryMismatch,
            "same_filesystem_check_failed" => Self::SameFilesystemCheckFailed,
            "sbom_media_type_unsupported" => Self::SbomMediaTypeUnsupported,
            "selection_ambiguous" => Self::SelectionAmbiguous,
            "self_fork_refused" => Self::SelfForkRefused,
            "separator_edged_list_value" => Self::SeparatorEdgedListValue,
            "session_path_not_absolute" => Self::SessionPathNotAbsolute,
            "session_path_not_utf8" => Self::SessionPathNotUtf8,
            "session_path_unencodable" => Self::SessionPathUnencodable,
            "setup_bootstrap_failed" => Self::SetupBootstrapFailed,
            "setup_io" => Self::SetupIo,
            "setup_profile_subprocess" => Self::SetupProfileSubprocess,
            "shell_section_in_project" => Self::ShellSectionInProject,
            "shim_claim_unfulfilled" => Self::ShimClaimUnfulfilled,
            "shim_name_invalid" => Self::ShimNameInvalid,
            "shim_name_not_claimed" => Self::ShimNameNotClaimed,
            "shim_names_not_enumerable" => Self::ShimNamesNotEnumerable,
            "short_blob_read" => Self::ShortBlobRead,
            "sidecar_requires_signature" => Self::SidecarRequiresSignature,
            "signature_invalid" => Self::SignatureInvalid,
            "simple_signing_claim_unsupported" => Self::SimpleSigningClaimUnsupported,
            "singleflight_abandoned" => Self::SingleflightAbandoned,
            "singleflight_capacity_exceeded" => Self::SingleflightCapacityExceeded,
            "singleflight_failed" => Self::SingleflightFailed,
            "singleflight_timeout" => Self::SingleflightTimeout,
            "ssrf_forbidden_target" => Self::SsrfForbiddenTarget,
            "statement_subject_absent" => Self::StatementSubjectAbsent,
            "statement_subject_mismatch" => Self::StatementSubjectMismatch,
            "statement_subject_weak_algorithm" => Self::StatementSubjectWeakAlgorithm,
            "statement_type_unsupported" => Self::StatementTypeUnsupported,
            "subject_digest_mismatch" => Self::SubjectDigestMismatch,
            "subject_digest_unsupported" => Self::SubjectDigestUnsupported,
            "symlink_not_found" => Self::SymlinkNotFound,
            "symlink_requires_tag" => Self::SymlinkRequiresTag,
            "symlink_walk_io" => Self::SymlinkWalkIo,
            "system_config_invalid" => Self::SystemConfigInvalid,
            "tag_not_an_image_index" => Self::TagNotAnImageIndex,
            "tag_not_found" => Self::TagNotFound,
            "target_not_an_index" => Self::TargetNotAnIndex,
            "target_not_found" => Self::TargetNotFound,
            "task_panicked" => Self::TaskPanicked,
            "template_ambiguous_dependency_ref" => Self::TemplateAmbiguousDependencyRef,
            "template_ambiguous_self_env_ref" => Self::TemplateAmbiguousSelfEnvRef,
            "template_dependency_not_installed" => Self::TemplateDependencyNotInstalled,
            "template_disallowed_token" => Self::TemplateDisallowedToken,
            "template_modifier_not_applicable" => Self::TemplateModifierNotApplicable,
            "template_undefined_self_env_ref" => Self::TemplateUndefinedSelfEnvRef,
            "template_unknown_dependency_ref" => Self::TemplateUnknownDependencyRef,
            "template_unknown_field" => Self::TemplateUnknownField,
            "template_unknown_modifier" => Self::TemplateUnknownModifier,
            "template_unknown_token" => Self::TemplateUnknownToken,
            "template_value_too_large" => Self::TemplateValueTooLarge,
            "tlog_binding_mismatch" => Self::TlogBindingMismatch,
            "too_many_attestations" => Self::TooManyAttestations,
            "toolchain_dir_group_or_world_writable" => Self::ToolchainDirGroupOrWorldWritable,
            "toolchain_dir_inaccessible" => Self::ToolchainDirInaccessible,
            "toolchain_dir_inside_global_home" => Self::ToolchainDirInsideGlobalHome,
            "toolchain_dir_is_containment_anchor" => Self::ToolchainDirIsContainmentAnchor,
            "toolchain_dir_no_containment_anchor" => Self::ToolchainDirNoContainmentAnchor,
            "toolchain_dir_not_a_directory" => Self::ToolchainDirNotADirectory,
            "toolchain_dir_not_owner_owned" => Self::ToolchainDirNotOwnerOwned,
            "toolchain_dir_outside_home" => Self::ToolchainDirOutsideHome,
            "toolchain_dir_parent_component" => Self::ToolchainDirParentComponent,
            "toolchain_dir_relative" => Self::ToolchainDirRelative,
            "toolchain_dir_system_prefix" => Self::ToolchainDirSystemPrefix,
            "toolchain_dir_unexpandable" => Self::ToolchainDirUnexpandable,
            "toolchain_home_not_absolute" => Self::ToolchainHomeNotAbsolute,
            "toolchain_name_control_character" => Self::ToolchainNameControlCharacter,
            "toolchain_name_empty" => Self::ToolchainNameEmpty,
            "toolchain_name_path_prefix" => Self::ToolchainNamePathPrefix,
            "toolchain_name_relative" => Self::ToolchainNameRelative,
            "toolchain_name_separator" => Self::ToolchainNameSeparator,
            "toolchain_name_trailing_dot_or_space" => Self::ToolchainNameTrailingDotOrSpace,
            "transparency_body_mismatch" => Self::TransparencyBodyMismatch,
            "transparency_log_key_unavailable" => Self::TransparencyLogKeyUnavailable,
            "transparency_log_response_invalid" => Self::TransparencyLogResponseInvalid,
            "transparency_log_unavailable" => Self::TransparencyLogUnavailable,
            "transparency_log_unreachable" => Self::TransparencyLogUnreachable,
            "traversal_limit_exceeded" => Self::TraversalLimitExceeded,
            "trust_policy_invalid" => Self::TrustPolicyInvalid,
            "trust_root_load" => Self::TrustRootLoad,
            "trust_root_unavailable" => Self::TrustRootUnavailable,
            "trust_root_unreadable" => Self::TrustRootUnreadable,
            "trusted_root_invalid" => Self::TrustedRootInvalid,
            "trusted_root_read_failed" => Self::TrustedRootReadFailed,
            "unclaimed_package" => Self::UnclaimedPackage,
            "undeclared_binary" => Self::UndeclaredBinary,
            "undeclared_libc" => Self::UndeclaredLibc,
            "unexpected_artifact_type" => Self::UnexpectedArtifactType,
            "unexpected_layer_media_type" => Self::UnexpectedLayerMediaType,
            "unexpected_manifest_type" => Self::UnexpectedManifestType,
            "unfollowed_redirect" => Self::UnfollowedRedirect,
            "unknown_compare_status" => Self::UnknownCompareStatus,
            "unknown_env_modifier" => Self::UnknownEnvModifier,
            "unknown_group" => Self::UnknownGroup,
            "unknown_group_section" => Self::UnknownGroupSection,
            "unparseable_elf" => Self::UnparseableElf,
            "unrecognized_interpreter" => Self::UnrecognizedInterpreter,
            "unresolved_tag" => Self::UnresolvedTag,
            "unsafe_destination" => Self::UnsafeDestination,
            "unsigned_rejected_by_policy" => Self::UnsignedRejectedByPolicy,
            "unsigned_type_unsupported" => Self::UnsignedTypeUnsupported,
            "unsupported_declaration_hash_version" => Self::UnsupportedDeclarationHashVersion,
            "unsupported_index_format" => Self::UnsupportedIndexFormat,
            "unsupported_key_backend" => Self::UnsupportedKeyBackend,
            "unsupported_lock_version" => Self::UnsupportedLockVersion,
            "unsupported_logo_format" => Self::UnsupportedLogoFormat,
            "unsupported_media_type" => Self::UnsupportedMediaType,
            "unsupported_tlog_entry_kind" => Self::UnsupportedTlogEntryKind,
            "unyank_tag_not_curated" => Self::UnyankTagNotCurated,
            "update_section_in_project" => Self::UpdateSectionInProject,
            "usage" => Self::Usage,
            "users_api_unavailable" => Self::UsersApiUnavailable,
            "version_invalid" => Self::VersionInvalid,
            "walked_digest_mismatch" => Self::WalkedDigestMismatch,
            "wrong_layer_count" => Self::WrongLayerCount,
            "yank_tag_not_curated" => Self::YankTagNotCurated,
            "yank_unyank_overlap" => Self::YankUnyankOverlap,
            "yanked_refused" => Self::YankedRefused,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for ErrorDetail {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for ErrorDetail {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ErrorDetail {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for ErrorDetail {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// The document a failed invocation prints on stdout under `--format json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ErrorDocument {
    /// The command that failed, as its words (`"package sign"`); empty when the command line named
    /// none.
    pub command: String,
    /// The process exit code.
    pub exit_code: ExitCode,
    /// What failed.
    pub error: ErrorBody,
}

impl Unknowns for ErrorDocument {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "command", &self.command);
        field(pointer, out, "exit_code", &self.exit_code);
        field(pointer, out, "error", &self.error);
    }
}

/// The process exit code.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ExitCode {
    /// The command succeeded
    Success,
    /// The command failed and no more specific code applies
    Failure,
    /// The command line is invalid: unknown flag, wrong argument count or bad syntax
    UsageError,
    /// Input data is malformed or fails verification
    DataError,
    /// A required service is unavailable and rerunning will not help
    Unavailable,
    /// A filesystem read or write failed
    IoError,
    /// A transient failure; the same command may succeed on retry
    TempFail,
    /// The operation was refused for lack of permission
    PermissionDenied,
    /// Configuration is invalid or incomplete
    ConfigError,
    /// A named package, tag, file or resource does not exist
    NotFound,
    /// Authentication failed or credentials are missing
    AuthError,
    /// A local policy or safeguard refused the operation; loosen the flag or pass --force
    PolicyBlocked,
    /// The operation as requested is not supported or not enabled by this registry, forge or build;
    /// retrying will not help
    Unsupported,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(i64),
}

impl ExitCode {
    /// The wire value.
    pub fn value(&self) -> i64 {
        match self {
            Self::Success => 0,
            Self::Failure => 1,
            Self::UsageError => 64,
            Self::DataError => 65,
            Self::Unavailable => 69,
            Self::IoError => 74,
            Self::TempFail => 75,
            Self::PermissionDenied => 77,
            Self::ConfigError => 78,
            Self::NotFound => 79,
            Self::AuthError => 80,
            Self::PolicyBlocked => 81,
            Self::Unsupported => 82,
            Self::Unknown(value) => *value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: i64) -> Self {
        match value {
            0 => Self::Success,
            1 => Self::Failure,
            64 => Self::UsageError,
            65 => Self::DataError,
            69 => Self::Unavailable,
            74 => Self::IoError,
            75 => Self::TempFail,
            77 => Self::PermissionDenied,
            78 => Self::ConfigError,
            79 => Self::NotFound,
            80 => Self::AuthError,
            81 => Self::PolicyBlocked,
            82 => Self::Unsupported,
            _ => Self::Unknown(value),
        }
    }
}

impl Serialize for ExitCode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i64(self.value())
    }
}

impl<'de> Deserialize<'de> for ExitCode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(i64::deserialize(deserializer)?))
    }
}

impl Unknowns for ExitCode {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// The `OCX_EXTRA_CA_CERTS` persistence outcome.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExtraCaCertsEntry {
    /// What the run did with the extra CA roots.
    pub status: ExtraCaCertsStatusKind,
    /// How many certificates the resolved value holds; absent on `not_configured` and
    /// `system_locked`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificates: Option<i64>,
}

impl Unknowns for ExtraCaCertsEntry {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "status", &self.status);
        field(pointer, out, "certificates", &self.certificates);
    }
}

/// What a run did with the extra CA roots.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ExtraCaCertsStatusKind {
    /// `OCX_EXTRA_CA_CERTS` is not set.
    NotConfigured,
    /// `config.toml` already holds the same roots.
    Unchanged,
    /// The roots were written to `config.toml`.
    Persisted,
    /// Dry run: the roots would be written to `config.toml`.
    WouldPersist,
    /// The system tier locks the setting, so nothing was validated or written.
    SystemLocked,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl ExtraCaCertsStatusKind {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::NotConfigured => "not_configured",
            Self::Unchanged => "unchanged",
            Self::Persisted => "persisted",
            Self::WouldPersist => "would_persist",
            Self::SystemLocked => "system_locked",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "not_configured" => Self::NotConfigured,
            "unchanged" => Self::Unchanged,
            "persisted" => Self::Persisted,
            "would_persist" => Self::WouldPersist,
            "system_locked" => Self::SystemLocked,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for ExtraCaCertsStatusKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for ExtraCaCertsStatusKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ExtraCaCertsStatusKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for ExtraCaCertsStatusKind {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// Flat view of the resolved dependency order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FlatDependencies {
    /// Every dependency in resolution order.
    pub items: Vec<FlatDependency>,
}

impl Unknowns for FlatDependencies {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "items", &self.items);
    }
}

/// One dependency in the flat view.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FlatDependency {
    /// The package, digest-pinned.
    pub identifier: PackageRef,
    /// The dependency's resolved visibility; a requested root is `public`.
    pub visibility: Visibility,
}

impl Unknowns for FlatDependency {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "identifier", &self.identifier);
        field(pointer, out, "visibility", &self.visibility);
    }
}

/// The forge the run wrote to, after `--forge`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Forge {
    /// GitHub.com or a GitHub Enterprise Server instance.
    Github,
    /// GitLab.com or a self-managed GitLab instance.
    Gitlab,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl Forge {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Github => "github",
            Self::Gitlab => "gitlab",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "github" => Self::Github,
            "gitlab" => Self::Gitlab,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for Forge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for Forge {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Forge {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for Forge {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// Which consent clause granted activation, and therefore how much it granted.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Grant {
    /// Clause 1 — a valid stamp for this directory whose source set covers the lock's. A human
    /// ran one of the stamp-writing commands here.
    Stamp,
    /// Clause 2 — every repository the store recorded for the lock's tools resolves to a source
    /// matching `\[shell.consent\] namespaces`, bounded by what this host actually pulled under
    /// that name rather than by what the lock claims.
    Namespace,
    /// Clause 3 — the canonical directory is covered by `\[shell.consent\] paths`: by an entry
    /// naming it exactly, or by a trailing-`/*` entry naming it or an ancestor of it and granting
    /// that whole subtree. Opens the project `\[env\]` channel, also for a checkout the entry never
    /// named.
    Path,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl Grant {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Stamp => "stamp",
            Self::Namespace => "namespace",
            Self::Path => "path",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "stamp" => Self::Stamp,
            "namespace" => Self::Namespace,
            "path" => Self::Path,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for Grant {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for Grant {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Grant {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for Grant {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// One group's declarations. `default` is a group like any other: the top-level `\[tools\]` and
/// `\[env\]` tables in `ocx.toml` ARE its tools and env.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GroupStatus {
    /// The group's bindings, keyed by binding name.
    pub tools: BTreeMap<String, ToolStatus>,
    /// This scope's `\[env\]` table alone — never merged with another scope's.
    pub env: BTreeMap<String, EnvValueOut>,
}

impl Unknowns for GroupStatus {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "tools", &self.tools);
        field(pointer, out, "env", &self.env);
    }
}

/// How the hand-off to the new binary's own `ocx self setup` ended, when it did not end cleanly.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum HandoffFailure {
    /// The new binary could not be started.
    SpawnFailed {
        /// The underlying error.
        detail: String,
    },
    /// The new binary's setup exited non-zero; `81` means it left a user-edited profile alone.
    Exited {
        /// The setup's exit code.
        exit_code: i64,
    },
    /// The new binary's setup was killed by a signal (Unix only).
    Signalled {
        /// The signal number.
        signal: i64,
    },
    /// A variant this SDK was not generated for: the whole object, as sent.
    Unknown(Value),
}

impl Serialize for HandoffFailure {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::SpawnFailed { detail: field_0 } => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "spawn_failed")?;
                object.serialize_entry("detail", field_0)?;
                object.end()
            }
            Self::Exited { exit_code: field_0 } => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "exited")?;
                object.serialize_entry("exit_code", field_0)?;
                object.end()
            }
            Self::Signalled { signal: field_0 } => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "signalled")?;
                object.serialize_entry("signal", field_0)?;
                object.end()
            }
            Self::Unknown(value) => value.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for HandoffFailure {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let Some(tag) = value.get("type").and_then(Value::as_str) else {
            return Err(D::Error::custom("a union object needs a string `type`"));
        };
        match tag {
            "spawn_failed" => {
                #[derive(Deserialize)]
                struct Fields {
                    detail: String,
                }
                let fields = payload::<Fields, D::Error>("spawn_failed", value)?;
                Ok(Self::SpawnFailed { detail: fields.detail })
            }
            "exited" => {
                #[derive(Deserialize)]
                struct Fields {
                    exit_code: i64,
                }
                let fields = payload::<Fields, D::Error>("exited", value)?;
                Ok(Self::Exited {
                    exit_code: fields.exit_code,
                })
            }
            "signalled" => {
                #[derive(Deserialize)]
                struct Fields {
                    signal: i64,
                }
                let fields = payload::<Fields, D::Error>("signalled", value)?;
                Ok(Self::Signalled { signal: fields.signal })
            }
            _ => Ok(Self::Unknown(value)),
        }
    }
}

impl Unknowns for HandoffFailure {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        match self {
            Self::SpawnFailed { detail: field_0 } => {
                field(pointer, out, "detail", field_0);
            }
            Self::Exited { exit_code: field_0 } => {
                field(pointer, out, "exit_code", field_0);
            }
            Self::Signalled { signal: field_0 } => {
                field(pointer, out, "signal", field_0);
            }
            Self::Unknown(_) => out.push(pointer.clone()),
        }
    }
}

/// The per-prompt hook's enablement, as its ladder resolved it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HookStatus {
    /// The deciding rung, rendered the way a user spells it.
    pub rung: String,
    /// The config tier that **actually** set it; present only when rung 4 decided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    /// The resolved answer; absent on rung 5, where "auto" is decided shell-side by the shim's
    /// interactivity probe, which a diagnostic cannot observe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

impl Unknowns for HookStatus {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "rung", &self.rung);
        field(pointer, out, "tier", &self.tier);
        field(pointer, out, "enabled", &self.enabled);
    }
}

/// Which rule produced an identity.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum IdentitySource {
    /// The forge answered: its users API, or its own account behind the credential.
    Resolved,
    /// A `LOGIN:ID` pair was taken on the operator's word because the users API was unreachable.
    Asserted,
    /// The CI environment's variables named the identity.
    CiEnvironment,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl IdentitySource {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Resolved => "resolved",
            Self::Asserted => "asserted",
            Self::CiEnvironment => "ci_environment",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "resolved" => Self::Resolved,
            "asserted" => Self::Asserted,
            "ci_environment" => Self::CiEnvironment,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for IdentitySource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for IdentitySource {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for IdentitySource {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for IdentitySource {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// A disagreement between the registry graph and the public index that publishes it.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum IndexFinding {
    /// The index committed a different digest than the alias points at today. Repair cannot fix
    /// this — announcing the tag can.
    Stale {
        /// The alias tag.
        tag: AliasTag,
        /// The digest the index committed.
        committed: Digest,
        /// The digest the alias points at today.
        live: Digest,
    },
    /// The registry carries the alias and the index has never recorded it.
    NotCommitted {
        /// The alias tag.
        tag: AliasTag,
    },
    /// A variant this SDK was not generated for: the whole object, as sent.
    Unknown(Value),
}

impl Serialize for IndexFinding {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Stale {
                tag: field_0,
                committed: field_1,
                live: field_2,
            } => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "stale")?;
                object.serialize_entry("tag", field_0)?;
                object.serialize_entry("committed", field_1)?;
                object.serialize_entry("live", field_2)?;
                object.end()
            }
            Self::NotCommitted { tag: field_0 } => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "not_committed")?;
                object.serialize_entry("tag", field_0)?;
                object.end()
            }
            Self::Unknown(value) => value.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for IndexFinding {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let Some(tag) = value.get("type").and_then(Value::as_str) else {
            return Err(D::Error::custom("a union object needs a string `type`"));
        };
        match tag {
            "stale" => {
                #[derive(Deserialize)]
                struct Fields {
                    tag: AliasTag,
                    committed: Digest,
                    live: Digest,
                }
                let fields = payload::<Fields, D::Error>("stale", value)?;
                Ok(Self::Stale {
                    tag: fields.tag,
                    committed: fields.committed,
                    live: fields.live,
                })
            }
            "not_committed" => {
                #[derive(Deserialize)]
                struct Fields {
                    tag: AliasTag,
                }
                let fields = payload::<Fields, D::Error>("not_committed", value)?;
                Ok(Self::NotCommitted { tag: fields.tag })
            }
            _ => Ok(Self::Unknown(value)),
        }
    }
}

impl Unknowns for IndexFinding {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        match self {
            Self::Stale {
                tag: field_0,
                committed: field_1,
                live: field_2,
            } => {
                field(pointer, out, "tag", field_0);
                field(pointer, out, "committed", field_1);
                field(pointer, out, "live", field_2);
            }
            Self::NotCommitted { tag: field_0 } => {
                field(pointer, out, "tag", field_0);
            }
            Self::Unknown(_) => out.push(pointer.clone()),
        }
    }
}

/// The report `ocx inspect` and `ocx package inspect` emit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InspectReport {
    /// The platform the run selected; absent when it selected none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<Platform>,
    /// One entry per inspected package, in request order.
    pub packages: Vec<PackageInspect>,
    /// The composed project-tier environment in application order; empty when nothing applies.
    pub env: Vec<EnvEntry>,
}

impl Unknowns for InspectReport {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "platform", &self.platform);
        field(pointer, out, "packages", &self.packages);
        field(pointer, out, "env", &self.env);
    }
}

/// A single install or select result entry for CLI output.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InstallEntry {
    /// The pinned identifier the package resolved to.
    pub identifier: PackageRef,
    /// The installed package's metadata.
    pub metadata: Metadata,
    /// The symlink written; absent when no host symlink was written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl Unknowns for InstallEntry {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "identifier", &self.identifier);
        field(pointer, out, "metadata", &self.metadata);
        field(pointer, out, "path", &self.path);
    }
}

/// Installed or selected packages.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Installs {
    /// One entry per package, keyed by the identifier as given, sorted by key.
    pub packages: BTreeMap<String, InstallEntry>,
}

impl Unknowns for Installs {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "packages", &self.packages);
    }
}

/// One admitted integration contribution, attributed to the declaring package.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IntegrationAttribution {
    /// The integration namespace the payload is keyed under.
    pub namespace: String,
    /// The declaring package; absent when attribution is unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// The interpolated payload, emitted exactly as composed.
    pub payload: OpaqueJson,
}

impl Unknowns for IntegrationAttribution {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "namespace", &self.namespace);
        field(pointer, out, "package", &self.package);
        field(pointer, out, "payload", &self.payload);
    }
}

/// A package's declared `integrations` map: namespace key → opaque payload.
pub type Integrations = BTreeMap<String, Value>;

/// What produced or verified a signature — the frozen `signatures\[\].key_backend` vocabulary.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum KeyBackendKind {
    /// A Fulcio-issued ephemeral certificate; no long-lived key.
    Keyless,
    /// A key read from the filesystem.
    File,
    /// A key PEM held in an environment variable.
    Env,
    /// AWS KMS.
    Awskms,
    /// Google Cloud KMS.
    Gcpkms,
    /// Azure Key Vault.
    Azurekms,
    /// HashiCorp Vault transit.
    Hashivault,
    /// A Kubernetes secret.
    K8s,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl KeyBackendKind {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Keyless => "keyless",
            Self::File => "file",
            Self::Env => "env",
            Self::Awskms => "awskms",
            Self::Gcpkms => "gcpkms",
            Self::Azurekms => "azurekms",
            Self::Hashivault => "hashivault",
            Self::K8s => "k8s",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "keyless" => Self::Keyless,
            "file" => Self::File,
            "env" => Self::Env,
            "awskms" => Self::Awskms,
            "gcpkms" => Self::Gcpkms,
            "azurekms" => Self::Azurekms,
            "hashivault" => Self::Hashivault,
            "k8s" => Self::K8s,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for KeyBackendKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for KeyBackendKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for KeyBackendKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for KeyBackendKind {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// A single layer descriptor from the inspected manifest (default mode) or the platform-selected
/// manifest (`--resolve`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Layer {
    /// The layer's digest.
    pub digest: Digest,
    /// The layer's media type.
    pub media_type: String,
    /// The layer's size; absent when the descriptor records a negative one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<ByteSize>,
}

impl Unknowns for Layer {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "digest", &self.digest);
        field(pointer, out, "media_type", &self.media_type);
        field(pointer, out, "size", &self.size);
    }
}

/// Aggregate counts of layer-push outcomes for a single package push.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayerCounts {
    /// Layers a cross-repository blob mount placed in the target repository without any upload.
    pub mounted: i64,
    /// Layers uploaded rather than mounted or verified.
    pub uploaded: i64,
    /// Layers referenced by digest and confirmed already present in the registry.
    pub verified: i64,
}

impl Unknowns for LayerCounts {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "mounted", &self.mounted);
        field(pointer, out, "uploaded", &self.uploaded);
        field(pointer, out, "verified", &self.verified);
    }
}

/// What a lazy advisory flags.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LazyAdvisoryKind {
    /// A `constant` or `list` variable interpolating `${installPath}`, which a tool may read before
    /// the package is materialized.
    InstallPathRootedNonPathVar,
    /// No `binaries` claim, so the deferred tool's launcher names cannot be enumerated.
    UndeclaredBinaries,
    /// A `path` value combining `${installPath}` with anything else, which the launcher cannot
    /// substitute.
    CombinedPathValue,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl LazyAdvisoryKind {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::InstallPathRootedNonPathVar => "install_path_rooted_non_path_var",
            Self::UndeclaredBinaries => "undeclared_binaries",
            Self::CombinedPathValue => "combined_path_value",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "install_path_rooted_non_path_var" => Self::InstallPathRootedNonPathVar,
            "undeclared_binaries" => Self::UndeclaredBinaries,
            "combined_path_value" => Self::CombinedPathValue,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for LazyAdvisoryKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for LazyAdvisoryKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for LazyAdvisoryKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for LazyAdvisoryKind {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// One advisory raised while composing a **deferred** tool, in wire shape.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LazyAdvisoryReport {
    /// What the advisory flags.
    pub kind: LazyAdvisoryKind,
    /// The deferred package the advisory concerns.
    pub package: String,
    /// The environment variable the advisory names; present for the two variable kinds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// The human rendering of the advisory.
    pub message: String,
}

impl Unknowns for LazyAdvisoryReport {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "kind", &self.kind);
        field(pointer, out, "package", &self.package);
        field(pointer, out, "key", &self.key);
        field(pointer, out, "message", &self.message);
    }
}

/// The decoded `__OCX_ENV_STATE` payload.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ledger {
    /// Schema version of the payload shape.
    pub v: i64,
    /// Watch-set fingerprint; the cached verdict expires when it changes.
    pub fp: String,
    /// The cached negative verdict: `inert` or `no_project`, never `activate`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<Verdict>,
    /// The config-tier paths in effect at compose time; omitted when empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tiers: Option<Vec<String>>,
    /// Digest of the ordered watch-path list baked into the shell's gate; omitted when empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ws: Option<String>,
    /// Digest of the deferred diagnostics the previous prompt printed; omitted when empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messages_fp: Option<String>,
    /// Scopes the carrier size cap dropped; omitted when empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub over_cap: Option<Vec<ScopeId>>,
    /// What each scope applied.
    pub scopes: Scopes,
}

impl Unknowns for Ledger {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "v", &self.v);
        field(pointer, out, "fp", &self.fp);
        field(pointer, out, "verdict", &self.verdict);
        field(pointer, out, "tiers", &self.tiers);
        field(pointer, out, "ws", &self.ws);
        field(pointer, out, "messages_fp", &self.messages_fp);
        field(pointer, out, "over_cap", &self.over_cap);
        field(pointer, out, "scopes", &self.scopes);
    }
}

/// One environment binding the reconciler applied, recorded literally.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LedgerEntry {
    /// Environment-variable name.
    pub key: String,
    /// The exact string ocx wrote, byte for byte.
    pub value: String,
    /// How the value combines.
    pub kind: ModifierKind,
    /// The effective list separator; present only for a `list` entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub separator: Option<String>,
}

impl Unknowns for LedgerEntry {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "key", &self.key);
        field(pointer, out, "value", &self.value);
        field(pointer, out, "kind", &self.kind);
        field(pointer, out, "separator", &self.separator);
    }
}

/// Whether every examined candidate was listed.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ListingStatus {
    /// Nothing was refused.
    Success,
    /// At least one candidate was refused; see `refused`.
    PartialFailure,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl ListingStatus {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Success => "success",
            Self::PartialFailure => "partial_failure",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "success" => Self::Success,
            "partial_failure" => Self::PartialFailure,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for ListingStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for ListingStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ListingStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for ListingStatus {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// The counts and the status a script branches on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ListingSummary {
    /// Whether any candidate was refused.
    pub status: ListingStatus,
    /// Which trust contract produced this listing: `verified` or `unverified`.
    pub verification: ListingVerification,
    /// `verified + unverified + refused` — every candidate the scan examined.
    pub total: i64,
    /// Attestations that passed every check.
    pub verified: i64,
    /// Documents no signature was checked for. Counted apart from `verified` so a script branches
    /// on the trust class instead of filtering the array — an unverified document is a real
    /// answer to "what SBOMs does this carry" and not a real answer to "who vouches for them".
    pub unverified: i64,
    /// Candidates examined and refused.
    pub refused: i64,
}

impl Unknowns for ListingSummary {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "status", &self.status);
        field(pointer, out, "verification", &self.verification);
        field(pointer, out, "total", &self.total);
        field(pointer, out, "verified", &self.verified);
        field(pointer, out, "unverified", &self.unverified);
        field(pointer, out, "refused", &self.refused);
    }
}

/// Which trust contract the whole listing was produced under.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ListingVerification {
    /// Signatures were checked against the resolved policies.
    Verified,
    /// Nothing was checked; every row is unverified.
    Unverified,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl ListingVerification {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Verified => "verified",
            Self::Unverified => "unverified",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "verified" => Self::Verified,
            "unverified" => Self::Unverified,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for ListingVerification {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for ListingVerification {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ListingVerification {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for ListingVerification {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// One located package: the directory `ocx package which` reports for it, and which kind of
/// directory that is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LocatedPath {
    /// The located directory.
    pub path: String,
    /// Whether `path` is a package root or a shim tree.
    pub kind: PathKind,
}

impl Unknowns for LocatedPath {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "path", &self.path);
        field(pointer, out, "kind", &self.kind);
    }
}

/// Located packages for `ocx package which`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LocatedPaths {
    /// Each located package, keyed by the identifier as given, in request order.
    pub paths: BTreeMap<String, LocatedPath>,
}

impl Unknowns for LocatedPaths {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "paths", &self.paths);
    }
}

/// One locked binding in the `ocx lock` report.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LockEntry {
    /// The binding's key in `ocx.toml`.
    pub binding: String,
    /// The owning group: `default` for the top-level `\[tools\]` table, else the `\[group.*\]` key.
    pub group: String,
    /// The host platform's leaf digest; absent when no leaf, or more than one, fits the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<Digest>,
    /// Every leaf the lock records, keyed by the canonical platform string
    /// (`os/arch\[/variant\]\[+feature,…\]` or `any`).
    pub platform_digests: BTreeMap<String, Digest>,
}

impl Unknowns for LockEntry {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "binding", &self.binding);
        field(pointer, out, "group", &self.group);
        field(pointer, out, "digest", &self.digest);
        field(pointer, out, "platform_digests", &self.platform_digests);
    }
}

/// Report emitted by `ocx lock`, `ocx add` and `ocx remove`: every locked binding.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LockReport {
    /// The locked bindings, in lock order.
    pub items: Vec<LockEntry>,
}

impl Unknowns for LockReport {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "items", &self.items);
    }
}

/// The `ocx.lock` header, plus whether the lock agrees with `ocx.toml`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LockStatus {
    /// Whether an `ocx.lock` exists beside `ocx.toml`.
    pub present: bool,
    /// Why the lock could not be parsed. Its presence IS the unreadable state — no separate
    /// boolean, which could only ever repeat what this key already says. The header fields and
    /// every binding's `platform_digests` are absent alongside it: nothing was read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// `true` when the lock binds to `ocx.toml`: its stored `declaration_hash` matches and every
    /// entry names its declared repository. `false` is the lock `ocx pull` and `ocx exec` refuse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<bool>,
    /// The lock file format version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_version: Option<i64>,
    /// The hash stored in `ocx.lock`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declaration_hash: Option<String>,
    /// The hash recomputed from `ocx.toml` — what the lock's stored hash would have to be for
    /// `current` to hold, so a consumer sees *why* `current` is false without recomputing the
    /// project's canonicalization.
    pub declaration_hash_expected: String,
    /// The ocx version that wrote the lock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_by: Option<String>,
    /// When the lock was written; absent when its recorded time is not an RFC 3339 instant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_at: Option<Timestamp>,
}

impl Unknowns for LockStatus {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "present", &self.present);
        field(pointer, out, "error", &self.error);
        field(pointer, out, "current", &self.current);
        field(pointer, out, "lock_version", &self.lock_version);
        field(pointer, out, "declaration_hash", &self.declaration_hash);
        field(
            pointer,
            out,
            "declaration_hash_expected",
            &self.declaration_hash_expected,
        );
        field(pointer, out, "generated_by", &self.generated_by);
        field(pointer, out, "generated_at", &self.generated_at);
    }
}

/// Successful `ocx login` result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoginResult {
    /// The registry the credentials were stored for.
    pub registry: RegistryHost,
    /// The user name the credentials were stored under.
    pub username: String,
}

impl Unknowns for LoginResult {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "registry", &self.registry);
        field(pointer, out, "username", &self.username);
    }
}

/// Successful `ocx logout` result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LogoutResult {
    /// The registry whose credentials were removed.
    pub registry: RegistryHost,
}

impl Unknowns for LogoutResult {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "registry", &self.registry);
    }
}

/// The managed-config adoption outcome, shared by `ocx self setup` and `ocx config setup`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ManagedConfigEntry {
    /// What the run did to the managed-config tier.
    pub status: ManagedConfigStatusKind,
    /// The adopted snapshot's digest, on every status that has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<Digest>,
    /// The digest the snapshot carried before a `refreshed` run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_digest: Option<Digest>,
    /// Why a `refresh_unavailable` run could not reach the registry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl Unknowns for ManagedConfigEntry {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "status", &self.status);
        field(pointer, out, "digest", &self.digest);
        field(pointer, out, "previous_digest", &self.previous_digest);
        field(pointer, out, "reason", &self.reason);
    }
}

/// What a run did to the managed-config tier.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ManagedConfigStatusKind {
    /// No managed config is configured.
    NotConfigured,
    /// The snapshot was already adopted and current.
    AlreadyAdopted,
    /// A snapshot was fetched and adopted.
    Adopted,
    /// The adopted snapshot was replaced by a newer one.
    Refreshed,
    /// The registry could not be reached; the adopted snapshot is kept.
    RefreshUnavailable,
    /// The managed-config tier was removed.
    Cleared,
    /// The managed block in `config.toml` carried user edits and was left untouched.
    Dirty,
    /// Dry run: a snapshot would be adopted.
    WouldAdopt,
    /// Dry run: the adopted snapshot would be refreshed.
    WouldRefresh,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl ManagedConfigStatusKind {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::NotConfigured => "not_configured",
            Self::AlreadyAdopted => "already_adopted",
            Self::Adopted => "adopted",
            Self::Refreshed => "refreshed",
            Self::RefreshUnavailable => "refresh_unavailable",
            Self::Cleared => "cleared",
            Self::Dirty => "dirty",
            Self::WouldAdopt => "would_adopt",
            Self::WouldRefresh => "would_refresh",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "not_configured" => Self::NotConfigured,
            "already_adopted" => Self::AlreadyAdopted,
            "adopted" => Self::Adopted,
            "refreshed" => Self::Refreshed,
            "refresh_unavailable" => Self::RefreshUnavailable,
            "cleared" => Self::Cleared,
            "dirty" => Self::Dirty,
            "would_adopt" => Self::WouldAdopt,
            "would_refresh" => Self::WouldRefresh,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for ManagedConfigStatusKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for ManagedConfigStatusKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ManagedConfigStatusKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for ManagedConfigStatusKind {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// The machine's own `\[managed\]` tier posture.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ManagedView {
    /// The effective managed-config source (env override, else the seed).
    pub source: String,
    /// Whether an absent or mismatched snapshot fails commands closed.
    pub required: bool,
}

impl Unknowns for ManagedView {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "source", &self.source);
        field(pointer, out, "required", &self.required);
    }
}

/// OCX package metadata.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Metadata {
    /// Bundle package metadata.
    Bundle {
        /// The version of the bundle metadata format. Reserved for future schema evolution.
        version: BundleMetadataVersion,
        /// Number of leading path components to strip when extracting the bundle. This is a
        /// convenient feature for archives not created by OCX, which often contain a single
        /// top-level directory. By default, OCX will not strip any components, and will extract the
        /// archive as-is.
        strip_components: Option<i64>,
        /// Environment variables the package declares, in declaration order.
        env: Option<Env>,
        /// Ordered list of package dependencies, pinned by digest. Array order defines environment
        /// import order.
        dependencies: Option<Dependencies>,
        /// Named entrypoints that `ocx install` generates launchers for.
        entrypoints: Option<Entrypoints>,
        /// Publisher-declared, unverified claim of interface-surface executable names exposed on
        /// `PATH` by this package. Absent means undeclared (predates this field); `\[\]` means the
        /// publisher asserts zero interface binaries. Deliberately distinct wire states.
        binaries: Option<Binaries>,
        /// Vendor-namespaced configuration blocks for tools OCX does not model. Keys are namespaces
        /// (reverse-DNS by convention, not enforced); values are opaque JSON OCX never interprets,
        /// merges, or validates the contents of. Absent and empty are the same state.
        integrations: Option<Integrations>,
    },
    /// A variant this SDK was not generated for: the whole object, as sent.
    Unknown(Value),
}

impl Serialize for Metadata {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Bundle {
                version: field_0,
                strip_components: field_1,
                env: field_2,
                dependencies: field_3,
                entrypoints: field_4,
                binaries: field_5,
                integrations: field_6,
            } => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "bundle")?;
                object.serialize_entry("version", field_0)?;
                if let Some(field_1) = field_1 {
                    object.serialize_entry("strip_components", field_1)?;
                }
                if let Some(field_2) = field_2 {
                    object.serialize_entry("env", field_2)?;
                }
                if let Some(field_3) = field_3 {
                    object.serialize_entry("dependencies", field_3)?;
                }
                if let Some(field_4) = field_4 {
                    object.serialize_entry("entrypoints", field_4)?;
                }
                if let Some(field_5) = field_5 {
                    object.serialize_entry("binaries", field_5)?;
                }
                if let Some(field_6) = field_6 {
                    object.serialize_entry("integrations", field_6)?;
                }
                object.end()
            }
            Self::Unknown(value) => value.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for Metadata {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let Some(tag) = value.get("type").and_then(Value::as_str) else {
            return Err(D::Error::custom("a union object needs a string `type`"));
        };
        match tag {
            "bundle" => {
                #[derive(Deserialize)]
                struct Fields {
                    version: BundleMetadataVersion,
                    #[serde(default, skip_serializing_if = "Option::is_none")]
                    strip_components: Option<i64>,
                    #[serde(default, skip_serializing_if = "Option::is_none")]
                    env: Option<Env>,
                    #[serde(default, skip_serializing_if = "Option::is_none")]
                    dependencies: Option<Dependencies>,
                    #[serde(default, skip_serializing_if = "Option::is_none")]
                    entrypoints: Option<Entrypoints>,
                    #[serde(default, skip_serializing_if = "Option::is_none")]
                    binaries: Option<Binaries>,
                    #[serde(default, skip_serializing_if = "Option::is_none")]
                    integrations: Option<Integrations>,
                }
                let fields = payload::<Fields, D::Error>("bundle", value)?;
                Ok(Self::Bundle {
                    version: fields.version,
                    strip_components: fields.strip_components,
                    env: fields.env,
                    dependencies: fields.dependencies,
                    entrypoints: fields.entrypoints,
                    binaries: fields.binaries,
                    integrations: fields.integrations,
                })
            }
            _ => Ok(Self::Unknown(value)),
        }
    }
}

impl Unknowns for Metadata {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        match self {
            Self::Bundle {
                version: field_0,
                strip_components: field_1,
                env: field_2,
                dependencies: field_3,
                entrypoints: field_4,
                binaries: field_5,
                integrations: field_6,
            } => {
                field(pointer, out, "version", field_0);
                field(pointer, out, "strip_components", field_1);
                field(pointer, out, "env", field_2);
                field(pointer, out, "dependencies", field_3);
                field(pointer, out, "entrypoints", field_4);
                field(pointer, out, "binaries", field_5);
                field(pointer, out, "integrations", field_6);
            }
            Self::Unknown(_) => out.push(pointer.clone()),
        }
    }
}

/// How a declared value combines with the variable's existing value.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ModifierKind {
    /// Prepended to any existing value of the variable.
    Path,
    /// Replaces any existing value of the variable.
    Constant,
    /// Appended to any existing value of the variable, joined by its separator.
    List,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl ModifierKind {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Path => "path",
            Self::Constant => "constant",
            Self::List => "list",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "path" => Self::Path,
            "constant" => Self::Constant,
            "list" => Self::List,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for ModifierKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for ModifierKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ModifierKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for ModifierKind {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// One integration namespace a closure node declares, attributed to the declaring package.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NamespaceAttribution {
    /// The integration namespace key.
    pub namespace: String,
    /// The declaring package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
}

impl Unknowns for NamespaceAttribution {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "namespace", &self.namespace);
        field(pointer, out, "package", &self.package);
    }
}

/// A reason row that is not an inertness reason, because it does not make the shell inert on its
/// own: it explains an answer the user would otherwise get wrong.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Note {
    /// The CWD walk skipped a symlinked `ocx.toml` candidate and promoted an ancestor.
    SymlinkedCandidateSkipped {
        /// The symlinked candidate that was skipped.
        candidate: String,
        /// The ancestor project activated in its place.
        ancestor: String,
    },
    /// Active via a `paths` grant, which is unconditional: source-set drift is **not** tracked for
    /// path grants.
    ActiveViaPathsGrant {
        /// The granting entry.
        entry: String,
    },
    /// A project file was reachable but could not be resolved (an unparseable `ocx.toml`, a
    /// canonicalization failure).
    ProjectUnresolved {
        /// The resolution failure, rendered for a human.
        detail: String,
    },
    /// A `paths` entry that would grant `canonical` if the compare folded ASCII case.
    PathsNearMiss {
        /// The entry that nearly matched.
        entry: String,
        /// The canonical project directory it was compared against.
        canonical: String,
    },
    /// A `\[shell.consent\] paths` entry that can never match any project by construction.
    PathsDefect {
        /// The malformed entry.
        entry: String,
        /// What is wrong with the entry, rendered for a human.
        defect: String,
    },
    /// The `ocx.toml` that answers for the reported scope exists but will not parse.
    ToolchainManifestUnparsed {
        /// The `ocx.toml` that would not parse — `$OCX_HOME/ocx.toml` for the global tier, the
        /// project's own otherwise.
        manifest: String,
        /// The parse failure, rendered for a human.
        detail: String,
    },
    /// A variant this SDK was not generated for: the whole object, as sent.
    Unknown(Value),
}

impl Serialize for Note {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::SymlinkedCandidateSkipped {
                candidate: field_0,
                ancestor: field_1,
            } => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "symlinked_candidate_skipped")?;
                object.serialize_entry("candidate", field_0)?;
                object.serialize_entry("ancestor", field_1)?;
                object.end()
            }
            Self::ActiveViaPathsGrant { entry: field_0 } => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "active_via_paths_grant")?;
                object.serialize_entry("entry", field_0)?;
                object.end()
            }
            Self::ProjectUnresolved { detail: field_0 } => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "project_unresolved")?;
                object.serialize_entry("detail", field_0)?;
                object.end()
            }
            Self::PathsNearMiss {
                entry: field_0,
                canonical: field_1,
            } => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "paths_near_miss")?;
                object.serialize_entry("entry", field_0)?;
                object.serialize_entry("canonical", field_1)?;
                object.end()
            }
            Self::PathsDefect {
                entry: field_0,
                defect: field_1,
            } => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "paths_defect")?;
                object.serialize_entry("entry", field_0)?;
                object.serialize_entry("defect", field_1)?;
                object.end()
            }
            Self::ToolchainManifestUnparsed {
                manifest: field_0,
                detail: field_1,
            } => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "toolchain_manifest_unparsed")?;
                object.serialize_entry("manifest", field_0)?;
                object.serialize_entry("detail", field_1)?;
                object.end()
            }
            Self::Unknown(value) => value.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for Note {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let Some(tag) = value.get("type").and_then(Value::as_str) else {
            return Err(D::Error::custom("a union object needs a string `type`"));
        };
        match tag {
            "symlinked_candidate_skipped" => {
                #[derive(Deserialize)]
                struct Fields {
                    candidate: String,
                    ancestor: String,
                }
                let fields = payload::<Fields, D::Error>("symlinked_candidate_skipped", value)?;
                Ok(Self::SymlinkedCandidateSkipped {
                    candidate: fields.candidate,
                    ancestor: fields.ancestor,
                })
            }
            "active_via_paths_grant" => {
                #[derive(Deserialize)]
                struct Fields {
                    entry: String,
                }
                let fields = payload::<Fields, D::Error>("active_via_paths_grant", value)?;
                Ok(Self::ActiveViaPathsGrant { entry: fields.entry })
            }
            "project_unresolved" => {
                #[derive(Deserialize)]
                struct Fields {
                    detail: String,
                }
                let fields = payload::<Fields, D::Error>("project_unresolved", value)?;
                Ok(Self::ProjectUnresolved { detail: fields.detail })
            }
            "paths_near_miss" => {
                #[derive(Deserialize)]
                struct Fields {
                    entry: String,
                    canonical: String,
                }
                let fields = payload::<Fields, D::Error>("paths_near_miss", value)?;
                Ok(Self::PathsNearMiss {
                    entry: fields.entry,
                    canonical: fields.canonical,
                })
            }
            "paths_defect" => {
                #[derive(Deserialize)]
                struct Fields {
                    entry: String,
                    defect: String,
                }
                let fields = payload::<Fields, D::Error>("paths_defect", value)?;
                Ok(Self::PathsDefect {
                    entry: fields.entry,
                    defect: fields.defect,
                })
            }
            "toolchain_manifest_unparsed" => {
                #[derive(Deserialize)]
                struct Fields {
                    manifest: String,
                    detail: String,
                }
                let fields = payload::<Fields, D::Error>("toolchain_manifest_unparsed", value)?;
                Ok(Self::ToolchainManifestUnparsed {
                    manifest: fields.manifest,
                    detail: fields.detail,
                })
            }
            _ => Ok(Self::Unknown(value)),
        }
    }
}

impl Unknowns for Note {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        match self {
            Self::SymlinkedCandidateSkipped {
                candidate: field_0,
                ancestor: field_1,
            } => {
                field(pointer, out, "candidate", field_0);
                field(pointer, out, "ancestor", field_1);
            }
            Self::ActiveViaPathsGrant { entry: field_0 } => {
                field(pointer, out, "entry", field_0);
            }
            Self::ProjectUnresolved { detail: field_0 } => {
                field(pointer, out, "detail", field_0);
            }
            Self::PathsNearMiss {
                entry: field_0,
                canonical: field_1,
            } => {
                field(pointer, out, "entry", field_0);
                field(pointer, out, "canonical", field_1);
            }
            Self::PathsDefect {
                entry: field_0,
                defect: field_1,
            } => {
                field(pointer, out, "entry", field_0);
                field(pointer, out, "defect", field_1);
            }
            Self::ToolchainManifestUnparsed {
                manifest: field_0,
                detail: field_1,
            } => {
                field(pointer, out, "manifest", field_0);
                field(pointer, out, "detail", field_1);
            }
            Self::Unknown(_) => out.push(pointer.clone()),
        }
    }
}

/// One live-session observation: which tool, and the signal that proved it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    /// The tool observed live in this shell.
    pub tool: Tool,
    /// The env-var evidence, rendered for a human.
    pub signal: String,
}

impl Unknowns for Observation {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "tool", &self.tool);
        field(pointer, out, "signal", &self.signal);
    }
}

/// OCI registry location in the format 'registry/repository\[:tag\]\[@digest\]'.
pub type OciIdentifier = String;

/// Publisher-supplied JSON, passed through unchanged.
pub type OpaqueJson = Value;

/// One forge account, as the claim root spells it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OwnerEntry {
    /// The forge's canonical login spelling.
    pub login: String,
    /// The forge's immutable numeric account id.
    pub id: i64,
}

impl Unknowns for OwnerEntry {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "login", &self.login);
        field(pointer, out, "id", &self.id);
    }
}

/// What `cascade check` found, one entry per package in input order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PackageCascadeCheck {
    /// One report per package, in input order: its alias states, slot rows, index findings and
    /// ignored tags.
    pub items: Vec<CascadeReport>,
}

impl Unknowns for PackageCascadeCheck {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "items", &self.items);
    }
}

/// What `cascade repair` did, one entry per package in input order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PackageCascadeRepair {
    /// One entry per package: the finding report, the planned writes, their outcomes and the `tags`
    /// this run left present.
    pub items: Vec<RepairEntry>,
    /// True when nothing was written because the run was a preview.
    pub dry_run: bool,
    /// Where `--tags-file` was written; absent when the flag was not passed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags_file: Option<String>,
}

impl Unknowns for PackageCascadeRepair {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "items", &self.items);
        field(pointer, out, "dry_run", &self.dry_run);
        field(pointer, out, "tags_file", &self.tags_file);
    }
}

/// One package's repository description.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PackageDescription {
    /// Whether the repository publishes a description; when `false` the three fields below are
    /// absent.
    pub published: bool,
    /// The description's title annotation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The description's one-line summary annotation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The description's keywords annotation, as published.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keywords: Option<String>,
}

impl Unknowns for PackageDescription {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "published", &self.published);
        field(pointer, out, "title", &self.title);
        field(pointer, out, "description", &self.description);
        field(pointer, out, "keywords", &self.keywords);
    }
}

/// The descriptions of every requested package.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PackageDescriptions {
    /// One entry per requested package, keyed by the identifier as given, in request order.
    pub descriptions: BTreeMap<String, PackageDescription>,
}

impl Unknowns for PackageDescriptions {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "descriptions", &self.descriptions);
    }
}

/// One inspected package. `pinned_identifier` and `pinned_digest` appear together, only when the
/// entry pinned one artifact. The other keys come from one shape: `candidates` (an image index or
/// an `ocx.lock` binding), `metadata` + `layers` (a manifest), or `platform` + `metadata` +
/// `layers` + `resolution` (a resolved package); `closure` joins the last two under `--closure`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PackageInspect {
    /// The package as the caller addressed it.
    pub name: String,
    /// The expanded request.
    pub identifier: PackageRef,
    /// The one artifact this entry pinned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_identifier: Option<PinnedPackageRef>,
    /// The digest of `pinned_identifier`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_digest: Option<Digest>,
    /// The platform children of an image index, or the platforms an `ocx.lock` binding pins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidates: Option<Vec<CandidateOut>>,
    /// The platform `--resolve` selected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<Platform>,
    /// The package metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Metadata>,
    /// The selected manifest's layers, in order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layers: Option<Vec<Layer>>,
    /// The resolution walk `--resolve` took.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<Resolution>,
    /// The dependency closure `--closure` computed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closure: Option<ClosureOut>,
}

impl Unknowns for PackageInspect {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "name", &self.name);
        field(pointer, out, "identifier", &self.identifier);
        field(pointer, out, "pinned_identifier", &self.pinned_identifier);
        field(pointer, out, "pinned_digest", &self.pinned_digest);
        field(pointer, out, "candidates", &self.candidates);
        field(pointer, out, "platform", &self.platform);
        field(pointer, out, "metadata", &self.metadata);
        field(pointer, out, "layers", &self.layers);
        field(pointer, out, "resolution", &self.resolution);
        field(pointer, out, "closure", &self.closure);
    }
}

/// Report emitted by `ocx package receipt`: what `ocx package create` recorded beside a bundle.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PackageReceipt {
    /// The `--platform` the bundle was built for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<Platform>,
    /// The `--identifier` the bundle was built to be pushed as.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identifier: Option<PackageRef>,
}

impl Unknowns for PackageReceipt {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "platform", &self.platform);
        field(pointer, out, "identifier", &self.identifier);
    }
}

/// OCI identifier in the format 'registry/repository\[:tag\]\[@digest\]'.
pub type PackageRef = String;

/// Per-package resolve-time policy from `\[package."<id>"\]`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PackageSettingsOut {
    /// Whether `no_patches` excludes the package from patch overlays.
    pub no_patches: bool,
}

impl Unknowns for PackageSettingsOut {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "no_patches", &self.no_patches);
    }
}

/// An OCX package version, e.g. `3.28.1`, a rolling `3.28`, `0.5.0-canary` or a variant
/// `debug-3.12.5`.
pub type PackageVersion = String;

/// Report emitted by `ocx \[--global\] patch freeze`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PatchFreezeReport {
    /// Number of companion packages pinned by the snapshot.
    pub companions: i64,
    /// Number of descriptor blobs pinned by the snapshot.
    pub descriptors: i64,
    /// Absolute path of the written `patches.snapshot.json` file.
    pub path: String,
}

impl Unknowns for PatchFreezeReport {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "companions", &self.companions);
        field(pointer, out, "descriptors", &self.descriptors);
        field(pointer, out, "path", &self.path);
    }
}

/// Report emitted by `ocx \[--global\] patch publish`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PatchPublishReport {
    /// Canonical reference the descriptor was published to (`registry/repository:__ocx.patch`).
    pub reference: String,
    /// Manifest digest of the pushed `__ocx.patch` artifact.
    pub manifest_digest: Digest,
    /// Number of rules in the published descriptor.
    pub rules: i64,
}

impl Unknowns for PatchPublishReport {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "reference", &self.reference);
        field(pointer, out, "manifest_digest", &self.manifest_digest);
        field(pointer, out, "rules", &self.rules);
    }
}

/// Report emitted by `ocx \[--global\] patch sync`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PatchSyncReport {
    /// Number of installed bases (plus global root) that were checked.
    pub bases_checked: i64,
    /// Number of descriptor blobs where the upstream digest advanced.
    pub descriptors_updated: i64,
    /// Number of companion packages installed or re-installed.
    pub companions_installed: i64,
}

impl Unknowns for PatchSyncReport {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "bases_checked", &self.bases_checked);
        field(pointer, out, "descriptors_updated", &self.descriptors_updated);
        field(pointer, out, "companions_installed", &self.companions_installed);
    }
}

/// A single composed environment variable entry shown by `ocx patch test`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PatchTestEntry {
    /// The variable name.
    pub key: String,
    /// The composed value.
    pub value: String,
    /// How the value folds into the environment.
    pub kind: ModifierKind,
    /// The separator a `list` entry folds with; omitted for every other `kind`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub separator: Option<String>,
    /// Provenance for a companion overlay entry; `None` for base-native entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<EntrySource>,
}

impl Unknowns for PatchTestEntry {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "key", &self.key);
        field(pointer, out, "value", &self.value);
        field(pointer, out, "kind", &self.kind);
        field(pointer, out, "separator", &self.separator);
        field(pointer, out, "source", &self.source);
    }
}

/// Report emitted by `ocx patch test` (env-inspection mode, no script/command).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PatchTestReport {
    /// The base identifier the descriptor was composed onto.
    pub base: String,
    /// Companion identifiers that matched the base under the descriptor's rules.
    pub companions: Vec<String>,
    /// Composed env entries: the base's interface surface (private under `--self`), then the
    /// companion overlay.
    pub items: Vec<PatchTestEntry>,
}

impl Unknowns for PatchTestReport {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "base", &self.base);
        field(pointer, out, "companions", &self.companions);
        field(pointer, out, "items", &self.items);
    }
}

/// A single env var an infrastructure-patch companion contributes to a base.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PatchWhyEntry {
    /// The env var name.
    pub variable: String,
    /// The descriptor rule `match` glob that admitted the companion.
    pub rule: String,
    /// The companion that produced the var.
    pub companion: String,
}

impl Unknowns for PatchWhyEntry {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "variable", &self.variable);
        field(pointer, out, "rule", &self.rule);
        field(pointer, out, "companion", &self.companion);
    }
}

/// `ocx patch why <base>`: each env var a companion contributes to `base`, with the matching rule
/// and companion. Empty is never an error: either no companion applies, or those that apply add
/// nothing to this surface.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PatchWhyReport {
    /// One entry per contributed variable; empty when no companion applies or none adds a var here.
    pub items: Vec<PatchWhyEntry>,
}

impl Unknowns for PatchWhyReport {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "items", &self.items);
    }
}

/// The `\[patches\]` tier as it would resolve after the candidate merges — defaults applied, so
/// an omitted field is reported as the value that would actually apply rather than as absent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PatchesView {
    /// The `\[patches\] registry` value: the registry, and optionally a repository prefix, hosting
    /// patch descriptors.
    pub repository_prefix: String,
    /// Path template for per-package patch repositories, placeholders intact.
    pub path_template: String,
    /// Whether an unavailable companion fails the launch.
    pub required: bool,
}

impl Unknowns for PatchesView {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "repository_prefix", &self.repository_prefix);
        field(pointer, out, "path_template", &self.path_template);
        field(pointer, out, "required", &self.required);
    }
}

/// What kind of directory a reported `path` names.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PathKind {
    /// The package root: the parent of `content/` and `entrypoints/`.
    Package,
    /// The generated shim directory of a deferred tool. Its `bin/` holds one launcher per declared
    /// name; the package directory does not exist yet.
    Shim,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl PathKind {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Package => "package",
            Self::Shim => "shim",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "package" => Self::Package,
            "shim" => Self::Shim,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for PathKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for PathKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for PathKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for PathKind {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// Resolved package roots for `ocx package pull`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Paths {
    /// Each package root, keyed by the identifier as given, in request order.
    pub paths: BTreeMap<String, String>,
}

impl Unknowns for Paths {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "paths", &self.paths);
    }
}

/// Pinned OCI identifier with a required digest and an optional advisory tag:
/// 'registry/repository\[:tag\]@digest'.
pub type PinnedPackageRef = String;

/// One alias index to write, recomputed whole.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlannedWrite {
    /// The alias tag to write.
    pub tag: AliasTag,
    /// The complete index to PUT at `tag`.
    pub index: Value,
    /// The digest the alias points at now — absent when the alias does not exist yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_digest: Option<Digest>,
    /// Every child manifest digest `index` references, deduped. Apply preflights each one so a
    /// missing child refuses the alias instead of publishing a dangling pointer.
    pub referenced_digests: Vec<String>,
    /// The rows that justify the write — what the user is being told changed.
    pub reasons: Vec<SlotRow>,
}

impl Unknowns for PlannedWrite {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "tag", &self.tag);
        field(pointer, out, "index", &self.index);
        field(pointer, out, "observed_digest", &self.observed_digest);
        field(pointer, out, "referenced_digests", &self.referenced_digests);
        field(pointer, out, "reasons", &self.reasons);
    }
}

/// An OCI image-spec platform object. `Platform::Any` writes os and architecture as "any".
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Platform {
    /// CPU architecture (`amd64`, `arm64`, …).
    pub architecture: String,
    /// Operating system (`linux`, `darwin`, `windows`, …).
    pub os: String,
    /// Operating system version the image requires.
    #[serde(rename = "os.version", default, skip_serializing_if = "Option::is_none")]
    pub os_version: Option<String>,
    /// Features the image requires of the host (`libc.glibc`, …).
    #[serde(rename = "os.features", default, skip_serializing_if = "Option::is_none")]
    pub os_features: Option<Vec<String>>,
    /// CPU variant (`v7`, `v8`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
}

impl Unknowns for Platform {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "architecture", &self.architecture);
        field(pointer, out, "os", &self.os);
        field(pointer, out, "os.version", &self.os_version);
        field(pointer, out, "os.features", &self.os_features);
        field(pointer, out, "variant", &self.variant);
    }
}

/// What a variable held before ocx set it.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Prior {
    /// The variable did not exist; reverting removes it.
    Unset,
    /// The variable held a value; reverting restores it.
    Value {
        /// The exact string the variable held.
        value: String,
    },
    /// A variant this SDK was not generated for: the whole object, as sent.
    Unknown(Value),
}

impl Serialize for Prior {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Unset => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "unset")?;
                object.end()
            }
            Self::Value { value: field_0 } => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "value")?;
                object.serialize_entry("value", field_0)?;
                object.end()
            }
            Self::Unknown(value) => value.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for Prior {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let Some(tag) = value.get("type").and_then(Value::as_str) else {
            return Err(D::Error::custom("a union object needs a string `type`"));
        };
        match tag {
            "unset" => Ok(Self::Unset),
            "value" => {
                #[derive(Deserialize)]
                struct Fields {
                    value: String,
                }
                let fields = payload::<Fields, D::Error>("value", value)?;
                Ok(Self::Value { value: fields.value })
            }
            _ => Ok(Self::Unknown(value)),
        }
    }
}

impl Unknowns for Prior {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        match self {
            Self::Unset => {}
            Self::Value { value: field_0 } => {
                field(pointer, out, "value", field_0);
            }
            Self::Unknown(_) => out.push(pointer.clone()),
        }
    }
}

/// Whether the project scope still holds the prior for one constant it owns.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PriorStatus {
    /// The constant's env key, as the carrier records it.
    pub key: String,
    /// Whether a prior is still recorded for it.
    pub intact: bool,
}

impl Unknowns for PriorStatus {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "key", &self.key);
        field(pointer, out, "intact", &self.intact);
    }
}

/// One shell profile and what this run did to its managed block.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProfileEntry {
    /// The profile file.
    pub path: String,
    /// What this run did to the profile's managed block.
    pub outcome: ProfileOutcomeKind,
}

impl Unknowns for ProfileEntry {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "path", &self.path);
        field(pointer, out, "outcome", &self.outcome);
    }
}

/// What a run did to one profile's managed block.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ProfileOutcomeKind {
    /// The managed block was written or upgraded.
    Completed,
    /// The managed block was already current.
    NoOp,
    /// A legacy footprint was migrated to the current fence.
    Migrated,
    /// The managed block carried user edits and was left untouched.
    SkippedDirty,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl ProfileOutcomeKind {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Completed => "completed",
            Self::NoOp => "no_op",
            Self::Migrated => "migrated",
            Self::SkippedDirty => "skipped_dirty",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "completed" => Self::Completed,
            "no_op" => Self::NoOp,
            "migrated" => Self::Migrated,
            "skipped_dirty" => Self::SkippedDirty,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for ProfileOutcomeKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for ProfileOutcomeKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ProfileOutcomeKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for ProfileOutcomeKind {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// The project scope's record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectScope {
    /// The project key derived from the canonical project directory.
    pub key: String,
    /// The canonical project directory, an advisory label.
    pub dir: String,
    /// What this scope applied, in emission order.
    pub applied: Vec<LedgerEntry>,
    /// Pre-apply values for this scope's constants, keyed by variable.
    pub priors: BTreeMap<String, Prior>,
}

impl Unknowns for ProjectScope {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "key", &self.key);
        field(pointer, out, "dir", &self.dir);
        field(pointer, out, "applied", &self.applied);
        field(pointer, out, "priors", &self.priors);
    }
}

/// The final state of one selected tag.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PruneAction {
    /// This run deleted it.
    Deleted,
    /// Already gone from the registry.
    Absent,
    /// A dry run would delete it.
    WouldDelete,
    /// Selected, then kept by `--keep-builds`.
    Kept,
    /// The safeguard refused it.
    Refused,
    /// Never reached: an earlier refusal or error stopped the run.
    NotAttempted,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl PruneAction {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Deleted => "deleted",
            Self::Absent => "absent",
            Self::WouldDelete => "would_delete",
            Self::Kept => "kept",
            Self::Refused => "refused",
            Self::NotAttempted => "not_attempted",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "deleted" => Self::Deleted,
            "absent" => Self::Absent,
            "would_delete" => Self::WouldDelete,
            "kept" => Self::Kept,
            "refused" => Self::Refused,
            "not_attempted" => Self::NotAttempted,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for PruneAction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for PruneAction {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for PruneAction {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for PruneAction {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// The index a run read, as reported.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PruneIndexReport {
    /// Where the served root was read from.
    pub url: String,
    /// sha256 of the root bytes as served.
    pub root_sha256: Digest,
}

impl Unknowns for PruneIndexReport {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "url", &self.url);
        field(pointer, out, "root_sha256", &self.root_sha256);
    }
}

/// What a run did, in processing order; printed whether or not the run failed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PruneOutcome {
    /// The package as named, without tag or digest.
    pub package: OciIdentifier,
    /// The repository the tags live in: the index's pointer, else `package`.
    pub repository: OciIdentifier,
    /// Which tags the run considered.
    pub selection: PruneSelection,
    /// Whether `--force` overrode the index safeguard.
    pub force: bool,
    /// Whether `--dry-run` was in force, so nothing was deleted.
    pub dry_run: bool,
    /// The index the safeguard judged; absent for a namespace with no configured index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<PruneIndexReport>,
    /// One row per selected tag, in processing order.
    pub tags: Vec<PruneTag>,
}

impl Unknowns for PruneOutcome {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "package", &self.package);
        field(pointer, out, "repository", &self.repository);
        field(pointer, out, "selection", &self.selection);
        field(pointer, out, "force", &self.force);
        field(pointer, out, "dry_run", &self.dry_run);
        field(pointer, out, "index", &self.index);
        field(pointer, out, "tags", &self.tags);
    }
}

/// Why a tag was kept or refused.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PruneReason {
    /// One of the newest `--keep-builds` builds.
    Newest,
    /// The pre-release's own tag, kept under `--keep-builds`.
    Rolling,
    /// The index lists it without the ephemeral marker.
    Durable,
    /// The registry has it but the index does not list it.
    NotInIndex,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl PruneReason {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Newest => "newest",
            Self::Rolling => "rolling",
            Self::Durable => "durable",
            Self::NotInIndex => "not_in_index",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "newest" => Self::Newest,
            "rolling" => Self::Rolling,
            "durable" => Self::Durable,
            "not_in_index" => Self::NotInIndex,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for PruneReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for PruneReason {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for PruneReason {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for PruneReason {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// Which tags a run considers.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum PruneSelection {
    /// Exactly these tags, deduplicated, in input order.
    Tags {
        /// The named tags.
        tags: Vec<String>,
    },
    /// Every build of one pre-release, plus its rolling tag.
    Prerelease {
        /// A pre-release without a build, e.g. `0.5.0-canary`.
        prerelease: PackageVersion,
        /// Keep the newest this many builds and the rolling tag; at least 1. Absent when every
        /// build is selected.
        keep_builds: Option<i64>,
    },
    /// A variant this SDK was not generated for: the whole object, as sent.
    Unknown(Value),
}

impl Serialize for PruneSelection {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Tags { tags: field_0 } => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "tags")?;
                object.serialize_entry("tags", field_0)?;
                object.end()
            }
            Self::Prerelease {
                prerelease: field_0,
                keep_builds: field_1,
            } => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "prerelease")?;
                object.serialize_entry("prerelease", field_0)?;
                if let Some(field_1) = field_1 {
                    object.serialize_entry("keep_builds", field_1)?;
                }
                object.end()
            }
            Self::Unknown(value) => value.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for PruneSelection {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let Some(tag) = value.get("type").and_then(Value::as_str) else {
            return Err(D::Error::custom("a union object needs a string `type`"));
        };
        match tag {
            "tags" => {
                #[derive(Deserialize)]
                struct Fields {
                    tags: Vec<String>,
                }
                let fields = payload::<Fields, D::Error>("tags", value)?;
                Ok(Self::Tags { tags: fields.tags })
            }
            "prerelease" => {
                #[derive(Deserialize)]
                struct Fields {
                    prerelease: PackageVersion,
                    #[serde(default, skip_serializing_if = "Option::is_none")]
                    keep_builds: Option<i64>,
                }
                let fields = payload::<Fields, D::Error>("prerelease", value)?;
                Ok(Self::Prerelease {
                    prerelease: fields.prerelease,
                    keep_builds: fields.keep_builds,
                })
            }
            _ => Ok(Self::Unknown(value)),
        }
    }
}

impl Unknowns for PruneSelection {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        match self {
            Self::Tags { tags: field_0 } => {
                field(pointer, out, "tags", field_0);
            }
            Self::Prerelease {
                prerelease: field_0,
                keep_builds: field_1,
            } => {
                field(pointer, out, "prerelease", field_0);
                field(pointer, out, "keep_builds", field_1);
            }
            Self::Unknown(_) => out.push(pointer.clone()),
        }
    }
}

/// One row of the report.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PruneTag {
    /// The selected tag.
    pub tag: String,
    /// The root row's content, else the digest the registry served; absent when neither is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<Digest>,
    /// The tag's final state.
    pub action: PruneAction,
    /// Why the tag was kept or refused; absent for every other action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<PruneReason>,
}

impl Unknowns for PruneTag {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "tag", &self.tag);
        field(pointer, out, "digest", &self.digest);
        field(pointer, out, "action", &self.action);
        field(pointer, out, "reason", &self.reason);
    }
}

/// Preview of what `ocx pull` would do without writing to the store.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PullDryRun {
    /// One entry per locked tool, in lock-file order.
    pub items: Vec<DryRunEntry>,
}

impl Unknowns for PullDryRun {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "items", &self.items);
    }
}

/// Whether a locked tool is already in the object store or would be fetched on a real `ocx pull`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PullStatus {
    /// Already in the object store.
    Cached,
    /// A real `ocx pull` would download it.
    WouldFetch,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl PullStatus {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Cached => "cached",
            Self::WouldFetch => "would_fetch",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "cached" => Self::Cached,
            "would_fetch" => Self::WouldFetch,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for PullStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for PullStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for PullStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for PullStatus {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// The push credential's kind; absent when the `api` transport pushed nothing.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PushCredentialKind {
    /// The push secret is this environment's own `CI_JOB_TOKEN`.
    JobToken,
    /// A push secret ocx injected but cannot classify further.
    Token,
    /// Nothing was injected; git's own credential helpers are in charge.
    GitHelper,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl PushCredentialKind {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::JobToken => "job_token",
            Self::Token => "token",
            Self::GitHelper => "git_helper",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "job_token" => Self::JobToken,
            "token" => Self::Token,
            "git_helper" => Self::GitHelper,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for PushCredentialKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for PushCredentialKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for PushCredentialKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for PushCredentialKind {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// Result of a successful `ocx package push`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PushReport {
    /// The pushed package, carrying the tag actually written — the `--build-timestamp` suffix
    /// included — not the tag the caller passed.
    pub identifier: PackageRef,
    /// Outcome of the push.
    pub status: PushStatus,
    /// Digest of the pushed multi-platform image index.
    pub manifest_digest: Digest,
    /// Rolling cascade tags written in addition to the primary version tag (e.g. `3.28`, `3`,
    /// `latest`). Empty for a non-cascade push.
    pub cascade_tags_written: Vec<String>,
    /// Digest-named `__ocx.keep.<algorithm>-<hex>` tags this push wrote, in push order, one per
    /// distinct platform manifest — platforms whose manifest is identical share a single tag, so
    /// this does not zip against the pushed platform list. Empty under `--no-keep-tag`. Reports
    /// what reached the registry, not what was requested.
    pub keep_tags_written: Vec<String>,
    /// Counts of layer-push outcomes (mounted/uploaded/verified), summed over every platform this
    /// push fanned out to. Layer blobs only — the config blob and manifest are not layers.
    pub layers: LayerCounts,
    /// Platform manifest digests this push produced, keyed by the canonical platform string
    /// (`os/arch\[/variant\]\[+feature,…\]`), sorted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform_digests: Option<BTreeMap<String, String>>,
    /// Every OCI annotation this push wrote onto the index of each tag it touched — the
    /// `--ci-annotations` set with the explicit `--annotation` pairs laid over it. Empty (and
    /// omitted) for a push that annotates nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations_written: Option<BTreeMap<String, String>>,
    /// The un-prefixed track tags `--default` aliased onto this push, bare version first (`1.2.3`,
    /// then `1.2`, `1`, `latest` under `--cascade`). Empty (and omitted) without the flag, and for
    /// a push whose tag carries no variant. JSON only. Each alias is an index write onto manifests
    /// this push already uploaded, never a second upload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aliases_written: Option<Vec<String>>,
    /// One row per platform manifest `--sign` signed inline, in push order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signatures: Option<Vec<SignedPlatformReport>>,
    /// Absent unless `--sbom` was passed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attestation: Option<AttestationOutcome>,
}

impl Unknowns for PushReport {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "identifier", &self.identifier);
        field(pointer, out, "status", &self.status);
        field(pointer, out, "manifest_digest", &self.manifest_digest);
        field(pointer, out, "cascade_tags_written", &self.cascade_tags_written);
        field(pointer, out, "keep_tags_written", &self.keep_tags_written);
        field(pointer, out, "layers", &self.layers);
        field(pointer, out, "platform_digests", &self.platform_digests);
        field(pointer, out, "annotations_written", &self.annotations_written);
        field(pointer, out, "aliases_written", &self.aliases_written);
        field(pointer, out, "signatures", &self.signatures);
        field(pointer, out, "attestation", &self.attestation);
    }
}

/// Outcome of a push.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PushStatus {
    /// The push landed; the registry merge is idempotent, so a repeat push lands too.
    Pushed,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl PushStatus {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Pushed => "pushed",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "pushed" => Self::Pushed,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for PushStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for PushStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for PushStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for PushStatus {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// Why a shell is not active — the enumerated set `ocx shell state` renders.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Reason {
    /// No valid stamp and no matching grant — with the project's derived source set and the
    /// grants it was tested against, so the user can see what to add. Also surfaces a `paths`
    /// **near-miss**: an entry differing from the canonical directory only by ASCII case.
    NoStampNoGrant {
        /// The source set derived from `ocx.lock`.
        derived_sources: Vec<String>,
        /// The `paths` entries compared against the canonical directory.
        paths_tested: Vec<String>,
        /// The `namespaces` patterns the derived sources were matched against.
        namespaces_tested: Vec<String>,
    },
    /// A stamp exists but the current lock's source set is not a subset of it — **naming the
    /// source that is new**.
    SourceSetDrift {
        /// Sources present in the lock and absent from the stamp.
        new_sources: Vec<String>,
    },
    /// Every source the lock *claims* matches `\[shell.consent\] namespaces`, but the package
    /// store's record of where the locked digests came from does not corroborate it, so clause 2
    /// refuses.
    UncorroboratedNamespace {
        /// The source set derived from `ocx.lock`'s repository fields.
        claimed_sources: Vec<String>,
        /// The source set derived from the store's recorded pull origins; absent when no complete
        /// record exists.
        verified_sources: Option<Vec<String>>,
    },
    /// The hook is disabled — naming which of the five enablement rungs decided it and the tier
    /// that set it, including the managed tier winning over a user's own file.
    HookDisabled {
        /// The deciding rung, rendered (`--no-hook`, `OCX_NO_HOOK`, `\[shell\] hook`, …).
        rung: String,
        /// The config tier that set it; present only when rung 4 decided.
        tier: Option<String>,
    },
    /// Yielded to another live per-prompt hook — naming the **live signal observed**. One row per
    /// observed tool.
    YieldedTo {
        /// The tool observed live in this shell.
        tool: Tool,
        /// The env-var evidence, rendered for a human.
        signal: String,
    },
    /// The ledger's payload exceeded the 16 KiB cap and this scope was abandoned, its record lost
    /// rather than repaired.
    LedgerOverCap {
        /// The scope whose payload was dropped.
        scope: ScopeId,
    },
    /// The carrier is absent, truncated, or carries an unrecognised envelope tag —
    /// **distinguishing** the first prompt of a shell (nothing applied, nothing to repair) from a
    /// corrupt carrier (a scope was applied and its record is gone).
    LedgerUnreadable {
        /// `true` for the ordinary first-prompt absence.
        first_prompt: bool,
    },
    /// `ocx.lock` is absent, unreadable or unparseable.
    LockUnavailable,
    /// A variant this SDK was not generated for: the whole object, as sent.
    Unknown(Value),
}

impl Serialize for Reason {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::NoStampNoGrant {
                derived_sources: field_0,
                paths_tested: field_1,
                namespaces_tested: field_2,
            } => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "no_stamp_no_grant")?;
                object.serialize_entry("derived_sources", field_0)?;
                object.serialize_entry("paths_tested", field_1)?;
                object.serialize_entry("namespaces_tested", field_2)?;
                object.end()
            }
            Self::SourceSetDrift { new_sources: field_0 } => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "source_set_drift")?;
                object.serialize_entry("new_sources", field_0)?;
                object.end()
            }
            Self::UncorroboratedNamespace {
                claimed_sources: field_0,
                verified_sources: field_1,
            } => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "uncorroborated_namespace")?;
                object.serialize_entry("claimed_sources", field_0)?;
                if let Some(field_1) = field_1 {
                    object.serialize_entry("verified_sources", field_1)?;
                }
                object.end()
            }
            Self::HookDisabled {
                rung: field_0,
                tier: field_1,
            } => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "hook_disabled")?;
                object.serialize_entry("rung", field_0)?;
                if let Some(field_1) = field_1 {
                    object.serialize_entry("tier", field_1)?;
                }
                object.end()
            }
            Self::YieldedTo {
                tool: field_0,
                signal: field_1,
            } => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "yielded_to")?;
                object.serialize_entry("tool", field_0)?;
                object.serialize_entry("signal", field_1)?;
                object.end()
            }
            Self::LedgerOverCap { scope: field_0 } => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "ledger_over_cap")?;
                object.serialize_entry("scope", field_0)?;
                object.end()
            }
            Self::LedgerUnreadable { first_prompt: field_0 } => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "ledger_unreadable")?;
                object.serialize_entry("first_prompt", field_0)?;
                object.end()
            }
            Self::LockUnavailable => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "lock_unavailable")?;
                object.end()
            }
            Self::Unknown(value) => value.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for Reason {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let Some(tag) = value.get("type").and_then(Value::as_str) else {
            return Err(D::Error::custom("a union object needs a string `type`"));
        };
        match tag {
            "no_stamp_no_grant" => {
                #[derive(Deserialize)]
                struct Fields {
                    derived_sources: Vec<String>,
                    paths_tested: Vec<String>,
                    namespaces_tested: Vec<String>,
                }
                let fields = payload::<Fields, D::Error>("no_stamp_no_grant", value)?;
                Ok(Self::NoStampNoGrant {
                    derived_sources: fields.derived_sources,
                    paths_tested: fields.paths_tested,
                    namespaces_tested: fields.namespaces_tested,
                })
            }
            "source_set_drift" => {
                #[derive(Deserialize)]
                struct Fields {
                    new_sources: Vec<String>,
                }
                let fields = payload::<Fields, D::Error>("source_set_drift", value)?;
                Ok(Self::SourceSetDrift {
                    new_sources: fields.new_sources,
                })
            }
            "uncorroborated_namespace" => {
                #[derive(Deserialize)]
                struct Fields {
                    claimed_sources: Vec<String>,
                    #[serde(default, skip_serializing_if = "Option::is_none")]
                    verified_sources: Option<Vec<String>>,
                }
                let fields = payload::<Fields, D::Error>("uncorroborated_namespace", value)?;
                Ok(Self::UncorroboratedNamespace {
                    claimed_sources: fields.claimed_sources,
                    verified_sources: fields.verified_sources,
                })
            }
            "hook_disabled" => {
                #[derive(Deserialize)]
                struct Fields {
                    rung: String,
                    #[serde(default, skip_serializing_if = "Option::is_none")]
                    tier: Option<String>,
                }
                let fields = payload::<Fields, D::Error>("hook_disabled", value)?;
                Ok(Self::HookDisabled {
                    rung: fields.rung,
                    tier: fields.tier,
                })
            }
            "yielded_to" => {
                #[derive(Deserialize)]
                struct Fields {
                    tool: Tool,
                    signal: String,
                }
                let fields = payload::<Fields, D::Error>("yielded_to", value)?;
                Ok(Self::YieldedTo {
                    tool: fields.tool,
                    signal: fields.signal,
                })
            }
            "ledger_over_cap" => {
                #[derive(Deserialize)]
                struct Fields {
                    scope: ScopeId,
                }
                let fields = payload::<Fields, D::Error>("ledger_over_cap", value)?;
                Ok(Self::LedgerOverCap { scope: fields.scope })
            }
            "ledger_unreadable" => {
                #[derive(Deserialize)]
                struct Fields {
                    first_prompt: bool,
                }
                let fields = payload::<Fields, D::Error>("ledger_unreadable", value)?;
                Ok(Self::LedgerUnreadable {
                    first_prompt: fields.first_prompt,
                })
            }
            "lock_unavailable" => Ok(Self::LockUnavailable),
            _ => Ok(Self::Unknown(value)),
        }
    }
}

impl Unknowns for Reason {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        match self {
            Self::NoStampNoGrant {
                derived_sources: field_0,
                paths_tested: field_1,
                namespaces_tested: field_2,
            } => {
                field(pointer, out, "derived_sources", field_0);
                field(pointer, out, "paths_tested", field_1);
                field(pointer, out, "namespaces_tested", field_2);
            }
            Self::SourceSetDrift { new_sources: field_0 } => {
                field(pointer, out, "new_sources", field_0);
            }
            Self::UncorroboratedNamespace {
                claimed_sources: field_0,
                verified_sources: field_1,
            } => {
                field(pointer, out, "claimed_sources", field_0);
                field(pointer, out, "verified_sources", field_1);
            }
            Self::HookDisabled {
                rung: field_0,
                tier: field_1,
            } => {
                field(pointer, out, "rung", field_0);
                field(pointer, out, "tier", field_1);
            }
            Self::YieldedTo {
                tool: field_0,
                signal: field_1,
            } => {
                field(pointer, out, "tool", field_0);
                field(pointer, out, "signal", field_1);
            }
            Self::LedgerOverCap { scope: field_0 } => {
                field(pointer, out, "scope", field_0);
            }
            Self::LedgerUnreadable { first_prompt: field_0 } => {
                field(pointer, out, "first_prompt", field_0);
            }
            Self::LockUnavailable => {}
            Self::Unknown(_) => out.push(pointer.clone()),
        }
    }
}

/// One candidate that was examined and refused.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RefusedEntry {
    /// The referrer's digest as the registry listed it. Absent when no well-formed digest names the
    /// candidate: a budget-stop row, or a registry listing a malformed digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referrer_digest: Option<Digest>,
    /// Why this candidate was refused, as prose for a human. Registry-sourced either way: several
    /// kinds quote a field read off the wire.
    pub reason: String,
    /// The same refusal as a frozen slug.
    pub reason_kind: String,
}

impl Unknowns for RefusedEntry {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "referrer_digest", &self.referrer_digest);
        field(pointer, out, "reason", &self.reason);
        field(pointer, out, "reason_kind", &self.reason_kind);
    }
}

/// One registry's drift-repair outcome.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RegenerateEntry {
    /// The registry whose catalog was rebuilt.
    pub registry: RegistryHost,
    /// The root documents the `p/` walk found.
    pub roots: i64,
    /// Packages the catalog gained.
    pub added: Vec<String>,
    /// Packages whose catalog row was rewritten.
    pub corrected: Vec<String>,
    /// Packages the catalog lost.
    pub removed: Vec<String>,
}

impl Unknowns for RegenerateEntry {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "registry", &self.registry);
        field(pointer, out, "roots", &self.roots);
        field(pointer, out, "added", &self.added);
        field(pointer, out, "corrected", &self.corrected);
        field(pointer, out, "removed", &self.removed);
    }
}

/// What `ocx index regenerate <REGISTRY>...` changed, per registry, in argument order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RegenerateReport {
    /// One entry per registry, in argument order.
    pub items: Vec<RegenerateEntry>,
}

impl Unknowns for RegenerateReport {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "items", &self.items);
    }
}

/// Registry authority, `host\[:port\]`.
pub type RegistryHost = String;

/// Results of an uninstall or deselect; `path` gets no plain column since it names something now
/// gone.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Removed {
    /// One entry per package, in request order.
    pub items: Vec<RemovedEntry>,
}

impl Unknowns for Removed {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "items", &self.items);
    }
}

/// A single uninstall or deselect result entry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RemovedEntry {
    /// The package as requested.
    pub package: String,
    /// What happened to it.
    pub status: RemovedStatus,
    /// What was removed; absent when nothing was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl Unknowns for RemovedEntry {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "package", &self.package);
        field(pointer, out, "status", &self.status);
        field(pointer, out, "path", &self.path);
    }
}

/// Whether the resource was actually removed, purged, or was already absent.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RemovedStatus {
    /// The symlink was removed.
    Removed,
    /// The package's object directory was deleted.
    Purged,
    /// Nothing was there to remove.
    Absent,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl RemovedStatus {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Removed => "removed",
            Self::Purged => "purged",
            Self::Absent => "absent",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "removed" => Self::Removed,
            "purged" => Self::Purged,
            "absent" => Self::Absent,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for RemovedStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for RemovedStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RemovedStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for RemovedStatus {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// One package's repair run: what was wrong, what the run planned, and what the registry accepted.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RepairEntry {
    /// The package's findings, the same report `cascade check` prints.
    pub report: CascadeReport,
    /// The alias writes this run planned.
    pub planned: Vec<PlannedWrite>,
    /// Empty for a preview run: nothing was attempted.
    pub outcomes: Vec<RepairOutcome>,
    /// The alias tags this run left present in the registry - the same lines written to
    /// `--tags-file`, echoed here so a JSON consumer gets them without reading the file back.
    pub tags: Vec<String>,
}

impl Unknowns for RepairEntry {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "report", &self.report);
        field(pointer, out, "planned", &self.planned);
        field(pointer, out, "outcomes", &self.outcomes);
        field(pointer, out, "tags", &self.tags);
    }
}

/// What happened to one alias tag in a repair run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RepairOutcome {
    /// The alias tag.
    pub tag: AliasTag,
    /// What the write did.
    pub outcome: WriteOutcome,
}

impl Unknowns for RepairOutcome {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "tag", &self.tag);
        field(pointer, out, "outcome", &self.outcome);
    }
}

/// One repository resolved to two or more distinct digests on the interface projection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RepositoryConflictOut {
    /// The contested repository.
    pub repository: String,
    /// Every digest it resolved to.
    pub digests: Vec<Digest>,
}

impl Unknowns for RepositoryConflictOut {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "repository", &self.repository);
        field(pointer, out, "digests", &self.digests);
    }
}

/// The OCI resolution chain for the selected platform. Carries only the walk (`index` →
/// `manifest` → `config`); the platform-selected manifest's layers are rendered alongside the
/// metadata, not inside the chain.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Resolution {
    /// The resolved artifact, digest-pinned.
    pub pinned: PinnedPackageRef,
    /// The blobs walked, in order: index when there is one, manifest, config.
    pub chain: Vec<ChainOut>,
}

impl Unknowns for Resolution {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "pinned", &self.pinned);
        field(pointer, out, "chain", &self.chain);
    }
}

/// Surfaced `RunResult` fields for the script's terminal/top-level `ocx.run`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunSummary {
    /// Child exit code (or `128 + signal` when signal-killed).
    pub exit_code: i64,
    /// Captured stdout (possibly truncated).
    pub stdout: String,
    /// Captured stderr (possibly truncated).
    pub stderr: String,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: i64,
    /// `true` iff stdout or stderr hit the capture cap.
    pub truncated: bool,
}

impl Unknowns for RunSummary {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "exit_code", &self.exit_code);
        field(pointer, out, "stdout", &self.stdout);
        field(pointer, out, "stderr", &self.stderr);
        field(pointer, out, "duration_ms", &self.duration_ms);
        field(pointer, out, "truncated", &self.truncated);
    }
}

/// One verified attestation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SbomEntry {
    /// predicateType. Read out of the **signed** payload when `verified`; derived from the
    /// referrer's `artifactType` otherwise, since an unsigned referrer states its type nowhere
    /// else.
    pub predicate_type: String,
    /// Whether a signature was verified over this document.
    pub verified: bool,
    /// `true` when a platform-level SBOM of the **same predicateType** supersedes this index-level
    /// one. A shadowed entry stays listed under `--format json`; only the human default collapses
    /// to the preferred one.
    pub shadowed: bool,
    /// The target digest. Proven bound by the signed Statement when `verified`; claimed by the
    /// referrer otherwise.
    pub subject_digest: Digest,
    /// What carried the document — **not always a manifest**.
    pub referrer_digest: Digest,
    /// Certificate SAN (identity) embedded in the Fulcio cert.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate_identity: Option<String>,
    /// Certificate OIDC issuer embedded in the Fulcio cert.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate_oidc_issuer: Option<String>,
    /// Rekor integrated time. Absent when no transparency record exists or its time is
    /// unrepresentable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed_at: Option<Timestamp>,
    /// Populated only under `--summary`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<SbomSummaryOut>,
}

impl Unknowns for SbomEntry {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "predicate_type", &self.predicate_type);
        field(pointer, out, "verified", &self.verified);
        field(pointer, out, "shadowed", &self.shadowed);
        field(pointer, out, "subject_digest", &self.subject_digest);
        field(pointer, out, "referrer_digest", &self.referrer_digest);
        field(pointer, out, "certificate_identity", &self.certificate_identity);
        field(pointer, out, "certificate_oidc_issuer", &self.certificate_oidc_issuer);
        field(pointer, out, "signed_at", &self.signed_at);
        field(pointer, out, "summary", &self.summary);
    }
}

/// Every verified attestation a package carries, plus what was refused.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SbomListingReport {
    /// One-glance counts, so a consumer branches on a field instead of measuring an array.
    pub summary: ListingSummary,
    /// One entry per listed attestation, in listing order.
    pub attestations: Vec<SbomEntry>,
    /// Every candidate examined and refused, in listing order. Never truncated in JSON.
    pub refused: Vec<RefusedEntry>,
}

impl Unknowns for SbomListingReport {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "summary", &self.summary);
        field(pointer, out, "attestations", &self.attestations);
        field(pointer, out, "refused", &self.refused);
    }
}

/// What `--summary` reports for one CycloneDX document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SbomSummaryOut {
    /// The document's own `specVersion`, verbatim.
    pub spec_version: String,
    /// `serialNumber`, when the document carries one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial_number: Option<String>,
    /// Length of the top-level `components` array.
    pub component_count: i64,
    /// `metadata.component.name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_level_component: Option<String>,
}

impl Unknowns for SbomSummaryOut {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "spec_version", &self.spec_version);
        field(pointer, out, "serial_number", &self.serial_number);
        field(pointer, out, "component_count", &self.component_count);
        field(pointer, out, "top_level_component", &self.top_level_component);
    }
}

/// Which scope a ledger datum belongs to: `global` or `project`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ScopeId {
    /// The `--global` toolchain tier.
    Global,
    /// The project resolved by the CWD walk.
    Project,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl ScopeId {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "global" => Self::Global,
            "project" => Self::Project,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for ScopeId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for ScopeId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ScopeId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for ScopeId {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// The two scope slots.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Scopes {
    /// The global toolchain tier's applied entries; absent when the tier applied nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global: Option<Vec<LedgerEntry>>,
    /// Pre-apply values for the global scope's constants, keyed by variable; omitted when empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global_priors: Option<BTreeMap<String, Prior>>,
    /// The resolved project tier; absent when none applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectScope>,
}

impl Unknowns for Scopes {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "global", &self.global);
        field(pointer, out, "global_priors", &self.global_priors);
        field(pointer, out, "project", &self.project);
    }
}

/// The `--script` result envelope.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScriptRunReport {
    /// Overall status (the structured mirror of the exit code).
    pub status: ScriptStatus,
    /// The terminating assertion record; absent unless the run failed on one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assertion: Option<AssertionRecord>,
    /// Surfaced `RunResult` fields; absent unless a terminal `ocx.run` is reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<RunSummary>,
}

impl Unknowns for ScriptRunReport {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "status", &self.status);
        field(pointer, out, "assertion", &self.assertion);
        field(pointer, out, "run", &self.run);
    }
}

/// Overall outcome of a scripted test run. Mirrors `ocx_script::ScriptOutcomeKind` at the
/// OCX-facing level.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ScriptStatus {
    /// Script ran to completion; all assertions passed.
    Passed,
    /// An assertion failed, `expect.fail`/`fail()`, or a host-fn failure.
    Failed,
    /// The script could not be used as given (unreadable script file).
    Usage,
    /// Syntax / arity / type error in the script source.
    ScriptError,
    /// A sandboxed filesystem operation failed for I/O reasons.
    Io,
    /// The wall-clock budget elapsed before the script finished.
    Timeout,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl ScriptStatus {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Usage => "usage",
            Self::ScriptError => "script_error",
            Self::Io => "io",
            Self::Timeout => "timeout",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "passed" => Self::Passed,
            "failed" => Self::Failed,
            "usage" => Self::Usage,
            "script_error" => Self::ScriptError,
            "io" => Self::Io,
            "timeout" => Self::Timeout,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for ScriptStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for ScriptStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ScriptStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for ScriptStatus {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// Report of `ocx self setup`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SelfSetupData {
    /// The run's overall outcome; `skipped` when a managed block was left dirty.
    pub status: SelfSetupStatus,
    /// Whether this run installed ocx into its own content store.
    pub bootstrap: BootstrapEntry,
    /// The env shim files this run wrote.
    pub shims: Vec<String>,
    /// One entry per shell profile this run considered.
    pub profiles: Vec<ProfileEntry>,
    /// One entry per session-PATH store this host owns, the skipped ones included; empty only where
    /// the platform has no session-PATH facility.
    pub session_path_stores: Vec<SessionPathEntry>,
    /// Profiles skipped because the user edited the managed block; present iff status = `skipped`.
    /// Carried separately for a script to `case` on without scanning the `profiles` list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dirty_profiles: Option<Vec<String>>,
    /// Windows execution-policy `Restricted` advisory, if applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_policy_warning: Option<String>,
    /// An `ocx` on `PATH` ahead of the directory the shim prepends, if found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflicting_ocx: Option<String>,
    /// Whether this run changed a PATH surface that the user must act on: a shim or managed profile
    /// block (re-source the shell), or a session-PATH store (log out and back in, so programs
    /// started outside a shell see it too).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reload_hint: Option<bool>,
    /// Result of adopting/clearing the `--managed-config` tier.
    pub managed_config: ManagedConfigEntry,
    /// Result of persisting `OCX_EXTRA_CA_CERTS` into `config.toml`.
    pub extra_ca_certs: ExtraCaCertsEntry,
}

impl Unknowns for SelfSetupData {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "status", &self.status);
        field(pointer, out, "bootstrap", &self.bootstrap);
        field(pointer, out, "shims", &self.shims);
        field(pointer, out, "profiles", &self.profiles);
        field(pointer, out, "session_path_stores", &self.session_path_stores);
        field(pointer, out, "dirty_profiles", &self.dirty_profiles);
        field(pointer, out, "exec_policy_warning", &self.exec_policy_warning);
        field(pointer, out, "conflicting_ocx", &self.conflicting_ocx);
        field(pointer, out, "reload_hint", &self.reload_hint);
        field(pointer, out, "managed_config", &self.managed_config);
        field(pointer, out, "extra_ca_certs", &self.extra_ca_certs);
    }
}

/// The overall outcome of an `ocx self setup` run; `skipped` exits 81, every other status 0.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SelfSetupStatus {
    /// Shims and/or profiles were written or upgraded.
    Completed,
    /// Nothing changed — every shim and profile was already current.
    NoOp,
    /// At least one profile carried user edits and was left untouched.
    Skipped,
    /// A legacy footprint was migrated to the v1 fence.
    Migrated,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl SelfSetupStatus {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Completed => "completed",
            Self::NoOp => "no_op",
            Self::Skipped => "skipped",
            Self::Migrated => "migrated",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "completed" => Self::Completed,
            "no_op" => Self::NoOp,
            "skipped" => Self::Skipped,
            "migrated" => Self::Migrated,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for SelfSetupStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for SelfSetupStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SelfSetupStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for SelfSetupStatus {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// Result of `ocx self update`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SelfUpdateData {
    /// What the update did.
    pub status: SelfUpdateStatus,
    /// Previously installed version; present on `installed` and `pulled` when the old binary's
    /// version query succeeded. Absent when that query was not available (binary absent, non-zero
    /// exit, malformed JSON output).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// Newly downloaded version; present iff status is `installed` or `pulled`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// Why the update was skipped; present iff status = `skipped`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped_reason: Option<SkippedReason>,
    /// How the hand-off to the new binary's own `ocx self setup` ended, when it did not end
    /// cleanly. Present on `installed` (the swap landed, some setup surface may not have been
    /// written) and on `pulled` (nothing was activated); absent whenever the child completed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff: Option<HandoffFailure>,
}

impl Unknowns for SelfUpdateData {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "status", &self.status);
        field(pointer, out, "from", &self.from);
        field(pointer, out, "to", &self.to);
        field(pointer, out, "skipped_reason", &self.skipped_reason);
        field(pointer, out, "handoff", &self.handoff);
    }
}

/// Outcome of an update check or a self-update.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SelfUpdateStatus {
    /// The installed version is the latest.
    UpToDate,
    /// The check did not run; `skipped_reason` says why.
    Skipped,
    /// A newer version is available (`--check`).
    UpdateAvailable,
    /// A newer version was pulled and activated.
    Installed,
    /// The release was downloaded but nothing was activated — `current` still names the
    /// previously installed binary.
    Pulled,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl SelfUpdateStatus {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::UpToDate => "up_to_date",
            Self::Skipped => "skipped",
            Self::UpdateAvailable => "update_available",
            Self::Installed => "installed",
            Self::Pulled => "pulled",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "up_to_date" => Self::UpToDate,
            "skipped" => Self::Skipped,
            "update_available" => Self::UpdateAvailable,
            "installed" => Self::Installed,
            "pulled" => Self::Pulled,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for SelfUpdateStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for SelfUpdateStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SelfUpdateStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for SelfUpdateStatus {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// One session-PATH store and what this run did to it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionPathEntry {
    /// Where the store lives: a file, or a registry location on Windows.
    pub location: String,
    /// What this run did to the store.
    pub outcome: SessionPathOutcomeKind,
}

impl Unknowns for SessionPathEntry {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "location", &self.location);
        field(pointer, out, "outcome", &self.outcome);
    }
}

/// What a run did to one session-PATH store.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SessionPathOutcomeKind {
    /// The ocx directories were written to the store.
    Written,
    /// The store already held the ocx directories.
    Unchanged,
    /// The ocx directories were removed from the store.
    Removed,
    /// Skipped: PATH modification is opted out.
    SkippedOptOut,
    /// Skipped: this host has no such store.
    SkippedUnsupported,
    /// Writing the store failed; the run warned and carried on.
    Failed,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl SessionPathOutcomeKind {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Written => "written",
            Self::Unchanged => "unchanged",
            Self::Removed => "removed",
            Self::SkippedOptOut => "skipped_opt_out",
            Self::SkippedUnsupported => "skipped_unsupported",
            Self::Failed => "failed",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "written" => Self::Written,
            "unchanged" => Self::Unchanged,
            "removed" => Self::Removed,
            "skipped_opt_out" => Self::SkippedOptOut,
            "skipped_unsupported" => Self::SkippedUnsupported,
            "failed" => Self::Failed,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for SessionPathOutcomeKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for SessionPathOutcomeKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SessionPathOutcomeKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for SessionPathOutcomeKind {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// What `ocx shell state` reports: all derived, none of it mutating.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShellStateReport {
    /// `$OCX_HOME`.
    pub ocx_home: AbsolutePath,
    /// Whether `$OCX_HOME` exists on disk.
    pub ocx_home_present: bool,
    /// Whether `ocx self setup` has ever wired this machine's shell.
    pub shell_integration_installed: bool,
    /// The **resolved toolchain home**, a machine-readable contract field.
    pub toolchain_home: AbsolutePath,
    /// The **PATH-facing trampoline directory** for the same tier, which a consumer outside ocx
    /// puts on `PATH`.
    pub toolchain_bin: String,
    /// The **effective** `activate` mode, past the whole ladder.
    pub activate: ActivateMode,
    /// The **effective** `pinned` value, resolved through the same ladder.
    pub pinned: bool,
    /// Why the project's `ocx.lock` refuses composition, when it does, as text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_refusal: Option<String>,
    /// Whether the `__OCX_ENV_STATE` carrier is set at all.
    pub carrier_present: bool,
    /// The carrier's encoded length in bytes, measured against the carrier cap.
    pub carrier_bytes: ByteSize,
    /// The decoded ledger, **rendered as fields, never as base64**.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ledger: Option<Ledger>,
    /// Whether the ledger's recorded `fp` still matches the watch set on disk now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint_current: Option<bool>,
    /// The watch set, with each member's presence, size and modification time.
    pub watch_set: Vec<WatchMember>,
    /// The project the CWD walk resolved, canonicalized; absent when none resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_dir: Option<AbsolutePath>,
    /// The 16-hex state key of `project_dir`; absent with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_key: Option<String>,
    /// Whether a usable consent stamp exists for that key; an unusable stamp counts as absent.
    pub project_stamped: bool,
    /// **Which clause activated this project**; absent when it is inert.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant: Option<Grant>,
    /// When the consent stamp was written, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stamp_written_at: Option<Timestamp>,
    /// Prior intactness for each constant the ledger's project scope owns.
    pub priors: Vec<PriorStatus>,
    /// The hook's enablement and the rung that decided it.
    pub hook: HookStatus,
    /// Every coexisting tool observed live in this shell.
    pub yielded_to: Vec<Observation>,
    /// Why the shell is not active, when it is not — **the command's reason to exist**. Absent
    /// when the project is active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inert_reason: Option<Reason>,
    /// Reason rows that explain an answer without being an inertness verdict of their own.
    pub notes: Vec<Note>,
}

impl Unknowns for ShellStateReport {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "ocx_home", &self.ocx_home);
        field(pointer, out, "ocx_home_present", &self.ocx_home_present);
        field(
            pointer,
            out,
            "shell_integration_installed",
            &self.shell_integration_installed,
        );
        field(pointer, out, "toolchain_home", &self.toolchain_home);
        field(pointer, out, "toolchain_bin", &self.toolchain_bin);
        field(pointer, out, "activate", &self.activate);
        field(pointer, out, "pinned", &self.pinned);
        field(pointer, out, "lock_refusal", &self.lock_refusal);
        field(pointer, out, "carrier_present", &self.carrier_present);
        field(pointer, out, "carrier_bytes", &self.carrier_bytes);
        field(pointer, out, "ledger", &self.ledger);
        field(pointer, out, "fingerprint_current", &self.fingerprint_current);
        field(pointer, out, "watch_set", &self.watch_set);
        field(pointer, out, "project_dir", &self.project_dir);
        field(pointer, out, "project_key", &self.project_key);
        field(pointer, out, "project_stamped", &self.project_stamped);
        field(pointer, out, "grant", &self.grant);
        field(pointer, out, "stamp_written_at", &self.stamp_written_at);
        field(pointer, out, "priors", &self.priors);
        field(pointer, out, "hook", &self.hook);
        field(pointer, out, "yielded_to", &self.yielded_to);
        field(pointer, out, "inert_reason", &self.inert_reason);
        field(pointer, out, "notes", &self.notes);
    }
}

/// One discovered, verified signature.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SignatureEntry {
    /// Which cosign wire shape carried this signature.
    pub signature_format: SignatureFormat,
    /// How the signature was found.
    pub discovery_method: DiscoveryMethod,
    /// What produced it: `keyless`, `file`, or a key-backend scheme.
    pub key_backend: KeyBackendKind,
    /// Digest of the referrer manifest or sidecar layer carrying it.
    pub referrer_digest: Digest,
    /// Certificate SAN (identity) embedded in the Fulcio cert. Absent under a key — a legal
    /// shape, not malformed input. Registry-served, so it is untrusted input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate_identity: Option<String>,
    /// Certificate OIDC issuer embedded in the Fulcio cert. Absent under a key. Registry-served, so
    /// it is untrusted input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate_oidc_issuer: Option<String>,
    /// Rekor `integratedTime`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed_at: Option<Timestamp>,
    /// Rekor log index, the dedup key when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rekor_log_index: Option<i64>,
}

impl Unknowns for SignatureEntry {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "signature_format", &self.signature_format);
        field(pointer, out, "discovery_method", &self.discovery_method);
        field(pointer, out, "key_backend", &self.key_backend);
        field(pointer, out, "referrer_digest", &self.referrer_digest);
        field(pointer, out, "certificate_identity", &self.certificate_identity);
        field(pointer, out, "certificate_oidc_issuer", &self.certificate_oidc_issuer);
        field(pointer, out, "signed_at", &self.signed_at);
        field(pointer, out, "rekor_log_index", &self.rekor_log_index);
    }
}

/// Which cosign wire shape a signature is written in, and which shape a verify pins.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SignatureFormat {
    /// An OCI 1.1 referrer carrying a Sigstore bundle v0.3. The default, and the only shape `cosign
    /// sign` v3 produces against a registry that implements the Referrers API.
    Bundle,
    /// The cosign `sha256-<hex>.sig` sidecar tag, whose layers carry simplesigning payloads.
    Simplesigning,
    /// Write both shapes.
    Both,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl SignatureFormat {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Bundle => "bundle",
            Self::Simplesigning => "simplesigning",
            Self::Both => "both",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "bundle" => Self::Bundle,
            "simplesigning" => Self::Simplesigning,
            "both" => Self::Both,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for SignatureFormat {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for SignatureFormat {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SignatureFormat {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for SignatureFormat {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// One wire shape's outcome, as reported.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SignatureLegReport {
    /// The shape: `bundle` or `simplesigning`.
    pub format: SignatureFormat,
    /// Digest of the signed payload blob — the Sigstore bundle under `bundle`, the simplesigning
    /// claim under `simplesigning`. Absent when the leg failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_digest: Option<Digest>,
    /// Digest of the manifest the payload hangs from — the OCI referrer under `bundle`, the
    /// `sha256-<hex>.sig` sidecar under `simplesigning`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_digest: Option<Digest>,
    /// Why the leg failed, when it did. `None` means it was written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Unknowns for SignatureLegReport {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "format", &self.format);
        field(pointer, out, "payload_digest", &self.payload_digest);
        field(pointer, out, "manifest_digest", &self.manifest_digest);
        field(pointer, out, "error", &self.error);
    }
}

/// Summary of a signing operation, keyless or under a key.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SignatureReport {
    /// The package that was signed, resolved against the default registry.
    pub identifier: PackageRef,
    /// Digest of the subject manifest that the bundle signs.
    pub subject_digest: Digest,
    /// One entry per wire shape that was written or attempted, in write order.
    pub legs: Vec<SignatureLegReport>,
    /// The `--platform` the run narrowed into; absent when none was given and the run signed
    /// whatever the identifier resolved to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<Platform>,
    /// Signing mechanism used: `"keyless-fulcio"`, or the key backend's own slug under a key.
    pub signer: String,
    /// Certificate SAN (identity) embedded in the Fulcio cert. Absent under a key, and when no leg
    /// was written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate_identity: Option<String>,
    /// Certificate OIDC issuer URL embedded in the Fulcio cert. Absent under a key, and when no leg
    /// was written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate_oidc_issuer: Option<String>,
    /// Which key model produced this signature: `keyless`, `file`, or a key-backend scheme.
    pub key_backend: KeyBackendKind,
    /// The signing key's cosign hint, in key mode only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_key_hint: Option<String>,
    /// The Rekor log index of the transparency record this run created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transparency_log_index: Option<i64>,
}

impl Unknowns for SignatureReport {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "identifier", &self.identifier);
        field(pointer, out, "subject_digest", &self.subject_digest);
        field(pointer, out, "legs", &self.legs);
        field(pointer, out, "platform", &self.platform);
        field(pointer, out, "signer", &self.signer);
        field(pointer, out, "certificate_identity", &self.certificate_identity);
        field(pointer, out, "certificate_oidc_issuer", &self.certificate_oidc_issuer);
        field(pointer, out, "key_backend", &self.key_backend);
        field(pointer, out, "public_key_hint", &self.public_key_hint);
        field(pointer, out, "transparency_log_index", &self.transparency_log_index);
    }
}

/// What a `--tags` / `--tags-file` sweep did, one row per swept tag.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SignatureReportSweep {
    /// One row per swept tag, in the order the tags were given.
    pub items: Vec<SignatureReportSweptTag>,
}

impl Unknowns for SignatureReportSweep {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "items", &self.items);
    }
}

/// One swept tag's row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SignatureReportSweptTag {
    /// The tag as the caller spelled it, so the report names what was asked for rather than what it
    /// resolved to.
    pub tag: String,
    /// What the sweep did to this tag.
    pub status: SweptStatus,
    /// The per-reference report, verbatim. Present for every tag whose run produced one, which
    /// includes a `failed` row carrying a partial report: a `--signature-format both` tag where one
    /// leg landed and one did not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<SignatureReport>,
    /// The error's slug, else its category: the `error.detail`, else the `error.kind`, its error
    /// document would carry. Present exactly when `status` is `failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Human-readable cause, sanitized for the terminal (CWE-150). Present exactly when `status` is
    /// `failed` or `covered` — for a `covered` row it names the tag whose run wrote the referrer,
    /// not a failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl Unknowns for SignatureReportSweptTag {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "tag", &self.tag);
        field(pointer, out, "status", &self.status);
        field(pointer, out, "report", &self.report);
        field(pointer, out, "kind", &self.kind);
        field(pointer, out, "message", &self.message);
    }
}

/// One platform manifest's inline-signing row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SignedPlatformReport {
    /// The platform whose manifest was signed; its canonical spelling keys `platform_digests`.
    pub platform: Platform,
    /// What the inline signing did to this platform.
    pub status: SweptStatus,
    /// That platform's own sign report, verbatim. Present for every platform whose run produced
    /// one, `failed` rows included: a `--signature-format both` platform where one leg landed and
    /// one did not is a failure that still carries the leg that landed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<SignatureReport>,
    /// The error's slug, else its category: the `error.detail`, else the `error.kind`, its error
    /// document would carry. Present exactly when `status` is `failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Human-readable cause, sanitized for the terminal (CWE-150). Present exactly when `status` is
    /// `failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl Unknowns for SignedPlatformReport {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "platform", &self.platform);
        field(pointer, out, "status", &self.status);
        field(pointer, out, "report", &self.report);
        field(pointer, out, "kind", &self.kind);
        field(pointer, out, "message", &self.message);
    }
}

/// Why `ocx upgrade` leaves a binding's tag where it is; a report row, never an error.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SkipReason {
    /// The binding names a digest, so there is no tag to move.
    DigestPinned,
    /// The binding tracks `latest`, which already follows every release.
    Latest,
    /// The tag is not a version (`nightly`, a bare variant name).
    NotAVersion,
    /// The tag carries a prerelease or build suffix, which pins one exact artifact.
    PrereleaseOrBuild,
    /// No newer release exists within the policy.
    UpToDate,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl SkipReason {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::DigestPinned => "digest_pinned",
            Self::Latest => "latest",
            Self::NotAVersion => "not_a_version",
            Self::PrereleaseOrBuild => "prerelease_or_build",
            Self::UpToDate => "up_to_date",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "digest_pinned" => Self::DigestPinned,
            "latest" => Self::Latest,
            "not_a_version" => Self::NotAVersion,
            "prerelease_or_build" => Self::PrereleaseOrBuild,
            "up_to_date" => Self::UpToDate,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for SkipReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for SkipReason {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SkipReason {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for SkipReason {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// One binding `ocx upgrade` left alone.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SkippedBinding {
    /// Local binding name (the `ocx.toml` key).
    pub name: String,
    /// Owning group — `default` for the top-level `\[tools\]` table.
    pub group: String,
    /// The declared tag; absent when the binding spells none (a bare name or a digest).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// Why the tag did not move.
    pub reason: SkipReason,
}

impl Unknowns for SkippedBinding {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "name", &self.name);
        field(pointer, out, "group", &self.group);
        field(pointer, out, "tag", &self.tag);
        field(pointer, out, "reason", &self.reason);
    }
}

/// Why an update check was skipped.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum SkippedReason {
    /// The installed version could not be queried, so the check cannot compare versions.
    Bootstrap,
    /// Offline mode blocked the registry probe.
    Offline,
    /// The 24-hour throttle window has not elapsed since the last probe.
    Throttled,
    /// The registry probe failed.
    RegistryProbeFailed {
        /// The probe error.
        detail: String,
    },
    /// The package was not found in the registry.
    NotFound,
    /// The installed version string is not a version.
    UnparseableCurrent {
        /// The installed version string.
        version: String,
    },
    /// The registry's latest tag is not a version.
    UnparseableLatest,
    /// The registry lists no `major.minor.patch` release tag.
    NoReleaseTag,
    /// A variant this SDK was not generated for: the whole object, as sent.
    Unknown(Value),
}

impl Serialize for SkippedReason {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Bootstrap => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "bootstrap")?;
                object.end()
            }
            Self::Offline => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "offline")?;
                object.end()
            }
            Self::Throttled => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "throttled")?;
                object.end()
            }
            Self::RegistryProbeFailed { detail: field_0 } => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "registry_probe_failed")?;
                object.serialize_entry("detail", field_0)?;
                object.end()
            }
            Self::NotFound => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "not_found")?;
                object.end()
            }
            Self::UnparseableCurrent { version: field_0 } => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "unparseable_current")?;
                object.serialize_entry("version", field_0)?;
                object.end()
            }
            Self::UnparseableLatest => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "unparseable_latest")?;
                object.end()
            }
            Self::NoReleaseTag => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "no_release_tag")?;
                object.end()
            }
            Self::Unknown(value) => value.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for SkippedReason {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let Some(tag) = value.get("type").and_then(Value::as_str) else {
            return Err(D::Error::custom("a union object needs a string `type`"));
        };
        match tag {
            "bootstrap" => Ok(Self::Bootstrap),
            "offline" => Ok(Self::Offline),
            "throttled" => Ok(Self::Throttled),
            "registry_probe_failed" => {
                #[derive(Deserialize)]
                struct Fields {
                    detail: String,
                }
                let fields = payload::<Fields, D::Error>("registry_probe_failed", value)?;
                Ok(Self::RegistryProbeFailed { detail: fields.detail })
            }
            "not_found" => Ok(Self::NotFound),
            "unparseable_current" => {
                #[derive(Deserialize)]
                struct Fields {
                    version: String,
                }
                let fields = payload::<Fields, D::Error>("unparseable_current", value)?;
                Ok(Self::UnparseableCurrent {
                    version: fields.version,
                })
            }
            "unparseable_latest" => Ok(Self::UnparseableLatest),
            "no_release_tag" => Ok(Self::NoReleaseTag),
            _ => Ok(Self::Unknown(value)),
        }
    }
}

impl Unknowns for SkippedReason {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        match self {
            Self::Bootstrap => {}
            Self::Offline => {}
            Self::Throttled => {}
            Self::RegistryProbeFailed { detail: field_0 } => {
                field(pointer, out, "detail", field_0);
            }
            Self::NotFound => {}
            Self::UnparseableCurrent { version: field_0 } => {
                field(pointer, out, "version", field_0);
            }
            Self::UnparseableLatest => {}
            Self::NoReleaseTag => {}
            Self::Unknown(_) => out.push(pointer.clone()),
        }
    }
}

/// One row of the diff: what one alias holds for one platform versus what the fold says it should
/// hold.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SlotRow {
    /// The alias tag this slot belongs to.
    pub tag: AliasTag,
    /// The slot's platform.
    pub platform: Platform,
    /// The slot's verdict.
    pub status: SlotStatus,
    /// The digest the alias carries for this platform; absent when it carries none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed: Option<String>,
    /// The digest the fold expects; absent when it expects none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    /// The version the expectation was folded from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<PackageVersion>,
    /// The version the observed digest belongs to, when the observed content is recognisable as
    /// some published version's — absent when the alias points at content no observed leaf
    /// carries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_source: Option<PackageVersion>,
}

impl Unknowns for SlotRow {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "tag", &self.tag);
        field(pointer, out, "platform", &self.platform);
        field(pointer, out, "status", &self.status);
        field(pointer, out, "observed", &self.observed);
        field(pointer, out, "expected", &self.expected);
        field(pointer, out, "source", &self.source);
        field(pointer, out, "observed_source", &self.observed_source);
    }
}

/// The verdict for one (alias, platform) slot.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SlotStatus {
    /// The alias carries exactly the expected entry.
    Ok,
    /// The alias should carry a platform it does not carry at all.
    Missing,
    /// The alias carries this platform, but not the expected content.
    Stale,
    /// The alias carries a platform nothing folds into it — a leftover from an earlier cascade.
    /// Reported always; removed only when the entry it points at no longer exists.
    Orphan,
    /// The alias carries more than one entry for this platform. Only the last one resolves, so a
    /// shadowed entry is invisible to every consumer while still being published — including when
    /// the surviving one is exactly what the fold expects. One row per shadowed entry, beside the
    /// row for the entry that wins.
    Duplicate,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl SlotStatus {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Ok => "ok",
            Self::Missing => "missing",
            Self::Stale => "stale",
            Self::Orphan => "orphan",
            Self::Duplicate => "duplicate",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "ok" => Self::Ok,
            "missing" => Self::Missing,
            "stale" => Self::Stale,
            "orphan" => Self::Orphan,
            "duplicate" => Self::Duplicate,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for SlotStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for SlotStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SlotStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for SlotStatus {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// Source position of a script failure.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceLocation {
    /// The label the script source was parsed under.
    pub file: String,
    /// 1-indexed line.
    pub line: i64,
    /// 1-indexed column.
    pub column: i64,
}

impl Unknowns for SourceLocation {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "file", &self.file);
        field(pointer, out, "line", &self.line);
        field(pointer, out, "column", &self.column);
    }
}

/// Report emitted by `ocx status`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StatusReport {
    /// Absolute path of the `ocx.toml` this report describes.
    pub project: String,
    /// The `ocx.lock` header and whether it agrees with `ocx.toml`.
    pub lock: LockStatus,
    /// Every group's declarations, keyed by group name.
    pub groups: BTreeMap<String, GroupStatus>,
    /// `\[package."<id>"\]` resolve-time policy, keyed by the canonical author string. Reported
    /// here because it is excluded from `declaration_hash`, so nothing lock-derived can surface it.
    pub package_settings: BTreeMap<String, PackageSettingsOut>,
}

impl Unknowns for StatusReport {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "project", &self.project);
        field(pointer, out, "lock", &self.lock);
        field(pointer, out, "groups", &self.groups);
        field(pointer, out, "package_settings", &self.package_settings);
    }
}

/// One projected surface — binaries/entrypoints/env/integrations admitted on a single axis.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SurfaceOut {
    /// Admitted `binaries` claims on this axis, attributed to their packages.
    pub binaries: Vec<BinaryAttribution>,
    /// Admitted `entrypoints` claims on this axis, attributed to their packages.
    pub entrypoints: Vec<BinaryAttribution>,
    /// Env keys each admitted node exposes on this axis, attributed to the declaring package.
    /// Values are omitted — they are `${installPath}`- templated and only concrete after install.
    pub env: Vec<EnvVarAttribution>,
    /// Integration namespace keys each admitted node declares, attributed to the declaring package.
    /// Payload-free — a closure node is not installed, so `${installPath}` has no value and the
    /// payload would be a half-truth, the same reason `env` omits values.
    pub integrations: Vec<NamespaceAttribution>,
    /// `false` iff any admitted node has undeclared `binaries` ("couldn't determine \u{2260}
    /// determined zero"). Entrypoints have no such flag — the entrypoint map keys are always
    /// authoritative.
    pub binaries_complete: bool,
}

impl Unknowns for SurfaceOut {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "binaries", &self.binaries);
        field(pointer, out, "entrypoints", &self.entrypoints);
        field(pointer, out, "env", &self.env);
        field(pointer, out, "integrations", &self.integrations);
        field(pointer, out, "binaries_complete", &self.binaries_complete);
    }
}

/// The two symmetric surface projections of a closure — "what binaries / entrypoints / env keys
/// would land, and on which axis, if this were installed", without installing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SurfacesOut {
    /// Consumer-facing: what reaches someone installing the root.
    pub interface: SurfaceOut,
    /// Internal: what is visible on the package's own private axis. Public entries appear in both
    /// surfaces (public crosses both axes).
    pub private: SurfaceOut,
}

impl Unknowns for SurfacesOut {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "interface", &self.interface);
        field(pointer, out, "private", &self.private);
    }
}

/// What the sweep did to one tag; one vocabulary for both verbs.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SweptStatus {
    /// The tag's index was acted on; `report` carries the outcome.
    Completed,
    /// The tag resolved to a bare manifest, so the sweep left it alone.
    Skipped,
    /// The tag names the index another tag in this same sweep already acted on, so one referrer
    /// covers both and nothing was written for this tag.
    Covered,
    /// This tag failed. The sweep carried on to the rest and the run exits non-zero at the end.
    Failed,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl SweptStatus {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Completed => "completed",
            Self::Skipped => "skipped",
            Self::Covered => "covered",
            Self::Failed => "failed",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "completed" => Self::Completed,
            "skipped" => Self::Skipped,
            "covered" => Self::Covered,
            "failed" => Self::Failed,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for SweptStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for SweptStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SweptStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for SweptStatus {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// One binding whose declared tag moved in `ocx.toml`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TagUpgrade {
    /// Local binding name (the `ocx.toml` key).
    pub name: String,
    /// Owning group — `default` for the top-level `\[tools\]` table.
    pub group: String,
    /// The tag declared before the upgrade.
    pub from_tag: String,
    /// The tag declared after the upgrade.
    pub to_tag: String,
}

impl Unknowns for TagUpgrade {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "name", &self.name);
        field(pointer, out, "group", &self.group);
        field(pointer, out, "from_tag", &self.from_tag);
        field(pointer, out, "to_tag", &self.to_tag);
    }
}

/// Tag listing for one or more packages, optionally including platform or variant details.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tags {
    /// One entry per package, sorted by package.
    pub items: Vec<TagsEntry>,
}

impl Unknowns for Tags {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "items", &self.items);
    }
}

/// One package's listing; exactly one of `tags`, `platforms` and `variants` is present, chosen by
/// the flag.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TagsEntry {
    /// The package as given.
    pub package: String,
    /// Its tags, sorted; present without `--platforms` and `--variants`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// The platforms of the listed tag, sorted; present under `--platforms`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platforms: Option<Vec<Platform>>,
    /// Its variant names, sorted, `""` naming the default variant; present under `--variants`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variants: Option<Vec<String>>,
}

impl Unknowns for TagsEntry {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "package", &self.package);
        field(pointer, out, "tags", &self.tags);
        field(pointer, out, "platforms", &self.platforms);
        field(pointer, out, "variants", &self.variants);
    }
}

/// RFC 3339 instant in UTC (`Z`), whole seconds.
pub type Timestamp = String;

/// A coexisting per-prompt environment manager.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Tool {
    /// Yielded on `DIRENV_DIR` **naming the resolved project's canonical directory**. A
    /// `DIRENV_DIR` naming a *different* directory is treated as absent — direnv is active for
    /// some ancestor, not for this project.
    Direnv,
    /// Yielded on `MISE_SHELL` **or** `__MISE_ORIG_PATH` being present.
    Mise,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl Tool {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Direnv => "direnv",
            Self::Mise => "mise",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "direnv" => Self::Direnv,
            "mise" => Self::Mise,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for Tool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for Tool {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Tool {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for Tool {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// One tool binding, as the two files describe it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolStatus {
    /// The `ocx.toml` value for this binding, verbatim (`ocx.sh/go-task:3`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared: Option<String>,
    /// EVERY platform leaf the lock records, not the host's, keyed by the canonical platform
    /// string; `ocx inspect` picks the host leaf.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform_digests: Option<BTreeMap<String, Digest>>,
}

impl Unknowns for ToolStatus {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "declared", &self.declared);
        field(pointer, out, "platform_digests", &self.platform_digests);
    }
}

/// The write transport the run used.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Transport {
    /// The forge's REST API.
    Api,
    /// A local `git` clone and one authenticated push.
    Git,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl Transport {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Api => "api",
            Self::Git => "git",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "api" => Self::Api,
            "git" => Self::Git,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for Transport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for Transport {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Transport {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for Transport {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// Something check found that no repair can fix without new content being published.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Unrepairable {
    /// A planned entry points at a child manifest the registry no longer holds, so writing the
    /// alias would publish a dangling pointer.
    ChildManifestMissing {
        /// The alias tag.
        tag: AliasTag,
        /// The missing child manifest.
        digest: Digest,
    },
    /// A planned entry names a digest algorithm this build cannot address, so whether the child is
    /// still there could not be checked at all. Distinct from `child_manifest_missing`: nothing was
    /// observed to be gone — the alias is refused because the check could not be made.
    ChildDigestUnaddressable {
        /// The alias tag.
        tag: AliasTag,
        /// The child digest exactly as the index writes it; no `Digest`, since this build cannot
        /// parse it.
        digest_text: String,
    },
    /// Repairing the alias would leave it with no entries at all. Refused: an empty index is worse
    /// than a stale one.
    WouldEmptyIndex {
        /// The alias tag.
        tag: AliasTag,
    },
    /// A variant this SDK was not generated for: the whole object, as sent.
    Unknown(Value),
}

impl Serialize for Unrepairable {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::ChildManifestMissing {
                tag: field_0,
                digest: field_1,
            } => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "child_manifest_missing")?;
                object.serialize_entry("tag", field_0)?;
                object.serialize_entry("digest", field_1)?;
                object.end()
            }
            Self::ChildDigestUnaddressable {
                tag: field_0,
                digest_text: field_1,
            } => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "child_digest_unaddressable")?;
                object.serialize_entry("tag", field_0)?;
                object.serialize_entry("digest_text", field_1)?;
                object.end()
            }
            Self::WouldEmptyIndex { tag: field_0 } => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "would_empty_index")?;
                object.serialize_entry("tag", field_0)?;
                object.end()
            }
            Self::Unknown(value) => value.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for Unrepairable {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let Some(tag) = value.get("type").and_then(Value::as_str) else {
            return Err(D::Error::custom("a union object needs a string `type`"));
        };
        match tag {
            "child_manifest_missing" => {
                #[derive(Deserialize)]
                struct Fields {
                    tag: AliasTag,
                    digest: Digest,
                }
                let fields = payload::<Fields, D::Error>("child_manifest_missing", value)?;
                Ok(Self::ChildManifestMissing {
                    tag: fields.tag,
                    digest: fields.digest,
                })
            }
            "child_digest_unaddressable" => {
                #[derive(Deserialize)]
                struct Fields {
                    tag: AliasTag,
                    digest_text: String,
                }
                let fields = payload::<Fields, D::Error>("child_digest_unaddressable", value)?;
                Ok(Self::ChildDigestUnaddressable {
                    tag: fields.tag,
                    digest_text: fields.digest_text,
                })
            }
            "would_empty_index" => {
                #[derive(Deserialize)]
                struct Fields {
                    tag: AliasTag,
                }
                let fields = payload::<Fields, D::Error>("would_empty_index", value)?;
                Ok(Self::WouldEmptyIndex { tag: fields.tag })
            }
            _ => Ok(Self::Unknown(value)),
        }
    }
}

impl Unknowns for Unrepairable {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        match self {
            Self::ChildManifestMissing {
                tag: field_0,
                digest: field_1,
            } => {
                field(pointer, out, "tag", field_0);
                field(pointer, out, "digest", field_1);
            }
            Self::ChildDigestUnaddressable {
                tag: field_0,
                digest_text: field_1,
            } => {
                field(pointer, out, "tag", field_0);
                field(pointer, out, "digest_text", field_1);
            }
            Self::WouldEmptyIndex { tag: field_0 } => {
                field(pointer, out, "tag", field_0);
            }
            Self::Unknown(_) => out.push(pointer.clone()),
        }
    }
}

/// Result of `ocx self update --check`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UpdateCheckData {
    /// What the check found.
    pub status: SelfUpdateStatus,
    /// Identifier of the available update; present iff status = `update_available`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identifier: Option<PackageRef>,
    /// Why the check was skipped; present iff status = `skipped`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped_reason: Option<SkippedReason>,
}

impl Unknowns for UpdateCheckData {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "status", &self.status);
        field(pointer, out, "identifier", &self.identifier);
        field(pointer, out, "skipped_reason", &self.skipped_reason);
    }
}

/// Report emitted by `ocx update` (and by `ocx update --check` before it exits 65).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UpdateReport {
    /// Pins whose pull identifier differs between the two locks, ordered by `(group, name,
    /// platform)`.
    pub changes: Vec<BindingChange>,
    /// Bindings this run examined and found unchanged, in the same order.
    pub unchanged: Vec<BindingState>,
    /// Whether the load-bearing lock metadata moved — `declaration_hash`,
    /// `declaration_hash_version` or `lock_version`. Advisory metadata (`generated_at`,
    /// `generated_by`) is deliberately ignored: it moves on every write and would make every
    /// `--check` fail.
    pub metadata_changed: bool,
}

impl Unknowns for UpdateReport {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "changes", &self.changes);
        field(pointer, out, "unchanged", &self.unchanged);
        field(pointer, out, "metadata_changed", &self.metadata_changed);
    }
}

/// Report emitted by `ocx upgrade` (and by `ocx upgrade --check` before it exits 65).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UpgradeReport {
    /// Bindings whose declared tag moved (or, under `--check`, would move).
    pub upgrades: Vec<TagUpgrade>,
    /// Bindings left alone, each with its reason.
    pub skipped: Vec<SkippedBinding>,
    /// Bindings with a newer release beyond their major.
    pub beyond_major: Vec<BeyondMajor>,
    /// What the re-lock moved in `ocx.lock`.
    pub lock: UpdateReport,
}

impl Unknowns for UpgradeReport {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "upgrades", &self.upgrades);
        field(pointer, out, "skipped", &self.skipped);
        field(pointer, out, "beyond_major", &self.beyond_major);
        field(pointer, out, "lock", &self.lock);
    }
}

/// An environment variable declaration.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Var {
    /// A path variable is prepended to any existing value of the environment variable.
    Path {
        /// The environment variable name (e.g. `PATH`, `JAVA_HOME`).
        key: String,
        /// Visibility on the entry axis — controls which exec surface (interface vs private) sees
        /// this entry. Defaults to `private` — publishers explicitly mark contract entries as
        /// `public` or `interface` to expose them to consumers. `"sealed"` is rejected at parse
        /// time.
        visibility: VarVisibility,
        /// Whether the resolved path must exist on disk. If `true` and the path is missing,
        /// installation fails. Defaults to `false`.
        required: bool,
        /// The value template. `${installPath}` — or its alias `${self.installPath}` — is this
        /// package's content directory, `${deps.NAME.installPath}` a declared dependency's, and
        /// `${self.env.KEY}` the resolved value of a variable declared earlier in this same list.
        /// Append `:native` or `:posix` to pick the path style.
        value: String,
    },
    /// A constant variable replaces any existing value of the environment variable.
    Constant {
        /// The environment variable name (e.g. `PATH`, `JAVA_HOME`).
        key: String,
        /// Visibility on the entry axis — controls which exec surface (interface vs private) sees
        /// this entry. Defaults to `private` — publishers explicitly mark contract entries as
        /// `public` or `interface` to expose them to consumers. `"sealed"` is rejected at parse
        /// time.
        visibility: VarVisibility,
        /// The value template. `${installPath}` — or its alias `${self.installPath}` — is this
        /// package's content directory, `${deps.NAME.installPath}` a declared dependency's, and
        /// `${self.env.KEY}` the resolved value of a variable declared earlier in this same list.
        /// Append `:native` or `:posix` to pick the path style.
        value: String,
    },
    /// A list variable is appended to any existing value of the environment variable, joined by its
    /// own separator, with earlier occurrences of the same contribution removed.
    List {
        /// The environment variable name (e.g. `PATH`, `JAVA_HOME`).
        key: String,
        /// Visibility on the entry axis — controls which exec surface (interface vs private) sees
        /// this entry. Defaults to `private` — publishers explicitly mark contract entries as
        /// `public` or `interface` to expose them to consumers. `"sealed"` is rejected at parse
        /// time.
        visibility: VarVisibility,
        /// The string joining this contribution to the variable's existing value — a single space
        /// for `JDK_JAVA_OPTIONS`, a comma for `GODEBUG`.
        separator: String,
        /// The value template. `${installPath}` — or its alias `${self.installPath}` — is this
        /// package's content directory, `${deps.NAME.installPath}` a declared dependency's, and
        /// `${self.env.KEY}` the resolved value of a variable declared earlier in this same list.
        /// Append `:native` or `:posix` to pick the path style.
        value: String,
    },
    /// A variant this SDK was not generated for: the whole object, as sent.
    Unknown(Value),
}

impl Serialize for Var {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Path {
                key: field_0,
                visibility: field_1,
                required: field_2,
                value: field_3,
            } => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "path")?;
                object.serialize_entry("key", field_0)?;
                object.serialize_entry("visibility", field_1)?;
                object.serialize_entry("required", field_2)?;
                object.serialize_entry("value", field_3)?;
                object.end()
            }
            Self::Constant {
                key: field_0,
                visibility: field_1,
                value: field_2,
            } => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "constant")?;
                object.serialize_entry("key", field_0)?;
                object.serialize_entry("visibility", field_1)?;
                object.serialize_entry("value", field_2)?;
                object.end()
            }
            Self::List {
                key: field_0,
                visibility: field_1,
                separator: field_2,
                value: field_3,
            } => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "list")?;
                object.serialize_entry("key", field_0)?;
                object.serialize_entry("visibility", field_1)?;
                object.serialize_entry("separator", field_2)?;
                object.serialize_entry("value", field_3)?;
                object.end()
            }
            Self::Unknown(value) => value.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for Var {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let Some(tag) = value.get("type").and_then(Value::as_str) else {
            return Err(D::Error::custom("a union object needs a string `type`"));
        };
        match tag {
            "path" => {
                #[derive(Deserialize)]
                struct Fields {
                    key: String,
                    visibility: VarVisibility,
                    required: bool,
                    value: String,
                }
                let fields = payload::<Fields, D::Error>("path", value)?;
                Ok(Self::Path {
                    key: fields.key,
                    visibility: fields.visibility,
                    required: fields.required,
                    value: fields.value,
                })
            }
            "constant" => {
                #[derive(Deserialize)]
                struct Fields {
                    key: String,
                    visibility: VarVisibility,
                    value: String,
                }
                let fields = payload::<Fields, D::Error>("constant", value)?;
                Ok(Self::Constant {
                    key: fields.key,
                    visibility: fields.visibility,
                    value: fields.value,
                })
            }
            "list" => {
                #[derive(Deserialize)]
                struct Fields {
                    key: String,
                    visibility: VarVisibility,
                    separator: String,
                    value: String,
                }
                let fields = payload::<Fields, D::Error>("list", value)?;
                Ok(Self::List {
                    key: fields.key,
                    visibility: fields.visibility,
                    separator: fields.separator,
                    value: fields.value,
                })
            }
            _ => Ok(Self::Unknown(value)),
        }
    }
}

impl Unknowns for Var {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        match self {
            Self::Path {
                key: field_0,
                visibility: field_1,
                required: field_2,
                value: field_3,
            } => {
                field(pointer, out, "key", field_0);
                field(pointer, out, "visibility", field_1);
                field(pointer, out, "required", field_2);
                field(pointer, out, "value", field_3);
            }
            Self::Constant {
                key: field_0,
                visibility: field_1,
                value: field_2,
            } => {
                field(pointer, out, "key", field_0);
                field(pointer, out, "visibility", field_1);
                field(pointer, out, "value", field_2);
            }
            Self::List {
                key: field_0,
                visibility: field_1,
                separator: field_2,
                value: field_3,
            } => {
                field(pointer, out, "key", field_0);
                field(pointer, out, "visibility", field_1);
                field(pointer, out, "separator", field_2);
                field(pointer, out, "value", field_3);
            }
            Self::Unknown(_) => out.push(pointer.clone()),
        }
    }
}

/// Visibility on the entry axis — controls which exec surface (interface vs private) sees this
/// entry. Defaults to `private` — publishers explicitly mark contract entries as `public` or
/// `interface` to expose them to consumers. `"sealed"` is rejected at parse time.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum VarVisibility {
    /// Visible to the package's own runtime only.
    Private,
    /// Visible to the package's own runtime and to consumers.
    Public,
    /// Visible to consumers only.
    Interface,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl VarVisibility {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Private => "private",
            Self::Public => "public",
            Self::Interface => "interface",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "private" => Self::Private,
            "public" => Self::Public,
            "interface" => Self::Interface,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for VarVisibility {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for VarVisibility {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for VarVisibility {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for VarVisibility {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// What `ocx shell state` reports: all derived, none of it mutating.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VerboseShellState {
    /// `$OCX_HOME`.
    pub ocx_home: AbsolutePath,
    /// Whether `$OCX_HOME` exists on disk.
    pub ocx_home_present: bool,
    /// Whether `ocx self setup` has ever wired this machine's shell.
    pub shell_integration_installed: bool,
    /// The **resolved toolchain home**, a machine-readable contract field.
    pub toolchain_home: AbsolutePath,
    /// The **PATH-facing trampoline directory** for the same tier, which a consumer outside ocx
    /// puts on `PATH`.
    pub toolchain_bin: String,
    /// The **effective** `activate` mode, past the whole ladder.
    pub activate: ActivateMode,
    /// The **effective** `pinned` value, resolved through the same ladder.
    pub pinned: bool,
    /// Why the project's `ocx.lock` refuses composition, when it does, as text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_refusal: Option<String>,
    /// Whether the `__OCX_ENV_STATE` carrier is set at all.
    pub carrier_present: bool,
    /// The carrier's encoded length in bytes, measured against the carrier cap.
    pub carrier_bytes: ByteSize,
    /// The decoded ledger, **rendered as fields, never as base64**.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ledger: Option<Ledger>,
    /// Whether the ledger's recorded `fp` still matches the watch set on disk now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint_current: Option<bool>,
    /// The watch set, with each member's presence, size and modification time.
    pub watch_set: Vec<WatchMember>,
    /// The project the CWD walk resolved, canonicalized; absent when none resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_dir: Option<AbsolutePath>,
    /// The 16-hex state key of `project_dir`; absent with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_key: Option<String>,
    /// Whether a usable consent stamp exists for that key; an unusable stamp counts as absent.
    pub project_stamped: bool,
    /// **Which clause activated this project**; absent when it is inert.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant: Option<Grant>,
    /// When the consent stamp was written, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stamp_written_at: Option<Timestamp>,
    /// Prior intactness for each constant the ledger's project scope owns.
    pub priors: Vec<PriorStatus>,
    /// The hook's enablement and the rung that decided it.
    pub hook: HookStatus,
    /// Every coexisting tool observed live in this shell.
    pub yielded_to: Vec<Observation>,
    /// Why the shell is not active, when it is not — **the command's reason to exist**. Absent
    /// when the project is active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inert_reason: Option<Reason>,
    /// Reason rows that explain an answer without being an inertness verdict of their own.
    pub notes: Vec<Note>,
}

impl Unknowns for VerboseShellState {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "ocx_home", &self.ocx_home);
        field(pointer, out, "ocx_home_present", &self.ocx_home_present);
        field(
            pointer,
            out,
            "shell_integration_installed",
            &self.shell_integration_installed,
        );
        field(pointer, out, "toolchain_home", &self.toolchain_home);
        field(pointer, out, "toolchain_bin", &self.toolchain_bin);
        field(pointer, out, "activate", &self.activate);
        field(pointer, out, "pinned", &self.pinned);
        field(pointer, out, "lock_refusal", &self.lock_refusal);
        field(pointer, out, "carrier_present", &self.carrier_present);
        field(pointer, out, "carrier_bytes", &self.carrier_bytes);
        field(pointer, out, "ledger", &self.ledger);
        field(pointer, out, "fingerprint_current", &self.fingerprint_current);
        field(pointer, out, "watch_set", &self.watch_set);
        field(pointer, out, "project_dir", &self.project_dir);
        field(pointer, out, "project_key", &self.project_key);
        field(pointer, out, "project_stamped", &self.project_stamped);
        field(pointer, out, "grant", &self.grant);
        field(pointer, out, "stamp_written_at", &self.stamp_written_at);
        field(pointer, out, "priors", &self.priors);
        field(pointer, out, "hook", &self.hook);
        field(pointer, out, "yielded_to", &self.yielded_to);
        field(pointer, out, "inert_reason", &self.inert_reason);
        field(pointer, out, "notes", &self.notes);
    }
}

/// Report emitted by `ocx update` (and by `ocx update --check` before it exits 65).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VerboseUpdateReport {
    /// Pins whose pull identifier differs between the two locks, ordered by `(group, name,
    /// platform)`.
    pub changes: Vec<BindingChange>,
    /// Bindings this run examined and found unchanged, in the same order.
    pub unchanged: Vec<BindingState>,
    /// Whether the load-bearing lock metadata moved — `declaration_hash`,
    /// `declaration_hash_version` or `lock_version`. Advisory metadata (`generated_at`,
    /// `generated_by`) is deliberately ignored: it moves on every write and would make every
    /// `--check` fail.
    pub metadata_changed: bool,
}

impl Unknowns for VerboseUpdateReport {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "changes", &self.changes);
        field(pointer, out, "unchanged", &self.unchanged);
        field(pointer, out, "metadata_changed", &self.metadata_changed);
    }
}

/// Report emitted by `ocx upgrade` (and by `ocx upgrade --check` before it exits 65).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VerboseUpgradeReport {
    /// Bindings whose declared tag moved (or, under `--check`, would move).
    pub upgrades: Vec<TagUpgrade>,
    /// Bindings left alone, each with its reason.
    pub skipped: Vec<SkippedBinding>,
    /// Bindings with a newer release beyond their major.
    pub beyond_major: Vec<BeyondMajor>,
    /// What the re-lock moved in `ocx.lock`.
    pub lock: UpdateReport,
}

impl Unknowns for VerboseUpgradeReport {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "upgrades", &self.upgrades);
        field(pointer, out, "skipped", &self.skipped);
        field(pointer, out, "beyond_major", &self.beyond_major);
        field(pointer, out, "lock", &self.lock);
    }
}

/// Version information reported by `ocx version`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VerboseVersionData {
    /// The version ocx reports for itself.
    pub version: String,
    /// The crate version, when it differs from `version`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cargo_pkg_version: Option<String>,
    /// The release channel the binary was built for (`dev`, `stable`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// The git commit the binary was built from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<CommitInfo>,
    /// The build environment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<BuildInfo>,
    /// The CI run that built the binary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci: Option<CiInfo>,
    /// The machine-interface versions this binary speaks.
    pub contract: ContractVersions,
}

impl Unknowns for VerboseVersionData {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "version", &self.version);
        field(pointer, out, "cargo_pkg_version", &self.cargo_pkg_version);
        field(pointer, out, "channel", &self.channel);
        field(pointer, out, "commit", &self.commit);
        field(pointer, out, "build", &self.build);
        field(pointer, out, "ci", &self.ci);
        field(pointer, out, "contract", &self.contract);
    }
}

/// A cached activation verdict; a change to the watch set expires it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Verdict {
    /// Activate the project; ocx never caches it, so a carrier holding it was not written by ocx.
    Activate,
    /// Consent refused activation for the project in effect.
    Inert,
    /// No project resolved from the working directory; entering one expires it.
    NoProject,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl Verdict {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Activate => "activate",
            Self::Inert => "inert",
            Self::NoProject => "no_project",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "activate" => Self::Activate,
            "inert" => Self::Inert,
            "no_project" => Self::NoProject,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for Verdict {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Verdict {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for Verdict {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// Summary of a successful Sigstore verification.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VerificationReport {
    /// Digest of the subject manifest whose signature was verified.
    pub subject_digest: Digest,
    /// What carried the verified signature — **not always a manifest**.
    pub referrer_digest: Digest,
    /// Certificate SAN (identity) embedded in the Fulcio cert. Absent under a key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate_identity: Option<String>,
    /// Certificate OIDC issuer embedded in the Fulcio cert. Absent under a key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate_oidc_issuer: Option<String>,
    /// Rekor integrated time of the signature entry. Absent when no transparency record exists (a
    /// key without a Rekor upload) or its time is unrepresentable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed_at: Option<Timestamp>,
    /// Every verified signature discovered for the subject, the passing one first; never empty,
    /// since a verification that passed has at least one.
    pub signatures: Vec<SignatureEntry>,
}

impl Unknowns for VerificationReport {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "subject_digest", &self.subject_digest);
        field(pointer, out, "referrer_digest", &self.referrer_digest);
        field(pointer, out, "certificate_identity", &self.certificate_identity);
        field(pointer, out, "certificate_oidc_issuer", &self.certificate_oidc_issuer);
        field(pointer, out, "signed_at", &self.signed_at);
        field(pointer, out, "signatures", &self.signatures);
    }
}

/// Version information reported by `ocx version`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VersionData {
    /// The version ocx reports for itself.
    pub version: String,
    /// The crate version, when it differs from `version`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cargo_pkg_version: Option<String>,
    /// The release channel the binary was built for (`dev`, `stable`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// The git commit the binary was built from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<CommitInfo>,
    /// The build environment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<BuildInfo>,
    /// The CI run that built the binary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci: Option<CiInfo>,
    /// The machine-interface versions this binary speaks.
    pub contract: ContractVersions,
}

impl Unknowns for VersionData {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "version", &self.version);
        field(pointer, out, "cargo_pkg_version", &self.cargo_pkg_version);
        field(pointer, out, "channel", &self.channel);
        field(pointer, out, "commit", &self.commit);
        field(pointer, out, "build", &self.build);
        field(pointer, out, "ci", &self.ci);
        field(pointer, out, "contract", &self.contract);
    }
}

/// Two-axis visibility of a dependency edge or env entry: whether it reaches the package's own
/// runtime, its consumers, both or neither.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Visibility {
    /// Visible to neither the package's own runtime nor its consumers.
    Sealed,
    /// Visible to the package's own runtime only.
    Private,
    /// Visible to the package's own runtime and to consumers.
    Public,
    /// Visible to consumers only.
    Interface,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl Visibility {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Sealed => "sealed",
            Self::Private => "private",
            Self::Public => "public",
            Self::Interface => "interface",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "sealed" => Self::Sealed,
            "private" => Self::Private,
            "public" => Self::Public,
            "interface" => Self::Interface,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for Visibility {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for Visibility {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Visibility {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for Visibility {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}

/// One pre-warmed tool: what `ocx pull` put on disk for it, and which kind of directory that is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WarmedPath {
    /// The directory that now exists for the tool.
    pub path: String,
    /// Whether `path` is a package root or a shim tree.
    pub kind: PathKind,
}

impl Unknowns for WarmedPath {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "path", &self.path);
        field(pointer, out, "kind", &self.kind);
    }
}

/// Pre-warmed tools, one per locked tool in scope.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WarmedPaths {
    /// Each pre-warmed tool, keyed by pulled identifier, in lock order.
    pub paths: BTreeMap<String, WarmedPath>,
    /// Advisories for the deferred tools this run pre-warmed, in the shape `ocx env` emits; also
    /// written to stderr.
    pub advisories: Vec<LazyAdvisoryReport>,
}

impl Unknowns for WarmedPaths {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "paths", &self.paths);
        field(pointer, out, "advisories", &self.advisories);
    }
}

/// One member of the fingerprint watch set, as it stands on disk right now.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WatchMember {
    /// The watched path.
    pub path: String,
    /// Whether it exists right now.
    pub present: bool,
    /// Size in bytes; absent when the member is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<ByteSize>,
    /// When it was last modified, at the whole-second granularity the per-prompt fast path
    /// compares; absent when the member is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<Timestamp>,
}

impl Unknowns for WatchMember {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        field(pointer, out, "path", &self.path);
        field(pointer, out, "present", &self.present);
        field(pointer, out, "size", &self.size);
        field(pointer, out, "modified_at", &self.modified_at);
    }
}

/// The result of attempting one alias index write.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum WriteOutcome {
    /// The index was written. `verified` is false when the post-write read-back returned a
    /// different digest — a concurrent writer, which is a warning rather than a failure: this
    /// run's write did land.
    Written {
        /// The digest of the index written.
        digest: Digest,
        /// Whether the post-write read-back returned the same digest.
        verified: bool,
        /// The dead child digests dropped from the planned index before it went on the wire, each
        /// backing nothing but orphan slots; absent when empty, as on every ordinary write.
        dropped: Option<Vec<String>>,
    },
    /// Refused before any write, because applying it would publish something broken.
    Refused {
        /// Why the alias was refused.
        reason: Unrepairable,
    },
    /// The alias moved between the gather this plan was computed from and the write, so nothing was
    /// written. Not a fault and not unrepairable: a publish landed in the middle of the run, and
    /// re-running the repair against the new state is the whole fix.
    Raced {
        /// The digest the plan expected; absent when the plan expected no alias.
        expected: Option<Digest>,
        /// The digest the alias holds now; absent when the alias is gone.
        live: Option<Digest>,
    },
    /// The registry rejected the write, with the rendered cause.
    Failed {
        /// The rendered cause.
        message: String,
    },
    /// A variant this SDK was not generated for: the whole object, as sent.
    Unknown(Value),
}

impl Serialize for WriteOutcome {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Written {
                digest: field_0,
                verified: field_1,
                dropped: field_2,
            } => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "written")?;
                object.serialize_entry("digest", field_0)?;
                object.serialize_entry("verified", field_1)?;
                if let Some(field_2) = field_2 {
                    object.serialize_entry("dropped", field_2)?;
                }
                object.end()
            }
            Self::Refused { reason: field_0 } => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "refused")?;
                object.serialize_entry("reason", field_0)?;
                object.end()
            }
            Self::Raced {
                expected: field_0,
                live: field_1,
            } => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "raced")?;
                if let Some(field_0) = field_0 {
                    object.serialize_entry("expected", field_0)?;
                }
                if let Some(field_1) = field_1 {
                    object.serialize_entry("live", field_1)?;
                }
                object.end()
            }
            Self::Failed { message: field_0 } => {
                let mut object = serializer.serialize_map(None)?;
                object.serialize_entry("type", "failed")?;
                object.serialize_entry("message", field_0)?;
                object.end()
            }
            Self::Unknown(value) => value.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for WriteOutcome {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let Some(tag) = value.get("type").and_then(Value::as_str) else {
            return Err(D::Error::custom("a union object needs a string `type`"));
        };
        match tag {
            "written" => {
                #[derive(Deserialize)]
                struct Fields {
                    digest: Digest,
                    verified: bool,
                    #[serde(default, skip_serializing_if = "Option::is_none")]
                    dropped: Option<Vec<String>>,
                }
                let fields = payload::<Fields, D::Error>("written", value)?;
                Ok(Self::Written {
                    digest: fields.digest,
                    verified: fields.verified,
                    dropped: fields.dropped,
                })
            }
            "refused" => {
                #[derive(Deserialize)]
                struct Fields {
                    reason: Unrepairable,
                }
                let fields = payload::<Fields, D::Error>("refused", value)?;
                Ok(Self::Refused { reason: fields.reason })
            }
            "raced" => {
                #[derive(Deserialize)]
                struct Fields {
                    #[serde(default, skip_serializing_if = "Option::is_none")]
                    expected: Option<Digest>,
                    #[serde(default, skip_serializing_if = "Option::is_none")]
                    live: Option<Digest>,
                }
                let fields = payload::<Fields, D::Error>("raced", value)?;
                Ok(Self::Raced {
                    expected: fields.expected,
                    live: fields.live,
                })
            }
            "failed" => {
                #[derive(Deserialize)]
                struct Fields {
                    message: String,
                }
                let fields = payload::<Fields, D::Error>("failed", value)?;
                Ok(Self::Failed {
                    message: fields.message,
                })
            }
            _ => Ok(Self::Unknown(value)),
        }
    }
}

impl Unknowns for WriteOutcome {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        match self {
            Self::Written {
                digest: field_0,
                verified: field_1,
                dropped: field_2,
            } => {
                field(pointer, out, "digest", field_0);
                field(pointer, out, "verified", field_1);
                field(pointer, out, "dropped", field_2);
            }
            Self::Refused { reason: field_0 } => {
                field(pointer, out, "reason", field_0);
            }
            Self::Raced {
                expected: field_0,
                live: field_1,
            } => {
                field(pointer, out, "expected", field_0);
                field(pointer, out, "live", field_1);
            }
            Self::Failed { message: field_0 } => {
                field(pointer, out, "message", field_0);
            }
            Self::Unknown(_) => out.push(pointer.clone()),
        }
    }
}

/// Whether a forge write moved anything.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum WriteStatus {
    /// The committed or proposed content already matched, so nothing was written.
    Unchanged,
    /// The run created or moved the content.
    Updated,
    /// A value this SDK was not generated for, kept as sent.
    Unknown(String),
}

impl WriteStatus {
    /// The wire value.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Unchanged => "unchanged",
            Self::Updated => "updated",
            Self::Unknown(value) => value,
        }
    }

    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.
    pub fn from_value(value: String) -> Self {
        match value.as_str() {
            "unchanged" => Self::Unchanged,
            "updated" => Self::Updated,
            _ => Self::Unknown(value),
        }
    }
}

impl std::fmt::Display for WriteStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for WriteStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for WriteStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(String::deserialize(deserializer)?))
    }
}

impl Unknowns for WriteStatus {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if matches!(self, Self::Unknown(_)) {
            out.push(pointer.clone());
        }
    }
}
