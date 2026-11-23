// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The concurrency-safe CAS writer behind a blob directory or cache: hash while writing, verify, publish atomically.
//!
//! Temp file in `<root>/.tmp/`, `sync_data`, a symlink-ancestor check before each `create_dir_all`, then a
//! no-clobber publish. A racing writer that lost re-verifies the winner; the occupant is verified before it counts
//! as present, so a planted or corrupt file never short-circuits a write.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ocx_util::fs::PersistOutcome;
use sha2::Digest as _;

use crate::{Algorithm, Digest};

use super::export_marker::{self, ExportMarker, ExportMarkerError};
use super::stream::FileIdentity;
use super::{LookupError, blob_dir_path, registry_slug_is_safe};

/// Staging directory under a root; never a registry slug, never read.
pub(super) const TMP_DIR: &str = ".tmp";

/// What one write did to the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOutcome {
    Written,
    /// A corrupt occupant was replaced.
    Replaced,
    /// A verified copy was already there; nothing was written.
    AlreadyPresent,
}

/// Running hash in the digest's own algorithm.
enum Hasher {
    Sha256(sha2::Sha256),
    Sha384(sha2::Sha384),
    Sha512(sha2::Sha512),
}

impl Hasher {
    fn new(algorithm: Algorithm) -> Self {
        match algorithm {
            Algorithm::Sha256 => Self::Sha256(sha2::Sha256::new()),
            Algorithm::Sha384 => Self::Sha384(sha2::Sha384::new()),
            Algorithm::Sha512 => Self::Sha512(sha2::Sha512::new()),
        }
    }

    fn update(&mut self, bytes: &[u8]) {
        match self {
            Self::Sha256(hasher) => hasher.update(bytes),
            Self::Sha384(hasher) => hasher.update(bytes),
            Self::Sha512(hasher) => hasher.update(bytes),
        }
    }

    fn finalize(self) -> Digest {
        match self {
            Self::Sha256(hasher) => Digest::Sha256(hex::encode(hasher.finalize())),
            Self::Sha384(hasher) => Digest::Sha384(hex::encode(hasher.finalize())),
            Self::Sha512(hasher) => Digest::Sha512(hex::encode(hasher.finalize())),
        }
    }
}

/// What a blob directory's `data` file holds right now.
enum Occupant {
    Absent,
    Verified,
    /// Present but not the expected bytes, or not a regular file.
    Corrupt,
}

enum State {
    /// A verified copy exists; the bytes written are discarded.
    Present,
    Staging {
        temp: tempfile::NamedTempFile,
        hasher: Box<Hasher>,
        replace: bool,
    },
}

/// A streaming writer for one blob; see the module doc for the protocol.
pub struct BlobWriter {
    root: PathBuf,
    registry: String,
    digest: Digest,
    state: State,
}

impl std::fmt::Debug for BlobWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlobWriter")
            .field("root", &self.root)
            .field("registry", &self.registry)
            .field("digest", &self.digest)
            .finish_non_exhaustive()
    }
}

fn io_error(path: &Path) -> impl FnOnce(std::io::Error) -> LookupError + '_ {
    |source| LookupError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn unusable_registry(root: &Path, registry: &str) -> LookupError {
    LookupError::Io {
        path: root.to_path_buf(),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("registry '{registry}' has no usable directory name"),
        ),
    }
}

/// Refuses `path` when it, or any ancestor from `root` down, is a symlink; `root`'s own parent and above are trusted.
pub(super) fn refuse_symlinks(root: &Path, path: &Path) -> Result<(), LookupError> {
    ocx_util::fs::refuse_if_symlink_in_path_sync(path, root.parent()).map_err(|error| LookupError::Io {
        path: path.to_path_buf(),
        source: std::io::Error::other(error),
    })
}

/// A temp file in `dir` whose final mode is the umask default, never the private tempfile default.
pub(super) fn umask_temp_in(dir: &Path) -> std::io::Result<tempfile::NamedTempFile> {
    #[cfg_attr(not(unix), allow(unused_mut))] // the mode is set on unix only
    let mut builder = tempfile::Builder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        builder.permissions(std::fs::Permissions::from_mode(0o666));
    }
    builder.tempfile_in(dir)
}

/// Hashes the `data` file of `dir` against `digest`.
fn inspect_occupant(dir: &Path, digest: &Digest) -> Result<Occupant, LookupError> {
    let data = dir.join("data");
    let Some((mut file, _)) = ocx_util::fs::open_regular(&data).map_err(io_error(&data))? else {
        return Ok(if std::fs::symlink_metadata(&data).is_ok() {
            Occupant::Corrupt
        } else {
            Occupant::Absent
        });
    };
    let mut hasher = Hasher::new(digest.algorithm());
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(io_error(&data))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(if hasher.finalize() == *digest {
        Occupant::Verified
    } else {
        Occupant::Corrupt
    })
}

/// Refuses a write when the sibling `digest` file names a different full digest: the shard path is another blob's.
fn refuse_foreign_shard(dir: &Path, digest: &Digest) -> Result<(), LookupError> {
    let Ok(bytes) = ocx_util::fs::read_bounded(&dir.join("digest"), 256) else {
        return Ok(());
    };
    let named = std::str::from_utf8(&bytes)
        .ok()
        .and_then(|text| Digest::try_from(text.trim()).ok());
    match named {
        Some(existing) if existing != *digest => Err(LookupError::ShardCollision {
            path: dir.to_path_buf(),
            existing: existing.to_string(),
            digest: digest.to_string(),
        }),
        _ => Ok(()),
    }
}

/// The staging temp under `root`, after the ancestor check and with the staging directory present.
fn new_temp(root: &Path) -> Result<tempfile::NamedTempFile, LookupError> {
    let tmp_dir = root.join(TMP_DIR);
    refuse_symlinks(root, &tmp_dir)?;
    std::fs::create_dir_all(&tmp_dir).map_err(io_error(&tmp_dir))?;
    umask_temp_in(&tmp_dir).map_err(io_error(&tmp_dir))
}

impl BlobWriter {
    /// Starts a write of `digest` under `root` for `registry`.
    ///
    /// # Errors
    ///
    /// Disk failure creating the temp directory, or a symlinked ancestor; [`LookupError::ShardCollision`] when the
    /// shard path belongs to another digest.
    pub fn root_for(root: &Path, registry: &str, digest: &Digest) -> Result<Self, LookupError> {
        if !registry_slug_is_safe(registry) {
            return Err(unusable_registry(root, registry));
        }
        let dir = blob_dir_path(root, registry, digest);
        refuse_symlinks(root, &dir)?;
        let replace = match inspect_occupant(&dir, digest)? {
            Occupant::Verified => {
                return Ok(Self::new(root, registry, digest, State::Present));
            }
            Occupant::Corrupt => {
                refuse_foreign_shard(&dir, digest)?;
                true
            }
            Occupant::Absent => false,
        };
        let state = State::Staging {
            temp: new_temp(root)?,
            hasher: Box::new(Hasher::new(digest.algorithm())),
            replace,
        };
        Ok(Self::new(root, registry, digest, state))
    }

    fn new(root: &Path, registry: &str, digest: &Digest, state: State) -> Self {
        Self {
            root: root.to_path_buf(),
            registry: registry.to_string(),
            digest: digest.clone(),
            state,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether a verified copy already occupies the destination, so no byte needs writing.
    pub fn is_present(&self) -> bool {
        matches!(self.state, State::Present)
    }

    pub fn registry(&self) -> &str {
        &self.registry
    }

    pub fn digest(&self) -> &Digest {
        &self.digest
    }

    /// Appends `bytes` to the temp file and the running hash. Blocking.
    ///
    /// # Errors
    ///
    /// Disk failure.
    pub fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        match &mut self.state {
            State::Present => Ok(()),
            State::Staging { temp, hasher, .. } => {
                hasher.update(bytes);
                temp.as_file_mut().write_all(bytes)
            }
        }
    }

    /// Verifies the hash and publishes the blob. A mismatch deletes the temp file. Blocking.
    ///
    /// # Errors
    ///
    /// [`LookupError::DigestMismatch`], [`LookupError::ShardCollision`], else I/O.
    pub fn finish(self) -> Result<WriteOutcome, LookupError> {
        let State::Staging { temp, hasher, replace } = self.state else {
            return Ok(WriteOutcome::AlreadyPresent);
        };
        let dir = blob_dir_path(&self.root, &self.registry, &self.digest);
        let data = dir.join("data");
        let actual = hasher.finalize();
        if actual != self.digest {
            return Err(LookupError::DigestMismatch {
                path: data,
                expected: self.digest.to_string(),
                actual: actual.to_string(),
            });
        }
        temp.as_file().sync_data().map_err(io_error(&data))?;
        refuse_symlinks(&self.root, &dir)?;
        std::fs::create_dir_all(&dir).map_err(io_error(&dir))?;
        // The recovery file lands before `data`, so a reader that sees `data` always sees its `digest`, and a
        // failure here leaves nothing published.
        refuse_foreign_shard(&dir, &self.digest)?;
        write_digest_file(&self.root, &dir, &self.digest)?;

        if replace {
            return publish_replacing(temp, &dir, &self.digest);
        }
        publish_noclobber(temp, &dir, &self.digest, &mut inspect_occupant)
    }
}

/// Publishes without clobbering, retrying while the occupant a refused publish collided with keeps vanishing.
fn publish_noclobber(
    mut temp: tempfile::NamedTempFile,
    dir: &Path,
    digest: &Digest,
    inspect: &mut dyn FnMut(&Path, &Digest) -> Result<Occupant, LookupError>,
) -> Result<WriteOutcome, LookupError> {
    let data = dir.join("data");
    for _ in 0..PUBLISH_ATTEMPTS {
        match ocx_util::fs::persist_temp_file_noclobber(temp, &data).map_err(io_error(&data))? {
            PersistOutcome::Published => return Ok(WriteOutcome::Written),
            PersistOutcome::Occupied(back) => match inspect(dir, digest)? {
                Occupant::Verified => return Ok(WriteOutcome::AlreadyPresent),
                Occupant::Corrupt => {
                    refuse_foreign_shard(dir, digest)?;
                    return publish_replacing(back, dir, digest);
                }
                // The occupant was removed between the refused publish and the look: publish again.
                Occupant::Absent => temp = back,
            },
        }
    }
    Err(LookupError::Io {
        path: data,
        source: std::io::Error::other("blob kept vanishing while it was being published"),
    })
}

/// How often a publish retries after the occupant it collided with disappeared.
const PUBLISH_ATTEMPTS: usize = 4;

/// Replaces a corrupt occupant; a failed replace is still success when the target now verifies.
fn publish_replacing(temp: tempfile::NamedTempFile, dir: &Path, digest: &Digest) -> Result<WriteOutcome, LookupError> {
    let data = dir.join("data");
    match ocx_util::fs::persist_temp_file(temp, &data) {
        Ok(()) => Ok(WriteOutcome::Replaced),
        Err(error) => match inspect_occupant(dir, digest) {
            Ok(Occupant::Verified) => Ok(WriteOutcome::AlreadyPresent),
            _ => Err(LookupError::Io {
                path: data,
                source: error,
            }),
        },
    }
}

/// Writes the recovery `digest` file naming the full digest of the shard's blob.
fn write_digest_file(root: &Path, dir: &Path, digest: &Digest) -> Result<(), LookupError> {
    let target = dir.join("digest");
    let mut temp = new_temp(root)?;
    temp.write_all(digest.to_string().as_bytes())
        .map_err(io_error(&target))?;
    // Unsynced, a crash after the rename can leave an empty `digest` beside a published `data`.
    temp.as_file().sync_data().map_err(io_error(&target))?;
    ocx_util::fs::persist_temp_file(temp, &target).map_err(io_error(&target))
}

/// A shared blob cache root: a blob directory this process also writes.
#[derive(Debug, Clone)]
pub struct BlobCacheDir {
    root: PathBuf,
    /// The home's central lock root; the marker lock lives there, never in the cache, which holds only blobs.
    locks: Option<PathBuf>,
    /// What this process knows of the marker, shared by every clone.
    marker: Arc<Mutex<MarkerState>>,
}

#[derive(Debug, Default)]
enum MarkerState {
    #[default]
    Unread,
    /// The marker as this process last read or wrote it; a registry missing here is re-read under the lock.
    Known(ExportMarker),
    /// The marker is invalid or of an unsupported version: warned once, then left alone.
    Refused,
}

/// How long a writer waits for another process to finish updating the marker.
const MARKER_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

impl PartialEq for BlobCacheDir {
    fn eq(&self, other: &Self) -> bool {
        self.root == other.root
    }
}

impl Eq for BlobCacheDir {}

impl BlobCacheDir {
    /// A cache at `root`, without touching the disk; nothing is created until the first write.
    pub fn at(root: PathBuf) -> Self {
        Self {
            root,
            locks: None,
            marker: Arc::default(),
        }
    }

    /// Serializes marker updates across processes through a lock under `locks`. Without one, only the handles
    /// of this process are serialized.
    #[must_use]
    pub fn with_locks(mut self, locks: PathBuf) -> Self {
        self.locks = Some(locks);
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Opens a cache, refusing a root with a symlinked ancestor it did not create.
    ///
    /// # Errors
    ///
    /// [`LookupError::Io`] (exit 74) for a symlinked ancestor or an unusable root.
    pub async fn open(root: PathBuf, locks: PathBuf) -> Result<Self, LookupError> {
        ocx_util::fs::refuse_if_symlink_in_path(&root, root.parent())
            .await
            .map_err(|error| LookupError::Io {
                path: root.clone(),
                source: std::io::Error::other(error),
            })?;
        match tokio::fs::metadata(&root).await {
            Ok(metadata) if !metadata.is_dir() => Err(LookupError::Io {
                path: root,
                source: std::io::Error::new(std::io::ErrorKind::NotADirectory, "not a directory"),
            }),
            Ok(_) => Ok(Self::at(root).with_locks(locks)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::at(root).with_locks(locks)),
            Err(source) => Err(LookupError::Io { path: root, source }),
        }
    }

    /// A writer for `digest` under the logical `registry`.
    ///
    /// # Errors
    ///
    /// As [`BlobWriter::root_for`].
    pub fn writer(&self, registry: &str, digest: &Digest) -> Result<BlobWriter, LookupError> {
        if !Self::holds_registry(registry) {
            return Err(unusable_registry(&self.root, registry));
        }
        let writer = BlobWriter::root_for(&self.root, registry, digest)?;
        self.note_registry(registry);
        Ok(writer)
    }

    /// False for a registry whose slug directory could not sit beside the marker file at the cache root.
    pub(super) fn holds_registry(registry: &str) -> bool {
        registry_slug_is_safe(registry)
            && ocx_util::prelude::StringExt::to_relaxed_slug(&registry) != export_marker::MARKER_FILE
    }

    /// Records `registry` in the cache's advisory `ocx-export.json`; a failure or a slug collision is one warning.
    fn note_registry(&self, registry: &str) {
        if let Err(error) = self.try_note_registry(registry) {
            log::warn!(
                "blob cache '{}' export marker not updated: {error}",
                self.root.display()
            );
        }
    }

    fn try_note_registry(&self, registry: &str) -> Result<(), ExportMarkerError> {
        let slug = ocx_util::prelude::StringExt::to_relaxed_slug(&registry);
        let mut state = self.marker.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match &*state {
            MarkerState::Refused => return Ok(()),
            MarkerState::Known(marker) if marker.registries.get(&slug).is_some_and(|known| known == registry) => {
                return Ok(());
            }
            _ => {}
        }
        match self.merge_under_lock(registry) {
            Ok(marker) => {
                *state = MarkerState::Known(marker);
                Ok(())
            }
            Err(error @ ExportMarkerError::Invalid { .. }) => {
                *state = MarkerState::Refused;
                Err(error)
            }
            Err(error) => Err(error),
        }
    }

    /// Re-reads the marker, adds `registry` and publishes it, all under a cross-process lock: a snapshot cached
    /// before another process added its registry would otherwise be written back and erase that entry.
    ///
    /// The lock is keyed by the cache root's identity under the home's lock root, so it serializes the processes
    /// of one home; processes of different homes sharing the cache can still lose an entry of this advisory
    /// marker, never a blob.
    fn merge_under_lock(&self, registry: &str) -> Result<ExportMarker, ExportMarkerError> {
        // Never a lock on the marker itself: the marker is replaced by rename, which strands a lock on its inode.
        let _lock = match &self.locks {
            Some(locks) => Some(
                ocx_util::fs::lock_scoped_blocking(locks, "blob-cache-marker", &self.root, "", MARKER_LOCK_TIMEOUT)
                    .map_err(|error| LookupError::Io {
                        path: self.root.clone(),
                        source: std::io::Error::other(error),
                    })?,
            ),
            None => None,
        };
        let mut marker = export_marker::read_marker(&self.root)?.unwrap_or_default();
        export_marker::merge_registry(&mut marker, registry)?;
        export_marker::write_marker(&self.root, &marker)?;
        Ok(marker)
    }

    /// Writes already-hashed metadata through the same protocol.
    ///
    /// # Errors
    ///
    /// As [`BlobWriter::finish`].
    pub async fn write_verified(&self, registry: &str, digest: &Digest, bytes: &[u8]) -> Result<(), LookupError> {
        let cache = self.clone();
        let (registry, digest, bytes) = (registry.to_string(), digest.clone(), bytes.to_vec());
        tokio::task::spawn_blocking(move || {
            let mut writer = cache.writer(&registry, &digest)?;
            writer.write(&bytes).map_err(io_error(&cache.root))?;
            writer.finish().map(|_| ())
        })
        .await
        .map_err(|join| LookupError::Io {
            path: self.root.clone(),
            source: std::io::Error::other(join),
        })?
    }

    /// Removes the `data` file of one entry, after an ancestor check; never follows a link.
    ///
    /// # Errors
    ///
    /// I/O failure other than "not found".
    pub async fn remove(&self, registry: &str, digest: &Digest) -> Result<(), LookupError> {
        self.remove_if(registry, digest, None).await
    }

    /// As [`remove`](Self::remove), but only when the file is still the one `expected` describes: a verified
    /// replacement published after the bad copy was read is left alone.
    pub(super) async fn remove_if(
        &self,
        registry: &str,
        digest: &Digest,
        expected: Option<&FileIdentity>,
    ) -> Result<(), LookupError> {
        if !Self::holds_registry(registry) {
            return Ok(());
        }
        let dir = blob_dir_path(&self.root, registry, digest);
        ocx_util::fs::refuse_if_symlink_in_path(&dir, self.root.parent())
            .await
            .map_err(|error| LookupError::Io {
                path: dir.clone(),
                source: std::io::Error::other(error),
            })?;
        let data = dir.join("data");
        if let Some(expected) = expected {
            match tokio::fs::symlink_metadata(&data).await {
                Ok(current) if FileIdentity::of(&current) == *expected => {}
                Ok(_) => return Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(source) => return Err(LookupError::Io { path: data, source }),
            }
        }
        // Check-then-unlink: a verified replacement published after the check is unlinked too, costing one re-download.
        match tokio::fs::remove_file(&data).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(LookupError::Io { path: data, source }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blob_lookup::test_support::{put_blob_as, sha256};
    use crate::blob_lookup::{blob_data_path, blob_dir_path};

    const REG: &str = "registry.test";

    fn write_all(root: &Path, digest: &Digest, bytes: &[u8]) -> Result<WriteOutcome, LookupError> {
        let mut writer = BlobWriter::root_for(root, REG, digest)?;
        writer.write(bytes).map_err(|source| LookupError::Io {
            path: root.to_path_buf(),
            source,
        })?;
        writer.finish()
    }

    fn tmp_is_empty(root: &Path) -> bool {
        std::fs::read_dir(root.join(".tmp")).map_or(true, |mut entries| entries.next().is_none())
    }

    #[test]
    fn noclobber_drops_temp() {
        let root = tempfile::tempdir().unwrap();
        let digest = sha256(b"payload");
        assert_eq!(
            write_all(root.path(), &digest, b"payload").unwrap(),
            WriteOutcome::Written
        );
        assert_eq!(
            write_all(root.path(), &digest, b"payload").unwrap(),
            WriteOutcome::AlreadyPresent
        );
        assert!(tmp_is_empty(root.path()));
    }

    #[test]
    fn mismatch_deletes_temp() {
        let root = tempfile::tempdir().unwrap();
        let digest = sha256(b"payload");
        let error = write_all(root.path(), &digest, b"other").expect_err("wrong bytes");
        assert!(matches!(error, LookupError::DigestMismatch { .. }), "{error:?}");
        assert!(tmp_is_empty(root.path()));
        assert!(!blob_data_path(root.path(), REG, &digest).exists());
    }

    #[test]
    fn verified_occupant_short_circuits() {
        let root = tempfile::tempdir().unwrap();
        let digest = sha256(b"payload");
        put_blob_as(root.path(), REG, &digest, b"payload");
        std::fs::write(
            blob_dir_path(root.path(), REG, &digest).join("digest"),
            digest.to_string(),
        )
        .unwrap();
        assert_eq!(
            write_all(root.path(), &digest, b"payload").unwrap(),
            WriteOutcome::AlreadyPresent
        );
    }

    #[test]
    fn corrupt_occupant_replaced() {
        let root = tempfile::tempdir().unwrap();
        let digest = sha256(b"payload");
        put_blob_as(root.path(), REG, &digest, b"tampered");
        assert_eq!(
            write_all(root.path(), &digest, b"payload").unwrap(),
            WriteOutcome::Replaced
        );
        assert_eq!(
            std::fs::read(blob_data_path(root.path(), REG, &digest)).unwrap(),
            b"payload"
        );
    }

    #[test]
    fn shard_collision_refused() {
        let root = tempfile::tempdir().unwrap();
        let digest = sha256(b"payload");
        // A different digest that shares the 32-hex shard prefix: same directory, other full digest.
        let mut hex = digest.hex().to_string();
        hex.replace_range(32.., &"0".repeat(hex.len() - 32));
        let sibling = Digest::Sha256(hex);
        assert_ne!(sibling, digest);
        put_blob_as(root.path(), REG, &digest, b"payload");
        std::fs::write(
            blob_dir_path(root.path(), REG, &digest).join("digest"),
            digest.to_string(),
        )
        .unwrap();

        let error = BlobWriter::root_for(root.path(), REG, &sibling)
            .and_then(|writer| writer.finish())
            .expect_err("another digest owns the shard");
        assert!(matches!(error, LookupError::ShardCollision { .. }), "{error:?}");
    }

    #[cfg(unix)]
    #[test]
    fn umask_mode() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = tempfile::tempdir().unwrap();
        let digest = sha256(b"payload");
        write_all(root.path(), &digest, b"payload").unwrap();
        let mode = std::fs::metadata(blob_data_path(root.path(), REG, &digest))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_ne!(mode, 0o600, "the umask default, never the private tempfile default");
        assert_eq!(mode & !0o666, 0, "never wider than a plain file");
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_ancestor_refused() {
        let outside = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let link = root.path().join("linked");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        let digest = sha256(b"payload");

        let error = write_all(&link, &digest, b"payload").expect_err("a symlinked ancestor");
        assert!(matches!(error, LookupError::Io { .. }), "{error:?}");
        assert!(
            std::fs::read_dir(outside.path()).unwrap().next().is_none(),
            "nothing is written through the link"
        );
    }

    #[cfg(unix)]
    #[test]
    fn digest_file_failure_publishes_nothing() {
        let root = tempfile::tempdir().unwrap();
        let digest = sha256(b"payload");
        // A directory where the `digest` file belongs: the recovery file cannot be written.
        std::fs::create_dir_all(blob_dir_path(root.path(), REG, &digest).join("digest")).unwrap();
        // A directory is not a readable digest file, so the foreign-shard check passes and the write fails.
        let error = write_all(root.path(), &digest, b"payload").expect_err("digest file unwritable");
        assert!(matches!(error, LookupError::Io { .. }), "{error:?}");
        assert!(
            !blob_data_path(root.path(), REG, &digest).exists(),
            "no data without its digest file"
        );
    }

    #[test]
    fn marker_with_unknown_version_refused_for_a_cache() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join(export_marker::MARKER_FILE),
            br#"{"format_version":9,"registries":{}}"#,
        )
        .unwrap();
        let cache = BlobCacheDir::at(root.path().to_path_buf());
        assert!(cache.try_note_registry(REG).is_err());
        let after = std::fs::read(root.path().join(export_marker::MARKER_FILE)).unwrap();
        assert!(String::from_utf8(after).unwrap().contains("\"format_version\":9"));
    }

    #[test]
    fn handles_on_one_dir_keep_each_others_registries() {
        let root = tempfile::tempdir().unwrap();
        let locks = tempfile::tempdir().unwrap();
        let first = BlobCacheDir::at(root.path().to_path_buf()).with_locks(locks.path().to_path_buf());
        let second = BlobCacheDir::at(root.path().to_path_buf()).with_locks(locks.path().to_path_buf());
        first.try_note_registry("one.test").unwrap();
        second.try_note_registry("two.test").unwrap();
        // `first` still holds its snapshot without `two.test`; it must not write that snapshot back.
        first.try_note_registry("three.test").unwrap();
        let marker = export_marker::read_marker(root.path()).unwrap().unwrap();
        let registries: Vec<_> = marker.registries.values().map(String::as_str).collect();
        assert_eq!(registries, ["one.test", "three.test", "two.test"]);
    }

    #[test]
    fn noting_a_registry_leaves_no_file_in_the_cache_but_the_marker() {
        let root = tempfile::tempdir().unwrap();
        let locks = tempfile::tempdir().unwrap();
        let cache = BlobCacheDir::at(root.path().to_path_buf()).with_locks(locks.path().to_path_buf());
        cache.writer(REG, &sha256(b"payload")).unwrap();
        let mut stack = vec![root.path().to_path_buf()];
        let mut files = Vec::new();
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                if entry.file_type().unwrap().is_dir() {
                    stack.push(entry.path());
                } else {
                    files.push(entry.path());
                }
            }
        }
        assert_eq!(files, [root.path().join(export_marker::MARKER_FILE)]);
    }

    #[test]
    fn unsupported_marker_is_refused_once_per_handle() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join(export_marker::MARKER_FILE),
            br#"{"format_version":9,"registries":{}}"#,
        )
        .unwrap();
        let cache = BlobCacheDir::at(root.path().to_path_buf());
        cache
            .try_note_registry(REG)
            .expect_err("first note reports the refusal");
        // Fixing the file would let a re-read succeed; the handle must not re-read.
        std::fs::remove_file(root.path().join(export_marker::MARKER_FILE)).unwrap();
        cache.try_note_registry(REG).expect("refusal is reported once");
        assert!(!root.path().join(export_marker::MARKER_FILE).exists());
    }

    #[test]
    fn publish_retries_when_the_occupant_vanishes() {
        let root = tempfile::tempdir().unwrap();
        let digest = sha256(b"payload");
        let dir = blob_dir_path(root.path(), REG, &digest);
        std::fs::create_dir_all(&dir).unwrap();
        let data = dir.join("data");
        std::fs::write(&data, b"squatter").unwrap();
        let mut temp = new_temp(root.path()).unwrap();
        temp.write_all(b"payload").unwrap();

        let mut looks = 0;
        let mut inspect = |dir: &Path, digest: &Digest| {
            looks += 1;
            if looks == 1 {
                std::fs::remove_file(dir.join("data")).unwrap();
                return Ok(Occupant::Absent);
            }
            inspect_occupant(dir, digest)
        };
        let outcome = publish_noclobber(temp, &dir, &digest, &mut inspect).expect("retry publishes");
        assert_eq!(outcome, WriteOutcome::Written);
        assert_eq!(std::fs::read(&data).unwrap(), b"payload");
    }

    #[test]
    fn marker_read_once_per_cache() {
        let root = tempfile::tempdir().unwrap();
        let cache = BlobCacheDir::at(root.path().to_path_buf());
        cache.try_note_registry(REG).unwrap();
        // Corrupting the file would fail a re-read; the cached marker answers a known registry.
        std::fs::write(root.path().join(export_marker::MARKER_FILE), b"not json").unwrap();
        cache.try_note_registry(REG).expect("known registry needs no re-read");
    }

    #[test]
    fn concurrent_writers_converge() {
        let root = tempfile::tempdir().unwrap();
        let digest = sha256(b"shared payload");
        let outcomes: Vec<WriteOutcome> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..16)
                .map(|_| scope.spawn(|| write_all(root.path(), &digest, b"shared payload").expect("write")))
                .collect();
            handles.into_iter().map(|h| h.join().expect("thread")).collect()
        });
        assert_eq!(outcomes.iter().filter(|o| **o == WriteOutcome::Written).count(), 1);
        assert_eq!(
            std::fs::read(blob_data_path(root.path(), REG, &digest)).unwrap(),
            b"shared payload"
        );
        assert!(tmp_is_empty(root.path()));
    }

    #[test]
    fn registry_named_like_the_marker_is_refused_and_the_marker_survives() {
        let root = tempfile::tempdir().unwrap();
        let cache = BlobCacheDir::at(root.path().to_path_buf());
        cache.writer(REG, &sha256(b"payload")).unwrap();
        let marker = root.path().join(export_marker::MARKER_FILE);
        let before = std::fs::read(&marker).unwrap();

        let error = cache
            .writer(export_marker::MARKER_FILE, &sha256(b"other"))
            .expect_err("no usable directory name");
        assert!(
            matches!(&error, LookupError::Io { source, .. } if source.kind() == std::io::ErrorKind::InvalidInput),
            "{error:?}"
        );
        assert_eq!(std::fs::read(&marker).unwrap(), before);
    }
}
