// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

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

pub mod layer_layout;
pub use layer_layout::{LayerLayoutError, LayerLayoutSpec, resolve_layer_placement};

pub mod client;
pub mod copy;
pub use client::Client;
pub use client::ClientBuilder;
pub use client::LayerCounts;
pub use client::MirrorMap;

pub mod ssrf;

pub mod index;
pub use index::Index;

pub mod transport_policy;

pub mod manifest;
pub mod manifest_builder;
pub use manifest_builder::{ManifestArtifacts, ManifestBuilder};

pub mod referrer;

// The cosign simplesigning claim — the payload a `sha256-<hex>.sig` sidecar
// layer carries. A peer of `referrer`, not a child of `sign`: verify reads it
// too, and it is a wire format in its own right.
pub mod simplesigning;

// The `--platform` optionality rule sign, attest and verify share. A peer of
// all three: a pure decision over a resolution outcome, deliberately holding no
// I/O, so the one rule cannot fork three ways.
pub mod resolve_target;

// Shared Sigstore endpoint URL validation (`UrlRejection`, `validate_sigstore_url`).
// Lifted here as a peer of `sign`/`verify` so verify does not depend on sign for a
// primitive both use (ADR `adr_oci_referrers_signing_v1.md` Amendment 2).
pub mod endpoint;

// `attest` owns the in-toto/DSSE wire formats. The module graph is NOT
// acyclic here, and that is accepted (D-h: no new error family) rather than a
// design lapse: attest/dsse.rs + attest/statement.rs return VerifyErrorKind
// (attest <-> verify), and attest/pipeline.rs + attest/statement.rs return
// SignErrorKind while sign/{bundle,rekor,signer}.rs import attest's DSSE and
// Statement types back (attest <-> sign). See adr_sbom_attestations.md D-i.
pub mod attest;

pub mod sign;

pub mod verify;

pub mod identifier;
pub use identifier::DEFAULT_REGISTRY;
pub use identifier::Identifier;
pub use identifier::OCX_SH_REGISTRY;
pub use identifier::error::{IdentifierError, IdentifierErrorKind};
pub use identifier::ocx_cli_identifier;

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

pub mod pinned_identifier;
pub use pinned_identifier::PinnedIdentifier;

pub mod repository;
pub use repository::Repository;

mod file_storage;
pub use file_storage::FileStorage;
