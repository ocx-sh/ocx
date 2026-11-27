# Research: ocx facts for a Bazel integration (content tree, foreign platform, offline pull, environment report, launchers, run-at-use cost)

## Metadata

**Date:** 2026-11-25
**Lane:** codebase recon and measurement (/hex-discuss, .agents/discussions/bazel-rbe-rules-ocx.md)
**Basis:** RUN with a test build of main and READ at origin/main b09ec709; each claim marked.
**Expires:** 2027-05-25
**Caveat:** findings only, no recommendation. Timings are from one machine with a warm file cache.

Conventions. `$O` is the test build `.../scratchpad/bin/ocx-main`. `$F` is the scratch directory `.../scratchpad/ocx-facts`. Every command ran with `OCX_HOME=$F/<fresh dir>`; `~/.ocx` was never written. "RUN" means I ran it; "READ" means I read `origin/main` at b09ec709 (path:line). Identifiers used: node `ocx.sh/nodejs/node@sha256:21f94283...7dc7`, python `ocx.sh/astral-sh/python-build-standalone@sha256:d0109a4b...1c8`, shfmt `ocx.sh/shfmt/shfmt@sha256:3af504ea...e09c9` (all linux/amd64 digests taken from `ocx.lock`), plus `ocx.sh/pypa/pipx:1.18` as the package with a dependency. The test build has no `--version` flag (`error: unexpected argument '--version'`), so its exact version string is NOT VERIFIED.

A harness note: the installed shell has an `ocx` per-prompt hook. Running `ocx` with a cwd inside a scratch home printed spurious `.../h1/ocx.toml: I/O error` lines. All later commands ran from outside the homes (and through `bash` scripts) to avoid it.

## 1. Content tree from a pull

RUN `OCX_HOME=$F/h1 $O --format json package pull <node digest>` (7.2 s wall, network). The report is `{"schema_version":1,"paths":{"<identifier>":"<absolute package root>"}}`. The root for node was `$F/h1/packages/ocx.sh/sha256/21/f942839c3f9367c5ce0f5dda804621`. The unpacked files are in `<root>/content/`, for example `content/bin/node`.

The path can be computed from the digest alone. The shape is `$OCX_HOME/packages/<registry>/sha256/<first 2 hex>/<next 30 hex>`. The directory name is a 32-hex truncation (2 + 30) of the digest, not the full 64. This held for all 19 packages I pulled (node `21/f942...`, python `d0/109a...`, shfmt `3a/f504...`). The registry segment is the registry host of the identifier (`ocx.sh`, or `ghcr.io` for the pipx dependency). `ocx package which <id>` (JSON: `{"paths":{id:{"path":...,"kind":"package"}}}`) and `ocx package pull` both report the same root.

The package root holds `content/`, `digest`, `install.json`, `manifest.json`, `metadata.json`, `resolve.json` and `refs/` (subdirectories `blobs`, `layers`, `origins`), and an `entrypoints/` directory when the package declares entrypoints. The layer is first extracted into `$OCX_HOME/layers/<registry>/sha256/<2>/<30>/content` and the package `content/` is assembled from it by hardlinks (READ `crates/ocx_store/src/file_structure/assemble.rs:4`: "hardlinked files and verbatim Unix symlinks"; called at `crates/ocx_package_manager/src/tasks/pull.rs:633`). Raw OCI blobs sit in `$OCX_HOME/blobs/<registry>/sha256/<2>/<30>/data`.

Dependencies. RUN `ocx package pull ocx.sh/pypa/pipx:1.18` pulled the dependency too (a second package under `packages/ghcr.io/sha256/0d/d5ac8eb7f146720e674624548fac05`), although the JSON report lists only the requested root. The `--help` text says "Transitive dependencies are pulled as well". `ocx package deps` shows the edge `ghcr.io/ocx-contrib/astral-sh/python-build-standalone:3.13.14 · private · sha256:0dd5ac8e...`.

Does any pinned tool have dependencies? No. I pulled the linux/amd64 leaf of all 19 distinct (tool, digest) pairs in `ocx.lock` (all groups) and grepped every `metadata.json` for `dependencies`: 0 of 19 had one, and every `resolve.json` for node was `{"dependencies": []}`. `ocx.lock` has no dependency field either. The third case is therefore pipx, not a lock tool.

## 2. Foreign platform

RUN `ocx package pull -p linux/arm64|darwin/arm64|windows/amd64 ocx.sh/shfmt/shfmt:3.13.1` into one home: all three succeed on this Linux x86_64 host. Content: `shfmt` (ELF magic `7f45 4c46`), `shfmt` (Mach-O `cffa edfe`), `shfmt.exe` (`MZ`). The flag is `-p, --platform os/arch[/variant][+feature,...]` on `package pull`, `package export`, `package import`, `package inspect` and `package exec`; there is no environment variable for it (READ `crates/ocx_env/src` grep: no platform variable; NOT VERIFIED beyond that grep). Default is the host platform. A package that publishes `linux/amd64+libc.glibc` needs that exact string, not `linux/amd64` (RUN: `export -p linux/amd64 ocx.sh/pypa/pipx:1.18` exit 79 "package not found"; `-p linux/amd64+libc.glibc` worked).

Export for a foreign platform also works: RUN `package export -p linux/arm64 -p darwin/arm64 -p windows/amd64` (three calls into one dir) printed `written` for each. Import then needs the platform named: `package import $F/exp_fp` without `-p` exited 65 (`platform linux/amd64 ... is not in the export`); `package import -p darwin/arm64 -p windows/amd64` reported `materialized` for both. Executing a foreign binary fails as expected (`package exec -p darwin/arm64 ... shfmt --version` ended in `shfmt: 1: Syntax ...`, the kernel handed the Mach-O to `sh`).

Platform-specific code at unpack time, all keyed to the host and not to the package platform:
- macOS code signing: READ `crates/ocx_store/src/codesign.rs` `sign_extracted_content`, first statement `if cfg!(not(target_os = "macos")) { return Ok(()) }`; opt-out `OCX_NO_CODESIGN`; it ad-hoc signs each Mach-O whose signature does not verify and removes quarantine. Called from `crates/ocx_package_manager/src/tasks/pull.rs:1224` (`finish_layer`, before the layer is published) and `tasks/pull_local.rs:401`. On a Linux host a darwin package is therefore unpacked unsigned.
- Windows launchers: READ `crates/ocx_package_manager/src/launcher/generate.rs:13,53` the `.exe` shim plus `.shim` sidecar is `#[cfg(windows)]`, so it is made only when the host is Windows. On a Linux host only the `.sh` launcher is written. The function is called from `pull.rs:643-647` for any package with entrypoints.
- Symlinks: READ `assemble.rs:65-66` has an error `layer symlinks are not supported on Windows` (a Windows host refuses layers that contain symlinks); Unix hosts recreate them verbatim.

## 3. Offline from a blob directory

Export (RUN, `package export -p <platform> -o <fresh dir>`, fresh home each run, network on; digest identifiers, pipx by tag with `-p linux/amd64+libc.glibc`). Wall seconds, run 1 / run 2: node 4.15 / 4.30, python 5.65 / 5.63, shfmt 1.98 / 1.93, pipx 7.45 / 8.73. Export sizes: node 38 MB, python 67 MB, shfmt 1.2 MB, pipx 66 MB. A digest export is 8 files: `ocx-export.json` plus, per blob, `blobs/<registry>/sha256/<2>/<30>/{data,digest}` (and `referrers/.complete`). A tag export (pipx) adds `index/` (29 files total).

Does pipx's export contain its dependency's blobs? Yes: `blobs/ghcr.io/sha256/0d/d5ac8eb7f146720e674624548fac05/{data,digest}` is the python dependency manifest, `blobs/ocx.sh/...` the pipx ones.

Offline pull (RUN, fresh home, `OCX_BLOB_DIR=<export>/blobs`, `ocx --offline package pull`). Wall seconds, run 1 / run 2, all exit 0: node 5.04 / 4.53, python 7.66 / 7.55, shfmt 0.16 / 0.17, pipx 7.69 / 8.81 (pipx needs `--index <export>/index` because it is addressed by tag; both the pipx and its python dependency landed in the home). The offline flag is `--offline` (`OCX_OFFLINE`).

What failed first, for the record. The blob directory is the export's `blobs/` subdirectory (docs `website/.../guides/export-and-import.mdx:104`), not the export root. With `OCX_BLOB_DIR=$F/exp_node_1` (root) `--offline package pull` exited 79 `package not found`. With a digest identifier and no `--offline` but `HTTPS_PROXY=http://127.0.0.1:9`, pull exited 75 (`index request to https://index.ocx.sh/config.json failed`), so a proxy alone is not "offline": the digest reference still asks the index. `--offline` is the working switch.

`package import <export> -p linux/amd64+libc.glibc [<id>]` also works offline (RUN, exit 0; wall run 1 / run 2: node 6.16 / 5.02, python 8.98 / 7.85, shfmt 0.94 / 0.99, pipx 10.24 / 9.94). A digest export has no `index/`, so `import` needs the package named; without a name it exits 65 ("it has no index to import").

## 4. Reproducibility

RUN: node, python and shfmt pulled by import into two fresh homes at different paths (`$F/hi_<n>_1`, `$F/hi_<n>_2`). Compared `find -printf '%p %y %m %s %l'` and `sha256sum` of every file under `content/`.
- node: 5888 entries, metadata diff 0 lines, sha256 diff 0 lines; python: 4771 entries, 1047 symlinks, diff 0, 0; shfmt: 2 entries, diff 0, 0. So file list, bytes, modes, sizes and link targets are identical. Symlink targets are relative (node: `bin/npm -> ../lib/node_modules/npm/bin/npm-cli.js`; python: `bin/python3 -> python3.14`); zero absolute targets in python.
- `digest`, `manifest.json`, `metadata.json`, `resolve.json` identical. `install.json` differs (it records the install timestamp).
- mtimes: file mtimes are equal (node 4797 files: 0 differences; they carry the archive's timestamp, 1153704088 = 2006). Directory mtimes follow the wall clock (all 1088 node directories differ between the two homes, for example `./bin` 1795564188 vs 1795564193).
- Exports: `diff -r` of two exports of the same package into fresh dirs was empty for node, python, shfmt (byte-identical). The pipx tag export differed in exactly one file name: an empty 0-byte file under `index/locks/` (`a0...lock` vs `16...lock`). Nothing else differed; mtimes of the export files were not compared.

## 5. What is tied to the machine or path

RUN in one home (`$F/h5`): pull node, then `package install` node.
- After pull: 3 symlinks with absolute targets, all under `packages/.../refs/` (`refs/blobs/*` and `refs/layers/*`, pointing into the same home's `blobs/` and `layers/`). No file anywhere contains the home path (`grep -rlF $H` empty).
- After install: 5 absolute symlinks: the install symlink `symlinks/ocx.sh/nodejs/node/candidates/latest -> $H/packages/...`, its back reference `refs/symlinks/<id> -> $H/symlinks/...`, plus the 3 above. Still no file contains the home path.
- Inside `content/`: 0 absolute symlinks and 0 files containing the home path (checked with `find -lname '/*'` and `grep -rlF`). Text mentioning `/tmp/` or `/home/` appears only as documentation (npm man pages, `cache.js`), unrelated to the home.
- Exception: generated launchers. In `$F/h1`, the pipx package's `entrypoints/pipx` and `entrypoints/python` contain the absolute package root (see section 7). That directory is outside `content/`.
- Hardlinks: content files have link count 2 (RUN `stat -c %h content/bin/node` = 2). The other name is in the layer store: `$OCX_HOME/layers/<registry>/sha256/<2>/<30>/content/...` (node: `layers/ocx.sh/sha256/43/86245aa4f581447c941651dc15a9a6/content`). Whole-home `du` is 212 MB for node against 193 MB apparent content, so the bytes are stored once.
- Detach test (RUN): moved `content/` to `$F/moved`, `rm -rf` the whole home, then `$F/moved/bin/node --version` printed `v24.21.0` and `PATH=$F/moved/bin:/usr/bin:/bin $F/moved/bin/npm --version` printed `11.19.0`. The hardlink count dropped to 1. `ldd` shows only system libraries (libdl, libstdc++, ...). This is for node (relocatable by construction); I did not repeat it for python or pipx.

## 6. Environment report

RUN `ocx --format json package env <node digest>` (shape, truncated):
```
{"schema_version":1,
 "items":[{"key":"PATH","value":"$H/packages/ocx.sh/sha256/21/f942839c3f9367c5ce0f5dda804621/content/bin","kind":"path"}],
 "binaries":[{"name":"corepack","package":"ocx.sh/nodejs/node@sha256:21f9..."}, ... "node","npm","npx"],
 "entrypoints":[],"integrations":[],"advisories":[]}
```
`kind` is `path` or `constant`. The pipx report (default surface) is a single `PATH` item `.../97/497f4cc7ffdaf08aa4ef530da16794/entrypoints` plus `entrypoints:[{"name":"pipx",...},{"name":"python",...}]`. With `--self` it is four items: `PATH .../ghcr.io/sha256/0d/d5ac.../content/bin` (the dependency), `PYTHONPATH .../97/497f.../content/lib/site-packages`, `PATH .../97/497f.../content/bin`, and `PYTHONDONTWRITEBYTECODE=1` (constant), with 12 python binaries attributed to the dependency.

Project level (RUN in a scratch copy of the repo `ocx.toml`/`ocx.lock`, fresh home, 12.7 s including pulling everything): `ocx --format json env` has the same keys; 15 `PATH` items, 32 binaries, 0 entrypoints. `ocx --format json env -g lint`: 5 items, 6 binaries. Values are project-relative links, not package roots: `$PROJ/.ocx/toolchain/links/default/actionlint/content`, `.../links/lint/grim/content/bin`. Each `links/<group>/<tool>` is a symlink with an absolute target to the package root (`-> $H/packages/ocx.sh/sha256/5f/935e...`); the project `.ocx/toolchain/` directory is created on first use inside the project.

Absolute? Yes: every path value in all reports was absolute. Splitting into root plus relative path: for the package reports, yes, using only `ocx` output. `ocx --format json package which <ids>` returns the roots; I checked the pipx `--self` report against the roots of pipx and its python dependency: every path value starts with one of the two roots plus `/content/...` (the constant `PYTHONDONTWRITEBYTECODE=1` is not a path). The dependency root is obtainable from `package deps` (identifier and digest) and `which`. For the project report the value is a link path, so the root needs the link's target (`readlink`), which `ocx` does not print in the env report; there is no project-level `which` in this build (`ocx which` is an unknown subcommand; `ocx inspect` reports locked digests and candidates, not roots). Dependencies: yes, a dependent package's `--self` env points into the dependency's `content/bin`, as above.

## 7. Entry points and launchers

Declaration (READ origin/main): `crates/ocx_package/src/metadata/bundle.rs:58-65` (`entrypoints: Entrypoints` in the bundle metadata) and `crates/ocx_package/src/metadata/entrypoint.rs:93-117` (an `Entrypoint` has an optional `command` and `args`; the map key is the launcher name, slug `^[a-z0-9][a-z0-9_-]*$`, max 64). The pipx `metadata.json` shows it: `"entrypoints": {"pipx": {"command":"python","args":["-c","..."]}, "python": {}}`. A separate `binaries` list is a publisher claim of executable names (bundle.rs:67), not a launcher.

Generated on pull/install (RUN + READ): a directory `<package root>/entrypoints/`, a sibling of `content/`, one executable file per entrypoint (`generate.rs:25-50`, called at `pull.rs:640-647`). Body (RUN, and READ `launcher/body.rs:15-22`):
```
#!/bin/sh
# Generated by ocx at install time. Do not edit.
exec "${OCX_BINARY_PIN:-ocx}" launcher exec '<absolute package root>' -- "${0##*/}" "$@"
```
So yes, it holds one absolute path (the package root, baked at pull time, 298 bytes) and calls back into `ocx` (`launcher exec`), looked up as `ocx` on `PATH` unless `OCX_BINARY_PIN` is set. On a Windows host it is an `.exe` shim plus `.shim` sidecar instead (generate.rs:53). A package without entrypoints gets no `entrypoints/` directory (node: none).

What `PATH` carries (RUN env reports above, READ `composer.rs:400-401` "`entrypoints/` > `bin/` > `shims/`" and `composer.rs:582-587`): the consumer view (default) puts the package's `entrypoints/` directory on `PATH` when it declares entrypoints, in addition to whatever its own `PATH` env entries say; the `--self` view omits `entrypoints/` and shows the package's own `content/bin` (pipx declares that `PATH` entry as `private`). Node declares only `PATH=${installPath}/bin` (public) and no entrypoints, so its `PATH` entry is `<root>/content/bin`. `npm` is not an entrypoint: `entrypoints` is `[]`, and `npm` is listed in `binaries`. In `content/bin`, `npm` is a relative symlink to `../lib/node_modules/npm/bin/npm-cli.js` whose first line is `#!/usr/bin/env node`, so it needs `node` on `PATH`.

## 8. Run-at-use cost

RUN, fresh home each, `OCX_BLOB_DIR=<export>/blobs`, `ocx --offline package exec <digest> -- <tool> --version`; wall seconds, "first" (unpacks) / "second" (same home): node 4.86 / 0.38, 4.88 / 0.03, 4.50 / 0.02; shfmt 0.17 / 0.02, 0.16 / 0.02, 0.18 / 0.02. All exit 0 and printed `v24.21.0` / `v3.13.1`. Of the first-run time, the extraction of a 39 MB `tar.xz` layer dominates; I did not profile it separately.

Unpacked size: node 4797 files, 3 symlinks, 1088 directories, 193 MB apparent in `content/` (home `du` 212 MB because the layer store and package share hardlinks); shfmt 1 file, 3.0 MB (home 3.2 MB).

Multiple packages: yes. `ocx --offline package exec $NODE $SHFMT -- sh -c 'node --version; shfmt --version'` (two `OCX_BLOB_DIR` entries separated by `:`) exited 0 in 4.88 s and printed both versions. The usage is `exec [OPTIONS] <PACKAGES>... <COMMAND>...`.

Project level: yes. A scratch project (`node`, `shfmt`, lock made with `ocx lock`), `ocx export -p linux/amd64+libc.glibc -o exp_proj` (4.0 s, 39 MB; `-p linux/amd64` failed with "no 'linux/amd64' leaf for binding 'node'"), then `ocx --offline --blob-dir exp_proj/blobs exec -- node --version` in the project: first run 4.56 / 4.45 / 4.42 s, second 0.03 s each, exit 0. The project-level import is `ocx import <dir>`; `ocx exec` reads the blob directory through the same `--blob-dir` / `OCX_BLOB_DIR`.

## 9. Listing files or links

I found no command or report that lists the files or symlinks of a package, before or after unpacking. `ocx package --help` has no listing verb. `ocx --format json package inspect <digest>` returns metadata and the layer list only (`digest`, `media_type`, `size`) and no file names. `package deps` lists packages, `package which` lists package roots, `package sbom` lists SBOM attestations (whether an SBOM carries file names is NOT VERIFIED). After unpacking, the file list is only obtainable by walking `content/` on disk.

## Not verified or failed

- The test build's exact version; platform environment variable beyond a source grep.
- Behaviour of a Windows or macOS host (unpack-time code read only, not run).
- Relocation test was node only.
- `package sbom` content; whether `exec` with a foreign platform could succeed under emulation.
- `ocx export -p linux/amd64` failure text was not captured exactly; the exit code is not recorded.
