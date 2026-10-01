// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The package names a composing ocx forwards to the entrypoint launchers it spawns, as
//! [`keys::OCX_LAUNCH_IDENTITIES`].
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

/// Digest-keyed `registry/repository[:tag]` names; a value never carries a digest, so an entry
/// cannot point one package's bytes at another's name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchIdentities(BTreeMap<Digest, BTreeSet<PackageRef>>);

/// Failure modes of decoding [`keys::OCX_LAUNCH_IDENTITIES`]; each rejects the whole map.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LaunchIdentityError {
    /// The value was present but not a JSON object of string arrays.
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
        let mut map: BTreeMap<Digest, BTreeSet<PackageRef>> = BTreeMap::new();
        let closure = infos.iter().flat_map(|info| {
            std::iter::once(info.identifier()).chain(info.resolved().dependencies.iter().map(|dep| &dep.identifier))
        });
        for identifier in closure {
            map.entry(identifier.digest())
                .or_default()
                .insert(identifier.without_digest());
        }
        Self(map)
    }

    /// The names forwarded for `digest`, sorted; empty when none were.
    pub fn identities_for(&self, digest: &Digest) -> Vec<PackageRef> {
        self.0
            .get(digest)
            .map(|names| names.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Serializes to the [`keys::OCX_LAUNCH_IDENTITIES`] wire form; `None` when empty.
    pub fn encode(&self) -> Option<String> {
        if self.0.is_empty() {
            return None;
        }
        let wire: BTreeMap<String, Vec<String>> = self
            .0
            .iter()
            .map(|(digest, names)| (digest.to_string(), names.iter().map(ToString::to_string).collect()))
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
        let Some(raw) = var(keys::OCX_LAUNCH_IDENTITIES) else {
            return Ok(None);
        };
        if raw.is_empty() {
            return Ok(None);
        }
        let wire = serde_json::from_str::<BTreeMap<String, Vec<String>>>(&raw)
            .map_err(|source| LaunchIdentityError::MalformedJson { source })?;
        let mut map = BTreeMap::new();
        for (key, names) in wire {
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
            map.insert(digest, parsed);
        }
        Ok(Some(Self(map)))
    }
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
                r#"{{"{DIGEST_A}":["ocx.sh/plantuml:1.2024","ocx.sh/plantuml-alias:2"],"{DIGEST_B}":["example.com/jre"]}}"#
            )
        );

        let guard = ocx_util::env::overrides::lock();
        guard.set(keys::OCX_LAUNCH_IDENTITIES, &encoded);
        assert_eq!(LaunchIdentities::from_env().unwrap(), Some(identities));
    }

    #[test]
    fn an_empty_map_encodes_to_nothing() {
        assert_eq!(LaunchIdentities::default().encode(), None);
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
            (r#"{"not-a-digest":["ocx.sh/a"]}"#, "digest"),
            (
                r#"{"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa":["no-registry:1"]}"#,
                "identity",
            ),
            (
                r#"{"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa":["ocx.sh/a@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"]}"#,
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
