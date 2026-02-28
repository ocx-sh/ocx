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

### 3. Reverse operations
- [ ] `deselect` command — remove current symlink via `ReferenceManager::unlink()`; report which packages were deselected
- [ ] `uninstall` command — unlink candidate (and optionally current) via `ReferenceManager::unlink()`; if `refs/` becomes empty, offer to remove object from store

### 3. Reverse operations
 - [ ] `clean` command — remove unreferenced objects from store, with `--dry-run` option to report what would be removed without actually removing it

### 5. Documentation
- [ ] User guide `## File Structure` section - explain intend, and reference all cli commands that work on the index.
- [ ] User guide `## File Structure` section — ASCII diagram of `~/.ocx/` layout, explain objects vs index vs installs, explain symlinks

### N. Spec (side note)
- [ ] Internal architecture spec: explain OCI-as-package-store design, local store layout, index vs object store, symlink/ref model, Windows notes, future layered storage

---

## Backlog

 - ~~env command~~
 - ~~select command~~
 - ~~retry push, if manifest unknown~~ (fixed: manifest passed through from push_package, no re-fetch)
 - ~~env command path is resolved correct but metadata may be picked from a different install~~
 - ~~file structure refactoring (ObjectStore / IndexStore / InstallStore composite)~~
 - link/unlink (→ see Next Steps §2)
 - symlink env/profile
 - layered storage for cached packages
 - more robust cascade, that is platform-aware (ie. migrating published platform and compute rolling releases relative to the same platform)
 - auto-index of unknown packages
 - target platform specific symlink structure
 - reconsider install lock and atomicity of installs. maybe temporary directory and rename on success, or symlink to a temporary directory and then rename the symlink on success?
 - document required windows developer mode for symlinks
 - refactor oci reference
 - local installed catalog (enabled by `ObjectStore::list_all()`)

## Long Term

 - gitlab steps for consuming ocx packages
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

## Ideas

 - testing packages
