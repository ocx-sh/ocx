// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Launcher generator: the Unix `.sh` launcher and the Windows `.exe` shim plus its `.shim` sidecar.

use std::path::{Path, PathBuf};

use tokio::task::JoinSet;

use ocx_package::metadata::entrypoint::Entrypoints;
use ocx_store::file_structure::{FileStructure, ShimBinStore};

#[cfg(windows)]
use super::body::shim_sidecar_body;
use super::body::{unix_launcher_body, utf8_baked_path};
use super::safety::LauncherSafeString;

/// Generates the launchers baking `pkg_root` for every entrypoint into `dest`; empty `entries`
/// does not create `dest`. `shim_bin` is used on Windows only.
///
/// # Errors
///
/// A failed write, a `pkg_root` that is not UTF-8 or holds a [`LauncherSafeString`]-unsafe
/// character, or a failed shim publish/hardlink (a cross-device `shim_bin` is not copied).
#[cfg_attr(not(windows), allow(unused_variables))]
pub async fn generate(
    pkg_root: &Path,
    entries: &Entrypoints,
    dest: &Path,
    shim_bin: &ShimBinStore,
) -> Result<(), crate::Error> {
    if entries.is_empty() {
        return Ok(());
    }

    // Not `to_string_lossy`: its U+FFFD passes `LauncherSafeString` and bakes a root that names nothing.
    let pkg_root_str = LauncherSafeString::new(utf8_baked_path(pkg_root)?)?;

    tokio::fs::create_dir_all(dest)
        .await
        .map_err(|e| crate::error::file_error(dest, e))?;

    let mut tasks: JoinSet<Result<(), crate::Error>> = JoinSet::new();

    for (name, _entry) in entries.iter() {
        let name = name.as_str();

        let unix_body = unix_launcher_body(&pkg_root_str);
        let unix_path = dest.join(name);
        tasks.spawn(write_unix_launcher(unix_path, unix_body));

        // No `.cmd` launcher: its `%*` re-parse is the injection vector this shim closes.
        #[cfg(windows)]
        {
            let exe_path = dest.join(format!("{name}.exe"));
            let sidecar_body = shim_sidecar_body(&pkg_root_str);
            let sidecar_path = dest.join(format!("{name}.shim"));
            tasks.spawn(write_shim_exe_then_sidecar(
                shim_bin.clone(),
                exe_path,
                sidecar_path,
                sidecar_body,
            ));
        }
    }

    while let Some(join_result) = tasks.join_next().await {
        match join_result.expect(
            "BUG: write_unix_launcher / write_launcher_file are panic-free — only the JoinSet host task can fault here",
        ) {
            Ok(()) => {}
            Err(err) => {
                tasks.abort_all();
                return Err(err);
            }
        }
    }

    Ok(())
}

/// The value of `OCX_BINARY_PIN` for children of the running ocx.
///
/// The installed path through `current` only when it is the binary running now and (Unix) the
/// chain from `$OCX_HOME` to it is owned by the effective uid or root and not group- or
/// world-writable; otherwise the running binary exactly. `None` when the running binary is
/// unknown; never an empty path (Windows `IF DEFINED` reads an empty value as set).
pub fn binary_pin_value(file_structure: &FileStructure, running: Option<&Path>) -> Option<PathBuf> {
    #[cfg(unix)]
    let chain_is_safe = |link: &Path, binary: &Path| {
        // SAFETY: `geteuid` takes no arguments, touches no memory, and always succeeds.
        let euid = unsafe { libc::geteuid() };
        unix_chain_is_safe(file_structure.root(), link, binary, euid, &native_owner_and_mode)
    };
    // The residual (a principal who can write `$OCX_HOME` can already replace package content) is named in `adr_exec_env_model.md` § Pin value function.
    #[cfg(not(unix))]
    let chain_is_safe = |_: &Path, _: &Path| true;
    pin_value(file_structure, running?, &chain_is_safe)
}

fn pin_value(
    file_structure: &FileStructure,
    running: &Path,
    chain_is_safe: &dyn Fn(&Path, &Path) -> bool,
) -> Option<PathBuf> {
    if running.as_os_str().is_empty() {
        return None;
    }
    let installed = file_structure
        .ocx_install_bin_path()
        .join(if cfg!(windows) { "ocx.exe" } else { "ocx" });
    let pinned_to_installed = match (dunce::canonicalize(running), dunce::canonicalize(&installed)) {
        (Ok(running_real), Ok(installed_real)) if running_real == installed_real => {
            // `<current>/content/bin/ocx`: the `current` link is three levels up.
            installed
                .ancestors()
                .nth(3)
                .is_some_and(|link| chain_is_safe(link, &installed_real))
        }
        _ => false,
    };
    Some(if pinned_to_installed {
        installed
    } else {
        running.to_path_buf()
    })
}

/// Owner and permission bits of `path` itself (a symlink is not followed): the injectable probe.
#[cfg(unix)]
type OwnerAndMode<'a> = &'a dyn Fn(&Path) -> Option<(u32, u32)>;

#[cfg(unix)]
fn native_owner_and_mode(path: &Path) -> Option<(u32, u32)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path).ok().map(|m| (m.uid(), m.mode()))
}

/// Whether the `current` link, the resolved binary and every directory from `root` down to
/// both belong to `euid` or root, and the binary and directories are not group- or
/// world-writable. A link's own mode is meaningless (0777 on Linux), so only its owner counts.
/// Any path the probe cannot read, or outside `root`, fails the check.
#[cfg(unix)]
fn unix_chain_is_safe(root: &Path, link: &Path, binary: &Path, euid: u32, probe: OwnerAndMode<'_>) -> bool {
    let owned = |uid: u32| uid == euid || uid == 0;
    let owned_and_closed = |path: &Path| probe(path).is_some_and(|(uid, mode)| owned(uid) && mode & 0o022 == 0);

    if !probe(link).is_some_and(|(uid, _)| owned(uid)) || !owned_and_closed(binary) {
        return false;
    }
    let Ok(real_root) = dunce::canonicalize(root) else {
        return false;
    };
    [(link, root), (binary, real_root.as_path())]
        .into_iter()
        .all(|(leaf, top)| {
            if !leaf.starts_with(top) {
                return false;
            }
            for dir in leaf.ancestors().skip(1) {
                if !owned_and_closed(dir) {
                    return false;
                }
                if dir == top {
                    break;
                }
            }
            true
        })
}

/// Resolves the absolute `ocx` a rendered trampoline re-enters through: the install tree's
/// binary (it floats across `ocx self update`), else [`std::env::current_exe`], else `None`
/// (`adr_toolchain_activation.md` § "Rationale from code: launcher trampolines").
pub(crate) async fn trampoline_ocx_binary(file_structure: &FileStructure) -> Option<PathBuf> {
    // Only via `ocx_install_bin_path`: a second spelling drifts from `setup`'s and the per-prompt repair then churns PATH every prompt.
    let installed = file_structure
        .ocx_install_bin_path()
        .join(if cfg!(windows) { "ocx.exe" } else { "ocx" });
    let running = std::env::current_exe()
        .inspect_err(|e| log::debug!("Could not resolve the running ocx binary: {e}"))
        .ok();
    first_bakeable_ocx(installed, running).await
}

/// The first candidate that is absolute and exists; a test seam for [`trampoline_ocx_binary`].
///
/// A relative winner would name an arbitrary file from the body's working directory, and a
/// baked path to a missing binary fails worse than the bare-name `PATH` lookup.
async fn first_bakeable_ocx(installed: PathBuf, running: Option<PathBuf>) -> Option<PathBuf> {
    for candidate in [Some(installed), running].into_iter().flatten() {
        if !candidate.is_absolute() {
            continue;
        }
        if tokio::fs::try_exists(&candidate).await.unwrap_or(false) {
            return Some(candidate);
        }
    }
    None
}

async fn write_launcher_file(path: PathBuf, body: String) -> Result<(), crate::Error> {
    tokio::fs::write(&path, body.as_bytes())
        .await
        .map_err(|e| crate::error::file_error(&path, e))
}

/// Writes the Unix launcher and sets its executable bit (no-op off Unix).
async fn write_unix_launcher(path: PathBuf, body: String) -> Result<(), crate::Error> {
    write_launcher_file(path.clone(), body).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .await
            .map_err(|e| crate::error::file_error(&path, e))?;
    }
    Ok(())
}

/// Hardlinks the shared shim blob to `exe_path`, then writes the sidecar.
///
/// One task, not two: a split lets a fault leave a `.shim` without its `.exe`.
#[cfg(windows)]
async fn write_shim_exe_then_sidecar(
    shim_bin: ShimBinStore,
    exe_path: PathBuf,
    sidecar_path: PathBuf,
    sidecar_body: String,
) -> Result<(), crate::Error> {
    let shim_bin_path = shim_bin.ensure().await?;
    ocx_store::hardlink::create(&shim_bin_path, &exe_path)?;
    write_launcher_file(sidecar_path, sidecar_body).await
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use std::collections::BTreeMap;

    use ocx_package::metadata::entrypoint::{Entrypoint, EntrypointName, Entrypoints};
    use ocx_store::file_structure::ShimBinStore;

    fn entries_for(names: &[&str]) -> Entrypoints {
        let map: BTreeMap<EntrypointName, Entrypoint> = names
            .iter()
            .map(|n| (EntrypointName::try_from(*n).unwrap(), Entrypoint::default()))
            .collect();
        Entrypoints::new(map)
    }

    /// A scratch `ShimBinStore` rooted under the test's own tempdir — never
    /// shared with a real `FileStructure`. `generate()` only reaches into it
    /// on Windows (`#[cfg(windows)]`), so on other platforms this is unused
    /// beyond satisfying the signature.
    fn shim_bin_store(tmp: &tempfile::TempDir) -> ShimBinStore {
        ShimBinStore::new(tmp.path().join("shim_bin"))
    }

    // ── Empty Entrypoints — no files generated ────────────────────────────

    #[tokio::test]
    async fn generate_empty_entrypoints_creates_nothing() {
        let tmp = tempdir().unwrap();
        let pkg_root = tmp.path().join("pkg");
        let dest = pkg_root.join("entrypoints");
        let entrypoints = entries_for(&[]);
        let shim_bin = shim_bin_store(&tmp);

        super::generate(&pkg_root, &entrypoints, &dest, &shim_bin)
            .await
            .unwrap();
        assert!(
            !dest.exists(),
            "entrypoints/ dir must not be created for empty entrypoints"
        );
    }

    // ── Generator writes files for non-empty entrypoints ──────────────────

    #[tokio::test]
    async fn generate_writes_unix_and_windows_launchers() {
        let tmp = tempdir().unwrap();
        let pkg_root = tmp.path().join("pkg");
        tokio::fs::create_dir_all(pkg_root.join("content")).await.unwrap();
        let dest = pkg_root.join("entrypoints");
        let entrypoints = entries_for(&["cmake"]);
        let shim_bin = shim_bin_store(&tmp);

        super::generate(&pkg_root, &entrypoints, &dest, &shim_bin)
            .await
            .unwrap();
        assert!(dest.join("cmake").exists(), "unix launcher must be created");
        // Cutover: no `.cmd` is ever emitted on any platform. On Windows the
        // launcher is `<name>.exe` + `<name>.shim` (asserted by the
        // Windows-gated tests below); on non-Windows only `<name>` exists.
        assert!(
            !dest.join("cmake.cmd").exists(),
            "`.cmd` launcher must NOT be created (cutover to `.exe`-only)"
        );
    }

    // ── Unix file mode 0755 ───────────────────────────────────────────────

    #[cfg(unix)]
    #[tokio::test]
    async fn generate_sets_unix_mode_0755() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempdir().unwrap();
        let pkg_root = tmp.path().join("pkg");
        tokio::fs::create_dir_all(pkg_root.join("content")).await.unwrap();
        let dest = pkg_root.join("entrypoints");
        let entrypoints = entries_for(&["cmake"]);
        let shim_bin = shim_bin_store(&tmp);

        super::generate(&pkg_root, &entrypoints, &dest, &shim_bin)
            .await
            .unwrap();
        let mode = std::fs::metadata(dest.join("cmake")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755, "unix launcher must be mode 0755");
    }

    // ── Path traversal defense-in-depth (EntrypointName) ─────────────────

    #[test]
    fn entrypoint_name_rejects_path_traversal_bytes() {
        // EntrypointName construction must reject names with / or \ or ..
        // (slug regex already excludes / and \; .. is caught by the regex too)
        assert!(EntrypointName::try_from("../evil").is_err());
        assert!(EntrypointName::try_from("sub/path").is_err());
        assert!(EntrypointName::try_from("back\\slash").is_err());
    }

    // ── Idempotent re-run (generating same entrypoints twice) ─────────────

    #[tokio::test]
    async fn generate_is_idempotent() {
        let tmp = tempdir().unwrap();
        let pkg_root = tmp.path().join("pkg");
        tokio::fs::create_dir_all(pkg_root.join("content")).await.unwrap();
        let dest = pkg_root.join("entrypoints");
        let entrypoints = entries_for(&["cmake"]);
        let shim_bin = shim_bin_store(&tmp);

        // First call
        let _ = super::generate(&pkg_root, &entrypoints, &dest, &shim_bin).await;
        // Second call — must not error due to already-existing files
        let _ = super::generate(&pkg_root, &entrypoints, &dest, &shim_bin).await;
        // Both calls should return Ok(()).
    }

    // ── W1: pkg_root rejection on launcher-unsafe characters ──────────────
    //
    // `generate()` validates `pkg_root`'s text against
    // `LauncherSafeString::new` before any file is written. A path containing
    // any character in `LAUNCHER_UNSAFE_CHARS` (e.g. `'`, `%`, `"`, `\n`,
    // `\r`, `\0`) must surface the rejection and never produce a launcher with
    // an unsafe pkg_root baked in. This pins the contract that the unsafe-char
    // check fires once at the entry boundary in `generate()` rather than per
    // platform body.
    #[tokio::test]
    async fn generate_rejects_pkg_root_with_unsafe_character() {
        let tmp = tempdir().unwrap();
        // Single-quote `'` is in LAUNCHER_UNSAFE_CHARS — would break Unix
        // single-quoted shell literals if it slipped through.
        let pkg_root = tmp.path().join("pkg'with'quote");
        tokio::fs::create_dir_all(pkg_root.join("content")).await.unwrap();
        let dest = pkg_root.join("entrypoints");
        let entrypoints = entries_for(&["cmake"]);
        let shim_bin = shim_bin_store(&tmp);

        let err = super::generate(&pkg_root, &entrypoints, &dest, &shim_bin)
            .await
            .expect_err("pkg_root with unsafe character must be rejected");
        match err {
            crate::Error::LauncherUnsafeCharacter { character, .. } => {
                assert_eq!(character, '\'', "must report the offending single quote");
            }
            other => panic!("expected LauncherUnsafeCharacter, got {other:?}"),
        }
        // No launcher files must have been written when validation fails at
        // the entry boundary.
        assert!(
            !dest.join("cmake").exists(),
            "no unix launcher must be written when pkg_root rejected"
        );
        assert!(
            !dest.join("cmake.cmd").exists(),
            "no `.cmd` is ever emitted (cutover); also absent on rejection"
        );
        // The unsafe-char rejection at the `LauncherSafeString` boundary
        // (`generate()` entry) fires BEFORE any of the Windows writes are
        // spawned — so `.exe` and `.shim` must also be absent (the sidecar
        // reuses the same `LauncherSafeString`, no second validator; ADR
        // Contract 2 §Edge cases).
        assert!(
            !dest.join("cmake.exe").exists(),
            "no `.exe` shim must be written when pkg_root rejected at the entry boundary"
        );
        assert!(
            !dest.join("cmake.shim").exists(),
            "no `.shim` sidecar must be written when pkg_root rejected at the entry boundary"
        );
    }

    /// The **third door** onto a baked path: the package root every ordinary launcher bakes.
    ///
    /// The trampoline's two doors are guarded in `body.rs`; this is the one an
    /// `ocx package install` reaches. `to_string_lossy` here would substitute
    /// U+FFFD, sail through `LauncherSafeString` (U+FFFD is not in its
    /// rejection set) and bake a package root naming nothing — into the `.sh`
    /// launcher **and** the `.shim` sidecar, which share the one value.
    ///
    /// Unix-only because only there can a path be arbitrary bytes; on Windows
    /// the equivalent input is an unpaired surrogate, which no `PathBuf`
    /// constructor in safe Rust will build from a `&str`.
    #[cfg(unix)]
    #[tokio::test]
    async fn generate_rejects_a_pkg_root_that_is_not_utf8() {
        use std::os::unix::ffi::OsStrExt as _;

        let tmp = tempdir().unwrap();
        // 0x80 is a bare continuation byte — a valid path component, not valid
        // UTF-8, and lossily rendered as U+FFFD.
        let pkg_root = tmp.path().join(std::ffi::OsStr::from_bytes(b"pkg-\x80-not-utf8"));
        assert!(
            pkg_root.to_str().is_none(),
            "precondition: the fixture root really is un-decodable"
        );
        assert!(
            super::LauncherSafeString::new(pkg_root.to_string_lossy().into_owned()).is_ok(),
            "precondition: the lossy rendering passes the unsafe-character gate, which is why \
             the UTF-8 refusal has to exist at all"
        );
        if let Err(refused) = tokio::fs::create_dir_all(pkg_root.join("content")).await {
            // APFS validates filenames as UTF-8 and rejects these bytes
            // outright, so the input under test cannot exist on a macOS volume.
            // Observed, not assumed via `target_os`: the row still runs on any
            // filesystem that accepts the name, and a refusal for any other
            // reason (a vanished tempdir) reds here rather than passing as this
            // carve-out. Same shape as `index_store`'s two non-UTF-8 rows.
            assert!(
                tmp.path().exists(),
                "only the non-UTF-8 component may be refused; the tempdir must exist: {refused}"
            );
            eprintln!("skipped: this filesystem refuses a non-UTF-8 filename: {refused}");
            return;
        }
        let dest = pkg_root.join("entrypoints");
        let entrypoints = entries_for(&["cmake"]);
        let shim_bin = shim_bin_store(&tmp);

        let err = super::generate(&pkg_root, &entrypoints, &dest, &shim_bin)
            .await
            .expect_err("a non-UTF-8 pkg_root must be refused, not lossily baked");
        match err {
            crate::Error::LauncherPathNotUtf8 { path } => {
                assert!(
                    path.contains('\u{FFFD}'),
                    "the refusal carries the lossy rendering, the only printable form the \
                     value has: {path:?}"
                );
            }
            other => panic!("expected LauncherPathNotUtf8, got {other:?}"),
        }
        assert!(
            !dest.join("cmake").exists(),
            "and no launcher is written: the refusal fires at the entry boundary, before any \
             file is created"
        );
    }

    // ── Windows native shim emission (`.exe` + `.shim`, NO `.cmd`) ─────────
    //
    // Contract 2 postcondition (post-cutover): on Windows, each entry yields
    // THREE files — `<name>`, `<name>.exe`, `<name>.shim` — and explicitly
    // NO `<name>.cmd`. The `.exe` bytes must be BYTE-EQUAL to
    // `ocx_store::shim::SHIM_BYTES` (the Authenticode verbatim-copy property —
    // not mere existence; a transformed copy would invalidate the signature).
    // Gated `#[cfg(windows)]` because the `.exe` and `.shim` writes are
    // themselves cfg-gated and `SHIM_BYTES` is only non-empty on a Windows
    // target.

    #[cfg(windows)]
    #[tokio::test]
    async fn generate_emits_three_windows_files_no_cmd() {
        let tmp = tempdir().unwrap();
        let pkg_root = tmp.path().join("pkg");
        tokio::fs::create_dir_all(pkg_root.join("content")).await.unwrap();
        let dest = pkg_root.join("entrypoints");
        let entrypoints = entries_for(&["cmake"]);
        let shim_bin = shim_bin_store(&tmp);

        super::generate(&pkg_root, &entrypoints, &dest, &shim_bin)
            .await
            .unwrap();

        assert!(dest.join("cmake").exists(), "unix launcher must be created");
        assert!(
            dest.join("cmake.exe").exists(),
            "`.exe` shim must be created on Windows"
        );
        assert!(
            dest.join("cmake.shim").exists(),
            "`.shim` sidecar must be created on Windows"
        );
        assert!(
            !dest.join("cmake.cmd").exists(),
            "`.cmd` launcher must NOT be created (cutover to `.exe`-only — the \
             residual `%*` orphan is gone)"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn generate_exe_is_byte_equal_to_shim_bytes() {
        let tmp = tempdir().unwrap();
        let pkg_root = tmp.path().join("pkg");
        tokio::fs::create_dir_all(pkg_root.join("content")).await.unwrap();
        let dest = pkg_root.join("entrypoints");
        let entrypoints = entries_for(&["cmake"]);
        let shim_bin = shim_bin_store(&tmp);

        super::generate(&pkg_root, &entrypoints, &dest, &shim_bin)
            .await
            .unwrap();

        let written = tokio::fs::read(dest.join("cmake.exe")).await.unwrap();
        assert_eq!(
            written.as_slice(),
            ocx_store::shim::SHIM_BYTES,
            "`.exe` must be a verbatim copy of `ocx_store::shim::SHIM_BYTES` — the \
             Authenticode verbatim-copy property (any transform invalidates the \
             signature). This is byte-equality, NOT mere existence."
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn generate_shim_sidecar_is_pkg_root_plus_lf() {
        let tmp = tempdir().unwrap();
        let pkg_root = tmp.path().join("pkg");
        tokio::fs::create_dir_all(pkg_root.join("content")).await.unwrap();
        let dest = pkg_root.join("entrypoints");
        let entrypoints = entries_for(&["cmake"]);
        let shim_bin = shim_bin_store(&tmp);

        super::generate(&pkg_root, &entrypoints, &dest, &shim_bin)
            .await
            .unwrap();

        let sidecar = tokio::fs::read(dest.join("cmake.shim")).await.unwrap();
        let expected = format!("{}\n", pkg_root.to_string_lossy());
        assert_eq!(
            sidecar,
            expected.as_bytes(),
            "`.shim` must be exactly `format!(\"{{pkg_root}}\\n\")` (UTF-8, no BOM, single LF)"
        );
        assert!(
            !sidecar.starts_with(&[0xEF, 0xBB, 0xBF]),
            "`.shim` must not carry a UTF-8 BOM"
        );
    }

    /// The `.exe`/`.shim` JoinSet spawn order is NOT
    /// the write order — Contract 2's write-ordering postcondition requires
    /// the implementation to SEQUENCE the writes so `.exe` lands before
    /// `.shim`. The recoverable partial state is therefore
    /// `.exe` present / `.shim` ABSENT (recoverable by re-running
    /// `generate()`), never the reverse (a `.shim` without its `.exe` is the
    /// worse state). This test injects a failure on the SECOND shim write
    /// (the `.shim`) by pre-creating `cmake.shim` as a directory so the file
    /// write fails, then asserts the surviving state is `.exe` present,
    /// `.shim`-as-file absent — pinning the ordering as a real guarantee, not
    /// an accident of spawn order.
    #[cfg(windows)]
    #[tokio::test]
    async fn generate_half_state_is_exe_present_shim_absent() {
        let tmp = tempdir().unwrap();
        let pkg_root = tmp.path().join("pkg");
        tokio::fs::create_dir_all(pkg_root.join("content")).await.unwrap();
        let dest = pkg_root.join("entrypoints");
        tokio::fs::create_dir_all(&dest).await.unwrap();
        // Pre-create `cmake.shim` as a DIRECTORY: the sidecar file write must
        // then fail, simulating a mid-generate fault on the 2nd shim write.
        tokio::fs::create_dir_all(dest.join("cmake.shim")).await.unwrap();
        let entrypoints = entries_for(&["cmake"]);
        let shim_bin = shim_bin_store(&tmp);

        let result = super::generate(&pkg_root, &entrypoints, &dest, &shim_bin).await;
        assert!(
            result.is_err(),
            "generate() must surface the failed `.shim` write as an error"
        );

        // The recoverable half-state: the `.exe` shim is present (written
        // first), the `.shim` was NOT successfully written as a file (the
        // directory we pre-created is not a valid sidecar). This pins
        // `.exe`-before-`.shim` as a guarantee — never `.shim` without `.exe`.
        assert!(
            dest.join("cmake.exe").exists(),
            "`.exe` must be present after a failed `.shim` write (.exe written first)"
        );
        assert!(
            !dest.join("cmake.shim").is_file(),
            "the `.shim` must NOT exist as a valid file — recoverable state is \
             `.exe` present / `.shim` absent (E1), never the reverse"
        );
    }

    // ── non-Windows emits only the Unix `<name>` launcher ─────────────────
    //
    // On non-Windows the `.exe`/`.shim` emission is skipped via `cfg`, and
    // `ocx_store::shim::SHIM_BYTES` is empty. Post-cutover, no `.cmd` is emitted
    // on any platform either — this pins that only `<name>` exists off
    // Windows, and `.cmd`/`.exe`/`.shim` are all absent.

    #[cfg(not(windows))]
    #[tokio::test]
    async fn generate_emits_only_unix_launcher_off_windows() {
        let tmp = tempdir().unwrap();
        let pkg_root = tmp.path().join("pkg");
        tokio::fs::create_dir_all(pkg_root.join("content")).await.unwrap();
        let dest = pkg_root.join("entrypoints");
        let entrypoints = entries_for(&["cmake"]);
        let shim_bin = shim_bin_store(&tmp);

        super::generate(&pkg_root, &entrypoints, &dest, &shim_bin)
            .await
            .unwrap();

        assert!(dest.join("cmake").exists(), "unix launcher must still be created");
        assert!(
            !dest.join("cmake.cmd").exists(),
            "no `.cmd` launcher on any platform (cutover to `.exe`-only)"
        );
        assert!(
            !dest.join("cmake.exe").exists(),
            "no `.exe` shim off Windows (emission cfg-gated)"
        );
        assert!(
            !dest.join("cmake.shim").exists(),
            "no `.shim` sidecar off Windows (emission cfg-gated)"
        );
    }

    /// Off Windows no `.exe` is emitted, so [`ShimBinStore::ensure`]
    /// must never be reached — the store root stays absent. Without this, an
    /// implementation that publishes the blob unconditionally (before the
    /// `cfg`-gated per-entry work) would litter `$OCX_HOME/.bin/ocx-shim/`
    /// with a zero-byte `.exe` on every Linux/macOS install, and nothing else
    /// in this file would notice.
    #[cfg(not(windows))]
    #[tokio::test]
    async fn generate_does_not_touch_the_shim_bin_store_off_windows() {
        let tmp = tempdir().unwrap();
        let pkg_root = tmp.path().join("pkg");
        tokio::fs::create_dir_all(pkg_root.join("content")).await.unwrap();
        let dest = pkg_root.join("entrypoints");
        let entrypoints = entries_for(&["cmake"]);
        let shim_bin = shim_bin_store(&tmp);

        super::generate(&pkg_root, &entrypoints, &dest, &shim_bin)
            .await
            .unwrap();

        assert!(
            !shim_bin.root().exists(),
            "the shared shim blob store must not be created off Windows — no \
             `.exe` is emitted there, so nothing may publish into it"
        );
    }

    /// Each `<name>.exe` is a **hardlink** to the one blob in the
    /// shared store, not an independent copy — one inode, one Authenticode
    /// signature, one Defender scan regardless of how many names a package
    /// declares.
    ///
    /// Link identity is observed by mutating the store's blob in place
    /// (`fs::write` truncates the existing inode rather than replacing it) and
    /// reading the change back through every emitted `.exe`. Byte-equality
    /// alone cannot distinguish a hardlink from a copy — which is the whole
    /// point of #301 — and the Windows metadata APIs that expose a file index
    /// (`windows_by_handle`) are unstable, so this is the portable identity
    /// probe.
    #[cfg(windows)]
    #[tokio::test]
    async fn generate_hardlinks_every_exe_to_the_one_shared_store_blob() {
        const MUTATED: &[u8] = b"mutated-through-the-shared-inode";

        let tmp = tempdir().unwrap();
        let pkg_root = tmp.path().join("pkg");
        tokio::fs::create_dir_all(pkg_root.join("content")).await.unwrap();
        let dest = pkg_root.join("entrypoints");
        let names = ["cmake", "ctest", "cpack"];
        let entrypoints = entries_for(&names);
        let shim_bin = shim_bin_store(&tmp);

        super::generate(&pkg_root, &entrypoints, &dest, &shim_bin)
            .await
            .unwrap();

        // The store is flat and holds exactly one blob for the running arch,
        // however many names were generated.
        let mut blobs: Vec<std::path::PathBuf> = std::fs::read_dir(shim_bin.root())
            .expect("the shim blob store root must exist after generate()")
            .map(|entry| entry.expect("readable directory entry").path())
            .collect();
        blobs.sort();
        assert_eq!(
            blobs.len(),
            1,
            "one shared blob backs all {} names, never one per entry",
            names.len()
        );

        std::fs::write(&blobs[0], MUTATED).expect("the store blob must be writable in place");

        for name in names {
            let exe = dest.join(format!("{name}.exe"));
            let bytes = tokio::fs::read(&exe).await.unwrap();
            // `assert!` on the same equality rather than `assert_eq!`: a failing
            // `assert_eq!` renders the whole file as a byte array, and the file
            // is the megabyte-scale shim PE — enough output to truncate the CI
            // log at the point the diagnosis is needed. The length is the datum
            // that tells the two outcomes apart.
            assert!(
                bytes == MUTATED,
                "`{name}.exe` must share the store blob's inode — an in-place \
                 mutation of the blob is visible through a hardlink and \
                 invisible through a copy; read {} bytes, expected {}",
                bytes.len(),
                MUTATED.len()
            );
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn shim_bytes_empty_off_windows() {
        assert!(
            ocx_store::shim::SHIM_BYTES.is_empty(),
            "`ocx` must carry zero shim weight off Windows (SHIM_BYTES empty)"
        );
    }

    // ── The absolute `ocx` a trampoline re-enters through ──────────
    //
    // Every rung is driven to both a winning and a declining outcome, because
    // the whole value of the ladder is telling the rungs apart — a test that
    // only ever saw one outcome would be indistinguishable from a resolver
    // that always answered the same way. The two rungs that read the real
    // filesystem run against a real tempdir; the rung whose declining branch
    // no in-process test can produce (`current_exe()` failing) is driven
    // through the `first_bakeable_ocx` seam.

    /// The store layout the resolver must reproduce, derived the same way it
    /// derives it — never the literal string, which would drift.
    fn installed_ocx_path(file_structure: &ocx_store::file_structure::FileStructure) -> std::path::PathBuf {
        file_structure
            .symlinks
            .current(&ocx_oci::ocx_cli_identifier())
            .join("content")
            .join("bin")
            .join(if cfg!(windows) { "ocx.exe" } else { "ocx" })
    }

    /// Rung 1 declines on an empty `$OCX_HOME`, and rung 2 answers: the
    /// bare-binary population gets the absolute path of the ocx that is
    /// rendering, never a path to nothing under a store that was never built.
    #[tokio::test]
    async fn trampoline_ocx_binary_falls_back_to_the_running_binary_when_ocx_is_not_installed() {
        let tmp = tempdir().unwrap();
        let file_structure = ocx_store::file_structure::FileStructure::with_root(tmp.path().to_path_buf());
        let resolved = super::trampoline_ocx_binary(&file_structure)
            .await
            .expect("rung 2 answers whenever the test binary itself is on disk");

        assert_eq!(
            resolved,
            std::env::current_exe().expect("the test binary has a path"),
            "an empty OCX_HOME has no install tree, so the ladder must drop to the running \
             binary rather than bake a path to nothing"
        );
        assert!(
            !resolved.starts_with(tmp.path()),
            "…and must not answer anything under the store it just found empty: {resolved:?}"
        );
    }

    /// Rung 2 declining leaves the bare-name fallback — the only state in which
    /// the self-resolution loop is accepted.
    ///
    /// Driven through the [`super::first_bakeable_ocx`] seam because
    /// `std::env::current_exe()` cannot be made to fail in-process, and a rung
    /// whose declining branch is unreachable in test is a branch nobody has
    /// seen run.
    #[tokio::test]
    async fn no_path_is_bakeable_when_neither_the_install_tree_nor_a_running_binary_answers() {
        let tmp = tempdir().unwrap();
        assert!(
            super::first_bakeable_ocx(tmp.path().join("absent-ocx"), None)
                .await
                .is_none(),
            "with no install tree and no running-binary answer, the body must degrade to the \
             bare-name PATH lookup"
        );
    }

    /// Neither rung may bake a **relative** path: the body runs from an
    /// arbitrary working directory, where `ocx/bin/ocx` names an arbitrary
    /// file — the one failure a plain existence probe cannot see, since a
    /// relative candidate can exist perfectly well relative to the *test's*
    /// CWD.
    #[tokio::test]
    async fn a_relative_candidate_is_never_bakeable_even_when_it_exists() {
        let relative = std::path::PathBuf::from("Cargo.toml");
        assert!(
            tokio::fs::try_exists(&relative).await.unwrap_or(false),
            "precondition: the relative candidate resolves from the crate's CWD, so only the \
             absoluteness clause can refuse it"
        );

        assert!(
            super::first_bakeable_ocx(relative.clone(), None).await.is_none(),
            "rung 1 must refuse a relative install path"
        );
        assert!(
            super::first_bakeable_ocx(std::path::PathBuf::from("absent"), Some(relative))
                .await
                .is_none(),
            "…and rung 2 must refuse a relative running-binary path"
        );
    }

    /// The rungs are **ordered**, and the order is load-bearing: `current`
    /// floats across `ocx self update`, a baked `current_exe()` does not. With
    /// both candidates present and distinct, the install tree wins.
    #[tokio::test]
    async fn the_install_tree_outranks_the_running_binary_when_both_exist() {
        let tmp = tempdir().unwrap();
        let installed = tmp.path().join("installed-ocx");
        let running = tmp.path().join("running-ocx");
        tokio::fs::write(&installed, b"#!/bin/sh\n").await.unwrap();
        tokio::fs::write(&running, b"#!/bin/sh\n").await.unwrap();

        assert_eq!(
            super::first_bakeable_ocx(installed.clone(), Some(running)).await,
            Some(installed),
            "rung 1 must win while it answers — `ocx self update` swaps `current` and every \
             rendered trampoline follows, which a baked running path would not"
        );
    }

    #[tokio::test]
    async fn trampoline_ocx_binary_resolves_the_installed_path_under_the_symlink_store() {
        let tmp = tempdir().unwrap();
        let file_structure = ocx_store::file_structure::FileStructure::with_root(tmp.path().to_path_buf());
        let expected = installed_ocx_path(&file_structure);
        tokio::fs::create_dir_all(expected.parent().expect("the binary has a parent"))
            .await
            .unwrap();
        tokio::fs::write(&expected, b"#!/bin/sh\n").await.unwrap();

        assert_eq!(
            super::trampoline_ocx_binary(&file_structure).await,
            Some(expected.clone()),
            "the resolver must answer the store-derived path once it exists"
        );
        assert!(
            expected.starts_with(tmp.path().join("symlinks")),
            "and that path must sit under $OCX_HOME/symlinks, not somewhere invented: {expected:?}"
        );
        assert!(
            expected.ends_with(std::path::Path::new("current/content/bin").join(if cfg!(windows) {
                "ocx.exe"
            } else {
                "ocx"
            })),
            "…and end at `current/content/bin/<ocx>`, the same tail `ocx self setup` \
             computes: {expected:?}"
        );
    }

    /// A **dangling** `current` — the state a half-uninstalled
    /// tree is in. `try_exists` follows the link and answers `false`, so rung 1
    /// declines and the ladder drops to the running binary.
    ///
    /// The third state, and the one neither shipped row reaches: `current`
    /// exists as a directory entry, so a probe written with `symlink_metadata`
    /// — or with a bare `Path::exists()` on the link itself — would answer
    /// `true` and bake a path to nothing, which `/bin/sh` reports as a naked
    /// ENOENT.
    #[cfg(unix)]
    #[tokio::test]
    async fn trampoline_ocx_binary_skips_a_dangling_current() {
        let tmp = tempdir().unwrap();
        let file_structure = ocx_store::file_structure::FileStructure::with_root(tmp.path().to_path_buf());
        let current = file_structure.symlinks.current(&ocx_oci::ocx_cli_identifier());
        tokio::fs::create_dir_all(current.parent().expect("`current` has a parent"))
            .await
            .unwrap();
        std::os::unix::fs::symlink(tmp.path().join("collected-by-clean"), &current).expect("create a dangling link");

        assert!(
            tokio::fs::symlink_metadata(&current).await.is_ok(),
            "precondition: the link itself exists, which is what makes this distinct from \
             the not-installed row"
        );
        assert!(
            !tokio::fs::try_exists(&current).await.unwrap_or(false),
            "precondition: …and it resolves to nothing"
        );
        let resolved = super::trampoline_ocx_binary(&file_structure)
            .await
            .expect("rung 2 answers whenever the test binary itself is on disk");
        assert!(
            !resolved.starts_with(&current),
            "a dangling `current` is not an install tree — baking a path through it would \
             make every trampoline fail with a bare ENOENT from /bin/sh: {resolved:?}"
        );
        assert_eq!(
            resolved,
            std::env::current_exe().expect("the test binary has a path"),
            "…so the ladder must drop to the running binary instead"
        );
    }
}

#[cfg(test)]
mod pin_tests {
    use std::path::{Path, PathBuf};

    use ocx_store::file_structure::FileStructure;

    use super::*;

    struct Installed {
        _tmp: tempfile::TempDir,
        fs: FileStructure,
        /// `<current>/content/bin/ocx`, the path the pin names.
        installed: PathBuf,
        /// The binary as the package store holds it.
        real_binary: PathBuf,
        link: PathBuf,
    }

    /// An install tree where `current` points at a package holding an `ocx` binary.
    fn installed_tree() -> Installed {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap().join("home");
        let fs = FileStructure::with_root(root.clone());
        let installed = fs
            .ocx_install_bin_path()
            .join(if cfg!(windows) { "ocx.exe" } else { "ocx" });
        let link = installed.ancestors().nth(3).unwrap().to_path_buf();
        let pkg = root.join("packages").join("pkg");
        let real_binary = pkg.join("content").join("bin").join(installed.file_name().unwrap());
        std::fs::create_dir_all(real_binary.parent().unwrap()).unwrap();
        std::fs::write(&real_binary, b"").unwrap();
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        ocx_util::fs::symlink::create(&pkg, &link).unwrap();
        #[cfg(unix)]
        close_permissions(&root);
        Installed {
            _tmp: tmp,
            fs,
            installed,
            real_binary,
            link,
        }
    }

    /// The test must not depend on the host umask (a user-private-group umask of 002 would open every directory).
    #[cfg(unix)]
    fn close_permissions(dir: &Path) {
        use std::os::unix::fs::PermissionsExt;
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                close_permissions(&path);
            }
            if !meta.file_type().is_symlink() {
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn none_running_gives_none() {
        let t = installed_tree();
        assert_eq!(binary_pin_value(&t.fs, None), None);
    }

    #[test]
    fn empty_running_never_gives_an_empty_pin() {
        let t = installed_tree();
        assert_eq!(binary_pin_value(&t.fs, Some(Path::new(""))), None);
    }

    #[test]
    fn running_the_installed_binary_pins_the_stable_installed_path() {
        let t = installed_tree();
        // Running through the store path, as `current_exe` reports after the link resolves.
        assert_eq!(binary_pin_value(&t.fs, Some(&t.real_binary)), Some(t.installed.clone()));
        assert_eq!(binary_pin_value(&t.fs, Some(&t.installed)), Some(t.installed));
    }

    #[test]
    fn a_different_build_pins_exactly_the_running_path() {
        let t = installed_tree();
        let other = t.fs.root().parent().unwrap().join("dev-build").join("ocx");
        std::fs::create_dir_all(other.parent().unwrap()).unwrap();
        std::fs::write(&other, b"").unwrap();
        assert_eq!(binary_pin_value(&t.fs, Some(&other)), Some(other));
    }

    #[test]
    fn a_running_path_that_cannot_be_canonicalized_is_kept_exactly() {
        let t = installed_tree();
        let missing = PathBuf::from("/definitely/not/here/ocx");
        assert_eq!(binary_pin_value(&t.fs, Some(&missing)), Some(missing));
    }

    #[cfg(unix)]
    mod unix {
        use std::os::unix::fs::PermissionsExt;

        use super::*;

        fn euid() -> u32 {
            // SAFETY: `geteuid` takes no arguments, touches no memory, and always succeeds.
            unsafe { libc::geteuid() }
        }

        /// The real probe, except `foreign` reads as owned by someone who is neither root nor us.
        fn pin_with_foreign_owner(t: &Installed, foreign: &Path) -> Option<PathBuf> {
            let probe = |path: &Path| {
                native_owner_and_mode(path).map(|(uid, mode)| {
                    if path == foreign {
                        (euid() + 1, mode)
                    } else {
                        (uid, mode)
                    }
                })
            };
            let safe = |link: &Path, binary: &Path| unix_chain_is_safe(t.fs.root(), link, binary, euid(), &probe);
            pin_value(&t.fs, &t.real_binary, &safe)
        }

        #[test]
        fn the_unmodified_tree_passes_the_injected_probe() {
            let t = installed_tree();
            assert_eq!(
                pin_with_foreign_owner(&t, Path::new("/nowhere")),
                Some(t.installed.clone())
            );
        }

        #[test]
        fn a_foreign_owned_current_link_falls_back_to_running() {
            let t = installed_tree();
            assert_eq!(pin_with_foreign_owner(&t, &t.link), Some(t.real_binary.clone()));
        }

        #[test]
        fn a_foreign_owned_binary_falls_back_to_running() {
            let t = installed_tree();
            assert_eq!(pin_with_foreign_owner(&t, &t.real_binary), Some(t.real_binary.clone()));
        }

        #[test]
        fn a_foreign_owned_middle_directory_falls_back_to_running() {
            let t = installed_tree();
            let packages_dir = t.fs.root().join("packages").join("pkg").join("content");
            assert_eq!(pin_with_foreign_owner(&t, &packages_dir), Some(t.real_binary.clone()));
        }

        #[test]
        fn a_foreign_owned_directory_above_the_link_falls_back_to_running() {
            let t = installed_tree();
            let above_link = t.link.parent().unwrap().to_path_buf();
            assert_eq!(pin_with_foreign_owner(&t, &above_link), Some(t.real_binary.clone()));
        }

        #[test]
        fn root_owned_entries_count_as_trusted() {
            let t = installed_tree();
            let probe = |path: &Path| native_owner_and_mode(path).map(|(_, mode)| (0, mode));
            // A euid that owns nothing: only the root owner rule can make this pass.
            assert!(unix_chain_is_safe(
                t.fs.root(),
                &t.link,
                &dunce::canonicalize(&t.real_binary).unwrap(),
                euid() + 1,
                &probe
            ));
        }

        #[test]
        fn a_group_writable_parent_of_current_falls_back_to_running() {
            let t = installed_tree();
            let parent = t.link.parent().unwrap();
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o775)).unwrap();
            assert_eq!(
                binary_pin_value(&t.fs, Some(&t.real_binary)),
                Some(t.real_binary.clone())
            );
        }

        #[test]
        fn a_world_writable_binary_falls_back_to_running() {
            let t = installed_tree();
            std::fs::set_permissions(&t.real_binary, std::fs::Permissions::from_mode(0o757)).unwrap();
            assert_eq!(
                binary_pin_value(&t.fs, Some(&t.real_binary)),
                Some(t.real_binary.clone())
            );
        }

        #[test]
        fn a_path_outside_the_home_fails_the_chain() {
            let t = installed_tree();
            let outside = tempfile::tempdir().unwrap();
            let binary = outside.path().join("ocx");
            std::fs::write(&binary, b"").unwrap();
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert!(!unix_chain_is_safe(
                t.fs.root(),
                &t.link,
                &dunce::canonicalize(&binary).unwrap(),
                euid(),
                &native_owner_and_mode
            ));
        }
    }

    /// The `current` link is a junction on Windows; the canonical installed path must equal `current_exe`'s.
    #[cfg(windows)]
    #[test]
    fn windows_junction_install_pins_the_installed_path() {
        let t = installed_tree();
        assert_eq!(binary_pin_value(&t.fs, Some(&t.real_binary)), Some(t.installed.clone()));
        assert_eq!(binary_pin_value(&t.fs, Some(&t.installed)), Some(t.installed));
    }
}
