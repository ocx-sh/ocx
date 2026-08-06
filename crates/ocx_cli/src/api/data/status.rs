// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Wire shape for `ocx status` — what `ocx.toml` and `ocx.lock` say, verbatim.
//!
//! Nothing here resolves anything. No platform is selected, no package metadata
//! is read, no relative `path` value is anchored to the project root. Those are
//! `ocx inspect`'s job; status reports the declaration and its lock, including
//! the states where the two disagree.

use std::collections::BTreeMap;

use ocx_lib::{
    cli::{Annotation, DataInterface, Theme, TreeItem},
    oci::Platform,
    package::metadata::env::modifier::ModifierKind,
    project::{PackageSettings, ProjectConfig, ProjectEnv, ProjectLock},
};
use serde::Serialize;

use crate::api::Printable;

/// One declared environment value, normalized.
///
/// Deliberately NOT `ProjectEnv`'s own `Serialize`: that one round-trips the
/// TOML authoring grammar, emitting a constant as a bare string and a path as a
/// `{ type, value }` table. A JSON consumer would have to branch on
/// string-vs-object per key to read it. The report always emits both fields.
#[derive(Serialize)]
pub struct EnvValueOut {
    #[serde(rename = "type")]
    kind: ModifierKind,
    /// The declared separator for a `list`-typed value. `None` for every
    /// other kind, and for a `list` that declared none — the project surface
    /// may omit it, and what the omission inherits is decided at compose
    /// time, which status does not do. Skipped in JSON when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    separator: Option<String>,
    /// Verbatim as written. A relative `path` value stays relative — resolving
    /// it against the project root is composition, which `ocx inspect` and
    /// `ocx env` do.
    value: String,
}

/// One tool binding, as the two files describe it.
///
/// Absence is the signal, matching the `binaries` convention in the inspect
/// closure ("key absent on the wire means undeclared"):
///
/// - both keys — declared in `ocx.toml` and locked.
/// - no `platforms` — declared but not yet locked (added since the last
///   `ocx lock`).
/// - no `declared` — locked but no longer declared (orphaned in a stale lock).
#[derive(Serialize)]
pub struct ToolStatus {
    /// The `ocx.toml` value for this binding, verbatim (`ocx.sh/go-task:3`).
    #[serde(skip_serializing_if = "Option::is_none")]
    declared: Option<String>,
    /// EVERY platform leaf the lock records, not the host's. Picking the host
    /// leaf is resolution — `ocx inspect` does that.
    #[serde(skip_serializing_if = "Option::is_none")]
    platforms: Option<BTreeMap<String, String>>,
}

/// One group's declarations. `default` is a group like any other: the
/// top-level `[tools]` and `[env]` tables in `ocx.toml` ARE its tools and env,
/// which is why `default` is a reserved group name.
#[derive(Serialize)]
pub struct GroupStatus {
    tools: BTreeMap<String, ToolStatus>,
    /// This scope's `[env]` table alone — never merged with another scope's.
    /// Merging is composition, and a merged view cannot show which scope
    /// declared a key.
    env: BTreeMap<String, EnvValueOut>,
}

/// The `ocx.lock` header, plus whether the lock agrees with `ocx.toml`.
///
/// Everything but `present` is absent when there is no lock — that is a valid
/// state (nothing has run `ocx lock` yet), not an error, so status reports it
/// and exits 0. Same for a lock that exists but cannot be parsed (an
/// unsupported `lock_version`, a corrupt file): `error` carries why, and the
/// header fields stay absent. Status is the command reached for when the
/// project is broken, so failing the way every other command already fails
/// would leave it with nothing to say in exactly that case.
#[derive(Serialize)]
pub struct LockStatus {
    present: bool,
    /// Why the lock could not be parsed. Its presence IS the unreadable state
    /// — no separate boolean, which could only ever repeat what this key
    /// already says. The header fields and every binding's `platforms` are
    /// absent alongside it: nothing was read.
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// `true` when the lock's stored `declaration_hash` matches the current
    /// config's. `false` means `ocx.toml` changed since the last `ocx lock`.
    #[serde(skip_serializing_if = "Option::is_none")]
    current: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lock_version: Option<u8>,
    /// The hash stored in `ocx.lock`.
    #[serde(skip_serializing_if = "Option::is_none")]
    declaration_hash: Option<String>,
    /// The hash recomputed from `ocx.toml` — what the lock's stored hash would
    /// have to be for `current` to hold. Emitted alongside the stored one so a
    /// consumer sees *why* `current` is false rather than having to recompute
    /// the project's canonicalization itself.
    ///
    /// Named `expected`, not `current`: `current` one level up is a verdict
    /// about the lock, and reusing the word for a hash value would give it two
    /// meanings in one object.
    ///
    /// Both hashes cover `[tools]` and `[group.*]` only — `[env]` and
    /// `[package.*]` are excluded by design, so an edit to either leaves
    /// `current` true.
    declaration_hash_expected: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    generated_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generated_at: Option<String>,
}

/// Report emitted by `ocx status`.
///
/// JSON format: `{ project, lock, groups, package_settings }`.
///
/// `groups` and the maps inside it are keyed **objects**, not arrays: within one
/// scope a binding name and an env key are unique by construction (they are TOML
/// table keys), and the underlying maps are key-sorted, so there is no authored
/// order to lose and lookup-by-name is the natural access. `ocx inspect` uses
/// arrays for the opposite reason — a composed env can carry one key twice.
///
/// Plain format: a tree (groups contain tools and env). Status holds the same
/// single-table exemption `inspect` does — its content is inherently nested,
/// and flattening groups × tools × env into one row shape would encode the
/// structure as string prefixes.
#[derive(Serialize)]
pub struct StatusReport {
    /// Absolute path of the `ocx.toml` this report describes.
    project: String,
    lock: LockStatus,
    groups: BTreeMap<String, GroupStatus>,
    /// `[package."<id>"]` resolve-time policy, keyed by the canonical author
    /// string. Reported here because it is excluded from `declaration_hash`,
    /// so nothing lock-derived can surface it.
    package_settings: BTreeMap<String, PackageSettingsOut>,
}

/// Per-package resolve-time policy from `[package."<id>"]`.
#[derive(Serialize)]
pub struct PackageSettingsOut {
    #[serde(rename = "no-patches")]
    no_patches: bool,
}

fn env_out(env: &ProjectEnv) -> BTreeMap<String, EnvValueOut> {
    env.iter()
        .map(|(key, declared)| {
            (
                key.clone(),
                EnvValueOut {
                    kind: declared.kind.clone(),
                    separator: declared.separator.clone(),
                    value: declared.value.clone(),
                },
            )
        })
        .collect()
}

impl StatusReport {
    /// Project the loaded config and optional lock into the report.
    ///
    /// `lock` is `Ok(None)` when `ocx.lock` does not exist and `Err` when it
    /// exists but could not be parsed — both are states to report, not
    /// failures.
    pub fn new(project: &std::path::Path, config: &ProjectConfig, lock: Result<Option<&ProjectLock>, String>) -> Self {
        let lock = match lock {
            Ok(lock) => lock,
            Err(error) => return Self::unreadable_lock(project, config, error),
        };
        let config_hash = config.declaration_hash_cached().to_string();

        // Seed every declared group first (including `default`, whose tools and
        // env are the top-level tables), so a group with no lock entries still
        // appears rather than looking undeclared.
        let mut groups: BTreeMap<String, GroupStatus> = BTreeMap::new();
        groups.insert(
            ocx_lib::project::DEFAULT_GROUP.to_owned(),
            GroupStatus {
                tools: declared_tools(&config.tools),
                env: env_out(&config.env),
            },
        );
        for (name, group) in &config.groups {
            groups.insert(
                name.clone(),
                GroupStatus {
                    tools: declared_tools(&group.tools),
                    env: env_out(&group.env),
                },
            );
        }

        // Fold the lock in. A binding the lock knows but the config no longer
        // declares is orphaned — it lands with `declared` absent, which is the
        // only place that state is visible.
        for locked in lock.map(|lock| lock.tools.as_slice()).unwrap_or_default() {
            let entry = groups.entry(locked.group.clone()).or_insert_with(|| GroupStatus {
                tools: BTreeMap::new(),
                env: BTreeMap::new(),
            });
            let tool = entry.tools.entry(locked.name.clone()).or_insert_with(|| ToolStatus {
                declared: None,
                platforms: None,
            });
            tool.platforms = Some(
                locked
                    .platforms
                    .iter()
                    .map(|(key, digest)| (key.clone(), digest.to_string()))
                    .collect(),
            );
        }

        let lock_status = match lock {
            Some(lock) => LockStatus {
                present: true,
                error: None,
                current: Some(lock.metadata.declaration_hash == config_hash),
                lock_version: Some(lock.metadata.lock_version as u8),
                declaration_hash: Some(lock.metadata.declaration_hash.clone()),
                declaration_hash_expected: config_hash,
                generated_by: Some(lock.metadata.generated_by.clone()),
                generated_at: Some(lock.metadata.generated_at.clone()),
            },
            None => LockStatus {
                present: false,
                error: None,
                current: None,
                lock_version: None,
                declaration_hash: None,
                declaration_hash_expected: config_hash,
                generated_by: None,
                generated_at: None,
            },
        };

        Self {
            project: project.display().to_string(),
            lock: lock_status,
            groups,
            package_settings: config
                .packages
                .iter()
                .map(|(identifier, settings)| (identifier.clone(), PackageSettingsOut::from(settings)))
                .collect(),
        }
    }
}

impl StatusReport {
    /// The lock exists but did not parse. Report the declaration in full — it
    /// is still readable and still the thing the user needs to see — with the
    /// lock reduced to "present, unreadable, here is why".
    fn unreadable_lock(project: &std::path::Path, config: &ProjectConfig, error: String) -> Self {
        let mut groups: BTreeMap<String, GroupStatus> = BTreeMap::new();
        groups.insert(
            ocx_lib::project::DEFAULT_GROUP.to_owned(),
            GroupStatus {
                tools: declared_tools(&config.tools),
                env: env_out(&config.env),
            },
        );
        for (name, group) in &config.groups {
            groups.insert(
                name.clone(),
                GroupStatus {
                    tools: declared_tools(&group.tools),
                    env: env_out(&group.env),
                },
            );
        }

        Self {
            project: project.display().to_string(),
            lock: LockStatus {
                present: true,
                error: Some(error),
                current: None,
                lock_version: None,
                declaration_hash: None,
                declaration_hash_expected: config.declaration_hash_cached().to_string(),
                generated_by: None,
                generated_at: None,
            },
            groups,
            package_settings: config
                .packages
                .iter()
                .map(|(identifier, settings)| (identifier.clone(), PackageSettingsOut::from(settings)))
                .collect(),
        }
    }
}

impl From<&PackageSettings> for PackageSettingsOut {
    fn from(settings: &PackageSettings) -> Self {
        Self {
            no_patches: settings.no_patches,
        }
    }
}

fn declared_tools(tools: &BTreeMap<String, ocx_lib::oci::Identifier>) -> BTreeMap<String, ToolStatus> {
    tools
        .iter()
        .map(|(binding, identifier)| {
            (
                binding.clone(),
                ToolStatus {
                    declared: Some(identifier.to_string()),
                    platforms: None,
                },
            )
        })
        .collect()
}

/// A plain-text tree node. Built only for `print_plain`; JSON goes through the
/// `Serialize` impls above.
struct Node {
    label: String,
    annotations: Vec<String>,
    children: Vec<Node>,
}

impl Node {
    fn leaf(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            annotations: Vec::new(),
            children: Vec::new(),
        }
    }

    fn with_annotation(mut self, annotation: impl Into<String>) -> Self {
        self.annotations.push(annotation.into());
        self
    }

    fn branch(label: impl Into<String>, children: Vec<Node>) -> Self {
        Self {
            label: label.into(),
            annotations: Vec::new(),
            children,
        }
    }
}

impl TreeItem for Node {
    fn label(&self, _theme: &Theme) -> String {
        self.label.clone()
    }

    fn children(&self) -> &[Self] {
        &self.children
    }

    fn annotations(&self, theme: &Theme) -> Vec<Annotation> {
        self.annotations
            .iter()
            .map(|text| Annotation::new(theme.note(text)))
            .collect()
    }
}

impl ToolStatus {
    /// Host-facing summary of the lock state for the plain tree.
    ///
    /// The host leaf is picked for DISPLAY only — the JSON keeps every
    /// platform, because choosing one is resolution.
    fn plain_annotation(&self, host: &Platform) -> String {
        let Some(platforms) = &self.platforms else {
            return "not locked".to_owned();
        };
        let host_key = host.to_string();
        match platforms.get(&host_key) {
            Some(digest) => format!("{} platform(s), {host_key}: {digest}", platforms.len()),
            None => format!("{} platform(s), none for {host_key}", platforms.len()),
        }
    }
}

impl Printable for StatusReport {
    fn print_plain(&self, data: &DataInterface) {
        let host = Platform::current().unwrap_or_else(Platform::any);

        let mut sections = Vec::new();

        let lock_label = match (self.lock.present, self.lock.error.as_deref(), self.lock.current) {
            (false, _, _) => "lock: absent (run `ocx lock`)".to_owned(),
            (true, Some(error), _) => format!("lock: unreadable ({error})"),
            (true, _, Some(true)) => "lock: current".to_owned(),
            (true, _, _) => "lock: stale (ocx.toml changed since `ocx lock`)".to_owned(),
        };
        sections.push(Node::leaf(lock_label));

        for (name, group) in &self.groups {
            let mut children = Vec::new();
            for (binding, tool) in &group.tools {
                let declared = tool.declared.as_deref().unwrap_or("(not declared)");
                children
                    .push(Node::leaf(format!("{binding} = {declared}")).with_annotation(tool.plain_annotation(&host)));
            }
            for (key, value) in &group.env {
                children
                    .push(Node::leaf(format!("env {key} = {}", value.value)).with_annotation(value.kind.to_string()));
            }
            sections.push(Node::branch(format!("group {name}"), children));
        }

        for (identifier, settings) in &self.package_settings {
            sections.push(
                Node::leaf(format!("package {identifier}"))
                    .with_annotation(format!("no-patches = {}", settings.no_patches)),
            );
        }

        data.print_tree(&Node::branch(self.project.clone(), sections));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── separator field (W-10) ─────────────────────────────────────────────
    //
    // Driven through a real `ProjectEnv`, not a hand-built `EnvValueOut`: the
    // thing worth pinning is that a separator declared in `ocx.toml` reaches
    // the report, which a stubbed report value cannot show.

    /// Parse a `[env]` table the way `ocx.toml` loading does, then project it
    /// through the report's own mapping.
    fn declared(source: serde_json::Value) -> BTreeMap<String, EnvValueOut> {
        let env: ProjectEnv = serde_json::from_value(source).expect("the fixture must parse as an [env] table");
        env_out(&env)
    }

    #[test]
    fn list_value_serializes_type_and_separator_when_present() {
        let reported = declared(serde_json::json!({
            "GODEBUG": { "type": "list", "separator": ",", "value": "gctrace=1" }
        }));
        let json = serde_json::to_string(&reported).expect("serializes");
        assert!(json.contains(r#""type":"list""#), "kind must serialize as list: {json}");
        assert!(
            json.contains(r#""separator":",""#),
            "the declared separator must reach the report: {json}"
        );
    }

    #[test]
    fn constant_and_path_values_omit_the_separator_field() {
        let reported = declared(serde_json::json!({
            "CI": "1",
            "TOOLS": { "type": "path", "value": "bin" }
        }));
        let json = serde_json::to_string(&reported).expect("serializes");
        assert!(
            !json.contains("\"separator\""),
            "a non-list value must omit separator entirely: {json}"
        );
    }

    /// A `list` entry that declared no separator reports none — the value it
    /// will inherit is settled at compose time, which status does not run.
    #[test]
    fn list_value_without_a_declared_separator_omits_the_field() {
        let reported = declared(serde_json::json!({
            "GODEBUG": { "type": "list", "value": "gctrace=1" }
        }));
        let json = serde_json::to_string(&reported).expect("serializes");
        assert!(json.contains(r#""type":"list""#), "kind must serialize as list: {json}");
        assert!(
            !json.contains("\"separator\""),
            "an undeclared separator must not be invented: {json}"
        );
    }
}
