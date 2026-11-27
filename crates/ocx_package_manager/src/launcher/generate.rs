// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Launcher writers: every launcher is a byte copy of the embedded launcher image beside a `.launch` record.

use std::path::{Path, PathBuf};

use ocx_package::metadata::entrypoint::Entrypoints;
use ocx_store::file_structure::{FileStructure, PackageDir};
use ocx_store::launcher_generation::LauncherGeneration;
use ocx_util::fs::{Published, Retired, publish_dir, retire_dir, symlink};

use super::body::utf8_baked_path;
use super::direct::write_direct_launcher_for;
use super::safety::LauncherSafeString;
use crate::tasks::toolchain_names::is_record_name;
use crate::tasks::vendor::VendorError;
use ocx_env_apply::launch::{
    DirectRecord, LaunchKind, LaunchRecord, PackageRecord, RECORD_EXTENSION, SCHEMA_VERSION, is_fully_qualified,
};

/// Writes the launchers of a package's entry points into `dest`, each recording `pkg_root`; empty `entries` does not
/// create `dest`. `pkg_root` is the package's final directory, not the temp one it is written beside.
///
/// # Errors
///
/// A failed write, a host without a launcher blob, or a `pkg_root` that is not UTF-8 or holds a
/// [`LauncherSafeString`]-unsafe character, or one the target's launcher would refuse to read (a Windows target needs a
/// drive or UNC root).
pub async fn generate(pkg_root: &Path, entries: &Entrypoints, dest: &Path) -> Result<(), crate::Error> {
    if entries.is_empty() {
        return Ok(());
    }
    generate_for(pkg_root, entries, dest, host_launcher_blob(dest)?, cfg!(windows)).await
}

/// [`generate`] for a target: `windows` is the target's, `blob` its launcher image.
pub(super) async fn generate_for(
    pkg_root: &Path,
    entries: &Entrypoints,
    dest: &Path,
    blob: &[u8],
    windows: bool,
) -> Result<(), crate::Error> {
    if entries.is_empty() {
        return Ok(());
    }

    // Not `to_string_lossy`: its U+FFFD passes `LauncherSafeString` and records a root that names nothing.
    let package_root = LauncherSafeString::new(utf8_baked_path(pkg_root)?)?;
    // The launcher exits 78 on a root without a drive letter on Windows; a record it cannot read is never written.
    if !is_fully_qualified(package_root.as_str(), windows) {
        return Err(crate::error::file_error(
            dest,
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "package root '{}' is not fully qualified for the launcher target",
                    package_root.as_str()
                ),
            ),
        ));
    }
    let record = LaunchRecord::Package(PackageRecord {
        kind: LaunchKind::Package,
        schema_version: SCHEMA_VERSION,
        package_root: package_root.as_str().to_owned(),
    });
    super::validate_for_write(&record, windows, dest)?;

    tokio::fs::create_dir_all(dest)
        .await
        .map_err(|e| crate::error::file_error(dest, e))?;
    for (index, (name, _entry)) in entries.iter().enumerate() {
        if fault_before(index) {
            return Err(crate::error::file_error(dest, std::io::Error::other("injected fault")));
        }
        write_launcher(dest, name.as_str(), blob, &record, windows).await?;
    }
    Ok(())
}

#[cfg(test)]
tokio::task_local! {
    /// Fails [`generate_for`] before the launcher at this index, so a test sees a fault part-way through a directory.
    static FAULT_AFTER: usize;
}

/// Test seam: whether a fault is injected before the launcher at `index`; never true outside tests.
fn fault_before(index: usize) -> bool {
    #[cfg(test)]
    return FAULT_AFTER.try_with(|after| *after == index).unwrap_or(false);
    #[cfg(not(test))]
    {
        let _ = index;
        false
    }
}

/// Whether `dir` is a complete launcher directory for `entries`: a real directory (`lstat`, no link) holding, per
/// name that gets a launcher, `NAME[.exe]` and `NAME.launch` as regular files.
pub(crate) async fn launchers_complete(dir: &Path, entries: &Entrypoints) -> bool {
    names_complete(dir, entries.iter().map(|(name, _entry)| name.as_str())).await
}

/// [`launchers_complete`] for any set of claimed names.
pub(crate) async fn names_complete<'a>(dir: &Path, names: impl IntoIterator<Item = &'a str>) -> bool {
    async fn is_file(path: PathBuf) -> bool {
        tokio::fs::symlink_metadata(path).await.is_ok_and(|m| m.is_file())
    }
    if !tokio::fs::symlink_metadata(dir).await.is_ok_and(|m| m.is_dir()) {
        return false;
    }
    for name in names.into_iter().filter(|name| !is_record_name(name)) {
        let launcher = if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.to_owned()
        };
        if !is_file(dir.join(launcher)).await || !is_file(dir.join(format!("{name}.{RECORD_EXTENSION}"))).await {
            return false;
        }
    }
    true
}

/// The launcher generation of this host, hashed once; `None` on a host with no launcher blob.
pub(crate) fn host_generation() -> Option<&'static LauncherGeneration> {
    static HOST: std::sync::OnceLock<Option<LauncherGeneration>> = std::sync::OnceLock::new();
    HOST.get_or_init(LauncherGeneration::host).as_ref()
}

/// The directory of a package's entry-point launchers that a composition puts on `PATH`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EntryPointDir {
    /// `launchers/<G>`, this build's.
    Launchers,
    /// `entrypoints/`, written by an older release; never written by this one.
    Legacy,
}

impl EntryPointDir {
    /// The directory under `package`.
    pub(crate) fn of(self, package: &PackageDir) -> PathBuf {
        match (self, host_generation()) {
            (Self::Launchers, Some(generation)) => package.launchers(generation),
            _ => package.entrypoints(),
        }
    }
}

/// Which directory of `package` to compose, read-only: a complete `launchers/<G>`, else a complete older
/// `entrypoints/`, else `launchers/<G>` (which [`first_use`] was meant to publish).
pub(crate) async fn entry_point_dir(package: &PackageDir, entries: &Entrypoints) -> EntryPointDir {
    let Some(generation) = host_generation() else {
        return EntryPointDir::Legacy;
    };
    if launchers_complete(&package.launchers(generation), entries).await {
        EntryPointDir::Launchers
    } else if legacy_complete(&package.entrypoints(), entries).await {
        EntryPointDir::Legacy
    } else {
        EntryPointDir::Launchers
    }
}

/// Whether an older release's `dir` holds a launcher for every name that gets one.
async fn legacy_complete(dir: &Path, entries: &Entrypoints) -> bool {
    for (name, _entry) in entries.iter().filter(|(name, _)| !is_record_name(name.as_str())) {
        let launcher = if cfg!(windows) {
            format!("{}.exe", name.as_str())
        } else {
            name.as_str().to_owned()
        };
        if !tokio::fs::symlink_metadata(dir.join(launcher))
            .await
            .is_ok_and(|m| m.is_file())
        {
            return false;
        }
    }
    true
}

/// First use of a package whose entry points go on `PATH`: leaves `launchers/<G>` complete and says what to
/// compose. A read-only or foreign-owned home composes a complete older `entrypoints/` instead, with one warning.
///
/// # Errors
///
/// A failed write, or a refusal with no complete `entrypoints/` to fall back to. A missing package directory is
/// [`crate::Error::InternalFile`] naming `<package>/launchers` with `NotFound`.
pub(crate) async fn first_use(
    fs: &FileStructure,
    package: &PackageDir,
    entries: &Entrypoints,
) -> Result<EntryPointDir, crate::Error> {
    let generation = host_generation().ok_or_else(|| {
        crate::error::file_error(package.root(), std::io::Error::other("no launcher blob for this host"))
    })?;
    let target = package.launchers(generation);
    if launchers_complete(&target, entries).await {
        return Ok(EntryPointDir::Launchers);
    }
    match publish_after_retiring(fs, package, generation, &target, entries).await {
        Ok(()) => Ok(EntryPointDir::Launchers),
        Err(crate::Error::InternalFile(path, error)) if refused(&error) => {
            if legacy_complete(&package.entrypoints(), entries).await {
                log::warn!(
                    "Cannot write launchers under '{}' ({error}); using the entry points an older ocx wrote",
                    package.root().display()
                );
                Ok(EntryPointDir::Legacy)
            } else {
                Err(crate::Error::InternalFile(path, error))
            }
        }
        Err(error) => Err(error),
    }
}

/// Whether an I/O error is a refusal the older `entrypoints/` can stand in for.
fn refused(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem
    )
}

/// Moves an incomplete or linked `target` away, then publishes a fresh one.
async fn publish_after_retiring(
    fs: &FileStructure,
    package: &PackageDir,
    generation: &LauncherGeneration,
    target: &Path,
    entries: &Entrypoints,
) -> Result<(), crate::Error> {
    match tokio::fs::symlink_metadata(target).await {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(crate::error::file_error(target, error)),
        Ok(_) => {
            log::warn!("Replacing the incomplete launcher directory '{}'", target.display());
            remove_entry(target).await?;
        }
    }
    publish_package_launchers(fs, package.root(), generation, entries).await?;
    Ok(())
}

/// Retires a real directory; removes a link or file (never following it) that `retire_dir` leaves alone.
async fn remove_entry(target: &Path) -> Result<(), crate::Error> {
    let owned = target.to_path_buf();
    let removed = tokio::task::spawn_blocking(move || -> std::io::Result<bool> {
        if symlink::is_link(&owned) {
            symlink::remove(&owned).map_err(|error| error.cause)?;
            return Ok(true);
        }
        if std::fs::symlink_metadata(&owned)?.is_dir() {
            return Ok(false);
        }
        std::fs::remove_file(&owned)?;
        Ok(true)
    })
    .await
    .map_err(|error| crate::error::file_error(target, std::io::Error::other(error)))?
    .map_err(|error| crate::error::file_error(target, error))?;
    if removed {
        return Ok(());
    }
    match retire_dir(target).await {
        Ok(Retired::Removed) => Ok(()),
        Ok(Retired::LeftForLater) => Err(crate::error::file_error(
            target,
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "in use or protected, not retired"),
        )),
        Err(error) => Err(crate::error::file_error(target, error)),
    }
}

/// Publishes `<package_dir>/launchers/<generation>` for `entries`, built complete in a locked scratch directory and
/// moved in with one rename; a published directory is never edited. `package_dir` is the final package directory and
/// is never created here.
///
/// # Errors
///
/// A failed write, a host without a launcher blob, a missing `package_dir`, or a directory that won the race and is
/// not complete.
pub(crate) async fn publish_package_launchers(
    fs: &FileStructure,
    package_dir: &Path,
    generation: &LauncherGeneration,
    entries: &Entrypoints,
) -> Result<Published, crate::Error> {
    let blob = host_launcher_blob(package_dir)?;
    let target = package_dir.join("launchers").join(generation.as_str());
    let scratch = fs
        .temp
        .scratch("launchers")
        .await
        .map_err(|e| crate::error::file_error(&target, std::io::Error::other(e)))?;
    generate_for(package_dir, entries, scratch.path(), blob, cfg!(windows)).await?;

    // No `create_dir_all`: a package directory removed meanwhile must not come back as a husk holding only launchers.
    let parent = package_dir.join("launchers");
    match tokio::fs::create_dir(&parent).await {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(crate::error::file_error(&parent, e)),
    }
    let published = publish_dir(scratch.path(), &target)
        .await
        .map_err(|e| crate::error::file_error(&target, e))?;
    if published == Published::AlreadyThere && !launchers_complete(&target, entries).await {
        return Err(crate::error::file_error(
            &target,
            std::io::Error::other("another process published an incomplete launcher directory"),
        ));
    }
    Ok(published)
}

/// The embedded launcher image of this host.
///
/// # Errors
///
/// The host has no launcher blob; the error names `dir`, where the launchers would go.
pub(crate) fn host_launcher_blob(dir: &Path) -> Result<&'static [u8], crate::Error> {
    ocx_store::shim::host_blob()
        .ok_or_else(|| crate::error::file_error(dir, std::io::Error::other("no launcher blob for this host")))
}

/// Writes the launcher of `name` into `dir`: `name` (`name.exe` for a Windows target) as a byte copy of `blob`, mode
/// 0755, then `name.launch` holding `record`; both synced.
///
/// A name ending in `.launch` gets neither, with one warning: its launcher would sit where another name's record goes.
///
/// # Errors
///
/// A failed write.
pub(crate) async fn write_launcher(
    dir: &Path,
    name: &str,
    blob: &[u8],
    record: &LaunchRecord,
    windows: bool,
) -> Result<(), crate::Error> {
    if is_record_name(name) {
        log::warn!(
            "Skipping the launcher '{name}': a name ending in '.{RECORD_EXTENSION}' collides with a launcher record"
        );
        return Ok(());
    }
    let launcher = dir.join(if windows {
        format!("{name}.exe")
    } else {
        name.to_owned()
    });
    write_synced(&launcher, blob).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755))
            .await
            .map_err(|e| crate::error::file_error(&launcher, e))?;
    }
    write_synced(&dir.join(format!("{name}.{RECORD_EXTENSION}")), &record.encode()).await
}

/// Writes `bytes` to `path` and `sync_data`s it, so a publish rename never exposes a file the disk lacks.
async fn write_synced(path: &Path, bytes: &[u8]) -> Result<(), crate::Error> {
    use tokio::io::AsyncWriteExt as _;
    let written = async {
        let mut file = tokio::fs::File::create(path).await?;
        file.write_all(bytes).await?;
        file.sync_data().await
    };
    written.await.map_err(|e| crate::error::file_error(path, e))
}

/// Writes one direct launcher per entry point of a vendored package into `dest` (see [`write_direct_launcher_for`]);
/// `windows` is the target's. `sidecar_for` yields the sidecar of a name, its private environment already composed.
/// Empty `entries` creates nothing.
///
/// # Errors
///
/// As [`write_direct_launcher_for`], a failed `dest` creation as [`VendorError::Package`], or whatever `sidecar_for`
/// returns.
pub async fn generate_direct(
    entries: &Entrypoints,
    dest: &Path,
    blob: &[u8],
    windows: bool,
    sidecar_for: &dyn Fn(&str) -> Result<DirectRecord, VendorError>,
) -> Result<(), VendorError> {
    if entries.is_empty() {
        return Ok(());
    }
    tokio::fs::create_dir_all(dest)
        .await
        .map_err(|e| crate::error::file_error(dest, e))?;
    for (name, _entry) in entries.iter() {
        write_direct_launcher_for(dest, name.as_str(), blob, &sidecar_for(name.as_str())?, windows).await?;
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

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use std::collections::BTreeMap;

    use ocx_package::metadata::entrypoint::{Entrypoint, EntrypointName, Entrypoints};

    use super::generate_direct;
    use super::{FileStructure, LauncherGeneration, Published};
    use ocx_env_apply::launch::{DirectRecord, LaunchKind, LaunchRecord, PackageRecord, SCHEMA_VERSION};

    /// A direct entry-point render writes `<name>` and `<name>.launch` carrying the entry's own sidecar.
    #[tokio::test]
    async fn generate_direct_writes_launcher_and_sidecar_per_entry_point() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dest = tmp.path().join("entrypoints");
        let entries = Entrypoints::from_names(["alpha", "beta"]);
        let sidecar_for = |name: &str| -> Result<DirectRecord, super::VendorError> {
            Ok(serde_json::from_value(serde_json::json!({
                "kind": "direct",
                "schema_version": 1,
                "vendor_id": "sha256:0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a",
                "self": format!("packages/p/entrypoints/{name}"),
                "program": format!("@VENDOR_DIR@/packages/p/content/bin/{name}"),
                "args": [],
                "unset": [],
                "env": [],
            }))
            .expect("fixture decodes"))
        };

        generate_direct(&entries, &dest, b"blob", cfg!(windows), &sidecar_for)
            .await
            .expect("generate succeeds");

        for name in ["alpha", "beta"] {
            let launcher = dest.join(if cfg!(windows) {
                format!("{name}.exe")
            } else {
                name.to_owned()
            });
            assert_eq!(std::fs::read(launcher).expect("launcher"), b"blob");
            let sidecar: DirectRecord =
                serde_json::from_slice(&std::fs::read(dest.join(format!("{name}.launch"))).expect("sidecar"))
                    .expect("json");
            assert_eq!(sidecar.program, format!("@VENDOR_DIR@/packages/p/content/bin/{name}"));
            assert_eq!(sidecar.self_path, format!("packages/p/entrypoints/{name}"));
        }

        let empty = tmp.path().join("none");
        generate_direct(
            &Entrypoints::from_names::<[&str; 0], &str>([]),
            &empty,
            b"blob",
            cfg!(windows),
            &sidecar_for,
        )
        .await
        .expect("empty succeeds");
        assert!(!empty.exists());
    }

    fn entries_for(names: &[&str]) -> Entrypoints {
        let map: BTreeMap<EntrypointName, Entrypoint> = names
            .iter()
            .map(|n| (EntrypointName::try_from(*n).unwrap(), Entrypoint::default()))
            .collect();
        Entrypoints::new(map)
    }

    fn host_blob() -> &'static [u8] {
        ocx_store::shim::host_blob().expect("the test host has a launcher blob")
    }

    /// Every file name in `dir`, sorted.
    fn listing(dir: &std::path::Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("read dest")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[tokio::test]
    async fn generate_empty_entrypoints_creates_nothing() {
        let tmp = tempdir().unwrap();
        let pkg_root = tmp.path().join("pkg");
        let dest = pkg_root.join("launchers");

        super::generate(&pkg_root, &entries_for(&[]), &dest).await.unwrap();
        assert!(!dest.exists(), "dest must not be created for empty entrypoints");
    }

    /// V-L37: every launcher is a byte copy of the embedded host blob with a link count of 1, mode 0755;
    /// rewriting one leaves the others unchanged (no shared inode).
    #[cfg(unix)]
    #[tokio::test]
    async fn every_launcher_is_a_byte_copy_of_the_host_blob_with_link_count_one() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let tmp = tempdir().unwrap();
        let dest = tmp.path().join("pkg").join("launchers");
        let names = ["cmake", "ctest", "cpack"];

        super::generate(&tmp.path().join("pkg"), &entries_for(&names), &dest)
            .await
            .unwrap();

        let mut inodes = std::collections::BTreeSet::new();
        for name in names {
            let path = dest.join(name);
            let meta = std::fs::metadata(&path).unwrap();
            // `assert!` rather than `assert_eq!`: a failing byte-slice comparison would print the whole blob.
            assert!(
                std::fs::read(&path).unwrap() == host_blob(),
                "{name}: bytes equal the host blob"
            );
            assert_eq!(meta.nlink(), 1, "{name}: link count");
            assert_eq!(meta.permissions().mode() & 0o777, 0o755, "{name}: mode");
            inodes.insert(meta.ino());
        }
        assert_eq!(inodes.len(), names.len(), "each launcher has an inode of its own");

        std::fs::write(dest.join("cmake"), b"changed").unwrap();
        for name in ["ctest", "cpack"] {
            assert!(
                std::fs::read(dest.join(name)).unwrap() == host_blob(),
                "{name}: unchanged by a write to cmake"
            );
        }
    }

    /// V-L38 writer half: a Windows target lists exactly `NAME.exe` and `NAME.launch` per name, on any host.
    #[tokio::test]
    async fn a_windows_target_lists_only_exe_and_launch_per_name() {
        let tmp = tempdir().unwrap();
        let dest = tmp.path().join("pkg").join("launchers");

        super::generate_for(
            std::path::Path::new("C:/ocx/pkg"),
            &entries_for(&["cmake", "ctest"]),
            &dest,
            host_blob(),
            true,
        )
        .await
        .unwrap();

        assert_eq!(
            listing(&dest),
            ["cmake.exe", "cmake.launch", "ctest.exe", "ctest.launch"],
            "no extensionless script, no `.shim`, no `.cmd`"
        );
    }

    /// A Windows target refuses a package root without a drive letter, which the launcher would exit 78 on, and
    /// writes nothing.
    #[tokio::test]
    async fn a_windows_target_refuses_a_package_root_without_a_drive() {
        let tmp = tempdir().unwrap();
        let dest = tmp.path().join("pkg").join("launchers");

        let result = super::generate_for(
            std::path::Path::new("/no/drive"),
            &entries_for(&["cmake"]),
            &dest,
            host_blob(),
            true,
        )
        .await;

        assert!(result.is_err(), "a driveless root on a Windows target is refused");
        assert!(!dest.exists(), "no directory, no file");
    }

    /// A root holding the `@VENDOR_DIR@` token passes the unsafe-character gate but not the record's own rule, so
    /// nothing is written.
    #[tokio::test]
    async fn a_package_root_holding_the_vendor_token_is_refused_before_anything_is_written() {
        let tmp = tempdir().unwrap();
        let dest = tmp.path().join("pkg").join("launchers");

        let result = super::generate_for(
            &tmp.path().join("@VENDOR_DIR@").join("pkg"),
            &entries_for(&["cmake"]),
            &dest,
            host_blob(),
            false,
        )
        .await;

        assert!(result.is_err(), "the launcher would refuse this record");
        assert!(!dest.exists(), "no directory, no file");
    }

    /// A non-Windows target lists exactly `NAME` and `NAME.launch` per name.
    #[tokio::test]
    async fn a_unix_target_lists_only_the_launcher_and_its_record_per_name() {
        let tmp = tempdir().unwrap();
        let dest = tmp.path().join("pkg").join("launchers");

        super::generate_for(
            &tmp.path().join("pkg"),
            &entries_for(&["cmake"]),
            &dest,
            host_blob(),
            false,
        )
        .await
        .unwrap();

        assert_eq!(listing(&dest), ["cmake", "cmake.launch"]);
    }

    /// The record is the compact package record with the final package root, then one LF.
    #[tokio::test]
    async fn the_record_is_the_compact_package_record_with_the_package_root() {
        let tmp = tempdir().unwrap();
        let pkg_root = tmp.path().join("pkg");
        let dest = pkg_root.join("launchers");

        super::generate(&pkg_root, &entries_for(&["cmake"]), &dest)
            .await
            .unwrap();

        let bytes = std::fs::read(dest.join("cmake.launch")).unwrap();
        let expected = format!(
            "{{\"kind\":\"package\",\"schema_version\":1,\"package_root\":{}}}\n",
            serde_json::to_string(pkg_root.to_str().unwrap()).unwrap()
        );
        assert_eq!(String::from_utf8(bytes.clone()).unwrap(), expected);
        let record: PackageRecord = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(record.kind, LaunchKind::Package);
        assert_eq!(record.package_root, pkg_root.to_str().unwrap());
    }

    /// C-130 / V-L27 home half: a name ending in `.launch` gets no launcher and no record, and does not stop its siblings.
    #[tokio::test]
    async fn a_launch_suffixed_name_is_skipped() {
        let tmp = tempdir().unwrap();
        let record = LaunchRecord::Package(PackageRecord {
            kind: LaunchKind::Package,
            schema_version: SCHEMA_VERSION,
            package_root: "/p".into(),
        });
        for name in ["foo.launch", "foo.LAUNCH"] {
            super::write_launcher(tmp.path(), name, host_blob(), &record, false)
                .await
                .unwrap();
        }
        super::write_launcher(tmp.path(), "bar", host_blob(), &record, false)
            .await
            .unwrap();

        assert_eq!(listing(tmp.path()), ["bar", "bar.launch"]);
    }

    /// C-140: a second publication over a complete `launchers/<G>` is `AlreadyThere` and rewrites nothing.
    #[tokio::test]
    async fn generate_is_idempotent() {
        let tmp = tempdir().unwrap();
        let fs = FileStructure::with_root(tmp.path().join("home"));
        let pkg_dir = tmp.path().join("pkg");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        let generation = LauncherGeneration::host().expect("the test host has a launcher blob");
        let entries = entries_for(&["cmake", "ctest"]);

        let first = super::publish_package_launchers(&fs, &pkg_dir, &generation, &entries)
            .await
            .unwrap();
        assert_eq!(first, Published::New);
        let dir = pkg_dir.join("launchers").join(generation.as_str());
        let before = snapshot(&dir);

        let second = super::publish_package_launchers(&fs, &pkg_dir, &generation, &entries)
            .await
            .unwrap();

        assert_eq!(second, Published::AlreadyThere);
        assert_eq!(snapshot(&dir), before, "names, bytes and inodes are unchanged");
        assert_eq!(listing(&dir).len(), 4);
        assert_eq!(listing(&pkg_dir.join("launchers")), [generation.as_str()]);
    }

    /// V-L24 helper half: a removed package directory is neither recreated nor given a `launchers/`.
    #[tokio::test]
    async fn publish_does_not_recreate_a_removed_package_directory() {
        let tmp = tempdir().unwrap();
        let fs = FileStructure::with_root(tmp.path().join("home"));
        let pkg_dir = tmp.path().join("gone");
        let generation = LauncherGeneration::host().expect("the test host has a launcher blob");

        let result = super::publish_package_launchers(&fs, &pkg_dir, &generation, &entries_for(&["cmake"])).await;

        assert!(result.is_err());
        assert!(!pkg_dir.exists());
    }

    fn host_generation() -> LauncherGeneration {
        LauncherGeneration::host().expect("the test host has a launcher blob")
    }

    /// A home with an installed package directory, plus the entries of its package.
    fn home_with_package(tmp: &std::path::Path) -> (FileStructure, ocx_store::file_structure::PackageDir) {
        let package = tmp.join("pkg");
        std::fs::create_dir_all(&package).unwrap();
        (
            FileStructure::with_root(tmp.join("home")),
            ocx_store::file_structure::PackageDir::with_root(package),
        )
    }

    /// V-L21: a fault after the first launcher is written leaves no `launchers/<G>`; a retry yields a complete one.
    #[tokio::test]
    async fn a_fault_after_the_first_launcher_leaves_no_directory_and_a_retry_completes_it() {
        let tmp = tempdir().unwrap();
        let (fs, package) = home_with_package(tmp.path());
        let generation = host_generation();
        let entries = entries_for(&["alpha", "beta"]);

        let faulted = super::FAULT_AFTER
            .scope(
                1,
                super::publish_package_launchers(&fs, package.root(), &generation, &entries),
            )
            .await;

        assert!(faulted.is_err());
        assert!(
            !package.launchers(&generation).exists(),
            "no half-written directory is published"
        );
        let retried = super::publish_package_launchers(&fs, package.root(), &generation, &entries)
            .await
            .unwrap();
        assert_eq!(retried, Published::New);
        assert!(super::launchers_complete(&package.launchers(&generation), &entries).await);
    }

    /// V-L22: two concurrent first uses both succeed, one publishing and one finding the winner, on one complete directory.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn two_concurrent_first_uses_converge_on_one_complete_directory() {
        let tmp = tempdir().unwrap();
        let (fs, package) = home_with_package(tmp.path());
        let generation = host_generation();
        let entries = entries_for(&["alpha", "beta"]);

        let (first, second) = tokio::join!(
            super::publish_package_launchers(&fs, package.root(), &generation, &entries),
            super::publish_package_launchers(&fs, package.root(), &generation, &entries),
        );

        let mut outcomes = vec![first.unwrap(), second.unwrap()];
        outcomes.sort_by_key(|published| *published == Published::AlreadyThere);
        assert_eq!(outcomes, [Published::New, Published::AlreadyThere]);
        assert!(super::launchers_complete(&package.launchers(&generation), &entries).await);
        assert_eq!(listing(&package.launchers_root()), [generation.as_str()]);

        let (a, b) = tokio::join!(
            super::first_use(&fs, &package, &entries),
            super::first_use(&fs, &package, &entries)
        );
        assert_eq!(
            (a.unwrap(), b.unwrap()),
            (super::EntryPointDir::Launchers, super::EntryPointDir::Launchers)
        );
    }

    /// V-L23: a directory missing one record is retired and republished whole.
    #[tokio::test]
    async fn first_use_republishes_a_directory_missing_a_record() {
        let tmp = tempdir().unwrap();
        let (fs, package) = home_with_package(tmp.path());
        let generation = host_generation();
        let entries = entries_for(&["alpha", "beta"]);
        let dir = package.launchers(&generation);
        super::generate(package.root(), &entries, &dir).await.unwrap();
        std::fs::remove_file(dir.join("beta.launch")).unwrap();

        let chosen = super::first_use(&fs, &package, &entries).await.unwrap();

        assert_eq!(chosen, super::EntryPointDir::Launchers);
        assert!(super::launchers_complete(&dir, &entries).await);
    }

    /// V-L23: a symlink at `launchers/<G>` is removed, never followed, and a real directory published in its place.
    #[cfg(unix)]
    #[tokio::test]
    async fn first_use_replaces_a_symlinked_directory_and_leaves_its_target_alone() {
        let tmp = tempdir().unwrap();
        let (fs, package) = home_with_package(tmp.path());
        let generation = host_generation();
        let entries = entries_for(&["alpha"]);
        let elsewhere = tmp.path().join("elsewhere");
        super::generate(package.root(), &entries, &elsewhere).await.unwrap();
        let before = snapshot(&elsewhere);
        std::fs::create_dir_all(package.launchers_root()).unwrap();
        std::os::unix::fs::symlink(&elsewhere, package.launchers(&generation)).unwrap();

        let chosen = super::first_use(&fs, &package, &entries).await.unwrap();

        assert_eq!(chosen, super::EntryPointDir::Launchers);
        let dir = package.launchers(&generation);
        assert!(!std::fs::symlink_metadata(&dir).unwrap().file_type().is_symlink());
        assert!(super::launchers_complete(&dir, &entries).await);
        assert_eq!(snapshot(&elsewhere), before, "the link target is untouched");
    }

    /// V-L24: a package directory removed before first use is not recreated by `launchers/`.
    #[tokio::test]
    async fn first_use_of_a_removed_package_fails_and_creates_nothing() {
        let tmp = tempdir().unwrap();
        let fs = FileStructure::with_root(tmp.path().join("home"));
        let package = ocx_store::file_structure::PackageDir::with_root(tmp.path().join("gone"));

        let error = super::first_use(&fs, &package, &entries_for(&["alpha"]))
            .await
            .unwrap_err();

        assert!(
            matches!(&error, crate::Error::InternalFile(path, cause)
                if *path == package.launchers_root() && cause.kind() == std::io::ErrorKind::NotFound),
            "{error:?}"
        );
        assert!(!package.root().exists());
    }

    /// Whether the test runs as root, which no permission bit refuses.
    #[cfg(unix)]
    fn running_as_root() -> bool {
        // SAFETY: `geteuid` takes no arguments, touches no memory, and always succeeds.
        unsafe { libc::geteuid() == 0 }
    }

    #[cfg(unix)]
    fn set_mode(path: &std::path::Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    /// V-L25: a read-only package directory composes a complete older `entrypoints/`.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_read_only_package_directory_falls_back_to_a_complete_older_entrypoints() {
        if running_as_root() {
            return;
        }
        let tmp = tempdir().unwrap();
        let (fs, package) = home_with_package(tmp.path());
        let entries = entries_for(&["alpha", "beta"]);
        std::fs::create_dir_all(package.entrypoints()).unwrap();
        for name in ["alpha", "beta"] {
            std::fs::write(package.entrypoints().join(name), b"#!/bin/sh\n").unwrap();
        }
        set_mode(package.root(), 0o555);

        let chosen = super::first_use(&fs, &package, &entries).await;
        let read_back = super::entry_point_dir(&package, &entries).await;
        set_mode(package.root(), 0o755);

        assert_eq!(chosen.unwrap(), super::EntryPointDir::Legacy);
        assert_eq!(read_back, super::EntryPointDir::Legacy);
        assert!(!package.launchers_root().exists());
        assert_eq!(
            listing(&package.entrypoints()),
            ["alpha", "beta"],
            "entrypoints/ is never written"
        );
    }

    /// V-L25: with no complete older `entrypoints/`, the refusal names the path with a permission cause.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_read_only_package_directory_without_entrypoints_fails_naming_the_path() {
        if running_as_root() {
            return;
        }
        let tmp = tempdir().unwrap();
        let (fs, package) = home_with_package(tmp.path());
        let entries = entries_for(&["alpha", "beta"]);
        std::fs::create_dir_all(package.entrypoints()).unwrap();
        std::fs::write(package.entrypoints().join("alpha"), b"#!/bin/sh\n").unwrap();
        set_mode(package.root(), 0o555);

        let error = super::first_use(&fs, &package, &entries).await.unwrap_err();
        set_mode(package.root(), 0o755);

        assert!(
            matches!(&error, crate::Error::InternalFile(path, cause)
                if *path == package.launchers_root() && cause.kind() == std::io::ErrorKind::PermissionDenied),
            "{error:?}"
        );
        assert_eq!(
            ocx_exit::ClassifyExitCode::classify(&error),
            Some(ocx_exit::ExitCode::PermissionDenied),
            "C-127: a permission refusal on first use exits 77"
        );
    }

    /// An incomplete directory is not complete; neither is a link, nor a directory short one record.
    #[tokio::test]
    async fn completeness_needs_a_real_directory_with_every_file() {
        let tmp = tempdir().unwrap();
        let entries = entries_for(&["cmake"]);
        let dir = tmp.path().join("d");
        super::generate(&tmp.path().join("pkg"), &entries, &dir).await.unwrap();
        assert!(super::launchers_complete(&dir, &entries).await);

        std::fs::remove_file(dir.join("cmake.launch")).unwrap();
        assert!(!super::launchers_complete(&dir, &entries).await);
        assert!(!super::launchers_complete(&tmp.path().join("absent"), &entries).await);
        #[cfg(unix)]
        {
            let link = tmp.path().join("link");
            let full = tmp.path().join("full");
            super::generate(&tmp.path().join("pkg"), &entries, &full).await.unwrap();
            std::os::unix::fs::symlink(&full, &link).unwrap();
            assert!(!super::launchers_complete(&link, &entries).await);
        }
    }

    /// Every file of `dir` as (name, bytes, inode on Unix), sorted.
    fn snapshot(dir: &std::path::Path) -> Vec<(String, Vec<u8>, u64)> {
        listing(dir)
            .into_iter()
            .map(|name| {
                let path = dir.join(&name);
                #[cfg(unix)]
                let inode = std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&path).unwrap());
                #[cfg(not(unix))]
                let inode = 0;
                (name, std::fs::read(&path).unwrap(), inode)
            })
            .collect()
    }

    #[test]
    fn entrypoint_name_rejects_path_traversal_bytes() {
        assert!(EntrypointName::try_from("../evil").is_err());
        assert!(EntrypointName::try_from("sub/path").is_err());
        assert!(EntrypointName::try_from("back\\slash").is_err());
    }

    /// `generate()` validates `pkg_root`'s text before any file is written: a launcher-unsafe character
    /// (`'`, `%`, `"`, `\n`, `\r`, `\0`) is refused at the boundary, and nothing is created.
    #[tokio::test]
    async fn generate_rejects_pkg_root_with_unsafe_character() {
        let tmp = tempdir().unwrap();
        let pkg_root = tmp.path().join("pkg'with'quote");
        let dest = pkg_root.join("launchers");

        let err = super::generate(&pkg_root, &entries_for(&["cmake"]), &dest)
            .await
            .expect_err("pkg_root with unsafe character must be rejected");
        match err {
            crate::Error::LauncherUnsafeCharacter { character, .. } => assert_eq!(character, '\''),
            other => panic!("expected LauncherUnsafeCharacter, got {other:?}"),
        }
        assert!(!dest.exists(), "nothing is written when the root is refused");
    }

    /// The package root every launcher record carries must be UTF-8: `to_string_lossy` would substitute
    /// U+FFFD, pass `LauncherSafeString` and record a root that names nothing.
    ///
    /// Unix only: only there can a path be arbitrary bytes.
    #[cfg(unix)]
    #[tokio::test]
    async fn generate_rejects_a_pkg_root_that_is_not_utf8() {
        use std::os::unix::ffi::OsStrExt as _;

        let tmp = tempdir().unwrap();
        let pkg_root = tmp.path().join(std::ffi::OsStr::from_bytes(b"pkg-\x80-not-utf8"));
        assert!(pkg_root.to_str().is_none(), "precondition: the root is not UTF-8");
        assert!(
            super::LauncherSafeString::new(pkg_root.to_string_lossy().into_owned()).is_ok(),
            "precondition: the lossy rendering passes the unsafe-character gate, which is why the UTF-8 refusal exists"
        );
        let dest = pkg_root.join("launchers");

        let err = super::generate(&pkg_root, &entries_for(&["cmake"]), &dest)
            .await
            .expect_err("a non-UTF-8 pkg_root must be refused, not lossily recorded");
        match err {
            crate::Error::LauncherPathNotUtf8 { path } => assert!(path.contains('\u{FFFD}'), "{path:?}"),
            other => panic!("expected LauncherPathNotUtf8, got {other:?}"),
        }
        assert!(!dest.exists(), "the refusal fires before any file is created");
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
