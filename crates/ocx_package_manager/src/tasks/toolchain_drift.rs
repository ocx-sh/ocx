// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Whether the advisory tags a toolchain lock pins have moved since `ocx.lock` was written.
//!
//! Compares platform leaf digests, never index digests: the lock pins leaves, and cascade
//! blocking can move one platform's leaf while another's stays.

use std::collections::BTreeMap;
use std::time::Duration;

use tokio::time::Instant;

use futures::StreamExt;
use ocx_index::{Index, IndexOperation};
use ocx_oci::{Digest, PackageRef, Platform};
use ocx_package::concrete_version::resolve_concrete_version;
use ocx_package::version::Version;
use ocx_project::BoundTool;

use super::super::PackageManager;

/// Overall budget for the whole drift check, every toolchain together; it runs inline before
/// the user's command.
pub const DRIFT_DEADLINE: Duration = Duration::from_secs(5);

/// Most bindings probed at once.
const MAX_IN_FLIGHT: usize = 8;

/// Most releases one version lookup probes before it reports no version.
const VERSION_PROBES: usize = 8;

/// One binding whose tag now names different leaves than the lock pins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriftedTool {
    /// The binding name in `ocx.toml`.
    pub name: String,
    /// The declared advisory tag.
    pub tag: String,
    /// The release the tag names now; `None` when no candidate matched the live leaves.
    pub version: Option<Version>,
}

impl PackageManager {
    /// The tagged bindings in `bound` whose live leaves differ from the lock, read through
    /// [`Index::remote_view`] so nothing is written to the local index.
    ///
    /// Never fails: an error or reaching `deadline` logs at debug and yields no drift. The
    /// deadline is an instant so several toolchains share one budget ([`DRIFT_DEADLINE`]).
    /// Digest-pinned bindings are skipped; fetches run at most 8 at a time.
    pub async fn toolchain_drift(&self, bound: &[BoundTool], deadline: Instant) -> Vec<DriftedTool> {
        probe_drift(&self.index().remote_view(), bound, deadline).await
    }
}

/// [`PackageManager::toolchain_drift`] over an explicit index.
async fn probe_drift(index: &Index, bound: &[BoundTool], deadline: Instant) -> Vec<DriftedTool> {
    let tagged = bound.iter().filter_map(|tool| {
        let tag = tool.advisory_tag()?;
        tool.declared().digest().is_none().then_some((tool, tag))
    });
    let probes = futures::stream::iter(tagged)
        .map(|(tool, tag)| probe_one(index, tool, tag))
        .buffered(MAX_IN_FLIGHT)
        .filter_map(std::future::ready)
        .collect::<Vec<_>>();
    // All or nothing: a partial answer would read as "the rest did not move".
    tokio::time::timeout_at(deadline, probes).await.unwrap_or_else(|_| {
        log::debug!("toolchain drift probe abandoned at its deadline");
        Vec::new()
    })
}

/// One binding's drift, or `None` when it did not move or the read failed.
async fn probe_one(index: &Index, tool: &BoundTool, tag: &str) -> Option<DriftedTool> {
    let declared = tool.declared();
    let candidates = match index.fetch_candidates(declared, IndexOperation::Query).await {
        Ok(Some(candidates)) => candidates,
        Ok(None) => {
            log::debug!("toolchain drift: {declared} not found");
            return None;
        }
        Err(error) => {
            log::debug!("toolchain drift: reading {declared} failed: {error}");
            return None;
        }
    };
    let live = drifted_leaves(&tool.locked().platforms, &candidates)?;
    let version = resolve_concrete_version(index, declared, &live, VERSION_PROBES)
        .await
        .inspect_err(|error| log::debug!("toolchain drift: version lookup for {declared} failed: {error}"))
        .ok()
        .flatten();
    Some(DriftedTool {
        name: tool.locked().name.clone(),
        tag: tag.to_string(),
        version,
    })
}

/// The live leaves on the platforms `locked` holds, when they differ from `locked`.
///
/// A locked platform the live index no longer offers is a difference; a platform only the live
/// index offers is not, since the lock never pinned it. Keys use the canonical platform string.
fn drifted_leaves(
    locked: &BTreeMap<String, Digest>,
    candidates: &[(PackageRef, Platform)],
) -> Option<BTreeMap<String, Digest>> {
    let live: BTreeMap<String, Digest> = locked
        .keys()
        .filter_map(|platform| {
            candidates
                .iter()
                .find(|(_, offered)| offered.to_string() == *platform)
                .and_then(|(leaf, _)| leaf.digest())
                .map(|digest| (platform.clone(), digest))
        })
        .collect();
    (live != *locked).then_some(live)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use ocx_index::{IndexImpl, IndexOperation};
    use ocx_oci::Manifest;
    use ocx_project::{Binding, ProjectConfig, ProjectLock};

    use super::*;

    const LINUX_AMD64: &str = r#"{"os":"linux","architecture":"amd64"}"#;
    const LINUX_ARM64: &str = r#"{"os":"linux","architecture":"arm64"}"#;

    fn digest(ch: char) -> Digest {
        Digest::Sha256(ch.to_string().repeat(64))
    }

    fn leaves(entries: &[(&str, char)]) -> BTreeMap<String, Digest> {
        entries
            .iter()
            .map(|(platform, ch)| (platform.to_string(), digest(*ch)))
            .collect()
    }

    fn candidates(entries: &[(&str, char)]) -> Vec<(PackageRef, Platform)> {
        let base = PackageRef::parse("example.com/cmake:3").expect("identifier parses");
        entries
            .iter()
            .map(|(platform, ch)| {
                (
                    base.clone_with_digest(digest(*ch)),
                    platform.parse::<Platform>().expect("platform parses"),
                )
            })
            .collect()
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

    /// `cmake` declared as `declared`, locked to `platforms`, bound as a current lock.
    fn bound(declared: &str, platforms: &[(&str, char)]) -> Vec<BoundTool> {
        let config =
            ProjectConfig::from_toml_str(&format!("[tools]\ncmake = \"{declared}\"\n")).expect("ocx.toml parses");
        let platform_rows = platforms
            .iter()
            .map(|(platform, ch)| format!("\"{platform}\" = \"{}\"\n", digest(*ch)))
            .collect::<String>();
        let lock = ProjectLock::from_toml_str(&format!(
            "[metadata]\nlock_version = 3\ndeclaration_hash_version = 1\ndeclaration_hash = \"{}\"\n\
             generated_by = \"test\"\ngenerated_at = \"2026-01-01T00:00:00Z\"\n\n\
             [[tool]]\nname = \"cmake\"\ngroup = \"default\"\nrepository = \"example.com/cmake\"\n\n\
             [tool.platforms]\n{platform_rows}",
            config.declaration_hash_cached()
        ))
        .expect("ocx.lock parses");
        match lock.bind(&config) {
            Binding::Current(bound) => bound,
            Binding::Stale(drift) => panic!("fixture lock must be current: {drift}"),
        }
    }

    #[derive(Clone, Default)]
    struct FakeSource {
        tags: Vec<String>,
        manifests: BTreeMap<String, Manifest>,
        /// Never answers a manifest fetch, to stand in for a registry that hangs.
        hang: bool,
        fetched: Arc<Mutex<Vec<String>>>,
    }

    impl FakeSource {
        fn new(tags: &[&str], manifests: Vec<(&str, Manifest)>) -> Self {
            Self {
                tags: tags.iter().map(|tag| tag.to_string()).collect(),
                manifests: manifests
                    .into_iter()
                    .map(|(tag, manifest)| (tag.to_string(), manifest))
                    .collect(),
                ..Self::default()
            }
        }

        fn fetched(&self) -> Vec<String> {
            self.fetched.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl IndexImpl for FakeSource {
        async fn list_repositories(&self, _: &str) -> ocx_index::error::Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &PackageRef) -> ocx_index::error::Result<Option<Vec<String>>> {
            Ok(Some(self.tags.clone()))
        }
        async fn fetch_manifest(
            &self,
            identifier: &PackageRef,
            _: IndexOperation,
        ) -> ocx_index::error::Result<Option<(Digest, Manifest)>> {
            if self.hang {
                std::future::pending::<()>().await;
            }
            let tag = identifier.tag_or_latest().to_string();
            self.fetched.lock().unwrap().push(tag.clone());
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
            self.box_clone()
        }
    }

    // ── drifted_leaves ────────────────────────────────────────────────

    #[test]
    fn identical_leaves_on_every_locked_platform_are_not_drift() {
        let locked = leaves(&[("linux/amd64", 'a'), ("linux/arm64", 'b')]);
        let live = candidates(&[("linux/amd64", 'a'), ("linux/arm64", 'b')]);
        assert_eq!(drifted_leaves(&locked, &live), None);
    }

    /// Cascade blocking can move one platform while the other stays: that is still drift.
    #[test]
    fn one_moved_platform_is_drift_and_returns_every_locked_platforms_live_leaf() {
        let locked = leaves(&[("linux/amd64", 'a'), ("linux/arm64", 'b')]);
        let live = candidates(&[("linux/amd64", 'a'), ("linux/arm64", 'c')]);
        assert_eq!(
            drifted_leaves(&locked, &live),
            Some(leaves(&[("linux/amd64", 'a'), ("linux/arm64", 'c')]))
        );
    }

    #[test]
    fn a_platform_only_the_live_index_offers_is_not_drift() {
        let locked = leaves(&[("linux/amd64", 'a')]);
        let live = candidates(&[("linux/amd64", 'a'), ("linux/arm64", 'z')]);
        assert_eq!(drifted_leaves(&locked, &live), None);
    }

    #[test]
    fn a_locked_platform_the_live_index_dropped_is_drift() {
        let locked = leaves(&[("linux/amd64", 'a'), ("linux/arm64", 'b')]);
        let live = candidates(&[("linux/amd64", 'a')]);
        assert_eq!(drifted_leaves(&locked, &live), Some(leaves(&[("linux/amd64", 'a')])));
    }

    // ── probe_drift ───────────────────────────────────────────────────

    #[tokio::test]
    async fn a_moved_tag_reports_the_binding_with_its_new_version() {
        let source = FakeSource::new(
            &["3", "3.28.4", "3.28.3"],
            vec![
                ("3", image_index(&[('b', LINUX_AMD64), ('c', LINUX_ARM64)])),
                ("3.28.4", image_index(&[('b', LINUX_AMD64), ('c', LINUX_ARM64)])),
                ("3.28.3", image_index(&[('a', LINUX_AMD64), ('c', LINUX_ARM64)])),
            ],
        );
        let bound = bound("example.com/cmake:3", &[("linux/amd64", 'a'), ("linux/arm64", 'c')]);

        let drift = probe_drift(&Index::from_impl(source), &bound, Instant::now() + DRIFT_DEADLINE).await;

        assert_eq!(
            drift,
            vec![DriftedTool {
                name: "cmake".to_string(),
                tag: "3".to_string(),
                version: Version::parse("3.28.4"),
            }]
        );
    }

    #[tokio::test]
    async fn an_unmoved_tag_reports_nothing() {
        let source = FakeSource::new(&["3"], vec![("3", image_index(&[('a', LINUX_AMD64)]))]);
        let bound = bound("example.com/cmake:3", &[("linux/amd64", 'a')]);

        let drift = probe_drift(&Index::from_impl(source), &bound, Instant::now() + DRIFT_DEADLINE).await;

        assert_eq!(drift, Vec::new());
    }

    #[tokio::test]
    async fn a_digest_pinned_binding_is_never_probed() {
        let source = FakeSource::new(&["3"], vec![("3", image_index(&[('b', LINUX_AMD64)]))]);
        let bound = bound(&format!("example.com/cmake@{}", digest('a')), &[("linux/amd64", 'a')]);

        let drift = probe_drift(
            &Index::from_impl(source.clone()),
            &bound,
            Instant::now() + DRIFT_DEADLINE,
        )
        .await;

        assert_eq!(drift, Vec::new());
        assert_eq!(source.fetched(), Vec::<String>::new());
    }

    /// Two toolchains probed against a hanging registry share one deadline: the whole check
    /// returns at [`DRIFT_DEADLINE`] with no drift, not at twice it.
    #[tokio::test(start_paused = true)]
    async fn a_hanging_registry_stalls_the_whole_check_for_one_deadline() {
        let source = FakeSource {
            hang: true,
            ..FakeSource::new(&["3"], Vec::new())
        };
        let index = Index::from_impl(source);
        let project = bound("example.com/cmake:3", &[("linux/amd64", 'a')]);
        let global = bound("example.com/cmake:3", &[("linux/amd64", 'b')]);
        let start = Instant::now();
        let deadline = start + DRIFT_DEADLINE;

        let both = async {
            let project = probe_drift(&index, &project, deadline).await;
            let global = probe_drift(&index, &global, deadline).await;
            (project, global)
        };
        let drift = tokio::time::timeout(DRIFT_DEADLINE * 3, both)
            .await
            .expect("the check must return by its deadline");

        assert_eq!(drift, (Vec::new(), Vec::new()));
        assert_eq!(start.elapsed(), DRIFT_DEADLINE);
    }
}
