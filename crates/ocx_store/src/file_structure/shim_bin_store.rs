// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The embedded `ocx-shim` blob every Windows launcher hardlinks: one inode, one signature, one Defender scan.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

type Result<T> = std::result::Result<T, ocx_util::error::FileError>;

/// Forces [`ShimBinStore::ensure`] down its lost-race leg; valued by store root so it reaches exactly one store.
#[cfg(any(test, feature = "__testing"))]
const LOST_PUBLISH_RACE_SEAM: &str = "__OCX_TESTING_SHIM_LOST_PUBLISH_RACE";

/// Hashes [`crate::shim::SHIM_BYTES`] rather than reading `SHIM_SHA256`, which is `""` off Windows and names no file.
fn shim_digest() -> &'static ocx_oci::Digest {
    static SHIM_DIGEST: OnceLock<ocx_oci::Digest> = OnceLock::new();
    SHIM_DIGEST.get_or_init(|| {
        let digest = ocx_oci::Algorithm::Sha256.hash(crate::shim::SHIM_BYTES);
        debug_assert!(
            crate::shim::SHIM_SHA256.is_empty() || digest.hex() == crate::shim::SHIM_SHA256,
            "sha256(SHIM_BYTES) must equal the recorded SHIM_SHA256 corruption canary"
        );
        digest
    })
}

/// A seam because no black-box input reaches the lost-race leg: pre-check and re-check test the same condition.
#[cfg(any(test, feature = "__testing"))]
fn simulated_lost_race(root: &Path) -> bool {
    std::env::var_os(LOST_PUBLISH_RACE_SEAM).is_some_and(|armed| Path::new(&armed) == root)
}

#[cfg(not(any(test, feature = "__testing")))]
fn simulated_lost_race(_root: &Path) -> bool {
    false
}

/// What [`ShimBinStore::ensure`]'s publish may do to a file already at the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Publish {
    /// Never replace a concurrent winner: its hardlinked launchers would be orphaned from the store's inode.
    OnlyIfAbsent,
    /// The pre-check saw a wrong-length blob, so replacing it is the point.
    OverTornBlob,
}

/// Twinned by `cfg`, never branched inline, so a release build carries no simulated-failure path.
#[cfg(any(test, feature = "__testing"))]
fn publish_staged(
    temp: tempfile::NamedTempFile,
    target: &Path,
    lost_race: bool,
    publish: Publish,
) -> std::io::Result<()> {
    if lost_race {
        // Drop first, as a real failed persist does, so the seam leaves the same filesystem state.
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
        Publish::OnlyIfAbsent => ocx_util::fs::persist_temp_file_if_absent(temp, target),
        Publish::OverTornBlob => ocx_util::fs::persist_temp_file(temp, target),
    }
}

/// Flat `{root}/<sha256-hex>.exe`; outside the GC tiers, so a superseded blob stays as accepted litter.
#[derive(Debug, Clone)]
pub struct ShimBinStore {
    root: PathBuf,
}

impl ShimBinStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `.exe` on every host: the blob is always a Windows PE.
    pub fn path(&self, digest: &ocx_oci::Digest) -> PathBuf {
        self.root.join(format!("{}.exe", digest.hex()))
    }

    /// Publishes the embedded blob when absent or torn and returns its path; idempotent and lock-free.
    ///
    /// # Errors
    ///
    /// Creating the root, staging, or publishing fails with the target still absent.
    pub async fn ensure(&self) -> Result<PathBuf> {
        let target = self.path(shim_digest());
        let lost_race = simulated_lost_race(&self.root);
        let mut publish = Publish::OnlyIfAbsent;

        // Length, not existence: a crash can leave a torn blob that every hardlinked `<name>.exe` would run.
        if !lost_race {
            match tokio::fs::metadata(&target).await {
                Ok(metadata) if crate::shim::published_blob_is_intact(metadata.len(), crate::shim::SHIM_BYTES) => {
                    return Ok(target);
                }
                Ok(metadata) => {
                    log::debug!(
                        "Published shim blob {} is {} bytes, expected {}; republishing over the torn write.",
                        target.display(),
                        metadata.len(),
                        crate::shim::SHIM_BYTES.len()
                    );
                    publish = Publish::OverTornBlob;
                }
                Err(error) => log::debug!(
                    "Cannot stat published shim blob {} ({error}); treating it as absent.",
                    target.display()
                ),
            }
        }

        tokio::fs::create_dir_all(&self.root)
            .await
            .map_err(|e| ocx_util::error::FileError::new(&self.root, e))?;

        let root = self.root.clone();
        let published = target.clone();
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            let mut temp = tempfile::NamedTempFile::new_in(&root)?;
            std::io::Write::write_all(&mut temp, crate::shim::SHIM_BYTES)?;
            temp.as_file().sync_data()?;

            match publish_staged(temp, &published, lost_race, publish) {
                Ok(()) => Ok(()),
                // Only `OnlyIfAbsent` may converge on a winner: after a failed `OverTornBlob` the file is still the torn PE.
                Err(error) if publish == Publish::OnlyIfAbsent && published.exists() => {
                    log::debug!(
                        "Shim blob {} was published concurrently ({error}); keeping the winner's file.",
                        published.display()
                    );
                    Ok(())
                }
                Err(error) => Err(error),
            }
        })
        .await
        .map_err(|join_error| ocx_util::error::FileError::new(&target, std::io::Error::other(join_error)))?
        .map_err(|io_error| ocx_util::error::FileError::new(&target, io_error))?;

        Ok(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn digest() -> ocx_oci::Digest {
        ocx_oci::Digest::Sha256(SHA256_HEX.to_string())
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
                Result::Ok(link)
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
