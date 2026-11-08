// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The package names a composing ocx hands to the entrypoint launchers it spawns, as replay records
//! of the execution state, with the project's `no-patches` opt-out per package.
//!
//! A package directory is keyed by digest and shared by every repository that resolves to
//! it, so only the composing process knows which name a launch used.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use ocx_config::exec_state::{IdentityRecord, Owner};
use ocx_oci::{Digest, PackageRef};

use crate::install_info::InstallInfo;

/// Digest-keyed `registry/repository[:tag]` names; a value never carries a digest, so an entry
/// cannot point one package's bytes at another's name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchIdentities(BTreeMap<Digest, Launch>);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Launch {
    names: BTreeSet<PackageRef>,
    /// The composing project opted this package out of the patch tier.
    no_patches: bool,
}

impl LaunchIdentities {
    /// The names of every package in a composed closure: each root and its dependencies,
    /// tag kept, digest stripped.
    pub fn from_infos(infos: &[Arc<InstallInfo>]) -> Self {
        let mut map: BTreeMap<Digest, Launch> = BTreeMap::new();
        let closure = infos.iter().flat_map(|info| {
            std::iter::once(info.identifier()).chain(info.resolved().dependencies.iter().map(|dep| &dep.identifier))
        });
        for identifier in closure {
            map.entry(identifier.digest())
                .or_default()
                .names
                .insert(identifier.without_digest());
        }
        Self(map)
    }

    /// Marks every package a name of which is in `no_patches` (canonical `registry/repository`).
    #[must_use]
    pub fn with_opt_out(mut self, no_patches: &BTreeSet<String>) -> Self {
        for launch in self.0.values_mut() {
            launch.no_patches |= launch
                .names
                .iter()
                .any(|name| no_patches.contains(&repository_key(name)));
        }
        self
    }

    /// The names forwarded for `digest`, sorted; empty when none were.
    pub fn identities_for(&self, digest: &Digest) -> Vec<PackageRef> {
        self.0
            .get(digest)
            .map(|launch| launch.names.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Rebuilds the identities from received replay records; a record whose digest or name does
    /// not parse, or whose name carries a digest, is dropped with a debug log. A record's
    /// `no_patches` mark is carried over.
    pub fn from_replay(records: &[IdentityRecord]) -> Self {
        let mut map: BTreeMap<Digest, Launch> = BTreeMap::new();
        for record in records {
            let Ok(digest) = Digest::try_from(record.digest.as_str()) else {
                log::debug!("replay identity dropped: '{}' is not a digest", record.digest);
                continue;
            };
            let launch = map.entry(digest).or_default();
            launch.no_patches |= record.no_patches;
            for name in &record.names {
                match PackageRef::parse(name) {
                    Ok(identity) if identity.digest().is_none() => {
                        launch.names.insert(identity);
                    }
                    _ => log::debug!("replay identity name '{name}' dropped"),
                }
            }
        }
        Self(map)
    }

    /// The replay records for these identities, one per digest in digest order, all owned by `owner`.
    pub fn to_records(&self, owner: Owner) -> Vec<IdentityRecord> {
        self.0
            .iter()
            .map(|(digest, launch)| IdentityRecord {
                digest: digest.to_string(),
                names: launch.names.iter().map(ToString::to_string).collect(),
                owner: owner.clone(),
                no_patches: launch.no_patches,
            })
            .collect()
    }
}

/// The `no-patches` key form: canonical `registry/repository`, tag and digest dropped.
fn repository_key(name: &PackageRef) -> String {
    format!("{}/{}", name.registry(), name.repository())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::Metadata;
    use crate::metadata::bundle::{Bundle, Version};
    use crate::metadata::visibility::Visibility;
    use crate::resolved_package::{ResolvedDependency, ResolvedPackage};
    use ocx_oci::PinnedPackageRef;

    const DIGEST_A: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const DIGEST_B: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn digest(text: &str) -> Digest {
        Digest::try_from(text).unwrap()
    }

    fn pinned(name: &str, digest_text: &str) -> PinnedPackageRef {
        PinnedPackageRef::pin(&PackageRef::parse(name).unwrap(), digest(digest_text))
    }

    fn info(root: PinnedPackageRef, deps: Vec<PinnedPackageRef>) -> Arc<InstallInfo> {
        let mut resolved = ResolvedPackage::new();
        resolved.dependencies = deps
            .into_iter()
            .map(|identifier| ResolvedDependency {
                identifier,
                visibility: Visibility::default(),
            })
            .collect();
        Arc::new(InstallInfo::new(
            root,
            Metadata::Bundle(Bundle {
                binaries: None,
                version: Version::V1,
                strip_components: None,
                env: Default::default(),
                dependencies: Default::default(),
                entrypoints: Default::default(),
                integrations: Default::default(),
            }),
            resolved,
            ocx_store::file_structure::PackageDir::with_root("/nonexistent".into()),
        ))
    }

    fn names(identities: &LaunchIdentities, digest_text: &str) -> Vec<String> {
        identities
            .identities_for(&digest(digest_text))
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn from_infos_keeps_the_tag_strips_the_digest_and_includes_dependencies() {
        let identities = LaunchIdentities::from_infos(&[info(
            pinned("ocx.sh/plantuml:1.2024", DIGEST_A),
            vec![pinned("ocx.sh/jre:21", DIGEST_B)],
        )]);
        assert_eq!(names(&identities, DIGEST_A), ["ocx.sh/plantuml:1.2024"]);
        assert_eq!(names(&identities, DIGEST_B), ["ocx.sh/jre:21"]);
    }

    #[test]
    fn from_infos_unions_two_names_for_one_digest_in_sorted_order_regardless_of_input_order() {
        let forward = LaunchIdentities::from_infos(&[
            info(pinned("ocx.sh/zeta:1", DIGEST_A), vec![]),
            info(pinned("ocx.sh/alpha:1", DIGEST_A), vec![]),
        ]);
        let backward = LaunchIdentities::from_infos(&[
            info(pinned("ocx.sh/alpha:1", DIGEST_A), vec![]),
            info(pinned("ocx.sh/zeta:1", DIGEST_A), vec![]),
        ]);
        assert_eq!(forward, backward);
        assert_eq!(names(&forward, DIGEST_A), ["ocx.sh/alpha:1", "ocx.sh/zeta:1"]);
    }

    #[test]
    fn an_unknown_digest_has_no_names() {
        let identities = LaunchIdentities::from_infos(&[info(pinned("ocx.sh/plantuml:1", DIGEST_A), vec![])]);
        assert!(identities.identities_for(&digest(DIGEST_B)).is_empty());
    }

    #[test]
    fn an_opt_out_marks_the_package_under_every_name() {
        let identities = LaunchIdentities::from_infos(&[
            info(
                pinned("ocx.sh/plantuml:1", DIGEST_A),
                vec![pinned("ocx.sh/jre:21", DIGEST_B)],
            ),
            info(pinned("ocx.sh/plantuml-alias:2", DIGEST_A), vec![]),
        ])
        .with_opt_out(&BTreeSet::from(["ocx.sh/plantuml".to_owned()]));
        let marked: Vec<(String, bool)> = identities
            .to_records(Owner::Inherited)
            .into_iter()
            .map(|record| (record.digest, record.no_patches))
            .collect();
        assert_eq!(marked, [(DIGEST_A.to_owned(), true), (DIGEST_B.to_owned(), false)]);
    }

    #[test]
    fn replay_records_round_trip() {
        let infos = [info(
            pinned("ocx.sh/root:1", DIGEST_A),
            vec![pinned("ocx.sh/dep:2", DIGEST_B)],
        )];
        let identities = LaunchIdentities::from_infos(&infos);
        let records = identities.to_records(Owner::Inherited);
        assert_eq!(records.len(), 2);
        assert!(records.iter().all(|record| record.owner == Owner::Inherited));
        assert_eq!(LaunchIdentities::from_replay(&records), identities);
    }

    #[test]
    fn replay_records_round_trip_keeps_opt_out() {
        let infos = [info(
            pinned("ocx.sh/root:1", DIGEST_A),
            vec![pinned("ocx.sh/dep:2", DIGEST_B)],
        )];
        let opted_out: BTreeSet<String> = ["ocx.sh/root".to_owned()].into_iter().collect();
        let identities = LaunchIdentities::from_infos(&infos).with_opt_out(&opted_out);
        let records = identities.to_records(Owner::Inherited);
        let marked: Vec<bool> = records.iter().map(|record| record.no_patches).collect();
        assert_eq!(marked.iter().filter(|mark| **mark).count(), 1);
        assert_eq!(LaunchIdentities::from_replay(&records), identities);
    }

    #[test]
    fn from_replay_drops_unparseable_records() {
        let records = [
            IdentityRecord {
                digest: "not-a-digest".to_owned(),
                names: vec!["ocx.sh/a:1".to_owned()],
                owner: Owner::Inherited,
                no_patches: false,
            },
            IdentityRecord {
                digest: DIGEST_A.to_owned(),
                names: vec!["no-registry:1".to_owned()],
                owner: Owner::Inherited,
                no_patches: false,
            },
        ];
        assert!(
            LaunchIdentities::from_replay(&records)
                .identities_for(&digest(DIGEST_A))
                .is_empty()
        );
    }
}
