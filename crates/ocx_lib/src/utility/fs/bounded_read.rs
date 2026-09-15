// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Read a whole file under a byte ceiling, refusing anything that is not a
//! regular file.
//!
//! Both guards are load-bearing on any operator-supplied path, and neither is
//! sufficient alone. `--key file:/dev/zero` read until memory ran out
//! (CWE-400) while a caller here used a bare `std::fs::read`: a character
//! device reports length 0 and then yields forever, which the regular-file
//! check refuses; a merely enormous regular file passes that check and is what
//! the ceiling is for.
//!
//! The two are folded here rather than repeated per caller because they had
//! already been written twice, wording-identical and value-identical, and a
//! third caller was about to make it three. Callers keep their own error types
//! and their own wording — [`BoundedReadError`] separates the three outcomes
//! precisely so each can map them where they belong.

use std::path::{Path, PathBuf};

/// Failure modes of [`read_bounded`].
///
/// Deliberately **not** `#[non_exhaustive]`, against the usual rule for error
/// enums: every caller exists to map each outcome onto an error of its own, and
/// a wildcard arm would let a fourth variant inherit whichever mapping happened
/// to be last. Nothing outside this crate consumes it, so a new variant should
/// break each caller and make it choose.
#[derive(Debug, thiserror::Error)]
pub enum BoundedReadError {
    /// `path` is not a regular file — a directory, a device, a FIFO.
    #[error("not a regular file: {}", path.display())]
    NotRegularFile {
        /// The path that was refused.
        path: PathBuf,
    },
    /// The file holds more than `cap` bytes.
    #[error("'{}' is larger than {cap} bytes", path.display())]
    TooLarge {
        /// The path that was refused.
        path: PathBuf,
        /// The ceiling it passed.
        cap: u64,
    },
    /// I/O failure opening, stat-ing or reading the file.
    #[error("I/O error reading '{}'", path.display())]
    Io {
        /// The path that could not be read.
        path: PathBuf,
        /// What the filesystem raised.
        #[source]
        source: std::io::Error,
    },
}

/// Read all of `path`, refusing a non-regular file and anything over `cap`
/// bytes.
///
/// Blocking — wrap in `spawn_blocking` from an async caller. Never blocks on
/// the *open*: a FIFO named as the path is refused from its metadata before
/// `open(2)` — which POSIX blocks until a writer appears — is attempted, and
/// on Unix the open itself is `O_NONBLOCK`, so a FIFO swapped in between the
/// two still returns a handle the post-open check refuses (CWE-367; the
/// `file_transport.rs` `open_for_read` shape).
///
/// # Errors
///
/// [`BoundedReadError::NotRegularFile`] for a directory, device or FIFO,
/// [`BoundedReadError::TooLarge`] when the content passes `cap`, and
/// [`BoundedReadError::Io`] for anything the filesystem raised.
pub fn read_bounded(path: &Path, cap: u64) -> Result<Vec<u8>, BoundedReadError> {
    let io = |source| BoundedReadError::Io {
        path: path.to_path_buf(),
        source,
    };
    let not_regular = || BoundedReadError::NotRegularFile {
        path: path.to_path_buf(),
    };

    // Before opening: a FIFO with no writer blocks `open(2)` forever, and a
    // directory opens on Unix but not on Windows (`ERROR_ACCESS_DENIED`), so
    // this is also what keeps a directory `NotRegularFile` on both. A path
    // whose metadata cannot be read falls through to the open, whose own
    // error (`NotFound`, `PermissionDenied`) is the one to report. The
    // post-open check below stays: this one is on the path, and the path
    // can be swapped between here and the open.
    if std::fs::metadata(path).is_ok_and(|metadata| !metadata.is_file()) {
        return Err(not_regular());
    }
    let file = open_for_read(path).map_err(io)?;
    // After opening, not before: `metadata()` on the path would leave a window
    // in which the path is swapped between the check and the read.
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

/// Opens `path` for reading without blocking on a FIFO swapped in after the
/// pre-stat. `O_NONBLOCK` is a no-op on a regular file.
#[cfg(unix)]
fn open_for_read(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;

    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
}

/// Opens `path` for reading. No `O_NONBLOCK` equivalent applies — the named
/// pipes this guards against are a Unix concern.
#[cfg(not(unix))]
fn open_for_read(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
}

/// The ceiling itself, over a reader rather than a path.
///
/// Split out because it is the only shape in which the bound is *observable*:
/// over-cap and the length check raise the same outcome whether or not the
/// `take` is there, so nothing in the result says the read was bounded. Holding
/// the reader, a test can ask `Take::limit` how much of the source went
/// untouched — which goes to zero the moment the bound is gone.
///
/// `take(cap + 1)` rather than trusting `metadata.len()`: a file can grow
/// between the stat and the read. The extra byte is what distinguishes "exactly
/// at the cap" from "over it". `Err(None)` is over-cap, `Err(Some(_))` is I/O.
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
}
