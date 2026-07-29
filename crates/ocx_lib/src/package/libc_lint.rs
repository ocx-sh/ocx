// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Create-time libc lint — checks what the packaged binaries actually demand
//! of a host's C library against what the declared platform *claims* they
//! demand.
//!
//! Sibling of [`super::bin_scan`] and [`super::dependency_pinning`]: the third
//! compile step `ocx package create` runs against the content tree before
//! archiving. Where `bin_scan` checks the `binaries` claim, this checks the
//! `os.features` claim.
//!
//! ## The bug it closes
//!
//! `os.features` subset matching (`oci::is_compatible`) reads an **empty**
//! feature list as "this artifact demands nothing of the host", so it matches
//! every host. That makes an omitted `libc.glibc` a *positive claim of libc
//! universality*, not a missing annotation. A glibc-linked binary published
//! that way resolves happily on Alpine and then fails to execute with a bare
//! `No such file or directory` — the kernel reporting the absent ELF
//! interpreter, naming a file that is plainly there. Nothing in the publish
//! path noticed, because nothing read the artifact.
//!
//! ## What it reads
//!
//! The ELF `PT_INTERP` program header — the absolute path of the dynamic
//! loader the kernel must run to start the binary. `/lib/ld-musl-*` is musl,
//! `/lib64/ld-linux-*` (and the other-arch `ld.so` / `ld64.so` spellings) is
//! glibc, and no `PT_INTERP` at all means statically linked and therefore no
//! libc requirement. Parsing is delegated to the `elf` crate, already a
//! workspace dependency and already the ELF reader behind
//! [`crate::oci::host_capabilities`]'s host-side loader discovery — the same
//! header, read from the other end of the contract.
//!
//! ## Scope
//!
//! **Linux targets only.** macOS ships exactly one C library (`libSystem`),
//! and while Windows genuinely does have several CRT flavours (MSVCRT, UCRT,
//! statically linked), OCX's `os.features` vocabulary has no `libc.*` tag for
//! any of them and [`crate::oci::HostCapabilities::detect`] returns an empty
//! set on both platforms — a feature declared there could never be satisfied
//! by any host, so checking for one would only manufacture false failures.
//!
//! **Interface binaries only.** The subjects are the files the package puts
//! on a consumer's `PATH` ([`super::bin_scan::scan_interface_files`]), not the
//! whole content tree. What this does *not* catch is listed on
//! [`check_declared_libc`].

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::oci::host_capabilities::LibcFlavor;
use crate::oci::{OperatingSystem, Platform};
use crate::package::metadata::authoring::AuthoringMetadata;

/// Checks every file the package puts on an interface `PATH` directory
/// against `platform`'s declared `os.features`, refusing a binary that needs
/// a libc family the declaration does not require.
///
/// A no-op for non-Linux targets (see the module "Scope" note). For
/// [`Platform::Any`] every dynamically linked native binary is a failure by
/// construction: `any` satisfies *every* host requirement, so it is a
/// strictly broader false claim than an empty feature list on a concrete
/// platform.
///
/// # What this does not catch
///
/// - **Non-libc shared-object dependencies.** A binary needing `libicu`,
///   `libatomic`, `libstdc++` or any other `DT_NEEDED` library passes this
///   check; only the dynamic loader named by `PT_INTERP` is read. Missing
///   those fails at runtime with a *different* message and is out of scope.
/// - **glibc symbol versions.** A binary built against glibc 2.38 and run on
///   glibc 2.28 satisfies `libc.glibc` and still fails. `os.features` has no
///   version vocabulary ([`LibcFlavor`] is deliberately unit-variant).
/// - **Files not on an interface `PATH` directory.** A private helper binary,
///   or one reached through a wrapper script, is never scanned.
/// - **`ocx package push` without a metadata sidecar.** The claim exists at
///   authoring time, where the content tree does; push sees only a built
///   archive. `--platform` on a bare push is unchecked.
///
/// # Errors
///
/// [`LibcLintError::UndeclaredLibc`] for the mismatch above;
/// [`LibcLintError::AgnosticPlatformClaim`] for the `any` case;
/// [`LibcLintError::UnrecognizedInterpreter`] /
/// [`LibcLintError::UnparseableElf`] / [`LibcLintError::Read`] for a file
/// whose requirement could not be determined (fail-closed — see
/// [`read_elf_libc`]); [`LibcLintError::Scan`] for a directory-walk failure.
pub async fn check_declared_libc(
    content_root: &Path,
    metadata: &AuthoringMetadata,
    platform: &Platform,
) -> Result<(), LibcLintError> {
    let declared = match platform {
        Platform::Specific {
            os: OperatingSystem::Linux,
            os_features,
            ..
        } => declared_libcs(os_features),
        // `Any` declares compatibility with every host, so it satisfies any
        // libc requirement a host could state — an empty declared set makes
        // every dynamic binary below a violation, which is the intent.
        Platform::Any => BTreeSet::new(),
        // Single-libc or no-vocabulary targets: nothing to check.
        Platform::Specific { .. } => return Ok(()),
    };

    for path in crate::package::bin_scan::scan_interface_files(content_root, metadata, platform).await? {
        // Blocking file I/O (`ElfStream` needs `Read + Seek`), so it goes to
        // the blocking pool. Sequential rather than fanned out: the subjects
        // are one `PATH` directory's entries, and a `JoinSet` here would buy
        // nothing but a sort to restore the deterministic first-offender
        // order the candidate map already provides.
        let probe = path.clone();
        let requirement = tokio::task::spawn_blocking(move || read_elf_libc(&probe))
            .await
            .expect("libc lint ELF read task panicked")?;

        let ElfLibc::Dynamic { interpreter, flavor } = requirement else {
            continue;
        };
        if declared.contains(&flavor) {
            continue;
        }
        let required = flavor.os_feature_tag();
        return Err(match platform {
            Platform::Any => LibcLintError::AgnosticPlatformClaim {
                path,
                interpreter,
                required,
            },
            platform => LibcLintError::UndeclaredLibc {
                path,
                interpreter,
                suggestion: with_os_feature(platform, &required).to_string(),
                platform: platform.to_string(),
                required,
            },
        });
    }
    Ok(())
}

/// Decodes the `libc.*` subset of a declared `os.features` list. Non-libc
/// features (`win32k`, and any future namespace) carry no libc meaning and
/// are dropped, exactly as [`crate::oci::host_capabilities::Feature`] treats
/// them on the resolution side.
fn declared_libcs(os_features: &[String]) -> BTreeSet<LibcFlavor> {
    os_features
        .iter()
        .filter_map(|tag| LibcFlavor::from_os_feature_tag(tag))
        .collect()
}

/// `platform` with `feature` added to its `os.features` — the paste-ready
/// `--platform` value the error message suggests. [`Platform`]'s `Display`
/// sorts and dedupes features, so the rendered result is canonical whatever
/// order they land in here.
fn with_os_feature(platform: &Platform, feature: &str) -> Platform {
    match platform {
        Platform::Specific {
            os,
            arch,
            variant,
            os_features,
        } => {
            let mut os_features = os_features.clone();
            os_features.push(feature.to_string());
            Platform::Specific {
                os: *os,
                arch: *arch,
                variant: variant.clone(),
                os_features,
            }
        }
        // `any` carries no fields, so there is nothing to add. Unreachable
        // from the caller (the agnostic case has its own error variant) but
        // kept total rather than panicking.
        Platform::Any => Platform::Any,
    }
}

/// What a file on the interface `PATH` demands of the host's C library.
#[derive(Debug, PartialEq, Eq)]
enum ElfLibc {
    /// Not an ELF object at all — a script, a data file, a README. Not a
    /// subject of this lint.
    NotElf,
    /// A parsed ELF carrying no `PT_INTERP`: statically linked, so it demands
    /// no libc of the host.
    Static,
    /// A parsed ELF naming a dynamic loader, attributed to a libc family.
    Dynamic { interpreter: String, flavor: LibcFlavor },
}

/// Reads `path`'s libc requirement out of its ELF program headers.
///
/// Fail-closed, with one deliberate distinction: **absence of the ELF magic
/// is a positive identification of "not in scope", not an unknown.** A file
/// that never claimed to be an ELF is [`ElfLibc::NotElf`] and is skipped. A
/// file that *does* claim to be one and cannot then be read, parsed, or
/// attributed to a libc family is an error — treating it as "no requirement"
/// would reintroduce the exact silent-pass this lint exists to close. A
/// successfully parsed ELF with no `PT_INTERP` is [`ElfLibc::Static`], which
/// is a fact the parse established, not information that went missing.
///
/// Only the ELF headers and the `PT_INTERP` segment are read
/// ([`elf::ElfStream`] seeks lazily), so a 400 MB binary costs a header read
/// and one seek rather than 400 MB of resident memory.
fn read_elf_libc(path: &Path) -> Result<ElfLibc, LibcLintError> {
    let read_error = |source| LibcLintError::Read {
        path: path.to_path_buf(),
        source,
    };
    let mut file = std::fs::File::open(path).map_err(read_error)?;

    let mut magic = [0u8; 4];
    match file.read_exact(&mut magic) {
        Ok(()) => {}
        // Shorter than the magic itself: definitively not an ELF.
        Err(source) if source.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(ElfLibc::NotElf),
        Err(source) => return Err(read_error(source)),
    }
    if magic != elf::abi::ELFMAGIC {
        return Ok(ElfLibc::NotElf);
    }

    let unparseable = |source| LibcLintError::UnparseableElf {
        path: path.to_path_buf(),
        source,
    };
    // `open_stream` seeks from the start itself, so the four bytes consumed
    // by the magic probe above do not need rewinding.
    let mut object = elf::ElfStream::<elf::endian::AnyEndian, _>::open_stream(file).map_err(unparseable)?;
    let Some(interp) = object
        .segments()
        .iter()
        .find(|header| header.p_type == elf::abi::PT_INTERP)
        .copied()
    else {
        return Ok(ElfLibc::Static);
    };

    let raw = object.segment_data(&interp).map_err(unparseable)?;
    // The interpreter is a NUL-terminated string filling the segment.
    let interpreter = raw.split(|&byte| byte == 0).next().unwrap_or_default();
    let interpreter = String::from_utf8_lossy(interpreter).into_owned();
    match classify_interpreter(&interpreter) {
        Some(flavor) => Ok(ElfLibc::Dynamic { interpreter, flavor }),
        None => Err(LibcLintError::UnrecognizedInterpreter {
            path: path.to_path_buf(),
            interpreter,
        }),
    }
}

/// Attributes a `PT_INTERP` loader path to a libc family by its filename.
///
/// The host-side counterpart in [`crate::oci::host_capabilities`] classifies
/// by running the loader and reading its `--version` banner, which is both
/// stronger and unavailable here: `ocx package create` routinely runs on a
/// glibc build host packaging a musl artifact for a foreign architecture, so
/// the artifact's loader can be neither present nor executable. The filename
/// is the only evidence a cross-build has, and it is the evidence every
/// toolchain writes deliberately.
///
/// Returns `None` for a loader OCX cannot attribute, which the caller turns
/// into a hard error rather than a silent pass.
fn classify_interpreter(interpreter: &str) -> Option<LibcFlavor> {
    let name = interpreter.rsplit('/').next()?;
    if name.contains("ld-musl") {
        return Some(LibcFlavor::Musl);
    }
    // `ld-linux-*` on x86_64/aarch64/arm, `ld.so.1` and `ld64.so.*` on the
    // architectures whose glibc loader is not spelled `ld-linux`.
    if name.contains("ld-linux") || name.starts_with("ld.so") || name.starts_with("ld64.so") {
        return Some(LibcFlavor::Glibc);
    }
    None
}

/// Failures of the create-time libc lint.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LibcLintError {
    /// A binary on the package's interface `PATH` needs a libc family the
    /// declared platform's `os.features` does not require. Under subset
    /// matching an undeclared family is a positive claim that no such family
    /// is needed, so this ships an artifact that resolves on hosts unable to
    /// execute it.
    #[error(
        "'{}' needs {required} (dynamic loader '{interpreter}'), but the declared platform \
         {platform} requires no such libc and so resolves on hosts that cannot execute it; \
         declare {suggestion}, or package a build that does not need {required}",
        path.display()
    )]
    UndeclaredLibc {
        /// The offending file in the content tree.
        path: PathBuf,
        /// The `PT_INTERP` value read out of it.
        interpreter: String,
        /// The `os.features` tag it needs, e.g. `libc.glibc`.
        required: String,
        /// The declared platform, rendered.
        ///
        /// Rendered rather than a [`Platform`]: both this and `suggestion`
        /// exist only to be interpolated into the message, and carrying two
        /// `Platform` values pushes this variant past clippy's
        /// `result_large_err` threshold, penalising every `Ok` path.
        platform: String,
        /// The same platform with `required` added — paste-ready for
        /// `--platform`.
        suggestion: String,
    },
    /// The package is declared platform-agnostic (`any`) but ships a
    /// dynamically linked native binary. `any` satisfies every host
    /// requirement, making it a broader false claim than an undeclared
    /// feature on a concrete platform.
    #[error(
        "'{}' is a dynamically linked ELF needing {required} (dynamic loader '{interpreter}'), \
         but the package is declared 'any'; 'any' claims every host can run it, including hosts \
         without {required} — declare the concrete os/arch this content targets, with {required} \
         among its os.features",
        path.display()
    )]
    AgnosticPlatformClaim {
        /// The offending file in the content tree.
        path: PathBuf,
        /// The `PT_INTERP` value read out of it.
        interpreter: String,
        /// The `os.features` tag it needs, e.g. `libc.glibc`.
        required: String,
    },
    /// A file carrying the ELF magic could not be parsed, so its libc
    /// requirement is unknown. Fail-closed: an unreadable claim is never
    /// treated as "claims nothing".
    #[error("'{}' carries an ELF header but could not be parsed, so its libc requirement is unknown", path.display())]
    UnparseableElf {
        /// The file that failed to parse.
        path: PathBuf,
        /// The underlying parse failure.
        #[source]
        source: elf::ParseError,
    },
    /// A parsed ELF names a dynamic loader OCX cannot attribute to a libc
    /// family, so its requirement cannot be checked against `os.features`.
    #[error(
        "'{}' names dynamic loader '{interpreter}', which OCX cannot attribute to a libc family, \
         so its requirement cannot be checked against os.features",
        path.display()
    )]
    UnrecognizedInterpreter {
        /// The file naming the unrecognised loader.
        path: PathBuf,
        /// The `PT_INTERP` value read out of it.
        interpreter: String,
    },
    /// A candidate file could not be read at all.
    #[error("failed to read '{}' while checking its libc requirement", path.display())]
    Read {
        /// The unreadable file.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// The content-tree scan that produces the candidate files failed.
    #[error("interface-binaries scan failed")]
    Scan(#[from] crate::Error),
}

impl crate::cli::ClassifyExitCode for LibcLintError {
    fn classify(&self) -> Option<crate::cli::ExitCode> {
        match self {
            // Input-data trouble, and deliberately the same code the
            // *resolution* side already returns for the mirror-image failure:
            // `SelectResult::FeatureMismatch` -> `PackageErrorKind::
            // FeatureMismatch` -> `DataError`. One number for both ends of
            // the os.features contract, whether the mismatch is caught at
            // publish time or at install time. Matches the sibling compile
            // step (`BinScanError`) too.
            Self::UndeclaredLibc { .. }
            | Self::AgnosticPlatformClaim { .. }
            | Self::UnparseableElf { .. }
            | Self::UnrecognizedInterpreter { .. } => Some(crate::cli::ExitCode::DataError),
            // A file we could not read is an I/O fault, not bad data.
            Self::Read { .. } => Some(crate::cli::ExitCode::IoError),
            // Delegate to the inner cause via the chain walker.
            Self::Scan(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Interpreter attribution ──────────────────────────────────────────

    #[test]
    fn classify_interpreter_attributes_glibc_loaders() {
        for interpreter in [
            "/lib64/ld-linux-x86-64.so.2",
            "/lib/ld-linux-aarch64.so.1",
            "/lib/ld-linux-armhf.so.3",
            "/lib64/ld64.so.2",
            "/lib/ld.so.1",
            // Non-FHS layouts (NixOS, Gentoo Prefix) keep the loader name.
            "/nix/store/abc123-glibc-2.39/lib/ld-linux-x86-64.so.2",
        ] {
            assert_eq!(
                classify_interpreter(interpreter),
                Some(LibcFlavor::Glibc),
                "{interpreter} must attribute to glibc"
            );
        }
    }

    #[test]
    fn classify_interpreter_attributes_musl_loaders() {
        for interpreter in ["/lib/ld-musl-x86_64.so.1", "/lib/ld-musl-aarch64.so.1"] {
            assert_eq!(
                classify_interpreter(interpreter),
                Some(LibcFlavor::Musl),
                "{interpreter} must attribute to musl"
            );
        }
    }

    #[test]
    fn classify_interpreter_refuses_to_guess() {
        // Fail-closed: an unattributable loader must not silently pass as
        // "no requirement". The empty string is the degenerate case of a
        // zero-length PT_INTERP segment.
        for interpreter in [
            "/lib/ld-uClibc.so.0",
            "/system/bin/linker64",
            "/usr/lib/exotic-loader",
            "",
        ] {
            assert_eq!(
                classify_interpreter(interpreter),
                None,
                "{interpreter:?} must not be attributed to a libc family"
            );
        }
    }

    // ── Declared-feature decoding ────────────────────────────────────────

    #[test]
    fn declared_libcs_decodes_libc_tags_and_drops_the_rest() {
        assert_eq!(declared_libcs(&[]), BTreeSet::new(), "empty declares no libc");
        assert_eq!(
            declared_libcs(&["libc.glibc".to_string()]),
            BTreeSet::from([LibcFlavor::Glibc])
        );
        assert_eq!(
            declared_libcs(&["libc.glibc".to_string(), "libc.musl".to_string()]),
            BTreeSet::from([LibcFlavor::Glibc, LibcFlavor::Musl])
        );
        assert_eq!(
            declared_libcs(&["win32k".to_string()]),
            BTreeSet::new(),
            "a non-libc os.feature carries no libc meaning"
        );
    }

    // ── Suggested platform ───────────────────────────────────────────────

    #[test]
    fn with_os_feature_renders_a_paste_ready_platform() {
        let declared: Platform = "linux/amd64".parse().expect("parses");
        assert_eq!(
            with_os_feature(&declared, "libc.glibc").to_string(),
            "linux/amd64+libc.glibc"
        );

        // Preserves a variant and merges into existing features canonically.
        let declared: Platform = "linux/arm64/v8+libc.musl".parse().expect("parses");
        assert_eq!(
            with_os_feature(&declared, "libc.glibc").to_string(),
            "linux/arm64/v8+libc.glibc,libc.musl"
        );
    }

    // ── Exit-code classification ─────────────────────────────────────────

    #[test]
    fn classify_maps_claim_failures_to_data_error_and_read_failures_to_io_error() {
        use crate::cli::{ClassifyExitCode, ExitCode};

        let undeclared = LibcLintError::UndeclaredLibc {
            path: PathBuf::from("bin/bazel"),
            interpreter: "/lib64/ld-linux-x86-64.so.2".to_string(),
            required: "libc.glibc".to_string(),
            platform: "linux/amd64".to_string(),
            suggestion: "linux/amd64+libc.glibc".to_string(),
        };
        assert_eq!(
            undeclared.classify(),
            Some(ExitCode::DataError),
            "a false os.features claim is the publish-time mirror of the resolve-time \
             FeatureMismatch, which is also DataError (65)"
        );

        let unrecognized = LibcLintError::UnrecognizedInterpreter {
            path: PathBuf::from("bin/tool"),
            interpreter: "/lib/ld-uClibc.so.0".to_string(),
        };
        assert_eq!(unrecognized.classify(), Some(ExitCode::DataError));

        let unreadable = LibcLintError::Read {
            path: PathBuf::from("bin/tool"),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        };
        assert_eq!(unreadable.classify(), Some(ExitCode::IoError));

        assert_eq!(
            LibcLintError::Scan(crate::Error::OfflineMode).classify(),
            None,
            "Scan must delegate classification to its inner crate::Error cause"
        );
    }

    // ── Real-ELF fixture cases ───────────────────────────────────────────
    //
    // The fixtures are compiled by the host C toolchain, never hand-written
    // byte arrays: this module's whole job is reading a binary format, and a
    // fixture the parser never had to face for real proves nothing.
    #[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]
    mod elf_fixtures {
        use super::*;

        #[cfg(target_arch = "x86_64")]
        const MUSL_INTERPRETER: &str = "/lib/ld-musl-x86_64.so.1";
        #[cfg(target_arch = "aarch64")]
        const MUSL_INTERPRETER: &str = "/lib/ld-musl-aarch64.so.1";

        /// Compiles one ELF fixture with `cc`.
        ///
        /// A missing `cc` is a hard failure, never a skip: linking this very
        /// test binary on a `*-linux-gnu` target already went through a C
        /// linker driver, so "cc absent" is unreachable wherever this test
        /// runs, and a skip there would be a green that never ran.
        fn compile(dir: &std::path::Path, name: &str, source: &str, args: &[&str]) -> PathBuf {
            let source_path = dir.join(format!("{name}.c"));
            std::fs::write(&source_path, source).expect("write fixture source");
            let output_path = dir.join(name);
            let result = std::process::Command::new("cc")
                .arg("-o")
                .arg(&output_path)
                .arg(&source_path)
                .args(args)
                .output()
                .expect("cc must be present: linking this test binary already required a C linker driver");
            assert!(
                result.status.success(),
                "cc failed to build fixture {name}: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            output_path
        }

        /// A dynamically linked glibc binary — the shape of the published
        /// `bazel` that started this.
        fn glibc_binary(dir: &std::path::Path, name: &str) -> PathBuf {
            compile(dir, name, "int main(void) { return 0; }\n", &[])
        }

        /// A dynamically linked binary naming the musl loader. Built by the
        /// glibc toolchain with an overridden `PT_INTERP`, so it is not
        /// runnable here — irrelevant, the lint reads the header, and this is
        /// exactly the cross-build case `classify_interpreter` exists for.
        fn musl_binary(dir: &std::path::Path, name: &str) -> PathBuf {
            compile(
                dir,
                name,
                "int main(void) { return 0; }\n",
                &[&format!("-Wl,--dynamic-linker={MUSL_INTERPRETER}")],
            )
        }

        /// A statically linked binary: no `PT_INTERP`, no libc requirement.
        /// `-nostdlib` avoids depending on static glibc being installed.
        fn static_binary(dir: &std::path::Path, name: &str) -> PathBuf {
            compile(
                dir,
                name,
                "void _start(void) { __builtin_trap(); }\n",
                &["-static", "-nostdlib"],
            )
        }

        // ── read_elf_libc ────────────────────────────────────────────────

        #[test]
        fn read_elf_libc_reads_a_real_glibc_binarys_requirement() {
            let dir = tempfile::tempdir().expect("tempdir");
            let binary = glibc_binary(dir.path(), "tool");
            match read_elf_libc(&binary).expect("glibc fixture reads") {
                ElfLibc::Dynamic { interpreter, flavor } => {
                    assert_eq!(flavor, LibcFlavor::Glibc);
                    assert!(
                        interpreter.contains("ld-linux"),
                        "expected a glibc loader path, got {interpreter:?}"
                    );
                }
                other => panic!("expected a dynamic glibc requirement, got {other:?}"),
            }
        }

        #[test]
        fn read_elf_libc_reads_a_real_musl_binarys_requirement() {
            let dir = tempfile::tempdir().expect("tempdir");
            let binary = musl_binary(dir.path(), "tool");
            assert_eq!(
                read_elf_libc(&binary).expect("musl fixture reads"),
                ElfLibc::Dynamic {
                    interpreter: MUSL_INTERPRETER.to_string(),
                    flavor: LibcFlavor::Musl,
                }
            );
        }

        #[test]
        fn read_elf_libc_reports_a_static_binary_as_demanding_nothing() {
            let dir = tempfile::tempdir().expect("tempdir");
            let binary = static_binary(dir.path(), "tool");
            assert_eq!(
                read_elf_libc(&binary).expect("static fixture reads"),
                ElfLibc::Static,
                "a successfully parsed ELF with no PT_INTERP demands no libc"
            );
        }

        #[test]
        fn read_elf_libc_skips_files_that_are_not_elf_objects() {
            let dir = tempfile::tempdir().expect("tempdir");
            for (name, bytes) in [
                ("README", &b"not a binary at all\n"[..]),
                ("wrapper.sh", &b"#!/bin/sh\nexec bazel-real \"$@\"\n"[..]),
                // Shorter than the ELF magic itself.
                ("stub", &b"#!"[..]),
                ("empty", &b""[..]),
            ] {
                let path = dir.path().join(name);
                std::fs::write(&path, bytes).expect("write fixture");
                assert_eq!(
                    read_elf_libc(&path).expect("non-ELF reads"),
                    ElfLibc::NotElf,
                    "{name} is not an ELF and must not be a subject of the lint"
                );
            }
        }

        #[test]
        fn read_elf_libc_fails_closed_on_a_file_claiming_to_be_an_elf() {
            // The fail-closed case that matters: the ELF magic is a positive
            // claim, so a file carrying it whose headers cannot be parsed
            // must error, never pass as "demands nothing".
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("truncated");
            let mut bytes = elf::abi::ELFMAGIC.to_vec();
            bytes.extend_from_slice(&[0xff; 40]);
            std::fs::write(&path, &bytes).expect("write fixture");
            assert!(
                matches!(read_elf_libc(&path), Err(LibcLintError::UnparseableElf { .. })),
                "a file carrying the ELF magic that will not parse must fail closed"
            );
        }

        #[test]
        fn read_elf_libc_fails_closed_on_an_unattributable_loader() {
            let dir = tempfile::tempdir().expect("tempdir");
            let binary = compile(
                dir.path(),
                "tool",
                "int main(void) { return 0; }\n",
                &["-Wl,--dynamic-linker=/lib/ld-uClibc.so.0"],
            );
            assert!(
                matches!(
                    read_elf_libc(&binary),
                    Err(LibcLintError::UnrecognizedInterpreter { .. })
                ),
                "a loader OCX cannot attribute must fail closed, not pass as static"
            );
        }

        // ── check_declared_libc — end to end ─────────────────────────────

        /// Builds a content tree with `bin/` on the interface `PATH`, the
        /// exact scan scope `ocx package create` uses, and returns the tree
        /// root plus its metadata. The `TempDir` is returned so the caller
        /// keeps it alive.
        fn content_tree() -> (tempfile::TempDir, PathBuf, AuthoringMetadata) {
            let dir = tempfile::tempdir().expect("tempdir");
            let bin = dir.path().join("bin");
            std::fs::create_dir_all(&bin).expect("create bin/");
            let metadata: AuthoringMetadata = serde_json::from_str(
                r#"{"type":"bundle","version":1,
                    "env":[{"key":"PATH","type":"path","value":"${installPath}/bin",
                            "required":false,"visibility":"interface"}]}"#,
            )
            .expect("fixture metadata parses");
            (dir, bin, metadata)
        }

        fn platform(spec: &str) -> Platform {
            spec.parse().expect("platform spec parses")
        }

        #[tokio::test]
        async fn refuses_a_glibc_binary_published_with_no_declared_libc() {
            // The reported bug, verbatim: a glibc-linked tool on the
            // interface PATH under a platform whose empty `os.features` is a
            // positive claim of libc universality.
            let (_dir, bin, metadata) = content_tree();
            glibc_binary(&bin, "bazel");

            let error = check_declared_libc(bin.parent().expect("tree root"), &metadata, &platform("linux/amd64"))
                .await
                .expect_err("a glibc binary under empty os.features must be refused");

            let LibcLintError::UndeclaredLibc {
                required, suggestion, ..
            } = &error
            else {
                panic!("expected UndeclaredLibc, got {error:?}");
            };
            assert_eq!(required, "libc.glibc");
            assert_eq!(
                suggestion.to_string(),
                "linux/amd64+libc.glibc",
                "the message must hand back a paste-ready --platform value"
            );
        }

        #[tokio::test]
        async fn admits_a_glibc_binary_whose_requirement_is_declared() {
            let (_dir, bin, metadata) = content_tree();
            glibc_binary(&bin, "bazel");

            check_declared_libc(
                bin.parent().expect("tree root"),
                &metadata,
                &platform("linux/amd64+libc.glibc"),
            )
            .await
            .expect("a declared requirement must be admitted");
        }

        #[tokio::test]
        async fn refuses_a_musl_binary_under_a_glibc_only_declaration() {
            // Subset matching runs per family, not "some libc is declared".
            let (_dir, bin, metadata) = content_tree();
            musl_binary(&bin, "tool");

            let error = check_declared_libc(
                bin.parent().expect("tree root"),
                &metadata,
                &platform("linux/amd64+libc.glibc"),
            )
            .await
            .expect_err("a musl binary under a glibc-only declaration must be refused");

            let LibcLintError::UndeclaredLibc { required, .. } = &error else {
                panic!("expected UndeclaredLibc, got {error:?}");
            };
            assert_eq!(required, "libc.musl");
        }

        #[tokio::test]
        async fn admits_a_static_binary_under_no_declared_libc() {
            // The legitimate empty-`os.features` case: nothing is demanded,
            // so the universality claim is true.
            let (_dir, bin, metadata) = content_tree();
            static_binary(&bin, "tool");

            check_declared_libc(bin.parent().expect("tree root"), &metadata, &platform("linux/amd64"))
                .await
                .expect("a static binary demands no libc");
        }

        #[tokio::test]
        async fn refuses_a_native_dynamic_binary_declared_platform_agnostic() {
            // `any` satisfies every host requirement, so it is a strictly
            // broader false claim than an empty feature list.
            let (_dir, bin, metadata) = content_tree();
            glibc_binary(&bin, "bazel");

            let error = check_declared_libc(bin.parent().expect("tree root"), &metadata, &Platform::Any)
                .await
                .expect_err("a dynamically linked ELF cannot be platform-agnostic");
            assert!(
                matches!(error, LibcLintError::AgnosticPlatformClaim { .. }),
                "expected AgnosticPlatformClaim, got {error:?}"
            );
        }

        #[tokio::test]
        async fn skips_targets_whose_libc_ocx_does_not_model() {
            // macOS has one libc and Windows has no `libc.*` vocabulary;
            // host detection returns an empty set on both, so a declared
            // feature there could never be satisfied by any host.
            let (_dir, bin, metadata) = content_tree();
            glibc_binary(&bin, "tool");

            for spec in ["darwin/arm64", "windows/amd64"] {
                check_declared_libc(bin.parent().expect("tree root"), &metadata, &platform(spec))
                    .await
                    .unwrap_or_else(|error| panic!("{spec} must not be checked, got {error:?}"));
            }
        }

        #[tokio::test]
        async fn ignores_files_outside_the_interface_path() {
            // Scope statement, locked in: a glibc binary that the package
            // does not put on a consumer's PATH is not a subject.
            let (dir, _bin, metadata) = content_tree();
            let private = dir.path().join("libexec");
            std::fs::create_dir_all(&private).expect("create libexec/");
            glibc_binary(&private, "helper");

            check_declared_libc(dir.path(), &metadata, &platform("linux/amd64"))
                .await
                .expect("only interface PATH directories are scanned");
        }
    }
}
