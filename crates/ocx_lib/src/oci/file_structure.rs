use crate::oci;

/// Abstracts the storage of OCI artifacts on the local filesystem,
/// providing a structured layout for storing tags, manifests and blobs.
/// 
/// Note, preferably the storage for the index and the blobs should be separated.
/// The index is a locked file that is updated frequently, while the blobs are immutable and can be
/// shared across different environments, for example using volumes in Docker.
#[derive(Clone)]
pub struct FileStructure {
    root: std::path::PathBuf,
}

impl FileStructure {
    /// Creates a new file structure with the given root directory.
    pub fn new(root: impl Into<std::path::PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The root directory of the file structure.
    pub fn root(&self) -> &std::path::PathBuf {
        &self.root
    }

    /// A path specific to the registry, e.g. "docker.io".
    /// All artifacts should not be shared between different registries, even if they share the same digest.
    pub fn registry(&self, identifier: &oci::Identifier) -> std::path::PathBuf {
        self.root().join(identifier.registry())
    }

    /// A path to a json file containing the list of known tags for the given identifier.
    pub fn tags(&self, identifier: &oci::Identifier) -> std::path::PathBuf {
        self.registry(identifier)
            .join("tags")
            .join(identifier.repository())
            .with_added_extension("json")
    }

    /// A path to the blob content for the given identifier and digest.
    /// This path has no extension, as the content can be of any type, e.g. a manifest or tarball.
    pub fn blob(&self, identifier: &oci::Identifier, digest: &oci::Digest) -> std::path::PathBuf {
        self.registry(identifier)
            .join("objects")
            .join(digest.to_path())
    }

    /// A path to the blob content for the given identifier and digest, with the given extension.
    /// This is useful for storing artifacts with a-priori known content type, e.g. a manifest with ".json" extension.
    pub fn blob_with_extension(&self, identifier: &oci::Identifier, digest: &oci::Digest, extension: impl AsRef<std::ffi::OsStr>) -> std::path::PathBuf {
        self.blob(identifier, digest).with_added_extension(extension)
    }

    /// A path to a json file containing the snapshot of the given identifier.
    pub fn manifest(&self, identifier: &oci::Identifier, digest: &oci::Digest) -> std::path::PathBuf {
        self.blob_with_extension(identifier, digest, "json")
    }
}
