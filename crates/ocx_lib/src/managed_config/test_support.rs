// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Test-only builders for managed-config package fixtures (v2 wire shape).
//!
//! Shared by `managed_config::persistence` and
//! `package_manager::tasks::managed_config` unit tests so the package shape
//! (index → any/any manifest → tar+gzip layer) is constructed in exactly one
//! place.

use crate::oci::client::test_transport::{StubTransport, StubTransportData};
use crate::oci::{Algorithm, Client, Identifier};

/// Builds a gzip'd tar archive from `(name, bytes)` entries.
pub(crate) fn gzip_tar(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    for (name, bytes) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, name, *bytes).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
}

/// Seeds `stub_data` with an ordinary managed-config package under
/// `identifier`: image index (`platform` os/arch pair) → image manifest →
/// gzip'd tar layer. Returns the index digest (the drift identity).
pub(crate) fn seed_package(
    stub_data: &StubTransportData,
    identifier: &Identifier,
    layer: Vec<u8>,
    platform_os_arch: (&str, &str),
    declared_size_override: Option<i64>,
    layer_media_type: &str,
) -> String {
    let layer_digest = Algorithm::Sha256.hash(&layer).to_string();
    let manifest_json = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "digest": format!("sha256:{}", "0".repeat(64)),
            "size": 2
        },
        "layers": [{
            "mediaType": layer_media_type,
            "digest": layer_digest,
            "size": declared_size_override.unwrap_or(layer.len() as i64),
        }],
    })
    .to_string();
    let manifest_bytes = manifest_json.into_bytes();
    let manifest_digest = Algorithm::Sha256.hash(&manifest_bytes);

    let index_json = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.index.v1+json",
        "manifests": [{
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "digest": manifest_digest.to_string(),
            "size": manifest_bytes.len(),
            "platform": { "os": platform_os_arch.0, "architecture": platform_os_arch.1 },
        }],
    })
    .to_string();
    let index_bytes = index_json.into_bytes();
    let index_digest = Algorithm::Sha256.hash(&index_bytes).to_string();

    let manifest_digest_string = manifest_digest.to_string();
    let child_identifier = identifier.without_specifiers().clone_with_digest(manifest_digest);
    let mut inner = stub_data.write();
    inner
        .manifests
        .insert(identifier.to_string(), (index_bytes, index_digest.clone()));
    inner
        .manifests
        .insert(child_identifier.to_string(), (manifest_bytes, manifest_digest_string));
    inner.blobs.insert(layer_digest, layer);
    index_digest
}

/// Builds an image manifest wrapping a single `layer` (tar+gzip media type)
/// and inserts the layer blob into `stub_data`, returning the manifest bytes.
/// The index-entry building block shared by [`seed_package_multi_platform`];
/// the caller hashes the returned bytes to register the child manifest and the
/// index entry that points at it.
fn build_child_manifest(stub_data: &StubTransportData, layer: Vec<u8>) -> Vec<u8> {
    let layer_digest = Algorithm::Sha256.hash(&layer).to_string();
    let manifest_json = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "digest": format!("sha256:{}", "0".repeat(64)),
            "size": 2
        },
        "layers": [{
            "mediaType": crate::media_type::MEDIA_TYPE_TAR_GZ,
            "digest": layer_digest.clone(),
            "size": layer.len(),
        }],
    })
    .to_string();
    stub_data.write().blobs.insert(layer_digest, layer);
    manifest_json.into_bytes()
}

/// Seeds `stub_data` with a package whose image index carries MULTIPLE
/// platform entries — one image manifest per `(platform_os_arch, layer)` pair,
/// in the given order. Each child manifest wraps its own tar+gzip layer, so a
/// fetch that selects the wrong entry surfaces a different `config.toml`.
///
/// Used to prove `fetch_managed_config` selects the platform-agnostic
/// `any/any` entry even when a concrete-platform entry precedes it in the
/// index. Returns the index digest.
pub(crate) fn seed_package_multi_platform(
    stub_data: &StubTransportData,
    identifier: &Identifier,
    entries: &[((&str, &str), Vec<u8>)],
) -> String {
    let mut index_entries = Vec::with_capacity(entries.len());
    let mut children = Vec::with_capacity(entries.len());
    for (platform_os_arch, layer) in entries {
        let manifest_bytes = build_child_manifest(stub_data, layer.clone());
        let manifest_digest = Algorithm::Sha256.hash(&manifest_bytes);
        index_entries.push(serde_json::json!({
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "digest": manifest_digest.to_string(),
            "size": manifest_bytes.len(),
            "platform": { "os": platform_os_arch.0, "architecture": platform_os_arch.1 },
        }));
        children.push((manifest_bytes, manifest_digest));
    }

    let index_json = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.index.v1+json",
        "manifests": index_entries,
    })
    .to_string();
    let index_bytes = index_json.into_bytes();
    let index_digest = Algorithm::Sha256.hash(&index_bytes).to_string();

    let mut inner = stub_data.write();
    inner
        .manifests
        .insert(identifier.to_string(), (index_bytes, index_digest.clone()));
    for (manifest_bytes, manifest_digest) in children {
        let child_identifier = identifier
            .without_specifiers()
            .clone_with_digest(manifest_digest.clone());
        inner.manifests.insert(
            child_identifier.to_string(),
            (manifest_bytes, manifest_digest.to_string()),
        );
    }
    index_digest
}

/// Seeds a well-formed package with `config.toml` = `config_toml` under an
/// `any/any` index entry; returns `(client, index_digest)`.
pub(crate) fn stub_client_with_package(identifier: &Identifier, config_toml: &str) -> (Client, String) {
    let stub_data = StubTransportData::new();
    let layer = gzip_tar(&[("config.toml", config_toml.as_bytes())]);
    let digest = seed_package(
        &stub_data,
        identifier,
        layer,
        ("any", "any"),
        None,
        crate::media_type::MEDIA_TYPE_TAR_GZ,
    );
    (Client::with_transport(Box::new(StubTransport::new(stub_data))), digest)
}
