// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The read half of `ocx package cascade check|repair`.
//!
//! One pass per package: list the registry's tags, classify them, fetch every
//! version-ish one concurrently, and — for a logical package — read the live
//! public index root as well. The result is a [`TagGraphObservation`], the
//! only input the pure core in [`super::graph`] ever sees.
//!
//! Two rules make the observation trustworthy enough to compute verdicts from:
//!
//! - **Any read failure aborts the run.** This is the deliberate inverse of
//!   the push path, which stops a cascade conservatively on a transient error.
//!   A push that skips a level under-delivers; a check that skips one invents
//!   a finding, and a repair acting on it would write the wrong graph.
//! - **Absence is derived, never observed.** Only listed tags are fetched, so
//!   an alias that was never created is simply absent from the map. A listed
//!   tag that 404s mid-run is a race, not an absence, and aborts.
//!
//! Reads authenticate for pull only — `check` never probes push access.
//!
//! Every read here addresses the **canonical** registry
//! ([`ReadAddressing::Canonical`]), not a configured mirror. The audit exists to
//! judge the repository a publisher writes to, and `repair` writes there; a
//! graph read from a mirror would be a verdict about a different, possibly
//! stale, possibly hostile copy.

use std::collections::BTreeMap;

use futures::stream::{self, StreamExt, TryStreamExt};

use super::graph::{AliasTag, ObservedTag, TagGraphObservation};
use crate::Result;
use crate::oci::client::ReadAddressing;
use crate::oci::client::error::ClientError;
use crate::oci::index::{Index, IndexRoot, OcxIndex};
use crate::package::version;
use crate::{log, oci};

/// How many manifest fetches are in flight at once.
///
/// Latency-bound work against a single repository — the same shape, and the
/// same reasoning, as the index's `TAG_REFRESH_CONCURRENCY`: wide enough that a
/// package with a few hundred tags reads in roughly one round trip, narrow
/// enough to stay a polite citizen of a registry shared with everyone else.
const CASCADE_GATHER_CONCURRENCY: usize = 64;

/// Reads one package's whole tag graph.
///
/// `identifier` is the physical repository every registry read addresses;
/// `logical` is the name the user asked for when it differed, carried through
/// so the report can name what was requested. `index` is the source that
/// serves the logical name's root — `Some` turns the index-staleness layer on,
/// `None` (a physical invocation) leaves it off, since no root maps back to a
/// bare repository.
///
/// # Errors
///
/// Any registry or index read failure, and any tag the listing named that the
/// registry then failed to serve.
pub async fn gather(
    client: &oci::Client,
    identifier: &oci::Identifier,
    logical: Option<&oci::Identifier>,
    index: Option<&OcxIndex>,
) -> Result<TagGraphObservation> {
    let listed = client
        .list_tags_addressed(identifier.clone(), ReadAddressing::Canonical)
        .await?;
    let (nodes, ignored_tags) = classify(&listed);
    log::debug!(
        "Cascade gather for '{identifier}': {} graph tags, {} ignored.",
        nodes.len(),
        ignored_tags.len()
    );

    let tags = stream::iter(nodes)
        .map(|(tag, alias)| async move {
            let reference = identifier.clone_with_tag(&tag);
            match client
                .fetch_manifest_raw_bytes_addressed(&reference, ReadAddressing::Canonical)
                .await?
            {
                Some((bytes, digest, manifest)) => Ok((
                    alias,
                    ObservedTag {
                        digest,
                        // The body the digest was computed over, not a
                        // re-serialization: a repair that wraps a bare manifest
                        // in an index entry needs the size the registry serves.
                        // `fetch_manifest_raw_bytes` refuses anything past its
                        // 32 MiB cap, so the conversion cannot fail.
                        size: i64::try_from(bytes.len()).expect("a capped manifest body fits i64"),
                        manifest,
                    },
                )),
                // The listing named this tag moments ago, so its absence now is
                // a concurrent publish or delete — not the "never created" the
                // pure core reads as `Absent`. Recording it as a gap would make
                // a race indistinguishable from real drift, and a repair acting
                // on that would write the wrong graph.
                None => Err(crate::Error::from(ClientError::ManifestNotFound(format!(
                    "{reference}, which the tag listing had just named"
                )))),
            }
        })
        .buffer_unordered(CASCADE_GATHER_CONCURRENCY)
        .try_collect::<BTreeMap<AliasTag, ObservedTag>>()
        .await?;

    let index_root = match index {
        Some(source) => fetch_index_root(source, logical.unwrap_or(identifier)).await?,
        None => None,
    };

    Ok(TagGraphObservation {
        identifier: identifier.clone(),
        logical: logical.cloned(),
        tags,
        ignored_tags,
        index_root,
    })
}

/// Splits a registry's tag listing into the graph's nodes and the tags that are
/// deliberately not part of it.
///
/// A bare variant name is that track's root only once the track exists: with no
/// `debug-*` version published, `debug` is an ordinary tag somebody made, and
/// promoting it would invent an alias for an empty track.
fn classify(listed: &[String]) -> (Vec<(String, AliasTag)>, Vec<String>) {
    let variants = version::variant_names(listed.iter().map(String::as_str));
    let mut nodes = Vec::new();
    let mut ignored = Vec::new();
    for tag in listed {
        // The classification rule itself lives on `AliasTag` — one definition of
        // what a graph node is, shared with every other reader of a tag string.
        match AliasTag::parse(tag, &variants) {
            Some(alias) => nodes.push((tag.clone(), alias)),
            None => ignored.push(tag.clone()),
        }
    }
    ignored.sort();
    (nodes, ignored)
}

/// Reads the live index root that publishes `name`.
///
/// Goes through [`Index::from_source`] rather than calling the source directly
/// because `fetch_root_document` is an `IndexImpl` method and that trait is
/// private to `oci::index`. Wrapping this one source is also the point: a
/// chained index inherits the trait's inert `None` default, so routing the read
/// through the chain would silently switch the whole staleness layer off.
async fn fetch_index_root(source: &OcxIndex, name: &oci::Identifier) -> Result<Option<IndexRoot>> {
    Ok(Index::from_source(source.clone())
        .fetch_root_document(name)
        .await?
        .map(|(_bytes, root)| root))
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use async_trait::async_trait;

    use super::*;
    use crate::oci::client::test_transport::{StubTransport, StubTransportData};
    use crate::oci::index::{IndexFetch, IndexTransport, OcxIndexConfig};

    const REGISTRY: &str = "example.com";
    const REPOSITORY: &str = "test/pkg";
    const INDEX_BASE: &str = "https://index.test";
    const INDEX_NAMESPACE: &str = "ocx.sh";
    const INDEX_REPOSITORY: &str = "kitware/cmake";

    // ── registry-side fixtures ───────────────────────────────────────────────

    fn client(data: &StubTransportData) -> oci::Client {
        oci::Client::with_transport(Box::new(StubTransport::new(data.clone())))
    }

    fn identifier() -> oci::Identifier {
        oci::Identifier::new_registry(REPOSITORY, REGISTRY)
    }

    /// The stub keys its manifest map on the transport reference's string form.
    fn manifest_key(tag: &str) -> String {
        identifier().clone_with_tag(tag).canonical_reference().to_string()
    }

    /// Registers `listed` as the single page of tags `list_tags` answers with.
    fn list(data: &StubTransportData, listed: &[&str]) {
        data.write().tags = vec![listed.iter().map(|tag| (*tag).to_string()).collect()];
    }

    /// Serves `bytes` at `tag` under the digest they actually hash to, which is
    /// what `fetch_manifest_raw_bytes` recomputes and verifies.
    fn seed_raw(data: &StubTransportData, tag: &str, bytes: Vec<u8>) {
        let digest = oci::Algorithm::Sha256.hash(&bytes).to_string();
        data.write().manifests.insert(manifest_key(tag), (bytes, digest));
    }

    fn index_bytes(tag: &str, platforms: &[&str], schema_version: u8) -> Vec<u8> {
        let manifests = platforms
            .iter()
            .map(|platform| {
                let parsed: oci::Platform = platform.parse().expect("test platform must parse");
                oci::ImageIndexEntry {
                    media_type: oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
                    digest: format!("sha256:fake_{tag}_{platform}"),
                    size: 100,
                    platform: Some(parsed.into()),
                    artifact_type: None,
                    annotations: None,
                }
            })
            .collect();
        let index = oci::Manifest::ImageIndex(oci::ImageIndex {
            schema_version,
            media_type: Some(oci::OCI_IMAGE_INDEX_MEDIA_TYPE.to_string()),
            artifact_type: None,
            manifests,
            annotations: None,
        });
        serde_json::to_vec(&index).expect("an image index serializes")
    }

    /// Seeds a well-formed image index at `tag`.
    fn seed_index(data: &StubTransportData, tag: &str, platforms: &[&str]) {
        seed_raw(data, tag, index_bytes(tag, platforms, oci::INDEX_SCHEMA_VERSION));
    }

    /// Seeds a bare image manifest at `tag` — the shape an alias must never
    /// have, and the one the pure core reads as `NotAnIndex`.
    fn seed_image(data: &StubTransportData, tag: &str) {
        let manifest = oci::Manifest::Image(oci::ImageManifest::default());
        seed_raw(
            data,
            tag,
            serde_json::to_vec(&manifest).expect("an image manifest serializes"),
        );
    }

    /// Seeds a two-version, two-alias package: the healthy baseline every
    /// finding-free case starts from.
    fn seed_healthy(data: &StubTransportData) -> Vec<&'static str> {
        let tags = vec!["latest", "3", "3.28", "3.28.1"];
        list(data, &tags);
        for tag in &tags {
            seed_index(data, tag, &["linux/amd64"]);
        }
        tags
    }

    /// The whole error chain rendered.
    ///
    /// `thiserror`'s `Display` stops at the outer message and takes no notice
    /// of `{:#}`, so several of the causes asserted on below — a dead index
    /// endpoint's transport failure most of all — are only reachable through
    /// `source()`.
    fn chain(error: &crate::Error) -> String {
        let mut rendered = error.to_string();
        let mut cause = std::error::Error::source(error);
        while let Some(current) = cause {
            rendered.push_str(&format!(": {current}"));
            cause = current.source();
        }
        rendered
    }

    fn calls(data: &StubTransportData, method: &str) -> usize {
        data.read().calls.iter().filter(|call| *call == method).count()
    }

    /// The observation flattened to comparable, order-revealing pairs.
    fn observed(observation: &TagGraphObservation) -> Vec<(String, String)> {
        observation
            .tags
            .iter()
            .map(|(alias, tag)| (alias.to_string(), tag.digest.to_string()))
            .collect()
    }

    // ── index-side fixtures ──────────────────────────────────────────────────

    /// HTTP boundary stub for the static-file index, mirroring the one in
    /// `oci::index::ocx_index`'s tests: a present entry is a `200`, an absent
    /// one a `404`, a registered failure a transport error.
    #[derive(Clone, Default)]
    struct StubIndexTransport {
        responses: Arc<Mutex<HashMap<String, Vec<u8>>>>,
        requests: Arc<Mutex<Vec<String>>>,
        failures: Arc<Mutex<HashSet<String>>>,
    }

    impl StubIndexTransport {
        fn insert(&self, url: &str, bytes: &[u8]) {
            self.responses
                .lock()
                .expect("stub lock")
                .insert(url.to_string(), bytes.to_vec());
        }

        fn fail(&self, url: &str) {
            self.failures.lock().expect("stub lock").insert(url.to_string());
        }

        fn request_urls(&self) -> Vec<String> {
            self.requests.lock().expect("stub lock").clone()
        }

        fn request_count(&self, url: &str) -> usize {
            self.request_urls().iter().filter(|requested| *requested == url).count()
        }
    }

    #[async_trait]
    impl IndexTransport for StubIndexTransport {
        async fn get(&self, url: &str) -> Result<IndexFetch> {
            self.requests.lock().expect("stub lock").push(url.to_string());
            if self.failures.lock().expect("stub lock").contains(url) {
                return Err(crate::oci::index::error::Error::IndexHttpFailed {
                    url: url.to_string(),
                    status: None,
                    source: "simulated index transport failure".into(),
                }
                .into());
            }
            Ok(match self.responses.lock().expect("stub lock").get(url) {
                Some(bytes) => IndexFetch::Found { bytes: bytes.clone() },
                None => IndexFetch::NotFound,
            })
        }

        fn box_clone(&self) -> Box<dyn IndexTransport> {
            Box::new(self.clone())
        }
    }

    fn config_url() -> String {
        format!("{INDEX_BASE}/config.json")
    }

    fn root_url() -> String {
        format!("{INDEX_BASE}/p/{INDEX_REPOSITORY}.json")
    }

    fn logical_identifier() -> oci::Identifier {
        oci::Identifier::new_registry(INDEX_REPOSITORY, INDEX_NAMESPACE)
    }

    /// A live index source whose root document commits `latest` at `committed`.
    fn seeded_index_source(committed: &oci::Digest) -> (OcxIndex, StubIndexTransport) {
        let transport = StubIndexTransport::default();
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        transport.insert(
            &root_url(),
            format!(
                r#"{{"repository":"oci://{REGISTRY}/{REPOSITORY}","tags":{{"latest":{{"content":"{committed}"}}}}}}"#
            )
            .as_bytes(),
        );
        let source = OcxIndex::new(OcxIndexConfig {
            transport: Box::new(transport.clone()),
            base_url: INDEX_BASE.to_string(),
            namespace: INDEX_NAMESPACE.to_string(),
            // The physical fetch client is never reached: gather reads the root
            // document only, never resolves through it.
            client: client(&StubTransportData::new()),
            allow_yanked: false,
            trusted_hosts: Vec::new(),
        });
        (source, transport)
    }

    /// A digest that is well-formed but names nothing — enough for a root
    /// document, which gather only records.
    fn some_digest() -> oci::Digest {
        oci::Algorithm::Sha256.hash(b"committed index")
    }

    // ── F1-F3: what lands in the map, and what aborts ────────────────────────

    #[tokio::test]
    async fn f1_every_listed_tag_is_observed_with_one_fetch_each() {
        let data = StubTransportData::new();
        let tags = seed_healthy(&data);

        let observation = gather(&client(&data), &identifier(), None, None)
            .await
            .expect("a healthy package gathers");

        assert_eq!(
            observation.tags.len(),
            tags.len(),
            "every listed version-ish tag must be observed"
        );
        assert!(observation.tags.contains_key(&AliasTag::Root { variant: None }));
        assert!(observation.ignored_tags.is_empty(), "nothing here is ignorable");
        assert_eq!(
            calls(&data, "pull_manifest_raw"),
            tags.len(),
            "each tag is fetched exactly once — no re-reads"
        );
        assert_eq!(observation.identifier, identifier());
        assert!(observation.logical.is_none());
        assert!(observation.index_root.is_none());
    }

    #[tokio::test]
    async fn f2_an_unlisted_alias_is_simply_absent_from_the_map() {
        let data = StubTransportData::new();
        // The pnpm shape: `latest` exists at the registry but the listing does
        // not name it. Gather fetches only what was listed, so the alias is
        // absent from the map and the pure core derives `Absent` from that.
        list(&data, &["3.28.1"]);
        seed_index(&data, "3.28.1", &["linux/amd64"]);
        seed_index(&data, "latest", &["linux/amd64"]);

        let observation = gather(&client(&data), &identifier(), None, None)
            .await
            .expect("an unlisted alias is not an error");

        assert!(
            !observation.tags.contains_key(&AliasTag::Root { variant: None }),
            "absence must be derived from the listing, never probed for"
        );
        assert_eq!(observation.tags.len(), 1);
        assert_eq!(calls(&data, "pull_manifest_raw"), 1, "an unlisted tag is never fetched");
    }

    #[tokio::test]
    async fn f3_a_listed_tag_the_registry_will_not_serve_aborts_naming_it() {
        let data = StubTransportData::new();
        list(&data, &["3.28.1", "latest"]);
        seed_index(&data, "3.28.1", &["linux/amd64"]);
        // `latest` is listed and then 404s — a concurrent delete, not an absence.

        let error = gather(&client(&data), &identifier(), None, None)
            .await
            .expect_err("a vanished listed tag is a race, not an absence");

        let rendered = chain(&error);
        assert!(
            rendered.contains("latest"),
            "the abort must name the tag that vanished, got: {rendered}"
        );
    }

    // ── F4-F5: read failures abort, the inverse of the push path ─────────────

    #[tokio::test]
    async fn f4_a_transient_fetch_error_aborts_the_whole_run() {
        let data = StubTransportData::new();
        seed_healthy(&data);
        // One healthy, published version whose fetch fails transiently mid-run.
        // The push path swallows exactly this and skips a cascade level; a read
        // that did the same would manufacture a finding out of a flaky network.
        data.write()
            .manifest_errors
            .insert(manifest_key("3.28"), "503 service unavailable".to_string());

        let error = gather(&client(&data), &identifier(), None, None)
            .await
            .expect_err("a transient registry error must abort, never be swallowed");

        assert!(
            chain(&error).contains("503 service unavailable"),
            "the registry's own failure must reach the caller"
        );
    }

    #[tokio::test]
    async fn f5_an_authentication_failure_aborts_before_any_manifest_fetch() {
        let data = StubTransportData::new();
        seed_healthy(&data);
        data.write().ensure_auth_error_override = Some("401 unauthorized".to_string());

        let error = gather(&client(&data), &identifier(), None, None)
            .await
            .expect_err("an auth failure must abort");

        assert!(chain(&error).contains("401 unauthorized"));
        assert_eq!(
            calls(&data, "pull_manifest_raw"),
            0,
            "the listing fails first, so no manifest is ever requested"
        );
    }

    /// F17: a configured mirror does not serve the audit's reads.
    ///
    /// The stub keys its manifests on the reference it is handed, and only the
    /// canonical references are seeded — so a mirror-aware read looks for
    /// `mirror.invalid/upstream/test/pkg:…` and finds nothing. `repair` writes
    /// to the canonical host; a graph judged from a mirror's copy would be a
    /// verdict about a repository the write never touches (CWE-345/367), and a
    /// stale or hostile mirror could steer a publisher's rewrite of its own
    /// rolling tags.
    #[tokio::test]
    async fn f17_reads_bypass_a_configured_mirror() {
        let data = StubTransportData::new();
        let tags = seed_healthy(&data);
        let mirrored = client(&data).with_test_mirror(REGISTRY, "mirror.invalid", "upstream");

        let observation = gather(&mirrored, &identifier(), None, None)
            .await
            .expect("the audit reads the canonical registry, which is the one that is seeded");

        assert_eq!(observation.tags.len(), tags.len());
        let auth_hosts: Vec<String> = data
            .read()
            .auth_calls
            .iter()
            .map(|(registry, _)| registry.clone())
            .collect();
        assert!(
            auth_hosts.iter().all(|host| host == REGISTRY),
            "every read must authenticate against the canonical host, got {auth_hosts:?}"
        );
    }

    // ── F6: the fetches overlap, and every tag keeps its own body ────────────

    /// The observation is keyed by alias, so no ordering of the map itself is
    /// at stake — what completion order could actually corrupt is the *pairing*
    /// of a tag with the body fetched for it. That is what this asserts, under
    /// delays that force the fetches to complete in reverse of submission
    /// order, plus the elapsed bound that proves they really did overlap.
    #[tokio::test]
    async fn f6_overlapping_fetches_keep_every_tag_paired_with_its_own_body() {
        let data = StubTransportData::new();
        let tags: Vec<String> = (0..8).map(|minor| format!("3.{minor}.0")).collect();
        let listed: Vec<&str> = tags.iter().map(String::as_str).collect();
        list(&data, &listed);
        for (position, tag) in tags.iter().enumerate() {
            seed_index(&data, tag, &["linux/amd64"]);
            // Descending delays: the last tag submitted answers first.
            data.write().manifest_delays.insert(
                manifest_key(tag),
                Duration::from_millis(50 - u64::try_from(position).expect("8 fits") * 5),
            );
        }

        let started = std::time::Instant::now();
        let observation = gather(&client(&data), &identifier(), None, None)
            .await
            .expect("the delayed run gathers");
        let elapsed = started.elapsed();

        // Each tag's seeded body embeds its own name, so its digest is unique
        // to it: a swapped pairing cannot survive this comparison, where
        // comparing two runs against each other only ever proved they agreed.
        let expected: Vec<(String, String)> = tags
            .iter()
            .map(|tag| {
                let digest = oci::Algorithm::Sha256
                    .hash(index_bytes(tag, &["linux/amd64"], oci::INDEX_SCHEMA_VERSION))
                    .to_string();
                (tag.clone(), digest)
            })
            .collect();
        assert_eq!(
            observed(&observation),
            expected,
            "every alias must carry the digest of the body served for its own tag"
        );
        assert!(
            elapsed < Duration::from_millis(220),
            "8 tags delayed 15-50ms each took {elapsed:?}; sequential fetching would exceed 260ms, \
             so the delays could not have overlapped and the reordering was never exercised"
        );
    }

    // ── F7-F8, F10: what the body itself can be ──────────────────────────────

    #[tokio::test]
    async fn f7_a_bare_image_manifest_is_kept_as_a_bare_manifest() {
        let data = StubTransportData::new();
        list(&data, &["3.28.1", "latest"]);
        seed_index(&data, "3.28.1", &["linux/amd64"]);
        seed_image(&data, "latest");

        let observation = gather(&client(&data), &identifier(), None, None)
            .await
            .expect("a bare-manifest alias is observable, not an error");

        let latest = observation
            .tags
            .get(&AliasTag::Root { variant: None })
            .expect("latest is observed");
        assert!(
            matches!(latest.manifest, oci::Manifest::Image(_)),
            "an alias pointing at a bare image manifest must be carried as one, \
             so the pure core can report NotAnIndex"
        );
        let leaf = observation
            .tags
            .get(&AliasTag::Version(
                version::Version::parse("3.28.1").expect("a version tag"),
            ))
            .expect("the leaf is observed");
        assert!(matches!(leaf.manifest, oci::Manifest::ImageIndex(_)));
        assert_eq!(
            leaf.size,
            i64::try_from(index_bytes("3.28.1", &["linux/amd64"], oci::INDEX_SCHEMA_VERSION).len())
                .expect("the fixture fits i64"),
            "size must be the byte length of the body the registry served"
        );
    }

    #[tokio::test]
    async fn f8_a_schema_version_one_index_aborts() {
        let data = StubTransportData::new();
        list(&data, &["3.28.1"]);
        seed_raw(&data, "3.28.1", index_bytes("3.28.1", &["linux/amd64"], 1));

        let error = gather(&client(&data), &identifier(), None, None)
            .await
            .expect_err("an index that fails validation must abort the run");

        assert!(
            chain(&error).contains("schemaVersion"),
            "the abort must carry the validator's own reason"
        );
    }

    #[tokio::test]
    async fn f10_an_oversized_manifest_body_aborts() {
        let data = StubTransportData::new();
        list(&data, &["3.28.1"]);
        // One byte past the admission cap. The refusal fires before the body is
        // parsed or digested, so the seeded digest is never consulted.
        let oversized = vec![b'{'; crate::oci::client::MAX_INDEX_DOCUMENT_BYTES + 1];
        data.write().manifests.insert(
            manifest_key("3.28.1"),
            (oversized, oci::Algorithm::Sha256.hash(b"unused").to_string()),
        );

        let error = gather(&client(&data), &identifier(), None, None)
            .await
            .expect_err("an over-cap body must abort");

        assert!(
            chain(&error).contains("cap"),
            "the abort must be the size refusal, not a parse failure"
        );
    }

    // ── F9: which tags are graph nodes ───────────────────────────────────────

    #[tokio::test]
    async fn f9_classification_keeps_versions_latest_and_a_live_variant_root() {
        let data = StubTransportData::new();
        let keep = format!("__ocx.keep.sha256-{}", "ab".repeat(32));
        let graph_tags = ["latest", "3.28.1", "debug-3.12.5", "debug"];
        let listed = [
            "latest",
            "3.28.1",
            "debug-3.12.5",
            "debug",
            // `canary` names no published track, so it is somebody's tag, not a root.
            "canary",
            "nightly-build",
            "__ocx.desc",
            keep.as_str(),
        ];
        list(&data, &listed);
        for tag in graph_tags {
            seed_index(&data, tag, &["linux/amd64"]);
        }

        let observation = gather(&client(&data), &identifier(), None, None)
            .await
            .expect("a mixed listing gathers");

        let mut nodes: Vec<String> = observation.tags.keys().map(AliasTag::to_string).collect();
        nodes.sort();
        assert_eq!(nodes, vec!["3.28.1", "debug", "debug-3.12.5", "latest"]);
        assert!(
            observation.tags.contains_key(&AliasTag::Root {
                variant: Some("debug".to_string())
            }),
            "a bare variant name is that track's root once the track has versions"
        );
        assert_eq!(
            observation.ignored_tags,
            vec![
                "__ocx.desc".to_string(),
                keep,
                "canary".to_string(),
                "nightly-build".to_string(),
            ],
            "everything excluded is reported, so nothing looks silently dropped"
        );
        assert_eq!(
            calls(&data, "pull_manifest_raw"),
            graph_tags.len(),
            "ignored tags are never fetched"
        );
    }

    // ── F11-F12: scale, and the authentication contract ──────────────────────

    #[tokio::test]
    async fn f11_two_hundred_tags_are_each_fetched_exactly_once() {
        let data = StubTransportData::new();
        let tags: Vec<String> = (0..200).map(|minor| format!("3.{minor}.0")).collect();
        // The stub's tag pages are consumed FIFO and the client's chunk size is
        // 100, so a full page keeps paginating and the empty third page stops it.
        data.write().tags = vec![tags[..100].to_vec(), tags[100..].to_vec()];
        for tag in &tags {
            seed_index(&data, tag, &["linux/amd64"]);
        }

        let observation = gather(&client(&data), &identifier(), None, None)
            .await
            .expect("a large package gathers");

        assert_eq!(observation.tags.len(), 200);
        assert_eq!(
            calls(&data, "pull_manifest_raw"),
            200,
            "bounded concurrency must not retry or duplicate a fetch"
        );
    }

    #[tokio::test]
    async fn f12_reads_authenticate_for_pull_only() {
        let data = StubTransportData::new();
        seed_healthy(&data);

        gather(&client(&data), &identifier(), None, None)
            .await
            .expect("a healthy package gathers");

        let auth_calls = data.read().auth_calls.clone();
        assert!(!auth_calls.is_empty(), "reads do authenticate");
        assert!(
            auth_calls
                .iter()
                .all(|(_, operation)| matches!(operation, oci::RegistryOperation::Pull)),
            "check must never probe push access: {auth_calls:?}"
        );
    }

    // ── F13-F15: the index-staleness layer ───────────────────────────────────

    #[tokio::test]
    async fn f13_a_logical_package_reads_the_index_root_exactly_once() {
        let data = StubTransportData::new();
        seed_healthy(&data);
        let committed = some_digest();
        let (source, transport) = seeded_index_source(&committed);

        let observation = gather(
            &client(&data),
            &identifier(),
            Some(&logical_identifier()),
            Some(&source),
        )
        .await
        .expect("a logical package gathers");

        assert_eq!(observation.logical, Some(logical_identifier()));
        let root = observation.index_root.expect("the live root is recorded");
        assert_eq!(
            root.tags.get("latest").map(|entry| entry.content.clone()),
            Some(committed),
            "the root's committed digest is what the staleness layer diffs against"
        );
        assert_eq!(
            transport.request_count(&root_url()),
            1,
            "the root is read once per package, not once per tag"
        );
    }

    #[tokio::test]
    async fn f14_an_index_root_fetch_failure_aborts_the_run() {
        let data = StubTransportData::new();
        seed_healthy(&data);
        let (source, transport) = seeded_index_source(&some_digest());
        transport.fail(&root_url());

        let error = gather(
            &client(&data),
            &identifier(),
            Some(&logical_identifier()),
            Some(&source),
        )
        .await
        .expect_err("a dead index endpoint must abort, not silently drop the layer");

        let rendered = chain(&error);
        assert!(
            rendered.contains(&root_url()),
            "the abort must name the root fetch, not some registry read: {rendered}"
        );
        assert!(
            rendered.contains("simulated index transport failure"),
            "and must carry the transport's own cause: {rendered}"
        );
    }

    /// F16: a logical package the index has never published gathers normally,
    /// with no root and so no index layer at all.
    ///
    /// The index is asked and answers 404 — the shape of a package that is on
    /// the registry before its first announce. That is not a broken index
    /// (F14's error case) and must not abort: the registry graph is exactly as
    /// checkable as it is for a physical invocation, and every index finding is
    /// derived from a root nobody has, so there are none.
    #[tokio::test]
    async fn f16_a_logical_package_with_no_published_root_gathers_without_the_index_layer() {
        let data = StubTransportData::new();
        seed_healthy(&data);
        // Seeded config, unseeded root: the stub answers the root with a 404.
        let transport = StubIndexTransport::default();
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        let source = OcxIndex::new(OcxIndexConfig {
            transport: Box::new(transport.clone()),
            base_url: INDEX_BASE.to_string(),
            namespace: INDEX_NAMESPACE.to_string(),
            client: client(&StubTransportData::new()),
            allow_yanked: false,
            trusted_hosts: Vec::new(),
        });

        let observation = gather(
            &client(&data),
            &identifier(),
            Some(&logical_identifier()),
            Some(&source),
        )
        .await
        .expect("an unpublished logical package is not an error");

        assert!(observation.index_root.is_none(), "a 404 root is an absence, not a root");
        assert_eq!(
            transport.request_count(&root_url()),
            1,
            "the root was genuinely asked for — the absence is observed, not assumed"
        );
        assert_eq!(
            observation.tags.len(),
            4,
            "the registry graph is read in full regardless"
        );

        // The pure core's half of the same claim: no root, no findings.
        let expected = crate::package::cascade::graph::fold_expected(&observation);
        let scope = crate::package::cascade::graph::scope_filter(&observation.versions(), &[None])
            .expect("the whole graph is in scope");
        let report = crate::package::cascade::graph::diff(&observation, &expected, &scope);
        assert!(
            report.index_findings.is_empty(),
            "with no root there is nothing to compare against: {:?}",
            report.index_findings
        );
    }

    #[tokio::test]
    async fn f15_a_physical_package_makes_no_index_request() {
        let data = StubTransportData::new();
        seed_healthy(&data);
        // The source is fully seeded and would answer — F13 proves it does. A
        // physical invocation must simply never ask it.
        let (_source, transport) = seeded_index_source(&some_digest());

        let observation = gather(&client(&data), &identifier(), None, None)
            .await
            .expect("a physical package gathers");

        assert!(observation.index_root.is_none());
        assert!(
            transport.request_urls().is_empty(),
            "no root maps back to a bare repository, so nothing may be requested: {:?}",
            transport.request_urls()
        );
    }
}
