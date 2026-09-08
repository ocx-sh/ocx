// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Rendering a toolchain home — trampolines and `links/<group>/<entry>`
//! links (plan contracts C-044…C-053, C-070…C-084,
//! `plan_toolchain_activation.md`).
//!
//! ```text
//! <home>/
//! ├── .gitignore                    "*" — C-004, owned by `ToolchainHome`
//! ├── active -> shells/default      the depth-1 link every PATH route resolves through (C-078)
//! ├── links/<group>/<entry>/        directory link to a package root, one per selected group
//! └── shells/<shell>/bin/<name>     one launcher trampoline per exposed name, DEFAULT group only
//!                                    (Windows: `<name>.exe` plus its `<name>.exec` sidecar — two entries)
//! ```
//!
//! Depth 1 is **closed and tree-owned** (C-071): the four names above are the
//! whole set, every user-supplied name lives one level below `links/`, and the
//! orphan scan at the root is therefore a closed-set comparison rather than a
//! lock-derived one (C-074). `bin/` is reached two ways and they are not
//! interchangeable — `ToolchainHome::bin()` is the **PATH-facing**
//! `<root>/active/bin` and `shell_bin()` is the **physical**
//! `<root>/shells/<shell>/bin`. Every write, prune, fingerprint and guard in
//! this module uses the physical one, so none of them can be redirected by a
//! repointed `active` (C-080).
//!
//! # The order one render runs in
//!
//! Fixed, and each step's position is forced by the one below it:
//!
//! 1. **A dry run returns here** (C-049). Every step below writes, so a dry run
//!    performs none of them — see [`RenderRequest::dry_run`] for the explicit
//!    list of what it therefore never creates.
//! 2. [`refuse_symlinked_home`] then [`ensure_home_root`], **before the
//!    case-fold probe** and before every other step. The probe writes *into*
//!    the home, so probing first made a first-ever `ocx pull` find no home,
//!    fail the probe, and route itself to C-050's skip — a fresh home would
//!    never render (RUL-37). This is also where a symlinked component above the
//!    home root (RUL-44), and a symlink at the home root or at any of the
//!    tree's own depth-1 directories (RUL-33, C-074), are refused.
//! 3. [`ToolchainHome::ensure_gitignore`](crate::file_structure::ToolchainHome::ensure_gitignore)
//!    — **the renderer calls it, and after step 2, never before.** C-004 has no
//!    other home: that function has no production caller anywhere else in the
//!    crate, so if the render does not call it the contract is unimplemented.
//!    The ordering is not cosmetic either — it does its own `create_dir_all`
//!    with the **ambient umask**, so reaching it with the root still absent
//!    would create that root with whatever the umask allows and silently defeat
//!    R-W19(a)'s owner-only-write on a fresh `toolchain-dir` root.
//! 4. **One [`lock_scoped`](crate::utility::fs::lock_scoped) over the home, for
//!    the whole body** (RUL-30). Two `ocx pull` runs against one home is an
//!    ordinary state — two terminals — and unlocked they interleave writes and
//!    prunes and then race the stamp into a C-061 mismatch that no later render
//!    clears, because each run's stamp describes a tree the other was still
//!    editing. It sits after step 2 for a second reason: `lock_scoped` keys on
//!    the guarded directory's file identity and documents that the directory
//!    must already exist. **A lock timeout is C-050's skip, never an error** —
//!    C-050's own trigger list names "lock timeout".
//! 5. The case-fold probe ([`filesystem_is_case_insensitive`]).
//! 6. [`ensure_shell_tree`] then [`heal_active`], **in that order** (C-083).
//!    The shell directory first, because `active` must never be published as a
//!    link to a directory that does not exist: a render interrupted between
//!    the two leaves `active` absent, which is a lookup *miss* and never a
//!    wrong answer, whereas the reverse order leaves a link the gate follows
//!    into nothing. Both sit inside step 4's lock and after step 2, which is
//!    what C-081 requires of the heal.
//! 7. [`render_with`] — which performs steps 8 and 9 itself, at the end of its
//!    own body, because the stamp has to be written after the last entry and
//!    the last prune and there is no way to observe that from outside.
//! 8. For a project scope, the `projects/` GC ledger registration (C-052). A
//!    **best-effort** step: a registration failure is a swallowed `warn!`,
//!    matching the shipped
//!    `project::registry::register_project_dir_best_effort` convention, and is
//!    **not** an item in [`RenderReport`] (RUL-28) — the ledger is not part of
//!    the rendered tree, so nothing in the report is about it.
//! 9. The stamp, last (C-048).
//!
//! # `bin/` on Windows is two files per name, and both are stamped
//!
//! A Windows trampoline is `<name>.exe` — a hardlink to the
//! [`ShimBinStore`](crate::file_structure::ShimBinStore) blob — plus
//! `<name>.exec`, the sidecar carrying the baked selector and, on its second
//! line, the absolute `ocx` this render resolved (C-031, V-9).
//! **Both get their own
//! [`bin_fingerprint`](crate::file_structure::RenderStamp::bin_fingerprint)
//! entry, keyed by the full on-disk file name** (RUL-26), so both appear in
//! [`RenderStamp::names`](crate::file_structure::RenderStamp::names) and both
//! are reported as their own [`RenderedItem`].
//!
//! That is forced, not chosen. `RenderStamp::names` documents itself as "the
//! on-disk entry set" and C-061's gate is a `readdir` of `bin/` compared
//! against exactly that set, so a stamp carrying one entry per *name* rather
//! than per *file* is a permanent Windows mismatch: the PATH entry withheld and
//! the `ocx pull` hint printed on every prompt, forever. C-003's "the `.exe`'s
//! hardlink identity **and** the sidecar's bytes" is then satisfied per entry
//! rather than by widening either field — the `.exe`'s entry carries the
//! hardlink identity in
//! [`BinEntryStamp::file_id`](crate::file_structure::BinEntryStamp::file_id),
//! the `.exec`'s entry carries the sidecar bytes in its `content_hash`.
//!
//! # Why the tree is reconciled and not republished
//!
//! `bin/` is a **whole-directory reconcile** (C-044): the computed name set is
//! written, and every name no longer in it is pruned — per-file atomic writes
//! and per-file removals, with **no completeness marker** and **no
//! directory-level atomic publish**. That is the opposite of
//! [`prepare_lazy`](super::prepare_lazy), whose shim tree lands by one
//! `rename` behind a `bin/` marker, and the difference is forced rather than
//! stylistic: a directory-level publish would go through
//! [`move_dir`](crate::utility::fs::move_dir), which calls `remove_dir_all` on
//! the destination — banned here, because the destination is a live directory
//! a running shell already has on `PATH`.
//!
//! # Everything this module writes stays inside the resolved home
//!
//! Render and prune act only inside [`RenderRequest::home`] (C-053). A tree at
//! a location `ocx` no longer resolves to — after a `toolchain-dir` change, or
//! after `[managed]` pushed one across a fleet — is **left in place, never
//! deleted**: this module has no delete path that can reach outside the home
//! it was handed. That property has one named seam, [`prune_within`], rather
//! than being spread through the render body — this is the plan's only delete
//! path into derived state, and a property with no signature is a property no
//! test can red.
//!
//! # No `refs/symlinks/` back-reference, ever
//!
//! A `<group>/<entry>` link is written with
//! [`symlink::replace_atomic`](crate::symlink::replace_atomic), never through
//! [`ReferenceManager`](crate::reference_manager::ReferenceManager) — see the
//! carve-out section in
//! [`toolchain_store`](crate::file_structure::ToolchainHome) for why the
//! obvious "use the shipped helper" edit pins every rendered package forever
//! (C-052).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::file_structure::{
    BinEntryStamp, DEFAULT_SHELL, FileStructure, RenderStamp, RenderStampScope, RenderStampTarget, ShimBinStore,
    TREE_OWN_DEPTH1_NAMES, ToolchainHome,
};
use crate::oci;
use crate::package_manager::error::PackageErrorKind;
use crate::package_manager::launcher::{
    TrampolineTarget, exec_sidecar_body, trampoline_ocx_binary, unix_trampoline_body,
};
use crate::project::{DEFAULT_GROUP, LockedTool, ProjectLock};
use crate::reference_manager::ReferenceManager;

use super::super::PackageManager;
use super::common::ClosureNode;
use super::toolchain_names::{NotEnumerablePolicy, exposed_names, fold_case_insensitive};

/// The `__OCX_TESTING_RENDER_FAULT` value that aborts a render **between the
/// first `bin/` entry write and the stamp write** (C-048).
///
/// Same *shape* as the shipped commit-path hook at
/// [`project::mutation`](crate::project::mutation) — one probe at render
/// entry, distinct stage values — but **not** the same variable, and not
/// ungated. That hook predates the project's test-seam convention;
/// `arch-principles.md` § *Code Style Conventions* names "a new `cfg(test)`-only
/// override **or a non-`__OCX_` env var** for a test seam" as `Deviation = Bug`,
/// and `subsystem-tests.md` requires release artifacts to **physically lack**
/// the path. Copying the older spelling would propagate the exception into a
/// new seam, and concretely: an `OCX_TEST_FAULT` already set in a released
/// environment for the commit hook would turn every `ocx pull` render into a
/// hard error, which C-050's skip does not cover.
///
/// C-048's negative control is *this* fault specifically, not a general write
/// failure: C-050 already covers the latter, and a render that fails a write
/// still writes a stamp for what it did land.
#[cfg(any(test, feature = "__testing"))]
pub const FAULT_AFTER_FIRST_ENTRY_WRITE: &str = "after_first_entry_write";

/// The `__OCX_TESTING_RENDER_FAULT` value that aborts a render **between
/// `shells/<shell>/` and the `active` link** (C-083).
///
/// C-083 states an order rather than a guarantee, and an order is only
/// observable at the point it can be interrupted: once both writes have landed
/// the finished tree is byte-identical either way, so a row asserting the
/// finished state passes with the two calls swapped. This seam is what makes
/// the *bad* interleaving — `active` published as a link into a directory that
/// does not exist — reachable and therefore assertable.
#[cfg(any(test, feature = "__testing"))]
pub const FAULT_AFTER_SHELL_TREE: &str = "after_shell_tree";

/// Everything one [`PackageManager::render_toolchain`] call needs, and nothing
/// it can re-derive.
///
/// A borrowed request struct rather than eight positional parameters: the
/// caller is `ocx pull` and the four mutation commands (C-054), and a
/// positional list of two paths, two slices, two booleans and a platform reads
/// as nothing at the call site.
///
/// **The rendered tree's content is a function of this request, plus the store
/// root named below; the report additionally reflects the tree's prior state.**
/// The first half is what makes C-047's idempotence and the plan's golden-tree
/// snapshots reachable without a registry — no field is re-derived from a
/// compose, a metadata read or the network. The second half is not a caveat to
/// be tightened away: [`RenderOutcome::Unchanged`] and [`RenderOutcome::Pruned`]
/// are *defined* by what was already on disk, so validation item 31's four
/// orphan classes are written by seeding a tree, not by varying this struct.
///
/// **The store root is the fourth input**, and it is deliberately not a field:
/// every trampoline body bakes an absolute `ocx` path chosen by
/// `launcher::generate::trampoline_ocx_binary`, whose ladder has **two rungs
/// and a fallback**, not one rung and a fallback:
///
/// 1. `<file structure root>/symlinks/<ocx cli id>/current/content/bin/ocx`,
///    when it is absolute and exists.
/// 2. **[`std::env::current_exe`]** — the `ocx` performing this very render —
///    under the same absolute-and-exists probe.
/// 3. Only when *both* decline is the bare name `ocx` baked, and WP-6's shipped
///    goldens pin that form too.
///
/// Rung 2 is what makes this a contract rather than a note. **Every golden-tree
/// snapshot must seed a real file at
/// `<file structure root>/symlinks/<ocx cli id>/current/content/bin/ocx`** so
/// rung 1 wins deterministically. A test that controls only the
/// [`FileStructure`] root and leaves that path absent does not get the bare-name
/// form — it falls to rung 2 and bakes the **test binary's own absolute path**,
/// which varies by machine, by cargo profile and by the target-directory hash.
/// The resulting instability is invisible locally, where the same path recurs,
/// and reds only on CI: exactly the shape validation items 12 and 31 cannot
/// afford, since both assert on rendered body bytes.
pub struct RenderRequest<'a> {
    /// The resolved home — `$OCX_HOME/toolchain` for the global tier, or
    /// [`resolve_toolchain_home`](crate::project::resolve_toolchain_home)'s
    /// answer for a project.
    ///
    /// Its root may not exist yet:
    /// [`ToolchainRoot::resolve`](crate::config::ToolchainRoot::resolve)
    /// deliberately has no filesystem side effect, so creating it is this
    /// module's job — see [`ensure_home_root`].
    pub home: &'a ToolchainHome,

    /// Which tier this render is for, carrying the canonical project directory
    /// when it is a project one.
    ///
    /// **One field, three consumers**, which is why it is the persisted
    /// [`RenderStampScope`] rather than a fourth type isomorphic to it: it is
    /// the scope the stamp records (C-003), the discriminant
    /// [`trampoline_target`] maps to the selector every body bakes (C-028),
    /// and the answer to whether this render registers the project in the
    /// `projects/` GC ledger (C-052). Three spellings of one tier is the drift
    /// class D-V14 argues against.
    ///
    /// # It carries a path; the stamp is addressed by a key
    ///
    /// [`StateStore::set_render_stamp`](crate::file_structure::StateStore::set_render_stamp)
    /// takes a [`RenderStampTarget`](crate::file_structure::RenderStampTarget),
    /// whose `Project` arm is a **16-hex string**, while this field's `Project`
    /// arm is a [`PathBuf`]. The renderer bridges them and nothing else does:
    ///
    /// - [`RenderStampScope::Global`] → `RenderStampTarget::Global`.
    /// - [`RenderStampScope::Project(dir)`](RenderStampScope::Project) →
    ///   `RenderStampTarget::Project(&`[`ReferenceManager::name_for_path`](crate::reference_manager::ReferenceManager::name_for_path)`(dir))`.
    ///
    /// **`dir` must already be the canonical project directory** —
    /// `project::consent::canonical_project_dir`'s answer, the same value
    /// [`ConsentStamp`](crate::project::consent::ConsentStamp) records — not
    /// merely some path naming the project. That is D-V13's contract: the render
    /// stamp's key has **parity with the consent stamp's**, so
    /// `state/projects/<key>/` holds both stamps for one project and `ocx
    /// clean`'s sweep of a `<key>` directory takes the pair together. Hashing a
    /// non-canonical spelling produces a second `<key>` for one project — the
    /// consent stamp under one, the render stamp under another, a sweep that
    /// removes one and leaves the other, and a C-061 gate that never matches.
    /// This field is therefore populated from the canonical derivation, never
    /// from `request.home`'s parent and never from `--project` as typed.
    pub scope: &'a RenderStampScope,

    /// The lock this render materialises. Supplies the `<group>/<entry>` set
    /// and each entry's per-platform digest.
    pub lock: &'a ProjectLock,

    /// The **default group's** merged multi-root closure — every default-group
    /// root and every interface-admitted dependency of each, concatenated in
    /// root order with each root's own closure in walk order (deps before
    /// dependents, root last).
    ///
    /// Produced by [`PackageManager::toolchain_surface`], which is the only
    /// sanctioned producer. **The default-group restriction is a contract this
    /// bare slice cannot express**: passing the closure of `-g ci`'s roots
    /// compiles, renders a `bin/` for the wrong group, and fails silently —
    /// `bin/` covers [`DEFAULT_GROUP`](crate::project::DEFAULT_GROUP) and
    /// nothing else (C-045), whatever [`Self::groups`] holds. A newtype would
    /// not earn its keep for one field with one producer, so the contract is
    /// stated here and asserted by the Specify phase instead.
    ///
    /// Consumed by
    /// [`exposed_names`](super::toolchain_names::exposed_names) under
    /// [`NotEnumerablePolicy::Skip`](super::toolchain_names::NotEnumerablePolicy::Skip)
    /// to produce `bin/`'s name set (C-023, C-045). Never a directory scan: a
    /// package with no `binaries` claim gets no trampolines. One call over the
    /// whole merged slice, so "last walked wins" has a well-defined total
    /// order (RUL-12).
    pub surface: &'a [ClosureNode],

    /// Which groups get `<group>/<entry>` links, following `pull`'s shipped
    /// `-g` default: bare `ocx pull` passes every group in the lock, `-g`
    /// narrows (C-045).
    ///
    /// # `bin/` is reconciled only when the default group is selected (RUL-25)
    ///
    /// `bin/` still covers [`DEFAULT_GROUP`](crate::project::DEFAULT_GROUP) and
    /// nothing else, whatever this slice holds — but **whether it is touched at
    /// all is this slice's decision**:
    ///
    /// - `DEFAULT_GROUP` ∈ `groups` → C-044's whole-directory reconcile runs.
    /// - `DEFAULT_GROUP` ∉ `groups` → **`bin/` is left entirely untouched**: no
    ///   writes, no prunes, not one `readdir`, and
    ///   [`RenderReport::bin_in_scope`] is `false`.
    ///
    /// Rendering `bin/` from a group the invocation did not select would force
    /// the default group's metadata to resolve — the closure walk
    /// [`Self::surface`] comes from reads metadata and may reach the network —
    /// so a narrowed `ocx pull -g ci` would grow a network dependency and a
    /// failure mode it never asked for. C-054 puts the reconcile "after roots
    /// resolve", and under `-g ci` the roots that resolved are `ci`'s. The
    /// resulting window in which `bin/` is older than the lock is not an
    /// oversight: it is the window C-064 already designs for, where the prompt
    /// path emits nothing and prints one hint rather than pruning.
    pub groups: &'a [String],

    /// Governs the `<group>/<entry>` links **only** (C-046).
    ///
    /// **`pinned = true` suppresses the entire link pass — no writes and no
    /// prunes** (RUL-23). C-044's whole-directory reconcile is *not* applied to
    /// an empty computed link set here: C-046 frames `pinned` as a property of
    /// what the *composition* yields while leaving the tree untouched, and
    /// C-066 says links are not *consulted* under `pinned`, not that they are
    /// removed. Reconciling instead would make the flip asymmetric — switching
    /// `pinned` on would delete the tree, and switching it back off would then
    /// need a re-render, against the plan's own "the flag takes effect with no
    /// re-render" (validation item 33).
    ///
    /// Every trampoline body stays byte-identical either way — a body bakes
    /// only the home selector, so flipping `pinned` changes what the
    /// *composition* yields and leaves `bin/` untouched. No trampoline reads
    /// this value at runtime either: `pinned` is resolved again at re-entry,
    /// from `ocx.toml`.
    pub pinned: bool,

    /// The platform whose leaf digest each `<group>/<entry>` link resolves to.
    pub platform: &'a oci::Platform,

    /// Report the delta and write nothing (C-049).
    ///
    /// **Performs no heal either** — a poisoned link is still present
    /// afterwards, which is the property S-007 pins. A dry run that repaired
    /// links would report a delta it had itself already closed.
    ///
    /// # "Writes nothing" is exhaustive, and the list is the contract
    ///
    /// A dry run creates **no** home root, **no** `.gitignore`, **no** render
    /// lock, **no** `state/projects/<key>/` directory, **no** `projects/` GC
    /// ledger entry, **no** stamp, and **no case-fold probe file anywhere at
    /// all**. It is the module doc's step 1, returning before
    /// [`ensure_home_root`] — which is the one ordering question RUL-37's
    /// hoist raises, and it lands on the `dry_run == true` side of the branch,
    /// not inside [`render_with`].
    ///
    /// The ledger is the case worth naming, because it is the one thing a dry
    /// run might plausibly be argued into doing: registration is best-effort and
    /// costs nothing, so "register anyway" is tempting. It does not (RUL-28).
    /// C-049 is "writes nothing", the `projects/` ledger is a symlink written
    /// under `$OCX_HOME` rather than part of the rendered home, and a dry run
    /// that registered would make `ocx pull --dry-run` a GC-visible mutation of
    /// a store the user asked it not to touch.
    ///
    /// The case-fold probe is the entry worth naming, because it is the one a
    /// dry run was twice argued into keeping (RUL-74): **the probe writes into
    /// the directory it judges**. Probing the nearest existing *ancestor* — the
    /// shape that shipped — put a `.ocx-case-probe-<pid>-<nanos>` file in the
    /// project checkout on every `ocx pull --dry-run` of a never-rendered home,
    /// and in `$HOME` when a configured `toolchain-dir`'s parents did not exist
    /// either. Narrowing it to "only an already-existing home root" left it
    /// creating and unlinking a file inside that root, which moves the root's
    /// own `mtime` on every dry run — still a write, and one an acceptance test
    /// snapshotting the tree observes. So a dry run does not probe at all, and
    /// renders against the unfolded name set.
    ///
    /// The residual is confined to the *prediction*, never to the tree: on a
    /// case-insensitive host a dry run may report two `Written` entries for a
    /// pair of case twins where the real run writes one, which is the same
    /// approximation [`prune_outcome`] already documents. A real render always
    /// probes the root it has just created, so it never inherits it.
    pub dry_run: bool,
}

/// What one render did to one artifact.
///
/// One type for both artifacts, because a trampoline and a link differ in what
/// they name, not in what can happen to them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedItem {
    /// Which artifact.
    pub artifact: RenderedArtifact,
    /// What happened to it.
    pub outcome: RenderOutcome,
}

/// One artifact a render is responsible for.
///
/// # What cannot be named here, and therefore cannot be reported or pruned
///
/// Every variant carries `String`s, so a `bin/` entry or a `<group>/` directory
/// whose **on-disk name is not valid UTF-8** has no representation: it cannot
/// become a [`RenderedItem`], cannot be reported, and is therefore never pruned.
/// Stated rather than left implicit, because the alternative reading — "the
/// prune is exact for every entry" — is false, and validation item 31's
/// exactness claim has to be read against this limit.
///
/// The cost is bounded and the choice is deliberate: an `OsString` here would
/// make every field of every consumer — the CLI's warn lines, the report
/// filters, the golden snapshots — lossy-convert at the point of display
/// anyway, and a non-UTF-8 entry in `bin/` is inert on the composing side too,
/// since it can match no [`BinaryName`](crate::package::metadata::BinaryName)
/// and no group key. It survives a render as an untouched foreign file, which
/// is the same outcome as [`RenderOutcome::Skipped`] minus the warn line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderedArtifact {
    /// A `bin/` launcher trampoline entry — DEFAULT group only (C-045).
    ///
    /// One value per **on-disk file**, not per exposed name: on Windows a name
    /// is two entries, `<name>.exe` and `<name>.exec` (RUL-26, module docs).
    ///
    /// A plain `String`, not a
    /// [`BinaryName`](crate::package::metadata::BinaryName). Writes still go
    /// through `BinaryName`; a report is a report. C-044's prune half removes
    /// every name in `bin/` no longer in the computed set, and a hostile
    /// clone's committed entry is exactly such a name. The three examples were
    /// each checked against `BinaryName`'s actual grammar rather than assumed,
    /// and all three hold: `CON.exe` fails the reserved-device-name rule, which
    /// splits on the first `.` and compares the basename case-insensitively;
    /// `.hidden` fails the leading-dot rule; a whitespace-bearing name fails the
    /// whitespace rule. `toolchain_names.rs` states the same fact from the other
    /// side — "every Windows-reserved device name is a valid slug and none is a
    /// valid `BinaryName`" — so the two modules agree rather than conflict. A
    /// typed variant could therefore construct neither [`RenderOutcome::Pruned`]
    /// nor, worse, the [`RenderOutcome::Skipped`] that says "I could not remove
    /// it". The [`Self::Link`] arm already uses plain `String`s for the same
    /// reason.
    Trampoline(String),
    /// A `links/<group>/<entry>` directory link to a package root.
    Link {
        /// The owning group.
        group: String,
        /// The tool's local binding name inside that group.
        entry: String,
    },
    /// A whole `links/<group>/` directory — validation item 31's fourth orphan
    /// class, the one that is a directory rather than a file.
    ///
    /// Its own variant because neither of the two above can name it.
    /// [`Self::Link`] names an entry *inside* a group, so a group directory that
    /// survives after every one of its entries was pruned — the state a removed
    /// `[group.ci]` table leaves — would have to be reported as an entry it does
    /// not contain, or not reported at all. Without this variant the class has
    /// no representation, and a class with no representation is one
    /// [`prune_within`] cannot be asked to handle and the Specify phase cannot
    /// red.
    ///
    /// The `String` is the group's **on-disk directory name**, exactly as
    /// `read_dir` yielded it — not a validated component. That is the point: the
    /// orphans worth naming include names
    /// [`ToolchainHome::entry`](crate::file_structure::ToolchainHome::entry)
    /// would refuse. See [`prune_within`] for what happens to those, and to a
    /// group directory that still holds a foreign file.
    GroupDirectory(String),
    /// A name at the **home root** that the tree does not own (C-074).
    ///
    /// A separate variant from [`Self::GroupDirectory`] because the two sit
    /// under different parents and one `String` cannot say which: a group
    /// directory is `<root>/links/<name>` and this is `<root>/<name>`. Folding
    /// them would let a `links/`-parented orphan be removed at the root, and a
    /// root name be removed from under `links/` — a wrong delete, silently, on
    /// every render.
    ///
    /// **It is not a group**, and the wording of any report over it must not
    /// say it is: depth 1 is closed and tree-owned (C-071), so everything the
    /// scan finds here is either a leftover of the pre-`links/` layout — a
    /// legacy `bin/`, a legacy `<group>/` — a leaked case probe, or something
    /// a third party put there. A populated one is **reported and never
    /// deleted** (C-076): [`prune_within`]'s `remove_dir` is non-recursive
    /// under every condition, so it fails `ENOTEMPTY` and the caller reports
    /// [`RenderOutcome::Skipped`] naming the path.
    RootEntry(String),
}

/// What a render did to one [`RenderedArtifact`].
///
/// `Unchanged` is a distinct answer from `Written` rather than a folded
/// "success": C-047 requires two consecutive renders of the same input to
/// leave a **byte-identical** tree, so "already correct, left exactly as it
/// is" is the observation that contract is about, and a report that could not
/// express it could not be asserted on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderOutcome {
    /// The artifact was created or replaced.
    Written,
    /// The artifact was already correct and was left byte-identical.
    Unchanged,
    /// The artifact was no longer in the computed set and was removed
    /// (C-044's prune half).
    Pruned,
    /// The write or the removal failed and the render continued (C-050).
    ///
    /// **Not an error.** A read-only checkout, a foreign-owned directory or a
    /// lock timeout warns, composes digest paths for this run, and exits 0 —
    /// so the outcome carries its own reason rather than a parallel list, and
    /// the caller's warn line is a filter over
    /// [`RenderReport::items`](RenderReport::items) instead of a second source
    /// of truth.
    Skipped {
        /// The path the render could not write or remove.
        path: PathBuf,
        /// Why, rendered for a warn line.
        ///
        /// **Human-facing prose with no grammar** (RUL-41). [`Self::Skipped`]'s
        /// machine-readable half is [`path`](Self::Skipped::path); this field
        /// is the underlying failure's `Display`, and nothing parses it. Tests
        /// assert the path and that the reason is non-empty — stated here so a
        /// later reviewer does not ask for a grammar that would have to be kept
        /// stable across every I/O error the platform can produce.
        reason: String,
    },
}

/// What one [`PackageManager::render_toolchain`] call did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderReport {
    /// Every artifact the render was responsible for, and what happened to it
    /// — including the ones it pruned and the ones it skipped.
    ///
    /// Under [`RenderRequest::dry_run`] this is the delta the render *would*
    /// have applied, with nothing written (C-049).
    pub items: Vec<RenderedItem>,

    /// Whether `bin/` was in this render's scope at all (RUL-25).
    ///
    /// `true` when [`DEFAULT_GROUP`](crate::project::DEFAULT_GROUP) was among
    /// [`RenderRequest::groups`] and C-044's whole-directory reconcile ran;
    /// `false` when a `-g`-narrowed invocation excluded it and `bin/` was left
    /// entirely untouched.
    ///
    /// **A field, not an inference from [`Self::items`].** Both cases produce
    /// zero `Trampoline` items, and the two mean opposite things: `false` is
    /// "`bin/` was not in scope, whatever is in it is still there", `true` with
    /// no items is "`bin/` was reconciled and is now empty". A caller printing
    /// "0 trampolines" for both would tell a user their tools had been removed
    /// when nothing was read, let alone written. Deriving it would also require
    /// [`RenderRequest::groups`] at every consumer, which is the second
    /// spelling of one decision.
    ///
    /// A render that never reached the reconcile at all — C-050's skip, taken
    /// before [`render_with`] is entered — reports `false` for the same reason
    /// the `false` reading already gives: `bin/` was not read, so whatever is
    /// in it is still there.
    pub bin_in_scope: bool,

    /// Whether this call wrote the render stamp.
    ///
    /// The stamp is written **last** (C-048), so `false` beside a non-empty
    /// [`Self::items`] is exactly the detectably-incomplete state a crash
    /// mid-render leaves — the state
    /// [`FAULT_AFTER_FIRST_ENTRY_WRITE`] injects on demand.
    ///
    /// # The stamp always describes the tree **as it stands on disk**, never
    /// # the intended tree (RUL-27)
    ///
    /// One rule, with three consequences that would otherwise read as three
    /// unrelated decisions:
    ///
    /// - A **partially-skipped** render stamps only what landed (RUL-22, below).
    /// - Under [`RenderRequest::pinned`] the link pass writes and prunes nothing
    ///   (RUL-23), **and the stamp still records the default group's links** —
    ///   observed by **one `readlink` pass** over the entries already on disk. No
    ///   compose, no metadata read, no network: the same arithmetic
    ///   [`heal_links`] is built from. A pinned render that stamped an empty
    ///   `link_fingerprint`, or one derived from the lock rather than from the
    ///   tree, would describe a tree it deliberately did not write.
    /// - A **dry run** writes no stamp at all, because it wrote no tree.
    ///
    /// Without this rule, C-061's gate mismatches on every prompt forever in
    /// exactly the cells validation item 20's `activate × pinned` matrix has to
    /// cover — a pinned project would be permanently un-activatable while the
    /// tree on disk was entirely correct.
    ///
    /// **A partially-skipped render still writes a stamp, and the stamp
    /// records only what actually landed** (RUL-22). That is forced by the
    /// shipped stamp, not chosen here:
    /// [`RenderStamp::names`](crate::file_structure::RenderStamp::names)
    /// documents itself as "the on-disk entry set" and
    /// [`RenderStamp::new`](crate::file_structure::RenderStamp::new) derives
    /// `names` from `bin_fingerprint`'s keys precisely so a producer cannot
    /// record an intended-but-unwritten set. A stamp claiming the intended set
    /// on a read-only checkout would mismatch C-061's `readdir` gate on
    /// **every** prompt forever, withholding the entry and printing the
    /// `ocx pull` hint indefinitely — the exact failure C-003 cites when it
    /// refuses to widen `link_fingerprint`.
    ///
    /// Two things therefore produce `false`, and only two: a dry run (always),
    /// and a stamp write that itself failed — which warns and returns `Ok`
    /// like every other write failure under C-050, leaving the prompt gate to
    /// withhold, the conservative answer.
    ///
    /// **The C-048 fault seam is not a third.** It propagates `Err` out of
    /// `reconcile_bin` through [`render_with`] to
    /// [`PackageManager::render_toolchain`], so no `RenderReport` is
    /// constructed on that path at all — the caller gets the error, and the
    /// detectably-incomplete state's on-disk evidence is the **unwritten
    /// stamp beside a non-empty `bin/`**, which is what C-061's gate reads and
    /// what the test pinning the seam asserts. Reading this field as the
    /// crash-detector would be reading a value that a crash never produces.
    pub stamp_written: bool,
}

impl PackageManager {
    /// The merged multi-root closure [`RenderRequest::surface`] takes — every
    /// root in `roots`, in order, each followed by its own metadata-only
    /// dependency closure in walk order (deps before dependents, root last).
    ///
    /// **This exists because nothing else in `ocx_lib`'s public API yields
    /// it.** `ClosureNode` is publicly nameable, but
    /// [`ComposeRoots::roots`](crate::package_manager::composer::ComposeRoots)
    /// is a `Vec<Arc<InstallInfo>>`,
    /// [`PreparedLazy::closure`](super::prepare_lazy::PreparedLazy) is one
    /// deferred tool's closure, and
    /// [`InspectClosure::nodes`](super::inspect::InspectClosure) is per-package
    /// and comes from a different walk — so without this method `ocx_cli` can
    /// name every other field of a [`RenderRequest`] and still not build one,
    /// and the module's whole public surface would be callable only from
    /// `#[cfg(test)]`.
    ///
    /// It is a facade over crate-internal machinery, not a second walker:
    /// every root goes through
    /// [`common::walk_closure_nodes`](super::common::walk_closure_nodes), the
    /// same walk `ocx package inspect --closure` and
    /// [`prepare_lazy`](PackageManager::prepare_lazy) use.
    ///
    /// **Deliberately not folded into
    /// [`render_toolchain`](Self::render_toolchain).** The walk reads metadata
    /// and may reach the network; the renderer reads neither. Folding it in
    /// would put a registry between validation item 31's golden-tree snapshots
    /// and the tree they assert on.
    ///
    /// `roots` is the **default group's** root set: `bin/` covers
    /// [`DEFAULT_GROUP`](crate::project::DEFAULT_GROUP) and nothing else
    /// (C-045).
    ///
    /// # Errors
    ///
    /// Whatever the underlying walk returns for any root — see
    /// [`common::walk_closure_nodes`](super::common::walk_closure_nodes).
    /// Fail-closed across the whole set: one root's failure aborts, because a
    /// partial surface renders a quietly incomplete `bin/`.
    pub async fn toolchain_surface(
        &self,
        roots: &[oci::Identifier],
        platform: &oci::Platform,
    ) -> Result<Vec<ClosureNode>, PackageErrorKind> {
        let (file_structure, index) = (self.file_structure(), self.index());
        let mut merged = Vec::new();
        for root in roots {
            let resolved = self.resolve(root, platform.clone()).await?;
            // The walk reads each dependency's config blob, and reads it from
            // the blob store — the same warm-the-whole-chain step
            // `prepare_lazy` and `inspect --closure` run before their own walk.
            super::common::stage_chain_blobs(file_structure, index, &resolved).await?;
            super::common::stage_leaf_manifest(file_structure, index, &resolved.pinned).await?;

            let metadata =
                super::common::load_config_metadata(index, &resolved.pinned, &resolved.final_manifest).await?;
            let config_digest = super::common::config_blob_digest(&resolved.final_manifest)?;
            merged.extend(
                super::common::walk_closure_nodes(
                    file_structure,
                    index,
                    self.is_offline(),
                    &resolved.pinned,
                    &metadata,
                    config_digest,
                    platform,
                )
                .await?,
            );
        }
        Ok(merged)
    }

    /// Render `request.home` — write `bin/`'s trampolines, repoint or write
    /// the selected groups' `<group>/<entry>` links, prune what is no longer
    /// in either set, then write the render stamp (C-044…C-053).
    ///
    /// # Order, and why the stamp is last
    ///
    /// The stamp is written after every entry write and every prune (C-048),
    /// so a crash mid-render leaves a tree that is **detectably incomplete**
    /// rather than one that looks finished: the prompt path's gate (C-061)
    /// reads the stamp, finds it absent or mismatched, and withholds the PATH
    /// entry instead of exporting a half-written `bin/`.
    /// [`FAULT_AFTER_FIRST_ENTRY_WRITE`] is the seam that makes that state
    /// reachable in a test. A render that skipped some entries still writes a
    /// stamp — see [`RenderReport::stamp_written`] for why that is forced.
    ///
    /// # Idempotence
    ///
    /// `render(render(x)) == render(x)`, byte-identical across two consecutive
    /// runs (C-047). An already-correct entry is left exactly as it is and
    /// reported [`RenderOutcome::Unchanged`] — never rewritten with fresh
    /// bytes, which would be identical in content but not in inode, and would
    /// invalidate every stat gate downstream.
    ///
    /// # Name collisions: this function emits nothing (RUL-34)
    ///
    /// Two packages claiming one name never refuse and never warn (C-024): last
    /// walked wins, and the losing claims land on the winner's
    /// [`NameOwner::shadowed`](super::toolchain_names::NameOwner). The plan caps
    /// that surface at one debug line plus an `ocx inspect` row (D-4), and
    /// [`RenderReport`] therefore carries no `collisions` field.
    ///
    /// **The debug line already exists and is not this function's to emit.**
    /// `toolchain_names::record_claim`'s collision branch emits it — one line
    /// per collision, naming the winner and the shadowed claim — and a shipped
    /// test asserts that branch keeps emitting it. That line *is* C-024's line.
    /// A renderer that emitted its own would log every collision twice, which is
    /// worse than either alternative: a reader would count collisions that did
    /// not happen. An earlier draft of this doc claimed the obligation was the
    /// renderer's; it is not, and the claim is corrected here rather than left
    /// to be discovered by whoever implements it.
    ///
    /// # Which platform a link resolves to (RUL-31)
    ///
    /// Every `<group>/<entry>` link's leaf digest is chosen from
    /// [`LockedTool::platforms`](crate::project::LockedTool) through the shared
    /// [`select_best`](crate::oci::platform::select_best) /
    /// [`is_compatible`](crate::oci::platform::is_compatible) helper —
    /// concretely `project::resolve::lookup_host_leaf`, which is that helper
    /// applied to exactly this map — and **never** by an exact key lookup on
    /// `request.platform`'s rendering.
    ///
    /// [`heal_links`] uses the identical rule, and that identity is the point.
    /// A *compatible but non-identical* key — `linux/amd64` in the lock against
    /// a host that renders as `linux/amd64/v3`, or a bare `any` leaf — makes an
    /// exact-lookup renderer and a `select_best` heal disagree about what the
    /// link should name: the render writes one target, the heal repoints it to
    /// the other, and the next render repoints it back. That oscillation costs a
    /// `readlink` and a rename on every invocation and never converges, so the
    /// stamp's `link_fingerprint` never matches for two consecutive runs.
    ///
    /// A [`Selection::None`](crate::oci::platform::Selection) — no compatible
    /// key for this host — **skips the entry silently**: no link, no
    /// [`RenderOutcome::Skipped`], no error. A lock legitimately carries tools
    /// that have no build for this platform, and refusing would make one
    /// unavailable tool fail the whole render.
    ///
    /// # Case folding, and what a failed probe means
    ///
    /// This function evaluates [`filesystem_is_case_insensitive`] **once**,
    /// against `request.home`, **after [`ensure_home_root`] has created it**
    /// (RUL-37, and the module doc's step list), and hands the answer to
    /// [`render_with`]; the
    /// fold ([`fold_case_insensitive`](super::toolchain_names::fold_case_insensitive))
    /// is applied there and only when it is `true` (R-W19(b), D-V18). The
    /// split is what makes both arms reachable from a unit test on either
    /// host — a probe consumed inline would put the `true` arm out of reach of
    /// every test on a case-sensitive machine, which is the unreachable-red
    /// class D-V18 moved the fold out of `exposed_names` to escape.
    ///
    /// **A probe that fails is C-050's skip, not a boolean default.** The
    /// probe writes into the very home this call is about to render, so a
    /// probe that cannot run is a home that cannot be written — the same
    /// reasoning that routes a home-root creation failure to the skip below.
    /// Inventing either answer would pick a silent corruption: `false` on a
    /// case-insensitive host writes two names that are one file, so the
    /// stamp's entry set can never match C-061's `readdir` and the entry is
    /// withheld forever; `true` on a case-sensitive host silently drops a tool
    /// from `bin/`.
    ///
    /// # Failure is not an error
    ///
    /// A write or removal that fails is [`RenderOutcome::Skipped`] and the
    /// render continues (C-050): a read-only checkout is an ordinary state for
    /// a CI container, and refusing to `pull` there would be worse than
    /// composing digest paths for the run. The call still returns `Ok`, and
    /// the caller exits 0.
    ///
    /// **Three further conditions route to that same skip, and none of them is
    /// an error:**
    ///
    /// - A **lock timeout** on the render lock (RUL-30) — C-050 names it
    ///   outright. The call warns, returns empty
    ///   [`items`](RenderReport::items) and `stamp_written: false`, and is `Ok`.
    /// - A **symlink at the home root or at any directory the tree owns**
    ///   (RUL-33, C-074) —
    ///   [`ensure_home_root`]'s refusal, warned **loudly** rather than at debug,
    ///   because unlike a read-only checkout it is not a benign state.
    /// - An **orphan that is a directory** the render cannot remove (RUL-32) —
    ///   [`RenderOutcome::Skipped`] naming the path, never a recursive removal.
    ///   See [`prune_within`].
    ///
    /// **A failure to create the home root is that same skip** (RUL-24), not a
    /// hard error: C-050's own trigger list opens with "read-only checkout",
    /// and a read-only checkout with no `.ocx/toolchain` yet *is* a home-root
    /// creation failure. Such a call warns, returns empty
    /// [`items`](RenderReport::items) and `stamp_written: false`, and is
    /// `Ok`. The boundary is with **resolution**, not with I/O: an unsound
    /// `toolchain-dir` is already refused at resolve time with exit 78
    /// (C-017–C-019), so a creation failure *after* a passing resolve carries
    /// no policy content — it is an I/O condition, and therefore a skip.
    ///
    /// # Errors
    ///
    /// - [`PackageErrorKind::ToolchainPath`] — a group or entry name from
    ///   `ocx.toml`, `ocx.lock` or `-g` cannot become a path component of the
    ///   tree (exit 78). Refused before anything is written for that entry.
    /// - [`PackageErrorKind::ShimNameInvalid`] — a declared entry point name
    ///   does not survive conversion to a
    ///   [`BinaryName`](crate::package::metadata::BinaryName), so no
    ///   trampoline can be written for it.
    /// - [`PackageErrorKind::Internal`] — a body or sidecar refusal
    ///   ([`crate::Error::ToolchainHomeNotAbsolute`],
    ///   [`crate::Error::LauncherPathNotUtf8`],
    ///   [`crate::Error::LauncherUnsafeCharacter`]), or the C-048 fault seam.
    ///   **No filesystem failure reaches this variant**: every write, every
    ///   prune, the home-root creation and the stamp write are skips.
    pub async fn render_toolchain(&self, request: RenderRequest<'_>) -> Result<RenderReport, PackageErrorKind> {
        let file_structure = self.file_structure();

        // Read once, at render entry, and threaded down as a parameter — never
        // re-probed per stage. Outside a gated build the variable is `None` and
        // the env var is never read at all (`subsystem-tests.md`).
        #[cfg(any(test, feature = "__testing"))]
        let fault = read_fault_hook();
        #[cfg(not(any(test, feature = "__testing")))]
        let fault: Option<String> = None;
        let fault = fault.as_deref();

        // Step 1 (module docs): a dry run performs none of the steps below,
        // and the case-fold probe is no exception (RUL-74). The probe writes
        // into the directory it judges, so even into a home root that already
        // exists it moves that directory's own mtime — a write C-049 promised
        // not to make. The unfolded name set is used instead; the residual is
        // confined to the prediction (see `RenderRequest::dry_run`).
        if request.dry_run {
            return render_with(file_structure, request, false, fault).await;
        }

        // Step 2: the home root, before the probe and before `.gitignore` —
        // preceded by RUL-44's component guard, in the same blocking unit,
        // because a symlink one level above the root relocates everything
        // below it before `ensure_home_root` gets a path to judge.
        let (scope, root) = (request.scope.clone(), request.home.root().to_path_buf());
        match tokio::task::spawn_blocking(move || {
            let home = ToolchainHome::new(root);
            refuse_symlinked_home(&scope, &home)?;
            ensure_home_root(&home)
        })
        .await
        {
            Ok(Ok(_created)) => {}
            Ok(Err(error)) => {
                // Loud, not debug: unlike a read-only checkout this is either a
                // tree ocx cannot own or a committed symlink out of it (RUL-33).
                crate::log::warn!(
                    "Skipping toolchain render: '{}' could not be prepared: {error}",
                    request.home.root().display()
                );
                return Ok(skipped_render());
            }
            Err(join_error) => {
                crate::log::warn!("Skipping toolchain render: the home-root task failed: {join_error}");
                return Ok(skipped_render());
            }
        }

        // Step 3: C-004's ignore file. Best-effort — a tree the repository
        // controls may deny it, and the trampolines are the render's product.
        let gitignore_home = ToolchainHome::new(request.home.root().to_path_buf());
        match tokio::task::spawn_blocking(move || gitignore_home.ensure_gitignore()).await {
            Ok(Ok(_written)) => {}
            Ok(Err(error)) => crate::log::warn!(
                "Toolchain home '{}' has no ignore file: {error}",
                request.home.root().display()
            ),
            Err(join_error) => crate::log::warn!("The toolchain ignore-file task failed: {join_error}"),
        }

        // Step 4: one lock over the whole body, held until this call returns.
        let _render_lock = match render_lock_parameters(file_structure, &request).acquire().await {
            Ok(guard) => guard,
            Err(error) => {
                crate::log::warn!(
                    "Skipping toolchain render of '{}': its render lock was not available: {error}",
                    request.home.root().display()
                );
                return Ok(skipped_render());
            }
        };

        // Step 5: the probe, against the root step 2 just created.
        let case_insensitive = match filesystem_is_case_insensitive(request.home.root()).await {
            Ok(answer) => answer,
            Err(error) => {
                crate::log::warn!(
                    "Skipping toolchain render: the case-fold probe of '{}' failed: {error}",
                    request.home.root().display()
                );
                return Ok(skipped_render());
            }
        };

        // Step 6: the shell directory, then `active` — in that order (C-083),
        // inside the render lock (C-081), and before any trampoline is written.
        // A failure here is a warn and not a return: the physical tree under
        // `shells/` still renders correctly, and an `active` left wrong fails
        // `active_is_valid`, so the prompt gate withholds rather than exposing
        // anything. Returning instead would make a home whose `active` cannot
        // be repaired render nothing at all, which is strictly worse for the
        // read-only checkout C-050 exists for.
        let root = request.home.root().to_path_buf();
        let shell_tree =
            tokio::task::spawn_blocking(move || ensure_shell_tree(&ToolchainHome::new(root), DEFAULT_SHELL)).await;

        // C-083's one interruption point between the two writes: on abort
        // `active` is **absent**, which is a lookup miss and never a wrong
        // answer — the state the reverse order could not produce.
        #[cfg(any(test, feature = "__testing"))]
        maybe_inject_fault(fault, RenderStage::AfterShellTree)?;

        let root = request.home.root().to_path_buf();
        let healed = match shell_tree {
            Ok(Ok(())) => {
                tokio::task::spawn_blocking(move || heal_active(&ToolchainHome::new(root), DEFAULT_SHELL)).await
            }
            other => other,
        };
        match healed {
            Ok(Ok(())) => {}
            Ok(Err(error)) => crate::log::warn!(
                "Toolchain '{}' has no usable '{}' link: {error}",
                request.home.root().display(),
                request.home.active().display()
            ),
            Err(join_error) => crate::log::warn!("The toolchain activation-link task failed: {join_error}"),
        }

        render_with(file_structure, request, case_insensitive, fault).await
    }
}

/// The report a render that never reached the reconcile returns (C-050).
///
/// Empty [`items`](RenderReport::items), no stamp, and
/// [`bin_in_scope`](RenderReport::bin_in_scope) `false` — nothing was read, so
/// nothing in `bin/` was touched.
fn skipped_render() -> RenderReport {
    RenderReport {
        items: Vec::new(),
        bin_in_scope: false,
        stamp_written: false,
    }
}

/// One `lock_scoped` call's five arguments, as a value (RUL-38, RUL-30).
///
/// A `pub(crate)` value rather than five arguments spelled at each site:
/// without it a test cannot take *the same* lock the code under test takes, and
/// C-050's lock trigger and RUL-36's contention clause would both be green in a
/// way indistinguishable from never having run.
///
/// Owned rather than borrowed because one of its two producers keys on a
/// `links/<group>` directory it was handed; a `PathBuf` clone per lock is noise
/// beside the acquire itself.
pub(crate) struct ScopedLockParameters {
    /// `$OCX_HOME/locks` — never a sidecar inside the guarded tree.
    pub(crate) locks_root: PathBuf,
    /// The lock's purpose.
    pub(crate) scope: &'static str,
    /// The directory whose file identity is the lock's primary key.
    pub(crate) guarded_directory: PathBuf,
    /// What distinguishes locks sharing a directory and a scope.
    pub(crate) discriminator: String,
    /// How long an acquire waits before the caller degrades (C-050, RUL-36).
    pub(crate) timeout: Duration,
}

impl ScopedLockParameters {
    /// Take the lock these parameters describe.
    ///
    /// # Errors
    ///
    /// [`lock_scoped`](crate::utility::fs::lock_scoped)'s own — the lock file
    /// could not be created, or the lock was not acquired within
    /// [`timeout`](Self::timeout).
    pub(crate) async fn acquire(&self) -> crate::Result<crate::utility::fs::LockedFile> {
        crate::utility::fs::lock_scoped(
            &self.locks_root,
            self.scope,
            &self.guarded_directory,
            &self.discriminator,
            self.timeout,
        )
        .await
    }
}

/// The `scope` every render lock is taken under.
const RENDER_LOCK_SCOPE: &str = "toolchain-render";

/// The `scope` every [`heal_links`] repoint is taken under.
const HEAL_LOCK_SCOPE: &str = "toolchain-heal";

/// How long either lock waits before its caller degrades.
///
/// The shipped [`PULL_LOCAL_LOCK_TIMEOUT`](super::pull::PULL_LOCAL_LOCK_TIMEOUT)
/// rather than a constant minted here: `ocx pull` is the render's caller
/// (C-054), that value is already this subsystem's answer to "how long a
/// foreground pull waits on a lock before giving up", and a second spelling of
/// one budget is the drift class D-V14 argues against.
const TOOLCHAIN_LOCK_TIMEOUT: Duration = super::pull::PULL_LOCAL_LOCK_TIMEOUT;

/// The lock parameters for one render (RUL-38).
///
/// **Discriminated by the stamp key**, so two homes never share a lock and one
/// home never takes two: the key is exactly what the stamp is addressed by —
/// the literal `global`, or
/// [`ReferenceManager::name_for_path`](crate::reference_manager::ReferenceManager::name_for_path)
/// of the canonical project directory.
pub(crate) fn render_lock_parameters(
    file_structure: &FileStructure,
    request: &RenderRequest<'_>,
) -> ScopedLockParameters {
    ScopedLockParameters {
        locks_root: file_structure.locks.clone(),
        scope: RENDER_LOCK_SCOPE,
        guarded_directory: request.home.root().to_path_buf(),
        discriminator: stamp_key(request.scope),
        timeout: TOOLCHAIN_LOCK_TIMEOUT,
    }
}

/// The lock parameters one [`heal_links`] repoint takes.
///
/// Guarded by the `links/<group>` directory and discriminated by the entry
/// name, so two processes contend only when they repoint the *same* link — the
/// whole heal is never serialized behind one lock, which matters because C-070
/// puts it on `ocx env`'s and `ocx exec`'s critical path.
///
/// Takes the group **directory**, never the group name: it is the entry path's
/// own parent by construction (C-072), and re-joining the name here would be a
/// second spelling of one path — which is exactly what broke when `links/`
/// moved every group one level down.
pub(crate) fn heal_lock_parameters(
    file_structure: &FileStructure,
    group_directory: PathBuf,
    entry: &str,
) -> ScopedLockParameters {
    ScopedLockParameters {
        locks_root: file_structure.locks.clone(),
        scope: HEAL_LOCK_SCOPE,
        guarded_directory: group_directory,
        discriminator: entry.to_string(),
        timeout: TOOLCHAIN_LOCK_TIMEOUT,
    }
}

/// `<root>/links`, derived from the grammar rather than re-spelled here
/// (C-072).
///
/// [`ToolchainHome::links_group`](crate::file_structure::ToolchainHome::links_group)
/// owns the `links` component, so its parent is the one spelling of that name
/// this module can hold and the two cannot drift. The derivation is total in
/// practice — [`DEFAULT_GROUP`] is a constant the grammar admits, and a
/// two-component path always has a parent — and
/// `links_root_is_the_grammars_own_parent` pins both halves, so the fallback
/// is unreachable rather than merely unlikely.
fn links_root(home: &ToolchainHome) -> PathBuf {
    home.links_group(DEFAULT_GROUP)
        .ok()
        .as_deref()
        .and_then(Path::parent)
        .map_or_else(|| home.root().to_path_buf(), Path::to_path_buf)
}

/// The key one tier's render stamp is addressed by, and the render lock
/// discriminated by.
fn stamp_key(scope: &RenderStampScope) -> String {
    match scope {
        RenderStampScope::Global => "global".to_string(),
        RenderStampScope::Project(directory) => ReferenceManager::name_for_path(directory),
    }
}

/// The render body, with the two ambient answers made parameters.
///
/// [`PackageManager::render_toolchain`] resolves both once — the case-fold
/// probe and the C-048 fault stage — and delegates here, so the Specify
/// phase can drive `case_insensitive` both ways and the fault both present and
/// absent on either host, with no `cfg!(target_os)` anywhere and **no
/// in-process environment write**. That last point is not incidental:
/// `crate::test::env::EnvLock`'s override table is consulted only by
/// [`crate::env::var`], never by `std::env::var_os`, so an env-only seam would
/// have forced `unsafe { std::env::set_var }` into the test.
///
/// A free function taking `&FileStructure` rather than a method on
/// [`PackageManager`], per `subsystem-package-manager.md`: only the facade's
/// `pub` surface belongs on the shared `impl`, and this makes the store-root
/// dependency [`RenderRequest`] documents — the `ocx` binary a trampoline body
/// bakes, and the `projects/` GC ledger C-052 registers into — a parameter
/// instead of ambient state.
///
/// # Preconditions the caller establishes, and this function assumes
///
/// Steps 2–4 of the module doc's order have already run when this is entered,
/// and none of them is repeated here:
///
/// - `request.home`'s root **exists**, is a real directory rather than a
///   symlink, and was created owner-only-write ([`ensure_home_root`], RUL-33).
/// - `.gitignore` is present (C-004).
/// - **The render lock is held by the caller** for the whole of this call
///   (RUL-30). Not acquired here because the lock must cover the stamp write
///   too, and a guard scoped to the reconcile passes inside this function would
///   leave it outside. It is emphatically *not* because the lock covers the
///   home-root creation — it does not, and cannot: the lock is taken **after**
///   that creation (module doc step 4), since `lock_scoped` keys on the guarded
///   directory's file identity and needs the directory to exist first.
///
/// The hoist is RUL-37's, and it is a correctness fix rather than tidying: with
/// the creation inside this function, the case-fold probe above it ran against
/// a home that did not exist yet, failed, and routed a first-ever `ocx pull` to
/// C-050's skip — a fresh home never rendered. Nothing is passed down to carry
/// the hoisted work, because nothing in this body reads it; the preconditions
/// are the contract instead.
///
/// # Errors
///
/// As [`PackageManager::render_toolchain`].
async fn render_with(
    file_structure: &FileStructure,
    request: RenderRequest<'_>,
    case_insensitive: bool,
    fault: Option<&str>,
) -> Result<RenderReport, PackageErrorKind> {
    let bin_in_scope = request.groups.iter().any(|group| group == DEFAULT_GROUP);
    let mut items: Vec<RenderedItem> = Vec::new();

    // ── `bin/` — C-044's whole-directory reconcile, default group only ──────
    let landed = if bin_in_scope {
        reconcile_bin(file_structure, &request, case_insensitive, fault, &mut items).await?
    } else {
        BTreeSet::new()
    };

    // ── `<group>/<entry>` — suppressed entirely under `pinned` (RUL-23) ─────
    if !request.pinned {
        reconcile_links(file_structure, &request, case_insensitive, &mut items).await?;
    }

    // ── The `projects/` GC ledger (C-052), then the stamp, last (C-048) ─────
    if !request.dry_run
        && let RenderStampScope::Project(project_directory) = request.scope
        && let Err(error) = crate::project::ProjectRegistry::new(file_structure.root())
            .register(project_directory)
            .await
    {
        crate::log::warn!(
            "Project '{}' was not registered as a GC root: {error}",
            project_directory.display()
        );
    }

    // One blocking unit for the whole stamp: `fingerprint_bin`'s per-entry
    // stat, bounded read and SHA-256, `observe_default_group_links`'
    // `read_dir`/`read_link` pass, and `StateStore::{render_stamp,
    // set_render_stamp}` — whose own docs say async callers wrap them.
    let stamp_written = if request.dry_run {
        false
    } else {
        let root = request.home.root().to_path_buf();
        let (state, scope, landed) = (file_structure.state.clone(), request.scope.clone(), landed.clone());
        let written = blocking(root.clone(), move || {
            Ok(write_render_stamp(
                &state,
                &ToolchainHome::new(root),
                &scope,
                bin_in_scope,
                &landed,
            ))
        })
        .await;
        match written {
            Ok(written) => written,
            Err(error) => {
                crate::log::warn!(
                    "Toolchain '{}' was rendered but its render-stamp task failed: {error}",
                    request.home.root().display()
                );
                false
            }
        }
    };

    Ok(RenderReport {
        items,
        bin_in_scope,
        stamp_written,
    })
}

/// Reconcile `bin/` and return the on-disk file names that actually landed.
///
/// "Landed" is `Written` or `Unchanged` — never `Skipped` — because the stamp
/// records the tree as it stands, and a half-published name is not a
/// trampoline the composing side may trust (RUL-22, RUL-27).
async fn reconcile_bin(
    file_structure: &FileStructure,
    request: &RenderRequest<'_>,
    case_insensitive: bool,
    fault: Option<&str>,
    items: &mut Vec<RenderedItem>,
) -> Result<BTreeSet<String>, PackageErrorKind> {
    #[cfg(not(any(test, feature = "__testing")))]
    let _ = fault;

    let home = request.home;
    // The **physical** directory, never `bin()` (C-080): every write, prune and
    // fingerprint below it would otherwise resolve through `active` and land
    // wherever that link points — including the stamp, which would then
    // *certify* an attacker's directory and make the prompt gate pass by
    // agreeing with it.
    let bin = home.shell_bin(DEFAULT_SHELL);

    // C-022/C-045: `Skip`, never `Refuse` — an ordinary package that claims
    // neither `binaries` nor entry points contributes nothing and is not an
    // error. The fold runs only when the probe said the home is
    // case-insensitive (D-V18, R-W19(b)).
    let names = exposed_names(request.surface, NotEnumerablePolicy::Skip)?;
    let names = if case_insensitive {
        fold_case_insensitive(names)
    } else {
        names
    };

    // Both bodies on every platform: the two producers are platform-split, but
    // D-V21's refusal of a non-absolute project root is not, and a `cfg`-gated
    // producer call would put one arm's refusal behind a `cfg` the CI leg that
    // actually runs never compiles.
    let target = trampoline_target(request.scope);
    let ocx_binary = trampoline_ocx_binary(file_structure).await;
    let body = unix_trampoline_body(&target, ocx_binary.as_deref()).map_err(PackageErrorKind::Internal)?;
    // Both producers take the resolved `ocx` (V-9). The Windows sidecar needs
    // it for exactly the reason the `.sh` body does: without an absolute
    // program baked, a trampoline resolves the bare name `ocx` — and on
    // Windows that search starts inside `bin/` itself, where a package
    // claiming the name renders `ocx.exe`.
    let sidecar = exec_sidecar_body(&target, ocx_binary.as_deref()).map_err(PackageErrorKind::Internal)?;

    let mut expected: BTreeSet<String> = BTreeSet::new();
    for name in names.keys() {
        expected.extend(trampoline_file_names(name.as_str()));
    }

    // Created before the first write, and only when there is one: a `-g`
    // narrowed render that reconciles an empty set must not conjure `bin/`.
    // Owner-only at create time, never the ambient umask (R-W19(a)): `bin/` is
    // the directory all three PATH routes point at, so a group-writable one is
    // a write primitive into everything the user's shell resolves.
    //
    // `create_directory_owner_only`, never `create_owner_only`: RUL-33's check
    // on `bin/` ran at Step 2, and the render **lock acquisition** sits between
    // that check and this use, blocking for an unbounded time. See that
    // function for why a recursive create cannot be used to close it. Its
    // parent, `shells/<shell>`, was created at Step 6.
    let mut writable = true;
    if !request.dry_run && !names.is_empty() {
        let target = bin.clone();
        if let Err(error) = blocking(bin.clone(), move || create_directory_owner_only(&target)).await {
            crate::log::warn!("Toolchain '{}' is not writable: {error}", bin.display());
            writable = false;
        }
    }

    let mut landed: BTreeSet<String> = BTreeSet::new();
    let mut wrote_first = false;
    for name in names.keys() {
        let files = trampoline_file_names(name.as_str());
        let outcome = if writable {
            publish_bin_entry(
                &bin,
                name.as_str(),
                &body,
                &sidecar,
                &file_structure.shim_bin,
                request.dry_run,
            )
            .await
        } else {
            Err(crate::error::file_error(
                &bin,
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "the trampoline directory is not writable",
                ),
            ))
        };

        match outcome {
            Ok(written) => {
                let outcome = if written {
                    RenderOutcome::Written
                } else {
                    RenderOutcome::Unchanged
                };
                for file in &files {
                    landed.insert(file.clone());
                    items.push(RenderedItem {
                        artifact: RenderedArtifact::Trampoline(file.clone()),
                        outcome: outcome.clone(),
                    });
                }
                if written && !request.dry_run && !wrote_first {
                    wrote_first = true;
                    // C-048's negative control: the one point at which a render
                    // may be aborted between an entry write and the stamp.
                    #[cfg(any(test, feature = "__testing"))]
                    maybe_inject_fault(fault, RenderStage::AfterFirstEntryWrite)?;
                }
            }
            Err(error) => {
                let reason = error.to_string();
                for file in &files {
                    items.push(RenderedItem {
                        artifact: RenderedArtifact::Trampoline(file.clone()),
                        outcome: RenderOutcome::Skipped {
                            path: bin.join(file),
                            reason: reason.clone(),
                        },
                    });
                }
            }
        }
    }

    // C-044's prune half. An absent `bin/` reads as empty; an entry whose name
    // is not valid UTF-8 has no `RenderedArtifact` and is therefore neither
    // reported nor removed (see [`RenderedArtifact`]).
    for name in read_dir_utf8_names(&bin).await {
        if is_expected(&expected, &name, case_insensitive) {
            continue;
        }
        let artifact = RenderedArtifact::Trampoline(name);
        items.push(RenderedItem {
            outcome: prune_outcome(home, &artifact, request.dry_run).await,
            artifact,
        });
    }

    Ok(landed)
}

/// Publish one exposed name's `bin/` entry; `Ok(true)` when this call wrote.
///
/// The platform split is a runtime branch, not a `cfg`: both producers and both
/// publishers are cross-platform, and the Windows arm's ordering postcondition
/// is the one property in it worth a test.
async fn publish_bin_entry(
    bin: &Path,
    name: &str,
    body: &str,
    sidecar: &str,
    shim_bin: &ShimBinStore,
    dry_run: bool,
) -> crate::Result<bool> {
    if cfg!(windows) {
        let exe = bin.join(format!("{name}.exe"));
        let sidecar_path = bin.join(format!("{name}.exec"));
        // The blob is what `<name>.exe` must *be* — see
        // [`windows_pair_unchanged`]. `ensure()` is idempotent and costs one
        // stat once the blob is published, but it does publish, so a dry run
        // does not call it and settles for the weaker predicate: a dry run
        // writes no stamp, so nothing it calls `Unchanged` is ever blessed.
        let blob = if dry_run { None } else { Some(shim_bin.ensure().await?) };

        let (exe_probe, sidecar_probe) = (exe.clone(), sidecar_path.clone());
        let sidecar_bytes = sidecar.to_string();
        let unchanged = blocking(exe.clone(), move || {
            windows_pair_unchanged(&exe_probe, &sidecar_probe, &sidecar_bytes, blob.as_deref())
        })
        .await?;
        if unchanged {
            return Ok(false);
        }
        if !dry_run {
            publish_windows_trampoline(shim_bin, &exe, &sidecar_path, sidecar).await?;
        }
        return Ok(true);
    }

    let path = bin.join(name);
    let body = body.to_string();
    blocking(path.clone(), move || {
        let existing = read_existing_trampoline(&path, body.len() as u64)?;
        // The mode is part of the predicate, not only the bytes: a clone
        // committing `bin/<name>` with the exact expected body at git mode
        // `100644` would otherwise stay non-executable through every later
        // render, since `write_trampoline_atomic` is the only thing that sets
        // the bit and it is never reached.
        if existing.as_deref() == Some(body.as_bytes()) && has_execute_bit(&path) {
            return Ok(false);
        }
        if !dry_run {
            write_trampoline_atomic(&path, &body)?;
        }
        Ok(true)
    })
    .await
}

/// Whether the Windows pair already on disk is the pair this render would
/// publish — the `.exec` sidecar's **bytes** and the `.exe`'s **file
/// identity**, never the `.exe`'s mere existence.
///
/// The existence-only form defeats the ADR's R1 mitigation. A sidecar body is
/// one line — the absolute project root plus `\n` — so it is predictable on a
/// CI runner, and a hostile clone shipping a substituted `bin/<name>.exe`
/// beside a matching `<name>.exec` would be reported [`RenderOutcome::Unchanged`],
/// never republished, and would land in `landed` — where `fingerprint_bin`
/// stamps it and C-061's gate then blesses an attacker-authored executable onto
/// `PATH`. The POSIX arm has no such hole: it byte-compares the body.
///
/// The identity is derived through
/// [`BinEntryStamp::from_metadata`](crate::file_structure::BinEntryStamp::from_metadata),
/// the one derivation the stamp itself uses, so the publish-time compare and
/// C-061's gate cannot disagree about what "the same file" means.
///
/// `blob` is `None` only under a dry run, which must not `ensure()` the blob
/// into existence (C-049); the predicate then falls back to the weaker
/// is-a-regular-file half, which is sound there because a dry run writes no
/// stamp and blesses nothing.
///
/// Not `#[cfg(windows)]`, for the reason [`publish_windows_trampoline`] states
/// at length: a `cfg`-gated seam puts the property behind a `cfg` the CI leg
/// that actually runs never compiles.
///
/// # Errors
///
/// The sidecar read's own I/O failure — see [`read_existing_trampoline`].
fn windows_pair_unchanged(exe: &Path, sidecar_path: &Path, sidecar: &str, blob: Option<&Path>) -> crate::Result<bool> {
    let existing = read_existing_trampoline(sidecar_path, sidecar.len() as u64)?;
    if existing.as_deref() != Some(sidecar.as_bytes()) {
        return Ok(false);
    }
    match blob {
        // A platform that reports no file identity at all falls through to the
        // write branch rather than guessing — the same fail-closed direction
        // `BinEntryStamp::file_id`'s `None` takes.
        Some(blob) => Ok(matches!(
            (file_identity(exe), file_identity(blob)),
            (Some(published), Some(expected)) if published == expected
        )),
        None => Ok(file_identity(exe).is_some()),
    }
}

/// One file's filesystem identity — the inode on Unix, the file index on
/// Windows — or `None` when the path is absent, is not a regular file, or the
/// platform reports none.
///
/// Derived through [`BinEntryStamp::from_metadata`] rather than by a second
/// `MetadataExt` call, because that function's doc names itself "the one
/// derivation of `file_id`, on purpose": the two ends of C-061's gate must
/// agree on what a file identity is, and this is the publish-side end. The
/// content hash is not part of this question, so it is passed empty and
/// dropped.
fn file_identity(path: &Path) -> Option<u64> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    BinEntryStamp::from_metadata(path, &metadata, String::new()).file_id
}

/// Whether `path` carries an execute bit for anyone — the mode half of the
/// POSIX [`RenderOutcome::Unchanged`] predicate (R-W19).
///
/// `mode & 0o111 != 0` rather than an equality against `0o755`: the render
/// publishes `0o755`, but a stricter umask or a later `chmod` may narrow it to
/// `0o700` without making the trampoline any less correct, and a render that
/// rewrote the file on every invocation for that would break C-047's
/// idempotence.
#[cfg(unix)]
fn has_execute_bit(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
}

/// Windows has no mode bits, and this arm never runs there anyway — the caller
/// branches on the host first.
#[cfg(not(unix))]
fn has_execute_bit(_path: &Path) -> bool {
    true
}

/// Reconcile every selected group's `<group>/<entry>` links, then the orphans
/// under `links/` — entries first, so a departed group's directory empties and
/// can be removed — then the home root's own depth-1 names.
///
/// `case_insensitive` reaches the **prune** comparisons and nothing else
/// (RUL-46) — see [`is_expected`]. The write pass is untouched: group and entry
/// names come from `ocx.lock`, not from a claim walk, so there is no
/// last-walked-wins fold to apply to them.
async fn reconcile_links(
    file_structure: &FileStructure,
    request: &RenderRequest<'_>,
    case_insensitive: bool,
    items: &mut Vec<RenderedItem>,
) -> Result<(), PackageErrorKind> {
    let home = request.home;
    let mut selected: BTreeSet<&str> = BTreeSet::new();
    for group in request.groups {
        selected.insert(group.as_str());
    }

    for group in &selected {
        let mut expected: BTreeSet<String> = BTreeSet::new();
        for tool in request.lock.tools.iter().filter(|tool| tool.group == **group) {
            // `ocx.lock` is a file a hostile clone ships, so its group and tool
            // keys are validated as path components before anything is written
            // for them (D-V14, exit 78).
            let entry = home.entry(&tool.group, &tool.name)?;
            expected.insert(tool.name.clone());

            // RUL-31: `select_best`, never an exact key lookup. No compatible
            // key for this host skips the entry silently — not a link, not a
            // `Skipped` item, not an error.
            let Some(target) = link_target(file_structure, tool, request.platform) else {
                continue;
            };
            items.push(RenderedItem {
                artifact: RenderedArtifact::Link {
                    group: tool.group.clone(),
                    entry: tool.name.clone(),
                },
                outcome: publish_link(&entry, &target, request.dry_run).await,
            });
        }

        for name in read_dir_utf8_names(&home.links_group(group)?).await {
            if is_expected(&expected, &name, case_insensitive) {
                continue;
            }
            let artifact = RenderedArtifact::Link {
                group: (*group).to_string(),
                entry: name,
            };
            items.push(RenderedItem {
                outcome: prune_outcome(home, &artifact, request.dry_run).await,
                artifact,
            });
        }
    }

    // `links/`'s own orphans — validation item 31's fourth class, one level
    // lower than it used to sit. Keyed on the **lock**, not on
    // `request.groups`: a group the invocation did not select is not an orphan,
    // it is simply out of scope (C-045).
    let locked_groups: BTreeSet<&str> = request.lock.tools.iter().map(|tool| tool.group.as_str()).collect();
    for name in read_dir_utf8_names(&links_root(home)).await {
        if is_expected(&locked_groups, &name, case_insensitive) {
            continue;
        }
        // The departed group's own entries first, or the directory never
        // empties: `remove_dir` is non-recursive, so without this pass every
        // render reports the same `ENOTEMPTY` skip forever and S-001's "the
        // rendered tree matches the new lock exactly" is unreachable for a
        // branch switch that drops a `[group.…]` table.
        //
        // **No new delete primitive.** Each entry takes the identical
        // `RenderedArtifact::Link` prune a *selected* group's unexpected names
        // already take, so what may be removed is decided in one place for both
        // — [`prune_within`], which resolves the parent, refuses anything
        // escaping the home, and refuses a name that is not a link at all. A
        // foreign file therefore survives and is *reported*, the `remove_dir`
        // below fails `ENOTEMPTY`, and the directory is reported with its
        // remedy: S-001's error clause, and the RUL-32 asymmetry C-082 draws.
        //
        // The path is joined raw rather than through `links_group`, matching
        // [`artifact_path`] (RUL-40): a group directory whose name the grammar
        // validator refuses is exactly the leftover worth scanning, and
        // containment is [`prune_within`]'s job, not this one's.
        for entry in read_dir_utf8_names(&links_root(home).join(&name)).await {
            let artifact = RenderedArtifact::Link {
                group: name.clone(),
                entry,
            };
            items.push(RenderedItem {
                outcome: prune_outcome(home, &artifact, request.dry_run).await,
                artifact,
            });
        }
        let artifact = RenderedArtifact::GroupDirectory(name);
        items.push(RenderedItem {
            outcome: prune_outcome(home, &artifact, request.dry_run).await,
            artifact,
        });
    }

    // Depth 1: a **closed-set comparison** against the tree's own names, and
    // nothing else (C-074). The lock is deliberately not consulted here —
    // groups are no longer depth-1 entries, so a directory named after a locked
    // group is a leftover of the pre-`links/` layout and is pruned like any
    // other, not kept because the lock happens to mention its name.
    //
    // `TREE_OWN_DEPTH1_NAMES`, never a name derived from an accessor:
    // `Path::file_name` yields the string `"bin"` for `bin()` and for
    // `shell_bin()` alike, so a keep-set built that way would keep a legacy
    // `bin/` at the root forever and no rename of the accessor could red it.
    //
    // The fold is unconditional, unlike `is_expected`'s: `LINKS` beside `links`
    // is the same directory on a case-insensitive host, and a case-sensitive
    // comparison there would report the tree's own directory as an orphan and
    // `remove_dir` it.
    for name in read_dir_utf8_names(home.root()).await {
        if TREE_OWN_DEPTH1_NAMES
            .iter()
            .any(|kept| kept.eq_ignore_ascii_case(&name))
        {
            continue;
        }
        let artifact = RenderedArtifact::RootEntry(name);
        items.push(RenderedItem {
            outcome: prune_outcome(home, &artifact, request.dry_run).await,
            artifact,
        });
    }

    Ok(())
}

/// Whether `expected` retains `name` through the prune pass, ASCII-case-folded
/// when the home is on a case-insensitive filesystem (RUL-46).
///
/// **The prune side of the fold, and only the prune side.** The write pass
/// keeps the winner's original spelling (RUL-19), so `expected` holds `Make`
/// while the file already on disk may be named `make` — one and the same file
/// there. A case-sensitive `contains` then sees `make` ∉ {`Make`}, removes the
/// entry the write pass had just left `Unchanged`, and `fingerprint_bin` stats
/// a file that no longer exists: the stamp records an empty name set that
/// matches the now-empty `bin/`, so C-061's gate is *satisfied* while the tool
/// has silently vanished. The exact-hit fast path is kept, so the linear scan
/// runs only on a case-insensitive host and only for a name that is not
/// already an exact member.
fn is_expected<S>(expected: &BTreeSet<S>, name: &str, case_insensitive: bool) -> bool
where
    S: std::borrow::Borrow<str> + Ord,
{
    expected.contains(name)
        || (case_insensitive
            && expected
                .iter()
                .any(|candidate| candidate.borrow().eq_ignore_ascii_case(name)))
}

/// Write or repoint one `<group>/<entry>` link, or report why not (C-050).
///
/// Every probe and every write is one blocking unit: this runs on `ocx pull`'s
/// async path, and `read_link`, `symlink_metadata` and the atomic replace are
/// all blocking syscalls.
async fn publish_link(entry: &Path, target: &Path, dry_run: bool) -> RenderOutcome {
    let (entry, target) = (entry.to_path_buf(), target.to_path_buf());
    let context = entry.clone();
    match blocking(context.clone(), move || {
        Ok(publish_link_within(&entry, &target, dry_run))
    })
    .await
    {
        Ok(outcome) => outcome,
        Err(error) => RenderOutcome::Skipped {
            path: context,
            reason: error.to_string(),
        },
    }
}

/// [`publish_link`]'s body, as one synchronous unit.
///
/// # A symlinked `<group>/` directory is refused, not written through
///
/// [`prune_within`] already refuses an entry reached through a symlinked group
/// directory; without the same refusal here the *write* side has no
/// counterpart, and [`ensure_home_root`] guards the tree's own directories,
/// never a `links/<group>` whose name comes from the lock.
/// `create_dir_all` succeeds silently through the link, and
/// [`replace_atomic`](crate::symlink::replace_atomic)'s POSIX arm is a `rename`
/// that replaces an existing regular file — so a bare `ocx pull` against a
/// hostile clone that committed `<home>/<group>` as a symlink would write into
/// an attacker-chosen directory and destroy a file there. The refusal is
/// C-050's skip, never an error (C-050's own trigger list already names a
/// foreign-owned directory), and it sits before the `dry_run` return so a dry
/// run predicts the skip rather than promising a write that will not happen.
fn publish_link_within(entry: &Path, target: &Path, dry_run: bool) -> RenderOutcome {
    if std::fs::read_link(entry).is_ok_and(|current| current == target) {
        return RenderOutcome::Unchanged;
    }
    if let Some(parent) = entry.parent()
        && std::fs::symlink_metadata(parent).is_ok_and(|metadata| metadata.is_symlink())
    {
        return RenderOutcome::Skipped {
            path: entry.to_path_buf(),
            // `render_chain`, not `to_string`: `Error::InternalFile` carries its
            // cause by `#[source]` alone, so `to_string` here would name the
            // path and nothing else — a refusal the user cannot act on.
            reason: crate::error::render_chain(&refuse_symlink(parent)),
        };
    }
    if dry_run {
        return RenderOutcome::Written;
    }
    let published = (|| {
        if let Some(parent) = entry.parent() {
            // Owner-only at create time, like the home root and `bin/`
            // (R-W19(a)) — not the ambient umask. `ensure_link_group`, never
            // `create_owner_only`: the recursive create follows a symlinked
            // `links/` on the way to the group, and the check above judges only
            // the group itself.
            ensure_link_group(parent)?;
        }
        // C-082 — dispatch on the *observed* kind before renaming. A
        // symlink-dereferencing copy (`cp -rL`, `unzip`, `rsync` without `-l`,
        // Docker `COPY`) leaves a real directory at this name, and `rename(2)`
        // fails `EISDIR` against one whether it is empty or not — so without
        // this the entry reports `Skipped` on every render forever and the
        // composing lane degrades to digest paths (C-067) with no route back.
        //
        // `remove_dir`, never `remove_dir_all`: both components come from
        // `ocx.lock`, so RUL-32 applies in full. An empty directory therefore
        // heals; a real package copy is left byte-for-byte intact and earns the
        // remedy below. Every other kind — a symlink (which
        // `symlink_metadata` does not follow), a regular file, an unstattable
        // path — falls through untouched, because `rename` already replaces a
        // non-directory and no observation alone may become a refusal.
        if occupied_by_directory(entry) {
            let _ = std::fs::remove_dir(entry);
        }
        // `replace_atomic`, never `symlink::update`: the update is
        // remove-then-create, which drops the link for an instant and makes
        // C-067's per-entry degrade fire on a link that is merely mid-repoint.
        crate::symlink::replace_atomic(target, entry)
    })();
    match published {
        Ok(()) => RenderOutcome::Written,
        Err(error) => RenderOutcome::Skipped {
            path: entry.to_path_buf(),
            // Re-observed rather than remembered, so the rename stays the
            // authority: an entry that became a directory after the check
            // above still earns the remedy, and one that stopped being one
            // gets its real error instead.
            reason: if occupied_by_directory(entry) {
                refuse_dereferenced_copy(entry)
            } else {
                crate::error::render_chain(&error)
            },
        },
    }
}

/// A **real** directory at `path` — not a symlink to one, and not a path that
/// could not be stat'd.
///
/// `symlink_metadata` does not follow, which is the whole point: a link
/// pointing at a directory is publishable by the rename and must never be
/// removed, and `remove_dir` is therefore never reachable for a path whose
/// target is someone else's tree. A failed stat reads as "no", so a permission
/// error keeps today's diagnostic instead of gaining a remedy nothing observed.
fn occupied_by_directory(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir())
}

/// C-082's remedy for a `<group>/<entry>` a dereferencing copy turned into a
/// real directory that a non-recursive `remove_dir` could not take.
///
/// A plain `String`, not a [`crate::Error`]: the reason has to *be* the remedy,
/// and `Error::InternalFile`'s `Display` names only its path. The path is
/// interpolated bare because
/// [`skipped_render_warnings`](crate::package_manager::skipped_render_warnings)
/// escapes the whole reason exactly once (RUL-52) — escaping it here too would
/// double it.
fn refuse_dereferenced_copy(entry: &Path) -> String {
    format!(
        "a directory occupies this link's name — the mark of a symlink-dereferencing copy \
         (`cp -rL`, `unzip`, `rsync` without `-l`, Docker `COPY`); it is left in place rather \
         than deleted recursively. Remove '{}' and run `ocx pull` again",
        entry.display()
    )
}

/// The digest root one locked tool's link must name on this host, or `None`
/// when the lock ships no compatible platform (RUL-31).
///
/// `pub(crate)` for WP-15's C-067, whose per-entry degrade asks exactly this
/// question — does the link on disk name the lock-derived digest root? — and
/// would otherwise become a third producer of the `host_leaf_identifier` →
/// `select_best` → `packages.path` arithmetic. RUL-31 exists to stop the render
/// and the heal disagreeing about a compatible-but-non-identical platform key;
/// a third spelling would reopen it from a third direction.
pub(crate) fn link_target(
    file_structure: &FileStructure,
    tool: &LockedTool,
    platform: &oci::Platform,
) -> Option<PathBuf> {
    // The shared `select_best` / `is_compatible` helper, applied to exactly this
    // map. An ambiguous lock is skipped for the same reason a missing one is:
    // one unavailable tool must not fail the whole render.
    let identifier = crate::project::compose::host_leaf_identifier(tool, platform).ok()?;
    let pinned = oci::PinnedIdentifier::try_from(identifier).ok()?;
    Some(file_structure.packages.path(&pinned))
}

/// [`prune_within`] as a reportable outcome — a dry run predicts the removal
/// without performing it, and a failure is C-050's skip.
///
/// **Under `dry_run` the `Pruned` answer is an approximation**: it is returned
/// before [`prune_within`] is consulted at all, so `--dry-run` promises a
/// removal that a non-empty group directory (`ENOTEMPTY`, RUL-32) or a
/// containment refusal will report `Skipped` when the real run applies it.
/// Consulting the delete path to find out would mean performing it.
///
/// One blocking unit per artifact: [`prune_within`] does two
/// `dunce::canonicalize` calls and a removal, and this runs on `ocx pull`'s
/// async path.
async fn prune_outcome(home: &ToolchainHome, artifact: &RenderedArtifact, dry_run: bool) -> RenderOutcome {
    if dry_run {
        return RenderOutcome::Pruned;
    }
    let path = artifact_path(home, artifact);
    let (root, owned) = (home.root().to_path_buf(), artifact.clone());
    let pruned = blocking(path.clone(), move || prune_within(&ToolchainHome::new(root), &owned)).await;
    match pruned {
        Ok(()) => RenderOutcome::Pruned,
        // The reason has to *be* the remedy, the same way
        // [`refuse_dereferenced_copy`]'s does: `Error::InternalFile`'s `Display`
        // names only its path, so `to_string` here would report a group
        // directory the render cannot remove without ever saying why or what to
        // do about it. `render_chain` supplies the cause — `ENOTEMPTY` for the
        // one that actually happens, a containment refusal for the rest — and
        // the sentence supplies the action.
        Err(error) => RenderOutcome::Skipped {
            reason: format!(
                "{} — it is left in place rather than deleted recursively. Remove '{}' and run `ocx pull` again",
                crate::error::render_chain(&error),
                path.display()
            ),
            path,
        },
    }
}

/// The UTF-8 entry names of `directory`, sorted; an absent or unreadable
/// directory reads as empty, and a name that is not valid UTF-8 is dropped —
/// it has no [`RenderedArtifact`] and is therefore never pruned.
async fn read_dir_utf8_names(directory: &Path) -> Vec<String> {
    let Ok(mut entries) = tokio::fs::read_dir(directory).await else {
        return Vec::new();
    };
    let mut names = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        if let Some(name) = entry.file_name().to_str() {
            names.push(name.to_string());
        }
    }
    names.sort();
    names
}

/// The path one artifact occupies under `home`, spelled without the grammar
/// validator (RUL-40) — containment is [`prune_within`]'s job, not this one's.
fn artifact_path(home: &ToolchainHome, artifact: &RenderedArtifact) -> PathBuf {
    match artifact {
        RenderedArtifact::Trampoline(name) => home.shell_bin(DEFAULT_SHELL).join(name),
        RenderedArtifact::Link { group, entry } => links_root(home).join(group).join(entry),
        RenderedArtifact::GroupDirectory(group) => links_root(home).join(group),
        RenderedArtifact::RootEntry(name) => home.root().join(name),
    }
}

/// Write the render stamp, last (C-048); `true` when this call wrote it.
///
/// A failed stamp write is C-050's skip like every other write failure: the
/// prompt gate then withholds, which is the conservative answer.
///
/// Blocking, and substantially so — a stat, a bounded read and a SHA-256 per
/// landed entry, a `read_dir`/`read_link` pass over the default group, and
/// `StateStore::{render_stamp, set_render_stamp}`, whose own docs say async
/// callers wrap them. [`render_with`] runs the whole of it as one
/// [`blocking`] unit.
fn write_render_stamp(
    state: &crate::file_structure::StateStore,
    home: &ToolchainHome,
    scope: &RenderStampScope,
    bin_in_scope: bool,
    landed: &BTreeSet<String>,
) -> bool {
    let key = stamp_key(scope);
    let target = match scope {
        RenderStampScope::Global => RenderStampTarget::Global,
        RenderStampScope::Project(_) => RenderStampTarget::Project(&key),
    };

    // RUL-25/RUL-27: a render that did not reconcile `bin/` knows nothing about
    // its correct name set, so it carries the previous stamp's half forward
    // rather than claiming an empty directory it never read.
    let bin_fingerprint = if bin_in_scope {
        // The **physical** directory (C-080). This is a *producer*: a stamp
        // written through `active` certifies whatever `active` points at, and
        // `bin_matches_recorded` — the second, independent guard — then passes
        // by agreeing with it. Re-anchoring the readers and not this would be
        // strictly worse than re-anchoring neither.
        fingerprint_bin(&home.shell_bin(DEFAULT_SHELL), landed)
    } else {
        state
            .render_stamp(target)
            .map(|previous| previous.bin_fingerprint)
            .unwrap_or_default()
    };

    let stamp = RenderStamp::new(
        home.root().to_path_buf(),
        scope.clone(),
        bin_fingerprint,
        observe_default_group_links(home),
    );
    match state.set_render_stamp(target, &stamp) {
        Ok(()) => true,
        Err(error) => {
            crate::log::warn!(
                "Toolchain '{}' was rendered but its render stamp was not written: {error}",
                home.root().display()
            );
            false
        }
    }
}

/// One `bin_fingerprint` entry per on-disk file that landed (RUL-22, RUL-26).
///
/// Keyed by the full on-disk file name, so a Windows name's `<name>.exe` and
/// `<name>.exec` are two entries — which is what C-061's `readdir` gate
/// compares against.
///
/// The stat and the hash both go through
/// [`BinEntryStamp::of_file`](crate::file_structure::BinEntryStamp::of_file) —
/// the one derivation this producer shares with the prompt gate that checks its
/// output (`activation::bin_stamp_matches`, RUL-89). Spelling the read and the
/// digest here as well would put the same contract in two files, and the two
/// only ever meet through the persisted stamp.
fn fingerprint_bin(bin: &Path, landed: &BTreeSet<String>) -> BTreeMap<String, BinEntryStamp> {
    let mut fingerprint = BTreeMap::new();
    for name in landed {
        let path = bin.join(name);
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let Some(stamp) = BinEntryStamp::of_file(&path, &metadata) else {
            continue;
        };
        fingerprint.insert(name.clone(), stamp);
    }
    fingerprint
}

/// The default group's links **as they stand on disk** (RUL-27) — one
/// `readlink` pass, no compose, no metadata read, no network.
///
/// Observed rather than derived from the lock precisely so a `pinned` render,
/// which writes and prunes nothing, still stamps a `link_fingerprint` that
/// matches the tree C-061's gate is about to read.
fn observe_default_group_links(home: &ToolchainHome) -> BTreeMap<String, String> {
    let mut fingerprint = BTreeMap::new();
    let Ok(group_directory) = home.links_group(DEFAULT_GROUP) else {
        return fingerprint;
    };
    let Ok(entries) = std::fs::read_dir(group_directory) else {
        return fingerprint;
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Ok(target) = std::fs::read_link(entry.path()) else {
            continue;
        };
        fingerprint.insert(format!("{DEFAULT_GROUP}/{name}"), target.to_string_lossy().into_owned());
    }
    fingerprint
}

/// The on-disk file names one exposed name occupies in `bin/`.
///
/// One on POSIX; **two** on Windows — `<name>.exe` plus its `<name>.exec`
/// sidecar (RUL-26).
fn trampoline_file_names(name: &str) -> Vec<String> {
    if cfg!(windows) {
        vec![format!("{name}.exe"), format!("{name}.exec")]
    } else {
        vec![name.to_string()]
    }
}

/// Run one blocking filesystem unit off the runtime, flattening the join.
///
/// `context` is the path the failure is attributed to when the blocking task
/// itself dies — a panic or a shutting-down runtime, neither of which the unit
/// under it can report.
async fn blocking<T, F>(context: PathBuf, work: F) -> crate::Result<T>
where
    F: FnOnce() -> crate::Result<T> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(result) => result,
        Err(join_error) => Err(crate::error::file_error(&context, std::io::Error::other(join_error))),
    }
}

/// What one [`heal_links`] call did to one toolchain home.
///
/// Two answers rather than one count, because the two states a count folded
/// together are opposites. `Healed(0)` is "I walked this tree and every link
/// was already correct"; [`Self::Refused`] is "I never entered it". While both
/// were `Ok(0)` no caller could tell them apart — the degrade-to-`Ok` shape
/// that leaves a security guard protecting only the write path: a hostile
/// clone that commits `.ocx` or `.ocx/toolchain` as a symlink has every
/// *write* refused here and then, on the strength of the same `Ok(0)`, has
/// the composer read *through* that symlink into `PATH` and `${installPath}`.
///
/// `#[must_use]` is the load-bearing half. `heal_links(..).await?;` as a
/// statement is how both callers used to spend the count, and it is exactly
/// the shape that discards a refusal; with the attribute the workspace's
/// `warnings = "deny"` turns that statement into a build failure, so a caller
/// that ignores the refusal cannot be written by accident.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use = "a refused tree was never entered — reading through it composes an attacker's links"]
pub(crate) enum HealOutcome {
    /// The tree was walked and this many links were repointed (C-051).
    ///
    /// Zero is an ordinary answer: nothing was stale, or every stale entry hit
    /// a condition RUL-36 degrades (a lock timeout, a read-only tree, a
    /// `readlink` that failed). Those stay uncounted *inside* this variant —
    /// they are per-entry states the composing side already degrades one entry
    /// at a time (C-067), not a verdict on the tree.
    Healed(usize),
    /// A whole-tree gate refused, and **nothing under `home` was read or
    /// written**.
    ///
    /// Either symlink guard ([`refuse_symlinked_project_path`] on the
    /// components above the root, [`ensure_home_root`] on the root and every
    /// directory the tree owns),
    /// or that same call's ordinary I/O failure — a root that could not be
    /// created. One variant for all of them because they carry one meaning for
    /// a caller: this call verified nothing about the tree, so nothing under it
    /// may be trusted on the strength of it.
    Refused {
        /// Why, rendered for a `debug` line.
        ///
        /// **Human-facing prose with no grammar**, the same rule
        /// [`RenderOutcome::Skipped::reason`] states: nothing parses it. The
        /// path is not carried beside it — unlike a render, a heal has exactly
        /// one tree and every caller passed it in.
        reason: String,
    },
}

/// Repoint every stale `<group>/<entry>` link in `groups` at the digest root
/// `lock` names, and report how many were repointed (C-051) — or that the tree
/// was refused and never entered ([`HealOutcome`]).
///
/// One `readlink` per entry, compared against the lock-derived digest root,
/// and a [`lock_scoped`](crate::utility::fs::lock_scoped) repoint on
/// mismatch. **Pure lock arithmetic: no compose, no metadata read, no
/// network** — that is what keeps the per-prompt caller inside the latency
/// ceiling, and it is a property of this function, not of its callers.
///
/// The digest root is selected through the same
/// [`select_best`](crate::oci::platform::select_best) /
/// [`is_compatible`](crate::oci::platform::is_compatible) helper the render uses
/// — `project::resolve::lookup_host_leaf` — and never by an exact key lookup on
/// `platform`'s rendering (RUL-31). The two must agree or a compatible-but-
/// non-identical key makes the render write one target and this function
/// repoint it to another, on every invocation, forever. No compatible key for
/// this host skips the entry silently: not a repair, not a failure, not counted.
///
/// # An absent link is created, not skipped (RUL-29)
///
/// "Heal" covers three states, not two: a link pointing at the wrong digest root
/// is repointed, a link pointing at the right one is left alone, and an **absent**
/// link is **created**. The absent case is not an edge — it is the commonest
/// post-`git pull` state, since a lock that gained an entry has no link for it
/// until something writes one, and on the composing path (C-070) nothing else
/// will before the emit. Skipping it would leave the one purpose C-070 states —
/// that the composing emitter can trust the link it is about to emit —
/// unsatisfied precisely when it matters. A created link counts toward the
/// returned total, like a repointed one.
///
/// # The group set is a parameter, and that is the whole point (C-070)
///
/// Two callers pass different sets and the difference is deliberate:
///
/// - **The `bin`-mode prompt path passes the default group only** (C-062). The
///   narrowing is a cost decision, and it is safe *there* because `bin/`
///   exposes the default group and nothing else.
/// - **A composing emitter passes every group the invocation selected** —
///   `-g`, else [`DEFAULT_GROUP`](crate::project::DEFAULT_GROUP) — before it
///   emits any link path (C-070). Without that, `ocx exec -g ci -- <cmd>` in
///   the following lane composes through a stale `ci/<entry>` link after a
///   branch switch and runs the previous package while the lock says
///   otherwise. C-062's narrowing belongs to the non-composing prompt path
///   alone and must not leak here.
///
/// **An empty `groups` is a vacuous no-op: [`HealOutcome::Healed(0)`](HealOutcome::Healed),
/// indistinguishable from "every link was already correct" — and it is
/// documented rather than refused.** Too narrow a set is C-070's exact failure mode, so the choice is
/// worth stating: a refusal would put an error on the one path C-051 says
/// never errors, and it would not catch the narrowing that actually bites
/// (passing the default group when the invocation selected `ci`), which is a
/// non-empty wrong answer. Every caller derives its set from `-g`-else-
/// `DEFAULT_GROUP` and can only reach empty by building an empty `Vec` itself;
/// the Specify phase asserts each caller's set instead.
///
/// # Nothing here errors on contention (RUL-36)
///
/// A link this call cannot repair is left alone rather than deleted; the
/// composing side degrades that entry to a digest path (C-067), so a mismatch is
/// never an error. **That extends to the repair itself**: a lock timeout, a
/// read-only tree, a foreign-owned directory, a `readlink` that fails — every
/// one of them leaves the entry unrepaired, **uncounted in the returned total**,
/// and logged at `debug`. The call still returns
/// [`HealOutcome::Healed`](HealOutcome::Healed).
///
/// # A whole-tree refusal is not one of them (the return type's whole point)
///
/// Everything above is *per entry*, and the composing side degrades per entry
/// (C-067). The two symlink gates below are not per entry: they decide whether
/// this call enters the tree at all, and while their refusal also spelled
/// itself `Ok(0)` it was indistinguishable from "every link was already
/// correct". That is the shape that leaves a security guard defending only the
/// write path — a hostile clone committing `.ocx/toolchain` as a symlink got
/// every write refused and then had the composer read *through* the same
/// symlink into `PATH`. It is now [`HealOutcome::Refused`], and
/// [`HealOutcome`] is `#[must_use]`, so a caller that drops it does not
/// compile.
///
/// This settles a contradiction the first draft shipped: the prose said "a
/// mismatch is never an error" while the `# Errors` block listed
/// `Internal — the lock acquisition … failure`. The `# Errors` reading is the
/// wrong one, and not marginally: C-070 puts this function on `ocx env`'s and
/// `ocx exec`'s critical path, so under it two shells composing at once would
/// fail one of them outright — an ordinary contention turned into a failed
/// command, on the path whose entire design premise is that it degrades instead.
/// The returned count stays meaningful either way, because it counts repairs
/// that landed rather than mismatches that were seen.
///
/// # Why the signature is not the one C-051 wrote (contract widening)
///
/// C-051 names `heal_links(home, lock, groups) -> Result<usize>`. Both halves
/// are widened here.
///
/// The **return type** is [`HealOutcome`] rather than `usize` because `usize`
/// cannot express the answer the security guards below produce. Their refusal
/// spelled itself `Ok(0)`, which is also what a tree whose links were all
/// already correct returns — so no caller could distinguish "I refused to enter
/// this tree" from "I walked it and found nothing to do". Both callers
/// (`ToolchainLinks`' composer and `activation::bin_mode_entry`) go on to
/// *read* the same tree, and reading a refused tree is how an attacker's link
/// reaches `PATH`. A per-call-site guard fixes one caller; the return type
/// fixes every caller, present and future, because [`HealOutcome`] is
/// `#[must_use]`.
///
/// Three **inputs** the function cannot be spelled without are added rather
/// than re-derived:
/// `file_structure` supplies both the `packages/` root the digest path is
/// built from and the `locks/` root `lock_scoped` requires, and `platform`
/// selects which of a [`LockedTool`](crate::project::LockedTool)'s
/// per-platform leaf digests the link must name. Deriving either locally would
/// be a second spelling of a store path — the rule
/// `subsystem-file-structure.md` states as Block-tier.
///
/// `scope` is the third, and it is a **security** input rather than an
/// arithmetic one (RUL-47): it is what lets this function run
/// [`refuse_symlinked_project_path`], and a [`ToolchainHome`] alone cannot
/// answer the question that guard asks — whether the home is a project's own
/// `<project>/.ocx/toolchain` or a configured `toolchain-dir` / global home,
/// which is the difference between a symlinked parent being an attack and
/// being an ordinary user setup. Heal is the *more* frequently reached of the
/// two writers, since C-070 puts it on every composing emit rather than on
/// `ocx pull` alone, so the guard the render takes has to be reachable here
/// too.
///
/// # Errors
///
/// [`PackageErrorKind::ToolchainPath`], and nothing else — a group key from
/// `ocx.lock` or a selected group name that cannot become a path component
/// (exit 78). `ocx.lock` is a file a hostile clone ships, which is why this
/// validation is on the grammar and not only at `ocx.toml` parse (D-V14), and
/// why it stays an error when every I/O condition is not: it is bad
/// *configuration data*, refused before any path is touched, not a transient
/// state a retry would clear.
pub(crate) async fn heal_links(
    file_structure: &FileStructure,
    home: &ToolchainHome,
    scope: &RenderStampScope,
    lock: &ProjectLock,
    groups: &[String],
    platform: &oci::Platform,
) -> Result<HealOutcome, PackageErrorKind> {
    // A vacuous no-op, and it performs no I/O at all: an empty set has nothing
    // to compare, so a home that does not exist yet is not created for it.
    if groups.is_empty() {
        return Ok(HealOutcome::Healed(0));
    }

    // Both of the render's gates, in one blocking unit, and for the same
    // reason it takes them: heal is a *writer* — RUL-29 creates absent links
    // and `repoint_link` creates the group directory under them — so without
    // them a hostile clone that committed `.ocx/toolchain`, or `.ocx` one
    // component higher, as a symlink to `$HOME` would make `ocx env` and
    // `ocx exec` write outside the project on **every prompt**, which is
    // strictly more often than `ocx pull` reaches the render (C-070). RUL-33
    // is the root and every directory the tree owns; RUL-44/RUL-47 is every
    // component between the
    // project directory and the root. A refusal degrades, never errors
    // (RUL-36).
    let guarded = ToolchainHome::new(home.root().to_path_buf());
    let guarded_scope = scope.clone();
    match blocking(home.root().to_path_buf(), move || {
        refuse_symlinked_project_path(&guarded_scope, &guarded)?;
        ensure_home_root(&guarded)
    })
    .await
    {
        Ok(_created) => {
            // C-004's ignore file, immediately after the root exists and never
            // before it — the renderer's own root-first ordering, so this
            // write can never be what creates the root under the ambient
            // umask. Heal is the *other* path that creates a home: on a fresh
            // clone with a committed `ocx.lock` and no `ocx pull`, one
            // `ocx env` leaves RUL-29's `<group>/<entry>` symlinks on disk,
            // and without this `git add -A` stages them. Best-effort for the
            // reason the render's Step 3 is: a tree the repository controls
            // may deny the write, and the links are heal's product.
            let gitignore_home = ToolchainHome::new(home.root().to_path_buf());
            if let Err(error) = blocking(home.root().to_path_buf(), move || gitignore_home.ensure_gitignore()).await {
                crate::log::debug!("Toolchain home '{}' has no ignore file: {error}", home.root().display());
            }
        }
        // RUL-36 — still not an error. But it is no longer the same answer a
        // clean pass gives: the caller is told the tree was never entered, so
        // it cannot go on to read through it. The `debug!` line stays here
        // because this is where the underlying error lives; the reason travels
        // with the value so a caller can say so in its own words too.
        Err(error) => {
            crate::log::debug!(
                "Toolchain links under '{}' were not healed: {error}",
                home.root().display()
            );
            return Ok(HealOutcome::Refused {
                reason: error.to_string(),
            });
        }
    }

    // Every candidate named before anything is probed. The grammar refusal
    // therefore still runs before a single path is touched, and the probes
    // that follow become one unit of work rather than N.
    let selected: BTreeSet<&str> = groups.iter().map(String::as_str).collect();
    let mut candidates: Vec<(&str, PathBuf, PathBuf)> = Vec::new();
    for group in &selected {
        for tool in lock.tools.iter().filter(|tool| tool.group == **group) {
            // `ocx.lock` is a file a hostile clone ships: bad configuration
            // data is refused before any path is touched, and it is the one
            // thing here that is an error rather than a degrade.
            let entry = home.entry(&tool.group, &tool.name)?;

            // RUL-31 — the same `select_best` rule the render used, so the two
            // never disagree and oscillate. No compatible key skips silently.
            let Some(target) = link_target(file_structure, tool, platform) else {
                continue;
            };
            candidates.push((tool.name.as_str(), entry, target));
        }
    }

    // **One** blocking unit for every probe, not one per tool: this is
    // `ocx env`'s and `ocx exec`'s per-prompt path (C-070), where the hop
    // dominates the two syscalls it carries — measured ~0.09 ms per tool
    // against `RECONCILE_BUDGET_MS`, which is the budget this path shares.
    // The consumer immediately downstream batches the identical shape for the
    // identical reason (`composer.rs`'s `ToolchainLinks` emit), so this is the
    // house pattern rather than a new one. `read_link`, `exists` and `is_link`
    // still must not run on the async runtime.
    //
    // Compared against the lock, never against the filesystem: a digest root
    // that is not materialised is still the correct target for a lazily-loaded
    // tool. `read_link`, never a canonicalising compare — heal must not read
    // *through* a link into a hostile tree.
    //
    // A shape heal cannot repair — a regular file or a directory where a link
    // belongs — is left exactly as it is. Removing it would give heal a delete
    // path C-051 does not authorise, inside a tree the repository controls.
    //
    // A join failure resolves to "not repairable" for the whole batch — the
    // same answer this pair gives for every state it cannot improve, and the
    // one RUL-36 already prescribes: the entries stay uncounted and the
    // composing side degrades each to a digest path (C-067).
    let probes: Vec<(PathBuf, PathBuf)> = candidates
        .iter()
        .map(|(_, entry, target)| (entry.clone(), target.clone()))
        .collect();
    let repairable = blocking(home.root().to_path_buf(), move || {
        Ok(probes
            .into_iter()
            .map(|(entry, target)| {
                if std::fs::read_link(&entry).is_ok_and(|current| current == target) {
                    return false;
                }
                !entry.exists() || crate::symlink::is_link(&entry)
            })
            .collect::<Vec<bool>>())
    })
    .await
    .unwrap_or_else(|_| vec![false; candidates.len()]);

    let mut repaired = 0usize;
    for ((name, entry, target), repairable) in candidates.iter().zip(repairable) {
        if !repairable {
            continue;
        }
        match repoint_link(file_structure, name, entry, target).await {
            Ok(()) => repaired += 1,
            // RUL-36 — a repair that cannot land leaves the entry unrepaired
            // and uncounted; the composing side degrades it to a digest path
            // (C-067). Never an error.
            Err(error) => crate::log::debug!("Toolchain link '{}' was not repaired: {error}", entry.display()),
        }
    }
    Ok(HealOutcome::Healed(repaired))
}

/// Repoint one `<group>/<entry>` at `target` under its own lock.
///
/// # Errors
///
/// A symlinked `links/` or `<group>/` directory (the same refusal
/// [`publish_link_within`] makes on the render side, through the same
/// [`ensure_link_group`]), the group directory's
/// creation, the lock acquisition, or the atomic replace — every one of which
/// [`heal_links`] turns into an uncounted, un-repaired entry inside
/// [`HealOutcome::Healed`] rather than a failure, with one `debug!` line
/// (RUL-36). A whole-tree refusal is [`HealOutcome::Refused`] instead.
async fn repoint_link(file_structure: &FileStructure, name: &str, entry: &Path, target: &Path) -> crate::Result<()> {
    // The group directory is `entry`'s parent **by construction**:
    // `ToolchainHome::entry` is built on `links_group`, so re-joining the group
    // name onto the root here would be a second spelling of one path (C-072) —
    // and it was, until `links/` moved every group one level down and the two
    // spellings stopped naming the same directory.
    let Some(group_directory) = entry.parent().map(Path::to_path_buf) else {
        return Err(refuse_escape(entry));
    };

    // RUL-29: an absent link is created, which is the commonest post-`git pull`
    // state — so the group directory may not exist yet. Owner-only at create
    // time (R-W19(a)), and never *through* a symlinked `links/` or group
    // directory: heal runs on every composing emit, so this is the write side's
    // most-travelled path into an attacker-shaped tree. `ensure_link_group`
    // carries both refusals, so this path and the render's share one rule.
    let created = group_directory.clone();
    blocking(group_directory.clone(), move || ensure_link_group(&created)).await?;

    let _guard = heal_lock_parameters(file_structure, group_directory, name)
        .acquire()
        .await?;
    let (entry, target) = (entry.to_path_buf(), target.to_path_buf());
    blocking(entry.clone(), move || crate::symlink::replace_atomic(&target, &entry)).await
}

/// Create `home`'s root and every missing ancestor, owner-only-write, and
/// report whether this call created it (R-W19(a)).
///
/// Takes the [`ToolchainHome`], not a bare `&Path`: this module's containment
/// story (C-053) is that every write is addressed through the home, and this
/// is the only function here that creates directories at all — its one correct
/// argument is `request.home`'s own root.
///
/// [`ToolchainRoot::resolve`](crate::config::ToolchainRoot::resolve)
/// deliberately performs no filesystem side effect, so the configured
/// `toolchain-dir` root does not exist until a render creates it — which makes
/// this the one place its permissions are chosen.
///
/// **Owner-only write, on every directory this call creates, set at create
/// time.** Not just the leaf, and not a post-hoc `chmod`, which leaves a window
/// in which the directory exists group-writable. C-019 refuses a
/// group-or-world-writable `toolchain-dir`, but it can only check a directory
/// that exists: for an absent root it examines the *nearest existing ancestor*
/// instead. With `toolchain-dir` set, the missing ancestors are
/// `<toolchain-dir>` and `<toolchain-dir>/<project-key>` — so **every**
/// directory this call creates is one C-019 never examined. A group-writable
/// intermediate lets an attacker rename the leaf out from under the tree,
/// which is the outcome that refusal exists to prevent, and every trampoline
/// below it is an executable on someone's `PATH`.
///
/// # It refuses a symlink at the home root or at any tree-own directory (RUL-33, C-074)
///
/// If [`ToolchainHome::root`](crate::file_structure::ToolchainHome::root),
/// `links/`, `shells/`, `shells/<shell>/` or
/// [`ToolchainHome::shell_bin`](crate::file_structure::ToolchainHome::shell_bin)
/// exists and is a symlink — judged by `symlink_metadata`, never by `metadata`,
/// which follows — this call refuses. `<root>/active` is the deliberate
/// exception: it must *be* a link, so its rule is
/// [`ToolchainHome::active_is_valid`](crate::file_structure::ToolchainHome::active_is_valid). The renderer routes the refusal to C-050's skip
/// with a **loud** warning, not to a hard error: DD6's never-block rule holds,
/// and a `pull` that failed outright would be worse than one that composes
/// digest paths for the run. Loud rather than debug, though, because unlike a
/// read-only checkout this is not a benign state.
///
/// **This is the only thing standing between C-053 and lexical containment for
/// a project home.** `<project>/.ocx/toolchain` never passes through
/// C-017–C-019, which gate a *configured* `toolchain-dir` and nothing else — so
/// a hostile clone that commits `.ocx/toolchain` as a symlink to `$HOME`, or
/// `.ocx/toolchain/links` as a symlink to `~/.local/share`, makes every write
/// and every prune below it land outside the project while every path this
/// module computes still `starts_with` the home. The refusal has to be here, before
/// anything is created: `create_dir_all` on an existing symlink-to-a-directory
/// succeeds silently, so the first write is already outside.
///
/// This is D-V14's argument about `.gitignore` applied one level up — the tree
/// is inside a directory the repository controls, so the *shape* of that
/// directory is untrusted input, not just its contents. [`prune_within`] closes
/// the same hole on the delete side, from the other direction.
///
/// Blocking: `mkdir` and a permission set per created level. Async callers wrap
/// it in `spawn_blocking`, as
/// [`ToolchainHome::ensure_gitignore`](crate::file_structure::ToolchainHome::ensure_gitignore)
/// already documents for itself.
///
/// # Errors
///
/// The creation's or the permission set's own I/O failure, with the path
/// attached, or the symlink refusal above. The renderer turns either into
/// C-050's skip (RUL-24, RUL-33), not into a hard error — see
/// [`PackageManager::render_toolchain`].
pub(crate) fn ensure_home_root(home: &ToolchainHome) -> crate::Result<bool> {
    let root = home.root();
    // Both refusals **before** the creation, because `create_dir_all` on an
    // existing symlink-to-a-directory succeeds silently and the first write is
    // already outside (RUL-33). A root that does not exist yet cannot hold any
    // of the tree's own directories either, so checking them all up front loses
    // nothing.
    refuse_symlinked_home_leaves(home)?;
    let existed = match std::fs::symlink_metadata(root) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(crate::error::file_error(root, error)),
    };
    if !existed {
        create_owner_only(root)?;
    }
    Ok(!existed)
}

/// Refuse a symlink on any component **between a project directory and the
/// home root it contains** (RUL-44), routed by the caller to C-050's skip.
///
/// [`ensure_home_root`] judges the home root and every directory below it that
/// the tree owns, and
/// `symlink_metadata` does not follow only the **last** component — so
/// committing `<project>/.ocx` as a symlink relocates the entire rendered tree
/// while every check either function makes still passes: `symlink_metadata` of
/// `<project>/.ocx/toolchain` reports an ordinary directory, and
/// [`prune_within`]'s containment canonicalises *both* sides, so a home reached
/// through the link is contained in itself. One component higher is a hole
/// neither guard can see from where it stands.
///
/// # Deliberately narrow: project scope, and only a home inside the project
///
/// Applied only when [`RenderStampScope::Project`]'s directory is a lexical
/// prefix of the home root — which is exactly the un-configured
/// `<project>/.ocx/toolchain` shape, the one no `toolchain-dir` key and
/// therefore none of C-017…C-019 ever gated. It is **not** applied to:
///
/// - the **global** home, whose parent is `$OCX_HOME` — a directory a user may
///   entirely legitimately symlink onto another volume, and refusing it would
///   skip every global render on a common benign setup (DD6's never-block rule,
///   and the no-warn-on-common-benign convention);
/// - a configured **`toolchain-dir`** home, which is outside the project
///   directory by construction and already passes through C-017…C-019.
///
/// The components checked are those strictly between the project directory and
/// the home root; the root itself is [`ensure_home_root`]'s, so neither guard
/// restates the other.
///
/// # Errors
///
/// [`refuse_symlink`] for the first symlinked component found.
/// **Every symlink refusal a toolchain home takes, read-only** (RUL-33, RUL-44).
///
/// The one entry point for "may ocx treat this home as its own tree?", shared by
/// the two paths that ask it — the renderer, before it writes, and
/// `activation::project_contribution`, before it widens the per-prompt
/// reconciler's `owned_prefixes` to the home's root.
///
/// # Why the read path needs it at all
///
/// A guard on the write path did nothing for the read path.
/// [`ensure_home_root`] and [`refuse_symlinked_project_path`] both ran only
/// under `ocx pull`, so a repository committing `.ocx/toolchain` as a symlink
/// was refused a *render* and still handed the prompt path a deletion
/// authority: `project::resolve_toolchain_home` builds the default home
/// lexically, `shell::reconcile::plan`'s owned-prefix set canonicalises it —
/// resolving that final component — and ownership is a `starts_with`. A link to
/// `/` therefore owned every `PATH` segment the desired set did not contribute,
/// on a project that needs no gesture at all to be consented under a
/// `namespaces` grant.
///
/// Read-only on purpose: creating anything is [`ensure_home_root`]'s, and the
/// prompt path must never write.
///
/// # Errors
///
/// [`refuse_symlink`] naming the offending component — a symlink between the
/// project directory and the home root, the root itself, or any directory the
/// tree owns below it.
pub(crate) fn refuse_symlinked_home(scope: &RenderStampScope, home: &ToolchainHome) -> crate::Result<()> {
    refuse_symlinked_project_path(scope, home)?;
    refuse_symlinked_home_leaves(home)
}

/// The home root and **every directory of the tree's own shape**, judged by
/// `symlink_metadata` — the components [`refuse_symlinked_project_path`]'s walk
/// deliberately stops short of.
///
/// `<root>/shells/<shell>/bin` is the same hole one level down: every
/// `bin/<name>` write and removal, and every `read_dir` the per-prompt stamp
/// comparison makes, would otherwise land in the link's target directory. The
/// **physical** spelling, never `bin()` — `lstat` does not follow a path's
/// final component but does follow every intermediate one, so judging
/// `<root>/active/bin` resolves through `active` and reports an ordinary
/// directory for whatever it points at (C-080).
///
/// # Why the intermediates, and why this is not padding
///
/// `links/` and `shells/` are components **this layout introduced**, and
/// nothing else judges them. [`publish_link_within`] refuses a symlinked
/// *immediate* parent, which is now `links/<group>` and never `links`;
/// [`create_owner_only`] is `DirBuilder::recursive(true)`, i.e. `create_dir_all`,
/// which follows an existing symlink. A committed
/// `.ocx/toolchain/links -> /outside` would therefore put every group directory
/// and every entry link outside the home on a plain `ocx pull`. Before this
/// layout `<group>/` sat at depth 1 and *was* the parent `publish_link_within`
/// already checked, so this is a guard the shape change would otherwise have
/// lost rather than one it never had.
///
/// `<root>/active` is deliberately **absent** from the set: it must *be* a
/// link, so its rule is
/// [`ToolchainHome::active_is_valid`](crate::file_structure::ToolchainHome::active_is_valid)
/// and the heal behind it (C-080, C-081), not this loop's negation. `.gitignore`
/// is absent for a different reason — it is a file, and
/// `ToolchainHome::ensure_gitignore` owns its write.
///
/// Outermost first, so the refusal names the component that relocates the most.
fn refuse_symlinked_home_leaves(home: &ToolchainHome) -> crate::Result<()> {
    for path in tree_own_directories(home) {
        // `symlink_metadata`, never `metadata`, which follows the link and
        // reports the target's kind.
        if std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_symlink()) {
            return Err(refuse_symlink(&path));
        }
    }
    Ok(())
}

/// Every directory the rendered tree owns, outermost first — the home root,
/// `links/`, `shells/`, `shells/<shell>/` and `shells/<shell>/bin`.
///
/// Spelled by walking **up** from the accessors rather than by re-joining
/// `links` and `shells` onto the root: the grammar owns those two names
/// (C-072), and a second spelling here is the drift that guard and scan would
/// then disagree about.
fn tree_own_directories(home: &ToolchainHome) -> Vec<PathBuf> {
    let mut paths = vec![home.root().to_path_buf(), links_root(home)];
    // `shells/<shell>/bin` and its two ancestors, reversed to outermost-first.
    // `ancestors` yields the path itself first, so three is exactly
    // `bin`, `shells/<shell>`, `shells` — and never the home root, which is
    // already the first element.
    let shell_bin = home.shell_bin(DEFAULT_SHELL);
    let mut chain: Vec<PathBuf> = shell_bin.ancestors().take(3).map(Path::to_path_buf).collect();
    chain.reverse();
    paths.extend(chain);
    paths
}

fn refuse_symlinked_project_path(scope: &RenderStampScope, home: &ToolchainHome) -> crate::Result<()> {
    let RenderStampScope::Project(project_directory) = scope else {
        return Ok(());
    };
    let Ok(relative) = home.root().strip_prefix(project_directory) else {
        return Ok(());
    };

    let mut current = project_directory.clone();
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        current.push(component);
        // The home root itself is `ensure_home_root`'s to judge.
        if components.peek().is_none() {
            break;
        }
        if std::fs::symlink_metadata(&current).is_ok_and(|metadata| metadata.is_symlink()) {
            return Err(refuse_symlink(&current));
        }
    }
    Ok(())
}

/// Create `root` and every missing ancestor **owner-only-write, at create
/// time** — never a post-hoc `chmod`, which leaves a window in which the
/// directory exists group-writable (R-W19(a)).
#[cfg(unix)]
fn create_owner_only(root: &Path) -> crate::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    // `DirBuilder::mode` applies to *every* level a recursive create makes,
    // which is the point: with `toolchain-dir` set, `<toolchain-dir>` and
    // `<toolchain-dir>/<project-key>` are both directories C-019 never examined.
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(root)
        .map_err(|error| crate::error::file_error(root, error))
}

#[cfg(not(unix))]
fn create_owner_only(root: &Path) -> crate::Result<()> {
    // Windows has no umask and no mode bits; the inherited ACL is the answer,
    // and the effective-ACL walk is the ADR's residual R9.
    std::fs::create_dir_all(root).map_err(|error| crate::error::file_error(root, error))
}

/// Create **one** directory owner-only, **without ever resolving a symlink at
/// that name** — the create half of RUL-33, re-judged at the moment of use.
///
/// [`create_owner_only`] cannot be used here. Its `recursive(true)` is
/// `create_dir_all`, which on an existing name calls `metadata` — following a
/// symlink — and returns `Ok` for a link that points at a directory. Every
/// trampoline then publishes into the link's target, and that directory goes on
/// the user's `PATH`. A plain `mkdir(2)` never resolves its final component, so
/// the create either makes the real directory or fails `EEXIST`, and the
/// `EEXIST` arm re-judges by `symlink_metadata` — the same rule
/// [`refuse_symlinked_home_leaves`] applies, applied where the write happens
/// rather than only where the render started.
///
/// # Non-recursive, and one call per level
///
/// `bin/`'s parent is no longer the home root: under the closed depth-1 layout
/// it is `<root>/shells/<shell>`, two levels down, and [`ensure_home_root`]
/// creates the root and nothing else. The repair that suggests itself — make
/// this recursive — reintroduces exactly the symlink-following hole the
/// function exists to close, and it does so on the path that ends in the
/// directory on everyone's `PATH`. So [`ensure_shell_tree`] calls this once per
/// level instead, and every level gets the same `EEXIST` re-judgement.
///
/// # What this does not close
///
/// The residual window between the `EEXIST` re-judgement and the first
/// `publish_bin_entry` write. Closing it needs the writes to go through a
/// directory handle (`openat` relative to an `O_NOFOLLOW | O_DIRECTORY` open of
/// `bin`) rather than by path, which is `publish_bin_entry`'s shape and not
/// this function's. What is closed here is the part that was silent: a
/// pre-existing symlink at `bin/` is now refused instead of written through.
///
/// # Errors
///
/// [`refuse_symlink`] for a symlink at `directory`, a `NotADirectory` refusal
/// for any other non-directory occupying the name, or the create's own I/O
/// failure. [`reconcile_bin`] turns all three into "not writable" — C-050 skips
/// for every entry, never a hard error.
fn create_directory_owner_only(directory: &Path) -> crate::Result<()> {
    // Two arms rather than one `mut` binding plus a `cfg` block: `mode` is the
    // only mutation, so off Unix the binding is never mutably borrowed and
    // `unused_mut` — denied workspace-wide — makes the Windows build a hard
    // error. Silencing the lint would hide the same shape next time; keeping
    // the `mut` inside the arm that uses it cannot.
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt as _;
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(0o700);
        builder
    };
    #[cfg(not(unix))]
    let builder = std::fs::DirBuilder::new();

    match builder.create(directory) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            // `symlink_metadata`, never `metadata`: `is_dir()` on the former is
            // false for a symlink even when it points at a directory, so one
            // check answers both "is it a link" and "is it a directory".
            match std::fs::symlink_metadata(directory) {
                Ok(metadata) if metadata.is_dir() => Ok(()),
                Ok(metadata) if metadata.is_symlink() => Err(refuse_symlink(directory)),
                Ok(_) => Err(crate::error::file_error(
                    directory,
                    std::io::Error::new(
                        std::io::ErrorKind::NotADirectory,
                        "a rendered toolchain directory name is occupied by something that is not a directory",
                    ),
                )),
                Err(error) => Err(crate::error::file_error(directory, error)),
            }
        }
        Err(error) => Err(crate::error::file_error(directory, error)),
    }
}

/// Create `<root>/shells` and `<root>/shells/<shell>`, one guarded level at a
/// time (C-083's second step).
///
/// Not `create_owner_only`: that is `create_dir_all`, which follows an existing
/// symlink at either level, and both levels are names a hostile clone can
/// commit. Each is created by [`create_directory_owner_only`] instead, whose
/// `EEXIST` arm re-judges by `symlink_metadata` — so the guard runs at the
/// moment of the write and not only at Step 2, which the render lock's
/// unbounded wait sits between.
///
/// `bin/` itself is **not** created here. It is [`reconcile_bin`]'s, and only
/// when that render has a name to put in it: a `-g`-narrowed run that
/// reconciles no default-group entry must not conjure the directory every PATH
/// route points at (RUL-25).
///
/// # Errors
///
/// Whatever [`create_directory_owner_only`] refuses at either level.
fn ensure_shell_tree(home: &ToolchainHome, shell: &str) -> crate::Result<()> {
    // `shells/<shell>/bin`'s two ancestors below the root, outermost first.
    let shell_bin = home.shell_bin(shell);
    let mut levels: Vec<&Path> = shell_bin.ancestors().skip(1).take(2).collect();
    levels.reverse();
    for level in levels {
        create_directory_owner_only(level)?;
    }
    Ok(())
}

/// Create `<root>/links` and `<root>/links/<group>`, one guarded level at a
/// time — the link tree's counterpart to [`ensure_shell_tree`].
///
/// Not [`create_owner_only`], for the reason stated there: it is
/// `create_dir_all`, and `create_dir_all` on `<root>/links/<group>` **creates
/// `links` and follows it if it is a symlink to a directory**. `links` was the
/// one tree-own directory nothing ever created under a guard — it came into
/// existence only as that call's side effect, so its only check was
/// [`ensure_home_root`]'s at step 2, and the render lock's unbounded wait sits
/// between the two. A local writer that swaps `links` for a symlink inside that
/// window sends the group create and the entry's `replace_atomic` into an
/// attacker-chosen directory. `bin/` and `shells/` already re-judge at the
/// moment of use; this is the same rule for the third level.
///
/// The group level goes through the same helper, so the write side's refusal of
/// a symlinked `<group>/` is [`create_directory_owner_only`]'s `EEXIST` arm
/// rather than a second `symlink_metadata` check beside it.
///
/// # Errors
///
/// Whatever [`create_directory_owner_only`] refuses at either level. Both are
/// non-recursive, so a home root that does not exist is an error rather than a
/// tree conjured from nothing — every caller has already run
/// [`ensure_home_root`].
fn ensure_link_group(group_directory: &Path) -> crate::Result<()> {
    if let Some(links) = group_directory.parent() {
        create_directory_owner_only(links)?;
    }
    create_directory_owner_only(group_directory)
}

/// Bring `<root>/active` to its one legal shape, by the kind observed there
/// (C-081).
///
/// | Observed | Action |
/// |---|---|
/// | a link at the derived target | nothing — C-047's byte-identical half |
/// | absent | create |
/// | a link at any other target | `replace_atomic` |
/// | a real directory | `remove_dir_all`, then create |
/// | any other kind | `remove_file`, then create |
///
/// Neither shipped primitive can do this alone: `symlink::update` `read_link`s
/// the existing path and errors `InvalidInput` on a real directory, and
/// `replace_atomic` renames a staged link onto the path, which a populated
/// directory refuses. Both of those are the **common copy outcome** — `cp -rL`,
/// Docker `COPY`, most zip extractors and `rsync` without `-l` dereference
/// symlinks — so this is a steady-state heal and not a migration step.
///
/// **Silent at `debug!` for every healable state, by decision.** Healing a
/// copied tree is the mandate, and a warning on a state a user reaches by
/// copying a directory is a warning on a common benign state. Only the
/// un-healable case speaks, and it speaks at the caller.
///
/// # The `remove_dir_all` carve-out, stated narrowly
///
/// RUL-32 refuses recursive deletion because a delete primitive reachable from
/// a hostile `ocx.lock` is not worth the convenience — and its subject is
/// `<group>/` and `<entry>`, whose names come from that lock. `<root>/active`
/// is the opposite case: **no untrusted string contributes to the path**, which
/// is the guarded root plus one constant. `std::fs::remove_dir_all` removes a
/// symlink entry rather than traversing it, so a hostile `active/link-to-$HOME`
/// costs the link and not the target. The rule this generalises to, and the one
/// a reviewer should apply: **recursion is permitted only on a path no
/// untrusted string contributed to.** Nothing else in this module qualifies.
///
/// # Errors
///
/// The removal's or the creation's own I/O failure — a read-only tree, a
/// permission denial, a concurrent swap. The caller warns and renders on: the
/// physical tree under `shells/` is still written correctly, and an `active`
/// that stayed wrong fails
/// [`ToolchainHome::active_is_valid`](crate::file_structure::ToolchainHome::active_is_valid),
/// so the prompt gate withholds rather than exposing anything.
fn heal_active(home: &ToolchainHome, shell: &str) -> crate::Result<()> {
    if home.active_is_valid(shell) {
        return Ok(());
    }
    let active = home.active();
    let target = home.expected_active_target(shell);

    // `crate::symlink::is_link`, never `Path::is_symlink` or `symlink_metadata`
    // alone: Windows writes a junction, which is a reparse point and not a
    // symlink to either of those (RUL-80). Asked first, so the link arms are
    // decided before any `is_dir()` can claim a junction.
    if crate::symlink::is_link(&active) {
        crate::log::debug!(
            "Toolchain link '{}' is repointed at its derived target",
            active.display()
        );
        return crate::symlink::replace_atomic(&target, &active);
    }
    match std::fs::symlink_metadata(&active) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(crate::error::file_error(&active, error)),
        Ok(metadata) => {
            if metadata.is_dir() {
                // The dereferenced-copy outcome. See the carve-out above.
                std::fs::remove_dir_all(&active).map_err(|error| crate::error::file_error(&active, error))?;
            } else {
                // A regular file, a FIFO, a device node. `remove_file`, never a
                // rename over it: `rename(2)` replaces a regular file silently,
                // which would lose the bytes without ever observing the kind.
                std::fs::remove_file(&active).map_err(|error| crate::error::file_error(&active, error))?;
            }
            crate::log::debug!("Toolchain link '{}' is replaced by its derived link", active.display());
        }
    }
    crate::symlink::create(&target, &active)
}

/// A rendered toolchain path that is a symlink — refused on the write side by
/// [`ensure_home_root`] and on the delete side by [`prune_within`], so neither
/// depends on the other having run.
fn refuse_symlink(path: &Path) -> crate::Error {
    crate::error::file_error(
        path,
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "a rendered toolchain path is a symlink; refusing to write or prune through it",
        ),
    )
}

/// A name occupied by something other than a link — a foreign file, or the
/// real directory a dereferencing copy leaves (C-082).
///
/// The prune side's counterpart to [`publish_link_within`]'s kind dispatch:
/// ocx removes what it wrote, and a name it did not write is reported rather
/// than deleted (RUL-32).
///
/// One helper for both refusing arms, not just `<group>/<entry>`: a
/// non-directory at `<home>/<name>` or `<home>/links/<name>` is as foreign as
/// one at a link's name, and answering the two differently is exactly how the
/// `links/` level this layout added became a silent-delete path.
fn refuse_not_a_link(path: &Path) -> crate::Error {
    crate::error::file_error(
        path,
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "this name holds something other than a link ocx wrote; refusing to remove it",
        ),
    )
}

/// An artifact whose resolved path is not inside its resolved home (C-053).
fn refuse_escape(path: &Path) -> crate::Error {
    crate::error::file_error(
        path,
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "a rendered toolchain artifact does not resolve inside its home; refusing to remove it",
        ),
    )
}

/// The stem every case probe writes, and the one shape [`prune_within`] will
/// delete a regular file for.
///
/// One constant rather than two literals because the two are one contract:
/// [`filesystem_is_case_insensitive`] writes the name, [`is_leaked_case_probe`]
/// recognises it, and a probe whose stem drifted from the predicate would leak
/// a file the prune then reports `Skipped` on every render forever.
const CASE_PROBE_PREFIX: &str = ".ocx-case-probe-";

/// Whether `name` is a probe file [`filesystem_is_case_insensitive`] leaked.
///
/// The suffix is `<pid>-<nanos>`, so digits and `-` and nothing else: a name
/// carrying anything further is not a probe this module wrote, and
/// [`prune_within`] refuses to delete it. ASCII-case-insensitive on the stem
/// because the probe writes both spellings, and on a case-insensitive
/// filesystem `read_dir` answers with whichever one that filesystem stored.
fn is_leaked_case_probe(name: &str) -> bool {
    let (name, prefix) = (name.as_bytes(), CASE_PROBE_PREFIX.as_bytes());
    name.len() > prefix.len()
        && name[..prefix.len()].eq_ignore_ascii_case(prefix)
        && name[prefix.len()..]
            .iter()
            .all(|byte| byte.is_ascii_digit() || *byte == b'-')
}

/// Whether the filesystem holding `directory` resolves two names differing
/// only in ASCII case to one file (R-W19(b), D-V18).
///
/// The renderer calls
/// [`fold_case_insensitive`](super::toolchain_names::fold_case_insensitive)
/// **only** when this answers `true`, never unconditionally: on a
/// case-sensitive host `Make` and `make` are two tools and folding them would
/// silently drop one.
///
/// A probe of the real directory rather than a `cfg!(target_os)` branch,
/// because the host is not the question — a case-sensitive volume on macOS and
/// a case-insensitive one on Linux are both ordinary, and a `cfg!` answer
/// would be an assertion no test on the other host could red.
///
/// # Errors
///
/// The probe's own I/O failure. Returned rather than folded into a `bool` so
/// the caller — not this function — decides which way to fail: answering
/// `false` wrongly writes two entries that are one file, and answering `true`
/// wrongly drops a tool from `bin/`, and neither is a safe default to bury
/// here. [`PackageManager::render_toolchain`] states the decision it made:
/// neither, the render skips.
///
/// # `directory` must exist, and must be the home root itself
///
/// The probe **writes into the directory it judges**, so the caller may hand it
/// only a directory it is entitled to write in. A real render runs
/// [`ensure_home_root`] first and passes the root it just created (RUL-37). A
/// dry run **never calls this at all** (RUL-74) and renders against the
/// unfolded name set: it creates nothing, so it has no directory it is
/// entitled to write in — not an ancestor, which is the project checkout or
/// `$HOME`, and not the home root either, whose `mtime` the probe would move
/// (see [`RenderRequest::dry_run`]).
pub(crate) async fn filesystem_is_case_insensitive(directory: &Path) -> crate::Result<bool> {
    let directory = directory.to_path_buf();
    blocking(directory.clone(), move || {
        // Digits and `-` only after the stem, so the two spellings differ in
        // ASCII case and in nothing else; pid and nanos so two probes of one
        // directory — a dry run beside a render — cannot collide.
        let suffix = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| since.as_nanos())
        );
        let lower = directory.join(format!("{CASE_PROBE_PREFIX}{suffix}"));
        let upper = directory.join(format!("{}{suffix}", CASE_PROBE_PREFIX.to_ascii_uppercase()));

        std::fs::File::create(&lower).map_err(|error| crate::error::file_error(&lower, error))?;
        let answer = std::fs::symlink_metadata(&upper).is_ok();
        // Deliberately ignored: the answer is already read, and a probe file
        // that outlives its probe is pruned by the very next render as the
        // orphan it is — which is true only because [`prune_within`]'s
        // home-root arm removes a regular file whose name
        // [`is_leaked_case_probe`] recognises. With a `remove_dir` there, a
        // leaked probe file would be reported `Skipped` on every render
        // forever.
        let _ = std::fs::remove_file(&lower);
        Ok(answer)
    })
    .await
}

/// Remove one artifact from inside `home`, and nowhere else — the module's
/// **only** delete path (C-044's prune half, C-053).
///
/// Every *write* path in this module has a named, separately-testable seam;
/// without this one, "act only inside the resolved home" would be a property of
/// the render body rather than of any signature, and the Specify phase would
/// have nothing to red. It takes the [`ToolchainHome`] rather than a computed
/// path for exactly that reason: the containment is re-derived here, from the
/// home the caller was handed, and cannot be smuggled in as an already-joined
/// path.
///
/// # The containment check canonicalises; it is not `starts_with` (S-4)
///
/// **Both sides are resolved before they are compared** — the home root and the
/// artifact's computed path — and the refusal fires when the resolved artifact
/// is not under the resolved home. A lexical `starts_with` on the joined path is
/// the obvious implementation and it is wrong, because three committed shapes
/// satisfy it while resolving outside the home:
///
/// - `<project>/.ocx/toolchain` itself committed as a symlink to `$HOME` — the
///   shape [`ensure_home_root`] refuses on the write side, and this is the same
///   refusal on the delete side, so neither depends on the other having run;
/// - the trampoline directory committed as a symlink, which makes every
///   `bin/<name>` removal land in the link's target directory;
/// - `links/`, or a `links/<group>` directory, that is itself a symlink out of
///   the home, which does the same for every entry below it.
///
/// Every one of those makes `<home>/…` a prefix of the *spelled* path and of
/// nothing that is actually removed. Stating the canonicalisation as a contract
/// is what lets a Specify test red it by substituting `starts_with` and watching
/// the planted-symlink cases pass.
///
/// # The whole-`<group>/` orphan, and what is never done to it (RUL-32)
///
/// This is where validation item 31's fourth orphan class lands —
/// [`RenderedArtifact::GroupDirectory`], the orphan that is a directory rather
/// than a file — and the rule is narrow:
///
/// - An **empty** orphan group directory is removed with a non-recursive
///   `remove_dir`.
/// - **Never `remove_dir_all`, under any condition.** A directory under the home
///   is a tree the ADR itself calls attacker-controlled, and a recursive delete
///   primitive reachable from a hostile `ocx.lock` is not worth the convenience
///   of tidying one directory. `move_dir` is banned here for the same reason
///   (module docs).
/// - A group directory **holding a foreign file** therefore fails its
///   `remove_dir` with `ENOTEMPTY`, and the caller reports
///   [`RenderOutcome::Skipped`] naming the path. The foreign file survives, which
///   is the correct outcome: the render did not put it there and cannot know
///   what it is.
/// - A group directory whose name
///   [`ToolchainHome::entry`](crate::file_structure::ToolchainHome::entry)'s
///   `validate_component` would refuse — a `:` on Windows, a trailing dot, a
///   control byte — is **never removed**: it cannot be
///   addressed through the home's own grammar, and a raw join around that
///   grammar is precisely the bypass the type signposts against. It is reported
///   `Skipped` with the refusal as its reason, so a hostile clone's committed
///   `bin./` shows up in the report instead of vanishing from it.
///
/// # A `<group>/<entry>` is removed only when it is a link
///
/// [`RenderedArtifact::Link`] is removed with
/// [`crate::symlink::remove`], which on Unix is `std::fs::remove_file` — it
/// would take a foreign file at that name as happily as a link. A name that is
/// not a symlink is therefore refused here, for both sweeps at once, and
/// reported [`RenderOutcome::Skipped`]: ocx removes what it wrote, and the
/// dereferenced package copy C-082 names survives byte-for-byte.
///
/// # No arm of this `match` deletes a regular file ocx did not write
///
/// The rule above is the *function's*, not that one variant's. A
/// [`RenderedArtifact::GroupDirectory`] or [`RenderedArtifact::RootEntry`]
/// that is neither a symlink nor a directory earns the same
/// [`refuse_not_a_link`] refusal — a regular file at `<home>/<name>` or
/// `<home>/links/<name>` survives byte-for-byte and is named, rather than
/// being deleted on an ordinary `ocx pull` by a report (`Pruned`) that carries
/// no warn line. The one exception is the probe file this module itself writes
/// and [`is_leaked_case_probe`] recognises by name.
///
/// # The home root's own entries, which are not groups (C-074, C-076)
///
/// [`RenderedArtifact::RootEntry`] takes the same non-recursive treatment, for
/// a different reason: depth 1 is closed and tree-owned, so a name there is a
/// leftover of the pre-`links/` layout, a leaked case probe, or something a
/// third party put there. A **populated** one — a legacy `bin/` holding
/// trampolines, a legacy `<group>/` holding links — fails `remove_dir` with
/// `ENOTEMPTY` and is reported `Skipped`, on **every** render, with the
/// contents byte-for-byte intact. That is C-076: reported with its remedy,
/// never deleted. **Depth 1 gets no child sweep**, unlike a departed
/// `links/<group>` (see [`reconcile_links`]): the renderer publishes nothing
/// under a depth-1 name in this layout, so its children are not links ocx wrote
/// but a pre-`links/` leftover or a third party's files, and sweeping them
/// would be a delete primitive over children ocx cannot vouch for — which
/// RUL-32 refuses.
///
/// # Errors
///
/// The removal's own I/O failure, or a refusal when `artifact` does not resolve
/// inside `home`. The caller turns either into [`RenderOutcome::Skipped`]
/// rather than propagating it (C-050).
fn prune_within(home: &ToolchainHome, artifact: &RenderedArtifact) -> crate::Result<()> {
    let (parent, name) = match artifact {
        RenderedArtifact::Trampoline(name) => (home.shell_bin(DEFAULT_SHELL), name.clone()),
        RenderedArtifact::Link { group, entry } => (links_root(home).join(group), entry.clone()),
        RenderedArtifact::GroupDirectory(group) => (links_root(home), group.clone()),
        RenderedArtifact::RootEntry(name) => (home.root().to_path_buf(), name.clone()),
    };

    // The home root itself, judged by `symlink_metadata`. Canonicalising both
    // sides cannot see this one: a root committed as a symlink to `$HOME`
    // resolves the same way on both sides of the comparison, so the containment
    // check below would pass while every removal landed outside the project.
    if std::fs::symlink_metadata(home.root()).is_ok_and(|metadata| metadata.is_symlink()) {
        return Err(refuse_symlink(home.root()));
    }

    // The on-disk name as `read_dir` yielded it, never one reconstructed
    // through `ToolchainHome::entry`'s grammar validator (RUL-40): that
    // validator exists to stop untrusted *input* becoming a path component,
    // and routing the prune through it would make a hostile clone's `<group>/`
    // named `C:` or `a\b` unprunable forever — a permanent foothold. What is
    // checked instead is containment, and its first half is that the name is a
    // single ordinary component *to this platform's own parser*: `PathBuf::join`
    // on Windows lets `C:` discard the parent outright.
    if !is_single_component(&name) {
        return Err(refuse_escape(&parent.join(&name)));
    }

    // Both sides resolved, never a lexical `starts_with` on the spelled path —
    // a symlinked `bin/` or `<group>/` keeps `<home>/…` a prefix of a path that
    // is not what gets removed.
    let resolved_home =
        dunce::canonicalize(home.root()).map_err(|error| crate::error::file_error(home.root(), error))?;
    let resolved_parent = dunce::canonicalize(&parent).map_err(|error| crate::error::file_error(&parent, error))?;
    if !resolved_parent.starts_with(&resolved_home) {
        return Err(refuse_escape(&parent.join(&name)));
    }

    let victim = resolved_parent.join(&name);
    match artifact {
        RenderedArtifact::Trampoline(_) => {
            std::fs::remove_file(&victim).map_err(|error| crate::error::file_error(&victim, error))
        }
        // `crate::symlink::remove` is `std::fs::remove_file` on Unix
        // (`symlink-0.1.0`'s `remove_symlink_auto` *is* that re-export), and
        // the wrapper's own guard is `exists() || is_link()` — so a regular
        // file at a link's name satisfies it and is deleted. Measured, from an
        // unguarded first pass: a planted file inside a group directory
        // vanished and the render reported nothing at all.
        //
        // Here rather than in either caller, because both the selected-group
        // sweep and the departed-group sweep reach this one line: what ocx
        // removes is decided once. The refusal is a `Skipped` item naming the
        // path (C-050), so the file survives *and* the user is told which one
        // is holding the directory open.
        RenderedArtifact::Link { .. } => {
            if !std::fs::symlink_metadata(&victim).is_ok_and(|metadata| metadata.is_symlink()) {
                return Err(refuse_not_a_link(&victim));
            }
            crate::symlink::remove(&victim)
        }
        // Non-recursive, under every condition (RUL-32): a directory under the
        // home is attacker-controlled, and a recursive delete primitive
        // reachable from a hostile `ocx.lock` is not worth tidying one
        // directory. A group directory holding a foreign file therefore fails
        // with `ENOTEMPTY` and the caller reports it `Skipped`.
        //
        // The removal is chosen by the entry's **on-disk type**, because this
        // arm names every orphan at either level, not only directories: a
        // leaked `.ocx-case-probe-<pid>-<nanos>` is a regular file, and
        // `remove_dir` on one fails `ENOTDIR` — so without the type check it
        // would be reported `Skipped` on every render forever, and
        // `filesystem_is_case_insensitive`'s "pruned by the very next render"
        // would be false. Still never `remove_dir_all`, and still never
        // recursive.
        //
        // `remove_file` is reached for that probe name and for nothing else.
        // The type check answers "which removal", never "may this be removed":
        // a plain `remove_file` fallthrough deletes a user's file at
        // `<home>/<name>` or `<home>/links/<name>` on an ordinary `ocx pull`,
        // silently, since `Pruned` carries no warn line — the same measured
        // data loss the `Link` arm above was hardened against, reached through
        // the other half of one `match`. Everything else takes that arm's
        // refusal: ocx removes what ocx wrote, and anything else survives
        // byte-for-byte and is named.
        RenderedArtifact::GroupDirectory(_) | RenderedArtifact::RootEntry(_) => {
            let metadata =
                std::fs::symlink_metadata(&victim).map_err(|error| crate::error::file_error(&victim, error))?;
            if metadata.is_symlink() {
                crate::symlink::remove(&victim)
            } else if metadata.is_dir() {
                std::fs::remove_dir(&victim).map_err(|error| crate::error::file_error(&victim, error))
            } else if is_leaked_case_probe(&name) {
                std::fs::remove_file(&victim).map_err(|error| crate::error::file_error(&victim, error))
            } else {
                Err(refuse_not_a_link(&victim))
            }
        }
    }
}

/// Whether `name` is one ordinary path component to *this* platform's parser.
///
/// `a\b` is one component on Unix and two on Windows, and `C:` is a prefix
/// there rather than a name — which is exactly the asymmetry the check has to
/// respect, since the removal is performed by that same parser's `join`.
fn is_single_component(name: &str) -> bool {
    let mut components = Path::new(name).components();
    let first_is_the_whole_name =
        matches!(components.next(), Some(std::path::Component::Normal(value)) if value == std::ffi::OsStr::new(name));
    first_is_the_whole_name && components.next().is_none()
}

/// The selector every trampoline in one render bakes, derived from the tier
/// (C-028).
///
/// Derived from [`RenderRequest::scope`] and never from
/// [`RenderRequest::home`]: `toolchain-dir` moves the home and leaves the
/// project root — and therefore the baked `--project '<abs root>'` — exactly
/// where it was. Deriving it from the home would make two projects sharing one
/// relocated root bake the same selector.
///
/// A named seam rather than an inline `match` because it is the one place the
/// tier becomes a baked byte string. It performs **no** validation of its own:
/// [`TrampolineTarget::Project`] constructs from any root, and D-V21's refusal
/// of a non-absolute one fires downstream, in `unix_trampoline_body` and the
/// `.exec` writer (RUL-20) — which is where this module's `# Errors` block
/// already routes it.
fn trampoline_target(scope: &RenderStampScope) -> TrampolineTarget {
    match scope {
        RenderStampScope::Global => TrampolineTarget::Global,
        RenderStampScope::Project(root) => TrampolineTarget::Project(root.clone()),
    }
}

/// Atomically publish one **POSIX** trampoline body at `path`, executable.
///
/// The Unix arm only — text, one file, one executable bit. The Windows arm is
/// [`publish_windows_trampoline`], which shares none of this shape: two files,
/// one of them a hardlinked binary blob, and no permission bit at all.
///
/// Temp-file-in-parent then rename, through the shipped
/// [`persist_temp_file`](crate::utility::fs::persist_temp_file) — which is
/// where the Windows transient-lock retry lives, so every new-byte write this
/// module performs inherits it rather than growing a bespoke one.
///
/// Not [`write_bytes_atomic`](crate::utility::fs::write_bytes_atomic), which
/// exists for *private* files and stages its temp at `0o600`: a trampoline is
/// an executable on someone's `PATH` and needs the mode set on the temp file
/// **before** the rename, so no window exists in which `bin/<name>` is present
/// and not executable.
///
/// Renaming over the live file rather than truncating it is also what makes a
/// concurrently-running trampoline safe: the running process keeps the old
/// inode.
///
/// Blocking: one write, one permission set and one rename. Async callers wrap
/// it in `spawn_blocking`, as
/// [`ToolchainHome::ensure_gitignore`](crate::file_structure::ToolchainHome::ensure_gitignore)
/// already documents for itself.
///
/// # Errors
///
/// The write's, the permission change's or the rename's own I/O failure. The
/// caller turns it into [`RenderOutcome::Skipped`] rather than propagating it
/// (C-050).
fn write_trampoline_atomic(path: &Path, body: &str) -> crate::Result<()> {
    use std::io::Write as _;

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut staged =
        tempfile::NamedTempFile::new_in(parent).map_err(|error| crate::error::file_error(parent, error))?;
    staged
        .write_all(body.as_bytes())
        .map_err(|error| crate::error::file_error(path, error))?;
    staged.flush().map_err(|error| crate::error::file_error(path, error))?;

    // On the temp file, before the rename: no window exists in which
    // `bin/<name>` is present and not executable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        staged
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o755))
            .map_err(|error| crate::error::file_error(path, error))?;
    }

    crate::utility::fs::persist_temp_file(staged, path).map_err(|error| crate::error::file_error(path, error))
}

/// Publish one Windows trampoline: the `<name>.exec` sidecar first, then
/// `<name>.exe` as a hardlink from `shim_bin`'s published blob (RUL-35, C-031).
///
/// Plan step 3.1 puts "every new-byte Windows write in the renderer — the
/// `.exec` sidecar in particular" in WP-7's scope, so this arm is the
/// renderer's, not WP-14's. It follows the shipped
/// `launcher::generate::write_shim_exe_then_sidecar` in mechanism —
/// `ShimBinStore::ensure` publishes the content-addressed blob once, then
/// [`hardlink::create`](crate::hardlink::create) links it, so `<name>.exe` is
/// byte-identical to the blob by construction and the Authenticode
/// verbatim-copy property survives; a cross-device store surfaces as
/// `CrossesDevices` and propagates, with no copy fallback.
///
/// # Postcondition: the sidecar lands first, and that inverts the precedent
///
/// **Ordering postcondition — the only recoverable partial state this function
/// can leave is `.exec`-present / `.exe`-absent, never the reverse.** The two
/// writes are sequenced for that reason, not merely for convenience.
///
/// The shipped precedent orders them the other way, and states why: it sequences
/// the link before the sidecar "so the only recoverable partial state on a
/// mid-generate fault is `.exe`-present / `.shim`-absent (ADR E1, recoverable by
/// re-running `generate()`), never the reverse — a `.shim` without its `.exe` is
/// the worse state". **That rationale does not transfer, because its premise is
/// false here.** It holds for `prepare_lazy`'s `entrypoints/` tree, which is
/// staged and then published by one `rename` behind a marker — nobody's `PATH`
/// contains it while it is half-written, so an `.exe`-without-`.shim` window is
/// never observed and the recoverability argument is the only one left.
///
/// `bin/` is the opposite case, and it is the reason this module reconciles
/// per-file instead of publishing a directory at all (module docs): it is a live
/// directory a running shell already has on `PATH`. There, `.exe`-present /
/// `.exec`-absent is directly observable and actively harmful — `which cmake`
/// resolves `cmake.exe`, the shim runs, finds no selector sidecar, and fails at
/// invocation, which is worse than the `PATH` lookup it replaced. The inverse
/// window is inert: `.exec` is on no ordinary `PATHEXT`, so an orphan sidecar is
/// a file nothing resolves, and the next render completes the pair.
///
/// Not `#[cfg(windows)]`, deliberately. Both writes are cross-platform
/// primitives, and a `cfg`-gated seam would put this ordering postcondition —
/// the one property here worth a test — behind a `cfg` the CI leg that actually
/// runs never compiles, which is the unreachable-red class this module's
/// case-fold probe already refuses. Only the *caller* branches on the host.
///
/// # Errors
///
/// The blob publish's, the hardlink's or the sidecar write's own I/O failure.
/// The caller turns it into [`RenderOutcome::Skipped`] rather than propagating it
/// (C-050).
async fn publish_windows_trampoline(
    shim_bin: &ShimBinStore,
    exe_path: &Path,
    sidecar_path: &Path,
    sidecar_body: &str,
) -> crate::Result<()> {
    // The sidecar first — the ordering postcondition this function exists for.
    let sidecar_target = sidecar_path.to_path_buf();
    let sidecar_bytes = sidecar_body.as_bytes().to_vec();
    blocking(sidecar_path.to_path_buf(), move || {
        crate::utility::fs::write_bytes_atomic(&sidecar_target, &sidecar_bytes)
            .map_err(|error| crate::error::file_error(&sidecar_target, error))
    })
    .await?;

    let blob = shim_bin.ensure().await?;
    let exe_path = exe_path.to_path_buf();
    // `hardlink::update`, not `create`: `bin/` is a live reconciled directory
    // whose slot may already be occupied by a previous render's `.exe`, unlike
    // `prepare_lazy`'s freshly staged tree where an occupied slot is a caller
    // bug. The link is still a hardlink of the published blob, so C-031's
    // Authenticode verbatim-copy property is unaffected; a cross-device store
    // still surfaces as `CrossesDevices`, with no copy fallback.
    blocking(exe_path.clone(), move || crate::hardlink::update(&blob, &exe_path)).await
}

/// Read an existing `bin/` entry for the [`RenderOutcome::Unchanged`] compare —
/// bounded, and never through a symlink.
///
/// `Ok(None)` means "nothing comparable is there, take the write branch": the
/// path is absent, is not a regular file, or holds more than `cap` bytes and is
/// therefore different from a body of exactly `cap` bytes by definition. Only
/// `Ok(Some(bytes))` is a candidate for the byte compare C-047's idempotence
/// turns on.
///
/// # Why deciding `Unchanged` needs its own seam
///
/// The naive spelling is `fs::read(path)? == body.as_bytes()`, and it faces the
/// same three hazards
/// [`ToolchainHome::ensure_gitignore`](crate::file_structure::ToolchainHome::ensure_gitignore)
/// already enumerates for its own read — this path is inside a tree a hostile
/// clone controls, and `fs::read` both follows symlinks and reads without a
/// ceiling:
///
/// - **A planted FIFO hangs `ocx pull` forever.** Not a slow render, not a
///   failed one: `open` on a FIFO with no writer blocks indefinitely, and C-050
///   cannot catch it, because C-050 turns a *returned* failure into a skip and
///   this call never returns. It is the one hazard here with no recovery path at
///   all.
/// - **A symlink to `~/.bashrc`** is read as if it were a trampoline; worse, a
///   link whose target happens to match short-circuits to `Unchanged` and
///   survives every render, so `bin/<name>` is permanently a link rather than a
///   trampoline.
/// - **A 200 MB planted regular file** passes every stat there is and costs a
///   hostile clone almost nothing in a packfile (CWE-400).
///
/// A `symlink_metadata` type gate closes the first two and
/// [`read_bounded`](crate::utility::fs::read_bounded) closes the third, which no
/// type check can reach. Both live behind this one signature so the property is
/// type-checked at every call site rather than re-argued inside the render body
/// — the same reason [`prune_within`] exists for the delete side. `read_bounded`
/// additionally stats the **handle** it opened rather than the name, so a
/// concurrent swap between the stat and the open cannot widen what is read.
///
/// `cap` is the expected body's own length, exactly tight, following
/// `ensure_gitignore`'s precedent: anything longer differs by definition, so
/// over-cap is the write branch and never an error.
///
/// Blocking: one stat and one bounded read. Async callers wrap it in
/// `spawn_blocking`.
///
/// # Errors
///
/// The stat's or the read's own I/O failure other than "absent". The caller turns
/// it into [`RenderOutcome::Skipped`] rather than propagating it (C-050).
fn read_existing_trampoline(path: &Path, cap: u64) -> crate::Result<Option<Vec<u8>>> {
    // The type gate is `symlink_metadata`, so a planted FIFO is never opened
    // and a committed symlink is never read as if it were a trampoline.
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(crate::error::file_error(path, error)),
    };
    if !metadata.is_file() {
        return Ok(None);
    }
    match crate::utility::fs::read_bounded(path, cap) {
        Ok(bytes) => Ok(Some(bytes)),
        // Both are "nothing comparable is there, take the write branch": a file
        // longer than `cap` differs from a body of exactly `cap` bytes by
        // definition, and a non-regular file is not a trampoline.
        Err(crate::utility::fs::BoundedReadError::TooLarge { .. })
        | Err(crate::utility::fs::BoundedReadError::NotRegularFile { .. }) => Ok(None),
        Err(crate::utility::fs::BoundedReadError::Io { path, source }) => Err(crate::error::file_error(&path, source)),
    }
}

/// The one point in a render at which a fault may be injected (C-048).
///
/// One variant, deliberately: C-048's control is a fault **between the first
/// entry write and the stamp write**, and nothing else. A general write
/// failure is C-050's, already covered by [`RenderOutcome::Skipped`], so a
/// second stage here would be a control for a contract that already has one.
#[cfg(any(test, feature = "__testing"))]
enum RenderStage {
    /// After the first `bin/` entry has landed, before the stamp is written.
    AfterFirstEntryWrite,
    /// After `shells/<shell>/` exists, before `active` is healed (C-083).
    AfterShellTree,
}

/// Read `__OCX_TESTING_RENDER_FAULT` exactly once, at the entry of a render.
///
/// Gated with the seam, so a release artifact physically lacks the path
/// (`subsystem-tests.md`). Inside a gated build the cost is one
/// allocation-free `var_os` probe per render, after which every
/// [`maybe_inject_fault`] call is an `Option::is_none` short-circuit. An empty
/// value reads as unset.
///
/// **Deliberately untested, and that is a decision rather than an omission**
/// (RUL-42): it is a one-line `std::env::var_os` read with no branch of its
/// own, and every behaviour that could be asserted about the value it produces
/// is asserted through [`maybe_inject_fault`]'s `fault` parameter, which
/// [`render_with`] takes directly. A test here could only restate `var_os`.
#[cfg(any(test, feature = "__testing"))]
fn read_fault_hook() -> Option<String> {
    std::env::var_os("__OCX_TESTING_RENDER_FAULT")
        .map(|value| value.to_string_lossy().into_owned())
        .filter(|value| !value.is_empty())
}

/// Fail the render when `fault` names `stage`.
///
/// # Errors
///
/// A synthetic [`PackageErrorKind::Internal`] when `fault` is
/// `Some(`[`FAULT_AFTER_FIRST_ENTRY_WRITE`]`)` and `stage` is
/// [`RenderStage::AfterFirstEntryWrite`]. Never otherwise.
#[cfg(any(test, feature = "__testing"))]
fn maybe_inject_fault(fault: Option<&str>, stage: RenderStage) -> Result<(), PackageErrorKind> {
    let requested = match stage {
        RenderStage::AfterFirstEntryWrite => FAULT_AFTER_FIRST_ENTRY_WRITE,
        RenderStage::AfterShellTree => FAULT_AFTER_SHELL_TREE,
    };
    if fault == Some(requested) {
        return Err(PackageErrorKind::Internal(crate::error::file_error(
            Path::new(requested),
            std::io::Error::other("__OCX_TESTING_RENDER_FAULT aborted the render"),
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;

    use super::*;
    use crate::file_structure::{RenderStamp, RenderStampTarget};
    use crate::oci::index::{ChainMode, Index, LocalConfig, LocalIndex};
    use crate::package::metadata::visibility::Visibility;
    use crate::package::metadata::{Binaries, BinaryName, EntrypointName};
    use crate::project::{DECLARATION_HASH_VERSION, DEFAULT_GROUP, LockMetadata, LockVersion, LockedTool};
    use crate::reference_manager::ReferenceManager;

    // ── Fixtures ─────────────────────────────────────────────────────────────
    //
    // Everything here is built from a `tempfile::TempDir`: no ambient
    // `$OCX_HOME`, no registry, no client. `render_with` and `heal_links` take
    // the `FileStructure` as a parameter precisely so that holds.

    const REGISTRY: &str = "example.com";

    /// The platform every request in this module carries.
    ///
    /// A fixed value rather than the host's: `RenderRequest::platform` is a
    /// parameter, so a host-derived one would make every link assertion answer
    /// differently on the Windows leg for a reason that has nothing to do with
    /// the contract under test.
    const PLATFORM_KEY: &str = "linux/amd64";

    fn platform() -> oci::Platform {
        PLATFORM_KEY.parse().expect("the fixture platform key is canonical")
    }

    fn digest_of(seed: char) -> oci::Digest {
        oci::Digest::Sha256(seed.to_string().repeat(64))
    }

    fn pinned(repository: &str, seed: char) -> oci::PinnedIdentifier {
        oci::PinnedIdentifier::try_from(
            oci::Identifier::new_registry(repository, REGISTRY).clone_with_digest(digest_of(seed)),
        )
        .expect("a digest-bearing identifier is pinned")
    }

    fn binary_name(value: &str) -> BinaryName {
        BinaryName::try_from(value).expect("the fixture binary name is valid")
    }

    fn binaries(names: &[&str]) -> Binaries {
        let set: BTreeSet<BinaryName> = names.iter().map(|n| binary_name(n)).collect();
        Binaries::try_from(set).expect("the fixture binaries claim is valid")
    }

    fn entrypoints(names: &[&str]) -> Vec<EntrypointName> {
        names
            .iter()
            .map(|n| EntrypointName::try_from((*n).to_string()).expect("the fixture entrypoint name is valid"))
            .collect()
    }

    /// A **root** closure node carrying only the two claim axes the name set is
    /// derived from; every other field is this axis's inert value.
    fn node(identifier: oci::PinnedIdentifier, claimed: Option<&[&str]>, entries: &[&str]) -> ClosureNode {
        ClosureNode {
            config_digest: identifier.digest(),
            identifier,
            effective_visibility: None,
            binaries: claimed.map(binaries),
            entrypoints: entrypoints(entries),
            env: Vec::new(),
            integrations: Vec::new(),
            dependencies: Vec::new(),
            is_root: true,
        }
    }

    /// A **dependency** node carrying an explicit effective visibility — the
    /// field `admitted_on_surface` gates on (C-023).
    fn dependency(identifier: oci::PinnedIdentifier, claimed: Option<&[&str]>, effective: Visibility) -> ClosureNode {
        let mut node = node(identifier, claimed, &[]);
        node.is_root = false;
        node.effective_visibility = Some(effective);
        node
    }

    /// The publisher asserting **zero** executables, distinct from `None`
    /// ("no claim at all"). Named because `Some(&[])` has no inferable element
    /// type at the call site.
    const ASSERTED_EMPTY: &[&str] = &[];

    fn locked_tool(name: &str, group: &str, repository: &str, platforms: &[(&str, char)]) -> LockedTool {
        LockedTool {
            name: name.to_string(),
            group: group.to_string(),
            repository: oci::Identifier::new_registry(repository, REGISTRY),
            platforms: platforms
                .iter()
                .map(|(key, seed)| ((*key).to_string(), digest_of(*seed)))
                .collect::<BTreeMap<String, oci::Digest>>(),
        }
    }

    fn lock_of(tools: Vec<LockedTool>) -> ProjectLock {
        ProjectLock {
            metadata: LockMetadata {
                lock_version: LockVersion::V3,
                declaration_hash_version: DECLARATION_HASH_VERSION,
                declaration_hash: "0".repeat(64),
                generated_by: "wp-7 specification fixture".to_string(),
                generated_at: "2026-01-01T00:00:00Z".to_string(),
            },
            tools,
        }
    }

    fn groups_of(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_string()).collect()
    }

    /// The digest root a `<group>/<entry>` link must name for `tool`, derived
    /// the way the contract says the renderer derives it — through the shared
    /// `select_best` helper (RUL-31), never by an exact key lookup.
    fn expected_link_target(file_structure: &FileStructure, tool: &LockedTool) -> PathBuf {
        let identifier = crate::project::compose::host_leaf_identifier(tool, &platform())
            .expect("the fixture lock ships a leaf compatible with the fixture platform");
        let pinned = oci::PinnedIdentifier::try_from(identifier).expect("a resolved host leaf is digest-bearing");
        file_structure.packages.path(&pinned)
    }

    /// One project tree under one tempdir: an `$OCX_HOME`, a project directory
    /// and the home the renderer writes into.
    struct Tree {
        tmp: TempDir,
        file_structure: FileStructure,
        project_dir: PathBuf,
        home: ToolchainHome,
        /// The absolute `ocx` every golden body must bake — seeded, never
        /// inferred. See [`Tree::new`].
        // Read only by the `#[cfg(unix)]` trampoline-body assertions and by
        // `expected_unix_trampoline_body` beside them; the Windows arm asserts
        // over the `.exec` sidecar instead. `expect`, so this line fails the
        // day a Windows test reads it.
        #[cfg_attr(
            not(unix),
            expect(dead_code, reason = "read only by cfg(unix) trampoline-body tests")
        )]
        ocx_binary: PathBuf,
    }

    impl Tree {
        /// A tree whose rung-1 `ocx` **exists** (C-1, case 43).
        ///
        /// `launcher::generate::trampoline_ocx_binary`'s ladder has two rungs:
        /// the store-derived install path, then `std::env::current_exe()`. A
        /// fixture that controls only the `FileStructure` root and leaves the
        /// install path absent does not get the bare-name form — it falls to
        /// rung 2 and bakes the **test binary's own absolute path**, which
        /// varies by machine, by cargo profile and by the target-directory
        /// hash. Every golden in this module would then be stable locally and
        /// unstable on CI. Seeding a real file here is what makes rung 1 win
        /// deterministically.
        fn new() -> Self {
            let tmp = tempfile::tempdir().expect("a tempdir is creatable");
            let file_structure = FileStructure::with_root(tmp.path().join("ocx-home"));
            let project_dir = tmp.path().join("proj");
            std::fs::create_dir_all(&project_dir).expect("the project directory is creatable");

            let ocx_binary = file_structure
                .symlinks
                .current(&oci::ocx_cli_identifier())
                .join("content")
                .join("bin")
                .join(if cfg!(windows) { "ocx.exe" } else { "ocx" });
            std::fs::create_dir_all(ocx_binary.parent().expect("the binary has a parent"))
                .expect("the install tree is creatable");
            std::fs::write(&ocx_binary, b"#!/bin/sh\n").expect("the seeded ocx is writable");

            let home = ToolchainHome::new(project_dir.join(".ocx").join("toolchain"));
            Self {
                tmp,
                file_structure,
                project_dir,
                home,
                ocx_binary,
            }
        }

        fn scope(&self) -> RenderStampScope {
            RenderStampScope::Project(self.project_dir.clone())
        }

        fn key(&self) -> String {
            ReferenceManager::name_for_path(&self.project_dir)
        }

        fn stamp(&self) -> Option<RenderStamp> {
            self.file_structure
                .state
                .render_stamp(RenderStampTarget::Project(&self.key()))
        }

        fn stamp_file(&self) -> PathBuf {
            self.file_structure.state.render_stamp_file(&self.key())
        }

        /// `render_with`'s documented precondition: steps 2–6 have run — the
        /// home root and `shells/<shell>/` exist and are real directories, and
        /// `active` is the link they render. Every `render_with` caller in this
        /// module establishes it; `render_toolchain` callers do not, because
        /// establishing it is what they are testing.
        ///
        /// `shells/<shell>/bin` itself is deliberately **not** created — that
        /// is `reconcile_bin`'s, and only when the render has a name to put in
        /// it (RUL-25).
        fn create_home_root(&self) {
            seed_rendered_home(&self.home);
        }

        /// `<root>/shells/<shell>` — `shell_bin`'s parent.
        fn shell_directory(&self) -> PathBuf {
            shell_directory_of(&self.home)
        }

        /// Whether this home can hold `make` and `Make` as **two** files —
        /// read through the *production* probe, so a row and `render_with`
        /// cannot disagree about what "case-sensitive" means.
        ///
        /// The premise of every row that seeds a differently-cased twin. A
        /// macOS runner's APFS and every Windows volume are case-insensitive by
        /// default, so the twin collapses into the entry it was meant to
        /// shadow, the injected `case_insensitive` flag stops describing the
        /// disk it is asserted against, and the fixture cannot build its own
        /// precondition. Skip, do not weaken.
        ///
        /// Requires the home root to exist ([`Self::create_home_root`]) — the
        /// probe writes into the directory it judges.
        async fn holds_case_twins(&self) -> bool {
            !filesystem_is_case_insensitive(self.home.root())
                .await
                .expect("the case-fold probe answers for a home this test just created")
        }

        /// An offline manager over this tree's store — no client, no sources.
        fn manager(&self) -> PackageManager {
            let index = Index::from_chained(
                LocalIndex::new(LocalConfig {
                    index_store: self.file_structure.index.clone(),
                }),
                Vec::new(),
                ChainMode::Offline,
            );
            PackageManager::new(self.file_structure.clone(), index, None, REGISTRY)
        }

        /// The on-disk names in the **physical** trampoline directory, sorted.
        /// Absent reads as empty, which is the state a render that never
        /// touched it leaves.
        ///
        /// `shell_bin`, never `bin()`: reading through `active` would make
        /// every assertion below agree with whatever that link points at, which
        /// is the property half this module's guards exist to defend.
        fn bin_entries(&self) -> Vec<String> {
            read_dir_names(&self.home.shell_bin(DEFAULT_SHELL))
        }
    }

    /// Steps 2 and 6 of the render order, for **any** home — the root and
    /// `shells/<shell>/` as real directories, `active` as the link they render.
    ///
    /// A free function rather than a `Tree` method because several rows render
    /// into a *second* home (`toolchain-dir` relocation, two project
    /// directories) that the fixture never wrapped.
    fn seed_rendered_home(home: &ToolchainHome) {
        std::fs::create_dir_all(shell_directory_of(home)).expect("the shell directory is creatable");
        crate::symlink::create(home.expected_active_target(DEFAULT_SHELL), home.active())
            .expect("the activation link is creatable");
    }

    /// `<root>/shells/<shell>` — `shell_bin`'s parent, spelled through the
    /// accessor so no fixture can drift from the tree it seeds.
    fn shell_directory_of(home: &ToolchainHome) -> PathBuf {
        home.shell_bin(DEFAULT_SHELL)
            .parent()
            .expect("the trampoline directory has a parent")
            .to_path_buf()
    }

    /// The on-disk entry names of `directory`, sorted; an absent directory
    /// reads as empty.
    fn read_dir_names(directory: &Path) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(directory) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .map(|entry| entry.expect("a readable directory entry").file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// The on-disk file names one exposed `name` occupies in `bin/`.
    ///
    /// One on POSIX; **two** on Windows — `<name>.exe` plus its `<name>.exec`
    /// sidecar (RUL-26). Every count and set assertion in this module goes
    /// through here rather than through a bare `name`, so a Windows-only
    /// off-by-one cannot pass on the POSIX leg.
    fn trampoline_files(name: &str) -> Vec<String> {
        if cfg!(windows) {
            vec![format!("{name}.exe"), format!("{name}.exec")]
        } else {
            vec![name.to_string()]
        }
    }

    /// The sorted `bin/` name set for a whole exposed-name set.
    fn expected_bin_entries(names: &[&str]) -> Vec<String> {
        let mut all: Vec<String> = names.iter().flat_map(|n| trampoline_files(n)).collect();
        all.sort();
        all
    }

    fn trampoline(name: &str) -> RenderedArtifact {
        RenderedArtifact::Trampoline(name.to_string())
    }

    fn link(group: &str, entry: &str) -> RenderedArtifact {
        RenderedArtifact::Link {
            group: group.to_string(),
            entry: entry.to_string(),
        }
    }

    fn group_directory(group: &str) -> RenderedArtifact {
        RenderedArtifact::GroupDirectory(group.to_string())
    }

    /// The outcome the report recorded for `artifact`, failing with the whole
    /// report when it is absent — a red then names what *was* reported instead
    /// of just "None".
    #[track_caller]
    fn outcome_of<'a>(report: &'a RenderReport, artifact: &RenderedArtifact) -> &'a RenderOutcome {
        report
            .items
            .iter()
            .find(|item| &item.artifact == artifact)
            .map(|item| &item.outcome)
            .unwrap_or_else(|| panic!("{artifact:?} must appear in the report; the report was {report:?}"))
    }

    fn is_skipped(outcome: &RenderOutcome) -> bool {
        matches!(outcome, RenderOutcome::Skipped { .. })
    }

    /// The POSIX trampoline body the renderer must produce for a project home.
    ///
    /// Restated here rather than called through
    /// `launcher::body::unix_trampoline_body`: a golden that asks the producer
    /// what it produces cannot tell a correct body from a changed one, and the
    /// producer's own goldens already pin it from the other side (C-034's
    /// paired-golden pattern). The marker is imported rather than spelled,
    /// because it is a *shared* constant the reader (`env::is_ocx_trampoline`)
    /// also reads — case 43(d).
    // Every caller is a `#[cfg(unix)]` test — the POSIX trampoline is a shell
    // script, and Windows renders an `.exe` plus a sidecar instead.
    #[cfg(unix)]
    fn expected_unix_trampoline_body(project_root: &Path, ocx_binary: &Path) -> String {
        let marker = crate::env::TRAMPOLINE_MARKER;
        format!(
            "#!/bin/sh\n\
             {marker}\n\
             unset OCX_GLOBAL OCX_PROJECT\n\
             __ocx_binary='{binary}'\n\
             exec \"${{OCX_BINARY_PIN:-$__ocx_binary}}\" --project '{root}' exec -- \"${{0##*/}}\" \"$@\"\n",
            binary = ocx_binary.display(),
            root = project_root.display(),
        )
    }

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::symlink_metadata(path)
            .unwrap_or_else(|e| panic!("{path:?} must exist: {e}"))
            .permissions()
            .mode()
    }

    #[cfg(unix)]
    fn inode_of(path: &Path) -> u64 {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::symlink_metadata(path)
            .unwrap_or_else(|e| panic!("{path:?} must exist: {e}"))
            .ino()
    }

    /// Make `directory` unwritable, and **prove the denial took**.
    ///
    /// A `chmod` is ignored for the superuser, so a test that merely set the
    /// bits and moved on would pass identically whether or not the code under
    /// test handles the failure — the green-that-never-ran shape. The probe
    /// write is what makes the precondition observed rather than assumed; it
    /// fails loudly under `root` instead of skipping silently.
    #[cfg(unix)]
    #[track_caller]
    fn deny_writes(directory: &Path) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o500))
            .unwrap_or_else(|e| panic!("{directory:?} must be chmod-able: {e}"));
        assert!(
            std::fs::write(directory.join("__write_probe"), b"x").is_err(),
            "precondition: writes into {directory:?} must actually be denied — under a uid that \
             ignores the mode bits this test would pass without exercising the skip path at all"
        );
    }

    #[cfg(unix)]
    fn allow_writes(directory: &Path) {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700));
    }

    /// `mkfifo(2)`, the one filesystem object `std::fs` cannot create.
    ///
    /// Copied from `env.rs`'s and `oci/index/file_transport.rs`'s helpers
    /// rather than shared: three `#[cfg(test)]` modules in different
    /// subsystems, and the crate has no test-support home for a three-line
    /// libc call.
    #[cfg(unix)]
    #[track_caller]
    fn mkfifo(path: &Path) {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt as _;

        let c_path = CString::new(path.as_os_str().as_bytes()).expect("a tempdir path holds no NUL");
        // SAFETY: `c_path` is a NUL-terminated C string alive for the whole
        // call, and `mkfifo` only reads it.
        let created = unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) };
        assert_eq!(created, 0, "mkfifo failed: {}", std::io::Error::last_os_error());
    }

    /// One filesystem object, identified by everything a "wrote nothing" claim
    /// has to survive.
    ///
    /// Bytes alone are not enough: rewriting a file with identical content is
    /// a write, and so is re-creating it. The inode catches the re-creation
    /// and the nanosecond mtime catches the rewrite — both are the shapes a
    /// bytes-only or count-only assertion reads as "unchanged" (case 66).
    #[cfg(unix)]
    #[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
    struct SubtreeEntry {
        relative: PathBuf,
        kind: &'static str,
        bytes: Option<Vec<u8>>,
        /// A symlink's **raw** target, `None` for every other kind (C-079).
        ///
        /// Without it every "the render wrote nothing" and "two renders are
        /// identical" assertion built on this helper is blind to a repoint:
        /// `active` swung from one directory to another keeps its kind, its
        /// inode and its mtime, so the snapshot compares equal to a tree whose
        /// one PATH-facing link now points somewhere else entirely.
        target: Option<PathBuf>,
        mtime_nsec: i64,
        inode: u64,
    }

    /// Every object under `root`, recursively, as [`SubtreeEntry`] values —
    /// sorted, so two snapshots compare as sets rather than as walk orders.
    /// An absent `root` snapshots as the empty vector, which is what makes
    /// "the dry run created no home at all" expressible.
    #[cfg(unix)]
    fn snapshot_subtree(root: &Path) -> Vec<SubtreeEntry> {
        use std::os::unix::fs::MetadataExt as _;

        fn walk(root: &Path, current: &Path, out: &mut Vec<SubtreeEntry>) {
            let Ok(entries) = std::fs::read_dir(current) else {
                return;
            };
            for entry in entries {
                let entry = entry.expect("a readable directory entry");
                let path = entry.path();
                let metadata = std::fs::symlink_metadata(&path).expect("a stat-able entry");
                let kind = if metadata.is_symlink() {
                    "symlink"
                } else if metadata.is_dir() {
                    "dir"
                } else {
                    "file"
                };
                out.push(SubtreeEntry {
                    relative: path
                        .strip_prefix(root)
                        .expect("every walked path is under the walk root")
                        .to_path_buf(),
                    kind,
                    bytes: if kind == "file" {
                        std::fs::read(&path).ok()
                    } else {
                        None
                    },
                    target: if kind == "symlink" {
                        std::fs::read_link(&path).ok()
                    } else {
                        None
                    },
                    mtime_nsec: metadata.mtime_nsec(),
                    inode: metadata.ino(),
                });
                if kind == "dir" {
                    walk(root, &path, out);
                }
            }
        }

        let mut out = Vec::new();
        if root.exists() {
            walk(root, root, &mut out);
        }
        out.sort();
        out
    }

    // ── 1. The name set — boundaries (C-021…C-025, C-044, C-045) ────────────

    /// C-044, case 1 — an **empty** computed set over a **non-empty** tree is
    /// the prune pass's whole job: every seeded entry reports `Pruned` and
    /// `bin/` ends empty.
    ///
    /// RED: early-return on an empty name set, skipping the prune pass.
    #[tokio::test]
    async fn an_empty_surface_prunes_every_entry_already_in_bin() {
        let tree = Tree::new();
        tree.create_home_root();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        std::fs::write(tree.home.shell_bin(DEFAULT_SHELL).join("cmake"), b"#!/bin/sh\n").unwrap();
        std::fs::write(tree.home.shell_bin(DEFAULT_SHELL).join("ctest"), b"#!/bin/sh\n").unwrap();

        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("an empty surface is an ordinary render, not a refusal");

        assert_eq!(
            outcome_of(&report, &trampoline("cmake")),
            &RenderOutcome::Pruned,
            "C-044 — a name no longer in the computed set is pruned"
        );
        assert_eq!(outcome_of(&report, &trampoline("ctest")), &RenderOutcome::Pruned);
        assert!(
            tree.bin_entries().is_empty(),
            "C-044 — and `bin/` is empty afterwards, not merely reported as pruned: {:?}",
            tree.bin_entries()
        );
    }

    /// C-044/C-048, case 2 — an empty surface over an empty tree still writes a
    /// stamp, carrying an **empty** `bin_fingerprint`.
    ///
    /// RED: gate the stamp on `!items.is_empty()`. C-061's gate then withholds
    /// forever on a legitimately toolless project, which is the permanent
    /// mismatch RUL-22/27 exist to prevent.
    #[tokio::test]
    async fn an_empty_render_still_writes_a_stamp_with_an_empty_fingerprint() {
        let tree = Tree::new();
        tree.create_home_root();

        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("a toolless project renders");

        assert_eq!(report.items, Vec::new(), "nothing to write and nothing to prune");
        assert!(
            report.stamp_written,
            "C-048 — the stamp is written even when the tree is empty"
        );
        let stamp = tree.stamp().expect("the stamp file must exist on disk");
        assert!(
            stamp.bin_fingerprint.is_empty(),
            "an empty tree stamps an empty fingerprint, not a withheld stamp"
        );
        assert!(stamp.names.is_empty());
    }

    /// C-022/C-045, case 3 — a root claiming neither `binaries` nor entry
    /// points contributes nothing and the render is `Ok`.
    ///
    /// RED: pass `NotEnumerablePolicy::Refuse` instead of `Skip` — an ordinary
    /// metadata-less package then hard-errors, which no contract sanctions.
    #[tokio::test]
    async fn a_root_claiming_no_names_contributes_nothing_and_does_not_refuse() {
        let tree = Tree::new();
        tree.create_home_root();

        let surface = vec![node(pinned("ns/plain", 'a'), None, &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("C-022 — the render path passes `Skip`, so an unenumerable node is ordinary");

        assert!(
            report.items.is_empty(),
            "the node contributes no trampoline: {report:?}"
        );
        assert!(tree.bin_entries().is_empty());
    }

    /// C-022, case 4 — `Some([])` ("the publisher asserted zero executables")
    /// is enumerable and distinct from `None`: it renders `Ok` and prunes
    /// nothing extra.
    ///
    /// RED: treat empty as absent in the consumer arm.
    #[tokio::test]
    async fn an_asserted_empty_binaries_claim_is_not_an_absent_claim() {
        let tree = Tree::new();
        tree.create_home_root();

        let surface = vec![node(pinned("ns/empty", 'a'), Some(ASSERTED_EMPTY), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("an asserted-empty claim renders");

        assert!(
            report.items.is_empty(),
            "no name is claimed, so no trampoline: {report:?}"
        );
        assert!(
            report.bin_in_scope,
            "RUL-25 — the default group was selected, so `bin/` was reconciled"
        );
    }

    /// C-021/C-023, case 5 — an interface-admitted dependency's claim lands in
    /// `bin/`; a private-only dependency's does not.
    ///
    /// RED: flip the surface flag on `admitted_on_surface` — the private
    /// dependency's name then appears on PATH.
    #[tokio::test]
    async fn an_interface_dependency_contributes_a_name_and_a_private_one_does_not() {
        let tree = Tree::new();
        tree.create_home_root();

        let surface = vec![
            dependency(pinned("ns/exposed", 'a'), Some(&["exposed"]), Visibility::INTERFACE),
            dependency(pinned("ns/hidden", 'b'), Some(&["hidden"]), Visibility::PRIVATE),
            node(pinned("ns/root", 'c'), Some(&["root-tool"]), &[]),
        ];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render succeeds");

        assert_eq!(
            tree.bin_entries(),
            expected_bin_entries(&["exposed", "root-tool"]),
            "C-023 — only the root and its interface-admitted dependency contribute names"
        );
    }

    /// C-013/C-044, cases 6 and 10 — a claimed name that collides with the
    /// home's own grammar word (`bin`) lands at `bin/bin`, a file, and leaves
    /// `ToolchainHome::bin()`'s directory itself untouched.
    ///
    /// Pinned rather than assumed: `bin` is a valid `BinaryName` and a
    /// reserved *component*, and the two rules are about different positions in
    /// the path. A future "validate bin entry names too" edit would silently
    /// change where this lands.
    #[tokio::test]
    async fn a_tool_named_bin_renders_inside_bin_and_not_at_the_home_root() {
        let tree = Tree::new();
        tree.create_home_root();

        let surface = vec![node(pinned("ns/bin", 'a'), Some(&["bin"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("`bin` is an ordinary claimed name");

        assert_eq!(tree.bin_entries(), expected_bin_entries(&["bin"]));
        assert!(
            tree.home.shell_bin(DEFAULT_SHELL).is_dir(),
            "C-013 — the trampoline directory itself stays a directory"
        );
        for file in trampoline_files("bin") {
            assert!(
                tree.home.shell_bin(DEFAULT_SHELL).join(&file).is_file(),
                "the claim lands at `bin/{file}`, never at the home root"
            );
            assert!(
                !tree.home.root().join(&file).is_file(),
                "…and nothing named {file} appears beside `bin/`"
            );
        }
    }

    /// C-025/D-V24/RUL-19, case 7 — on a case-insensitive home the twins fold
    /// to **one** entry, spelled as the greatest-`walk_index` winner's own
    /// spelling, in `bin/` **and** in `RenderStamp.names`.
    ///
    /// RED: pick the winner by `BTreeMap` key order (ASCII lowercase sorts
    /// above uppercase, so the all-lowercase spelling would always win), or
    /// lowercase the survivor.
    #[tokio::test]
    async fn case_twins_fold_to_the_last_walked_spelling_on_a_case_insensitive_home() {
        let tree = Tree::new();
        tree.create_home_root();

        // `make` is walked first, `Make` last — so `Make` is the winner and
        // `Make` is the spelling that must survive.
        let surface = vec![
            node(pinned("ns/first", 'a'), Some(&["make"]), &[]),
            node(pinned("ns/second", 'b'), Some(&["Make"]), &[]),
        ];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            true,
            None,
        )
        .await
        .expect("the fold is not a refusal");

        assert_eq!(
            tree.bin_entries(),
            expected_bin_entries(&["Make"]),
            "RUL-19 — one file, spelled as the last-walked claim spelled it"
        );
        let stamp = tree.stamp().expect("the stamp is written");
        assert_eq!(
            stamp.names.iter().cloned().collect::<Vec<_>>(),
            expected_bin_entries(&["Make"]),
            "…and the stamp records the same spelling, or C-061's readdir gate never matches"
        );
    }

    /// D-V18/R-W19(b), case 8 — on a **case-sensitive** home the fold must not
    /// run: `Make` and `make` are two tools and two files.
    ///
    /// RED: call `fold_case_insensitive` unconditionally — one tool silently
    /// disappears on Linux.
    #[tokio::test]
    async fn case_twins_stay_two_entries_on_a_case_sensitive_home() {
        let tree = Tree::new();
        tree.create_home_root();

        // The row's own precondition, read rather than presupposed. The logic
        // under test is platform-neutral (`case_insensitive` is driven `false`
        // below), but the two files it expects only stay two files on a
        // case-sensitive volume.
        if !tree.holds_case_twins().await {
            eprintln!(
                "skipped: the probe reported a case-insensitive home, so `make` and `Make` cannot be \
                 two files here"
            );
            return;
        }

        let surface = vec![
            node(pinned("ns/first", 'a'), Some(&["make"]), &[]),
            node(pinned("ns/second", 'b'), Some(&["Make"]), &[]),
        ];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("two distinct names render");

        assert_eq!(
            tree.bin_entries(),
            expected_bin_entries(&["Make", "make"]),
            "D-V18 — the fold runs only when the probe said the home is case-insensitive"
        );
        let stamp = tree.stamp().expect("the stamp is written");
        assert_eq!(stamp.names.len(), expected_bin_entries(&["Make", "make"]).len());
    }

    /// S-012/C-024/D-V19, case 9 — a package claiming the name `ocx` renders
    /// like any other name: exit 0, no refusal, and the body bakes an
    /// **absolute** ocx path so the trampoline cannot re-invoke itself.
    ///
    /// RED: restore the `ShimNameShadowsOcx` refusal (the ADR's own named
    /// mutation for validation item 16).
    #[tokio::test]
    async fn a_package_claiming_the_name_ocx_renders_without_a_refusal() {
        let tree = Tree::new();
        tree.create_home_root();

        let surface = vec![node(pinned("ns/ocx", 'a'), Some(&["ocx"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("S-012 — a project may pin its own ocx; this is never a refusal");

        assert_eq!(tree.bin_entries(), expected_bin_entries(&["ocx"]));
        assert!(
            report.items.iter().all(|item| !is_skipped(&item.outcome)),
            "…and never a skip either: {report:?}"
        );

        #[cfg(unix)]
        {
            let body = std::fs::read_to_string(tree.home.shell_bin(DEFAULT_SHELL).join("ocx")).unwrap();
            assert!(
                body.contains(&format!("__ocx_binary='{}'", tree.ocx_binary.display())),
                "D-V19 — the body re-enters ocx through the absolute install path, or a \
                 `bin/ocx` trampoline resolves itself through PATH: {body}"
            );
        }
    }

    /// RUL-25, case 11 — a `-g`-narrowed pull that does **not** select the
    /// default group leaves `bin/` entirely untouched and reports
    /// `bin_in_scope == false`.
    ///
    /// RED: derive `bin/`'s set from `request.groups`. A narrowed
    /// `ocx pull -g ci` then either empties `bin/` or resolves the default
    /// group's metadata the invocation never asked for.
    #[tokio::test]
    async fn a_narrowed_pull_that_excludes_the_default_group_does_not_touch_bin() {
        let tree = Tree::new();
        tree.create_home_root();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        std::fs::write(tree.home.shell_bin(DEFAULT_SHELL).join("cmake"), b"#!/bin/sh\nold\n").unwrap();

        let surface = vec![node(pinned("ns/other", 'a'), Some(&["other"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(vec![locked_tool("ninja", "ci", "ns/ninja", &[(PLATFORM_KEY, 'd')])]);
        let groups = groups_of(&["ci"]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("a narrowed pull renders its own groups");

        assert!(
            !report.bin_in_scope,
            "RUL-25 — `bin/` was not in scope, and that is a field rather than an inference"
        );
        assert_eq!(
            tree.bin_entries(),
            vec!["cmake".to_string()],
            "…so the pre-existing entry is neither rewritten nor pruned"
        );
        assert_eq!(
            std::fs::read(tree.home.shell_bin(DEFAULT_SHELL).join("cmake")).unwrap(),
            b"#!/bin/sh\nold\n",
            "…byte-identical, not merely present"
        );
        assert!(
            report
                .items
                .iter()
                .all(|item| !matches!(item.artifact, RenderedArtifact::Trampoline(_))),
            "…and no trampoline item is reported at all: {report:?}"
        );
    }

    /// C-025, case 12 — one node claiming both case twins on its **own two
    /// axes** folds to one entry and never shadows itself.
    ///
    /// The render-observable half is the file count. The `NameOwner::shadowed`
    /// half is `toolchain_names.rs`'s (RUL-11) and is asserted there — a
    /// `RenderReport` carries no collision field (RUL-34), so this layer
    /// cannot see it.
    #[tokio::test]
    async fn one_node_claiming_both_case_twins_yields_one_entry() {
        let tree = Tree::new();
        tree.create_home_root();

        let surface = vec![node(pinned("ns/make", 'a'), Some(&["Make"]), &["make"])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            true,
            None,
        )
        .await
        .expect("a self-collision is never a refusal");

        assert_eq!(
            tree.bin_entries().len(),
            trampoline_files("make").len(),
            "one name survives the fold: {:?}",
            tree.bin_entries()
        );
    }

    // ── 2. Prune — validation item 31's four orphan classes (C-044) ─────────

    /// C-044, case 13 — an orphan **trampoline** (a `binaries` claim
    /// disappeared) is removed, and the `<group>/<entry>` link is untouched.
    ///
    /// RED: skip the `bin/` prune pass.
    #[tokio::test]
    async fn an_orphan_trampoline_is_pruned_and_its_group_link_is_left_alone() {
        let tree = Tree::new();
        tree.create_home_root();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        std::fs::write(tree.home.shell_bin(DEFAULT_SHELL).join("gone"), b"#!/bin/sh\n").unwrap();

        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]);
        let expected_target = expected_link_target(&tree.file_structure, &tool);
        let surface = vec![node(pinned("ns/cmake", 'd'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(vec![tool]);
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render succeeds");

        assert_eq!(outcome_of(&report, &trampoline("gone")), &RenderOutcome::Pruned);
        assert_eq!(tree.bin_entries(), expected_bin_entries(&["cmake"]));
        let entry = tree
            .home
            .entry(DEFAULT_GROUP, "cmake")
            .expect("an admitted component pair");
        assert_eq!(
            std::fs::read_link(&entry).expect("the link exists"),
            expected_target,
            "C-044 — the link half of the tree is untouched by the trampoline prune"
        );
    }

    /// C-044, case 14 — an orphan **link** (its entry left the lock) takes its
    /// trampoline with it: both halves are pruned in one render.
    ///
    /// RED: prune only one of the two.
    #[tokio::test]
    async fn an_entry_that_left_the_lock_loses_both_its_link_and_its_trampoline() {
        let tree = Tree::new();
        tree.create_home_root();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        std::fs::write(tree.home.shell_bin(DEFAULT_SHELL).join("ninja"), b"#!/bin/sh\n").unwrap();
        let stale_entry = tree
            .home
            .entry(DEFAULT_GROUP, "ninja")
            .expect("an admitted component pair");
        std::fs::create_dir_all(stale_entry.parent().unwrap()).unwrap();
        std::fs::create_dir_all(tree.tmp.path().join("stale-package")).unwrap();
        crate::symlink::create(tree.tmp.path().join("stale-package"), &stale_entry).unwrap();

        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render succeeds");

        assert_eq!(
            outcome_of(&report, &link(DEFAULT_GROUP, "ninja")),
            &RenderOutcome::Pruned
        );
        assert_eq!(outcome_of(&report, &trampoline("ninja")), &RenderOutcome::Pruned);
        assert!(
            std::fs::symlink_metadata(&stale_entry).is_err(),
            "the link itself is gone, not merely reported"
        );
        assert!(tree.bin_entries().is_empty());
    }

    /// C-044, case 15 — a removed or renamed group leaves the whole
    /// `<group>/` **directory** orphaned, and it is reported as
    /// `RenderedArtifact::GroupDirectory` and removed.
    ///
    /// RED: leave the emptied directory behind — validation item 31's fourth
    /// orphan class then has no representation and no coverage.
    #[tokio::test]
    async fn a_group_no_longer_in_the_lock_loses_its_whole_directory() {
        let tree = Tree::new();
        tree.create_home_root();
        let stale_entry = tree.home.entry("ci", "ninja").expect("an admitted component pair");
        std::fs::create_dir_all(stale_entry.parent().unwrap()).unwrap();
        std::fs::create_dir_all(tree.tmp.path().join("stale-package")).unwrap();
        crate::symlink::create(tree.tmp.path().join("stale-package"), &stale_entry).unwrap();

        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP, "ci"]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render succeeds");

        assert_eq!(outcome_of(&report, &link("ci", "ninja")), &RenderOutcome::Pruned);
        assert_eq!(
            outcome_of(&report, &group_directory("ci")),
            &RenderOutcome::Pruned,
            "the emptied group directory is its own artifact, not an entry it no longer contains"
        );
        assert!(
            !tree.home.root().join("ci").exists(),
            "…and it is gone from disk: {:?}",
            read_dir_names(tree.home.root())
        );
    }

    /// C-044, case 16 — a renamed tool key is one render, not two: both old
    /// spellings are gone and both new ones are present afterwards.
    ///
    /// RED: implement the write pass without the prune pass — the old spelling
    /// survives on PATH beside the new one.
    #[tokio::test]
    async fn a_renamed_tool_key_replaces_both_halves_in_one_render() {
        let tree = Tree::new();
        tree.create_home_root();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        std::fs::write(tree.home.shell_bin(DEFAULT_SHELL).join("old-name"), b"#!/bin/sh\n").unwrap();
        let old_entry = tree.home.entry(DEFAULT_GROUP, "old-name").expect("an admitted pair");
        std::fs::create_dir_all(old_entry.parent().unwrap()).unwrap();
        std::fs::create_dir_all(tree.tmp.path().join("old-package")).unwrap();
        crate::symlink::create(tree.tmp.path().join("old-package"), &old_entry).unwrap();

        let tool = locked_tool("new-name", DEFAULT_GROUP, "ns/tool", &[(PLATFORM_KEY, 'd')]);
        let surface = vec![node(pinned("ns/tool", 'd'), Some(&["new-name"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(vec![tool]);
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render succeeds");

        assert_eq!(outcome_of(&report, &trampoline("old-name")), &RenderOutcome::Pruned);
        assert_eq!(
            outcome_of(&report, &link(DEFAULT_GROUP, "old-name")),
            &RenderOutcome::Pruned
        );
        assert_eq!(
            outcome_of(&report, &link(DEFAULT_GROUP, "new-name")),
            &RenderOutcome::Written
        );
        assert_eq!(tree.bin_entries(), expected_bin_entries(&["new-name"]));
        assert!(std::fs::symlink_metadata(&old_entry).is_err(), "the old link is gone");
    }

    /// S-003, case 17 — a file in `bin/` ocx never wrote is pruned. The
    /// **dotfile** is the discriminating half.
    ///
    /// RED: filter `.`-prefixed entries out of the prune `readdir`. A hostile
    /// clone's `.hidden` then survives every render inside a directory that is
    /// on someone's PATH.
    #[tokio::test]
    async fn a_committed_foreign_file_in_bin_is_pruned_including_a_dotfile() {
        let tree = Tree::new();
        tree.create_home_root();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        std::fs::write(
            tree.home.shell_bin(DEFAULT_SHELL).join("cmake"),
            b"#!/bin/sh\nhostile\n",
        )
        .unwrap();
        std::fs::write(
            tree.home.shell_bin(DEFAULT_SHELL).join(".hidden"),
            b"#!/bin/sh\nhostile\n",
        )
        .unwrap();

        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render succeeds");

        assert_eq!(outcome_of(&report, &trampoline("cmake")), &RenderOutcome::Pruned);
        assert_eq!(
            outcome_of(&report, &trampoline(".hidden")),
            &RenderOutcome::Pruned,
            "S-003 — a dot-prefixed committed file is exactly as foreign as any other"
        );
        assert!(tree.bin_entries().is_empty());
    }

    /// RUL-32, case 18 — an orphan that is a **directory** is `Skipped` with a
    /// reason, never removed recursively, and the render continues to later
    /// entries.
    ///
    /// RED: propagate the error instead — one hostile directory then aborts
    /// every later entry.
    #[tokio::test]
    async fn an_orphan_directory_in_bin_is_skipped_and_does_not_abort_the_render() {
        let tree = Tree::new();
        tree.create_home_root();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL).join("hostile")).unwrap();
        std::fs::write(
            tree.home.shell_bin(DEFAULT_SHELL).join("hostile").join("payload"),
            b"keep me\n",
        )
        .unwrap();

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("RUL-32 — a hostile directory is a skip, never an error");

        assert!(
            is_skipped(outcome_of(&report, &trampoline("hostile"))),
            "the orphan directory is reported as skipped: {report:?}"
        );
        assert!(
            tree.home
                .shell_bin(DEFAULT_SHELL)
                .join("hostile")
                .join("payload")
                .exists(),
            "…and nothing under it was removed recursively"
        );
        assert!(
            tree.bin_entries().contains(&trampoline_files("cmake")[0]),
            "…and the render continued to the entries after it: {:?}",
            tree.bin_entries()
        );
    }

    /// C-044/RUL-40, case 19 — an orphan whose on-disk name
    /// `ToolchainHome::entry`'s grammar would refuse is **pruned**, by that
    /// on-disk name exactly as `read_dir` yielded it.
    ///
    /// The validator exists to stop untrusted *input* becoming a path
    /// component; a name yielded by `read_dir` of a directory already proven to
    /// be inside the home is not that. Routing the prune through it would make
    /// a hostile clone's `<group>/` named `C:` or `a\b` unprunable forever — a
    /// permanent foothold inside a tree ocx owns. Containment is
    /// [`prune_within`]'s canonicalising check, which is the right guard.
    ///
    /// RED: route the prune through `ToolchainHome::entry`'s grammar validator
    /// — both directories then survive every render.
    ///
    /// `#[cfg(unix)]`, because the fixture cannot build its own premise
    /// anywhere else: neither `C:` nor `a\b` is a filename on Windows, and
    /// `Path::join` reinterprets both before they reach the disk — `C:` is a
    /// drive prefix that discards the home root outright, `a\b` is two
    /// components. `read_dir` there can never yield either name, so there is no
    /// orphan to prune and the row would assert against a tree it did not
    /// create. The Windows half of the same rule is
    /// [`is_single_component`], pinned by
    /// `the_single_component_test_reads_this_platforms_parser` below — that is
    /// where a `C:` reaching the prune is refused rather than joined.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_orphan_whose_name_the_grammar_refuses_is_reported_rather_than_dropped() {
        let tree = Tree::new();
        tree.create_home_root();
        for hostile in ["C:", "a\\b"] {
            std::fs::create_dir_all(tree.home.root().join(hostile)).unwrap();
        }

        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("a hostile on-disk name is not an error");

        for hostile in ["C:", "a\\b"] {
            assert_eq!(
                outcome_of(&report, &root_entry(hostile)),
                &RenderOutcome::Pruned,
                "RUL-40 — `{hostile}` is pruned by its on-disk name, not left unprunable by a \
                 grammar validator that exists for untrusted input"
            );
            assert!(
                !tree.home.root().join(hostile).exists(),
                "…and it is gone from disk: {:?}",
                read_dir_names(tree.home.root())
            );
        }
    }

    /// RUL-40's containment half, on **every** host: the name the prune is
    /// about to `join` must be one ordinary component *to this platform's own
    /// parser*, because that same parser performs the removal.
    ///
    /// This is the Windows counterpart of the row above, which can only run on
    /// a host where `C:` and `a\b` are creatable filenames. Here the asymmetry
    /// is the assertion rather than the fixture, so both hosts are covered:
    /// what is one component on Unix is a drive prefix or two components on
    /// Windows, and a `PathBuf::join` on either discards or escapes the parent.
    ///
    /// RED: return `true` unconditionally from `is_single_component` — the
    /// Windows arm fires. RED: return `false` for every name — the shared arm
    /// fires, so this is not a check that only ever refuses.
    #[test]
    fn the_single_component_test_reads_this_platforms_parser() {
        for ordinary in ["cmake", "Make", "a.b", "..hidden"] {
            assert!(
                is_single_component(ordinary),
                "{ordinary} is one ordinary component on every platform"
            );
        }
        for escaping in ["a/b", "..", "/abs", ""] {
            assert!(
                !is_single_component(escaping),
                "{escaping:?} is not a single ordinary component on any platform"
            );
        }
        for windows_only in ["C:", r"a\b"] {
            assert_eq!(
                is_single_component(windows_only),
                cfg!(not(windows)),
                "{windows_only:?} is an ordinary name on Unix and a prefix or a separator on \
                 Windows — the prune must follow the parser that will do the join"
            );
        }
    }

    /// The `RenderedArtifact` doc limit, case 20 — a non-UTF-8 `bin/` entry
    /// has no representation in the report, is therefore **never pruned**, and
    /// survives the render as an untouched foreign file.
    ///
    /// Specified rather than left implicit: the alternative reading — "the
    /// prune is exact for every entry" — is false, and validation item 31's
    /// exactness claim has to be read against this limit.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_non_utf8_bin_entry_is_neither_reported_nor_pruned() {
        use std::os::unix::ffi::OsStrExt as _;

        let tree = Tree::new();
        tree.create_home_root();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        let hostile = tree
            .home
            .shell_bin(DEFAULT_SHELL)
            .join(std::ffi::OsStr::from_bytes(b"invalid-\xff-name"));
        if let Err(refused) = std::fs::write(&hostile, b"#!/bin/sh\n") {
            // APFS validates filenames as UTF-8 and rejects these bytes
            // outright, so the on-disk state under test cannot exist on a macOS
            // volume. Observed, not assumed via `target_os`: the row still runs
            // on any filesystem that accepts the name, and a refusal for any
            // other reason (an absent `bin/`) reds here rather than passing as
            // this carve-out. Same shape as `index_store`'s two non-UTF-8 rows.
            assert!(
                tree.home.shell_bin(DEFAULT_SHELL).exists(),
                "only the non-UTF-8 component may be refused; bin/ must exist: {refused}"
            );
            eprintln!("skipped: this filesystem refuses a non-UTF-8 filename: {refused}");
            return;
        }

        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("an unnameable entry is not an error");

        assert!(
            report.items.is_empty(),
            "the entry cannot become a `RenderedArtifact`, so it is not reported: {report:?}"
        );
        assert!(
            std::fs::symlink_metadata(&hostile).is_ok(),
            "…and it survives as an untouched foreign file"
        );
    }

    /// C-004, case 21 — `.gitignore` is never pruned, including by a render
    /// whose computed set is empty.
    ///
    /// RED: widen the prune `readdir` from `bin/` to the home root.
    #[tokio::test]
    async fn the_gitignore_is_never_pruned_even_by_an_empty_render() {
        let tree = Tree::new();
        tree.create_home_root();
        tree.home.ensure_gitignore().expect("the ignore file is writable");
        let before = std::fs::read(tree.home.gitignore()).unwrap();

        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render succeeds");

        assert_eq!(
            std::fs::read(tree.home.gitignore()).unwrap(),
            before,
            "C-004 — the ignore file is the home's own, not an orphan"
        );
    }

    /// C-003, case 22 — a render touches nothing under
    /// `state/projects/<key>/` except the render stamp itself.
    ///
    /// RED: write the stamp inside the home and watch a root-level prune eat
    /// it, or widen the prune to the state store.
    #[tokio::test]
    async fn a_render_touches_nothing_in_the_project_state_directory_but_the_stamp() {
        let tree = Tree::new();
        tree.create_home_root();
        let state_dir = tree.file_structure.state.project_state_dir(&tree.key());
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::write(state_dir.join("consent.json"), b"{}\n").unwrap();

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render succeeds");

        assert_eq!(
            std::fs::read(state_dir.join("consent.json")).unwrap(),
            b"{}\n",
            "C-003 — the consent stamp shares the `<key>` directory and is not the renderer's"
        );
        let mut names = read_dir_names(&state_dir);
        names.sort();
        assert_eq!(
            names,
            vec!["consent.json".to_string(), "render_stamp.json".to_string()],
            "the render adds exactly one file to that directory"
        );
    }

    /// C-045, case 23 — a non-default group the invocation did not select is
    /// byte-identical after `ocx pull -g default`.
    ///
    /// RED: reconcile the whole home root against `request.groups` — silent
    /// data loss across every group the invocation did not name.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_group_the_invocation_did_not_select_is_byte_identical_afterwards() {
        let tree = Tree::new();
        tree.create_home_root();
        let ci_entry = tree.home.entry("ci", "ninja").expect("an admitted component pair");
        std::fs::create_dir_all(ci_entry.parent().unwrap()).unwrap();
        std::fs::create_dir_all(tree.tmp.path().join("ci-package")).unwrap();
        crate::symlink::create(tree.tmp.path().join("ci-package"), &ci_entry).unwrap();
        let before = snapshot_subtree(&tree.home.links_group("ci").expect("an admitted group name"));

        let tool = locked_tool("ninja", "ci", "ns/ninja", &[(PLATFORM_KEY, 'd')]);
        let scope = tree.scope();
        let lock = lock_of(vec![tool]);
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render succeeds");

        assert_eq!(
            snapshot_subtree(&tree.home.links_group("ci").expect("an admitted group name")),
            before,
            "C-045 — `-g default` reconciles the default group and nothing else"
        );
    }

    /// V-30 / RUL-33 — a symlink at `<home>/bin` is refused **at the create**,
    /// never followed.
    ///
    /// Step 2's `refuse_symlinked_home_leaves` judges `bin/` and then the
    /// render **lock acquisition** runs, blocking for an unbounded time; a
    /// project home inside a group-writable checkout can have `bin/` swapped in
    /// that window. This test plants the link where such a swap leaves it — the
    /// check has already passed — and asserts the publish does not follow it.
    ///
    /// RED: `create_owner_only` in place of `create_bin_owner_only`. Its
    /// `create_dir_all` stats with `metadata`, which follows the link, returns
    /// `Ok`, and every trampoline lands in `outside/` — a directory that then
    /// goes on the user's `PATH`.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_bin_directory_is_refused_at_the_create_never_followed() {
        let tree = Tree::new();
        tree.create_home_root();
        let outside = tree.tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, tree.home.shell_bin(DEFAULT_SHELL)).unwrap();

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("RUL-33 degrades, never errors");

        assert!(
            std::fs::read_dir(&outside)
                .expect("the planted target is readable")
                .next()
                .is_none(),
            "nothing may be published through a symlinked `bin/` — the link's target must stay empty"
        );
        assert!(
            report
                .items
                .iter()
                .any(|item| matches!(item.outcome, RenderOutcome::Skipped { .. })),
            "C-050 — the refusal is reported as a skip, one warn line each"
        );
    }

    /// C-053, case 24 — setting and then unsetting `toolchain-dir` leaves the
    /// **abandoned** tree byte-identical and renders a complete new one.
    ///
    /// RED: pass a "previous home" to the prune pass — a mistaken `[managed]`
    /// push then destroys trees across a fleet.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_tree_at_a_home_no_longer_resolved_is_left_exactly_as_it_is() {
        let tree = Tree::new();
        tree.create_home_root();
        let relocated = ToolchainHome::new(tree.tmp.path().join("relocated").join("toolchain"));
        seed_rendered_home(&relocated);

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();

        // `toolchain-dir` unset: the project's own home renders.
        render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the in-project home renders");
        let abandoned = snapshot_subtree(tree.home.root());
        assert!(!abandoned.is_empty(), "precondition: there is a tree to abandon");

        // `toolchain-dir` set: the home re-resolves, and every later render
        // goes there. The tree at the old location is now unreachable to ocx.
        render_with(
            &tree.file_structure,
            RenderRequest {
                home: &relocated,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the relocated home renders again");

        assert_eq!(
            snapshot_subtree(tree.home.root()),
            abandoned,
            "C-053 — a tree at a location ocx no longer resolves to is left in place, never deleted"
        );
        assert_eq!(
            read_dir_names(&relocated.shell_bin(DEFAULT_SHELL)),
            expected_bin_entries(&["cmake"]),
            "…and the new home is complete"
        );
    }

    /// C-053/S-4/RUL-33, case 25(a) — a home root committed as a **symlink**
    /// (to `$HOME`, in the wild) is refused before anything is created.
    ///
    /// `create_dir_all` on an existing symlink-to-a-directory succeeds
    /// silently, so the first write would already be outside the project.
    #[cfg(unix)]
    #[test]
    fn ensure_home_root_refuses_a_symlinked_home_root() {
        let tree = Tree::new();
        let outside = tree.tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(tree.home.root().parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&outside, tree.home.root()).unwrap();

        assert!(
            ensure_home_root(&tree.home).is_err(),
            "RUL-33 — a symlinked home root is refused, judged by symlink_metadata"
        );
    }

    /// C-053/S-4/RUL-33, case 25(b) — a `bin/` committed as a symlink is
    /// refused too: every `bin/<name>` write and removal would otherwise land
    /// in the link's target directory.
    #[cfg(unix)]
    #[test]
    fn ensure_home_root_refuses_a_symlinked_bin_directory() {
        let tree = Tree::new();
        let outside = tree.tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(tree.shell_directory()).unwrap();
        std::os::unix::fs::symlink(&outside, tree.home.shell_bin(DEFAULT_SHELL)).unwrap();

        assert!(
            ensure_home_root(&tree.home).is_err(),
            "RUL-33 — a symlinked `bin/` is the same hole one level down"
        );
    }

    /// C-053/S-4, case 25(b) delete side — `prune_within` refuses a
    /// `bin/<name>` that resolves outside the home through a symlinked `bin/`.
    ///
    /// RED: replace the canonicalising containment check with `starts_with` —
    /// the spelled path is still under the home, so the delete escapes.
    #[cfg(unix)]
    #[test]
    fn prune_within_refuses_a_trampoline_reached_through_a_symlinked_bin() {
        let tree = Tree::new();
        let outside = tree.tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("cmake"), b"not ours\n").unwrap();
        std::fs::create_dir_all(tree.shell_directory()).unwrap();
        std::os::unix::fs::symlink(&outside, tree.home.shell_bin(DEFAULT_SHELL)).unwrap();

        assert!(
            prune_within(&tree.home, &trampoline("cmake")).is_err(),
            "C-053 — the delete path canonicalises both sides before comparing"
        );
        assert!(
            outside.join("cmake").exists(),
            "…and the file outside the home survives"
        );
    }

    /// C-053/S-4, case 25(c) — a `<group>/` that is a symlink out of the home
    /// does the same for every `<group>/<entry>`, and `prune_within` refuses
    /// that too.
    #[cfg(unix)]
    #[test]
    fn prune_within_refuses_an_entry_reached_through_a_symlinked_group_directory() {
        let tree = Tree::new();
        let outside = tree.tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("cmake"), b"not ours\n").unwrap();
        std::fs::create_dir_all(links_root(&tree.home)).unwrap();
        std::os::unix::fs::symlink(&outside, links_root(&tree.home).join(DEFAULT_GROUP)).unwrap();

        assert!(
            prune_within(&tree.home, &link(DEFAULT_GROUP, "cmake")).is_err(),
            "C-053 — a group directory that is a symlink out of the home is the third route"
        );
        assert!(outside.join("cmake").exists());
    }

    /// C-044/C-053 — the positive half of the containment guard: an artifact
    /// that really is inside the home **is** removed.
    ///
    /// Without this, the three refusals above would be satisfied by a
    /// `prune_within` that refuses everything — the always-red shape that is
    /// half a proof.
    #[test]
    fn prune_within_removes_an_artifact_that_resolves_inside_the_home() {
        let tree = Tree::new();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        let victim = tree.home.shell_bin(DEFAULT_SHELL).join("cmake");
        std::fs::write(&victim, b"#!/bin/sh\n").unwrap();

        prune_within(&tree.home, &trampoline("cmake")).expect("an entry inside the home is removable");
        assert!(!victim.exists(), "C-044 — the prune half actually removes");
    }

    /// RUL-32 — `prune_within` never removes a group directory recursively: a
    /// directory still holding a foreign file fails its non-recursive
    /// `remove_dir` and the foreign file survives.
    ///
    /// RED: `remove_dir_all` — a recursive delete primitive reachable from a
    /// hostile `ocx.lock`.
    #[test]
    fn prune_within_never_removes_a_non_empty_group_directory_recursively() {
        let tree = Tree::new();
        let group = tree.home.links_group("ci").expect("an admitted group name");
        std::fs::create_dir_all(&group).unwrap();
        std::fs::write(group.join("foreign"), b"keep me\n").unwrap();

        assert!(
            prune_within(&tree.home, &group_directory("ci")).is_err(),
            "RUL-32 — a non-empty group directory fails `remove_dir` rather than being emptied"
        );
        assert!(
            group.join("foreign").exists(),
            "…and the foreign file the render did not put there survives"
        );
    }

    // ── 3. Concurrency and the atomic-publish shapes ────────────────────────

    /// C-048/C-061, case 27 — a trampoline rewrite is temp-file-plus-rename,
    /// never a truncate in place.
    ///
    /// The **inode** is the discriminating assertion: a truncating write
    /// leaves the bytes correct and the inode identical, and a reconciler
    /// reading `bin/` concurrently would then observe a torn file. A running
    /// trampoline keeping the old inode is the other half of the same
    /// property.
    ///
    /// RED: make the write truncate in place.
    #[cfg(unix)]
    #[test]
    fn write_trampoline_atomic_replaces_the_inode_rather_than_truncating_in_place() {
        let tree = Tree::new();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        let path = tree.home.shell_bin(DEFAULT_SHELL).join("cmake");
        std::fs::write(&path, b"#!/bin/sh\nold\n").unwrap();
        let before = inode_of(&path);

        write_trampoline_atomic(&path, "#!/bin/sh\nnew\n").expect("the write succeeds");

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "#!/bin/sh\nnew\n");
        assert_ne!(
            inode_of(&path),
            before,
            "C-061 — the publish is a rename over the live file, so a running trampoline keeps \
             the old inode and no reader ever sees a half-written one"
        );
    }

    /// C-044, case 38 — a published trampoline is **owner-executable**.
    ///
    /// Asserted as `mode & 0o100 != 0`, never `== 0o755`: the exact mode
    /// depends on the ambient umask, so an equality assertion would be a CI
    /// flake rather than a check.
    ///
    /// RED: drop the mode set — the default `0o600` publishes a
    /// non-executable trampoline onto someone's PATH.
    #[cfg(unix)]
    #[test]
    fn write_trampoline_atomic_publishes_an_owner_executable_file() {
        let tree = Tree::new();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        let path = tree.home.shell_bin(DEFAULT_SHELL).join("cmake");

        write_trampoline_atomic(&path, "#!/bin/sh\n").expect("the write succeeds");

        assert_ne!(
            mode_of(&path) & 0o100,
            0,
            "C-044 — the owner-execute bit is set, and it is set on the temp file before the \
             rename so no window exists in which the entry is present and not executable"
        );
    }

    /// C-044, case 38 through the render — the same property must hold for
    /// what a whole render publishes, not only for the seam.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_rendered_trampoline_is_owner_executable() {
        let tree = Tree::new();
        tree.create_home_root();

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render succeeds");

        assert_ne!(mode_of(&tree.home.shell_bin(DEFAULT_SHELL).join("cmake")) & 0o100, 0);
    }

    /// C-048, case 28 — the ADR's named negative control: a fault injected
    /// **between the first entry write and the stamp write** leaves `bin/`
    /// holding at least one entry, no new stamp, and any previous stamp
    /// unmodified.
    ///
    /// The fault is a **parameter**, not an environment write: the override
    /// table `crate::test::env::EnvLock` maintains is consulted only by
    /// `crate::env::var`, never by `std::env::var_os`, so an env-only seam
    /// would have forced `unsafe { std::env::set_var }` into this test.
    ///
    /// RED: write the stamp before the entry pass — the tree then reports as
    /// finished while it is half-written.
    #[tokio::test]
    async fn a_fault_between_the_first_entry_and_the_stamp_leaves_a_detectably_incomplete_tree() {
        let tree = Tree::new();
        tree.create_home_root();
        let state_dir = tree.file_structure.state.project_state_dir(&tree.key());
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::write(tree.stamp_file(), b"{\"previous\": true}\n").unwrap();

        let surface = vec![
            node(pinned("ns/one", 'a'), Some(&["one"]), &[]),
            node(pinned("ns/two", 'b'), Some(&["two"]), &[]),
        ];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let result = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            Some(FAULT_AFTER_FIRST_ENTRY_WRITE),
        )
        .await;

        assert!(
            result.is_err(),
            "C-048 — the injected fault aborts the render: {result:?}"
        );
        assert!(
            !tree.bin_entries().is_empty(),
            "…after at least one entry landed, which is what makes the tree *partially* rendered"
        );
        assert_eq!(
            std::fs::read(tree.stamp_file()).unwrap(),
            b"{\"previous\": true}\n",
            "…and the previous stamp is untouched, so C-061's gate sees a mismatch rather than \
             a tree that looks finished"
        );
    }

    /// C-048 — the fault seam fires for its one stage and never otherwise.
    ///
    /// The positive control for the test above: without it, an
    /// always-`Ok` `maybe_inject_fault` and a correctly-ordered render are
    /// indistinguishable.
    #[test]
    fn the_fault_seam_fires_only_for_its_own_stage_and_value() {
        assert!(
            maybe_inject_fault(Some(FAULT_AFTER_FIRST_ENTRY_WRITE), RenderStage::AfterFirstEntryWrite).is_err(),
            "C-048 — the named value at the named stage aborts"
        );
        assert!(
            maybe_inject_fault(None, RenderStage::AfterFirstEntryWrite).is_ok(),
            "…and an absent fault never does"
        );
        assert!(
            maybe_inject_fault(Some("some-other-stage"), RenderStage::AfterFirstEntryWrite).is_ok(),
            "…nor does an unrelated value"
        );
    }

    /// C-051, case 30 — repointing a `<group>/<entry>` is an atomic replace: a
    /// reader in a tight loop sees the old target or the new one, never
    /// absence.
    ///
    /// RED: use remove-then-create — the reader observes `ENOENT`, and
    /// C-067's per-entry digest degrade fires spuriously on a link that is
    /// merely mid-repoint.
    ///
    /// One-sided by construction: a passing run does not prove the window is
    /// impossible, only that this reader never hit it. It is written this way
    /// because the alternative — asserting on the implementation's choice of
    /// primitive — is a source-text guard for a property with a real
    /// behavioural seam.
    #[cfg(unix)]
    #[tokio::test]
    async fn repointing_a_group_entry_never_makes_it_momentarily_absent() {
        let tree = Tree::new();
        tree.create_home_root();
        let entry = tree.home.entry(DEFAULT_GROUP, "cmake").expect("an admitted pair");
        std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
        std::fs::create_dir_all(tree.tmp.path().join("stale-package")).unwrap();
        crate::symlink::create(tree.tmp.path().join("stale-package"), &entry).unwrap();

        let observed_absence = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader = {
            let (entry, observed_absence, stop) = (entry.clone(), observed_absence.clone(), stop.clone());
            // The deadline, not the flag, is what bounds this thread: a
            // panicking render never reaches `stop.store`, and a busy loop that
            // outlived its test would spin for the rest of the run.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) && std::time::Instant::now() < deadline {
                    if std::fs::symlink_metadata(&entry).is_err() {
                        observed_absence.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            })
        };

        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]);
        let expected_target = expected_link_target(&tree.file_structure, &tool);
        let scope = tree.scope();
        let lock = lock_of(vec![tool]);
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        // The result is captured before the reader is stopped, so a panicking
        // render cannot leave the busy-loop thread spinning for the rest of
        // the run.
        let result = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await;
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        reader.join().expect("the reader thread joins");
        result.expect("the repoint succeeds");

        assert!(
            !observed_absence.load(std::sync::atomic::Ordering::Relaxed),
            "C-051 — the repoint is an atomic replace, so a concurrent reader never sees the \
             link absent"
        );
        assert_eq!(std::fs::read_link(&entry).unwrap(), expected_target);
    }

    /// RUL-30/RUL-38, case 26 — two renders against one home are **serialized**
    /// by the render lock, which is discriminated by the stamp key.
    ///
    /// Two terminals running `ocx pull` is an ordinary state; unlocked they
    /// interleave writes and prunes and race the stamp into a C-061 mismatch no
    /// later render clears, because each run's stamp describes a tree the other
    /// was still editing.
    ///
    /// Driven through [`render_lock_parameters`] so the test takes *the same*
    /// lock rather than one that merely resembles it — without that, "the render
    /// takes a lock" would be green in a way indistinguishable from never having
    /// run.
    ///
    /// RED: remove the lock — the render then returns immediately instead of
    /// waiting for the held guard, and the elapsed assertion reds.
    #[tokio::test]
    async fn two_renders_against_one_home_are_serialized_by_the_render_lock() {
        let tree = Tree::new();
        tree.create_home_root();

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();

        let parameters = render_lock_parameters(
            &tree.file_structure,
            &RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
        );
        assert_eq!(parameters.scope, "toolchain-render");
        assert_eq!(
            parameters.discriminator,
            tree.key(),
            "RUL-38 — discriminated by the stamp key, so two homes never share a lock and one \
             home never takes two"
        );
        assert_eq!(parameters.guarded_directory, tree.home.root());
        let held = parameters.acquire().await.expect("the test can take the same lock");

        let hold = std::time::Duration::from_millis(150);
        let manager = tree.manager();
        let started = std::time::Instant::now();
        let (rendered, ()) = tokio::join!(
            manager.render_toolchain(RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            }),
            async {
                tokio::time::sleep(hold).await;
                drop(held);
            }
        );
        let elapsed = started.elapsed();

        rendered.expect("the render succeeds once the lock is free");
        assert!(
            elapsed >= hold,
            "RUL-30 — the second render waited for the first's lock rather than interleaving with \
             it: {elapsed:?}"
        );
        assert_eq!(tree.bin_entries(), expected_bin_entries(&["cmake"]));
    }

    /// C-050/RUL-30/RUL-38, case 52 — a render lock that cannot be acquired
    /// within its timeout is a **skip**, never an error.
    ///
    /// C-050's own trigger list names "lock timeout" outright: the call warns,
    /// returns empty `items` and `stamp_written: false`, and is `Ok`. The
    /// elapsed assertion is what makes this a timeout rather than any other
    /// acquisition failure.
    ///
    /// It costs the lock's own timeout in wall-clock, deliberately: shortening
    /// it would mean asserting on a value the render does not use.
    ///
    /// RED: propagate the timeout — `ocx pull` then fails outright because a
    /// second terminal happened to be pulling.
    #[tokio::test]
    async fn a_render_lock_timeout_is_a_skip_and_never_an_error() {
        let tree = Tree::new();
        tree.create_home_root();

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();

        let parameters = render_lock_parameters(
            &tree.file_structure,
            &RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
        );
        // Held for the whole call: the render must time out rather than acquire.
        let _held = parameters.acquire().await.expect("the test can take the same lock");

        let started = std::time::Instant::now();
        let report = tree
            .manager()
            .render_toolchain(RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            })
            .await
            .expect("C-050 — a lock timeout is a skip, never an error");
        let elapsed = started.elapsed();

        assert!(
            elapsed >= parameters.timeout,
            "the render actually waited out its own timeout rather than failing for another \
             reason: {elapsed:?} < {:?}",
            parameters.timeout
        );
        assert!(report.items.is_empty(), "nothing was rendered: {report:?}");
        assert!(!report.stamp_written, "…and nothing was stamped");
        assert!(tree.stamp().is_none());
        assert!(
            tree.bin_entries().is_empty(),
            "…and `bin/` was left exactly as the lock holder had it: {:?}",
            tree.bin_entries()
        );
    }

    // ── 4. Platform divergence ──────────────────────────────────────────────

    /// RUL-35/C-031, case 32 — the Windows publish hardlinks `<name>.exe` from
    /// the `ShimBinStore` blob: byte-equal **and sharing the blob's file
    /// identity**, which a copy would not.
    ///
    /// Not `#[cfg(windows)]`: both writes are cross-platform primitives, and
    /// gating the seam would put its one testable property behind a `cfg` the
    /// CI leg that actually runs never compiles.
    ///
    /// RED: copy the blob instead of hardlinking — the bytes still match, only
    /// the identity assertion reds.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_windows_publish_hardlinks_the_exe_from_the_shim_blob() {
        let tree = Tree::new();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        let exe = tree.home.shell_bin(DEFAULT_SHELL).join("cmake.exe");
        let sidecar = tree.home.shell_bin(DEFAULT_SHELL).join("cmake.exec");

        let blob = tree
            .file_structure
            .shim_bin
            .ensure()
            .await
            .expect("the committed shim blob publishes");
        publish_windows_trampoline(&tree.file_structure.shim_bin, &exe, &sidecar, "C:\\proj\n")
            .await
            .expect("the Windows publish succeeds");

        assert_eq!(
            std::fs::read(&exe).unwrap(),
            std::fs::read(&blob).unwrap(),
            "C-031 — the `.exe` is byte-identical to the published blob"
        );
        assert_eq!(
            inode_of(&exe),
            inode_of(&blob),
            "RUL-35 — …by construction, through a hardlink, so the Authenticode verbatim-copy \
             property survives; a copy would satisfy the bytes and fail here"
        );
        assert_eq!(std::fs::read_to_string(&sidecar).unwrap(), "C:\\proj\n");
    }

    /// RUL-35, case 32's ordering postcondition — the **sidecar lands first**.
    ///
    /// The only recoverable partial state this function may leave is
    /// `.exec`-present / `.exe`-absent. The inverse is directly harmful:
    /// `which cmake` resolves `cmake.exe`, the shim runs, finds no selector
    /// sidecar and fails at invocation — worse than the PATH lookup it
    /// replaced. Driven by making the hardlink fail while the sidecar write
    /// can still succeed.
    ///
    /// RED: swap the two writes, following the `prepare_lazy` precedent whose
    /// premise (a staged tree nobody's PATH contains) does not hold for `bin/`.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_windows_publish_writes_the_sidecar_before_the_exe() {
        let tree = Tree::new();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        let sidecar = tree.home.shell_bin(DEFAULT_SHELL).join("cmake.exec");
        // A directory at the `.exe` path makes the hardlink fail while the
        // sidecar write beside it still succeeds — the one input that tells
        // the two orderings apart.
        let exe = tree.home.shell_bin(DEFAULT_SHELL).join("cmake.exe");
        std::fs::create_dir_all(&exe).unwrap();

        let result = publish_windows_trampoline(&tree.file_structure.shim_bin, &exe, &sidecar, "C:\\proj\n").await;

        assert!(result.is_err(), "the blocked `.exe` publish fails: {result:?}");
        assert!(
            sidecar.is_file(),
            "RUL-35 — the sidecar was already written when the `.exe` failed; the reverse order \
             would leave an `.exe` on PATH with no selector"
        );
    }

    /// RUL-26, case 33 — the stamp keys on the **on-disk file set**: after a
    /// render, `readdir(bin/)` and `bin_fingerprint` have the same length.
    ///
    /// Host-agnostic on purpose. It is satisfied trivially on POSIX (one file
    /// per name) and is the assertion a Windows-only defect reds: `bin/` holds
    /// two files per name there, so a stamp keyed on the bare stem is a
    /// permanent C-061 mismatch — the PATH entry withheld and the `ocx pull`
    /// hint printed on every prompt, forever.
    #[tokio::test]
    async fn the_stamp_fingerprint_has_one_entry_per_on_disk_bin_file() {
        let tree = Tree::new();
        tree.create_home_root();

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake", "ctest"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render succeeds");

        let stamp = tree.stamp().expect("the stamp is written");
        assert_eq!(
            stamp.bin_fingerprint.len(),
            tree.bin_entries().len(),
            "RUL-26 — one fingerprint entry per on-disk file, keyed by its full on-disk name"
        );
        assert_eq!(
            stamp.bin_fingerprint.keys().cloned().collect::<Vec<_>>(),
            tree.bin_entries(),
            "…and the keys are those names exactly, which is what C-061's readdir gate compares"
        );
    }

    /// RUL-20/D-V21, case 34 — `trampoline_target` performs **no** validation:
    /// a non-absolute project root constructs fine here, and the refusal fires
    /// downstream at the writer.
    ///
    /// RED: move the refusal into `trampoline_target` — the `.exec`-collision
    /// regression in `body.rs` (`Project("global")` emitting a sidecar
    /// byte-identical to the global home's) then loses its anchor, because the
    /// input that produces it can no longer be constructed.
    #[test]
    fn trampoline_target_maps_the_tier_and_validates_nothing() {
        assert!(
            matches!(trampoline_target(&RenderStampScope::Global), TrampolineTarget::Global),
            "C-028 — the global tier bakes the valueless `--global`"
        );
        let relative = PathBuf::from("global");
        assert!(
            matches!(
                trampoline_target(&RenderStampScope::Project(relative.clone())),
                TrampolineTarget::Project(root) if root == relative
            ),
            "RUL-20 — a non-absolute root constructs; `unix_trampoline_body` and the `.exec` \
             writer are where D-V21's refusal fires"
        );
    }

    /// C-028, case 41 and validation item 12's positive control — the same
    /// project rendered from two **different absolute directories** produces
    /// different bodies.
    ///
    /// This is the one input that rewrites a body, which is why it, and not a
    /// `pinned` flip, is item 12's control: C-046 asserts bodies stay
    /// byte-identical across a `pinned` flip on purpose.
    ///
    /// RED: derive `TrampolineTarget` from `request.home` instead of
    /// `request.scope`.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_same_project_rendered_from_two_directories_bakes_two_different_bodies() {
        let tree = Tree::new();
        let other_project = tree.tmp.path().join("proj-elsewhere");
        std::fs::create_dir_all(&other_project).unwrap();
        let other_home = ToolchainHome::new(other_project.join(".ocx").join("toolchain"));
        tree.create_home_root();
        seed_rendered_home(&other_home);

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();

        for (home, project_dir) in [
            (&tree.home, tree.project_dir.clone()),
            (&other_home, other_project.clone()),
        ] {
            let scope = RenderStampScope::Project(project_dir);
            render_with(
                &tree.file_structure,
                RenderRequest {
                    home,
                    scope: &scope,
                    lock: &lock,
                    surface: &surface,
                    groups: &groups,
                    pinned: false,
                    platform: &platform,
                    dry_run: false,
                },
                false,
                None,
            )
            .await
            .expect("both render");
        }

        let first = std::fs::read_to_string(tree.home.shell_bin(DEFAULT_SHELL).join("cmake")).unwrap();
        let second = std::fs::read_to_string(other_home.shell_bin(DEFAULT_SHELL).join("cmake")).unwrap();
        assert_ne!(
            first, second,
            "C-028 — the baked selector names the project, so two projects bake two bodies"
        );
        assert_eq!(
            first,
            expected_unix_trampoline_body(&tree.project_dir, &tree.ocx_binary),
            "…and each body is exactly the shipped grammar, not merely different"
        );
        assert_eq!(second, expected_unix_trampoline_body(&other_project, &tree.ocx_binary));
    }

    /// C-046 and validation item 33, case 42(a) — flipping `pinned` leaves
    /// `bin/` **byte-identical**, and RUL-23 leaves the link pass entirely
    /// alone: no writes and no prunes.
    ///
    /// RED: bake `pinned` (or any composition flag) into the body — the flag
    /// then stops taking effect without a re-render, and item 11's parity
    /// oracle breaks with it. Second RED: reconcile the link set to empty
    /// under `pinned` — switching it on would delete the tree.
    #[cfg(unix)]
    #[tokio::test]
    async fn flipping_pinned_leaves_both_halves_of_the_tree_byte_identical() {
        let tree = Tree::new();
        tree.create_home_root();

        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]);
        let surface = vec![node(pinned("ns/cmake", 'd'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(vec![tool]);
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the following-lane render succeeds");
        let before = snapshot_subtree(tree.home.root());

        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: true,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the pinned render succeeds");

        assert_eq!(
            std::fs::read_to_string(tree.home.shell_bin(DEFAULT_SHELL).join("cmake")).unwrap(),
            expected_unix_trampoline_body(&tree.project_dir, &tree.ocx_binary),
            "C-046 — no digest and no `pinned` value is ever baked, so the body is unchanged"
        );
        assert!(
            report
                .items
                .iter()
                .all(|item| !matches!(item.artifact, RenderedArtifact::Link { .. })),
            "RUL-23 — `pinned = true` suppresses the entire link pass, writes and prunes alike: \
             {report:?}"
        );
        let after = snapshot_subtree(tree.home.root());
        let entry = tree.home.entry(DEFAULT_GROUP, "cmake").expect("an admitted pair");
        assert!(
            std::fs::symlink_metadata(&entry).is_ok(),
            "…so the link written by the previous render is still there"
        );
        assert_eq!(
            after.iter().filter(|e| e.kind == "symlink").count(),
            before.iter().filter(|e| e.kind == "symlink").count(),
            "…and no link was removed"
        );
    }

    /// RUL-27, case 42's stamp half — a **pinned** render still stamps the
    /// default group's links, observed by one `readlink` pass over what is on
    /// disk rather than derived from the lock.
    ///
    /// RED: stamp an empty `link_fingerprint` under `pinned` — C-061's gate
    /// then mismatches on every prompt forever in exactly the cells validation
    /// item 20's `activate × pinned` matrix has to cover.
    #[tokio::test]
    async fn a_pinned_render_stamps_the_links_that_are_on_disk() {
        let tree = Tree::new();
        tree.create_home_root();

        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]);
        let surface = vec![node(pinned("ns/cmake", 'd'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(vec![tool]);
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        for pinned_flag in [false, true] {
            render_with(
                &tree.file_structure,
                RenderRequest {
                    home: &tree.home,
                    scope: &scope,
                    lock: &lock,
                    surface: &surface,
                    groups: &groups,
                    pinned: pinned_flag,
                    platform: &platform,
                    dry_run: false,
                },
                false,
                None,
            )
            .await
            .expect("both renders succeed");
        }

        let stamp = tree.stamp().expect("the pinned render still writes a stamp");
        assert!(
            !stamp.link_fingerprint.is_empty(),
            "RUL-27 — the stamp describes the tree as it stands on disk, never the tree the \
             render deliberately did not write: {stamp:?}"
        );
    }

    /// C-003 — the **persisted** `link_fingerprint` key is `"<group>/<entry>"`,
    /// and the tree's internal shape is no part of it.
    ///
    /// `observe_default_group_links` builds the key from the `DEFAULT_GROUP`
    /// constant and the entry name while it `read_dir`s `links/<group>`, so the
    /// well-meant "make the key match the path" edit is a change to a
    /// **persisted format that nothing in this crate reads back**: the producer,
    /// the type and some doc comments are the whole of `link_fingerprint`'s
    /// occurrences, and no comparison exists anywhere. It would break silently
    /// and forever. `state_store.rs`'s key-set test checks field *names* only
    /// and cannot see this.
    ///
    /// RED: prefix the key with `links/` in `observe_default_group_links`.
    #[tokio::test]
    async fn the_stamped_link_fingerprint_key_is_group_slash_entry() {
        let tree = Tree::new();
        tree.create_home_root();
        let entry = tree.home.entry(DEFAULT_GROUP, "cmake").expect("an admitted pair");
        std::fs::create_dir_all(entry.parent().expect("the entry has a parent")).unwrap();
        std::fs::create_dir_all(tree.tmp.path().join("some-package")).unwrap();
        crate::symlink::create(tree.tmp.path().join("some-package"), &entry).unwrap();

        let scope = tree.scope();
        let lock = lock_of(vec![locked_tool(
            "cmake",
            DEFAULT_GROUP,
            "ns/cmake",
            &[(PLATFORM_KEY, 'd')],
        )]);
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: true,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render succeeds");

        let stamp = tree.stamp().expect("the render writes a stamp");
        assert_eq!(
            stamp.link_fingerprint.keys().cloned().collect::<Vec<_>>(),
            vec![format!("{DEFAULT_GROUP}/cmake")],
            "the key carries the group and the entry, with no `links/` and no other tree component"
        );
    }

    /// C-053, case 42(b) — setting `toolchain-dir` yields a **fresh tree whose
    /// bodies are byte-identical**: the selector names the project, which
    /// `toolchain-dir` does not move.
    ///
    /// Stated as a negative control precisely because it looks like a second
    /// item-12 control and is not one.
    #[cfg(unix)]
    #[tokio::test]
    async fn relocating_the_home_leaves_every_body_byte_identical() {
        let tree = Tree::new();
        tree.create_home_root();
        let relocated = ToolchainHome::new(tree.tmp.path().join("relocated").join("toolchain"));
        seed_rendered_home(&relocated);

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        for home in [&tree.home, &relocated] {
            render_with(
                &tree.file_structure,
                RenderRequest {
                    home,
                    scope: &scope,
                    lock: &lock,
                    surface: &surface,
                    groups: &groups,
                    pinned: false,
                    platform: &platform,
                    dry_run: false,
                },
                false,
                None,
            )
            .await
            .expect("both homes render");
        }

        assert_eq!(
            std::fs::read_to_string(tree.home.shell_bin(DEFAULT_SHELL).join("cmake")).unwrap(),
            std::fs::read_to_string(relocated.shell_bin(DEFAULT_SHELL).join("cmake")).unwrap(),
            "C-053 — a `toolchain-dir` change is not a body-rewriting input"
        );
    }

    /// C-1/D-V19, case 43 — the golden's inputs are pinned, and the seeded
    /// rung-1 `ocx` is the one that must be baked.
    ///
    /// Without the seeding every golden in this module would silently bake
    /// `std::env::current_exe()` — the test binary's own path, which varies by
    /// machine, by cargo profile and by the target-directory hash. That is
    /// stable locally and unstable on CI, which is the one failure mode
    /// validation items 12 and 31 cannot afford.
    ///
    /// RED: delete the seeding from `Tree::new`.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_rendered_body_bakes_the_seeded_install_path_and_not_the_running_binary() {
        let tree = Tree::new();
        tree.create_home_root();

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render succeeds");

        let body = std::fs::read_to_string(tree.home.shell_bin(DEFAULT_SHELL).join("cmake")).unwrap();
        assert_eq!(
            body,
            expected_unix_trampoline_body(&tree.project_dir, &tree.ocx_binary),
            "the golden pins all of: the project root, the store root, the baked ocx, the \
             trampoline marker and the platform"
        );
        let running = std::env::current_exe().expect("the test binary has a path");
        assert!(
            !body.contains(&running.display().to_string()),
            "rung 1 answered, so rung 2's machine-dependent path is nowhere in the body"
        );
    }

    /// C-028, case 44 — a **dangling** rung-1 `current` (a half-uninstalled
    /// tree) does not stop the render: the ladder drops to rung 2 and the body
    /// differs from the rung-1 golden.
    ///
    /// RED: make rung 1 win on path shape alone — the body then bakes a path
    /// to nothing and `/bin/sh` reports a naked `ENOENT` naming nothing.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_dangling_install_tree_still_renders_and_bakes_a_different_binary() {
        let tree = Tree::new();
        tree.create_home_root();
        let current = tree.file_structure.symlinks.current(&oci::ocx_cli_identifier());
        std::fs::remove_dir_all(&current).expect("the seeded install tree is removable");
        std::os::unix::fs::symlink(tree.tmp.path().join("collected-by-clean"), &current)
            .expect("the dangling link is creatable");

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("C-028 — a dangling install tree is not a render failure");

        let body = std::fs::read_to_string(tree.home.shell_bin(DEFAULT_SHELL).join("cmake")).unwrap();
        assert_ne!(
            body,
            expected_unix_trampoline_body(&tree.project_dir, &tree.ocx_binary),
            "rung 1 declined, so the body must not bake the path that no longer resolves"
        );
    }

    /// R-W19(b), case 35 — the case-fold answer is a **probe of the real
    /// directory**, not a `cfg!(target_os)` branch.
    ///
    /// Asserted against a probe the test performs itself on the same
    /// directory, so the check is about that filesystem rather than about the
    /// host: a case-sensitive APFS volume and a case-insensitive loopback are
    /// both ordinary, and a `cfg!` answer would be an assertion no test on the
    /// other host could red.
    #[tokio::test]
    async fn the_case_fold_probe_answers_for_the_directory_it_was_given() {
        let tree = Tree::new();
        let probe_dir = tree.tmp.path().join("probe");
        std::fs::create_dir_all(&probe_dir).unwrap();
        std::fs::write(probe_dir.join("probe-case-a"), b"").unwrap();
        let truth = probe_dir.join("PROBE-CASE-A").exists();

        let answer = filesystem_is_case_insensitive(&probe_dir)
            .await
            .expect("a writable directory is probeable");

        assert_eq!(
            answer, truth,
            "R-W19(b) — the probe must answer what this filesystem actually does with two names \
             differing only in ASCII case"
        );
    }

    /// RUL-33/C-050, case 36 — a probe that **fails** is C-050's skip, not a
    /// boolean default.
    ///
    /// The probe writes into the very home the render is about to write, so a
    /// probe that cannot run is a home that cannot be written. Inventing
    /// either answer picks a silent corruption: `false` on a case-insensitive
    /// host writes two names that are one file and the stamp's entry set can
    /// never match C-061's `readdir`; `true` on a case-sensitive host silently
    /// drops a tool from `bin/`.
    ///
    /// RED: `unwrap_or(false)`.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_render_whose_case_fold_probe_cannot_run_skips_rather_than_guessing() {
        let tree = Tree::new();
        tree.create_home_root();
        deny_writes(tree.home.root());

        let surface = vec![
            node(pinned("ns/first", 'a'), Some(&["make"]), &[]),
            node(pinned("ns/second", 'b'), Some(&["Make"]), &[]),
        ];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let result = tree
            .manager()
            .render_toolchain(RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            })
            .await;
        allow_writes(tree.home.root());

        let report = result.expect("C-050 — an unwritable home is a skip, never an error");
        assert!(report.items.is_empty(), "nothing was rendered: {report:?}");
        assert!(!report.stamp_written, "…and nothing was stamped");
        assert!(tree.stamp().is_none());
    }

    /// C-051, case 37 — a `<group>/<entry>` link points at the package
    /// **root**, never at a file inside it.
    ///
    /// RED: point it at `content/bin/<name>` — POSIX still resolves, and the
    /// Windows leg reds because a junction's target must be a directory.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_group_entry_link_targets_the_package_root_and_not_a_file_inside_it() {
        let tree = Tree::new();
        tree.create_home_root();

        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]);
        let expected_target = expected_link_target(&tree.file_structure, &tool);
        let scope = tree.scope();
        let lock = lock_of(vec![tool]);
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render succeeds");

        let entry = tree.home.entry(DEFAULT_GROUP, "cmake").expect("an admitted pair");
        assert_eq!(
            std::fs::read_link(&entry).unwrap(),
            expected_target,
            "C-051 — the target is the lock-derived digest root itself"
        );
        assert!(
            !expected_target.ends_with("ocx"),
            "…and not a path down into `content/bin/<name>`: {expected_target:?}"
        );
    }

    // ── 5. Idempotence and the goldens (validation items 12 and 31) ─────────

    /// C-047, case 39 — `render(render(x)) == render(x)`: the second render
    /// reports every item `Unchanged` **and leaves every `bin/<name>` inode
    /// unchanged**.
    ///
    /// The inode is the discriminating half. An unconditional rewrite produces
    /// byte-identical content, so only the inode assertion reds — and every
    /// stat gate downstream (C-061's `(name, size, inode)` recomputation) is
    /// invalidated by exactly that rewrite.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_second_render_changes_nothing_and_replaces_no_inode() {
        let tree = Tree::new();
        tree.create_home_root();

        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]);
        let surface = vec![node(pinned("ns/cmake", 'd'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(vec![tool]);
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let first = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the first render succeeds");
        let inode_before = inode_of(&tree.home.shell_bin(DEFAULT_SHELL).join("cmake"));
        let subtree_before = snapshot_subtree(tree.home.root());

        let second = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the second render succeeds");

        assert!(
            first.items.iter().any(|item| item.outcome == RenderOutcome::Written),
            "precondition: the first render actually wrote something: {first:?}"
        );
        assert!(
            second.items.iter().all(|item| item.outcome == RenderOutcome::Unchanged),
            "C-047 — an already-correct entry is `Unchanged`, never rewritten: {second:?}"
        );
        assert_eq!(
            inode_of(&tree.home.shell_bin(DEFAULT_SHELL).join("cmake")),
            inode_before,
            "…and the file was left exactly as it is, which bytes alone cannot show"
        );
        assert_eq!(
            snapshot_subtree(tree.home.root()),
            subtree_before,
            "…across the whole tree, not just the one entry"
        );
    }

    /// C-004, case 40 — `.gitignore` idempotence asserted **through the
    /// render**: the second render leaves the file's inode unchanged, which is
    /// `ensure_gitignore` reporting "nothing written" from the outside.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_second_render_leaves_the_gitignore_untouched() {
        let tree = Tree::new();
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let manager = tree.manager();

        for _ in 0..1 {
            manager
                .render_toolchain(RenderRequest {
                    home: &tree.home,
                    scope: &scope,
                    lock: &lock,
                    surface: &[],
                    groups: &groups,
                    pinned: false,
                    platform: &platform,
                    dry_run: false,
                })
                .await
                .expect("the first render succeeds");
        }
        let before = inode_of(&tree.home.gitignore());

        manager
            .render_toolchain(RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            })
            .await
            .expect("the second render succeeds");

        assert_eq!(
            inode_of(&tree.home.gitignore()),
            before,
            "C-004 — ensure-present, not rewrite-every-time"
        );
    }

    // ── 6. The skip path (C-050) and the stamp (RUL-22 / RUL-24 / RUL-27) ───

    /// RUL-24, case 45 — a read-only checkout with **no** home root yet warns,
    /// renders nothing, stamps nothing, and is `Ok`.
    ///
    /// RED: return `Internal` — `ocx pull` then exits non-zero in an ordinary
    /// CI container, which is precisely the state C-050's trigger list opens
    /// with.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_home_root_that_cannot_be_created_is_a_skip_and_not_an_error() {
        let tree = Tree::new();
        let checkout = tree.home.root().parent().expect("`.ocx` is the parent");
        std::fs::create_dir_all(checkout).unwrap();
        deny_writes(checkout);

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let result = tree
            .manager()
            .render_toolchain(RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            })
            .await;
        allow_writes(checkout);

        let report = result.expect("RUL-24 — a creation failure after a passing resolve is I/O, not policy");
        assert!(report.items.is_empty(), "{report:?}");
        assert!(!report.stamp_written);
        assert!(!tree.home.root().exists(), "…and nothing was created");
    }

    /// C-050/RUL-39, case 46 — the state store is denied, so the **stamp write
    /// itself** fails: every item `Skipped`, `stamp_written == false`, still
    /// `Ok`.
    ///
    /// Named for what it drives. `stamp_written == false` means exactly one
    /// thing — the stamp write failed — and denying the state store is the only
    /// input that produces it. `bin/` is denied too so there is something for
    /// every item to be skipped *about*; the sibling below covers the commoner
    /// shape, a read-only `bin/` beside a **writable** state store, where the
    /// stamp is written and records nothing.
    ///
    /// RED: propagate a stamp-write failure.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_stamp_write_reports_stamp_written_false_and_still_returns_ok() {
        let tree = Tree::new();
        tree.create_home_root();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        std::fs::create_dir_all(tree.file_structure.state.root()).unwrap();
        deny_writes(&tree.home.shell_bin(DEFAULT_SHELL));
        deny_writes(tree.file_structure.state.root());

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake", "ctest"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let result = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await;
        allow_writes(&tree.home.shell_bin(DEFAULT_SHELL));
        allow_writes(tree.file_structure.state.root());

        let report = result.expect("C-050 — a read-only checkout composes digest paths and exits 0");
        assert!(
            !report.items.is_empty() && report.items.iter().all(|item| is_skipped(&item.outcome)),
            "every item is skipped with its own reason: {report:?}"
        );
        assert!(!report.stamp_written, "…and the failed stamp write is a skip too");
    }

    /// C-050/RUL-39 — the commoner half of case 46: a read-only `bin/` beside a
    /// **writable** state store. Every item is `Skipped` and the stamp **is**
    /// written, recording only what landed — which here is nothing.
    ///
    /// That is the correct outcome, not a defect: C-061 then compares an empty
    /// stamp against a `bin/` still holding the stale trampolines, mismatches,
    /// and withholds the PATH entry. `stamp_written == true` beside an empty
    /// fingerprint is a different state from `stamp_written == false`, and both
    /// are reachable — which is why they need two tests.
    ///
    /// RED: gate the stamp write on "nothing was skipped", or build the
    /// fingerprint from the intended name set — the stamp would then claim a
    /// `bin/` that was never written, and C-061 would match on a stale tree.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_read_only_bin_with_a_writable_state_store_still_stamps_what_landed() {
        let tree = Tree::new();
        tree.create_home_root();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        // What a previous render left, and what this one can neither rewrite
        // nor prune.
        std::fs::write(tree.home.shell_bin(DEFAULT_SHELL).join("cmake"), b"#!/bin/sh\nstale\n").unwrap();
        deny_writes(&tree.home.shell_bin(DEFAULT_SHELL));

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let result = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await;
        allow_writes(&tree.home.shell_bin(DEFAULT_SHELL));

        let report = result.expect("C-050 — an unwritable `bin/` is a skip, never an error");
        assert!(
            !report.items.is_empty() && report.items.iter().all(|item| is_skipped(&item.outcome)),
            "every entry is skipped with its own reason: {report:?}"
        );
        assert!(
            report.stamp_written,
            "RUL-39 — the state store was writable, so the stamp is written"
        );
        let stamp = tree.stamp().expect("the stamp exists");
        assert!(
            stamp.names.is_empty(),
            "RUL-22 — …recording only what landed, which here is nothing: {stamp:?}"
        );
        assert_eq!(
            std::fs::read(tree.home.shell_bin(DEFAULT_SHELL).join("cmake")).unwrap(),
            b"#!/bin/sh\nstale\n",
            "…while `bin/` still holds the stale trampoline C-061's gate will mismatch on"
        );
    }

    /// RUL-22/27, case 47 and case 48 — one entry unwritable, the others
    /// succeed: that entry is `Skipped` naming its path, the rest are
    /// `Written`, a stamp **is** written, and `bin_fingerprint` carries only
    /// what actually landed.
    ///
    /// RED: build the stamp from the intended name set. C-061's `readdir` gate
    /// then mismatches on every prompt forever — the exact failure C-003 cites
    /// when it refuses to widen `link_fingerprint`. Second RED:
    /// `remove_dir_all` the blocking directory, turning a hostile clone's
    /// committed directory into a delete primitive.
    #[tokio::test]
    async fn a_partially_skipped_render_stamps_only_what_landed() {
        let tree = Tree::new();
        tree.create_home_root();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        // A directory where a trampoline must go: the write cannot succeed and
        // RUL-32 forbids removing it recursively.
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL).join(&trampoline_files("blocked")[0])).unwrap();
        std::fs::write(
            tree.home
                .shell_bin(DEFAULT_SHELL)
                .join(&trampoline_files("blocked")[0])
                .join("payload"),
            b"keep me\n",
        )
        .unwrap();

        let surface = vec![node(pinned("ns/tools", 'a'), Some(&["blocked", "fine"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("C-050 — one blocked entry does not fail the render");

        let blocked = outcome_of(&report, &trampoline(&trampoline_files("blocked")[0]));
        match blocked {
            RenderOutcome::Skipped { path, reason } => {
                assert!(
                    path.ends_with(&trampoline_files("blocked")[0]),
                    "RUL-32 — the skip names the path it could not write: {path:?}"
                );
                assert!(!reason.is_empty(), "…and carries a reason for the warn line");
            }
            other => panic!("the blocked entry must be skipped, not {other:?}"),
        }
        assert_eq!(
            outcome_of(&report, &trampoline(&trampoline_files("fine")[0])),
            &RenderOutcome::Written,
            "…and the render continued to the entries after it"
        );
        assert!(report.stamp_written, "RUL-22 — a partially-skipped render still stamps");
        let stamp = tree.stamp().expect("the stamp exists");
        assert_eq!(
            stamp.names.iter().cloned().collect::<Vec<_>>(),
            trampoline_files("fine"),
            "RUL-22 — …and records only what landed, never the intended set: {stamp:?}"
        );
        assert!(
            tree.home
                .shell_bin(DEFAULT_SHELL)
                .join(&trampoline_files("blocked")[0])
                .join("payload")
                .exists(),
            "RUL-32 — the blocking directory was never removed recursively"
        );
    }

    /// C-044, case 49 — a `bin/<name>` committed as a **symlink** to a file
    /// outside the tree is *replaced* by the rename; the link's target is
    /// byte-identical afterwards and `bin/<name>` is a regular file.
    ///
    /// RED: implement the write as `fs::write` + `set_permissions` — the write
    /// then follows the link and overwrites `~/.bashrc`.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_committed_symlink_in_bin_is_replaced_and_its_target_is_not_written_through() {
        let tree = Tree::new();
        tree.create_home_root();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        let victim = tree.tmp.path().join("bashrc");
        std::fs::write(&victim, b"# the user's shell profile\n").unwrap();
        std::os::unix::fs::symlink(&victim, tree.home.shell_bin(DEFAULT_SHELL).join("cmake")).unwrap();

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render succeeds");

        assert_eq!(
            std::fs::read(&victim).unwrap(),
            b"# the user's shell profile\n",
            "C-044 — nothing is ever written *through* a committed link"
        );
        let metadata = std::fs::symlink_metadata(tree.home.shell_bin(DEFAULT_SHELL).join("cmake")).unwrap();
        assert!(
            metadata.is_file() && !metadata.is_symlink(),
            "…and the entry is a regular file afterwards"
        );
    }

    /// S-3, case 50 — the idempotence compare never opens a planted **FIFO**.
    ///
    /// This is the one hazard in this module with no recovery path: `open` on
    /// a FIFO with no writer blocks indefinitely, so C-050 cannot catch it —
    /// C-050 turns a *returned* failure into a skip, and this call would never
    /// return. `Ok(None)` is "nothing comparable is there, take the write
    /// branch".
    ///
    /// RED: swap in an unbounded `std::fs::read` and watch the test hang
    /// rather than fail.
    #[cfg(unix)]
    #[test]
    fn reading_an_existing_entry_never_opens_a_planted_fifo() {
        let tree = Tree::new();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        let fifo = tree.home.shell_bin(DEFAULT_SHELL).join("cmake");
        mkfifo(&fifo);

        assert_eq!(
            read_existing_trampoline(&fifo, 64).expect("a FIFO is not an error, it is not comparable"),
            None,
            "S-3 — the type gate is `symlink_metadata`, so nothing is opened"
        );
    }

    /// C-047/S-3 — `read_existing_trampoline`'s other two refusals and its one
    /// acceptance, so the FIFO case above is not the only reachable branch.
    ///
    /// A symlink whose target happens to match would otherwise short-circuit
    /// to `Unchanged` and survive every render, leaving `bin/<name>`
    /// permanently a link rather than a trampoline; an over-cap regular file
    /// differs from a body of exactly `cap` bytes by definition (CWE-400).
    #[cfg(unix)]
    #[test]
    fn reading_an_existing_entry_refuses_a_symlink_and_an_over_cap_file() {
        let tree = Tree::new();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        let target = tree.tmp.path().join("elsewhere");
        std::fs::write(&target, b"body").unwrap();
        let linked = tree.home.shell_bin(DEFAULT_SHELL).join("linked");
        std::os::unix::fs::symlink(&target, &linked).unwrap();
        let oversized = tree.home.shell_bin(DEFAULT_SHELL).join("oversized");
        std::fs::write(&oversized, vec![b'x'; 4096]).unwrap();
        let ordinary = tree.home.shell_bin(DEFAULT_SHELL).join("ordinary");
        std::fs::write(&ordinary, b"body").unwrap();

        assert_eq!(
            read_existing_trampoline(&linked, 4).expect("a symlink is not an error"),
            None,
            "a link is never read as if it were a trampoline"
        );
        assert_eq!(
            read_existing_trampoline(&oversized, 4).expect("an over-cap file is not an error"),
            None,
            "over-cap is the write branch, never an error"
        );
        assert_eq!(
            read_existing_trampoline(&ordinary, 4).expect("an ordinary file reads"),
            Some(b"body".to_vec()),
            "…and the acceptance branch is reachable, or the three refusals above prove nothing"
        );
        assert_eq!(
            read_existing_trampoline(&tree.home.shell_bin(DEFAULT_SHELL).join("absent"), 4)
                .expect("absence is not an error"),
            None
        );
    }

    /// C-004/C-050, case 53 — an unwritable `.gitignore` does not stop the
    /// render: the trampolines still land.
    ///
    /// RED: `?` on `ensure_gitignore` — one unwritable file in a tree the
    /// repository controls then blocks the whole feature.
    #[tokio::test]
    async fn an_unwritable_gitignore_does_not_stop_the_trampolines_from_landing() {
        let tree = Tree::new();
        std::fs::create_dir_all(tree.home.root()).unwrap();
        // A directory where the ignore file must go: `ensure_gitignore`'s
        // rename cannot land, and nothing else in the home is affected.
        std::fs::create_dir_all(tree.home.gitignore()).unwrap();

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        tree.manager()
            .render_toolchain(RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            })
            .await
            .expect("C-050 — a failed `.gitignore` write is a skip, not an error");

        assert_eq!(
            tree.bin_entries(),
            expected_bin_entries(&["cmake"]),
            "the trampolines are the render's product; the ignore file is a convenience"
        );
    }

    /// R-W19(a)/C-4, case 54 — `ensure_home_root` runs **before**
    /// `ensure_gitignore`, and every directory it creates is owner-only-write.
    ///
    /// C-019 refuses a group-or-world-writable `toolchain-dir`, but it can
    /// only check a directory that exists — for an absent root it examines the
    /// nearest existing *ancestor*, so every directory this call creates is
    /// one C-019 never examined. `ensure_gitignore` does its own
    /// `create_dir_all` with the **ambient umask**, so reaching it with the
    /// root still absent would create that root with whatever the umask allows.
    ///
    /// RED: reorder the two calls — no other assertion in this suite notices.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_first_ever_render_creates_the_home_root_owner_only_writable() {
        let tree = Tree::new();
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();

        assert!(!tree.home.root().exists(), "precondition: this is a first-ever render");
        tree.manager()
            .render_toolchain(RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            })
            .await
            .expect("the first render succeeds");

        assert_eq!(
            mode_of(tree.home.root()) & 0o077,
            0,
            "R-W19(a) — the home root is created owner-only-write, at create time, not by a \
             post-hoc chmod that leaves a group-writable window"
        );
        assert!(
            tree.home.gitignore().is_file(),
            "C-004 — …and the ignore file was still written, after the root existed"
        );
    }

    /// R-W19(a) — `ensure_home_root` reports whether **this** call created the
    /// root, and creates every missing ancestor owner-only-write.
    #[cfg(unix)]
    #[test]
    fn ensure_home_root_creates_every_missing_ancestor_and_reports_the_first_creation() {
        let tree = Tree::new();
        let deep = ToolchainHome::new(tree.tmp.path().join("relocated").join("project-key").join("toolchain"));

        assert!(
            ensure_home_root(&deep).expect("the root is creatable"),
            "the first call created it, and says so"
        );
        assert!(
            !ensure_home_root(&deep).expect("a second call is a no-op"),
            "…and the second call reports that it created nothing"
        );
        for ancestor in [
            tree.tmp.path().join("relocated"),
            tree.tmp.path().join("relocated").join("project-key"),
            deep.root().to_path_buf(),
        ] {
            assert_eq!(
                mode_of(&ancestor) & 0o077,
                0,
                "R-W19(a) — every level this call created, not only the leaf: {ancestor:?}"
            );
        }
    }

    /// C-052 and validation item 23 — rendering a **project** tree registers
    /// the project in the `projects/` GC ledger; rendering the **global** tree
    /// registers nothing.
    ///
    /// RED: register unconditionally — the global home's project dir is
    /// `$OCX_HOME`, and a self-link is barred by the no-self-link invariant.
    #[tokio::test]
    async fn a_project_render_registers_in_the_gc_ledger_and_a_global_render_does_not() {
        let tree = Tree::new();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let manager = tree.manager();

        let project_scope = tree.scope();
        manager
            .render_toolchain(RenderRequest {
                home: &tree.home,
                scope: &project_scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            })
            .await
            .expect("the project render succeeds");
        let after_project = read_dir_names(&tree.file_structure.root().join("projects"));
        assert_eq!(
            after_project.len(),
            1,
            "C-052 — a project render takes a GC-root ledger entry: {after_project:?}"
        );

        let global_home = tree.file_structure.toolchain.home().clone();
        manager
            .render_toolchain(RenderRequest {
                home: &global_home,
                scope: &RenderStampScope::Global,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            })
            .await
            .expect("the global render succeeds");

        assert_eq!(
            read_dir_names(&tree.file_structure.root().join("projects")),
            after_project,
            "C-052 — the global tier registers nothing; `register` is a no-op for $OCX_HOME"
        );
    }

    // ── 7. `heal_links` (C-051, C-070, RUL-29, RUL-31, RUL-36) ─────────────

    /// C-051, case 55 — an empty `groups` is a vacuous no-op:
    /// [`HealOutcome::Healed(0)`](HealOutcome::Healed), and no
    /// refusal.
    ///
    /// RED: add a refusal, putting an error on the one path C-051 says never
    /// errors — and one that would not catch the narrowing that actually bites
    /// (passing the default group when the invocation selected `ci`), which is
    /// a non-empty wrong answer.
    #[tokio::test]
    async fn healing_an_empty_group_set_is_a_vacuous_no_op() {
        let tree = Tree::new();
        let lock = lock_of(vec![locked_tool(
            "cmake",
            DEFAULT_GROUP,
            "ns/cmake",
            &[(PLATFORM_KEY, 'd')],
        )]);

        assert_eq!(
            heal_links(&tree.file_structure, &tree.home, &tree.scope(), &lock, &[], &platform())
                .await
                .expect("C-051 — an empty group set never errors"),
            HealOutcome::Healed(0)
        );
        assert!(
            !tree.home.root().exists(),
            "…and nothing was created for a call that had nothing to do"
        );
    }

    /// C-051, case 56 — a selected group the lock carries no entries for is
    /// [`HealOutcome::Healed(0)`](HealOutcome::Healed), and heal neither creates
    /// nor removes its directory.
    #[tokio::test]
    async fn healing_a_group_with_no_locked_entries_creates_and_removes_nothing() {
        let tree = Tree::new();
        std::fs::create_dir_all(tree.home.root()).unwrap();
        let lock = lock_of(vec![locked_tool(
            "cmake",
            DEFAULT_GROUP,
            "ns/cmake",
            &[(PLATFORM_KEY, 'd')],
        )]);

        assert_eq!(
            heal_links(
                &tree.file_structure,
                &tree.home,
                &tree.scope(),
                &lock,
                &groups_of(&["ci"]),
                &platform()
            )
            .await
            .expect("an empty group is not an error"),
            HealOutcome::Healed(0)
        );
        assert!(
            !tree.home.links_group("ci").expect("an admitted group name").exists(),
            "heal creates no directory for a group with nothing in it"
        );
    }

    /// C-004, V-10 — healing a **fresh** home writes the ignore file.
    ///
    /// `ocx env` and `ocx exec` reach `heal_links` and never the render, so on
    /// a fresh clone with a committed `ocx.lock` and no `ocx pull` this is the
    /// only thing that creates `<project>/.ocx/toolchain` — after which RUL-29
    /// fills it with `<group>/<entry>` symlinks. Without the ignore file
    /// `git add -A` stages them, which is precisely the state C-004 exists to
    /// prevent, and `ensure_gitignore` had exactly one caller: the render.
    ///
    /// RED: drop the `ensure_gitignore` call from `heal_links` — the links
    /// below still land, so only this assertion discriminates.
    #[tokio::test]
    async fn healing_a_fresh_home_writes_the_ignore_file() {
        let tree = Tree::new();
        assert!(
            !tree.home.root().exists(),
            "the fresh-clone precondition: nothing has rendered this home yet"
        );
        let lock = lock_of(vec![locked_tool(
            "cmake",
            DEFAULT_GROUP,
            "ns/cmake",
            &[(PLATFORM_KEY, 'd')],
        )]);

        assert_eq!(
            heal_links(
                &tree.file_structure,
                &tree.home,
                &tree.scope(),
                &lock,
                &groups_of(&[DEFAULT_GROUP]),
                &platform(),
            )
            .await
            .expect("healing a fresh home is not an error"),
            HealOutcome::Healed(1),
            "the precondition for the assertion below: heal entered the tree and created the link"
        );
        assert!(
            tree.home.gitignore().is_file(),
            "C-004 — every path that creates the home writes the ignore file, not just the render"
        );
    }

    /// RUL-29, case 57 — an **absent** link is created, and counted.
    ///
    /// Not an edge: it is the commonest post-`git pull` state, since a lock
    /// that gained an entry has no link for it until something writes one, and
    /// on the composing path (C-070) nothing else will before the emit.
    ///
    /// RED: invert to skip — C-070's one purpose, that the composing emitter
    /// can trust the link it is about to emit, is then unsatisfied exactly
    /// when it matters.
    #[tokio::test]
    async fn healing_creates_an_absent_link_and_counts_it() {
        let tree = Tree::new();
        std::fs::create_dir_all(tree.home.root()).unwrap();
        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]);
        let expected_target = expected_link_target(&tree.file_structure, &tool);
        let lock = lock_of(vec![tool]);

        let repaired = heal_links(
            &tree.file_structure,
            &tree.home,
            &tree.scope(),
            &lock,
            &groups_of(&[DEFAULT_GROUP]),
            &platform(),
        )
        .await
        .expect("healing succeeds");

        assert_eq!(
            repaired,
            HealOutcome::Healed(1),
            "RUL-29 — a created link counts like a repointed one"
        );
        let entry = tree.home.entry(DEFAULT_GROUP, "cmake").expect("an admitted pair");
        assert_eq!(std::fs::read_link(&entry).unwrap(), expected_target);
    }

    /// C-051, case 58 — a `<group>/<entry>` that is a **regular file** rather
    /// than a link is left alone, counted as un-repaired, and is never an
    /// error and never deleted.
    ///
    /// RED: remove-and-recreate — heal then gains a delete path C-051 does not
    /// authorise, inside a tree the repository controls.
    #[tokio::test]
    async fn healing_leaves_a_regular_file_where_a_link_belongs_untouched() {
        let tree = Tree::new();
        let entry = tree.home.entry(DEFAULT_GROUP, "cmake").expect("an admitted pair");
        std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
        std::fs::write(&entry, b"not a link\n").unwrap();
        let lock = lock_of(vec![locked_tool(
            "cmake",
            DEFAULT_GROUP,
            "ns/cmake",
            &[(PLATFORM_KEY, 'd')],
        )]);

        let repaired = heal_links(
            &tree.file_structure,
            &tree.home,
            &tree.scope(),
            &lock,
            &groups_of(&[DEFAULT_GROUP]),
            &platform(),
        )
        .await
        .expect("C-051 — a shape heal cannot repair is never an error");

        assert_eq!(
            repaired,
            HealOutcome::Healed(0),
            "the entry is uncounted because nothing was repaired"
        );
        assert_eq!(
            std::fs::read(&entry).unwrap(),
            b"not a link\n",
            "…and the file is byte-identical, not deleted"
        );
    }

    /// C-051, case 59 — a link pointing **outside** the store is repointed at
    /// the lock-derived digest root, and heal never reads *through* it.
    ///
    /// RED: compare canonicalised targets — a link into a hostile tree that
    /// happens to canonicalise onto the right place then reads as correct.
    #[cfg(unix)]
    #[tokio::test]
    async fn healing_repoints_a_link_that_targets_something_outside_the_store() {
        let tree = Tree::new();
        let hostile = tree.tmp.path().join("hostile-package");
        std::fs::create_dir_all(&hostile).unwrap();
        std::fs::write(hostile.join("payload"), b"hostile\n").unwrap();
        let entry = tree.home.entry(DEFAULT_GROUP, "cmake").expect("an admitted pair");
        std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&hostile, &entry).unwrap();

        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]);
        let expected_target = expected_link_target(&tree.file_structure, &tool);
        let lock = lock_of(vec![tool]);

        let repaired = heal_links(
            &tree.file_structure,
            &tree.home,
            &tree.scope(),
            &lock,
            &groups_of(&[DEFAULT_GROUP]),
            &platform(),
        )
        .await
        .expect("healing succeeds");

        assert_eq!(repaired, HealOutcome::Healed(1));
        assert_eq!(std::fs::read_link(&entry).unwrap(), expected_target);
        assert!(
            hostile.join("payload").exists(),
            "…and the tree it used to point at is untouched: heal repoints, it does not clean up"
        );
    }

    /// C-051, case 60 — a link whose target digest root does not exist on disk
    /// is still **correct**: the lock names that digest, so heal repoints
    /// nothing.
    ///
    /// RED: add a `try_exists` gate on the target — heal then repoints (or
    /// worse, deletes) links for every package that is not currently
    /// materialised, which is the ordinary state of a lazily-loaded tool.
    #[tokio::test]
    async fn healing_leaves_a_correct_link_alone_even_when_its_target_does_not_exist() {
        let tree = Tree::new();
        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]);
        let expected_target = expected_link_target(&tree.file_structure, &tool);
        let entry = tree.home.entry(DEFAULT_GROUP, "cmake").expect("an admitted pair");
        std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
        crate::symlink::create(&expected_target, &entry).expect("a dangling link is creatable");
        assert!(
            !expected_target.exists(),
            "precondition: the package is not materialised, which is the lazy tool's ordinary state"
        );
        let lock = lock_of(vec![tool]);

        assert_eq!(
            heal_links(
                &tree.file_structure,
                &tree.home,
                &tree.scope(),
                &lock,
                &groups_of(&[DEFAULT_GROUP]),
                &platform()
            )
            .await
            .expect("healing succeeds"),
            HealOutcome::Healed(0),
            "C-051 — the comparison is against the lock, never against the filesystem"
        );
    }

    /// RUL-31, case 61 — render and heal select the platform through the same
    /// `select_best` helper, never an exact key lookup: (a) an exact key
    /// resolves, (b) a **compatible but non-identical** key resolves the same
    /// way the render did, and (c) no compatible key skips the entry silently.
    ///
    /// RED: exact-key lookup. Case (b) then makes the render write one target
    /// and the heal repoint it to another, on every invocation, forever — and
    /// the stamp's `link_fingerprint` never matches for two consecutive runs.
    #[tokio::test]
    async fn render_and_heal_agree_on_a_compatible_non_identical_platform_key() {
        let tree = Tree::new();
        tree.create_home_root();

        // The lock carries the bare `linux/amd64`; the request carries a
        // feature-bearing host that only `is_compatible` relates to it.
        let host: oci::Platform = "linux/amd64+libc.glibc"
            .parse()
            .expect("the fixture host platform is canonical");
        let exact = locked_tool("exact", DEFAULT_GROUP, "ns/exact", &[("linux/amd64+libc.glibc", 'e')]);
        let compatible = locked_tool("compatible", DEFAULT_GROUP, "ns/compatible", &[("linux/amd64", 'c')]);
        let incompatible = locked_tool(
            "incompatible",
            DEFAULT_GROUP,
            "ns/incompatible",
            &[("windows/amd64", 'w')],
        );
        let expected_exact = {
            let identifier = crate::project::compose::host_leaf_identifier(&exact, &host).expect("exact resolves");
            tree.file_structure
                .packages
                .path(&oci::PinnedIdentifier::try_from(identifier).expect("digest-bearing"))
        };
        let expected_compatible = {
            let identifier =
                crate::project::compose::host_leaf_identifier(&compatible, &host).expect("compatible resolves");
            tree.file_structure
                .packages
                .path(&oci::PinnedIdentifier::try_from(identifier).expect("digest-bearing"))
        };
        let lock = lock_of(vec![exact, compatible, incompatible]);
        let scope = tree.scope();
        let groups = groups_of(&[DEFAULT_GROUP]);

        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &host,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render succeeds");

        let entry_of = |name: &str| tree.home.entry(DEFAULT_GROUP, name).expect("an admitted pair");
        assert_eq!(std::fs::read_link(entry_of("exact")).unwrap(), expected_exact);
        assert_eq!(
            std::fs::read_link(entry_of("compatible")).unwrap(),
            expected_compatible,
            "RUL-31 — a compatible non-identical key resolves through `select_best`"
        );
        assert!(
            std::fs::symlink_metadata(entry_of("incompatible")).is_err(),
            "…and no compatible key skips the entry silently"
        );
        assert!(
            report
                .items
                .iter()
                .all(|item| item.artifact != link(DEFAULT_GROUP, "incompatible")),
            "…with no `Skipped` item and no error: {report:?}"
        );

        assert_eq!(
            heal_links(&tree.file_structure, &tree.home, &tree.scope(), &lock, &groups, &host)
                .await
                .expect("healing succeeds"),
            HealOutcome::Healed(0),
            "RUL-31 — the heal agrees with the render, so nothing oscillates"
        );
    }

    /// C-051, case 62 — **purity**, asserted behaviourally: an offline
    /// `FileStructure` with no client, no index and nothing installed still
    /// heals.
    ///
    /// A source-text grep for "no compose" would be a denylist that cannot
    /// enumerate every way to reach the network. Building the call out of a
    /// store that has no client at all is what makes the property structural.
    ///
    /// RED: resolve the entry through an install path "to be safe".
    #[tokio::test]
    async fn healing_is_pure_lock_arithmetic_over_a_store_with_no_client() {
        let tree = Tree::new();
        std::fs::create_dir_all(tree.home.root()).unwrap();
        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]);
        let expected_target = expected_link_target(&tree.file_structure, &tool);
        let lock = lock_of(vec![tool]);
        assert!(
            !tree.file_structure.packages.root().exists(),
            "precondition: nothing is installed, so a heal that resolved through an install path \
             could not answer at all"
        );

        assert_eq!(
            heal_links(
                &tree.file_structure,
                &tree.home,
                &tree.scope(),
                &lock,
                &groups_of(&[DEFAULT_GROUP]),
                &platform()
            )
            .await
            .expect("C-051 — no compose, no metadata read, no network"),
            HealOutcome::Healed(1)
        );
        let entry = tree.home.entry(DEFAULT_GROUP, "cmake").expect("an admitted pair");
        assert_eq!(std::fs::read_link(&entry).unwrap(), expected_target);
    }

    /// RUL-36 — a link heal cannot repair leaves the entry unrepaired,
    /// uncounted and logged; the call still returns
    /// [`HealOutcome::Healed`] — a per-entry degrade, never a whole-tree
    /// refusal.
    ///
    /// C-070 puts this function on `ocx env`'s and `ocx exec`'s critical path,
    /// so an ordinary contention turned into a failed command would break the
    /// path whose whole design premise is that it degrades instead.
    ///
    /// RED: propagate the failure.
    #[cfg(unix)]
    #[tokio::test]
    async fn healing_never_errors_when_it_cannot_write() {
        let tree = Tree::new();
        let group_directory = tree
            .home
            .links_group(DEFAULT_GROUP)
            .expect("the default group is admitted");
        std::fs::create_dir_all(&group_directory).unwrap();
        deny_writes(&group_directory);

        let lock = lock_of(vec![
            locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]),
            locked_tool("ninja", DEFAULT_GROUP, "ns/ninja", &[(PLATFORM_KEY, 'n')]),
        ]);
        let result = heal_links(
            &tree.file_structure,
            &tree.home,
            &tree.scope(),
            &lock,
            &groups_of(&[DEFAULT_GROUP]),
            &platform(),
        )
        .await;
        allow_writes(&group_directory);

        assert_eq!(
            result.expect("RUL-36 — a repair that cannot land is never an error"),
            HealOutcome::Healed(0),
            "…and the unrepaired entries are uncounted, because the count is repairs that landed"
        );
    }

    /// RUL-36/RUL-38, case 64's contention clause — an entry whose per-repoint
    /// lock is held elsewhere is left unrepaired and **uncounted**, its
    /// uncontended sibling is repaired, and the call returns
    /// [`HealOutcome::Healed`].
    ///
    /// C-070 puts this function on `ocx env`'s and `ocx exec`'s critical path,
    /// so ordinary contention must degrade per entry rather than fail the
    /// command — the composing side turns an unrepaired entry into a digest
    /// path (C-067).
    ///
    /// Driven through [`heal_lock_parameters`] so the test contends on *the
    /// same* lock. It costs the lock's own timeout in wall-clock, for the same
    /// reason case 52 does.
    ///
    /// RED: propagate the failure — one busy link then fails the whole compose.
    #[tokio::test]
    async fn healing_leaves_a_contended_entry_unrepaired_and_uncounted() {
        let tree = Tree::new();
        // The per-entry lock keys on the group directory's file identity.
        std::fs::create_dir_all(tree.home.links_group(DEFAULT_GROUP).expect("admitted")).unwrap();

        let contended = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]);
        let free = locked_tool("ninja", DEFAULT_GROUP, "ns/ninja", &[(PLATFORM_KEY, 'n')]);
        let free_target = expected_link_target(&tree.file_structure, &free);
        let lock = lock_of(vec![contended, free]);

        let group_directory = tree
            .home
            .links_group(DEFAULT_GROUP)
            .expect("the default group is admitted");
        let _held = heal_lock_parameters(&tree.file_structure, group_directory, "cmake")
            .acquire()
            .await
            .expect("the test can take the same per-entry lock");

        let repaired = heal_links(
            &tree.file_structure,
            &tree.home,
            &tree.scope(),
            &lock,
            &groups_of(&[DEFAULT_GROUP]),
            &platform(),
        )
        .await
        .expect("RUL-36 — contention is never an error");

        assert_eq!(
            repaired,
            HealOutcome::Healed(1),
            "the contended entry is uncounted, because the count is repairs that landed"
        );
        assert!(
            std::fs::symlink_metadata(tree.home.entry(DEFAULT_GROUP, "cmake").expect("an admitted pair")).is_err(),
            "…and nothing was written for it"
        );
        assert_eq!(
            std::fs::read_link(tree.home.entry(DEFAULT_GROUP, "ninja").expect("an admitted pair")).unwrap(),
            free_target,
            "…while its uncontended sibling was repaired in the same call"
        );
    }

    // ── 8. `--dry-run` (C-049, S-007, RUL-28) ───────────────────────────────

    /// C-049, case 66 — "wrote nothing" asserted with the right instrument: a
    /// `(path, file type, bytes, mtime_nsec, inode)` snapshot of the whole
    /// home subtree, before and after, compared as sets.
    ///
    /// Empty stdout, a zero file count and "the file I checked is unchanged"
    /// are all silent-negative shapes here. A dry run that called
    /// `ensure_gitignore` would satisfy a bytes-only or count-only assertion
    /// and red only this one.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_dry_run_leaves_every_object_in_the_home_bit_for_bit_identical() {
        let tree = Tree::new();
        tree.create_home_root();

        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]);
        let surface = vec![node(pinned("ns/cmake", 'd'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(vec![tool]);
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the seeding render succeeds");
        // Something for the dry run to *want* to change, so the snapshot is
        // not comparing two empty trees.
        std::fs::write(tree.home.shell_bin(DEFAULT_SHELL).join("stale"), b"#!/bin/sh\n").unwrap();
        let before = snapshot_subtree(tree.home.root());

        let report = tree
            .manager()
            .render_toolchain(RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: true,
            })
            .await
            .expect("the dry run succeeds");

        assert_eq!(
            snapshot_subtree(tree.home.root()),
            before,
            "C-049 — nothing under the home changed identity, content or timestamp"
        );
        assert_eq!(
            outcome_of(&report, &trampoline("stale")),
            &RenderOutcome::Pruned,
            "…while the report still names the delta it *would* have applied"
        );
    }

    /// S-007/C-049, case 67 — a dry run over a **poisoned** link performs no
    /// heal: `readlink` is byte-identical afterwards, and the report names the
    /// delta.
    ///
    /// RED: call `heal_links` from the dry-run arm — the run then reports a
    /// delta it has itself already closed.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_dry_run_performs_no_heal_over_a_poisoned_link() {
        let tree = Tree::new();
        tree.create_home_root();
        let poison = tree.tmp.path().join("poison");
        std::fs::create_dir_all(&poison).unwrap();
        let entry = tree.home.entry(DEFAULT_GROUP, "cmake").expect("an admitted pair");
        std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&poison, &entry).unwrap();

        let scope = tree.scope();
        let lock = lock_of(vec![locked_tool(
            "cmake",
            DEFAULT_GROUP,
            "ns/cmake",
            &[(PLATFORM_KEY, 'd')],
        )]);
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let report = tree
            .manager()
            .render_toolchain(RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: true,
            })
            .await
            .expect("the dry run succeeds");

        assert_eq!(
            std::fs::read_link(&entry).unwrap(),
            poison,
            "S-007 — the poisoned link is still present afterwards"
        );
        assert_eq!(
            outcome_of(&report, &link(DEFAULT_GROUP, "cmake")),
            &RenderOutcome::Written,
            "…and the delta is reported as the write it would have performed"
        );
    }

    /// RUL-28/S-5, case 68 — a dry run on a never-rendered home creates
    /// **nothing**: no home root, no `state/projects/<key>/`, and no
    /// `projects/` GC ledger entry.
    ///
    /// The ledger is the case worth naming: registration is best-effort and
    /// costs nothing, so "register anyway" is tempting. It would make
    /// `ocx pull --dry-run` a GC-visible mutation of a store the user asked it
    /// not to touch.
    ///
    /// RED: hoist `ensure_home_root` above the `dry_run` branch.
    #[tokio::test]
    async fn a_dry_run_on_a_never_rendered_home_creates_nothing_at_all() {
        let tree = Tree::new();
        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();

        let report = tree
            .manager()
            .render_toolchain(RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: true,
            })
            .await
            .expect("the dry run succeeds");

        assert!(!tree.home.root().exists(), "no home root");
        assert!(
            !tree.file_structure.state.project_state_dir(&tree.key()).exists(),
            "no per-project state directory"
        );
        assert!(
            read_dir_names(&tree.file_structure.root().join("projects")).is_empty(),
            "RUL-28 — and no GC ledger entry"
        );
        assert!(!report.stamp_written);
    }

    /// C-049, case 69 — the dry run **predicts what a real run applies**: the
    /// two `items` sets are equal modulo `Written`/`Unchanged`.
    ///
    /// RED: build the dry-run delta from a different code path than the write
    /// pass — the two then drift silently, and `--dry-run` becomes advice
    /// about a render that never happens.
    #[tokio::test]
    async fn a_dry_run_predicts_exactly_what_the_real_run_applies() {
        let tree = Tree::new();
        tree.create_home_root();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        std::fs::write(tree.home.shell_bin(DEFAULT_SHELL).join("stale"), b"#!/bin/sh\n").unwrap();

        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]);
        let surface = vec![node(pinned("ns/cmake", 'd'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(vec![tool]);
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let manager = tree.manager();

        let predicted = manager
            .render_toolchain(RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: true,
            })
            .await
            .expect("the dry run succeeds");
        let applied = manager
            .render_toolchain(RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            })
            .await
            .expect("the real run succeeds");

        let normalise = |report: &RenderReport| {
            let mut rows: Vec<(RenderedArtifact, bool)> = report
                .items
                .iter()
                .map(|item| (item.artifact.clone(), is_skipped(&item.outcome)))
                .collect();
            rows.sort_by_key(|(artifact, _)| format!("{artifact:?}"));
            rows
        };
        assert_eq!(
            normalise(&predicted),
            normalise(&applied),
            "C-049 — the same delta, from the same code path"
        );
        assert_eq!(predicted.bin_in_scope, applied.bin_in_scope);
    }

    /// C-049, case 70 — `stamp_written` is always `false` under a dry run,
    /// **and the existing stamp file's mtime is unchanged**.
    ///
    /// The mtime is the discriminating half: validation item 32 makes the
    /// stamp's *fingerprint* mtime-independent, which is exactly what hides a
    /// dry run that rewrote the stamp with identical content. Asserting the
    /// *file's* mtime is what catches it.
    ///
    /// RED: write the stamp under dry run.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_dry_run_writes_no_stamp_and_does_not_even_touch_the_existing_one() {
        let tree = Tree::new();
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let manager = tree.manager();

        manager
            .render_toolchain(RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            })
            .await
            .expect("the seeding render writes a stamp");
        let before = snapshot_subtree(&tree.file_structure.state.project_state_dir(&tree.key()));
        assert!(!before.is_empty(), "precondition: a stamp exists to be left alone");

        let report = manager
            .render_toolchain(RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: true,
            })
            .await
            .expect("the dry run succeeds");

        assert!(
            !report.stamp_written,
            "C-049 — a dry run wrote no tree, so it stamps nothing"
        );
        assert_eq!(
            snapshot_subtree(&tree.file_structure.state.project_state_dir(&tree.key())),
            before,
            "…and the existing stamp file's own mtime and inode are untouched"
        );
    }

    // ── 12. The review round's regressions (B1…B5, W8) ──────────────────────

    /// The directory's own `mtime`, in nanoseconds.
    ///
    /// The one observation that catches a file **created and then unlinked**
    /// inside a directory: a subtree snapshot of that directory cannot, because
    /// the entry is gone by the time it is taken, and the litter is the point
    /// of the claim, not the leftover.
    #[cfg(unix)]
    #[track_caller]
    fn dir_mtime_nsec(path: &Path) -> i64 {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::symlink_metadata(path)
            .unwrap_or_else(|e| panic!("{path:?} must exist: {e}"))
            .mtime_nsec()
    }

    /// B1/RUL-33 — `heal_links` refuses a home root that is a symlink out of
    /// the project, and creates **nothing** in the link's target.
    ///
    /// C-070 puts heal on every composing emit, so `ocx env` and `ocx exec`
    /// reach this on every prompt: without the guard a hostile clone that
    /// commits `.ocx/toolchain` as a symlink to `$HOME` turns the per-prompt
    /// path into a write primitive outside the project, since RUL-29 has heal
    /// *create* absent links and `repoint_link` `create_dir_all`s the group
    /// directory under them.
    ///
    /// The second half is the positive control: without it a `heal_links` that
    /// refused every home would pass the first half.
    ///
    /// RED: drop the `ensure_home_root` call at the top of `heal_links`.
    #[cfg(unix)]
    #[tokio::test]
    async fn healing_refuses_a_home_root_that_is_a_symlink_out_of_the_project() {
        let tree = Tree::new();
        let outside = tree.tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(tree.home.root().parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&outside, tree.home.root()).unwrap();
        let before = snapshot_subtree(&outside);

        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]);
        let lock = lock_of(vec![tool]);
        let groups = groups_of(&[DEFAULT_GROUP]);

        assert!(
            matches!(
                heal_links(
                    &tree.file_structure,
                    &tree.home,
                    &tree.scope(),
                    &lock,
                    &groups,
                    &platform()
                )
                .await
                .expect("RUL-36 — a refusal degrades, it never errors"),
                HealOutcome::Refused { .. }
            ),
            "an unrepairable home repairs nothing — and says so, rather than answering what a \
             clean pass answers"
        );
        assert_eq!(
            snapshot_subtree(&outside),
            before,
            "RUL-33 — and nothing was written into the symlink's target"
        );

        // The positive control: an ordinary home still heals.
        let honest = Tree::new();
        std::fs::create_dir_all(honest.home.root()).unwrap();
        assert_eq!(
            heal_links(
                &honest.file_structure,
                &honest.home,
                &honest.scope(),
                &lock,
                &groups,
                &platform()
            )
            .await
            .expect("an ordinary home heals"),
            HealOutcome::Healed(1),
            "the guard must not refuse a legitimate home — three refusals are otherwise \
             satisfied by a function that refuses everything"
        );
    }

    /// B2/C-050 — `publish_link` refuses a `<group>/` directory that is a
    /// symlink, and destroys nothing in its target.
    ///
    /// `prune_within` already refuses an entry reached this way (case 25(c));
    /// the write side had no counterpart, and `ensure_home_root` guards only
    /// the home root and `bin/`. `create_dir_all` succeeds silently through the
    /// link and `replace_atomic`'s POSIX arm is a `rename` that **replaces an
    /// existing regular file** — so a bare `ocx pull` destroyed an
    /// attacker-chosen file outside the home.
    ///
    /// RED: **two** mutations, because two guards now defend this and either
    /// alone leaves the row green — drop the `symlink_metadata(parent)` refusal
    /// in `publish_link_within` *and* give `ensure_link_group` back the
    /// recursive `create_owner_only`, whose `create_dir_all` follows the link.
    /// `outside/cmake` is then replaced by a symlink.
    #[cfg(unix)]
    #[tokio::test]
    async fn publishing_a_link_refuses_a_symlinked_group_directory() {
        let tree = Tree::new();
        let outside = tree.tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("cmake"), b"someone else's file\n").unwrap();
        std::fs::create_dir_all(links_root(&tree.home)).unwrap();
        std::os::unix::fs::symlink(&outside, links_root(&tree.home).join(DEFAULT_GROUP)).unwrap();
        let before = snapshot_subtree(&outside);

        let target = tree.tmp.path().join("package-root");
        let entry = tree.home.entry(DEFAULT_GROUP, "cmake").expect("an admitted pair");
        let outcome = publish_link(&entry, &target, false).await;

        assert!(
            is_skipped(&outcome),
            "C-050 — a symlinked group directory is a skip, not a write and not an error; got {outcome:?}"
        );
        assert_eq!(
            snapshot_subtree(&outside),
            before,
            "…and the file the rename would have replaced is byte-for-byte intact"
        );

        // The positive control: a real group directory is still written.
        let honest = tree.home.links_group("ci").expect("an admitted group name");
        std::fs::create_dir_all(&honest).unwrap();
        let outcome = publish_link(&honest.join("cmake"), &target, false).await;
        assert_eq!(
            outcome,
            RenderOutcome::Written,
            "the guard must not refuse an ordinary group directory"
        );
        assert_eq!(std::fs::read_link(honest.join("cmake")).unwrap(), target);
    }

    /// S1 — `publish_link` refuses a symlinked **`links/`**, one level above
    /// the group directory it already refuses.
    ///
    /// `links` was the one tree-own directory nothing ever created under a
    /// guard: it came into existence only as `create_dir_all(<root>/links/
    /// <group>)`'s side effect, so its only check was `ensure_home_root`'s at
    /// step 2 — and the render lock's unbounded wait sits between that check and
    /// this write. A local writer that swaps `links` for a symlink inside that
    /// window sends the group create and the entry's `replace_atomic` into an
    /// attacker-chosen directory. `bin/` and `shells/` already re-judge at the
    /// moment of use; this is the third level joining them.
    ///
    /// **The plant stands in for that swap.** Through the public render entry,
    /// step 2 refuses a pre-existing symlinked `links` and the whole render
    /// skips, so the write-side re-judgement is only reachable by calling the
    /// write directly — which is precisely the post-step-2 state a concurrent
    /// writer produces. The positive control is the whole rest of this module:
    /// every render row publishes through a real `links/`, so a guard that
    /// refused everything could not stay green here.
    ///
    /// RED: give `ensure_link_group` back the recursive `create_owner_only` —
    /// `create_dir_all` follows the link, `outside/` gains the group directory,
    /// and the entry is published inside it.
    #[cfg(unix)]
    #[tokio::test]
    async fn publishing_a_link_refuses_a_symlinked_links_directory() {
        let tree = Tree::new();
        let outside = tree.tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("bystander"), b"someone else's file\n").unwrap();
        std::fs::create_dir_all(tree.home.root()).unwrap();
        std::os::unix::fs::symlink(&outside, links_root(&tree.home)).unwrap();
        let before = snapshot_subtree(&outside);

        let target = tree.tmp.path().join("package-root");
        let entry = tree.home.entry(DEFAULT_GROUP, "cmake").expect("an admitted pair");
        let outcome = publish_link(&entry, &target, false).await;

        assert!(
            is_skipped(&outcome),
            "a symlinked `links/` is a skip, not a write and not an error; got {outcome:?}"
        );
        assert_eq!(
            snapshot_subtree(&outside),
            before,
            "…and nothing was created inside the directory that link points at"
        );
    }

    /// B2/RUL-36 — the same refusal on the heal side: `repoint_link` never
    /// writes through a symlinked `<group>/`, and the entry stays uncounted.
    ///
    /// The seeded entry is **absent**, deliberately: RUL-29 has heal *create*
    /// one, and that is the only shape that reaches `repoint_link` at all. With
    /// a regular file planted at `<group>/<entry>` instead, heal's own
    /// shape probe (`entry.exists() && !is_link`) skips the entry before the
    /// group directory is ever touched — a second guard that makes this
    /// mutation green for the wrong reason.
    ///
    /// RED: give `ensure_link_group` back the recursive `create_owner_only` —
    /// `create_dir_all` follows the link, and the repoint lands in its target.
    #[cfg(unix)]
    #[tokio::test]
    async fn healing_refuses_a_symlinked_group_directory() {
        let tree = Tree::new();
        let outside = tree.tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("bystander"), b"someone else's file\n").unwrap();
        std::fs::create_dir_all(links_root(&tree.home)).unwrap();
        std::os::unix::fs::symlink(&outside, links_root(&tree.home).join(DEFAULT_GROUP)).unwrap();
        let before = snapshot_subtree(&outside);

        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]);
        let lock = lock_of(vec![tool]);

        assert_eq!(
            heal_links(
                &tree.file_structure,
                &tree.home,
                &tree.scope(),
                &lock,
                &groups_of(&[DEFAULT_GROUP]),
                &platform()
            )
            .await
            .expect("RUL-36 — never an error"),
            HealOutcome::Healed(0),
            "RUL-36 — an entry that cannot be repaired is uncounted, and the tree was still \
             entered: this is a per-entry degrade, not a whole-tree refusal"
        );
        assert_eq!(
            snapshot_subtree(&outside),
            before,
            "…and nothing outside the home was created, written or replaced"
        );

        // The positive control: a real group directory still gets its link.
        let honest = Tree::new();
        std::fs::create_dir_all(honest.home.root()).unwrap();
        assert_eq!(
            heal_links(
                &honest.file_structure,
                &honest.home,
                &honest.scope(),
                &lock,
                &groups_of(&[DEFAULT_GROUP]),
                &platform()
            )
            .await
            .expect("an ordinary home heals"),
            HealOutcome::Healed(1),
            "RUL-29 — the guard must not refuse an ordinary group directory"
        );
    }

    /// B3/RUL-46, the `bin/` half — on a **case-insensitive** home the prune
    /// comparison is ASCII-case-folded, so a pre-existing entry differing from
    /// the winner only in case is **not** removed.
    ///
    /// The bug this pins is data loss, and it needs the write pass and the
    /// prune pass read together. `expected` holds the winner's *original*
    /// spelling (RUL-19), so with `Make` winning and `bin/make` already on
    /// disk: on a case-insensitive filesystem the two names are one file, the
    /// write pass finds the body already correct and leaves it `Unchanged`
    /// under its on-disk name `make`, and a case-**sensitive** prune then sees
    /// `make` ∉ {`Make`} and removes it. `fingerprint_bin` afterwards stats a
    /// file that is gone, so the stamp records an empty name set that *matches*
    /// the now-empty `bin/` — C-061's gate is satisfied while the tool has
    /// silently vanished.
    ///
    /// On the case-**sensitive** filesystem this test runs on, `bin/make` and
    /// `bin/Make` are two files, so the end state differs; what is pinned here
    /// is the rule that prevents the loss — the folded entry survives the prune
    /// and is not reported — plus its gate, in the second half.
    ///
    /// RED: use `expected.contains(&name)` in `reconcile_bin`'s prune loop.
    #[tokio::test]
    async fn a_differently_cased_bin_entry_survives_the_prune_on_a_case_insensitive_home() {
        for case_insensitive in [true, false] {
            let tree = Tree::new();
            tree.create_home_root();
            // The row's premise, read rather than presupposed: `bin/make` is
            // the winner `Make`'s *twin* only where the two are separate files.
            // On a case-insensitive volume they are one, the injected flag stops
            // describing the disk it is asserted against, and neither arm's end
            // state is the one written down above.
            if !tree.holds_case_twins().await {
                eprintln!(
                    "skipped: the probe reported a case-insensitive home, so `make` and `Make` cannot \
                     be two files here"
                );
                return;
            }
            std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
            let twin = tree.home.shell_bin(DEFAULT_SHELL).join("make");
            std::fs::write(&twin, b"#!/bin/sh\n# already here\n").unwrap();

            let surface = vec![node(pinned("ns/make", 'a'), Some(&["Make"]), &[])];
            let scope = tree.scope();
            let lock = lock_of(Vec::new());
            let groups = groups_of(&[DEFAULT_GROUP]);
            let platform = platform();
            let report = render_with(
                &tree.file_structure,
                RenderRequest {
                    home: &tree.home,
                    scope: &scope,
                    lock: &lock,
                    surface: &surface,
                    groups: &groups,
                    pinned: false,
                    platform: &platform,
                    dry_run: false,
                },
                case_insensitive,
                None,
            )
            .await
            .expect("the render is not a refusal");

            let pruned = report
                .items
                .iter()
                .any(|item| item.artifact == trampoline("make") && item.outcome == RenderOutcome::Pruned);
            assert_eq!(
                twin.exists(),
                case_insensitive,
                "RUL-46 — `make` is the winner `Make` itself on a case-insensitive home and must \
                 survive; on a case-sensitive one it is an ordinary orphan and must not \
                 (case_insensitive = {case_insensitive})"
            );
            assert_eq!(
                pruned, !case_insensitive,
                "…and the report says the same thing (case_insensitive = {case_insensitive})"
            );
        }
    }

    /// B3/RUL-46, the `<group>/<entry>` half — the identical shape in
    /// `reconcile_links`, with the same gate.
    ///
    /// RED: use `expected.contains(&name)` in `reconcile_links`' prune loop.
    #[tokio::test]
    async fn a_differently_cased_group_entry_survives_the_prune_on_a_case_insensitive_home() {
        for case_insensitive in [true, false] {
            let tree = Tree::new();
            tree.create_home_root();
            // As in the `bin/` half above: `CMAKE` is `cmake`'s twin only where
            // the two are separate entries.
            if !tree.holds_case_twins().await {
                eprintln!(
                    "skipped: the probe reported a case-insensitive home, so `cmake` and `CMAKE` \
                     cannot be two entries here"
                );
                return;
            }
            let group = tree.home.links_group(DEFAULT_GROUP).expect("an admitted group name");
            std::fs::create_dir_all(&group).unwrap();
            let twin = group.join("CMAKE");
            crate::symlink::create(tree.tmp.path().join("some-package"), &twin).unwrap();

            let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]);
            let scope = tree.scope();
            let lock = lock_of(vec![tool]);
            let groups = groups_of(&[DEFAULT_GROUP]);
            let platform = platform();
            let report = render_with(
                &tree.file_structure,
                RenderRequest {
                    home: &tree.home,
                    scope: &scope,
                    lock: &lock,
                    surface: &[],
                    groups: &groups,
                    pinned: false,
                    platform: &platform,
                    dry_run: false,
                },
                case_insensitive,
                None,
            )
            .await
            .expect("the render is not a refusal");

            let pruned = report
                .items
                .iter()
                .any(|item| item.artifact == link(DEFAULT_GROUP, "CMAKE") && item.outcome == RenderOutcome::Pruned);
            assert_eq!(
                crate::symlink::is_link(&twin),
                case_insensitive,
                "RUL-46 — on a case-insensitive home `CMAKE` *is* the entry `cmake` the write pass \
                 just repointed (case_insensitive = {case_insensitive})"
            );
            assert_eq!(
                pruned, !case_insensitive,
                "…and the report says the same thing (case_insensitive = {case_insensitive})"
            );
        }
    }

    /// RUL-74/C-049 — a dry run writes no case-fold probe file **anywhere**:
    /// not into the project checkout of a never-rendered home, and not into a
    /// home root that already exists either.
    ///
    /// The shipped shape handed the probe `nearest_existing_directory(root)`,
    /// which for a never-rendered project home is the checkout itself — and
    /// `$HOME` when a configured `toolchain-dir`'s parents are absent — so
    /// every `ocx pull --dry-run` created and unlinked a
    /// `.ocx-case-probe-<pid>-<nanos>` there. Narrowing it to "probe only an
    /// existing home root" fixed the litter and kept the write: the probe still
    /// created and unlinked a file *inside* the home, moving that directory's
    /// `mtime` on every dry run, which `ocx pull --dry-run` over an already
    /// rendered tree reports as a changed path. C-049 is "writes nothing", and
    /// that beats an exact prediction in an edge case.
    ///
    /// The directory's `mtime` is the discriminating observation: the probe
    /// removes its own file, so a subtree snapshot taken afterwards is clean in
    /// both worlds. The third block is the positive control — it calls the
    /// probe directly and shows the `mtime` *does* move, so the two negatives
    /// above are the probe not running rather than the instrument not reading.
    ///
    /// RED: probe `request.home.root()` under `dry_run`, in either shape.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_dry_run_writes_no_case_probe_anywhere() {
        let tree = Tree::new();
        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let manager = tree.manager();
        let request = || RenderRequest {
            home: &tree.home,
            scope: &scope,
            lock: &lock,
            surface: &surface,
            groups: &groups,
            pinned: false,
            platform: &platform,
            dry_run: true,
        };

        assert!(!tree.home.root().exists(), "precondition: the home was never rendered");
        let before = snapshot_subtree(&tree.project_dir);
        let before_mtime = dir_mtime_nsec(&tree.project_dir);

        manager.render_toolchain(request()).await.expect("the dry run succeeds");

        assert_eq!(
            snapshot_subtree(&tree.project_dir),
            before,
            "C-049 — the dry run left no object in the project checkout"
        );
        assert_eq!(
            dir_mtime_nsec(&tree.project_dir),
            before_mtime,
            "…and created none there either: a probe file that is created and unlinked is \
             invisible to a snapshot but moves the directory's own mtime"
        );

        // A home root that exists is not a licence to write into it either.
        tree.create_home_root();
        let seeded_mtime = dir_mtime_nsec(tree.home.root());
        manager.render_toolchain(request()).await.expect("the dry run succeeds");
        assert_eq!(
            dir_mtime_nsec(tree.home.root()),
            seeded_mtime,
            "RUL-74 — the probe writes into the directory it judges, so probing an existing home \
             root is still a write C-049 forbids"
        );

        // The positive control: the probe does move the mtime this test reads,
        // so the two negatives above are the probe not running — not
        // `dir_mtime_nsec` failing to observe it.
        filesystem_is_case_insensitive(tree.home.root())
            .await
            .expect("the probe answers for a directory it may write in");
        assert_ne!(
            dir_mtime_nsec(tree.home.root()),
            seeded_mtime,
            "the probe moves the home root's mtime — without this, a `dir_mtime_nsec` that never \
             changed would pass both assertions above"
        );
    }

    /// B5/C-003 — the Windows `Unchanged` predicate requires `<name>.exe` to
    /// **be** the published `ShimBinStore` blob, not merely to be a regular
    /// file beside a matching sidecar.
    ///
    /// The sidecar body is one line — the absolute project root plus `\n` — so
    /// it is predictable on a CI runner. With the existence-only predicate, a
    /// hostile clone shipping a substituted `bin/<name>.exe` beside a matching
    /// `<name>.exec` was reported `Unchanged`, never republished, landed in
    /// `landed`, was stamped by `fingerprint_bin` and then blessed by C-061's
    /// gate — defeating the ADR's R1 mitigation, whose whole claim is that the
    /// first `ocx pull` renders over it.
    ///
    /// Runs on every host: the predicate is deliberately not `#[cfg(windows)]`,
    /// for the reason `publish_windows_trampoline` states — a `cfg`-gated seam
    /// puts the property behind a `cfg` the CI leg that actually runs never
    /// compiles.
    ///
    /// RED: return `symlink_metadata(exe).is_ok_and(|m| m.is_file())` for the
    /// `Some(blob)` arm.
    #[test]
    fn the_windows_unchanged_predicate_requires_the_exe_to_be_the_published_blob() {
        let tmp = tempfile::tempdir().unwrap();
        let blob = tmp.path().join("0123abcd.exe");
        std::fs::write(&blob, b"MZ\x00the published shim blob\n").unwrap();
        let exe = tmp.path().join("cmake.exe");
        let sidecar_path = tmp.path().join("cmake.exec");
        let sidecar = "/home/someone/project\n";
        std::fs::write(&sidecar_path, sidecar).unwrap();

        // The positive control, first: a hardlink of the blob beside a matching
        // sidecar is exactly what the render publishes, and republishing it on
        // every invocation would break C-047's idempotence.
        std::fs::hard_link(&blob, &exe).unwrap();
        assert!(
            windows_pair_unchanged(&exe, &sidecar_path, sidecar, Some(&blob)).unwrap(),
            "the pair this render would publish is `Unchanged`"
        );

        // A substituted executable beside the same matching sidecar.
        std::fs::remove_file(&exe).unwrap();
        std::fs::write(&exe, b"MZ\x00attacker-authored\n").unwrap();
        assert!(
            !windows_pair_unchanged(&exe, &sidecar_path, sidecar, Some(&blob)).unwrap(),
            "C-003 — a substituted `.exe` takes the write branch, whatever the sidecar says"
        );

        // A byte-identical *copy* is still not the blob: #301's property is one
        // inode per store, so an `ocx` upgrade that re-signs the blob must
        // reach every published name.
        std::fs::copy(&blob, &exe).unwrap();
        assert!(
            !windows_pair_unchanged(&exe, &sidecar_path, sidecar, Some(&blob)).unwrap(),
            "the predicate is file identity, not content equality"
        );

        // And the sidecar half still decides on its own.
        std::fs::remove_file(&exe).unwrap();
        std::fs::hard_link(&blob, &exe).unwrap();
        std::fs::write(&sidecar_path, "/somewhere/else\n").unwrap();
        assert!(
            !windows_pair_unchanged(&exe, &sidecar_path, sidecar, Some(&blob)).unwrap(),
            "a sidecar naming another project is not this render's pair"
        );
    }

    /// W6/R-W19 — the POSIX `Unchanged` predicate decides on the mode as well
    /// as the bytes: a clone committing `bin/<name>` with the exact expected
    /// body at git mode `100644` is republished, not left non-executable
    /// forever.
    ///
    /// RED: drop `&& has_execute_bit(&path)` from the POSIX arm.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_committed_trampoline_body_without_its_execute_bit_is_republished() {
        use std::os::unix::fs::PermissionsExt as _;

        let tree = Tree::new();
        tree.create_home_root();
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        let entry = tree.home.shell_bin(DEFAULT_SHELL).join("cmake");
        let body = expected_unix_trampoline_body(&tree.project_dir, &tree.ocx_binary);
        std::fs::write(&entry, body.as_bytes()).unwrap();
        std::fs::set_permissions(&entry, std::fs::Permissions::from_mode(0o644)).unwrap();

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render is not a refusal");

        assert_eq!(
            outcome_of(&report, &trampoline("cmake")),
            &RenderOutcome::Written,
            "R-W19 — matching bytes at a non-executable mode are not `Unchanged`"
        );
        assert!(
            mode_of(&entry) & 0o111 != 0,
            "…and the republished entry carries an execute bit"
        );
    }

    /// W5/R-W19(a) — **every** directory a render creates is owner-only at
    /// create time, like the home root, and never with the ambient umask.
    ///
    /// The trampoline directory is what all three PATH routes point at, so a
    /// group-writable one is a write primitive into everything the user's shell
    /// resolves — and `links/`, `shells/` and `shells/<shell>/` are each an
    /// ancestor of something on that path, so a group-writable one of those is
    /// a rename primitive over the whole subtree below it. This layout added
    /// three directories and nothing mode-checked them.
    ///
    /// RED: restore `tokio::fs::create_dir_all` in `reconcile_bin` and
    /// `std::fs::create_dir_all` in `publish_link_within` — under the ordinary
    /// `umask 002` both come out group-writable. RED for the three new rows:
    /// give `ensure_shell_tree` a plain `std::fs::create_dir_all`.
    #[cfg(unix)]
    #[tokio::test]
    async fn every_directory_a_render_creates_is_owner_only() {
        let tree = Tree::new();
        // Not `create_home_root`: this row is about the modes the **render**
        // chooses, and a fixture that pre-created `shells/<shell>` with the
        // ambient umask would answer for itself.
        std::fs::create_dir_all(tree.home.root()).unwrap();

        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]);
        let surface = vec![node(pinned("ns/cmake", 'd'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(vec![tool]);
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        // The **public** entry, so step 6 creates `shells/` and
        // `shells/<shell>/` under the code that chooses their modes.
        tree.manager()
            .render_toolchain(RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            })
            .await
            .expect("the render is not a refusal");

        for directory in [
            tree.home.shell_bin(DEFAULT_SHELL),
            tree.shell_directory(),
            tree.home
                .links_group(DEFAULT_GROUP)
                .expect("the default group is admitted"),
            links_root(&tree.home),
            shell_directory_of(&tree.home)
                .parent()
                .expect("the shell directory has a parent")
                .to_path_buf(),
        ] {
            assert!(directory.is_dir(), "precondition: {directory:?} was created");
            assert_eq!(
                mode_of(&directory) & 0o077,
                0,
                "R-W19(a) — {directory:?} must grant nothing to group or other"
            );
            assert!(
                mode_of(&directory) & 0o700 == 0o700,
                "…while staying fully usable by its owner"
            );
        }
    }

    /// W3/RUL-32 — a leaked case-probe file at the home root is pruned as the
    /// orphan it is, because the home-root arm chooses its removal by the
    /// entry's **on-disk type**.
    ///
    /// `filesystem_is_case_insensitive` states that a probe file outliving its
    /// probe "is pruned by the very next render"; with an unconditional
    /// `remove_dir` there that was false — `ENOTDIR` on a regular file, so the
    /// litter was reported `Skipped` on every render forever.
    ///
    /// The second half is the control RUL-32 is about: a **non-empty**
    /// directory is still not removed, and still not removed recursively.
    ///
    /// RED: restore the unconditional `remove_dir` in the shared
    /// `GroupDirectory | RootEntry` arm.
    #[test]
    fn a_leaked_probe_file_at_the_home_root_is_pruned_and_a_directory_is_not_recursed() {
        let tree = Tree::new();
        std::fs::create_dir_all(tree.home.root()).unwrap();
        let litter = tree.home.root().join(".ocx-case-probe-4242-1");
        std::fs::write(&litter, b"").unwrap();

        prune_within(&tree.home, &root_entry(".ocx-case-probe-4242-1"))
            .expect("a regular file at the home root is removable");
        assert!(!litter.exists(), "W3 — the orphan the probe left is gone");

        let occupied = tree.home.links_group("ci").expect("an admitted group name");
        std::fs::create_dir_all(&occupied).unwrap();
        std::fs::write(occupied.join("foreign"), b"not ours\n").unwrap();
        assert!(
            prune_within(&tree.home, &group_directory("ci")).is_err(),
            "RUL-32 — a non-empty group directory is still refused, never recursed"
        );
        assert!(occupied.join("foreign").exists());
    }

    /// RUL-32 — a foreign **regular file** at `<home>/<name>` or at
    /// `<home>/links/<name>` survives byte-for-byte on an ordinary render, and
    /// is named in the report.
    ///
    /// Both names reach the one shared `GroupDirectory | RootEntry` prune arm,
    /// whose `remove_file` fallthrough dispatched on the on-disk type alone —
    /// so anything that was neither a symlink nor a directory was deleted and
    /// reported `Pruned`, which carries **no** warn line. A user's file
    /// therefore vanished on a plain `ocx pull` with nothing said. The
    /// `links/<name>` half is the one this layout newly reaches: before
    /// `links/` existed there was no second level for a foreign file to sit
    /// on. The rule the `Link` arm already follows now holds for the whole
    /// function — ocx removes what ocx wrote, and anything else survives and
    /// is named.
    ///
    /// RED: restore the unconditional `std::fs::remove_file` fallthrough in
    /// that arm (drop the `is_leaked_case_probe` guard and the
    /// `refuse_not_a_link` else). Both files are deleted and both outcomes
    /// read `Pruned`.
    #[tokio::test]
    async fn a_foreign_file_at_a_pruned_name_survives_and_is_reported() {
        let tree = Tree::new();
        std::fs::create_dir_all(links_root(&tree.home)).unwrap();
        let at_root = tree.home.root().join("notes.txt");
        let under_links = links_root(&tree.home).join("notes.txt");
        std::fs::write(&at_root, b"mine, not ocx's\n").unwrap();
        std::fs::write(&under_links, b"mine either\n").unwrap();

        let report = render_a_default_tool(&tree).await;

        // Both survivals in **one** assertion, so a red names both halves at
        // once: asserting them in the loop below stops at the depth-1 file and
        // says nothing about the `links/` level, which is the half this layout
        // newly reaches.
        assert_eq!(
            (std::fs::read(&at_root).ok(), std::fs::read(&under_links).ok()),
            (Some(b"mine, not ocx's\n".to_vec()), Some(b"mine either\n".to_vec())),
            "both planted files survive byte-for-byte"
        );

        for (planted, artifact) in [
            (&at_root, root_entry("notes.txt")),
            (&under_links, group_directory("notes.txt")),
        ] {
            let outcome = outcome_of(&report, &artifact);
            let RenderOutcome::Skipped { path, reason } = outcome else {
                panic!("{artifact:?} must be reported Skipped, not {outcome:?}");
            };
            assert_eq!(path, planted, "…and the report names the path it left alone");
            assert!(!reason.is_empty(), "…with a reason the user can act on");
        }
    }

    /// W8/RUL-44 — a symlink on a component **between** the project directory
    /// and the home root is refused, and the render skips (C-050).
    ///
    /// `ensure_home_root` judges the home root and `bin/`, and
    /// `symlink_metadata` does not follow only the last component — so
    /// committing `<project>/.ocx` as a symlink relocated the whole rendered
    /// tree while every existing guard still passed, `prune_within` included:
    /// its containment canonicalises both sides, so a home reached through the
    /// link is contained in itself.
    ///
    /// The second half is the positive control: an ordinary `<project>/.ocx`
    /// still renders, or the guard could refuse everything.
    ///
    /// RED: drop the `refuse_symlinked_project_path` call in
    /// `render_toolchain`'s step 2.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_component_above_the_home_root_skips_the_render() {
        let tree = Tree::new();
        let outside = tree.tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, tree.project_dir.join(".ocx")).unwrap();
        let before = snapshot_subtree(&outside);

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let report = tree
            .manager()
            .render_toolchain(RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            })
            .await
            .expect("C-050 — a refusal is a skip, never an error");

        assert!(
            report.items.is_empty() && !report.stamp_written,
            "RUL-44 — the render reached nothing; got {report:?}"
        );
        assert_eq!(
            snapshot_subtree(&outside),
            before,
            "…and not one byte was written through the relocated component"
        );

        // The positive control: an ordinary `.ocx` renders.
        let honest = Tree::new();
        let scope = honest.scope();
        let report = honest
            .manager()
            .render_toolchain(RenderRequest {
                home: &honest.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            })
            .await
            .expect("an ordinary project home renders");
        assert_eq!(
            honest.bin_entries(),
            expected_bin_entries(&["cmake"]),
            "the guard must not refuse an ordinary project home"
        );
        assert!(report.stamp_written);
    }

    /// B1/RUL-47 — `heal_links` refuses a symlink on a component **between**
    /// the project directory and the home root, and creates nothing in the
    /// link's target.
    ///
    /// The render's own RUL-44 guard is not enough on its own: C-070 puts heal
    /// on **every composing emit**, so `ocx env` and `ocx exec` reach this per
    /// prompt while `ocx pull` reaches the render occasionally — the more
    /// frequently travelled of the two writers was the unguarded one. A
    /// hostile clone committing `<project>/.ocx` as a symlink relocates
    /// everything RUL-29's create and `repoint_link`'s `create_owner_only`
    /// write, and `ensure_home_root` cannot see it: `symlink_metadata` does
    /// not follow only the *last* component, so the home root under the link
    /// reports as an ordinary directory.
    ///
    /// The second half is the positive control: an ordinary project home still
    /// heals its entry, or the widening would be satisfied by a `heal_links`
    /// that refuses every project.
    ///
    /// RED: drop the `refuse_symlinked_project_path` call in `heal_links`.
    #[cfg(unix)]
    #[tokio::test]
    async fn healing_refuses_a_symlinked_component_above_the_home_root() {
        let tree = Tree::new();
        let outside = tree.tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, tree.project_dir.join(".ocx")).unwrap();
        let before = snapshot_subtree(&outside);

        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]);
        let lock = lock_of(vec![tool]);
        let groups = groups_of(&[DEFAULT_GROUP]);

        assert!(
            matches!(
                heal_links(
                    &tree.file_structure,
                    &tree.home,
                    &tree.scope(),
                    &lock,
                    &groups,
                    &platform()
                )
                .await
                .expect("RUL-36 — a refusal degrades, it never errors"),
                HealOutcome::Refused { .. }
            ),
            "RUL-47 — a relocated home repairs nothing, and reports the refusal rather than a \
             count a clean pass also produces"
        );
        assert_eq!(
            snapshot_subtree(&outside),
            before,
            "…and not one byte was written through the relocated component"
        );

        // The positive control: an ordinary project home still heals.
        let honest = Tree::new();
        std::fs::create_dir_all(honest.home.root()).unwrap();
        assert_eq!(
            heal_links(
                &honest.file_structure,
                &honest.home,
                &honest.scope(),
                &lock,
                &groups,
                &platform()
            )
            .await
            .expect("an ordinary home heals"),
            HealOutcome::Healed(1),
            "the guard must not refuse an ordinary project home"
        );
    }

    /// B1 — a **refusal** is distinguishable from a **clean pass** at
    /// `heal_links`' own signature, not only at whichever call site remembered
    /// to re-take the guard.
    ///
    /// The two sibling tests above pin that a refused tree repairs nothing.
    /// Neither can pin that a caller can *tell*: a clean pass repairs nothing
    /// either, so while the signature folded both into `Ok(0)` the two states
    /// were one value. That is the "degrade-to-`Ok` erases the refusal for
    /// every caller" shape — the guard then protects only the write path, and
    /// a composer that reads through the same symlink into `PATH` and
    /// `${installPath}` sees a return value indistinguishable from "every link
    /// was already correct".
    ///
    /// The inequality is the whole property, and it is asserted between two
    /// answers produced by the same call with the same lock and the same
    /// groups — the only difference is the tree. Anything weaker (asserting
    /// only the refused half) is satisfied by a `heal_links` that reports a
    /// refusal for every tree.
    ///
    /// RED: fold either arm back into a shared value — the two answers become
    /// one and this fails.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_refused_tree_is_distinguishable_from_a_clean_pass() {
        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", &[(PLATFORM_KEY, 'd')]);
        let lock = lock_of(vec![tool]);
        let groups = groups_of(&[DEFAULT_GROUP]);

        // A clean pass: heal once so the entry is correct, then again — there
        // is nothing left to repair, which is the state that used to share a
        // value with the refusal.
        let honest = Tree::new();
        std::fs::create_dir_all(honest.home.root()).unwrap();
        assert_eq!(
            heal_links(
                &honest.file_structure,
                &honest.home,
                &honest.scope(),
                &lock,
                &groups,
                &platform(),
            )
            .await
            .expect("the first heal repairs the entry"),
            HealOutcome::Healed(1),
            "the fixture's premise: there was something to repair"
        );
        let clean = heal_links(
            &honest.file_structure,
            &honest.home,
            &honest.scope(),
            &lock,
            &groups,
            &platform(),
        )
        .await
        .expect("the second heal has nothing to repair");

        // A refusal: a hostile clone commits `<project>/.ocx` as a symlink out
        // of the project (RUL-47).
        let hostile = Tree::new();
        let outside = hostile.tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, hostile.project_dir.join(".ocx")).unwrap();
        let refused = heal_links(
            &hostile.file_structure,
            &hostile.home,
            &hostile.scope(),
            &lock,
            &groups,
            &platform(),
        )
        .await
        .expect("RUL-36 — a refusal degrades, it never errors");

        assert_ne!(
            clean, refused,
            "a refused tree must not answer what a clean pass answers — a caller that cannot \
             tell them apart reads through the refused tree as if its links were trustworthy"
        );
        // Which is which, so the inequality cannot be satisfied by swapping them.
        assert_eq!(
            clean,
            HealOutcome::Healed(0),
            "the clean pass had nothing left to repair"
        );
        assert!(
            matches!(refused, HealOutcome::Refused { .. }),
            "…and the refusal names itself, carrying the reason with it: {refused:?}"
        );
    }
    // ── 12. The closed depth-1 set, its guard, and `active` (C-074…C-083) ───

    /// The `links` component this module derives is the grammar's own — the
    /// drift guard for [`links_root`]'s `parent()` walk.
    ///
    /// The literal lives **here**, in a test, and nowhere in the production
    /// body: a second production spelling is what would let scan and guard
    /// disagree about which directory `links` is.
    ///
    /// RED: derive `links_root` from `home.root()` directly, or from any other
    /// component.
    #[test]
    fn links_root_is_the_grammars_own_parent() {
        let home = ToolchainHome::new(PathBuf::from("/ocx/toolchain"));
        assert_eq!(links_root(&home), PathBuf::from("/ocx/toolchain/links"));
        assert_eq!(
            home.links_group("ci").expect("an admitted group"),
            links_root(&home).join("ci"),
            "…and every group directory is a child of it, by construction"
        );
    }

    /// Every `RenderedArtifact` variant resolves to **one** path, whichever of
    /// the two independent derivations is asked.
    ///
    /// [`artifact_path`] computes what a `Skipped` item *reports*;
    /// [`prune_within`] computes, independently, what a prune *removes*. They
    /// are separate `match`es over the same enum and nothing compared them — so
    /// move one parent and not the other and every warning names a path the
    /// render never touched, and the prune deletes a path nothing reported.
    ///
    /// **Behavioural, not a second restatement**: the object is created at
    /// `artifact_path`'s answer and `prune_within` is then asked to remove it.
    /// A test that spelled the prune's parent a third time in test code would
    /// compare two things it wrote itself and observe neither function.
    ///
    /// RED: give any one variant a different parent in either function — the
    /// object survives and the assertion fires, or `prune_within` errors on a
    /// path that is not there.
    #[test]
    fn artifact_path_and_prune_within_agree_on_every_variant() {
        for artifact in [
            trampoline("cmake"),
            link("ci", "ninja"),
            group_directory("ci"),
            root_entry("legacy"),
        ] {
            let tree = Tree::new();
            std::fs::create_dir_all(tree.home.root()).unwrap();
            let reported = artifact_path(&tree.home, &artifact);
            std::fs::create_dir_all(reported.parent().expect("every artifact has a parent")).unwrap();
            match artifact {
                RenderedArtifact::Link { .. } => {
                    std::fs::create_dir_all(tree.tmp.path().join("package")).unwrap();
                    crate::symlink::create(tree.tmp.path().join("package"), &reported).unwrap();
                }
                RenderedArtifact::Trampoline(_) => std::fs::write(&reported, b"#!/bin/sh\n").unwrap(),
                _ => std::fs::create_dir_all(&reported).unwrap(),
            }
            assert!(reported.symlink_metadata().is_ok(), "precondition: {reported:?} exists");

            prune_within(&tree.home, &artifact)
                .unwrap_or_else(|error| panic!("{artifact:?} must be prunable at {reported:?}: {error}"));
            assert!(
                reported.symlink_metadata().is_err(),
                "the prune removed something other than the path the report names, for {artifact:?}"
            );
        }
    }

    /// C-074 — a depth-1 directory named after a **locked** group is pruned.
    ///
    /// Groups are no longer depth-1 entries, so a `ci/` at the home root is a
    /// leftover of the pre-`links/` layout and not a group, however loudly the
    /// lock mentions the name.
    ///
    /// RED: keep the lock-derived `locked_groups` skip in the depth-1 pass —
    /// the directory survives and no item names it.
    #[tokio::test]
    async fn a_depth_1_directory_named_after_a_locked_group_is_pruned() {
        let tree = Tree::new();
        tree.create_home_root();
        std::fs::create_dir_all(tree.home.root().join("ci")).unwrap();

        let scope = tree.scope();
        let lock = lock_of(vec![locked_tool("ninja", "ci", "ns/ninja", &[(PLATFORM_KEY, 'd')])]);
        let groups = groups_of(&[DEFAULT_GROUP, "ci"]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render succeeds");

        assert_eq!(
            outcome_of(&report, &root_entry("ci")),
            &RenderOutcome::Pruned,
            "the lock declares group `ci`, and that keeps `links/ci` — never `<root>/ci`"
        );
        assert!(
            !tree.home.root().join("ci").exists(),
            "…and it is gone from disk: {:?}",
            read_dir_names(tree.home.root())
        );
    }

    /// C-074's trap, stated as a test — the depth-1 keep-set is the tree's own
    /// **names**, never a name derived from an accessor.
    ///
    /// `Path::file_name` yields the string `"bin"` for `bin()` and for
    /// `shell_bin()` alike, so the keep-set the shipped code had
    /// (`reserved_name(&home.bin())`) survives a mechanical
    /// `bin() → shell_bin()` rename **byte for byte** — it compiles, it runs,
    /// and a legacy `bin/` stays at the home root forever with C-074 and C-076
    /// unimplemented.
    ///
    /// RED, and it is the mutation that matters: restore the derived keep-set
    /// in either spelling —
    /// `let reserved = [reserved_name(&home.shell_bin(DEFAULT_SHELL)), reserved_name(&home.gitignore())];`
    /// with the `reserved.iter().any(…)` skip. Both spellings red this row
    /// identically, which is the point.
    #[tokio::test]
    async fn a_legacy_bin_directory_at_the_home_root_is_not_kept_by_the_depth_1_scan() {
        let tree = Tree::new();
        tree.create_home_root();
        std::fs::create_dir_all(tree.home.root().join("bin")).unwrap();

        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &[],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render succeeds");

        assert_eq!(
            outcome_of(&report, &root_entry("bin")),
            &RenderOutcome::Pruned,
            "an **empty** legacy `bin/` is an ordinary depth-1 orphan: `remove_dir` takes it"
        );
        assert!(
            !tree.home.root().join("bin").exists(),
            "…and it is gone: {:?}",
            read_dir_names(tree.home.root())
        );
        assert_eq!(
            read_dir_names(tree.home.root()),
            vec!["active".to_string(), "shells".to_string()],
            "…leaving exactly the tree's own depth-1 names this render produced — `.gitignore` is \
             step 3's and `links/` appears once a group renders"
        );
    }

    /// C-076 — a **populated** legacy tree is reported with its remedy and
    /// never deleted, on every render.
    ///
    /// Two runs, because "reported once" was dropped as unimplementable: the
    /// report is derived from the tree as it stands, and the tree still stands.
    ///
    /// RED, and it is the one that matters (RUL-32): change `prune_within`'s
    /// directory branch from `std::fs::remove_dir` to `remove_dir_all` — the
    /// payload assertions red on the first run. RED for the *reported* half:
    /// put the legacy names back in the keep-set — the `Skipped` assertions red
    /// while the payloads still survive, so the two halves are independent and
    /// both are needed.
    #[tokio::test]
    async fn a_legacy_tree_at_the_home_root_is_reported_and_never_deleted() {
        let tree = Tree::new();
        tree.create_home_root();
        std::fs::create_dir_all(tree.home.root().join("oldgroup").join("oldentry")).unwrap();
        std::fs::write(
            tree.home.root().join("oldgroup").join("oldentry").join("payload"),
            b"planted\n",
        )
        .unwrap();
        std::fs::create_dir_all(tree.home.root().join("bin")).unwrap();
        std::fs::write(tree.home.root().join("bin").join("oldtool"), b"#!/bin/sh\nlegacy\n").unwrap();

        let scope = tree.scope();
        let lock = lock_of(Vec::new());
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        for run in 1..=2 {
            let report = render_with(
                &tree.file_structure,
                RenderRequest {
                    home: &tree.home,
                    scope: &scope,
                    lock: &lock,
                    surface: &[],
                    groups: &groups,
                    pinned: false,
                    platform: &platform,
                    dry_run: false,
                },
                false,
                None,
            )
            .await
            .expect("a legacy tree is not an error");

            for legacy in ["oldgroup", "bin"] {
                let outcome = outcome_of(&report, &root_entry(legacy));
                assert!(
                    is_skipped(outcome),
                    "run {run}: a populated `{legacy}/` is reported, never removed: {outcome:?}"
                );
                let RenderOutcome::Skipped { path, reason } = outcome else {
                    unreachable!("just asserted");
                };
                assert_eq!(path, &tree.home.root().join(legacy), "run {run}: …naming its own path");
                assert!(!reason.is_empty(), "run {run}: …with a non-empty reason (RUL-41)");
            }
            assert_eq!(
                std::fs::read(tree.home.root().join("oldgroup").join("oldentry").join("payload")).unwrap(),
                b"planted\n",
                "run {run}: RUL-32 — the payload is byte-identical, never recursed into"
            );
            assert_eq!(
                std::fs::read(tree.home.root().join("bin").join("oldtool")).unwrap(),
                b"#!/bin/sh\nlegacy\n",
                "run {run}: …and so is the legacy trampoline"
            );
        }
    }

    /// C-074's second trap — **every** directory the tree owns is refused when
    /// it is a symlink, not only the home root and the trampoline directory.
    ///
    /// This layout inserts `links/` and `shells/` between the home root and
    /// every entry link, and no shipped guard judged them:
    /// `publish_link_within` checks an entry's *immediate* parent —
    /// `links/<group>`, never `links` — and `create_owner_only` is
    /// `DirBuilder::recursive(true)`, i.e. `create_dir_all`, which follows an
    /// existing symlink. A committed `.ocx/toolchain/links -> /outside` would
    /// otherwise put every group directory and every entry link outside the
    /// home on a plain `ocx pull`.
    ///
    /// RED: shrink `tree_own_directories` back to `[root, shell_bin]` — the
    /// `links` and `shells` rows write into `outside/` and both assertions
    /// fire.
    #[cfg(unix)]
    #[tokio::test]
    async fn every_tree_own_directory_is_refused_when_it_is_a_symlink() {
        for relative in ["links", "shells", "shells/default", "shells/default/bin"] {
            let tree = Tree::new();
            std::fs::create_dir_all(tree.home.root()).unwrap();
            let outside = tree.tmp.path().join(format!("outside-{}", relative.replace('/', "-")));
            std::fs::create_dir_all(&outside).unwrap();

            let planted = tree.home.root().join(relative);
            std::fs::create_dir_all(planted.parent().unwrap()).unwrap();
            crate::symlink::create(&outside, &planted).unwrap();

            let lock = lock_of(vec![locked_tool(
                "cmake",
                DEFAULT_GROUP,
                "ns/cmake",
                &[(PLATFORM_KEY, 'a')],
            )]);
            let groups = groups_of(&[DEFAULT_GROUP]);
            let platform = platform();
            let report = tree
                .manager()
                .render_toolchain(RenderRequest {
                    home: &tree.home,
                    scope: &tree.scope(),
                    lock: &lock,
                    surface: &[node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])],
                    groups: &groups,
                    pinned: false,
                    platform: &platform,
                    dry_run: false,
                })
                .await
                .expect("RUL-33 — a refused tree is a skip, never an error");

            assert!(
                report.items.is_empty() && !report.stamp_written,
                "`{relative}` as a symlink refuses the whole render before anything is written: {report:?}"
            );
            assert_eq!(
                read_dir_names(&outside),
                Vec::<String>::new(),
                "…and nothing was written through it into {outside:?}"
            );
        }
    }

    /// C-081, row 1 — an **absent** `active` is created, silently.
    ///
    /// RED: delete the create arm — `active` stays absent and `bin()` resolves
    /// to nothing for every PATH route.
    #[tokio::test]
    async fn an_absent_active_link_is_created_by_the_render() {
        let tree = Tree::new();
        let report = render_a_default_tool(&tree).await;

        assert!(
            crate::symlink::is_link(&tree.home.active()),
            "…and it is a link, not a directory: {:?}",
            read_dir_names(tree.home.root())
        );
        assert!(
            tree.home.active_is_valid(DEFAULT_SHELL),
            "C-080 — at exactly its derived target"
        );
        assert_eq!(
            tree.bin_entries(),
            expected_bin_entries(&["cmake"]),
            "…and the trampoline landed in the physical directory: {report:?}"
        );
    }

    /// C-081, corrupt-states row 21, the **heal** half — an `active` pointing
    /// outside the home is repointed before anything is written.
    ///
    /// RED: delete the `replace_atomic` arm from [`heal_active`] — the link
    /// stays pointed at `outside/` and both assertions fire.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_active_pointing_outside_the_home_is_repointed_by_the_render() {
        let tree = Tree::new();
        std::fs::create_dir_all(tree.home.root()).unwrap();
        let outside = tree.tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        crate::symlink::create(&outside, tree.home.active()).unwrap();

        let report = render_a_default_tool(&tree).await;

        assert!(
            tree.home.active_is_valid(DEFAULT_SHELL),
            "the hostile link is repointed at its derived target: {report:?}"
        );
        assert_eq!(
            read_dir_names(&outside),
            Vec::<String>::new(),
            "…and nothing was written through it on the way"
        );
    }

    /// C-080, corrupt-states row 21, the **anchor** half — the render writes,
    /// prunes and stamps through the *physical* directory even while `active`
    /// points outside the home.
    ///
    /// # Why this drives `render_with` and not the public entry
    ///
    /// Step 6 heals `active` before the body runs, so a hostile link planted
    /// before `ocx pull` is gone by the time `reconcile_bin` writes — and a row
    /// that plants it there therefore measures the heal and **cannot** observe
    /// the anchors at all. That was this row's first form, and mutating
    /// `reconcile_bin`, `fingerprint_bin` and `prune_within` back to `bin()`
    /// each left it green.
    ///
    /// The state this row is about is the one the anchors exist for and the
    /// heal cannot rule out: `active` is a symlink, and **any process that can
    /// write the home can swing it at any instant** — the render lock is
    /// ocx's own convention and stops no one (R12). So the link is swung after
    /// the fixture's steps 2–6 and before the body, which is exactly where a
    /// concurrent repoint lands.
    ///
    /// RED: revert `reconcile_bin`'s binding, `write_render_stamp`'s
    /// `fingerprint_bin` argument, `artifact_path`'s trampoline arm or
    /// `prune_within`'s trampoline parent to `home.bin()`.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_render_writes_prunes_and_stamps_through_the_physical_directory() {
        let tree = Tree::new();
        tree.create_home_root();
        // A committed orphan, so the prune pass has something to act on too.
        std::fs::create_dir_all(tree.home.shell_bin(DEFAULT_SHELL)).unwrap();
        std::fs::write(tree.home.shell_bin(DEFAULT_SHELL).join("orphan"), b"#!/bin/sh\n").unwrap();

        let outside = tree.tmp.path().join("outside");
        std::fs::create_dir_all(outside.join("bin")).unwrap();
        std::fs::write(outside.join("bin").join("orphan"), b"not ours\n").unwrap();
        crate::symlink::replace_atomic(&outside, tree.home.active()).unwrap();
        assert!(
            !tree.home.active_is_valid(DEFAULT_SHELL),
            "precondition: `active` is swung outside the home, as a concurrent writer leaves it"
        );

        let surface = vec![node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])];
        let scope = tree.scope();
        let lock = lock_of(vec![locked_tool(
            "cmake",
            DEFAULT_GROUP,
            "ns/cmake",
            &[(PLATFORM_KEY, 'a')],
        )]);
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let report = render_with(
            &tree.file_structure,
            RenderRequest {
                home: &tree.home,
                scope: &scope,
                lock: &lock,
                surface: &surface,
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            },
            false,
            None,
        )
        .await
        .expect("the render succeeds");

        assert_eq!(
            tree.bin_entries(),
            expected_bin_entries(&["cmake"]),
            "the write and the prune both acted on the physical directory: {report:?}"
        );
        assert_eq!(
            read_dir_names(&outside.join("bin")),
            vec!["orphan".to_string()],
            "…and the attacker's directory was neither written into nor pruned"
        );
        assert_eq!(
            std::fs::read(outside.join("bin").join("orphan")).unwrap(),
            b"not ours\n",
            "…byte-identical, not merely present"
        );
        let stamp = tree.stamp().expect("the stamp exists");
        assert_eq!(
            stamp.bin_fingerprint.keys().cloned().collect::<Vec<_>>(),
            expected_bin_entries(&["cmake"]),
            "…and the stamp certifies what landed, never what `active` pointed at: {stamp:?}"
        );
    }

    /// C-080, corrupt-states row 22 — validity is **raw** equality with the
    /// derived target, never a containment or canonicalising test.
    ///
    /// `shells/../shells/default` is contained *and* resolves to exactly the
    /// right directory, and it is still not the value this tree writes. A
    /// containment test admits it; a canonicalising compare admits it; only raw
    /// equality repoints it.
    ///
    /// RED: swap `active_is_valid`'s `read_link` for `dunce::canonicalize`, or
    /// replace the equality with a `starts_with` containment check — the link
    /// is left as it is and the target assertion fires.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_active_that_resolves_correctly_by_a_different_spelling_is_still_repointed() {
        let tree = Tree::new();
        std::fs::create_dir_all(tree.shell_directory()).unwrap();
        crate::symlink::create(
            Path::new("shells").join("..").join("shells").join(DEFAULT_SHELL),
            tree.home.active(),
        )
        .unwrap();
        assert_eq!(
            std::fs::canonicalize(tree.home.active()).unwrap(),
            std::fs::canonicalize(tree.shell_directory()).unwrap(),
            "precondition: the planted spelling resolves to the right directory, so only a raw \
             comparison can reject it"
        );

        render_a_default_tool(&tree).await;

        assert_eq!(
            std::fs::read_link(tree.home.active()).unwrap(),
            tree.home.expected_active_target(DEFAULT_SHELL),
            "the raw target is the one this tree writes, not merely one that resolves there"
        );
    }

    /// C-081, row 4 (inverted from the corrupt-states matrix, which asserted
    /// the opposite) — a **real directory** at `active` is the `cp -rL` /
    /// Docker `COPY` / zip outcome, and it is healed: removed and replaced by
    /// the link. Its contents do **not** survive, because they are a copy of
    /// the tree this render is about to rewrite.
    ///
    /// RED: drop the `remove_dir_all` arm and refuse instead — `active` stays a
    /// directory and the link assertion fires. RED for the carve-out's bound:
    /// replace `remove_dir_all` with `remove_dir` — a populated copy fails
    /// `ENOTEMPTY` and the same assertion fires.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_real_directory_at_active_is_healed_rather_than_refused() {
        let tree = Tree::new();
        std::fs::create_dir_all(tree.home.active().join("bin")).unwrap();
        std::fs::write(tree.home.active().join("bin").join("stale"), b"copied\n").unwrap();

        render_a_default_tool(&tree).await;

        assert!(
            crate::symlink::is_link(&tree.home.active()),
            "the dereferenced copy is replaced by the link it was a copy of"
        );
        assert!(tree.home.active_is_valid(DEFAULT_SHELL), "…at its derived target");
        assert_eq!(
            tree.bin_entries(),
            expected_bin_entries(&["cmake"]),
            "…and the physical directory holds exactly this render's output"
        );
    }

    /// C-081, row 5 — `active` occupied by a **regular file**, or by any other
    /// non-directory, is cleared and re-created.
    ///
    /// **Both plants take the same arm, and neither discriminates the
    /// primitive.** On POSIX `rename(2)` replaces a regular file and a FIFO
    /// alike, so a heal that reached `symlink::replace_atomic` unconditionally
    /// leaves a finished tree byte-identical to this one — the occupant's
    /// bytes are gone either way, and no assertion over the result can tell
    /// the two apart. The FIFO is planted because it is the second corrupt
    /// state a user can reach, not because it observes the arm. What this row
    /// pins is that the occupant is cleared **before** the link is published,
    /// which is the whole of the non-directory arm's job.
    ///
    /// RED: delete the `remove_file` from `heal_active`'s non-directory branch
    /// and fall straight through to `symlink::create` — `symlink(2)` fails
    /// `EEXIST` against the occupant, the heal only warns, and `active` stays
    /// a file (or a FIFO) on both legs.
    #[cfg(unix)]
    #[tokio::test]
    async fn active_occupied_by_a_non_directory_is_removed_and_re_created() {
        for plant in ["file", "fifo"] {
            let tree = Tree::new();
            std::fs::create_dir_all(tree.home.root()).unwrap();
            if plant == "file" {
                std::fs::write(tree.home.active(), b"not a link\n").unwrap();
            } else {
                mkfifo(&tree.home.active());
            }

            render_a_default_tool(&tree).await;

            assert!(
                crate::symlink::is_link(&tree.home.active()),
                "a {plant} at `active` is removed and replaced by the link"
            );
            assert!(
                tree.home.active_is_valid(DEFAULT_SHELL),
                "…at its derived target ({plant})"
            );
        }
    }

    /// C-081, corrupt-states row 28 — a **self-referential** `active` is
    /// repointed, and the call returns.
    ///
    /// RED: make the heal canonicalise the existing link before deciding —
    /// `ELOOP` surfaces and the render never finishes the step. RED for the
    /// two-step cycle: an equality check against the link's own path catches
    /// `active -> active` and sails straight past `active -> b`, `b -> active`,
    /// which is why both are planted.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_cyclic_active_is_repointed_without_hanging() {
        for cycle in ["self", "two-step"] {
            let tree = Tree::new();
            std::fs::create_dir_all(tree.home.root()).unwrap();
            if cycle == "self" {
                crate::symlink::create("active", tree.home.active()).unwrap();
            } else {
                crate::symlink::create("b", tree.home.active()).unwrap();
                crate::symlink::create("active", tree.home.root().join("b")).unwrap();
            }

            render_a_default_tool(&tree).await;

            assert!(
                tree.home.active_is_valid(DEFAULT_SHELL),
                "the {cycle} cycle is repointed at the derived target"
            );
        }
    }

    /// C-083, corrupt-states row 31 — the shell directory is created **before**
    /// `active`, so an interrupted render never leaves a link to a directory
    /// that does not exist.
    ///
    /// A lookup through an absent `active` is a **miss**; a lookup through a
    /// link into nothing is an error every reader then has to be careful to
    /// read as a mismatch. The order is the whole guarantee.
    ///
    /// **Driven through the fault seam, because the finished tree cannot show
    /// it.** With both writes landed the tree is byte-identical either way —
    /// the swapped order publishes a dangling link and then creates its target
    /// — so an assertion over the finished state passes with the calls
    /// reversed. Measured: it did.
    ///
    /// RED: swap the two calls in step 6 — `active` is present, and dangling,
    /// at the abort.
    #[tokio::test]
    async fn an_interrupted_render_never_leaves_active_pointing_at_an_absent_shell() {
        let tree = Tree::new();
        let _lock = crate::test::env::lock();
        // SAFETY: nextest gives every test its own process, and `EnvLock`
        // serialises this write against every test that goes through
        // `crate::test::env`. `read_fault_hook` reads `std::env::var_os`, which
        // the override table does not reach, so the variable has to be real.
        unsafe { std::env::set_var("__OCX_TESTING_RENDER_FAULT", FAULT_AFTER_SHELL_TREE) };

        let lock = lock_of(vec![locked_tool(
            "cmake",
            DEFAULT_GROUP,
            "ns/cmake",
            &[(PLATFORM_KEY, 'a')],
        )]);
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        let outcome = tree
            .manager()
            .render_toolchain(RenderRequest {
                home: &tree.home,
                scope: &tree.scope(),
                lock: &lock,
                surface: &[node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            })
            .await;
        // SAFETY: as above.
        unsafe { std::env::remove_var("__OCX_TESTING_RENDER_FAULT") };

        assert!(outcome.is_err(), "precondition: the seam aborted the render");
        assert!(
            tree.shell_directory().is_dir(),
            "the shell directory had already landed at the abort: {:?}",
            read_dir_names(tree.home.root())
        );
        assert!(
            tree.home.active().symlink_metadata().is_err(),
            "…and `active` had not — an absent link is a miss, a dangling one is not"
        );
    }

    /// One render of one default-group tool, through the **public** entry, so
    /// step 6 runs. Returns the report.
    async fn render_a_default_tool(tree: &Tree) -> RenderReport {
        let lock = lock_of(vec![locked_tool(
            "cmake",
            DEFAULT_GROUP,
            "ns/cmake",
            &[(PLATFORM_KEY, 'a')],
        )]);
        let groups = groups_of(&[DEFAULT_GROUP]);
        let platform = platform();
        tree.manager()
            .render_toolchain(RenderRequest {
                home: &tree.home,
                scope: &tree.scope(),
                lock: &lock,
                surface: &[node(pinned("ns/cmake", 'a'), Some(&["cmake"]), &[])],
                groups: &groups,
                pinned: false,
                platform: &platform,
                dry_run: false,
            })
            .await
            .expect("the render succeeds")
    }

    fn root_entry(name: &str) -> RenderedArtifact {
        RenderedArtifact::RootEntry(name.to_string())
    }
}
