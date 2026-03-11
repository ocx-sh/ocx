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

    pub use oci_client::manifest::OciImageIndex as ImageIndex;
    pub use oci_client::manifest::OciImageManifest as ImageManifest;
    pub use oci_client::manifest::OciManifest as Manifest;

    pub use oci_client::secrets::RegistryAuth as Auth;

    pub use docker_credential;
    pub use docker_credential::CredentialRetrievalError as DockerCredentialRetrievalError;
    pub use docker_credential::DockerCredential;
    pub use docker_credential::get_credential as get_docker_credential;
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
pub mod client;
pub use client::Client;
pub use client::ClientBuilder;

pub mod index;
pub use index::Index;

pub mod manifest;

pub mod identifier;
pub use identifier::DEFAULT_REGISTRY;
pub use identifier::Identifier;
pub use identifier::OCX_SH_REGISTRY;

mod platform;
pub use platform::Platform;

mod digest;
pub use digest::Digest;

mod file_storage;
pub use file_storage::FileStorage;
