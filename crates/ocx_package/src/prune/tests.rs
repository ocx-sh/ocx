// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Behaviour of tag pruning, driven through the registry and index doubles.
//!
//! Registry answers come from FIFO queues on the transport double, so every
//! test that seeds more than one probe answer seeds one uniform answer, or
//! only one probe can occur. That keeps the tests independent of the order
//! the safeguard probes tags in.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use ocx_index::{IndexFetch, IndexTransport, OcxIndexConfig};
use ocx_oci::client::error::ClientError;
use ocx_oci::client::test_transport::{StubTransportData, mirrored_stub_client, stub_client};
use ocx_oci::client::{DeleteOutcome, ManifestPresence, NotFoundCode};
use ocx_oci::{Algorithm, RegistryOperation};

use super::*;

const CANONICAL_HOST: &str = "registry.example";
const MIRROR_HOST: &str = "mirror.example";
const REPOSITORY: &str = "acme/tool";
const INDEX_URL: &str = "https://index.example";

// ── fixtures: the registry side ──────────────────────────────────────────────

/// The digest an index root records for `tag`.
fn root_content(tag: &str) -> Digest {
    Algorithm::Sha256.hash(format!("root:{tag}").as_bytes())
}

/// The digest the registry serves `tag` under, deliberately unequal to [`root_content`].
fn registry_digest(tag: &str) -> Digest {
    Algorithm::Sha256.hash(format!("registry:{tag}").as_bytes())
}

fn package() -> OciIdentifier {
    OciIdentifier::from_parts(REPOSITORY, "ocx.example")
}

fn repository() -> OciIdentifier {
    OciIdentifier::from_parts(REPOSITORY, CANONICAL_HOST)
}

/// The reference string the transport double records for `tag` of [`repository`].
fn reference(tag: &str) -> String {
    repository().clone_with_tag(tag).canonical_reference().to_string()
}

/// A root listing `rows`, each `(tag, ephemeral)`.
fn root(rows: &[(&str, bool)]) -> IndexRoot {
    let tags = rows
        .iter()
        .map(|(tag, ephemeral)| {
            let marker = if *ephemeral { r#","ephemeral":true"# } else { "" };
            format!(r#""{tag}":{{"content":"{}"{marker}}}"#, root_content(tag))
        })
        .collect::<Vec<_>>()
        .join(",");
    serde_json::from_str(&format!(
        r#"{{"repository":"oci://{CANONICAL_HOST}/{REPOSITORY}","tags":{{{tags}}}}}"#
    ))
    .expect("a root parses")
}

fn root_sha256() -> Digest {
    Algorithm::Sha256.hash(b"served root bytes")
}

/// A located package whose served root lists `rows`.
fn target(rows: &[(&str, bool)]) -> PruneTarget {
    PruneTarget {
        package: package(),
        repository: repository(),
        index: Some(IndexedRoot {
            url: INDEX_URL.to_string(),
            root_sha256: root_sha256(),
            root: root(rows),
        }),
    }
}

fn target_without_index() -> PruneTarget {
    PruneTarget {
        package: package(),
        repository: package(),
        index: None,
    }
}

/// The registry double plus the client speaking to it.
struct Registry {
    data: StubTransportData,
    client: ocx_oci::Client,
}

impl Registry {
    fn new() -> Self {
        let data = StubTransportData::new();
        let client = stub_client(&data);
        Self { data, client }
    }

    fn listing(&self, tags: &[&str]) {
        self.data.write().tags = vec![tags.iter().map(|tag| (*tag).to_string()).collect()];
    }

    fn deletes(&self, results: Vec<Result<DeleteOutcome, ClientError>>) {
        self.data.write().delete_results = results;
    }

    fn probes(&self, results: Vec<Result<ManifestPresence, ClientError>>) {
        self.data.write().probe_results = results;
    }

    fn delete_calls(&self) -> Vec<String> {
        self.data.read().delete_calls.clone()
    }

    fn probe_count(&self) -> usize {
        self.data.read().probe_calls.len()
    }

    fn method_calls(&self, method: &str) -> usize {
        self.data.read().calls.iter().filter(|call| *call == method).count()
    }

    async fn run(&self, target: &PruneTarget, request: &PruneRequest) -> PruneRun {
        prune(&self.client, target, request).await
    }
}

fn absent() -> Result<ManifestPresence, ClientError> {
    Ok(ManifestPresence::Absent(NotFoundCode::ManifestUnknown))
}

fn present(tag: &str) -> Result<ManifestPresence, ClientError> {
    Ok(ManifestPresence::Present(registry_digest(tag)))
}

fn deleted() -> Result<DeleteOutcome, ClientError> {
    Ok(DeleteOutcome::Deleted)
}

fn transient() -> ClientError {
    ClientError::RegistryTransient("simulated 503".into())
}

// ── fixtures: requests and reports ───────────────────────────────────────────

fn explicit(tags: &[&str]) -> PruneRequest {
    PruneRequest {
        selection: PruneSelection::Tags {
            tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
        },
        force: false,
        dry_run: false,
    }
}

fn family(prerelease: &str, keep_builds: Option<u32>) -> PruneRequest {
    PruneRequest {
        selection: PruneSelection::Prerelease {
            prerelease: Version::parse(prerelease).expect("the fixture pre-release parses"),
            keep_builds,
        },
        force: false,
        dry_run: false,
    }
}

fn dry_run(request: PruneRequest) -> PruneRequest {
    PruneRequest {
        dry_run: true,
        ..request
    }
}

fn forced(request: PruneRequest) -> PruneRequest {
    PruneRequest { force: true, ..request }
}

/// `(tag, action, reason)` of every row, in report order.
fn rows(run: &PruneRun) -> Vec<(&str, PruneAction, Option<PruneReason>)> {
    run.outcome
        .tags
        .iter()
        .map(|row| (row.tag.as_str(), row.action, row.reason))
        .collect()
}

fn row<'a>(run: &'a PruneRun, tag: &str) -> &'a PruneTag {
    run.outcome
        .tags
        .iter()
        .find(|row| row.tag == tag)
        .unwrap_or_else(|| panic!("no row for {tag}: {:?}", run.outcome.tags))
}

fn tag_names(run: &PruneRun) -> Vec<&str> {
    run.outcome.tags.iter().map(|row| row.tag.as_str()).collect()
}

fn build(number: u32) -> String {
    format!("0.5.0-canary_2026010{number}000000")
}

const ROLLING: &str = "0.5.0-canary";

// ── fixtures: the index side ─────────────────────────────────────────────────

const INDEX_NAMESPACE: &str = "ocx.sh";
const INDEX_REPOSITORY: &str = "acme/tool";

/// Static-file index boundary: a present document is a hit, an absent one a
/// miss, a registered failure a transport error.
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

    fn request_count(&self, url: &str) -> usize {
        self.requests
            .lock()
            .expect("stub lock")
            .iter()
            .filter(|requested| *requested == url)
            .count()
    }

    fn total_requests(&self) -> usize {
        self.requests.lock().expect("stub lock").len()
    }
}

#[async_trait]
impl IndexTransport for StubIndexTransport {
    async fn get(&self, url: &str) -> ocx_index::error::Result<IndexFetch> {
        self.requests.lock().expect("stub lock").push(url.to_string());
        if self.failures.lock().expect("stub lock").contains(url) {
            return Err(ocx_index::error::Error::IndexHttpFailed {
                url: url.to_string(),
                status: None,
                source: "simulated index transport failure".into(),
            });
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

fn root_url() -> String {
    format!("{INDEX_URL}/p/{INDEX_REPOSITORY}.json")
}

/// The package as the user names it: the index namespace and the repository path.
fn logical_package() -> OciIdentifier {
    OciIdentifier::from_parts(INDEX_REPOSITORY, INDEX_NAMESPACE)
}

/// Deliberately not the canonical serialisation of what it parses to, so a
/// digest over a re-serialised root cannot equal the digest of these bytes.
fn served_root(pointer: &str) -> String {
    format!(
        "{{\n  \"repository\" :  \"{pointer}\",\n  \"tags\" : {{ \"0.5.0-canary_20260101000000\" : {{ \"content\" : \"{}\", \"ephemeral\" : true }} }}\n}}\n",
        root_content("0.5.0-canary_20260101000000")
    )
}

/// An index serving `root_bytes` for [`INDEX_REPOSITORY`], and the transport behind it.
fn index_serving(root_bytes: Option<&str>) -> (OcxIndex, StubIndexTransport) {
    index_serving_at(INDEX_URL, root_bytes)
}

/// [`index_serving`] under another base URL.
fn index_serving_at(base_url: &str, root_bytes: Option<&str>) -> (OcxIndex, StubIndexTransport) {
    let transport = StubIndexTransport::default();
    transport.insert(&format!("{base_url}/config.json"), br#"{"format_version":1}"#);
    if let Some(bytes) = root_bytes {
        transport.insert(&format!("{base_url}/p/{INDEX_REPOSITORY}.json"), bytes.as_bytes());
    }
    let source = OcxIndex::new(OcxIndexConfig {
        transport: Box::new(transport.clone()),
        base_url: base_url.to_string(),
        namespace: INDEX_NAMESPACE.to_string(),
        client: stub_client(&StubTransportData::new()),
        allow_yanked: false,
        trusted_hosts: Vec::new(),
        insecure_hosts: Vec::new(),
        proxy_rules: ocx_oci::ssrf::ProxyRules::direct(),
    });
    (source, transport)
}

fn index_source(source: &OcxIndex) -> Option<&OcxIndex> {
    Some(source)
}

/// A public address literal: it passes the host guard without a DNS lookup.
const PUBLIC_POINTER: &str = "oci://93.184.216.34/acme/tool";

// ── parse_prerelease_family ──────────────────────────────────────────────────

#[test]
fn a_prerelease_without_a_build_is_a_family() {
    let version = parse_prerelease_family("0.5.0-canary").expect("a pre-release family parses");

    assert_eq!(version.prerelease().as_deref(), Some("canary"));
    assert!(!version.has_build());
    assert!(version.variant().is_none());
}

#[test]
fn a_variant_prefixed_prerelease_is_a_family() {
    let version = parse_prerelease_family("slim-0.5.0-canary").expect("a variant pre-release parses");

    assert_eq!(version.variant(), Some("slim"));
    assert_eq!(version.prerelease().as_deref(), Some("canary"));
}

#[test]
fn anything_but_a_prerelease_without_a_build_is_not_a_family() {
    for value in [
        "0.5.0",
        "0.5",
        "0",
        "0.5.0-canary_20260101000000",
        "0.5.0-canary+build",
        "canary",
        "not a version",
        "",
    ] {
        let error = parse_prerelease_family(value).expect_err(&format!("{value:?} is not a pre-release family"));
        assert!(
            matches!(&error, PruneError::NotAPrereleaseFamily { value: named } if named == value),
            "{value:?} must name itself in NotAPrereleaseFamily, got {error:?}"
        );
    }
}

// ── parse_tag ────────────────────────────────────────────────────────────────

#[test]
fn a_plain_tag_is_kept_verbatim() {
    for value in ["1.2.3", "0.5.0-canary_20260101000000", "latest", "main"] {
        assert_eq!(parse_tag(value).expect("a tag parses"), value);
    }
}

#[test]
fn a_digest_is_not_a_tag() {
    let value = format!("sha256:{}", "a".repeat(64));

    let error = parse_tag(&value).expect_err("a digest must be refused");

    assert!(matches!(&error, PruneError::DigestTag { value: named } if *named == value));
}

#[test]
fn a_reference_pinned_by_digest_is_not_a_tag() {
    let value = format!("1.2.3@sha256:{}", "a".repeat(64));

    let error = parse_tag(&value).expect_err("a digest-pinned reference must be refused");

    assert!(matches!(&error, PruneError::DigestTag { value: named } if *named == value));
}

#[test]
fn a_tag_outside_the_oci_grammar_is_refused() {
    let too_long = "a".repeat(129);
    for value in [
        "x/../../other/manifests/1.0",
        "sha256%3Aab",
        "a?b",
        "a#b",
        "a+b",
        "",
        ".hidden",
        "-dash",
        too_long.as_str(),
    ] {
        let error = parse_tag(value).expect_err("an invalid tag must be refused");

        assert!(
            matches!(&error, PruneError::InvalidTag { value: named, .. } if named == value),
            "{value:?}: got {error:?}"
        );
    }
}

#[test]
fn a_tag_of_128_characters_is_accepted() {
    let value = "a".repeat(128);

    assert_eq!(parse_tag(&value).expect("128 characters is the limit"), value);
}

#[test]
fn a_colon_or_at_sign_is_refused_as_a_digest() {
    for value in ["a:b", "a@b"] {
        let error = parse_tag(value).expect_err("a `:` or `@` never names a tag");

        assert!(
            matches!(&error, PruneError::DigestTag { .. }),
            "{value:?}: got {error:?}"
        );
    }
}

#[test]
fn a_keep_tag_is_never_pruned() {
    let value = format!("__ocx.keep.sha256-{}", "a".repeat(64));

    let error = parse_tag(&value).expect_err("a keep tag must be refused");

    assert!(matches!(&error, PruneError::InvalidTag { value: named, .. } if *named == value));
}

// ── locate ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn locate_returns_the_repository_the_root_points_at() {
    let (source, _) = index_serving(Some(&served_root(PUBLIC_POINTER)));

    let located = locate(&logical_package(), index_source(&source))
        .await
        .expect("a served root locates");

    assert_eq!(located.package, logical_package());
    assert_eq!(located.repository.registry(), "93.184.216.34");
    assert_eq!(located.repository.repository(), "acme/tool");
}

#[tokio::test]
async fn locate_records_the_index_url_and_the_hash_of_the_served_bytes() {
    let served = served_root(PUBLIC_POINTER);
    let (source, _) = index_serving(Some(&served));

    let located = locate(&logical_package(), index_source(&source))
        .await
        .expect("a served root locates");

    let indexed = located.index.expect("a configured index is recorded");
    assert_eq!(indexed.url, INDEX_URL);
    assert_eq!(indexed.root_sha256, Algorithm::Sha256.hash(served.as_bytes()));
    assert!(indexed.root.tags.contains_key("0.5.0-canary_20260101000000"));
}

#[tokio::test]
async fn locate_reads_the_root_once_per_call_and_remembers_nothing() {
    let (source, transport) = index_serving(Some(&served_root(PUBLIC_POINTER)));

    locate(&logical_package(), index_source(&source)).await.expect("first");
    assert_eq!(transport.request_count(&root_url()), 1, "one GET per locate");

    locate(&logical_package(), index_source(&source)).await.expect("second");
    assert_eq!(
        transport.request_count(&root_url()),
        2,
        "a removal decision never reads a memo"
    );
}

#[tokio::test]
async fn locate_without_an_index_uses_the_package_as_the_repository() {
    let located = locate(&package(), None).await.expect("no index locates trivially");

    assert_eq!(located.repository, package());
    assert!(located.index.is_none());
}

#[tokio::test]
async fn locate_reports_a_package_without_a_root_as_not_in_the_index() {
    let (source, _) = index_serving(None);

    let error = locate(&logical_package(), index_source(&source))
        .await
        .expect_err("no root, no package");

    assert!(
        matches!(&error, PruneError::NotInIndex { url, .. } if url == INDEX_URL),
        "got {error:?}"
    );
}

#[tokio::test]
async fn locate_reports_an_unreachable_index_as_an_unreadable_root() {
    let (source, transport) = index_serving(Some(&served_root(PUBLIC_POINTER)));
    transport.fail(&root_url());

    let error = locate(&logical_package(), index_source(&source))
        .await
        .expect_err("a failing root read must not locate");

    assert!(
        matches!(&error, PruneError::RootUnreadable { url, .. } if url == INDEX_URL),
        "got {error:?}"
    );
}

#[tokio::test]
async fn locate_refuses_a_pointer_at_a_forbidden_host() {
    let (source, _) = index_serving(Some(&served_root("oci://127.0.0.1/acme/tool")));

    let error = locate(&logical_package(), index_source(&source))
        .await
        .expect_err("a loopback pointer must be refused");

    assert!(
        matches!(
            &error,
            PruneError::RepositoryPointer {
                source: ocx_index::error::Error::Ssrf { .. },
                ..
            }
        ),
        "got {error:?}"
    );
}

#[tokio::test]
async fn locate_refuses_a_pointer_that_does_not_parse() {
    let (source, _) = index_serving(Some(&served_root("not an oci pointer")));

    let error = locate(&logical_package(), index_source(&source))
        .await
        .expect_err("an unparseable pointer must be refused");

    assert!(matches!(&error, PruneError::RepositoryPointer { .. }), "got {error:?}");
}

#[tokio::test]
async fn locate_refuses_a_package_that_names_a_tag_or_a_digest_before_any_read() {
    let (source, transport) = index_serving(Some(&served_root(PUBLIC_POINTER)));
    let with_tag = logical_package().clone_with_tag("1.0.0");
    let with_digest = logical_package().clone_with_digest(root_content("x"));

    for named in [with_tag, with_digest] {
        let error = locate(&named, index_source(&source))
            .await
            .expect_err("a package must be bare");
        assert!(matches!(&error, PruneError::PackageNotBare { .. }), "got {error:?}");
    }
    assert_eq!(transport.total_requests(), 0, "refused before the index is asked");
}

#[tokio::test]
async fn an_index_url_credential_never_reaches_the_report_or_an_error() {
    const CREDENTIALED: &str = "https://user:secret@index.example";
    let assert_redacted = |text: &str| {
        assert!(
            !text.contains("secret") && !text.contains("user:"),
            "the index credential leaked: {text}"
        );
    };

    let (source, _) = index_serving_at(CREDENTIALED, Some(&served_root(PUBLIC_POINTER)));
    let located = locate(&logical_package(), Some(&source))
        .await
        .expect("a served root locates");
    assert_eq!(
        located.index.as_ref().map(|indexed| indexed.url.as_str()),
        Some("https://***@index.example"),
        "the reported URL is the redacted URL the root was read from"
    );
    let registry = Registry::new();
    registry.probes(vec![present("fresh")]);
    let refused = registry.run(&located, &explicit(&["fresh"])).await;
    let error = refused.error.expect("a tag the root lacks is refused");
    assert!(matches!(error, PruneError::Refused { .. }), "got {error:?}");
    assert_redacted(&error.to_string());
    assert_redacted(&serde_json::to_string(&refused.outcome).expect("the outcome serialises"));

    let (without_root, _) = index_serving_at(CREDENTIALED, None);
    let error = locate(&logical_package(), Some(&without_root))
        .await
        .expect_err("no root, no package");
    assert!(matches!(error, PruneError::NotInIndex { .. }), "got {error:?}");
    assert_redacted(&error.to_string());

    let (unreachable, transport) = index_serving_at(CREDENTIALED, Some(&served_root(PUBLIC_POINTER)));
    transport.fail(&format!("{CREDENTIALED}/p/{INDEX_REPOSITORY}.json"));
    let error = locate(&logical_package(), Some(&unreachable))
        .await
        .expect_err("a failing root read must not locate");
    assert!(matches!(error, PruneError::RootUnreadable { .. }), "got {error:?}");
    assert_redacted(&error.to_string());
}

// ── structural selection ─────────────────────────────────────────────────────

fn mixed_listing() -> Vec<String> {
    let mut listing = vec![
        build(3),
        ROLLING.to_string(),
        build(1),
        build(2),
        "0.5.0-other_20260101000000".to_string(),
        "0.5.0-other".to_string(),
        "0.5.0".to_string(),
        "0.5".to_string(),
        "0".to_string(),
        "latest".to_string(),
        "slim-0.5.0-canary_20260101000000".to_string(),
        "slim-0.5.0-canary".to_string(),
        "0.5.1-canary_20260101000000".to_string(),
        "0.4.0-canary_20260101000000".to_string(),
        "main".to_string(),
        format!("__ocx.keep.sha256-{}", "a".repeat(64)),
    ];
    listing.retain(|tag| !tag.is_empty());
    listing
}

fn ephemeral_family(builds: &[u32]) -> Vec<(String, bool)> {
    let mut rows: Vec<(String, bool)> = builds.iter().map(|number| (build(*number), true)).collect();
    rows.push((ROLLING.to_string(), true));
    rows
}

fn target_for(rows: &[(String, bool)]) -> PruneTarget {
    let borrowed: Vec<(&str, bool)> = rows.iter().map(|(tag, ephemeral)| (tag.as_str(), *ephemeral)).collect();
    target(&borrowed)
}

fn set_listing(registry: &Registry, listing: &[String]) {
    let borrowed: Vec<&str> = listing.iter().map(String::as_str).collect();
    registry.listing(&borrowed);
}

#[tokio::test]
async fn a_family_selects_its_builds_oldest_first_and_its_rolling_tag_last() {
    let registry = Registry::new();
    set_listing(&registry, &mixed_listing());
    let target = target_for(&ephemeral_family(&[1, 2, 3]));

    let run = registry.run(&target, &dry_run(family("0.5.0-canary", None))).await;

    assert!(run.error.is_none(), "got {:?}", run.error);
    assert_eq!(
        rows(&run),
        vec![
            (build(1).as_str(), PruneAction::WouldDelete, None),
            (build(2).as_str(), PruneAction::WouldDelete, None),
            (build(3).as_str(), PruneAction::WouldDelete, None),
            (ROLLING, PruneAction::WouldDelete, None),
        ]
    );
    assert_eq!(
        registry.probe_count(),
        0,
        "selection reads the listing, never a manifest"
    );
}

#[tokio::test]
async fn a_family_leaves_other_variants_pre_releases_releases_and_non_versions_alone() {
    let registry = Registry::new();
    set_listing(&registry, &mixed_listing());
    let target = target_for(&ephemeral_family(&[1, 2, 3]));

    let run = registry.run(&target, &dry_run(family("0.5.0-canary", None))).await;

    let selected = tag_names(&run);
    for untouched in [
        "0.5.0-other_20260101000000",
        "0.5.0-other",
        "0.5.0",
        "0.5",
        "0",
        "latest",
        "slim-0.5.0-canary_20260101000000",
        "slim-0.5.0-canary",
        "0.5.1-canary_20260101000000",
        "0.4.0-canary_20260101000000",
        "main",
    ] {
        assert!(!selected.contains(&untouched), "{untouched} is not in the family");
    }
    assert!(
        !selected.iter().any(|tag| tag.starts_with("__ocx.keep.")),
        "a keep tag is never a version"
    );
}

#[tokio::test]
async fn a_variant_family_selects_only_that_variant() {
    let registry = Registry::new();
    set_listing(&registry, &mixed_listing());
    let target = target(&[("slim-0.5.0-canary_20260101000000", true), ("slim-0.5.0-canary", true)]);

    let run = registry.run(&target, &dry_run(family("slim-0.5.0-canary", None))).await;

    assert!(run.error.is_none(), "got {:?}", run.error);
    assert_eq!(
        tag_names(&run),
        vec!["slim-0.5.0-canary_20260101000000", "slim-0.5.0-canary"]
    );
}

#[tokio::test]
async fn a_rolling_tag_the_listing_lacks_is_not_selected() {
    let registry = Registry::new();
    let listing: Vec<String> = mixed_listing().into_iter().filter(|tag| tag != ROLLING).collect();
    set_listing(&registry, &listing);
    let target = target_for(&ephemeral_family(&[1, 2, 3]));

    let run = registry.run(&target, &dry_run(family("0.5.0-canary", None))).await;

    assert!(run.error.is_none(), "got {:?}", run.error);
    assert_eq!(tag_names(&run), vec![build(1), build(2), build(3)]);
    assert_eq!(registry.probe_count(), 0, "a missing rolling tag is never probed for");
}

#[tokio::test]
async fn keep_builds_keeps_the_newest_builds_by_version_order_and_the_rolling_tag() {
    let registry = Registry::new();
    // Listed out of order: newest is decided by version order, not by listing order.
    let listing = vec![build(4), build(1), ROLLING.to_string(), build(5), build(3), build(2)];
    set_listing(&registry, &listing);
    let target = target_for(&ephemeral_family(&[1, 2, 3, 4, 5]));

    let run = registry.run(&target, &dry_run(family("0.5.0-canary", Some(2)))).await;

    assert!(run.error.is_none(), "got {:?}", run.error);
    assert_eq!(
        rows(&run),
        vec![
            (build(1).as_str(), PruneAction::WouldDelete, None),
            (build(2).as_str(), PruneAction::WouldDelete, None),
            (build(3).as_str(), PruneAction::WouldDelete, None),
            (build(4).as_str(), PruneAction::Kept, Some(PruneReason::Newest)),
            (build(5).as_str(), PruneAction::Kept, Some(PruneReason::Newest)),
            (ROLLING, PruneAction::Kept, Some(PruneReason::Rolling)),
        ]
    );
    assert_eq!(row(&run, &build(5)).digest, Some(root_content(&build(5))));
}

#[tokio::test]
async fn keep_builds_beyond_the_number_of_builds_deletes_nothing() {
    let registry = Registry::new();
    set_listing(&registry, &mixed_listing());
    let target = target_for(&ephemeral_family(&[1, 2, 3]));

    let run = registry.run(&target, &family("0.5.0-canary", Some(10))).await;

    assert!(run.error.is_none(), "got {:?}", run.error);
    assert_eq!(
        rows(&run),
        vec![
            (build(1).as_str(), PruneAction::Kept, Some(PruneReason::Newest)),
            (build(2).as_str(), PruneAction::Kept, Some(PruneReason::Newest)),
            (build(3).as_str(), PruneAction::Kept, Some(PruneReason::Newest)),
            (ROLLING, PruneAction::Kept, Some(PruneReason::Rolling)),
        ]
    );
    assert!(registry.delete_calls().is_empty());
}

#[tokio::test]
async fn a_family_with_no_matching_tag_selects_nothing_and_still_owes_a_tags_file() {
    let registry = Registry::new();
    registry.listing(&["0.5.0", "latest", "0.5.0-other_20260101000000"]);

    let run = registry.run(&target(&[]), &family("0.5.0-canary", None)).await;

    assert!(run.error.is_none(), "got {:?}", run.error);
    assert!(run.outcome.tags.is_empty());
    assert_eq!(run.outcome.tags_file_entries(), Some(Vec::new()));
    assert!(registry.delete_calls().is_empty());
}

#[tokio::test]
async fn a_repository_the_registry_does_not_know_holds_an_empty_family() {
    let registry = Registry::new();
    registry.data.write().list_tags_results = vec![Err(ClientError::RepositoryNotFound(REPOSITORY.into()))];

    let run = registry.run(&target(&[]), &family("0.5.0-canary", None)).await;

    assert!(run.error.is_none(), "got {:?}", run.error);
    assert!(run.outcome.tags.is_empty());
    assert_eq!(run.outcome.tags_file_entries(), Some(Vec::new()));
    assert!(registry.delete_calls().is_empty());
}

#[tokio::test]
async fn a_failed_listing_ends_the_run_before_selection_and_owes_no_tags_file() {
    let registry = Registry::new();
    registry.data.write().list_tags_results = vec![Err(ClientError::Registry("registry unreachable".into()))];

    let run = registry.run(&target(&[]), &family("0.5.0-canary", None)).await;

    assert!(
        matches!(run.error, Some(PruneError::Registry(_))),
        "got {:?}",
        run.error
    );
    assert_eq!(run.outcome.tags_file_entries(), None);
}

// ── explicit selection ───────────────────────────────────────────────────────

#[tokio::test]
async fn explicit_tags_keep_input_order_and_drop_repeats() {
    let registry = Registry::new();
    let target = target(&[("b", true), ("a", true), ("c", true)]);

    let run = registry
        .run(&target, &dry_run(explicit(&["b", "a", "b", "c", "a"])))
        .await;

    assert!(run.error.is_none(), "got {:?}", run.error);
    assert_eq!(tag_names(&run), vec!["b", "a", "c"]);
    assert_eq!(registry.method_calls("list_tags"), 0, "explicit mode never lists");
}

// ── safeguard ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_tag_the_root_marks_ephemeral_is_deletable() {
    let registry = Registry::new();

    let run = registry
        .run(&target(&[("snap", true)]), &dry_run(explicit(&["snap"])))
        .await;

    assert!(run.error.is_none(), "got {:?}", run.error);
    assert_eq!(rows(&run), vec![("snap", PruneAction::WouldDelete, None)]);
    assert_eq!(row(&run, "snap").digest, Some(root_content("snap")));
    assert_eq!(registry.probe_count(), 0, "a tag in the root needs no probe");
}

#[tokio::test]
async fn a_durable_tag_is_refused_and_nothing_is_deleted() {
    let registry = Registry::new();
    let target = target(&[("snap", true), ("release", false)]);

    let run = registry.run(&target, &explicit(&["snap", "release"])).await;

    assert!(
        matches!(
            &run.error,
            Some(PruneError::Refused { durable, not_in_index, .. })
                if durable == &["release"] && not_in_index.is_empty()
        ),
        "got {:?}",
        run.error
    );
    assert!(
        registry.delete_calls().is_empty(),
        "a refusal aborts before the first DELETE"
    );
    assert_eq!(
        rows(&run),
        vec![
            ("snap", PruneAction::NotAttempted, None),
            ("release", PruneAction::Refused, Some(PruneReason::Durable)),
        ]
    );
    assert_eq!(row(&run, "release").digest, Some(root_content("release")));
}

#[tokio::test]
async fn a_tag_in_the_registry_but_not_the_root_is_refused_as_not_in_the_index() {
    let registry = Registry::new();
    registry.probes(vec![present("fresh")]);

    let run = registry.run(&target(&[]), &explicit(&["fresh"])).await;

    assert!(
        matches!(
            &run.error,
            Some(PruneError::Refused { durable, not_in_index, .. })
                if durable.is_empty() && not_in_index == &["fresh"]
        ),
        "got {:?}",
        run.error
    );
    assert!(registry.delete_calls().is_empty());
    assert_eq!(
        rows(&run),
        vec![("fresh", PruneAction::Refused, Some(PruneReason::NotInIndex))]
    );
    assert_eq!(
        row(&run, "fresh").digest,
        Some(registry_digest("fresh")),
        "with no root row the digest is the one the registry served"
    );
}

#[test]
fn a_refusal_hint_agrees_in_number_with_the_tags_it_refuses() {
    let tags = |names: &[&str]| names.iter().map(ToString::to_string).collect::<Vec<_>>();

    let one = refusal_message("acme/tool", INDEX_URL, &tags(&["a"]), &tags(&["b"]));
    assert!(
        one.contains("delete it anyway") && one.contains("the index row stays"),
        "{one}"
    );
    assert!(
        one.contains("once its announce") && one.contains("announce it with"),
        "{one}"
    );

    let two = refusal_message("acme/tool", INDEX_URL, &tags(&["a", "b"]), &tags(&["c", "d"]));
    assert!(
        two.contains("delete them anyway") && two.contains("the index rows stay"),
        "{two}"
    );
    assert!(
        two.contains("once their announce") && two.contains("announce them with"),
        "{two}"
    );
}

#[tokio::test]
async fn a_tag_absent_from_root_and_registry_is_skipped_without_refusal() {
    let registry = Registry::new();
    registry.probes(vec![absent()]);

    let run = registry.run(&target(&[]), &explicit(&["ghost"])).await;

    assert!(run.error.is_none(), "got {:?}", run.error);
    assert_eq!(rows(&run), vec![("ghost", PruneAction::Absent, None)]);
    assert_eq!(row(&run, "ghost").digest, None);
    assert!(registry.delete_calls().is_empty(), "there is nothing to delete");
}

#[tokio::test]
async fn both_refusal_reasons_are_reported_together() {
    let registry = Registry::new();
    registry.probes(vec![present("fresh")]);
    let target = target(&[("release", false)]);

    let run = registry.run(&target, &explicit(&["release", "fresh"])).await;

    assert!(
        matches!(
            &run.error,
            Some(PruneError::Refused { durable, not_in_index, .. })
                if durable == &["release"] && not_in_index == &["fresh"]
        ),
        "got {:?}",
        run.error
    );
    assert!(registry.delete_calls().is_empty());
}

#[tokio::test]
async fn a_dry_run_refuses_exactly_as_the_real_run_does() {
    let request = explicit(&["snap", "release"]);
    let target = target(&[("snap", true), ("release", false)]);

    let real = Registry::new().run(&target, &request).await;
    let preview = Registry::new().run(&target, &dry_run(request)).await;

    assert!(matches!(
        (&real.error, &preview.error),
        (
            Some(PruneError::Refused { durable: real_durable, .. }),
            Some(PruneError::Refused { durable: preview_durable, .. }),
        ) if real_durable == preview_durable
    ));
}

#[tokio::test]
async fn a_passing_dry_run_deletes_nothing_and_writes_no_tags_file() {
    let registry = Registry::new();

    let run = registry
        .run(&target(&[("a", true), ("b", true)]), &dry_run(explicit(&["a", "b"])))
        .await;

    assert!(run.error.is_none(), "got {:?}", run.error);
    assert_eq!(
        rows(&run),
        vec![
            ("a", PruneAction::WouldDelete, None),
            ("b", PruneAction::WouldDelete, None),
        ]
    );
    assert!(registry.delete_calls().is_empty());
    assert_eq!(run.outcome.tags_file_entries(), None);
}

#[tokio::test]
async fn without_an_index_and_without_force_nothing_is_deleted() {
    for request in [explicit(&["snap"]), dry_run(explicit(&["snap"]))] {
        let registry = Registry::new();

        let run = registry.run(&target_without_index(), &request).await;

        assert!(
            matches!(run.error, Some(PruneError::NoIndex { .. })),
            "got {:?}",
            run.error
        );
        assert!(registry.delete_calls().is_empty());
    }
}

#[tokio::test]
async fn force_deletes_a_durable_tag_and_reports_no_reason() {
    let registry = Registry::new();
    registry.deletes(vec![deleted()]);
    registry.probes(vec![absent()]);

    let run = registry
        .run(&target(&[("release", false)]), &forced(explicit(&["release"])))
        .await;

    assert!(run.error.is_none(), "got {:?}", run.error);
    assert_eq!(rows(&run), vec![("release", PruneAction::Deleted, None)]);
    assert_eq!(registry.delete_calls(), vec![reference("release")]);
}

#[tokio::test]
async fn force_deletes_a_tag_the_index_does_not_list_after_still_probing_for_it() {
    let registry = Registry::new();
    registry.deletes(vec![deleted()]);
    // The not-in-index probe finds it, then the confirmation finds it gone.
    registry.probes(vec![present("fresh"), absent()]);

    let run = registry.run(&target(&[]), &forced(explicit(&["fresh"]))).await;

    assert!(run.error.is_none(), "got {:?}", run.error);
    assert_eq!(rows(&run), vec![("fresh", PruneAction::Deleted, None)]);
    assert_eq!(registry.probe_count(), 2, "the probe still runs under --force");
}

#[tokio::test]
async fn a_forced_delete_of_a_tag_the_root_does_not_list_stays_out_of_the_tags_file() {
    let registry = Registry::new();
    registry.deletes(vec![deleted()]);
    registry.probes(vec![present("fresh"), absent()]);

    let run = registry.run(&target(&[]), &forced(explicit(&["fresh"]))).await;

    assert!(run.error.is_none(), "got {:?}", run.error);
    assert_eq!(rows(&run), vec![("fresh", PruneAction::Deleted, None)]);
    assert_eq!(
        run.outcome.tags_file_entries(),
        Some(Vec::new()),
        "the root has no row to remove, so announce would fail on the tag"
    );
}

#[tokio::test]
async fn force_leaves_a_tag_absent_everywhere_absent() {
    let registry = Registry::new();
    registry.probes(vec![absent()]);

    let run = registry.run(&target(&[]), &forced(explicit(&["ghost"]))).await;

    assert!(run.error.is_none(), "got {:?}", run.error);
    assert_eq!(rows(&run), vec![("ghost", PruneAction::Absent, None)]);
    assert!(registry.delete_calls().is_empty());
}

// ── delete loop ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn builds_are_deleted_oldest_first_and_the_rolling_tag_last_each_confirmed_before_the_next() {
    let registry = Registry::new();
    registry.listing(&[&build(3), ROLLING, &build(1), &build(2)]);
    registry.deletes(vec![deleted(), deleted(), deleted(), deleted()]);
    registry.probes(vec![absent(), absent(), absent(), absent()]);
    let target = target_for(&ephemeral_family(&[1, 2, 3]));

    let run = registry.run(&target, &family("0.5.0-canary", None)).await;

    assert!(run.error.is_none(), "got {:?}", run.error);
    assert_eq!(
        registry.delete_calls(),
        vec![
            reference(&build(1)),
            reference(&build(2)),
            reference(&build(3)),
            reference(ROLLING)
        ]
    );
    let registry_calls: Vec<String> = registry
        .data
        .read()
        .calls
        .iter()
        .filter(|call| *call == "delete_manifest" || *call == "probe_manifest")
        .cloned()
        .collect();
    assert_eq!(
        registry_calls,
        ["delete_manifest", "probe_manifest"].repeat(4),
        "every DELETE is confirmed before the next one"
    );
    assert!(rows(&run).iter().all(|(_, action, _)| *action == PruneAction::Deleted));
}

#[tokio::test]
async fn the_confirmation_probe_retries_until_the_tag_is_gone() {
    let registry = Registry::new();
    registry.deletes(vec![deleted()]);
    registry.probes(vec![present("snap"), present("snap"), absent()]);

    let run = registry.run(&target(&[("snap", true)]), &explicit(&["snap"])).await;

    assert!(run.error.is_none(), "got {:?}", run.error);
    assert_eq!(rows(&run), vec![("snap", PruneAction::Deleted, None)]);
    assert_eq!(registry.probe_count(), 3);
    assert_eq!(
        registry.delete_calls().len(),
        1,
        "the tag is deleted once, not per retry"
    );
}

#[tokio::test]
async fn a_tag_still_present_after_three_confirmations_stops_the_run() {
    let registry = Registry::new();
    registry.deletes(vec![deleted()]);
    registry.probes(vec![present("first"), present("first"), present("first")]);
    let target = target(&[("first", true), ("second", true)]);

    let run = registry.run(&target, &explicit(&["first", "second"])).await;

    assert!(
        matches!(&run.error, Some(PruneError::StillPresent { tag, .. }) if tag == "first"),
        "got {:?}",
        run.error
    );
    assert_eq!(registry.probe_count(), 3, "exactly three confirmations");
    assert_eq!(registry.delete_calls().len(), 1, "the second tag is never attempted");
    assert_eq!(row(&run, "second").action, PruneAction::NotAttempted);
    assert_eq!(
        run.outcome.tags_file_entries(),
        Some(Vec::new()),
        "a tag that is still served is not gone"
    );
}

#[tokio::test]
async fn a_not_found_answer_of_either_kind_after_the_runs_own_delete_counts_as_gone() {
    for code in [
        NotFoundCode::ManifestUnknown,
        NotFoundCode::NameUnknown,
        NotFoundCode::Unspecified,
    ] {
        let registry = Registry::new();
        registry.deletes(vec![deleted()]);
        registry.probes(vec![Ok(ManifestPresence::Absent(code))]);

        let run = registry.run(&target(&[("snap", true)]), &explicit(&["snap"])).await;

        assert!(
            run.error.is_none(),
            "{code:?} after the run's own DELETE: {:?}",
            run.error
        );
        assert_eq!(rows(&run), vec![("snap", PruneAction::Deleted, None)], "{code:?}");
    }
}

#[tokio::test]
async fn a_tag_already_absent_is_still_confirmed_and_reported_absent_when_gone() {
    let registry = Registry::new();
    registry.deletes(vec![Ok(DeleteOutcome::AlreadyAbsent)]);
    registry.probes(vec![absent()]);

    let run = registry.run(&target(&[("snap", true)]), &explicit(&["snap"])).await;

    assert!(run.error.is_none(), "got {:?}", run.error);
    assert_eq!(rows(&run), vec![("snap", PruneAction::Absent, None)]);
    assert_eq!(
        registry.probe_count(),
        1,
        "the confirmation runs after AlreadyAbsent too"
    );
}

#[tokio::test]
async fn a_tag_the_registry_calls_absent_but_still_serves_fails_as_still_present() {
    let registry = Registry::new();
    // A registry hiding a repository the caller may not modify answers 404 to the DELETE.
    registry.deletes(vec![Ok(DeleteOutcome::AlreadyAbsent)]);
    registry.probes(vec![present("snap"), present("snap"), present("snap")]);

    let run = registry.run(&target(&[("snap", true)]), &explicit(&["snap"])).await;

    assert!(
        matches!(&run.error, Some(PruneError::StillPresent { tag, .. }) if tag == "snap"),
        "got {:?}",
        run.error
    );
}

#[tokio::test]
async fn a_credential_refused_on_the_first_delete_stops_the_run() {
    let registry = Registry::new();
    registry.deletes(vec![Err(ClientError::Authentication("token lacks delete".into()))]);
    let target = target(&[("first", true), ("second", true)]);

    let run = registry.run(&target, &explicit(&["first", "second"])).await;

    assert!(
        matches!(&run.error, Some(PruneError::DeleteDenied { tag, .. }) if tag == "first"),
        "got {:?}",
        run.error
    );
    assert_eq!(registry.delete_calls().len(), 1);
    assert_eq!(row(&run, "second").action, PruneAction::NotAttempted);
    assert!(
        !rows(&run).iter().any(|(_, action, _)| *action == PruneAction::Deleted),
        "nothing was deleted"
    );
}

#[tokio::test]
async fn a_registry_that_cannot_delete_tags_stops_on_the_first_tag() {
    let registry = Registry::new();
    registry.deletes(vec![Err(ClientError::DeleteUnsupported {
        registry: CANONICAL_HOST.to_string(),
        status: 405,
    })]);
    let target = target(&[("first", true), ("second", true)]);

    let run = registry.run(&target, &explicit(&["first", "second"])).await;

    assert!(
        matches!(
            &run.error,
            Some(PruneError::Registry(ClientError::DeleteUnsupported { .. }))
        ),
        "got {:?}",
        run.error
    );
    assert_eq!(registry.delete_calls().len(), 1, "one DELETE, then stop");
    assert!(
        !rows(&run).iter().any(|(_, action, _)| *action == PruneAction::Deleted),
        "nothing was deleted"
    );
    assert_eq!(run.outcome.tags_file_entries(), Some(Vec::new()));
}

#[tokio::test]
async fn an_error_mid_loop_leaves_the_rest_not_attempted_and_keeps_what_is_gone() {
    let registry = Registry::new();
    registry.deletes(vec![deleted(), Err(transient())]);
    registry.probes(vec![absent()]);
    let target = target(&[("a", true), ("b", true), ("c", true)]);

    let run = registry.run(&target, &explicit(&["a", "b", "c"])).await;

    assert!(
        matches!(run.error, Some(PruneError::Registry(_))),
        "got {:?}",
        run.error
    );
    assert_eq!(row(&run, "a").action, PruneAction::Deleted);
    assert_eq!(row(&run, "b").action, PruneAction::NotAttempted);
    assert_eq!(row(&run, "c").action, PruneAction::NotAttempted);
    assert_eq!(
        run.outcome.tags_file_entries(),
        Some(vec!["a".to_string()]),
        "the tags file lists what is gone so far"
    );
}

#[tokio::test]
async fn the_tags_file_lists_deleted_tags_and_tags_gone_but_still_in_the_root() {
    let registry = Registry::new();
    // `a` is deleted now, `b` was already gone, `c` was never anywhere.
    registry.deletes(vec![deleted(), Ok(DeleteOutcome::AlreadyAbsent)]);
    registry.probes(vec![absent(), absent(), absent()]);
    let target = target(&[("a", true), ("b", true)]);

    let run = registry.run(&target, &explicit(&["a", "b", "c"])).await;

    assert!(run.error.is_none(), "got {:?}", run.error);
    assert_eq!(
        rows(&run),
        vec![
            ("a", PruneAction::Deleted, None),
            ("b", PruneAction::Absent, None),
            ("c", PruneAction::Absent, None),
        ]
    );
    assert_eq!(
        run.outcome.tags_file_entries(),
        Some(vec!["a".to_string(), "b".to_string()]),
        "c is in neither the registry nor the root, so announce has nothing to remove for it"
    );
}

#[tokio::test]
async fn every_registry_call_goes_to_the_canonical_host_never_a_mirror() {
    let data = StubTransportData::new();
    let client = mirrored_stub_client(&data, CANONICAL_HOST, MIRROR_HOST, "");
    data.write().tags = vec![vec![build(1), ROLLING.to_string()]];
    data.write().delete_results = vec![deleted(), deleted()];
    data.write().probe_results = vec![absent(), absent()];
    let target = target_for(&ephemeral_family(&[1]));

    let run = prune(&client, &target, &family("0.5.0-canary", None)).await;

    assert!(run.error.is_none(), "got {:?}", run.error);
    let inner = data.read();
    assert!(
        inner.delete_calls.iter().all(|call| call.starts_with(CANONICAL_HOST)),
        "deletes: {:?}",
        inner.delete_calls
    );
    assert!(
        inner.probe_calls.iter().all(|call| call.starts_with(CANONICAL_HOST)),
        "probes: {:?}",
        inner.probe_calls
    );
    assert!(!inner.auth_calls.is_empty());
    assert!(
        inner.auth_calls.iter().all(|(registry, _)| registry == CANONICAL_HOST),
        "auth: {:?}",
        inner.auth_calls
    );
    assert!(
        inner
            .auth_calls
            .iter()
            .any(|(_, operation)| *operation == RegistryOperation::Delete),
        "deletes authenticate with the delete scope"
    );
}

// ── the report ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_outcome_echoes_the_request_and_the_target() {
    let registry = Registry::new();
    let request = forced(dry_run(explicit(&["snap"])));

    let run = registry.run(&target(&[("snap", true)]), &request).await;

    assert_eq!(run.outcome.package, package());
    assert_eq!(run.outcome.repository, repository());
    assert_eq!(run.outcome.selection, request.selection);
    assert!(run.outcome.force);
    assert!(run.outcome.dry_run);
    let index = run.outcome.index.expect("a located root is reported");
    assert_eq!(index.url, INDEX_URL);
    assert_eq!(index.root_sha256, root_sha256());
}

#[tokio::test]
async fn the_report_document_has_the_documented_shape() {
    let registry = Registry::new();
    registry.deletes(vec![deleted()]);
    registry.probes(vec![absent()]);
    let target = target(&[("old", true)]);

    let run = registry.run(&target, &explicit(&["old"])).await;
    let document = serde_json::to_value(&run.outcome).expect("the outcome serialises");

    let mut keys: Vec<&str> = document
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "dry_run",
            "force",
            "index",
            "package",
            "repository",
            "selection",
            "tags"
        ]
    );
    assert_eq!(
        document["package"],
        serde_json::to_value(package()).expect("serialises")
    );
    assert_eq!(
        document["repository"],
        serde_json::to_value(repository()).expect("serialises")
    );
    assert_eq!(document["selection"], serde_json::json!({"tags": ["old"]}));
    assert_eq!(document["force"], false);
    assert_eq!(document["dry_run"], false);
    assert_eq!(
        document["index"],
        serde_json::json!({"url": INDEX_URL, "root_sha256": root_sha256().to_string()})
    );
    assert_eq!(
        document["tags"],
        serde_json::json!([{
            "tag": "old",
            "digest": root_content("old").to_string(),
            "action": "deleted",
            "reason": null,
        }])
    );
}

#[tokio::test]
async fn action_and_reason_serialise_as_their_documented_words() {
    let registry = Registry::new();
    let listing = vec![build(1), build(2), ROLLING.to_string()];
    set_listing(&registry, &listing);
    let target = target_for(&ephemeral_family(&[1, 2]));

    let run = registry.run(&target, &dry_run(family("0.5.0-canary", Some(1)))).await;
    let document = serde_json::to_value(&run.outcome).expect("the outcome serialises");

    let words: Vec<(&str, &serde_json::Value)> = document["tags"]
        .as_array()
        .expect("tags is an array")
        .iter()
        .map(|entry| (entry["action"].as_str().expect("action"), &entry["reason"]))
        .collect();
    assert_eq!(
        words,
        vec![
            ("would_delete", &serde_json::Value::Null),
            ("kept", &serde_json::json!("newest")),
            ("kept", &serde_json::json!("rolling")),
        ]
    );
}

#[tokio::test]
async fn a_refusal_serialises_its_reason_and_the_skipped_rows_as_not_attempted() {
    let registry = Registry::new();
    let target = target(&[("snap", true), ("release", false)]);

    let run = registry.run(&target, &explicit(&["snap", "release"])).await;
    let document = serde_json::to_value(&run.outcome).expect("the outcome serialises");

    assert_eq!(document["tags"][0]["action"], "not_attempted");
    assert_eq!(document["tags"][1]["action"], "refused");
    assert_eq!(document["tags"][1]["reason"], "durable");
}

#[tokio::test]
async fn the_family_selection_serialises_with_its_keep_builds() {
    let registry = Registry::new();
    registry.listing(&[]);

    let with_keep = registry
        .run(&target(&[]), &dry_run(family("0.5.0-canary", Some(3))))
        .await;
    let document = serde_json::to_value(&with_keep.outcome).expect("the outcome serialises");

    assert_eq!(
        document["selection"],
        serde_json::json!({"prerelease": "0.5.0-canary", "keep_builds": 3})
    );
}

#[tokio::test]
async fn a_run_without_an_index_reports_a_null_index() {
    let registry = Registry::new();

    let run = registry
        .run(&target_without_index(), &forced(dry_run(explicit(&[]))))
        .await;
    let document = serde_json::to_value(&run.outcome).expect("the outcome serialises");

    assert!(run.error.is_none(), "got {:?}", run.error);
    assert!(document["index"].is_null());
    assert_eq!(
        document["repository"],
        serde_json::to_value(package()).expect("serialises")
    );
}

#[tokio::test]
async fn a_forced_run_without_an_index_deletes_from_the_package_repository() {
    let registry = Registry::new();
    registry.deletes(vec![deleted()]);
    // The not-in-index probe finds it, then the confirmation finds it gone.
    registry.probes(vec![present("snap"), absent()]);

    let run = registry
        .run(&target_without_index(), &forced(explicit(&["snap"])))
        .await;

    assert!(run.error.is_none(), "got {:?}", run.error);
    assert_eq!(rows(&run), vec![("snap", PruneAction::Deleted, None)]);
    assert_eq!(
        registry.delete_calls(),
        vec![package().clone_with_tag("snap").canonical_reference().to_string()]
    );
}
