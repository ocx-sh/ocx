// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::error::{FileError, Result, SerializationError};
use crate::result_ext::ResultExt as _;

pub trait SerdeExt: Sized {
    /// Reads and deserializes JSON from the given path.
    fn read_json(path: impl AsRef<std::path::Path> + Send) -> impl std::future::Future<Output = Result<Self>> + Send;

    /// Serializes and writes JSON to the given path.
    fn write_json(
        &self,
        path: impl AsRef<std::path::Path> + Send,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
}

impl<T> SerdeExt for T
where
    T: serde::de::DeserializeOwned + serde::Serialize + Send + Sync,
{
    async fn read_json(path: impl AsRef<std::path::Path> + Send) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|error| FileError::new(&path, error))?;
        Ok(serde_json::from_slice(&bytes).map_err(SerializationError::from)?)
    }

    async fn write_json(&self, path: impl AsRef<std::path::Path> + Send) -> Result<()> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.ignore();
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(SerializationError::from)?;
        tokio::fs::write(&path, bytes)
            .await
            .map_err(|error| FileError::new(&path, error))?;
        Ok(())
    }
}
