use std::path::{Path, PathBuf};

use crate::{log, Result};

/// Applies ad-hoc code signatures to Mach-O binaries and `.app` bundles after extraction.
///
/// On macOS, unsigned Mach-O binaries are killed on Apple Silicon (`Killed: 9`) and blocked
/// by Gatekeeper on Intel. This function walks the extracted content directory, detects Mach-O
/// files by their magic bytes, and applies ad-hoc signatures via `codesign --sign -`.
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

    let targets = collect_sign_targets(content_path).await;
    sign_targets(targets).await;

    Ok(())
}

// -- Discovery (async) --------------------------------------------------------

/// Mach-O magic bytes (native and byte-swapped variants).
const MACHO_MAGIC: &[u32] = &[
    0xFEED_FACE, // MH_MAGIC (32-bit)
    0xFEED_FACF, // MH_MAGIC_64 (64-bit)
    0xCAFE_BABE, // FAT_MAGIC (Universal)
    0xCEFA_EDFE, // MH_CIGAM (32-bit, swapped)
    0xCFFA_EDFE, // MH_CIGAM_64 (64-bit, swapped)
    0xBEBA_FECA, // FAT_CIGAM (Universal, swapped)
];

#[cfg_attr(test, derive(Debug))]
enum SignTarget {
    AppBundle(PathBuf),
    Binary(PathBuf),
}

/// Walk the content directory asynchronously to collect signing targets.
///
/// `.app` bundles are collected first. Individual Mach-O binaries that live
/// outside any `.app` bundle are collected separately — files inside `.app`
/// bundles are skipped because `codesign --deep` already covers them.
async fn collect_sign_targets(content_path: &Path) -> Vec<SignTarget> {
    let entries = match walkdir(content_path).await {
        Ok(e) => e,
        Err(e) => {
            log::warn!("Failed to walk content directory for code signing: {}", e);
            return Vec::new();
        }
    };

    let mut targets = Vec::new();
    let mut app_bundle_prefixes: Vec<PathBuf> = Vec::new();

    // First: collect .app bundles.
    for entry in &entries {
        if entry.path.is_dir() && entry.path.extension().is_some_and(|ext| ext == "app") {
            app_bundle_prefixes.push(entry.path.clone());
            targets.push(SignTarget::AppBundle(entry.path.clone()));
        }
    }

    // Second: collect standalone Mach-O binaries outside .app bundles.
    for entry in &entries {
        if !entry.is_file {
            continue;
        }
        if app_bundle_prefixes.iter().any(|prefix| entry.path.starts_with(prefix)) {
            continue;
        }
        if is_macho(&entry.path).await {
            targets.push(SignTarget::Binary(entry.path.clone()));
        }
    }

    targets
}

struct DirEntry {
    path: PathBuf,
    is_file: bool,
}

async fn walkdir(path: &Path) -> std::io::Result<Vec<DirEntry>> {
    let mut entries = Vec::new();
    walk_recursive(path, &mut entries).await?;
    Ok(entries)
}

async fn walk_recursive(path: &Path, entries: &mut Vec<DirEntry>) -> std::io::Result<()> {
    let mut read_dir = tokio::fs::read_dir(path).await?;
    while let Some(entry) = read_dir.next_entry().await? {
        let file_type = entry.file_type().await?;
        let path = entry.path();
        entries.push(DirEntry {
            path: path.clone(),
            is_file: file_type.is_file(),
        });
        if file_type.is_dir() {
            Box::pin(walk_recursive(&path, entries)).await?;
        }
    }
    Ok(())
}

async fn is_macho(path: &Path) -> bool {
    let Ok(mut file) = tokio::fs::File::open(path).await else {
        return false;
    };

    let mut magic = [0u8; 4];
    if tokio::io::AsyncReadExt::read_exact(&mut file, &mut magic).await.is_err() {
        return false;
    }

    let value = u32::from_be_bytes(magic);
    MACHO_MAGIC.contains(&value)
}

// -- Signing (async, parallel) ------------------------------------------------

async fn sign_targets(targets: Vec<SignTarget>) {
    let mut tasks = tokio::task::JoinSet::new();

    for target in targets {
        match target {
            SignTarget::AppBundle(path) => {
                tasks.spawn(async move { sign_app_bundle(&path).await });
            }
            SignTarget::Binary(path) => {
                tasks.spawn(async move { sign_binary(&path).await });
            }
        }
    }

    while let Some(result) = tasks.join_next().await {
        if let Err(e) = result {
            log::warn!("Code signing task panicked: {}", e);
        }
    }
}

fn codesign_available() -> bool {
    find_in_path("codesign").is_some()
}

/// Search `PATH` for an executable by name. Returns the first match.
fn find_in_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
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
            log::debug!("xattr quarantine removal returned non-zero (attribute may not exist): {}", stderr.trim());
        }
        Err(e) => {
            log::debug!("xattr command failed: {}", e);
        }
        _ => {}
    }
}

/// Run a codesign command, capture all output, and log the result.
async fn run_codesign(args: &[&str], path: &Path, label: &str) {
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
            log::debug!("Signed {}: {}", label, path.display());
        }
        Ok(output) => {
            let combined = format_process_output(&output.stdout, &output.stderr);
            log::warn!("Failed to sign {} {}: {}", label, path.display(), combined);
        }
        Err(e) => {
            log::warn!("Failed to run codesign on {} {}: {}", label, path.display(), e);
        }
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

async fn sign_app_bundle(path: &Path) {
    log::debug!("Signing .app bundle: {}", path.display());
    run_codesign(&["--sign", "-", "--force", "--deep"], path, ".app bundle").await;
}

async fn sign_binary(path: &Path) {
    log::debug!("Signing Mach-O binary: {}", path.display());
    run_codesign(
        &["--sign", "-", "--force", "--preserve-metadata=entitlements,requirements,flags,runtime"],
        path,
        "Mach-O binary",
    ).await;
}

// -- Tests --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::{SignTarget, MACHO_MAGIC};

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
            assert!(super::is_macho(&path).await, "should detect swapped magic {:#010x}", magic);
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

    // -- collect_sign_targets -----------------------------------------------------

    #[tokio::test]
    async fn collect_targets_finds_standalone_binaries() {
        let dir = TempDir::new().unwrap();
        let bin_dir = dir.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();

        create_file_with_magic(&bin_dir, "tool_a", &0xFEED_FACFu32.to_be_bytes());
        create_file_with_magic(&bin_dir, "tool_b", &0xFEED_FACEu32.to_be_bytes());
        create_file_with_magic(&bin_dir, "script.sh", b"#!/b");

        let targets = super::collect_sign_targets(dir.path()).await;
        let bin_count = targets.iter().filter(|t| matches!(t, SignTarget::Binary(_))).count();
        assert_eq!(bin_count, 2);
    }

    #[tokio::test]
    async fn collect_targets_finds_app_bundles() {
        let dir = TempDir::new().unwrap();
        let app_macos = dir.path().join("MyApp.app").join("Contents").join("MacOS");
        std::fs::create_dir_all(&app_macos).unwrap();
        create_file_with_magic(&app_macos, "MyApp", &0xFEED_FACFu32.to_be_bytes());

        let targets = super::collect_sign_targets(dir.path()).await;
        let app_count = targets.iter().filter(|t| matches!(t, SignTarget::AppBundle(_))).count();
        assert_eq!(app_count, 1);
    }

    #[tokio::test]
    async fn collect_targets_skips_binaries_inside_app_bundle() {
        let dir = TempDir::new().unwrap();
        let app_macos = dir.path().join("Foo.app").join("Contents").join("MacOS");
        std::fs::create_dir_all(&app_macos).unwrap();
        create_file_with_magic(&app_macos, "Foo", &0xFEED_FACFu32.to_be_bytes());
        create_file_with_magic(&app_macos, "helper", &0xFEED_FACFu32.to_be_bytes());

        let targets = super::collect_sign_targets(dir.path()).await;
        let app_count = targets.iter().filter(|t| matches!(t, SignTarget::AppBundle(_))).count();
        let bin_count = targets.iter().filter(|t| matches!(t, SignTarget::Binary(_))).count();

        assert_eq!(app_count, 1, "should find the .app bundle");
        assert_eq!(bin_count, 0, "should not list binaries inside .app separately");
    }

    #[tokio::test]
    async fn collect_targets_mixed_content() {
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

        let targets = super::collect_sign_targets(&content).await;
        let app_count = targets.iter().filter(|t| matches!(t, SignTarget::AppBundle(_))).count();
        let bin_count = targets.iter().filter(|t| matches!(t, SignTarget::Binary(_))).count();

        assert_eq!(app_count, 1, "should find exactly one .app bundle");
        assert_eq!(bin_count, 1, "should find exactly one standalone Mach-O");
    }

    #[tokio::test]
    async fn collect_targets_empty_directory() {
        let dir = TempDir::new().unwrap();
        let targets = super::collect_sign_targets(dir.path()).await;
        assert!(targets.is_empty());
    }

    #[tokio::test]
    async fn collect_targets_ignores_non_macho_files() {
        let dir = TempDir::new().unwrap();
        create_file_with_magic(dir.path(), "readme.txt", b"Hell");
        create_file_with_magic(dir.path(), "data.json", b"{\"ke");
        create_file_with_magic(dir.path(), "image.png", &[0x89, b'P', b'N', b'G']);

        let targets = super::collect_sign_targets(dir.path()).await;
        assert!(targets.is_empty());
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
        assert!(compile.status.success(), "failed to compile test binary: {}",
            String::from_utf8_lossy(&compile.stderr));
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
        assert!(!verify_signature(&binary).await, "binary should be unsigned before signing");

        super::sign_binary(&binary).await;

        assert!(verify_signature(&binary).await, "binary should have a valid ad-hoc signature after signing");
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn sign_app_bundle_signs_app() {
        let dir = TempDir::new().unwrap();
        let app_dir = dir.path().join("Test.app");
        let macos_dir = app_dir.join("Contents").join("MacOS");
        std::fs::create_dir_all(&macos_dir).unwrap();
        build_unsigned_binary(&macos_dir, "Test");
        assert!(!verify_signature(&app_dir).await, ".app bundle should be unsigned before signing");

        super::sign_app_bundle(&app_dir).await;

        assert!(verify_signature(&app_dir).await, ".app bundle should have a valid ad-hoc signature after signing");
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
        assert!(!verify_signature(&binary).await, "binary should be unsigned before signing");

        let result = super::sign_extracted_content(&content).await;
        assert!(result.is_ok(), "sign_extracted_content should succeed");

        assert!(verify_signature(&binary).await, "binary should be signed after sign_extracted_content");
    }
}
