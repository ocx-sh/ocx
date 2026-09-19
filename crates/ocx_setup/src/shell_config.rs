// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Surgical `[shell]` writes into the home-tier `$OCX_HOME/config.toml`.
//!
//! Contract stub only — the body belongs to WP-10 of
//! `.claude/artifacts/plan_shell_env_overhaul.md` (C-040).
//!
//! **A separate module from the crate root on purpose.** The shipped
//! `--managed` write shares only the *target path*: it reads the whole file as
//! a string and drives a fenced-block state machine through
//! [`crate::rc_block`], classifying `Fresh`/`Current`/`FormatUpgraded`/
//! `Dirty` and exiting 82 on user edits. Keeping the fenced writer and the
//! surgical writer in one file would make its reader hold two mental models.
//!
//! **`[shell]` is deliberately not fenced**, so exit 82 (`DirtyRcBlock`) does
//! **not** apply here: there is no fence, so there is no dirty state. A user's
//! hand-written `[shell] hook = false` is simply overwritten by an explicit
//! `--hook`, which is what the flag means. A write failure is 74 `IoError`.
//!
//! The read-modify-write itself is [`ocx_config::edit::edit`] (ocx#468):
//! this module owns only the `[shell]` closure.

use std::path::{Path, PathBuf};

use toml_edit::{Array, DocumentMut, Item, Table};

use ocx_config::edit::{self, EditError};

/// Which `[shell]` key a write targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKey {
    /// `[shell] hook`.
    Hook,
    /// `[shell] completions`.
    Completions,
    /// `[shell] modify_path`.
    ModifyPath,
    /// `[shell] profiles`.
    Profiles,
}

impl ShellKey {
    /// The `[shell]` key this variant writes, spelled exactly as
    /// [`ocx_config::ShellConfig`] deserializes it.
    pub const fn key(self) -> &'static str {
        match self {
            Self::Hook => "hook",
            Self::Completions => "completions",
            Self::ModifyPath => "modify_path",
            Self::Profiles => "profiles",
        }
    }
}

/// The value written under a [`ShellKey`].
///
/// `Bool` covers the boolean rungs (`hook`, `completions`, `modify_path`).
/// `Paths` covers `profiles`, where an **empty slice is a real, distinct
/// value** from the key being absent: absent means auto-detect, empty means
/// write no profile blocks.
#[derive(Debug, Clone, Copy)]
pub enum ShellValue<'a> {
    /// A boolean rung.
    Bool(bool),
    /// An explicit path list.
    Paths(&'a [PathBuf]),
}

/// Set exactly one `[shell]` key in the home-tier `config.toml` (C-040).
///
/// `config_path` is `file_structure.root().join("config.toml")` — i.e.
/// `$OCX_HOME/config.toml`, **not** `ConfigLoader::user_path()`
/// (`config_dir()/ocx/config.toml`). `--config` / `OCX_CONFIG` name a **read**
/// override and never redirect this write. `locks_root` is
/// `file_structure.locks`, where [`edit::edit`] takes the cross-process lock
/// every `config.toml` writer shares (ocx#468).
///
/// A **missing file is created** with just the one section.
///
/// The mechanism is a **surgical `toml_edit` edit** (`toml_edit` is already a
/// workspace dependency of `ocx_lib`), not a whole-file rewrite and not a
/// fenced block: `Config` derives `Deserialize` only, so a serde round-trip is
/// unavailable; a rewrite would discard comments and unknown keys the
/// forward-compat contract exists to preserve; and a fence would make
/// `[shell]` an ocx-owned region a user may not edit, which is the opposite of
/// the intent for a user-facing toggle. Create the table if absent and
/// preserve every other byte of the file.
///
/// A [`ShellValue::Paths`] renders as a TOML array of strings via
/// `Path::to_string_lossy()` — a path is not guaranteed UTF-8 on Windows, and
/// a lossy render of the rare non-UTF-8 byte beats refusing the whole write.
///
/// Callers pass the key only when a write was requested: **no call, no
/// change**, and the default applies. When a higher tier already sets the
/// key, the write still lands and the CLI reports which tier will win
/// (C-034).
///
/// # Errors
///
/// [`EditError`] — the read/parse/atomic-write failure, classified 74
/// `IoError`; a document over the config size ceiling is 78.
pub async fn set(locks_root: &Path, config_path: &Path, key: ShellKey, value: ShellValue<'_>) -> Result<(), EditError> {
    // Rendered ahead of the closure so it owns nothing borrowed.
    let value = match value {
        ShellValue::Bool(flag) => toml_edit::value(flag),
        ShellValue::Paths(paths) => {
            let array: Array = paths.iter().map(|path| path.to_string_lossy().into_owned()).collect();
            toml_edit::value(array)
        }
    };
    edit::edit(locks_root, config_path, false, move |document| {
        let created = !document.contains_key("shell");
        let table = document
            .entry("shell")
            .or_insert_with(|| Item::Table(Table::new()))
            .as_table_like_mut()
            .ok_or("`shell` is present but is not a table")?;
        table.insert(key.key(), value);
        if created {
            hoist_above_every_table(document);
        }
        Ok(())
    })
    .await
    .map(drop)
}

/// Render a freshly created `[shell]` table **before** every table already in
/// the document.
///
/// The default position for a new table is end-of-document, and that is not
/// safe here: `$OCX_HOME/config.toml` is also where `ocx self setup
/// --managed-config` appends its `[managed]` seed inside an
/// [`rc_block`](crate::rc_block) fence. The fence *closer* parses as
/// trailing trivia, so a table appended at the end renders **between** the
/// `[managed]` body and the closer — inside the fence. The block then hashes to
/// something its own marker disagrees with, `rc_block::classify` calls it
/// `Dirty`, and `ocx self setup` starts exiting 82 (which C-051 forbids for
/// this write) while `--force` collapses the fence and deletes the toggle with
/// it.
///
/// Going to the front is what makes that impossible rather than unlikely: a
/// fence opener is a comment attached to the header of the table it precedes,
/// so every fenced region in the document begins at or after the first table.
/// Nothing can be hoisted above the first table and still be inside a fence.
fn hoist_above_every_table(document: &mut DocumentMut) {
    // `doc_position` is a signed ordering key and a parsed document numbers its
    // tables from zero, so a negative slot sorts ahead of all of them without
    // renumbering — and therefore without touching one byte of anyone else's
    // table.
    if let Some(Item::Table(shell)) = document.get_mut("shell") {
        shell.set_position(Some(-1));
    }
}

/// A `config.toml` a person wrote: a header comment, an unrelated table
/// with odd spacing and a trailing comment, and a `[shell]` table carrying
/// a comment plus a key this binary does not know.
///
/// [`ocx_config::edit`]'s tests carry their own copy of these bytes and
/// prove the same survival there — deliberately a second copy, so the config
/// tier names nothing under `setup`.
#[cfg(test)]
const HAND_WRITTEN: &str = "\
# a user's own header comment
[registry]
default   =   \"ghcr.io\"   # trailing note

[shell]
# keep me
future_key = 1
completions = true
";

#[cfg(test)]
mod tests {
    use super::*;

    fn locks(dir: &std::path::Path) -> std::path::PathBuf {
        dir.join("locks")
    }

    fn write(dir: &std::path::Path, content: &str) -> std::path::PathBuf {
        let path = dir.join("config.toml");
        std::fs::write(&path, content).unwrap();
        path
    }

    /// C-040 / S-016: a missing `$OCX_HOME/config.toml` is created carrying
    /// only the one section — no scaffold, no other table.
    #[tokio::test]
    async fn missing_file_is_created_with_just_the_one_section() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.toml");

        set(&locks(home.path()), &path, ShellKey::Hook, ShellValue::Bool(false))
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[shell]\nhook = false\n",
            "a created file carries the one section and nothing else"
        );
    }

    /// C-040 — **the load-bearing assertion**: a pre-existing comment and an
    /// unknown key survive the write byte-for-byte. A whole-file
    /// parse→serialize rewrite discards both, which is the named red state for
    /// this work package.
    #[tokio::test]
    async fn comments_and_unknown_keys_survive_the_write() {
        let home = tempfile::tempdir().unwrap();
        let path = write(home.path(), HAND_WRITTEN);

        set(&locks(home.path()), &path, ShellKey::Hook, ShellValue::Bool(true))
            .await
            .unwrap();

        let after = std::fs::read_to_string(&path).unwrap();
        assert!(
            after.starts_with(HAND_WRITTEN),
            "every byte the user wrote must survive verbatim; got:\n{after}"
        );
        assert_eq!(
            after,
            format!("{HAND_WRITTEN}hook = true\n"),
            "the only change is the one key, appended to the table it belongs to"
        );
    }

    /// C-040: two successive writes on the two new keys compose the same way
    /// the two original boolean keys always did, and the surrounding comment
    /// plus the unrecognised key survive both writes byte-for-byte.
    ///
    /// **Red-state**: replace the surgical `table.insert` in [`set`] with a
    /// whole-file rewrite (rebuild `document` from scratch instead of editing
    /// the parsed one in place) and this test fails — the comment and
    /// `future_key` are gone. Demonstrated by hand for this work package: see
    /// the builder's report for the red output.
    #[tokio::test]
    async fn writes_compose_and_preserve_bytes() {
        let home = tempfile::tempdir().unwrap();
        let path = write(home.path(), HAND_WRITTEN);

        set(
            &locks(home.path()),
            &path,
            ShellKey::ModifyPath,
            ShellValue::Bool(false),
        )
        .await
        .unwrap();
        set(&locks(home.path()), &path, ShellKey::Profiles, ShellValue::Paths(&[]))
            .await
            .unwrap();

        let after = std::fs::read_to_string(&path).unwrap();
        assert!(
            after.starts_with(HAND_WRITTEN),
            "every byte the user wrote must survive both writes verbatim; got:\n{after}"
        );
        assert_eq!(
            after,
            format!("{HAND_WRITTEN}modify_path = false\nprofiles = []\n"),
            "both keys land, in write order, without disturbing the table's existing keys"
        );
    }

    /// C-040: an existing key is set in place — the surrounding decor, and any
    /// key declared after it, keep their position.
    #[tokio::test]
    async fn an_existing_key_is_set_in_place() {
        let home = tempfile::tempdir().unwrap();
        let path = write(
            home.path(),
            "[shell]\nhook = false\n# tail comment\ncompletions = true\n",
        );

        set(&locks(home.path()), &path, ShellKey::Hook, ShellValue::Bool(true))
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[shell]\nhook = true\n# tail comment\ncompletions = true\n",
            "setting a key in place must not move it below its successors"
        );
    }

    /// C-040: each variant targets its own key name, and two successive writes
    /// compose instead of replacing each other.
    #[tokio::test]
    async fn each_flag_targets_its_own_key_and_writes_compose() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.toml");

        set(&locks(home.path()), &path, ShellKey::Hook, ShellValue::Bool(false))
            .await
            .unwrap();
        set(
            &locks(home.path()),
            &path,
            ShellKey::Completions,
            ShellValue::Bool(true),
        )
        .await
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[shell]\nhook = false\ncompletions = true\n",
            "`hook` and `completions` are distinct keys and neither write clobbers the other"
        );
    }

    /// C-051: a failure **publishing** the file is 74 `IoError`, never 82
    /// `DirtyRcBlock` — `[shell]` is not fenced, so there is no dirty state to
    /// report.
    ///
    /// The fixture has to reach `write_bytes_atomic` to mean anything, so the
    /// parent directory exists and is readable (the read arm returns
    /// `NotFound`, the create case) but is not writable. A regular file
    /// standing in for the parent would return `ENOTDIR` from the *read* and
    /// never reach the write at all.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_publish_is_exit_74() {
        use std::os::unix::fs::PermissionsExt as _;

        let home = tempfile::tempdir().unwrap();
        let directory = home.path().join("read-only");
        std::fs::create_dir(&directory).unwrap();
        let unwritable = std::fs::Permissions::from_mode(0o555);
        let writable = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&directory, unwritable).unwrap();

        // A process that bypasses the mode (root, or a permissive filesystem)
        // cannot produce the failure this fixture exists to produce. Observed
        // by probing, not assumed from the environment.
        if std::fs::File::create(directory.join(".probe")).is_ok() {
            std::fs::set_permissions(&directory, writable).unwrap();
            eprintln!("skipped: this process can create files inside a 0o555 directory");
            return;
        }

        let _error = set(
            &locks(home.path()),
            &directory.join("config.toml"),
            ShellKey::Hook,
            ShellValue::Bool(true),
        )
        .await
        .expect_err("publishing into a directory this process cannot write must fail");
        std::fs::set_permissions(&directory, writable).unwrap();
    }

    /// C-051: a read that fails for any reason other than "not there" is 74 as
    /// well, and stops before anything is published. A FIFO at the config
    /// path: the parent exists and the lock is taken, so the failure is the
    /// bounded read's own `NotRegularFile` — the read arm, not the directory
    /// arm a `<file>/config.toml` path hits first.
    ///
    /// **Red-state** (review W2 / mutation M5): swallow the read error into
    /// an empty document and the edit renders `[shell]`, renames a regular
    /// file over the FIFO, and returns `Ok` — the FIFO is gone.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_unreadable_config_is_exit_74() {
        use std::os::unix::fs::FileTypeExt as _;

        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.toml");
        ocx_test_support::fifo::mkfifo(&path);

        let _error = set(&locks(home.path()), &path, ShellKey::Hook, ShellValue::Bool(true))
            .await
            .expect_err("the read cannot succeed");
        assert!(
            std::fs::symlink_metadata(&path).unwrap().file_type().is_fifo(),
            "a refused read must publish nothing over the path"
        );
    }

    /// C-051: a `shell` key that is not a table cannot take the edit — the
    /// closure refuses, that is [`EditError::Malformed`] (74), and the file
    /// is left byte-identical (review W3).
    #[tokio::test]
    async fn a_shell_key_that_is_not_a_table_is_refused_as_exit_74() {
        let home = tempfile::tempdir().unwrap();
        let path = write(home.path(), "shell = 1\n");

        let error = set(&locks(home.path()), &path, ShellKey::Hook, ShellValue::Bool(true))
            .await
            .expect_err("a scalar `shell` cannot hold a key");

        assert!(
            matches!(&error, EditError::Malformed { reason, .. } if reason.contains("not a table")),
            "{error:?}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "shell = 1\n");
    }

    /// C-051: the `[shell]` write must not disturb the **other** fence that
    /// lives in the same file. `$OCX_HOME/config.toml` carries the `[managed]`
    /// seed inside an [`rc_block`](crate::rc_block) fence appended at
    /// end of document, so a newly created `[shell]` table placed after the
    /// last table lands **inside** that fence: the block then hashes to
    /// something its own marker disagrees with, classifies `Dirty`, and
    /// `ocx self setup --hook` exits **82** — the one code C-051 says this
    /// write must never produce. `--force` then collapses the fence and
    /// deletes the toggle with it.
    #[tokio::test]
    async fn a_new_table_lands_outside_the_managed_fence() {
        use crate::rc_block::{self, BlockState, MANAGED_LABEL};

        const BODY: &str = "[managed]\nsource = \"ghcr.io/acme/cfg:1\"\nrequired = false";
        const NEXT_BODY: &str = "[managed]\nsource = \"ghcr.io/acme/cfg:2\"\nrequired = false";

        let home = tempfile::tempdir().unwrap();
        let fenced = rc_block::apply("", BODY, false, MANAGED_LABEL)
            .unwrap()
            .expect("a file with no fence gets a fresh one");
        let path = write(home.path(), &fenced);

        set(&locks(home.path()), &path, ShellKey::Hook, ShellValue::Bool(true))
            .await
            .unwrap();
        let after = std::fs::read_to_string(&path).unwrap();

        assert_eq!(
            after,
            format!("[shell]\nhook = true\n{fenced}"),
            "a created [shell] table goes above the fence, and the fence keeps every byte"
        );
        assert_ne!(
            rc_block::classify(&after, BODY, MANAGED_LABEL),
            BlockState::Dirty,
            "the [shell] write must leave the managed fence ocx-authored, not user-edited:\n{after}"
        );

        // And the toggle must survive the fence rewrite a later `ocx self setup`
        // performs: everything between the fences is replaced wholesale.
        let rewritten = rc_block::apply(&after, NEXT_BODY, false, MANAGED_LABEL)
            .unwrap()
            .expect("a changed managed body rewrites the fence");
        assert!(
            rewritten.contains("hook = true"),
            "a later managed-fence rewrite must not delete the [shell] toggle:\n{rewritten}"
        );
    }

    /// C-051: the fence-avoidance guard generalizes to the new `Paths` value —
    /// a freshly created `[shell]` table carrying `profiles` must land above
    /// the `[managed]` fence exactly like a freshly created `hook` does.
    ///
    /// **Red-state**: comment out the `hoist_above_every_table` call in
    /// [`set`] and this test fails — the new table renders after the fence
    /// body, inside the fence, and `[shell]` and `[managed]` swap places in
    /// the byte comparison below. Demonstrated by hand for this work package:
    /// see the builder's report for the red output.
    #[tokio::test]
    async fn new_table_lands_outside_the_managed_fence() {
        use crate::rc_block::{self, MANAGED_LABEL};

        const BODY: &str = "[managed]\nsource = \"ghcr.io/acme/cfg:1\"\nrequired = false";

        let home = tempfile::tempdir().unwrap();
        let fenced = rc_block::apply("", BODY, false, MANAGED_LABEL)
            .unwrap()
            .expect("a file with no fence gets a fresh one");
        let path = write(home.path(), &fenced);

        set(&locks(home.path()), &path, ShellKey::Profiles, ShellValue::Paths(&[]))
            .await
            .unwrap();
        let after = std::fs::read_to_string(&path).unwrap();

        assert_eq!(
            after,
            format!("[shell]\nprofiles = []\n{fenced}"),
            "a created [shell] table goes above the fence even when the write is a Paths value"
        );
    }

    /// C-040: `profiles` renders as a TOML array of strings, and an explicitly
    /// empty slice renders as an explicitly empty array — not an absent key.
    /// That distinction is the whole point of the key: absent means
    /// auto-detect, empty means write no profile blocks.
    #[tokio::test]
    async fn profiles_renders_as_an_array_of_strings() {
        let home = tempfile::tempdir().unwrap();
        let path_a = home.path().join("config.toml");
        let a = home.path().join("a").join(".bashrc");
        let b = home.path().join("b").join(".zshrc");

        set(
            &locks(home.path()),
            &path_a,
            ShellKey::Profiles,
            ShellValue::Paths(&[a.clone(), b.clone()]),
        )
        .await
        .unwrap();

        // Parse, do not grep: `toml_edit` picks a literal string (`'…'`) when
        // the value carries backslashes and a basic string (`"…"`) otherwise,
        // so a byte-exact assertion encodes the host's path separator. The
        // contract is the parsed value, and only that.
        let written = std::fs::read_to_string(&path_a).unwrap();
        let document: toml_edit::DocumentMut = written.parse().unwrap();
        let profiles: Vec<String> = document["shell"]["profiles"]
            .as_array()
            .expect("profiles is a TOML array")
            .iter()
            .map(|item| item.as_str().expect("each profile is a string").to_owned())
            .collect();
        assert_eq!(
            profiles,
            vec![a.to_string_lossy().into_owned(), b.to_string_lossy().into_owned()],
            "profiles parses back as the array of path strings that was written"
        );

        let path_b = home.path().join("empty.toml");
        set(&locks(home.path()), &path_b, ShellKey::Profiles, ShellValue::Paths(&[]))
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(&path_b).unwrap(),
            "[shell]\nprofiles = []\n",
            "an explicitly empty list renders as an explicitly empty array, not an absent key"
        );
    }

    /// C-051: a `config.toml` that does not parse is reported, not silently
    /// replaced — the file on disk is left exactly as it was.
    #[tokio::test]
    async fn an_unparseable_file_is_reported_and_left_alone() {
        let home = tempfile::tempdir().unwrap();
        let broken = "[shell\nhook = ";
        let path = write(home.path(), broken);

        let _error = set(&locks(home.path()), &path, ShellKey::Hook, ShellValue::Bool(true))
            .await
            .expect_err("broken TOML cannot be edited");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            broken,
            "a failed edit must not truncate or rewrite the user's file"
        );
    }
}
