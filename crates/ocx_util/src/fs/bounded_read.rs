// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Read a whole file under a byte ceiling, refusing anything that is not a regular file.
//!
//! Both guards are needed: `/dev/zero` reports length 0 and yields forever, a huge regular file passes the type check.

use std::path::{Path, PathBuf};

/// Failure modes of [`read_bounded`].
///
/// Not `#[non_exhaustive]`: a new variant must break each caller's mapping, not fall into a wildcard arm.
#[derive(Debug, thiserror::Error)]
pub enum BoundedReadError {
    #[error("not a regular file: {}", path.display())]
    NotRegularFile {
        /// The path that was refused.
        path: PathBuf,
    },
    #[error("'{}' is larger than {cap} bytes", path.display())]
    TooLarge {
        /// The path that was refused.
        path: PathBuf,
        /// The ceiling it passed.
        cap: u64,
    },
    #[error("I/O error reading '{}'", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl BoundedReadError {
    /// The refusal as a `std::io::Error` that never names the path, which the caller may redact as a secret.
    pub fn into_io_error(self) -> std::io::Error {
        match self {
            Self::Io { source, .. } => source,
            Self::NotRegularFile { .. } => std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a regular file"),
            Self::TooLarge { cap, .. } => {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("over the {cap}-byte cap"))
            }
        }
    }
}

/// [`read_bounded`] on the blocking pool.
///
/// A panicked task is `Io` with `ErrorKind::Other`, never `NotFound`, so it cannot pass for an absent file.
///
/// # Errors
///
/// As [`read_bounded`].
pub async fn read_bounded_async(path: &Path, cap: u64) -> Result<Vec<u8>, BoundedReadError> {
    let target = path.to_path_buf();
    match tokio::task::spawn_blocking(move || read_bounded(&target, cap)).await {
        Ok(result) => result,
        Err(join) => Err(BoundedReadError::Io {
            path: path.to_path_buf(),
            source: std::io::Error::other(format!("bounded read task panicked: {join}")),
        }),
    }
}

/// Read all of `path` (blocking), refusing a non-regular file and anything over `cap` bytes.
///
/// # Errors
///
/// [`BoundedReadError::NotRegularFile`] for a directory, device or FIFO,
/// [`BoundedReadError::TooLarge`] over `cap`, else [`BoundedReadError::Io`].
pub fn read_bounded(path: &Path, cap: u64) -> Result<Vec<u8>, BoundedReadError> {
    let io = |source| BoundedReadError::Io {
        path: path.to_path_buf(),
        source,
    };
    let not_regular = || BoundedReadError::NotRegularFile {
        path: path.to_path_buf(),
    };

    // Pre-open: a writerless FIFO blocks `open(2)` forever, and Windows fails a directory open as access denied.
    // Unreadable metadata falls through so the open reports its own error.
    if std::fs::metadata(path).is_ok_and(|metadata| !metadata.is_file()) {
        return Err(not_regular());
    }
    let file = open_for_read(path).map_err(io)?;
    // Re-checked on the handle: the path can be swapped between the pre-check and the open.
    let metadata = file.metadata().map_err(io)?;
    if !metadata.is_file() {
        return Err(not_regular());
    }
    read_under_cap(file, cap).map_err(|error| match error {
        None => BoundedReadError::TooLarge {
            path: path.to_path_buf(),
            cap,
        },
        Some(source) => io(source),
    })
}

/// `O_NONBLOCK` so a FIFO swapped in after the pre-stat cannot block the open.
#[cfg(unix)]
fn open_for_read(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;

    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
}

#[cfg(not(unix))]
fn open_for_read(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
}

/// The ceiling over a reader: `Err(None)` is over-cap, `Err(Some(_))` is I/O.
///
/// `take(cap + 1)`, not `metadata.len()`: a file can grow between stat and read.
fn read_under_cap(source: impl std::io::Read, cap: u64) -> Result<Vec<u8>, Option<std::io::Error>> {
    use std::io::Read as _;

    let mut content = Vec::new();
    source.take(cap + 1).read_to_end(&mut content).map_err(Some)?;
    if content.len() as u64 > cap {
        return Err(None);
    }
    Ok(content)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cap bounds the **read**, not just the answer.
    ///
    /// A path-level test cannot see this: delete the `take(cap + 1)` and it
    /// stays green, because the length check refuses the same file with the
    /// same error — after `read_to_end` has pulled every byte of it into
    /// memory, which is the CWE-400 the cap exists to stop. Nothing in the
    /// *result* distinguishes the two, so the assertion is on what the source
    /// still holds.
    #[test]
    fn the_cap_stops_the_read_rather_than_consuming_the_whole_source() {
        use std::io::Read as _;

        let mut source = std::io::repeat(b'x').take(4096);
        assert!(
            read_under_cap(&mut source, 512).is_err(),
            "a source past the cap is refused"
        );
        assert_eq!(
            source.limit(),
            4096 - 513,
            "the read must stop one byte past the cap; consuming the whole source means the \
             length check refused it and the `take` bound never ran"
        );
    }

    /// The three outcomes stay apart, because every caller maps them to a
    /// different error of its own.
    #[test]
    fn the_three_outcomes_are_distinguishable() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let path = dir.path().join("content");
        std::fs::write(&path, vec![b'x'; 17]).expect("write");

        assert_eq!(read_bounded(&path, 64).expect("under the cap reads"), vec![b'x'; 17]);
        assert_eq!(
            read_bounded(&path, 17).expect("exactly at the cap reads"),
            vec![b'x'; 17]
        );
        assert!(matches!(
            read_bounded(&path, 16),
            Err(BoundedReadError::TooLarge { cap: 16, .. })
        ));
        assert!(matches!(
            read_bounded(dir.path(), 64),
            Err(BoundedReadError::NotRegularFile { .. })
        ));
        let absent = dir.path().join("absent");
        let Err(BoundedReadError::Io { source, .. }) = read_bounded(&absent, 64) else {
            panic!("a missing file is an I/O failure, not a refusal");
        };
        assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
    }

    /// A FIFO with no writer blocks `open(2)` until one appears — with a bare
    /// `std::fs::read`, forever. Two guards keep that from happening (the
    /// pre-open metadata check, then `O_NONBLOCK` on the open itself), so
    /// this only reds once *both* are gone; the `recv_timeout` is what turns
    /// that red into a failure rather than a hung test.
    #[cfg(unix)]
    #[test]
    fn a_fifo_is_refused_without_blocking_the_open() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let fifo = dir.path().join("pipe");
        ocx_test_support::fifo::mkfifo(&fifo);

        let (sender, receiver) = std::sync::mpsc::channel();
        let target = fifo.clone();
        std::thread::spawn(move || {
            let _ = sender.send(read_bounded(&target, 64));
        });
        let outcome = receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the read must return promptly, not block in open(2) waiting for a writer");
        assert!(
            matches!(outcome, Err(BoundedReadError::NotRegularFile { .. })),
            "a FIFO is refused as not a regular file, got {outcome:?}"
        );
    }
}
