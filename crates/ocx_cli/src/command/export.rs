// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use ocx_package_manager::{ExportRequest, LockPin};
use ocx_project::LockedTool;

use crate::api::data::export::ExportReport;
use crate::app::project_context::{ensure_group_segments_nonempty, load_project_with_lock};
use crate::{conventions, options};

/// Writes every package the project lock pins to a directory.
#[derive(Parser, Clone)]
pub struct Export {
    /// Directory to write the export to; created when absent.
    #[arg(short = 'o', long = "output", value_name = "DIR", required = true)]
    output: PathBuf,

    /// Export only the named group(s); repeatable and comma-separated. Defaults to every group.
    #[clap(flatten)]
    groups: options::GroupSelection,

    /// Export only this platform; repeat for several. Defaults to every platform the lock records.
    #[arg(short = 'p', long = "platform", value_name = "PLATFORM")]
    platforms: Vec<ocx_oci::Platform>,

    #[clap(flatten)]
    referrers: options::ExportReferrers,
}

impl Export {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        ensure_group_segments_nonempty(self.groups.names())?;
        let project = load_project_with_lock(&context).await?;
        // Unlike `pull`, no `-g` is every group: the export is for a machine that cannot ask for more later.
        let groups = if self.groups.names().is_empty() {
            ocx_project::expand_all_keyword(&["all".to_owned()], &project.config)
        } else {
            self.groups.effective(&project.config)?
        };
        let lock_pins = lock_pins(&project.lock.tools, &groups, &self.platforms)?;
        let outcome = context
            .manager()
            .export(ExportRequest {
                output: conventions::absolute_dir(&self.output)?,
                packages: Vec::new(),
                lock_pins,
                platforms: self.platforms.clone(),
                referrers: self.referrers.enabled(),
            })
            .await?;
        context.api().report(&ExportReport::from_outcome(outcome)?)?;
        Ok(ExitCode::SUCCESS)
    }
}

/// The platform leaves of `tools` in `groups`, one pin each: every leaf the lock records without `platforms`,
/// else the best leaf for each named platform. A leaf two groups share is pinned once.
///
/// # Errors
///
/// [`ocx_project::ProjectErrorKind::NoHostLeaf`] (exit 78) when a selected tool has no leaf for a named platform;
/// a usage error (exit 64) when nothing is left to pin.
pub(super) fn lock_pins(
    tools: &[LockedTool],
    groups: &[String],
    platforms: &[ocx_oci::Platform],
) -> anyhow::Result<Vec<LockPin>> {
    let mut pins: Vec<LockPin> = Vec::new();
    let mut push = |pin: LockPin| {
        if !pins.contains(&pin) {
            pins.push(pin);
        }
    };
    for tool in tools.iter().filter(|tool| groups.contains(&tool.group)) {
        if platforms.is_empty() {
            for (key, digest) in &tool.platforms {
                if let Ok(platform) = key.parse() {
                    push(LockPin {
                        identifier: tool.repository.pin_untagged(digest.clone()),
                        platform,
                    });
                }
            }
            continue;
        }
        for wanted in platforms {
            let digest = tool.host_leaf(wanted)?;
            let platform = tool
                .platforms
                .iter()
                .filter(|(_, candidate)| **candidate == digest)
                .find_map(|(key, _)| key.parse().ok())
                .unwrap_or_else(|| wanted.clone());
            push(LockPin {
                identifier: tool.repository.pin_untagged(digest),
                platform,
            });
        }
    }
    if pins.is_empty() {
        return Err(crate::error::UsageError::new(
            "the project lock pins nothing for the selected groups and platforms",
        )
        .into());
    }
    Ok(pins)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ocx_oci::{Digest, PackageRef, Platform};

    use super::*;

    fn digest(fill: char) -> Digest {
        Digest::Sha256(fill.to_string().repeat(64))
    }

    fn tool(name: &str, group: &str, leaves: &[(&str, char)]) -> LockedTool {
        LockedTool {
            name: name.to_owned(),
            group: group.to_owned(),
            repository: (&PackageRef::new_registry(format!("team/{name}"), "registry.test")).into(),
            declared: None,
            platforms: leaves
                .iter()
                .map(|(platform, fill)| ((*platform).to_owned(), digest(*fill)))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    fn groups(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn lock_pins_cover_selected_groups_and_platforms() {
        let tools = [
            tool("cmake", "default", &[("linux/amd64", 'a'), ("linux/arm64", 'b')]),
            tool("lint", "ci", &[("linux/amd64", 'c'), ("linux/arm64", 'd')]),
            tool("docs", "docs", &[("linux/amd64", 'e')]),
        ];

        let every = lock_pins(&tools, &groups(&["default", "ci", "docs"]), &[]).expect("pins");
        assert_eq!(every.len(), 5, "every group, every platform");

        let ci_only = lock_pins(&tools, &groups(&["ci"]), &[]).expect("pins");
        assert_eq!(ci_only.len(), 2, "only the selected group");
        assert!(ci_only.iter().all(|pin| pin.identifier.repository() == "team/lint"));

        let arm: Platform = "linux/arm64".parse().expect("platform");
        let narrowed = lock_pins(&tools, &groups(&["default", "ci"]), std::slice::from_ref(&arm)).expect("pins");
        assert_eq!(narrowed.len(), 2, "one leaf per tool for the named platform");
        assert!(narrowed.iter().all(|pin| pin.platform == arm));
        assert_eq!(narrowed[0].identifier.digest(), digest('b'));
    }

    #[test]
    fn lock_pins_share_one_leaf_across_groups() {
        let shared = [
            tool("cmake", "default", &[("linux/amd64", 'a')]),
            tool("cmake", "ci", &[("linux/amd64", 'a')]),
        ];
        let pins = lock_pins(&shared, &groups(&["default", "ci"]), &[]).expect("pins");
        assert_eq!(pins.len(), 1, "a leaf two groups share is exported once");
    }

    #[test]
    fn lock_pins_refuse_an_empty_selection() {
        let tools = [tool("cmake", "default", &[("linux/amd64", 'a')])];
        let err = lock_pins(&tools, &groups(&["ci"]), &[]).expect_err("no tool is in the selected group");
        assert!(err.downcast_ref::<crate::error::UsageError>().is_some(), "{err}");
    }
}
