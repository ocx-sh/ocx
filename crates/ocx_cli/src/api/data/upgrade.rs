// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The `ocx upgrade` report: which declared tags moved, which stayed and why, and the lock diff.

use ocx_console::{Cell, DataInterface};
use ocx_package::upgrade_target::SkipReason;
use serde::Serialize;

use crate::api::Printable;
use crate::api::data::update::UpdateReport;

/// One binding whose declared tag moved in `ocx.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct TagUpgrade {
    /// Local binding name (the `ocx.toml` key).
    pub name: String,
    /// Owning group — `default` for the top-level `[tools]` table.
    pub group: String,
    /// The tag declared before the upgrade.
    pub from_tag: String,
    /// The tag declared after the upgrade.
    pub to_tag: String,
}

/// One binding `ocx upgrade` left alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct SkippedBinding {
    /// Local binding name (the `ocx.toml` key).
    pub name: String,
    /// Owning group — `default` for the top-level `[tools]` table.
    pub group: String,
    /// The declared tag; `null` when the binding spells none (a bare name or a digest).
    pub tag: Option<String>,
    /// Why the tag did not move.
    pub reason: SkipReason,
}

/// A newer release outside the current major, which only `--major` moves to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct BeyondMajor {
    /// Local binding name (the `ocx.toml` key).
    pub name: String,
    /// Owning group — `default` for the top-level `[tools]` table.
    pub group: String,
    /// The tag declared after this run.
    pub tag: String,
    /// The newest tag at the same precision in a higher major.
    pub newest_tag: String,
}

/// Report emitted by `ocx upgrade` (and by `ocx upgrade --check` before it exits 65).
///
/// Rows are ordered by `(group, name)`. `lock` is the `ocx update` report of the re-lock the
/// retagged bindings caused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
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

impl UpgradeReport {
    /// Whether any declared tag moved, which `--check` turns into exit 65.
    pub fn upgraded(&self) -> bool {
        !self.upgrades.is_empty()
    }
}

impl UpgradeReport {
    /// The shared plain rendering; `verbose` appends the skipped rows instead of a hint.
    fn render(&self, printer: &DataInterface, verbose: bool) {
        let mut rows: [Vec<Cell>; 4] = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        for upgrade in &self.upgrades {
            rows[0].push(Cell::from(upgrade.name.clone()));
            rows[1].push(Cell::from(upgrade.group.clone()));
            rows[2].push(Cell::from(upgrade.from_tag.clone()));
            rows[3].push(Cell::from(upgrade.to_tag.clone()));
        }
        if verbose {
            for skipped in &self.skipped {
                let tag = skipped.tag.clone().unwrap_or_else(|| "-".to_owned());
                rows[0].push(Cell::from(skipped.name.clone()));
                rows[1].push(Cell::from(skipped.group.clone()));
                rows[2].push(Cell::from(tag));
                rows[3].push(Cell::from(format!("skipped: {}", skipped.reason)));
            }
        }
        printer.print_table(&["Binding".into(), "Group".into(), "From".into(), "To".into()], &rows);
        if !verbose && !self.skipped.is_empty() {
            let count = self.skipped.len();
            let noun = if count == 1 { "binding" } else { "bindings" };
            printer.print_hint(&format!(
                "{count} skipped {noun} not shown; re-run with --verbose to list them with the reason"
            ));
        }
        for beyond in &self.beyond_major {
            printer.print_hint(&format!(
                "{}:{} ({}): {} is available; re-run with --major to move to it",
                beyond.name, beyond.tag, beyond.group, beyond.newest_tag
            ));
        }
    }
}

impl Printable for UpgradeReport {
    fn print_plain(&self, printer: &DataInterface) {
        self.render(printer, false);
    }
}

/// [`UpgradeReport`] rendered with the skipped bindings — `ocx upgrade --verbose`.
///
/// JSON delegates to the inner report: the wire shape is identical with or without `--verbose`.
pub struct VerboseUpgradeReport(pub UpgradeReport);

impl Printable for VerboseUpgradeReport {
    fn print_plain(&self, printer: &DataInterface) {
        self.0.render(printer, true);
    }
}

impl Serialize for VerboseUpgradeReport {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl schemars::JsonSchema for VerboseUpgradeReport {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "VerboseUpgradeReport".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        <UpgradeReport>::json_schema(generator)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `--verbose` changes the plain rendering only; the JSON payload is the same.
    #[test]
    fn verbose_wrapper_serializes_as_the_report() {
        let report = UpgradeReport {
            upgrades: vec![TagUpgrade {
                name: "cmake".into(),
                group: "default".into(),
                from_tag: "3.28".into(),
                to_tag: "3.29".into(),
            }],
            skipped: vec![SkippedBinding {
                name: "ninja".into(),
                group: "default".into(),
                tag: None,
                reason: SkipReason::Latest,
            }],
            beyond_major: Vec::new(),
            lock: UpdateReport {
                changes: Vec::new(),
                unchanged: Vec::new(),
                metadata_changed: false,
            },
        };
        let plain = serde_json::to_value(&report).expect("the report serializes");
        let verbose = serde_json::to_value(VerboseUpgradeReport(report)).expect("the wrapper serializes");
        assert_eq!(plain, verbose);
        assert_eq!(plain["skipped"][0]["reason"], "latest");
        assert_eq!(plain["skipped"][0]["tag"], serde_json::Value::Null);
    }
}
