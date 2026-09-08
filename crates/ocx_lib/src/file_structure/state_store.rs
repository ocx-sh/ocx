// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::log;
use crate::oci;
use crate::prelude::StringExt as _;

/// Filename of the managed-config snapshot metadata (`source`/`tag`/`digest`/
/// `fetched_at`) — a small JSON file. The payload it describes lives beside it
/// in [`MANAGED_CONFIG_PAYLOAD_FILE`].
const MANAGED_CONFIG_SNAPSHOT_FILE: &str = "snapshot.json";

/// Filename of the managed-config payload — the raw `config.toml` bytes the
/// metadata snapshot describes, kept as a human-readable sibling of
/// [`MANAGED_CONFIG_SNAPSHOT_FILE`] (readable, greppable, diffable) rather than
/// embedded as an escaped string inside the JSON.
const MANAGED_CONFIG_PAYLOAD_FILE: &str = "config.toml";

/// Filename of a project's shell-activation consent stamp.
const CONSENT_STAMP_FILE: &str = "consent.json";

/// Filename of a rendered toolchain tree's render stamp (C-003).
///
/// The same basename in both tiers, at two different roots: `state/` for the
/// global tree, `state/projects/<key>/` for a project's — see
/// [`StateStore::global_render_stamp_file`] for why the global one may not
/// live under `projects/`.
const RENDER_STAMP_FILE: &str = "render_stamp.json";

/// The only [`RenderStamp`] schema version this binary understands.
///
/// A stamp at any other `v` reads as **absent**, never as an error: the
/// caller's answer to an absent stamp is "re-render", which is always safe,
/// whereas an error would fail a prompt over a file that is pure derived
/// state. Same rule, same reason, as `consent`'s `STAMP_VERSION`.
const RENDER_STAMP_VERSION: u8 = 1;

// Why the key canonicalizes the project FILE and then takes its parent, rather
// than canonicalizing the directory: `resolve_explicit_project_path` follows
// symlinks by design and returns the UN-canonicalized path, so
// `OCX_PROJECT=/w/fake/ocx.toml` symlinked to `/attacker/ocx.toml` would key —
// and grant `paths` consent — under `/w/fake`. Canonicalizing the file makes
// the identity `/attacker`, which is not granted. The two-call order is also
// load-bearing on Windows: `tokio::fs::canonicalize` on the config file and
// `dunce::canonicalize` on the directory do not produce the same string, and
// the ledger's key is the second form. Reuse `register_project_dir_best_effort`
// as the one shared helper; never add a second, directory-based derivation.

/// One `bin/` entry's identity in a [`RenderStamp`] (C-003, D-V13).
///
/// **Per entry, never one folded digest.** C-061's prompt gate compares
/// `(name, size, file id)` against the stamp and hashes only the entries that
/// differ; a single rolled-up digest gives it nothing to compare and no way to
/// recover an individual entry's hash, which degenerates the gate into the
/// unconditional re-hash it exists to avoid.
///
/// **No field covers mtime**, here or in [`RenderStamp`] — a stated invariant,
/// not an oversight. mtime is trivially forgeable and is preserved by an
/// in-place overwrite, so it would report "unchanged" for exactly the edit the
/// stamp is meant to notice.
///
/// # Residual: an in-place overwrite passes the stat gate (R-W4)
///
/// The gate's cheap half is `(name, size, file id)`, and an **in-place**
/// overwrite — written without a rename, which preserves the inode on POSIX —
/// changes none of the three when the replacement is the same size under the
/// same name. Such an entry is never re-hashed, on the prompt path or on the
/// emit path, so its `content_hash` is believed rather than checked. The tree
/// this happens in is one the ADR itself calls attacker-writable, and the
/// stamp exists for the committed-`bin/` attack in the first place, so the
/// residual belongs on the type where a caller reasoning about the gate will
/// meet it — not only in the plan that recorded it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BinEntryStamp {
    /// The entry's size in bytes — half of the cheap stat comparison, and the
    /// half a `read_dir` walk does not yield on Unix.
    pub size: u64,

    /// The entry's filesystem identity: the inode on Unix, the file index on
    /// Windows. `None` when the platform would not report one, or when the
    /// entry could not be re-opened to ask — which makes the gate fall through
    /// to the content hash rather than guess.
    ///
    /// Not speculative on Windows: C-003 already requires proving a
    /// trampoline `.exe`'s hardlink identity against the `ShimBinStore` blob,
    /// which *is* the file index.
    ///
    /// The one `Option` in the stamp tree, and deliberately so: serde resolves
    /// a **missing** key here to `None` with no `#[serde(default)]` involved,
    /// which lands on exactly the fall-through-to-the-content-hash behaviour
    /// the absent case already has. Do not read [`RenderStamp`]'s serde
    /// paragraph as covering this field — there, that same resolution is the
    /// hazard [`RenderStampScope`] exists to remove; here it is the intent.
    pub file_id: Option<u64>,

    /// Hex digest of the entry's bytes
    /// ([`oci::Algorithm::Sha256`](crate::oci::Algorithm::Sha256)) — the
    /// authority the stat pair is only an index into.
    pub content_hash: String,
}

impl BinEntryStamp {
    /// One entry's stamp, taken from its `metadata` and an
    /// already-computed `content_hash`.
    ///
    /// **This is the one derivation of `file_id`, on purpose.** The stamp is
    /// produced in one work package and compared in another, in different
    /// files: if the two ends derived "file id" differently on Windows the
    /// stat gate would silently mis-answer — every entry looking changed, or
    /// none — which is exactly the class C-061 exists to prevent, arriving
    /// through the seam *between* the two rather than inside either. Neither
    /// side re-derives it; both call this.
    ///
    /// The `cfg` split is the inode on Unix and the NTFS file index on
    /// Windows — see [`file_id_of`], which owns both and documents why the
    /// Windows arm goes through an open handle rather than through `metadata`.
    ///
    /// Takes **both** `path` and `metadata`, and each is load-bearing for one
    /// platform: `size` and the Unix inode come from the `metadata` the caller
    /// already took — re-stat'ing here would be wasted work and a TOCTOU
    /// widening — while the Windows file index cannot be read out of a
    /// `Metadata` on stable at all, so that arm opens `path`. `path` must
    /// therefore be the path the `metadata` was taken from; handing it a
    /// different one produces a stamp describing two files.
    #[must_use]
    pub fn from_metadata(path: &Path, metadata: &std::fs::Metadata, content_hash: String) -> Self {
        Self {
            size: metadata.len(),
            file_id: file_id_of(path, metadata),
            content_hash,
        }
    }

    /// One entry's stamp read from the file itself — the stat pair from
    /// `metadata`, the `content_hash` from the bytes on disk.
    ///
    /// **The one hashing derivation, called by both ends** (RUL-89): the render
    /// that *writes* a `bin_fingerprint` and C-061's prompt gate that *checks
    /// one*. The two live in different modules and different work packages, and
    /// a hash spelled twice is the class of defect this tree has already met —
    /// a contract documented at one site and violated at its sibling. Neither
    /// side may spell `read_bounded` + SHA-256 itself.
    ///
    /// The read is bounded by `metadata.len()`, so an entry that grew between
    /// the stat and the read answers `None` rather than being hashed at a size
    /// nobody observed. `None` is also every other read failure — a vanished
    /// entry, a permission refusal, something that is no longer a regular file.
    /// Both ends already have one answer for that: the producer omits the
    /// entry, the gate calls it a mismatch.
    ///
    /// Takes `&Metadata` beside the path for the reason
    /// [`Self::from_metadata`] does: the caller has already stat'ed the entry
    /// to decide it is a regular file, and re-stat'ing here would be wasted
    /// work and a TOCTOU widening.
    ///
    /// Blocking: one read. Async callers wrap it in `spawn_blocking`.
    #[must_use]
    pub fn of_file(path: &Path, metadata: &std::fs::Metadata) -> Option<Self> {
        let bytes = crate::utility::fs::read_bounded(path, metadata.len())
            .inspect_err(|e| log::debug!("Could not hash '{}': {e}", path.display()))
            .ok()?;
        let content_hash = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&bytes));
        Some(Self::from_metadata(path, metadata, content_hash))
    }
}

/// The Unix inode — the platform's own answer to "is this the same file".
///
/// `path` is unused: `stat(2)` already carried the inode, so the metadata the
/// caller took is the whole answer. It is in the signature for the Windows arm
/// below, which cannot read the identity out of a `Metadata` at all.
#[cfg(unix)]
fn file_id_of(_path: &Path, metadata: &std::fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt as _;
    Some(metadata.ino())
}

/// The NTFS file index, read from an **open handle** rather than from
/// `metadata` — the Windows answer to "is this the same file".
///
/// # Why not `MetadataExt::file_index()`
///
/// It is the obvious spelling and it does not compile on stable: it sits behind
/// the unstable `windows_by_handle` feature (rust-lang/rust#63010), and the
/// stabilization blocker is that the 64-bit index is not a complete identity on
/// every Windows filesystem (ReFS has a 128-bit file id). `hardlink.rs`'s W3
/// test already routes around it through `GetFileInformationByHandle`; this is
/// the same datum by the same route, on the production path.
///
/// The handle route is also strictly better than the accessor would have been:
/// the index a `read_dir`-produced `Metadata` does not carry is always present
/// when the identity is read from a handle, so the caveat that a caller "must
/// stat the path itself" is gone. The 64-bit index is compared only against
/// another index taken the same way on the same volume — the identical
/// assumption the Unix arm makes by comparing a bare `ino()` without a `dev()`.
///
/// # Why a real identity, and not `None`
///
/// `None` here would be sound — the content hash is the authority the file id
/// only indexes into — but it would silently retire two Windows-only
/// behaviours. C-061's per-prompt gate would SHA-256 every trampoline
/// (a ~300 KiB `.exe` per tool) on every prompt instead of comparing a stat
/// pair, and `windows_pair_unchanged` — whose *entire* subject is the `.exe`'s
/// hardlink identity against the [`ShimBinStore`](super::ShimBinStore) blob —
/// would take its fail-closed `None` branch forever, so every render would
/// republish every Windows trampoline and `--dry-run` would report a delta that
/// no render can ever clear. The mitigation would then never run on the only
/// platform it exists for.
///
/// # Failure is `None`
///
/// An absent path, a refused open, an API failure: all `None`, which is the
/// documented fall-through to the content hash. `FILE_FLAG_OPEN_REPARSE_POINT`
/// keeps the open on the name the caller judged rather than following a link to
/// its target — every caller decides `is_file()` on a `symlink_metadata` first,
/// and this makes the handle honour that decision even if the name is swapped
/// in between.
#[cfg(windows)]
fn file_id_of(path: &Path, _metadata: &std::fs::Metadata) -> Option<u64> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::os::windows::io::AsRawHandle as _;

    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_FLAG_OPEN_REPARSE_POINT, GetFileInformationByHandle,
    };

    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .inspect_err(|e| log::debug!("Could not open '{}' for its file index: {e}", path.display()))
        .ok()?;
    // SAFETY: `BY_HANDLE_FILE_INFORMATION` is a plain-old-data struct of
    // integers and `FILETIME`s, so an all-zero bit pattern is a valid — if
    // meaningless — value. It is overwritten by the call below before anything
    // reads it, and the call's failure arm returns without reading it at all.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: `file` owns a live handle for the whole call, and `&mut info`
    // points at writable, aligned storage of exactly the size the API writes.
    let queried = unsafe { GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut info) };
    if queried == 0 {
        log::debug!(
            "GetFileInformationByHandle failed for '{}': {}",
            path.display(),
            std::io::Error::last_os_error()
        );
        return None;
    }
    Some((u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow))
}

/// No file identity is available, so the gate falls through to the content
/// hash — the same behaviour as an absent `file_id`.
#[cfg(not(any(unix, windows)))]
fn file_id_of(_path: &Path, _metadata: &std::fs::Metadata) -> Option<u64> {
    None
}

/// What one render left on disk, written **last** so a crash mid-render leaves
/// a tree that is detectably incomplete rather than one that looks finished
/// (C-003, C-048).
///
/// Lives at [`StateStore::render_stamp_file`] for a project tree and at
/// [`StateStore::global_render_stamp_file`] for the global one, written
/// through [`write_bytes_atomic`](crate::utility::fs::write_bytes_atomic) and
/// replaced, never edited in place — the same discipline as the consent stamp.
///
/// Serde matches [`ConsentStamp`](crate::project::consent::ConsentStamp)
/// deliberately: `deny_unknown_fields` and **no `#[serde(default)]` on any
/// field**. What that pair buys is narrower than it reads, and the gap is why
/// [`RenderStampScope`] exists rather than an `Option<PathBuf>`:
/// `deny_unknown_fields` governs *extra* keys only, and serde resolves a
/// **missing** `Option<T>` to `None` on its own, with no `#[serde(default)]`
/// involved. A tier spelled `Option<PathBuf>` whose key was dropped —
/// truncation, a hand edit, a hostile rewrite — would therefore deserialize
/// cleanly and present as the global tier, skipping the identity check D-V13
/// added it for.
///
/// So no field of this struct is `Option`-shaped: dropping any one of them is
/// a deserialize *error*, which [`StateStore::render_stamp`] reports as an
/// absent stamp and answers by re-rendering.
/// [`BinEntryStamp::file_id`] is the tree's one `Option`, and safe — see that
/// field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderStamp {
    /// Stamp schema version. A `v` this binary does not recognise makes the
    /// stamp unusable, which is the same as absent.
    ///
    /// **Private, unlike every other field**, so [`Self::new`] is the only way
    /// to build one: a public `v` leaves `RenderStamp { v: 1, .. }` spellable
    /// as a struct literal, which is the second source of truth for a
    /// persisted wire version the constructor exists to prevent. Serde is
    /// unaffected — field privacy is not a serialization property — so the key
    /// stays on the wire under its own name. Read it with [`Self::version`].
    v: u8,

    /// The toolchain home this stamp describes — the lookup key half of the
    /// pair.
    pub home: PathBuf,

    /// Which tier the home was rendered for, carrying the canonical project
    /// directory when it was a project one (D-V13).
    ///
    /// The identity half, beside `home`'s lookup key — the same split
    /// [`ConsentStamp`](crate::project::consent::ConsentStamp) documents,
    /// and load-bearing for the same reason. With `toolchain-dir` configured,
    /// two projects colliding in the 64 bits of `name_for_path` share one home
    /// *and* one stamp, so project B's `bin` mode would pass the gate over
    /// trampolines that bake `--project '<A>'`. Without `toolchain-dir` the
    /// `home` field alone catches that; the defect is `toolchain-dir`-specific
    /// and this is its fix.
    pub scope: RenderStampScope,

    /// The exposed name set the render wrote into `bin/`.
    ///
    /// **Invariant: this is the on-disk entry set, so it must equal
    /// [`Self::bin_fingerprint`]'s key set.** C-003 mandates both fields, and
    /// the two encode the same fact — a stamp where they disagree is
    /// representable but meaningless, and the failure it produces is silent:
    /// wave 3's prompt gate could compare one set while the heal repairs the
    /// other, so an entry present in exactly one of them is either checked and
    /// never repaired or repaired and never checked. Producers build both from
    /// one walk; [`Self::new`] takes them as one pair for that reason.
    pub names: BTreeSet<String>,

    /// `bin/` entry name → its [`BinEntryStamp`]. See that type for why the
    /// map is per-entry.
    pub bin_fingerprint: BTreeMap<String, BinEntryStamp>,

    /// `"<group>/<entry>"` → the digest root the link points at,
    /// **default group only**.
    ///
    /// **The key is `<group>/<entry>`, not the on-disk path.** The links moved
    /// under `links/` when depth 1 became a closed, tree-owned set (C-071), and
    /// this key did **not** move with them: it is derived from the group name
    /// and the entry name, never from where the render happens to put them, so
    /// the next layout change does not rewrite a persisted format for nothing.
    /// Nothing compares it against a path — see
    /// `render_toolchain`'s
    /// `the_stamped_link_fingerprint_key_is_group_slash_entry`, which is the
    /// only thing standing between this format and a silent, unobservable
    /// break — and which lives beside the producer, because a check over a
    /// hand-built sample here would pin the sample and not the code.
    ///
    /// Scoped to exactly what `bin` mode's per-prompt heal can repair (C-062).
    /// Widening it would make every non-default-group repoint mismatch the
    /// stamp on every prompt while the prompt path — which does not heal those
    /// groups — withheld the entry and printed the `ocx pull` hint forever.
    /// Non-default groups are protected on the composing side instead (C-070).
    pub link_fingerprint: BTreeMap<String, String>,
}

impl RenderStamp {
    /// A stamp at the schema version this binary writes.
    ///
    /// **The constructor exists so `v` is never a producer's decision.**
    /// `RENDER_STAMP_VERSION` is module-private and the first producer lives
    /// in another module, so without this a producer would hardcode `1` and
    /// create a second source of truth for a *persisted wire version* — the
    /// one number that must not drift, since the reader treats an
    /// unrecognised `v` as an absent stamp and silently re-renders forever.
    /// It also deletes "did you remember `v`?" from that producer's review.
    ///
    /// **`names` is derived here, not accepted**, and that is the whole
    /// enforcement of [`Self::names`]'s invariant. Taking the two as a pair
    /// still let a producer pass a set that disagreed with the map, and the
    /// disagreement is silent by construction — wave 3's prompt gate would
    /// compare one set while the heal repaired the other. Deriving makes the
    /// meaningless state unconstructible through the one sanctioned
    /// constructor rather than merely detectable after the fact; the field
    /// stays on the wire because C-003 mandates it.
    #[must_use]
    pub fn new(
        home: PathBuf,
        scope: RenderStampScope,
        bin_fingerprint: BTreeMap<String, BinEntryStamp>,
        link_fingerprint: BTreeMap<String, String>,
    ) -> Self {
        Self {
            v: RENDER_STAMP_VERSION,
            home,
            scope,
            names: bin_fingerprint.keys().cloned().collect(),
            bin_fingerprint,
            link_fingerprint,
        }
    }

    /// The stamp's schema version.
    ///
    /// The read half of [`Self::v`]'s privacy: the field is not `pub` so that
    /// the constructor is the only writer, which leaves callers outside this
    /// module needing a way to see it.
    #[must_use]
    pub fn version(&self) -> u8 {
        self.v
    }
}

/// Which tier a persisted [`RenderStamp`] describes (C-003, D-V13).
///
/// A **required, tagged** field of the stamp rather than an
/// `Option<PathBuf>`: a dropped tag is a deserialize error instead of a silent
/// fall-back to the global tier (see [`RenderStamp`]), and "a global stamp
/// carrying a project directory" is unrepresentable in the persisted form the
/// way [`RenderStampTarget`] makes it unrepresentable in the API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderStampScope {
    /// A project tree, rendered for this **canonical** project directory —
    /// the file-first derivation, i.e. the same path
    /// [`ConsentStamp::project_dir`](crate::project::consent::ConsentStamp)
    /// records, not merely the key it hashes to.
    Project(PathBuf),
    /// The global tree at `$OCX_HOME/toolchain`, which has no project.
    Global,
}

/// Which tier's render stamp a [`StateStore`] call addresses (C-003).
///
/// One value rather than two method pairs, because the tier is a runtime
/// choice its caller makes once and carries — and because it gives
/// [`StateStore::render_stamp`] and [`StateStore::set_render_stamp`] a
/// parameter in which "wrote the global stamp under a project key" cannot be
/// spelled.
///
/// That is the whole of what it buys, and it is worth stating narrowly: the
/// two path accessors, [`StateStore::render_stamp_file`] and
/// [`StateStore::global_render_stamp_file`], are both public and ungated, so a
/// caller that derives a path itself and writes it can still put either stamp
/// anywhere. The enum constrains this API pair, not the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderStampTarget<'a> {
    /// A project tree, keyed by
    /// [`ReferenceManager::name_for_path`](crate::reference_manager::ReferenceManager::name_for_path)
    /// of its canonical directory — the same key the consent stamp uses.
    Project(&'a str),
    /// The global tree at `$OCX_HOME/toolchain`.
    Global,
}

/// Manages persistent runtime state files under `$OCX_HOME/state/`.
///
/// `StateStore` is the typed home for state files whose existence or mtime IS
/// the data.  Unlike `cache/` (regenerable bulk), files here are persistent
/// across sessions.
///
/// Layout:
/// ```text
/// {root}/
///   update-check/
///     {slug}          — zero-byte file; mtime = time of last registry probe
///   projects/
///     {key}/
///       consent.json  — per-project shell-activation consent stamp
/// ```
#[derive(Debug, Clone)]
pub struct StateStore {
    root: PathBuf,
}

impl StateStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The root directory of the state store (e.g., `$OCX_HOME/state`).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the directory for update-check throttle state files.
    ///
    /// Path: `{root}/update-check/`
    ///
    /// Each file under this directory has the form `{slug}` where `slug` is
    /// the strict (no-dot) slug of the identifier. The mtime of the file is
    /// the only datum — content is always zero bytes.
    pub fn update_check_dir(&self) -> PathBuf {
        self.root.join("update-check")
    }

    /// Returns the throttle state file path for the given identifier.
    ///
    /// Path: `{root}/update-check/{slug}` where `{slug}` is the strict
    /// (no-dot) slug of `identifier.to_string()` — all non-alphanumeric
    /// characters replaced with `_`. For `ocx.sh/ocx/cli` this produces
    /// `ocx_sh_ocx_cli`.
    ///
    /// The file is zero-byte; its mtime is the only datum (time of last
    /// registry probe). The parent directory is created lazily on first touch.
    ///
    /// # Atomic-touch portability
    ///
    /// The throttle write path ([`StateStore::touch`]) relies on
    /// `std::fs::rename` to publish a new mtime atomically without races on
    /// the destination. Platform semantics:
    ///
    /// - **Linux**: `rename(2)` follows symlinks at source, replaces the
    ///   destination inode atomically without intermediate "absent" state.
    /// - **macOS**: BSD `rename(2)` provides the same atomic-replace
    ///   semantics as Linux.
    /// - **Windows**: `std::fs::rename` shims to `MoveFileExW` with
    ///   `MOVEFILE_REPLACE_EXISTING`, which gives equivalent atomic-replace
    ///   semantics (modulo open-handle constraints on the destination).
    ///
    /// In all three cases the atomic-touch contract holds. See
    /// <https://doc.rust-lang.org/std/fs/fn.rename.html> for the cross-platform
    /// shim's documented invariants.
    pub fn update_check_file(&self, identifier: &oci::Identifier) -> PathBuf {
        let slug = identifier.to_string().to_slug();
        self.update_check_dir().join(slug)
    }

    /// Returns the directory holding the managed-config tier's persistent
    /// state.
    ///
    /// Path: `{root}/managed-config/`
    pub fn managed_config_dir(&self) -> PathBuf {
        self.root.join("managed-config")
    }

    /// Returns the path of the managed-config snapshot metadata file
    /// (`ManagedConfigSnapshot`, written atomically by `persist_managed_config`).
    /// The payload it describes lives in the sibling
    /// [`Self::managed_config_toml_file`].
    ///
    /// Path: `{root}/managed-config/snapshot.json`
    pub fn managed_config_snapshot_file(&self) -> PathBuf {
        self.managed_config_dir().join(MANAGED_CONFIG_SNAPSHOT_FILE)
    }

    /// Returns the path of the managed-config payload file — the raw
    /// `config.toml` bytes the metadata snapshot describes, written as a
    /// readable sibling of `snapshot.json` by `persist_managed_config`.
    ///
    /// Path: `{root}/managed-config/config.toml`
    pub fn managed_config_toml_file(&self) -> PathBuf {
        self.managed_config_dir().join(MANAGED_CONFIG_PAYLOAD_FILE)
    }

    /// Returns the zero-byte freshness marker touched by the background
    /// refresh tick (separate from the snapshot file itself so a throttled
    /// probe never has to touch — and risk racing — the content file).
    ///
    /// Path: `{root}/managed-config/.last-refresh-check`
    pub fn managed_config_refresh_marker(&self) -> PathBuf {
        self.managed_config_dir().join(".last-refresh-check")
    }

    /// Content-bearing pause file for the managed-config background tick
    /// (`ocx config update --pause` — see `managed_config::pause`).
    pub fn managed_config_pause_file(&self) -> PathBuf {
        self.managed_config_dir().join("pause.json")
    }

    /// Returns the per-project state directory for `key`.
    ///
    /// Path: `{root}/projects/<key>/`
    ///
    /// `key` is [`ReferenceManager::name_for_path`](crate::reference_manager::ReferenceManager::name_for_path)
    /// of the project's canonical directory — the first 16 hex of SHA-256, the
    /// same key `refs/symlinks/` and the project ledger already use. The
    /// canonical directory is
    /// `dunce::canonicalize(canonicalize(<resolved project FILE>).parent())`:
    /// canonicalize the **file**, take its parent, then `dunce`.
    ///
    /// Deletable at any time — nothing here is GC truth. Not to be confused
    /// with `$OCX_HOME/projects/`, the symlink ledger whose lifetime is tied to
    /// installs.
    pub fn project_state_dir(&self, key: &str) -> PathBuf {
        self.project_state_root().join(key)
    }

    /// Returns the consent-stamp file for the project keyed by `key`.
    ///
    /// Path: `{root}/projects/<key>/consent.json`
    ///
    /// Read by [`crate::project::consent::load`], written only by
    /// [`crate::project::consent::record`], and **replaced, never edited in
    /// place** — so a future multi-writer surface here uses `lock_scoped` into
    /// `$OCX_HOME/locks`, never a sidecar.
    pub fn consent_stamp_file(&self, key: &str) -> PathBuf {
        self.project_state_dir(key).join(CONSENT_STAMP_FILE)
    }

    /// Returns the sweep root for `ocx clean`'s consent-stamp pass.
    ///
    /// Path: `{root}/projects/`
    ///
    /// `ocx clean` removes `state/projects/<key>/` iff the stamp's own
    /// `project_dir` no longer exists on disk — the one exception to
    /// `state/` not being walked by `ocx clean`.
    pub fn project_state_root(&self) -> PathBuf {
        self.root.join("projects")
    }

    /// Returns the render stamp for the **project** tree keyed by `key`
    /// (C-003).
    ///
    /// Path: `{root}/projects/<key>/render_stamp.json` — beside the shipped
    /// consent stamp, same key, same "deletable at any time" lifetime.
    ///
    /// Named per subsystem like every other accessor on this store; there is
    /// deliberately no generic `state_file(subsystem, key)` API, so the layout
    /// stays greppable and one subsystem cannot key into another's namespace.
    ///
    /// A path accessor, and ungated: reading and writing a stamp goes through
    /// [`Self::render_stamp`] and [`Self::set_render_stamp`], whose
    /// [`RenderStampTarget`] is what keeps a tier from being stamped at the
    /// other tier's path. Reach for this one only when the *path* itself is
    /// the answer — a diagnostic, a sweep, a test.
    pub fn render_stamp_file(&self, key: &str) -> PathBuf {
        self.project_state_dir(key).join(RENDER_STAMP_FILE)
    }

    /// Returns the render stamp for the **global** tree (C-003, D-V13).
    ///
    /// Path: `{root}/render_stamp.json` — directly under `$OCX_HOME/state/`,
    /// **not** under `projects/<key>/`.
    ///
    /// # Why not beside the project stamps
    ///
    /// Two shipped mechanisms make the `projects/` path unusable for the
    /// global tier, and both fail silently rather than loudly:
    ///
    /// - [`consent::record_in`](crate::project::consent) refuses to stamp
    ///   `$OCX_HOME` at all (A-44), so `state/projects/<key-for-$OCX_HOME>/`
    ///   is never created — a global stamp written there would be the first
    ///   thing ever to create a directory the consent invariant asserts
    ///   nothing writes.
    /// - `ocx clean` classifies a `state/projects/<key>/` whose stamp records
    ///   `$OCX_HOME` as `SweepReason::OcxHome` and removes the **whole**
    ///   directory, so the stamp would be deleted on every clean and the
    ///   global gate would re-render from scratch forever.
    pub fn global_render_stamp_file(&self) -> PathBuf {
        self.root.join(RENDER_STAMP_FILE)
    }

    /// Read `target`'s render stamp, or `None` if there is not a usable one
    /// (C-003).
    ///
    /// Returns `None` on **every** failure — absent, unreadable, corrupt JSON,
    /// an unknown field, or a `v` this binary does not recognise — logged at
    /// debug and never warned, exactly as
    /// [`consent::load`](crate::project::consent::load) does: an unusable
    /// stamp is an absent stamp, and the caller's answer is "re-render",
    /// which is always safe.
    ///
    /// Absence is the ordinary state (every unrendered tree, every first
    /// prompt), which is why even a genuine read failure stays at debug. The
    /// debug line must name the cause it actually observed — a parse failure
    /// says the stamp did not parse, never that the file is absent, or the one
    /// diagnostic a user has for a corrupt stamp points them at the wrong
    /// thing.
    ///
    /// Blocking: one synchronous read. Async callers wrap it in
    /// `spawn_blocking` — `ocx pull`'s path reaches this.
    #[must_use]
    pub fn render_stamp(&self, target: RenderStampTarget<'_>) -> Option<RenderStamp> {
        let path = self.render_stamp_path(target);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) => {
                // Absence is the ordinary state — every unrendered tree, every
                // first prompt — so even a genuine read failure stays at debug:
                // the outcome is identical and a WARN would fire on the common
                // case.
                log::debug!("No usable render stamp at '{}': {e}", path.display());
                return None;
            }
        };
        let stamp: RenderStamp = match serde_json::from_slice(&bytes) {
            Ok(stamp) => stamp,
            Err(e) => {
                // Never "absent" here: a corrupt stamp is the one case a user
                // has a diagnostic for, and naming the wrong cause points them
                // at the wrong file.
                log::debug!(
                    "Render stamp '{}' did not parse, treating as absent: {e}",
                    path.display()
                );
                return None;
            }
        };
        if stamp.v != RENDER_STAMP_VERSION {
            log::debug!(
                "Render stamp '{}' is version {}, which this ocx does not recognise; treating as absent",
                path.display(),
                stamp.v
            );
            return None;
        }
        Some(stamp)
    }

    /// Write `stamp` for `target`, replacing any existing one (C-003).
    ///
    /// Written **last** in a render (C-048) and replaced atomically via
    /// [`write_bytes_atomic`](crate::utility::fs::write_bytes_atomic), never
    /// edited in place.
    ///
    /// The parent directory must be created first: `write_bytes_atomic` stages
    /// its temp file *in the target's parent* and explicitly does not create
    /// it, so a `create_dir_all` on the parent precedes the write — the same
    /// sequence, for the same reason, as
    /// [`consent`](crate::project::consent)'s `record_at`.
    ///
    /// Blocking: a `create_dir_all` and an atomic write. Async callers wrap it
    /// in `spawn_blocking` — `ocx pull`'s path reaches this.
    ///
    /// # Errors
    ///
    /// The serialization's failure, or the write's own I/O failure with the
    /// stamp path attached.
    pub fn set_render_stamp(&self, target: RenderStampTarget<'_>, stamp: &RenderStamp) -> crate::Result<()> {
        let path = self.render_stamp_path(target);
        let bytes =
            serde_json::to_vec_pretty(stamp).map_err(|e| crate::error::file_error(&path, std::io::Error::other(e)))?;

        // `write_bytes_atomic` stages its temp file in the target's parent and
        // explicitly does not create it, so the tier's directory has to exist
        // first — the same sequence, for the same reason, as `consent`'s
        // `record_at`.
        let parent = path.parent().ok_or_else(|| {
            crate::error::file_error(
                &path,
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "render stamp path has no parent"),
            )
        })?;
        std::fs::create_dir_all(parent).map_err(|e| crate::error::file_error(parent, e))?;
        crate::utility::fs::write_bytes_atomic(&path, &bytes).map_err(|e| crate::error::file_error(&path, e))
    }

    /// The stamp file for `target` — the one place the tier tag becomes a path.
    ///
    /// Private so [`RenderStampTarget`]'s guarantee holds for this API pair:
    /// resolving the tier in one function is what makes "wrote the global stamp
    /// under a project key" unspellable through [`Self::render_stamp`] and
    /// [`Self::set_render_stamp`].
    fn render_stamp_path(&self, target: RenderStampTarget<'_>) -> PathBuf {
        match target {
            RenderStampTarget::Project(key) => self.render_stamp_file(key),
            RenderStampTarget::Global => self.global_render_stamp_file(),
        }
    }

    /// Returns the OCI Referrers-API capability-cache file for `registry`.
    ///
    /// Path: `{root}/referrers/{registry_slug}.json` where `{registry_slug}` is
    /// the relaxed slug of `registry` (dots preserved, `/`/`:` neutralised) so a
    /// hostile registry string cannot escape the store root. Owned by
    /// [`crate::oci::referrer::capability::ReferrersApiCapability`].
    pub fn referrers_capability_file(&self, registry: &str) -> PathBuf {
        self.root
            .join("referrers")
            .join(format!("{}.json", registry.to_relaxed_slug()))
    }

    /// Returns the host-capability record.
    ///
    /// Path: `{root}/host/capabilities.json`. Keyed by nothing: the record
    /// describes the machine ocx is running on, of which there is one. Owned by
    /// [`crate::oci::host_capabilities::HostCapabilities`].
    pub fn host_capabilities_file(&self) -> PathBuf {
        self.root.join("host").join("capabilities.json")
    }

    /// Returns the offline-verify trust-root-cache file for `rekor_authority`.
    ///
    /// Path: `{root}/trust_root/{rekor_authority_slug}.json` where the slug is
    /// the relaxed slug of the Rekor URL authority (`host[:port]`), so public and
    /// private Sigstore instances never collide and a hostile authority string
    /// cannot escape the store root. Owned by
    /// [`crate::oci::verify::trust_cache::TrustRootCache`].
    /// Checkout directory for the TUF client that fetches the public-good
    /// Sigstore trust root.
    ///
    /// `sigstore`'s TUF client writes the verified targets here so the next run
    /// reuses them instead of re-fetching. Not keyed by anything: there is one
    /// public-good repository.
    pub fn tuf_cache_dir(&self) -> PathBuf {
        self.root.join("tuf")
    }

    pub fn trust_root_file(&self, rekor_authority: &str) -> PathBuf {
        self.root
            .join("trust_root")
            .join(format!("{}.json", rekor_authority.to_relaxed_slug()))
    }

    /// Pure associated fn deriving the managed-config snapshot path from
    /// `ocx_home` (the `$OCX_HOME` root, NOT this store's `state/` root)
    /// without constructing a [`StateStore`].
    ///
    /// Shared by [`crate::config::loader::ConfigLoader`] (which must not
    /// depend on [`crate::file_structure::FileStructure`] or reconstruct a
    /// store) and this store's own [`Self::managed_config_snapshot_file`]
    /// accessor — both must agree on one path so the loader's discovery
    /// candidate and the writer's persist target never drift apart.
    pub fn managed_config_snapshot_path(ocx_home: &Path) -> PathBuf {
        ocx_home
            .join("state")
            .join("managed-config")
            .join(MANAGED_CONFIG_SNAPSHOT_FILE)
    }

    /// Derives the managed-config payload path (`config.toml`) sitting beside the
    /// metadata snapshot at `snapshot_path`.
    ///
    /// Pure sibling derivation for
    /// [`read_managed_config_snapshot_at`](crate::managed_config::read_managed_config_snapshot_at),
    /// which holds only the snapshot path (the config loader never constructs a
    /// [`StateStore`]). Both this and the writer's
    /// [`Self::managed_config_toml_file`] resolve to one path, so reader and
    /// writer can never drift.
    pub fn managed_config_toml_path_for_snapshot(snapshot_path: &Path) -> PathBuf {
        snapshot_path.with_file_name(MANAGED_CONFIG_PAYLOAD_FILE)
    }

    /// Returns `true` when the state file at `path` was last touched within
    /// `interval`, meaning the next probe is not yet due.
    ///
    /// Returns `false` (probe is due) when the file is absent, unreadable, or
    /// older than `interval`. An `interval` of `Duration::ZERO` always returns
    /// `false` (bypass semantics).
    ///
    /// Synchronous (blocking) I/O — callers on an async runtime should wrap
    /// in `tokio::task::spawn_blocking`.
    pub fn is_throttled(path: &Path, interval: Duration) -> bool {
        if interval.is_zero() {
            return false;
        }
        let Ok(metadata) = std::fs::metadata(path) else {
            return false;
        };
        let Ok(mtime) = metadata.modified() else {
            return false;
        };
        let Ok(elapsed) = mtime.elapsed() else {
            // mtime in the future — treat as "file was just touched", i.e. throttled
            return true;
        };
        elapsed < interval
    }

    /// Awaits [`touch_atomic`] on a blocking thread, logging at debug on
    /// failure.
    ///
    /// Drives the sync write off the async executor, swallows the result at
    /// debug, and awaits completion before returning so the throttle window
    /// resets before the next invocation lands.
    ///
    /// `JoinError` (task panic) is silently dropped at debug level: the
    /// throttle file failing to touch is non-fatal — the next invocation
    /// re-probes, which is the correct behaviour after a panic anyway.
    pub async fn touch(path: PathBuf) {
        let _ = tokio::task::spawn_blocking(move || {
            if let Err(touch_err) = touch_atomic(&path) {
                log::debug!("state store: failed to touch state file: {touch_err}");
            }
        })
        .await;
    }
}

/// Creates or updates the state file at `path` atomically.
///
/// Write a zero-byte temp file next to `path`, then `std::fs::rename` into
/// place so other processes observe an atomic mtime update. Parent directory
/// is created lazily on first touch.
///
/// # Portability
///
/// The atomic-replace contract here relies on platform-level `rename`
/// semantics:
///
/// - **Linux**: `rename(2)` follows symlinks at the source path and replaces
///   the destination inode atomically — no intermediate "absent" state.
///   `renameat2(RENAME_NOFOLLOW)` is not used; we deliberately accept the
///   source-symlink-follow semantics because the temp file is always a freshly
///   created regular file under our control.
/// - **macOS**: BSD `rename(2)` matches the Linux semantics relied on above.
/// - **Windows**: `std::fs::rename` shims to `MoveFileExW` with
///   `MOVEFILE_REPLACE_EXISTING`, which gives equivalent atomic-replace
///   behaviour modulo open-handle constraints on the destination.
///
/// See <https://doc.rust-lang.org/std/fs/fn.rename.html> for the cross-platform
/// shim contract. The throttle directory is OCX-private under `$OCX_HOME`, so
/// attacker symlink races on the destination are out of scope; the doc-comment
/// is here so future refactors don't accidentally depend on `renameat2`-only
/// guarantees that the cross-platform shim doesn't provide.
fn touch_atomic(path: &Path) -> std::io::Result<()> {
    use std::fs;

    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "state path has no parent"))?;

    fs::create_dir_all(parent)?;

    // Build a unique temp filename using PID + a counter derived from the
    // thread ID so concurrent tasks don't collide on the same suffix.
    let unique_suffix = {
        use std::sync::atomic::{AtomicU64, Ordering};
        // PID gives cross-process uniqueness; a process-global monotonic
        // counter gives within-process uniqueness. A wall-clock nanos suffix
        // (the previous approach) collides when concurrent tasks land in the
        // same clock bucket on coarse-resolution timers — observed flaky on
        // macOS. The counter guarantees every call gets a distinct temp name,
        // so concurrent writers never race on the same temp file.
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("{}.{}", std::process::id(), n)
    };
    let tmp_path = parent.join(format!(
        ".{}.tmp.{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("state"),
        unique_suffix
    ));
    fs::write(&tmp_path, b"")?;
    fs::rename(&tmp_path, path)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use std::time::Duration;

    use super::{StateStore, touch_atomic};
    use crate::oci;

    #[test]
    fn root_returns_store_root() {
        let store = StateStore::new("/state");
        assert_eq!(store.root(), Path::new("/state"));
    }

    #[test]
    fn update_check_dir_is_rooted_under_state() {
        let store = StateStore::new("/ocx/state");
        assert_eq!(store.update_check_dir(), PathBuf::from("/ocx/state/update-check"));
    }

    /// The state file for `ocx.sh/ocx/cli` must be rooted under `update-check/`
    /// with the slug `ocx_sh_ocx_cli` (strict no-dot encoding).
    #[test]
    fn update_check_file_produces_correct_path() {
        let store = StateStore::new("/ocx/state");
        let identifier = oci::Identifier::new_registry("ocx/cli", oci::OCX_SH_REGISTRY);
        let path = store.update_check_file(&identifier);
        assert_eq!(path, PathBuf::from("/ocx/state/update-check/ocx_sh_ocx_cli"));
    }

    /// The slug must contain no dots and no forward slashes.
    #[test]
    fn update_check_file_slug_is_dot_free() {
        let store = StateStore::new("/state");
        let identifier = oci::Identifier::new_registry("ocx/cli", oci::OCX_SH_REGISTRY);
        let path = store.update_check_file(&identifier);
        let file_name = path.file_name().unwrap().to_str().unwrap();
        assert!(!file_name.contains('.'), "slug must not contain dots; got: {file_name}");
        assert!(
            !file_name.contains('/'),
            "slug must not contain slashes; got: {file_name}"
        );
    }

    // ── referrers_capability_file / trust_root_file ──────────────────────────

    /// The referrers capability cache for `ghcr.io` lands under `referrers/`
    /// with the relaxed slug (dots preserved) plus a `.json` suffix.
    #[test]
    fn referrers_capability_file_produces_correct_path() {
        let store = StateStore::new("/ocx/state");
        assert_eq!(
            store.referrers_capability_file("ghcr.io"),
            PathBuf::from("/ocx/state/referrers/ghcr.io.json")
        );
    }

    /// The trust-root cache is keyed by Rekor authority under `trust_root/`;
    /// the `:` in `host:port` is neutralised to `_` by the relaxed slug.
    #[test]
    fn trust_root_file_produces_correct_path() {
        let store = StateStore::new("/ocx/state");
        assert_eq!(
            store.trust_root_file("rekor.example:443"),
            PathBuf::from("/ocx/state/trust_root/rekor.example_443.json")
        );
    }

    /// A hostile registry / authority string must never escape the store root:
    /// the relaxed slug replaces `/` and `..`-forming dots so every produced
    /// path stays a direct child of the subsystem directory. (Moved here from
    /// `capability.rs` when the layout moved onto these accessors.)
    #[test]
    fn cache_files_reject_hostile_slug() {
        let store = StateStore::new("/ocx/state");
        for hostile in ["../evil", "/etc/passwd", "..", "../../etc/shadow", "foo/../bar"] {
            for file in [store.referrers_capability_file(hostile), store.trust_root_file(hostile)] {
                let name = file.file_name().unwrap().to_str().unwrap();
                assert!(!name.contains('/'), "slug must not contain slashes; got: {name}");
                // The only path components below the store root are the subsystem
                // dir and the single slugged filename — no `..` traversal.
                let tail: Vec<_> = file.strip_prefix("/ocx/state").unwrap().components().collect();
                assert_eq!(tail.len(), 2, "expected {{subsystem}}/{{slug}}.json, got {file:?}");
            }
        }
    }

    // ── project-scoped state (C-022) ─────────────────────────────────────────

    /// C-022 — the sweep root `ocx clean` walks is `{root}/projects/`.
    #[test]
    fn project_state_root_is_projects_under_the_state_root() {
        let store = StateStore::new("/ocx/state");
        assert_eq!(store.project_state_root(), PathBuf::from("/ocx/state/projects"));
    }

    /// C-022 — a project's state directory is its key nested under the sweep root.
    #[test]
    fn project_state_dir_nests_the_key_under_the_sweep_root() {
        let store = StateStore::new("/ocx/state");
        assert_eq!(
            store.project_state_dir("0123456789abcdef"),
            PathBuf::from("/ocx/state/projects/0123456789abcdef")
        );
        assert_eq!(
            store.project_state_dir("0123456789abcdef").parent(),
            Some(store.project_state_root().as_path()),
            "every project state dir must be a direct child of the sweep root"
        );
    }

    /// C-022 — the consent stamp is `consent.json` inside the project's state dir.
    #[test]
    fn consent_stamp_file_is_consent_json_in_the_project_state_dir() {
        let store = StateStore::new("/ocx/state");
        assert_eq!(
            store.consent_stamp_file("0123456789abcdef"),
            PathBuf::from("/ocx/state/projects/0123456789abcdef/consent.json")
        );
    }

    /// C-022 / A-30 — the key is derived by canonicalizing the resolved project
    /// **file**, taking its parent, then `dunce`. A symlinked `ocx.toml` must
    /// therefore land on the real directory's stamp, not on the symlink's own
    /// parent.
    ///
    /// The second assertion is the discriminator: it shows the parent-first
    /// order (`.parent()` of the *un*-canonicalized path, which is what
    /// `resolve_explicit_project_path` returns) produces a different key — so a
    /// green here cannot be mistaken for a test that cannot tell the two orders
    /// apart. That is the `OCX_PROJECT=/w/fake/ocx.toml → /attacker/ocx.toml`
    /// case: parent-first keys the stamp to the victim's granted directory.
    #[cfg(unix)]
    #[test]
    fn consent_stamp_file_follows_a_symlinked_project_file_to_the_real_directory() {
        fn key_file_first(project_file: &Path) -> String {
            let canonical_file = std::fs::canonicalize(project_file).unwrap();
            let dir = dunce::canonicalize(canonical_file.parent().unwrap()).unwrap();
            crate::reference_manager::ReferenceManager::name_for_path(&dir)
        }
        fn key_parent_first(project_file: &Path) -> String {
            crate::reference_manager::ReferenceManager::name_for_path(project_file.parent().unwrap())
        }

        let tmp = tempfile::tempdir().unwrap();
        let real_dir = tmp.path().join("real");
        let fake_dir = tmp.path().join("fake");
        std::fs::create_dir_all(&real_dir).unwrap();
        std::fs::create_dir_all(&fake_dir).unwrap();
        let real_file = real_dir.join("ocx.toml");
        std::fs::write(&real_file, b"").unwrap();
        let fake_file = fake_dir.join("ocx.toml");
        std::os::unix::fs::symlink(&real_file, &fake_file).unwrap();

        let store = StateStore::new(tmp.path().join("state"));
        assert_eq!(
            store.consent_stamp_file(&key_file_first(&fake_file)),
            store.consent_stamp_file(&key_file_first(&real_file)),
            "a symlinked project file must key onto the real directory's stamp"
        );
        assert_ne!(
            key_parent_first(&fake_file),
            key_file_first(&real_file),
            "parent-first derivation must diverge, or this test cannot tell the two orders apart"
        );
    }

    // ── is_throttled decision table ──────────────────────────────────────────

    /// A path that does not exist is never throttled (no previous probe record).
    #[test]
    fn is_throttled_absent_path_returns_false() {
        let tmp = tempfile::tempdir().unwrap();
        let absent = tmp.path().join("does_not_exist");
        assert!(!StateStore::is_throttled(&absent, Duration::from_secs(86_400)));
    }

    /// A file whose mtime is *newer* than the interval → throttled (still within
    /// the window, so the next probe is not yet due).
    ///
    /// Uses a very large interval so the freshly-created file is always within it.
    #[test]
    fn is_throttled_file_within_interval_returns_true() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state_file");
        // Write the file now; mtime = approximately now.
        std::fs::write(&path, b"").unwrap();
        // 10 minutes in the future means "file was touched very recently"
        assert!(
            StateStore::is_throttled(&path, Duration::from_secs(600)),
            "file with mtime=now must be throttled within a 10-minute interval"
        );
    }

    /// A file whose mtime is *older* than the interval → not throttled (window
    /// has elapsed; probe is due).
    ///
    /// We use a zero-duration interval so any existing file is always past it.
    #[test]
    fn is_throttled_zero_interval_always_returns_false() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state_file");
        std::fs::write(&path, b"").unwrap();
        // Duration::ZERO = bypass semantics; always return false.
        assert!(
            !StateStore::is_throttled(&path, Duration::ZERO),
            "Duration::ZERO must always return false (bypass semantics)"
        );
    }

    /// A file whose mtime is older than a positive interval → not throttled.
    ///
    /// Uses a 1-nanosecond interval: no file can have been written within
    /// that window, so the file is always past the interval and the probe is due.
    #[test]
    fn is_throttled_file_older_than_positive_interval_returns_false() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state_file");
        std::fs::write(&path, b"").unwrap();
        // 1-nanosecond interval: the file cannot have been written that recently.
        assert!(
            !StateStore::is_throttled(&path, Duration::from_nanos(1)),
            "file older than 1ns interval must not be throttled"
        );
    }

    /// A file whose mtime is in the **future** (clock skew) must be treated as
    /// throttled (i.e. "file was just touched").
    ///
    /// The `is_throttled` implementation returns `true` when `mtime.elapsed()`
    /// errors — which happens when the mtime is ahead of the system clock.
    /// This test sets the file mtime to `now + 10 seconds` and asserts that
    /// `is_throttled` returns `true` for a 24-hour interval.
    ///
    /// Regression guard: without this branch, a host with an NTP drift that
    /// pushes the state-file mtime into the future would trigger a probe on
    /// every single command invocation (elapsed() → Err → return false).
    #[test]
    fn is_throttled_with_future_mtime_returns_true() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state_file");
        std::fs::write(&path, b"").unwrap();

        // Set mtime to 10 seconds in the future.
        let future_time = std::time::SystemTime::now() + std::time::Duration::from_secs(10);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(future_time)
            .unwrap();

        assert!(
            StateStore::is_throttled(&path, Duration::from_secs(86_400)),
            "future mtime must be treated as throttled (clock-skew safety)"
        );
    }

    // ── touch_atomic ─────────────────────────────────────────────────────────

    /// `touch_atomic` must create parent directories lazily if absent,
    /// write the file, and succeed on repeated calls.
    #[test]
    fn touch_atomic_creates_parent_and_writes() {
        let tmp = tempfile::tempdir().unwrap();
        // Parent dir does NOT exist yet.
        let path = tmp.path().join("state").join("update-check").join("ocx_sh_ocx_cli");

        // First call: parent created, file created.
        touch_atomic(&path).expect("first touch must succeed");
        assert!(path.exists(), "state file must exist after first touch");

        // Second call: must succeed without error (idempotent).
        touch_atomic(&path).expect("second touch must succeed (idempotent)");
        assert!(path.exists(), "state file must still exist after second touch");
    }

    /// Concurrent `touch_atomic` calls for the same path must all succeed
    /// (no panics, no I/O errors) and the file must exist at the end.
    #[tokio::test(flavor = "multi_thread")]
    async fn touch_atomic_concurrent_safety() {
        let tmp = tempfile::tempdir().unwrap();
        let path = std::sync::Arc::new(tmp.path().join("state").join("update-check").join("concurrent_slug"));

        // Pre-create parent so the race is on the file, not the directory.
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        let tasks: Vec<_> = (0..10)
            .map(|_| {
                let p = std::sync::Arc::clone(&path);
                tokio::spawn(async move {
                    // touch_atomic is sync; run in spawn_blocking to avoid blocking the
                    // async executor.
                    let p2 = p.as_ref().clone();
                    tokio::task::spawn_blocking(move || touch_atomic(&p2))
                        .await
                        .expect("spawn_blocking must not panic")
                })
            })
            .collect();

        for task in tasks {
            task.await
                .expect("task must not panic")
                .expect("touch_atomic must not return an error");
        }

        assert!(path.exists(), "state file must exist after concurrent writes");
    }

    // ── C-003: the render stamp ──────────────────────────────────────────────

    use std::collections::BTreeSet;

    use super::{BinEntryStamp, RENDER_STAMP_VERSION, RenderStamp, RenderStampScope, RenderStampTarget};

    /// The 16-hex project key shape `name_for_path` produces.
    const SAMPLE_PROJECT_KEY: &str = "0123456789abcdef";

    fn sample_bin_entry(size: u64, content_hash: &str) -> BinEntryStamp {
        BinEntryStamp {
            size,
            file_id: Some(424_242),
            content_hash: content_hash.to_string(),
        }
    }

    fn sample_render_stamp() -> RenderStamp {
        RenderStamp {
            v: 1,
            home: PathBuf::from("/w/proj/.ocx/toolchain"),
            scope: RenderStampScope::Project(PathBuf::from("/w/proj")),
            names: ["cmake".to_string(), "ninja".to_string()].into_iter().collect(),
            bin_fingerprint: [
                ("cmake".to_string(), sample_bin_entry(120, "1".repeat(64).as_str())),
                ("ninja".to_string(), sample_bin_entry(131, "2".repeat(64).as_str())),
            ]
            .into_iter()
            .collect(),
            link_fingerprint: [(
                "default/cmake".to_string(),
                "/ocx/packages/sha256/aa/bbccdd".to_string(),
            )]
            .into_iter()
            .collect(),
        }
    }

    fn sample_global_render_stamp() -> RenderStamp {
        RenderStamp {
            v: 1,
            home: PathBuf::from("/ocx/toolchain"),
            scope: RenderStampScope::Global,
            names: ["cmake".to_string()].into_iter().collect(),
            bin_fingerprint: [("cmake".to_string(), sample_bin_entry(120, "3".repeat(64).as_str()))]
                .into_iter()
                .collect(),
            link_fingerprint: [(
                "default/cmake".to_string(),
                "/ocx/packages/sha256/cc/ddeeff".to_string(),
            )]
            .into_iter()
            .collect(),
        }
    }

    /// C-003 — a stamp survives a JSON round trip unchanged, per-entry values
    /// included. The per-entry `size` assertion is separate from the whole-value
    /// equality on purpose: it is the field C-061's cheap stat gate compares,
    /// and it must be the *written* one, not a zero.
    #[test]
    fn a_render_stamp_round_trips_through_json() {
        let stamp = sample_render_stamp();
        let json = serde_json::to_string(&stamp).expect("the stamp must serialize");
        let parsed: RenderStamp = serde_json::from_str(&json).expect("the stamp must deserialize");

        assert_eq!(parsed, stamp, "C-003 — a stamp must survive the round trip unchanged");
        assert_eq!(
            parsed.names,
            parsed.bin_fingerprint.keys().cloned().collect::<BTreeSet<String>>(),
            "C-003 — `names` is the on-disk entry set, so it must equal `bin_fingerprint`'s key set; a stamp \
             where they disagree lets wave 3's gate compare one set while the heal repairs the other"
        );
        assert_eq!(
            parsed.bin_fingerprint["cmake"].size, 120,
            "C-061 — the per-entry size the stat gate compares must survive the write"
        );
        assert_eq!(
            parsed.bin_fingerprint["cmake"].file_id,
            Some(424_242),
            "C-003 — the file identity the Windows hardlink check needs must survive too"
        );
    }

    /// C-003 — `deny_unknown_fields`: a stamp carrying an extra key is a
    /// deserialize error, at both levels of the tree.
    #[test]
    fn a_render_stamp_carrying_an_unknown_key_fails_to_deserialize() {
        let mut value = serde_json::to_value(sample_render_stamp()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("unexpected".to_string(), serde_json::json!("x"));
        assert!(
            serde_json::from_value::<RenderStamp>(value).is_err(),
            "C-003 — an unknown key at the stamp's own level must be refused"
        );

        let mut entry = serde_json::to_value(sample_bin_entry(120, "aa")).unwrap();
        entry
            .as_object_mut()
            .unwrap()
            .insert("unexpected".to_string(), serde_json::json!("x"));
        assert!(
            serde_json::from_value::<BinEntryStamp>(entry).is_err(),
            "C-003 — an unknown key inside a per-entry stamp must be refused too"
        );
    }

    /// C-003, D-V13 — every field is required, so dropping any one of them is a
    /// deserialize *error* rather than a silent default.
    ///
    /// This is B-C's regression test. `scope` is the field that motivates it:
    /// as an `Option<PathBuf>` its dropped key would deserialize cleanly and
    /// present as the global tier, skipping the project-identity check D-V13
    /// added it for. `deny_unknown_fields` governs *extra* keys only and would
    /// not have caught that.
    #[test]
    fn dropping_any_render_stamp_field_fails_to_deserialize() {
        let value = serde_json::to_value(sample_render_stamp()).unwrap();
        let keys: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
        assert!(
            keys.contains(&"scope".to_string()),
            "the loop must actually reach the tier tag, or this test proves nothing about D-V13"
        );

        for key in &keys {
            let mut reduced = value.clone();
            reduced.as_object_mut().unwrap().remove(key);
            assert!(
                serde_json::from_value::<RenderStamp>(reduced).is_err(),
                "C-003 — dropping {key:?} must fail to deserialize, never resolve to a default"
            );
        }
    }

    /// C-003 — a per-entry stamp requires `size` and `content_hash`; `file_id`
    /// is the tree's one `Option` and its absence is the documented
    /// fall-through-to-the-content-hash case, not a defect.
    #[test]
    fn a_bin_entry_stamp_requires_every_field_except_the_file_id() {
        let value = serde_json::to_value(sample_bin_entry(120, "aa")).unwrap();

        for key in ["size", "content_hash"] {
            let mut reduced = value.clone();
            reduced.as_object_mut().unwrap().remove(key);
            assert!(
                serde_json::from_value::<BinEntryStamp>(reduced).is_err(),
                "C-003 — dropping {key:?} must fail to deserialize"
            );
        }

        let mut without_file_id = value.clone();
        without_file_id.as_object_mut().unwrap().remove("file_id");
        let parsed: BinEntryStamp =
            serde_json::from_value(without_file_id).expect("an absent file_id is the documented fall-through");
        assert_eq!(
            parsed.file_id, None,
            "C-003 — an absent file id must read as `None`, which falls the gate through to the content hash"
        );
    }

    /// C-003 — **no field covers mtime**, at either level.
    ///
    /// Rust cannot assert a field's absence, so the invariant is asserted over
    /// the serialized key set instead: an exact set equality reds both when a
    /// documented key disappears and when an undocumented one — `mtime` being
    /// the one the contract names — appears. Without this form the "neither
    /// fingerprint covers mtime" contract has no reachable red at all.
    ///
    /// mtime is trivially forgeable and is preserved by an in-place overwrite,
    /// so it would report "unchanged" for exactly the edit the stamp exists to
    /// notice.
    #[test]
    fn the_render_stamp_json_carries_exactly_the_documented_keys_and_no_mtime() {
        let value = serde_json::to_value(sample_render_stamp()).unwrap();

        let top_level: BTreeSet<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
        let expected_top: BTreeSet<&str> = ["v", "home", "scope", "names", "bin_fingerprint", "link_fingerprint"]
            .into_iter()
            .collect();
        assert_eq!(
            top_level, expected_top,
            "C-003 — the stamp's key set is exactly these six; an `mtime` key must never appear"
        );

        let entry = value
            .get("bin_fingerprint")
            .and_then(|map| map.get("cmake"))
            .expect("the sample must carry a per-entry stamp to inspect");
        let entry_level: BTreeSet<&str> = entry.as_object().unwrap().keys().map(String::as_str).collect();
        let expected_entry: BTreeSet<&str> = ["size", "file_id", "content_hash"].into_iter().collect();
        assert_eq!(
            entry_level, expected_entry,
            "C-003 — a per-entry stamp's key set is exactly these three; an `mtime` key must never appear"
        );
    }

    /// C-003, C-061 — per-entry hashes stay **individually** recoverable by
    /// name across the round trip. A single folded digest gives the prompt gate
    /// nothing to compare per entry and degenerates "hash only what differs"
    /// into the unconditional re-hash the gate exists to avoid.
    #[test]
    fn per_entry_hashes_stay_individually_recoverable_across_the_round_trip() {
        let stamp = sample_render_stamp();
        let json = serde_json::to_string(&stamp).expect("the stamp must serialize");
        let parsed: RenderStamp = serde_json::from_str(&json).expect("the stamp must deserialize");

        let cmake = parsed
            .bin_fingerprint
            .get("cmake")
            .expect("cmake's entry must be recoverable by name");
        let ninja = parsed
            .bin_fingerprint
            .get("ninja")
            .expect("ninja's entry must be recoverable by name");

        assert_ne!(
            cmake.content_hash, ninja.content_hash,
            "two distinct entries must not collapse into one folded digest"
        );
        assert_eq!(cmake.content_hash, stamp.bin_fingerprint["cmake"].content_hash);
        assert_eq!(ninja.content_hash, stamp.bin_fingerprint["ninja"].content_hash);
    }

    /// C-003 — a project's render stamp lives beside its consent stamp, under
    /// the same key, with the same "deletable at any time" lifetime.
    #[test]
    fn a_projects_render_stamp_sits_beside_its_consent_stamp() {
        let store = StateStore::new("/ocx/state");
        assert_eq!(
            store.render_stamp_file(SAMPLE_PROJECT_KEY),
            PathBuf::from("/ocx/state/projects/0123456789abcdef/render_stamp.json")
        );
        assert_eq!(
            store.render_stamp_file(SAMPLE_PROJECT_KEY).parent(),
            store.consent_stamp_file(SAMPLE_PROJECT_KEY).parent(),
            "C-003 — the render stamp is a sibling of the consent stamp, no new store"
        );
    }

    /// C-003, D-V13 — the **global** stamp lives directly under `state/`, never
    /// under `projects/`.
    ///
    /// Two shipped mechanisms make the `projects/` path unusable for it, both
    /// silently: `consent::record_in` refuses to stamp `$OCX_HOME` at all, so
    /// that directory is never created; and `ocx clean` classifies a
    /// `state/projects/<key>/` recording `$OCX_HOME` as `SweepReason::OcxHome`
    /// and removes the whole directory, so the stamp would be deleted on every
    /// clean and the global gate would re-render forever.
    #[test]
    fn the_global_render_stamp_is_not_under_the_projects_sweep_root() {
        let store = StateStore::new("/ocx/state");
        assert_eq!(
            store.global_render_stamp_file(),
            PathBuf::from("/ocx/state/render_stamp.json")
        );
        assert!(
            !store.global_render_stamp_file().starts_with(store.project_state_root()),
            "C-003 / D-V13 — the global stamp must not sit under the sweep root `ocx clean` walks"
        );
    }

    /// C-003 — a written stamp reads back for the tier it was written for, at
    /// the documented path.
    #[test]
    fn a_written_render_stamp_reads_back_for_the_tier_it_was_written_for() {
        let tmp = tempfile::tempdir().unwrap();
        let store = StateStore::new(tmp.path().join("state"));
        let stamp = sample_render_stamp();

        store
            .set_render_stamp(RenderStampTarget::Project(SAMPLE_PROJECT_KEY), &stamp)
            .expect("writing a project stamp must succeed");

        assert_eq!(
            store.render_stamp(RenderStampTarget::Project(SAMPLE_PROJECT_KEY)),
            Some(stamp),
            "C-003 — the stamp must read back exactly as written"
        );
        assert!(
            store.render_stamp_file(SAMPLE_PROJECT_KEY).is_file(),
            "C-003 — the bytes must land at the documented project path"
        );
    }

    /// C-003, D-V13 — the two tiers do not read each other's stamps: a written
    /// global stamp is invisible to a project lookup and lands outside
    /// `projects/`.
    #[test]
    fn the_two_tiers_render_stamps_do_not_read_each_other() {
        let tmp = tempfile::tempdir().unwrap();
        let store = StateStore::new(tmp.path().join("state"));

        store
            .set_render_stamp(RenderStampTarget::Global, &sample_global_render_stamp())
            .expect("writing the global stamp must succeed");

        assert_eq!(
            store.render_stamp(RenderStampTarget::Project(SAMPLE_PROJECT_KEY)),
            None,
            "C-003 — a project lookup must not find the global stamp"
        );
        assert!(
            store.global_render_stamp_file().is_file(),
            "C-003 — the global stamp must land directly under `state/`"
        );
        assert!(
            !store.render_stamp_file(SAMPLE_PROJECT_KEY).exists(),
            "C-003 — writing the global stamp must not create a project stamp"
        );
    }

    /// C-003 — an unusable stamp is an absent stamp: a corrupt file reads as
    /// `None` so the caller's answer is "re-render", which is always safe.
    ///
    /// The write-valid-first half is the positive control, the pattern the
    /// version sibling below already uses: a bare `None` assertion is
    /// indistinguishable from a test that wrote to the wrong path, or from a
    /// reader that returns `None` unconditionally. Proving the same path reads
    /// `Some` first is what makes the `None` the corruption answering.
    #[test]
    fn an_unusable_render_stamp_reads_as_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let store = StateStore::new(tmp.path().join("state"));
        let path = store.render_stamp_file(SAMPLE_PROJECT_KEY);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        std::fs::write(&path, serde_json::to_vec(&sample_render_stamp()).unwrap()).unwrap();
        assert!(
            store
                .render_stamp(RenderStampTarget::Project(SAMPLE_PROJECT_KEY))
                .is_some(),
            "the valid stamp at this exact path must read back, or the `None` below proves nothing"
        );

        std::fs::write(&path, b"{ not json").unwrap();
        assert_eq!(
            store.render_stamp(RenderStampTarget::Project(SAMPLE_PROJECT_KEY)),
            None,
            "C-003 — a corrupt stamp must read as absent, never propagate an error"
        );
    }

    /// C-003 — [`RenderStamp::new`] stamps the version the reader accepts.
    ///
    /// The wire version has one source of truth and it is not a producer's
    /// literal: `RENDER_STAMP_VERSION` is module-private, so a producer in
    /// another module would otherwise hardcode `1`. The assertion is a
    /// *round trip through the reader* rather than `v == 1`, because that is
    /// the property that matters — a constructor stamping a version the reader
    /// rejects would make every render re-render forever, silently.
    #[test]
    fn a_constructed_render_stamp_carries_the_version_the_reader_accepts() {
        let tmp = tempfile::tempdir().unwrap();
        let store = StateStore::new(tmp.path().join("state"));
        let sample = sample_render_stamp();

        let constructed = RenderStamp::new(
            sample.home.clone(),
            sample.scope.clone(),
            sample.bin_fingerprint.clone(),
            sample.link_fingerprint.clone(),
        );
        assert_eq!(
            constructed, sample,
            "C-003 — the constructor must produce exactly the stamp a producer would otherwise write by hand"
        );
        assert_eq!(
            constructed.names,
            constructed
                .bin_fingerprint
                .keys()
                .cloned()
                .collect::<BTreeSet<String>>(),
            "C-003 — `new` derives `names` from the map rather than accepting it, so the pair cannot \
             disagree at the producer; the round-trip test checks the same invariant on the wire"
        );

        store
            .set_render_stamp(RenderStampTarget::Project(SAMPLE_PROJECT_KEY), &constructed)
            .expect("writing a constructed stamp must succeed");
        assert_eq!(
            store.render_stamp(RenderStampTarget::Project(SAMPLE_PROJECT_KEY)),
            Some(constructed),
            "C-003 — a constructed stamp must read back, so its `v` is the one this binary recognises"
        );
    }

    /// C-003, C-061 — [`BinEntryStamp::from_metadata`] is the one derivation
    /// of the stat pair, so WP-7's producer and WP-9's comparator cannot
    /// disagree about what a "file id" is.
    ///
    /// The `size` assertion is against the bytes actually written, not a
    /// literal, and the Unix half pins `file_id` to the inode — the value the
    /// comparator will stat for itself. The `cfg`-free half below pins what
    /// "file identity" has to *mean* on every supported platform (present, one
    /// value per file, shared by a hardlink), which is the only assertion the
    /// Windows `GetFileInformationByHandle` arm has covering it.
    #[test]
    fn a_bin_entry_stamp_from_metadata_carries_the_platforms_own_file_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let entry = tmp.path().join("cmake");
        let bytes = b"#!/bin/sh\nexec cmake \"$@\"\n";
        std::fs::write(&entry, bytes).unwrap();

        let metadata = std::fs::metadata(&entry).unwrap();
        let stamp = BinEntryStamp::from_metadata(&entry, &metadata, "1".repeat(64));

        assert_eq!(
            stamp.size,
            bytes.len() as u64,
            "C-061 — the stat gate's size must be the entry's real length"
        );
        assert_eq!(
            stamp.content_hash,
            "1".repeat(64),
            "the caller's hash is carried verbatim"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            assert_eq!(
                stamp.file_id,
                Some(metadata.ino()),
                "C-061 — on Unix the file identity is the inode, which is what a comparator stats for itself"
            );
        }

        // The property both consumers actually rest on, asserted without a
        // `cfg`: `file_id` is *the same file*, not merely *some number*. The
        // Windows arm reads it through `GetFileInformationByHandle`, so this is
        // the one place a handle read that answered `None`, a constant, or the
        // wrong field would go red — on the only platform that compiles it.
        assert!(
            stamp.file_id.is_some(),
            "a supported platform must report a file identity, or the stat gate degenerates into an \
             unconditional re-hash"
        );

        let link = tmp.path().join("cmake-hardlink");
        crate::hardlink::create(&entry, &link).expect("a hardlink inside one tempdir is creatable");
        let linked = BinEntryStamp::from_metadata(&link, &std::fs::metadata(&link).unwrap(), "1".repeat(64));
        assert_eq!(
            linked.file_id, stamp.file_id,
            "C-003 — a hardlink IS the same file, and proving a trampoline `.exe` against the ShimBinStore \
             blob is exactly that comparison"
        );

        let other = tmp.path().join("ninja");
        std::fs::write(&other, bytes).unwrap();
        let other_stamp = BinEntryStamp::from_metadata(&other, &std::fs::metadata(&other).unwrap(), "1".repeat(64));
        assert_ne!(
            other_stamp.file_id, stamp.file_id,
            "a different file with identical bytes must not share the identity, or the gate would bless a \
             substituted entry"
        );
    }

    /// C-003 — a stamp at a `v` this binary does not recognise reads as
    /// **absent**, not as an error.
    ///
    /// The contract settled after the rest of this module was written, so the
    /// accepted set now exists and the property is falsifiable. The first half
    /// is the positive control: the very same bytes at the recognised version
    /// DO read back, so the `None` below is the version check answering rather
    /// than a fixture that never parsed. Without it, deleting the version check
    /// entirely would still leave this test green.
    #[test]
    fn a_render_stamp_at_an_unknown_version_reads_as_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let store = StateStore::new(tmp.path().join("state"));
        let path = store.render_stamp_file(SAMPLE_PROJECT_KEY);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        let mut value = serde_json::to_value(sample_render_stamp()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("v".to_string(), serde_json::json!(RENDER_STAMP_VERSION));
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(
            store
                .render_stamp(RenderStampTarget::Project(SAMPLE_PROJECT_KEY))
                .is_some(),
            "the recognised version must read back, or the refusal below proves nothing"
        );

        value
            .as_object_mut()
            .unwrap()
            .insert("v".to_string(), serde_json::json!(RENDER_STAMP_VERSION + 1));
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(
            store.render_stamp(RenderStampTarget::Project(SAMPLE_PROJECT_KEY)),
            None,
            "C-003 — an unrecognised `v` must read as absent, never as an error"
        );
    }
}
