// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Drift repair for a local index source's `c/index.json`
//! (`adr_servable_index_snapshot.md` Decision C, C-007/C-008).
//!
//! One drift is unrecoverable by every other path: a catalog entry naming a
//! package whose root document is gone. [`CatalogTransaction::write_root`]
//! only upserts and [`IndexStore::read_root`]'s self-heal only adds, so that
//! entry is permanent and the catalog lies about the tree's contents from then
//! on. [`regenerate_catalog`] re-derives the whole map from the `p/` walk,
//! which is the only operation that clears it.

use crate::oci::index::CatalogIndex;
use crate::{Result, file_structure::IndexStore};

/// Re-derives `source`'s `c/index.json` from the root documents on disk.
///
/// The tree is the source of truth; the catalog is derived data. Every root the
/// `p/` walk finds contributes `<repository> -> sha256(root bytes on disk)`, and
/// the derivation **replaces** the map read under the lock — an entry naming a
/// root that is not on disk is dropped (C-008).
///
/// # Three invariants
///
/// 1. **It never writes `config.json`.** `name_segments` is an operator
///    declaration OCX cannot derive from a tree, and fabricating one for a
///    foreign tree under repair would be wrong. Creation is C-023's job alone,
///    in `LocalIndex::commit_published_root` — deliberately **not** in
///    [`CatalogTransaction::commit`], which is a shared primitive that this
///    function and the `read_root` catalog self-heal
///    (`IndexStore::persist_recovered_catalog_entry`) both call. A hook there
///    would make `regenerate` inject metadata into a tree OCX does not own, and
///    would make a plain resolve create a wire document as a side effect.
///    Asserted by S-019.
/// 2. **It removes no root document and no `o/` object, ever.** Wholesale
///    replacement of a *derived* document is derivation, not deletion, which is
///    why this does not violate the merge-only-never-delete rule
///    (`subsystem-oci.md`) — that rule governs roots and objects, the things
///    that carry pins. `c/index.json` is the only path it *writes*, but not the
///    only path it touches: it inherits two side effects from the primitives it
///    drives. [`CatalogTransaction::commit`] unconditionally `remove_file`s
///    `c/index.json.etag` *before* its `catalog == original` early return
///    (`index_store.rs:1019-1023`, failure ignored on purpose), so against a
///    served-tree checkout that tracks such a file `regenerate` **deletes a
///    version-controlled file** and cannot fail on it; and
///    [`IndexStore::lock_source`] `create_dir_all`s the source directory — which
///    is why the Preconditions below have to be checked before the transaction
///    opens rather than inside it.
/// 3. **Nothing symlinked under `p/` is enumerated — neither a root document
///    nor a directory.** [`IndexStore::list_wire_repositories`] walks with
///    `tokio::fs::DirEntry::file_type()`, which reports the link's own type,
///    and branches `is_dir()` then `!is_file()` (`index_store.rs:772-782`): a
///    symlink is **neither**, so a symlinked `p/**.json` is skipped, and a
///    symlinked *directory* is never queued — taking **every root beneath it**
///    in one step. Because this contract replaces the catalog wholesale, that
///    is not one missing entry but silent **bulk removal** from `c/index.json`.
///    `regenerate` is specified for trees whose roots *and intermediate
///    directories* are real — which is every tree OCX produces. An operator
///    running it against a symlink-deduplicated layout must know this; the
///    limit is stated, not worked around (C-016's scope note).
///
/// # Preconditions
///
/// `source` is contained and **its subtree already exists**. No assumption that
/// the tree was OCX-authored: no prior `c/index.json`, no `config.json`, roots
/// possibly written by another implementation.
///
/// Existence is checked *before* [`IndexStore::begin_catalog_transaction`],
/// because `lock_source` `create_dir_all`s the source directory
/// (`index_store.rs:341`): without the pre-flight a mistyped source **creates**
/// the tree, walks nothing, reports `roots: 0` and exits **0** —
/// indistinguishable from a clean tree — and under the C-026 addressing below
/// drops a stray empty directory beside a served checkout. A repair verb
/// pointed at nothing is a user error, not a no-op.
///
/// A served tree whose root **is** the source directory (`config.json` / `c/` /
/// `p/` at a repo checkout root) needs no new API: root the store at the
/// checkout's *parent* and pass the checkout's own directory name as `source`,
/// because `wire_source_dir` is `root.join(slugify(source))` (C-026). That
/// carries two caller obligations this signature cannot enforce:
///
/// 1. **The checkout directory name must be slug-identical to itself**, or the
///    store addresses a sibling directory that does not exist rather than the
///    checkout. `to_relaxed_slug` preserves `[a-zA-Z0-9._-]`, so any ordinary
///    name is.
/// 2. **`locks_root` must be redirected** off its `root/locks` default
///    (`IndexStore::with_locks_root`), which would otherwise create a `locks/`
///    directory beside the checkout — inside the served tree. A CI checkout is
///    exclusive, so a scratch dir is fine.
///
/// # Network
///
/// Zero. No `IndexTransport`, no `oci::Client`; no source is constructible from
/// this signature. `--frozen` and `--offline` therefore both permit it (C-021):
/// it consults no source, moves no `tags[].content`, and touches no root's
/// `repository`.
///
/// # Idempotence
///
/// A second call returns three empty vectors and leaves the tree byte- and
/// mtime-identical — exact from the *second* run onward. A **first** run against
/// a tree written by an older ocx also drops the stray `c/index.json.etag`
/// (invariant 2), so a byte-identity test must seed none.
///
/// # Errors
///
/// - [`Error::MalformedRootDocument`] (exit 65) — a root under `p/` does not
///   parse.
/// - `file_error` (exit 74) — `source`'s subtree does not exist (Preconditions);
///   the `p/` walk found no root at all while the catalog on disk names packages
///   (C-008 — `list_wire_repositories` answers `Ok(vec![])` for a missing `p/`
///   exactly as it does for a tree genuinely holding zero packages, and
///   wholesale replacement must never be reachable from "I could not find the
///   tree"); or the source directory cannot be locked (lock timeout).
///
/// [`CatalogTransaction`]: crate::file_structure::CatalogTransaction
/// [`CatalogTransaction::commit`]: crate::file_structure::CatalogTransaction::commit
/// [`CatalogTransaction::write_root`]: crate::file_structure::CatalogTransaction::write_root
/// [`Error::MalformedRootDocument`]: crate::file_structure::error::Error::MalformedRootDocument
pub async fn regenerate_catalog(store: &IndexStore, source: &str) -> Result<RegenerateOutcome> {
    // Containment first, because the pre-flight below builds its path through
    // `source_config_path` — a pure builder with NO guard, unlike the nine
    // `IndexStore` entry points that call this check themselves. `slugify`
    // preserves `.`, so a `source` of `".."` survives it verbatim and the
    // pre-flight would stat the index home's parent. Nothing escapes today
    // (`begin_catalog_transaction` guards on the next line, and `ocx index
    // regenerate` refuses a source that is not a configured namespace) — but
    // this function is `ocx_lib`'s, and the CLI is not its only future caller.
    IndexStore::ensure_source_contained(source)?;

    // The source directory, derived from the one public path accessor that
    // exposes it — `source_config_path` is `<source dir>/config.json`.
    let config_path = store.source_config_path(source);
    let source_dir = config_path
        .parent()
        .ok_or_else(|| crate::Error::InternalPathInvalid(config_path.clone()))?;

    // C-007 Preconditions: before the transaction, because `lock_source`
    // creates what it locks — inside it this check can never fail.
    if !crate::utility::fs::path_exists_lossy(source_dir).await {
        return Err(crate::error::file_error(
            source_dir,
            std::io::Error::new(std::io::ErrorKind::NotFound, "index source subtree does not exist"),
        ));
    }

    let mut transaction = store.begin_catalog_transaction(source).await?;

    let mut derived = CatalogIndex::new();
    for repository in store.list_wire_repositories(source).await? {
        // C-007 step 2: the `repository_check` is `|_| Ok(())` and must stay
        // that way. Its failure is a HARD error through `read_root_inner`, and
        // the neighbouring call site's `oci://`-scheme validator
        // (`local_index.rs`) would reject exactly the foreign trees the
        // Preconditions promise to accept. `regenerate` never reads
        // `repository` — that is also what makes it `--frozen`-safe (C-021).
        let Some(read) = store.read_root_uncatalogued(source, &repository, |_| Ok(())).await? else {
            // Not on disk now, which is the only thing the derivation asks —
            // but because the derivation *replaces* the map, a skip here is a
            // silent removal from `c/index.json` that `removed` never names.
            // One cause: a deletion racing the walk. The other — a non-UTF-8
            // name decoded lossily into a path that cannot be found — is a hard
            // error at the walk since C-028, so it no longer arrives here.
            crate::log::debug!(
                "regenerate: skipping repository '{repository}' of source '{source}' — \
                 the p/ walk listed it but no root document is readable at that path"
            );
            continue;
        };
        derived.insert(repository, IndexStore::root_catalog_entry(&read.bytes));
    }

    let roots = derived.len();
    // C-008: wholesale replacement must never be reachable from "I could not
    // find the tree". `list_wire_repositories` answers `Ok(vec![])` for a
    // missing `p/` and for a tree genuinely holding zero packages alike, so a
    // sparse checkout, a CI job running before `p/` is materialized, or a store
    // rooted one level off would otherwise write an empty catalog over a live
    // one and exit 0 — loud in `removed`, silent in the exit code.
    if roots == 0 && !transaction.catalog().is_empty() {
        return Err(crate::error::file_error(
            source_dir.join("p"),
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "refusing to replace a catalog of {} package(s) with an empty one: \
                     the p/ walk found no root document",
                    transaction.catalog().len()
                ),
            ),
        ));
    }

    // Wholesale replacement, not a merge: dropping an entry whose root is gone
    // is the one drift no other path can repair (C-008). The post-lock map comes
    // back out as the diff basis — both maps are sorted, so all three report
    // lists come out in repository order for free.
    let previous = std::mem::replace(transaction.catalog(), derived);
    let derived = transaction.catalog();
    let mut added = Vec::new();
    let mut corrected = Vec::new();
    for (repository, entry) in derived.iter() {
        match previous.get(repository) {
            None => added.push(repository.clone()),
            Some(stale) if stale != entry => corrected.push(repository.clone()),
            Some(_) => {}
        }
    }
    let removed: Vec<String> = previous
        .keys()
        .filter(|repository| !derived.contains_key(*repository))
        .cloned()
        .collect();

    transaction.commit().await?;

    Ok(RegenerateOutcome {
        source: source.to_string(),
        roots,
        added,
        corrected,
        removed,
    })
}

/// What one [`regenerate_catalog`] run changed, as the CLI reports it (C-010).
///
/// The three lists are `<ns>/<pkg>` repository paths. All three empty means the
/// catalog already matched the tree and nothing was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegenerateOutcome {
    /// The index source the catalog was re-derived for.
    pub source: String,
    /// Roots found under `p/`.
    pub roots: usize,
    /// Root on disk, absent from the catalog.
    pub added: Vec<String>,
    /// Entry digest disagreed with the root on disk.
    pub corrected: Vec<String>,
    /// Catalog named a package with no root on disk. The one drift nothing
    /// else can repair.
    pub removed: Vec<String>,
}

/// Specification tests for [`regenerate_catalog`], written from
/// `design_spec_servable_index_snapshot.md`'s **C-007**, **C-008** and
/// **C-026** rather than from the implementation — each one names the clause it
/// pins, and a test that stops failing when its clause is violated is a bug in
/// the test.
///
/// Trees are built on a `tempfile::TempDir` through the store's own public API
/// ([`IndexStore::write_root_document`] for a catalogue-free root,
/// [`IndexStore::begin_catalog_transaction`] for a root plus its entry, or for
/// forcing an entry into a chosen state). `locks_root` is redirected off-tree
/// the way both production construction sites do, so no `locks/` directory can
/// ever appear inside a snapshotted subtree.
#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::time::SystemTime;

    use super::*;
    use crate::cli::{ClassifyExitCode, ExitCode};
    use crate::oci::Algorithm;
    use crate::oci::index::{CatalogDocument, CatalogIndex, IndexRoot};

    const SOURCE: &str = "ocx.sh";

    /// A syntactically valid `sha256:<64-hex>` tag pointer. Never dereferenced
    /// — `regenerate` reads no tag — it only has to survive `oci::Digest`'s
    /// exact-wire deserialize so the root document parses at all.
    const TAG_CONTENT: &str = "sha256:43567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d9";

    /// A well-formed catalog value that is not the digest of anything on disk —
    /// what a hand-edited or stale `c/index.json` carries.
    const FOREIGN_ENTRY: &str = "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    /// A store rooted at `root` with its cross-process locks redirected off-tree
    /// (`with_locks_root`), matching both production construction sites — and
    /// keeping a `locks/` directory out of every before/after snapshot below.
    fn store_at(root: &Path, locks: &Path) -> IndexStore {
        IndexStore::new(root).with_locks_root(locks)
    }

    /// `p/<ns>/<pkg>.json` bytes in the shape `test/src/static_index.py`'s
    /// `write_package()` emits: a `repository` pointer plus one tag.
    fn root_bytes(repository: &str) -> Vec<u8> {
        serde_json::json!({
            "repository": repository,
            "tags": { "1.0": { "content": TAG_CONTENT, "observed": "2026-08-09T09:00:00Z" } }
        })
        .to_string()
        .into_bytes()
    }

    /// A minimal OCI image index — the shape a `p/<ns>/<pkg>/o/<algo>/<hex>.json`
    /// dispatch object actually holds (A3).
    fn dispatch_object_bytes() -> Vec<u8> {
        serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.index.v1+json",
            "manifests": [{
                "mediaType": "application/vnd.oci.image.manifest.v1+json",
                "digest": TAG_CONTENT,
                "size": 42,
                "platform": { "architecture": "amd64", "os": "linux" }
            }]
        })
        .to_string()
        .into_bytes()
    }

    /// Seeds a root document AND its derived catalog entry — the consistent
    /// state an `ocx index update` leaves behind.
    async fn seed_root_with_catalog_entry(store: &IndexStore, repository: &str, bytes: &[u8]) {
        let mut transaction = store.begin_catalog_transaction(SOURCE).await.unwrap();
        transaction
            .write_root(repository, bytes, |_root: &IndexRoot| Ok(()))
            .await
            .unwrap();
        transaction.commit().await.unwrap();
    }

    /// Forces one catalog entry to `entry` without touching any root document —
    /// how these tests fabricate the drift `regenerate` exists to repair.
    async fn force_catalog_entry(store: &IndexStore, repository: &str, entry: &str) {
        let mut transaction = store.begin_catalog_transaction(SOURCE).await.unwrap();
        transaction.catalog().insert(repository.to_string(), entry.to_string());
        transaction.commit().await.unwrap();
    }

    /// Decodes `c/index.json` straight off disk. Deliberately NOT a call to
    /// [`IndexStore::read_source_catalog`] — these tests assert what the writer
    /// actually put on disk, so going through the reader under test would let a
    /// matched pair of read/write bugs pass.
    fn catalog_on_disk(path: &Path) -> CatalogIndex {
        let document: CatalogDocument = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        document.into_packages().unwrap()
    }

    /// Every regular file under `dir`, keyed by path, with its bytes and its
    /// mtime. C-008's "removes no root and no `o/` object" claim is a statement
    /// about the filesystem that the return value cannot witness — a dropped
    /// root leaves `removed` looking exactly as it should — so it has to be
    /// checked here instead. Modelled on the structural before/after shape of
    /// `chained_index::chain_refs_tests::op_query_never_writes_local_index_in_any_mode`.
    fn file_snapshot(dir: &Path) -> BTreeMap<PathBuf, (Vec<u8>, SystemTime)> {
        let mut snapshot = BTreeMap::new();
        let mut queue = vec![dir.to_path_buf()];
        while let Some(current) = queue.pop() {
            for entry in std::fs::read_dir(&current).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                if entry.file_type().unwrap().is_dir() {
                    queue.push(path);
                    continue;
                }
                let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
                snapshot.insert(path.clone(), (std::fs::read(&path).unwrap(), modified));
            }
        }
        snapshot
    }

    /// The bytes projection of a [`file_snapshot`], for the clauses the spec
    /// words as "byte-identical".
    fn bytes_only(snapshot: &BTreeMap<PathBuf, (Vec<u8>, SystemTime)>) -> BTreeMap<PathBuf, Vec<u8>> {
        snapshot
            .iter()
            .map(|(path, (bytes, _))| (path.clone(), bytes.clone()))
            .collect()
    }

    /// The mtime projection of a [`file_snapshot`], for the clauses the spec
    /// words as "mtime-identical" — asserted separately from the bytes so a
    /// gratuitous rewrite of identical content fails with its own message.
    fn mtimes_only(snapshot: &BTreeMap<PathBuf, (Vec<u8>, SystemTime)>) -> BTreeMap<PathBuf, SystemTime> {
        snapshot
            .iter()
            .map(|(path, (_, modified))| (path.clone(), *modified))
            .collect()
    }

    // ── C-008: the headline drift — an entry naming a root that is gone ──────

    /// **C-008.** The one drift nothing else can repair: `write_root` only
    /// upserts (`index_store.rs:993-995`) and `read_root`'s self-heal only adds,
    /// so an entry whose root document is not on disk is permanent until the
    /// catalog is re-derived wholesale. This is the contract's reason to exist.
    #[tokio::test(flavor = "multi_thread")]
    async fn regenerate_drops_a_catalog_entry_naming_a_root_that_is_not_on_disk() {
        let home = tempfile::tempdir().unwrap();
        let locks = tempfile::tempdir().unwrap();
        let store = store_at(home.path(), locks.path());
        let cmake = root_bytes("oci://ocx.sh/kitware/cmake");
        seed_root_with_catalog_entry(&store, "kitware/cmake", &cmake).await;
        force_catalog_entry(&store, "ns/ghost", FOREIGN_ENTRY).await;

        let outcome = regenerate_catalog(&store, SOURCE).await.unwrap();

        assert_eq!(
            outcome.removed,
            vec!["ns/ghost".to_string()],
            "C-008: an entry naming a package with no root on disk must be dropped"
        );
        assert!(
            outcome.added.is_empty() && outcome.corrected.is_empty(),
            "C-008: the surviving root already agreed with its entry, so nothing was added or corrected; got {outcome:?}"
        );
        assert_eq!(outcome.roots, 1, "C-007: one root document lives under p/");
        assert_eq!(
            outcome.source, SOURCE,
            "C-007: the outcome names the source it repaired"
        );

        let catalog = catalog_on_disk(&store.source_catalog_path(SOURCE));
        assert!(
            !catalog.contains_key("ns/ghost"),
            "C-008: the ghost must be gone from c/index.json on disk, not merely from the report"
        );
        assert_eq!(
            catalog.get("kitware/cmake").map(String::as_str),
            Some(IndexStore::root_catalog_entry(&cmake).as_str()),
            "C-008: the written entry is exactly sha256(root bytes on disk)"
        );
    }

    // ── C-007/C-008: the three report lists ──────────────────────────────────

    /// **C-007 `added`.** A root on disk that the catalog never named — the
    /// state a `write_root_document` publish (or a foreign tree with no
    /// `c/index.json` at all) leaves behind.
    #[tokio::test(flavor = "multi_thread")]
    async fn regenerate_adds_a_root_the_catalog_never_named() {
        let home = tempfile::tempdir().unwrap();
        let locks = tempfile::tempdir().unwrap();
        let store = store_at(home.path(), locks.path());
        let cmake = root_bytes("oci://ocx.sh/kitware/cmake");
        store
            .write_root_document(SOURCE, "kitware/cmake", &cmake)
            .await
            .unwrap();
        assert!(
            !store.source_catalog_path(SOURCE).exists(),
            "fixture: the tree must start with no c/index.json at all"
        );

        let outcome = regenerate_catalog(&store, SOURCE).await.unwrap();

        assert_eq!(
            outcome.added,
            vec!["kitware/cmake".to_string()],
            "C-007: a root on disk that the catalog does not name is reported as added"
        );
        assert!(
            outcome.corrected.is_empty() && outcome.removed.is_empty(),
            "C-007: nothing was corrected or removed; got {outcome:?}"
        );
        assert_eq!(outcome.roots, 1, "C-007: roots counts what the p/ walk found");
        assert_eq!(
            catalog_on_disk(&store.source_catalog_path(SOURCE)).get("kitware/cmake"),
            Some(&IndexStore::root_catalog_entry(&cmake)),
            "C-008: the derived entry is sha256(root bytes on disk)"
        );
    }

    /// **C-007 `corrected`.** An entry whose digest disagrees with the root
    /// document actually on disk. The tree is the source of truth; the catalog
    /// is derived data, so the root wins.
    #[tokio::test(flavor = "multi_thread")]
    async fn regenerate_corrects_an_entry_whose_digest_disagrees_with_the_root() {
        let home = tempfile::tempdir().unwrap();
        let locks = tempfile::tempdir().unwrap();
        let store = store_at(home.path(), locks.path());
        let cmake = root_bytes("oci://ocx.sh/kitware/cmake");
        seed_root_with_catalog_entry(&store, "kitware/cmake", &cmake).await;
        force_catalog_entry(&store, "kitware/cmake", FOREIGN_ENTRY).await;

        let outcome = regenerate_catalog(&store, SOURCE).await.unwrap();

        assert_eq!(
            outcome.corrected,
            vec!["kitware/cmake".to_string()],
            "C-007: an entry disagreeing with the root on disk is reported as corrected"
        );
        assert!(
            outcome.added.is_empty() && outcome.removed.is_empty(),
            "C-007: the package was already catalogued and its root is present; got {outcome:?}"
        );
        assert_eq!(
            catalog_on_disk(&store.source_catalog_path(SOURCE)).get("kitware/cmake"),
            Some(&IndexStore::root_catalog_entry(&cmake)),
            "C-008: the corrected value is re-derived from the root bytes, never kept from the catalog"
        );
    }

    /// **C-007 `roots`.** The count is what the `p/` walk found — root
    /// documents only. A dispatch object under `p/<ns>/<pkg>/o/<algo>/<hex>.json`
    /// is a `.json` file inside `p/` and must not be counted as a fourth root.
    #[tokio::test(flavor = "multi_thread")]
    async fn regenerate_counts_roots_under_p_and_never_a_dispatch_object() {
        let home = tempfile::tempdir().unwrap();
        let locks = tempfile::tempdir().unwrap();
        let store = store_at(home.path(), locks.path());
        for repository in ["kitware/cmake", "gnu/make", "ninja/ninja"] {
            seed_root_with_catalog_entry(&store, repository, &root_bytes(&format!("oci://ocx.sh/{repository}"))).await;
        }
        let object = dispatch_object_bytes();
        let digest = Algorithm::Sha256.hash(&object);
        store
            .write_dispatch_object(SOURCE, "kitware/cmake", &digest, &object)
            .await
            .unwrap();

        let outcome = regenerate_catalog(&store, SOURCE).await.unwrap();

        assert_eq!(
            outcome.roots, 3,
            "C-007: roots counts the three root documents under p/, never the dispatch object beside one of them"
        );
        assert!(
            outcome.added.is_empty() && outcome.corrected.is_empty() && outcome.removed.is_empty(),
            "C-007: a catalog that already matches the tree reports no change; got {outcome:?}"
        );
    }

    // ── C-007: idempotence ───────────────────────────────────────────────────

    /// **C-007 Idempotence.** A second call returns three empty vectors and
    /// leaves the tree byte- and mtime-identical.
    ///
    /// The fixture seeds **no** `c/index.json.etag`: `commit` unconditionally
    /// `remove_file`s that path *before* its `catalog == original` early return
    /// (`index_store.rs:1013-1026`), so a first run against a tree carrying one
    /// legitimately differs, and a byte-identity assertion over such a tree
    /// would fail for a reason the contract explicitly allows (C-008).
    #[tokio::test(flavor = "multi_thread")]
    async fn regenerate_is_idempotent_byte_and_mtime_identical_on_the_second_run() {
        let home = tempfile::tempdir().unwrap();
        let locks = tempfile::tempdir().unwrap();
        let store = store_at(home.path(), locks.path());
        seed_root_with_catalog_entry(&store, "kitware/cmake", &root_bytes("oci://ocx.sh/kitware/cmake")).await;
        force_catalog_entry(&store, "ns/ghost", FOREIGN_ENTRY).await;
        let source_dir = store.source_config_path(SOURCE).parent().unwrap().to_path_buf();
        assert!(
            !store.source_catalog_path(SOURCE).with_added_extension("etag").exists(),
            "fixture: no stale .etag may exist, or the first run's cleanup would legitimately change the tree"
        );

        let first = regenerate_catalog(&store, SOURCE).await.unwrap();
        assert_eq!(
            first.removed,
            vec!["ns/ghost".to_string()],
            "fixture: the first call must have done real work, or an implementation that no-ops \
             both calls satisfies the idempotence assertions vacuously"
        );
        let before = file_snapshot(&source_dir);
        assert!(
            before.contains_key(&store.source_catalog_path(SOURCE)),
            "fixture: the first call must have written c/index.json into the snapshotted subtree"
        );

        let second = regenerate_catalog(&store, SOURCE).await.unwrap();

        assert_eq!(
            (
                second.added.as_slice(),
                second.corrected.as_slice(),
                second.removed.as_slice()
            ),
            ([].as_slice(), [].as_slice(), [].as_slice()),
            "C-007: a second call reports three empty vectors"
        );
        let after = file_snapshot(&source_dir);
        assert_eq!(
            bytes_only(&after),
            bytes_only(&before),
            "C-007: a second call leaves the tree byte-identical"
        );
        assert_eq!(
            mtimes_only(&after),
            mtimes_only(&before),
            "C-007: a second call writes nothing, so no mtime moves (commit's catalog == original early return)"
        );
    }

    // ── C-008 / C-022: containment — what regenerate must never touch ────────

    /// **C-008, C-022.** `regenerate` never writes `config.json`. `name_segments`
    /// is an operator declaration OCX cannot derive from a tree, and creation is
    /// C-023's job alone — which is only true because C-023's hook lives in
    /// `LocalIndex::commit_published_root`, not in the shared
    /// `CatalogTransaction::commit` this function also drives. Asserted against a
    /// tree that has no `config.json` before the run.
    #[tokio::test(flavor = "multi_thread")]
    async fn regenerate_never_writes_config_json() {
        let home = tempfile::tempdir().unwrap();
        let locks = tempfile::tempdir().unwrap();
        let store = store_at(home.path(), locks.path());
        seed_root_with_catalog_entry(&store, "kitware/cmake", &root_bytes("oci://ocx.sh/kitware/cmake")).await;
        force_catalog_entry(&store, "ns/ghost", FOREIGN_ENTRY).await;
        let config = store.source_config_path(SOURCE);
        assert!(
            !config.exists(),
            "fixture: the tree must have no config.json before the run, or the assertion after it proves nothing"
        );

        regenerate_catalog(&store, SOURCE).await.unwrap();

        assert!(
            !config.exists(),
            "C-008/C-022: regenerate must not create config.json — the hook belongs to commit_published_root alone"
        );
    }

    /// **C-008.** It removes no root document and no `o/` object, ever, and
    /// `c/index.json` is the only path whose bytes it changes. Asserted against
    /// the **filesystem** — a full file list plus per-file bytes over the whole
    /// source directory, not just `p/`, before and after — because the return
    /// value cannot witness a deletion, and a write outside `p/` is exactly what
    /// a `p/`-only snapshot would miss.
    #[tokio::test(flavor = "multi_thread")]
    async fn regenerate_removes_no_root_document_and_no_dispatch_object() {
        let home = tempfile::tempdir().unwrap();
        let locks = tempfile::tempdir().unwrap();
        let store = store_at(home.path(), locks.path());
        for repository in ["kitware/cmake", "gnu/make"] {
            seed_root_with_catalog_entry(&store, repository, &root_bytes(&format!("oci://ocx.sh/{repository}"))).await;
        }
        let object = dispatch_object_bytes();
        let digest = Algorithm::Sha256.hash(&object);
        store
            .write_dispatch_object(SOURCE, "kitware/cmake", &digest, &object)
            .await
            .unwrap();
        // Drift, so the run actually rewrites the catalog rather than taking
        // commit's no-op path — a no-op run would satisfy this assertion vacuously.
        force_catalog_entry(&store, "ns/ghost", FOREIGN_ENTRY).await;
        let source_dir = store.source_config_path(SOURCE).parent().unwrap().to_path_buf();
        let before = bytes_only(&file_snapshot(&source_dir));
        assert!(
            before.contains_key(&store.dispatch_object_path(SOURCE, "kitware/cmake", &digest)),
            "fixture: the dispatch object must be inside the snapshotted subtree"
        );

        let outcome = regenerate_catalog(&store, SOURCE).await.unwrap();

        assert_eq!(
            outcome.removed,
            vec!["ns/ghost".to_string()],
            "fixture: the run must have done real work, or the survival assertion below is vacuous"
        );
        let after = bytes_only(&file_snapshot(&source_dir));
        assert_eq!(
            after.keys().collect::<Vec<_>>(),
            before.keys().collect::<Vec<_>>(),
            "C-008: the run creates and deletes no file anywhere under the source directory — \
             every root document and every o/ object survives"
        );
        let changed: Vec<&PathBuf> = after
            .iter()
            .filter(|(path, bytes)| before.get(*path) != Some(*bytes))
            .map(|(path, _)| path)
            .collect();
        assert_eq!(
            changed,
            vec![&store.source_catalog_path(SOURCE)],
            "C-008: c/index.json is the only path whose bytes change — wholesale replacement \
             applies to the derived catalog alone"
        );
    }

    // ── C-007 Preconditions: foreign trees ───────────────────────────────────

    /// **C-007 Preconditions.** No prior `c/index.json`, no `config.json`, and a
    /// root written by another implementation whose `repository` value would not
    /// satisfy `LocalIndex`'s `oci://` validator.
    ///
    /// This is the test that pins C-007 step 2's `repository_check` closure as
    /// `|_| Ok(())`. `read_root_uncatalogued` takes one and propagates its
    /// failure as a hard error (`index_store.rs:613`); its only existing caller
    /// passes `parse_physical_repository` (`local_index.rs:582`), and copying
    /// that neighbouring call site — the easy, silently wrong move — hard-fails
    /// exactly the foreign trees these preconditions promise to accept.
    #[tokio::test(flavor = "multi_thread")]
    async fn regenerate_accepts_a_foreign_tree_whose_root_repository_is_not_an_oci_ref() {
        let home = tempfile::tempdir().unwrap();
        let locks = tempfile::tempdir().unwrap();
        let store = store_at(home.path(), locks.path());
        let foreign = root_bytes("https://example.invalid/kitware/cmake");
        assert!(
            crate::oci::index::parse_physical_repository("https://example.invalid/kitware/cmake").is_err(),
            "fixture: this repository value must be one LocalIndex's oci:// validator rejects, \
             or the test does not pin the |_| Ok(()) decision"
        );
        store
            .write_root_document(SOURCE, "kitware/cmake", &foreign)
            .await
            .unwrap();
        assert!(
            !store.source_catalog_path(SOURCE).exists() && !store.source_config_path(SOURCE).exists(),
            "fixture: a foreign tree has neither c/index.json nor config.json"
        );

        let outcome = regenerate_catalog(&store, SOURCE).await.unwrap();

        assert_eq!(
            outcome.added,
            vec!["kitware/cmake".to_string()],
            "C-007: regenerate validates nothing about `repository` — it never reads the field"
        );
        assert_eq!(
            catalog_on_disk(&store.source_catalog_path(SOURCE)).get("kitware/cmake"),
            Some(&IndexStore::root_catalog_entry(&foreign)),
            "C-008: the entry is sha256 of the foreign root's bytes, verbatim"
        );
    }

    // ── C-026: addressing a served-tree root ─────────────────────────────────

    /// **C-026.** A served tree whose root **is** the source directory
    /// (`config.json` / `c/` / `p/` directly in a repo checkout) is addressed
    /// with no new API: the store is rooted at the checkout's *parent* and the
    /// checkout's own directory name is the source, because `wire_source_dir` is
    /// `root.join(slugify(source))`. `locks_root` must be redirected, or the
    /// default `root/locks` lands beside the checkout.
    #[tokio::test(flavor = "multi_thread")]
    async fn regenerate_addresses_a_served_tree_laid_out_at_a_checkout_root() {
        let workspace = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let checkout = workspace.path().join("ocx-index");
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::write(checkout.join("config.json"), b"{\n  \"format_version\": 1\n}\n").unwrap();

        let store = IndexStore::new(workspace.path()).with_locks_root(scratch.path());
        let source = checkout.file_name().unwrap().to_str().unwrap();
        store
            .write_root_document(source, "kitware/cmake", &root_bytes("oci://ocx.sh/kitware/cmake"))
            .await
            .unwrap();
        assert!(
            checkout.join("p").join("kitware").join("cmake.json").is_file(),
            "fixture: the root must land inside the checkout, proving the parent/name addressing"
        );
        let config_before = std::fs::read(checkout.join("config.json")).unwrap();

        let outcome = regenerate_catalog(&store, source).await.unwrap();

        assert_eq!(
            outcome.added,
            vec!["kitware/cmake".to_string()],
            "C-026: the checkout's own p/ walk is what gets enumerated"
        );
        assert!(
            checkout.join("c").join("index.json").is_file(),
            "C-026: the catalog lands at <checkout>/c/index.json"
        );
        assert!(
            !workspace.path().join("c").exists(),
            "C-026: never beside the checkout in the store root"
        );
        assert_eq!(
            std::fs::read(checkout.join("config.json")).unwrap(),
            config_before,
            "S-015: an existing config.json is never rewritten — not reformatted, not re-pinned"
        );
    }

    // ── C-008 invariant 3: what a symlink hides ──────────────────────────────

    /// **C-008.** Neither a symlinked root document nor a symlinked *directory*
    /// under `p/` is enumerated: `list_wire_repositories` branches `is_dir()`
    /// then `!is_file()` and a symlink is neither. Characterization, not a wish
    /// — this is the documented data-loss limit of invariant 3, and under
    /// wholesale replacement the directory case is silent **bulk** removal, so
    /// it needs something failing behind it rather than a paragraph alone.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn regenerate_enumerates_neither_a_symlinked_root_nor_a_symlinked_package_directory() {
        let home = tempfile::tempdir().unwrap();
        let locks = tempfile::tempdir().unwrap();
        let store = store_at(home.path(), locks.path());
        let cmake = root_bytes("oci://ocx.sh/kitware/cmake");
        seed_root_with_catalog_entry(&store, "kitware/cmake", &cmake).await;
        let real_root = store.root_document_path(SOURCE, "kitware/cmake");
        let package_dir = real_root.parent().unwrap().to_path_buf();

        let linked_root = store.root_document_path(SOURCE, "ns/linked");
        std::fs::create_dir_all(linked_root.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&real_root, &linked_root).unwrap();
        let linked_dir = package_dir.parent().unwrap().join("mirror");
        std::os::unix::fs::symlink(&package_dir, &linked_dir).unwrap();
        assert!(
            linked_root.is_file() && linked_dir.join("cmake.json").is_file(),
            "fixture: both symlinks must resolve to a real root document, or the test proves nothing"
        );

        let outcome = regenerate_catalog(&store, SOURCE).await.unwrap();

        assert_eq!(
            outcome.roots, 1,
            "C-008: only the real root is enumerated; the symlinked root and every root beneath \
             the symlinked directory are invisible to the walk"
        );
        assert!(
            outcome.added.is_empty() && outcome.corrected.is_empty() && outcome.removed.is_empty(),
            "C-008: the one real root already agreed with its entry; got {outcome:?}"
        );
        let catalog = catalog_on_disk(&store.source_catalog_path(SOURCE));
        assert_eq!(
            catalog.keys().collect::<Vec<_>>(),
            vec!["kitware/cmake"],
            "C-008: nothing reached through a symlink lands in c/index.json"
        );
    }

    // ── C-007 error table ────────────────────────────────────────────────────

    /// **C-007 Preconditions.** A source whose subtree does not exist is a user
    /// error, not a clean tree. Without the pre-flight, `lock_source`'s
    /// `create_dir_all` would make a mistyped source *create* the directory,
    /// walk nothing, report `roots: 0` and exit **0** — and under C-026 leave a
    /// stray empty directory beside a served checkout, which is why the
    /// non-creation half is asserted too.
    #[tokio::test(flavor = "multi_thread")]
    async fn regenerate_refuses_a_source_whose_subtree_does_not_exist() {
        let home = tempfile::tempdir().unwrap();
        let locks = tempfile::tempdir().unwrap();
        let store = store_at(home.path(), locks.path());
        let source_dir = store.source_config_path("typo.sh").parent().unwrap().to_path_buf();
        assert!(
            !source_dir.exists(),
            "fixture: the subtree must be absent before the run"
        );

        let error = regenerate_catalog(&store, "typo.sh").await.unwrap_err();

        assert!(
            matches!(&error, crate::Error::InternalFile(path, cause)
                if path == &source_dir && cause.kind() == std::io::ErrorKind::NotFound),
            "C-007: an absent subtree is a file_error naming the missing path, got {error:?}"
        );
        assert_eq!(
            error.classify(),
            Some(ExitCode::IoError),
            "C-007: the missing-subtree failure is I/O class"
        );
        assert!(
            !source_dir.exists(),
            "C-007: the refusal must happen before begin_catalog_transaction, or lock_source's \
             create_dir_all leaves a stray empty directory behind"
        );
    }

    /// **C-007 Preconditions — "`source` is contained", asserted rather than
    /// assumed.** `slugify` preserves `.`, so a `source` of `".."` survives it
    /// verbatim; this pins that such a source is refused as a containment
    /// violation and classifies **65**, which nothing asserted before.
    ///
    /// **It does not discriminate the guard's placement, and it was measured
    /// not to.** Removing the `ensure_source_contained` call at the top of
    /// `regenerate_catalog` leaves this test green: `begin_catalog_transaction`
    /// runs the same check, so the same variant surfaces one call later. What
    /// the ordering buys is that no path is *built or stat'd* from an unguarded
    /// name first — without it, `source_config_path` (a pure builder with no
    /// guard) yields `<home>/../config.json` and the existence pre-flight stats
    /// the index home's parent. That is CWE-22 posture, not a behaviour change,
    /// and this test is not evidence for it. Anyone tempted to drop the call
    /// because "the test still passes" has read this correctly and should weigh
    /// the posture, not the assertion.
    #[tokio::test(flavor = "multi_thread")]
    async fn regenerate_refuses_a_source_that_escapes_the_index_home() {
        let home = tempfile::tempdir().unwrap();
        let locks = tempfile::tempdir().unwrap();
        let store = store_at(home.path(), locks.path());

        let error = regenerate_catalog(&store, "..").await.unwrap_err();

        assert!(
            matches!(
                &error,
                crate::Error::FileStructure(crate::file_structure::error::Error::RepositoryEscapesIndexHome { .. })
            ),
            "C-007: an escaping source is refused as a containment violation, not as a missing subtree, got {error:?}"
        );
        assert_eq!(
            error.classify(),
            Some(ExitCode::DataError),
            "a malformed source name is bad input data (65), not an I/O fault"
        );

        // The ordering itself, which the assertions above cannot see. Structural
        // because no input discriminates it: `<home>/..` always exists, so the
        // pre-flight always passes and the transaction raises the identical
        // variant one call later either way.
        // Comment lines dropped: a comment explaining the ordering names both
        // call sites, and one that does would otherwise decide the `find`
        // positions instead of the code.
        let body: String = include_str!("regenerate.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("the module has a non-test half")
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        // Call forms, not bare names: the comment explaining the ordering names
        // `source_config_path` before either call site.
        // Unwrapped, because `Option` orders `None < Some(_)`: comparing the
        // two `find` results directly made the assertion TRUE when the guard was
        // deleted, which is the one thing it exists to catch — and the
        // behavioural half above stays green without the guard too, so nothing
        // held down the single production line this commit added.
        let guard = body
            .find("IndexStore::ensure_source_contained(source)")
            .expect("the containment guard must be present");
        let path_builder = body
            .find("store.source_config_path(")
            .expect("the pre-flight builds its path here");
        assert!(
            guard < path_builder,
            "containment must be checked before a path is built from the unguarded name"
        );
    }

    /// **C-008.** A derivation that found nothing must never replace a catalog
    /// that names packages. `list_wire_repositories` answers `Ok(vec![])` for a
    /// missing `p/` exactly as it does for a tree genuinely holding zero
    /// packages, so a sparse checkout or a store rooted one level off would
    /// otherwise write `{"format_version":1,"packages":{}}` over a live served
    /// catalog and exit **0** — loud in `removed`, silent in the exit code, and
    /// a CI script that checks only the status commits the wipe.
    #[tokio::test(flavor = "multi_thread")]
    async fn regenerate_refuses_to_replace_a_non_empty_catalog_with_an_empty_one() {
        let home = tempfile::tempdir().unwrap();
        let locks = tempfile::tempdir().unwrap();
        let store = store_at(home.path(), locks.path());
        seed_root_with_catalog_entry(&store, "kitware/cmake", &root_bytes("oci://ocx.sh/kitware/cmake")).await;
        let source_dir = store.source_config_path(SOURCE).parent().unwrap().to_path_buf();
        // A sparse checkout: `c/` is materialized, `p/` is not.
        std::fs::remove_dir_all(source_dir.join("p")).unwrap();
        let catalog_before = std::fs::read(store.source_catalog_path(SOURCE)).unwrap();

        let error = regenerate_catalog(&store, SOURCE).await.unwrap_err();

        assert!(
            matches!(&error, crate::Error::InternalFile(path, _) if path == &source_dir.join("p")),
            "C-008: an empty derivation against a non-empty catalog is a hard error naming p/, got {error:?}"
        );
        assert_eq!(
            error.classify(),
            Some(ExitCode::IoError),
            "C-008: the guard must not exit 0 — a CI script checking only the status is what it protects"
        );
        assert_eq!(
            std::fs::read(store.source_catalog_path(SOURCE)).unwrap(),
            catalog_before,
            "C-008: the refusal happens before the commit, so the live catalog is byte-identical"
        );
    }

    /// **C-008.** The guard is about *replacement*, not about emptiness: a tree
    /// that genuinely holds nothing and a catalog that names nothing agree, so
    /// the run succeeds and reports three empty vectors.
    #[tokio::test(flavor = "multi_thread")]
    async fn regenerate_accepts_an_empty_tree_whose_catalog_is_empty_too() {
        let home = tempfile::tempdir().unwrap();
        let locks = tempfile::tempdir().unwrap();
        let store = store_at(home.path(), locks.path());
        std::fs::create_dir_all(store.source_config_path(SOURCE).parent().unwrap()).unwrap();

        let outcome = regenerate_catalog(&store, SOURCE).await.unwrap();

        assert_eq!(
            outcome.roots, 0,
            "C-008: the walk found nothing, and nothing is correct here"
        );
        assert!(
            outcome.added.is_empty() && outcome.corrected.is_empty() && outcome.removed.is_empty(),
            "C-008: nothing to add, correct or remove; got {outcome:?}"
        );
        assert!(
            !store.source_catalog_path(SOURCE).exists(),
            "C-008: commit writes nothing when the derived map equals the one read"
        );
    }

    /// **C-007 failure table.** A root under `p/` that does not parse is
    /// `Error::MalformedRootDocument`, exit **65** — a hard error, never a
    /// silently skipped package (which wholesale replacement would turn into a
    /// removal from the catalog).
    #[tokio::test(flavor = "multi_thread")]
    async fn regenerate_refuses_an_unparseable_root_document_as_data_error() {
        let home = tempfile::tempdir().unwrap();
        let locks = tempfile::tempdir().unwrap();
        let store = store_at(home.path(), locks.path());
        let target = store.root_document_path(SOURCE, "kitware/cmake");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, b"{ this is not a root document").unwrap();

        let outcome = regenerate_catalog(&store, SOURCE).await;

        assert!(
            matches!(
                outcome,
                Err(crate::Error::FileStructure(
                    crate::file_structure::error::Error::MalformedRootDocument { .. }
                ))
            ),
            "C-007: an unparseable root under p/ is MalformedRootDocument, got {outcome:?}"
        );
        assert_eq!(
            outcome.unwrap_err().classify(),
            Some(ExitCode::DataError),
            "C-007: MalformedRootDocument classifies as exit 65"
        );
    }
}
