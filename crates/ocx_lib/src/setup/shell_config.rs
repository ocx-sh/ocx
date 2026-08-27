// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Surgical `[shell]` writes into the home-tier `$OCX_HOME/config.toml`.
//!
//! Contract stub only — the body belongs to WP-10 of
//! `.claude/artifacts/plan_shell_env_overhaul.md` (C-040).
//!
//! **A separate module from [`crate::setup`] on purpose.** The shipped
//! `--managed` write shares only the *target path*: it reads the whole file as
//! a string and drives a fenced-block state machine through
//! [`crate::setup::rc_block`], classifying `Fresh`/`Current`/`FormatUpgraded`/
//! `Dirty` and exiting 82 on user edits. Keeping the fenced writer and the
//! surgical writer in one file would make its reader hold two mental models.
//!
//! **`[shell]` is deliberately not fenced**, so exit 82 (`DirtyRcBlock`) does
//! **not** apply here: there is no fence, so there is no dirty state. A user's
//! hand-written `[shell] hook = false` is simply overwritten by an explicit
//! `--hook`, which is what the flag means. A write failure is 74 `IoError`.

use std::path::Path;

use toml_edit::{DocumentMut, Item, Table};

/// Which `[shell]` key a write targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellFlag {
    /// `[shell] hook`.
    Hook,
    /// `[shell] completions`.
    Completions,
}

impl ShellFlag {
    /// The `[shell]` key this flag writes, spelled exactly as
    /// [`crate::config::ShellConfig`] deserializes it.
    pub const fn key(self) -> &'static str {
        match self {
            Self::Hook => "hook",
            Self::Completions => "completions",
        }
    }
}

/// Set exactly one `[shell]` key in the home-tier `config.toml` (C-040).
///
/// `config_path` is `file_structure.root().join("config.toml")` — i.e.
/// `$OCX_HOME/config.toml`, **not** `ConfigLoader::user_path()`
/// (`config_dir()/ocx/config.toml`). `--config` / `OCX_CONFIG` name a **read**
/// override and never redirect this write.
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
/// Callers pass the flag only when it was given: **flag absent writes
/// nothing**, and the default applies. When a higher tier already sets the key,
/// the write still lands and the CLI reports which tier will win (C-034).
///
/// # Errors
///
/// Propagates the read/parse/atomic-write failure — classified 74 `IoError`.
pub fn set_flag(config_path: &Path, flag: ShellFlag, value: bool) -> crate::Result<()> {
    let original = match std::fs::read_to_string(config_path) {
        Ok(text) => text,
        // A missing file is the create case, not a failure. Every other read
        // error is real and must not be papered over with an empty document —
        // that would silently replace a file we could not read.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(crate::error::file_error(config_path, error)),
    };

    let mut document: DocumentMut = original
        .parse()
        .map_err(|error| malformed(config_path, &format!("config.toml does not parse as TOML: {error}")))?;

    let created = !document.contains_key("shell");
    let table = document
        .entry("shell")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_like_mut()
        .ok_or_else(|| malformed(config_path, "`shell` is present but is not a table"))?;
    table.insert(flag.key(), toml_edit::value(value));

    if created {
        hoist_above_every_table(&mut document);
    }

    let rendered = document.to_string();
    if let Some(parent) = config_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|error| crate::error::file_error(parent, error))?;
    }
    crate::utility::fs::write_bytes_atomic(config_path, rendered.as_bytes())
        .map_err(|error| crate::error::file_error(config_path, error))
}

/// Render a freshly created `[shell]` table **before** every table already in
/// the document.
///
/// The default position for a new table is end-of-document, and that is not
/// safe here: `$OCX_HOME/config.toml` is also where `ocx self setup
/// --managed-config` appends its `[managed]` seed inside an
/// [`rc_block`](crate::setup::rc_block) fence. The fence *closer* parses as
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

/// A `config.toml` this writer cannot edit surgically.
///
/// Classified 74 `IoError` like the read and the write it sits between: the
/// contract for this command is one code for "the `[shell]` write did not
/// happen", and 78 `ConfigError` is already spoken for by the loader, which
/// refuses the same file earlier and louder on the read path.
fn malformed(config_path: &Path, reason: &str) -> crate::Error {
    crate::error::file_error(config_path, std::io::Error::other(reason.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `config.toml` a person wrote: a header comment, an unrelated table
    /// with odd spacing and a trailing comment, and a `[shell]` table carrying
    /// a comment plus a key this binary does not know.
    const HAND_WRITTEN: &str = "\
# a user's own header comment
[registry]
default   =   \"ghcr.io\"   # trailing note

[shell]
# keep me
future_key = 1
completions = true
";

    fn write(dir: &std::path::Path, content: &str) -> std::path::PathBuf {
        let path = dir.join("config.toml");
        std::fs::write(&path, content).unwrap();
        path
    }

    /// C-040 / S-016: a missing `$OCX_HOME/config.toml` is created carrying
    /// only the one section — no scaffold, no other table.
    #[test]
    fn missing_file_is_created_with_just_the_one_section() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.toml");

        set_flag(&path, ShellFlag::Hook, false).unwrap();

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
    #[test]
    fn comments_and_unknown_keys_survive_the_write() {
        let home = tempfile::tempdir().unwrap();
        let path = write(home.path(), HAND_WRITTEN);

        set_flag(&path, ShellFlag::Hook, true).unwrap();

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

    /// C-040: an existing key is set in place — the surrounding decor, and any
    /// key declared after it, keep their position.
    #[test]
    fn an_existing_key_is_set_in_place() {
        let home = tempfile::tempdir().unwrap();
        let path = write(
            home.path(),
            "[shell]\nhook = false\n# tail comment\ncompletions = true\n",
        );

        set_flag(&path, ShellFlag::Hook, true).unwrap();

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[shell]\nhook = true\n# tail comment\ncompletions = true\n",
            "setting a key in place must not move it below its successors"
        );
    }

    /// C-040: each variant targets its own key name, and two successive writes
    /// compose instead of replacing each other.
    #[test]
    fn each_flag_targets_its_own_key_and_writes_compose() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.toml");

        set_flag(&path, ShellFlag::Hook, false).unwrap();
        set_flag(&path, ShellFlag::Completions, true).unwrap();

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
    #[test]
    fn a_failed_publish_is_exit_74() {
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

        let error = set_flag(&directory.join("config.toml"), ShellFlag::Hook, true)
            .expect_err("publishing into a directory this process cannot write must fail");
        std::fs::set_permissions(&directory, writable).unwrap();

        assert_eq!(
            crate::cli::classify_error(&error),
            crate::cli::ExitCode::IoError,
            "a failed `[shell]` write is 74, not 82"
        );
    }

    /// C-051: a read that fails for any reason other than "not there" is 74 as
    /// well, and stops before anything is published.
    #[test]
    fn an_unreadable_config_is_exit_74() {
        let home = tempfile::tempdir().unwrap();
        // A regular file standing in for the parent directory: the read of
        // `<file>/config.toml` fails with `ENOTDIR`, which is not `NotFound`
        // and so is not the create case.
        std::fs::write(home.path().join("not-a-dir"), b"").unwrap();
        let path = home.path().join("not-a-dir").join("config.toml");

        let error = set_flag(&path, ShellFlag::Hook, true).expect_err("the read cannot succeed");
        assert_eq!(crate::cli::classify_error(&error), crate::cli::ExitCode::IoError);
    }

    /// C-051: the `[shell]` write must not disturb the **other** fence that
    /// lives in the same file. `$OCX_HOME/config.toml` carries the `[managed]`
    /// seed inside an [`rc_block`](crate::setup::rc_block) fence appended at
    /// end of document, so a newly created `[shell]` table placed after the
    /// last table lands **inside** that fence: the block then hashes to
    /// something its own marker disagrees with, classifies `Dirty`, and
    /// `ocx self setup --hook` exits **82** — the one code C-051 says this
    /// write must never produce. `--force` then collapses the fence and
    /// deletes the toggle with it.
    #[test]
    fn a_new_table_lands_outside_the_managed_fence() {
        use crate::setup::rc_block::{self, BlockState, MANAGED_LABEL};

        const BODY: &str = "[managed]\nsource = \"ghcr.io/acme/cfg:1\"\nrequired = false";
        const NEXT_BODY: &str = "[managed]\nsource = \"ghcr.io/acme/cfg:2\"\nrequired = false";

        let home = tempfile::tempdir().unwrap();
        let fenced = rc_block::apply("", BODY, false, MANAGED_LABEL)
            .unwrap()
            .expect("a file with no fence gets a fresh one");
        let path = write(home.path(), &fenced);

        set_flag(&path, ShellFlag::Hook, true).unwrap();
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

    /// C-051: a `config.toml` that does not parse is reported, not silently
    /// replaced — the file on disk is left exactly as it was.
    #[test]
    fn an_unparseable_file_is_reported_and_left_alone() {
        let home = tempfile::tempdir().unwrap();
        let broken = "[shell\nhook = ";
        let path = write(home.path(), broken);

        let error = set_flag(&path, ShellFlag::Hook, true).expect_err("broken TOML cannot be edited");
        assert_eq!(crate::cli::classify_error(&error), crate::cli::ExitCode::IoError);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            broken,
            "a failed edit must not truncate or rewrite the user's file"
        );
    }
}
