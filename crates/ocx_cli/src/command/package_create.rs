// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{archive, compression, log, oci, package, package::metadata::authoring::AuthoringMetadata, prelude::*};

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
    ///
    /// Required whenever `--metadata` is given: it declares the platform the
    /// packaged content runs on, which cannot be read off the machine doing
    /// the build. Dependencies carrying no digest are resolved against the
    /// selected index to a platform manifest digest for this platform, and
    /// the content tree is scanned under this platform's executable
    /// convention. Resolution honors `--remote`, `--offline`, and `--frozen`.
    ///
    /// The value is written to a build receipt beside the bundle, which `ocx
    /// package push` and `ocx package test` fall back to when they are given
    /// no `--platform` of their own. Passing `--platform` to either of those
    /// simply wins; the receipt is not consulted for it.
    ///
    /// Also used to infer the output filename.
    ///
    /// With `--metadata`, a Linux target or `any` also has its declared
    /// `os.features` checked against what the packaged binaries actually
    /// need: a binary linked against a libc this value does not require is
    /// refused (exit 65), because an undeclared libc claims the package runs
    /// on hosts that cannot execute it. `any` requires no libc at all, so
    /// under it every dynamically linked binary is refused. Static binaries
    /// need no declaration.
    #[clap(short, long)]
    platform: Option<oci::Platform>,
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
    ///
    /// An escape hatch, not a convenience: a false refusal from the check
    /// would otherwise block every `ocx package create` for a Linux target
    /// with no way through. Skipping it leaves the declared `os.features`
    /// unverified, so a binary needing a libc the platform does not require
    /// can be published and will then resolve on hosts that cannot execute
    /// it; a warning naming the platform is printed wherever the check would
    /// have run, which is `--metadata` with a Linux target or `--platform
    /// any`. Anywhere else the check inspects nothing, so the flag
    /// suppresses nothing and says nothing. Nothing else changes - the same
    /// metadata and the same layers are written either way. See
    /// https://ocx.sh/docs/reference/command-line#package-create-libc-check
    /// for what the check does.
    #[clap(long)]
    no_libc_lint: bool,
    /// Treat PATH as an archive and bundle what it extracts to
    ///
    /// The format is taken from the file name - tar, tar with gzip, xz, zstd
    /// or bzip2 compression, or zip. The archive is unpacked into a temporary
    /// directory that is removed when the command exits, and that tree, not
    /// the archive file, is what everything else sees: the metadata sidecar,
    /// the binaries scan, the libc check and the bundle all behave exactly as
    /// they do for a directory input.
    ///
    /// Without this flag an archive is bundled as the single file it is.
    /// Implied by `--strip-components`. An archive that extracts to nothing
    /// is refused (exit 65).
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

        // Typed like every other `--output` touch in this command. The bare
        // `?` here was the one the class sweep missed: an `--output` whose
        // parent is a regular file answers ENOTDIR, and an untyped `io::Error`
        // has no rung in the downcast ladder, so an operator's bad path exited
        // 1 `internal` before `create_dir_all` below ever typed anything.
        let exists = tokio::fs::try_exists(&output)
            .await
            .map_err(|error| ocx_lib::error::file_error(&output, error))?;
        if exists && !self.force {
            anyhow::bail!(
                "output file {} already exists; use --force to overwrite",
                output.display()
            );
        }

        // Under `--extract`/`--strip-components` the content tree is what the
        // archive unpacks to, not the argument itself. The `TempDir` is bound
        // for the rest of `execute` — dropping it deletes the tree, and every
        // consumer below reads `content_root`, so there is one place where the
        // archive and directory forms diverge. D9: the scratch tree lives under
        // `$OCX_HOME/temp/create/`, beside the digest-keyed `TempStore`, not in
        // the system temp dir — one owned, GC-visible root for OCX scratch.
        let create_root = context.file_structure().temp.root().join("create");
        // Hold the scratch lock (`temp/create.lock`) for the rest of `execute` so
        // a concurrent `ocx clean` treats `temp/create` as busy — its stale scan
        // acquires this sibling lock, which fails while it is held — rather than
        // sweeping it as an orphan while the extracted tree beneath it is still
        // being read. `_create_lock` is held for its `Drop`; only the extract
        // path writes scratch, so the directory-input path takes no lock.
        let (extracted, _create_lock) = if self.extract || self.strip_components.is_some() {
            let lock = ocx_lib::utility::fs::LockedFile::open_exclusive(
                ocx_lib::file_structure::TempStore::lock_path_for(&create_root),
            )
            .await?;
            (Some(self.extract_archive(&create_root).await?), Some(lock))
        } else {
            (None, None)
        };
        let content_root = extracted.as_ref().map_or(self.path.as_path(), tempfile::TempDir::path);

        // Resolve + validate the sidecar BEFORE writing the output bundle:
        // dependency resolution can fail (network / policy / missing tag /
        // empty platform intersection), and a failure must leave no orphan
        // bundle on disk (Codex #3). Only after the metadata is fully validated
        // do we build the archive and, last, write the resolved sidecar.
        let resolved_metadata = match self.metadata.as_deref().zip(declared_platform) {
            Some((metadata_source, platform)) => {
                let metadata = AuthoringMetadata::read_json(metadata_source).await?;
                let metadata = self.resolve_dependency_pins(metadata, &context, &platform).await?;
                let metadata = self.resolve_binaries(content_root, metadata, &platform).await?;
                // Project to the published form and run the publish-time
                // env/entrypoint checks over it. This projection is what gets
                // written beside the bundle: push and test read the compiled
                // wire shape, never the authoring input.
                //
                // `validate_for_publish`, not `ValidMetadata::try_from`: the
                // token checks live only in the publish gate now (D14), and a
                // publisher is present here to be told about a typo. Downgrading
                // this line to the structural check would let an unrecognised
                // token reach a registry with no error anywhere.
                //
                // Ahead of the libc lint, because the lint resolves its scan
                // scope out of the same `PATH` value: an unrecognised token
                // there is not a directory it can name, so it lands on
                // `unresolvable` and the publisher is told the scope could not
                // be resolved rather than which token was misspelled. Both
                // refuse the same publish; only one of them says what is wrong.
                let valid = package::metadata::validate_for_publish(metadata.to_published()?)?;
                // Check what the packaged binaries actually demand of a host
                // against what `--platform` claims they demand. Runs after
                // the binaries scan (both read the same content tree) and,
                // like every other step in this arm, before the archive is
                // written — a refused artifact leaves no bundle on disk.
                //
                // Not gated on `--bin-scan`: that flag governs the `binaries`
                // metadata claim, while this governs the `os.features` claim.
                // A publisher passing `--no-bin-scan` is declining to have
                // their binary list filled in, not declining to have a false
                // libc claim caught.
                //
                // `--no-libc-lint` skips the whole call, refusals and
                // scan-scope failures alike. A partial bypass would leave a
                // bug in the un-bypassed half still able to block publishing,
                // which is the availability failure the flag exists to
                // prevent. Everything below still runs, so the flag suppresses
                // one check and nothing else.
                if self.no_libc_lint {
                    // Gated on the lint's own scope predicate, not on the flag
                    // alone: `check_declared_libc` returns `Ok(())` for a
                    // target whose libc OCX does not model, so on `darwin/*`
                    // or `windows/*` the two runs are behaviourally identical
                    // and a warning would name a verification that was never
                    // going to happen. In the per-platform matrix a shared
                    // create step carries this flag on every leg, and a
                    // warning that fires where nothing was suppressed dilutes
                    // exactly the loudness the escape hatch depends on.
                    if package::libc_lint::checks_declared_libc(&platform) {
                        context.ui().warn(format!(
                            "--no-libc-lint: skipped the libc check, so the os.features declared by \
                             {platform} are unverified against the packaged binaries"
                        ));
                    }
                } else {
                    package::libc_lint::check_declared_libc(content_root, &metadata, &platform).await?;
                }
                Some(valid)
            }
            None => None,
        };

        if let Some(parent) = output.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| ocx_lib::error::file_error(parent, error))?;
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
            package::bundle::BundleBuilder::from_path(content_root)
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
            // Always rewrite the sidecar canonically (never a byte copy): the
            // file next to the bundle is the compiled, pin-resolved published
            // form.
            let metadata_target = crate::conventions::infer_metadata_file(&output)?;
            package::metadata::Metadata::from(metadata)
                .write_json(&metadata_target)
                .await?;
        }

        // The receipt records what this invocation was told, so push and test
        // do not have to be told it again — with or without `--metadata`.
        // Written last: the bundle and its sidecar are the artifacts, the
        // receipt only describes how they were asked for. Nothing declared
        // means nothing to record, so no file — and any receipt an earlier
        // build left at this path is removed rather than kept, because it
        // describes a build that no longer exists here and would silently
        // supply push with an identifier and platform this invocation never
        // named. A removal that fails is fatal for the same reason.
        let receipt_target = crate::conventions::infer_receipt_file(&output)?;
        match crate::build_receipt::BuildReceipt::new(self.platform.clone(), identifier) {
            Some(receipt) => receipt.write_json(&receipt_target).await?,
            None => match tokio::fs::remove_file(&receipt_target).await {
                Ok(()) => log::info!("Removed the stale build receipt at {}", receipt_target.display()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(ocx_lib::error::file_error(&receipt_target, error).into()),
            },
        }

        Ok(ExitCode::SUCCESS)
    }

    /// Unpacks `--extract`'s archive into a temporary directory and hands the
    /// directory back, so the caller decides how long the tree lives.
    ///
    /// An extraction that leaves the directory empty is refused rather than
    /// bundled: the usual cause is a `--strip-components` deeper than the
    /// archive, and the alternative is publishing a package that installs no
    /// files. The directory answers that question directly - no entry count is
    /// threaded out of the extractor for it.
    async fn extract_archive(&self, create_root: &std::path::Path) -> anyhow::Result<tempfile::TempDir> {
        // H8: --extract/--strip-components treat PATH as an archive. A directory
        // has no archive format to unpack; refuse it by name (exit 64) rather
        // than letting the extractor fail deep with "Is a directory".
        let is_dir = tokio::fs::metadata(&self.path)
            .await
            .map(|metadata| metadata.is_dir())
            .unwrap_or(false);
        if is_dir {
            return Err(ocx_lib::cli::UsageError::new(format!(
                "--extract needs an archive file, but {} is a directory; drop --extract to bundle a directory",
                self.path.display()
            ))
            .into());
        }
        // Map an unrecognised suffix to UnsupportedFormat (exit 65) up front,
        // rather than attempting a plain-tar read that fails deep in the extractor.
        if !archive_format_recognized(&self.path) {
            return Err(archive::Error::UnsupportedFormat(self.path.display().to_string()).into());
        }

        let strip_components = self.strip_components.unwrap_or(0);
        tokio::fs::create_dir_all(create_root)
            .await
            .map_err(|error| ocx_lib::error::file_error(create_root, error))?;
        let target = tempfile::Builder::new()
            .prefix("create-")
            .tempdir_in(create_root)
            .map_err(|error| ocx_lib::error::file_error(create_root, error))?;
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
            .map_err(|error| ocx_lib::error::file_error(target.path(), error))?
            .next_entry()
            .await
            .map_err(|error| ocx_lib::error::file_error(target.path(), error))?;
        if first_entry.is_none() {
            return Err(self.empty_extraction_error(strip_components));
        }
        Ok(target)
    }

    /// The empty-extraction refusal (exit 65).
    ///
    /// W2: this is a CLI policy — an archive that unpacks to nothing must not
    /// become an empty package — so it lives here rather than in `archive::Error`,
    /// whose other extract caller (local blob materialization) extracts with
    /// strip 0, where an empty layer is legitimate. W10: the `--strip-components`
    /// clause is emitted only when the operator passed a non-zero strip, so the
    /// message never invents a `--strip-components 0` the operator never typed.
    fn empty_extraction_error(&self, strip_components: usize) -> anyhow::Error {
        let message = if strip_components == 0 {
            format!("archive '{}' extracted no entries", self.path.display())
        } else {
            format!(
                "archive '{}' extracted no entries with --strip-components {strip_components}",
                self.path.display()
            )
        };
        crate::app::CommandError::new(message, ocx_lib::cli::ExitCode::DataError).into()
    }

    /// Rejects an explicit `--bin-scan` given without `--metadata` (`-m`):
    /// the flag has nothing to verify, and silently no-op'ing would defeat
    /// its purpose as an explicit verification switch. `--no-bin-scan`
    /// without `--metadata` stays a harmless no-op — there is nothing to
    /// disable.
    fn validate_bin_scan(&self) -> anyhow::Result<()> {
        if self.bin_scan.mode() == options::BinScanMode::Verify && self.metadata.is_none() {
            return Err(ocx_lib::cli::UsageError::new(
                "--bin-scan requires --metadata (-m); nothing to verify without a metadata sidecar",
            )
            .into());
        }
        Ok(())
    }

    /// The platform `--metadata` is compiled for: dependency pins resolve
    /// against it and the binaries scan applies its executable convention.
    /// `None` when no sidecar was supplied — `--platform` then only shapes the
    /// inferred output filename and the build receipt (which records the flag
    /// itself, sidecar or not).
    ///
    /// There is no default. The host platform describes what the build
    /// machine *supplies*; the recorded platform describes what the packaged
    /// artifact *demands* — a static musl binary cross-built on a glibc host
    /// demands neither the host's libc nor its architecture. Guessing one
    /// from the other corrupts every downstream consumer of the recorded
    /// value, so an absent `--platform` is a usage error rather than a
    /// silent host default.
    fn declared_platform(&self) -> anyhow::Result<Option<oci::Platform>> {
        match (&self.metadata, &self.platform) {
            (None, _) => Ok(None),
            (Some(_), Some(platform)) => Ok(Some(platform.clone())),
            (Some(_), None) => Err(ocx_lib::cli::UsageError::new(
                "--platform (-p) is required with --metadata (-m); the sidecar records the platform \
                 the packaged content runs on, which cannot be inferred from the build host",
            )
            .into()),
        }
    }

    /// Resolve unpinned dependencies against the selected index for
    /// `platform`. Already-pinned dependencies pass through untouched (no
    /// network).
    async fn resolve_dependency_pins(
        &self,
        metadata: AuthoringMetadata,
        context: &crate::app::Context,
        platform: &oci::Platform,
    ) -> anyhow::Result<AuthoringMetadata> {
        let _spin = context.progress().spinner("Resolving dependency pins");
        Ok(package::dependency_pinning::pin_dependencies(metadata, context.default_index(), platform).await?)
    }

    /// Runs the create-time interface-binaries scan/fill/verify step
    /// against `content_root`'s content tree, per `self.bin_scan`'s resolved
    /// mode (`adr_declared_binaries_metadata.md` §2 / §2.1 ordering block).
    async fn resolve_binaries(
        &self,
        content_root: &std::path::Path,
        metadata: AuthoringMetadata,
        platform: &oci::Platform,
    ) -> anyhow::Result<AuthoringMetadata> {
        let mode = match self.bin_scan.mode() {
            options::BinScanMode::Auto => package::bin_scan::ScanMode::Auto,
            options::BinScanMode::Verify => package::bin_scan::ScanMode::Verify,
            options::BinScanMode::Off => package::bin_scan::ScanMode::Off,
        };
        Ok(package::bin_scan::resolve_binaries(content_root, metadata, platform, mode).await?)
    }

    /// Infers a filename for the package bundle based on the identifier and platform, or the input path if no identifier is provided.
    fn infer_filename(&self, identifier: Option<&oci::Identifier>) -> String {
        let mut name = match identifier {
            Some(identifier) => format!("{}-{}", identifier.name(), identifier.tag_or_latest()),
            None => self.inferred_stem(),
        };
        if let Some(platform) = &self.platform {
            name.push_str(&format!("-{}", platform.ascii_segments().join("-")));
        }
        format!("{}.tar.xz", name)
    }

    /// The bundle-name stem derived from the input path when no identifier is
    /// given.
    ///
    /// W11: under `--extract` the input is an archive, so strip the whole
    /// recognized suffix as a unit (`hello-1.2.3.tar.gz` → `hello-1.2.3`).
    /// `file_prefix` stops at the first dot (`hello-1`), which is wrong for a
    /// versioned archive name. Without `--extract` the input is bundled as a
    /// single file, and the historical `file_prefix` stem is unchanged.
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

/// Whether `path`'s suffix names an archive format `--extract` can unpack: zip,
/// plain tar, or tar with a supported compression. `compression`'s `from_file`
/// is the single source of truth for the compressed suffixes.
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

/// Strips a recognized archive suffix as a unit, so `hello-1.2.3.tar.gz` yields
/// `hello-1.2.3`. Longest compound suffixes are tried first, case-insensitively.
fn strip_archive_suffix(name: &str) -> &str {
    const SUFFIXES: &[&str] = &[
        ".tar.gz", ".tar.xz", ".tar.zst", ".tar.bz2", ".tgz", ".tzst", ".tar", ".zip",
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
            crate::app::classify_error(error.as_ref()),
            ocx_lib::cli::ExitCode::UsageError
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
        let store = ocx_lib::file_structure::TempStore::new(temp.path());
        let create_root = store.root().join("create");
        let lock_path = ocx_lib::file_structure::TempStore::lock_path_for(&create_root);

        // With the lock released, `clean` would acquire it and sweep the dir.
        assert!(
            store.try_acquire(&create_root).unwrap().is_some(),
            "an unheld scratch dir must be acquirable by clean"
        );

        // Hold it exactly as `execute` does; `clean` must now be turned away.
        let held = ocx_lib::utility::fs::LockedFile::open_exclusive(&lock_path)
            .await
            .unwrap();
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
