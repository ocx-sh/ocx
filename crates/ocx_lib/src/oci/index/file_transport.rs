// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `file://` transport for a **shipped copy** of a static-file index
//! (`adr_servable_index_snapshot.md` Decision D, contracts C-015 – C-017).
//!
//! The air-gap half of [`IndexTransport`]: an operator copies
//! `$OCX_HOME/index/<source>/` onto a disconnected machine, points
//! `[registries."<ns>"] index` at `file:///srv/ocx-index`, and every fetch the
//! HTTPS transport would have made becomes a bounded read under one directory.
//! Read-only — the trait has no write path.
//!
//! Scheme policy stays out of this module. C-018's closed scheme set and
//! C-019's empty-authority rule live in
//! [`OcxIndex::resolve_base_url`](super::OcxIndex::resolve_base_url), the
//! single place a scheme is decided; it hands the `(base_url, root)` pair here
//! already checked. A second parse here would be a second place for the two to
//! disagree.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use tokio::fs;
use tokio::io::AsyncReadExt as _;

use super::{IndexFetch, IndexTransport};
use crate::Result;
use crate::oci::client::MAX_INDEX_DOCUMENT_BYTES;
use crate::utility::fs::path::join_under_root;

/// Ceiling on one [`IndexTransport::get`], mirroring the HTTPS sibling's
/// `INDEX_REQUEST_TIMEOUT` (`ocx_index.rs:108-116`).
const INDEX_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// [`IndexTransport`] backed by a directory tree instead of an HTTPS origin.
///
/// `base_url` is the `file://<abs>` prefix every URL must carry, stored
/// trailing-slash-trimmed; `root` is that same absolute path. Both are needed
/// because [`IndexTransport::get`] is handed a full URL and must recover the
/// tail relative to the base before it can touch the filesystem.
///
/// Every refusal is [`IndexHttpFailed`](super::error::Error::IndexHttpFailed),
/// which classifies as
/// [`ExitCode::Unavailable`](crate::cli::ExitCode::Unavailable) — **69**. A
/// refusal is never `Ok(IndexFetch::NotFound)`: under Decision A a base that
/// misses everything resolves as a valid, empty v1 index, so reporting
/// "refused" as "absent" silently erases packages from a resolve. That is the
/// defect this change exists to fix, and it is why the line is drawn at every
/// arm below.
///
/// # Contract
///
/// Enforced by [`get`](IndexTransport::get); C-015's outcome table in
/// `adr_servable_index_snapshot.md` is the full mapping.
///
/// 1. **Containment, no percent-decoding (C-016).** `url` must carry
///    `base_url` as a prefix *and* leave a remainder that is empty or starts
///    with `/`; a bare prefix test lets a sibling base `<base>2/p/ns/x.json`
///    read a wrong path under this root. The single leading `/` is stripped
///    and the tail joined through
///    [`join_under_root`](crate::utility::fs::path::join_under_root) as a
///    **literal** relative path — decoding would manufacture a `%2e%2e`
///    traversal vector out of what is, literally, a filename. `join_under_root`
///    folds `.`/`..` lexically and refuses only a *residual escaping* `..`, so
///    `a/../b` resolves to `root/b`.
/// 2. **Symlinks may stay inside the tree (C-016).** `rsync`/hardlink/symlink
///    staging is legitimate, so a link is not refused for being a link — but
///    the resolved target is canonicalized and must remain under a
///    canonicalized `root`. The lexical fold of rule 1 runs first and stays the
///    primary defence (it normalizes `..` before the OS sees the path, so a
///    symlinked directory component cannot be re-expanded); canonicalization is
///    a second gate, not a replacement.
/// 3. **Regular files only (C-017),** checked on a pre-stat *and* re-checked on
///    the open handle. The two lookups are independent: a concurrent `rsync`,
///    or any local user with write access, can swap a FIFO in behind a passed
///    `is_file()`. The pre-stat is kept because it avoids `open()`ing device
///    nodes, which can have side effects.
/// 4. **Size cap, measured not declared (C-017).** At most
///    `MAX_INDEX_DOCUMENT_BYTES + 1` bytes are read and the count decides. A
///    `len()` from [`std::fs::Metadata`] describes a different moment than the
///    read, and a `/proc` entry declares 0 while yielding content.
/// 5. **A `root` that is not a directory is not an empty index (C-015).** Below
///    a missing root, or one that is a regular file, every read is `NotFound`
///    or `NotADirectory` — the two kinds that may read as absence — so the
///    absence arm re-checks the root and refuses instead.
///
/// `get` is additionally bounded by [`INDEX_REQUEST_TIMEOUT`] (CWE-400): on a
/// stalled network mount the first blocking call is the `stat` itself, upstream
/// of every type check, and each retry pins another `spawn_blocking` thread.
///
/// All reads go through [`tokio::fs`] — `std::fs` on an async path blocks a
/// runtime worker.
#[derive(Clone)]
pub struct FileIndexTransport {
    /// The `file://<abs>` prefix every URL handed to `get` must carry
    /// verbatim, at a path boundary.
    base_url: String,
    /// The containment root — the absolute path `base_url` names.
    root: PathBuf,
}

/// Strips `user[:password]@` userinfo from `url`'s authority before it lands in
/// an error or log line (CWE-532).
///
/// A `file://` base has an empty authority, but rule 1's prefix-mismatch arm is
/// by definition handed a *foreign* URL, which may carry credentials.
///
// Duplicated from `ocx_index::redact_url`, which is module-private and lives in
// a file another work package owns; fold the two together when both can be
// edited in one change.
fn redact_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    match authority.rsplit_once('@') {
        Some((_userinfo, host)) => format!("{scheme}://***@{host}{tail}"),
        None => url.to_string(),
    }
}

/// A C-015 refusal: [`IndexHttpFailed`](super::error::Error::IndexHttpFailed),
/// hence [`ExitCode::Unavailable`](crate::cli::ExitCode::Unavailable) (**69**).
///
/// One constructor so no arm can drift to another variant — and with it another
/// exit code — while the module grows.
fn refused(url: &str, source: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> crate::Error {
    super::error::Error::IndexHttpFailed {
        url: redact_url(url),
        source: source.into(),
    }
    .into()
}

/// Splits an I/O failure into the two kinds that may read as absence and
/// everything else (C-015).
///
/// `NotFound` and `NotADirectory` (a path component is a regular file) are the
/// whole absence set; `PermissionDenied`, `IsADirectory`, `ELOOP` and the rest
/// are refusals. Absence additionally requires `root` to be a **directory**:
/// those same two kinds are exactly what a base pointed at a missing path or at
/// a regular file produces for *every* read, and calling that an empty index is
/// C-015's own defect. Checking here rather than up front gates only the
/// `Ok(NotFound)` return and keeps the extra stat off the happy path.
async fn absent_or_refused(root: &Path, url: &str, source: std::io::Error) -> Result<IndexFetch> {
    if !matches!(source.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) {
        return Err(refused(url, source));
    }
    match fs::metadata(root).await {
        Ok(metadata) if metadata.is_dir() => Ok(IndexFetch::NotFound),
        Ok(_) => Err(refused(
            url,
            format!("index root {} is not a directory", root.display()),
        )),
        Err(source) => Err(refused(url, source)),
    }
}

/// Bounds one fetch in time (C-017, CWE-400); elapsing is a refusal.
///
/// Takes the deadline rather than reading [`INDEX_REQUEST_TIMEOUT`] directly so
/// the elapsed arm is testable in milliseconds: once the type checks are in
/// place no filesystem fixture stalls a fetch short of a real hung mount.
async fn bounded(
    url: &str,
    deadline: Duration,
    fetch: impl Future<Output = Result<IndexFetch>> + Send,
) -> Result<IndexFetch> {
    tokio::time::timeout(deadline, fetch)
        .await
        .unwrap_or_else(|_elapsed| Err(refused(url, format!("index read exceeded {deadline:?}"))))
}

/// Opens `path` for reading without blocking on a FIFO or device node.
///
/// `O_NONBLOCK` is a no-op on a regular file. It is what keeps the handle
/// re-check in [`FileIndexTransport::fetch`] reachable: without it a FIFO
/// swapped in after the pre-stat blocks inside `open()` itself, with nothing to
/// cancel (C-017).
#[cfg(unix)]
async fn open_for_read(path: &Path) -> std::io::Result<fs::File> {
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .await
}

/// Opens `path` for reading. No `O_NONBLOCK` equivalent applies — the named
/// pipes this guards against are a Unix concern.
#[cfg(not(unix))]
async fn open_for_read(path: &Path) -> std::io::Result<fs::File> {
    fs::File::open(path).await
}

impl FileIndexTransport {
    /// Build a transport serving `root` under `base_url`.
    ///
    /// Infallible:
    /// [`OcxIndex::resolve_base_url`](super::OcxIndex::resolve_base_url) is the
    /// gate (C-018/C-019) and only calls this once it has established an empty
    /// authority and an absolute path. Re-deriving either here would put the
    /// scheme decision in two places.
    ///
    /// `base_url` is stored trailing-slash-trimmed, matching `OcxIndex::new`
    /// (`ocx_index.rs:405`): every URL is minted as `format!("{base}/…")`, so a
    /// base configured with a trailing `/` would leave a `//`-prefixed
    /// remainder and refuse every fetch this transport exists to serve.
    pub fn new(base_url: String, root: PathBuf) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            root,
        }
    }

    /// The body of [`IndexTransport::get`], minus the timeout wrapper.
    async fn fetch(&self, url: &str) -> Result<IndexFetch> {
        // 1. Prefix *and* path boundary (C-016). `filter` is the boundary half:
        //    a sibling base sharing a string prefix (`<base>2/…`) leaves a tail
        //    that starts with neither nothing nor `/`, and must refuse rather
        //    than read a wrong path under this root.
        let tail = url
            .strip_prefix(&self.base_url)
            .filter(|tail| tail.is_empty() || tail.starts_with('/'))
            .ok_or_else(|| refused(url, "URL does not lie under the file:// index base"))?;

        // 2. Strip the one leading separator every minted URL carries, and
        //    change nothing else — the tail is a *literal* relative path, never
        //    percent-decoded (C-016). `join_under_root` owns lexical
        //    containment and runs before the OS sees the path, which is what
        //    stops a symlinked directory component being re-expanded by `..`.
        let path = join_under_root(&self.root, Path::new(tail.strip_prefix('/').unwrap_or(tail)))
            .map_err(|source| refused(url, source))?;

        // 3. Pre-stat, before `open()` so a device node is never opened at all
        //    (C-017). Follows symlinks on purpose — an operator-staged tree may
        //    legitimately link (C-016).
        let metadata = match fs::metadata(&path).await {
            Ok(metadata) => metadata,
            Err(source) => return absent_or_refused(&self.root, url, source).await,
        };
        if !metadata.file_type().is_file() {
            return Err(refused(url, "index object is not a regular file"));
        }

        // 4. Symlink containment (C-016). After the stat because canonicalizing
        //    a path that does not exist fails, and a clean miss must stay a
        //    miss; before the open because an out-of-tree target must not be
        //    opened at all. `root` is canonicalized too — the shipped tree may
        //    itself sit behind a link, and only like-for-like compares.
        let resolved = fs::canonicalize(&path).await.map_err(|source| refused(url, source))?;
        let canonical_root = fs::canonicalize(&self.root)
            .await
            .map_err(|source| refused(url, source))?;
        if !resolved.starts_with(&canonical_root) {
            return Err(refused(url, "index object resolves outside the index root"));
        }

        // 5. Open, then re-check the type on the *handle* (C-017): the pre-stat
        //    and the open are independent lookups, and anything that replaces
        //    the path between them passes the check above.
        let file = match open_for_read(&path).await {
            Ok(file) => file,
            Err(source) => return absent_or_refused(&self.root, url, source).await,
        };
        let opened = file.metadata().await.map_err(|source| refused(url, source))?;
        if !opened.file_type().is_file() {
            return Err(refused(url, "index object is not a regular file"));
        }

        // 6. Counted read (C-017): at most `cap + 1` bytes, so an over-cap body
        //    is detected from what actually arrived. Not `absent_or_refused` —
        //    this path was stat'd and opened, so absence is no longer a
        //    reachable meaning for a failure here.
        let mut bytes = Vec::new();
        let mut reader = file.take(MAX_INDEX_DOCUMENT_BYTES as u64 + 1);
        reader
            .read_to_end(&mut bytes)
            .await
            .map_err(|source| refused(url, source))?;
        if bytes.len() > MAX_INDEX_DOCUMENT_BYTES {
            return Err(refused(
                url,
                format!("index document exceeds the {MAX_INDEX_DOCUMENT_BYTES}-byte cap"),
            ));
        }

        Ok(IndexFetch::Found { bytes })
    }
}

#[async_trait]
impl IndexTransport for FileIndexTransport {
    /// Read the object `url` names from the shipped tree.
    ///
    /// Unconditional, like every [`IndexTransport`]: there is no ETag, no
    /// `If-None-Match`, and no revalidation anywhere in this subsystem.
    ///
    /// # Errors
    ///
    /// [`IndexHttpFailed`](super::error::Error::IndexHttpFailed) —
    /// [`ExitCode::Unavailable`](crate::cli::ExitCode::Unavailable) — for a URL
    /// that fails the prefix-and-boundary test, a tail that fails containment,
    /// a target resolving outside the root, a `root` that is not a directory, a
    /// target that is not a regular file, a body over the cap, an elapsed
    /// [`INDEX_REQUEST_TIMEOUT`], or any other I/O error.
    /// `Ok(IndexFetch::NotFound)` is reserved for
    /// [`ErrorKind::NotFound`](std::io::ErrorKind::NotFound) and
    /// [`ErrorKind::NotADirectory`](std::io::ErrorKind::NotADirectory) below a
    /// root that is a directory.
    async fn get(&self, url: &str) -> Result<IndexFetch> {
        bounded(url, INDEX_REQUEST_TIMEOUT, self.fetch(url)).await
    }

    fn box_clone(&self) -> Box<dyn IndexTransport> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::Error;
    use crate::cli::{ExitCode, classify_error};
    use crate::oci::client::MAX_INDEX_DOCUMENT_BYTES;
    use crate::oci::index::error::Error as IndexError;

    // ── fixture helpers ──────────────────────────────────────────────────────

    /// The `file://<abs>` base `resolve_base_url` (C-018) hands the transport
    /// for `root`.
    ///
    /// Built here rather than hard-coded so no test asserts a POSIX-absolute
    /// literal against a real tempdir path. The Windows drive form keeps the
    /// third slash (`file:///C:/…`) that C-018's table specifies.
    fn base_url_for(root: &Path) -> String {
        let path = root.display().to_string().replace('\\', "/");
        if path.starts_with('/') {
            format!("file://{path}")
        } else {
            format!("file:///{path}")
        }
    }

    /// A transport serving `root`, plus the exact base URL it was built with —
    /// every test URL is `format!("{base}/…")`, the shape `ocx_index.rs` mints.
    fn transport_for(root: &Path) -> (FileIndexTransport, String) {
        let base_url = base_url_for(root);
        (FileIndexTransport::new(base_url.clone(), root.to_path_buf()), base_url)
    }

    /// Asserts the C-015 refusal arm: [`IndexError::IndexHttpFailed`],
    /// classifying as [`ExitCode::Unavailable`] (**69**).
    ///
    /// 75 ([`ExitCode::TempFail`]) is deliberately never asserted anywhere in
    /// this module — nothing on this path returns it, and 75 would declare a
    /// retry safe when re-reading an unreadable or over-cap file cannot help.
    #[track_caller]
    fn assert_refused(outcome: Result<IndexFetch>, what: &str) {
        let error = match outcome {
            Ok(fetch) => panic!("{what}: expected a refusal, got Ok({fetch:?})"),
            Err(error) => error,
        };
        assert!(
            matches!(error, Error::OciIndex(IndexError::IndexHttpFailed { .. })),
            "{what}: expected IndexHttpFailed, got {error:?}"
        );
        assert_eq!(
            classify_error(&error),
            ExitCode::Unavailable,
            "{what}: a refusal on this path is 69 (Unavailable)"
        );
    }

    /// Asserts a served object and its exact bytes.
    #[track_caller]
    fn assert_found(outcome: Result<IndexFetch>, expected: &[u8], what: &str) {
        match outcome {
            Ok(IndexFetch::Found { bytes }) => assert_eq!(bytes, expected, "{what}: wrong bytes"),
            other => panic!("{what}: expected Found, got {other:?}"),
        }
    }

    /// Asserts the one arm that may read as absence: a clean per-object miss.
    #[track_caller]
    fn assert_missing(outcome: Result<IndexFetch>, what: &str) {
        match outcome {
            Ok(IndexFetch::NotFound) => {}
            other => panic!("{what}: expected Ok(NotFound), got {other:?}"),
        }
    }

    // ── C-015: the seven-row outcome table ───────────────────────────────────

    /// C-015 row 1 — an existing readable file is served verbatim.
    #[tokio::test]
    async fn c015_existing_readable_file_returns_exact_bytes() {
        let body = br#"{"schemaVersion":1,"tags":{}}"#;
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("p/kitware")).expect("mkdir p/kitware");
        std::fs::write(tmp.path().join("p/kitware/cmake.json"), body).expect("write root document");

        let (transport, base) = transport_for(tmp.path());
        let outcome = transport.get(&format!("{base}/p/kitware/cmake.json")).await;

        assert_found(outcome, body, "an existing readable root document");
    }

    /// C-015 row 2 — `ErrorKind::NotFound` **below an existing root** is the
    /// clean per-object miss, and the only ENOENT that may read as absence.
    #[tokio::test]
    async fn c015_absent_object_below_an_existing_root_is_a_miss() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("p/kitware")).expect("mkdir p/kitware");

        let (transport, base) = transport_for(tmp.path());
        let outcome = transport.get(&format!("{base}/p/kitware/cmake.json")).await;

        assert_missing(outcome, "an absent object below an existing root");
    }

    /// C-015 row 3 — `ErrorKind::NotADirectory`: a path component is a regular
    /// file, so nothing can exist below it. Absence, not a fault.
    #[tokio::test]
    async fn c015_path_component_that_is_a_regular_file_is_a_miss() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("p"), b"a regular file where a directory would be")
            .expect("write the blocking component");

        let (transport, base) = transport_for(tmp.path());
        let outcome = transport.get(&format!("{base}/p/kitware/cmake.json")).await;

        assert_missing(outcome, "a path component that is a regular file");
    }

    /// C-015 row 4 — `ErrorKind::IsADirectory` is a refusal, never a miss.
    #[tokio::test]
    async fn c015_directory_target_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("p/kitware")).expect("mkdir p/kitware");

        let (transport, base) = transport_for(tmp.path());
        let outcome = transport.get(&format!("{base}/p/kitware")).await;

        assert_refused(outcome, "a directory target");
    }

    /// C-015 row 5 — `PermissionDenied` is a refusal. Mapping it to `NotFound`
    /// would, under decision A, promote an unreadable tree to a valid empty v1
    /// index.
    ///
    /// Skips gracefully when running as root (DAC mode bits are ignored, so the
    /// unreadable condition cannot be constructed).
    #[tokio::test]
    #[cfg(unix)]
    async fn c015_permission_denied_is_refused() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("p/kitware")).expect("mkdir p/kitware");
        let document = tmp.path().join("p/kitware/cmake.json");
        std::fs::write(&document, br#"{"schemaVersion":1}"#).expect("write root document");
        std::fs::set_permissions(&document, std::fs::Permissions::from_mode(0o000)).expect("chmod 000");

        // The probe IS the precondition: if a chmod-000 file still reads, this
        // process is root and the row is unconstructible here.
        if std::fs::read(&document).is_ok() {
            eprintln!("skipping c015_permission_denied_is_refused: running as root");
            return;
        }

        let (transport, base) = transport_for(tmp.path());
        let outcome = transport.get(&format!("{base}/p/kitware/cmake.json")).await;

        assert_refused(outcome, "an unreadable (mode 000) root document");
    }

    /// C-015 row 6 — any other I/O error is a refusal. A self-referential
    /// symlink yields `ELOOP`, which is neither of the two absence kinds and
    /// neither `IsADirectory` nor `PermissionDenied`.
    #[tokio::test]
    #[cfg(unix)]
    async fn c015_other_io_error_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("p/kitware")).expect("mkdir p/kitware");
        std::os::unix::fs::symlink("cmake.json", tmp.path().join("p/kitware/cmake.json")).expect("symlink loop");

        let (transport, base) = transport_for(tmp.path());
        let outcome = transport.get(&format!("{base}/p/kitware/cmake.json")).await;

        assert_refused(outcome, "a symlink loop (ELOOP)");
    }

    /// C-015 row 7 — **the row that matters most.** A `root` that does not
    /// exist is a misconfigured base, not an empty mirror.
    ///
    /// Every read below a missing root surfaces `ErrorKind::NotFound`, so a
    /// transport that took the ENOENT row literally would report the whole
    /// index as absent. Under decision A a missing `config.json` means format
    /// version 1, so that base would resolve as a valid, empty v1 index and
    /// every package in it would report a clean miss — this ADR's own defect,
    /// reproduced at a new layer.
    #[tokio::test]
    async fn c015_absent_root_is_refused_not_an_empty_index() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("shipped-index-that-never-landed");

        let (transport, base) = transport_for(&root);

        assert_refused(
            transport.get(&format!("{base}/config.json")).await,
            "config.json below a non-existent root (a v1-empty-index misread)",
        );
        assert_refused(
            transport.get(&format!("{base}/p/kitware/cmake.json")).await,
            "a package root below a non-existent root",
        );
    }

    /// C-015 row 7, the half `try_exists` misses — a root that **exists but is
    /// not a directory**.
    ///
    /// An operator typo, or an `rsync` that landed the tree as a tarball, points
    /// the base at a regular file. `join_under_root` is purely lexical and
    /// happily yields `<file>/config.json`; every read below it is `ENOTDIR`,
    /// which the table maps to absence. Under decision A the base would then
    /// resolve as a valid, empty v1 index — the same silent-empty-mirror row 7
    /// exists to prevent, one character away from the case that is guarded.
    #[tokio::test]
    async fn c015_root_that_is_not_a_directory_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("ocx-index.tar");
        std::fs::write(&root, b"a shipped index that landed as a file").expect("write the root file");

        let (transport, base) = transport_for(&root);

        assert_refused(
            transport.get(&format!("{base}/config.json")).await,
            "config.json below a root that is a regular file",
        );
        assert_refused(
            transport.get(&format!("{base}/p/kitware/cmake.json")).await,
            "a package root below a root that is a regular file",
        );
    }

    // ── C-016: containment, and no percent-decoding ──────────────────────────

    /// C-016 — a URL that does not carry the base at all is a refusal, never a
    /// silent miss.
    #[tokio::test]
    async fn c016_url_outside_the_base_is_refused_not_a_miss() {
        let tmp = tempfile::tempdir().expect("tempdir");

        let (transport, _base) = transport_for(tmp.path());
        let outcome = transport.get("https://elsewhere.example/config.json").await;

        assert_refused(outcome, "a URL from a foreign base");
    }

    /// C-016 — the prefix-mismatch arm is handed a foreign URL by definition,
    /// so the error must not echo its userinfo (CWE-532).
    #[tokio::test]
    async fn c016_foreign_url_userinfo_is_redacted_in_the_refusal() {
        let tmp = tempfile::tempdir().expect("tempdir");

        let (transport, _base) = transport_for(tmp.path());
        let outcome = transport
            .get("https://robot:hunter2@elsewhere.example/config.json")
            .await;

        match outcome {
            Err(Error::OciIndex(IndexError::IndexHttpFailed { url, .. })) => {
                assert!(!url.contains("hunter2"), "the refusal leaked a password: {url}");
                assert_eq!(url, "https://***@elsewhere.example/config.json");
            }
            other => panic!("a foreign URL must be refused, got {other:?}"),
        }
    }

    /// C-016 — the path-boundary half of the prefix test.
    ///
    /// With base `<tmp>/index`, the URL `<tmp>/index2/p/kitware/cmake.json`
    /// passes a bare `starts_with`, yields the tail `2/p/kitware/cmake.json`,
    /// and reads `<tmp>/index/2/p/kitware/cmake.json` — contained, so not an
    /// escape, but a foreign-base URL silently demoted to a wrong-path hit.
    /// The decoy below is planted at exactly that wrong path so a bare-prefix
    /// transport answers `Found` and fails this test loudly.
    #[tokio::test]
    async fn c016_sibling_base_sharing_a_string_prefix_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("index");
        std::fs::create_dir_all(root.join("2/p/kitware")).expect("mkdir the wrong-path decoy");
        std::fs::write(root.join("2/p/kitware/cmake.json"), b"WRONG PATH").expect("write the decoy");

        let (transport, base) = transport_for(&root);
        // `{base}2/…` — a sibling directory, not a path boundary under `base`.
        let outcome = transport.get(&format!("{base}2/p/kitware/cmake.json")).await;

        assert_refused(outcome, "a sibling base sharing a string prefix");
    }

    /// C-016 — the empty-remainder arm: `get(base_url)` exactly.
    ///
    /// The `filter` must **accept** an empty remainder; `join_under_root` then
    /// maps the empty tail to the containment root itself, which is a directory
    /// and so is refused on the regular-file rule. Unreachable from a minted URL
    /// — every one is `format!("{base}/…")` — but it is a stated clause.
    ///
    /// Asserted on the *source*, not just the variant: a `filter` that rejected
    /// the empty remainder would refuse too, with the same variant and the same
    /// exit code, so only the message distinguishes which arm ran.
    #[tokio::test]
    async fn c016_url_equal_to_the_base_resolves_to_the_root_and_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");

        let (transport, base) = transport_for(tmp.path());
        let outcome = transport.get(&base).await;

        match outcome {
            Err(Error::OciIndex(IndexError::IndexHttpFailed { source, .. })) => assert!(
                source.to_string().contains("regular file"),
                "the empty remainder must reach the filesystem and be refused on the \
                 regular-file rule, not rejected by the path-boundary filter; got: {source}"
            ),
            other => panic!("a URL equal to the base must be refused, got {other:?}"),
        }
    }

    /// C-016 — the tail is a **literal** relative path: `%2e%2e` is a filename,
    /// not `..`.
    ///
    /// The directory is literally named `%2e%2e`, and a decoy sits at the
    /// location a percent-decoding transport would reach instead — so a
    /// decoder returns the wrong bytes rather than merely missing.
    #[tokio::test]
    async fn c016_percent_encoded_dot_dot_stays_literal() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("p/%2e%2e")).expect("mkdir the literal %2e%2e directory");
        std::fs::write(tmp.path().join("p/%2e%2e/cmake.json"), b"literal").expect("write the literal target");
        // Where `p/../cmake.json` would land if `%2e%2e` were decoded to `..`.
        std::fs::write(tmp.path().join("cmake.json"), b"decoded").expect("write the decode decoy");

        let (transport, base) = transport_for(tmp.path());
        let outcome = transport.get(&format!("{base}/p/%2e%2e/cmake.json")).await;

        assert_found(outcome, b"literal", "a literal %2e%2e path component");
    }

    /// C-016 — a `..` sequence that **escapes** the root is a refusal.
    #[tokio::test]
    async fn c016_dot_dot_escaping_the_root_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("p")).expect("mkdir p");

        let (transport, base) = transport_for(tmp.path());
        let outcome = transport.get(&format!("{base}/p/../../etc/passwd")).await;

        assert_refused(outcome, "a `..` sequence escaping the root");
    }

    /// C-016 — the converse, and the easy one to get wrong: `join_under_root`
    /// folds `.`/`..` lexically and errors only on a **residual escaping**
    /// `..`, so `p/a/../b.json` resolves to `root/p/b.json` and succeeds.
    ///
    /// A test written from an "any `..` is refused" reading would assert a
    /// refusal the mandated primitive does not produce.
    #[tokio::test]
    async fn c016_dot_dot_contained_within_the_root_resolves_and_succeeds() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("p")).expect("mkdir p");
        std::fs::write(tmp.path().join("p/b.json"), b"contained").expect("write the folded target");

        let (transport, base) = transport_for(tmp.path());
        let outcome = transport.get(&format!("{base}/p/a/../b.json")).await;

        assert_found(outcome, b"contained", "a `..` folded inside the root");
    }

    /// C-016 — exactly one leading `/` is stripped, so a doubled separator
    /// leaves an absolute tail, which `join_under_root` refuses.
    #[tokio::test]
    async fn c016_absolute_tail_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");

        let (transport, base) = transport_for(tmp.path());
        // One `/` stripped leaves `/etc/passwd` — an absolute component.
        let outcome = transport.get(&format!("{base}//etc/passwd")).await;

        assert_refused(outcome, "an absolute tail component");
    }

    /// C-016 — Windows drive-letter and UNC tails are refused on every
    /// platform (`join_under_root` rejects them host-independently).
    #[tokio::test]
    async fn c016_windows_drive_and_unc_tails_are_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");

        let (transport, base) = transport_for(tmp.path());

        assert_refused(
            transport.get(&format!("{base}/C:/Windows/win.ini")).await,
            "a Windows drive-letter tail",
        );
        // One `/` stripped leaves `//srv/share/x.json` — a UNC form.
        assert_refused(transport.get(&format!("{base}///srv/share/x.json")).await, "a UNC tail");
    }

    /// C-016 — a base configured with a trailing `/` still serves.
    ///
    /// `new` trims it, so the boundary test sees the same prefix a minted
    /// `format!("{base}/…")` URL carries. Untrimmed, every remainder would
    /// begin `//`, leave an absolute tail after the single-separator strip, and
    /// report the whole index as refused.
    #[tokio::test]
    async fn c016_trailing_slash_base_still_serves() {
        let body = br#"{"schemaVersion":1,"tags":{}}"#;
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("p/kitware")).expect("mkdir p/kitware");
        std::fs::write(tmp.path().join("p/kitware/cmake.json"), body).expect("write root document");

        let base = base_url_for(tmp.path());
        let transport = FileIndexTransport::new(format!("{base}/"), tmp.path().to_path_buf());
        let outcome = transport.get(&format!("{base}/p/kitware/cmake.json")).await;

        assert_found(outcome, body, "a base configured with a trailing slash");
    }

    /// C-016 — symlinks **inside** the tree are permitted: a shipped copy is
    /// operator-managed and `rsync`/hardlink/symlink layouts are legitimate
    /// ways to stage one, so canonicalization must not break them.
    #[tokio::test]
    #[cfg(unix)]
    async fn c016_symlinked_file_inside_the_tree_is_served() {
        let body = br#"{"schemaVersion":1,"tags":{}}"#;
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("p/kitware")).expect("mkdir p/kitware");
        std::fs::create_dir_all(tmp.path().join("staging")).expect("mkdir staging");
        std::fs::write(tmp.path().join("staging/real.json"), body).expect("write the link target");
        // Across directories inside the tree — the shape an rsync-staged copy
        // produces, and the one a naive same-directory test would not cover.
        std::os::unix::fs::symlink("../../staging/real.json", tmp.path().join("p/kitware/cmake.json"))
            .expect("symlink");

        let (transport, base) = transport_for(tmp.path());
        let outcome = transport.get(&format!("{base}/p/kitware/cmake.json")).await;

        assert_found(outcome, body, "a symlinked document inside the tree");
    }

    /// C-016 — a symlink **out** of the tree is a refusal.
    ///
    /// The tail is fully contained, so the lexical gate passes; `metadata` and
    /// `File::open` both follow the link, so without the canonicalization gate
    /// `get` returns the target's bytes as an index document — an arbitrary
    /// file read, whose content `persist_published_root` would then write into
    /// the local store. The target lives outside the root but inside the test's
    /// own tempdir, so the test needs no privileged path.
    #[tokio::test]
    #[cfg(unix)]
    async fn c016_symlink_escaping_the_tree_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let outside = tmp.path().join("outside-the-root");
        std::fs::write(&outside, b"SECRET").expect("write the out-of-tree target");
        let root = tmp.path().join("index");
        std::fs::create_dir_all(root.join("p/kitware")).expect("mkdir p/kitware");
        std::os::unix::fs::symlink(&outside, root.join("p/kitware/cmake.json")).expect("symlink out of the tree");

        let (transport, base) = transport_for(&root);
        let outcome = transport.get(&format!("{base}/p/kitware/cmake.json")).await;

        assert_refused(outcome, "a symlink resolving outside the index root");
    }

    // ── C-017: bounds ────────────────────────────────────────────────────────

    /// C-017 — the boundary from below: a body of exactly
    /// `MAX_INDEX_DOCUMENT_BYTES` is served.
    #[tokio::test]
    async fn c017_body_of_exactly_the_cap_is_served() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("p/kitware")).expect("mkdir p/kitware");
        std::fs::write(
            tmp.path().join("p/kitware/cmake.json"),
            vec![b'x'; MAX_INDEX_DOCUMENT_BYTES],
        )
        .expect("write a cap-sized body");

        let (transport, base) = transport_for(tmp.path());
        let outcome = transport.get(&format!("{base}/p/kitware/cmake.json")).await;

        // Asserted field-wise rather than through `assert_found`: a failing
        // 32 MiB `assert_eq!` would dump both bodies into the test log.
        match outcome {
            Ok(IndexFetch::Found { bytes }) => {
                assert_eq!(
                    bytes.len(),
                    MAX_INDEX_DOCUMENT_BYTES,
                    "a cap-sized body must arrive whole"
                );
                assert!(
                    bytes.iter().all(|byte| *byte == b'x'),
                    "a cap-sized body must arrive intact"
                );
            }
            other => panic!("a body of exactly the cap must be served, got {other:?}"),
        }
    }

    /// C-017 — the boundary from above: one byte over the cap is a refusal.
    #[tokio::test]
    async fn c017_body_one_byte_over_the_cap_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("p/kitware")).expect("mkdir p/kitware");
        std::fs::write(
            tmp.path().join("p/kitware/cmake.json"),
            vec![b'x'; MAX_INDEX_DOCUMENT_BYTES + 1],
        )
        .expect("write an over-cap body");

        let (transport, base) = transport_for(tmp.path());
        let outcome = transport.get(&format!("{base}/p/kitware/cmake.json")).await;

        assert_refused(outcome, "a body one byte over the cap");
    }

    /// C-017 — the read is **counted**, never sized from file metadata.
    ///
    /// `/proc/self/cmdline` is the one place metadata and content demonstrably
    /// disagree without a race: `stat` reports length 0 while the file yields
    /// this process's argv. A transport that sized its read from
    /// [`std::fs::Metadata::len`] returns an empty body here; a counted read
    /// returns the real bytes.
    ///
    /// Named for what it proves. The over-cap direction — metadata declaring
    /// *less* than a body that exceeds the cap — needs a concurrent writer,
    /// i.e. a racy fixture, and is pinned instead by the sized fixture above.
    #[tokio::test]
    #[cfg(target_os = "linux")]
    async fn c017_read_is_not_sized_from_file_metadata() {
        let procfs = std::path::PathBuf::from("/proc");
        let declared = match std::fs::metadata("/proc/self/cmdline") {
            Ok(metadata) => metadata.len(),
            Err(error) => {
                eprintln!("skipping c017_read_is_not_sized_from_file_metadata: /proc: {error}");
                return;
            }
        };
        if declared != 0 {
            eprintln!(
                "skipping c017_read_is_not_sized_from_file_metadata: \
                 /proc/self/cmdline declares {declared} bytes, so metadata and content agree here"
            );
            return;
        }

        let (transport, base) = transport_for(&procfs);
        let outcome = transport.get(&format!("{base}/self/cmdline")).await;

        match outcome {
            Ok(IndexFetch::Found { bytes }) => assert!(
                !bytes.is_empty(),
                "a metadata-sized read returns an empty body for a file whose stat length is 0; \
                 the read must be counted from the actual content"
            ),
            other => {
                panic!("a zero-length-metadata regular file must still be read, got {other:?}")
            }
        }
    }

    /// C-017 — a fetch that outruns its deadline is a refusal (**69**), not a
    /// hang.
    ///
    /// Driven through [`bounded`] with a future that never completes: with the
    /// type checks in place, nothing short of a genuinely hung network mount
    /// stalls a fetch, and pinning the elapsed arm must not depend on one. The
    /// deadline is a parameter for exactly this reason; production passes
    /// [`INDEX_REQUEST_TIMEOUT`].
    #[tokio::test]
    async fn c017_a_fetch_that_outruns_its_deadline_is_refused() {
        let outcome = bounded(
            "file:///srv/ocx-index/config.json",
            Duration::from_millis(10),
            std::future::pending::<Result<IndexFetch>>(),
        )
        .await;

        assert_refused(outcome, "a fetch that outran its deadline");
    }

    /// C-017 — a FIFO under the root is refused, and the refusal must arrive
    /// rather than block.
    ///
    /// The byte cap bounds memory, not **time**: a blocking `open()` on a
    /// writer-less FIFO never returns. So the read is driven from a worker
    /// thread and the test thread waits with a deadline — a regression FAILS
    /// the suite instead of wedging CI. That is also why this test is `#[test]`
    /// with its own runtime rather than `#[tokio::test]`: a
    /// `tokio::time::timeout` awaited in the same task cannot fire while that
    /// task is blocked inside `poll`.
    #[test]
    #[cfg(unix)]
    fn c017_fifo_under_the_root_is_refused_and_does_not_hang() {
        use std::sync::mpsc;
        use std::time::Duration;

        const FIFO_DEADLINE: Duration = Duration::from_secs(10);

        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("p/kitware")).expect("mkdir p/kitware");
        mkfifo(&tmp.path().join("p/kitware/cmake.json"));

        let (transport, base) = transport_for(tmp.path());
        let url = format!("{base}/p/kitware/cmake.json");

        let (finished_tx, finished_rx) = mpsc::channel::<()>();
        let worker = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("worker runtime");
            let outcome = runtime.block_on(transport.get(&url));
            assert_refused(outcome, "a FIFO under the root");
            finished_tx.send(()).expect("the test thread is still listening");
        });

        match finished_rx.recv_timeout(FIFO_DEADLINE) {
            Ok(()) => worker.join().expect("worker thread"),
            // The worker panicked before sending: re-raise its panic here so
            // the failure reads as the real one, not as a dead channel.
            Err(mpsc::RecvTimeoutError::Disconnected) => std::panic::resume_unwind(
                worker
                    .join()
                    .expect_err("a disconnected channel means the worker panicked"),
            ),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("get() blocked on a writer-less FIFO for {FIFO_DEADLINE:?} — the regular-file check is missing")
            }
        }
    }

    /// C-017 — the regular-file rule holds on the **open handle**, not on a
    /// path stat'd earlier.
    ///
    /// `metadata(&path)` and `open(&path)` are two independent lookups. A
    /// swapper thread renames a regular file and a FIFO onto the served path in
    /// a loop, so some iteration stats the regular file and opens the FIFO. The
    /// FIFO's pipe buffer is primed with bytes and kept alive by a held read
    /// fd, so a transport that trusts only the pre-stat answers `Found` with
    /// pipe content — an index document forged by whoever can write to the
    /// index directory. Nothing here can hang: the read fd means the write end
    /// is closed (EOF, not a block), and `open` carries `O_NONBLOCK`.
    ///
    /// Probabilistic in *catching* a regression, never in failing: every
    /// outcome it accepts is a correct one.
    #[tokio::test]
    #[cfg(unix)]
    async fn c017_regular_file_type_is_rechecked_on_the_open_handle() {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        const BODY: &[u8] = br#"{"schemaVersion":1,"tags":{}}"#;
        const SWAP_ROUNDS: usize = 500;

        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("p/kitware");
        std::fs::create_dir_all(&dir).expect("mkdir p/kitware");
        let served = dir.join("cmake.json");
        let regular = dir.join("staged-regular");
        let fifo = dir.join("staged-fifo");
        std::fs::write(&regular, BODY).expect("write the regular staging file");
        mkfifo(&fifo);

        // Prime the pipe buffer and keep it alive. `O_RDONLY | O_NONBLOCK` on a
        // FIFO returns immediately; the write end is opened, filled and dropped,
        // so a reader sees the bytes and then EOF rather than blocking.
        let _pipe_alive = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&fifo)
            .expect("hold the fifo read end");
        std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&fifo)
            .expect("open the fifo write end")
            .write_all(b"FORGED BY A PIPE")
            .expect("prime the pipe buffer");

        let swapping = Arc::new(AtomicBool::new(true));
        let swapper = std::thread::spawn({
            let swapping = Arc::clone(&swapping);
            let (served, regular, fifo) = (served.clone(), regular.clone(), fifo.clone());
            move || {
                while swapping.load(Ordering::Relaxed) {
                    // Errors are expected and ignored: the loser of a rename
                    // race just retries on the next pass.
                    let _ = std::fs::rename(&regular, &served);
                    let _ = std::fs::rename(&served, &regular);
                    let _ = std::fs::rename(&fifo, &served);
                    let _ = std::fs::rename(&served, &fifo);
                }
            }
        });

        let (transport, base) = transport_for(tmp.path());
        let url = format!("{base}/p/kitware/cmake.json");
        for _ in 0..SWAP_ROUNDS {
            match transport.get(&url).await {
                // The regular file, or a rename window in which nothing is at
                // the served path.
                Ok(IndexFetch::Found { bytes }) => assert_eq!(
                    bytes, BODY,
                    "the transport served bytes that are not the staged document — \
                     the file type was trusted from the pre-stat, not the open handle"
                ),
                Ok(IndexFetch::NotFound) => {}
                Err(error) => assert!(
                    matches!(error, Error::OciIndex(IndexError::IndexHttpFailed { .. })),
                    "every refusal on this path is IndexHttpFailed, got {error:?}"
                ),
            }
        }

        swapping.store(false, Ordering::Relaxed);
        swapper.join().expect("swapper thread");
    }

    /// `mkfifo(2)`, the one filesystem object `std::fs` cannot create.
    #[cfg(unix)]
    #[track_caller]
    fn mkfifo(path: &Path) {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt as _;

        let c_path = CString::new(path.as_os_str().as_bytes()).expect("a tempdir path holds no NUL");
        // SAFETY: `c_path` is a NUL-terminated C string alive for the whole
        // call, and `mkfifo` only reads it.
        let created = unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) };
        assert_eq!(created, 0, "mkfifo failed: {}", std::io::Error::last_os_error());
    }
}
