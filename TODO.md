## Next Steps

These are ordered by dependency — each group should be completed before the next.

### 1. File structure finish-up ✓
- [x] Tests for `ObjectStore` (path sharding, content, metadata, metadata_for_content, refs_dir_for_content, list_all)
- [x] Tests for `IndexStore` (tags, manifest, blob, list_repositories)
- [x] Tests for `InstallStore` (current, candidate, candidates, symlink kinds)

### 2. Reference manager wiring ✓
- [x] `task/package/install.rs` — replace `symlink::update()` with `ReferenceManager::link()` for candidate and current symlinks
- [x] `command/select.rs` — replace `symlink::update()` with `ReferenceManager::link()` for current symlink
- [x] Add doc note to `symlink::update` and `symlink::create` warning to use `ReferenceManager` for package install symlinks (so back-refs are maintained)

### 3. Reverse operations ✓
- [x] `deselect` command — remove current symlink via `ReferenceManager::unlink()`; report which packages were deselected
- [x] `uninstall` command — unlink candidate (and optionally current) via `ReferenceManager::unlink()`; if `refs/` becomes empty, offer to remove object from store
- [x] `clean` command — remove unreferenced objects from store, with `--dry-run` option to report what would be removed without actually removing it

### 4. Documentation
- [ ] User guide `## File Structure` section - explain intend, and reference all cli commands that work on the index.
- [ ] All new commands should have a secion in the command line reference.
- [ ] `--current` tag-not-validated caveat: with `--current` the tag portion of the identifier is silently ignored (only registry+repo are used to locate the symlink). The note currently lives in `options/content_path.rs`. Verify it surfaces in the `--help` output of every command that accepts `ContentPath` (`find`, `env`, `shell env`) and decide whether a `log::warn!` at runtime is warranted.

### N. Spec (side note)
- [ ] Internal architecture spec: explain OCI-as-package-store design, local store layout, index vs object store, symlink/ref model, Windows notes, future layered storage

---

## Backlog

 - ~~env command~~
 - ~~select command~~
 - ~~retry push, if manifest unknown~~ (fixed: manifest passed through from push_package, no re-fetch)
 - ~~env command path is resolved correct but metadata may be picked from a different install~~
 - ~~file structure refactoring (ObjectStore / IndexStore / InstallStore composite)~~
 - ~~link/unlink (→ done in §2 + §3)~~
 - `link`/`unlink` commands — GC-aware user-managed symlinks: `ocx link <PACKAGE> <PATH>` creates a symlink at an arbitrary user-provided filesystem path pointing to the package's content dir, with a back-reference registered in `refs/` so `clean` does not GC the object; `ocx unlink <PATH>` removes the symlink and its back-ref; deferred until there is a concrete use case (most users are served by `candidates/{tag}` and `current`)
 - move tasks to library with CLI as thin wrapper
 - process refs, a process can lock a package to prevent deletion, especiall if the program does not require a ref.
 - symlink env/profile
  - should support common files and maybe search if a specific comment is present? To not add the same again? Or garbage in garbage out?
 - layered storage for cached packages
 - more robust cascade, that is platform-aware (ie. migrating published platform and compute rolling releases relative to the same platform)
 - auto-index of unknown packages
 - target platform specific symlink structure
 - reconsider install lock and atomicity of installs. maybe temporary directory and rename on success, or symlink to a temporary directory and then rename the symlink on success?
 - document required windows developer mode for symlinks
 - refactor oci reference
 - api reports with , seperated tags and platforms
 - composite errors, ie. uninstall_all should not fail on first error, incl. other commands
 - local installed catalog (enabled by `ObjectStore::list_all()`)

## Long Term

 - gitlab steps for consuming ocx packages
 - make the error handling more robust and user-friendly, ie. if a symlink to uninstall is not existent, report that instead of just failing with a broken symlink error
 - bazel rule for consuming ocx packages
 - dependencies and lockfiles
 - multiple layer
 - mirror cli for github/gitlab releases, cargo crates, npm packages, etc.
 - sbom and signature
 - shims & lazy loading
 - infrastructure extensions

## Quality of Life

 - advanced log settings with layers and filters
 - progress bars for long operations
 - get urls from manifests not supported atm, breaking SHA?
 - mirrors config
 - homebrew style auto codesigning on macOS, incl. user-friendly error messages when it fails and docs on how to fix it
 - make rustdoc type links less verbose (e.g. `ocx_lib::reference_manager::ReferenceManager` → `ReferenceManager`)
 - modern ui components for website

## Ideas

 - testing packages

