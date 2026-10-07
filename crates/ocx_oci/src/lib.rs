// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! OCI/distribution-spec-generic registry work: references, digests, manifests, transport, referrers, layer-placement annotations, SSRF guard, registry auth.
//!
//! Two compile-time contracts, each pinned by a `compile_fail` example below:
//! [`OciTransport`](client::OciTransport) is sealed, and a [`PackageRef`] never
//! converts to or from an [`OciIdentifier`] (`adr_index_indirection.md` § Layer 2).
//! Each refusal is complete or has a compiling twin, or it keeps failing to compile
//! for an unrelated reason and the doctest stays green after the contract breaks.
//!
//! # Examples
//!
//! A complete transport impl is refused by the seal alone:
//!
//! ```compile_fail,E0277
//! use std::path::Path;
//! use ocx_oci::client::{DeleteOutcome, ManifestPresence, OciTransport, ProgressFn};
//! use ocx_oci::client::error::ClientError;
//! use ocx_oci::{Descriptor, Digest, Manifest, Reference, RegistryOperation};
//!
//! type Answer<T> = std::result::Result<T, ClientError>;
//!
//! struct MyTransport;
//!
//! #[async_trait::async_trait]
//! impl OciTransport for MyTransport {
//!     async fn ensure_auth(&self, _: &Reference, _: RegistryOperation) -> Answer<()> { todo!() }
//!     async fn list_tags(&self, _: &Reference, _: usize, _: Option<String>) -> Answer<Vec<String>> { todo!() }
//!     async fn catalog(&self, _: &Reference, _: usize, _: Option<String>) -> Answer<Vec<String>> { todo!() }
//!     async fn fetch_manifest_digest(&self, _: &Reference) -> Answer<String> { todo!() }
//!     async fn pull_manifest_raw(&self, _: &Reference, _: &[&str]) -> Answer<(Vec<u8>, String)> { todo!() }
//!     async fn pull_blob(&self, _: &Reference, _: &Digest) -> Answer<Vec<u8>> { todo!() }
//!     async fn pull_blob_to_file(&self, _: &Reference, _: &Digest, _: &Path) -> Answer<()> { todo!() }
//!     async fn head_blob(&self, _: &Reference, _: &Digest) -> Answer<u64> { todo!() }
//!     async fn push_manifest(&self, _: &Reference, _: &Manifest) -> Answer<String> { todo!() }
//!     async fn push_manifest_raw(&self, _: &Reference, _: Vec<u8>, _: &str) -> Answer<String> { todo!() }
//!     async fn push_blob(&self, _: &Reference, _: Vec<u8>, _: &Digest, _: ProgressFn) -> Answer<String> { todo!() }
//!     async fn push_blob_from_path(&self, _: &Reference, _: &Path, _: &Digest, _: ProgressFn) -> Answer<String> { todo!() }
//!     async fn delete_manifest(&self, _: &Reference) -> Answer<DeleteOutcome> { todo!() }
//!     async fn probe_manifest(&self, _: &Reference) -> Answer<ManifestPresence> { todo!() }
//!     async fn push_referrer_manifest(&self, _: &Reference, _: &Digest, _: &[u8], _: &str) -> Answer<Descriptor> { todo!() }
//!     async fn list_referrers(&self, _: &Reference, _: &Digest, _: Option<&str>) -> Answer<Vec<Descriptor>> { todo!() }
//!     fn box_clone(&self) -> Box<dyn OciTransport> { todo!() }
//! }
//! ```
//!
//! A package identifier handed to the client is a type mismatch:
//!
//! ```compile_fail,E0308
//! async fn read(client: ocx_oci::Client, location: ocx_oci::OciIdentifier, identifier: ocx_oci::PackageRef) {
//!     let _ = client.fetch_manifest(&identifier).await;
//! }
//! ```
//!
//! ```
//! async fn read(client: ocx_oci::Client, location: ocx_oci::OciIdentifier, identifier: ocx_oci::PackageRef) {
//!     let _ = client.fetch_manifest(&location).await;
//! }
//! ```
//!
//! A location does not turn back into a package identifier:
//!
//! ```compile_fail,E0277
//! fn relabel(location: ocx_oci::OciIdentifier, identifier: ocx_oci::PackageRef) {
//!     let _: ocx_oci::PackageRef = location.into();
//! }
//! ```
//!
//! ```
//! fn relabel(location: ocx_oci::OciIdentifier, identifier: ocx_oci::PackageRef) {
//!     let _: ocx_oci::PackageRef = identifier.into();
//! }
//! ```
//!
//! Nor does a package identifier become a location, by `into` or `from`:
//!
//! ```compile_fail,E0277
//! fn unroute(location: ocx_oci::OciIdentifier, identifier: ocx_oci::PackageRef) {
//!     let _: ocx_oci::OciIdentifier = identifier.into();
//! }
//! ```
//!
//! ```
//! fn unroute(location: ocx_oci::OciIdentifier, identifier: ocx_oci::PackageRef) {
//!     let _: ocx_oci::OciIdentifier = location.into();
//! }
//! ```
//!
//! ```compile_fail,E0277
//! fn unroute(identifier: ocx_oci::PackageRef) {
//!     let _ = ocx_oci::OciIdentifier::from(&identifier);
//! }
//! ```
//!
//! ```
//! fn unroute(identifier: ocx_oci::PackageRef) {
//!     let _ = ocx_oci::OciIdentifier::passthrough(&identifier);
//! }
//! ```

/// The seal on [`client::OciTransport`]: a private module, so no crate outside
/// this one can implement the transport and skip its guarded defaults.
mod sealed {
    pub trait Sealed {}
}

/// Common type aliases of the external OCI related libraries.
pub mod native {
    pub use oci_client;

    pub use oci_client::client::Client;
    pub use oci_client::client::ClientConfig;
    pub use oci_client::client::ClientProtocol;

    pub use oci_client::Reference;
    pub use oci_client::manifest::Platform;

    pub use oci_client::config::Architecture as Arch;
    pub use oci_client::config::Os;

    pub use oci_client::manifest::ImageIndexEntry;
    pub use oci_client::manifest::OciDescriptor;
    pub use oci_client::manifest::OciImageIndex as ImageIndex;
    pub use oci_client::manifest::OciImageManifest as ImageManifest;
    pub use oci_client::manifest::OciManifest as Manifest;

    pub use oci_client::secrets::RegistryAuth as Auth;

    pub use docker_credential;
    pub use docker_credential::CredentialRetrievalError as DockerCredentialRetrievalError;
    pub use docker_credential::DockerCredential;
    pub use docker_credential::detect_default_helper as detect_default_docker_helper;
    pub use docker_credential::erase_credential as erase_docker_credential;
    pub use docker_credential::get_credential as get_docker_credential;
    pub use docker_credential::list_credentials as list_docker_credentials;
    pub use docker_credential::store_credential as store_docker_credential;
}

pub use oci_client::{
    Reference, RegistryOperation,
    manifest::{
        ImageIndexEntry, OCI_IMAGE_INDEX_MEDIA_TYPE, OCI_IMAGE_MEDIA_TYPE, OciDescriptor as Descriptor,
        OciImageIndex as ImageIndex, OciImageManifest as ImageManifest, OciManifest as Manifest,
    },
};

pub const INDEX_SCHEMA_VERSION: u8 = 2;

pub mod annotations;

pub mod auth;

pub mod media_type;

pub mod tag;

pub mod layer_layout;
pub use layer_layout::{LayerLayoutError, LayerLayoutSpec, resolve_layer_placement};

pub mod layer_ref;
pub use layer_ref::{ArchiveMediaType, LayerRef, LayerRefParseError};

pub mod client;
pub mod copy;
pub use client::Client;
pub use client::ClientBuilder;
pub use client::LayerCounts;
pub use client::MirrorMap;

pub mod ssrf;

pub mod transport_policy;

pub mod manifest;
pub mod manifest_builder;
pub use manifest_builder::{ManifestArtifacts, ManifestBuilder};

pub mod referrer;

pub mod resolve_target;

// A peer of `sign`/`verify`, not part of either (`adr_oci_referrers_signing_v1.md` Amendment 2).
pub mod endpoint;

pub mod package_ref;
pub use package_ref::DEFAULT_REGISTRY;
pub use package_ref::OCX_SH_REGISTRY;
pub use package_ref::PackageRef;
pub use package_ref::error::{IdentifierError, IdentifierErrorKind};
pub use package_ref::ocx_cli_identifier;

pub mod host_capabilities;
pub use host_capabilities::{Feature, HostCapabilities, LibcFlavor, cached_libc_labels};

pub mod platform;
pub use platform::Architecture;
pub use platform::OperatingSystem;
pub use platform::Platform;
pub use platform::{Selection, compatibility_score, is_compatible, render_native_platform, select_best};

pub mod digest;
pub use digest::Algorithm;
pub use digest::Digest;

pub mod pinned_package_ref;
pub use pinned_package_ref::PinnedPackageRef;

pub mod oci_identifier;
pub use oci_identifier::{OciIdentifier, PinnedOciIdentifier};

pub mod repository;
pub use repository::Repository;

pub mod redacted_url;
pub use redacted_url::RedactedUrl;

pub mod registry_host;
pub use registry_host::{RegistryHost, RegistryHostError};

mod file_storage;
pub use file_storage::FileStorage;

#[cfg(any(test, feature = "__testing"))]
pub mod testing;
