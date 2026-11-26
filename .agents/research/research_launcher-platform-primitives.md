# Research: platform primitives for the unified launcher

Date: 2026-11-26 · Expires: 2027-02-26 · Triggered by: `/hex-architect` (tier high) for `adr_unified_launcher.md`, goal loop `.agents/goals/bazel-rbe-rules-ocx.md` · Domain: launcher

One research worker read documentation and source on 2026-11-26. Tags: **documented** (URL), **source** (read in source, URL and file), **secondhand**, **not established**. Nothing ran on Windows or macOS. Runfiles library sources were read at `main`/`master` on 2026-11-26. Bazel 9 was not read separately. This file builds on `research_launcher-atomic-publish-and-runfiles.md` and answers its "Not established" items where a source exists.

## 1. Bazel runfiles manifest

### File names

- Bazel writes the input manifest `<exe>.runfiles_manifest` beside the executable, and the output manifest `MANIFEST` inside `<exe>.runfiles/`. [source: https://raw.githubusercontent.com/bazelbuild/bazel/master/src/main/java/com/google/devtools/build/lib/analysis/RunfilesSupport.java, `INPUT_MANIFEST_EXT`, `OUTPUT_MANIFEST_BASENAME`]
- The input manifest exists only while `--build_runfile_manifests` is on. `--nobuild_runfile_manifests` removes it. [source: same file]
- The `MANIFEST` and the symlink farm exist only while `--build_runfile_links` is on. Without it the input manifest is the runfiles manifest. [source: same file]
- Under `--nobuild_runfile_links`, expect `<exe>.runfiles_manifest` only.
- The name is built from the executable path, so Windows uses `foo.exe.runfiles_manifest`. [secondhand: Bazel's Windows runfiles test checks `bin_path + ".runfiles_manifest"` for `bar-<lang>.exe`, https://bazel.googlesource.com/bazel/+/87d458d09e9a520b9777f9ee07b89d3a5083d6d8/src/test/py/bazel/runfiles_test.py]
- On Windows `--enable_runfiles` is off by default and only the manifest is created. [secondhand: https://bazel.googlesource.com/bazel/+/2ff8b054349ea0281eb0b8495e3cadcccf0c9692]
- A Windows runfiles directory can exist holding only `MANIFEST`, so the directory's existence does not prove a tree. [secondhand: https://hackage.haskell.org/package/bazel-runfiles-0.12/docs/src/Bazel.Runfiles.html]
- `--noenable_runfiles` leaves the manifest in place. [secondhand: same commit as above]
- The input manifest also lists `_repo_mapping` as a root entry when Bazel supplies one. [source: https://raw.githubusercontent.com/bazelbuild/bazel/master/src/main/java/com/google/devtools/build/lib/analysis/Runfiles.java]

### Tests

- The test wrapper sets `RUNFILES_MANIFEST_FILE=${TEST_SRCDIR}/MANIFEST`. It exports `RUNFILES_MANIFEST_FILE` and `RUNFILES_MANIFEST_ONLY` only when `RUNFILES_MANIFEST_ONLY=1` and the file exists. [source: https://raw.githubusercontent.com/bazelbuild/bazel/master/tools/test/test-setup.sh]
- The wrapper makes a relative `RUNFILES_DIR` and `TEST_SRCDIR` absolute against `$PWD`. [source: same file]
- The test encyclopedia documents `TEST_SRCDIR` and does not mention `RUNFILES_*`. [documented: https://bazel.build/reference/test-encyclopedia]

### Remote execution

- A remote Windows test runs with an empty `RUNFILES_MANIFEST_FILE`. Bazel's own test says "Remotely, the runfiles manifest does not exist." [source: https://raw.githubusercontent.com/bazelbuild/bazel/master/src/test/py/bazel/windows_remote_test.py]
- A binary built remotely must still run locally, so the manifest exists locally. [source: same file]
- A remote action finds runfiles relative to its working directory, so it gets a tree. EngFlow strips both `RUNFILES_MANIFEST_*` variables from remote actions. [documented: https://docs.engflow.com/re/client/windows.html]
- Whether a remote action also receives the manifest as an input file: not established.
- Consequence: the launcher tries the tree first and the manifest second. A remote action never has a usable manifest.

### Line format

- Bazel writes `<source> <target>\n` in ISO-8859-1. A null target gives `<source> ` with a trailing space and nothing after it. [source: https://raw.githubusercontent.com/bazelbuild/bazel/master/src/main/java/com/google/devtools/build/lib/analysis/SourceManifestAction.java]
- Empty files get no special case; they carry their real path. The empty-target line comes from null entries, paths Bazel adds on its own (Python `__init__.py` markers). [source: SourceManifestAction.java, Runfiles.java]
- A line is escaped if the source holds a space or newline, or the target holds a newline. Then the line starts with a space; the source maps space to `\s`, newline to `\n`, backslash to `\b`; the target maps only newline to `\n` and backslash to `\b`, and spaces stay. [source: SourceManifestAction.java]
- Unescaped lines carry backslashes verbatim, kept for old libraries. A Windows target such as `C:\x\y` is literal on an unescaped line. [source: SourceManifestAction.java]
- The escaping was introduced in Bazel 7.4.0. [documented: https://github.com/bazelbuild/bazel/pull/23912]
- Bazel does no slash conversion here. Whether Windows targets arrive with `\` or `/` is not established; accept both.
- Runfiles.java never expands a tree artifact per file, and a nested runfiles tree is rejected. Whether a directory is one manifest line is not established from Bazel source. The rules_cc library has a longest-prefix lookup that implies directory lines exist. [source: https://raw.githubusercontent.com/bazelbuild/rules_cc/main/cc/runfiles/runfiles.cc]
- rules_rust does exact match only, with no prefix search. [source: https://raw.githubusercontent.com/bazelbuild/rules_rust/main/rust/runfiles/runfiles.rs]

### Parse algorithm and lookup order of the runfiles libraries

- rules_cc parse: split at the first space. A line that starts with a space is stripped of it, and source and target are unescaped with `\s`, `\n`, `\b`; other `\x` stay as two characters. The loop stops at the first empty line. [source: rules_cc `runfiles.cc`]
- rules_rust parse: `split_once(' ')`, unescape `\s`/`\n`/`\b` in the key and `\n`/`\b` in the value on escaped lines. Malformed lines are skipped silently. A later duplicate key wins. [source: rules_rust `runfiles.rs`]
- rules_cc order: `RUNFILES_MANIFEST_FILE` or `RUNFILES_DIR`; then `argv0.runfiles/MANIFEST`; then `argv0.runfiles_manifest`; then `dir/MANIFEST` and `dir_manifest`. With only a manifest found, the directory is derived by stripping `.runfiles_manifest` or `/MANIFEST`. Test mode passes `TEST_SRCDIR` as the directory. [source: `runfiles.cc`]
- rules_cc lookup: exact key, then the longest prefix with `prefix_target + "/" + rest`, then `directory + "/" + path`. It rejects paths containing `//`, `./`, `/.` or `../`. [source: `runfiles.cc`]
- rules_rust order: `RUNFILES_MANIFEST_FILE`; then `RUNFILES_DIR`, `TEST_SRCDIR`, then an `argv0` walk for `<bin>.runfiles` and ancestors ending in `.runfiles`; a directory holding `MANIFEST` has it parsed; then `<argv0>.runfiles_manifest`, with `current_exe()` for a bare name. [source: `runfiles.rs`]
- rules_python and the Java runfiles library were not read. Keys use canonical repository names; the mapping from apparent names is the separate `_repo_mapping` file.

## 2. Windows junctions

- `FSCTL_SET_REPARSE_POINT` requires `SE_CREATE_SYMBOLIC_LINK_NAME` per its page. [documented: https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-fsctl_set_reparse_point]
- Practice contradicts it: `mklink /J` creates junctions without elevation. [secondhand: https://github.com/git-for-windows/git/wiki/symbolic-links] The Rust `junction` crate documents no admin need and has no privilege code. [secondhand: https://docs.rs/crate/junction/1.4.0; source: https://raw.githubusercontent.com/tesuji/junction/master/src/internals.rs] The conflict stays open until a CI job runs it as a non-admin user.
- Overwrite: open with `FILE_FLAG_OPEN_REPARSE_POINT` and set again; the tag must match, else `ERROR_REPARSE_TAG_MISMATCH`. [documented: https://learn.microsoft.com/en-gb/windows/win32/fileio/reparse-point-operations]
- Atomicity of an in-place retarget for a concurrent `CreateFile` through the junction: not established. No source states it either way.
- A new reparse point needs an empty directory. [documented via summary: same page]
- The `junction` crate never retargets: `create` runs `create_dir` first and fails on an existing path. [source: `internals.rs`] No prior art found that retargets in place.
- `MoveFileEx` with replace fails for a directory destination. [documented: https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw]
- Kernel documentation: with `ReplaceIfExists` the rename "will still fail if a file with the same name already exists and is a directory". [documented: https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-_file_rename_information]
- `FILE_RENAME_POSIX_SEMANTICS` with `REPLACE_IF_EXISTS` allows replacing a file that has open handles; the page says nothing about directories or junctions. [documented: same page] Whether a POSIX rename replaces a junction on NTFS: not established.
- Rust std `rename` uses `MoveFileExW`, then falls back to `FileRenameInfoEx` with POSIX and replace flags. [source: https://raw.githubusercontent.com/rust-lang/rust/master/library/std/src/sys/fs/windows.rs] It is unreliable on some volumes (`STATUS_VOLUME_NOT_UPGRADED`). [source: https://github.com/rust-lang/rust/issues/137499]
- A process with an open handle below the old target keeps working, because a handle names the object, not the path. Inference, not a source.

## 3. Trampoline recognition on Unix

- rustup recognises a proxy by the name it was started under (`argv[0]`); `RUSTUP_FORCE_ARG0` overrides it in tests. [secondhand: https://rust-lang.github.io/rustup/dev-guide/tips-and-tricks.html]
- rustup links proxies by symlink, then hardlink; it copies on Windows. [secondhand: https://users.rust-lang.org/t/why-does-all-cargo-binaries-have-the-same-hash/128948]
- proto keeps shims in `~/.proto/shims` and symlinks to native binaries in `~/.proto/bin`. [secondhand: https://moonrepo.dev/blog/proto-v0.20] How proto, mise and Volta tell a shim apart: not established.
- Options (analysis, no source):
  - Sibling record file: one `open` of a file the launcher needs anyway; fails if the record is deleted or a copy lacks it. Cheapest here.
  - Magic bytes or a section: needs a read and format knowledge; survives copies; breaks on strip or relink.
  - `(dev, inode)` equal to the store blob: one `stat`; true only for hardlinks, false for copies and after a blob change.
  - xattr: lost by copy, tar and some file systems; an extra system call.
  - `argv[0]`: names the start, not the file.

## 4. macOS

- The kernel caches code-signature data per Mach-O file and does not flush it when the file is changed in place; replace with a new file by `rename`. [documented: https://developer.apple.com/documentation/security/updating-mac-software]
- `rename` over a running binary is the recommended pattern: the old file stays unchanged and keeps its own cache. [documented: same page]
- A copy under a new name works. [secondhand, Apple DTS: https://developer.apple.com/forums/thread/689327]
- Hardlinks: no source. Both names share one file, so one cache entry and one signature; unchanged content stays valid. Inference. Many hardlinks to one signed binary: not established.
- Linker-signed arm64 against unsigned x86_64: not established beyond the earlier research file.

## 5. Windows running executables

- A directory cannot be renamed while it or a subdirectory holds a file with open handles. [documented: https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-_file_rename_information] A running `.exe` is such a file. [secondhand: https://www.python.org/pipermail/python-dev/2018-July/154806.html]
- A running `.exe` can be renamed, not deleted or overwritten. [secondhand: https://learn.microsoft.com/en-us/windows-hardware/drivers/ifs/executable-images]
- Creating a new directory beside a running hardlinked `.exe` touches nothing its handle covers. Inference only.
- A junction retarget changes the junction's own reparse data and touches no file under the target, so a running `.exe` under the old target should not block it. Inference only; no source.

## Design implications

1. Lookup order: the tree first (started path, `<exe>.runfiles`, `RUNFILES_DIR`), then the manifest (`RUNFILES_MANIFEST_FILE`, `<exe>.runfiles/MANIFEST`, `<exe>.runfiles_manifest`). Never assume a manifest exists remotely.
2. Parse like rules_cc: split at the first space, a leading space marks escaping, unescape `\s`, `\n`, `\b`, keep other `\x` literal.
3. A prefix lookup is needed if a directory is one line; rules_rust has none.
4. An empty target means "no file".
5. Accept `\` and `/` in Windows targets.
6. The Windows junction retarget stays unproven; the fallback is today's remove-then-rename with its gap.
7. On Unix recognise a trampoline through its record file; an inode or size comparison fails on copies.
8. On macOS publish a complete new file or directory with one `rename`; never write into a signed file.

## Still not established, with the CI check that settles each

| Item | CI check |
|---|---|
| Junction retarget is atomic for a concurrent lookup | Windows runner job: one process loops `CreateFile` through `toolchain/active` while another retargets it; count failures |
| Junction retarget needs no privilege | the same job as a non-admin user, with and without Developer Mode |
| POSIX rename replaces a junction on NTFS | Windows job calling `FileRenameInfoEx` with both flags onto an existing junction |
| A running `.exe` blocks a directory rename; a hardlinked `.exe` stays; a junction retarget with a running `.exe` | Windows job that starts a hardlinked `.exe`, then runs each operation |
| Manifest name and line shape on Windows (`.exe` suffix, slash style, escaping) | Windows job building one target with `--noenable_runfiles` and printing `bazel-bin/*.runfiles_manifest` |
| Remote Windows actions receive a tree, a manifest or both | a remote Windows run; not possible on a hosted runner; stays a claim |
| A directory is one manifest line | Linux job with `--noenable_runfiles` and a directory in `data` |
| A hardlink or copy of a signed arm64 binary stays valid; rename over a running one; many hardlinks | macOS arm64 job running `codesign --verify` and a start after each step |
| Linker-signed arm64 against unsigned x86_64 | macOS x86_64 job |

## Sources

- https://raw.githubusercontent.com/bazelbuild/rules_cc/main/cc/runfiles/runfiles.cc
- https://raw.githubusercontent.com/bazelbuild/rules_rust/main/rust/runfiles/runfiles.rs
- https://raw.githubusercontent.com/bazelbuild/bazel/master/src/main/java/com/google/devtools/build/lib/analysis/SourceManifestAction.java
- https://raw.githubusercontent.com/bazelbuild/bazel/master/src/main/java/com/google/devtools/build/lib/analysis/RunfilesSupport.java
- https://raw.githubusercontent.com/bazelbuild/bazel/master/src/main/java/com/google/devtools/build/lib/analysis/Runfiles.java
- https://raw.githubusercontent.com/bazelbuild/bazel/master/tools/test/test-setup.sh
- https://raw.githubusercontent.com/bazelbuild/bazel/master/src/test/py/bazel/windows_remote_test.py
- https://github.com/bazelbuild/bazel/pull/23912
- https://docs.engflow.com/re/client/windows.html
- https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-fsctl_set_reparse_point
- https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-_file_rename_information
- https://learn.microsoft.com/en-gb/windows/win32/fileio/reparse-point-operations
- https://raw.githubusercontent.com/rust-lang/rust/master/library/std/src/sys/fs/windows.rs
- https://raw.githubusercontent.com/tesuji/junction/master/src/internals.rs
- https://developer.apple.com/documentation/security/updating-mac-software
- https://developer.apple.com/forums/thread/689327
