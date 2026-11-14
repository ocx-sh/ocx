// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Tag deletion for GitHub Packages (`ghcr.io`, `containers.<ghes-host>`), which has no OCI tag DELETE.
//!
//! Contract: the owner type comes from `GET /users/{owner}`; the package's versions are listed with
//! pagination; the delete set is the version carrying the tag, plus the platform-manifest versions only
//! it references, plus the referrer fallback-tag versions (`sha256-<digest>`). A version in that set that
//! carries another tag refuses the run with `prune_shared_version`, as does a version another remaining tagged
//! version's index lists. A 401 or 403 is the existing `DeleteDenied` (exit 80), a 404 on the owner or the
//! listing too, but a spent rate limit (429, or 403 with `x-ratelimit-remaining: 0`) is a transient failure
//! (exit 75). The API has no conditional delete, so a candidate prune must not overlap a release push of
//! identical bytes.

use futures::StreamExt as _;
use ocx_oci::{Digest, OciIdentifier};
use ocx_package::prune::{DeletePlan, DeleteReport, PruneError, TagDeleter, confirm_gone};

use super::ForgeKind;
use super::ForgeToken;

/// Deletes tags by deleting the GitHub Packages versions that carry them.
pub struct GithubPackagesDeleter<'a> {
    client: &'a ocx_oci::Client,
    http: reqwest::Client,
    api_base: String,
    owner: String,
    package: String,
    token: ForgeToken,
}

impl<'a> GithubPackagesDeleter<'a> {
    /// A deleter for `owner`'s `package`, reading manifests through `client` and talking to the REST API at
    /// `api_base` through `http`, which must be the redirect-free forge client.
    #[must_use]
    pub fn new(
        client: &'a ocx_oci::Client,
        http: reqwest::Client,
        api_base: String,
        owner: String,
        package: String,
        token: ForgeToken,
    ) -> Self {
        Self {
            client,
            http,
            api_base,
            owner,
            package,
            token,
        }
    }
}

/// One package version as the REST API lists it: a manifest named by digest, with the tags pointing at it.
struct Version {
    id: u64,
    digest: String,
    tags: Vec<String>,
}

/// Versions per listing page; the API's maximum.
const PAGE_SIZE: usize = 100;
/// Listing pages read before the run gives up, bounding a listing the server never ends.
const MAX_PAGES: usize = 500;
/// Manifests of kept versions read at once while computing the delete set.
const MANIFEST_READ_CONCURRENCY: usize = 8;

impl GithubPackagesDeleter<'_> {
    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.api_base.trim_end_matches('/')))
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {}", self.token.0))
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
    }

    /// Sends `builder`; 401 and 403 become [`PruneError::DeleteDenied`] for `tag`, every other non-success
    /// answer, a redirect included, a registry failure.
    async fn send(
        &self,
        builder: reqwest::RequestBuilder,
        repository: &OciIdentifier,
        tag: &str,
    ) -> Result<reqwest::Response, PruneError> {
        let response = builder.send().await.map_err(|error| forge_failure(&error))?;
        let status = response.status();
        let headers = response.headers();
        let exhausted = headers
            .get("x-ratelimit-remaining")
            .and_then(|value| value.to_str().ok())
            == Some("0");
        // The secondary limit answers 403 with `retry-after` and a non-zero remaining count.
        let throttled = exhausted || headers.contains_key(reqwest::header::RETRY_AFTER);
        // A spent rate limit answers 403 (or 429) too; denying the credential for it would exit 80, not 75.
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS || (status == reqwest::StatusCode::FORBIDDEN && throttled) {
            return Err(transient(&format!("rate limit reached ({status}); retry later")));
        }
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(denied(repository, tag, &format!("the forge answered {status}")));
        }
        Ok(response)
    }

    /// `orgs` or `users`, from the one owner lookup of this run.
    async fn owner_collection(&self, repository: &OciIdentifier, tag: &str) -> Result<&'static str, PruneError> {
        let path = format!("/users/{}", encode(&self.owner));
        let response = self
            .send(self.request(reqwest::Method::GET, &path), repository, tag)
            .await?;
        refuse_not_found(&response, repository, tag)?;
        let body = ok_json(response).await?;
        match body.get("type").and_then(serde_json::Value::as_str) {
            Some("Organization") => Ok("orgs"),
            Some("User") => Ok("users"),
            other => Err(forge_failure(&format!(
                "owner {} has an unknown type {other:?}",
                self.owner
            ))),
        }
    }

    fn versions_path(&self, collection: &str) -> String {
        format!(
            "/{collection}/{}/packages/container/{}/versions",
            encode(&self.owner),
            encode(&self.package)
        )
    }

    async fn list_versions(
        &self,
        collection: &str,
        repository: &OciIdentifier,
        tag: &str,
    ) -> Result<Vec<Version>, PruneError> {
        let mut versions = Vec::new();
        for page in 1..=MAX_PAGES {
            let path = format!("{}?per_page={PAGE_SIZE}&page={page}", self.versions_path(collection));
            let response = self
                .send(self.request(reqwest::Method::GET, &path), repository, tag)
                .await?;
            refuse_not_found(&response, repository, tag)?;
            let body = ok_json(response).await?;
            let entries = body
                .as_array()
                .ok_or_else(|| forge_failure(&"the version listing is not an array"))?;
            for entry in entries {
                // A version the run cannot name is invisible to the protection pass, so it fails the run.
                let (Some(id), Some(digest)) = (
                    entry.get("id").and_then(serde_json::Value::as_u64),
                    entry.get("name").and_then(serde_json::Value::as_str),
                ) else {
                    return Err(forge_failure(&"a listed version has no id or name"));
                };
                let tags = entry
                    .pointer("/metadata/container/tags")
                    .and_then(serde_json::Value::as_array)
                    .map(|tags| tags.iter().filter_map(|tag| tag.as_str().map(str::to_owned)).collect())
                    .unwrap_or_default();
                versions.push(Version {
                    id,
                    digest: digest.to_owned(),
                    tags,
                });
            }
            if entries.len() < PAGE_SIZE {
                return Ok(versions);
            }
        }
        Err(forge_failure(&"the version listing never ended"))
    }

    /// The digests an index manifest lists, empty for an image manifest; `None` when the registry does not
    /// serve the manifest.
    async fn children(&self, repository: &OciIdentifier, digest: &str) -> Result<Option<Vec<String>>, PruneError> {
        let parsed = Digest::try_from(digest).map_err(|error| forge_failure(&error))?;
        let manifest = match self
            .client
            .fetch_manifest_any_format(&repository.clone_with_digest(parsed))
            .await
        {
            Ok((_, manifest)) => manifest,
            Err(ocx_oci::client::error::ClientError::ManifestNotFound(_)) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        Ok(Some(match manifest {
            ocx_oci::Manifest::ImageIndex(index) => index.manifests.into_iter().map(|entry| entry.digest).collect(),
            ocx_oci::Manifest::Image(_) => Vec::new(),
        }))
    }

    /// [`children`](Self::children) for a version that is being deleted anyway: an unserved manifest lists nothing.
    async fn children_or_none(&self, repository: &OciIdentifier, digest: &str) -> Result<Vec<String>, PruneError> {
        Ok(self.children(repository, digest).await?.unwrap_or_default())
    }
}

#[async_trait::async_trait]
impl TagDeleter for GithubPackagesDeleter<'_> {
    async fn delete(&self, plan: &DeletePlan, report: &mut DeleteReport) -> Result<(), PruneError> {
        let Some(first_tag) = plan.tags.first() else {
            return Ok(());
        };
        let repository = &plan.repository;
        let collection = self.owner_collection(repository, first_tag).await?;
        let versions = self.list_versions(collection, repository, first_tag).await?;

        let mut roots: Vec<&Version> = Vec::new();
        let mut found: Vec<(&String, u64)> = Vec::new();
        for tag in &plan.tags {
            match versions.iter().find(|version| version.tags.contains(tag)) {
                Some(version) => {
                    found.push((tag, version.id));
                    if !roots.iter().any(|root| root.id == version.id) {
                        roots.push(version);
                    }
                }
                None => {
                    confirm_gone(self.client, repository, tag).await?;
                    report.absent.push(tag.clone());
                }
            }
        }

        let set = self.delete_set(repository, &plan.tags, &versions, &roots).await?;
        refuse_shared(repository, &plan.tags, &set)?;
        refuse_stale_rows(repository, plan, &found, &versions)?;

        if plan.dry_run {
            for version in &set {
                log::info!("would delete {repository} version {} ({})", version.id, version.digest);
            }
            report
                .would_delete
                .extend(found.into_iter().map(|(tag, _)| tag.clone()));
            return Ok(());
        }
        // The listing is minutes old by now; any tagged version that appeared or changed since may list a set member,
        // so the run refuses rather than delete on stale protection.
        let fresh = self.list_versions(collection, repository, first_tag).await?;
        if !listing_unchanged(&set, &versions, &fresh) {
            return Err(transient(
                "the package changed while the delete set was computed; retry",
            ));
        }
        // Roots lead the set. A tag is reported the moment its own version is gone, so a later failure
        // on a platform or referrer version cannot relabel it as untouched.
        for (position, version) in set.iter().enumerate() {
            // A failure names the tag this version carries; a platform or referrer version, the last root's.
            let label = found
                .iter()
                .find(|(_, id)| *id == version.id)
                .map_or_else(|| found.last().map_or(first_tag, |(tag, _)| *tag), |(tag, _)| *tag);
            self.delete_version(collection, version, repository, label).await?;
            if position < roots.len() {
                for (tag, _) in found.iter().filter(|(_, id)| *id == version.id) {
                    confirm_gone(self.client, repository, tag).await?;
                    report.deleted.push((*tag).clone());
                }
            }
        }
        Ok(())
    }
}

impl GithubPackagesDeleter<'_> {
    async fn delete_version(
        &self,
        collection: &str,
        version: &Version,
        repository: &OciIdentifier,
        tag: &str,
    ) -> Result<(), PruneError> {
        let path = format!("{}/{}", self.versions_path(collection), version.id);
        let response = self
            .send(self.request(reqwest::Method::DELETE, &path), repository, tag)
            .await?;
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            log::debug!("{repository} version {} was already gone (404)", version.id);
        } else if !status.is_success() {
            return Err(forge_failure(&format!(
                "deleting version {} answered {status}",
                version.id
            )));
        }
        Ok(())
    }

    /// The versions removing `roots` removes: the roots, the platform manifests no remaining tagged version
    /// lists, and the referrer fallback-tag versions of all of those, referrers of referrers included.
    /// Roots first. A root another remaining tagged version lists is refused as shared.
    async fn delete_set<'v>(
        &self,
        repository: &OciIdentifier,
        requested: &[String],
        versions: &'v [Version],
        roots: &[&'v Version],
    ) -> Result<Vec<&'v Version>, PruneError> {
        let by_digest = |digest: &str| versions.iter().find(|version| version.digest == digest);
        let is_root = |version: &Version| roots.iter().any(|root| root.id == version.id);

        let kept = versions
            .iter()
            .filter(|version| !is_root(version) && !version.tags.is_empty());
        // `buffered`, not `buffer_unordered`: the first failing version in listing order is the one reported.
        // Futures built up front: a closure inside the stream fails `async_trait`'s higher-ranked `Send` check.
        let reads: Vec<_> = kept
            .map(|version| async move { (version, self.children(repository, &version.digest).await) })
            .collect();
        let mut reads = futures::stream::iter(reads).buffered(MANIFEST_READ_CONCURRENCY);
        let mut protected: Vec<(String, String)> = Vec::new();
        while let Some((version, children)) = reads.next().await {
            let other_tag = version.tags.first().cloned().unwrap_or_default();
            // An unserved manifest protects nothing, so a version kept on the registry's word alone is
            // read as unknown, never as empty: the run stops before any DELETE.
            let children = children?.ok_or_else(|| {
                transient(&format!(
                    "the manifest of kept version {} ({}) is not served; retry",
                    version.id, version.digest
                ))
            })?;
            for child in children {
                protected.push((child, other_tag.clone()));
            }
        }
        for root in roots {
            if let Some((_, other_tag)) = protected.iter().find(|(digest, _)| *digest == root.digest) {
                let own = root.tags.iter().find(|tag| requested.contains(tag));
                return Err(PruneError::SharedVersion {
                    repository: repository.to_string(),
                    tag: own.or(requested.first()).cloned().unwrap_or_default(),
                    other_tag: other_tag.clone(),
                });
            }
        }
        let mut set: Vec<&Version> = roots.to_vec();
        for root in roots {
            for child in self.children_or_none(repository, &root.digest).await? {
                if protected.iter().any(|(digest, _)| *digest == child) {
                    continue;
                }
                if let Some(version) = by_digest(&child) {
                    push_unique(&mut set, version);
                }
            }
        }
        // Fixpoint over the referrers: an attestation's own signature hangs off the attestation's
        // fallback tag. `push_unique` is the visited set.
        let mut position = 0;
        while let Some(&member) = set.get(position) {
            position += 1;
            let wanted = carrier_tags(&member.digest);
            for carrier in versions
                .iter()
                .filter(|version| version.tags.iter().any(|tag| wanted.contains(tag)))
            {
                if !push_unique(&mut set, carrier) {
                    continue;
                }
                for referrer in self.children_or_none(repository, &carrier.digest).await? {
                    if let Some(version) = by_digest(&referrer) {
                        push_unique(&mut set, version);
                    }
                }
            }
        }
        Ok(set)
    }
}

/// Whether every member of `set` is still listed with the same digest and tags, and the tagged versions are
/// exactly those of the first listing. A new or changed tagged version may list a member as an index child, which
/// the protection computed from the first listing never saw.
fn listing_unchanged(set: &[&Version], first: &[Version], fresh: &[Version]) -> bool {
    let key = |version: &Version| {
        let mut tags = version.tags.clone();
        tags.sort();
        (version.id, version.digest.clone(), tags)
    };
    let members_hold = set.iter().all(|member| fresh.iter().any(|now| key(now) == key(member)));
    let tagged = |versions: &[Version]| {
        let mut keys: Vec<_> = versions
            .iter()
            .filter(|version| !version.tags.is_empty())
            .map(key)
            .collect();
        keys.sort();
        keys
    };
    members_hold && tagged(first) == tagged(fresh)
}

/// Refuses when the version carrying a tag is not the digest the index row for that tag names: the row is
/// stale, and the delete would remove a different build than the one the index vouched for.
#[expect(
    clippy::result_large_err,
    reason = "refusals are cold and the error is the shared PruneError; boxing would only allocate on the refusal path"
)]
fn refuse_stale_rows(
    repository: &OciIdentifier,
    plan: &DeletePlan,
    found: &[(&String, u64)],
    versions: &[Version],
) -> Result<(), PruneError> {
    for (tag, indexed) in &plan.indexed {
        let served = found
            .iter()
            .find(|(found_tag, _)| *found_tag == tag)
            .and_then(|(_, id)| versions.iter().find(|version| version.id == *id));
        if let Some(served) = served.filter(|version| version.digest != indexed.to_string()) {
            return Err(PruneError::IndexStale {
                repository: repository.to_string(),
                tag: tag.clone(),
                indexed: indexed.to_string(),
                served: served.digest.clone(),
            });
        }
    }
    Ok(())
}

/// Adds `version` unless the set holds it; whether it was added.
fn push_unique<'v>(set: &mut Vec<&'v Version>, version: &'v Version) -> bool {
    let absent = !set.iter().any(|member| member.id == version.id);
    if absent {
        set.push(version);
    }
    absent
}

/// The credential cannot do what the request needs; exit 80.
fn denied(repository: &OciIdentifier, tag: &str, message: &str) -> PruneError {
    PruneError::DeleteDenied {
        repository: repository.to_string(),
        tag: tag.to_string(),
        source: ocx_oci::client::error::ClientError::Authentication(Box::new(std::io::Error::other(
            message.to_owned(),
        ))),
    }
}

/// GitHub answers 404 for a package the token cannot read as well as for one that does not exist.
#[expect(
    clippy::result_large_err,
    reason = "refusals are cold and the error is the shared PruneError; boxing would only allocate on the refusal path"
)]
fn refuse_not_found(response: &reqwest::Response, repository: &OciIdentifier, tag: &str) -> Result<(), PruneError> {
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(denied(
            repository,
            tag,
            "the package was not found, or the token lacks package read access",
        ));
    }
    Ok(())
}

/// `sha256:<hex>` becomes the referrers fallback tag `sha256-<hex>`.
fn fallback_tag(digest: &str) -> String {
    digest.replace(':', "-")
}

/// The tags a referrer of `digest` may hang off: the referrers fallback tag and the cosign `.sig` and `.att` tags.
fn carrier_tags(digest: &str) -> [String; 3] {
    let fallback = fallback_tag(digest);
    [format!("{fallback}.sig"), format!("{fallback}.att"), fallback]
}

/// Refuses when a version in the set carries a tag that is neither requested nor a carrier or keep tag
/// of a manifest in the set.
#[expect(
    clippy::result_large_err,
    reason = "refusals are cold and the error is the shared PruneError; boxing would only allocate on the refusal path"
)]
fn refuse_shared(repository: &OciIdentifier, requested: &[String], set: &[&Version]) -> Result<(), PruneError> {
    let mut allowed: Vec<String> = requested.to_vec();
    for member in set {
        allowed.push(format!("__ocx.keep.{}", fallback_tag(&member.digest)));
        allowed.extend(carrier_tags(&member.digest));
    }
    for member in set {
        if let Some(other) = member.tags.iter().find(|tag| !allowed.contains(tag)) {
            let own = member.tags.iter().find(|tag| requested.contains(tag));
            return Err(PruneError::SharedVersion {
                repository: repository.to_string(),
                tag: own.or(requested.first()).cloned().unwrap_or_default(),
                other_tag: other.clone(),
            });
        }
    }
    Ok(())
}

async fn ok_json(response: reqwest::Response) -> Result<serde_json::Value, PruneError> {
    let status = response.status();
    if !status.is_success() {
        return Err(forge_failure(&format!("the forge answered {status}")));
    }
    response.json().await.map_err(|error| forge_failure(&error))
}

/// A failure a retry may clear (exit 75); the message never carries the credential.
fn transient(cause: &str) -> PruneError {
    PruneError::Registry(ocx_oci::client::error::ClientError::RegistryTransient(Box::new(
        std::io::Error::other(format!("GitHub Packages API: {cause}")),
    )))
}

/// A forge failure as a registry failure; the message never carries the credential.
fn forge_failure(cause: &dyn std::fmt::Display) -> PruneError {
    PruneError::Registry(ocx_oci::client::error::ClientError::Registry(Box::new(
        std::io::Error::other(format!("GitHub Packages API: {cause}")),
    )))
}

fn encode(segment: &str) -> String {
    percent_encoding::utf8_percent_encode(segment, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// The REST API base for a registry host: `ghcr.io` is `https://api.github.com`, `containers.<host>` is
/// a GitHub Enterprise Server at `https://<host>/api/v3`, anything else has none.
#[must_use]
pub fn api_base_for_registry(host: &str) -> Option<String> {
    if host.eq_ignore_ascii_case("ghcr.io") {
        return Some("https://api.github.com".to_owned());
    }
    let enterprise = host
        .get(.."containers.".len())
        .filter(|prefix| prefix.eq_ignore_ascii_case("containers."))?;
    let rest = &host[enterprise.len()..];
    (!rest.is_empty()).then(|| format!("https://{rest}/api/v3"))
}

/// The forge a registry host implies: `ghcr.io` only. A `containers.<host>` name is not inferred because a
/// registry host can come from an index document, and the credential would go to the host it names.
#[must_use]
pub fn forge_for_registry(host: &str) -> Option<ForgeKind> {
    host.eq_ignore_ascii_case("ghcr.io").then_some(ForgeKind::GitHub)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use ocx_oci::client::ManifestPresence;
    use ocx_oci::client::NotFoundCode;
    use ocx_oci::client::test_transport::{StubTransportData, stub_client};
    use ocx_oci::{Algorithm, Digest, OciIdentifier};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    const REGISTRY: &str = "ghcr.io";
    const OWNER: &str = "acme";
    const PACKAGE: &str = "tool";

    /// A loopback HTTP peer: every request is recorded as `"METHOD /path?query"` and answered by `route`.
    struct Fake {
        base: String,
        requests: Arc<Mutex<Vec<String>>>,
    }

    impl Fake {
        async fn start(route: impl Fn(&str, &str) -> (u16, String) + Send + Sync + 'static) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind loopback");
            let base = format!("http://{}", listener.local_addr().expect("local addr"));
            let requests = Arc::new(Mutex::new(Vec::new()));
            let seen = Arc::clone(&requests);
            tokio::spawn(async move {
                loop {
                    let Ok((mut stream, _)) = listener.accept().await else {
                        return;
                    };
                    let mut buffer = Vec::new();
                    let mut chunk = [0u8; 2048];
                    while !buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                        match stream.read(&mut chunk).await {
                            Ok(0) | Err(_) => break,
                            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
                        }
                    }
                    let text = String::from_utf8_lossy(&buffer).into_owned();
                    let mut parts = text.lines().next().unwrap_or_default().split(' ');
                    let (method, target) = (parts.next().unwrap_or_default(), parts.next().unwrap_or_default());
                    seen.lock().expect("requests lock").push(format!("{method} {target}"));
                    let (status, body) = route(method, target);
                    let limit = if body.contains("secondary") {
                        "Retry-After: 60\r\n"
                    } else if body.contains("rate limit") {
                        "X-RateLimit-Remaining: 0\r\n"
                    } else {
                        ""
                    };
                    let reply = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n{limit}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(reply.as_bytes()).await;
                }
            });
            Self { base, requests }
        }

        fn requests(&self) -> Vec<String> {
            self.requests.lock().expect("requests lock").clone()
        }

        fn deletes(&self) -> Vec<String> {
            self.requests()
                .into_iter()
                .filter(|r| r.starts_with("DELETE "))
                .collect()
        }
    }

    fn digest(label: &str) -> String {
        Algorithm::Sha256.hash(label.as_bytes()).to_string()
    }

    fn fallback(digest: &str) -> String {
        digest.replace(':', "-")
    }

    /// An OCI image index listing `children`, as bytes plus the digest those bytes hash to.
    fn index(children: &[&str]) -> (Vec<u8>, String) {
        let manifests = children
            .iter()
            .map(|child| {
                format!(r#"{{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"{child}","size":1}}"#)
            })
            .collect::<Vec<_>>()
            .join(",");
        let bytes = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{manifests}]}}"#
        )
        .into_bytes();
        let digest = Algorithm::Sha256.hash(&bytes).to_string();
        (bytes, digest)
    }

    fn repository() -> OciIdentifier {
        OciIdentifier::from_parts(format!("{OWNER}/{PACKAGE}"), REGISTRY)
    }

    /// Serves `bytes` for `digest` in the stub registry.
    fn serve(data: &StubTransportData, digest_text: &str, bytes: Vec<u8>) {
        let digest = Digest::try_from(digest_text).expect("a digest");
        let key = repository().clone_with_digest(digest).canonical_reference().to_string();
        data.write().manifests.insert(key, (bytes, digest_text.to_string()));
    }

    /// The registry confirms every tag gone.
    fn confirm_all_gone(data: &StubTransportData, count: usize) {
        for _ in 0..count {
            data.write()
                .probe_results
                .push(Ok(ManifestPresence::Absent(NotFoundCode::ManifestUnknown)));
        }
    }

    fn version(id: u64, name: &str, tags: &[&str]) -> serde_json::Value {
        serde_json::json!({"id": id, "name": name, "metadata": {"container": {"tags": tags}}})
    }

    fn deleter<'a>(client: &'a ocx_oci::Client, base: &str) -> GithubPackagesDeleter<'a> {
        let http = super::super::http::build_forge_http_client(Duration::from_secs(5), &Default::default())
            .expect("http client");
        GithubPackagesDeleter::new(
            client,
            http,
            base.to_string(),
            OWNER.to_string(),
            PACKAGE.to_string(),
            ForgeToken::new("secret".to_string()),
        )
    }

    fn plan(tags: &[&str], dry_run: bool) -> DeletePlan {
        DeletePlan {
            repository: repository(),
            tags: tags.iter().map(ToString::to_string).collect(),
            indexed: Vec::new(),
            dry_run,
        }
    }

    /// A route table: the owner lookup answers `owner_type`, the version list answers `versions` in pages of
    /// 100, every DELETE answers 204.
    fn route(owner_type: &'static str, versions: Vec<serde_json::Value>) -> impl Fn(&str, &str) -> (u16, String) {
        move |method, target| {
            if method == "GET" && target == format!("/users/{OWNER}") {
                return (200, format!(r#"{{"type":"{owner_type}"}}"#));
            }
            let kind = if owner_type == "Organization" { "orgs" } else { "users" };
            let base = format!("/{kind}/{OWNER}/packages/container/{PACKAGE}/versions");
            if method == "GET" && target.starts_with(&base) {
                let page: usize = target
                    .split("&page=")
                    .nth(1)
                    .and_then(|rest| rest.split('&').next())
                    .and_then(|number| number.parse().ok())
                    .unwrap_or(1);
                let slice: Vec<_> = versions.iter().skip((page - 1) * 100).take(100).cloned().collect();
                return (200, serde_json::Value::Array(slice).to_string());
            }
            if method == "DELETE" && target.starts_with(&format!("{base}/")) {
                return (204, String::new());
            }
            (404, r#"{"message":"unrouted"}"#.to_string())
        }
    }

    #[tokio::test]
    async fn an_organization_owner_is_listed_and_deleted_under_orgs_with_one_owner_lookup() {
        let (index_bytes, index_digest) = index(&[]);
        let data = StubTransportData::new();
        serve(&data, &index_digest, index_bytes);
        confirm_all_gone(&data, 1);
        let client = stub_client(&data);
        let fake = Fake::start(route("Organization", vec![version(7, &index_digest, &["old"])])).await;

        let mut report = DeleteReport::default();
        deleter(&client, &fake.base)
            .delete(&plan(&["old"], false), &mut report)
            .await
            .expect("deleted");

        assert_eq!(report.deleted, ["old"]);
        let lookups = fake
            .requests()
            .iter()
            .filter(|r| *r == &format!("GET /users/{OWNER}"))
            .count();
        assert_eq!(lookups, 1, "one owner lookup per run: {:?}", fake.requests());
        assert_eq!(
            fake.deletes(),
            [format!("DELETE /orgs/{OWNER}/packages/container/{PACKAGE}/versions/7")]
        );
    }

    #[tokio::test]
    async fn a_user_owner_goes_through_the_users_routes() {
        let (index_bytes, index_digest) = index(&[]);
        let data = StubTransportData::new();
        serve(&data, &index_digest, index_bytes);
        confirm_all_gone(&data, 1);
        let client = stub_client(&data);
        let fake = Fake::start(route("User", vec![version(7, &index_digest, &["old"])])).await;

        let mut report = DeleteReport::default();
        deleter(&client, &fake.base)
            .delete(&plan(&["old"], false), &mut report)
            .await
            .expect("deleted");

        assert_eq!(
            fake.deletes(),
            [format!("DELETE /users/{OWNER}/packages/container/{PACKAGE}/versions/7")]
        );
    }

    #[tokio::test]
    async fn a_version_past_the_first_page_is_found() {
        let (index_bytes, index_digest) = index(&[]);
        let data = StubTransportData::new();
        serve(&data, &index_digest, index_bytes);
        confirm_all_gone(&data, 1);
        let client = stub_client(&data);
        let mut versions: Vec<_> = (0..100)
            .map(|n| version(1000 + n, &digest(&format!("filler{n}")), &[]))
            .collect();
        versions.push(version(7, &index_digest, &["old"]));
        let fake = Fake::start(route("Organization", versions)).await;

        let mut report = DeleteReport::default();
        deleter(&client, &fake.base)
            .delete(&plan(&["old"], false), &mut report)
            .await
            .expect("deleted");

        assert_eq!(report.deleted, ["old"]);
        assert!(
            fake.requests().iter().any(|r| r.contains("page=2")),
            "{:?}",
            fake.requests()
        );
    }

    /// The tagged index I lists A and B; a durable index J lists B. A referrer fallback index F lists R.
    struct Graph {
        data: StubTransportData,
        versions: Vec<serde_json::Value>,
    }

    fn graph(index_tags: &[&str], extra_tag_on_a: Option<&str>) -> Graph {
        let (a, b, referrer) = (digest("a"), digest("b"), digest("referrer"));
        let (index_bytes, index_digest) = index(&[&a, &b]);
        let (durable_bytes, durable_digest) = index(&[&b]);
        let (fallback_bytes, fallback_digest) = index(&[&referrer]);
        let data = StubTransportData::new();
        serve(&data, &index_digest, index_bytes);
        serve(&data, &durable_digest, durable_bytes);
        serve(&data, &fallback_digest, fallback_bytes);
        serve(&data, &a, index(&[]).0);
        let mut a_tags = vec![];
        a_tags.extend(extra_tag_on_a);
        let fallback_tag = fallback(&index_digest);
        let versions = vec![
            version(1, &index_digest, index_tags),
            version(2, &a, &a_tags),
            version(3, &b, &[]),
            version(4, &durable_digest, &["1.0.0"]),
            version(5, &fallback_digest, &[&fallback_tag]),
            version(6, &referrer, &[]),
        ];
        Graph { data, versions }
    }

    #[tokio::test]
    async fn the_delete_set_is_the_version_its_private_platforms_and_the_referrer_fallback_versions() {
        let graph = graph(&["old"], None);
        confirm_all_gone(&graph.data, 1);
        let client = stub_client(&graph.data);
        let fake = Fake::start(route("Organization", graph.versions.clone())).await;

        let mut report = DeleteReport::default();
        deleter(&client, &fake.base)
            .delete(&plan(&["old"], false), &mut report)
            .await
            .expect("deleted");

        let mut deleted = fake.deletes();
        deleted.sort();
        let expected: Vec<String> = [1, 2, 5, 6]
            .iter()
            .map(|id| format!("DELETE /orgs/{OWNER}/packages/container/{PACKAGE}/versions/{id}"))
            .collect();
        assert_eq!(
            deleted, expected,
            "B is still listed by the durable index and must survive"
        );
        assert_eq!(report.deleted, ["old"]);
    }

    #[tokio::test]
    async fn a_keep_tag_on_a_version_in_the_set_does_not_make_it_shared() {
        let (a, b) = (digest("a"), digest("b"));
        let (index_bytes, index_digest) = index(&[&a, &b]);
        let data = StubTransportData::new();
        serve(&data, &index_digest, index_bytes);
        serve(&data, &a, index(&[]).0);
        confirm_all_gone(&data, 1);
        let client = stub_client(&data);
        let keep = format!("__ocx.keep.{}", fallback(&a));
        let versions = vec![
            version(1, &index_digest, &["old"]),
            version(2, &a, &[&keep]),
            version(3, &b, &[]),
        ];
        let fake = Fake::start(route("Organization", versions)).await;

        let mut report = DeleteReport::default();
        deleter(&client, &fake.base)
            .delete(&plan(&["old"], false), &mut report)
            .await
            .expect("deleted");

        assert_eq!(fake.deletes().len(), 3);
    }

    #[tokio::test]
    async fn a_version_also_tagged_durable_refuses_the_run_and_deletes_nothing() {
        let graph = graph(&["old", "latest"], None);
        let client = stub_client(&graph.data);
        let fake = Fake::start(route("Organization", graph.versions.clone())).await;

        let mut report = DeleteReport::default();
        let error = deleter(&client, &fake.base)
            .delete(&plan(&["old"], false), &mut report)
            .await
            .expect_err("shared version");

        match error {
            PruneError::SharedVersion { tag, other_tag, .. } => {
                assert_eq!((tag.as_str(), other_tag.as_str()), ("old", "latest"));
            }
            other => panic!("expected SharedVersion, got {other:?}"),
        }
        assert!(fake.deletes().is_empty(), "{:?}", fake.requests());
        assert!(report.deleted.is_empty());
    }

    #[tokio::test]
    async fn a_platform_version_carrying_a_foreign_tag_refuses_the_run() {
        let graph = graph(&["old"], Some("pinned"));
        let client = stub_client(&graph.data);
        let fake = Fake::start(route("Organization", graph.versions.clone())).await;

        let mut report = DeleteReport::default();
        let error = deleter(&client, &fake.base)
            .delete(&plan(&["old"], false), &mut report)
            .await
            .expect_err("shared version");

        assert!(
            matches!(&error, PruneError::SharedVersion { other_tag, .. } if other_tag == "pinned"),
            "{error:?}"
        );
        assert!(fake.deletes().is_empty());
    }

    #[tokio::test]
    async fn a_dry_run_lists_the_tag_and_deletes_nothing() {
        let graph = graph(&["old"], None);
        let client = stub_client(&graph.data);
        let fake = Fake::start(route("Organization", graph.versions.clone())).await;

        let mut report = DeleteReport::default();
        deleter(&client, &fake.base)
            .delete(&plan(&["old"], true), &mut report)
            .await
            .expect("dry run");

        assert_eq!(report.would_delete, ["old"]);
        assert!(report.deleted.is_empty());
        assert!(fake.deletes().is_empty(), "{:?}", fake.requests());
    }

    #[tokio::test]
    async fn a_dry_run_still_refuses_a_shared_version() {
        let graph = graph(&["old", "latest"], None);
        let client = stub_client(&graph.data);
        let fake = Fake::start(route("Organization", graph.versions.clone())).await;

        let mut report = DeleteReport::default();
        let error = deleter(&client, &fake.base)
            .delete(&plan(&["old"], true), &mut report)
            .await
            .expect_err("shared version");

        assert!(matches!(error, PruneError::SharedVersion { .. }), "{error:?}");
    }

    #[tokio::test]
    async fn a_mid_run_denial_names_the_tag_being_deleted_and_states_the_cause_once() {
        let (a_bytes, a_digest) = index(&[]);
        let (b_bytes, b_digest) = index(&[&a_digest]);
        let data = StubTransportData::new();
        serve(&data, &a_digest, a_bytes);
        serve(&data, &b_digest, b_bytes);
        confirm_all_gone(&data, 1);
        let client = stub_client(&data);
        let versions = vec![version(7, &a_digest, &["first"]), version(8, &b_digest, &["second"])];
        let list = route("Organization", versions);
        let fake = Fake::start(move |method, target| {
            if method == "DELETE" && target.ends_with("/versions/8") {
                return (403, r#"{"message":"no"}"#.to_string());
            }
            list(method, target)
        })
        .await;

        let mut report = DeleteReport::default();
        let error = deleter(&client, &fake.base)
            .delete(&plan(&["first", "second"], false), &mut report)
            .await
            .expect_err("the second delete is denied");

        let PruneError::DeleteDenied { tag, .. } = &error else {
            panic!("{error:?}");
        };
        assert_eq!(tag, "second", "the first tag was already deleted");
        let mut chain = Vec::new();
        let mut cause: Option<&dyn std::error::Error> = std::error::Error::source(&error);
        while let Some(c) = cause {
            chain.push(c.to_string());
            cause = c.source();
        }
        let joined = chain.join(" | ");
        assert_eq!(joined.matches("answered 403").count(), 1, "{joined}");
    }

    #[tokio::test]
    async fn a_credential_the_forge_refuses_is_delete_denied() {
        for status in [401u16, 403] {
            let data = StubTransportData::new();
            let client = stub_client(&data);
            let fake = Fake::start(move |_, _| (status, r#"{"message":"no"}"#.to_string())).await;

            let mut report = DeleteReport::default();
            let error = deleter(&client, &fake.base)
                .delete(&plan(&["old"], false), &mut report)
                .await
                .expect_err("denied");

            assert!(matches!(error, PruneError::DeleteDenied { .. }), "{status}: {error:?}");
        }
    }

    #[tokio::test]
    async fn a_redirect_is_an_error_and_is_never_followed() {
        let data = StubTransportData::new();
        let client = stub_client(&data);
        let fake = Fake::start(|_, _| (302, String::new())).await;

        let mut report = DeleteReport::default();
        let error = deleter(&client, &fake.base)
            .delete(&plan(&["old"], false), &mut report)
            .await
            .expect_err("a redirect fails");

        assert!(matches!(error, PruneError::Registry(_)), "{error:?}");
        assert_eq!(fake.requests().len(), 1);
    }

    #[tokio::test]
    async fn a_tag_no_version_carries_is_reported_absent() {
        let (index_bytes, index_digest) = index(&[]);
        let data = StubTransportData::new();
        serve(&data, &index_digest, index_bytes);
        confirm_all_gone(&data, 1);
        let client = stub_client(&data);
        let fake = Fake::start(route("Organization", vec![version(7, &index_digest, &["other"])])).await;

        let mut report = DeleteReport::default();
        deleter(&client, &fake.base)
            .delete(&plan(&["old"], false), &mut report)
            .await
            .expect("absent");

        assert_eq!(report.absent, ["old"]);
        assert!(fake.deletes().is_empty());
    }

    #[tokio::test]
    async fn a_root_another_tagged_version_lists_is_refused_as_shared() {
        let a = digest("a");
        let (root_bytes, root_digest) = index(&[&a]);
        let (durable_bytes, durable_digest) = index(&[&root_digest]);
        let data = StubTransportData::new();
        serve(&data, &root_digest, root_bytes);
        serve(&data, &durable_digest, durable_bytes);
        let client = stub_client(&data);
        let versions = vec![
            version(1, &root_digest, &["old"]),
            version(2, &a, &[]),
            version(3, &durable_digest, &["1.0.0"]),
        ];
        let fake = Fake::start(route("Organization", versions)).await;

        let mut report = DeleteReport::default();
        let error = deleter(&client, &fake.base)
            .delete(&plan(&["old"], false), &mut report)
            .await
            .expect_err("a root the durable index lists is shared");

        match error {
            PruneError::SharedVersion { tag, other_tag, .. } => {
                assert_eq!((tag.as_str(), other_tag.as_str()), ("old", "1.0.0"));
            }
            other => panic!("expected SharedVersion, got {other:?}"),
        }
        assert!(fake.deletes().is_empty(), "{:?}", fake.requests());
    }

    #[tokio::test]
    async fn a_tag_whose_version_is_deleted_stays_deleted_when_a_later_delete_fails() {
        let graph = graph(&["old"], None);
        confirm_all_gone(&graph.data, 1);
        let client = stub_client(&graph.data);
        let inner = route("Organization", graph.versions.clone());
        let failing = format!("/orgs/{OWNER}/packages/container/{PACKAGE}/versions/2");
        let fake = Fake::start(move |method, target| {
            if method == "DELETE" && target == failing {
                return (500, r#"{"message":"boom"}"#.to_string());
            }
            inner(method, target)
        })
        .await;

        let mut report = DeleteReport::default();
        let error = deleter(&client, &fake.base)
            .delete(&plan(&["old"], false), &mut report)
            .await
            .expect_err("the platform version delete fails");

        assert!(matches!(error, PruneError::Registry(_)), "{error:?}");
        assert_eq!(report.deleted, ["old"], "the root was deleted before the failure");
    }

    #[tokio::test]
    async fn referrers_of_referrers_go_with_the_set() {
        let (a, attestation, signature) = (digest("a"), digest("attestation"), digest("signature"));
        let (root_bytes, root_digest) = index(&[&a]);
        let (first_bytes, first_digest) = index(&[&attestation]);
        let (second_bytes, second_digest) = index(&[&signature]);
        let data = StubTransportData::new();
        serve(&data, &root_digest, root_bytes);
        serve(&data, &first_digest, first_bytes);
        serve(&data, &second_digest, second_bytes);
        confirm_all_gone(&data, 1);
        let client = stub_client(&data);
        let (first_tag, second_tag) = (fallback(&root_digest), fallback(&attestation));
        let versions = vec![
            version(1, &root_digest, &["old"]),
            version(2, &a, &[]),
            version(3, &first_digest, &[&first_tag]),
            version(4, &attestation, &[]),
            version(5, &second_digest, &[&second_tag]),
            version(6, &signature, &[]),
        ];
        let fake = Fake::start(route("Organization", versions)).await;

        let mut report = DeleteReport::default();
        deleter(&client, &fake.base)
            .delete(&plan(&["old"], false), &mut report)
            .await
            .expect("deleted");

        let mut deleted = fake.deletes();
        deleted.sort();
        let expected: Vec<String> = (1..=6)
            .map(|id| format!("DELETE /orgs/{OWNER}/packages/container/{PACKAGE}/versions/{id}"))
            .collect();
        assert_eq!(deleted, expected);
    }

    #[tokio::test]
    async fn a_404_on_the_owner_or_the_listing_is_delete_denied() {
        for listing_only in [false, true] {
            let data = StubTransportData::new();
            let client = stub_client(&data);
            let fake = Fake::start(move |_, target| {
                if listing_only && target == format!("/users/{OWNER}") {
                    return (200, r#"{"type":"Organization"}"#.to_string());
                }
                (404, r#"{"message":"Not Found"}"#.to_string())
            })
            .await;

            let mut report = DeleteReport::default();
            let error = deleter(&client, &fake.base)
                .delete(&plan(&["old"], false), &mut report)
                .await
                .expect_err("not found");

            assert!(
                matches!(&error, PruneError::DeleteDenied { .. }),
                "{listing_only}: {error:?}"
            );
            let cause = std::iter::successors(std::error::Error::source(&error), |e| e.source())
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" | ");
            assert!(cause.contains("lacks package read access"), "{listing_only}: {cause}");
        }
    }

    #[tokio::test]
    async fn a_spent_rate_limit_is_a_transient_failure_not_a_denied_credential() {
        for (status, body) in [
            (429u16, r#"{"message":"slow down"}"#),
            (403, r#"{"message":"API rate limit exceeded"}"#),
            (403, r#"{"message":"You have exceeded a secondary rate limit"}"#),
        ] {
            let data = StubTransportData::new();
            let client = stub_client(&data);
            let fake = Fake::start(move |_, _| (status, body.to_string())).await;

            let mut report = DeleteReport::default();
            let error = deleter(&client, &fake.base)
                .delete(&plan(&["old"], false), &mut report)
                .await
                .expect_err("rate limited");

            assert!(
                matches!(
                    &error,
                    PruneError::Registry(ocx_oci::client::error::ClientError::RegistryTransient(_))
                ),
                "{status}: {error:?}"
            );
        }
    }

    fn is_transient(error: &PruneError) -> bool {
        matches!(
            error,
            PruneError::Registry(ocx_oci::client::error::ClientError::RegistryTransient(_))
        )
    }

    #[tokio::test]
    async fn a_kept_version_whose_manifest_is_not_served_stops_the_run_before_any_delete() {
        let (root_bytes, root_digest) = index(&[]);
        let data = StubTransportData::new();
        serve(&data, &root_digest, root_bytes);
        let client = stub_client(&data);
        let versions = vec![
            version(1, &root_digest, &["old"]),
            version(2, &digest("durable"), &["1.0.0"]),
        ];
        let fake = Fake::start(route("Organization", versions)).await;

        let mut report = DeleteReport::default();
        let error = deleter(&client, &fake.base)
            .delete(&plan(&["old"], false), &mut report)
            .await
            .expect_err("a kept version nobody can read protects nothing");

        assert!(is_transient(&error), "{error:?}");
        assert!(fake.deletes().is_empty(), "{:?}", fake.requests());
    }

    #[tokio::test]
    async fn the_manifests_of_kept_versions_are_read_at_most_eight_at_a_time() {
        const KEPT: u64 = 9;
        const DELAY: Duration = Duration::from_millis(300);
        let (root_bytes, root_digest) = index(&[]);
        let data = StubTransportData::new();
        serve(&data, &root_digest, root_bytes);
        let mut versions = vec![version(1, &root_digest, &["old"])];
        for id in 0..KEPT {
            let (bytes, kept) = index(&[&digest(&format!("child-{id}"))]);
            serve(&data, &kept, bytes);
            let key = repository()
                .clone_with_digest(Digest::try_from(kept.as_str()).expect("a digest"))
                .canonical_reference()
                .to_string();
            data.write().manifest_delays.insert(key, DELAY);
            versions.push(version(10 + id, &kept, &[&format!("1.0.{id}")]));
        }
        let client = stub_client(&data);
        let fake = Fake::start(route("Organization", versions)).await;

        let started = std::time::Instant::now();
        deleter(&client, &fake.base)
            .delete(&plan(&["old"], true), &mut DeleteReport::default())
            .await
            .expect("dry run");
        let elapsed = started.elapsed();

        // One at a time takes KEPT delays; eight at a time takes two; unbounded takes one.
        assert!(elapsed < DELAY * KEPT as u32, "read one after another: {elapsed:?}");
        assert!(elapsed >= DELAY * 2, "more than eight in flight: {elapsed:?}");
    }

    #[tokio::test]
    async fn manifests_are_read_accepting_the_docker_manifest_list() {
        let graph = graph(&["old"], None);
        confirm_all_gone(&graph.data, 1);
        let client = stub_client(&graph.data);
        let fake = Fake::start(route("Organization", graph.versions.clone())).await;

        deleter(&client, &fake.base)
            .delete(&plan(&["old"], true), &mut DeleteReport::default())
            .await
            .expect("dry run");

        let accepted = graph.data.read().accepted_media_types.clone();
        assert!(!accepted.is_empty());
        assert!(
            accepted.iter().all(|types| types
                .iter()
                .any(|t| t == "application/vnd.docker.distribution.manifest.list.v2+json")),
            "{accepted:?}"
        );
    }

    #[tokio::test]
    async fn a_listed_version_without_an_id_or_name_fails_the_run_before_any_delete() {
        let (index_bytes, index_digest) = index(&[]);
        let data = StubTransportData::new();
        serve(&data, &index_digest, index_bytes);
        let client = stub_client(&data);
        let versions = vec![version(7, &index_digest, &["old"]), serde_json::json!({"id": 9})];
        let fake = Fake::start(route("Organization", versions)).await;

        let mut report = DeleteReport::default();
        let error = deleter(&client, &fake.base)
            .delete(&plan(&["old"], false), &mut report)
            .await
            .expect_err("an unreadable listing entry");

        assert!(
            matches!(
                &error,
                PruneError::Registry(ocx_oci::client::error::ClientError::Registry(_))
            ),
            "{error:?}"
        );
        assert!(fake.deletes().is_empty());
    }

    /// Serves `before` for the first versions listing and `after` for every later one.
    fn relisted(
        before: Vec<serde_json::Value>,
        after: Vec<serde_json::Value>,
    ) -> impl Fn(&str, &str) -> (u16, String) + Send + Sync + 'static {
        let (first, later) = (route("Organization", before), route("Organization", after));
        let listings = std::sync::atomic::AtomicUsize::new(0);
        move |method, target| {
            if method == "GET" && target.contains("/versions?") {
                let seen = listings.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                return if seen == 0 {
                    first(method, target)
                } else {
                    later(method, target)
                };
            }
            first(method, target)
        }
    }

    #[tokio::test]
    async fn a_listing_that_changes_before_the_first_delete_aborts_with_nothing_deleted() {
        let (index_bytes, index_digest) = index(&[]);
        let data = StubTransportData::new();
        serve(&data, &index_digest, index_bytes);
        let client = stub_client(&data);
        let before = vec![version(7, &index_digest, &["old"])];
        let gone = vec![];
        let retagged = vec![version(7, &index_digest, &["old", "1.0.0"])];
        let swapped = vec![version(7, &index_digest, &[]), version(8, &digest("new"), &["old"])];
        for after in [gone, retagged, swapped] {
            let fake = Fake::start(relisted(before.clone(), after.clone())).await;
            let mut report = DeleteReport::default();
            let error = deleter(&client, &fake.base)
                .delete(&plan(&["old"], false), &mut report)
                .await
                .expect_err("the package changed");

            assert!(is_transient(&error), "{after:?}: {error:?}");
            assert!(fake.deletes().is_empty(), "{after:?}: {:?}", fake.requests());
            assert!(report.deleted.is_empty());
        }
    }

    #[tokio::test]
    async fn a_tagged_version_that_appears_after_planning_aborts_with_nothing_deleted() {
        let (a, b) = (digest("a"), digest("b"));
        let (root_bytes, root_digest) = index(&[&a]);
        let (newer_bytes, newer_digest) = index(&[&a, &b]);
        let data = StubTransportData::new();
        serve(&data, &root_digest, root_bytes);
        serve(&data, &newer_digest, newer_bytes);
        serve(&data, &a, index(&[]).0);
        let client = stub_client(&data);
        let before = vec![version(1, &root_digest, &["old"]), version(2, &a, &[])];
        let mut after = before.clone();
        after.push(version(9, &newer_digest, &["2.0"]));
        let fake = Fake::start(relisted(before, after)).await;

        let mut report = DeleteReport::default();
        let error = deleter(&client, &fake.base)
            .delete(&plan(&["old"], false), &mut report)
            .await
            .expect_err("a new tagged version may share a member");

        assert!(is_transient(&error), "{error:?}");
        assert!(fake.deletes().is_empty(), "{:?}", fake.requests());
    }

    #[tokio::test]
    async fn an_unchanged_relisting_still_deletes() {
        let (index_bytes, index_digest) = index(&[]);
        let data = StubTransportData::new();
        serve(&data, &index_digest, index_bytes);
        confirm_all_gone(&data, 1);
        let client = stub_client(&data);
        let listing = vec![version(7, &index_digest, &["old"])];
        let fake = Fake::start(relisted(listing.clone(), listing)).await;

        let mut report = DeleteReport::default();
        deleter(&client, &fake.base)
            .delete(&plan(&["old"], false), &mut report)
            .await
            .expect("deleted");

        assert_eq!(report.deleted, ["old"]);
    }

    #[tokio::test]
    async fn an_index_row_naming_another_build_than_the_tag_carries_refuses_before_any_delete() {
        let (index_bytes, index_digest) = index(&[]);
        let data = StubTransportData::new();
        serve(&data, &index_digest, index_bytes);
        let client = stub_client(&data);
        let fake = Fake::start(route("Organization", vec![version(7, &index_digest, &["old"])])).await;
        let row = Digest::try_from(digest("the row").as_str()).expect("a digest");
        let mut stale = plan(&["old"], false);
        stale.indexed = vec![("old".to_owned(), row)];

        let mut report = DeleteReport::default();
        let error = deleter(&client, &fake.base)
            .delete(&stale, &mut report)
            .await
            .expect_err("a stale row");

        match error {
            PruneError::IndexStale { tag, served, .. } => {
                assert_eq!((tag.as_str(), served.as_str()), ("old", index_digest.as_str()));
            }
            other => panic!("expected IndexStale, got {other:?}"),
        }
        assert!(fake.deletes().is_empty(), "{:?}", fake.requests());
    }

    #[tokio::test]
    async fn an_index_row_naming_the_served_build_deletes() {
        let (index_bytes, index_digest) = index(&[]);
        let data = StubTransportData::new();
        serve(&data, &index_digest, index_bytes);
        confirm_all_gone(&data, 1);
        let client = stub_client(&data);
        let fake = Fake::start(route("Organization", vec![version(7, &index_digest, &["old"])])).await;
        let mut matching = plan(&["old"], false);
        matching.indexed = vec![(
            "old".to_owned(),
            Digest::try_from(index_digest.as_str()).expect("a digest"),
        )];

        let mut report = DeleteReport::default();
        deleter(&client, &fake.base)
            .delete(&matching, &mut report)
            .await
            .expect("deleted");

        assert_eq!(report.deleted, ["old"]);
    }

    #[tokio::test]
    async fn cosign_signature_and_attestation_tags_of_every_member_go_with_the_set() {
        let (a, other) = (digest("a"), digest("other"));
        let (root_bytes, root_digest) = index(&[&a]);
        let (signature_bytes, signature_digest) = index(&[&other]);
        let (attestation_bytes, attestation_digest) = index(&[&digest("attested")]);
        let data = StubTransportData::new();
        serve(&data, &root_digest, root_bytes);
        serve(&data, &signature_digest, signature_bytes);
        serve(&data, &attestation_digest, attestation_bytes);
        confirm_all_gone(&data, 1);
        let client = stub_client(&data);
        let versions = vec![
            version(1, &root_digest, &["old"]),
            version(2, &a, &[]),
            version(3, &signature_digest, &[&format!("{}.sig", fallback(&root_digest))]),
            version(4, &attestation_digest, &[&format!("{}.att", fallback(&a))]),
        ];
        let fake = Fake::start(route("Organization", versions)).await;

        let mut report = DeleteReport::default();
        deleter(&client, &fake.base)
            .delete(&plan(&["old"], false), &mut report)
            .await
            .expect("deleted");

        let mut deleted = fake.deletes();
        deleted.sort();
        let expected: Vec<String> = (1..=4)
            .map(|id| format!("DELETE /orgs/{OWNER}/packages/container/{PACKAGE}/versions/{id}"))
            .collect();
        assert_eq!(deleted, expected);
    }

    #[test]
    fn ghcr_maps_to_the_public_api() {
        assert_eq!(
            api_base_for_registry("ghcr.io").as_deref(),
            Some("https://api.github.com")
        );
        assert_eq!(forge_for_registry("GHCR.io"), Some(ForgeKind::GitHub));
    }

    #[test]
    fn only_ghcr_implies_the_github_forge() {
        // A registry host can arrive from an index document; inferring a forge from it would send the
        // registry credential to a host another party chose.
        assert_eq!(forge_for_registry("containers.git.acme.example"), None);
        assert_eq!(forge_for_registry("localhost:5000"), None);
    }

    #[test]
    fn containers_host_maps_to_enterprise_api() {
        assert_eq!(
            api_base_for_registry("containers.git.acme.example").as_deref(),
            Some("https://git.acme.example/api/v3")
        );
    }

    #[test]
    fn other_hosts_map_to_nothing() {
        for host in [
            "localhost:5000",
            "containers.",
            "docker.io",
            "ghcr.io.evil.example",
            "é",
        ] {
            assert_eq!(api_base_for_registry(host), None, "{host}");
            assert_eq!(forge_for_registry(host), None, "{host}");
        }
    }
}
