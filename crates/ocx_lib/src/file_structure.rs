use crate::{
    Result,
    oci
};

#[derive(Debug, Clone)]
pub struct FileStructure {
    root: std::path::PathBuf,
}

impl Default for FileStructure {
    fn default() -> Self {
        Self::new()
    }
}

impl FileStructure {
    pub fn new() -> Self {
        Self {
            root: default_ocx_root().expect("Could not determine default OCX root directory."),
        }
    }

    pub fn with_root(root: std::path::PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &std::path::PathBuf {
        &self.root
    }

    pub fn objects(&self) -> std::path::PathBuf {
        self.root.join("objects")
    }

    pub fn object(&self, identifier: &oci::Identifier) -> Result<std::path::PathBuf> {
        let digest = identifier.digest().ok_or_else(|| {
            crate::error::Error::UndefinedWithMessage(format!(
                "Object store is only well-defined for identifiers with sha256 digest, got identifier with reference: {}",
                identifier
            ))
        })?;
        Ok(self.objects().join(format!(
            "{}/{}",
            identifier.reference.registry(),
            identifier.reference.repository(),
        )).join(digest.to_path()))
    }

    pub fn object_metadata(&self, identifier: &oci::Identifier) -> Result<std::path::PathBuf> {
        Ok(self.object(identifier)?.join("metadata.json"))
    }

    pub fn object_content(&self, identifier: &oci::Identifier) -> Result<std::path::PathBuf> {
        Ok(self.object(identifier)?.join("content"))
    }

    pub fn indices(&self) -> std::path::PathBuf {
        self.root.join("index")
    }

    pub fn installs(&self) -> std::path::PathBuf {
        self.root.join("installs")
    }

    pub fn install(&self, identifier: &oci::Identifier) -> std::path::PathBuf {
        self.installs().join(format!(
            "{}/{}",
            identifier.reference.registry(),
            identifier.reference.repository(),
        ))
    }

    pub fn install_current(&self, identifier: &oci::Identifier) -> std::path::PathBuf {
        self.install(identifier).join("current")
    }

    pub fn install_candidates(&self, identifier: &oci::Identifier) -> std::path::PathBuf {
        self.install(identifier).join("candidates")
    }

    pub fn install_candidate(&self, identifier: &oci::Identifier) -> std::path::PathBuf {
        self.install_candidates(identifier).join(identifier.tag_or_latest())
    }
}

pub fn default_ocx_root() -> Option<std::path::PathBuf> {
    std::env::home_dir().map(|home| home.join(".ocx"))
}
