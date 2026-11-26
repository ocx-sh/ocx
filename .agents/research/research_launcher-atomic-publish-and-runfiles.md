# Research: launcher construction, atomic publish and the runfiles lookup

Date: 2026-11-26 · Expires: 2027-02-26 · Triggered by: owner questions in the goal loop `.agents/goals/bazel-rbe-rules-ocx.md` · Domain: launcher

Five read-only workers ran on this host (Linux x86_64) on 2026-11-26. Two traced the ocx code, one prototyped with Bazel 9.2.0, two read documentation and other projects' source. Every statement carries a tag: `read` (code or record read, with `path:line`), `observed` (a command was run here), `documented` (a primary source says so), `secondhand` (a report), `not established`. Nothing ran on Windows or macOS.

## Questions

1. How are launchers built and published today, and how can construction and refresh leave no broken intermediate state by design?
2. Is there a better approach under Bazel than the launcher's runfiles lookup?

## 1. Construction today, per writer (`read`)

| Writer | Sequence | Publishing operation | States a reader or a crash can see |
|---|---|---|---|
| Package entry points, `launcher::generate` from `tasks/pull.rs:696-707` | Runs in the locked temp directory, before `move_temp_to_object_store` (`pull.rs:737`, `960`). Unix: truncating write, then `chmod 755` (`generate.rs:232-249`). Windows: `hardlink::create(blob, NAME.exe)`, then `NAME.shim` (`generate.rs:255-264`). | `move_dir(temp, packages/...)`. With an existing destination it does `remove_dir_all` first (`crates/ocx_util/src/fs.rs:48-63`). | Whole package or none. A re-run on an existing directory rewrites a Unix script in place and fails on Windows with `AlreadyExists` (`hardlink.rs:71-77`). |
| Deferred tools, `prepare_lazy.rs:63-98` | Returns an existing `shims/.../bin` as it is (`:66`). Else stages a tree in `temp/`, writes `digest`, `refs/blobs`, then `bin/` last. | `publish_shim_dir` renames the staged tree and never deletes a destination (`:237-269`). | No tree or a whole tree. The staging directory has no lock file, so `ocx clean` treats it as an orphan and can delete it during the build (`temp_store.rs:157-161`). |
| Toolchain trampolines, `render_toolchain.rs:205-308`, `445-511` | Under `lock_scoped("toolchain-render")`: heal `active` first (`:1483-1512`), then write each entry into `shells/default/bin` in place, prune, switch links, write the stamp last. Unix entry: temp file plus `persist_temp_file`. Windows entry: `NAME.exec` first, then `hardlink::update` (`:1725-1746`). | None. The directory is edited entry by entry. | A partial directory behind `active/bin`; on Windows a new `.exec` beside an old `.exe`, or a missing `.exe`, because `hardlink::update` is `remove_file` then `hard_link` (`hardlink.rs:91-99`). |
| Vendored tree, `vendor.rs:130-154` | `StagedTree` beside the target, launcher blob then record per name, `manifest.json` last. | `rename`; on Windows `remove_dir` then `rename` (`vendor_layout.rs:195-216`). | Absent or empty target, then whole. |

Further facts (`read`):

- The render stamp records per file `{size, file_id, content_hash}` and the prompt gate compares the disk with the previous stamp, never with what the running `ocx` would render (`state_store.rs:31-65`, `activation.rs:772-830`). An `ocx` upgrade leaves trampolines stale with a matching stamp.
- Render runs on `ocx pull`, `add`, `remove`, `lock`, `update`, `upgrade` only. `ocx self update` and `ocx self setup` render nothing; setup selects the new binary through the `current` link (`reference_manager.rs:96`).
- `symlink::replace_atomic` is an atomic rename on Unix. On Windows it removes the junction, then renames, and documents a "brief window with no entry" (`crates/ocx_util/src/fs/symlink.rs:171-178`, `220-288`).
- No helper exchanges two directories or replaces a non-empty one.
- Paths that outlive a command: `.../toolchain/active/bin` and the `ocx` install directory are written into `HKCU\Environment`, `environment.d` and a LaunchAgent (`ocx_setup/src/lib.rs:488-491`, `session_path.rs:40-48`). Package and deferred-tool launcher directories are recomputed by the composer on every use (`composer.rs:375`, `594`, `622`, `938-942`, `996`).
- `ocx clean` reads nothing inside a package directory except `content` and `refs`, so a second launcher directory there lives and dies with the package. `toolchain/` is outside the collection graph. A deferred-tool directory is classified by its `bin/` child (`shim_store.rs:92-102`).
- `generate_is_idempotent` (`generate.rs:421-433`) discards both results and asserts nothing.

## 2. Launcher files today (`read`)

| Kind | Body or fields | What starts |
|---|---|---|
| Unix package entry point (script) | `exec "${OCX_BINARY_PIN:-ocx}" launcher exec 'ROOT' -- "${0##*/}" "$@"` (`body.rs:15`) | `ocx launcher exec` |
| Unix deferred tool (script) | the same with `launcher shim 'PINNED ID' --` (`body.rs:29`) | `ocx launcher shim` |
| Unix trampoline (script) | marker line, `unset OCX_GLOBAL OCX_PROJECT`, baked `ocx` path, `--project 'ROOT' exec --` or `--global exec --` (`body.rs:74`) | `ocx exec` |
| Windows `.shim`, `.shimref`, `.exec` | one-line text: package root; pinned identifier; home selector plus an optional `ocx.exe` path (`body.rs:103-186`) | the native launcher builds the same command lines (`ocx_shim/src/core.rs:407`) |
| Vendored `.direct` | JSON `DirectSidecar`, `deny_unknown_fields`, no `kind` key (`ocx_env_apply/src/direct.rs:23`) | the program directly, no `ocx` |

- Windows homes also hold the Unix script beside each `.exe` for package and deferred launchers (`generate.rs:48-66`, `prepare_lazy.rs:154-165`).
- The scripts treat an empty `OCX_BINARY_PIN` as unset; the native launcher treats it as a pin and fails (`core.rs:366-370`).
- Trampoline detection, the self-recursion guard of every exec, reads line 2 of the script on POSIX and looks for a sibling `.exec` on Windows (`ocx_config/src/env.rs:486-540`). A native trampoline on Unix has no signal yet.
- The embedded blobs cover Linux musl, macOS and Windows, x86_64 and aarch64 (`ocx_store/src/shim.rs:28-52`). `ShimBinStore::ensure` already resolves a Linux or macOS blob, but its `path()` always appends `.exe`, and only Windows code calls it (`shim_bin_store.rs:100-109`). The arm64 macOS blob carries a linker signature, the x86_64 one none.
- Binding sentences in decision records: `adr_windows_exe_shim.md:573` (the sidecar is a one-way door), `adr_package_entry_points.md:16`, `:250` (old launchers outlive every upgrade; baking forces regeneration), `adr_toolchain_activation.md:160-176`, `:394-404`, `:474-479`, `:1037-1050` (body, `.exec`, stamp, closed depth-1 set), `adr_exec_env_model.md:77`, `:872`, `:917` (generated files never change what they read; refresh deferred to issue 679), `adr_self_update_handoff.md:34-57` (the hand-off runs the new binary's code), `system_design_vendor_rules_ocx.md:212`, `:321`, `:336` (record, lookup, accepted kinds).
- Test surface: `launcher/body.rs` 54 unit tests, `generate.rs` 36, `render_toolchain.rs` 116, `ocx_shim/src/main.rs` 111, `ocx_shim/src/direct.rs` 45; acceptance modules `test_windows_shim.py`, `test_entrypoints.py`, `test_entrypoints_crossplat.py`, `test_launcher_exec.py`, `test_trampoline_exec.py`, `test_lazy_loading.py`, `test_toolchain_render.py`, `test_toolchain_cli.py`, `test_toolchain_activate.py`, `test_vendor*.py`, `test_exec_env_compat.py` (replays real 0.6.5 bodies).

## 3. Operating-system primitives

| Fact | Tag | Source |
|---|---|---|
| Linux `renameat2(RENAME_EXCHANGE)` swaps two paths atomically, directories included; `rename` over a symlink replaces it with no gap. | documented | https://man7.org/linux/man-pages/man2/rename.2.html |
| macOS `renamex_np(RENAME_SWAP)` swaps atomically where the file system reports the capability. | documented | https://manpagez.com/man/2/renamex_np/osx-10.12.3.php |
| macOS caches a code signature per file; replace a signed binary with a new file, never in place. | documented | https://developer.apple.com/forums/thread/689327 |
| Windows `MoveFileEx` with `MOVEFILE_REPLACE_EXISTING` reports an error for a directory destination. | documented | https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw |
| Windows has no exchange of two directories; Transactional NTFS is deprecated. | not established, documented | https://learn.microsoft.com/en-us/windows/win32/fileio/deprecation-of-txf |
| `FSCTL_SET_REPARSE_POINT` on a directory that already carries a reparse point with the same tag overwrites the data in place; a different tag fails. What a concurrent lookup sees is not stated. | documented (read through a summary) | https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-fsa/4aeefef8-92c3-4abc-af7a-a610caf8a165 |
| A running `.exe` can be renamed. It cannot be deleted or overwritten the classic way (`STATUS_CANNOT_DELETE` for a mapped image). | documented, secondhand | https://learn.microsoft.com/en-us/windows-hardware/drivers/ifs/executable-images |
| On Windows 10 1709 or later a non-final hardlink of a running image can be deleted with `FileDispositionInfoEx`. | secondhand | https://github.com/OSRDrivers/deleteex |
| Renaming a directory fails while a file inside is open. | secondhand | https://www.python.org/pipermail/python-dev/2018-July/154806.html |
| Rust `std::fs::rename` on Windows does not give POSIX semantics reliably across versions. | read in source and issue | https://github.com/rust-lang/rust/issues/137499 |

## 4. Prior art for refreshing shims

| Tool | Shim | Refresh |
|---|---|---|
| rustup | symlinks or hardlinks to the one binary, no data file | relinks each proxy with remove then link |
| mise | symlink on Unix; copy, hardlink or script on Windows | staging directory, then one rename per file; accepts a mix of old and new after an interruption |
| Volta, pyenv, npm `cmd-shim`, Scoop | script or copied exe, Scoop with a `.shim` text file | delete then create, or write in place |
| proto | one shim binary plus a `registry.json` it reads | recreated when the shim version differs |
| Nix profiles | a generation directory behind one symlink | new generation built first, then one `rename` over the link |

Only the generation design rules out a mixed state by construction, and its guarantee is documented for Linux and macOS. No reviewed tool replaces launcher and data file as one unit on Windows.

## 5. Bazel (9.2.0)

Setup (`observed`): the dev build `0.6.8-pr784+20261126125712`, `--ignore_all_rc_files`, a scratch output root, `--lockfile_mode=off`. Sets: jq 1.8.1 (one file), node 24.21.0 (4,797 files, 3 links), a 15-package project set.

- Runfiles tree of one tool: 23 entries for jq, 39 for node, 147 for the project set. Directory labels keep it small. Five entries sit directly under `TOOL.runfiles/`.
- Launcher start, traced: 14 file system calls through the hub path, 7 at the in-tree path. The lookup lists one directory level and reads one small file per entry.
- 300 runs of `--version`, median: jq 1,140 µs through the hub, 1,059 µs in-tree, 489 µs direct. For node the difference is inside the noise.
- In a `processwrapper-sandbox` action the set exists only under `TOOL.runfiles/`; `external/SET` is absent.
- `--noenable_runfiles`, local execution: exit 78, `no vendored root found`. Sandboxed: works. `--nobuild_runfile_links`: works.

| Alternative | Result (`observed`) |
|---|---|
| Rule on the toolchain, `ctx.actions.run(executable = launcher source file, inputs = set files)` | Works without a runfiles tree for jq and node. `npm` and `npx` fail with exit 74: their links are generated outputs and are missing in the source tree. |
| `DefaultInfo(executable = source file of another repository)` | Refused: `'executable' provided by an executable rule ... should be created by the same rule.` |
| `alias` with `select()` to the launcher source file | Fails in the sandbox (exit 78) unless the user also lists the set in `tools` or `srcs`. |
| `filegroup(srcs = [launcher], data = [set])` as a tool | The data is not staged in the sandbox. |
| `bazel run` on the source file label | Works, in the output base. |

Path of the real program, relative to the execroot: through the hub with runfiles the prefix is 101 characters for jq, in-tree it is 42. With the short layout the package part falls from 67 to 23.

Documentation facts:

- `--enable_runfiles` defaults to off on Windows in Bazel 7, 8 and 9 (`read` in `CoreOptions.java`, `BuildConfigurationValue.java`). A tree on Windows needs `--enable_runfiles` and symlinks, which need Developer Mode (`documented`, https://bazel.build/configure/windows). rules_ocx sets both in its own `.bazelrc` files and tells consumers to (`read`: `.bazelrc:8-9`, `README.md:19`, `docs/concepts/design.md:43-44`).
- Bazel's runfiles libraries look in this order: `RUNFILES_MANIFEST_FILE` and `RUNFILES_DIR`, then `ARGV0.runfiles/MANIFEST`, then `ARGV0.runfiles_manifest` (`read` in rules_cc `runfiles.cc`). The manifest line format on Windows is not established.
- Canonical repository names are documented as not an API (https://bazel.build/external/extension). No documented way lets a prebuilt program learn its repository directory at run time.
- rules_python, rules_nodejs, rules_java and rules_rust hand a consumer one file plus a `files` set. `native_binary` in bazel-skylib is the symlink-plus-runfiles form that rules_ocx uses (`read` in each ruleset's source).
- Source directories as inputs are tracked by default since Bazel 8.5.0 and reach remote execution since 8.3.0; the upload follows links and sends copies (`read` in the issue tracker and `MerkleTreeComputer.java`).

The recorded reasons in rules_ocx are thin: `.agents/adr/adr_0002_vendor_tool_sets.md` names the lookup at `:85` and `--omit-links` at `:82` without a reason; `ocx/private/set_rules.bzl:30-38` explains why links and restored files are declared in the set's own package.

## 6. Defects found on the way

- `ocx vendor --omit-links` refuses node 26.13.1: `binary corepack is expected at .../content/bin/corepack, which the package content does not hold as a file` (`observed`). The cause is unconfirmed; the worker suspects a declared binary that is a link.
- A tag without a digest fails in `ocx.package` of rules_ocx on this host: the extension runs `ocx vendor --manifest-only` without `--remote` (`observed`).
- `ocx clean` can delete a deferred-tool staging directory that is still being built (`read`, above).
- `generate_is_idempotent` asserts nothing (`read`, above).
- The stale-stamp gap after an `ocx` upgrade (`read`, above).

## 7. Conclusion of the orchestrator (not a measured fact)

- Publish, never edit: build every launcher directory complete in a staging directory and publish it with one rename; refresh by publishing a new directory and switching to it. Three of four writers already publish this way. The toolchain directory needs a new directory under `shells/` per render and a switch of `active`.
- Carry the launcher generation in the directory name and create the directory on first use, where deferred-tool launchers are created today. Old directories stay untouched.
- On Windows, retarget the junction in place and prove it in CI; keep today's remove-then-rename if the proof fails.
- Keep the runfiles lookup: only the runfiles tree holds the complete set, and its cost is 7 file system calls and about 75 µs. Add a lookup in the runfiles manifest for the case without a tree.

## Not established

- Every Windows and macOS behaviour above; none was run.
- Whether a junction retarget is atomic for a concurrent lookup, and whether it needs a privilege.
- Which manifest file name Bazel writes on Windows, and its line format there.
- Whether remote execution on Windows receives a runfiles tree.
- A vendored set with a Bazel-reserved file name started in-tree.
