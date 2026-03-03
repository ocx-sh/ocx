## Next

High-impact items that fix existing gaps or unblock future work.

- Atomic installs: write to a temporary directory, rename on success. Prevents partial state if the process is interrupted mid-install.
- Local installed catalog: `ocx index catalog --installed` (or similar) listing locally installed packages, enabled by `ObjectStore::list_all()`.
- User guide: document `.tar.xz` vs `.tar.gz` trade-off for `ocx package create` (compression ratio vs speed, network vs CPU).

## Backlog

### Robustness

- Process refs: a running process can lock a package to prevent GC, even without a persistent symlink ref.

### Architecture

- Refactor OCI reference data types (`Reference`, `Identifier`, `Digest`) for clarity and consistency. (The client/transport layer was refactored in `074fe3a`; data types remain.)
- Externalize index cache: replace the internal `RwLock` cache in `RemoteIndex` with a caller-provided `IndexCache` trait, so consumers control invalidation, TTL, and refresh strategy.
- Layered storage: cached/temporary package layers that sit above the content-addressed store.

### Features

- `link`/`unlink` commands: GC-aware user-managed symlinks at arbitrary paths, with back-refs in `refs/`. Deferred until a concrete use case arises.
- `update` command: update symlinked packages to their latest versions.
- platform specific candidate symlinks: e.g. `ocx install --platform linux/arm64` creates `linux/arm64/my-package` instead of `my-package`.
- Shell profile integration: `ocx shell install` writes export blocks directly into `.bashrc`/`.zshrc`/etc. with marker comments for idempotent updates.
- Auto-index of unknown packages: automatically sync index metadata when a package is referenced but not yet indexed locally.
- Platform-aware cascade: rolling releases that are aware of published platforms, so updates are computed relative to the same platform.
- Platform-specific symlink structure: install symlink layout that accounts for platform differences.

### CI & Distribution

- CI rework: multi-platform test matrix (Windows aarch64, macOS, musl, Linux arm64), coverage, doc tests.
- Snapshot releases: continuously deploy to GitHub Releases (and optionally a package registry).
- Multiple install methods for ocx itself: Homebrew, Chocolatey/Scoop, install scripts.
- Fix GitHub Pages base path (`/ocx/`) vs reverse proxy path (`/`) causing broken relative links. (Logo path fixed in `81543b5`; VitePress `base` config and absolute cast paths still need attention.)

### Developer Experience

- Progress bars for long operations (downloads, extractions).
- Advanced log settings with layers and filters.
- API output: support comma-separated tags and platforms in report formatting.
- Rustdoc: shorten type links (e.g. `ocx_lib::reference_manager::ReferenceManager` → `ReferenceManager`).

## Long Term

- Dependencies and lockfiles.
- OCI manifest mirror URLs: allow manifests to carry download mirror URLs. Challenge: adding a URL mutates the manifest digest, which breaks lockfile integrity.
- Mirror CLI: `ocx mirror` to re-publish packages from external sources (GitHub/GitLab releases, Cargo crates, npm).
- SBOM and signature support.
- metadata schema v2: richer package metadata (e.g. dependencies, build instructions, config options) in a structured format.
- Shims and lazy loading: on-demand download triggered by a shim executable.
- GitLab CI steps for consuming ocx packages.
- Bazel rules for consuming ocx packages.
- Package hooks/plugins: company-specific post-install actions (e.g. setting proxy/mirror env vars, downloading default configs). Declarative, per-package extension points.
- Modern UI components for the documentation website.

## Ideas

- `ocx test`: packages declare test scripts; ocx runs them to verify correct installation.
