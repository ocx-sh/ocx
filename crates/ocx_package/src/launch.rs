// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The package names a composing ocx forwards to the entrypoint launchers it spawns, as
//! [`keys::OCX_LAUNCH_IDENTITIES`], with the project's `no-patches` opt-out per package.
//!
//! A package directory is keyed by digest and shared by every repository that resolves to
//! it, so only the composing process knows which name a launch used.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use ocx_config::env::keys;
use ocx_oci::digest::error::DigestError;
use ocx_oci::{Digest, IdentifierError, PackageRef};
use ocx_util::env::var;

use crate::install_info::InstallInfo;
use crate::metadata::env::entry::Entry;
use crate::metadata::env::modifier::ModifierKind;

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

/// One digest's value in the wire form.
#[derive(serde::Serialize, serde::Deserialize)]
struct WireLaunch {
    names: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    no_patches: bool,
}

/// Failure modes of decoding [`keys::OCX_LAUNCH_IDENTITIES`]; each rejects the whole map.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LaunchIdentityError {
    /// The value was present but not a JSON object of `{"names": [...]}` objects.
    #[error("malformed OCX_LAUNCH_IDENTITIES env value")]
    MalformedJson {
        #[source]
        source: serde_json::Error,
    },

    /// A key is not a content digest.
    #[error("OCX_LAUNCH_IDENTITIES key '{key}' is not a content digest")]
    InvalidDigest {
        key: String,
        #[source]
        source: DigestError,
    },

    /// A value is not a package reference with an explicit registry.
    #[error("OCX_LAUNCH_IDENTITIES name '{name}' is not a package reference")]
    InvalidIdentity {
        name: String,
        #[source]
        source: IdentifierError,
    },

    /// A value carries a digest; the launcher attaches its own.
    #[error("OCX_LAUNCH_IDENTITIES name '{name}' carries a digest")]
    DigestPinned { name: String },
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

    /// The `no-patches` keys of every marked package, for the launcher's opt-out set.
    pub fn opted_out_repositories(&self) -> BTreeSet<String> {
        self.0
            .values()
            .filter(|launch| launch.no_patches)
            .flat_map(|launch| launch.names.iter().map(repository_key))
            .collect()
    }

    /// Serializes to the [`keys::OCX_LAUNCH_IDENTITIES`] wire form; `None` when empty.
    pub fn encode(&self) -> Option<String> {
        if self.0.is_empty() {
            return None;
        }
        let wire: BTreeMap<String, WireLaunch> = self
            .0
            .iter()
            .map(|(digest, launch)| {
                let names = launch.names.iter().map(ToString::to_string).collect();
                (
                    digest.to_string(),
                    WireLaunch {
                        names,
                        no_patches: launch.no_patches,
                    },
                )
            })
            .collect();
        match serde_json::to_string(&wire) {
            Ok(json) => Some(json),
            Err(error) => {
                log::warn!("failed to encode OCX_LAUNCH_IDENTITIES: {error}");
                None
            }
        }
    }

    /// Parses [`keys::OCX_LAUNCH_IDENTITIES`]; absent or empty yields `None`.
    ///
    /// # Errors
    ///
    /// [`LaunchIdentityError`] for a malformed value: a corrupt map must not degrade to
    /// "no identity" silently.
    pub fn from_env() -> Result<Option<Self>, LaunchIdentityError> {
        match var(keys::OCX_LAUNCH_IDENTITIES) {
            Some(raw) if !raw.is_empty() => Self::decode(&raw).map(Some),
            _ => Ok(None),
        }
    }

    /// Parses the [`keys::OCX_LAUNCH_IDENTITIES`] wire form.
    ///
    /// # Errors
    ///
    /// [`LaunchIdentityError`] when `raw` is not a well-formed map.
    pub fn decode(raw: &str) -> Result<Self, LaunchIdentityError> {
        let wire = serde_json::from_str::<BTreeMap<String, WireLaunch>>(raw)
            .map_err(|source| LaunchIdentityError::MalformedJson { source })?;
        let mut map = BTreeMap::new();
        for (key, WireLaunch { names, no_patches }) in wire {
            let digest = Digest::try_from(key.as_str()).map_err(|source| LaunchIdentityError::InvalidDigest {
                key: key.clone(),
                source,
            })?;
            let mut parsed = BTreeSet::new();
            for name in names {
                let identity = PackageRef::parse(&name).map_err(|source| LaunchIdentityError::InvalidIdentity {
                    name: name.clone(),
                    source,
                })?;
                if identity.digest().is_some() {
                    return Err(LaunchIdentityError::DigestPinned { name });
                }
                parsed.insert(identity);
            }
            map.insert(
                digest,
                Launch {
                    names: parsed,
                    no_patches,
                },
            );
        }
        Ok(Self(map))
    }

    /// Folds an inherited wire value in: a digest this map holds keeps its own entry, so the
    /// innermost export decides a package's names and opt-out.
    ///
    /// A malformed `raw` is dropped with a debug log: this map replaces it either way.
    pub fn inherit(&mut self, raw: &str) {
        match Self::decode(raw) {
            Ok(inherited) => {
                for (digest, launch) in inherited.0 {
                    self.0.entry(digest).or_insert(launch);
                }
            }
            Err(error) => log::debug!("inherited launch identities dropped: {error}"),
        }
    }

    /// The constant [`Entry`] an exported environment carries; `None` when empty.
    pub fn to_entry(&self) -> Option<Entry> {
        Some(Entry {
            key: keys::OCX_LAUNCH_IDENTITIES.to_owned(),
            value: self.encode()?,
            kind: ModifierKind::Constant,
            separator: None,
        })
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
    fn the_codec_round_trips_through_the_environment() {
        let identities = LaunchIdentities::from_infos(&[
            info(
                pinned("ocx.sh/plantuml:1.2024", DIGEST_A),
                vec![pinned("example.com/jre", DIGEST_B)],
            ),
            info(pinned("ocx.sh/plantuml-alias:2", DIGEST_A), vec![]),
        ]);
        let encoded = identities.encode().expect("a non-empty map encodes");
        assert_eq!(
            encoded,
            format!(
                r#"{{"{DIGEST_A}":{{"names":["ocx.sh/plantuml:1.2024","ocx.sh/plantuml-alias:2"]}},"{DIGEST_B}":{{"names":["example.com/jre"]}}}}"#
            )
        );

        let guard = ocx_util::env::overrides::lock();
        guard.set(keys::OCX_LAUNCH_IDENTITIES, &encoded);
        assert_eq!(LaunchIdentities::from_env().unwrap(), Some(identities));
    }

    #[test]
    fn an_empty_map_encodes_to_nothing() {
        assert_eq!(LaunchIdentities::default().encode(), None);
        assert!(LaunchIdentities::default().to_entry().is_none());
    }

    #[test]
    fn an_inherited_map_fills_only_the_digests_this_one_lacks() {
        let inherited = LaunchIdentities::from_infos(&[
            info(pinned("ocx.sh/plantuml:1", DIGEST_A), vec![]),
            info(pinned("ocx.sh/jre:21", DIGEST_B), vec![]),
        ])
        .with_opt_out(&BTreeSet::from(["ocx.sh/jre".to_owned()]));
        let mut own = LaunchIdentities::from_infos(&[info(pinned("ocx.sh/jre-alias:21", DIGEST_B), vec![])]);
        own.inherit(&inherited.encode().unwrap());

        assert_eq!(names(&own, DIGEST_A), ["ocx.sh/plantuml:1"]);
        assert_eq!(names(&own, DIGEST_B), ["ocx.sh/jre-alias:21"], "the own names win");
        assert!(own.opted_out_repositories().is_empty(), "and so does the own opt-out");

        let entry = own.to_entry().expect("a non-empty map is an entry");
        assert_eq!(entry.key, keys::OCX_LAUNCH_IDENTITIES);
        assert_eq!(entry.kind, ModifierKind::Constant);
        assert_eq!(LaunchIdentities::decode(&entry.value).unwrap(), own);

        let before = own.clone();
        own.inherit("not json");
        assert_eq!(own, before, "a malformed inherited value changes nothing");
    }

    #[test]
    fn an_opt_out_marks_the_package_under_every_name_and_round_trips() {
        let identities = LaunchIdentities::from_infos(&[
            info(
                pinned("ocx.sh/plantuml:1", DIGEST_A),
                vec![pinned("ocx.sh/jre:21", DIGEST_B)],
            ),
            info(pinned("ocx.sh/plantuml-alias:2", DIGEST_A), vec![]),
        ])
        .with_opt_out(&BTreeSet::from(["ocx.sh/plantuml".to_owned()]));
        assert_eq!(
            identities.opted_out_repositories(),
            BTreeSet::from(["ocx.sh/plantuml".to_owned(), "ocx.sh/plantuml-alias".to_owned()])
        );

        let encoded = identities.encode().unwrap();
        assert!(
            encoded.contains(r#"["ocx.sh/plantuml:1","ocx.sh/plantuml-alias:2"],"no_patches":true"#),
            "{encoded}"
        );
        assert!(encoded.contains(r#"{"names":["ocx.sh/jre:21"]}"#), "{encoded}");
        assert_eq!(LaunchIdentities::decode(&encoded).unwrap(), identities);
    }

    #[test]
    fn an_absent_or_empty_variable_is_no_identity() {
        let guard = ocx_util::env::overrides::lock();
        guard.remove(keys::OCX_LAUNCH_IDENTITIES);
        assert_eq!(LaunchIdentities::from_env().unwrap(), None);
        guard.set(keys::OCX_LAUNCH_IDENTITIES, "");
        assert_eq!(LaunchIdentities::from_env().unwrap(), None);
    }

    fn kind(error: &LaunchIdentityError) -> &'static str {
        match error {
            LaunchIdentityError::MalformedJson { .. } => "json",
            LaunchIdentityError::InvalidDigest { .. } => "digest",
            LaunchIdentityError::InvalidIdentity { .. } => "identity",
            LaunchIdentityError::DigestPinned { .. } => "pinned",
        }
    }

    #[test]
    fn a_malformed_variable_is_a_typed_error() {
        let guard = ocx_util::env::overrides::lock();
        let cases = [
            ("not json {{{", "json"),
            (r#"{"sha256:aa":"ocx.sh/a"}"#, "json"),
            (
                r#"{"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa":["ocx.sh/a"]}"#,
                "json",
            ),
            (r#"{"not-a-digest":{"names":["ocx.sh/a"]}}"#, "digest"),
            (
                r#"{"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa":{"names":["no-registry:1"]}}"#,
                "identity",
            ),
            (
                r#"{"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa":{"names":["ocx.sh/a@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"]}}"#,
                "pinned",
            ),
        ];
        for (raw, expected) in cases {
            guard.set(keys::OCX_LAUNCH_IDENTITIES, raw);
            let error = LaunchIdentities::from_env().expect_err(raw);
            assert_eq!(kind(&error), expected, "{raw} gave {error:?}");
        }
    }
}
