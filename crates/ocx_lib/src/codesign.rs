// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::{Result, log};

/// Applies ad-hoc code signatures to all Mach-O binaries after extraction.
///
/// On macOS, unsigned Mach-O binaries are killed on Apple Silicon (`Killed: 9`) and blocked
/// by Gatekeeper on Intel. This function recursively walks the extracted content directory,
/// detects Mach-O files by their magic bytes, and applies ad-hoc signatures via `codesign --sign -`.
///
/// **Design: per-file signing only, no bundle sealing.**
///
/// Only individual Mach-O files are signed. Bundle seals (`_CodeSignature/CodeResources` inside
/// `.app` / `.framework` directories) are intentionally left as-is (stale after re-signing).
/// This is safe because:
/// - OCX packages are run via `ocx exec`, not launched through Finder. The kernel only checks
///   each binary's own embedded signature at load time — not the bundle-level `CodeResources`.
/// - Signing at the individual file level avoids "bundle format is ambiguous" errors and Team ID
///   conflicts from re-sealing third-party bundles (e.g. Qt frameworks signed by The Qt Company).
/// - Only `entitlements` are preserved (`flags` and `requirements` are intentionally dropped —
///   see `sign_binary` for the full rationale).
///
/// Hardlinked files (same inode) are signed only once. Symlinks are not followed.
///
/// On non-macOS platforms this is a no-op.
///
/// Signing failures are logged as warnings — they do not abort the installation.
/// Disable with `OCX_NO_CODESIGN=1`.
pub async fn sign_extracted_content(content_path: &Path) -> Result<()> {
    if cfg!(not(target_os = "macos")) {
        return Ok(());
    }

    if crate::env::flag("OCX_NO_CODESIGN", false) {
        log::debug!("Code signing disabled via OCX_NO_CODESIGN");
        return Ok(());
    }

    if !codesign_available() {
        log::warn!("codesign not found — skipping ad-hoc code signing");
        return Ok(());
    }

    remove_quarantine(content_path).await;

    let signed_inodes = Arc::new(Mutex::new(HashSet::new()));
    sign_directory(content_path.to_path_buf(), signed_inodes).await;

    Ok(())
}

// -- Mach-O detection ---------------------------------------------------------

/// Mach-O magic bytes (native and byte-swapped variants).
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

// -- Recursive per-file signing -----------------------------------------------

/// Recursively signs all Mach-O regular files under `path`.
///
/// - Recurses into subdirectories in parallel (symlinks are not followed).
/// - Signs each regular Mach-O file in parallel within each directory, deduplicated by inode.
/// - Bundle directories (`.app`, `.framework`) are recursed into but not
///   sealed — only the individual Mach-O files inside are signed.
///
/// Returns an explicit `Pin<Box<dyn Future + Send>>` so that the recursive call inside
/// `JoinSet::spawn` resolves to a concrete `Send` type, breaking the circularity that prevents
/// the compiler from proving `Send` for recursive `async fn` futures.
fn sign_directory(
    path: std::path::PathBuf,
    signed_inodes: Arc<Mutex<HashSet<u64>>>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
    Box::pin(async move {
        let Ok(mut read_dir) = tokio::fs::read_dir(&path).await else {
            return;
        };

        let mut subdirs = Vec::new();
        let mut files = Vec::new();

        while let Ok(Some(entry)) = read_dir.next_entry().await {
            let Ok(ft) = entry.file_type().await else {
                continue;
            };
            // file_type() does NOT follow symlinks — symlinks are neither recursed nor signed.
            if ft.is_dir() {
                subdirs.push(entry.path());
            } else if ft.is_file() {
                files.push(entry.path());
            }
        }

        // Recurse into subdirectories in parallel. Each call returns a Send future (explicit
        // return type above), so JoinSet::spawn accepts it without Box::pin.
        let mut subdir_tasks = tokio::task::JoinSet::new();
        for dir in subdirs {
            let inodes = Arc::clone(&signed_inodes);
            subdir_tasks.spawn(sign_directory(dir, inodes));
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
            if let Some(inode) = file_inode(&file).await
                && !signed_inodes
                    .lock()
                    .expect("signed_inodes mutex poisoned")
                    .insert(inode)
            {
                continue; // Already signed via hardlink
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

/// Returns the inode number of a regular file, used for hardlink deduplication.
#[cfg(unix)]
async fn file_inode(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    tokio::fs::metadata(path).await.ok().map(|m| m.ino())
}

#[cfg(not(unix))]
async fn file_inode(_path: &Path) -> Option<u64> {
    None
}

// -- Signing ------------------------------------------------------------------

fn codesign_available() -> bool {
    which::which("codesign").is_ok()
}

/// Remove quarantine extended attributes from the content directory.
/// Fails silently — the attribute may not exist.
async fn remove_quarantine(content_path: &Path) {
    let result = tokio::process::Command::new("xattr")
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

/// Signs a Mach-O binary with an ad-hoc signature, retrying with an inode workaround on failure.
///
/// Preserved metadata from the original signature:
/// - `entitlements`: app-declared capabilities (JIT, network extensions, sandbox) — must be
///   retained so the binary runs correctly under its intended privilege model.
///
/// Intentionally NOT preserved:
/// - `flags`: code signing flags such as `CS_RUNTIME` (hardened runtime). Preserving `CS_RUNTIME`
///   from a Developer-ID-signed binary (e.g. Kitware's cmake-gui, Qt Company's Qt frameworks)
///   enables Library Validation, which requires all loaded libraries to have the same Team ID as
///   the process. After ad-hoc re-signing, every binary has Team ID = "" (none). Apple treats
///   "" as "no Team ID" rather than a real Team ID, so Library Validation rejects ad-hoc libraries
///   even when both the process and library have identical empty Team IDs — causing the
///   "different Team IDs" dyld error at runtime. Dropping `flags` disables Library Validation,
///   allowing ad-hoc libraries to load. Note: Homebrew's `--preserve-metadata=flags` works because
///   Homebrew builds its own binaries from source; those binaries never have `CS_RUNTIME` to begin
///   with. OCX re-signs third-party pre-built binaries which do.
/// - `requirements`: the original Designated Requirement encodes the issuing certificate's Team ID.
///   An ad-hoc signature cannot satisfy a third-party Team ID constraint.
/// - `runtime`: SDK version metadata only; no effect on execution behavior.
///
/// If the first attempt fails (known Apple `codesign` bug with certain inodes), the file is
/// copied to a temp path (new inode), signed there, and moved back.
async fn sign_binary(path: &Path) {
    log::debug!("Signing Mach-O binary: {}", path.display());

    let args = &["--sign", "-", "--force", "--preserve-metadata=entitlements"];

    if try_codesign(args, path).await {
        return;
    }

    // Retry: copy to new inode, sign, move back (Apple codesign bug workaround).
    log::debug!("Retrying with inode workaround: {}", path.display());
    if let Err(e) = retry_sign_with_copy(args, path).await {
        log::warn!("Failed to sign {} (even after retry): {}", path.display(), e);
    }
}

/// Runs `codesign` with the given arguments. Returns `true` on success.
///
/// Failures are logged at DEBUG level — callers are responsible for logging at WARN
/// if all retry attempts are exhausted.
async fn try_codesign(args: &[&str], path: &Path) -> bool {
    let result = tokio::process::Command::new("codesign")
        .args(args)
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await;

    match result {
        Ok(output) if output.status.success() => {
            log::debug!("Signed: {}", path.display());
            true
        }
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

/// Copies the file to a temp path (new inode), signs it, and moves it back.
///
/// Works around a known Apple `codesign` bug where signing fails on certain inodes.
/// Homebrew uses the same technique in `codesign_patched_binary`.
///
/// The temp file is placed alongside the original with `.codesign_tmp` appended to the
/// full filename (not replacing the extension) to avoid collisions between files that share
/// a stem but differ only in extension (e.g. `foo.bar` and `foo.baz`).
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

// -- Tests --------------------------------------------------------------------

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

    #[tokio::test]
    async fn sign_directory_finds_standalone_binaries() {
        let dir = TempDir::new().unwrap();
        let bin_dir = dir.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();

        create_file_with_magic(&bin_dir, "tool_a", &0xFEED_FACFu32.to_be_bytes());
        create_file_with_magic(&bin_dir, "tool_b", &0xFEED_FACEu32.to_be_bytes());
        create_file_with_magic(&bin_dir, "script.sh", b"#!/b");

        let signed_inodes = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
        super::sign_directory(dir.path().to_path_buf(), signed_inodes.clone()).await;

        // Two Mach-O files; script.sh is not Mach-O.
        assert_eq!(signed_inodes.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn sign_directory_empty_directory() {
        let dir = TempDir::new().unwrap();
        let signed_inodes = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
        super::sign_directory(dir.path().to_path_buf(), signed_inodes.clone()).await;
        assert!(signed_inodes.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn sign_directory_ignores_non_macho_files() {
        let dir = TempDir::new().unwrap();
        create_file_with_magic(dir.path(), "readme.txt", b"Hell");
        create_file_with_magic(dir.path(), "data.json", b"{\"ke");
        create_file_with_magic(dir.path(), "image.png", &[0x89, b'P', b'N', b'G']);

        let signed_inodes = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
        super::sign_directory(dir.path().to_path_buf(), signed_inodes.clone()).await;
        assert!(signed_inodes.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn sign_directory_skips_symlinks() {
        let dir = TempDir::new().unwrap();
        let bin_dir = dir.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();

        let real = create_file_with_magic(&bin_dir, "real_binary", &0xFEED_FACFu32.to_be_bytes());
        let link = bin_dir.join("link_binary");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let signed_inodes = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
        super::sign_directory(dir.path().to_path_buf(), signed_inodes.clone()).await;

        // Only the real file should be signed, not the symlink.
        assert_eq!(signed_inodes.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn sign_directory_deduplicates_hardlinks() {
        let dir = TempDir::new().unwrap();
        let bin_dir = dir.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();

        let original = create_file_with_magic(&bin_dir, "original", &0xFEED_FACFu32.to_be_bytes());
        let hardlink = bin_dir.join("hardlink");
        std::fs::hard_link(&original, &hardlink).unwrap();

        let signed_inodes = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
        super::sign_directory(dir.path().to_path_buf(), signed_inodes.clone()).await;

        // Same inode — should only be signed once.
        assert_eq!(signed_inodes.lock().unwrap().len(), 1);
    }

    // -- sign_extracted_content (stubbed) -----------------------------------------

    #[tokio::test]
    async fn sign_extracted_content_noop_when_disabled() {
        let env = crate::test::env::lock();
        env.set("OCX_NO_CODESIGN", "1");

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
        let env = crate::test::env::lock();
        env.remove("OCX_NO_CODESIGN");

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
}
