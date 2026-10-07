// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

type Result<T> = std::result::Result<T, ocx_util::error::FileError>;

/// Ad-hoc signs every Mach-O file under `content_path` whose signature does not verify, or Apple Silicon kills it;
/// no-op off macOS, failures only warn.
///
/// Per file, never per bundle: re-sealing third-party bundles raises Team ID conflicts (`adr_codesign_per_file_signing.md`).
pub async fn sign_extracted_content(content_path: &Path) -> Result<()> {
    if cfg!(not(target_os = "macos")) {
        return Ok(());
    }

    if ocx_env::OCX_NO_CODESIGN.bool_or(false).unwrap_or(false) {
        log::debug!("Code signing disabled via OCX_NO_CODESIGN");
        return Ok(());
    }

    if !codesign_available() {
        log::warn!("codesign not found — skipping ad-hoc code signing");
        return Ok(());
    }

    remove_quarantine(content_path).await;

    let groups = Arc::new(Mutex::new(HardLinkGroups::default()));
    sign_directory(content_path.to_path_buf(), Arc::clone(&groups)).await;
    let groups = std::mem::take(&mut *groups.lock().expect("hard-link groups mutex poisoned"));
    relink_replaced_aliases(&groups).await;

    Ok(())
}

/// The Mach-O names met per inode: the first is signed, the others are hard links to it.
#[derive(Default)]
struct HardLinkGroups {
    first_names: HashMap<u64, PathBuf>,
    aliases: Vec<(u64, PathBuf)>,
}

/// Re-points each alias at its first name when signing gave that name a new inode, or the alias keeps the unsigned bytes.
async fn relink_replaced_aliases(groups: &HardLinkGroups) {
    for (inode, alias) in &groups.aliases {
        let first = &groups.first_names[inode];
        if file_inode(first).await == Some(*inode) {
            continue;
        }
        // Link beside, then rename over: a failed link leaves the alias unsigned rather than missing.
        let staged = alias.with_file_name(format!(
            "{}.codesign_link",
            alias.file_name().unwrap_or_default().to_string_lossy()
        ));
        // A failed link leaves `staged` untouched: it may be a package file of that name, never ours to delete.
        let relinked = match tokio::fs::hard_link(first, &staged).await {
            Ok(()) => {
                let renamed = tokio::fs::rename(&staged, alias).await;
                if renamed.is_err() {
                    tokio::fs::remove_file(&staged).await.ok(); // best effort; the warning below reports the failure
                }
                renamed
            }
            Err(error) => Err(error),
        };
        if let Err(error) = relinked {
            log::warn!(
                "Failed to relink {} to signed {}: {}",
                alias.display(),
                first.display(),
                error
            );
        }
    }
}

const MACHO_MAGIC: &[u32] = &[
    0xFEED_FACE, // MH_MAGIC (32-bit)
    0xFEED_FACF, // MH_MAGIC_64 (64-bit)
    0xCAFE_BABE, // FAT_MAGIC (Universal)
    0xCEFA_EDFE, // MH_CIGAM (32-bit, swapped)
    0xCFFA_EDFE, // MH_CIGAM_64 (64-bit, swapped)
    0xBEBA_FECA, // FAT_CIGAM (Universal, swapped)
];

async fn is_macho(path: &Path) -> bool {
    let Ok(mut file) = tokio::fs::File::open(path).await else {
        return false;
    };

    let mut magic = [0u8; 4];
    if tokio::io::AsyncReadExt::read_exact(&mut file, &mut magic)
        .await
        .is_err()
    {
        return false;
    }

    let value = u32::from_be_bytes(magic);
    MACHO_MAGIC.contains(&value)
}

/// Boxed so the recursive `JoinSet::spawn` gets a nameable `Send` future, which a recursive `async fn` cannot prove.
fn sign_directory(
    path: std::path::PathBuf,
    groups: Arc<Mutex<HardLinkGroups>>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
    Box::pin(async move {
        let Ok(mut read_dir) = tokio::fs::read_dir(&path).await else {
            return;
        };

        let mut subdirs = Vec::new();
        let mut files = Vec::new();

        while let Ok(Some(entry)) = read_dir.next_entry().await {
            // `file_type` does not follow symlinks; following one would re-sign a host file outside the package
            // or recurse on a loop.
            let Ok(ft) = entry.file_type().await else {
                continue;
            };
            if ft.is_dir() {
                subdirs.push(entry.path());
            } else if ft.is_file() {
                files.push(entry.path());
            }
        }

        let mut subdir_tasks = tokio::task::JoinSet::new();
        for dir in subdirs {
            subdir_tasks.spawn(sign_directory(dir, Arc::clone(&groups)));
        }
        while let Some(result) = subdir_tasks.join_next().await {
            if let Err(e) = result {
                log::warn!("Directory signing task panicked: {}", e);
            }
        }

        let mut file_tasks = tokio::task::JoinSet::new();
        for file in files {
            if !is_macho(&file).await {
                continue;
            }
            if let Some(inode) = file_inode(&file).await {
                let mut groups = groups.lock().expect("hard-link groups mutex poisoned");
                if groups.first_names.contains_key(&inode) {
                    groups.aliases.push((inode, file));
                    continue;
                }
                groups.first_names.insert(inode, file.clone());
            }
            file_tasks.spawn(async move { sign_binary(&file).await });
        }
        while let Some(result) = file_tasks.join_next().await {
            if let Err(e) = result {
                log::warn!("Code signing task panicked: {}", e);
            }
        }
    })
}

#[cfg(unix)]
async fn file_inode(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    tokio::fs::metadata(path).await.ok().map(|m| m.ino())
}

#[cfg(not(unix))]
async fn file_inode(_path: &Path) -> Option<u64> {
    None
}

// Absolute, never a `PATH` lookup: extraction inherits a `PATH` holding the package's own shims, so a package could run its own `codesign`.
const XATTR_BIN: &str = "/usr/bin/xattr";
const CODESIGN_BIN: &str = "/usr/bin/codesign";

fn codesign_available() -> bool {
    std::path::Path::new(CODESIGN_BIN).is_file()
}

#[expect(
    clippy::disallowed_types,
    reason = "runs the fixed macOS system utility `xattr` during extraction; not a tool launch"
)]
async fn remove_quarantine(content_path: &Path) {
    let result = tokio::process::Command::new(XATTR_BIN)
        .args(["-dr", "com.apple.quarantine"])
        .arg(content_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await;

    match result {
        Ok(output) if !output.status.success() => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            log::debug!(
                "xattr quarantine removal returned non-zero (attribute may not exist): {}",
                stderr.trim()
            );
        }
        Err(e) => {
            log::debug!("xattr command failed: {}", e);
        }
        _ => {}
    }
}

/// Preserves only `entitlements`: keeping `flags` carries `CS_RUNTIME`, whose Library Validation then rejects every ad-hoc library.
///
/// Keeping `requirements` pins a Team ID an ad-hoc signature cannot satisfy (`adr_codesign_per_file_signing.md § Signing command`).
async fn sign_binary(path: &Path) {
    // `--force` on a bundle's main executable replaces the bundle signature, unbinding `Info.plist` and
    // breaking the resource seal (ocx#546), so a signature that already verifies is never touched.
    if try_codesign(&["--verify", "--strict"], path).await {
        log::debug!("Keeping valid signature: {}", path.display());
        return;
    }

    log::debug!("Signing Mach-O binary: {}", path.display());

    let args = &["--sign", "-", "--force", "--preserve-metadata=entitlements"];

    if try_codesign(args, path).await {
        log::debug!("Signed: {}", path.display());
        return;
    }

    log::debug!("Retrying with inode workaround: {}", path.display());
    if let Err(e) = retry_sign_with_copy(args, path).await {
        log::warn!("Failed to sign {} (even after retry): {}", path.display(), e);
    }
}

#[expect(
    clippy::disallowed_types,
    reason = "runs the fixed macOS system utility `codesign` during extraction; not a tool launch"
)]
async fn try_codesign(args: &[&str], path: &Path) -> bool {
    let result = tokio::process::Command::new(CODESIGN_BIN)
        .args(args)
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await;

    match result {
        Ok(output) if output.status.success() => true,
        Ok(output) => {
            let msg = format_process_output(&output.stdout, &output.stderr);
            log::debug!("codesign attempt failed for {}: {}", path.display(), msg);
            false
        }
        Err(e) => {
            log::debug!("Failed to run codesign on {}: {}", path.display(), e);
            false
        }
    }
}

/// Signs a copy on a fresh inode and renames it back, working around Apple `codesign` failing on some inodes.
///
/// The suffix is appended, not swapped for the extension, or `foo.bar` and `foo.baz` collide.
async fn retry_sign_with_copy(args: &[&str], path: &Path) -> std::io::Result<()> {
    let tmp_name = format!(
        "{}.codesign_tmp",
        path.file_name().unwrap_or_default().to_string_lossy()
    );
    let tmp = path.with_file_name(tmp_name);
    tokio::fs::copy(path, &tmp).await?;

    if try_codesign(args, &tmp).await {
        tokio::fs::rename(&tmp, path).await?;
        Ok(())
    } else {
        if let Err(error) = tokio::fs::remove_file(&tmp).await {
            log::debug!("Failed to remove temp file {}: {}", tmp.display(), error);
        }
        Err(std::io::Error::other("codesign failed after inode workaround"))
    }
}

fn format_process_output(stdout: &[u8], stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let stdout = String::from_utf8_lossy(stdout);
    let stderr = stderr.trim();
    let stdout = stdout.trim();
    match (stderr.is_empty(), stdout.is_empty()) {
        (false, false) => format!("{}\n{}", stderr, stdout),
        (false, true) => stderr.to_string(),
        (true, false) => stdout.to_string(),
        (true, true) => "(no output)".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::MACHO_MAGIC;

    fn create_file_with_magic(dir: &std::path::Path, name: &str, magic: &[u8]) -> PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(magic).unwrap();
        file.write_all(&[0u8; 64]).unwrap();
        path
    }

    // -- is_macho -----------------------------------------------------------------

    #[tokio::test]
    async fn is_macho_detects_64bit() {
        let dir = TempDir::new().unwrap();
        let path = create_file_with_magic(dir.path(), "binary", &0xFEED_FACFu32.to_be_bytes());
        assert!(super::is_macho(&path).await);
    }

    #[tokio::test]
    async fn is_macho_detects_32bit() {
        let dir = TempDir::new().unwrap();
        let path = create_file_with_magic(dir.path(), "binary", &0xFEED_FACEu32.to_be_bytes());
        assert!(super::is_macho(&path).await);
    }

    #[tokio::test]
    async fn is_macho_detects_fat_universal() {
        let dir = TempDir::new().unwrap();
        let path = create_file_with_magic(dir.path(), "binary", &0xCAFE_BABEu32.to_be_bytes());
        assert!(super::is_macho(&path).await);
    }

    #[tokio::test]
    async fn is_macho_detects_swapped_variants() {
        let dir = TempDir::new().unwrap();
        for magic in &[0xCEFA_EDFEu32, 0xCFFA_EDFE, 0xBEBA_FECA] {
            let path = create_file_with_magic(dir.path(), &format!("bin_{:08x}", magic), &magic.to_be_bytes());
            assert!(
                super::is_macho(&path).await,
                "should detect swapped magic {:#010x}",
                magic
            );
        }
    }

    #[tokio::test]
    async fn is_macho_all_known_magics() {
        let dir = TempDir::new().unwrap();
        for magic in MACHO_MAGIC {
            let path = create_file_with_magic(dir.path(), &format!("bin_{:08x}", magic), &magic.to_be_bytes());
            assert!(super::is_macho(&path).await, "should detect magic {:#010x}", magic);
        }
    }

    #[tokio::test]
    async fn is_macho_rejects_elf() {
        let dir = TempDir::new().unwrap();
        let path = create_file_with_magic(dir.path(), "elf_binary", &[0x7F, b'E', b'L', b'F']);
        assert!(!super::is_macho(&path).await);
    }

    #[tokio::test]
    async fn is_macho_rejects_script() {
        let dir = TempDir::new().unwrap();
        let path = create_file_with_magic(dir.path(), "script.sh", b"#!/b");
        assert!(!super::is_macho(&path).await);
    }

    #[tokio::test]
    async fn is_macho_rejects_empty_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("empty");
        std::fs::File::create(&path).unwrap();
        assert!(!super::is_macho(&path).await);
    }

    #[tokio::test]
    async fn is_macho_rejects_short_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("short");
        std::fs::write(&path, [0xFE, 0xED]).unwrap(); // only 2 bytes
        assert!(!super::is_macho(&path).await);
    }

    // -- sign_directory -----------------------------------------------------------

    // Unix-only: `groups` dedups by inode, but `file_inode` returns
    // `None` on non-Unix, so the count expectation only holds on Unix.
    #[cfg(unix)]
    #[tokio::test]
    async fn sign_directory_finds_standalone_binaries() {
        let dir = TempDir::new().unwrap();
        let bin_dir = dir.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();

        create_file_with_magic(&bin_dir, "tool_a", &0xFEED_FACFu32.to_be_bytes());
        create_file_with_magic(&bin_dir, "tool_b", &0xFEED_FACEu32.to_be_bytes());
        create_file_with_magic(&bin_dir, "script.sh", b"#!/b");

        let groups = std::sync::Arc::new(std::sync::Mutex::new(super::HardLinkGroups::default()));
        super::sign_directory(dir.path().to_path_buf(), groups.clone()).await;

        // Two Mach-O files; script.sh is not Mach-O.
        assert_eq!(groups.lock().unwrap().first_names.len(), 2);
    }

    #[tokio::test]
    async fn sign_directory_empty_directory() {
        let dir = TempDir::new().unwrap();
        let groups = std::sync::Arc::new(std::sync::Mutex::new(super::HardLinkGroups::default()));
        super::sign_directory(dir.path().to_path_buf(), groups.clone()).await;
        assert!(groups.lock().unwrap().first_names.is_empty());
    }

    #[tokio::test]
    async fn sign_directory_ignores_non_macho_files() {
        let dir = TempDir::new().unwrap();
        create_file_with_magic(dir.path(), "readme.txt", b"Hell");
        create_file_with_magic(dir.path(), "data.json", b"{\"ke");
        create_file_with_magic(dir.path(), "image.png", &[0x89, b'P', b'N', b'G']);

        let groups = std::sync::Arc::new(std::sync::Mutex::new(super::HardLinkGroups::default()));
        super::sign_directory(dir.path().to_path_buf(), groups.clone()).await;
        assert!(groups.lock().unwrap().first_names.is_empty());
    }

    // Unix-only: relies on Unix symlinks and inode-based dedup.
    #[cfg(unix)]
    #[tokio::test]
    async fn sign_directory_skips_symlinks() {
        let dir = TempDir::new().unwrap();
        let bin_dir = dir.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();

        // `real`/`link` are only used by the Unix `symlink()` call below;
        // prefix with `_` so the compiler does not warn on Windows where the
        // `#[cfg(unix)]` block is excluded.
        let _real = create_file_with_magic(&bin_dir, "real_binary", &0xFEED_FACFu32.to_be_bytes());
        let _link = bin_dir.join("link_binary");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&_real, &_link).unwrap();

        let groups = std::sync::Arc::new(std::sync::Mutex::new(super::HardLinkGroups::default()));
        super::sign_directory(dir.path().to_path_buf(), groups.clone()).await;

        // Only the real file should be signed, not the symlink.
        assert_eq!(groups.lock().unwrap().first_names.len(), 1);
    }

    // Unix-only: `groups` dedups by inode, but `file_inode` returns
    // `None` on non-Unix, so the count expectation only holds on Unix.
    #[cfg(unix)]
    #[tokio::test]
    async fn sign_directory_deduplicates_hardlinks() {
        let dir = TempDir::new().unwrap();
        let bin_dir = dir.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();

        let original = create_file_with_magic(&bin_dir, "original", &0xFEED_FACFu32.to_be_bytes());
        let hardlink = bin_dir.join("hardlink");
        crate::hardlink::create(&original, &hardlink).unwrap();

        let groups = std::sync::Arc::new(std::sync::Mutex::new(super::HardLinkGroups::default()));
        super::sign_directory(dir.path().to_path_buf(), groups.clone()).await;

        // Same inode — should only be signed once.
        assert_eq!(groups.lock().unwrap().first_names.len(), 1);
        assert_eq!(groups.lock().unwrap().aliases.len(), 1);
    }

    #[cfg(unix)]
    fn names(dir: &std::path::Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[cfg(unix)]
    fn inode(path: &std::path::Path) -> u64 {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).unwrap().ino()
    }

    /// The copy fallback renames a signed copy over the first name; every alias must follow it to the new inode.
    #[cfg(unix)]
    #[tokio::test]
    async fn relink_moves_aliases_onto_a_replaced_first_name() {
        let dir = TempDir::new().unwrap();
        let first = create_file_with_magic(dir.path(), "first", b"old!");
        let alias = dir.path().join("alias");
        crate::hardlink::create(&first, &alias).unwrap();
        let groups = super::HardLinkGroups {
            first_names: [(inode(&first), first.clone())].into(),
            aliases: vec![(inode(&first), alias.clone())],
        };

        let signed = dir.path().join("signed");
        std::fs::write(&signed, b"new!").unwrap();
        std::fs::rename(&signed, &first).unwrap();
        super::relink_replaced_aliases(&groups).await;

        assert_eq!(inode(&alias), inode(&first));
        assert_eq!(std::fs::read(&alias).unwrap(), b"new!");
        assert_eq!(names(dir.path()), ["alias", "first"]);
    }

    /// A package file that happens to carry the staging name is never deleted by a failed relink.
    #[cfg(unix)]
    #[tokio::test]
    async fn relink_keeps_a_package_file_named_like_the_staged_link() {
        let dir = TempDir::new().unwrap();
        let first = create_file_with_magic(dir.path(), "first", b"old!");
        let alias = dir.path().join("alias");
        crate::hardlink::create(&first, &alias).unwrap();
        let occupant = dir.path().join("alias.codesign_link");
        std::fs::write(&occupant, b"package data").unwrap();
        let groups = super::HardLinkGroups {
            first_names: [(inode(&first), first.clone())].into(),
            aliases: vec![(inode(&first), alias.clone())],
        };

        let signed = dir.path().join("signed");
        std::fs::write(&signed, b"new!").unwrap();
        std::fs::rename(&signed, &first).unwrap();
        super::relink_replaced_aliases(&groups).await;

        assert_eq!(std::fs::read(&occupant).unwrap(), b"package data");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn relink_leaves_aliases_of_an_unreplaced_first_name() {
        let dir = TempDir::new().unwrap();
        let first = create_file_with_magic(dir.path(), "first", b"same");
        let alias = dir.path().join("alias");
        crate::hardlink::create(&first, &alias).unwrap();
        let original = inode(&alias);
        let groups = super::HardLinkGroups {
            first_names: [(original, first.clone())].into(),
            aliases: vec![(original, alias.clone())],
        };

        super::relink_replaced_aliases(&groups).await;

        assert_eq!(inode(&alias), original);
        assert_eq!(inode(&first), original);
        assert_eq!(names(dir.path()), ["alias", "first"]);
    }

    // -- sign_extracted_content (stubbed) -----------------------------------------

    #[tokio::test]
    async fn sign_extracted_content_noop_when_disabled() {
        let env = ocx_env::overrides::lock();
        env.set(&ocx_env::OCX_NO_CODESIGN, "1");

        let dir = TempDir::new().unwrap();
        create_file_with_magic(dir.path(), "binary", &0xFEED_FACFu32.to_be_bytes());

        // Should return Ok and do nothing.
        let result = super::sign_extracted_content(dir.path()).await;
        assert!(result.is_ok());
    }

    // -- macOS integration tests (actually invoke codesign) -----------------------

    /// Compile a minimal Mach-O binary and strip its signature.
    /// On Apple Silicon, `ld` ad-hoc signs by default — we strip that so tests
    /// can verify that our signing code actually transforms unsigned → signed.
    #[cfg(target_os = "macos")]
    #[expect(
        clippy::disallowed_types,
        reason = "test-only: builds and inspects Mach-O fixtures with the system toolchain"
    )]
    fn build_unsigned_binary(dir: &std::path::Path, name: &str) -> PathBuf {
        let src = dir.join(format!("{name}.c"));
        std::fs::write(&src, "int main(){return 0;}\n").unwrap();
        let dst = dir.join(name);
        let compile = std::process::Command::new("cc")
            .args(["-o"])
            .arg(&dst)
            .arg(&src)
            .output()
            .expect("cc not found — Xcode CLI tools required");
        assert!(
            compile.status.success(),
            "failed to compile test binary: {}",
            String::from_utf8_lossy(&compile.stderr)
        );
        let strip = std::process::Command::new("codesign")
            .args(["--remove-signature"])
            .arg(&dst)
            .output()
            .expect("failed to strip signature");
        assert!(strip.status.success(), "failed to strip signature");
        dst
    }

    /// Run `codesign --verify --verbose` and return whether the signature is valid.
    #[cfg(target_os = "macos")]
    #[expect(
        clippy::disallowed_types,
        reason = "test-only: builds and inspects Mach-O fixtures with the system toolchain"
    )]
    async fn verify_signature(path: &std::path::Path) -> bool {
        let output = tokio::process::Command::new("codesign")
            .args(["--verify", "--verbose"])
            .arg(path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .await
            .expect("failed to run codesign --verify");
        output.status.success()
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn sign_binary_signs_macho() {
        let dir = TempDir::new().unwrap();
        let binary = build_unsigned_binary(dir.path(), "my_tool");
        assert!(
            !verify_signature(&binary).await,
            "binary should be unsigned before signing"
        );

        super::sign_binary(&binary).await;

        assert!(
            verify_signature(&binary).await,
            "binary should have a valid ad-hoc signature after signing"
        );
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn sign_extracted_content_signs_real_binaries() {
        let env = ocx_env::overrides::lock();
        env.remove(&ocx_env::OCX_NO_CODESIGN);

        let dir = TempDir::new().unwrap();
        let content = dir.path().join("content");
        let bin_dir = content.join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();

        let binary = build_unsigned_binary(&bin_dir, "my_tool");
        assert!(
            !verify_signature(&binary).await,
            "binary should be unsigned before signing"
        );

        let result = super::sign_extracted_content(&content).await;
        assert!(result.is_ok(), "sign_extracted_content should succeed");

        assert!(
            verify_signature(&binary).await,
            "binary should be signed after sign_extracted_content"
        );
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[expect(
        clippy::disallowed_types,
        reason = "test-only: builds and inspects Mach-O fixtures with the system toolchain"
    )]
    async fn sign_extracted_content_leaves_sealed_app_bundle_untouched() {
        let env = ocx_env::overrides::lock();
        env.remove(&ocx_env::OCX_NO_CODESIGN);

        let dir = TempDir::new().unwrap();
        let content = dir.path().join("content");
        let bundle = content.join("Foo.app");
        let macos_dir = bundle.join("Contents/MacOS");
        std::fs::create_dir_all(&macos_dir).unwrap();
        build_unsigned_binary(&macos_dir, "Foo");
        std::fs::remove_file(macos_dir.join("Foo.c")).unwrap();
        std::fs::write(
            bundle.join("Contents/Info.plist"),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>Foo</string>
<key>CFBundleIdentifier</key><string>sh.ocx.test.foo</string>
<key>CFBundlePackageType</key><string>APPL</string>
</dict></plist>
"#,
        )
        .unwrap();

        let seal = std::process::Command::new("codesign")
            .args(["--sign", "-", "--force"])
            .arg(&bundle)
            .output()
            .expect("failed to seal bundle");
        assert!(
            seal.status.success(),
            "failed to seal bundle: {}",
            String::from_utf8_lossy(&seal.stderr)
        );

        // A fixed past mtime makes any rewrite of the executable observable, however it re-signs.
        let executable = macos_dir.join("Foo");
        let pinned = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
        std::fs::File::options()
            .write(true)
            .open(&executable)
            .unwrap()
            .set_modified(pinned)
            .unwrap();

        let result = super::sign_extracted_content(&content).await;
        assert!(result.is_ok(), "sign_extracted_content should succeed");

        assert_eq!(
            std::fs::metadata(&executable).unwrap().modified().unwrap(),
            pinned,
            "a validly signed executable must not be re-signed"
        );
        let verify = tokio::process::Command::new("codesign")
            .args(["--verify", "--strict"])
            .arg(&bundle)
            .output()
            .await
            .expect("failed to run codesign --verify");
        assert!(
            verify.status.success(),
            "sealed bundle must still verify after install: {}",
            String::from_utf8_lossy(&verify.stderr)
        );
    }
}
