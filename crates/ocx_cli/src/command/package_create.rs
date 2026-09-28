// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_package::metadata::authoring::AuthoringMetadata;
use ocx_util::prelude::*;
use ocx_util::{archive, compression};

use crate::options;

#[derive(Parser)]
pub struct PackageCreate {
    /// Path to the package to bundle
    path: std::path::PathBuf,
    /// Identifier the bundle will be published under (e.g. `repo:2.0.0`)
    ///
    /// Used to infer the output filename when `--output` names no file, and
    /// recorded in the build receipt beside the bundle, which `ocx package
    /// push` falls back to when it is given no `--identifier` of its own.
    #[clap(short, long)]
    identifier: Option<options::Identifier>,
    /// Platform of the package content (e.g. `linux/amd64`, or `any` for platform-agnostic content)
    #[arg(long_help = "\
        Platform of the package content (e.g. `linux/amd64`, or `any` for platform-agnostic content)\n\n\
        Required whenever `--metadata` is given: it declares the platform the packaged content runs \
        on, which cannot be read off the machine doing the build. Dependencies carrying no digest \
        are resolved against the selected index to a platform manifest digest for this platform, \
        and the content tree is scanned under this platform's executable convention. Resolution \
        honors `--remote`, `--offline`, and `--frozen`.\n\n\
        The value is written to a build receipt beside the bundle, which `ocx package push` and \
        `ocx package test` fall back to when they are given no `--platform` of their own. Passing \
        `--platform` to either of those simply wins; the receipt is not consulted for it.\n\n\
        Also used to infer the output filename.\n\n\
        With `--metadata`, a Linux target or `any` also has its declared `os.features` checked \
        against what the packaged binaries actually need: a binary linked against a libc this value \
        does not require is refused (exit 65), because an undeclared libc claims the package runs \
        on hosts that cannot execute it. `any` requires no libc at all, so under it every \
        dynamically linked binary is refused. Static binaries need no declaration.")]
    #[clap(short, long)]
    platform: Option<ocx_oci::Platform>,
    /// Output file or directory, if a directory is provided the filename will be inferred
    #[clap(short, long)]
    output: Option<std::path::PathBuf>,
    /// Force overwrite of output file if it already exists
    #[clap(short, long)]
    force: bool,
    /// Path to a `metadata.json` file to validate, resolve, and write alongside the output bundle
    ///
    /// Requires `--platform`. Dependencies without a digest are pinned to
    /// that platform's manifest digests, and the compiled result (the same
    /// form `ocx package push` publishes) is written next to the output
    /// bundle. The build receipt is written whether or not this flag is
    /// given - it records the invocation, not the sidecar.
    #[clap(short, long)]
    metadata: Option<std::path::PathBuf>,
    /// Compression level to use for the package bundle
    #[arg(short = 'l', long, value_enum, default_value_t = options::CompressionLevel::Default)]
    compression_level: options::CompressionLevel,
    /// Number of compression threads (0 = auto-detect, 1 = single-threaded)
    #[arg(short = 'j', long, default_value_t = 0)]
    threads: u32,
    /// Scan the content tree for executables the package puts on `PATH` to
    /// fill or verify the `binaries` metadata claim; see
    /// `--bin-scan`/`--no-bin-scan`.
    #[clap(flatten)]
    bin_scan: options::BinScan,
    /// Skip the libc check on the packaged binaries
    #[arg(long_help = "\
        Skip the libc check on the packaged binaries\n\n\
        An escape hatch, not a convenience: a false refusal from the check would otherwise block \
        every `ocx package create` for a Linux target with no way through. Skipping it leaves the \
        declared `os.features` unverified, so a binary needing a libc the platform does not require \
        can be published and will then resolve on hosts that cannot execute it; a warning naming \
        the platform is printed wherever the check would have run, which is `--metadata` with a \
        Linux target or `--platform any`. Anywhere else the check inspects nothing, so the flag \
        suppresses nothing and says nothing. Nothing else changes - the same metadata and the same \
        layers are written either way. See \
        https://ocx.sh/docs/reference/command-line#package-create-libc-check for what the check \
        does.")]
    #[clap(long)]
    no_libc_lint: bool,
    /// Treat PATH as an archive and bundle what it extracts to
    #[arg(long_help = "\
        Treat PATH as an archive and bundle what it extracts to\n\n\
        The format is taken from the file name - tar, tar with gzip, xz, zstd or bzip2 compression, \
        or zip. The archive is unpacked into a temporary directory that is removed when the command \
        exits, and that tree, not the archive file, is what everything else sees: the metadata \
        sidecar, the binaries scan, the libc check and the bundle all behave exactly as they do for \
        a directory input.\n\n\
        Without this flag an archive is bundled as the single file it is. Implied by \
        `--strip-components`. An archive that extracts to nothing is refused (exit 65).")]
    #[clap(long)]
    extract: bool,
    /// Drop the leading N path components of every extracted entry
    ///
    /// Implies `--extract`. `--strip-components 1` turns the usual
    /// `hello-1.2.3/bin/hello` release layout into `bin/hello`, so the bundle
    /// has no version-named directory at its root. An entry whose whole path
    /// the strip consumes is dropped, and a depth that drops every entry is
    /// refused (exit 65) rather than bundling an empty tree.
    #[clap(long, value_name = "N")]
    strip_components: Option<usize>,
}

impl PackageCreate {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        self.validate_bin_scan()?;
        let declared_platform = self.declared_platform()?;

        let identifier = options::Identifier::transform_optional(self.identifier.clone(), context.default_registry())?;
        let output = match &self.output {
            Some(output) => {
                let is_dir = tokio::fs::metadata(output).await.map(|m| m.is_dir()).unwrap_or(false);
                if is_dir {
                    output.join(self.infer_filename(identifier.as_ref()))
                } else {
                    output.clone()
                }
            }
            None => self.infer_filename(identifier.as_ref()).into(),
        };

        // Typed as `FileError`: an untyped `io::Error` (ENOTDIR on a bad `--output` parent) classifies as exit 1.
        let exists = tokio::fs::try_exists(&output)
            .await
            .map_err(|error| ocx_util::error::FileError::new(&output, error))?;
        if exists && !self.force {
            anyhow::bail!(
                "output file {} already exists; use --force to overwrite",
                output.display()
            );
        }

        // `content_root` points into the extracted `TempDir`; dropping it early deletes that tree.
        let create_root = context.file_structure().temp.root().join("create");
        // Held (named, not `_`) to the end of `execute`, or `clean` sweeps `temp/create` mid-read.
        let (extracted, _create_lock) = if self.extract || self.strip_components.is_some() {
            let lock = ocx_util::fs::LockedFile::open_exclusive(ocx_store::file_structure::TempStore::lock_path_for(
                &create_root,
            ))
            .await?;
            (Some(self.extract_archive(&create_root).await?), Some(lock))
        } else {
            (None, None)
        };
        let content_root = extracted.as_ref().map_or(self.path.as_path(), tempfile::TempDir::path);

        // Resolve and validate before writing anything, so a failure leaves no orphan bundle.
        let resolved_metadata = match self.metadata.as_deref().zip(declared_platform) {
            Some((metadata_source, platform)) => {
                let metadata = AuthoringMetadata::read_json(metadata_source).await?;
                let metadata = self.resolve_dependency_pins(metadata, &context, &platform).await?;
                let metadata = self.resolve_binaries(content_root, metadata, &platform).await?;
                // Not `ValidMetadata::try_from`: only the publish gate rejects unknown tokens.
                let valid = ocx_package::metadata::validate_for_publish(metadata.to_published()?)?;
                // Before the archive write, so a refusal leaves no bundle; `--no-bin-scan` does not skip it.
                if self.no_libc_lint {
                    // Warn only where the lint would have run; elsewhere it names a no-op.
                    if ocx_package::libc_lint::checks_declared_libc(&platform) {
                        context.ui().warn(format!(
                            "--no-libc-lint: skipped the libc check, so the os.features declared by \
                             {platform} are unverified against the packaged binaries"
                        ));
                    }
                } else {
                    ocx_package::libc_lint::check_declared_libc(content_root, &metadata, &platform).await?;
                }
                Some(valid)
            }
            None => None,
        };

        if let Some(parent) = output.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| ocx_util::error::FileError::new(parent, error))?;
        }
        let compression_options =
            compression::CompressionOptions::from_level(self.compression_level.into()).with_threads(self.threads);
        log::info!(
            "Creating package bundle from {} with compression level {:?}",
            self.path.display(),
            self.compression_level
        );
        {
            let _spin = context.progress().spinner(format!("Bundling {}", self.path.display()));
            ocx_package::bundle::BundleBuilder::from_path(content_root)
                .with_compression(compression_options)
                .create(&output)
                .await?;
        }
        log::info!(
            "Created package bundle from {} at {}",
            self.path.display(),
            output.display()
        );

        if let Some(metadata) = resolved_metadata {
            // Written from the resolved form (pins resolved, binaries filled), never a byte copy of `--metadata`.
            let metadata_target = crate::conventions::infer_metadata_file(&output)?;
            ocx_package::metadata::Metadata::from(metadata)
                .write_json(&metadata_target)
                .await?;
        }

        // With nothing to record, a stale receipt is removed (failure fatal), or push silently
        // inherits an identifier and platform this invocation never named.
        let receipt_target = crate::conventions::infer_receipt_file(&output)?;
        match crate::build_receipt::BuildReceipt::new(self.platform.clone(), identifier) {
            Some(receipt) => receipt.write_json(&receipt_target).await?,
            None => match tokio::fs::remove_file(&receipt_target).await {
                Ok(()) => log::info!("Removed the stale build receipt at {}", receipt_target.display()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(ocx_util::error::FileError::new(&receipt_target, error).into()),
            },
        }

        Ok(ExitCode::SUCCESS)
    }

    /// Unpacks the `--extract` archive into a `TempDir` the caller owns; an empty result is
    /// refused (exit 65) rather than published as a package that installs nothing.
    async fn extract_archive(&self, create_root: &std::path::Path) -> anyhow::Result<tempfile::TempDir> {
        // Refused up front (exit 64), or the extractor fails deep with "Is a directory".
        let is_dir = tokio::fs::metadata(&self.path)
            .await
            .map(|metadata| metadata.is_dir())
            .unwrap_or(false);
        if is_dir {
            return Err(crate::error::UsageError::new(format!(
                "--extract needs an archive file, but {} is a directory; drop --extract to bundle a directory",
                self.path.display()
            ))
            .into());
        }
        // Checked up front, or an unknown suffix falls through to a plain-tar read that fails deep.
        if !archive_format_recognized(&self.path) {
            return Err(archive::Error::UnsupportedFormat(self.path.display().to_string()).into());
        }

        let strip_components = self.strip_components.unwrap_or(0);
        tokio::fs::create_dir_all(create_root)
            .await
            .map_err(|error| ocx_util::error::FileError::new(create_root, error))?;
        let target = tempfile::Builder::new()
            .prefix("create-")
            .tempdir_in(create_root)
            .map_err(|error| ocx_util::error::FileError::new(create_root, error))?;
        log::info!(
            "Extracting {} with strip_components {strip_components}",
            self.path.display()
        );
        archive::Archive::extract_with_options(
            &self.path,
            target.path(),
            Some(archive::ExtractOptions {
                algorithm: None,
                strip_components,
            }),
        )
        .await?;
        let first_entry = tokio::fs::read_dir(target.path())
            .await
            .map_err(|error| ocx_util::error::FileError::new(target.path(), error))?
            .next_entry()
            .await
            .map_err(|error| ocx_util::error::FileError::new(target.path(), error))?;
        if first_entry.is_none() {
            return Err(self.empty_extraction_error(strip_components));
        }
        Ok(target)
    }

    /// The empty-extraction refusal (exit 65); kept out of `archive::Error`, since an empty
    /// layer is legitimate when blob materialization extracts.
    fn empty_extraction_error(&self, strip_components: usize) -> anyhow::Error {
        let message = if strip_components == 0 {
            format!("archive '{}' extracted no entries", self.path.display())
        } else {
            format!(
                "archive '{}' extracted no entries with --strip-components {strip_components}",
                self.path.display()
            )
        };
        crate::app::CommandError::new(message, ocx_exit::ExitCode::DataError).into()
    }

    /// Refuses `--bin-scan` without `--metadata` (exit 64): there is nothing to verify.
    fn validate_bin_scan(&self) -> anyhow::Result<()> {
        if self.bin_scan.mode() == options::BinScanMode::Verify && self.metadata.is_none() {
            return Err(crate::error::UsageError::new(
                "--bin-scan requires --metadata (-m); nothing to verify without a metadata sidecar",
            )
            .into());
        }
        Ok(())
    }

    /// The platform `--metadata` is compiled for, or `None` without a sidecar.
    ///
    /// Never defaults to the host: the build machine's platform is not what the artifact demands.
    fn declared_platform(&self) -> anyhow::Result<Option<ocx_oci::Platform>> {
        match (&self.metadata, &self.platform) {
            (None, _) => Ok(None),
            (Some(_), Some(platform)) => Ok(Some(platform.clone())),
            (Some(_), None) => Err(crate::error::UsageError::new(
                "--platform (-p) is required with --metadata (-m); the sidecar records the platform \
                 the packaged content runs on, which cannot be inferred from the build host",
            )
            .into()),
        }
    }

    async fn resolve_dependency_pins(
        &self,
        metadata: AuthoringMetadata,
        context: &crate::app::Context,
        platform: &ocx_oci::Platform,
    ) -> anyhow::Result<AuthoringMetadata> {
        let _spin = context.progress().spinner("Resolving dependency pins");
        Ok(ocx_package::dependency_pinning::pin_dependencies(metadata, context.default_index(), platform).await?)
    }

    /// Fills or verifies the `binaries` claim from `content_root` per `--bin-scan`
    /// (`adr_declared_binaries_metadata.md` §2 / §2.1 ordering block).
    async fn resolve_binaries(
        &self,
        content_root: &std::path::Path,
        metadata: AuthoringMetadata,
        platform: &ocx_oci::Platform,
    ) -> anyhow::Result<AuthoringMetadata> {
        let mode = match self.bin_scan.mode() {
            options::BinScanMode::Auto => ocx_package::bin_scan::ScanMode::Auto,
            options::BinScanMode::Verify => ocx_package::bin_scan::ScanMode::Verify,
            options::BinScanMode::Off => ocx_package::bin_scan::ScanMode::Off,
        };
        Ok(ocx_package::bin_scan::resolve_binaries(content_root, metadata, platform, mode).await?)
    }

    fn infer_filename(&self, identifier: Option<&ocx_oci::PackageRef>) -> String {
        let mut name = match identifier {
            Some(identifier) => format!("{}-{}", identifier.name(), identifier.tag_or_latest()),
            None => self.inferred_stem(),
        };
        if let Some(platform) = &self.platform {
            name.push_str(&format!("-{}", platform.ascii_segments().join("-")));
        }
        format!("{}.tar.xz", name)
    }

    /// Bundle-name stem from the input path; under `--extract` the whole archive suffix goes,
    /// since `file_prefix` would cut `hello-1.2.3.tar.gz` to `hello-1`.
    fn inferred_stem(&self) -> String {
        if (self.extract || self.strip_components.is_some())
            && let Some(name) = self.path.file_name().and_then(|name| name.to_str())
        {
            return strip_archive_suffix(name).to_string();
        }
        self.path
            .file_prefix()
            .and_then(|stem| stem.to_str())
            .unwrap_or("package")
            .to_string()
    }
}

/// Whether `--extract` can unpack `path`; compressed suffixes come from `CompressionAlgorithm::from_file` alone.
fn archive_format_recognized(path: &std::path::Path) -> bool {
    if compression::CompressionAlgorithm::from_file(path).is_some() {
        return true;
    }
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("zip" | "tar")
    )
}

/// Strips a recognized archive suffix as a unit, longest first, case-insensitively.
///
/// Must list every tar spelling [`archive_format_recognized`] accepts, or `hello-1.2.3.tbz2` bundles as
/// `hello-1.2.3.tbz2.tar.xz`; held by `tar_archive_suffixes_are_recognized_and_stripped`.
fn strip_archive_suffix(name: &str) -> &str {
    const SUFFIXES: &[&str] = &[
        ".tar.gz", ".tar.xz", ".tar.zst", ".tar.bz2", ".tgz", ".tzst", ".tbz2", ".tbz", ".tar", ".zip",
    ];
    let lower = name.to_ascii_lowercase();
    for suffix in SUFFIXES {
        if lower.ends_with(suffix) {
            return &name[..name.len() - suffix.len()];
        }
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two suffix lists in this file answer for the same inputs: a name
    /// `archive_format_recognized` accepts is a name `--extract` will unpack, and
    /// `inferred_stem` then has to strip that suffix to name the bundle. `.tgz`
    /// and `.tzst` were in both lists; `.tbz2`/`.tbz` reached only the first when
    /// `CompressionAlgorithm::from_file` learned bzip2.
    ///
    /// The input set is hand-written, so a brand-new alias still needs a row —
    /// what this catches is one list gaining a spelling the other already had a
    /// place for.
    #[test]
    fn tar_archive_suffixes_are_recognized_and_stripped() {
        for suffix in [
            ".tar", ".tar.gz", ".tgz", ".tar.xz", ".tar.zst", ".tzst", ".tar.bz2", ".tbz2", ".tbz", ".zip",
        ] {
            let name = format!("hello-1.2.3{suffix}");
            assert!(
                archive_format_recognized(std::path::Path::new(&name)),
                "{name} should be a recognized archive format"
            );
            assert_eq!(
                strip_archive_suffix(&name),
                "hello-1.2.3",
                "{name} should strip to its stem"
            );
        }
    }

    /// `--bin-scan` without `--metadata` has nothing to verify — Cluster 2
    /// (arch-Warn) flagged the prior behavior as a silent no-op that exits 0
    /// without scanning anything.
    #[test]
    fn bin_scan_without_metadata_is_rejected() {
        let create = PackageCreate::try_parse_from(["package-create", "--bin-scan", "."]).expect("parse");
        let err = create
            .validate_bin_scan()
            .expect_err("--bin-scan without --metadata must be a usage error");
        let message = err.to_string();
        assert!(
            message.contains("--bin-scan") && message.contains("--metadata"),
            "usage error must name both flags: {message}"
        );
    }

    /// `--bin-scan` with `--metadata` present has a declaration to verify.
    #[test]
    fn bin_scan_with_metadata_is_accepted() {
        let create =
            PackageCreate::try_parse_from(["package-create", "--bin-scan", "-m", "metadata.json", "."]).expect("parse");
        assert!(create.validate_bin_scan().is_ok());
    }

    /// `--no-bin-scan` without `--metadata` stays a harmless no-op: there is
    /// nothing to disable, so it is not an error.
    #[test]
    fn no_bin_scan_without_metadata_is_accepted() {
        let create = PackageCreate::try_parse_from(["package-create", "--no-bin-scan", "."]).expect("parse");
        assert!(create.validate_bin_scan().is_ok());
    }

    /// Neither flag (Auto mode) without `--metadata` is unaffected — Auto
    /// never verifies, only `--bin-scan` does.
    #[test]
    fn auto_mode_without_metadata_is_accepted() {
        let create = PackageCreate::try_parse_from(["package-create", "."]).expect("parse");
        assert!(create.validate_bin_scan().is_ok());
    }

    /// `--metadata` without `--platform` must not fall back to the host
    /// platform. The host describes what the build machine supplies; the
    /// sidecar field describes what the artifact demands. A musl bundle
    /// cross-built on a glibc host would otherwise be recorded as demanding
    /// `libc.glibc` in the build receipt, and `ocx package push` / `ocx
    /// package test` bind to that value — so the corruption is unrecoverable
    /// downstream.
    #[test]
    fn metadata_without_platform_is_rejected_instead_of_defaulting_to_the_host() {
        let create = PackageCreate::try_parse_from(["package-create", "-m", "metadata.json", "."]).expect("parse");
        let error = create
            .declared_platform()
            .expect_err("--metadata without --platform must not resolve to the host platform");
        let message = error.to_string();
        assert!(
            message.contains("--platform") && message.contains("--metadata"),
            "usage error must name both flags: {message}"
        );
    }

    /// The rejection above is a usage error (64), not a data error — the
    /// invocation is malformed, nothing was read or parsed yet. Classified
    /// through the same path `main` uses.
    #[test]
    fn metadata_without_platform_exits_with_usage_error() {
        let create = PackageCreate::try_parse_from(["package-create", "-m", "metadata.json", "."]).expect("parse");
        let error = create.declared_platform().expect_err("rejected above");
        assert_eq!(
            crate::exit::classify_error(error.as_ref()),
            ocx_exit::ExitCode::UsageError
        );
    }

    /// Control for the two tests above: with `--platform` given, the declared
    /// value is what gets recorded — verbatim, including a libc feature the
    /// build host does not have.
    #[test]
    fn declared_platform_is_the_flag_value_verbatim() {
        let create = PackageCreate::try_parse_from([
            "package-create",
            "-m",
            "metadata.json",
            "-p",
            "linux/amd64+libc.musl",
            ".",
        ])
        .expect("parse");
        let platform = create.declared_platform().expect("explicit platform is accepted");
        assert_eq!(
            platform.map(|platform| platform.to_string()),
            Some("linux/amd64+libc.musl".to_string())
        );
    }

    /// No sidecar, no recorded platform: `--platform` stays optional there,
    /// where it only shapes the inferred output filename.
    #[test]
    fn no_metadata_needs_no_platform() {
        let create = PackageCreate::try_parse_from(["package-create", "."]).expect("parse");
        assert!(
            create.declared_platform().expect("no sidecar is ok").is_none(),
            "without --metadata there is nothing to record a platform on"
        );
    }

    /// W5: the scratch lock `execute` holds must make a concurrent `ocx clean`
    /// skip `temp/create`. `clean`'s stale scan calls `TempStore::try_acquire`,
    /// which returns `None` while the lock is held (the `.lock` sibling is busy),
    /// so the directory is not swept out from under an in-flight build. Held ⇒
    /// `None`; released ⇒ `Some`, which is what distinguishes the guard from a
    /// no-op.
    #[tokio::test]
    async fn a_held_scratch_lock_makes_clean_skip_the_create_dir() {
        let temp = tempfile::tempdir().unwrap();
        let store = ocx_store::file_structure::TempStore::new(temp.path());
        let create_root = store.root().join("create");
        let lock_path = ocx_store::file_structure::TempStore::lock_path_for(&create_root);

        // With the lock released, `clean` would acquire it and sweep the dir.
        assert!(
            store.try_acquire(&create_root).unwrap().is_some(),
            "an unheld scratch dir must be acquirable by clean"
        );

        // Hold it exactly as `execute` does; `clean` must now be turned away.
        let held = ocx_util::fs::LockedFile::open_exclusive(&lock_path).await.unwrap();
        assert!(
            store.try_acquire(&create_root).unwrap().is_none(),
            "clean must skip temp/create while the scratch lock is held"
        );
        drop(held);

        // Once released, `clean` can acquire it again — proving the None above
        // was the held lock, not a permanently unacquirable path.
        assert!(
            store.try_acquire(&create_root).unwrap().is_some(),
            "clean must be able to acquire the scratch dir once the lock is released"
        );
    }
}
