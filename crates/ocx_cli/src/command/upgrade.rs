// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use futures::StreamExt;
use ocx_package::upgrade_target::{SkipReason, newest_beyond_major, tracked_version, upgrade_target};
use ocx_package::version::Version;
use ocx_project::{
    DEFAULT_GROUP, ProjectConfig, ProjectLock, ResolveLockOptions, resolve_lock_touched, retag_binding_in_memory,
};

use crate::api::data::update::UpdateReport;
use crate::api::data::upgrade::{BeyondMajor, SkippedBinding, TagUpgrade, UpgradeReport, VerboseUpgradeReport};
use crate::app::project_context::{load_project_for_mutate, materialize_lock, record_activation_consent};
use crate::command::index_common::policy_blocked;
use crate::command::update::{concrete_versions, missing_lock, print_verdict, select_touched};
use crate::conventions;

/// Arguments for `ocx upgrade`; its help text lives on `Command::Upgrade`.
#[derive(Parser, Clone)]
pub struct Upgrade {
    /// Report the tags that would move and exit without writing.
    ///
    /// Exits 0 when every selected binding is up to date and 65 (`DataError`)
    /// when at least one would move. `ocx.toml` and `ocx.lock` are left
    /// untouched either way.
    #[arg(long = "check", default_value_t = false)]
    pub check: bool,

    /// Allow a tag to move to a newer major version.
    ///
    /// Without it a binding stays within its current major (`cmake:3.28` moves
    /// to `3.29`, never to `4.0`), and a newer major is listed for information
    /// only.
    #[arg(long = "major", default_value_t = false)]
    pub major: bool,

    /// List the bindings that were left alone as well as the ones that moved.
    ///
    /// Affects the plain rendering only. The structured report
    /// (`ocx --format json upgrade`) carries every field either way.
    #[arg(short, long)]
    pub verbose: bool,

    /// Upgrade every binding in the named group(s); leave the rest.
    ///
    /// Repeatable and comma-separated: `-g ci,lint -g release`. The reserved
    /// name `default` selects the top-level `[tools]` table; `all` expands to
    /// `default` plus every declared `[group.*]`. Combine with binding names to
    /// upgrade only those bindings within the named groups.
    #[arg(short = 'g', long = "group", value_delimiter = ',')]
    pub groups: Vec<String>,

    /// Binding names to upgrade; leave every other binding as declared.
    ///
    /// Each name is the `ocx.toml` binding key and is upgraded in every group
    /// it appears in (narrow with `-g`). An unknown name exits 64.
    #[arg(num_args = 0..)]
    pub names: Vec<String>,
}

impl Upgrade {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let guard = load_project_for_mutate(&context).await?;

        // Every guard runs before the first registry read, so a refusal never depends on the network.
        let Some(previous) = guard.previous_lock().cloned() else {
            return Err(missing_lock(guard.lock_path()).into());
        };
        if context.is_offline() {
            return Err(policy_blocked("ocx upgrade", "offline").into());
        }
        if context.config_view().frozen {
            return Err(policy_blocked("ocx upgrade", "frozen").into());
        }
        let selected = select_touched(guard.config(), &self.groups, &self.names)?;
        previous.bind_current(guard.config())?;

        // Live tag listings, never committed to the local index, as for `ocx update`.
        let index = context.update_index();
        let plans = futures::stream::iter(selected)
            .map(|(group, name)| {
                let binding = declared(guard.config(), &group, &name).cloned();
                let index = &index;
                async move {
                    let Some(binding) = binding else {
                        return Ok::<_, anyhow::Error>(None);
                    };
                    let plan = match tracked_version(&binding) {
                        Err(reason) => Plan::Skip(reason),
                        Ok(current) => {
                            let tags = index.list_tags(&binding).await?.unwrap_or_default();
                            plan_for(&current, &tags, self.major)
                        }
                    };
                    Ok(Some((group, name, binding, plan)))
                }
            })
            .buffered(TAG_LISTING_CONCURRENCY)
            .collect::<Vec<_>>()
            .await;

        let mut upgrades = Vec::new();
        let mut skipped = Vec::new();
        let mut beyond_major = Vec::new();
        for plan in plans {
            let Some((group, name, binding, plan)) = plan? else {
                continue;
            };
            let tag = binding.tag().map(str::to_owned);
            match plan {
                Plan::Skip(reason) => skipped.push(SkippedBinding {
                    name,
                    group,
                    tag,
                    reason,
                }),
                Plan::Track { to, beyond } => {
                    let declared_tag = tag.clone().unwrap_or_default();
                    if let Some(newest_tag) = beyond {
                        beyond_major.push(BeyondMajor {
                            name: name.clone(),
                            group: group.clone(),
                            tag: to.clone().unwrap_or_else(|| declared_tag.clone()),
                            newest_tag,
                        });
                    }
                    match to {
                        Some(to_tag) => upgrades.push(TagUpgrade {
                            name,
                            group,
                            from_tag: declared_tag,
                            to_tag,
                        }),
                        None => skipped.push(SkippedBinding {
                            name,
                            group,
                            tag,
                            reason: SkipReason::UpToDate,
                        }),
                    }
                }
            }
        }
        let by_key = |group: &String, name: &String| (group.clone(), name.clone());
        upgrades.sort_by_key(|row: &TagUpgrade| by_key(&row.group, &row.name));
        skipped.sort_by_key(|row: &SkippedBinding| by_key(&row.group, &row.name));
        beyond_major.sort_by_key(|row: &BeyondMajor| by_key(&row.group, &row.name));

        // Nothing moves: no resolve and no write, so both files stay byte-identical.
        if upgrades.is_empty() {
            let lock = UpdateReport::diff(Some(&previous), &previous, guard.config(), Some(&[]));
            self.emit(
                &context,
                UpgradeReport {
                    upgrades,
                    skipped,
                    beyond_major,
                    lock,
                },
            )?;
            return Ok(ExitCode::SUCCESS);
        }

        let staged = guard.stage(|config| {
            for upgrade in &upgrades {
                retag_binding_in_memory(
                    config,
                    guard.config_path(),
                    &upgrade.name,
                    &upgrade.group,
                    &upgrade.to_tag,
                )?;
            }
            Ok(())
        })?;
        let retagged: Vec<(String, String)> = upgrades
            .iter()
            .map(|row| (row.group.clone(), row.name.clone()))
            .collect();
        let new_lock = resolve_lock_touched(
            staged.config(),
            guard.config(),
            &previous,
            &index,
            &retagged,
            ResolveLockOptions::default(),
        )
        .await?;

        let lock = lock_report(&index, &previous, &new_lock, guard.config(), staged.config(), &retagged).await;
        let report = UpgradeReport {
            upgrades,
            skipped,
            beyond_major,
            lock,
        };

        if self.check {
            let count = report.upgrades.len();
            let tags = if count == 1 { "tag" } else { "tags" };
            self.emit(&context, report)?;
            return Ok(print_verdict(&format!(
                "{count} {tags} would move; run `ocx upgrade` to apply"
            )));
        }

        let scope = context.toolchain_render_scope(guard.config_path()).await?;
        let platform = conventions::platform_or_default(None);
        // Cloned before the commit consumes `staged`; the pull binds the new lock to it.
        let config = staged.config().clone();
        let commit = context
            .manager()
            .commit_and_render(
                guard,
                staged,
                new_lock.clone(),
                ocx_package_manager::ToolchainRender {
                    scope: &scope,
                    toolchain_root: context.toolchain_root(),
                    platform: &platform,
                },
            )
            .await?
            .commit;

        // After the commit, so the stamp records the new source set.
        record_activation_consent(&commit.config_path, &new_lock, None).await;

        // After the commit, so a failure here never rolls back the lock.
        materialize_lock(&context, &new_lock, &config, true, platform).await?;

        self.emit(&context, report)?;
        Ok(ExitCode::SUCCESS)
    }

    /// One report, two renderings: `--verbose` appends the skipped bindings to plain output only.
    fn emit(&self, context: &crate::app::Context, report: UpgradeReport) -> anyhow::Result<()> {
        if self.verbose {
            context.api().report(&VerboseUpgradeReport(report))
        } else {
            context.api().report(&report)
        }
    }
}

/// Most tag listings in flight at once.
const TAG_LISTING_CONCURRENCY: usize = 8;

/// What one binding's tag does: stays for a reason, or tracks a version and may move.
enum Plan {
    Skip(SkipReason),
    /// `to` is the tag it moves to; `beyond` the newest tag past the major it ends on.
    Track {
        to: Option<String>,
        beyond: Option<String>,
    },
}

fn plan_for(current: &Version, tags: &[String], allow_major: bool) -> Plan {
    let target = upgrade_target(current, tags, allow_major);
    let landed = target.as_ref().unwrap_or(current);
    Plan::Track {
        to: target.as_ref().map(Version::to_string),
        beyond: newest_beyond_major(landed, tags).map(|version| version.to_string()),
    }
}

fn declared<'c>(config: &'c ProjectConfig, group: &str, name: &str) -> Option<&'c ocx_oci::PackageRef> {
    let tools = if group == DEFAULT_GROUP {
        &config.tools
    } else {
        &config.groups.get(group)?.tools
    };
    tools.get(name)
}

/// The re-lock's [`UpdateReport`], each side's version found under the tag that named it.
///
/// `diff` reads one declaration for both sides, so it runs once per declaration and the
/// old pin's version comes from the run under the old tag.
// ponytail: each side also probes under the other tag (one miss per pin); split the lookups if that shows up.
async fn lock_report(
    index: &ocx_index::Index,
    previous: &ProjectLock,
    next: &ProjectLock,
    before: &ProjectConfig,
    after: &ProjectConfig,
    retagged: &[(String, String)],
) -> UpdateReport {
    let mut report = UpdateReport::diff(Some(previous), next, after, Some(retagged));
    let mut old = UpdateReport::diff(Some(previous), next, before, Some(retagged));
    let mut lookups = report.version_lookups();
    lookups.extend(old.version_lookups());
    let versions = concrete_versions(index, lookups).await;
    report.fill_versions(&versions);
    old.fill_versions(&versions);
    // Same lock pair, so both diffs list the same changes in the same order.
    for (change, before) in report.changes.iter_mut().zip(old.changes) {
        change.from_version = before.from_version;
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Upgrade {
        Upgrade::try_parse_from(args).unwrap()
    }

    #[test]
    fn flags_and_names_parse() {
        let upgrade = parse(&[
            "upgrade", "--check", "--major", "-v", "-g", "ci,lint", "-g", "release", "cmake", "ninja",
        ]);
        assert!(upgrade.check);
        assert!(upgrade.major);
        assert!(upgrade.verbose);
        assert_eq!(upgrade.groups, ["ci", "lint", "release"]);
        assert_eq!(upgrade.names, ["cmake", "ninja"]);
    }

    fn tags(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    fn track(plan: Plan) -> (Option<String>, Option<String>) {
        match plan {
            Plan::Track { to, beyond } => (to, beyond),
            Plan::Skip(reason) => panic!("expected a tracked plan, got skip: {reason}"),
        }
    }

    fn version(value: &str) -> Version {
        Version::parse(value).expect("version parses")
    }

    /// Within the major the tag moves; the newer major is listed beyond the tag it lands on.
    #[test]
    fn plan_moves_within_major_and_lists_the_next_major() {
        let tags = tags(&["3.28", "3.29", "4.0", "4.1"]);
        assert_eq!(
            track(plan_for(&version("3.28"), &tags, false)),
            (Some("3.29".to_owned()), Some("4.1".to_owned()))
        );
    }

    /// `--major` lands on the newest major, so nothing is left beyond it.
    #[test]
    fn plan_with_major_leaves_nothing_beyond() {
        let tags = tags(&["3", "4", "5"]);
        assert_eq!(
            track(plan_for(&version("3"), &tags, true)),
            (Some("5".to_owned()), None)
        );
        assert_eq!(
            track(plan_for(&version("3"), &tags, false)),
            (None, Some("5".to_owned()))
        );
    }

    #[test]
    fn defaults_stay_within_major_and_write() {
        let upgrade = parse(&["upgrade"]);
        assert!(!upgrade.check);
        assert!(!upgrade.major);
        assert!(!upgrade.verbose);
        assert!(upgrade.groups.is_empty());
        assert!(upgrade.names.is_empty());
    }
}
