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
pub use client::Client;
pub use client::ClientBuilder;
pub use client::MirrorMap;

pub mod index;
pub use index::Index;

pub mod manifest;
pub mod manifest_builder;
pub use manifest_builder::{ManifestArtifacts, ManifestBuilder};

pub mod identifier;
pub use identifier::DEFAULT_REGISTRY;
pub use identifier::Identifier;
pub use identifier::OCX_SH_REGISTRY;
pub use identifier::error::{IdentifierError, IdentifierErrorKind};
pub use identifier::ocx_cli_identifier;

pub mod platform;
pub use platform::Architecture;
pub use platform::OperatingSystem;
pub use platform::Platform;

pub mod digest;
pub use digest::Algorithm;
pub use digest::Digest;

pub mod pinned_identifier;
pub use pinned_identifier::PinnedIdentifier;

pub mod repository;
pub use repository::Repository;

mod file_storage;
pub use file_storage::FileStorage;
