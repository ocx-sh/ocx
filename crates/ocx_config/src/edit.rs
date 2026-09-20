// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The one locked read-modify-write for a `config.toml` (ocx#468).
//!
//! Every writer of `$OCX_HOME/config.toml` — the surgical `[shell]` toggles
//! (`setup::shell_config::set`) and the `OCX_EXTRA_CA_CERTS`
//! persistence in `ocx self setup` phase 0.5 through [`edit`], the fenced
//! `[managed]` seed through [`edit_text`] — goes through this module, so two
//! concurrent `ocx` invocations serialize on one cross-process lock instead of
//! each reading the file, rendering its own edit and publishing over the
//! other's. The file is replaced by atomic rename, so its inode rotates on
//! every write and a lock on the data file would strand on the old inode:
//! per the arch-principles Locking Policy the lock is a [`lock_scoped`]
//! entry under `$OCX_HOME/locks`, keyed by the parent directory's identity
//! and the file name, never a sidecar beside the file.
//!
//! One home, three closures: the caller supplies the edit as a closure over
//! the parsed [`DocumentMut`] (or, for the fence, over the text); this module
//! owns the bounded read, the parse, the render, the unchanged check, the
//! size ceiling, the dry-run gate and the atomic write. A dry run is a
//! read-only walk of the same steps: it neither creates the directory nor
//! takes the lock, so `--dry-run` on a fresh machine leaves `$OCX_HOME` absent.

use std::path::{Path, PathBuf};
use std::time::Duration;

use toml_edit::DocumentMut;

use crate::loader::MAX_CONFIG_SIZE;
use ocx_util::fs::{BoundedReadError, lock_scoped, read_bounded, write_bytes_atomic};

/// The `scope` every `config.toml` edit lock is taken under.
const LOCK_SCOPE: &str = "config-edit";

/// How long an edit waits behind another editor before giving up. An edit
/// is a few file operations, so a holder that outlives this is stuck.
const LOCK_TIMEOUT: Duration = Duration::from_secs(5);

/// Test-only seam: hold the lock for this many milliseconds between the read
/// and the write, so an acceptance test can make two `ocx` invocations
/// overlap on purpose. Gated per the `__OCX_*` seam convention in
/// `subsystem-tests.md` — absent from release builds.
#[cfg(any(test, feature = "__testing"))]
const HOLD_OVERRIDE: &str = "__OCX_TESTING_CONFIG_EDIT_HOLD_MS";

/// What an [`edit`] did to the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditOutcome {
    /// The closure left the document as it was: nothing was written, and an
    /// absent file stays absent.
    Unchanged,
    /// The document changed and was written — or, under `dry_run`, would
    /// have been.
    Written,
}

/// Why an `edit` did not land.
#[derive(Debug, thiserror::Error)]
pub enum EditError {
    /// The directory, the lock, the read or the atomic write failed.
    #[error("I/O error for {}", path.display())]
    Io {
        /// The path that failed.
        path: PathBuf,
        /// What the filesystem raised.
        #[source]
        source: std::io::Error,
    },
    /// Another `ocx` held the edit lock for longer than the timeout. The same
    /// wording as `ocx.toml`'s `crate::project::ProjectErrorKind::Locked`,
    /// and the same code (75): a rerun may well succeed.
    #[error("{} is locked by another process", path.display())]
    Locked {
        /// The `config.toml` the edit waited for — never the hashed lock file.
        path: PathBuf,
    },
    /// The file on disk is not TOML; it is left exactly as it was.
    #[error("{} does not parse as TOML", path.display())]
    Parse {
        /// The file that would not parse.
        path: PathBuf,
        /// The parser's own report.
        #[source]
        source: toml_edit::TomlError,
    },
    /// The file parses but has a shape the closure cannot edit (a key that
    /// should be a table and is not); it is left exactly as it was.
    #[error("{}: {reason}", path.display())]
    Malformed {
        /// The file that was refused.
        path: PathBuf,
        /// The closure's one-line reason.
        reason: &'static str,
    },
    /// The document is, or would render, over the loader's own size ceiling
    /// (`MAX_CONFIG_SIZE`) — refused before any write, so a file the loader
    /// would refuse is never produced.
    #[error(
        "editing {} would leave it at {bytes} bytes, over the {MAX_CONFIG_SIZE}-byte config limit",
        path.display()
    )]
    TooLarge {
        /// The `config.toml` the write was refused for.
        path: PathBuf,
        /// The size the document has, or would have had.
        bytes: usize,
    },
}

/// Apply `apply` to `config_path` under the machine-global edit lock.
///
/// Creates the parent directory, takes the [`lock_scoped`] entry for
/// `(parent, "config-edit", file name)` under `locks_root`, then on the
/// blocking pool: reads the file bounded by `MAX_CONFIG_SIZE` (absent →
/// empty document), parses it, runs `apply`, renders. A render equal to the
/// original is [`EditOutcome::Unchanged`] and writes nothing — an absent
/// file is not created. A render over the ceiling is
/// [`EditError::TooLarge`]. Under `dry_run` the directory is not created and
/// the lock is not taken (there is no write to serialize); a changed render
/// is reported [`EditOutcome::Written`] without touching the file. Otherwise
/// it is published by atomic rename, and the lock is held until the closure
/// has returned and the write has landed.
///
/// `apply` returns `Err(reason)` for a document whose shape it cannot edit;
/// that surfaces as [`EditError::Malformed`] and the file is left alone.
///
/// # Errors
///
/// [`EditError`] — see its variants.
pub async fn edit<F>(locks_root: &Path, config_path: &Path, dry_run: bool, apply: F) -> Result<EditOutcome, EditError>
where
    F: FnOnce(&mut DocumentMut) -> Result<(), &'static str> + Send + 'static,
{
    edit_with(locks_root, config_path, dry_run, LOCK_TIMEOUT, move |path, original| {
        let mut document: DocumentMut = original.parse().map_err(|source| EditError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        apply(&mut document).map_err(|reason| EditError::Malformed {
            path: path.to_path_buf(),
            reason,
        })?;
        Ok(document.to_string())
    })
    .await
}

/// [`edit`] for a writer that works on the file's text, not its TOML: the
/// `[managed]` fence is a comment-delimited region `setup::rc_block`
/// addresses by line, which `toml_edit` cannot see. Same lock, bounded read,
/// unchanged gate, size ceiling, dry-run gate and atomic write; `apply` gets
/// the text as read (absent → `""`) and returns the text to publish.
///
/// # Errors
///
/// [`EditError`] — see its variants; [`EditError::Parse`] is unreachable here.
pub async fn edit_text<F>(
    locks_root: &Path,
    config_path: &Path,
    dry_run: bool,
    apply: F,
) -> Result<EditOutcome, EditError>
where
    F: FnOnce(&str) -> Result<String, &'static str> + Send + 'static,
{
    edit_with(locks_root, config_path, dry_run, LOCK_TIMEOUT, move |path, original| {
        apply(original).map_err(|reason| EditError::Malformed {
            path: path.to_path_buf(),
            reason,
        })
    })
    .await
}

/// The shared body of [`edit`] and [`edit_text`]: lock (unless `dry_run`),
/// then [`edit_blocking`] on the pool with `apply` as the text → text step.
///
/// `timeout` is [`LOCK_TIMEOUT`] for both public entries — it is a parameter
/// only so the test that proves a held lock surfaces as [`EditError::Locked`]
/// can wait out a short one instead of paying the shipped five seconds. The
/// shipped value is pinned in that same test.
async fn edit_with<F>(
    locks_root: &Path,
    config_path: &Path,
    dry_run: bool,
    timeout: Duration,
    apply: F,
) -> Result<EditOutcome, EditError>
where
    F: FnOnce(&Path, &str) -> Result<String, EditError> + Send + 'static,
{
    let io = |path: &Path, source| EditError::Io {
        path: path.to_path_buf(),
        source,
    };

    // A dry run writes nothing, so there is nothing to serialize: no
    // directory (an absent `$OCX_HOME` stays absent) and no lock file.
    let _guard = if dry_run {
        None
    } else {
        let parent = config_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let file_name = config_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        // The guarded directory must exist before it can be identified.
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|source| io(parent, source))?;
        let guard = lock_scoped(locks_root, LOCK_SCOPE, parent, &file_name, timeout)
            .await
            .map_err(|error| lock_error(config_path, error))?;
        Some(guard)
    };

    let path = config_path.to_path_buf();
    tokio::task::spawn_blocking(move || edit_blocking(&path, dry_run, apply))
        .await
        .map_err(|join| io(config_path, std::io::Error::other(join.to_string())))?
    // `_guard` drops here: after the closure returned and the write landed.
}

/// What a failed [`lock_scoped`] means for the edit of `config_path`: a
/// timeout is [`EditError::Locked`] on the config file (75), anything else
/// is the lock file's own I/O failure (74).
///
/// Takes [`lock_scoped`]'s own [`FileError`](ocx_util::error::FileError) rather
/// than the crate-wide `Error` it used to be widened into. The widening was
/// `From<FileError>`, whose only output is `Error::InternalFile(path, cause)`,
/// so the two arms this match kept are the same two it always took and the
/// third — the `other` wildcard — was unreachable: `lock_scoped` returns one
/// concrete type, never an enum. Same codes, 75 and 74, from the same inputs.
fn lock_error(config_path: &Path, error: ocx_util::error::FileError) -> EditError {
    if error.cause.kind() == std::io::ErrorKind::TimedOut {
        return EditError::Locked {
            path: config_path.to_path_buf(),
        };
    }
    EditError::Io {
        path: error.path,
        source: error.cause,
    }
}

/// Blocking body of [`edit_with`]: read → `apply` → gate → write.
fn edit_blocking<F>(path: &Path, dry_run: bool, apply: F) -> Result<EditOutcome, EditError>
where
    F: FnOnce(&Path, &str) -> Result<String, EditError>,
{
    let io = |source| EditError::Io {
        path: path.to_path_buf(),
        source,
    };

    // Bounded like the loader's own read: a file already over the ceiling
    // (reachable under `OCX_NO_CONFIG=1`, which skips the loader) is refused
    // as the same over-size document, not pulled whole into memory first.
    let original = match read_bounded(path, MAX_CONFIG_SIZE) {
        Ok(bytes) => {
            String::from_utf8(bytes).map_err(|error| io(std::io::Error::new(std::io::ErrorKind::InvalidData, error)))?
        }
        // A missing file is the create case, not a failure. Every other read
        // error is real and must not be papered over with an empty document —
        // that would silently replace a file we could not read.
        Err(BoundedReadError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(BoundedReadError::TooLarge { .. }) => {
            let bytes = std::fs::metadata(path).map_or(0, |metadata| metadata.len()) as usize;
            return Err(EditError::TooLarge {
                path: path.to_path_buf(),
                bytes,
            });
        }
        Err(refused) => return Err(io(refused.into_io_error())),
    };

    let rendered = apply(path, &original)?;

    #[cfg(any(test, feature = "__testing"))]
    if let Some(millis) = ocx_util::env::var(HOLD_OVERRIDE).and_then(|value| value.parse().ok()) {
        std::thread::sleep(Duration::from_millis(millis));
    }

    if rendered == original {
        return Ok(EditOutcome::Unchanged);
    }
    if rendered.len() as u64 > MAX_CONFIG_SIZE {
        return Err(EditError::TooLarge {
            path: path.to_path_buf(),
            bytes: rendered.len(),
        });
    }
    if dry_run {
        return Ok(EditOutcome::Written);
    }
    write_bytes_atomic(path, rendered.as_bytes()).map_err(io)?;
    Ok(EditOutcome::Written)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `config.toml` a person wrote: a header comment, an unrelated table
    /// with odd spacing and a trailing comment, and a `[shell]` table carrying
    /// a comment plus a key this binary does not know.
    ///
    /// Written out here rather than shared with the identical fixture in
    /// `setup::shell_config`'s tests: the two prove the same bytes survive two
    /// *different* writers, and a test that reads its input from the module it
    /// is proving independence of has none. DAMP over DRY, and it is what keeps
    /// the config tier from naming the installer.
    const HAND_WRITTEN: &str = "\
# a user's own header comment
[registry]
default   =   \"ghcr.io\"   # trailing note

[shell]
# keep me
future_key = 1
completions = true
";

    /// A home with its `locks/` root and the `config.toml` path an edit targets.
    fn home() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let locks = dir.path().join("locks");
        let config = dir.path().join("config.toml");
        (dir, locks, config)
    }

    fn set_hook(document: &mut DocumentMut) -> Result<(), &'static str> {
        document.entry("shell").or_insert(toml_edit::table())["hook"] = toml_edit::value(true);
        Ok(())
    }

    /// ocx#468 — **the load-bearing assertion**: an edit of a file whose lock
    /// another editor holds waits for that editor, and lands once it is gone.
    ///
    /// **Red-state**: change `LOCK_SCOPE` (or the discriminator) in [`edit`]
    /// and the spawned edit finishes inside the 50 ms — it never contended.
    #[tokio::test(flavor = "multi_thread")]
    async fn edit_blocks_behind_a_concurrent_editor_of_the_same_file() {
        let (dir, locks, config) = home();
        let held = lock_scoped(&locks, "config-edit", dir.path(), "config.toml", LOCK_TIMEOUT)
            .await
            .unwrap();

        let task = tokio::spawn({
            let (locks, config) = (locks.clone(), config.clone());
            async move { edit(&locks, &config, false, set_hook).await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !task.is_finished(),
            "an edit must block behind a concurrent editor of the same file"
        );

        drop(held);
        assert_eq!(task.await.unwrap().unwrap(), EditOutcome::Written);
        assert_eq!(std::fs::read_to_string(&config).unwrap(), "[shell]\nhook = true\n");
    }

    /// A closure that touches nothing reports `Unchanged`, and an absent file
    /// stays absent — no empty `config.toml` materializes from a no-op.
    #[tokio::test]
    async fn edit_reports_unchanged_and_writes_nothing_when_the_closure_leaves_the_document_alone() {
        let (_dir, locks, config) = home();

        let outcome = edit(&locks, &config, false, |_| Ok(())).await.unwrap();

        assert_eq!(outcome, EditOutcome::Unchanged);
        assert!(!config.exists(), "a no-op edit must not create the file");

        std::fs::write(&config, HAND_WRITTEN).unwrap();
        let outcome = edit(&locks, &config, false, |_| Ok(())).await.unwrap();
        assert_eq!(outcome, EditOutcome::Unchanged);
        assert_eq!(std::fs::read_to_string(&config).unwrap(), HAND_WRITTEN);
    }

    /// The edit is surgical: a header comment, odd spacing, a trailing note
    /// and an unknown key all survive byte-for-byte around the one change.
    #[tokio::test]
    async fn edit_preserves_every_other_byte() {
        let (_dir, locks, config) = home();
        std::fs::write(&config, HAND_WRITTEN).unwrap();

        let outcome = edit(&locks, &config, false, set_hook).await.unwrap();

        assert_eq!(outcome, EditOutcome::Written);
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            format!("{HAND_WRITTEN}hook = true\n")
        );
    }

    /// A render over `MAX_CONFIG_SIZE` is refused as 78 before any write —
    /// the file on disk is byte-identical.
    #[tokio::test]
    async fn edit_refuses_a_render_over_the_config_limit_before_writing() {
        let (_dir, locks, config) = home();
        std::fs::write(&config, HAND_WRITTEN).unwrap();

        let error = edit(&locks, &config, false, |document| {
            document["blob"] = toml_edit::value("x".repeat(MAX_CONFIG_SIZE as usize));
            Ok(())
        })
        .await
        .expect_err("a render over the ceiling must be refused");

        assert!(
            matches!(&error, EditError::TooLarge { path, bytes } if *path == config && *bytes as u64 > MAX_CONFIG_SIZE),
            "{error:?}"
        );
        assert_eq!(std::fs::read_to_string(&config).unwrap(), HAND_WRITTEN);
    }

    /// A file already over the ceiling is refused the same way, before the
    /// closure runs, and reports the size it has on disk.
    #[tokio::test]
    async fn edit_refuses_an_already_oversize_file() {
        let (_dir, locks, config) = home();
        let oversize = format!("# {}\n", "x".repeat(MAX_CONFIG_SIZE as usize));
        std::fs::write(&config, &oversize).unwrap();

        let error = edit(&locks, &config, false, |_| {
            panic!("the closure must not run on a refused read")
        })
        .await
        .expect_err("an oversize file must be refused");

        assert!(
            matches!(&error, EditError::TooLarge { bytes, .. } if *bytes == oversize.len()),
            "{error:?}"
        );
        assert_eq!(std::fs::read_to_string(&config).unwrap(), oversize);
    }

    /// `dry_run` reports what a real run would do and touches nothing — not
    /// the file, not its directory, not the lock root (review H1: `ocx self
    /// setup --dry-run` on a fresh machine must leave `$OCX_HOME` absent).
    ///
    /// **Red-state**: take the directory or the lock ahead of the dry-run
    /// gate and `home/` or `locks/` materializes.
    #[tokio::test]
    async fn edit_under_dry_run_reports_written_and_touches_nothing() {
        let (dir, _, _) = home();
        let locks = dir.path().join("locks");
        let home_dir = dir.path().join("home");
        let config = home_dir.join("config.toml");

        let outcome = edit(&locks, &config, true, set_hook).await.unwrap();
        assert_eq!(outcome, EditOutcome::Written);
        assert!(!config.exists(), "a dry run must not create the file");
        assert!(!home_dir.exists(), "a dry run must not create the file's directory");
        assert!(!locks.exists(), "a dry run must not create a lock file");

        std::fs::create_dir(&home_dir).unwrap();
        std::fs::write(&config, HAND_WRITTEN).unwrap();
        let outcome = edit(&locks, &config, true, set_hook).await.unwrap();
        assert_eq!(outcome, EditOutcome::Written);
        assert_eq!(std::fs::read_to_string(&config).unwrap(), HAND_WRITTEN);
        assert!(
            !locks.exists(),
            "a dry run over an existing file must not create a lock file either"
        );
    }

    /// An editor that outlives the edit's lock timeout surfaces as
    /// [`EditError::Locked`] on the config file — 75, the ADR's lock-timeout
    /// code — never as an I/O error on the hashed lock path (review H2).
    ///
    /// Driven through [`edit_with`] with a short timeout rather than [`edit`]
    /// with the shipped one: the claim is about *which error a timeout
    /// becomes*, which is identical at 5 s and at 80 ms, and waiting out the
    /// shipped value cost the unit suite five seconds to observe it. The
    /// shipped value is pinned below instead, so a change to it still reds
    /// here.
    ///
    /// **Red-state**: map the timeout to `Io` (or classify `Locked` as 74)
    /// and either assertion fails.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_editor_held_past_the_timeout_is_locked_and_exit_75() {
        assert_eq!(
            LOCK_TIMEOUT,
            Duration::from_secs(5),
            "the shipped edit-lock timeout, which `edit` and `edit_text` pass and this test stands in for"
        );

        let (dir, locks, config) = home();
        let timeout = Duration::from_millis(80);
        let _held = lock_scoped(&locks, "config-edit", dir.path(), "config.toml", timeout)
            .await
            .unwrap();

        let started = std::time::Instant::now();
        let error = edit_with(&locks, &config, false, timeout, |_, _| {
            panic!("the apply step must not run behind a held lock")
        })
        .await
        .expect_err("the edit must give up behind a holder that never lets go");
        let elapsed = started.elapsed();

        assert!(
            matches!(&error, EditError::Locked { path } if *path == config),
            "the lock timeout must name the config file: {error:?}"
        );
        assert!(
            elapsed >= timeout,
            "the edit waited out its own timeout rather than failing for another reason: \
             {elapsed:?} < {timeout:?}"
        );
        assert!(!config.exists(), "a refused edit writes nothing");
    }

    /// [`edit_text`] runs the same pipeline with the text as the document:
    /// an identical return is `Unchanged` and writes nothing, a changed one
    /// lands byte-for-byte, and a refusal is `Malformed` with the file kept.
    #[tokio::test]
    async fn edit_text_publishes_the_returned_text_and_gates_like_edit() {
        let (_dir, locks, config) = home();

        let outcome = edit_text(&locks, &config, false, |text| Ok(text.to_owned()))
            .await
            .unwrap();
        assert_eq!(outcome, EditOutcome::Unchanged);
        assert!(!config.exists(), "an unchanged text must not create the file");

        let outcome = edit_text(&locks, &config, false, |text| Ok(format!("{text}# fence\n")))
            .await
            .unwrap();
        assert_eq!(outcome, EditOutcome::Written);
        assert_eq!(std::fs::read_to_string(&config).unwrap(), "# fence\n");

        let error = edit_text(&locks, &config, false, |_| Err("refused"))
            .await
            .expect_err("a refusing closure must not publish");
        assert!(
            matches!(&error, EditError::Malformed { reason: "refused", .. }),
            "{error:?}"
        );
        assert_eq!(std::fs::read_to_string(&config).unwrap(), "# fence\n");
    }
}
