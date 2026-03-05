use std::collections::HashSet;
use std::path::Path;
use std::sync::Mutex;

use crate::{Result, log};

/// Applies ad-hoc code signatures to Mach-O binaries after extraction.
///
/// On macOS, unsigned Mach-O binaries are killed on Apple Silicon (`Killed: 9`) and blocked
/// by Gatekeeper on Intel. This function recursively walks the extracted content directory,
/// detects Mach-O files by their magic bytes, and applies ad-hoc signatures via `codesign --sign -`.
///
/// Signing is performed inside-out: subdirectories are processed before the current directory,
/// so nested bundles (`.framework`, `.app`) are sealed after their contents are signed.
/// Hardlinked files (same inode) are signed only once. Symlinks are not followed.
///
/// On non-macOS platforms this is a no-op.
///
/// Signing failures are logged as warnings — they do not abort the installation.
/// Disable with `OCX_DISABLE_CODESIGN=1`.
pub async fn sign_extracted_content(content_path: &Path) -> Result<()> {
    if cfg!(not(target_os = "macos")) {
        return Ok(());
    }

    if crate::env::flag("OCX_DISABLE_CODESIGN", false) {
        log::debug!("Code signing disabled via OCX_DISABLE_CODESIGN");
        return Ok(());
    }

    if !codesign_available() {
        log::warn!("codesign not found — skipping ad-hoc code signing");
        return Ok(());
    }

    remove_quarantine(content_path).await;

    let signed_inodes = Mutex::new(HashSet::new());
    sign_directory(content_path, &signed_inodes).await;

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

// -- Recursive inside-out signing ---------------------------------------------

/// Recursively signs all Mach-O binaries and bundles in inside-out order.
///
/// 1. Recurse into real subdirectories (symlinks are not followed)
/// 2. Sign Mach-O files in this directory in parallel (deduplicated by inode)
/// 3. If this directory is a bundle (`.app` / `.framework`), sign the bundle itself
///
/// Inode tracking via `signed_inodes` prevents signing the same physical file
/// twice when hardlinks exist across directories.
async fn sign_directory(path: &Path, signed_inodes: &Mutex<HashSet<u64>>) {
    let Ok(mut read_dir) = tokio::fs::read_dir(path).await else {
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

    // Step 1: Recurse into subdirectories first (inside-out order).
    // Sequential at this level so the Mutex sees a consistent inode set;
    // parallelism happens within each directory's file signing.
    for dir in subdirs {
        Box::pin(sign_directory(&dir, signed_inodes)).await;
    }

    // Step 2: Sign Mach-O files in this directory (parallel, with inode dedup).
    let mut tasks = tokio::task::JoinSet::new();
    for file in files {
        if !is_macho(&file).await {
            continue;
        }
        if let Some(inode) = file_inode(&file).await
            && !signed_inodes.lock().unwrap().insert(inode)
        {
            continue; // Already signed via hardlink
        }
        tasks.spawn(async move { sign_binary(&file).await });
    }
    while let Some(result) = tasks.join_next().await {
        if let Err(e) = result {
            log::warn!("Code signing task panicked: {}", e);
        }
    }

    // Step 3: Sign the bundle after all its contents are signed.
    if is_bundle(path) {
        sign_bundle(path).await;
    }
}

/// Returns true for directories that are macOS bundles (`.app` or `.framework`).
fn is_bundle(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "app" || ext == "framework")
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

/// Signs a Mach-O binary with ad-hoc signature, retrying with an inode workaround on failure.
///
/// Uses `--preserve-metadata=entitlements,requirements,flags,runtime` to retain any existing
/// entitlements and hardened-runtime settings from the original signature.
///
/// If the first attempt fails (known Apple `codesign` bug with certain inodes), the file is
/// copied to a temp path (new inode), signed there, and moved back.
async fn sign_binary(path: &Path) {
    log::debug!("Signing Mach-O binary: {}", path.display());

    let args = &[
        "--sign",
        "-",
        "--force",
        "--preserve-metadata=entitlements,requirements,flags,runtime",
    ];

    if try_codesign(args, path).await {
        return;
    }

    // Retry: copy to new inode, sign, move back (Apple codesign bug workaround).
    log::debug!("Retrying with inode workaround: {}", path.display());
    if let Err(e) = retry_sign_with_copy(args, path).await {
        log::warn!("Failed to sign {} (even after retry): {}", path.display(), e);
    }
}

/// Signs a bundle (`.app` or `.framework`) without `--deep`.
///
/// Expects all nested content to already be signed (called after recursive descent).
async fn sign_bundle(path: &Path) {
    log::debug!("Signing bundle: {}", path.display());
    try_codesign(&["--sign", "-", "--force"], path).await;
}

/// Runs `codesign` with the given arguments. Returns `true` on success.
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
            log::warn!("codesign failed for {}: {}", path.display(), msg);
            false
        }
        Err(e) => {
            log::warn!("Failed to run codesign on {}: {}", path.display(), e);
            false
        }
    }
}

/// Copies the file to a temp path (new inode), signs it, and moves it back.
///
/// Works around a known Apple `codesign` bug where signing fails on certain inodes.
/// Homebrew uses the same technique in `codesign_patched_binary`.
async fn retry_sign_with_copy(args: &[&str], path: &Path) -> std::io::Result<()> {
    let tmp = path.with_extension("codesign_tmp");
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
        std::fs::write(&path, &[0xFE, 0xED]).unwrap(); // only 2 bytes
        assert!(!super::is_macho(&path).await);
    }

    // -- is_bundle ----------------------------------------------------------------

    #[test]
    fn is_bundle_detects_app() {
        assert!(super::is_bundle(std::path::Path::new("/tmp/Foo.app")));
    }

    #[test]
    fn is_bundle_detects_framework() {
        assert!(super::is_bundle(std::path::Path::new("/tmp/QtCore.framework")));
    }

    #[test]
    fn is_bundle_rejects_plain_directory() {
        assert!(!super::is_bundle(std::path::Path::new("/tmp/bin")));
    }

    #[test]
    fn is_bundle_rejects_file_extension() {
        assert!(!super::is_bundle(std::path::Path::new("/tmp/file.txt")));
    }

    // -- sign_directory -----------------------------------------------------------

    #[tokio::test]
    async fn sign_directory_finds_standalone_binaries() {
        // Verifies that sign_directory visits Mach-O files by checking the inode set.
        let dir = TempDir::new().unwrap();
        let bin_dir = dir.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();

        create_file_with_magic(&bin_dir, "tool_a", &0xFEED_FACFu32.to_be_bytes());
        create_file_with_magic(&bin_dir, "tool_b", &0xFEED_FACEu32.to_be_bytes());
        create_file_with_magic(&bin_dir, "script.sh", b"#!/b");

        let signed_inodes = std::sync::Mutex::new(std::collections::HashSet::new());
        super::sign_directory(dir.path(), &signed_inodes).await;

        // Two Mach-O files should have their inodes recorded (script.sh is not Mach-O).
        assert_eq!(signed_inodes.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn sign_directory_finds_binaries_inside_app_bundle() {
        // With the new approach, binaries inside .app bundles ARE signed individually.
        let dir = TempDir::new().unwrap();
        let app_macos = dir.path().join("MyApp.app").join("Contents").join("MacOS");
        std::fs::create_dir_all(&app_macos).unwrap();

        create_file_with_magic(&app_macos, "MyApp", &0xFEED_FACFu32.to_be_bytes());

        let signed_inodes = std::sync::Mutex::new(std::collections::HashSet::new());
        super::sign_directory(dir.path(), &signed_inodes).await;

        assert_eq!(signed_inodes.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn sign_directory_finds_all_binaries_in_app() {
        let dir = TempDir::new().unwrap();
        let app_macos = dir.path().join("Foo.app").join("Contents").join("MacOS");
        std::fs::create_dir_all(&app_macos).unwrap();
        create_file_with_magic(&app_macos, "Foo", &0xFEED_FACFu32.to_be_bytes());
        create_file_with_magic(&app_macos, "helper", &0xFEED_FACFu32.to_be_bytes());

        let signed_inodes = std::sync::Mutex::new(std::collections::HashSet::new());
        super::sign_directory(dir.path(), &signed_inodes).await;

        assert_eq!(
            signed_inodes.lock().unwrap().len(),
            2,
            "should sign both Mach-O binaries inside .app"
        );
    }

    #[tokio::test]
    async fn sign_directory_mixed_content() {
        let dir = TempDir::new().unwrap();
        let content = dir.path().join("content");
        std::fs::create_dir_all(&content).unwrap();

        // Standalone Mach-O
        let bin_dir = content.join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        create_file_with_magic(&bin_dir, "tool", &0xFEED_FACFu32.to_be_bytes());

        // Shell script (not Mach-O)
        create_file_with_magic(&bin_dir, "wrapper.sh", b"#!/b");

        // .app bundle with Mach-O inside
        let app_macos = content.join("MyApp.app").join("Contents").join("MacOS");
        std::fs::create_dir_all(&app_macos).unwrap();
        create_file_with_magic(&app_macos, "MyApp", &0xFEED_FACFu32.to_be_bytes());

        // ELF binary (should be ignored)
        create_file_with_magic(&bin_dir, "linux_bin", &[0x7F, b'E', b'L', b'F']);

        let signed_inodes = std::sync::Mutex::new(std::collections::HashSet::new());
        super::sign_directory(&content, &signed_inodes).await;

        assert_eq!(
            signed_inodes.lock().unwrap().len(),
            2,
            "should sign standalone Mach-O + Mach-O inside .app"
        );
    }

    #[tokio::test]
    async fn sign_directory_empty_directory() {
        let dir = TempDir::new().unwrap();
        let signed_inodes = std::sync::Mutex::new(std::collections::HashSet::new());
        super::sign_directory(dir.path(), &signed_inodes).await;
        assert!(signed_inodes.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn sign_directory_ignores_non_macho_files() {
        let dir = TempDir::new().unwrap();
        create_file_with_magic(dir.path(), "readme.txt", b"Hell");
        create_file_with_magic(dir.path(), "data.json", b"{\"ke");
        create_file_with_magic(dir.path(), "image.png", &[0x89, b'P', b'N', b'G']);

        let signed_inodes = std::sync::Mutex::new(std::collections::HashSet::new());
        super::sign_directory(dir.path(), &signed_inodes).await;
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

        let signed_inodes = std::sync::Mutex::new(std::collections::HashSet::new());
        super::sign_directory(dir.path(), &signed_inodes).await;

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

        let signed_inodes = std::sync::Mutex::new(std::collections::HashSet::new());
        super::sign_directory(dir.path(), &signed_inodes).await;

        // Same inode — should only be signed once.
        assert_eq!(signed_inodes.lock().unwrap().len(), 1);
    }

    // -- sign_extracted_content (stubbed) -----------------------------------------

    #[tokio::test]
    async fn sign_extracted_content_noop_when_disabled() {
        let env = crate::test::env::lock();
        env.set("OCX_DISABLE_CODESIGN", "1");

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
        env.remove("OCX_DISABLE_CODESIGN");

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
    async fn sign_extracted_content_signs_app_bundle_inside_out() {
        let env = crate::test::env::lock();
        env.remove("OCX_DISABLE_CODESIGN");

        let dir = TempDir::new().unwrap();
        let content = dir.path().join("content");
        let app_dir = content.join("Test.app");
        let macos_dir = app_dir.join("Contents").join("MacOS");
        std::fs::create_dir_all(&macos_dir).unwrap();

        let binary = build_unsigned_binary(&macos_dir, "Test");
        assert!(
            !verify_signature(&binary).await,
            "binary should be unsigned before signing"
        );

        let result = super::sign_extracted_content(&content).await;
        assert!(result.is_ok(), "sign_extracted_content should succeed");

        assert!(verify_signature(&binary).await, "binary inside .app should be signed");
        assert!(
            verify_signature(&app_dir).await,
            ".app bundle should be signed after its contents"
        );
    }
}
