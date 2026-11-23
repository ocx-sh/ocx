// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The ordered walk over every source of one blob, driven by a caller that verifies what it is handed.

use std::path::PathBuf;

use crate::Digest;

use super::writer::{BlobCacheDir, refuse_symlinks};
use super::{BlobLookup, LookupError, blob_data_path, registry_slug_is_safe};

/// Which source answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LookupOrigin {
    /// The blob directory at this position in the lookup's list.
    Dir(usize),
    Cache,
}

/// An opened, unverified `data` file.
#[derive(Debug)]
pub struct LookupStream {
    pub origin: LookupOrigin,
    pub reader: tokio::fs::File,
    pub size: u64,
}

/// Who a `data` file was when it was opened: a replacement published since has another identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FileIdentity {
    len: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    inode: (u64, u64),
}

impl FileIdentity {
    pub(super) fn of(metadata: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt as _;
        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            inode: (metadata.dev(), metadata.ino()),
        }
    }
}

/// What a walk found once every source was tried without a clean copy and no directory held a bad one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalkEnd {
    /// Cache entries removed after failing verification, counted for the caller's message.
    pub discarded_cache_entries: usize,
}

/// One blob's walk: blob directories in list order, then the cache.
///
/// The caller reads and verifies each stream, then either returns or calls [`reject`](Self::reject) and asks for
/// the next. A bad directory copy is remembered, so exhausting the sources is the 65 verdict rather than "absent";
/// a bad cache entry is removed and is only a miss.
#[derive(Debug)]
pub struct LookupWalk<'a> {
    lookup: &'a BlobLookup,
    registry: String,
    digest: Digest,
    next: usize,
    bad_dir_copies: usize,
    last_bad_dir_copy: Option<PathBuf>,
    /// The copy most recently offered; a rejection removes that copy and nothing newer.
    offered: Option<FileIdentity>,
    discarded_cache_entries: usize,
}

impl<'a> LookupWalk<'a> {
    pub(super) fn new(lookup: &'a BlobLookup, registry: &str, digest: &Digest) -> Self {
        Self {
            lookup,
            registry: registry.to_string(),
            digest: digest.clone(),
            next: 0,
            bad_dir_copies: 0,
            last_bad_dir_copy: None,
            offered: None,
            discarded_cache_entries: 0,
        }
    }

    /// The next source holding a regular `data` file, unverified; `None` once every source was offered.
    ///
    /// # Errors
    ///
    /// A source that exists but cannot be opened.
    pub async fn next_source(&mut self) -> Result<Option<LookupStream>, LookupError> {
        let total = self.lookup.dirs().len() + usize::from(self.lookup.cache().is_some());
        if !registry_slug_is_safe(&self.registry) {
            self.next = total;
        }
        while self.next < total {
            let position = self.next;
            self.next += 1;
            let (origin, root) = match self.lookup.dirs().get(position) {
                Some(dir) => (LookupOrigin::Dir(position), dir.clone()),
                None => match self.lookup.cache() {
                    Some(cache) => (LookupOrigin::Cache, cache.root().to_path_buf()),
                    None => continue,
                },
            };
            let path = blob_data_path(&root, &self.registry, &self.digest);
            // Home and `--blob-dir` follow links (operator-provided, digest-verified); ocx writes the cache, which may be shared.
            if origin == LookupOrigin::Cache {
                if !BlobCacheDir::holds_registry(&self.registry) {
                    continue;
                }
                if let Err(error) = refuse_symlinks(&root, &path) {
                    log::warn!("skipping cached blob {}: {error}", self.digest);
                    continue;
                }
            }
            let opened = {
                let path = path.clone();
                tokio::task::spawn_blocking(move || ocx_util::fs::open_regular(&path)).await
            };
            let opened = opened
                .map_err(|join| LookupError::Io {
                    path: path.clone(),
                    source: std::io::Error::other(join),
                })?
                .map_err(|source| LookupError::Io {
                    path: path.clone(),
                    source,
                })?;
            match opened {
                Some((file, metadata)) => {
                    self.offered = Some(FileIdentity::of(&metadata));
                    return Ok(Some(LookupStream {
                        origin,
                        reader: tokio::fs::File::from_std(file),
                        size: metadata.len(),
                    }));
                }
                None => {
                    if matches!(origin, LookupOrigin::Dir(_)) {
                        self.lookup.warn_if_missing_dir(&root).await;
                    }
                }
            }
        }
        Ok(None)
    }

    /// Records that the stream from `origin` failed verification or ended short.
    ///
    /// A directory copy is remembered; a cache entry is removed (one warning when removal fails) and counted.
    pub async fn reject(&mut self, origin: LookupOrigin) {
        match origin {
            LookupOrigin::Dir(position) => {
                self.bad_dir_copies += 1;
                if let Some(dir) = self.lookup.dirs().get(position) {
                    let path = blob_data_path(dir, &self.registry, &self.digest);
                    log::warn!("blob '{}' in a blob directory failed verification", path.display());
                    self.last_bad_dir_copy = Some(path);
                }
            }
            LookupOrigin::Cache => {
                self.discarded_cache_entries += 1;
                if let Some(cache) = self.lookup.cache() {
                    log::warn!(
                        "discarding cached blob {} from '{}'",
                        self.digest,
                        cache.root().display()
                    );
                    if let Err(error) = cache
                        .remove_if(&self.registry, &self.digest, self.offered.as_ref())
                        .await
                    {
                        log::warn!("could not remove the bad cache entry: {error}");
                    }
                }
            }
        }
    }

    /// The verdict once [`next_source`](Self::next_source) returned `None`.
    ///
    /// # Errors
    ///
    /// [`LookupError::DigestMismatch`] when any blob directory held a bad copy.
    pub fn exhausted(&self) -> Result<WalkEnd, LookupError> {
        if self.bad_dir_copies > 0 {
            return Err(LookupError::DigestMismatch {
                path: self.last_bad_dir_copy.clone().unwrap_or_default(),
                expected: self.digest.to_string(),
                actual: "unverified content".to_string(),
            });
        }
        Ok(WalkEnd {
            discarded_cache_entries: self.discarded_cache_entries,
        })
    }
}
