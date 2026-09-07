// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Resolving a **project's** toolchain home root (C-002, D-V10).
//!
//! The tree's grammar is not here: [`ToolchainHome`] and its validation live
//! in [`file_structure`](crate::file_structure), because `project::consent`
//! already reads [`StateStore`](crate::file_structure::StateStore) and the
//! reverse import would close a `use` cycle on the crate's foundational
//! layer. That cycle would still *compile* — Rust module graphs may be cyclic
//! within one crate — so the reason is layering plus the planned `ocx_lib`
//! split, where a `use` cycle does not compile across a crate boundary
//! ([ocx-sh/ocx#313](https://github.com/ocx-sh/ocx/issues/313)); the qualifier
//! matters so a reader still recognises a genuine cross-crate cycle when they
//! meet one. What *is* project domain is the keying — which project a home
//! belongs to, and the file-first canonicalisation that answers it — so that
//! is what this module owns.

use std::path::Path;

use crate::config::ToolchainRoot;
use crate::file_structure::ToolchainHome;
use crate::reference_manager::ReferenceManager;

/// Resolve the toolchain home for the project rooted at `project_dir`
/// (C-002, D-V10).
///
/// - `toolchain_root` absent → `<project_dir>/.ocx/toolchain`.
/// - `toolchain_root` present → `<root>/<project-key>/toolchain`, where
///   `<project-key>` is
///   [`ReferenceManager::name_for_path`](crate::reference_manager::ReferenceManager::name_for_path)
///   over the canonical project **directory** — the same 16-hex key the
///   `projects/` GC ledger and `state/projects/<key>/` already use.
///
/// The global home is not resolved here: it is
/// [`FileStructure::toolchain`](crate::file_structure::FileStructure), a field
/// built once in `with_root`, and it ignores `toolchain-dir` entirely (C-016).
///
/// # Why this takes [`ToolchainRoot`] and not a path or a `&Config` (D-V10, R-W20)
///
/// Taking the whole `Config` would ship a branch that is permanently `None` for
/// as long as no tier declares one, and is wider than this needs: one root,
/// never the whole config.
///
/// Taking a bare `Option<&Path>` is the shape R-W20 records as a **compile-legal
/// bypass**: `Config::toolchain_dir()` yields exactly that, so feeding one
/// straight to the other type-checked and skipped every C-017–C-019 refusal.
/// [`ToolchainRoot`] is constructible only by [`ToolchainRoot::resolve`], so an
/// unvalidated path is now unspellable here — the funnel is closed by the type,
/// not by a doc comment.
///
/// # Precondition on `project_dir` (D-V13)
///
/// `project_dir` must be the output of the shipped **file-first**
/// canonicalisation — canonicalize the project *file*, take `.parent()`, then
/// `dunce::canonicalize` — i.e. exactly what
/// [`consent::canonical_project_dir`](crate::project::consent::canonical_project_dir)
/// produces. Canonicalizing the directory instead is not the same derivation:
/// a symlinked `ocx.toml` would key the home under the victim directory rather
/// than under the file's real one, which is the defect that derivation exists
/// to close, and on Windows the two spellings do not even produce the same
/// string.
///
/// The implementation canonicalizes defensively anyway, so it is idempotent on
/// an already-canonical input; the precondition is what makes the key **equal**
/// to the consent key rather than merely well-formed, and a unit test asserts
/// that parity.
///
/// Blocking: one filesystem resolution. Async callers wrap it in
/// `spawn_blocking` — the same note, for the same reason, as
/// [`consent::canonical_project_dir`](crate::project::consent::canonical_project_dir),
/// which `activation.rs` wraps *because* the note is there.
///
/// # Errors
///
/// The canonicalisation's own I/O failure, with the offending path attached.
pub fn resolve_toolchain_home(
    project_dir: &Path,
    toolchain_root: Option<&ToolchainRoot>,
) -> crate::Result<ToolchainHome> {
    // `dunce::canonicalize`, never bare `std::fs::canonicalize`: the latter
    // yields a `\\?\` verbatim path on Windows, whose bytes hash to a different
    // key than every shipped producer's — the consent stamp and the `projects/`
    // ledger both key on the `dunce` form. Canonicalizing at all is what makes
    // `/w/proj` and `/w/proj/`, and a symlinked checkout and its target, resolve
    // to one home; `name_for_path` hashes raw path bytes and would key each
    // spelling separately.
    let canonical = dunce::canonicalize(project_dir).map_err(|e| crate::error::file_error(project_dir, e))?;

    let root = match toolchain_root {
        None => canonical.join(".ocx").join("toolchain"),
        // `<key>` first and `toolchain` second, never the reverse: the reverse
        // order would land every project's tree inside a directory
        // indistinguishable from a group directory (R-W1's data-loss path).
        Some(root) => root
            .as_path()
            .join(ReferenceManager::name_for_path(&canonical))
            .join("toolchain"),
    };
    Ok(ToolchainHome::new(root))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::resolve_toolchain_home;
    use crate::config::ToolchainRoot;
    use crate::reference_manager::ReferenceManager;

    /// A scratch `toolchain-dir` root, taken as already validated.
    ///
    /// `ToolchainRoot::resolve` refuses anything outside `$HOME`/`$OCX_HOME`, so
    /// no `tempfile` directory can travel through the real funnel, and its
    /// anchor-injecting sibling is private to `crate::config`. The `#[cfg(test)]`
    /// constructor exists for exactly this, and the containment rules it skips
    /// have their own tests in `config.rs`.
    fn validated(root: &Path) -> ToolchainRoot {
        ToolchainRoot::from_validated(root)
    }

    /// A canonical scratch directory. `tempfile` hands back a path under `/tmp`,
    /// which is itself a symlink on macOS, so every expected value in this
    /// module is derived from the canonical form rather than from `tmp.path()`.
    fn canonical_scratch(tmp: &tempfile::TempDir) -> PathBuf {
        dunce::canonicalize(tmp.path()).expect("the scratch directory must canonicalize")
    }

    /// C-002 — with no `toolchain-dir` configured, a project's home is
    /// `.ocx/toolchain` beside its own `ocx.toml`.
    #[test]
    fn an_absent_toolchain_dir_resolves_to_dot_ocx_toolchain_inside_the_project() {
        let tmp = tempfile::tempdir().unwrap();
        let project = canonical_scratch(&tmp).join("proj");
        std::fs::create_dir_all(&project).unwrap();

        let home = resolve_toolchain_home(&project, None).expect("the default branch must resolve");
        assert_eq!(
            home.root(),
            project.join(".ocx").join("toolchain"),
            "C-002 — the default home is `<project>/.ocx/toolchain`"
        );
    }

    /// C-002 — with a `toolchain-dir` root configured, the home is
    /// `<root>/<project-key>/toolchain`.
    ///
    /// The component-order assertion is the discriminator: `<root>/toolchain/<key>`
    /// would land the project trees *inside* a directory indistinguishable from a
    /// group directory, which is the shape R-W1 records as a data-loss path.
    #[test]
    fn a_configured_toolchain_dir_keys_the_home_by_project_under_the_root() {
        let tmp = tempfile::tempdir().unwrap();
        let scratch = canonical_scratch(&tmp);
        let project = scratch.join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let root = scratch.join("toolchain-root");
        std::fs::create_dir_all(&root).unwrap();

        let key = ReferenceManager::name_for_path(&project);
        let home =
            resolve_toolchain_home(&project, Some(&validated(&root))).expect("the configured branch must resolve");

        assert_eq!(
            home.root(),
            root.join(&key).join("toolchain"),
            "C-002 — the configured home is `<root>/<project-key>/toolchain`"
        );

        let tail: Vec<_> = home
            .root()
            .strip_prefix(&root)
            .expect("the home must stay under the configured root")
            .components()
            .collect();
        assert_eq!(
            tail.len(),
            2,
            "expected `<project-key>/toolchain`, got {:?}",
            home.root()
        );
        assert_eq!(
            tail[0].as_os_str(),
            std::ffi::OsStr::new(key.as_str()),
            "C-002 — the project key comes first and `toolchain` second, never the reverse"
        );
    }

    /// C-002 — the `<project-key>` is `name_for_path` over the **canonical**
    /// directory, not over the spelling the caller passed.
    ///
    /// The project is built under a symlink so the two spellings genuinely
    /// differ; the `assert_ne!` is the non-vacuity guard, because
    /// `name_for_path` hashes the raw path bytes and an already-canonical input
    /// would make `name_for_path(canonical)` and `name_for_path(as_given)`
    /// indistinguishable — a green either way.
    #[cfg(unix)]
    #[test]
    fn the_project_key_is_derived_from_the_canonical_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let scratch = canonical_scratch(&tmp);
        let real = scratch.join("real-project");
        std::fs::create_dir_all(&real).unwrap();
        let via_link = scratch.join("link-to-project");
        std::os::unix::fs::symlink(&real, &via_link).unwrap();
        let root = scratch.join("toolchain-root");
        std::fs::create_dir_all(&root).unwrap();

        let canonical_key = ReferenceManager::name_for_path(&real);
        let as_given_key = ReferenceManager::name_for_path(&via_link);
        assert_ne!(
            canonical_key, as_given_key,
            "the two spellings must differ, or this test cannot tell the canonical derivation from the as-given one"
        );

        let home =
            resolve_toolchain_home(&via_link, Some(&validated(&root))).expect("a symlinked project must resolve");
        assert_eq!(
            home.root(),
            root.join(&canonical_key).join("toolchain"),
            "C-002 — the key is over the canonical project directory"
        );

        let default_home = resolve_toolchain_home(&via_link, None).expect("the default branch must resolve too");
        assert_eq!(
            default_home.root(),
            real.join(".ocx").join("toolchain"),
            "C-002 — the default branch resolves under the canonical directory as well"
        );
    }

    /// C-002, D-V13 — the toolchain key **equals** the key the shipped consent
    /// stamp derives for the same project.
    ///
    /// This is the test D-V13's precondition exists for: `project_dir` is
    /// contractually the output of `consent::canonical_project_dir` — canonicalize
    /// the project *file*, take `.parent()`, then `dunce::canonicalize` — so the
    /// two keys must be one value, not merely two well-formed 16-hex strings.
    #[test]
    fn the_toolchain_key_equals_the_shipped_consent_key() {
        let tmp = tempfile::tempdir().unwrap();
        let scratch = canonical_scratch(&tmp);
        let project = scratch.join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let project_file = project.join("ocx.toml");
        std::fs::write(&project_file, b"").unwrap();
        let root = scratch.join("toolchain-root");
        std::fs::create_dir_all(&root).unwrap();

        let consent_dir = crate::project::consent::canonical_project_dir(&project_file)
            .expect("the shipped file-first derivation must resolve");
        let consent_key = ReferenceManager::name_for_path(&consent_dir);

        let home =
            resolve_toolchain_home(&consent_dir, Some(&validated(&root))).expect("the configured branch must resolve");
        assert_eq!(
            home.root(),
            root.join(&consent_key).join("toolchain"),
            "D-V13 — the toolchain home must key on exactly the consent stamp's key"
        );
    }

    /// C-002 — two spellings of one directory resolve to one home.
    ///
    /// The `assert_ne!` is the non-vacuity guard: `Path` equality ignores a
    /// trailing separator, but `name_for_path` hashes the raw bytes, so without
    /// canonicalisation the trailing-slash spelling keys a *different* home.
    #[test]
    fn a_trailing_separator_resolves_to_the_same_home() {
        let tmp = tempfile::tempdir().unwrap();
        let scratch = canonical_scratch(&tmp);
        let project = scratch.join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let root = scratch.join("toolchain-root");
        std::fs::create_dir_all(&root).unwrap();

        let mut spelled_with_separator = project.clone().into_os_string();
        spelled_with_separator.push(std::path::MAIN_SEPARATOR_STR);
        let spelled_with_separator = PathBuf::from(spelled_with_separator);

        assert_ne!(
            ReferenceManager::name_for_path(&project),
            ReferenceManager::name_for_path(&spelled_with_separator),
            "the raw-byte keys must differ, or this test cannot observe the canonicalisation"
        );

        assert_eq!(
            resolve_toolchain_home(&project, Some(&validated(&root))).expect("the bare spelling must resolve"),
            resolve_toolchain_home(&spelled_with_separator, Some(&validated(&root)))
                .expect("the trailing-separator spelling must resolve"),
            "C-002 — `/w/proj` and `/w/proj/` are one project and must give one home"
        );
        assert_eq!(
            resolve_toolchain_home(&project, None).expect("the bare spelling must resolve"),
            resolve_toolchain_home(&spelled_with_separator, None)
                .expect("the trailing-separator spelling must resolve"),
            "C-002 — the default branch must agree on the two spellings too"
        );
    }

    /// C-002 — a relative `project_dir` resolves through the canonical directory
    /// it names, so the home does not depend on how the path was spelled.
    ///
    /// The stub's contract does not refuse a relative input; it canonicalizes
    /// defensively. So the property asserted here is the canonicalisation, not
    /// working-directory independence in the absolute sense — a resolved home is
    /// anchored on the directory the relative path names *from this process's*
    /// working directory. A genuine two-working-directory experiment is not
    /// writable as a unit test here: `std::env::set_current_dir` is process-wide
    /// mutable state and nextest runs these tests as threads in one process, so
    /// mutating it would make every path-resolving test in the suite
    /// order-dependent. The probe below therefore *reads* the working directory
    /// and never writes it.
    #[cfg(unix)]
    #[test]
    fn a_relative_project_directory_resolves_through_the_canonical_directory_it_names() {
        let tmp = tempfile::tempdir().unwrap();
        let scratch = canonical_scratch(&tmp);
        let project = scratch.join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let root = scratch.join("toolchain-root");
        std::fs::create_dir_all(&root).unwrap();

        // Imported here rather than at module scope: this is the only user,
        // and the test is `#[cfg(unix)]`, so a module-scope import is an
        // `unused_import` error on Windows under the workspace's denied
        // warnings.
        use std::path::Component;

        let working_directory = std::env::current_dir().expect("the working directory must be readable");
        let mut relative = PathBuf::new();
        for component in working_directory.components() {
            if matches!(component, Component::Normal(_)) {
                relative.push("..");
            }
        }
        for component in project.components() {
            if let Component::Normal(name) = component {
                relative.push(name);
            }
        }
        assert!(relative.is_relative(), "the constructed probe must be a relative path");

        assert_eq!(
            resolve_toolchain_home(&relative, Some(&validated(&root)))
                .expect("a relative project directory must resolve"),
            resolve_toolchain_home(&project, Some(&validated(&root))).expect("the canonical spelling must resolve"),
            "C-002 — a relative spelling must resolve to the same home as the canonical one it names"
        );
    }

    /// C-002 — a `project_dir` that does not exist is an error: the
    /// canonicalisation's own I/O failure, with the offending path attached.
    ///
    /// The configured branch is unambiguous — the key cannot be derived without
    /// a real directory. The default branch is asserted from the stub's
    /// "canonicalizes defensively anyway" clause rather than from a per-branch
    /// statement; flagged as a design gap in the plan.
    #[test]
    fn a_project_directory_that_does_not_exist_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let scratch = canonical_scratch(&tmp);
        let absent = scratch.join("no-such-project");
        let root = scratch.join("toolchain-root");
        std::fs::create_dir_all(&root).unwrap();

        assert!(
            resolve_toolchain_home(&absent, Some(&validated(&root))).is_err(),
            "C-002 — an absent project directory cannot yield a project key"
        );
        assert!(
            resolve_toolchain_home(&absent, None).is_err(),
            "C-002 — the default branch canonicalizes defensively, so it errors too"
        );
    }
}
