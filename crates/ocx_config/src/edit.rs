// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The one locked read-modify-write for a `config.toml`.
//!
//! Every `config.toml` writer goes through here, or concurrent `ocx` runs publish over each other's edit.
//! The lock is a [`lock_scoped`] entry, never a lock on the file: atomic rename rotates the inode.

use std::path::{Path, PathBuf};
use std::time::Duration;

use toml_edit::DocumentMut;

use crate::loader::MAX_CONFIG_SIZE;
use ocx_util::fs::{BoundedReadError, lock_scoped, read_bounded, write_bytes_atomic};

/// The `scope` every `config.toml` edit lock is taken under.
const LOCK_SCOPE: &str = "config-edit";

/// How long an edit waits behind another editor before giving up.
const LOCK_TIMEOUT: Duration = Duration::from_secs(5);

/// Test seam: hold the lock this many milliseconds between read and write.
#[cfg(any(test, feature = "__testing"))]
const HOLD_OVERRIDE: &str = "__OCX_TESTING_CONFIG_EDIT_HOLD_MS";

/// What an [`edit`] did to the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditOutcome {
    /// The closure left the document as it was; nothing was written.
    Unchanged,
    /// The document changed and was written (or, under `dry_run`, would have been).
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
    /// Another `ocx` held the edit lock past the timeout.
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
    /// The file parses but has a shape the closure cannot edit; it is left as it was.
    #[error("{}: {reason}", path.display())]
    Malformed {
        /// The file that was refused.
        path: PathBuf,
        /// The closure's one-line reason.
        reason: &'static str,
    },
    /// The document is, or would render, over `MAX_CONFIG_SIZE`; refused before any write.
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
/// An absent file edits as empty; `dry_run` reports a change but creates, locks and writes nothing.
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

/// [`edit`] over the file's raw text (absent → `""`), for the comment-delimited `[managed]` fence.
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

/// The shared body of [`edit`] and [`edit_text`]; `timeout` is a parameter only for tests.
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

    // No directory or lock under `dry_run`, so an absent `$OCX_HOME` stays absent.
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
    // `_guard` must outlive the write, or a second editor reads the pre-edit file.
}

/// A lock timeout is [`EditError::Locked`] on the config file (75); anything else is I/O (74).
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

fn edit_blocking<F>(path: &Path, dry_run: bool, apply: F) -> Result<EditOutcome, EditError>
where
    F: FnOnce(&Path, &str) -> Result<String, EditError>,
{
    let io = |source| EditError::Io {
        path: path.to_path_buf(),
        source,
    };

    // Bounded: `OCX_NO_CONFIG=1` skips the loader, so an oversize file can reach here.
    let original = match read_bounded(path, MAX_CONFIG_SIZE) {
        Ok(bytes) => {
            String::from_utf8(bytes).map_err(|error| io(std::io::Error::new(std::io::ErrorKind::InvalidData, error)))?
        }
        // Only NotFound reads as empty; any other error as empty would overwrite an unreadable file.
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

    /// **The load-bearing assertion**: an edit of a file whose lock
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
