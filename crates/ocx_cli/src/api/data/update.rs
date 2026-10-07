// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The `ocx update` report — **what moved**, not what the lock now is.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

use ocx_console::{Cell, Column, DataInterface};
use ocx_oci::{Digest, PackageRef, Platform, Selection, select_best};
use ocx_project::{DEFAULT_GROUP, LockedTool, ProjectConfig, ProjectLock};
use serde::Serialize;

use crate::api::Printable;

/// The cell a `None` `from`/`to` renders as.
const ABSENT: &str = "-";

/// The cell an unknown release renders as in the default table.
const UNKNOWN: &str = "?";

/// One concrete-version lookup: a pin's pull identifier and the tag its declaration spells.
pub type VersionKey = (PackageRef, String);

/// One `(group, binding, platform)` pin that moved between the predecessor
/// lock and the candidate.
///
/// `from` is absent when the pin is newly introduced (a binding or a platform
/// the predecessor did not carry); `to` is absent when it was dropped. Both
/// are the full pull identifier — `registry/repository@sha256:<hex>` — so a
/// consumer can feed either straight back to `ocx pull` without rebuilding it
/// from parts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct BindingChange {
    /// Local binding name (the `ocx.toml` key).
    pub name: String,
    /// Owning group — `default` for the top-level `[tools]` table.
    pub group: String,
    /// The platform the pin belongs to.
    pub platform: Platform,
    /// The tag the declaration spells; absent when it is digest-pinned.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// Pull identifier before the update; absent when newly pinned.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<PackageRef>,
    /// Pull identifier after the update; absent when dropped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<PackageRef>,
    /// The release `tag` named before the update (`3` -> `3.28.3`); absent when unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_version: Option<String>,
    /// The release `tag` names after the update; absent when unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_version: Option<String>,
}

/// One `(group, binding, platform)` pin the update left where it was.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct BindingState {
    /// Local binding name (the `ocx.toml` key).
    pub name: String,
    /// Owning group — `default` for the top-level `[tools]` table.
    pub group: String,
    /// The platform the pin belongs to.
    pub platform: Platform,
    /// The tag the declaration spells; absent when it is digest-pinned.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// The unchanged pull identifier — the same
    /// `registry/repository@sha256:<hex>` form `changes[].from` / `to`
    /// carry, so the two arrays are directly comparable.
    pub identifier: PackageRef,
    /// The release `tag` names (`3` -> `3.28.4`); absent when unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Report emitted by `ocx update` (and by `ocx update --check` before it
/// exits 65).
///
/// A version is best effort: a miss, an `--offline` or `--frozen` run, or a digest-pinned binding
/// omits it. **The payload is identical with and without `--verbose`**: verbosity changes the
/// plain rendering only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct UpdateReport {
    /// Pins whose pull identifier differs between the two locks, ordered by
    /// `(group, name, platform)`.
    pub changes: Vec<BindingChange>,
    /// Bindings this run examined and found unchanged, in the same order.
    ///
    /// A scoped run (`-g` / positional names) lists only its own selection;
    /// every other pin is carried forward verbatim, never re-resolved. A
    /// whole-lock run examines everything.
    // Never list a carried-forward pin: "unchanged" would claim a check that did not happen.
    pub unchanged: Vec<BindingState>,
    /// Whether the load-bearing lock metadata moved — `declaration_hash`,
    /// `declaration_hash_version` or `lock_version`. Advisory metadata
    /// (`generated_at`, `generated_by`) is deliberately ignored: it moves on
    /// every write and would make every `--check` fail.
    pub metadata_changed: bool,
}

/// The compared value of one pin: the pull identifier, not the bare digest.
// A digest compare reports "unchanged" for a binding whose repository moved to a mirror.
fn pull_identifier(tool: &LockedTool, leaf: &ocx_oci::Digest) -> PackageRef {
    tool.repository.pin_untagged(leaf.clone()).into()
}

/// Flatten a lock into `(group, name, platform key) -> pull identifier`.
// `BTreeMap`, not the `tools` order: an in-memory candidate need not be sorted, and the report's order comes from here.
fn pins(lock: &ProjectLock) -> BTreeMap<(String, String, String), PackageRef> {
    let mut out = BTreeMap::new();
    for tool in &lock.tools {
        for (platform, leaf) in &tool.platforms {
            out.insert(
                (tool.group.clone(), tool.name.clone(), platform.clone()),
                pull_identifier(tool, leaf),
            );
        }
    }
    out
}

/// The tag the declaration spells for `(group, name)`; `None` when digest-pinned or no longer declared.
fn declared_tag(config: &ProjectConfig, group: &str, name: &str) -> Option<String> {
    let table = if group == DEFAULT_GROUP {
        &config.tools
    } else {
        &config.groups.get(group)?.tools
    };
    table.get(name)?.tag().map(str::to_owned)
}

/// The digest column for one pull identifier: the CLI-wide `sha256:<12hex>` abbreviation.
fn short_digest(pull: &PackageRef) -> String {
    pull.digest()
        .map_or_else(|| pull.to_string(), |digest| digest.to_short_string())
}

/// A digest cell, followed by the release it is when known: `sha256:<12hex> (3.28.4)`.
fn pin_cell(pull: Option<&PackageRef>, version: Option<&str>) -> String {
    match (pull, version) {
        (None, _) => ABSENT.to_owned(),
        (Some(pull), None) => short_digest(pull),
        (Some(pull), Some(version)) => format!("{} ({version})", short_digest(pull)),
    }
}

/// `name` or `name:tag` — the binding as the user spelled it in `ocx.toml`.
fn binding_label(name: &str, tag: Option<&str>) -> String {
    match tag {
        Some(tag) => format!("{name}:{tag}"),
        None => name.to_owned(),
    }
}

impl UpdateReport {
    /// Diff `previous` (`None` when no `ocx.lock` exists yet) against `next`, keyed by `(group, name, platform)`.
    ///
    /// `examined` is a scoped run's `(group, name)` selection (`None` for a whole-lock run) and narrows
    /// [`Self::unchanged`] only: `changes` is never filtered, so an out-of-scope pin that moves still shows.
    ///
    /// # Errors
    ///
    /// A platform key that is not a canonical platform string; a lock that loaded has none.
    pub fn diff(
        previous: Option<&ProjectLock>,
        next: &ProjectLock,
        config: &ProjectConfig,
        examined: Option<&[(String, String)]>,
    ) -> Result<Self, ocx_oci::platform::error::PlatformError> {
        let before = previous.map(pins).unwrap_or_default();
        let after = pins(next);
        let in_scope = |group: &String, name: &String| {
            examined.is_none_or(|scope| scope.iter().any(|(g, n)| g == group && n == name))
        };

        let mut changes = Vec::new();
        let mut unchanged = Vec::new();
        let keys: BTreeSet<&(String, String, String)> = before.keys().chain(after.keys()).collect();
        for key in keys {
            let (group, name, platform) = key;
            let platform: Platform = platform.parse()?;
            let tag = declared_tag(config, group, name);
            let from = before.get(key);
            let to = after.get(key);
            match (from, to) {
                (Some(from), Some(to)) if from == to => {
                    if in_scope(group, name) {
                        unchanged.push(BindingState {
                            name: name.clone(),
                            group: group.clone(),
                            platform,
                            tag,
                            identifier: to.clone(),
                            version: None,
                        });
                    }
                }
                _ => changes.push(BindingChange {
                    name: name.clone(),
                    group: group.clone(),
                    platform,
                    tag,
                    from: from.cloned(),
                    to: to.cloned(),
                    from_version: None,
                    to_version: None,
                }),
            }
        }

        Ok(Self {
            changes,
            unchanged,
            metadata_changed: previous.is_none_or(|prev| {
                next.metadata.declaration_hash != prev.metadata.declaration_hash
                    || next.metadata.declaration_hash_version != prev.metadata.declaration_hash_version
                    || next.metadata.lock_version != prev.metadata.lock_version
            }),
        })
    }

    /// The concrete-version lookups the report needs, one per `(pull identifier, tag)`, each
    /// with the `platform -> leaf digest` pins it must match. Digest-pinned rows need none.
    pub fn version_lookups(&self) -> BTreeMap<VersionKey, BTreeMap<String, Digest>> {
        let changed = self.changes.iter().flat_map(|change| {
            [&change.from, &change.to]
                .into_iter()
                .flatten()
                .map(|pull| (pull, change.tag.as_ref(), &change.platform))
        });
        let held = self
            .unchanged
            .iter()
            .map(|state| (&state.identifier, state.tag.as_ref(), &state.platform));
        let mut lookups: BTreeMap<VersionKey, BTreeMap<String, Digest>> = BTreeMap::new();
        for (pull, tag, platform) in changed.chain(held) {
            let (Some(tag), Some(leaf)) = (tag, pull.digest()) else {
                continue;
            };
            lookups
                .entry((pull.clone(), tag.clone()))
                .or_default()
                .insert(platform.to_string(), leaf);
        }
        lookups
    }

    /// Fill every version field from `versions`, keyed as [`Self::version_lookups`] keys them.
    pub fn fill_versions(&mut self, versions: &BTreeMap<VersionKey, String>) {
        let find =
            |pull: Option<&PackageRef>, tag: &Option<String>| versions.get(&(pull?.clone(), tag.clone()?)).cloned();
        for change in &mut self.changes {
            change.from_version = find(change.from.as_ref(), &change.tag);
            change.to_version = find(change.to.as_ref(), &change.tag);
        }
        for state in &mut self.unchanged {
            state.version = find(Some(&state.identifier), &state.tag);
        }
    }

    /// Whether this update would move anything on disk — the `--check` verdict.
    pub fn moved(&self) -> bool {
        !self.changes.is_empty() || self.metadata_changed
    }

    /// The moved changes, one slice per `(group, name)` binding (`changes` is ordered by it).
    fn moved_bindings(&self) -> impl Iterator<Item = &[BindingChange]> {
        self.changes.chunk_by(|a, b| a.group == b.group && a.name == b.name)
    }

    /// The default table's rows, one per moved binding.
    ///
    /// Platforms that disagree show the `host` platform's releases; without a moved host leaf, the
    /// releases most platforms share (the first such platform on a tie).
    fn version_rows(&self, host: Option<&Platform>) -> Vec<VersionRow> {
        self.moved_bindings()
            .map(|binding| {
                let cells: Vec<(String, String)> = binding
                    .iter()
                    .map(|change| {
                        (
                            version_cell(change.from.as_ref(), change.from_version.as_deref()),
                            version_cell(change.to.as_ref(), change.to_version.as_deref()),
                        )
                    })
                    .collect();
                let shared = |i: usize| cells.iter().filter(|cell| **cell == cells[i]).count();
                let majority = (0..cells.len()).max_by_key(|&i| (shared(i), Reverse(i))).unwrap_or(0);
                let pick = host.and_then(|host| host_row(binding, host)).unwrap_or(majority);
                let (from, to) = cells[pick].clone();
                VersionRow {
                    binding: binding_label(&binding[0].name, binding[0].tag.as_deref()),
                    group: binding[0].group.clone(),
                    from,
                    to,
                }
            })
            .collect()
    }

    /// The default table: one row per tool, releases not digests, `Group` only when rows span several.
    fn render_versions(&self, printer: &DataInterface) {
        let rows = self.version_rows(Platform::current().as_ref());
        let grouped = rows.iter().any(|row| row.group != rows[0].group);
        let width = rows.iter().map(|row| row.from.chars().count()).max().unwrap_or(0);
        let mut columns: Vec<Column> = vec!["Binding".into()];
        if grouped {
            columns.push("Group".into());
        }
        columns.extend(["From".into(), "To".into()]);
        let mut cells: Vec<Vec<Cell>> = columns.iter().map(|_| Vec::new()).collect();
        for row in rows {
            let mut line = vec![row.binding];
            if grouped {
                line.push(row.group);
            }
            // In the From cell, so the arrow sits one space after the widest release.
            line.push(format!("{:<width$} →", row.from));
            line.push(row.to);
            for (column, text) in cells.iter_mut().zip(line) {
                column.push(Cell::from(text));
            }
        }
        // A header over no rows says nothing the hint and the summary do not.
        if !cells[0].is_empty() {
            printer.print_table(&columns, &cells);
        }
        if let Some(hint) = self.unchanged_hint() {
            printer.print_hint(&hint);
        }
    }

    /// The `--verbose` table: one row per platform with its digests, then the pins that held still.
    fn render_pins(&self, printer: &DataInterface) {
        let mut rows: [Vec<Cell>; 5] = [Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        let mut push = |binding: String, group: &str, platform: &Platform, from: String, to: String| {
            rows[0].push(Cell::from(binding));
            rows[1].push(Cell::from(group.to_owned()));
            rows[2].push(Cell::from(platform.to_string()));
            rows[3].push(Cell::from(from));
            rows[4].push(Cell::from(to));
        };
        for change in &self.changes {
            push(
                binding_label(&change.name, change.tag.as_deref()),
                &change.group,
                &change.platform,
                pin_cell(change.from.as_ref(), change.from_version.as_deref()),
                pin_cell(change.to.as_ref(), change.to_version.as_deref()),
            );
        }
        for state in &self.unchanged {
            let pinned = pin_cell(Some(&state.identifier), state.version.as_deref());
            push(
                binding_label(&state.name, state.tag.as_deref()),
                &state.group,
                &state.platform,
                pinned.clone(),
                pinned,
            );
        }
        printer.print_table(
            &[
                "Binding".into(),
                "Group".into(),
                "Platform".into(),
                "From".into(),
                "To".into(),
            ],
            &rows,
        );
    }

    /// Counts the bindings that held on every platform; `None` when there are none.
    fn unchanged_hint(&self) -> Option<String> {
        let moved: BTreeSet<(&str, &str)> = self
            .changes
            .iter()
            .map(|change| (change.group.as_str(), change.name.as_str()))
            .collect();
        let held: BTreeSet<(&str, &str)> = self
            .unchanged
            .iter()
            .map(|state| (state.group.as_str(), state.name.as_str()))
            .filter(|binding| !moved.contains(binding))
            .collect();
        let count = held.len();
        let (noun, pronoun) = if count == 1 { ("tool", "it") } else { ("tools", "them") };
        (count > 0).then(|| format!("{count} unchanged {noun} not shown; re-run with --verbose to list {pronoun}"))
    }

    /// The closing line: what `--check` would move, or what a real run moved; `None` when nothing did.
    pub fn summary(&self, check: bool) -> Option<String> {
        let count = self.moved_bindings().count();
        let tools = if count == 1 { "tool" } else { "tools" };
        match (check, count) {
            (true, 0) if self.metadata_changed => {
                Some("ocx.lock metadata would change; run `ocx update` to apply".to_owned())
            }
            (_, 0) => None,
            (true, _) => Some(format!("{count} {tools} would move; run `ocx update` to apply")),
            (false, _) => Some(format!("{count} {tools} moved")),
        }
    }
}

/// One row of the default table.
#[derive(Debug, PartialEq, Eq)]
struct VersionRow {
    binding: String,
    group: String,
    from: String,
    to: String,
}

/// The index of `binding`'s change on the leaf `host` would run, by the same selection a lock read uses.
fn host_row(binding: &[BindingChange], host: &Platform) -> Option<usize> {
    let candidates: Vec<(usize, Platform)> = binding
        .iter()
        .enumerate()
        .map(|(i, change)| (i, change.platform.clone()))
        .collect();
    match select_best(host, &candidates) {
        Selection::Found(i) => Some(i),
        Selection::Ambiguous(_) | Selection::None => None,
    }
}

/// A release cell: `-` when the pin is absent on that side, `?` when its release is unknown.
fn version_cell(pull: Option<&PackageRef>, version: Option<&str>) -> String {
    match (pull, version) {
        (None, _) => ABSENT.to_owned(),
        (Some(_), None) => UNKNOWN.to_owned(),
        (Some(_), Some(version)) => version.to_owned(),
    }
}

impl Printable for UpdateReport {
    const SCHEMA_VERSION: u32 = 1;
    const ROOT: &'static str = "UpdateReport";

    fn print_plain(&self, printer: &DataInterface) {
        self.render_versions(printer);
    }
}

/// [`UpdateReport`] per platform with digests, plus the pins that held still — `ocx update --verbose`.
///
/// JSON delegates to the inner report: the wire shape is identical with or without `--verbose`.
pub struct VerboseUpdateReport(pub UpdateReport);

impl Printable for VerboseUpdateReport {
    const SCHEMA_VERSION: u32 = 1;
    const ROOT: &'static str = "VerboseUpdateReport";

    fn print_plain(&self, printer: &DataInterface) {
        self.0.render_pins(printer);
    }
}

impl Serialize for VerboseUpdateReport {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl schemars::JsonSchema for VerboseUpdateReport {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "VerboseUpdateReport".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        <UpdateReport>::json_schema(generator)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ocx_oci::{Digest, PackageRef};
    use ocx_project::{LockMetadata, LockVersion};

    use super::*;

    /// A 64-hex sha256 filled with one character.
    fn digest_of(byte: char) -> Digest {
        Digest::Sha256(std::iter::repeat_n(byte, 64).collect())
    }

    fn metadata(declaration_hash: char) -> LockMetadata {
        LockMetadata {
            lock_version: LockVersion::V3,
            declaration_hash_version: 1,
            declaration_hash: format!(
                "sha256:{}",
                std::iter::repeat_n(declaration_hash, 64).collect::<String>()
            ),
            generated_by: "ocx 0.0.0-test".to_string(),
            // Advisory: moves on every write, must never reach the verdict.
            generated_at: "2026-09-21T00:00:00Z".to_string(),
        }
    }

    /// One locked tool in the default group, at `ocx.sh/<repo>`, with the
    /// given `(platform key, digest byte)` leaves.
    fn tool(name: &str, repo: &str, leaves: &[(&str, char)]) -> LockedTool {
        LockedTool {
            name: name.to_string(),
            group: DEFAULT_GROUP.to_string(),
            repository: ocx_oci::Repository::new("ocx.sh", repo),
            platforms: leaves
                .iter()
                .map(|(key, byte)| ((*key).to_string(), digest_of(*byte)))
                .collect(),
        }
    }

    fn lock(declaration_hash: char, tools: Vec<LockedTool>) -> ProjectLock {
        ProjectLock {
            metadata: metadata(declaration_hash),
            tools,
        }
    }

    /// `[tools] cmake = "ocx.sh/cmake:3.28"` — the declaration the tag column
    /// reads from.
    fn config() -> ProjectConfig {
        let declared = PackageRef::new_registry("cmake", "ocx.sh").clone_with_tag("3.28");
        ProjectConfig::from_parts(BTreeMap::from([("cmake".to_string(), declared)]), BTreeMap::new())
    }

    /// The pull identifier `tool()` produces for `repo` at the leaf filled
    /// with `byte` — built the way the report builds it, from the parts.
    fn pull(repo: &str, byte: char) -> PackageRef {
        PackageRef::new_registry(repo, "ocx.sh").clone_with_digest(digest_of(byte))
    }

    const LINUX: &str = "linux/amd64";
    const MAC: &str = "darwin/arm64";

    /// The digest byte moved, the repository did not: one change row carrying
    /// both pull identifiers.
    #[test]
    fn digest_move_is_one_change() {
        let previous = lock('d', vec![tool("cmake", "cmake", &[(LINUX, 'a')])]);
        let next = lock('d', vec![tool("cmake", "cmake", &[(LINUX, 'b')])]);

        let report = UpdateReport::diff(Some(&previous), &next, &config(), None).expect("canonical platform keys");

        assert!(report.unchanged.is_empty(), "the only pin moved");
        assert!(!report.metadata_changed, "the declaration did not change");
        assert_eq!(report.changes.len(), 1);
        let change = &report.changes[0];
        assert_eq!(change.name, "cmake");
        assert_eq!(change.group, DEFAULT_GROUP);
        assert_eq!(change.platform.to_string(), LINUX);
        assert_eq!(change.tag.as_deref(), Some("3.28"), "the tag comes from ocx.toml");
        assert_eq!(
            change.from,
            Some(pull("cmake", 'a')),
            "the full pull identifier, not a bare digest"
        );
        assert_eq!(change.to, Some(pull("cmake", 'b')));
        assert!(report.moved());
    }

    /// The publisher started shipping a second platform: the new leaf is a
    /// change with no `from`, the old leaf is unchanged.
    #[test]
    fn added_platform_has_no_from() {
        let previous = lock('d', vec![tool("cmake", "cmake", &[(LINUX, 'a')])]);
        let next = lock('d', vec![tool("cmake", "cmake", &[(LINUX, 'a'), (MAC, 'c')])]);

        let report = UpdateReport::diff(Some(&previous), &next, &config(), None).expect("canonical platform keys");

        assert_eq!(report.changes.len(), 1);
        assert_eq!(report.changes[0].platform.to_string(), MAC);
        assert_eq!(
            report.changes[0].from, None,
            "a newly shipped platform has no predecessor"
        );
        assert!(report.changes[0].to.is_some());
        assert_eq!(report.unchanged.len(), 1);
        assert_eq!(report.unchanged[0].platform.to_string(), LINUX);
    }

    /// The publisher stopped shipping a platform: `to` is `None`.
    #[test]
    fn dropped_platform_has_no_to() {
        let previous = lock('d', vec![tool("cmake", "cmake", &[(LINUX, 'a'), (MAC, 'c')])]);
        let next = lock('d', vec![tool("cmake", "cmake", &[(LINUX, 'a')])]);

        let report = UpdateReport::diff(Some(&previous), &next, &config(), None).expect("canonical platform keys");

        assert_eq!(report.changes.len(), 1);
        assert_eq!(report.changes[0].platform.to_string(), MAC);
        assert!(report.changes[0].from.is_some());
        assert_eq!(report.changes[0].to, None, "a dropped platform has no successor");
    }

    /// A binding that did not exist before is a change per platform, all with
    /// `from: None`.
    #[test]
    fn added_binding_has_no_from() {
        let previous = lock('d', vec![tool("cmake", "cmake", &[(LINUX, 'a')])]);
        let next = lock(
            'd',
            vec![
                tool("cmake", "cmake", &[(LINUX, 'a')]),
                tool("ninja", "ninja", &[(LINUX, 'e')]),
            ],
        );

        let report = UpdateReport::diff(Some(&previous), &next, &config(), None).expect("canonical platform keys");

        assert_eq!(report.changes.len(), 1);
        assert_eq!(report.changes[0].name, "ninja");
        assert_eq!(report.changes[0].from, None);
        assert_eq!(
            report.changes[0].tag, None,
            "ninja is not declared in the sample ocx.toml, so no tag is known"
        );
    }

    /// A binding that disappeared is a change per platform, all with
    /// `to: None`.
    #[test]
    fn dropped_binding_has_no_to() {
        let previous = lock(
            'd',
            vec![
                tool("cmake", "cmake", &[(LINUX, 'a')]),
                tool("ninja", "ninja", &[(LINUX, 'e')]),
            ],
        );
        let next = lock('d', vec![tool("cmake", "cmake", &[(LINUX, 'a')])]);

        let report = UpdateReport::diff(Some(&previous), &next, &config(), None).expect("canonical platform keys");

        assert_eq!(report.changes.len(), 1);
        assert_eq!(report.changes[0].name, "ninja");
        assert_eq!(report.changes[0].to, None);
    }

    /// **The case a bare digest comparison misses.** Same leaf digest, new
    /// repository — a different pull, so a change.
    #[test]
    fn repository_move_is_a_change() {
        let previous = lock('d', vec![tool("cmake", "cmake", &[(LINUX, 'a')])]);
        let next = lock('d', vec![tool("cmake", "mirror/cmake", &[(LINUX, 'a')])]);

        let report = UpdateReport::diff(Some(&previous), &next, &config(), None).expect("canonical platform keys");

        assert_eq!(
            report.changes.len(),
            1,
            "a repository move is a different pull even at the same digest"
        );
        assert_eq!(report.changes[0].from, Some(pull("cmake", 'a')));
        assert_eq!(report.changes[0].to, Some(pull("mirror/cmake", 'a')));
        assert_ne!(
            report.changes[0].from, report.changes[0].to,
            "the repository, not the digest, is what differs"
        );
        assert!(report.unchanged.is_empty());
    }

    /// Nothing moved but the declaration hash did: no change rows, and the
    /// verdict is still "moved" so `--check` refuses.
    #[test]
    fn metadata_only_change_still_counts_as_moved() {
        let previous = lock('d', vec![tool("cmake", "cmake", &[(LINUX, 'a')])]);
        let next = lock('f', vec![tool("cmake", "cmake", &[(LINUX, 'a')])]);

        let report = UpdateReport::diff(Some(&previous), &next, &config(), None).expect("canonical platform keys");

        assert!(report.changes.is_empty(), "no pin moved");
        assert_eq!(report.unchanged.len(), 1);
        assert!(report.metadata_changed, "the declaration hash moved");
        assert!(report.moved(), "--check must refuse a metadata-only change");
    }

    /// Two byte-identical locks: nothing moved, nothing to report, exit 0.
    #[test]
    fn identical_locks_have_not_moved() {
        let previous = lock('d', vec![tool("cmake", "cmake", &[(LINUX, 'a')])]);
        let next = lock('d', vec![tool("cmake", "cmake", &[(LINUX, 'a')])]);

        let report = UpdateReport::diff(Some(&previous), &next, &config(), None).expect("canonical platform keys");

        assert!(report.changes.is_empty());
        assert!(!report.metadata_changed);
        assert!(!report.moved(), "an unchanged lock must exit 0");
    }

    /// `generated_at` / `generated_by` move on every write; weighing them
    /// would make every `--check` fail.
    #[test]
    fn advisory_metadata_is_ignored() {
        let previous = lock('d', vec![tool("cmake", "cmake", &[(LINUX, 'a')])]);
        let mut next = lock('d', vec![tool("cmake", "cmake", &[(LINUX, 'a')])]);
        next.metadata.generated_at = "2030-01-01T00:00:00Z".to_string();
        next.metadata.generated_by = "ocx 99.0.0".to_string();

        assert!(
            !UpdateReport::diff(Some(&previous), &next, &config(), None)
                .expect("canonical platform keys")
                .moved()
        );
    }

    /// No predecessor lock (bare `ocx update` before any `ocx lock`): every
    /// pin is newly introduced.
    #[test]
    fn absent_predecessor_reports_every_pin_as_new() {
        let next = lock('d', vec![tool("cmake", "cmake", &[(LINUX, 'a'), (MAC, 'c')])]);

        let report = UpdateReport::diff(None, &next, &config(), None).expect("canonical platform keys");

        assert_eq!(report.changes.len(), 2);
        assert!(report.changes.iter().all(|c| c.from.is_none()));
        assert!(report.unchanged.is_empty());
        assert!(report.metadata_changed);
    }

    /// The shipped contract for the flag name: `--verbose` is a rendering
    /// tier, never a different payload.
    #[test]
    fn verbose_serializes_identically() {
        let previous = lock('d', vec![tool("cmake", "cmake", &[(LINUX, 'a'), (MAC, 'c')])]);
        let next = lock('d', vec![tool("cmake", "cmake", &[(LINUX, 'b'), (MAC, 'c')])]);
        let report = UpdateReport::diff(Some(&previous), &next, &config(), None).expect("canonical platform keys");

        assert_eq!(report.changes.len(), 1, "one moved pin");
        assert_eq!(report.unchanged.len(), 1, "one held still");
        let plain = serde_json::to_value(&report).expect("the report serializes");
        let verbose = serde_json::to_value(VerboseUpdateReport(report)).expect("the wrapper serializes");
        assert_eq!(plain, verbose, "--verbose must not change the JSON payload");
        assert!(
            plain.get("changes").is_some()
                && plain.get("unchanged").is_some()
                && plain.get("metadata_changed").is_some(),
            "the three contracted keys are present: {plain}"
        );
    }

    /// The digest column is the CLI-wide abbreviation — algorithm prefix
    /// included — not a bare or hand-truncated hex string.
    #[test]
    fn short_digest_is_the_shared_cli_abbreviation() {
        let rendered = short_digest(&pull("cmake", 'a'));
        assert_eq!(
            rendered,
            digest_of('a').to_short_string(),
            "the column must be whatever `Digest::to_short_string` renders"
        );
        assert!(
            rendered.starts_with("sha256:"),
            "the algorithm prefix is part of the shared abbreviation: {rendered}"
        );
        assert_eq!(ABSENT, "-");
    }

    /// A scoped run examines only its own selection, so a pin it carried
    /// forward verbatim is never reported as checked-and-unchanged.
    #[test]
    fn scoped_diff_excludes_out_of_scope_bindings_from_unchanged() {
        let tools = vec![
            tool("cmake", "cmake", &[(LINUX, 'a')]),
            tool("ninja", "ninja", &[(LINUX, 'e')]),
        ];
        let previous = lock('d', tools.clone());
        let next = lock('d', tools);
        let scope = [(DEFAULT_GROUP.to_string(), "cmake".to_string())];

        let scoped =
            UpdateReport::diff(Some(&previous), &next, &config(), Some(&scope)).expect("canonical platform keys");

        assert!(scoped.changes.is_empty(), "nothing moved either way");
        assert_eq!(
            scoped.unchanged.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["cmake"],
            "ninja was carried forward verbatim, never re-resolved"
        );
        assert!(!scoped.moved(), "an untouched scope still exits 0");

        let whole = UpdateReport::diff(Some(&previous), &next, &config(), None).expect("canonical platform keys");
        assert_eq!(
            whole.unchanged.len(),
            2,
            "a whole-lock run examines every binding, so nothing is filtered"
        );
    }

    /// `Binding` carries the declared tag so the table needs no sixth column.
    #[test]
    fn binding_label_appends_the_declared_tag() {
        assert_eq!(binding_label("cmake", Some("3.28")), "cmake:3.28");
        assert_eq!(
            binding_label("cmake", None),
            "cmake",
            "a digest-pinned binding has no tag"
        );
    }

    /// The hint counts tools, not pins, skips a tool that moved elsewhere, and agrees in number.
    #[test]
    fn unchanged_hint_counts_tools_that_held_everywhere() {
        let tools = vec![
            tool("cmake", "cmake", &[(LINUX, 'a'), (MAC, 'c')]),
            tool("ninja", "ninja", &[(LINUX, 'e'), (MAC, 'f')]),
        ];
        let held = UpdateReport::diff(Some(&lock('d', tools.clone())), &lock('d', tools), &config(), None)
            .expect("canonical platform keys");
        assert_eq!(held.unchanged.len(), 4, "four pins held");
        assert_eq!(
            held.unchanged_hint().as_deref(),
            Some("2 unchanged tools not shown; re-run with --verbose to list them")
        );

        // cmake moves on Linux only: its held macOS pin is a verbose detail, not an unchanged tool.
        let previous = lock(
            'd',
            vec![
                tool("cmake", "cmake", &[(LINUX, 'a'), (MAC, 'c')]),
                tool("ninja", "ninja", &[(LINUX, 'e')]),
            ],
        );
        let next = lock(
            'd',
            vec![
                tool("cmake", "cmake", &[(LINUX, 'b'), (MAC, 'c')]),
                tool("ninja", "ninja", &[(LINUX, 'e')]),
            ],
        );
        let one = UpdateReport::diff(Some(&previous), &next, &config(), None).expect("canonical platform keys");
        assert_eq!(
            one.unchanged_hint().as_deref(),
            Some("1 unchanged tool not shown; re-run with --verbose to list it")
        );

        let moved = lock('d', vec![tool("cmake", "cmake", &[(LINUX, 'b')])]);
        let none = UpdateReport::diff(
            Some(&lock('d', vec![tool("cmake", "cmake", &[(LINUX, 'a')])])),
            &moved,
            &config(),
            None,
        )
        .expect("canonical platform keys");
        assert_eq!(none.unchanged_hint(), None, "nothing hidden, no hint");
    }

    /// `cmake:3.28` moving on Linux, macOS and Windows with the given `(from, to)` releases.
    fn three_platform_move(versions: [(Option<&str>, Option<&str>); 3]) -> UpdateReport {
        const WINDOWS: &str = "windows/amd64";
        let previous = lock(
            'd',
            vec![tool("cmake", "cmake", &[(LINUX, 'a'), (MAC, 'b'), (WINDOWS, 'c')])],
        );
        let next = lock(
            'd',
            vec![tool("cmake", "cmake", &[(LINUX, 'd'), (MAC, 'e'), (WINDOWS, 'f')])],
        );
        let mut report = UpdateReport::diff(Some(&previous), &next, &config(), None).expect("canonical platform keys");
        // `changes` is ordered by platform: darwin, linux, windows.
        for (change, (from, to)) in report.changes.iter_mut().zip(versions) {
            change.from_version = from.map(str::to_owned);
            change.to_version = to.map(str::to_owned);
        }
        report
    }

    fn row(from: &str, to: &str) -> VersionRow {
        VersionRow {
            binding: "cmake:3.28".to_string(),
            group: DEFAULT_GROUP.to_string(),
            from: from.to_string(),
            to: to.to_string(),
        }
    }

    /// Agreeing platforms collapse to one row; an unknown release is `?`.
    #[test]
    fn version_rows_collapse_agreeing_platforms() {
        let report = three_platform_move([(None, Some("3.28.4")); 3]);
        assert_eq!(report.version_rows(None), vec![row("?", "3.28.4")]);
    }

    /// Disagreeing platforms still give one row: the host's releases when its leaf moved.
    #[test]
    fn version_rows_prefer_the_host_platform() {
        let report = three_platform_move([
            (Some("3.28.3"), Some("3.28.4")),
            (Some("3.28.3"), Some("3.28.5")),
            (Some("3.28.3"), Some("3.28.4")),
        ]);
        let linux: Platform = LINUX.parse().expect("platform");
        assert_eq!(report.version_rows(Some(&linux)), vec![row("3.28.3", "3.28.5")]);
    }

    /// No host leaf among the moved pins: the releases most platforms share.
    #[test]
    fn version_rows_fall_back_to_the_majority() {
        let report = three_platform_move([
            (Some("3.28.3"), Some("3.28.4")),
            (Some("3.28.3"), Some("3.28.5")),
            (Some("3.28.3"), Some("3.28.4")),
        ]);
        let host: Platform = "linux/arm64".parse().expect("platform");
        assert_eq!(report.version_rows(Some(&host)), vec![row("3.28.3", "3.28.4")]);
        assert_eq!(report.version_rows(None), vec![row("3.28.3", "3.28.4")]);
    }

    /// The closing line counts tools, names the next command under `--check`, and is absent when nothing moved.
    #[test]
    fn summary_counts_tools_and_names_the_next_command() {
        let report = three_platform_move([(None, None); 3]);
        assert_eq!(
            report.summary(true).as_deref(),
            Some("1 tool would move; run `ocx update` to apply"),
            "three moved platforms are one tool"
        );
        assert_eq!(report.summary(false).as_deref(), Some("1 tool moved"));

        let same = lock('d', vec![tool("cmake", "cmake", &[(LINUX, 'a')])]);
        let still = UpdateReport::diff(Some(&same), &same, &config(), None).expect("canonical platform keys");
        assert_eq!((still.summary(true), still.summary(false)), (None, None));

        let rehashed = lock('f', vec![tool("cmake", "cmake", &[(LINUX, 'a')])]);
        let metadata = UpdateReport::diff(Some(&same), &rehashed, &config(), None).expect("canonical platform keys");
        assert!(
            metadata.summary(true).is_some_and(|line| line.contains("metadata")),
            "a metadata-only move still explains the 65"
        );
    }

    // ── concrete versions ───────────────────────────────────────────────

    /// `cmake` moves on Linux and holds on macOS; `pinned` is declared by
    /// digest, so it carries no tag.
    fn versioned_report() -> UpdateReport {
        let previous = lock(
            'd',
            vec![
                tool("cmake", "cmake", &[(LINUX, 'a'), (MAC, 'c')]),
                tool("pinned", "pinned", &[(LINUX, 'e')]),
            ],
        );
        let next = lock(
            'd',
            vec![
                tool("cmake", "cmake", &[(LINUX, 'b'), (MAC, 'c')]),
                tool("pinned", "pinned", &[(LINUX, 'f')]),
            ],
        );
        UpdateReport::diff(Some(&previous), &next, &config(), None).expect("canonical platform keys")
    }

    fn key(repo: &str, byte: char) -> VersionKey {
        (pull(repo, byte), "3.28".to_string())
    }

    /// One lookup per tagged pin, each keyed to its own platform's leaf; a
    /// digest-pinned binding needs none.
    #[test]
    fn version_lookups_cover_tagged_pins_only() {
        let lookups = versioned_report().version_lookups();

        assert_eq!(
            lookups.keys().cloned().collect::<Vec<_>>(),
            vec![key("cmake", 'a'), key("cmake", 'b'), key("cmake", 'c')],
            "from, to and the held pin; `pinned` has no tag to look up"
        );
        assert_eq!(
            lookups[&key("cmake", 'a')],
            BTreeMap::from([(LINUX.to_string(), digest_of('a'))])
        );
        assert_eq!(
            lookups[&key("cmake", 'c')],
            BTreeMap::from([(MAC.to_string(), digest_of('c'))])
        );
    }

    /// The same pin reached by two bindings is looked up once.
    #[test]
    fn version_lookups_dedupe_a_shared_pin() {
        let mut ci = tool("cmake", "cmake", &[(LINUX, 'b')]);
        ci.group = "ci".to_string();
        let next = lock('d', vec![tool("cmake", "cmake", &[(LINUX, 'b')]), ci]);
        let declared = PackageRef::new_registry("cmake", "ocx.sh").clone_with_tag("3.28");
        let config = ProjectConfig::from_parts(
            BTreeMap::from([("cmake".to_string(), declared.clone())]),
            BTreeMap::from([("ci".to_string(), BTreeMap::from([("cmake".to_string(), declared)]))]),
        );

        let report = UpdateReport::diff(None, &next, &config, None).expect("canonical platform keys");

        assert_eq!(report.changes.len(), 2, "one row per group");
        assert_eq!(report.version_lookups().len(), 1, "one probe for both rows");
    }

    /// Every row reads its own key; a pin without an answer stays `None`.
    #[test]
    fn fill_versions_sets_each_row_and_leaves_misses_null() {
        let mut report = versioned_report();
        report.fill_versions(&BTreeMap::from([
            (key("cmake", 'a'), "3.28.3".to_string()),
            (key("cmake", 'c'), "3.28.1".to_string()),
        ]));

        let cmake = report
            .changes
            .iter()
            .find(|change| change.name == "cmake")
            .expect("cmake moved");
        assert_eq!(cmake.from_version.as_deref(), Some("3.28.3"));
        assert_eq!(cmake.to_version, None, "no answer for the new pin");
        let pinned = report
            .changes
            .iter()
            .find(|change| change.name == "pinned")
            .expect("pinned moved");
        assert_eq!(
            (pinned.from_version.as_deref(), pinned.to_version.as_deref()),
            (None, None)
        );
        assert_eq!(report.unchanged.len(), 1);
        assert_eq!(report.unchanged[0].version.as_deref(), Some("3.28.1"));
    }

    /// `--verbose`: the version rides in the digest cell, so the table keeps five columns.
    #[test]
    fn pin_cell_appends_a_known_version() {
        let pin = pull("cmake", 'a');
        assert_eq!(
            pin_cell(Some(&pin), Some("3.28.3")),
            format!("{} (3.28.3)", short_digest(&pin))
        );
        assert_eq!(pin_cell(Some(&pin), None), short_digest(&pin));
        assert_eq!(pin_cell(None, Some("3.28.3")), ABSENT);
    }

    /// Default view: no digest; an absent pin is `-`, an unknown release `?`.
    #[test]
    fn version_cell_never_shows_a_digest() {
        let pin = pull("cmake", 'a');
        assert_eq!(version_cell(Some(&pin), Some("3.28.3")), "3.28.3");
        assert_eq!(version_cell(Some(&pin), None), UNKNOWN);
        assert_eq!(version_cell(None, Some("3.28.3")), ABSENT);
    }

    /// The fields are omitted when unknown, never `null`.
    #[test]
    fn version_fields_are_omitted_when_unknown() {
        let json = serde_json::to_value(versioned_report()).expect("serializes");
        assert!(
            !json["changes"][0]
                .as_object()
                .expect("row")
                .contains_key("from_version")
        );
        assert!(!json["changes"][0].as_object().expect("row").contains_key("to_version"));
        assert!(!json["unchanged"][0].as_object().expect("row").contains_key("version"));
    }

    /// Unset optionals are omitted, never `null`, and the platform is the OCI object.
    #[test]
    fn a_new_digest_pinned_pin_omits_tag_and_from() {
        let next = lock('d', vec![tool("ninja", "ninja", &[(LINUX, 'e')])]);
        let report = UpdateReport::diff(None, &next, &config(), None).expect("canonical platform keys");
        let json = serde_json::to_value(&report).expect("the report serializes");
        assert_eq!(
            json["changes"][0],
            serde_json::json!({
                "name": "ninja",
                "group": DEFAULT_GROUP,
                "platform": {"architecture": "amd64", "os": "linux"},
                "to": pull("ninja", 'e').to_string(),
            })
        );
    }
}
