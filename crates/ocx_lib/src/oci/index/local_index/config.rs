// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::file_structure::{BlobStore, TagStore};

pub struct Config {
    pub tag_store: TagStore,
    pub blob_store: BlobStore,
}
