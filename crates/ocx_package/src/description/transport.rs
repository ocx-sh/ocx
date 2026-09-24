// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Reading and writing a [`Description`] as an OCI artifact on the
//! `__ocx.desc` tag.
//!
//! Free functions over an [`ocx_oci::Client`], not methods on it: the wire *shape*
//! of a description — which layer carries the README, which media type the logo
//! declares, which annotations ride the manifest — is the package layer's
//! vocabulary, and the client only supplies blob and manifest primitives.

use crate::error::Error as PackageError;
use std::path::Path;

use super::{Description, Logo};
use ocx_oci::Algorithm;
use ocx_oci::Digest;
use ocx_oci::ManifestBuilder;
use ocx_oci::OciIdentifier;
use ocx_oci::client::ReadAddressing;
use ocx_oci::client::error::ClientError;
use ocx_oci::tag::InternalTag;
use ocx_oci::{
    self, media_type::MEDIA_TYPE_DESCRIPTION_V1, media_type::MEDIA_TYPE_MARKDOWN,
    media_type::MEDIA_TYPE_OCI_EMPTY_CONFIG, media_type::MEDIA_TYPE_OCI_IMAGE_MANIFEST, media_type::MEDIA_TYPE_PNG,
    media_type::MEDIA_TYPE_SVG,
};

/// Pushes a description artifact to the `__ocx.desc` tag.
///
/// Builds an OCI ImageManifest with `artifact_type` set to the description media type,
/// an empty config blob, layers for the README (and optional logo), and manifest-level
/// annotations for catalog metadata (title, description, keywords).
pub async fn push_description(
    client: &ocx_oci::Client,
    identifier: &OciIdentifier,
    description: &Description,
) -> Result<(), PackageError> {
    let desc_identifier = identifier.clone_with_tag(InternalTag::DESCRIPTION_TAG);
    // Push stays canonical (mirror-free): remote/proxy mirrors are read-only —
    // `ensure_auth` routes a `Push` scope to the canonical host for that reason.
    client
        .ensure_auth(&desc_identifier, ocx_oci::RegistryOperation::Push)
        .await?;

    let config_data = b"{}".to_vec();
    let config_digest = Algorithm::Sha256.hash(&config_data);
    client.push_blob(&desc_identifier, config_data, &config_digest).await?;

    let readme_bytes = description.readme.as_bytes();
    let readme_len = readme_bytes.len();
    let readme_digest = Algorithm::Sha256.hash(readme_bytes);
    client
        .push_blob(&desc_identifier, readme_bytes.to_vec(), &readme_digest)
        .await?;

    let readme_size = i64::try_from(readme_len)
        .map_err(|_| ClientError::InvalidManifest(format!("readme blob size {readme_len} exceeds i64::MAX")))?;
    let mut layers = vec![ocx_oci::Descriptor {
        media_type: MEDIA_TYPE_MARKDOWN.to_string(),
        digest: readme_digest.to_string(),
        size: readme_size,
        urls: None,
        artifact_type: None,
        annotations: Some([(ocx_oci::annotations::TITLE.to_string(), "README.md".to_string())].into()),
    }];

    if let Some(logo) = &description.logo {
        let logo_len = logo.data.len();
        let logo_digest = Algorithm::Sha256.hash(&logo.data);
        client
            .push_blob(&desc_identifier, logo.data.clone(), &logo_digest)
            .await?;

        let ext = match logo.media_type {
            MEDIA_TYPE_PNG => "png",
            MEDIA_TYPE_SVG => "svg",
            _ => "bin",
        };
        let logo_size = i64::try_from(logo_len)
            .map_err(|_| ClientError::InvalidManifest(format!("logo blob size {logo_len} exceeds i64::MAX")))?;
        layers.push(ocx_oci::Descriptor {
            media_type: logo.media_type.to_string(),
            digest: logo_digest.to_string(),
            size: logo_size,
            urls: None,
            artifact_type: None,
            annotations: Some([(ocx_oci::annotations::TITLE.to_string(), format!("logo.{ext}"))].into()),
        });
    }

    let mut builder = ManifestBuilder::new()
        .artifact_type(MEDIA_TYPE_DESCRIPTION_V1)
        .config_bytes(MEDIA_TYPE_OCI_EMPTY_CONFIG, b"{}".to_vec())
        .layers(layers);
    if !description.annotations.is_empty() {
        builder = builder.annotations(description.annotations.clone());
    }
    let parts = builder.build()?;
    // Sanity: the empty-config blob digest computed by the builder must
    // match the one we already pushed above.
    debug_assert_eq!(parts.config_digest.to_string(), config_digest.to_string());
    let manifest_data = parts.manifest_bytes;

    // Push to the tag reference directly (not by digest) so the tag is created.
    client
        .push_manifest_raw(&desc_identifier, manifest_data, MEDIA_TYPE_OCI_IMAGE_MANIFEST)
        .await?;

    log::debug!("Pushed description for {}", identifier);
    Ok(())
}

/// Pulls the description artifact from the `__ocx.desc` tag, from the
/// canonical registry.
///
/// Returns `Ok(None)` if no description tag exists for the identifier.
/// Uses a temporary directory to download blobs before reading them into memory.
///
/// Canonical by default because the two commands that copy a description
/// (`package copy --description`, `package description push --from`) and the one
/// that merges into it (`package description push`) all *write back* what this read
/// returns: a mirror's answer applied to the canonical host is a decision
/// about a repository nobody read (invariant 5). A description served by a
/// mirror is `pull_description_addressed` with
/// `ReadAddressing::Mirrored`, asked for by name (both crate-private, so this
/// names them as text rather than as links that would not resolve).
pub async fn pull_description(
    client: &ocx_oci::Client,
    identifier: &OciIdentifier,
    temp_dir: &Path,
) -> std::result::Result<Option<Description>, ClientError> {
    pull_description_addressed(client, identifier, temp_dir, ReadAddressing::Canonical).await
}

/// [`pull_description`] against a caller-chosen host.
///
/// `ReadAddressing::Mirrored` is for a description nothing is written from —
/// a catalog page rendered for a human, an announce observation — see
/// [`ReadAddressing`].
pub async fn pull_description_addressed(
    client: &ocx_oci::Client,
    identifier: &OciIdentifier,
    temp_dir: &Path,
    addressing: ReadAddressing,
) -> std::result::Result<Option<Description>, ClientError> {
    let desc_identifier = identifier.clone_with_tag(InternalTag::DESCRIPTION_TAG);

    let (manifest, _digest) = match client.fetch_artifact_manifest(&desc_identifier, addressing).await {
        Ok(result) => result,
        Err(ClientError::ManifestNotFound(_)) => return Ok(None),
        Err(e) => return Err(e),
    };

    let image_manifest = match manifest {
        ocx_oci::Manifest::Image(m) => m,
        ocx_oci::Manifest::ImageIndex(_) => {
            return Err(ClientError::InvalidManifest(
                "expected image manifest for description, got image index".to_string(),
            ));
        }
    };

    match &image_manifest.artifact_type {
        Some(at) if at == MEDIA_TYPE_DESCRIPTION_V1 => {}
        other => {
            return Err(ClientError::InvalidManifest(format!(
                "expected artifact_type '{}', got '{}'",
                MEDIA_TYPE_DESCRIPTION_V1,
                other.as_deref().unwrap_or("<none>")
            )));
        }
    }

    let mut readme: Option<String> = None;
    let mut logo: Option<Logo> = None;

    for (i, layer) in image_manifest.layers.iter().enumerate() {
        let blob_path = temp_dir.join(format!("layer_{i}"));
        let layer_digest = Digest::try_from(layer.digest.as_str()).map_err(|e| {
            ClientError::InvalidManifest(format!("description layer digest '{}' is malformed: {e}", layer.digest))
        })?;
        client
            .pull_blob_to_file(&desc_identifier, addressing, &layer_digest, &blob_path)
            .await?;

        match layer.media_type.as_str() {
            MEDIA_TYPE_MARKDOWN => {
                let data = tokio::fs::read(&blob_path).await.map_err(|e| ClientError::Io {
                    path: blob_path,
                    source: e,
                })?;
                readme = Some(String::from_utf8(data).map_err(ClientError::InvalidEncoding)?);
            }
            MEDIA_TYPE_PNG | MEDIA_TYPE_SVG => {
                let data = tokio::fs::read(&blob_path).await.map_err(|e| ClientError::Io {
                    path: blob_path,
                    source: e,
                })?;
                logo = Some(Logo {
                    data,
                    media_type: if layer.media_type == MEDIA_TYPE_PNG {
                        MEDIA_TYPE_PNG
                    } else {
                        MEDIA_TYPE_SVG
                    },
                });
            }
            _ => {
                log::debug!("Ignoring unknown description layer media type: {}", layer.media_type);
            }
        }
    }

    let readme =
        readme.ok_or_else(|| ClientError::InvalidManifest("description manifest has no markdown layer".to_string()))?;

    let annotations = image_manifest.annotations.unwrap_or_default();

    Ok(Some(Description {
        readme,
        logo,
        annotations,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocx_oci::client::test_transport::{StubTransportData, mirrored_stub_client, stub_client};

    const UPSTREAM_REGISTRY: &str = "ghcr.io";
    const MIRROR_HOST: &str = "mirror-routing.corp";
    const MIRROR_PREFIX: &str = "ghcr-proxy";
    const REPOSITORY: &str = "owner/tool";

    fn test_identifier(tag: &str) -> OciIdentifier {
        OciIdentifier::from_parts(REPOSITORY, UPSTREAM_REGISTRY).clone_with_tag(tag)
    }

    fn description() -> Description {
        Description {
            readme: "# Test".to_string(),
            logo: None,
            annotations: Default::default(),
        }
    }

    /// The repository a mirrored read must observe:
    /// `<mirror path-prefix>/<upstream repository>`.
    fn mirrored_repository() -> String {
        format!("{MIRROR_PREFIX}/{REPOSITORY}")
    }

    /// `(registry, repository)` the transport was handed for `method`. Panics
    /// when `method` was never called — a missing row must fail, never pass.
    fn read_target(data: &StubTransportData, method: &str) -> (String, String) {
        data.read()
            .read_targets
            .iter()
            .find(|(name, _, _)| *name == method)
            .map(|(_, registry, repository)| (registry.clone(), repository.clone()))
            .unwrap_or_else(|| panic!("transport method '{method}' was never called"))
    }

    /// The description push authenticates with `Push` scope, and does so before
    /// its first transport call.
    ///
    /// A security property, not incidental coverage: without it a standalone
    /// invocation issues anonymous requests and a registry requiring auth
    /// answers 401. Held in `oci/client.rs` while this was `Client::push_description`;
    /// WP-14b moved the orchestration here, so the guard moved with it.
    #[tokio::test]
    async fn push_description_authenticates_with_push() {
        let data = StubTransportData::new();
        data.write().capture_pushes = true;
        let client = stub_client(&data);

        let _ = push_description(&client, &test_identifier("1.0"), &description()).await;

        let inner = data.read();
        assert!(!inner.auth_calls.is_empty(), "push_description must call ensure_auth");
        assert!(
            matches!(inner.auth_calls[0].1, ocx_oci::RegistryOperation::Push),
            "the first ensure_auth must carry Push scope, got {:?}",
            inner.auth_calls[0].1
        );
        // …and it came FIRST. `auth_calls` alone cannot say that: it is its own
        // log, so its first entry is `Push` whether the handshake preceded the
        // blob upload or followed it.
        assert_eq!(
            inner.first_call.as_deref(),
            Some("ensure_auth"),
            "the handshake must precede every transport call, got {:?} first (calls: {:?})",
            inner.first_call,
            inner.calls
        );
        assert!(
            inner.calls.iter().any(|c| c.starts_with("push_blob:")),
            "a blob push must actually have happened, else the ordering above is vacuous; calls: {:?}",
            inner.calls
        );
    }

    /// The description read authenticates with `Pull` scope, exactly once.
    ///
    /// The read half of the pair above; likewise held in `oci/client.rs` before
    /// WP-14b.
    #[tokio::test]
    async fn pull_description_authenticates_with_pull() {
        let data = StubTransportData::new();
        let client = stub_client(&data);
        let dir = tempfile::tempdir().unwrap();

        let _ = pull_description(&client, &test_identifier("1.0"), dir.path()).await;

        let calls = data.read().auth_calls.clone();
        assert_eq!(calls.len(), 1, "pull_description must authenticate exactly once");
        assert!(
            matches!(calls[0].1, ocx_oci::RegistryOperation::Pull),
            "the ensure_auth must carry Pull scope, got {:?}",
            calls[0].1
        );
    }

    /// Asking for `ReadAddressing::Mirrored` by name reaches the mirror.
    ///
    /// The positive control for the default below: same client, same
    /// identifier, same recorded call — naming the host is the only difference.
    #[tokio::test]
    async fn pull_description_routes_through_mirror() {
        let data = StubTransportData::new();
        let client = mirrored_stub_client(&data, UPSTREAM_REGISTRY, MIRROR_HOST, MIRROR_PREFIX);
        let dir = tempfile::tempdir().unwrap();

        let _ =
            pull_description_addressed(&client, &test_identifier("1.0"), dir.path(), ReadAddressing::Mirrored).await;

        assert_eq!(
            read_target(&data, "pull_manifest_raw"),
            (MIRROR_HOST.to_string(), mirrored_repository()),
            "a mirrored description read must reach the mirror host under its path-prefix"
        );
    }

    /// The description read that `package copy --description` and
    /// `package description push --from` write back from goes to the CANONICAL
    /// host, with a mirror configured for it.
    ///
    /// Invariant 5: a mirror's answer applied to the canonical host is a
    /// decision about a repository nobody read. The default is the whole
    /// guarantee — `pull_description` names no addressing, so this is what a
    /// call site written tomorrow gets.
    #[tokio::test]
    async fn pull_description_defaults_to_the_canonical_host() {
        let data = StubTransportData::new();
        let client = mirrored_stub_client(&data, UPSTREAM_REGISTRY, MIRROR_HOST, MIRROR_PREFIX);
        let dir = tempfile::tempdir().unwrap();

        let _ = pull_description(&client, &test_identifier("1.0"), dir.path()).await;

        assert_eq!(
            read_target(&data, "pull_manifest_raw"),
            (UPSTREAM_REGISTRY.to_string(), REPOSITORY.to_string()),
            "a description that will be written back must be read from the upstream host"
        );
    }
}
