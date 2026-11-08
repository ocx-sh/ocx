// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Every wire type of the execution state, field by field, with the serde attributes that make the
//! payload a versioned wire contract.
//!
//! The payload is compact JSON. Unknown fields are ignored (a newer minor), a missing field takes its
//! default, and duplicate fields are an error. `narrowing` and `home` travel in the plaintext header,
//! not in the payload, so they are `serde(skip)` here.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Major version, in the plaintext header.
pub const MAJOR: u32 = 2;
/// Minor version, in the payload.
pub const MINOR: u32 = 0;
/// Whole value, every platform: cmd.exe's 8,191-char line minus `SET "`, the name, `=` and `"`.
pub const MAX_ENCODED: usize = 8_169;
/// Bytes before the first `.`; part of the frozen header grammar.
pub const MAX_HEADER: usize = 2_048;
/// Bytes a payload may inflate to.
pub const MAX_INFLATED: usize = 256 * 1024;
/// Applied + identity + replay records.
pub const MAX_RECORDS: usize = 4_096;
/// Tiers a session cache may hold.
pub const MAX_RECORDED_TIERS: usize = 8;
/// Inherited scopes; the encoder drops the oldest above it.
pub const MAX_INHERITED_SCOPES: usize = 16;

/// The state a process hands its children.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExecState {
    /// Header `n=`.
    #[serde(skip)]
    pub narrowing: Narrowing,
    /// Header `h=`; absolute.
    #[serde(skip)]
    pub home: Option<PathBuf>,
    /// Any value is accepted within [`MAJOR`]; fields of a newer minor are ignored.
    pub minor: u32,
    pub policy: Policy,
    /// Application order, oldest first.
    pub applied: Vec<AppliedValue>,
    /// Scopes an outer frame composed: distinct, oldest first, at most [`MAX_INHERITED_SCOPES`].
    pub inherited_scopes: Vec<Scope>,
    pub replay: Replay,
    pub selection: Selection,
    /// Reset at every spawn.
    pub session: SessionCache,
    /// Sticky across Layer and Isolate spawns.
    pub truncated: Truncation,
}

impl Default for ExecState {
    fn default() -> Self {
        Self {
            narrowing: Narrowing::default(),
            home: None,
            minor: MINOR,
            policy: Policy::default(),
            applied: Vec::new(),
            inherited_scopes: Vec::new(),
            replay: Replay::default(),
            selection: Selection::default(),
            session: SessionCache::default(),
            truncated: Truncation::default(),
        }
    }
}

impl ExecState {
    /// Marks every session record and identity as inherited and adds the scopes they belonged to.
    ///
    /// A `Project` session scope enters as the session's project root (`PinnedTier::Project.dir`,
    /// else `session.project.dir`); when neither is known the record is retagged but no scope is added.
    pub fn retag_sessions(&mut self) {
        let project_root = match &self.selection {
            Selection::Pinned(Pinned {
                tier: PinnedTier::Project { dir },
                ..
            }) => Some(dir.clone()),
            _ => self.session.project.as_ref().map(|hint| hint.dir.clone()),
        };
        let mut scopes = Vec::new();
        let owners = self
            .applied
            .iter_mut()
            .map(|record| &mut record.owner)
            .chain(self.replay.launch_identities.iter_mut().map(|record| &mut record.owner));
        for owner in owners {
            if let Owner::Session { scope, .. } = owner {
                match scope {
                    ScopeId::Global => scopes.push(Scope::Global),
                    ScopeId::Project => {
                        if let Some(root) = &project_root {
                            scopes.push(Scope::Project { root: root.clone() });
                        }
                    }
                }
                *owner = Owner::Inherited;
            }
        }
        // State-derived roots go in as written: canonicalising one would `stat` (on Windows, open) a
        // path a forged state chose, such as a UNC share or a hung mount.
        for scope in scopes {
            self.insert_scope(scope);
        }
    }

    /// Adds a scope this process composed unless an equal one is present; drops the oldest above
    /// [`MAX_INHERITED_SCOPES`]. An existing scope keeps its position.
    ///
    /// The given `Project` root is dunce-canonicalised, so a symlinked spelling of a known root is the
    /// same scope. Stored roots are received state and compare as written, never resolved.
    pub fn add_scope(&mut self, scope: Scope) {
        self.insert_scope(scope.canonical());
    }

    fn insert_scope(&mut self, scope: Scope) {
        if !self.inherited_scopes.contains(&scope) {
            self.inherited_scopes.push(scope);
        }
        let excess = self.inherited_scopes.len().saturating_sub(MAX_INHERITED_SCOPES);
        self.inherited_scopes.drain(..excess);
    }
}

/// Booleans the header carries; the header is their single source.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Narrowing {
    pub offline: bool,
    pub frozen: bool,
    pub no_consent: bool,
    /// Implies offline.
    pub credential_helper: bool,
}

/// Neutral and relaxing settings; `None` is "not written". Each value is the string the replaced
/// variable would hold, validated by that variable's parser.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Policy {
    /// Neutral, `OCX_REMOTE`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote: Option<bool>,
    /// Neutral, `OCX_NO_PROJECT`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_project: Option<bool>,
    /// Relaxing, `OCX_NO_CONFIG_REFRESH`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_config_refresh: Option<bool>,
    /// Relaxing, `OCX_NO_UPDATE_CHECK`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_update_check: Option<bool>,
    /// Relaxing, `OCX_CONFIG`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_path: Option<PathBuf>,
    /// Relaxing, `OCX_NO_CONFIG`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_config: Option<bool>,
    /// Relaxing, `OCX_INDEX`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_dir: Option<PathBuf>,
    /// Relaxing, `OCX_TOOLCHAIN_DIR`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub toolchain_dir: Option<PathBuf>,
    /// Relaxing, `OCX_MIRRORS`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mirrors: Option<String>,
    /// Relaxing, `OCX_PATCHES`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patches: Option<String>,
    /// Relaxing, `OCX_PATCH_SNAPSHOT`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch_snapshot_path: Option<PathBuf>,
    /// Relaxing, `OCX_MANAGED_CONFIG`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub managed_config: Option<String>,
    /// Relaxing, `OCX_NO_VERIFY`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_verify: Option<bool>,
    /// Relaxing, `OCX_RECORDS_DIR`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub records_dir: Option<PathBuf>,
    /// Relaxing, `OCX_RECORDS_NAME`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub records_name: Option<String>,
    /// Relaxing, `OCX_ALLOW_YANKED`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_yanked: Option<bool>,
    /// Relaxing, `OCX_DEFAULT_REGISTRY`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_registry: Option<String>,
    /// Relaxing, `OCX_INSECURE_REGISTRIES` (a `host[:port]` list).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub insecure_registries: Option<String>,
}

/// One value ocx applied to an environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppliedValue {
    /// A valid env key.
    pub key: String,
    pub value: RecordedValue,
    pub kind: ValueKind,
    /// `List` only; non-empty and effective.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub separator: Option<String>,
    /// Path/List: the element was already present and only moved; revert keeps it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pre_existing: bool,
    pub owner: Owner,
    /// `Constant` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior: Option<Prior>,
}

/// One variant, kept as an enum for the `{"literal": ...}` wire shape of `__OCX_ENV_STATE`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordedValue {
    /// Raw, byte-exact.
    Literal(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueKind {
    Path,
    List,
    Constant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Owner {
    Session { scope: ScopeId, source: SessionSource },
    Inherited,
}

/// A session record's scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScopeId {
    /// The global toolchain tier.
    Global,
    /// The project in effect.
    Project,
}

/// A composed scope with its identity; compared only, never opened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Global,
    /// Dunce-canonical project dir.
    Project {
        root: PathBuf,
    },
}

impl Scope {
    /// The scope with a `Project` root canonicalised; an unresolvable root stays as written.
    fn canonical(&self) -> Self {
        match self {
            Self::Global => Self::Global,
            Self::Project { root } => Self::Project {
                root: dunce::canonicalize(root).unwrap_or_else(|_| root.clone()),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionSource {
    /// From `ocx.toml` groups or the global tier; refreshed.
    Composed,
    /// From `ocx shell --with/--env`; remove-only.
    Invocation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Prior {
    /// The variable did not exist.
    Unset,
    /// The exact prior string; set-but-empty is `Value("")`.
    Value(String),
    /// A non-UTF-8 prior; never restored.
    Withheld,
}

/// Projection input; not on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposedValue {
    pub key: String,
    pub value: String,
    pub kind: ValueKind,
    pub separator: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Replay {
    pub env: Vec<ReplayValue>,
    pub launch_identities: Vec<IdentityRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayValue {
    pub key: String,
    pub kind: ValueKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub separator: Option<String>,
    pub value: ReplayPayload,
}

/// One variant, kept as an enum for the `{"literal": ...}` wire shape of `__OCX_ENV_STATE`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayPayload {
    Literal(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityRecord {
    /// `sha256:<64 hex>`.
    pub digest: String,
    /// Package names (`registry/repository[:tag]`, never a digest), parsed as package references on read.
    pub names: Vec<String>,
    pub owner: Owner,
    /// The composing project opted this package out of the patch tier.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_patches: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Selection {
    #[default]
    FollowCwd,
    Pinned(Pinned),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pinned {
    pub tier: PinnedTier,
    /// Group names, resolved only against the consented `ocx.toml` of `tier`.
    #[serde(default)]
    pub groups: Vec<String>,
    /// Display only: `--with` identifiers as typed.
    #[serde(default)]
    pub with_names: Vec<String>,
    /// Display only: `--env` keys, never values.
    #[serde(default)]
    pub env_keys: Vec<String>,
    /// 64 hex; equality and display only.
    pub invocation_digest: String,
    /// The `--pinned`/`--no-pinned` tier of the `pinned` ladder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_paths: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PinnedTier {
    Project {
        dir: PathBuf,
    },
    /// Always `$OCX_HOME/ocx.toml`; nothing from state.
    Global,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionCache {
    /// `None` is unclaimed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<SessionToken>,
    /// Watch-set fingerprint; empty is none.
    pub fp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<Verdict>,
    /// At most [`MAX_RECORDED_TIERS`]; only `stat`ed.
    pub tiers: Vec<PathBuf>,
    /// Watch-list digest.
    pub ws: String,
    /// Deferred-diagnostics digest.
    pub messages_fp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectScopeHint>,
}

/// Shell-instance identity; not a secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionToken {
    pub pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Inert,
    NoProject,
}

/// Advisory; compared only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectScopeHint {
    /// 16 hex project key.
    pub key: String,
    /// Absolute; never joined.
    pub dir: PathBuf,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Truncation {
    #[default]
    None,
    InheritedDropped,
    AllAppliedDropped,
    PayloadDropped,
}

/// What a process received in `__OCX_ENV_STATE`.
// The contract fixes `Readable(ExecState)` by value; one value per process, so the size is immaterial.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inherited {
    Absent,
    Readable(ExecState),
    Unreadable {
        header: HeaderView,
        reason: UnreadableReason,
    },
}

/// What the plaintext header yields, whatever else failed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeaderView {
    pub major: Option<u32>,
    pub narrowing: Narrowing,
    pub home: Option<PathBuf>,
}

/// Why a received state would not decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnreadableReason {
    HeaderInvalid,
    OtherMajor,
    TooLong,
    NotUtf8,
    Base64,
    Inflate,
    InflateLimit,
    TrailingBytes,
    Json,
    RecordLimit,
    TierLimit,
    ScopeLimit,
    Invalid { field: &'static str },
}

impl std::fmt::Display for UnreadableReason {
    /// The cause in words; the invalid field is the only part that came from the value, so it
    /// passes `display_safe` and the text is safe to print.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HeaderInvalid => f.write_str("invalid header"),
            Self::OtherMajor => f.write_str("another format"),
            Self::TooLong => f.write_str("too long"),
            Self::NotUtf8 => f.write_str("not valid text"),
            Self::Base64 => f.write_str("bad encoding"),
            Self::Inflate => f.write_str("bad compression"),
            Self::InflateLimit => f.write_str("too large when expanded"),
            Self::TrailingBytes => f.write_str("trailing bytes"),
            Self::Json => f.write_str("invalid payload"),
            Self::RecordLimit => f.write_str("too many records"),
            Self::TierLimit => f.write_str("too many config tiers"),
            Self::ScopeLimit => f.write_str("too many scopes"),
            Self::Invalid { field } => write!(f, "invalid {}", super::display_safe(field)),
        }
    }
}

/// The result of [`crate::exec_state::encode()`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encoded {
    pub value: Option<String>,
    pub truncated: Truncation,
    pub encoded_len: usize,
}

#[cfg(test)]
mod tests;
