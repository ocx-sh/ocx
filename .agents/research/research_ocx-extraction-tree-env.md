# Research: ocx extraction, tree shape and env reporting

## Metadata

**Date:** 2026-11-18
**Lane:** codebase recon (`/hex-discuss`, `.agents/discussions/bazel-hermetic-package-bundles.md`)
**Basis:** ocx worktree `uno` at 5f1f5962 and the local `~/.ocx` store; no shell was available to the lane, so no file counts
**Expires:** 2027-05-18
**Caveat:** findings only, no recommendation; items marked UNVERIFIED had no primary source read.

All paths are relative to `/home/mherwig/.herdr/worktrees/ocx/uno` unless they start with `~/.ocx`.

### Findings

**1. Extraction determinism**
- **Layer extraction:** `tar` 0.4.46 sets mtime from the archive header (default `preserve_mtime: true`). It does not apply ownership (`preserve_ownerships: false`) and does not unpack xattrs (`unpack_xattrs: false`) (`~/.cargo/registry/.../tar-0.4.46/src/archive.rs:59-62`). ocx turns on `set_preserve_permissions(true)` (`crates/ocx_util/src/archive/tar.rs:183`).
- **Mode masking:** every file and directory mode is masked with `& !(0o7000|0o022)` (tar.rs:284). Implicit parent directories go through `create_dir_all_capped` (tar.rs:243). The mask is tested under umask 000 (tar.rs:1376-1424). I did not verify how a restrictive umask (e.g. 077) affects the implicitly created directories.
- **mtimes and ownership:** file mtimes come from the archive. Directory mtimes do not: implicit parents and assembled dirs get local creation time. `assemble_from_layers_with_layouts` creates real subdirs (assemble.rs:4, `dirs_created`) and hardlinks files from the layer store (`subsystem-file-structure.md:394`). Ownership is whoever runs the process.
- **Symlinks:** an absolute symlink target is always refused at extraction (`crates/ocx_util/src/fs/symlink.rs:12-21`), so layer symlinks are relative. Escapes are re-checked by a post-pass `sweep_symlinks` (tar.rs:305). Assembly recreates symlinks verbatim (subsystem-file-structure.md:394).
- **Hardlinks:** tar `Link` entries are resolved inside the extraction root with `fs::hard_link` (tar.rs:255-266).
- **Ordering and collisions:** the assembly walker is parallel and collisions fail closed (assemble.rs:62-81). On a case-insensitive filesystem a case-only overlap surfaces as `AlreadyExists` (assemble.rs:137).
- **Windows:** layer symlinks return `Unsupported` in assembly (assemble.rs:65). Extraction makes junctions, whose targets are absolute (symlink.rs:310; tar.rs tests ~1189).
- **macOS codesign:** runs once per layer, before the layer is finalised (`crates/ocx_package_manager/src/tasks/pull.rs:861`). It does two things (`crates/ocx_store/src/codesign.rs`):
  - Removes quarantine via `/usr/bin/xattr` (codesign.rs:29, 177).
  - Ad-hoc signs each Mach-O file whose signature does not verify (codesign.rs:10, 219). It signs a copy on a fresh inode and renames it back (267-285), and re-links hardlink aliases (47-77).
  - `OCX_NO_CODESIGN` disables it (codesign.rs:19). Failures only warn.
  - This rewrites Mach-O bytes inside `content/`. I did not check whether the signature bytes are deterministic.
- **Machine-specific data inside `content/`:** I found none other than the macOS signature bytes, directory mtimes and ownership. Absolute paths are baked only outside `content/`, in `entrypoints/` launchers (item 5).

**2. Package tree shape**
- **Layout** (`subsystem-file-structure.md:134-138`): `packages/<registry>/<algo>/<2hex>/<30hex>/` holds `content/`, `metadata.json`, `manifest.json`, `resolve.json`, `install.json`, `digest`, `refs/{symlinks,deps,layers,blobs,origins,verified}`, and `entrypoints/` (`package_store.rs:121`).
- **`content/`:** a real directory, not a symlink into `layers/` (subsystem-file-structure.md:394).
- **Symlinks, all absolute store paths:**
  - `refs/deps/<algo>_<32hex>` targets the dependency's `content/` (pull.rs:875-894).
  - `refs/layers/...` targets a layer's `content/` (pull.rs:898-918).
  - `refs/blobs/...` targets blob `data` files.
  - `refs/symlinks/` holds back-refs.
  - `refs/origins/` entries are regular files.
- **`install.json`:** carries a timestamp (`~/.ocx/packages/ocx.sh/sha256/85/3a43…/install.json`: `{"timestamp": "2026-11-01T…","ok": true}`). It is a sibling of `content/`, not inside it.
- **Real example (ocx itself):** `metadata.json` is `env: PATH path "${installPath}/bin" visibility public` plus `binaries: ["ocx"]`. `resolve.json` is `{"dependencies": []}`. `content/` is `bin/ocx`, `LICENSE`, `LICENSE-THIRD-PARTY.md`.

**3. Real numbers from `~/.ocx`**
I had no shell tool. Only Glob was available, so there is no stat, du, link count or size data. Glob also does not list symlinks: the ocx package's `refs/layers` entry is absent from its listing.

| Package | Files in `content/` (Glob) | Dirs | Symlinks | Hardlink-shared | Size |
|---|---|---|---|---|---|
| ocx (`85/3a43…`) | 3 | not measurable | not measurable | not measurable | not measurable |
| whole `~/.ocx` | 55,986 files, layers and packages together (Glob header) | – | – | – | – |

The node layer (`layers/ocx.sh/sha256/37/5e0a…`) is large: it contains `lib/node_modules/npm/man/...`. It shows the tree is dominated by many small files.

**4. Environment reporting**
- **Tokens:** `${installPath}`, `${self.installPath}` (alias), `${self.env.KEY}` and `${deps.NAME.installPath}`. Each may take a `:native` or `:posix` suffix (`crates/ocx_package/src/metadata/template/scanner.rs:10, 35, 44-55, 124-132`). `$${` is the escape.
- **Expansion:** `EnvResolver::resolve` expands against a fixed `install_path`. A relative `path` value is joined under it (`metadata/env/resolver.rs:84, 98`).
- **Dependency paths:** `${deps.*}` contexts are built from `paths.install_path_for(..., PathLane::Digest)`, i.e. the digest store path (`crates/ocx_package_manager/src/composer.rs:299, 495-498`).
- **JSON entry:** `EnvEntry { key, value, kind, separator?, source? }` (`crates/ocx_cli/src/api/data/env.rs:52-66`). `source` appears only under `--show-patches`.
- **No unexpanded mode:** `package env` flags are `--self`, `--env`, platform, content-path, `--lazy-mode`, `--shell`, `--ci`, `--export-file`, `--show-patches` (`crates/ocx_cli/src/command/env.rs:35-108`). The raw templates are readable in `metadata.json`.
- **Different root, partial:**
  - `--candidate`, `--current` and `--link PATH` select `Materialization::Symlink(LinkSource)`, rooting the package's own values in an install link (`options/content_path.rs:51-64`, `command/env.rs:112-119`, `composer.rs:1000-1013`).
  - `--link PATH` is written by `ocx package install --link PATH` (`command/install.rs:20-26`). The caller picks the path, but it must be a link ocx wrote.
  - Dependency contexts still use the Digest lane (composer.rs:299, 495). I did not verify whether Symlink mode changes dependency paths.
  - The toolchain `PathLane::Following` swaps in trusted `links/<group>/<entry>` (composer.rs:885-893).

**5. Entry points**
- **Metadata:** `entrypoints` is a map `name → {command?, args[]}`. `args` may use only `${installPath}`; `${deps.*}` is rejected (`crates/ocx_package/src/metadata/entrypoint.rs:93-129`).
- **POSIX launcher:** `#!/bin/sh` then `exec "${OCX_BINARY_PIN:-ocx}" launcher exec '<pkg_root>' -- "${0##*/}" "$@"` (`crates/ocx_package_manager/src/launcher/body.rs:18-20`). It bakes only the absolute package root (generate.rs:37).
- **Windows:** `<name>.exe` plus a `<name>.shim` sidecar carrying the root (generate.rs:53-63).
- **`launcher exec`** (`crates/ocx_cli/src/command/launcher/exec.rs`):
  - It requires the root to be absolute, canonically under `$OCX_HOME/packages` or the two `temp` scratch roots, and to contain `metadata.json` (367-418).
  - It composes the self-view env with `self_view=true` (107-122), resolves `command` on the composed PATH (250), and substitutes baked args (153-160).
- **Static from metadata alone:** name→command and args, yes. The target binary path, no: it is found at run time on the PATH composed from the env of the whole closure.

**6. Closest primitives**
- **`package pull`** (`command/package_pull.rs:38-45`): `pull_all(..., false)` creates no candidate or current symlink. It reports the package root. It still writes layers, blobs, the package tree, `refs/origins`, and index/dispatch objects under the home per the resolve policy.
- **`package install`:** adds the candidate symlink, plus `--link PATH` and `-s`.
- **`pull_local(info, layers, dest_override)`** (`tasks/pull_local.rs:66-81`): only the root lands at `dest_override`. It must be on the same filesystem as `$OCX_HOME/layers/`, because assembly hardlinks. Dependencies still go to `$OCX_HOME/packages/`. Entrypoint launchers bake the override root. The only non-test callers I found are in `pull.rs` and `pull_local.rs`; no CLI flag.
- **Home override:** only the `OCX_HOME` env var (`crates/ocx_env/src/vars.rs:28`). I found no `--home` flag (`app/context_options.rs` mentions only `--index`).
- **Shared home:** temp-dir lock plus wait (`pull.rs:529-557`), layer dedup by singleflight, locks under `$OCX_HOME/locks`. The home must be a single volume (`subsystem-file-structure.md:392`).
- **Unshared home:** each home re-downloads and re-extracts.

**7. Dependencies**
- **Representation:** `metadata.json` holds `dependencies: [{identifier: PinnedPackageRef (registry + digest required), visibility, name?}]` (`crates/ocx_package/src/metadata/dependency.rs:105-122`).
- **Closure is fixed by the root digest:** the config blob is addressed from the manifest, so the root digest fixes every direct dependency digest, transitively. `resolve.json` mirrors the resolved list with advisory tags (pull.rs:604-616).
- **Platform:** `ocx package create` collapses each dependency to a platform manifest digest for the single declared platform (`subsystem-package.md`, `dependency_pinning.rs` row). The doc comment still allows an image-index digest (dependency.rs:101-103). Dependencies are pulled with the parent's platform (`setup_dependencies`, pull.rs:566-586). I did not verify how a dependency that names an index digest is resolved.

### negative:
- No shell, so no `find`, `stat`, `du`, symlink or hardlink counts for item 3.
- No `--home` flag and no per-invocation home override beyond `OCX_HOME`.
- No unexpanded or caller-rooted env flag beyond `--candidate`, `--current` and `--link`.
- No cross-machine extraction-determinism test; only archive-bundle reproducibility (`tar.rs:758`).

### leads:
- `crates/ocx_package_manager/src/tasks/pull_local.rs` and `tasks/pull.rs` (`move_temp_to_object_store`, `dest_override`) are the nearest to "content in a chosen directory".
- `crates/ocx_package_manager/src/composer.rs:475-498` (dependency context paths) and `:885-893` (`PathLane`).
- The `tar` 0.4.46 unpack path (`~/.cargo/registry/.../tar-0.4.46/src/entry.rs`) for the umask and dir-mtime behaviour I left unverified.
- Item 3 needs a shell run of `find`/`stat`/`du` on `~/.ocx/packages/*/sha256/*/*/*/content`.
