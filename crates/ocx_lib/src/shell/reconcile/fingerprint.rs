// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The watch-set fingerprint ([ocx-sh/ocx#345](https://github.com/ocx-sh/ocx/issues/345)):
//! [`watch_paths`] names the member files a per-prompt reconcile must notice,
//! and [`fingerprint`]/[`current_fingerprint`] fold them into the value
//! compared against [`super::Ledger::fp`] to decide whether the environment
//! is stale (C-019, C-044).

use std::path::{Path, PathBuf};

/// Fold the watch set into the ledger's `fp` (C-019, A-13, A-14).
///
/// The **only** definition of what makes an environment stale. `fp` is compared
/// against [`Ledger::fp`]; equal means nothing in the watch set moved, and —
/// together with a cached [`Verdict::Inert`] — that is what makes the per-prompt
/// path stat-only (C-042).
///
/// `watch_paths` is the recorded member list, in order, exactly as the emitted
/// hook body carries it (C-044): the project's `ocx.toml` and `ocx.lock`, the
/// global tier's pair, the managed-config snapshot, the config-tier paths the
/// last `ConfigLoader` pass discovered, and the project's consent stamp. Each is
/// folded with its **presence**, its size and its mtime, so a tier file that did
/// not exist becomes a change the moment it is created. `project_dir` is member
/// 7 — which project the CWD walk resolved — folded as identity, so moving
/// between two projects is a change even when no watched file was touched. The
/// binary version is folded from `CARGO_PKG_VERSION`, so `self update` moves it.
///
/// A-13 — `consent_paths` and `consent_namespaces` are the **raw**
/// `OCX_CONSENT_PATHS` / `OCX_CONSENT_NAMESPACES` values, passed in rather than
/// read here so the fold stays pure and unit-testable without a process-wide env
/// lock. Without them a grant exported from another terminal would never expire
/// the cached `inert` verdict until the shell restarted. Set-but-empty is a
/// distinct state from unset, on the same set-ness rule [`Prior`] follows.
///
/// A-14 — the mtime is the **full** `SystemTime`, never a seconds-truncated
/// value, so the named ceiling ("an unchanged `(mtime, size)` pair is
/// invisible") is the filesystem's own granularity and nothing coarser.
///
/// Blocking: one `stat` per member. Call it from a blocking context — the whole
/// point of C-044 is that this is cheaper than the exec that reaches it.
pub fn fingerprint(
    watch_paths: &[PathBuf],
    project_dir: Option<&Path>,
    consent_paths: Option<&str>,
    consent_namespaces: Option<&str>,
) -> String {
    use sha2::Digest as _;

    let mut hasher = sha2::Sha256::new();
    fold(&mut hasher, "ocx", env!("CARGO_PKG_VERSION").as_bytes());
    fold_optional(
        &mut hasher,
        "dir",
        project_dir.map(|dir| dir.as_os_str().as_encoded_bytes()),
    );

    for path in watch_paths {
        fold(&mut hasher, "path", path.as_os_str().as_encoded_bytes());
        // `metadata`, not `symlink_metadata`: the shell-side newer-than test
        // this fold has to agree with follows symlinks too (C-044).
        match std::fs::metadata(path) {
            Ok(meta) => {
                fold(&mut hasher, "present", &[1]);
                fold(&mut hasher, "size", &meta.len().to_le_bytes());
                fold(&mut hasher, "mtime", &mtime_bytes(&meta));
            }
            // Presence is a member in its own right — an absent tier file that
            // appears must read as a change, not as "nothing to compare".
            Err(_) => fold(&mut hasher, "present", &[0]),
        }
    }

    fold_optional(&mut hasher, "consent_paths", consent_paths.map(str::as_bytes));
    fold_optional(&mut hasher, "consent_namespaces", consent_namespaces.map(str::as_bytes));

    hex::encode(hasher.finalize())
}

/// [`fingerprint`] over this process's own `OCX_CONSENT_*` environment.
///
/// The two env reads happen **here**, at the one seam every consumer shares —
/// `ocx self activate --reconcile`, which folds the fingerprint it records, and
/// `ocx shell state`, which folds the one it reports — so the fold itself stays
/// pure and unit-testable without a process-wide env lock (A-13). Forgetting one
/// would silently make the cached `inert` verdict unexpirable, and a second copy
/// of this wrapper would let exactly that happen in one consumer and not the
/// other: the reporter would then print a fingerprint the reconciler never
/// computes.
///
/// Blocking: [`fingerprint`]'s one `stat` per member. Call it from a blocking
/// context.
#[must_use]
pub fn current_fingerprint(watch_paths: &[PathBuf], project_dir: Option<&Path>) -> String {
    fingerprint(
        watch_paths,
        project_dir,
        crate::env::var(crate::config::shell::OCX_CONSENT_PATHS).as_deref(),
        crate::env::var(crate::config::shell::OCX_CONSENT_NAMESPACES).as_deref(),
    )
}

/// The **membership** digest of the watch set — the ordered path list, and
/// nothing about the files themselves.
///
/// Deliberately not [`fingerprint`]. That fold answers *"did anything move?"*
/// and mixes in every member's presence, size and mtime, so it changes on every
/// edit; this one answers *"is the shell watching the right files?"* and changes
/// only when the list itself does. The gate baked into the emitted hook body
/// carries the **list**, so the list is what has to be compared against it.
///
/// Recorded in [`Ledger::ws`] as the membership the shell's gate currently
/// holds, which is why the emission that redefines that gate and the write of
/// this value are one step in the caller.
///
/// Truncated to 16 hex characters, on [`Ledger::messages_fp`]'s reasoning: the
/// carrier has a 16 KiB ceiling (C-004) and the only question asked of this
/// value is equality against the previous one.
///
/// Pure — no `stat`, so unlike [`fingerprint`] it costs nothing to call on the
/// hot path.
///
/// [`Ledger::ws`]: super::Ledger::ws
/// [`Ledger::messages_fp`]: super::Ledger::messages_fp
#[must_use]
pub fn watch_set_fingerprint(watch_paths: &[PathBuf]) -> String {
    use sha2::Digest as _;

    let mut hasher = sha2::Sha256::new();
    for path in watch_paths {
        fold(&mut hasher, "path", path.as_os_str().as_encoded_bytes());
    }
    hex::encode(hasher.finalize())[..16].to_owned()
}

/// The watch set's member paths, in the order [`fingerprint`] folds them and
/// the emitted hook body carries them (C-019, C-044, A-13).
///
/// **Candidates, not survivors**: a path that does not exist is a member too,
/// because one becoming present is exactly the change the watch set must
/// notice. Discovery happens here — during the shell-start `ConfigLoader` pass
/// and again only when a recomposition is already due — so the per-prompt path
/// stats this recorded list and parses nothing (C-042).
///
/// One definition, deliberately: the emitted hook body, the fingerprint fold and
/// `ocx shell state`'s evidence table all read this list, and two definitions of
/// *"what makes the environment stale"* drift.
pub fn watch_paths(
    file_structure: &crate::file_structure::FileStructure,
    project_dir: Option<&Path>,
    project_key: Option<&str>,
    recorded_tiers: Option<&[PathBuf]>,
) -> Vec<PathBuf> {
    let mut paths = Vec::with_capacity(9);

    // Members 1-2 — the project tier. `[env]` applies on its own authority
    // independently of the lock, so watching locks alone would miss an
    // `[env]`-only edit.
    if let Some(dir) = project_dir {
        paths.push(dir.join("ocx.toml"));
        paths.push(dir.join("ocx.lock"));
    }

    // Members 3-5 — the global tier and the managed-config snapshot.
    let home = file_structure.root();
    paths.push(home.join("ocx.toml"));
    paths.push(home.join("ocx.lock"));
    paths.push(file_structure.state.managed_config_snapshot_file());

    // Member 6's *observable* half. `CARGO_PKG_VERSION` alone does not move on
    // this project's floating `<version>-dev` channel — `self update` swaps
    // `current` to a different binary carrying the same version string — so the
    // binary the `current` symlink resolves to is watched directly. Its mtime
    // and size move whenever the symlink is repointed.
    paths.push(
        file_structure
            .symlinks
            .current(&crate::oci::ocx_cli_identifier())
            .join("content")
            .join("bin"),
    );

    // Member 8 — the config tiers (A-13, A-33).
    //
    // The **recorded** list wins whenever there is one: it came from
    // `LoadedConfig::config_tier_paths`, which honours `OCX_NO_CONFIG` and
    // includes the `--config` overlay that a per-prompt process cannot see for
    // itself. Re-deriving is the fallback for the one run that has no record
    // yet, and it is deliberately the *same* arithmetic the loader uses.
    match recorded_tiers {
        Some(recorded) => paths.extend(recorded.iter().cloned()),
        None => {
            if !crate::env::flag("OCX_NO_CONFIG", false) {
                paths.push(crate::config::loader::ConfigLoader::system_path());
                paths.extend(crate::config::loader::ConfigLoader::user_path());
                paths.extend(crate::config::loader::ConfigLoader::home_path());
            }
            if let Some(explicit) = crate::env::var(crate::env::keys::OCX_CONFIG).filter(|value| !value.is_empty()) {
                paths.push(PathBuf::from(explicit));
            }
        }
    }

    // Member 9 — the project's consent stamp. Without it a grant written from
    // another terminal never expires the cached `inert` verdict.
    if let Some(key) = project_key {
        paths.push(file_structure.state.consent_stamp_file(key));
    }

    paths
}

/// Absorb one named member, length-prefixed so two different member lists can
/// never collide by concatenating to the same byte stream.
fn fold(hasher: &mut sha2::Sha256, tag: &str, bytes: &[u8]) {
    use sha2::Digest as _;
    hasher.update(tag.as_bytes());
    hasher.update([0u8]);
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

/// [`fold`] with an explicit presence byte, so unset and set-but-empty differ.
fn fold_optional(hasher: &mut sha2::Sha256, tag: &str, bytes: Option<&[u8]>) {
    match bytes {
        Some(bytes) => {
            fold(hasher, tag, &[1]);
            fold(hasher, tag, bytes);
        }
        None => fold(hasher, tag, &[0]),
    }
}

/// The full modification time, sign byte first so a pre-epoch mtime folds
/// distinctly from its post-epoch mirror (A-14 — never seconds-truncated).
fn mtime_bytes(meta: &std::fs::Metadata) -> Vec<u8> {
    // A filesystem with no modification time contributes the empty member
    // rather than a fabricated one; presence and size still fold above.
    let Ok(modified) = meta.modified() else {
        return Vec::new();
    };
    let (sign, delta) = match modified.duration_since(std::time::UNIX_EPOCH) {
        Ok(delta) => (1u8, delta),
        Err(before_epoch) => (0u8, before_epoch.duration()),
    };
    let mut bytes = Vec::with_capacity(13);
    bytes.push(sign);
    bytes.extend_from_slice(&delta.as_secs().to_le_bytes());
    bytes.extend_from_slice(&delta.subsec_nanos().to_le_bytes());
    bytes
}

#[cfg(test)]
mod fingerprint_tests {
    use std::path::PathBuf;

    use super::*;

    /// Every member of the watch set, folded once, with no consent channel set.
    fn fold(paths: &[PathBuf], project_dir: Option<&Path>) -> String {
        fingerprint(paths, project_dir, None, None)
    }

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).expect("write fixture");
        path
    }

    /// A-14 — force an mtime collision explicitly. `std::fs::FileTimes` is the
    /// stdlib seam for this, so the forced-collision fixtures need no
    /// dev-dependency of their own.
    fn set_mtime(path: &Path, time: std::time::SystemTime) {
        let file = std::fs::File::options()
            .write(true)
            .open(path)
            .expect("open the fixture for set_times");
        file.set_times(std::fs::FileTimes::new().set_modified(time))
            .expect("set the fixture mtime");
    }

    fn unix_time(seconds: u64, nanos: u32) -> std::time::SystemTime {
        std::time::UNIX_EPOCH + std::time::Duration::new(seconds, nanos)
    }

    /// C-019 — the fold is deterministic: the same watch set folds to the same
    /// string, so an unchanged environment never reports itself stale.
    #[test]
    fn fingerprint_is_stable_for_an_unchanged_watch_set_c019() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let toml = write(dir.path(), "ocx.toml", "[tools]\n");
        let paths = vec![toml];

        assert_eq!(
            fold(&paths, Some(dir.path())),
            fold(&paths, Some(dir.path())),
            "an unchanged watch set must fold to the same fingerprint"
        );
    }

    /// C-019 member 8 — **presence** is folded, not only content: a tier file
    /// that did not exist becomes a change the moment it is created.
    #[test]
    fn fingerprint_changes_when_an_absent_member_appears_c019() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let absent = dir.path().join("config.toml");
        let paths = vec![absent.clone()];

        let before = fold(&paths, None);
        std::fs::write(&absent, "[shell]\n").expect("create the tier file");

        assert_ne!(
            before,
            fold(&paths, None),
            "creating a recorded config tier must change the fingerprint (C-019 member 8)"
        );
    }

    /// C-019 — size is folded, so an edit that keeps the mtime still moves the
    /// fingerprint when the length changes.
    #[test]
    fn fingerprint_changes_when_a_member_changes_size_c019() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let lock = write(dir.path(), "ocx.lock", "version = 3\n");
        let recorded = std::fs::metadata(&lock).expect("metadata").modified().expect("mtime");
        let paths = vec![lock.clone()];
        let before = fold(&paths, None);

        std::fs::write(&lock, "version = 3\n# a longer body\n").expect("rewrite");
        // A-14 — force the mtime collision explicitly rather than racing the
        // clock, so the assertion observes the *size* member and not the
        // filesystem's timestamp granularity.
        set_mtime(&lock, recorded);

        assert_ne!(before, fold(&paths, None), "a size change must move the fingerprint");
    }

    /// A-14 — the ceiling, stated as a test rather than discovered in the
    /// field: an unchanged `(mtime, size)` pair is invisible. Forced, never
    /// raced — the mtime is explicitly set back to the recorded value.
    /// EC-FS-001 — the same-second ceiling is an unchanged (mtime, size) pair, per A-14.
    #[test]
    fn fingerprint_ceiling_is_an_unchanged_mtime_size_pair_a14() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let lock = write(dir.path(), "ocx.lock", "aaaa");
        let recorded = std::fs::metadata(&lock).expect("metadata").modified().expect("mtime");
        let paths = vec![lock.clone()];
        let before = fold(&paths, None);

        std::fs::write(&lock, "bbbb").expect("rewrite, same length");
        set_mtime(&lock, recorded);

        assert_eq!(
            before,
            fold(&paths, None),
            "the named ceiling is that an unchanged (mtime, size) pair is invisible (A-14)"
        );
    }

    /// A-14 — the fold compares the **full** `SystemTime`, never a
    /// seconds-truncated value. Two mtimes inside the same second must produce
    /// different fingerprints.
    ///
    /// Skipped, with the probe asserted, on a filesystem that stores whole
    /// seconds. A-14 names FAT/exFAT (2 s) and NFS (1 s) as widening the ceiling
    /// and Windows as a first-class host for them, so asserting sub-second
    /// precision there would be a portability red about the *filesystem*, not
    /// about the fold. The probe is the skip's evidence: it reads the stored
    /// value back rather than assuming the write took.
    #[test]
    fn fingerprint_folds_subsecond_mtime_precision_a14() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let lock = write(dir.path(), "ocx.lock", "aaaa");
        let paths = vec![lock.clone()];

        set_mtime(&lock, unix_time(1_700_000_000, 0));
        let whole_second = fold(&paths, None);
        set_mtime(&lock, unix_time(1_700_000_000, 500_000_000));
        let stored = std::fs::metadata(&lock)
            .expect("metadata")
            .modified()
            .expect("mtime")
            .duration_since(std::time::UNIX_EPOCH)
            .expect("post-epoch");
        if stored.subsec_nanos() == 0 {
            // The filesystem discarded the sub-second half; there is nothing
            // for the fold to distinguish and the ceiling is the storage's.
            return;
        }

        assert_ne!(
            whole_second,
            fold(&paths, None),
            "a sub-second mtime difference must move the fingerprint — a seconds-truncated \
             fold would report these two as identical (A-14)"
        );
    }

    /// A-13 — the raw `OCX_CONSENT_PATHS` value folds in, so a grant exported
    /// from another terminal expires the cached `inert` verdict at the next
    /// prompt instead of waiting for a shell restart.
    #[test]
    fn fingerprint_folds_the_raw_consent_paths_value_a13() {
        let unset = fingerprint(&[], None, None, None);
        let granted = fingerprint(&[], None, Some("/work/proj"), None);

        assert_ne!(
            unset, granted,
            "OCX_CONSENT_PATHS must fold into fp (A-13) — without it the negative-consent \
             cache is unexpirable"
        );
    }

    /// A-13 — same for `OCX_CONSENT_NAMESPACES`, and set-but-empty is a third
    /// state distinct from unset.
    #[test]
    fn fingerprint_folds_the_raw_consent_namespaces_value_a13() {
        let unset = fingerprint(&[], None, None, None);
        let empty = fingerprint(&[], None, None, Some(""));
        let granted = fingerprint(&[], None, None, Some("ocx.sh/acme"));

        assert_ne!(unset, empty, "set-but-empty must not fold as unset");
        assert_ne!(empty, granted, "the namespace value itself must fold");
    }

    /// C-019 member 7 — which project the CWD walk resolved is part of the
    /// fingerprint, so `cd`-ing between two projects is a change even when
    /// every watched file is untouched.
    /// EC-CFG-013 — the resolved project directory is folded into the fingerprint, so a scope switch moves it.
    #[test]
    fn fingerprint_folds_the_resolved_project_directory_c019() {
        let first = fingerprint(&[], Some(Path::new("/work/one")), None, None);
        let second = fingerprint(&[], Some(Path::new("/work/two")), None, None);
        let none = fingerprint(&[], None, None, None);

        assert_ne!(first, second, "a different project directory must fold differently");
        assert_ne!(first, none, "no project at all is its own state");
    }

    /// C-019 — the watch set is ordered: the fold is over the recorded list, so
    /// two different lists never collide by concatenation.
    #[test]
    fn fingerprint_distinguishes_member_order_c019() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let one = write(dir.path(), "a", "x");
        let two = write(dir.path(), "b", "y");

        assert_ne!(
            fold(&[one.clone(), two.clone()], None),
            fold(&[two, one], None),
            "the fold must be length-prefixed per member, not a plain concatenation"
        );
    }
}

#[cfg(test)]
mod watch_set_fingerprint_tests {
    use super::*;

    /// The membership digest must move when the **list** moves, which is the
    /// only signal that says the shell's baked gate is out of date (#347).
    #[test]
    fn a_changed_membership_changes_the_digest() {
        let before = [PathBuf::from("/w/ocx.toml"), PathBuf::from("/w/ocx.lock")];
        let after = [
            PathBuf::from("/w/ocx.toml"),
            PathBuf::from("/w/ocx.lock"),
            PathBuf::from("/w/sub/ocx.toml"),
        ];

        assert_ne!(
            watch_set_fingerprint(&before),
            watch_set_fingerprint(&after),
            "a member entering the set must be visible to the gate comparison"
        );
        // Built again from scratch rather than cloned: the digest has to depend
        // on the paths, not on the identity of the slice it was handed.
        let same_members = [PathBuf::from("/w/ocx.toml"), PathBuf::from("/w/ocx.lock")];
        assert_eq!(
            watch_set_fingerprint(&before),
            watch_set_fingerprint(&same_members),
            "the same list must fold to the same digest, or the gate is redefined every prompt"
        );
    }

    /// Order is part of the membership: the emitted gate carries the terms in
    /// this order, so two lists that differ only in order are two different
    /// gates and the comparison has to say so.
    #[test]
    fn order_is_part_of_the_membership() {
        let one = [PathBuf::from("/w/a"), PathBuf::from("/w/b")];
        let other = [PathBuf::from("/w/b"), PathBuf::from("/w/a")];

        assert_ne!(watch_set_fingerprint(&one), watch_set_fingerprint(&other));
    }

    /// It folds **names only**. A member whose content moves is
    /// [`fingerprint`]'s business; conflating the two would redefine the gate on
    /// every `ocx.lock` edit, which is a fresh hook body emitted into the shell
    /// on most prompts.
    #[test]
    fn the_files_themselves_are_not_folded() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let member = directory.path().join("ocx.lock");
        std::fs::write(&member, b"one").expect("write");
        let watch = [member.clone()];

        let before = watch_set_fingerprint(&watch);
        std::fs::write(&member, b"a different length entirely").expect("rewrite");
        let after = watch_set_fingerprint(&watch);

        assert_eq!(before, after, "content is `fingerprint`'s job, never this one");
        // …and the pairing that proves the sentence above is not vacuous: the
        // content fold *does* see the same edit.
        assert_ne!(
            fingerprint(&watch, None, None, None),
            {
                std::fs::write(&member, b"changed again, at a third length").expect("rewrite");
                fingerprint(&watch, None, None, None)
            },
            "the content fold must react to what this one ignores"
        );
    }

    /// A path that does not exist is a member, exactly as in [`watch_paths`] —
    /// no `stat` is reached at all, which is why this is free on the hot path.
    #[test]
    fn an_absent_member_still_folds() {
        let absent = [PathBuf::from("/definitely/not/here/ocx.toml")];
        assert_eq!(watch_set_fingerprint(&absent).len(), 16);
        assert_ne!(watch_set_fingerprint(&absent), watch_set_fingerprint(&[]));
    }
}

#[cfg(test)]
mod watch_path_tests {
    use super::*;
    use crate::file_structure::FileStructure;

    /// C-019 — the project tier contributes **both** `ocx.toml` and `ocx.lock`:
    /// `[env]` applies on its own authority independently of the lock, so
    /// watching locks alone would miss an `[env]`-only edit.
    #[test]
    fn watch_paths_carry_both_project_files_c019() {
        let file_structure = FileStructure::with_root(PathBuf::from("/tmp/ocx_home"));
        let project = Path::new("/work/proj");

        let paths = watch_paths(&file_structure, Some(project), None, None);

        assert!(
            paths.contains(&project.join("ocx.toml")),
            "project ocx.toml is member 1"
        );
        assert!(
            paths.contains(&project.join("ocx.lock")),
            "project ocx.lock is member 2"
        );
    }

    /// C-019 members 3-5 — the global tier's pair and the managed-config
    /// snapshot are members whether or not a project resolved.
    #[test]
    fn watch_paths_carry_the_global_tier_without_a_project_c019() {
        let file_structure = FileStructure::with_root(PathBuf::from("/tmp/ocx_home"));

        let paths = watch_paths(&file_structure, None, None, None);

        assert!(paths.contains(&PathBuf::from("/tmp/ocx_home/ocx.toml")));
        assert!(paths.contains(&PathBuf::from("/tmp/ocx_home/ocx.lock")));
        assert!(paths.contains(&file_structure.state.managed_config_snapshot_file()));
    }

    /// A-13 / A-33 — a **recorded** tier list is used verbatim, so the
    /// `--config` overlay a per-prompt process cannot re-derive still reaches
    /// the watch set. Without this the cached `inert` verdict is unexpirable
    /// for a grant made through that channel.
    ///
    /// Red state: drop the `Some(recorded)` arm so `watch_paths` always
    /// re-derives, and the explicit tier disappears from the watch set.
    #[test]
    fn watch_paths_use_the_recorded_tier_list_verbatim_a13() {
        let file_structure = FileStructure::with_root(PathBuf::from("/tmp/ocx_home"));
        let explicit = PathBuf::from("/etc/fleet/consent.toml");
        let recorded = vec![PathBuf::from("/etc/ocx/config.toml"), explicit.clone()];

        let with = watch_paths(&file_structure, None, None, Some(&recorded));
        let derived = watch_paths(&file_structure, None, None, None);

        assert!(
            with.contains(&explicit),
            "a recorded --config tier must reach the watch set (A-13, A-33); got: {with:?}"
        );
        assert!(
            !derived.contains(&explicit),
            "re-derivation structurally cannot see --config - that is why the list is recorded"
        );
    }

    /// A-13 member 9 — the consent stamp joins the watch set, which is what
    /// makes the cached `inert` verdict expirable by a grant written from
    /// another terminal.
    #[test]
    fn watch_paths_carry_the_consent_stamp_a13() {
        let file_structure = FileStructure::with_root(PathBuf::from("/tmp/ocx_home"));

        let without = watch_paths(&file_structure, None, None, None);
        let with = watch_paths(
            &file_structure,
            Some(Path::new("/work/proj")),
            Some("a1b2c3d4e5f60718"),
            None,
        );

        let stamp = file_structure.state.consent_stamp_file("a1b2c3d4e5f60718");
        assert!(with.contains(&stamp), "the consent stamp is a watch-set member (A-13)");
        assert!(!without.contains(&stamp), "no project key, no stamp member");
    }
}
