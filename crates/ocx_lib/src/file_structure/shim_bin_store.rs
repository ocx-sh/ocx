// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Content-addressed store for the embedded `ocx-shim` executable blob
//! (ADR Contract 3 / [#301](https://github.com/ocx-sh/ocx/issues/301)).
//!
//! Every generated Windows entrypoint launcher hardlinks its `<name>.exe`
//! from the single blob this store publishes, instead of each entry writing
//! its own byte-for-byte copy — one inode, one Authenticode signature, one
//! Defender scan regardless of how many tool names a package declares.
//!
//! Layout: `{root}/<sha256-hex>.exe` — **flat**, never sharded via
//! [`super::cas_shard_path`]. At most two blobs (one per Windows arch, see
//! [`crate::shim`]) ever live here, so sharding a two-entry directory is
//! pure overhead. `<sha256-hex>` is the bare lowercase hex of the blob's
//! SHA-256 ([`crate::oci::Digest::hex`]), no `sha256:` prefix; the `.exe`
//! suffix is unconditional on every host platform building `ocx` — the blob
//! it names is always a Windows PE, regardless of whether it is generated
//! from a Linux, macOS, or Windows host.
//!
//! `FileStructure`'s `shim_bin` field roots this store at
//! `$OCX_HOME/.bin/ocx-shim/`, outside the three GC tiers (`blobs/`,
//! `layers/`, `packages/`) — never walked or collected by `ocx clean` (plan
//! decision D4). A shim binary changes only on an `ocx` version bump, at
//! which point its digest changes too and a new blob is written under a new
//! name; the superseded blob is orphaned litter accepted by design, the same
//! way `locks/` litter is accepted (`utility/fs/scoped_lock.rs`).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::{Result, oci};

/// Test-only seam (`arch-principles.md` "Test-only seams") forcing
/// [`ShimBinStore::ensure`] down the leg a caller takes when it loses the
/// publish race. Its value is the store root it applies to, so an armed seam
/// reaches exactly one store even though the variable is process-global.
#[cfg(any(test, feature = "__testing"))]
const LOST_PUBLISH_RACE_SEAM: &str = "__OCX_TESTING_SHIM_LOST_PUBLISH_RACE";

/// SHA-256 of the embedded shim blob ([`crate::shim::SHIM_BYTES`]), hashed
/// once per process.
///
/// The bytes are hashed rather than [`crate::shim::SHIM_SHA256`] being read:
/// that constant is `""` off Windows, where no blob is embedded, and an empty
/// string is not a digest — reading it would leave `ensure()` unable to name a
/// file at all on Linux and macOS. Hashing is also simply what content
/// addressing means here. `SHIM_SHA256` keeps its existing role as the
/// corruption canary (`shim.rs`), and the two are bound by the assertion
/// below wherever that constant carries a value, so the derivation is checked
/// against it instead of quietly diverging from it. The assertion is a
/// `debug_assert!` because a library must not panic a release binary over a
/// build-time invariant the `shim.rs` canary test already gates.
fn shim_digest() -> &'static oci::Digest {
    static SHIM_DIGEST: OnceLock<oci::Digest> = OnceLock::new();
    SHIM_DIGEST.get_or_init(|| {
        let digest = oci::Algorithm::Sha256.hash(crate::shim::SHIM_BYTES);
        debug_assert!(
            crate::shim::SHIM_SHA256.is_empty() || digest.hex() == crate::shim::SHIM_SHA256,
            "sha256(SHIM_BYTES) must equal the recorded SHIM_SHA256 corruption canary"
        );
        digest
    })
}

/// Whether this `ensure()` call must behave as one that lost the publish race:
/// its pre-check found nothing and its rename then failed. Always `false` in a
/// release build — the env read is not compiled in there at all.
///
/// No black-box input produces that state, because the pre-check and the
/// re-check test the same condition (target present): whatever makes the
/// re-check succeed also makes the pre-check short-circuit, and only a real
/// interleaving separates them.
#[cfg(any(test, feature = "__testing"))]
fn simulated_lost_race(root: &Path) -> bool {
    std::env::var_os(LOST_PUBLISH_RACE_SEAM).is_some_and(|armed| Path::new(&armed) == root)
}

#[cfg(not(any(test, feature = "__testing")))]
fn simulated_lost_race(_root: &Path) -> bool {
    false
}

/// What [`ShimBinStore::ensure`]'s pre-check found, and therefore what its
/// publish is allowed to do to a file already sitting at the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Publish {
    /// The pre-check found nothing readable. Any file at the target by rename
    /// time is a concurrent winner carrying byte-identical content, and
    /// replacing it would swap in a fresh file record — orphaning the one every
    /// `<name>.exe` that winner already hardlinked points at, which is exactly
    /// the shared inode #301 exists to guarantee. Publish only if still absent.
    OnlyIfAbsent,
    /// The pre-check found a blob whose length is not the embedded one. This
    /// call has positive evidence the file there is wrong, so it replaces it —
    /// the one case where losing the winner's record is the point.
    OverTornBlob,
}

/// Atomically publishes the staged blob, or — when the seam above is armed —
/// discards it and reports the failure a lost race produces. Twinned rather
/// than branched inline so a release build carries no simulated-failure path
/// at all.
#[cfg(any(test, feature = "__testing"))]
fn publish_staged(
    temp: tempfile::NamedTempFile,
    target: &Path,
    lost_race: bool,
    publish: Publish,
) -> std::io::Result<()> {
    if lost_race {
        // Drop the staged temp first: a genuine failed persist consumes and
        // drops it too, so the seam leaves the same filesystem state behind.
        drop(temp);
        return Err(std::io::Error::other(format!(
            "{LOST_PUBLISH_RACE_SEAM}: simulated publish failure"
        )));
    }
    publish_with(temp, target, publish)
}

#[cfg(not(any(test, feature = "__testing")))]
fn publish_staged(
    temp: tempfile::NamedTempFile,
    target: &Path,
    _lost_race: bool,
    publish: Publish,
) -> std::io::Result<()> {
    publish_with(temp, target, publish)
}

fn publish_with(temp: tempfile::NamedTempFile, target: &Path, publish: Publish) -> std::io::Result<()> {
    match publish {
        Publish::OnlyIfAbsent => crate::utility::fs::persist_temp_file_if_absent(temp, target),
        Publish::OverTornBlob => crate::utility::fs::persist_temp_file(temp, target),
    }
}

/// Content-addressed store for the embedded `ocx-shim` executable blob.
///
/// See the module docs for the layout and GC-exemption rationale.
#[derive(Debug, Clone)]
pub struct ShimBinStore {
    root: PathBuf,
}

impl ShimBinStore {
    /// Creates a `ShimBinStore` rooted at `root` (conventionally
    /// `$OCX_HOME/.bin/ocx-shim`, wired by [`super::FileStructure::with_root`]).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The root directory of the store.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the path for the blob identified by `digest`.
    ///
    /// Flat layout — `{root}/<sha256-hex>.exe`, never sharded via
    /// [`super::cas_shard_path`] (see module docs for why). `digest` is
    /// expected to be a SHA-256 digest; its bare lowercase hex
    /// ([`oci::Digest::hex`]) forms the filename, with no `sha256:` prefix.
    pub fn path(&self, digest: &oci::Digest) -> PathBuf {
        self.root.join(format!("{}.exe", digest.hex()))
    }

    /// Publishes the embedded shim blob for the running target's
    /// architecture ([`crate::shim::SHIM_BYTES`]) and returns its path,
    /// writing it only when absent.
    ///
    /// Idempotent and safe under concurrent callers: the blob is
    /// content-addressed by its own SHA-256 (see `shim_digest`), so every
    /// writer publishes the same bytes to the same path — a losing concurrent
    /// writer converges on the winner's file rather than corrupting it.
    ///
    /// Convergence is the caller's job, not the publish primitive's: the dance
    /// `finalize_layer_dir` (`package_manager/tasks/layer_staging.rs`) runs for
    /// layer directories applies verbatim — pre-check the target, publish, and
    /// on a publish failure re-check: present ⇒ the race was lost and the
    /// winner's file is byte-identical, absent ⇒ propagate. It takes no lock;
    /// two writers of the same bytes need no mutual exclusion.
    ///
    /// A pre-check that found **nothing** publishes through
    /// [`crate::utility::fs::persist_temp_file_if_absent`], not the replacing
    /// form. Identical bytes are not enough here: `persist`'s rename swaps in a
    /// fresh file record, so a loser replacing the winner's blob orphans the
    /// record every `<name>.exe` hardlinked from it, and mutating the store's
    /// blob afterwards (an `ocx` upgrade, a re-sign) would no longer reach
    /// them — one inode per name instead of one inode per store, which is the
    /// #301 property inverted. `generate()` publishes one launcher per declared
    /// name concurrently, so this is the ordinary path, not an edge case.
    ///
    /// # Errors
    ///
    /// Returns an error if creating the store's root directory, staging the
    /// blob, or atomically publishing it fails with the target still absent.
    pub async fn ensure(&self) -> Result<PathBuf> {
        let target = self.path(shim_digest());
        let lost_race = simulated_lost_race(&self.root);
        let mut publish = Publish::OnlyIfAbsent;

        // Pre-check: the blob is content-addressed, so a file already at this
        // path is the file this call would write — provided it is whole.
        // Publishing "only when absent" is what keeps N launcher generations
        // down to one write; C-001 narrows "absent" to "absent or torn",
        // because existence alone cannot tell a healthy blob from one a crashed
        // earlier run left created-but-unwritten, and on Windows that blob is
        // what every hardlinked `<name>.exe` actually executes. The length
        // comparison lives in `crate::shim::published_blob_is_intact`, whose
        // empty-`embedded` clause keeps this existence-only on hosts that embed
        // no blob.
        if !lost_race {
            match tokio::fs::metadata(&target).await {
                Ok(metadata) if crate::shim::published_blob_is_intact(metadata.len(), crate::shim::SHIM_BYTES) => {
                    return Ok(target);
                }
                Ok(metadata) => {
                    crate::log::debug!(
                        "Published shim blob {} is {} bytes, expected {}; republishing over the torn write.",
                        target.display(),
                        metadata.len(),
                        crate::shim::SHIM_BYTES.len()
                    );
                    publish = Publish::OverTornBlob;
                }
                // Same lossy contract the previous existence probe had: an
                // unreadable target is treated as absent and the publish below
                // decides. A genuine I/O fault resurfaces there with context.
                Err(error) => crate::log::debug!(
                    "Cannot stat published shim blob {} ({error}); treating it as absent.",
                    target.display()
                ),
            }
        }

        tokio::fs::create_dir_all(&self.root)
            .await
            .map_err(|e| crate::error::file_error(&self.root, e))?;

        let root = self.root.clone();
        let published = target.clone();
        // `persist_temp_file` is blocking (`NamedTempFile` is synchronous), so
        // the whole stage-and-publish runs on a blocking thread.
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            let mut temp = tempfile::NamedTempFile::new_in(&root)?;
            std::io::Write::write_all(&mut temp, crate::shim::SHIM_BYTES)?;
            temp.as_file().sync_data()?;

            match publish_staged(temp, &published, lost_race, publish) {
                Ok(()) => Ok(()),
                // A concurrent `ensure()` published between this call's
                // pre-check and its rename. Its file carries the same
                // `SHIM_BYTES` this call staged, so the winner's blob is this
                // call's answer too; the staged temp is already gone with the
                // failed publish, leaving no litter behind.
                Err(error) if published.exists() => {
                    crate::log::debug!(
                        "Shim blob {} was published concurrently ({error}); keeping the winner's file.",
                        published.display()
                    );
                    Ok(())
                }
                Err(error) => Err(error),
            }
        })
        .await
        .map_err(|join_error| crate::error::file_error(&target, std::io::Error::other(join_error)))?
        .map_err(|io_error| crate::error::file_error(&target, io_error))?;

        Ok(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::oci;

    /// An arbitrary valid SHA-256 hex. Deliberately **not**
    /// [`crate::shim::SHIM_SHA256`] — the layout golden must not churn when the
    /// committed blob is refreshed.
    const SHA256_HEX: &str = "43567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d9";

    /// Content the real blob can never carry, used to observe whether a second
    /// `ensure()` re-published over an already-present file.
    const SENTINEL: &[u8] = b"not-the-shim-blob";

    /// [`SENTINEL`] padded to the embedded blob's length.
    ///
    /// C-001 narrowed `ensure()`'s pre-check from "present" to "present and
    /// intact", and intactness is a length comparison against
    /// [`crate::shim::SHIM_BYTES`]. A bare 17-byte `SENTINEL` therefore reads
    /// as a torn write on a build that embeds a blob (Windows), where
    /// `ensure()` would then republish over it — correctly, but for a
    /// different question than the one the test below asks. Padding restores
    /// the healthy length while the *content* still differs from the embedded
    /// blob, which is what keeps "`ensure()` did not re-publish" observable.
    /// Off Windows nothing is embedded and this is `SENTINEL` unchanged.
    fn intact_length_sentinel() -> Vec<u8> {
        let mut bytes = SENTINEL.to_vec();
        bytes.resize(SENTINEL.len().max(crate::shim::SHIM_BYTES.len()), b'.');
        bytes
    }

    fn digest() -> oci::Digest {
        oci::Digest::Sha256(SHA256_HEX.to_string())
    }

    /// Every entry directly under `root`, sorted. The store is flat, so this is
    /// the entire store.
    fn entries(root: &Path) -> Vec<PathBuf> {
        let mut found: Vec<PathBuf> = std::fs::read_dir(root)
            .expect("store root must exist")
            .map(|entry| entry.expect("readable directory entry").path())
            .collect();
        found.sort();
        found
    }

    // ── C-001: layout golden ──────────────────────────────────────────────
    //
    // `path()` is the store's wire-ish contract: the launcher generator, the
    // GC exemption and any future `.bin` consumer all address the blob by this
    // name. Asserted as a full path built from the digest's bare hex, so a
    // switch to CAS sharding or to a prefixed / suffixed name fails here.

    #[test]
    fn path_is_bare_hex_dot_exe_directly_under_the_root() {
        let store = ShimBinStore::new("/ocx/.bin/ocx-shim");
        let path = store.path(&digest());
        let expected_name = format!("{SHA256_HEX}.exe");

        assert_eq!(
            path,
            Path::new("/ocx/.bin/ocx-shim").join(&expected_name),
            "C-001: `path(digest)` must be `<root>/<sha256-hex>.exe`"
        );
        assert_eq!(
            path.parent(),
            Some(store.root()),
            "the blob sits DIRECTLY under the root — never CAS-sharded into \
             `<algo>/<2hex>/<30hex>/`"
        );
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some(expected_name.as_str()),
            "the file name is the bare lowercase hex plus `.exe`"
        );
        assert!(
            !path.to_string_lossy().contains("sha256:"),
            "the name carries no `sha256:` algorithm prefix"
        );
    }

    #[test]
    fn file_structure_roots_the_store_at_dot_bin_ocx_shim() {
        let home = Path::new("/ocx-home");
        let file_structure = super::super::FileStructure::with_root(home.to_path_buf());

        assert_eq!(
            file_structure.shim_bin.path(&digest()),
            home.join(".bin").join("ocx-shim").join(format!("{SHA256_HEX}.exe")),
            "C-001: the published path is `$OCX_HOME/.bin/ocx-shim/<sha256>.exe`"
        );
    }

    // ── C-001: `ensure()` ─────────────────────────────────────────────────
    //
    // These run on EVERY host, deliberately. `crate::shim::SHIM_BYTES` is empty
    // off Windows, so the content assertions are weak there — but the publish
    // mechanics they pin (root creation, single-blob publication, the pre-check
    // that skips a re-write, race convergence) are platform-independent, and
    // Windows is not a host this suite ever runs on. Off-Windows `ensure()`
    // publishing a zero-byte blob is the behaviour these encode; if it is made
    // to refuse there instead, C-001 keeps NO host-runnable coverage at all —
    // that is a design decision to take deliberately, not by cfg-gating this
    // module away.
    //
    // They do not assert the exact file name `ensure()` chooses: off Windows
    // `SHIM_SHA256` is `""` while `sha256(SHIM_BYTES)` is the empty-input
    // digest, so the two plausible implementations disagree on a host that
    // embeds no blob. Only the invariants both satisfy are pinned here.

    #[tokio::test]
    async fn ensure_creates_the_root_and_publishes_one_blob() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("absent-parent").join("ocx-shim");
        let store = ShimBinStore::new(root.clone());
        assert!(!root.exists(), "precondition: the store root must not exist yet");

        let published = store.ensure().await.unwrap();

        assert!(
            published.is_file(),
            "`ensure()` must leave a file at the path it returns"
        );
        assert_eq!(
            published.parent(),
            Some(root.as_path()),
            "the published blob is flat under the store root"
        );
        assert_eq!(
            published.extension().and_then(|ext| ext.to_str()),
            Some("exe"),
            "the published blob is named `<sha256-hex>.exe` on every host"
        );
        assert_eq!(
            entries(&root),
            vec![published.clone()],
            "exactly one blob under the root — no temp litter left behind"
        );
    }

    #[tokio::test]
    async fn ensure_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("ocx-shim");
        let store = ShimBinStore::new(root.clone());

        let first = store.ensure().await.unwrap();
        let second = store.ensure().await.unwrap();

        assert_eq!(first, second, "both calls must return the same path");
        assert_eq!(entries(&root), vec![first], "a second call must not add a second file");
    }

    #[tokio::test]
    async fn ensure_publishes_the_embedded_shim_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ShimBinStore::new(tmp.path().join("ocx-shim"));

        let published = store.ensure().await.unwrap();

        assert_eq!(
            tokio::fs::read(&published).await.unwrap(),
            crate::shim::SHIM_BYTES,
            "the published blob is `crate::shim::SHIM_BYTES` verbatim — the \
             Authenticode verbatim-copy property every hardlinked `<name>.exe` \
             inherits"
        );
    }

    #[tokio::test]
    async fn ensure_does_not_rewrite_a_blob_that_is_already_published() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ShimBinStore::new(tmp.path().join("ocx-shim"));

        let published = store.ensure().await.unwrap();
        // C-001: the blob is written "only when absent". Overwriting the
        // published file with a sentinel makes the pre-check observable — an
        // implementation that publishes unconditionally replaces the sentinel
        // (off Windows it would truncate it to zero bytes). The sentinel
        // carries the embedded blob's LENGTH so the intactness half of the
        // pre-check reads it as healthy on every host; see
        // `intact_length_sentinel`.
        let sentinel = intact_length_sentinel();
        tokio::fs::write(&published, &sentinel).await.unwrap();

        assert_eq!(
            store.ensure().await.unwrap(),
            published,
            "a present blob is still reported at the same path"
        );
        assert_eq!(
            tokio::fs::read(&published).await.unwrap(),
            sentinel,
            "`ensure()` must pre-check and skip the write when the blob is \
             already present and whole, never re-publish over it"
        );
    }

    /// C-001's corrupt-blob pre-check, at its call site rather than in the
    /// predicate alone.
    ///
    /// Host-dependent by construction, and it says so instead of pretending
    /// otherwise: `published_blob_is_intact` admits every length when nothing
    /// is embedded, so off Windows there is no torn state to detect and the
    /// pre-check stays existence-only — that inertness is C-001's accepted
    /// design, not an omission. The expectation branches on whether this build
    /// embeds a blob at all, so on a Windows build this asserts the republish
    /// and on Linux it asserts the documented inertness.
    #[tokio::test]
    async fn ensure_republishes_a_blob_whose_length_is_not_the_embedded_blob() {
        /// One byte: never the embedded blob's length on a host that embeds
        /// one, and the realistic torn shape — created, barely written.
        const TORN: &[u8] = b"\0";

        let tmp = tempfile::tempdir().unwrap();
        let store = ShimBinStore::new(tmp.path().join("ocx-shim"));

        let published = store.ensure().await.unwrap();
        tokio::fs::write(&published, TORN).await.unwrap();

        assert_eq!(
            store.ensure().await.unwrap(),
            published,
            "the blob is reported at the same content-addressed path either way"
        );

        let after = tokio::fs::read(&published).await.unwrap();
        if crate::shim::SHIM_BYTES.is_empty() {
            assert_eq!(
                after, TORN,
                "with nothing embedded there is no length to compare against, so \
                 the pre-check stays existence-only and serves the file as-is"
            );
        } else {
            assert_eq!(
                after,
                crate::shim::SHIM_BYTES,
                "a torn blob must be republished — existence alone cannot tell it \
                 from a healthy one, and it is hardlinked into every launcher"
            );
        }
    }

    /// C-001 (corrected 2026-08-10): concurrent callers must all converge on
    /// `Ok(path)`. A publish that found nothing at its pre-check refuses to
    /// replace an already-present target (`persist_temp_file_if_absent`), so a
    /// loser's publish **fails** on every host; an implementation that
    /// propagates that error hands the loser an I/O error instead of the
    /// winner's blob. The required dance re-checks the target on publish
    /// failure and returns `Ok` when it is present (`finalize_layer_dir`,
    /// `tasks/layer_staging.rs:27-64`).
    ///
    /// The race is genuine, not simulated: there is no black-box input that
    /// forces "publish fails AND the target exists" deterministically, because
    /// the pre-check and the re-check test the same condition — only a real
    /// interleaving separates them. So the row is red-capable but not
    /// red-on-demand: with 16 concurrent callers a loser is near-certain, never
    /// guaranteed. It is never falsely red.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn ensure_converges_when_called_concurrently() {
        const CALLERS: usize = 16;

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("ocx-shim");
        let store = ShimBinStore::new(root.clone());

        let mut callers = tokio::task::JoinSet::new();
        for _ in 0..CALLERS {
            let store = store.clone();
            callers.spawn(async move { store.ensure().await });
        }

        let mut published = Vec::with_capacity(CALLERS);
        while let Some(joined) = callers.join_next().await {
            published.push(
                joined
                    .expect("the `ensure()` task must not panic")
                    .expect("every concurrent caller must converge on Ok — a loser that propagates its failed publish is the C-001 defect"),
            );
        }

        assert_eq!(published.len(), CALLERS);
        assert!(
            published.iter().all(|path| *path == published[0]),
            "every caller must return the same path"
        );
        assert_eq!(
            entries(&root),
            vec![published[0].clone()],
            "exactly one blob survives the race — and no temp litter"
        );
        assert_eq!(
            tokio::fs::read(&published[0]).await.unwrap(),
            crate::shim::SHIM_BYTES,
            "the surviving blob is intact: correctness rests on content \
             identity, so a loser must never leave a truncated or partial file"
        );
    }

    /// C-001 / #301: a loser must leave the winner's **file**, not merely its
    /// bytes.
    ///
    /// `persist` is a rename, so a loser that publishes anyway swaps a fresh
    /// file record in at the target and orphans the winner's — and on Windows
    /// every generated `<name>.exe` is a hardlink to whichever record was there
    /// when its own `ensure()` returned. The store then holds one record per
    /// caller instead of one record per store, which is the #301 property
    /// inverted. Byte-equality cannot see it: every record carries the same
    /// `SHIM_BYTES`. An in-place mutation of the surviving blob can.
    ///
    /// The shape is `launcher::generate`'s: publish, then immediately link,
    /// once per declared name, all concurrently, against a **cold** store —
    /// the state where every caller's pre-check finds nothing and they all
    /// publish. Runs on every host, because the property belongs to the store
    /// and `std::fs::hard_link` shares a record on NTFS and POSIX alike; the
    /// Windows-only `generate_hardlinks_every_exe_to_the_one_shared_store_blob`
    /// is the same property observed one layer up.
    ///
    /// Red-capable but not red-on-demand, same caveat as the row above: 16
    /// concurrent cold callers make a loser near-certain, never guaranteed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn ensure_never_replaces_a_blob_an_earlier_caller_already_linked() {
        /// Content no `ensure()` can have written, so reading it back through a
        /// link proves that link still names the store's own blob.
        const MUTATED: &[u8] = b"mutated-through-the-shared-record";
        const CALLERS: usize = 16;

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("ocx-shim");
        let links = tmp.path().join("links");
        std::fs::create_dir_all(&links).unwrap();
        let store = ShimBinStore::new(root.clone());

        let mut callers = tokio::task::JoinSet::new();
        for caller in 0..CALLERS {
            let store = store.clone();
            let link = links.join(format!("name-{caller}.exe"));
            callers.spawn(async move {
                let published = store.ensure().await?;
                crate::hardlink::create(&published, &link)?;
                crate::Result::Ok(link)
            });
        }

        let mut linked = Vec::with_capacity(CALLERS);
        while let Some(joined) = callers.join_next().await {
            linked.push(
                joined
                    .expect("the publish-and-link task must not panic")
                    .expect("every caller must publish and link"),
            );
        }

        let published = store.ensure().await.unwrap();
        std::fs::write(&published, MUTATED).expect("the store blob must be writable in place");

        for link in linked {
            assert_eq!(
                std::fs::read(&link).unwrap(),
                MUTATED,
                "{} must still name the store's blob — a loser that replaced the \
                 published file left this link on an orphaned record",
                link.display()
            );
        }
    }

    /// C-001 (b) — the re-check leg, which
    /// `ensure_converges_when_called_concurrently` can only reach by winning a
    /// genuine interleaving. The `__OCX_TESTING_SHIM_LOST_PUBLISH_RACE` seam
    /// puts `ensure()` in exactly the state a loser is in — pre-check saw
    /// nothing, publish then failed — and both outcomes of the re-check are
    /// observed here, which is what makes the green mean something: with the
    /// winner's blob present the failure converges to `Ok`; with nothing at
    /// the target the same failure propagates. An implementation with no
    /// re-check fails the first leg; one that swallows every publish failure
    /// fails the second.
    ///
    /// One test function owns the process-global variable for the whole
    /// binary — the serial scope of a single `#[test]` is the ordering
    /// guarantee (precedent:
    /// `host_capabilities::detect_with_ocx_test_libc_override_cases`). The
    /// seam is additionally scoped by value to one store root, so an armed
    /// seam cannot reach a store another test is publishing into.
    #[tokio::test]
    async fn ensure_converges_when_the_publish_loses_the_race() {
        let tmp = tempfile::tempdir().unwrap();

        // ── Winner present: the losing publish converges on the winner ──
        let root = tmp.path().join("winner");
        let store = ShimBinStore::new(root.clone());
        let published = store.ensure().await.unwrap();
        // Stand in for the winner's bytes with content this call cannot have
        // written, so "the winner's file survived" is observable off Windows
        // where `SHIM_BYTES` is empty and byte-equality proves nothing.
        tokio::fs::write(&published, SENTINEL).await.unwrap();

        // SAFETY: this test is the only place that touches
        // `LOST_PUBLISH_RACE_SEAM`, and it is removed again before the next
        // await point that could observe it.
        unsafe { std::env::set_var(LOST_PUBLISH_RACE_SEAM, &root) };
        let converged = store.ensure().await;
        // SAFETY: see above.
        unsafe { std::env::remove_var(LOST_PUBLISH_RACE_SEAM) };

        assert_eq!(
            converged.expect("a losing publish must not surface an error to the caller"),
            published,
            "the loser reports the same path the winner published"
        );
        assert_eq!(
            tokio::fs::read(&published).await.unwrap(),
            SENTINEL,
            "the winner's file is never overwritten by the loser"
        );
        assert_eq!(
            entries(&root),
            vec![published],
            "the loser's staged temp file is discarded, never left behind"
        );

        // ── No winner: the same publish failure propagates ──
        let empty_root = tmp.path().join("no-winner");
        let empty_store = ShimBinStore::new(empty_root.clone());

        // SAFETY: see above.
        unsafe { std::env::set_var(LOST_PUBLISH_RACE_SEAM, &empty_root) };
        let propagated = empty_store.ensure().await;
        // SAFETY: see above.
        unsafe { std::env::remove_var(LOST_PUBLISH_RACE_SEAM) };

        assert!(
            propagated.is_err(),
            "a failed publish with nothing at the target is a genuine failure — \
             the re-check must not turn every publish error into `Ok`"
        );
        assert!(
            entries(&empty_root).is_empty(),
            "no temp litter survives the propagated failure"
        );
    }
}
