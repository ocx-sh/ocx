// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! OCI/distribution-spec-generic registry work: references, digests, manifests, transport, referrers, layer-placement annotations, SSRF guard, registry auth.
//!
//! # [`OciTransport`](client::OciTransport) is sealed
//!
//! Every registry read and write crosses that trait, and the SSRF guard, the
//! retry ladder, the mirror routing and the referrers-fallback write all hang
//! off implementing it correctly — several of the trait's own default methods
//! are load-bearing security behaviour, not conveniences. An outside
//! implementor could silently opt out of all of it while still satisfying
//! `Client::with_transport`, so the trait carries a private supertrait and only
//! this crate can implement it. Consumers that need a double take one from
//! `testing` instead — `__testing`-gated, so a release build has no module
//! here to link to.
//!
//! The doctest below is a **complete** implementation — every required method
//! is there — so the only thing that refuses it is the supertrait. Drop the
//! seal and it compiles, and this test goes red.
//!
//! ```compile_fail,E0277
//! use std::path::Path;
//! use ocx_oci::client::{OciTransport, ProgressFn};
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
//!     async fn push_referrer_manifest(&self, _: &Reference, _: &Digest, _: &[u8], _: &str) -> Answer<Descriptor> { todo!() }
//!     async fn list_referrers(&self, _: &Reference, _: &Digest, _: Option<&str>) -> Answer<Vec<Descriptor>> { todo!() }
//!     fn box_clone(&self) -> Box<dyn OciTransport> { todo!() }
//! }
//! ```
//!
//! # A package identifier cannot be dialled
//!
//! [`PackageRef`] names a package; [`OciIdentifier`] names the registry
//! location a request goes to. An index may serve the first from somewhere
//! else entirely, so dialling it as-is reaches whatever host shares its
//! spelling (ocx#504). Every [`Client`] read and write takes the second, and
//! the two types have no conversion in either direction — the way across is
//! routing through the index, or one of the explicit `OciIdentifier`
//! constructors a workspace ratchet counts. Each refusal below has a twin that compiles
//! with the right type, so each one fails for the reason it names. A twin
//! differs from its refusal in exactly one expression and names the same
//! types, so renaming either type turns the twin red instead of letting the
//! refusal pass on an unresolved name.
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
//! And a package identifier does not become a location without routing,
//! neither by `into` nor by `from`:
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

/// The seal on [`client::OciTransport`].
///
/// The module is private, so `Sealed` is unnameable outside this crate and
/// nothing outside it can write the impl the supertrait bound demands. Every
/// in-crate implementor of `OciTransport` writes a one-line `impl Sealed`
/// beside its own — deliberately manual, so adding a transport is a decision
/// taken in this crate rather than a trait anyone can pick up.
mod sealed {
    /// Supertrait of [`crate::client::OciTransport`]; see the module docs.
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

// Registry credentials: the store, the login flow, the `OCX_AUTH_*` env form and
// the canonical registry key both halves agree on.
pub mod auth;

// The OCI layer and manifest media-type vocabulary, and the archive-extension
// inference that picks one.
pub mod media_type;

// The registry-wire tag conventions: the `__ocx` namespace, the frozen legacy
// keep-tag form, and the OCI Referrers fallback / cosign sidecar tags. A peer
// of `client` and `index`, and deliberately not part of `package`: these are
// names a registry holds, not versions a package publishes. `package::tag::Tag`
// classifies on top of them.
pub mod tag;

pub mod layer_layout;
pub use layer_layout::{LayerLayoutError, LayerLayoutSpec, resolve_layer_placement};

// What a publisher names a layer by on the wire — a local archive path or a
// digest already in the registry, plus the per-layer strip/prefix tail. A peer
// of `layer_layout` (which reads the same intent back off a manifest
// descriptor), and deliberately not part of `publisher`: the push path is the
// only *writer*, but the vocabulary is the client's argument type.
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

// The `--platform` optionality rule sign, attest and verify share. A peer of
// all three: a pure decision over a resolution outcome, deliberately holding no
// I/O, so the one rule cannot fork three ways.
pub mod resolve_target;

// Shared Sigstore endpoint URL validation (`UrlRejection`, `validate_sigstore_url`).
// Lifted here as a peer of `sign`/`verify` so verify does not depend on sign for a
// primitive both use (ADR `adr_oci_referrers_signing_v1.md` Amendment 2).
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

// The physical side of the identifier split (ocx#504): what `Client` dials.
pub mod oci_identifier;
pub use oci_identifier::{OciIdentifier, PinnedOciIdentifier};

pub mod repository;
pub use repository::Repository;

mod file_storage;
pub use file_storage::FileStorage;

// The transport doubles and client seams this crate lends its consumers; see
// the module docs for why they are gated rather than plain `pub`.
#[cfg(any(test, feature = "__testing"))]
pub mod testing;
