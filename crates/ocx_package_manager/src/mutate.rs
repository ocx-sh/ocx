// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Commit-then-render: the one orchestration `ocx add`, `ocx remove`, `ocx lock`
//! and `ocx update` share, plus the render half `ocx pull` shares with them.
//!
//! A render failure never rolls the commit back: the lock is the declaration,
//! the tree only its materialisation.

use ocx_project::{DEFAULT_GROUP, MutationCommit, MutationGuard, ProjectLock, StagedMutation};
use ocx_store::file_structure::{RenderStampScope, ToolchainHome};

use super::PackageManager;
use super::tasks::render_toolchain::{RenderOutcome, RenderReport, RenderRequest, RenderedArtifact};

/// Where a render writes and for which platform — the three inputs
/// [`PackageManager::render_home`] cannot derive from a lock.
// `groups`, `pinned` and `dry_run` stay parameters: the two callers disagree on all three.
pub struct ToolchainRender<'a> {
    /// Which tier's tree this render targets, and — for a project — the
    /// **canonical** project directory (parity with the consent key).
    pub scope: &'a RenderStampScope,

    /// The validated `toolchain_dir` root, or `None` for the in-project
    /// `<project>/.ocx/toolchain` default; ignored for [`RenderStampScope::Global`].
    pub toolchain_root: Option<&'a ocx_config::ToolchainRoot>,

    /// The platform whose leaf each locked tool resolves to.
    pub platform: &'a ocx_oci::Platform,
}

/// What the four mutation commands report after [`PackageManager::commit_and_render`].
pub struct MutationOutcome {
    /// The paths the commit rewrote — `ocx.toml` and `ocx.lock`.
    pub commit: MutationCommit,

    /// What the render did, or `None` when the surface could not be resolved
    /// (e.g. `--frozen`/`--offline` with metadata not local) — not an error; the
    /// exit code stays 0 and the next `ocx pull` renders.
    pub render: Option<RenderReport>,
}

/// One warning line per [`RenderOutcome::Skipped`] entry in `report`, the single
/// CWE-117-safe rendering of a render skip.
// Names are `read_dir` results a hostile clone controls: interpolate each with `{:?}`,
// or a `\n` or ANSI sequence forges output lines.
#[must_use]
pub fn skipped_render_warnings(report: &RenderReport) -> Vec<String> {
    report
        .items
        .iter()
        .filter_map(|item| match &item.outcome {
            RenderOutcome::Skipped { path, reason } => Some(format!(
                "Toolchain render skipped {} at {path:?}: {reason:?}",
                describe_artifact(&item.artifact)
            )),
            RenderOutcome::Written | RenderOutcome::Unchanged | RenderOutcome::Pruned => None,
        })
        .collect()
}

/// One artifact, named for a warn line — **every name component escaped**.
// `{:?}` per name, not on the whole sentence, so the surrounding words stay legible.
fn describe_artifact(artifact: &RenderedArtifact) -> String {
    match artifact {
        RenderedArtifact::Trampoline(name) => format!("trampoline {name:?}"),
        RenderedArtifact::Link { group, entry } => format!("link {entry:?} in group {group:?}"),
        RenderedArtifact::GroupDirectory(group) => format!("group directory {group:?}"),
        // Never "group directory": a depth-1 name is no group, and calling it one sends
        // the user hunting a nonexistent `ocx.toml` table.
        RenderedArtifact::RootEntry(name) => format!("home-root entry {name:?}"),
    }
}

impl PackageManager {
    /// The home a render targets, for either tier: `$OCX_HOME/toolchain` for
    /// [`RenderStampScope::Global`], else
    /// [`resolve_toolchain_home`](ocx_project::resolve_toolchain_home) over the
    /// canonical project directory.
    ///
    /// # Errors
    ///
    /// The canonicalisation's own I/O failure, with the offending path attached.
    pub fn toolchain_home(
        &self,
        scope: &RenderStampScope,
        toolchain_root: Option<&ocx_config::ToolchainRoot>,
    ) -> crate::Result<ToolchainHome> {
        match scope {
            // Never consult `toolchain_root`: a global tree under a project-keyed root has no key.
            RenderStampScope::Global => Ok(ToolchainHome::new(self.file_structure().toolchain.root().to_path_buf())),
            RenderStampScope::Project(project_directory) => {
                ocx_project::resolve_toolchain_home(project_directory, toolchain_root).map_err(Into::into)
            }
        }
    }

    /// Commit a staged `ocx.toml` + `ocx.lock` mutation, then re-render the
    /// toolchain home it describes. Calling [`MutationGuard::commit`] directly
    /// instead leaves the rendered tree describing the previous lock.
    ///
    /// # Errors
    ///
    /// - The commit's own failures propagate unchanged, with nothing written.
    /// - A render failure after a successful commit degrades to
    ///   [`MutationOutcome::render`] `= None`.
    pub async fn commit_and_render(
        &self,
        guard: MutationGuard,
        staged: StagedMutation,
        new_lock: ProjectLock,
        render: ToolchainRender<'_>,
    ) -> crate::Result<MutationOutcome> {
        // From the staged manifest, not the one on disk, or `pinned` reflects the pre-mutation composition.
        let pinned = super::pinned_for_project(None, staged.config());

        // Default group always in scope, or removing its last tool leaves the dropped trampoline behind.
        let mut groups: Vec<String> = vec![DEFAULT_GROUP.to_owned()];
        for tool in &new_lock.tools {
            if !groups.iter().any(|group| group == &tool.group) {
                groups.push(tool.group.clone());
            }
        }

        // Commit before render: the reverse order leaves a tree for a lock never written.
        let commit = guard.commit(staged, new_lock.clone()).await?;

        let render = match self.render_home(&new_lock, pinned, &render, &groups, false).await {
            Ok(report) => {
                for line in skipped_render_warnings(&report) {
                    log::warn!("{line}");
                }
                Some(report)
            }
            // Debug, not warn: `--no-pull` renders offline, where a cold store is the expected state.
            Err(error) if self.is_offline() => {
                log::debug!("The toolchain home was not re-rendered: {error}");
                None
            }
            Err(error) => {
                log::warn!("The toolchain home was not re-rendered after the mutation: {error}");
                None
            }
        };

        Ok(MutationOutcome { commit, render })
    }

    /// Render `<home>/toolchain/` as a whole-compose pass over `groups`; the
    /// network-reaching `bin/` closure walk runs only when `groups` holds the
    /// default group, otherwise `bin/` is left untouched.
    ///
    /// # Errors
    ///
    /// The home's canonicalisation failure, or a surface resolution that could
    /// not complete. A per-entry write failure is a [`RenderOutcome::Skipped`]
    /// in the report instead; render it through [`skipped_render_warnings`].
    // The one `RenderRequest` producer: `surface` must be the default group's closure, which its type cannot enforce.
    pub async fn render_home(
        &self,
        lock: &ProjectLock,
        pinned: bool,
        render: &ToolchainRender<'_>,
        groups: &[String],
        dry_run: bool,
    ) -> Result<RenderReport, crate::error::PackageErrorKind> {
        use crate::error::PackageErrorKind;

        let home = self
            .toolchain_home(render.scope, render.toolchain_root)
            .map_err(PackageErrorKind::Internal)?;

        let surface = if groups.iter().any(|group| group == DEFAULT_GROUP) {
            let roots = default_group_roots(lock, render.platform);
            self.toolchain_surface(&roots, render.platform).await?
        } else {
            Vec::new()
        };

        self.render_toolchain(RenderRequest {
            home: &home,
            scope: render.scope,
            lock,
            surface: &surface,
            groups,
            pinned,
            platform: render.platform,
            // Passed through, never short-circuited here, or a dry run reports a delta of zero.
            dry_run,
        })
        .await
    }
}

/// The **default group's** root identifiers, deduplicated; a tool with no leaf
/// for this platform is skipped, not refused.
// Refusing would make one tool without a build here cost every other trampoline.
#[must_use]
pub fn default_group_roots(lock: &ProjectLock, platform: &ocx_oci::Platform) -> Vec<ocx_oci::PackageRef> {
    let mut roots: Vec<ocx_oci::PackageRef> = Vec::new();
    for tool in lock.tools.iter().filter(|tool| tool.group == DEFAULT_GROUP) {
        match tool.host_leaf(platform) {
            Ok(leaf) => {
                let identifier = ocx_oci::PackageRef::from(tool.repository.pin_untagged(leaf));
                if !roots.contains(&identifier) {
                    roots.push(identifier);
                }
            }
            Err(error) => log::debug!(
                "Tool '{}' contributes no trampoline on this platform: {error}",
                tool.name
            ),
        }
    }
    roots
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;

    use super::*;
    use crate::tasks::render_toolchain::{RenderOutcome, RenderedArtifact, RenderedItem};
    use ocx_config::ToolchainRoot;
    use ocx_index::{ChainMode, Index, LocalConfig, LocalIndex};
    use ocx_project::{
        DECLARATION_HASH_VERSION, DEFAULT_GROUP, LockMetadata, LockVersion, LockedTool, ManifestSnapshot,
        ProjectConfig, acquire_project_lock,
    };
    use ocx_store::file_structure::FileStructure;
    use ocx_store::reference_manager::ReferenceManager;

    const REGISTRY: &str = "example.com";
    const PLATFORM_KEY: &str = "linux/amd64";

    fn platform() -> ocx_oci::Platform {
        PLATFORM_KEY.parse().expect("the fixture platform key is canonical")
    }

    // ── A "wrote nothing" oracle ─────────────────────────────────────────────

    /// One directory entry, identified by everything a write would disturb.
    ///
    /// Deliberately **not** "the file I looked at is unchanged" and not an
    /// empty-output check: both are silent negatives. A rewrite that lands the
    /// same bytes still moves `mtime_nsec`; a replace-by-rename still moves the
    /// inode; a new sibling still appears in the map's key set.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Entry {
        is_dir: bool,
        is_symlink: bool,
        len: u64,
        mtime_nsec: i64,
        inode: u64,
    }

    /// A recursive `(path, file type, bytes, mtime_nsec, inode)` snapshot of a
    /// subtree. An absent root snapshots as the empty map, which is the state a
    /// render that created nothing leaves.
    fn snapshot(root: &Path) -> BTreeMap<PathBuf, Entry> {
        let mut out = BTreeMap::new();
        collect(root, root, &mut out);
        out
    }

    fn collect(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Entry>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries {
            let entry = entry.expect("a readable directory entry");
            let path = entry.path();
            // `symlink_metadata`: a snapshot must record the link itself, not
            // whatever it currently points at — a repointed link is a write.
            let meta = std::fs::symlink_metadata(&path).expect("a stat-able entry");
            let relative = path
                .strip_prefix(root)
                .expect("every collected path is under the root")
                .to_path_buf();
            out.insert(
                relative,
                Entry {
                    is_dir: meta.is_dir(),
                    is_symlink: meta.file_type().is_symlink(),
                    len: meta.len(),
                    mtime_nsec: mtime_nsec(&meta),
                    inode: inode(&meta),
                },
            );
            if meta.is_dir() && !meta.file_type().is_symlink() {
                collect(root, &path, out);
            }
        }
    }

    fn mtime_nsec(meta: &std::fs::Metadata) -> i64 {
        meta.modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
    }

    #[cfg(unix)]
    fn inode(meta: &std::fs::Metadata) -> u64 {
        std::os::unix::fs::MetadataExt::ino(meta)
    }

    #[cfg(not(unix))]
    fn inode(_meta: &std::fs::Metadata) -> u64 {
        0
    }

    // ── The tree ─────────────────────────────────────────────────────────────

    /// One `$OCX_HOME` plus one project directory under a single tempdir. No
    /// ambient home, no registry, no client.
    struct Tree {
        _tmp: TempDir,
        file_structure: FileStructure,
        ocx_home: PathBuf,
        project_dir: PathBuf,
    }

    impl Tree {
        fn new() -> Self {
            let tmp = tempfile::tempdir().expect("a tempdir is creatable");
            let ocx_home = tmp.path().join("ocx-home");
            let file_structure = FileStructure::with_root(ocx_home.clone());
            let project_dir = tmp.path().join("proj");
            std::fs::create_dir_all(&project_dir).expect("the project directory is creatable");
            Self {
                _tmp: tmp,
                file_structure,
                ocx_home,
                project_dir,
            }
        }

        /// The canonical project directory — `resolve_toolchain_home`'s own
        /// precondition, and the value `RenderStampScope::Project` carries.
        /// `tempfile` hands back a path under `/tmp`, itself a symlink
        /// on macOS, so a non-canonical spelling here would hash to a second
        /// project key.
        fn canonical_project_dir(&self) -> PathBuf {
            dunce::canonicalize(&self.project_dir).expect("the project directory canonicalises")
        }

        fn scope(&self) -> RenderStampScope {
            RenderStampScope::Project(self.canonical_project_dir())
        }

        /// An offline manager over this tree's store — no client, no sources.
        fn manager(&self) -> PackageManager {
            let index = Index::from_chained(
                LocalIndex::new(LocalConfig {
                    index_store: ocx_index::IndexStore::machine_local(&self.file_structure),
                }),
                Vec::new(),
                ChainMode::Offline,
            );
            PackageManager::new(self.file_structure.clone(), index, None, REGISTRY)
        }

        fn config_path(&self) -> PathBuf {
            self.project_dir.join("ocx.toml")
        }

        fn lock_path(&self) -> PathBuf {
            self.project_dir.join("ocx.lock")
        }

        /// A stale trampoline in the directory the renderer actually writes and
        /// prunes — the **physical** `<home>/shells/default/bin`.
        ///
        /// Through the accessor, never a literal join: a fixture that spelled
        /// the tree itself would keep seeding the old place after a layout
        /// move and the prune assertions would pass over a file the renderer
        /// never looked at.
        fn stale_trampoline(&self) -> PathBuf {
            ocx_store::file_structure::ToolchainHome::new(self.project_dir.join(".ocx").join("toolchain"))
                .shell_bin(ocx_store::file_structure::DEFAULT_SHELL)
                .join("stale")
        }

        /// A guard over `ocx.toml` holding `text`, plus the staged candidate and
        /// a lock whose `declaration_hash` agrees with it.
        ///
        /// The hash agreement is `MutationGuard::commit`'s own coherence gate:
        /// a mismatch is refused before anything is written, which is what the
        /// refusal case below deliberately triggers.
        async fn staged(&self, text: &str, tools: Vec<LockedTool>) -> (MutationGuard, StagedMutation, ProjectLock) {
            std::fs::write(self.config_path(), text).expect("the project manifest is writable");
            let config = ProjectConfig::from_toml_str(text).expect("the fixture manifest parses");
            let mutate_lock = acquire_project_lock(&self.project_dir, &self.file_structure.locks)
                .await
                .expect("an uncontended project lock is acquirable");
            let guard = MutationGuard::from_parts(
                mutate_lock,
                self.config_path(),
                self.lock_path(),
                self.ocx_home.clone(),
                ManifestSnapshot {
                    config,
                    text: text.to_owned(),
                },
                None,
                None,
            );
            let staged = guard.stage(|_| Ok(())).expect("an identity staging closure succeeds");
            let hash = staged_hash(&guard, text);
            (guard, staged, lock_of(hash, tools))
        }
    }

    /// The declaration hash the staged candidate carries. An identity staging
    /// closure leaves the candidate equal to the parsed manifest, so the hash is
    /// the manifest's own.
    fn staged_hash(_guard: &MutationGuard, text: &str) -> String {
        ProjectConfig::from_toml_str(text)
            .expect("the fixture manifest parses")
            .declaration_hash_cached()
            .to_owned()
    }

    /// A unique marker written into the lock's `generated_by`, so "the lock on
    /// disk is the *new* one" is decidable from the file's own bytes rather than
    /// from its mtime.
    const NEW_LOCK_MARKER: &str = "wp-8 specification fixture";

    fn lock_of(declaration_hash: String, tools: Vec<LockedTool>) -> ProjectLock {
        ProjectLock {
            metadata: LockMetadata {
                lock_version: LockVersion::V3,
                declaration_hash_version: DECLARATION_HASH_VERSION,
                declaration_hash,
                generated_by: NEW_LOCK_MARKER.to_owned(),
                generated_at: "2026-01-01T00:00:00Z".to_owned(),
            },
            tools,
        }
    }

    fn locked_tool(name: &str, group: &str, repository: &str) -> LockedTool {
        LockedTool {
            name: name.to_owned(),
            group: group.to_owned(),
            repository: ocx_oci::Repository::new(REGISTRY, repository),
            platforms: [(PLATFORM_KEY.to_owned(), ocx_oci::Digest::Sha256("a".repeat(64)))]
                .into_iter()
                .collect::<std::collections::BTreeMap<String, ocx_oci::Digest>>(),
        }
    }

    fn render_over<'a>(scope: &'a RenderStampScope, platform: &'a ocx_oci::Platform) -> ToolchainRender<'a> {
        ToolchainRender {
            scope,
            toolchain_root: None,
            platform,
        }
    }

    // ── Commit, then render ────────────────────────

    /// The sequence is **commit then render**, and the render is
    /// reached: a mutation whose new lock declares no tools still reconciles
    /// `bin/`, so a stale trampoline left by the removed tool is pruned.
    ///
    /// Two claims in one, and both are load-bearing:
    ///
    /// - The commit happened (`ocx.lock` on disk carries the new lock's
    ///   marker).
    /// - The render happened *after* it and covered `bin/`
    ///   (`bin_in_scope == true`, and the stale entry is gone).
    ///
    /// Mutation that reds it: a `commit_and_render` that only calls
    /// `guard.commit(...)` — the four-copies-of-a-two-step-sequence defect the shared function
    /// exists to prevent — leaves `render == None` and `bin/stale` on disk.
    // Multi-thread flavour: `MutationGuard::commit` fans its two publishes
    // (`ocx.lock`, then `ocx.toml`) onto the blocking pool.
    #[tokio::test(flavor = "multi_thread")]
    async fn commit_and_render_reconciles_bin_after_the_commit_lands() {
        let tree = Tree::new();
        let stale = tree.stale_trampoline();
        std::fs::create_dir_all(stale.parent().expect("bin has a parent")).expect("bin/ is creatable");
        std::fs::write(&stale, b"#!/bin/sh\n").expect("the stale trampoline is writable");

        let (guard, staged, new_lock) = tree.staged("", Vec::new()).await;
        let (scope, platform) = (tree.scope(), platform());
        let outcome = tree
            .manager()
            .commit_and_render(guard, staged, new_lock, render_over(&scope, &platform))
            .await
            .expect("a mutation whose render succeeds is Ok");

        assert_eq!(
            outcome.commit.lock_path,
            tree.lock_path(),
            "the outcome must name the lock the commit rewrote"
        );
        let written = std::fs::read_to_string(tree.lock_path()).expect("the committed lock is readable");
        assert!(
            written.contains(NEW_LOCK_MARKER),
            "the commit must have landed the new lock before the render ran; got:\n{written}"
        );

        let report = outcome.render.expect("the render must run after a successful commit");
        assert!(
            report.bin_in_scope,
            "C-054 — a mutation re-renders the whole home, so `bin/` is always in scope"
        );
        assert!(
            !stale.exists(),
            "S-008 — a trampoline the new lock no longer declares must be pruned by the re-render"
        );
    }

    /// `-g` on `ocx lock` / `ocx update` scopes **resolution**, and the
    /// re-render is whole-home.
    ///
    /// Expressed as the type contract it is: [`ToolchainRender`] carries no
    /// group set, so no caller can narrow the tree, and `bin/` is reconciled
    /// whatever groups the invocation named. This case pairs with the one above
    /// — there the lock declares nothing, here it declares a non-default group
    /// only, which is the input a narrowing implementation would answer
    /// differently for.
    ///
    /// Mutation that reds it: deriving the render's `groups` from the lock's
    /// declared groups (`["ci"]` here) instead of passing the whole home leaves
    /// `bin_in_scope == false` and the stale entry in place.
    // Multi-thread flavour: `MutationGuard::commit` fans its two publishes
    // (`ocx.lock`, then `ocx.toml`) onto the blocking pool.
    #[tokio::test(flavor = "multi_thread")]
    async fn commit_and_render_covers_bin_even_when_the_lock_declares_no_default_group() {
        let tree = Tree::new();
        let stale = tree.stale_trampoline();
        std::fs::create_dir_all(stale.parent().expect("bin has a parent")).expect("bin/ is creatable");
        std::fs::write(&stale, b"#!/bin/sh\n").expect("the stale trampoline is writable");

        let manifest = "[group.ci.tools]\nlinter = \"example.com/linter:1.0.0\"\n";
        let (guard, staged, new_lock) = tree.staged(manifest, vec![locked_tool("linter", "ci", "linter")]).await;
        assert_ne!(
            new_lock.tools[0].group, DEFAULT_GROUP,
            "the fixture must declare a non-default group, or this case cannot discriminate"
        );

        let (scope, platform) = (tree.scope(), platform());
        let outcome = tree
            .manager()
            .commit_and_render(guard, staged, new_lock, render_over(&scope, &platform))
            .await
            .expect("a mutation whose render succeeds is Ok");

        let written = std::fs::read_to_string(tree.lock_path()).expect("the committed lock is readable");
        assert!(written.contains(NEW_LOCK_MARKER), "the commit must have landed");
        let report = outcome
            .render
            .expect("the default group's surface is empty, not unresolvable, so the render runs");
        assert!(
            report.bin_in_scope,
            "RUL-59 — the re-render is whole-home; a lock declaring only `ci` still reconciles `bin/`"
        );
        assert!(
            !stale.exists(),
            "RUL-59 — `bin/` is reconciled even though no selected group is the default one"
        );
    }

    /// A render that cannot run **never rolls the commit back**.
    ///
    /// The lock declares a tool whose package is in no index this offline
    /// manager can reach, so the surface walk fails. The contract is: `Ok`, a
    /// `render` of `None`, and the new lock still on disk.
    ///
    /// Mutation that reds it: propagating the render's error out of
    /// `commit_and_render` (the obvious `?`), or restoring the predecessor lock
    /// on a render failure.
    // Multi-thread flavour: `MutationGuard::commit` fans its two publishes
    // (`ocx.lock`, then `ocx.toml`) onto the blocking pool.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_render_that_cannot_resolve_leaves_the_committed_lock_in_place() {
        let tree = Tree::new();
        let manifest = "[tools]\nunreachable = \"example.com/unreachable:1.0.0\"\n";
        let (guard, staged, new_lock) = tree
            .staged(manifest, vec![locked_tool("unreachable", DEFAULT_GROUP, "unreachable")])
            .await;

        let (scope, platform) = (tree.scope(), platform());
        let outcome = tree
            .manager()
            .commit_and_render(guard, staged, new_lock, render_over(&scope, &platform))
            .await
            .expect("RUL-53 — a render failure after a successful commit is not an error");

        assert!(
            outcome.render.is_none(),
            "an unresolvable surface must report `render: None`, not a fabricated empty report"
        );
        let written = std::fs::read_to_string(tree.lock_path()).expect("the committed lock is readable");
        assert!(
            written.contains(NEW_LOCK_MARKER),
            "RUL-53 — the lock stays written; a render skip is never a rollback"
        );
    }

    /// The commit's own refusal propagates, and **nothing is written** — the
    /// counterweight to never rolling back, so "never roll back" is not read as "always
    /// write something".
    ///
    /// The input is `MutationGuard::commit`'s coherence gate: a lock whose
    /// `declaration_hash` does not describe the staged manifest. "Wrote nothing"
    /// is the whole project subtree, snapshotted by `(path, type, bytes,
    /// mtime_nsec, inode)`, not an inspection of the one file the case is about.
    #[tokio::test]
    async fn a_refused_commit_writes_nothing_and_never_renders() {
        let tree = Tree::new();
        let manifest = "[tools]\na = \"example.com/a:1.0.0\"\n";
        let (guard, staged, mut new_lock) = tree.staged(manifest, Vec::new()).await;
        new_lock.metadata.declaration_hash = "0".repeat(64);

        let before = snapshot(&tree.project_dir);
        let (scope, platform) = (tree.scope(), platform());
        // `is_err()` rather than `expect_err`: `MutationOutcome` is not `Debug`,
        // and the success value is not what this case is about.
        let refused = tree
            .manager()
            .commit_and_render(guard, staged, new_lock, render_over(&scope, &platform))
            .await
            .is_err();
        assert!(
            refused,
            "a lock that does not describe the staged manifest must be refused"
        );

        assert_eq!(
            snapshot(&tree.project_dir),
            before,
            "a refused commit must leave the project subtree byte-, inode- and mtime-identical, \
             and must not have rendered a toolchain home for a lock that was never written"
        );
    }

    // ── The home derivation ──────────────────────────────────────────

    /// The **global** home is `$OCX_HOME/toolchain` and ignores
    /// `toolchain_dir` entirely.
    ///
    /// The discriminating input: a `toolchain_root` is supplied and must make no
    /// difference. An implementation that honoured it for both tiers passes
    /// every project case below and reds only here.
    #[test]
    fn the_global_home_ignores_a_configured_toolchain_root() {
        let tree = Tree::new();
        let relocated = tree.ocx_home.join("elsewhere");
        let root = ToolchainRoot::from_validated(&relocated);

        let with_root = tree
            .manager()
            .toolchain_home(&RenderStampScope::Global, Some(&root))
            .expect("the global home resolves");
        let without_root = tree
            .manager()
            .toolchain_home(&RenderStampScope::Global, None)
            .expect("the global home resolves");

        assert_eq!(
            with_root.root(),
            tree.file_structure.toolchain.root(),
            "C-016 — the global home is `$OCX_HOME/toolchain`, whatever `toolchain_dir` says"
        );
        assert_eq!(
            with_root, without_root,
            "C-016 — a configured root must make no difference to the global tier"
        );
    }

    /// With no `toolchain_dir`, a project's home is the in-project
    /// `<project>/.ocx/toolchain`.
    #[test]
    fn a_project_home_defaults_to_the_in_project_tree() {
        let tree = Tree::new();
        let home = tree
            .manager()
            .toolchain_home(&tree.scope(), None)
            .expect("the in-project home resolves");
        assert_eq!(
            home.root(),
            tree.canonical_project_dir().join(".ocx").join("toolchain"),
            "C-002 — the default home is `<project>/.ocx/toolchain`"
        );
    }

    /// A configured `toolchain_dir` relocates a project's home
    /// to `<root>/<project-key>/toolchain`, keyed by the same 16-hex derivation
    /// the consent stamp uses.
    ///
    /// Mutation that reds it: `<root>/toolchain/<key>` — the reversed order
    /// that is the data-loss path, since it puts every project's tree
    /// inside a directory indistinguishable from a group directory.
    #[test]
    fn a_configured_root_relocates_a_project_home_under_its_project_key() {
        let tree = Tree::new();
        let relocated = tree.ocx_home.join("toolchains");
        let root = ToolchainRoot::from_validated(&relocated);

        let home = tree
            .manager()
            .toolchain_home(&tree.scope(), Some(&root))
            .expect("the relocated home resolves");
        assert_eq!(
            home.root(),
            relocated
                .join(ReferenceManager::name_for_path(&tree.canonical_project_dir()))
                .join("toolchain"),
            "C-002 — `<root>/<project-key>/toolchain`, key first and `toolchain` second"
        );
    }

    // ── The skip warn line escapes its untrusted name (CWE-117) ────

    fn report_of(items: Vec<RenderedItem>) -> RenderReport {
        RenderReport {
            items,
            bin_in_scope: true,
            stamp_written: false,
        }
    }

    fn skipped(artifact: RenderedArtifact, path: &str, reason: &str) -> RenderedItem {
        RenderedItem {
            artifact,
            outcome: RenderOutcome::Skipped {
                path: PathBuf::from(path),
                reason: reason.to_owned(),
            },
        }
    }

    fn written(artifact: RenderedArtifact) -> RenderedItem {
        RenderedItem {
            artifact,
            outcome: RenderOutcome::Written,
        }
    }

    /// Only skips warn. A report of successes produces no lines at all,
    /// so a clean render is silent.
    #[test]
    fn a_report_without_skips_produces_no_warning_lines() {
        let report = report_of(vec![
            written(RenderedArtifact::Trampoline("cmake".to_owned())),
            RenderedItem {
                artifact: RenderedArtifact::Trampoline("ninja".to_owned()),
                outcome: RenderOutcome::Unchanged,
            },
            RenderedItem {
                artifact: RenderedArtifact::GroupDirectory("ci".to_owned()),
                outcome: RenderOutcome::Pruned,
            },
        ]);
        assert!(
            skipped_render_warnings(&report).is_empty(),
            "C-050 — Written, Unchanged and Pruned are not warnings"
        );
    }

    /// One line per skipped entry, each naming the path it could not
    /// write, across all three artifact kinds.
    #[test]
    fn every_skipped_artifact_kind_produces_one_line_naming_its_path() {
        let report = report_of(vec![
            skipped(
                RenderedArtifact::Trampoline("cmake".to_owned()),
                "/w/proj/.ocx/toolchain/bin/cmake",
                "Permission denied (os error 13)",
            ),
            skipped(
                RenderedArtifact::Link {
                    group: "ci".to_owned(),
                    entry: "linter".to_owned(),
                },
                "/w/proj/.ocx/toolchain/ci/linter",
                "Read-only file system (os error 30)",
            ),
            skipped(
                RenderedArtifact::GroupDirectory("lint".to_owned()),
                "/w/proj/.ocx/toolchain/lint",
                "Directory not empty (os error 39)",
            ),
            written(RenderedArtifact::Trampoline("ninja".to_owned())),
        ]);

        let lines = skipped_render_warnings(&report);
        assert_eq!(
            lines.len(),
            3,
            "one line per skipped entry, and none for the written one"
        );
        for (line, path) in lines.iter().zip([
            "/w/proj/.ocx/toolchain/bin/cmake",
            "/w/proj/.ocx/toolchain/ci/linter",
            "/w/proj/.ocx/toolchain/lint",
        ]) {
            assert!(
                line.contains(path),
                "the warn line must name the path it could not write: {line:?}"
            );
            assert!(!line.is_empty(), "an empty warn line tells a user nothing");
        }
        assert!(
            lines[0].contains("Permission denied"),
            "the underlying failure's own words carry the diagnosis: {:?}",
            lines[0]
        );
    }

    /// **CWE-117** — every untrusted component is interpolated
    /// with `{:?}`, so a `read_dir` name a hostile clone controls cannot forge a
    /// line in the consumer's output.
    ///
    /// The names here are exactly what a committed `bin/` entry or `<group>/`
    /// directory can be called on POSIX: any byte but `/` and NUL, newlines and
    /// ANSI introducers included.
    ///
    /// Mutation that reds it: `format!("could not write {name}: {reason}")` —
    /// the bare interpolation — turns each of these into two or three lines,
    /// the second of which reads as ocx's own output.
    #[test]
    fn a_hostile_artifact_name_cannot_forge_a_warning_line() {
        let hostile_trampoline = "cmake\nwarning: ocx: everything is fine";
        let hostile_group = "ci\r\x1b[2Klint";
        let hostile_reason = "denied\nwarning: ocx: nothing was skipped";
        let report = report_of(vec![
            skipped(
                RenderedArtifact::Trampoline(hostile_trampoline.to_owned()),
                "/w/proj/.ocx/toolchain/bin/x",
                "Permission denied",
            ),
            skipped(
                RenderedArtifact::GroupDirectory(hostile_group.to_owned()),
                "/w/proj/.ocx/toolchain/y",
                hostile_reason,
            ),
            skipped(
                RenderedArtifact::Link {
                    group: hostile_group.to_owned(),
                    entry: hostile_trampoline.to_owned(),
                },
                "/w/proj/.ocx/toolchain/z",
                "Permission denied",
            ),
        ]);

        let lines = skipped_render_warnings(&report);
        assert_eq!(
            lines.len(),
            3,
            "three skips, three lines — one per entry, whatever they are named"
        );
        for line in &lines {
            assert!(
                !line.contains('\n') && !line.contains('\r'),
                "CWE-117 — a warn line must not carry a raw newline or carriage return: {line:?}"
            );
            assert!(
                !line.contains('\x1b'),
                "CWE-117 — a warn line must not carry a raw ANSI introducer: {line:?}"
            );
        }
        assert!(
            lines[0].contains("\\n"),
            "the hostile newline must survive as an *escaped* sequence, not be stripped — \
             a dropped byte is a different defect from a forged line: {:?}",
            lines[0]
        );
        // Positive control on the same formatter: an ordinary name is still
        // legible, so the escaping above is not "every line is unreadable".
        let benign = skipped_render_warnings(&report_of(vec![skipped(
            RenderedArtifact::Trampoline("cmake".to_owned()),
            "/w/proj/.ocx/toolchain/bin/cmake",
            "Permission denied",
        )]));
        assert!(
            benign[0].contains("cmake"),
            "an ordinary name must still appear verbatim: {:?}",
            benign[0]
        );
    }
}
