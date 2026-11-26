# Research: relocatable native launchers and runfiles discovery (prior art)

Date: 2026-11-26 · Expires: 2027-02-26 · Triggered by: hex-architect high, rules_ocx under remote execution · Domain: launcher

Axis: technology/tooling. Web research by a researcher worker; sources read through fetches, several partial. Items marked (memory) are unverified. Not checked: `runfiles.bash`, the C++ and Java lookup code, Chocolatey, Volta, mise and rustup specifics, the `makeBinaryWrapper` docs, the Windows launcher byte layout, any macOS hardlink or rename behaviour.

## 1. Bazel runfiles conventions

From the rules_rust crate source ([runfiles.rs](https://raw.githubusercontent.com/bazelbuild/rules_rust/main/rust/runfiles/runfiles.rs)), `Runfiles::create()` resolves in this order:

1. An explicit `directory()` or `manifest()` on the builder.
2. `RUNFILES_MANIFEST_FILE`, if set and non-empty.
3. `RUNFILES_DIR`, if it is a directory.
4. `TEST_SRCDIR`, if it is a directory.
5. `argv[0]`: a sibling `<name>.runfiles`, then ancestors whose name ends in `.runfiles`, then follow symlinks and repeat; a `MANIFEST` file in the directory is read as a manifest.
6. `<argv0>.runfiles_manifest`; for a bare `argv[0]`, next to `current_exe()`.
7. Otherwise an error.

- `_repo_mapping` lines are `source_repo,apparent_name,canonical_repo`; `rlocation_from` maps the first path segment.
- The environment variables are exported to children, so nested tools reuse the parent's tree.
- Other languages share the shape (memory). `JAVA_RUNFILES` and `PYTHON_RUNFILES` are legacy aliases.
- Bazel's Windows launcher ([launcher.h](https://bazel.googlesource.com/bazel/+/HEAD/src/tools/launcher/launcher.h), [launcher_main.cc](https://bazel.googlesource.com/bazel/+/HEAD/src/tools/launcher/launcher_main.cc)) derives `<launcher>.exe.runfiles`, expects `<name>.runfiles/MANIFEST` or `<name>.runfiles_manifest`, and reads per-target launch info ([LauncherFileWriteAction](https://bazel.googlesource.com/bazel/+/8a2f09c0290fd39d612390d2ccec1cc2d770f0ea)). From memory the info is appended to the PE image, so bytes differ per target.
- [bazel.build/docs/windows](https://bazel.build/docs/windows): runfiles symlinks need `--enable_runfiles` and `--windows_enable_symlinks`; otherwise runfiles are manifest-only.

## 2. Own-path APIs

| Platform | `argv[0]` | OS own-path API |
|---|---|---|
| Linux | As the parent passed it; a bare name when started through `PATH` | `/proc/self/exe` resolves symlinks ([Rust unix os.rs](https://doc.rust-lang.org/src/std/sys/pal/unix/os.rs.html)) |
| macOS | Same | `_NSGetExecutablePath` returns the path as exec'd, not symlink-resolved (memory) |
| Windows | The command-line token | `GetModuleFileNameW` returns the path used to load; no symlink resolution (memory) |

`argv[0]` is the only Linux source that keeps the symlink path, and only when the parent passed it faithfully. A bare name needs a `PATH` search. [std::env::current_exe](https://doc.rust-lang.org/std/env/fn.current_exe.html) warns against using the result for security decisions.

## 3. Shim plus sidecar prior art

- [Scoop Shim](https://github.com/ScoopInstaller/Shim): one `shim.exe` copied and renamed, reads `<name>.shim` in its own directory; identical bytes, name is the key.
- [uv-trampoline](https://github.com/astral-sh/uv/tree/main/crates/uv-trampoline): payload in PE resources; bytes differ per target.
- Chocolatey shimgen (memory): per-target exe plus `.ignore`/`.gui` sidecars.
- pnpm and npm `cmd-shim` (memory): text scripts with `%~dp0` / `$basedir` relative paths.
- rustup (memory): one binary hardlinked under each name, dispatch on `argv[0]`, no sidecar.
- Volta and mise (memory): one binary dispatched by name, data from global config.
- Nix `makeBinaryWrapper` (memory): generated C per wrapper with absolute store paths; `--inherit-argv0`, `--resolve-argv0`.

Only Scoop and rustup keep the binary identical; Scoop is the one with a sidecar named after the binary.

## 4. macOS signatures

- The linker's ad-hoc signature is enough to run, on other machines too ([Go issue 42684](https://golang.org/issue/42684)).
- Bytes modified after signing are killed ("Code Signature Invalid"; [LLVM thread](https://discourse.llvm.org/t/ad-hoc-code-signature-anomaly-on-arm64-macos/57628), [GNU #44774](https://debbugs.gnu.org/db/44/44774.html)).
- Hardlink, copy, rename: no source found. From memory the file name is not covered by the signature. Unproven; needs a test on arm64.
- `com.apple.quarantine` is an xattr and triggers Gatekeeper, not a kill by itself.
- Reported but unexplained: [Apple forum 697361](https://developer.apple.com/forums/thread/697361) (a copied binary killed), [849002](https://developer.apple.com/forums/thread/849002).

## 5. Environment application semantics

`makeBinaryWrapper` (memory): `--set`, `--set-default`, `--prefix`, `--suffix`, `--unset`; no dedup, so nested wrappers stack duplicates.

## Implications for a launcher lookup order

1. Own path: the unresolved path it was started by — `argv[0]` when it holds a separator, else a `PATH` search; `GetModuleFileNameW` on Windows. Not `/proc/self/exe` first: it resolves symlinks.
2. Sidecar `<own-dir>/<name>.<ext>`, the name from step 1's basename, as Scoop does.
3. Expand the root token against the sidecar's directory, never the working directory, with no symlink resolution.
4. Under Bazel the sidecar and the files land in one runfiles tree; on manifest-only Windows runfiles, resolve through `RUNFILES_MANIFEST_FILE`.
5. A missing sidecar is a hard error that names the paths tried.
6. Apply the environment after the root is known, with idempotent prepend.
