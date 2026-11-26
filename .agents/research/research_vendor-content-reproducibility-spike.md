# Research: is a pulled package's content identical across machines, and what does macOS signing change

Date: 2026-11-26
Expires: 2027-02-26
Triggered by: hex-architect high, rules_ocx under remote execution
Domain: reproducibility

## Question

1. Is the materialised `content/` tree of a package the same on two different machines (libc, uid, umask, timezone, working path)? Which attributes differ?
2. When does ocx re-sign Mach-O files at pull, which files, do bytes change, is there an opt-out?
3. What do the Mach-O files of a darwin/arm64 package look like on disk (signed? ad-hoc?)

## What was run

All with `ocx` 0.6.8 (copy of the host binary, statically linked), `OCX_HOME`, `HOME` inside the scratch dir, `OCX_NO_CONFIG=1`. The real `~/.ocx` and the shared cache were not touched. Docker containers `arch-sa-*`, started with `--rm`, no published ports.

| | machine 1 | machine 2 |
|---|---|---|
| image | `debian:bookworm-slim` (glibc 2.36) | `ubuntu:24.04` (24.04.5, glibc 2.39) |
| uid | 1001 | 2500 |
| umask | 022 | 077 |
| TZ | Asia/Tokyo (effective, tzdata present) | America/Los_Angeles (tzdata absent in image, so effectively UTC) |
| OCX_HOME | `/w/one/home` | `/srv/deep/two/home` |

Commands in each (`-r` = remote index, because a fresh home has no tag index):

```
ocx -r --format json package pull -p linux/amd64+libc.glibc ocx.sh/nodejs/node@sha256:21f94283...27dc7
ocx -r --format json package pull -p linux/amd64+libc.glibc ocx.sh/pypa/pipx:1.18      # pulls its ghcr.io python dependency too
ocx -r --format json package pull -p darwin/arm64 ocx.sh/nodejs/node@sha256:c25372df...849c9
```

Node is pinned by the platform-manifest digests of the repo `ocx.lock` (tag `24.21.0` is no longer in the public tag list; latest is 26.x). Listing per `content/` dir: `find -printf '%P\t%y\t%m\t%l\t%T@'` (type, octal mode, link target, mtime as epoch) plus `sha256sum` of every regular file; a sha256 list of every file under `$OCX_HOME/blobs` and `layers`. Diff run in a third container as root (machine 2 files are mode 700). Mach-O scan: a `python3 -I` script (Mach-O headers, fat headers, `LC_CODE_SIGNATURE`, SuperBlob, CodeDirectory flags) kept outside the tree, run in `python:3.12-slim`. Extra darwin/arm64 samples for the signing read: `ocx.sh/shfmt/shfmt@sha256:5d1573ae...`, `ocx.sh/astral-sh/python-build-standalone@sha256:ad31f2c3...` (digests from `ocx.lock`).

## Results

### Cross-machine content tree (two machines)

| package content dir | dirs | files | links | file sha256 | file+link path, type, mode, link target | file mtime (epoch) | dir mode | dir and link mtime |
|---|---|---|---|---|---|---|---|---|
| node linux/amd64 | 1088 | 4797 | 3 | identical | identical | identical | 755 vs 700 | differ |
| pipx | 37 | 201 | 0 | identical | identical | identical | 755 vs 700 | differ |
| pipx dependency (python, ghcr.io) | 198 | 3478 | 1047 | identical | identical | identical | 755 vs 700 | differ |
| node darwin/arm64 | 1088 | 4797 | 3 | identical | identical | identical | 755 vs 700 | differ |

- File modes (644, 755) and file mtimes come from the archive: unchanged by umask, uid or TZ (an earlier run printing local time showed a false diff; epoch output is identical). File mtimes are archive values (2006-07-24, or 1970-01-01 +1 s for pipx).
- Symlinks are relative, targets identical (e.g. `bin/corepack -> ../lib/node_modules/corepack/dist/corepack.js`), link mode 777.
- Directory mode follows the umask of the process (755 vs 700, every directory, including the store dirs above `content/`). Directory and symlink mtimes are the extraction time. Nothing else differs.
- The blob and layer store (13291 files in the pull of all three packages) is byte-identical (sha256 list equal).
- `ocx package export` does not exist in 0.6.8 (`unrecognized subcommand 'export'`, absent from `ocx package --help`), so the export comparison was not run. The prior research (`research_ocx-content-tree-env-offline-facts.md`) already compared two exports of a newer build byte-identical; the blob-store comparison above is the closest equivalent here.

### What ocx does about signing (read in the repo)

- `crates/ocx_store/src/codesign.rs` `sign_extracted_content`, called from `crates/ocx_package_manager/src/tasks/pull_local.rs:401` right after a layer is extracted into the layer store, before `content/` is used or linked.
- Condition: `cfg!(target_os = "macos")` (compile-time host OS; no-op on Linux and Windows, whatever the package platform), not `OCX_NO_CODESIGN` (Bool, public, `ocx_env/src/vars.rs:411`), and `codesign` found on `PATH`; otherwise it returns Ok (missing tool only warns). Failures only warn.
- Files: every regular Mach-O file found by a recursive walk (symlinks not followed, hard links signed once per inode and the aliases re-linked). It also runs `xattr` quarantine removal on the tree.
- Per file: `codesign --verify --strict` first; a file that verifies is left untouched (ADR amendment 2026-09-29, ocx#546). Only a file that fails verification is signed: `codesign --sign - --force --preserve-metadata=entitlements` (ad-hoc, drops flags, requirements and runtime).
- Bytes change only for Mach-O files whose existing signature fails `--verify --strict` (unsigned or broken after repack). They then gain an ad-hoc signature, so the result depends on that macOS host's `codesign` output. A file with a valid signature is byte-stable.
- Opt-out: `OCX_NO_CODESIGN=1`. No other switch.

### Mach-O in darwin/arm64 content (pulled on Linux, never signed by ocx here)

| package | Mach-O files | with `LC_CODE_SIGNATURE` | ad-hoc | fat |
|---|---|---|---|---|
| node (24.21.0) | 1 (`bin/node`) | 1 | 0: Developer ID, team `HX7739G8FX`, CodeDirectory flags `0x10000` (runtime), SHA-256 | 0 |
| shfmt 3.13.1 | 1 | 1 | 1 (flags `0x20002` = adhoc + linker-signed, ident `a.out`) | 0 |
| python-build-standalone | 10 (`bin/python3.14`, 9 dylibs) | 10 | 10 (flags `0x20002`) | 0 |

So all 12 Mach-O files ship signed. Expect `codesign --verify --strict` to pass for them on macOS (not proven here), so ocx would leave them alone.

## Not run and why

- No macOS host here, so no `codesign` run: whether `--verify --strict` passes for these exact files and whether a macOS pull leaves bytes unchanged is unproven.
- CI check that should prove it on a macOS runner: pull the same digests (node, shfmt, python-build-standalone, darwin/arm64) twice, once with `OCX_NO_CODESIGN=1` and once without, on `macos-latest`; compare the sha256 of every file in `content/`; the lists must be equal, and `codesign --verify --strict` on each Mach-O must exit 0. Run the signing pass twice on a deliberately stripped copy (`codesign --remove-signature`) and compare the two results to see whether ad-hoc signatures are deterministic.
- `package export` byte comparison (command absent in 0.6.8).
- Two containers share one kernel and one filesystem type; a different kernel or filesystem (case-insensitive, NTFS) was not tried.
- Non-glibc (musl) and windows packages not pulled.

## Answer

On Linux, for the packages tried, the file tree is the same on both machines: paths, types, file modes, symlink targets, file bytes and file mtimes are identical, and so is the blob store. It differs only in directory modes (follow the umask: 755 vs 700), directory mtimes and symlink mtimes (extraction time). A consumer that hashes content (file bytes, link targets, executable bit) gets one digest on both machines; one that hashes directory mode or any mtime does not.

macOS signing: ocx signs at pull only on a macOS host, only Mach-O files that fail `codesign --verify --strict`, ad-hoc, per file; `OCX_NO_CODESIGN=1` turns it off. The 12 darwin/arm64 Mach-O files sampled all ship signed (node by Developer ID, the rest ad-hoc), so signing is expected to be a no-op for them, but that needs the macOS CI check above to be shown.

## How this check is shown red and green

- Green: the listing diff of file sha256, path, type, mode and link target came out empty for all four content dirs (and for the blob list), after the mtime column was changed from local time to epoch.
- Red: the same comparison with the first listing (local-time mtimes, directory modes) reported differences in every row (9448 diff lines for pipx's dependency), and the per-column breakdown named exactly which attributes differed (dir mode, dir mtime, link mtime). So the comparison can fail; the identical result for file bytes is not a comparison that never ran (13291 and 4797 line counts match the file counts per dir).
- The Mach-O script is shown positive on files it must flag (signed counts above come from parsed headers: node reports a team id, the others none) and returns an empty result (`{}`) on the Linux ELF node tree, so it does not report Mach-O where there is none. It was not run against a known-unsigned Mach-O (none available), so the "unsigned" branch of the script is untested.
