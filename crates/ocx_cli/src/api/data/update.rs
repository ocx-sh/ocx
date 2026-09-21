// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The `ocx update` report (C-011) — **what moved**, not what the lock now is.
//!
//! `ocx lock` and `ocx add` answer "what is pinned"; only `update` can answer
//! "what changed", because only `update` holds both the predecessor lock and
//! the candidate at the same moment (`MutationGuard::previous_lock`).
//!
//! # The compared value is the pull identifier, not the digest
//!
//! A `LockedTool` pins `repository` (bare) plus one leaf digest per shipped
//! platform, and the thing actually pulled is
//! `repository.clone_with_digest(leaf)`. Comparing bare digests would report
//! "unchanged" for a binding whose *repository* moved to a mirror holding the
//! same bytes — a different pull, reported as no pull at all. So the diff
//! compares the reconstructed pull identifier, which is exactly what
//! `locked_tool_content_equal` weighs (`repository` **and** the platform map)
//! projected onto one row per platform.

use std::collections::{BTreeMap, BTreeSet};

use ocx_console::{Cell, DataInterface};
use ocx_oci::Identifier;
use ocx_project::{DEFAULT_GROUP, LockedTool, ProjectConfig, ProjectLock};
use serde::Serialize;

use crate::api::Printable;

/// The cell a `None` `from`/`to` renders as — a platform that did not exist on
/// one side of the diff.
const ABSENT: &str = "-";

/// One `(group, binding, platform)` pin that moved between the predecessor
/// lock and the candidate.
///
/// `from` is `None` when the pin is newly introduced (a binding or a platform
/// the predecessor did not carry); `to` is `None` when it was dropped. Both
/// are the full pull identifier — `registry/repository@sha256:<hex>` — so a
/// consumer can feed either straight back to `ocx pull` without rebuilding it
/// from parts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct BindingChange {
    /// Local binding name (the `ocx.toml` key).
    pub name: String,
    /// Owning group — `default` for the top-level `[tools]` table.
    pub group: String,
    /// Canonical platform key the pin belongs to.
    pub platform: String,
    /// The tag the declaration spells, `null` when it is digest-pinned.
    pub tag: Option<String>,
    /// Pull identifier before the update; `null` when newly pinned. Held
    /// typed so `Serialize` emits the full pinned form while the plain table
    /// renders the shared short-digest abbreviation.
    pub from: Option<Identifier>,
    /// Pull identifier after the update; `null` when dropped.
    pub to: Option<Identifier>,
}

/// One `(group, binding, platform)` pin the update left where it was.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct BindingState {
    /// Local binding name (the `ocx.toml` key).
    pub name: String,
    /// Owning group — `default` for the top-level `[tools]` table.
    pub group: String,
    /// Canonical platform key the pin belongs to.
    pub platform: String,
    /// The tag the declaration spells, `null` when it is digest-pinned.
    pub tag: Option<String>,
    /// The unchanged pull identifier — the same
    /// `registry/repository@sha256:<hex>` form `BindingChange::from` / `to`
    /// carry, so the two arrays are directly comparable.
    pub digest: Identifier,
}

/// Report emitted by `ocx update` (and by `ocx update --check` before it
/// exits 65).
///
/// Plain format: one five-column table (Binding | Group | Platform | From |
/// To) holding the `changes` rows, where `Binding` is `name` or `name:tag`
/// and the digest columns carry the CLI-wide short-digest abbreviation
/// [`ocx_oci::Digest::to_short_string`] (`-` when the pin did not exist on
/// that side). When `unchanged` is non-empty a trailing hint names the count
/// and `--verbose`.
///
/// JSON format: `{ "changes": [...], "unchanged": [...], "metadata_changed":
/// bool }`. Every `BindingChange` carries `{ name, group, platform, tag,
/// from, to }` and every `BindingState` `{ name, group, platform, tag,
/// digest }`, with `tag`/`from`/`to` present and `null` rather than absent.
/// **The payload is identical with and without `--verbose`** — see
/// [`VerboseUpdateReport`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct UpdateReport {
    /// Pins whose pull identifier differs between the two locks, ordered by
    /// `(group, name, platform)`.
    pub changes: Vec<BindingChange>,
    /// Bindings this run examined and found unchanged, in the same order.
    ///
    /// A scoped run (`-g` / positional names) examines only its own
    /// selection; every other pin is carried forward verbatim and was never
    /// re-resolved, so listing it as "unchanged" would claim a check that did
    /// not happen. A whole-lock run examines everything, so nothing is
    /// filtered out.
    pub unchanged: Vec<BindingState>,
    /// Whether the load-bearing lock metadata moved — `declaration_hash`,
    /// `declaration_hash_version` or `lock_version`. Advisory metadata
    /// (`generated_at`, `generated_by`) is deliberately ignored: it moves on
    /// every write and would make every `--check` fail.
    pub metadata_changed: bool,
}

/// The compared value of one pin: its reconstructed pull identifier.
fn pull_identifier(tool: &LockedTool, leaf: &ocx_oci::Digest) -> Identifier {
    tool.repository.clone_with_digest(leaf.clone())
}

/// Flatten a lock into `(group, name, platform) -> pull identifier`.
///
/// `BTreeMap` for the ordering the report and the table inherit: a lock's
/// `tools` vector is sorted by `(group, name)` at write time but an in-memory
/// candidate need not be.
fn pins(lock: &ProjectLock) -> BTreeMap<(String, String, String), Identifier> {
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

/// The tag the declaration spells for `(group, name)`, or `None` when the
/// binding is digest-pinned or no longer declared (a dropped binding still
/// gets a row, and its `ocx.toml` entry is gone).
fn declared_tag(config: &ProjectConfig, group: &str, name: &str) -> Option<String> {
    let table = if group == DEFAULT_GROUP {
        &config.tools
    } else {
        &config.groups.get(group)?.tools
    };
    table.get(name)?.tag().map(str::to_owned)
}

/// The digest column for one pull identifier — the same `sha256:<12hex>`
/// abbreviation `ocx package inspect`, `ocx package cascade check` and
/// `ocx package sbom` render, so one short digest means one thing across the
/// CLI.
///
/// The fallback cannot be reached through [`UpdateReport::diff`], which builds
/// every value with `clone_with_digest`; it is the honest answer rather than a
/// dash if some later caller hands over a tagless, digestless coordinate.
fn short_digest(pull: &Identifier) -> String {
    pull.digest()
        .map_or_else(|| pull.to_string(), |digest| digest.to_short_string())
}

/// `name` or `name:tag` — the binding as the user spelled it in `ocx.toml`.
///
/// Keeps the declared tag inside the five-column budget instead of buying a
/// sixth column for it.
fn binding_label(name: &str, tag: Option<&str>) -> String {
    match tag {
        Some(tag) => format!("{name}:{tag}"),
        None => name.to_owned(),
    }
}

impl UpdateReport {
    /// Diff `previous` against `next`, keyed by `(group, name, platform)`.
    ///
    /// `previous` is an [`Option`] because bare `ocx update` in a project that
    /// has no `ocx.lock` yet is legal (it resolves the whole file, the way
    /// `ocx lock` does) — there every pin is newly introduced, which is what
    /// `from: None` on every row says. The scoped and `--check` paths gate on
    /// a predecessor themselves and always pass `Some`.
    ///
    /// `examined` is the `(group, name)` selection a scoped run re-resolved,
    /// and `None` for a whole-lock run. It narrows [`Self::unchanged`] only:
    /// a pin outside the scope was carried forward verbatim rather than
    /// checked, so reporting it as unchanged would claim a check that never
    /// ran. `changes` is never filtered — a scoped run cannot legitimately
    /// move an untouched pin, and if one moves anyway the user must see it.
    pub fn diff(
        previous: Option<&ProjectLock>,
        next: &ProjectLock,
        config: &ProjectConfig,
        examined: Option<&[(String, String)]>,
    ) -> Self {
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
            let tag = declared_tag(config, group, name);
            let from = before.get(key);
            let to = after.get(key);
            match (from, to) {
                (Some(from), Some(to)) if from == to => {
                    if in_scope(group, name) {
                        unchanged.push(BindingState {
                            name: name.clone(),
                            group: group.clone(),
                            platform: platform.clone(),
                            tag,
                            digest: to.clone(),
                        });
                    }
                }
                _ => changes.push(BindingChange {
                    name: name.clone(),
                    group: group.clone(),
                    platform: platform.clone(),
                    tag,
                    from: from.cloned(),
                    to: to.cloned(),
                }),
            }
        }

        Self {
            changes,
            unchanged,
            metadata_changed: previous.is_none_or(|prev| {
                next.metadata.declaration_hash != prev.metadata.declaration_hash
                    || next.metadata.declaration_hash_version != prev.metadata.declaration_hash_version
                    || next.metadata.lock_version != prev.metadata.lock_version
            }),
        }
    }

    /// Whether this update would move anything on disk — the `--check`
    /// verdict, and the one place that question is answered.
    pub fn moved(&self) -> bool {
        !self.changes.is_empty() || self.metadata_changed
    }

    /// The shared plain rendering; `verbose` appends the `unchanged` rows
    /// instead of the hint that names them.
    fn render(&self, printer: &DataInterface, verbose: bool) {
        let mut rows: [Vec<Cell>; 5] = [Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        let mut push = |binding: String, group: &str, platform: &str, from: String, to: String| {
            rows[0].push(Cell::from(binding));
            rows[1].push(Cell::from(group.to_owned()));
            rows[2].push(Cell::from(platform.to_owned()));
            rows[3].push(Cell::from(from));
            rows[4].push(Cell::from(to));
        };
        for change in &self.changes {
            push(
                binding_label(&change.name, change.tag.as_deref()),
                &change.group,
                &change.platform,
                change.from.as_ref().map_or(ABSENT.to_owned(), short_digest),
                change.to.as_ref().map_or(ABSENT.to_owned(), short_digest),
            );
        }
        if verbose {
            for state in &self.unchanged {
                let pinned = short_digest(&state.digest);
                push(
                    binding_label(&state.name, state.tag.as_deref()),
                    &state.group,
                    &state.platform,
                    pinned.clone(),
                    pinned,
                );
            }
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
        if !verbose && !self.unchanged.is_empty() {
            printer.print_hint(&self.unchanged_hint());
        }
    }

    /// The trailing line naming how many pins held still and the flag that
    /// lists them.
    fn unchanged_hint(&self) -> String {
        let count = self.unchanged.len();
        let (noun, pronoun) = if count == 1 { ("pin", "it") } else { ("pins", "them") };
        format!("{count} unchanged {noun} not shown; re-run with --verbose to list {pronoun}")
    }
}

impl Printable for UpdateReport {
    fn print_plain(&self, printer: &DataInterface) {
        self.render(printer, false);
    }
}

/// [`UpdateReport`] rendered with the pins that held still — `ocx update
/// --verbose`.
///
/// Plain format: the same table, with one `From == To` row appended per
/// `unchanged` entry and no hint.
///
/// JSON format: delegates to the inner [`UpdateReport`] — **identical wire
/// shape whether verbose or not**, the contract `VerboseVersionData` and
/// `VerboseShellState` already keep. `--verbose` is a human flag; a
/// `--format json` consumer never sees less for its absence.
pub struct VerboseUpdateReport(pub UpdateReport);

impl Printable for VerboseUpdateReport {
    fn print_plain(&self, printer: &DataInterface) {
        self.0.render(printer, true);
    }
}

impl Serialize for VerboseUpdateReport {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

// The `Serialize` impl above is transparent, so the published schema is the
// inner type's. Verbosity changes the plain rendering only.
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

    use ocx_oci::{Digest, Identifier};
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
            repository: Identifier::new_registry(repo, "ocx.sh"),
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
        let declared = Identifier::new_registry("cmake", "ocx.sh").clone_with_tag("3.28");
        ProjectConfig::from_parts(BTreeMap::from([("cmake".to_string(), declared)]), BTreeMap::new())
    }

    /// The pull identifier `tool()` produces for `repo` at the leaf filled
    /// with `byte` — built the way the report builds it, from the parts.
    fn pull(repo: &str, byte: char) -> Identifier {
        Identifier::new_registry(repo, "ocx.sh").clone_with_digest(digest_of(byte))
    }

    const LINUX: &str = "linux/amd64";
    const MAC: &str = "darwin/arm64";

    /// The digest byte moved, the repository did not: one change row carrying
    /// both pull identifiers.
    #[test]
    fn digest_move_is_one_change() {
        let previous = lock('d', vec![tool("cmake", "cmake", &[(LINUX, 'a')])]);
        let next = lock('d', vec![tool("cmake", "cmake", &[(LINUX, 'b')])]);

        let report = UpdateReport::diff(Some(&previous), &next, &config(), None);

        assert!(report.unchanged.is_empty(), "the only pin moved");
        assert!(!report.metadata_changed, "the declaration did not change");
        assert_eq!(report.changes.len(), 1);
        let change = &report.changes[0];
        assert_eq!(change.name, "cmake");
        assert_eq!(change.group, DEFAULT_GROUP);
        assert_eq!(change.platform, LINUX);
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

        let report = UpdateReport::diff(Some(&previous), &next, &config(), None);

        assert_eq!(report.changes.len(), 1);
        assert_eq!(report.changes[0].platform, MAC);
        assert_eq!(
            report.changes[0].from, None,
            "a newly shipped platform has no predecessor"
        );
        assert!(report.changes[0].to.is_some());
        assert_eq!(report.unchanged.len(), 1);
        assert_eq!(report.unchanged[0].platform, LINUX);
    }

    /// The publisher stopped shipping a platform: `to` is `None`.
    #[test]
    fn dropped_platform_has_no_to() {
        let previous = lock('d', vec![tool("cmake", "cmake", &[(LINUX, 'a'), (MAC, 'c')])]);
        let next = lock('d', vec![tool("cmake", "cmake", &[(LINUX, 'a')])]);

        let report = UpdateReport::diff(Some(&previous), &next, &config(), None);

        assert_eq!(report.changes.len(), 1);
        assert_eq!(report.changes[0].platform, MAC);
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

        let report = UpdateReport::diff(Some(&previous), &next, &config(), None);

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

        let report = UpdateReport::diff(Some(&previous), &next, &config(), None);

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

        let report = UpdateReport::diff(Some(&previous), &next, &config(), None);

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

        let report = UpdateReport::diff(Some(&previous), &next, &config(), None);

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

        let report = UpdateReport::diff(Some(&previous), &next, &config(), None);

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

        assert!(!UpdateReport::diff(Some(&previous), &next, &config(), None).moved());
    }

    /// No predecessor lock (bare `ocx update` before any `ocx lock`): every
    /// pin is newly introduced.
    #[test]
    fn absent_predecessor_reports_every_pin_as_new() {
        let next = lock('d', vec![tool("cmake", "cmake", &[(LINUX, 'a'), (MAC, 'c')])]);

        let report = UpdateReport::diff(None, &next, &config(), None);

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
        let report = UpdateReport::diff(Some(&previous), &next, &config(), None);

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

        let scoped = UpdateReport::diff(Some(&previous), &next, &config(), Some(&scope));

        assert!(scoped.changes.is_empty(), "nothing moved either way");
        assert_eq!(
            scoped.unchanged.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["cmake"],
            "ninja was carried forward verbatim, never re-resolved"
        );
        assert!(!scoped.moved(), "an untouched scope still exits 0");

        let whole = UpdateReport::diff(Some(&previous), &next, &config(), None);
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

    /// The hint names the count and the flag, and agrees in number.
    #[test]
    fn unchanged_hint_names_the_count_and_the_flag() {
        let previous = lock('d', vec![tool("cmake", "cmake", &[(LINUX, 'a'), (MAC, 'c')])]);
        let next = lock('d', vec![tool("cmake", "cmake", &[(LINUX, 'a'), (MAC, 'c')])]);
        let two = UpdateReport::diff(Some(&previous), &next, &config(), None);
        let hint = two.unchanged_hint();
        assert!(hint.contains('2') && hint.contains("--verbose"), "{hint}");
        assert!(hint.contains("pins") && hint.contains("them"), "{hint}");

        let one = UpdateReport {
            changes: Vec::new(),
            unchanged: two.unchanged[..1].to_vec(),
            metadata_changed: false,
        };
        let hint = one.unchanged_hint();
        assert!(hint.contains("1 unchanged pin ") && hint.contains(" it"), "{hint}");
    }
}
