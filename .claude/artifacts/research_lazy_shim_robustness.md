# Research: Declared-Command Manifests and Shim Robustness Traps

- **Date:** 2026-08-09
- **Axis:** design patterns / known pitfalls
- **Consumed by:** [`adr_lazy_package_loading.md`](./adr_lazy_package_loading.md), [ocx-sh/ocx#302](https://github.com/ocx-sh/ocx/issues/302)
- **Companion:** [`research_lazy_shim_prior_art.md`](./research_lazy_shim_prior_art.md)

## Axis A — Declared-command-name manifest fields

- **npm `package.json` `bin`** — string form only when the command name equals the
  package name; otherwise an object map `{cmd: path}`. Never verified before the
  install-time symlink (Unix) or `cmd-shim`-generated shim (Windows).
  [npm docs](https://docs.npmjs.com/cli/v11/configuring-npm/package-json/) ·
  [npm/cmd-shim](https://github.com/npm/cmd-shim)
  - **CVE-2019-16775** (npm <6.13.3/.4): `bin: "../../../usr/local/bin/x"`
    path-traversed outside `node_modules/.bin`.
    [The Register](https://www.theregister.com/2019/12/13/npm_path_traversal_bug/)
  - **CVE-2026-23890** (pnpm ≤10.28.0, GHSA-xpqm-wm3m-f34h): `@`-prefixed bin names
    skipped validation; normalization stripped only `@scope/`, left `../../` intact,
    fed unsanitized into `path.join()`. **Same bug class, different codebase, seven
    years later.**
    [Advisory](https://github.com/pnpm/pnpm/security/advisories/GHSA-xpqm-wm3m-f34h)
- **Cargo `[[bin]]`** — name defaults from the crate name or `src/bin/*.rs` filename;
  `autobins=false` disables discovery. A build-time target list only; nothing reads it
  without cloning. [Cargo Targets](https://doc.rust-lang.org/cargo/reference/cargo-targets.html)
  - **cargo-binstall** needs a separate `[package.metadata.binstall]` block (templated
    `pkg-url`/`bin-dir`) to resolve a prebuilt-artifact URL without cloning; falls back
    to a source build. [README](https://github.com/cargo-bins/cargo-binstall)
- **Python `uv tool run` / `uvx`, `pipx run`** — the package is **inferred from the
  command name** unless `--from <pkg>` is given. No PyPI-side "which package provides
  command X" lookup exists pre-install. [uv Tools](https://docs.astral.sh/uv/concepts/tools/)
- **Homebrew** — installs into `Cellar/<name>/<ver>`, then symlinks into `bin`.
  **Keg-only formulae skip the bin symlink** — install success ≠ on PATH.
  [Formula Cookbook](https://docs.brew.sh/Formula-Cookbook) ·
  [keg-only](https://github.com/orgs/Homebrew/discussions/239)
- **Nix `meta.mainProgram`** — `nix run` execs `<out>/bin/<name>`, name =
  `mainProgram` → `pname` → `name`. Falling through to guessing is now **deprecated
  upstream** ("leads to surprising errors when the assumption doesn't hold").
  [commit](https://github.com/NixOS/nix/commit/7bd9898d5ca72ed136032590745c56826317a328)
  - **nix-index / nix-index-database** — a SQLite binary→package DB built by indexing
    the binary cache, published pre-built and weekly-refreshed. A genuine registry-side
    "provides" index, decoupled from installation.
    [nix-index](https://github.com/nix-community/nix-index)
- **Debian** — `apt-file` searches a downloaded Contents index (path→package, all
  packages, needs `apt-file update`); `dpkg -S` is installed-only and offline. Two
  tiers: fast local vs complete-but-refreshed. [wiki](https://wiki.debian.org/apt-file)
- **OCI** — **no standardized "provides executable X" field found anywhere surveyed.**
  `org.opencontainers.image.*` has 14 keys, none analogous; ORAS annotations are
  freeform; Docker `LABEL` writes `Config.Labels`, distinct from manifest
  `annotations` (BuildKit `--annotation` required to populate the latter).
  [ORAS annotations](https://oras.land/docs/how_to_guides/manifest_annotations/) ·
  [labels vs annotations](https://snyk.io/blog/how-and-when-to-use-docker-labels-oci-container-annotations/)

## Axis B — Shim/launcher robustness traps

**Env vars fixed at process start.** `LD_LIBRARY_PATH`/`DYLD_LIBRARY_PATH` are read by
the dynamic linker at startup and at `dlopen()`; a change cannot reach an
already-running process ([ld.so(8)](https://www.man7.org/linux/man-pages/man8/ld.so.8.html)).
macOS **SIP (10.11+) strips all `DYLD_*`/`LD_*` from child-process environments**,
defeating even a shim that sets them correctly before exec
([hynek.me](https://hynek.me/articles/macos-dyld-env/)).

**Path-based, not exec-based, discovery.** `pkg-config` and CMake
`find_package(... CONFIG)` locate `.pc` / `<Package>Config.cmake` via
`PKG_CONFIG_PATH` / `CMAKE_PREFIX_PATH` — filesystem search, never PATH, never an exec
([CMake](https://cmake.org/cmake/help/latest/command/find_package.html)). GCC/Clang read
`C_INCLUDE_PATH`/`CPLUS_INCLUDE_PATH`/`LIBRARY_PATH` the same way
([GCC](https://gcc.gnu.org/onlinedocs/gcc/Environment-Variables.html)). Python `import`
resolves via `sys.path`, independent of PATH entirely.

**Presence probes don't exec.** `command -v` / `which` / `test -x` (Autotools
`configure`, CI) check presence and the exec bit only — a shim satisfies them instantly,
with no concept of "present but will block on a first fetch"
([Configure script](https://en.wikipedia.org/wiki/Configure_script)). mise refuses to
auto-install from the shim/prompt-hook path for exactly this reason
([#8312](https://github.com/jdx/mise/discussions/8312)).

**Concurrent first-exec** (`make -j`, parallel CI, IDE + terminal):

- **rustup [#988](https://github.com/rust-lang/rustup/issues/988)** (open) — concurrent
  proxy invocations install the same toolchain simultaneously and corrupt it; needs an
  install-serializing lock *plus* notification to running proxies, NFS-safe.
- **Cargo's package cache** — the counter-example done right: three lock modes,
  `Shared` / `DownloadExclusive` / `MutateExclusive`, so many builds run concurrently
  and only download/mutate serializes
  ([cache_lock](https://doc.rust-lang.org/beta/nightly-rustc/cargo/util/cache_lock/index.html),
  [PR #12706](https://github.com/rust-lang/cargo/pull/12706)).
- **pyenv** — the rehash lock (`.pyenv-shim`) itself races on concurrent shell starts
  ([#2829](https://github.com/pyenv/pyenv/issues/2829)).
- **asdf** — a shim can mask a real executable without a resolvable version
  ([#1216](https://github.com/asdf-vm/asdf/issues/1216)).
- **uv [#15335](https://github.com/astral-sh/uv/issues/15335)** (open, Linux) —
  concurrent installs where one process cannot "see" another's just-installed package;
  suspected fsync/flush ordering, i.e. **write visibility across processes is a hazard
  separate from lock acquisition**.

**Windows.** `.cmd` is the only universally-working shim format — default PowerShell
execution policy (`Restricted`/`RemoteSigned`) blocks unsigned `.ps1`
([cmd-shim](https://github.com/npm/cmd-shim)). `PATHEXT` resolution is spawner-dependent:
`child_process.spawn` without `shell: true` does not consult it, producing real ENOENT
on bare command names ([claude-code#51191](https://github.com/anthropics/claude-code/issues/51191)).
Batch wrappers must `EXIT /B %ERRORLEVEL%` or silently return 0
([ss64](https://ss64.com/nt/errorlevel.html)). Ctrl-C/SIGINT delivery to a shimmed child
is not guaranteed — `windowsHide: true` was found to suppress it entirely, orphaning
processes ([nodejs/node#29837](https://github.com/nodejs/node/issues/29837)).

## Hard constraints for any lazy design

1. Declared command names are **trusted, never verified**, by every surveyed ecosystem.
   Sanitize against path traversal (the CVE class recurred independently in two
   implementations) and decide explicitly whether the declaration is checked before the
   shim is trusted to route.
2. A PATH-only shim **structurally cannot** cover: linker env vars fixed at startup
   (further defeated by macOS SIP), pkg-config / CMake / compiler path-based discovery,
   or Python's import-based resolution. Architectural boundary, not an implementation gap.
3. Presence probes succeed the instant a shim file exists, decoupled from whether
   invoking it blocks. Define what "present" means before materialization.
4. Concurrent first-exec is a repeatedly-hit, partly-open failure class industry-wide.
   The only clean prior art is Cargo's **tiered** lock — not a single global lock, and
   not no lock.
5. Windows needs: `.cmd` (or a native `.exe`) rather than `.ps1`; PATHEXT-independent
   invocation handling; explicit exit-code propagation; explicit Ctrl-C handling.

## Application to OCX (recorded at research time)

- OCX's `BinaryName` already forbids `/`, `\` and the Windows-reserved set **at
  construction**, citing the npm/pnpm CVE family — constraint 1 is closed by grammar.
- OCX ships a native `.exe` shim and emits no `.cmd`, closing the PowerShell-policy,
  `EXIT /B` and PATHEXT legs of constraint 5.
- `child_process::exec` `execvp`s on Unix — the correctness axis proto's v0.26 rewrite
  was about.
- Constraints 2 and 3 are the ones the design must answer rather than inherit.
