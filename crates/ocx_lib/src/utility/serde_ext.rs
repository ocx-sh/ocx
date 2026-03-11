// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::prelude::*;

pub trait SerdeExt: Sized {
    fn read_json_from_path(path: impl AsRef<std::path::Path>) -> crate::Result<Self>;
    fn write_json_to_path(&self, path: impl AsRef<std::path::Path>) -> crate::Result<()>;
}

impl<T> SerdeExt for T
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    fn read_json_from_path(path: impl AsRef<std::path::Path>) -> crate::Result<Self> {
        let file = std::fs::File::open(&path).map_err(|error| crate::error::file_error(&path, error))?;
        let reader = std::io::BufReader::new(file);
        let value = serde_json::from_reader(reader).map_to_undefined_error()?;
        Ok(value)
    }

    fn write_json_to_path(&self, path: impl AsRef<std::path::Path>) -> crate::Result<()> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent).ignore();
        }
        let file = std::fs::File::create(&path).map_err(|error| crate::error::file_error(&path, error))?;
        let writer = std::io::BufWriter::new(file);
        serde_json::to_writer_pretty(writer, self).map_to_undefined_error()?;
        Ok(())
    }
}
