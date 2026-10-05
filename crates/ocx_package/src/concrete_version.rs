// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The concrete version an advisory tag (`3`, `3.28`, `latest`) currently names, found by
//! cascade algebra plus a bounded leaf-digest probe.

use std::collections::BTreeMap;

use ocx_index::{Index, IndexOperation};
use ocx_oci::{Digest, PackageRef};

use crate::cascade::decompose_targets;
use crate::error::Error;
use crate::version::Version;

type Result<T> = std::result::Result<T, Error>;

/// The patch-precision release tags that `advisory` could currently name, newest first.
///
/// A version advisory (`3`, `debug-3.28`) keeps the tags whose cascade chain contains it;
/// `latest` keeps the default variant's `latest`-eligible tags, and a bare variant name
/// (`debug`) that variant's. A full version keeps itself. Prerelease and build-suffixed tags
/// never qualify.
pub fn candidate_versions(advisory: &str, tags: &[String]) -> Vec<Version> {
    let named = Version::parse(advisory);
    // Floating `latest` tags: `latest` for the default variant, the bare name for a variant.
    let latest_variant = match advisory {
        "latest" => Some(None),
        _ if named.is_none() => Some(Some(advisory)),
        _ => None,
    };
    let mut candidates: Vec<Version> = tags
        .iter()
        .filter_map(|tag| Version::parse(tag))
        // Without the patch filter `3.28` sorts above `3.28.4` and the rolling minor wins.
        .filter(|version| version.has_patch() && !version.has_prerelease() && !version.has_build())
        .filter(|version| match (&named, latest_variant) {
            (Some(named), _) => version == named || decompose_targets(version).targets.contains(named),
            (None, Some(variant)) => version.variant() == variant && decompose_targets(version).latest_eligible,
            (None, None) => false,
        })
        .collect();
    candidates.sort_by(|left, right| right.cmp(left));
    candidates.dedup();
    candidates
}

/// The newest of [`candidate_versions`] whose leaf digest equals `leaves[platform]` on every
/// platform in `leaves`; `None` when nothing matches within `max_probes` probes or `leaves`
/// is empty.
///
/// Reads only, through a read-only view with [`IndexOperation::Query`], so it never writes the
/// local index. `leaves` is keyed by the canonical platform string an `ocx.lock` uses.
///
/// # Errors
///
/// An index failure while listing tags or fetching a candidate's manifest.
pub async fn resolve_concrete_version(
    index: &Index,
    advisory: &PackageRef,
    leaves: &BTreeMap<String, Digest>,
    max_probes: usize,
) -> Result<Option<Version>> {
    // An empty map would match the first candidate on no evidence at all.
    if leaves.is_empty() {
        return Ok(None);
    }
    let index = index.read_only_view();
    let Some(tags) = index.list_tags(advisory).await? else {
        return Ok(None);
    };
    for candidate in candidate_versions(advisory.tag_or_latest(), &tags)
        .into_iter()
        .take(max_probes)
    {
        let identifier = advisory.clone_with_tag(candidate.to_string());
        let Some(manifests) = index.fetch_candidates(&identifier, IndexOperation::Query).await? else {
            continue;
        };
        let matches_every_leaf = leaves.iter().all(|(platform, leaf)| {
            manifests
                .iter()
                .any(|(manifest, offered)| offered.to_string() == *platform && manifest.digest().as_ref() == Some(leaf))
        });
        if matches_every_leaf {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use ocx_index::IndexImpl;
    use ocx_oci::Manifest;

    use super::*;

    fn tags(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    fn versions(values: &[&str]) -> Vec<Version> {
        values
            .iter()
            .map(|value| Version::parse(value).expect("version parses"))
            .collect()
    }

    // ── candidate_versions ────────────────────────────────────────────

    #[test]
    fn major_advisory_keeps_patch_tags_only_newest_first() {
        let tags = tags(&["3", "3.28", "3.28.4", "3.28.3"]);
        assert_eq!(candidate_versions("3", &tags), versions(&["3.28.4", "3.28.3"]));
    }

    #[test]
    fn minor_advisory_excludes_other_minors_and_majors() {
        let tags = tags(&["3.28.4", "3.29.0", "3.28.1", "4.28.9"]);
        assert_eq!(candidate_versions("3.28", &tags), versions(&["3.28.4", "3.28.1"]));
    }

    #[test]
    fn prerelease_build_and_non_version_tags_never_qualify() {
        let tags = tags(&["3.28.5-rc1", "3.28.4_b1", "3.28.4-rc1_b1", "nightly", "3.28.2"]);
        assert_eq!(candidate_versions("3", &tags), versions(&["3.28.2"]));
    }

    #[test]
    fn variant_advisory_keeps_only_its_variant() {
        let tags = tags(&["3.28.9", "debug-3.28.5", "debug-3.28.4", "debug-3", "pgo-3.28.6"]);
        assert_eq!(
            candidate_versions("debug-3", &tags),
            versions(&["debug-3.28.5", "debug-3.28.4"])
        );
    }

    #[test]
    fn latest_keeps_default_variant_releases() {
        let tags = tags(&["latest", "3.28.4", "4.0.0", "4.1.0-rc1", "4.0.1_b1", "debug-5.0.0"]);
        assert_eq!(candidate_versions("latest", &tags), versions(&["4.0.0", "3.28.4"]));
    }

    #[test]
    fn bare_variant_name_keeps_that_variants_releases() {
        let tags = tags(&["debug", "debug-1.2.3", "debug-1.3.0", "1.4.0"]);
        assert_eq!(
            candidate_versions("debug", &tags),
            versions(&["debug-1.3.0", "debug-1.2.3"])
        );
    }

    #[test]
    fn full_version_advisory_keeps_itself() {
        let tags = tags(&["3.28.4", "3.28.5", "3.28"]);
        assert_eq!(candidate_versions("3.28.4", &tags), versions(&["3.28.4"]));
    }

    // ── resolve_concrete_version ──────────────────────────────────────

    const LINUX_AMD64: &str = r#"{"os":"linux","architecture":"amd64"}"#;
    const LINUX_ARM64: &str = r#"{"os":"linux","architecture":"arm64"}"#;

    fn digest(ch: char) -> Digest {
        Digest::Sha256(ch.to_string().repeat(64))
    }

    fn image_index(children: &[(char, &str)]) -> Manifest {
        let manifests = children
            .iter()
            .map(|(child, platform)| {
                format!(
                    r#"{{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"{}","size":1,"platform":{platform}}}"#,
                    digest(*child)
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        serde_json::from_str(&format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{manifests}]}}"#
        ))
        .expect("image index parses")
    }

    /// One recorded `fetch_manifest` call: tag, operation, and whether it came through a
    /// read-only view.
    type Probe = (String, IndexOperation, bool);

    #[derive(Clone)]
    struct FakeSource {
        tags: Option<Vec<String>>,
        manifests: BTreeMap<String, Manifest>,
        read_only: bool,
        probes: Arc<Mutex<Vec<Probe>>>,
    }

    impl FakeSource {
        fn new(tags: &[&str], manifests: Vec<(&str, Manifest)>) -> Self {
            Self {
                tags: Some(self::tags(tags)),
                manifests: manifests
                    .into_iter()
                    .map(|(tag, manifest)| (tag.to_string(), manifest))
                    .collect(),
                read_only: false,
                probes: Arc::default(),
            }
        }

        fn probes(&self) -> Vec<Probe> {
            self.probes.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl IndexImpl for FakeSource {
        async fn list_repositories(&self, _: &str) -> ocx_index::error::Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &PackageRef) -> ocx_index::error::Result<Option<Vec<String>>> {
            Ok(self.tags.clone())
        }
        async fn fetch_manifest(
            &self,
            identifier: &PackageRef,
            op: IndexOperation,
        ) -> ocx_index::error::Result<Option<(Digest, Manifest)>> {
            let tag = identifier.tag_or_latest().to_string();
            self.probes.lock().unwrap().push((tag.clone(), op, self.read_only));
            Ok(self.manifests.get(&tag).map(|manifest| (digest('f'), manifest.clone())))
        }
        async fn fetch_manifest_digest(
            &self,
            _: &PackageRef,
            _: IndexOperation,
        ) -> ocx_index::error::Result<Option<Digest>> {
            Ok(None)
        }
        async fn fetch_blob(&self, _: &ocx_oci::PinnedPackageRef) -> ocx_index::error::Result<Option<Vec<u8>>> {
            Ok(None)
        }
        fn box_clone(&self) -> Box<dyn IndexImpl> {
            Box::new(self.clone())
        }
        fn read_only_view(&self) -> Box<dyn IndexImpl> {
            Box::new(Self {
                read_only: true,
                ..self.clone()
            })
        }
    }

    fn advisory(tag: &str) -> PackageRef {
        PackageRef::parse(&format!("example.com/cmake:{tag}")).expect("identifier parses")
    }

    fn leaves(entries: &[(&str, char)]) -> BTreeMap<String, Digest> {
        entries
            .iter()
            .map(|(platform, ch)| (platform.to_string(), digest(*ch)))
            .collect()
    }

    fn two_releases() -> FakeSource {
        FakeSource::new(
            &["3", "3.28", "3.28.4", "3.28.3"],
            vec![
                ("3.28.4", image_index(&[('b', LINUX_AMD64), ('c', LINUX_ARM64)])),
                ("3.28.3", image_index(&[('a', LINUX_AMD64), ('c', LINUX_ARM64)])),
            ],
        )
    }

    #[tokio::test]
    async fn returns_newest_candidate_whose_leaf_matches_and_probes_read_only_query() {
        let source = two_releases();
        let index = Index::from_impl(source.clone());

        let found = resolve_concrete_version(&index, &advisory("3"), &leaves(&[("linux/amd64", 'a')]), 8)
            .await
            .unwrap();

        assert_eq!(found, Version::parse("3.28.3"));
        assert_eq!(
            source.probes(),
            vec![
                ("3.28.4".to_string(), IndexOperation::Query, true),
                ("3.28.3".to_string(), IndexOperation::Query, true),
            ]
        );
    }

    #[tokio::test]
    async fn every_platform_in_leaves_must_match() {
        let index = Index::from_impl(two_releases());
        let resolve = |entries: &'static [(&'static str, char)]| {
            let index = &index;
            async move {
                resolve_concrete_version(index, &advisory("3"), &leaves(entries), 8)
                    .await
                    .unwrap()
            }
        };

        assert_eq!(resolve(&[("linux/arm64", 'c')]).await, Version::parse("3.28.4"));
        assert_eq!(
            resolve(&[("linux/amd64", 'a'), ("linux/arm64", 'c')]).await,
            Version::parse("3.28.3")
        );
        assert_eq!(resolve(&[("linux/amd64", 'b'), ("linux/arm64", 'd')]).await, None);
    }

    #[tokio::test]
    async fn stops_after_max_probes() {
        let source = two_releases();
        let index = Index::from_impl(source.clone());

        let found = resolve_concrete_version(&index, &advisory("3"), &leaves(&[("linux/amd64", 'a')]), 1)
            .await
            .unwrap();

        assert_eq!(found, None);
        assert_eq!(source.probes().len(), 1);
    }

    #[tokio::test]
    async fn full_version_advisory_returns_itself_when_leaves_match() {
        let index = Index::from_impl(two_releases());

        let found = resolve_concrete_version(&index, &advisory("3.28.4"), &leaves(&[("linux/amd64", 'b')]), 8)
            .await
            .unwrap();

        assert_eq!(found, Version::parse("3.28.4"));
    }

    #[tokio::test]
    async fn empty_leaves_and_unknown_package_resolve_to_none() {
        let source = two_releases();
        let index = Index::from_impl(source.clone());
        let found = resolve_concrete_version(&index, &advisory("3"), &BTreeMap::new(), 8)
            .await
            .unwrap();
        assert_eq!(found, None);
        assert!(source.probes().is_empty());

        let unknown = Index::from_impl(FakeSource {
            tags: None,
            ..two_releases()
        });
        let found = resolve_concrete_version(&unknown, &advisory("3"), &leaves(&[("linux/amd64", 'a')]), 8)
            .await
            .unwrap();
        assert_eq!(found, None);
    }
}
