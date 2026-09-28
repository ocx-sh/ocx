// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Deterministic wheel → `tar.zst` repack with `.data` relocation.
//!
//! The layer holds the final relocated tree across three prefixes (`lib/site-packages/`, `bin/`, the content root),
//! so it must apply at the content root with an empty [`LayerLayoutSpec`](ocx_oci::LayerLayoutSpec).

use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// The repack-determinism grammar version, stamped as a `repack-vN` annotation.
pub const REPACK_VERSION: &str = "repack-v1";

// Pinned: another level changes every layer's bytes and digest.
const ZSTD_LEVEL: i32 = 3;

const MODE_FILE: u32 = 0o644;
const MODE_EXECUTABLE: u32 = 0o755;

/// Zip-bomb budget for a wheel's total decompressed bytes.
const MAX_TOTAL_DECOMPRESSED_BYTES: u64 = 1 << 30;

/// A repacked wheel layer plus the metadata `compose` and `collide` need.
#[derive(Debug, Clone)]
pub struct RepackedWheel {
    /// The source wheel filename; `compose` parses its ABI tag.
    pub filename: String,
    /// Path to the written `tar.zst` layer.
    pub layer_path: PathBuf,
    /// The OCI digest of the layer (`sha256:…`).
    pub layer_digest: String,
    /// The `sha256` of the source wheel.
    pub wheel_sha256: String,
    /// The `[console_scripts]` entry points.
    pub entry_points: Vec<ConsoleScript>,
    /// Every installed path from the wheel `RECORD`, post-relocation and sorted.
    pub record_paths: Vec<String>,
}

/// A `[console_scripts]` entry point, as extracted from the wheel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleScript {
    /// The script name (the generated launcher's invocable name).
    pub name: String,
    /// The raw, unparsed object reference `module[:attr[.attr…]]`.
    pub reference: String,
    /// The extras that must be requested for this script to be synthesized; empty means always.
    pub extras: Vec<String>,
}

/// Repacks a wheel into a deterministic `tar.zst` layer under `output_dir`.
///
/// Blocks despite being `async`: the zip read and tar/zstd write run inline.
///
/// # Errors
///
/// [`RepackError::Io`] on a filesystem failure, [`RepackError::Zip`] for an unreadable zip,
/// [`RepackError::UnsafeEntryPath`] for an entry escaping the wheel root, and [`RepackError::WheelTooLarge`] past the
/// zip-bomb budget.
pub async fn repack_wheel(wheel_path: &Path, output_dir: &Path) -> Result<RepackedWheel, RepackError> {
    repack_wheel_with_budget(wheel_path, output_dir, MAX_TOTAL_DECOMPRESSED_BYTES).await
}

/// [`repack_wheel`] with the decompressed-size budget as a parameter, for tests.
async fn repack_wheel_with_budget(
    wheel_path: &Path,
    output_dir: &Path,
    decompressed_budget: u64,
) -> Result<RepackedWheel, RepackError> {
    // ponytail: runs inline, no `spawn_blocking`; a caller wraps it in its own if large wheels stall the executor.
    let filename = wheel_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();

    let wheel_bytes = std::fs::read(wheel_path).map_err(RepackError::Io)?;
    std::fs::create_dir_all(output_dir).map_err(RepackError::Io)?;

    let wheel_sha256 = hex_encode(Sha256::digest(&wheel_bytes));

    let mut zip = zip::ZipArchive::new(Cursor::new(wheel_bytes.as_slice())).map_err(RepackError::Zip)?;

    let mut tree = Vec::with_capacity(zip.len());
    let mut record_text: Option<String> = None;
    let mut entry_points_text: Option<String> = None;
    let mut decompressed_total: u64 = 0;

    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).map_err(RepackError::Zip)?;
        if entry.is_dir() {
            continue;
        }
        let raw_name = entry.name().to_string();
        let enclosed = entry
            .enclosed_name()
            .ok_or_else(|| RepackError::UnsafeEntryPath(raw_name.clone()))?;
        let components = path_components(&enclosed);

        let remaining_budget = decompressed_budget.saturating_sub(decompressed_total);
        let data = read_entry_capped(&mut entry, remaining_budget, decompressed_budget)?;
        decompressed_total += data.len() as u64;

        if let [dist_info, leaf] = components.as_slice()
            && dist_info.ends_with(".dist-info")
        {
            match leaf.as_str() {
                "RECORD" => record_text = Some(String::from_utf8_lossy(&data).into_owned()),
                "entry_points.txt" => entry_points_text = Some(String::from_utf8_lossy(&data).into_owned()),
                _ => {}
            }
        }

        let (path, executable) = relocate(&components);
        if path.is_empty() {
            continue;
        }
        tree.push(TreeEntry { path, executable, data });
    }

    tree.sort_by(|a, b| a.path.cmp(&b.path));

    let layer_bytes = write_deterministic_tar_zst(&tree)?;
    let digest_hex = hex_encode(Sha256::digest(&layer_bytes));
    let layer_digest = format!("sha256:{digest_hex}");
    let layer_path = output_dir.join(format!("{digest_hex}.tar.zst"));
    std::fs::write(&layer_path, &layer_bytes).map_err(RepackError::Io)?;

    let entry_points = entry_points_text
        .as_deref()
        .map(parse_console_scripts)
        .unwrap_or_default();
    let mut record_paths = match record_text.as_deref() {
        Some(text) => relocate_record_paths(text)?,
        None => Vec::new(),
    };
    record_paths.sort();

    Ok(RepackedWheel {
        filename,
        layer_path,
        layer_digest,
        wheel_sha256,
        entry_points,
        record_paths,
    })
}

/// Distribution metadata from a wheel's `*.dist-info/METADATA` (PEP 566 core metadata).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WheelDescription {
    /// The `Summary` header (one-line project description), when present.
    pub summary: Option<String>,
    /// The `Keywords` header (comma-separated), when present.
    pub keywords: Option<String>,
    /// The `License` header, when present.
    pub license: Option<String>,
}

/// Zip-bomb budget for one `*.dist-info/METADATA` entry.
const MAX_METADATA_BYTES: u64 = 1 << 20;

/// Reads a wheel's `METADATA` header fields; all `None` when the wheel has no `METADATA`.
///
/// # Errors
///
/// [`RepackError::Io`] on a filesystem failure, [`RepackError::Zip`] for an unreadable zip, and
/// [`RepackError::WheelTooLarge`] for a `METADATA` over its zip-bomb budget.
pub fn read_wheel_description(wheel_path: &Path) -> Result<WheelDescription, RepackError> {
    let wheel_bytes = std::fs::read(wheel_path).map_err(RepackError::Io)?;
    let mut zip = zip::ZipArchive::new(Cursor::new(wheel_bytes.as_slice())).map_err(RepackError::Zip)?;

    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).map_err(RepackError::Zip)?;
        if entry.is_dir() {
            continue;
        }
        let Some(enclosed) = entry.enclosed_name() else {
            continue;
        };
        let components = path_components(&enclosed);
        if let [dist_info, leaf] = components.as_slice()
            && dist_info.ends_with(".dist-info")
            && leaf == "METADATA"
        {
            let data = read_entry_capped(&mut entry, MAX_METADATA_BYTES, MAX_METADATA_BYTES)?;
            return Ok(parse_wheel_metadata(&String::from_utf8_lossy(&data)));
        }
    }
    Ok(WheelDescription::default())
}

/// Parses the `Summary`, `Keywords` and `License` headers; `UNKNOWN` (setuptools' placeholder) counts as absent.
fn parse_wheel_metadata(text: &str) -> WheelDescription {
    let mut description = WheelDescription::default();
    for line in text.lines() {
        if line.is_empty() {
            break;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() || value == "UNKNOWN" {
            continue;
        }
        match key.trim() {
            "Summary" => description.summary = Some(value.to_string()),
            "Keywords" => description.keywords = Some(value.to_string()),
            "License" => description.license = Some(value.to_string()),
            _ => {}
        }
    }
    description
}

/// Reads a zip entry, capping actual decompressed bytes at `remaining_budget`.
///
/// Caps the bytes read, never the declared size, which is attacker-controlled.
fn read_entry_capped<R: Read>(entry: &mut R, remaining_budget: u64, total_budget: u64) -> Result<Vec<u8>, RepackError> {
    let mut data = Vec::new();
    entry
        .take(remaining_budget.saturating_add(1))
        .read_to_end(&mut data)
        .map_err(RepackError::Io)?;
    if data.len() as u64 > remaining_budget {
        return Err(RepackError::WheelTooLarge { limit: total_budget });
    }
    Ok(data)
}

struct TreeEntry {
    path: String,
    executable: bool,
    data: Vec<u8>,
}

fn path_components(path: &Path) -> Vec<String> {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect()
}

/// Relocates a wheel-relative path into the final tree; returns the path and whether it is executable.
fn relocate(components: &[String]) -> (String, bool) {
    let Some(first) = components.first() else {
        return (String::new(), false);
    };
    if first.ends_with(".data") {
        let rest = components.get(2..).unwrap_or(&[]).join("/");
        return match components.get(1).map(String::as_str) {
            Some("scripts") => (format!("bin/{rest}"), true),
            Some("data") => (rest, false),
            // ponytail: `.data/headers` also lands in site-packages; split it out when a real wheel needs it.
            _ => (format!("lib/site-packages/{rest}"), false),
        };
    }
    (format!("lib/site-packages/{}", components.join("/")), false)
}

/// Splits a RECORD path into relative components, rejecting absolute paths and `..` traversal.
fn record_components(raw_path: &str) -> Result<Vec<String>, RepackError> {
    if raw_path.starts_with('/') {
        return Err(RepackError::UnsafeEntryPath(raw_path.to_string()));
    }
    let mut components = Vec::new();
    for part in raw_path.split('/') {
        match part {
            "" | "." => continue,
            ".." => return Err(RepackError::UnsafeEntryPath(raw_path.to_string())),
            _ => components.push(part.to_string()),
        }
    }
    Ok(components)
}

/// Relocates each PEP 376 `RECORD` path the same way the layer entries were.
fn relocate_record_paths(record_text: &str) -> Result<Vec<String>, RepackError> {
    record_text
        .lines()
        .filter_map(|line| {
            // ponytail: first-field split misreads CSV-quoted paths with commas; use a CSV parser when one appears.
            let field = line.split(',').next()?.trim();
            (!field.is_empty()).then(|| field.to_string())
        })
        .map(|raw| record_components(&raw).map(|components| relocate(&components).0))
        .filter(|relocated| !matches!(relocated, Ok(path) if path.is_empty()))
        .collect()
}

/// Writes `tree` as a deterministic `tar.zst`; the caller must sort `tree` by path first.
fn write_deterministic_tar_zst(tree: &[TreeEntry]) -> Result<Vec<u8>, RepackError> {
    let encoder = zstd::stream::write::Encoder::new(Vec::new(), ZSTD_LEVEL).map_err(RepackError::Io)?;
    let mut builder = tar::Builder::new(encoder);
    for entry in tree {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(entry.data.len() as u64);
        header.set_mode(if entry.executable { MODE_EXECUTABLE } else { MODE_FILE });
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        builder
            .append_data(&mut header, &entry.path, entry.data.as_slice())
            .map_err(RepackError::Io)?;
    }
    let encoder = builder.into_inner().map_err(RepackError::Io)?;
    encoder.finish().map_err(RepackError::Io)
}

/// Parses `[console_scripts]` entries from `entry_points.txt`, keeping each object reference verbatim.
fn parse_console_scripts(text: &str) -> Vec<ConsoleScript> {
    let mut scripts = Vec::new();
    let mut in_console_scripts = false;
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            in_console_scripts = line.eq_ignore_ascii_case("[console_scripts]");
            continue;
        }
        if !in_console_scripts {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        let (reference, extras) = match value.trim().split_once('[') {
            Some((reference, extras)) => (
                reference.trim().to_string(),
                extras
                    .trim_end()
                    .trim_end_matches(']')
                    .split(',')
                    .map(str::trim)
                    .filter(|extra| !extra.is_empty())
                    .map(str::to_string)
                    .collect(),
            ),
            None => (value.trim().to_string(), Vec::new()),
        };
        scripts.push(ConsoleScript {
            name: name.trim().to_string(),
            reference,
            extras,
        });
    }
    scripts
}

fn hex_encode(bytes: impl AsRef<[u8]>) -> String {
    bytes.as_ref().iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Errors from repacking a wheel.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RepackError {
    /// A filesystem read/write failed.
    #[error("I/O error repacking wheel")]
    Io(#[source] std::io::Error),
    /// The wheel could not be read as a zip archive.
    #[error("failed to read wheel zip")]
    Zip(#[source] zip::result::ZipError),
    /// A wheel entry or `RECORD` path escapes the wheel root (zip-slip).
    #[error("unsafe path in wheel entry: {0}")]
    UnsafeEntryPath(String),
    /// The decompressed content exceeds the `limit`-byte zip-bomb budget.
    #[error("wheel decompressed size exceeds the {limit}-byte safety budget")]
    WheelTooLarge {
        /// The decompressed-byte budget that was exceeded.
        limit: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drives a future to completion without a runtime. Production code in
    /// this module never actually suspends (see the `ponytail` note in
    /// [`repack_wheel`]), so the first poll always resolves.
    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        let mut future = std::pin::pin!(future);
        let waker = std::task::Waker::noop();
        let mut context = std::task::Context::from_waker(waker);
        match future.as_mut().poll(&mut context) {
            std::task::Poll::Ready(value) => value,
            std::task::Poll::Pending => {
                panic!("repack_wheel unexpectedly returned Pending (no async runtime in this crate)")
            }
        }
    }

    fn fixture_wheel(name: &str) -> PathBuf {
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/wheels")).join(name)
    }

    /// A fresh, per-test scratch directory under the OS temp dir (no
    /// `tempfile` dependency declared for this crate).
    fn scratch_dir(label: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "ocx_python-repack-test-{label}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    /// Decompresses + reads back a written layer as `(path, mode, mtime, data)` tuples.
    fn read_tar_entries(layer_path: &Path) -> Vec<(String, u32, u64, Vec<u8>)> {
        let file = std::fs::File::open(layer_path).expect("open layer");
        let decoder = zstd::stream::read::Decoder::new(file).expect("zstd decoder");
        let mut archive = tar::Archive::new(decoder);
        archive
            .entries()
            .expect("tar entries")
            .map(|entry| {
                let mut entry = entry.expect("tar entry");
                let path = entry.path().expect("entry path").to_string_lossy().into_owned();
                let mode = entry.header().mode().expect("entry mode");
                let mtime = entry.header().mtime().expect("entry mtime");
                let mut data = Vec::new();
                entry.read_to_end(&mut data).expect("read entry data");
                (path, mode, mtime, data)
            })
            .collect()
    }

    #[test]
    fn purelib_wheel_relocates_into_site_packages() {
        let output_dir = scratch_dir("purelib");
        let repacked = block_on(repack_wheel(
            &fixture_wheel("purelib_pkg-1.0.0-py3-none-any.whl"),
            &output_dir,
        ))
        .expect("repack succeeds");

        assert_eq!(repacked.filename, "purelib_pkg-1.0.0-py3-none-any.whl");
        assert!(repacked.layer_digest.starts_with("sha256:"));
        assert_eq!(repacked.wheel_sha256.len(), 64, "wheel_sha256 is a bare hex digest");
        assert!(repacked.entry_points.is_empty());

        let entries = read_tar_entries(&repacked.layer_path);
        let paths: Vec<&str> = entries.iter().map(|(path, ..)| path.as_str()).collect();
        assert!(paths.contains(&"lib/site-packages/purelib_pkg/__init__.py"));
        assert!(paths.contains(&"lib/site-packages/purelib_pkg/core.py"));
        assert!(paths.contains(&"lib/site-packages/purelib_pkg-1.0.0.dist-info/METADATA"));
        assert!(paths.contains(&"lib/site-packages/purelib_pkg-1.0.0.dist-info/RECORD"));

        for (path, mode, mtime, _) in &entries {
            assert_eq!(*mtime, 0, "{path} did not get an epoch mtime");
            assert_eq!(*mode, 0o644, "{path} should be a plain 0o644 file");
        }

        assert!(
            repacked
                .record_paths
                .contains(&"lib/site-packages/purelib_pkg/__init__.py".to_string())
        );

        std::fs::remove_dir_all(&output_dir).ok();
    }

    #[test]
    fn data_pkg_relocates_scripts_and_data() {
        let output_dir = scratch_dir("data-pkg");
        let repacked = block_on(repack_wheel(
            &fixture_wheel("data_pkg-1.0.0-py3-none-any.whl"),
            &output_dir,
        ))
        .expect("repack succeeds");

        let entries = read_tar_entries(&repacked.layer_path);
        let by_path: std::collections::HashMap<&str, (u32, u64, &[u8])> = entries
            .iter()
            .map(|(path, mode, mtime, data)| (path.as_str(), (*mode, *mtime, data.as_slice())))
            .collect();

        let (launcher_mode, launcher_mtime, launcher_data) = *by_path
            .get("bin/data_pkg-launcher")
            .expect("launcher relocated to bin/");
        assert_eq!(launcher_mode, 0o755, "script launcher must be executable");
        assert_eq!(launcher_mtime, 0);
        assert_eq!(launcher_data, b"#!python\nimport data_pkg\nprint('launched')\n");

        let (config_mode, _, config_data) = *by_path
            .get("share/data_pkg/config.json")
            .expect(".data/data relocated to content root");
        assert_eq!(config_mode, 0o644);
        assert_eq!(config_data, b"{\"greeting\": \"hi\"}\n");

        assert!(by_path.contains_key("lib/site-packages/data_pkg/__init__.py"));

        assert!(repacked.record_paths.contains(&"bin/data_pkg-launcher".to_string()));
        assert!(
            repacked
                .record_paths
                .contains(&"share/data_pkg/config.json".to_string())
        );

        std::fs::remove_dir_all(&output_dir).ok();
    }

    #[test]
    fn console_scripts_are_extracted_raw() {
        let output_dir = scratch_dir("console-pkg");
        let repacked = block_on(repack_wheel(
            &fixture_wheel("console_pkg-1.0.0-py3-none-any.whl"),
            &output_dir,
        ))
        .expect("repack succeeds");

        let by_name: std::collections::HashMap<&str, &ConsoleScript> = repacked
            .entry_points
            .iter()
            .map(|script| (script.name.as_str(), script))
            .collect();
        assert_eq!(repacked.entry_points.len(), 3);

        let plain = by_name["console-pkg"];
        assert_eq!(plain.reference, "console_pkg:main");
        assert!(plain.extras.is_empty());

        let gated = by_name["blackd"];
        assert_eq!(gated.reference, "blackd:main");
        assert_eq!(gated.extras, vec!["d".to_string()]);

        let dotted = by_name["foo"];
        assert_eq!(dotted.reference, "console_pkg.mod:Class.method");
        assert!(dotted.extras.is_empty());

        std::fs::remove_dir_all(&output_dir).ok();
    }

    #[test]
    fn golden_digest_is_stable_across_runs() {
        const EXPECTED_DIGEST: &str = "sha256:330a642c4e7fcc3a565889e85091f8397780a78ad360601c81fbe9e371cd8ebe";

        let wheel = fixture_wheel("purelib_pkg-1.0.0-py3-none-any.whl");

        let first_output = scratch_dir("golden-1");
        let first = block_on(repack_wheel(&wheel, &first_output)).expect("first repack succeeds");
        assert_eq!(
            first.layer_digest, EXPECTED_DIGEST,
            "repack output drifted from the pinned golden digest — determinism regression"
        );

        let second_output = scratch_dir("golden-2");
        let second = block_on(repack_wheel(&wheel, &second_output)).expect("second repack succeeds");
        assert_eq!(
            second.layer_digest, first.layer_digest,
            "re-running repack must reproduce the same layer digest"
        );

        std::fs::remove_dir_all(&first_output).ok();
        std::fs::remove_dir_all(&second_output).ok();
    }

    #[test]
    fn zip_slip_entry_is_rejected() {
        use std::io::Write as _;

        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        writer
            .start_file("../evil.txt", options)
            .expect("start malicious entry");
        writer.write_all(b"pwned").expect("write malicious entry");
        let cursor = writer.finish().expect("finish malicious zip");

        let output_dir = scratch_dir("zip-slip");
        let malicious_wheel = output_dir.join("malicious-1.0.0-py3-none-any.whl");
        std::fs::write(&malicious_wheel, cursor.into_inner()).expect("write malicious wheel");

        let result = block_on(repack_wheel(&malicious_wheel, &output_dir.join("out")));
        match result {
            Err(RepackError::UnsafeEntryPath(_)) => {}
            other => panic!("expected UnsafeEntryPath, got {other:?}"),
        }
        // Nothing outside output_dir should exist as a result of the attempt.
        assert!(!Path::new("evil.txt").exists());

        std::fs::remove_dir_all(&output_dir).ok();
    }

    #[test]
    fn zip_bomb_decompressed_size_is_capped() {
        use std::io::Write as _;

        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        writer
            .start_file("pkg-1.0.0.dist-info/METADATA", options)
            .expect("start entry");
        // 64 decompressed bytes against a 16-byte test budget below.
        writer.write_all(&[b'a'; 64]).expect("write entry");
        let cursor = writer.finish().expect("finish zip");

        let output_dir = scratch_dir("zip-bomb");
        let wheel_path = output_dir.join("bomb-1.0.0-py3-none-any.whl");
        std::fs::write(&wheel_path, cursor.into_inner()).expect("write wheel");

        let result = block_on(repack_wheel_with_budget(&wheel_path, &output_dir.join("out"), 16));
        match result {
            Err(RepackError::WheelTooLarge { limit }) => assert_eq!(limit, 16),
            other => panic!("expected WheelTooLarge, got {other:?}"),
        }

        std::fs::remove_dir_all(&output_dir).ok();
    }

    /// Writes a minimal wheel zip carrying only a `*.dist-info/METADATA` entry
    /// with the given raw content, for exercising [`read_wheel_description`]
    /// without a full wheel fixture.
    fn write_metadata_only_wheel(output_dir: &Path, name: &str, metadata: &str) -> PathBuf {
        use std::io::Write as _;

        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        writer
            .start_file(format!("{name}-1.0.0.dist-info/METADATA"), options)
            .expect("start METADATA entry");
        writer.write_all(metadata.as_bytes()).expect("write METADATA entry");
        let cursor = writer.finish().expect("finish zip");

        let wheel_path = output_dir.join(format!("{name}-1.0.0-py3-none-any.whl"));
        std::fs::write(&wheel_path, cursor.into_inner()).expect("write wheel");
        wheel_path
    }

    #[test]
    fn read_wheel_description_extracts_summary_keywords_license() {
        let output_dir = scratch_dir("description-full");
        let wheel = write_metadata_only_wheel(
            &output_dir,
            "pkg",
            "Metadata-Version: 2.1\nName: pkg\nVersion: 1.0.0\nSummary: A tiny test package\nKeywords: test,fixture\nLicense: MIT\n\nLong-form description body, not parsed.\n",
        );

        let description = read_wheel_description(&wheel).expect("read description");
        assert_eq!(description.summary.as_deref(), Some("A tiny test package"));
        assert_eq!(description.keywords.as_deref(), Some("test,fixture"));
        assert_eq!(description.license.as_deref(), Some("MIT"));

        std::fs::remove_dir_all(&output_dir).ok();
    }

    #[test]
    fn read_wheel_description_treats_unknown_placeholder_as_absent() {
        let output_dir = scratch_dir("description-unknown");
        let wheel = write_metadata_only_wheel(
            &output_dir,
            "pkg",
            "Metadata-Version: 2.1\nName: pkg\nVersion: 1.0.0\nSummary: UNKNOWN\nLicense: UNKNOWN\n",
        );

        let description = read_wheel_description(&wheel).expect("read description");
        assert_eq!(description, WheelDescription::default());

        std::fs::remove_dir_all(&output_dir).ok();
    }

    #[test]
    fn read_wheel_description_defaults_when_no_optional_fields_present() {
        // Reuses the shared purelib fixture — its METADATA has no
        // Summary/Keywords/License, only the always-present core fields.
        let description =
            read_wheel_description(&fixture_wheel("purelib_pkg-1.0.0-py3-none-any.whl")).expect("read description");
        assert_eq!(description, WheelDescription::default());
    }
}
