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
//! on a consumer's `PATH` — resolved by this module's own
//! [`resolve_scan_scope`], deliberately not [`super::bin_scan`]'s, because the
//! two answer different questions about the same metadata. What this does
//! *not* catch is listed on [`check_declared_libc`].

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::oci::host_capabilities::LibcFlavor;
use crate::oci::{OperatingSystem, Platform};
use crate::package::metadata::authoring::AuthoringMetadata;

/// Whether this check inspects anything at all for `platform` — the lint's
/// scope rule, and the **one** implementation of it.
///
/// Linux targets and [`Platform::Any`] are checked; every other concrete
/// target is not (see the module "Scope" note: macOS ships one C library, and
/// OCX's `os.features` vocabulary has no `libc.*` tag for any Windows CRT, so
/// a feature declared there could never be satisfied by any host).
///
/// Exposed because a caller that *bypasses* the check needs the same rule to
/// know whether a bypass is worth mentioning: [`check_declared_libc`] returns
/// `Ok(())` immediately for an out-of-scope platform, so announcing a skipped
/// verification there would name something that was never going to be
/// verified. Two consumers, one predicate — restating it at the call site is
/// how the two drift apart.
pub fn checks_declared_libc(platform: &Platform) -> bool {
    matches!(
        platform,
        Platform::Any
            | Platform::Specific {
                os: OperatingSystem::Linux,
                ..
            }
    )
}

/// Checks every file the package puts on an interface `PATH` directory
/// against `platform`'s declared `os.features`, refusing a binary that needs
/// a libc family the declaration does not require.
///
/// A no-op for any platform [`checks_declared_libc`] excludes. For
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
/// [`read_elf_libc`]); [`LibcLintError::UnresolvableScanScope`] and
/// [`LibcLintError::ModifierBearingScanScope`] for a `PATH` segment that names
/// this package and scopes no directory; [`LibcLintError::Scan`] for a
/// directory-walk failure.
pub async fn check_declared_libc(
    content_root: &Path,
    metadata: &AuthoringMetadata,
    platform: &Platform,
) -> Result<(), LibcLintError> {
    // Single-libc or no-vocabulary targets: nothing to check.
    if !checks_declared_libc(platform) {
        return Ok(());
    }
    let declared = match platform {
        // `Any` declares compatibility with every host, so it satisfies any
        // libc requirement a host could state — an empty declared set makes
        // every dynamic binary below a violation, which is the intent.
        Platform::Any => BTreeSet::new(),
        // Linux, by the scope check above — no other concrete target reaches
        // here.
        Platform::Specific { os_features, .. } => declared_libcs(os_features),
    };

    let scope = resolve_scan_scope(metadata);
    // A shape that names this package's install path and still will not
    // resolve leaves a directory uninspected, and a subset scan would report
    // success over it.
    if !scope.unresolvable.is_empty() {
        return Err(LibcLintError::UnresolvableScanScope {
            values: scope.unresolvable,
        });
    }
    // Same refusal, different cause and therefore a different remedy: the
    // directory is just as uninspected, but the value's shape is fine.
    if !scope.modifier_bearing.is_empty() {
        return Err(LibcLintError::ModifierBearingScanScope {
            values: scope.modifier_bearing,
        });
    }

    // No empty-result refusal. The invariant is that the lint may only pass
    // when it inspected the file set it was supposed to inspect — and a
    // resolved scope holding nothing IS that set, inspected. A package that
    // puts no file on PATH has no libc requirement, so an empty `os.features`
    // is true and there is nothing to contradict. Only an *unresolvable*
    // scope (above) means the lint looked nowhere. Conflating the two refused
    // `binaries`-declared-but-absent, which ADR §2 rules legal.
    for path in collect_scan_files(content_root, metadata, &scope.directories).await? {
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
                suggestion: platform.with_os_feature(&required).to_string(),
                platform: platform.to_string(),
                required,
            },
        });
    }
    Ok(())
}

/// Where the package puts its own content on an interface `PATH`.
#[derive(Debug)]
struct ScanScope {
    /// `${installPath}`-relative directories to inspect. Empty means the
    /// package puts nothing of its own on `PATH` — nothing to check.
    directories: Vec<crate::utility::fs::path::RelativePath>,
    /// Values naming this package's install path in a shape that cannot be
    /// resolved to a directory.
    unresolvable: Vec<String>,
    /// Values whose *only* obstacle is a render modifier on the install-path
    /// token. Kept apart from `unresolvable` because the two need different
    /// remedies: this one has no shape problem at all, and the respelling
    /// `unresolvable` advises would leave it exactly as unscoped as before.
    modifier_bearing: Vec<String>,
}

/// Resolves the lint's scan scope from the metadata alone (no filesystem).
///
/// Deliberately **not** [`super::bin_scan`]'s scope. That module answers "which
/// directories claim command names", and excludes awkward shapes best-effort
/// because a missed name is a missed *claim*. This answers "which directories
/// hold files a consumer will execute", where a missed directory is a binary
/// whose loader never got read. Same metadata, different question — sharing
/// one projection between them is what let a bare `${installPath}` be refused
/// as illegal and a `:`-joined value be dropped in silence.
///
/// A `PATH` value is a separator-joined list, so each segment is classified on
/// its own: `${installPath}/bin:${deps.other.installPath}/bin` contributes
/// `bin` and ignores the dependency's tree rather than failing whole. Segments
/// that never name this package's install path (`/usr/bin`, a `${deps.*}` tree)
/// are not this package's to inspect. A bare `${installPath}` is the content
/// root itself — a legal shape, and one this lint must scan rather than refuse.
fn resolve_scan_scope(metadata: &AuthoringMetadata) -> ScanScope {
    use crate::package::metadata::env::modifier::Modifier;

    let AuthoringMetadata::Bundle(bundle) = metadata;
    let mut scope = ScanScope {
        directories: Vec::new(),
        unresolvable: Vec::new(),
        modifier_bearing: Vec::new(),
    };
    for var in &bundle.env {
        if !var.visibility.has_interface() {
            continue;
        }
        let Modifier::Path(path_var) = &var.modifier else {
            continue;
        };
        for segment in path_list_segments(&path_var.value) {
            classify_path_segment(segment, &mut scope);
        }
    }
    scope
}

/// Splits a `PATH` value into its separator-joined segments.
///
/// Split on `:` rather than `std::env::split_paths`: the value is authored for
/// the *target*, and this lint only runs for Linux targets, so the host's
/// separator is the wrong one to use here.
///
/// A `:` inside a `${…}` is a render modifier's separator, not a `PATH` one —
/// `${self.installPath:posix}/bin` is one segment, and a plain `split(':')`
/// would tear it into `${self.installPath` and `posix}/bin`, neither of which
/// names this package. That is the fail-open shape the whole lint exists to
/// avoid. **Which bytes lie inside a `${…}` is
/// [`crate::package::metadata::template::scanner::scan`]'s answer and never
/// this module's** (D10): the value is scanned once, and a `:` splits only when
/// it falls in a [`Segment::Literal`] run. A second rule for `${`/`}` extent
/// here would be a second recogniser, free to disagree with the one that
/// decides what actually resolves.
///
/// A value the scanner refuses is returned whole, so it reaches
/// [`classify_path_segment`] as a single unresolvable segment. One
/// unrecognised token therefore costs the whole value rather than just its own
/// segment — the fail-closed direction, and the one this lint takes everywhere
/// else.
fn path_list_segments(value: &str) -> Vec<&str> {
    use crate::package::metadata::template::scanner::{Segment, scan};

    let Ok(scanned) = scan(value) else {
        return vec![value];
    };

    // Cut on `value` at the offsets the scan reports, so each segment is an
    // exact subslice of what the publisher wrote — escapes and all — rather
    // than a re-rendering of it. The offsets are the scanner's own, which is
    // what makes them right for a fired escape too: that run emits two bytes
    // out of three, so the pieces' lengths do not sum to the value's and no
    // cursor kept here could locate them.
    let mut segments = Vec::new();
    let mut start = 0usize;
    for piece in &scanned {
        let Segment::Literal { text, at } = piece else {
            continue;
        };
        for (offset, _) in text.match_indices(':') {
            let split = at + offset;
            segments.push(&value[start..split]);
            start = split + 1;
        }
    }

    segments.push(&value[start..]);
    segments
}

/// Folds one `PATH` segment into `scope`.
///
/// Recognition is [`crate::package::metadata::template::scanner::scan`]'s, not
/// a substring test: `${installPath}` and `${self.installPath}` are the same
/// referent (D4), so a textual `contains("${installPath}")` both missed the
/// alias and matched the escaped `$${installPath}` that renders as literal
/// text.
///
/// Four outcomes, one scan:
///
/// - A lone modifier-free install-path token — the content root itself — pushes
///   [`crate::utility::fs::path::RelativePath::default`] onto `directories`.
/// - An install-path-rooted directory (via
///   [`crate::package::metadata::template::classify_install_path_rooted_dir`])
///   pushes that relative directory.
/// - A segment whose only obstacle is a render modifier on the install-path
///   token — `${self.installPath:posix}/bin` — goes to `modifier_bearing`,
///   decided by [`modifier_is_the_only_obstacle`] over the pieces already
///   scanned. It scopes no directory either, and is refused just as hard; it is
///   held apart because the remedy differs, and telling a publisher with a
///   legal, publish-validated value to respell it would be advice they cannot
///   act on.
/// - A segment that names the install path in any other shape — a combined
///   value, an escaping `<rel>` — goes to `unresolvable`, and so does a segment
///   whose scan **errors**. That last branch is defensive at the only call site
///   there is: `ocx package create` runs `validate_for_publish` — which scans
///   every env value — before [`check_declared_libc`], deliberately, so that a
///   misspelt token is named as a misspelt token. A value reaching here has
///   therefore already scanned clean. It is kept because the ordering is
///   another module's, and because the alternative to recording an unscannable
///   segment is dropping it.
/// - A segment naming no install-path token at all contributes nothing.
fn classify_path_segment(segment: &str, scope: &mut ScanScope) {
    use crate::package::metadata::template::classify_install_path_rooted_dir;
    use crate::package::metadata::template::scanner::{Segment, TokenShape, scan};
    use crate::utility::fs::path::RelativePath;

    let Ok(scanned) = scan(segment) else {
        scope.unresolvable.push(segment.to_string());
        return;
    };

    let names_install_path = scanned
        .iter()
        .any(|piece| matches!(piece, Segment::Token(token) if token.shape == TokenShape::InstallPath));
    if !names_install_path {
        return;
    }

    // A lone modifier-free token is the content root itself — a legal shape,
    // and one this lint must scan rather than refuse or drop.
    if let [Segment::Token(token)] = scanned.as_slice()
        && token.modifier.is_none()
    {
        scope.directories.push(RelativePath::default());
        return;
    }

    if let Some(directory) = classify_install_path_rooted_dir(segment) {
        scope.directories.push(directory);
        return;
    }

    // Names this package and still will not resolve: recorded, never dropped.
    // Leaving a directory uninspected while reporting success is the failure
    // mode the whole lint exists to avoid. Which list it lands in decides which
    // remedy the publisher is handed, and a render modifier — the one obstacle
    // a value with no shape problem can have — needs a different one.
    if modifier_is_the_only_obstacle(&scanned) {
        scope.modifier_bearing.push(segment.to_string());
    } else {
        scope.unresolvable.push(segment.to_string());
    }
}

/// Whether the only thing keeping an already-scanned segment from classifying
/// to a directory is a render modifier on its install-path token.
///
/// Decided over the segments the caller already scanned: re-reading the text
/// here would be a second recogniser, free to disagree with the one that
/// classified it. The two shapes accepted below mirror the two that resolve —
/// [`crate::package::metadata::template::classify_install_path_rooted_dir`]'s
/// `[token][/<rel>]`, and [`classify_path_segment`]'s lone-token content root —
/// with `modifier.is_none()` inverted: drop the modifier and each would scope a
/// directory. Everything else (an escaping `<rel>`, a combined value, a second
/// token) is a shape problem the modifier is not to blame for.
fn modifier_is_the_only_obstacle(scanned: &[crate::package::metadata::template::scanner::Segment<'_>]) -> bool {
    use crate::package::metadata::template::scanner::{Segment, TokenShape};
    use crate::utility::fs::path::RelativePath;

    let (token, rest) = match scanned {
        [Segment::Token(token)] => (token, None),
        [Segment::Token(token), Segment::Literal { text: rest, .. }] => (token, Some(*rest)),
        _ => return false,
    };

    if token.shape != TokenShape::InstallPath || token.modifier.is_none() {
        return false;
    }

    rest.is_none_or(|rest| {
        rest.strip_prefix('/')
            .is_some_and(|relative| RelativePath::parse(relative).is_ok())
    })
}

/// Every regular file under `directories`, resolved against the wildcard top
/// level `strip_components` maps `${installPath}` onto.
///
/// The walk is [`bin_scan::scan_directory_files`], shared with the binaries
/// scan. What is *not* shared is the filter over it: this keeps every file the
/// walk yields, because the binaries scan's predicates belong to the *binaries
/// claim* and applying them here would hide files whose loader still matters —
/// a bundled `.so`, a name `BinaryName` rejects, a file whose exec bit a
/// non-Unix build host cannot even read.
async fn collect_scan_files(
    content_root: &Path,
    metadata: &AuthoringMetadata,
    directories: &[crate::utility::fs::path::RelativePath],
) -> Result<Vec<PathBuf>, LibcLintError> {
    use crate::package::bin_scan;
    use crate::utility::fs::path::join_under_root;

    let AuthoringMetadata::Bundle(bundle) = metadata;
    let strip = usize::from(bundle.strip_components.unwrap_or(0));
    let wildcard_dirs = bin_scan::wildcard_target_dirs(content_root, strip).await?;

    let mut files = Vec::new();
    for wildcard_dir in &wildcard_dirs {
        for relative in directories {
            let Ok(scan_dir) = join_under_root(wildcard_dir, relative.as_path()) else {
                continue;
            };
            // No filter — the loader of every file that ships here matters.
            files.extend(
                bin_scan::scan_directory_files(&scan_dir)
                    .await?
                    .into_iter()
                    .map(|(path, _metadata)| path),
            );
        }
    }
    files.sort();
    Ok(files)
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
    let Some(interpreter_header) = object
        .segments()
        .iter()
        .find(|header| header.p_type == elf::abi::PT_INTERP)
        .copied()
    else {
        return Ok(ElfLibc::Static);
    };

    let raw = object.segment_data(&interpreter_header).map_err(unparseable)?;
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
/// Do not "unify" the two. Beyond the banner probe being unrunnable here, the
/// host module's loader-name fragments are `#[cfg]`-gated to the **build
/// host's** architecture, so reusing them would misclassify precisely the
/// foreign-arch artifact this exists to check. The seam the two modules do
/// share is the one that shows up on the wire — [`LibcFlavor`] and its
/// `os_feature_tag` / `from_os_feature_tag` round trip — not the strategy for
/// arriving at a family.
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
    /// A `PATH` segment names this package's install path in a shape that
    /// cannot be resolved to a directory, so that directory's files were
    /// never inspected.
    #[error(
        "cannot resolve which directory {} names, so its files could not be checked against the \
         declared os.features; write it as `${{self.installPath}}` or \
         `${{self.installPath}}/<dir>`",
        values.join(", ")
    )]
    UnresolvableScanScope {
        /// The unresolvable `PATH` segments.
        values: Vec<String>,
    },
    /// A `PATH` segment names this package's install path through a
    /// modifier-bearing token, which scopes no directory — so that directory's
    /// files were never inspected.
    ///
    /// Sibling of [`LibcLintError::UnresolvableScanScope`], separate because
    /// the value has no shape problem: `${self.installPath:posix}/bin` is legal
    /// and publish-validated, and the respelling that message advises would
    /// leave it just as unscoped. Naming the modifier is what lets the
    /// publisher act.
    #[error(
        "a render modifier leaves {} unscoped, so its files could not be checked against the \
         declared os.features; write the install-path token without one — `${{self.installPath}}` \
         or `${{self.installPath}}/<dir>`",
        values.join(", ")
    )]
    ModifierBearingScanScope {
        /// The modifier-bearing `PATH` segments.
        values: Vec<String>,
    },
    /// Walking the content tree for candidate files failed.
    #[error("failed to walk the content tree while checking declared os.features")]
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
            | Self::UnrecognizedInterpreter { .. }
            | Self::UnresolvableScanScope { .. }
            | Self::ModifierBearingScanScope { .. } => Some(crate::cli::ExitCode::DataError),
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

    // ── Scope rule ───────────────────────────────────────────────────────

    #[test]
    fn checks_declared_libc_covers_linux_and_any_and_nothing_else() {
        // The one implementation of "does this platform get checked", shared
        // with the CLI's `--no-libc-lint` warning. Both outcomes, so a
        // predicate that answered a constant would fail one of them.
        for spec in ["linux/amd64", "linux/arm64/v8", "linux/amd64+libc.musl", "any"] {
            let platform: Platform = spec.parse().expect("parses");
            assert!(checks_declared_libc(&platform), "{spec} must be in scope");
        }
        for spec in ["darwin/arm64", "darwin/amd64", "windows/amd64", "windows/amd64+win32k"] {
            let platform: Platform = spec.parse().expect("parses");
            assert!(!checks_declared_libc(&platform), "{spec} must be out of scope");
        }
    }

    // ── Scan scope (D10 / C-011) ─────────────────────────────────────────

    /// Authoring metadata carrying exactly one interface `PATH` var, so a scope
    /// assertion is about the segment classifier and nothing else.
    fn path_var_metadata(value: &str) -> AuthoringMetadata {
        serde_json::from_str(&format!(
            r#"{{"type":"bundle","version":1,"env":[{{"key":"PATH","type":"path","value":"{value}","required":false,"visibility":"interface"}}]}}"#
        ))
        .expect("fixture metadata parses")
    }

    /// `(directories, unresolvable)` for a package whose interface `PATH` is
    /// `value`. Asserting on the scope *value* rather than on
    /// `check_declared_libc` returning `Ok` is the discriminating form: a
    /// correctly-scoped, correctly-declared package and a scope that was never
    /// resolved both produce "no error".
    fn scope_of(value: &str) -> (Vec<PathBuf>, Vec<String>) {
        let scope = resolve_scan_scope(&path_var_metadata(value));
        (
            scope
                .directories
                .iter()
                .map(|relative| relative.as_path().to_path_buf())
                .collect(),
            scope.unresolvable,
        )
    }

    /// C-011 — the scan scope is the same for both spellings of the install
    /// path, because they are the same referent (D4).
    ///
    /// The hazard this closes is fail-open and silent: the recogniser it
    /// replaces is `segment.contains("${installPath}")`, and `${installPath}` is
    /// not a substring of `${self.installPath}`, so every segment was skipped,
    /// the scope came out **empty**, and nothing landed in `unresolvable`
    /// either. Per the `ScanScope` contract an empty scope means "the package
    /// puts nothing of its own on `PATH` — nothing to check", so the lint passed
    /// vacuously and a glibc/musl mismatch would have shipped unnoticed. Both
    /// halves of the tuple are therefore asserted: an implementation that
    /// recorded the alias as unresolvable would refuse the package instead, and
    /// that is a different wrong answer.
    #[test]
    fn the_scan_scope_treats_the_self_alias_like_the_bare_install_path_token() {
        let bare = scope_of("${installPath}/bin");
        assert_eq!(
            bare,
            (vec![PathBuf::from("bin")], Vec::<String>::new()),
            "the bare spelling scopes to bin with nothing unresolvable"
        );
        assert_eq!(
            scope_of("${self.installPath}/bin"),
            bare,
            "the alias must produce an identical scope, not an empty one"
        );
    }

    /// C-011 — a `:`-joined value is classified segment by segment: this
    /// package's directory is scanned, a dependency's tree is not this
    /// package's to inspect, and neither outcome is `unresolvable`.
    #[test]
    fn the_scan_scope_ignores_a_dependency_segment_in_a_joined_path_value() {
        assert_eq!(
            scope_of("${self.installPath}/bin:${deps.other.installPath}/bin"),
            (vec![PathBuf::from("bin")], Vec::<String>::new())
        );
    }

    /// A bare install-path token is the content root itself — a legal shape,
    /// and one the lint must scan rather than refuse or drop.
    #[test]
    fn the_scan_scope_reads_a_bare_install_path_token_as_the_content_root() {
        assert_eq!(
            scope_of("${self.installPath}"),
            (vec![PathBuf::from("")], Vec::<String>::new())
        );
    }

    /// A shape that names this package's install path and still will not
    /// resolve is recorded, not dropped: leaving a directory uninspected while
    /// reporting success is the failure mode the whole lint exists to avoid.
    ///
    /// A render modifier is recorded on its own list rather than among the
    /// unresolvable shapes — the value has no shape problem, and the two lists
    /// carry different remedies. Both are asserted: an implementation that
    /// simply stopped recording the modifier case would empty `unresolvable`
    /// too, and that is the silent drop this contract forbids.
    #[test]
    fn the_scan_scope_records_an_install_path_shape_it_cannot_resolve() {
        let scope = resolve_scan_scope(&path_var_metadata("${self.installPath:posix}/bin"));
        assert!(scope.directories.is_empty(), "a modifier-bearing token scopes nothing");
        assert_eq!(
            scope.modifier_bearing,
            vec!["${self.installPath:posix}/bin".to_string()],
            "the segment must be recorded, and as the modifier case it is"
        );
        assert!(
            scope.unresolvable.is_empty(),
            "the value's shape is fine, so it is not an unresolvable shape"
        );
    }

    /// A `:` splits only where the scanner puts it outside a `${…}`, and the
    /// segments it produces are exact subslices of what the publisher wrote —
    /// escapes included. `$${installPath}` is literal text and names no token,
    /// so it scopes nothing; a split that handed on the *rendered* segment
    /// would hand on `${installPath}/bin` and scope a directory the package
    /// never put on `PATH`.
    #[test]
    fn the_scan_scope_splits_around_an_escaped_delimiter_without_unescaping_it() {
        assert_eq!(
            scope_of("$${installPath}/bin:${self.installPath}/lib"),
            (vec![PathBuf::from("lib")], Vec::<String>::new()),
            "only the real token scopes; the escaped one is text on both sides of the split"
        );
    }

    /// A segment naming no install-path token at all contributes nothing — not
    /// a directory, and not an `unresolvable` entry. Without this leg the
    /// contract above cannot tell "recorded because it names us" from "records
    /// everything".
    #[test]
    fn the_scan_scope_ignores_a_segment_that_never_names_this_package() {
        assert_eq!(
            scope_of("/usr/bin"),
            (Vec::<PathBuf>::new(), Vec::<String>::new()),
            "a foreign absolute path is not this package's to inspect"
        );
    }

    // ── Scan-scope refusal messages ──────────────────────────────────────

    /// `${self.installPath:posix}/bin` on a Linux target is a legal,
    /// publish-validated `PATH` value, and the lint still refuses it —
    /// correctly, because a modifier-bearing token classifies to no directory
    /// and reporting "libc verified" over a directory nobody opened is the
    /// failure this lint exists to prevent. What is wrong is the *message*: the
    /// publisher is told OCX "cannot resolve which directory this names" and is
    /// pointed at a respelling, for a value with no shape problem at all. The
    /// cause is the render modifier, and only naming it lets them act.
    ///
    /// Both legs are required. `UnresolvableScanScope` folds several causes into
    /// one message, so an implementation that blamed the render modifier for
    /// *every* unresolvable segment would satisfy the first leg while telling a
    /// root-escaping value something simply untrue.
    #[tokio::test]
    async fn a_modifier_bearing_path_segment_is_refused_for_its_modifier_not_as_an_unresolvable_shape() {
        use crate::cli::{ClassifyExitCode, ExitCode};

        let linux: Platform = "linux/amd64".parse().expect("platform spec parses");
        // The scope refusal fires before any candidate file is read, so the two
        // legs differ only in the `PATH` value under test.
        let content_root = std::path::Path::new("content-root-never-read");

        let modifier = check_declared_libc(
            content_root,
            &path_var_metadata("${self.installPath:posix}/bin"),
            &linux,
        )
        .await
        .expect_err("a segment that classifies to no directory must still be refused, not scanned past");
        let modifier_message = modifier.to_string();

        assert!(
            modifier_message.contains("${self.installPath:posix}/bin"),
            "the offending value must be named verbatim: {modifier_message}"
        );
        assert!(
            modifier_message.contains("render modifier"),
            "the publisher must be told the render modifier is what leaves the directory unscoped, \
             not that OCX cannot work out what their value names: {modifier_message}"
        );
        assert_eq!(
            modifier.classify(),
            Some(ExitCode::DataError),
            "a refused publish stays 65; only the message changes"
        );

        let shape = check_declared_libc(content_root, &path_var_metadata("${installPath}/../etc"), &linux)
            .await
            .expect_err("a root-escaping segment must still be refused")
            .to_string();
        assert!(
            shape.contains("${installPath}/../etc"),
            "the offending value must be named verbatim here too: {shape}"
        );
        assert!(
            !shape.contains("render modifier"),
            "a value carrying no modifier must not be blamed on one: {shape}"
        );
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
        async fn refuses_a_binary_whose_name_the_binary_grammar_rejects() {
            // Defends the second half of `scan_directory_files`'s contract:
            // the binaries scan's `BinaryName` grammar must not leak into the
            // shared walk. A leading-dot name is a `PATH` file with a dynamic
            // loader like any other — every other fixture here is
            // grammar-valid, so pushing that predicate into the walk would
            // stop reading this file and ship the false `os.features` claim
            // with nothing red.
            let (_dir, bin, metadata) = content_tree();
            glibc_binary(&bin, ".ld-shim");

            let error = check_declared_libc(bin.parent().expect("tree root"), &metadata, &platform("linux/amd64"))
                .await
                .expect_err("a name the binaries grammar rejects still has a loader that matters");

            let LibcLintError::UndeclaredLibc { path, required, .. } = &error else {
                panic!("expected UndeclaredLibc, got {error:?}");
            };
            assert!(
                path.ends_with(".ld-shim"),
                "the refusal must name the grammar-invalid file, got {path:?}"
            );
            assert_eq!(required, "libc.glibc");
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

        /// A content tree with a metadata sidecar built from raw env-var
        /// JSON, for the shapes `content_tree`'s single `${installPath}/bin`
        /// var cannot express.
        fn tree_with_env(env_json: &str) -> (tempfile::TempDir, AuthoringMetadata) {
            let dir = tempfile::tempdir().expect("tempdir");
            let metadata: AuthoringMetadata =
                serde_json::from_str(&format!(r#"{{"type":"bundle","version":1,"env":[{env_json}]}}"#))
                    .expect("fixture metadata parses");
            (dir, metadata)
        }

        fn path_var(value: &str) -> String {
            format!(r#"{{"key":"PATH","type":"path","value":"{value}","required":false,"visibility":"interface"}}"#)
        }

        #[tokio::test]
        async fn refuses_a_path_segment_it_cannot_resolve_to_a_directory() {
            // A root-escaping segment names this package's install path and
            // still resolves to no directory, so its files went uninspected.
            let (dir, metadata) = tree_with_env(&path_var("${installPath}/../etc"));
            glibc_binary(dir.path(), "bazel");

            let error = check_declared_libc(dir.path(), &metadata, &platform("linux/amd64"))
                .await
                .expect_err("an unresolvable segment must be refused, not skipped");
            assert!(
                matches!(error, LibcLintError::UnresolvableScanScope { .. }),
                "expected UnresolvableScanScope, got {error:?}"
            );
        }

        #[tokio::test]
        async fn admits_a_path_that_only_names_a_dependency_tree() {
            // The review's probe: a sidecar whose only interface PATH var
            // points at a pinned dependency's tree. Legal, publish-validated
            // metadata, and nothing of THIS package ships there — so there is
            // nothing for this lint to inspect and nothing to refuse. The
            // remedy an unresolvable-scope error would name is impossible
            // here: a dependency's directory can never be respelled
            // `${installPath}/<dir>`.
            let (dir, metadata) = tree_with_env(&path_var("${deps.jdk.installPath}/bin"));
            glibc_binary(dir.path(), "stray");

            check_declared_libc(dir.path(), &metadata, &platform("linux/amd64"))
                .await
                .expect("a dependency-only PATH is not this package's to inspect");
        }

        #[tokio::test]
        async fn scans_the_package_segment_of_a_joined_path_value() {
            // A `PATH` value is a separator-joined list. The package's own
            // segment is inspected; the dependency's tree is not this
            // package's to check. Refusing the whole value would block a
            // legal shape, and scanning nothing would pass one silently.
            let (dir, metadata) = tree_with_env(&path_var("${installPath}/bin:${deps.other.installPath}/bin"));
            std::fs::create_dir_all(dir.path().join("bin")).expect("create bin/");
            glibc_binary(&dir.path().join("bin"), "bazel");

            let error = check_declared_libc(dir.path(), &metadata, &platform("linux/amd64"))
                .await
                .expect_err("the package's own segment must still be scanned");
            assert!(
                matches!(error, LibcLintError::UndeclaredLibc { .. }),
                "expected the libc mismatch from bin/, got {error:?}"
            );
        }

        #[tokio::test]
        async fn scans_the_content_root_for_a_bare_install_path() {
            // `${installPath}` puts the package root on PATH — a legal,
            // validated shape. It must be scanned, never refused as illegal.
            let (dir, metadata) = tree_with_env(&path_var("${installPath}"));
            glibc_binary(dir.path(), "bazel");

            let error = check_declared_libc(dir.path(), &metadata, &platform("linux/amd64"))
                .await
                .expect_err("a root-level binary must be read, not refused for its PATH shape");
            assert!(
                matches!(error, LibcLintError::UndeclaredLibc { .. }),
                "expected the libc mismatch from the content root, got {error:?}"
            );
        }

        #[tokio::test]
        async fn admits_a_bare_install_path_whose_root_binaries_need_nothing() {
            // The same legal shape must be able to PASS, or the test above
            // would be satisfied by a lint that refuses it for any reason.
            let (dir, metadata) = tree_with_env(&path_var("${installPath}"));
            static_binary(dir.path(), "tool");

            check_declared_libc(dir.path(), &metadata, &platform("linux/amd64"))
                .await
                .expect("a bare ${installPath} with a static binary is a clean publish");
        }

        #[tokio::test]
        async fn admits_a_declared_path_directory_that_does_not_exist() {
            // Case 2: the scope RESOLVES and the directory is absent. The
            // lint looked, and the package puts no file on PATH — so it has
            // no libc requirement and an empty `os.features` is true. ADR §2
            // rules a declared-but-absent directory legal; refusing it here
            // would overrule a decision the project already made.
            let (dir, metadata) = tree_with_env(&path_var("${installPath}/bin"));
            glibc_binary(dir.path(), "stray");

            check_declared_libc(dir.path(), &metadata, &platform("linux/amd64"))
                .await
                .expect("a resolved scope holding nothing is an inspected scope");
        }

        #[tokio::test]
        async fn admits_a_declared_path_directory_that_is_empty() {
            // The sibling route to the same empty: the directory exists and
            // ships nothing. Same verdict, same reason.
            let (dir, metadata) = tree_with_env(&path_var("${installPath}/bin"));
            std::fs::create_dir_all(dir.path().join("bin")).expect("create bin/");

            check_declared_libc(dir.path(), &metadata, &platform("linux/amd64"))
                .await
                .expect("an empty declared directory is not a failure to inspect");
        }

        #[tokio::test]
        async fn admits_system_path_entries_alongside_a_scannable_package_dir() {
            // A package may legitimately put system directories on PATH
            // (`/bin`, `/usr/bin` — the shape `--clean` fixtures use so `sh`
            // stays reachable). Those are unresolvable *by intent*: the
            // package ships nothing there, so nothing of its went
            // uninspected. Only a value referencing `${installPath}` that
            // will not classify means the check could not look.
            let (dir, metadata) = tree_with_env(&format!(
                "{},{},{}",
                path_var("${installPath}/bin"),
                path_var("/bin"),
                path_var("/usr/bin")
            ));
            std::fs::create_dir_all(dir.path().join("bin")).expect("create bin/");
            static_binary(&dir.path().join("bin"), "tool");

            check_declared_libc(dir.path(), &metadata, &platform("linux/amd64"))
                .await
                .expect("system PATH entries must not be mistaken for an unlookable scan scope");
        }

        #[tokio::test]
        async fn reads_every_same_named_file_across_two_interface_path_dirs() {
            // `collect_candidates` keys on the bare name, so a second
            // directory shipping the same filename used to be dropped. For
            // the binaries scan that merge is benign — the name is claimed
            // either way. Here the dropped sibling is a file that ships and
            // never gets its loader read.
            let (dir, metadata) = tree_with_env(&format!(
                "{},{}",
                path_var("${installPath}/bin"),
                path_var("${installPath}/tools")
            ));
            std::fs::create_dir_all(dir.path().join("bin")).expect("create bin/");
            std::fs::create_dir_all(dir.path().join("tools")).expect("create tools/");
            // Only the *second* directory's file needs a libc; the first is
            // static, so a first-wins map hides the offender entirely.
            static_binary(&dir.path().join("bin"), "tool");
            glibc_binary(&dir.path().join("tools"), "tool");

            let error = check_declared_libc(dir.path(), &metadata, &platform("linux/amd64"))
                .await
                .expect_err("the same-named sibling in the second PATH dir must still be read");
            let LibcLintError::UndeclaredLibc { path, required, .. } = &error else {
                panic!("expected UndeclaredLibc, got {error:?}");
            };
            assert_eq!(required, "libc.glibc");
            assert!(
                path.ends_with("tools/tool"),
                "the offender must be the tools/ sibling, got {path:?}"
            );
        }

        #[tokio::test]
        async fn reads_a_file_the_binaries_scan_would_skip_as_non_executable() {
            // The lint asks what a file *is*, not whether the filesystem
            // marks it runnable: a glibc-linked object in an interface dir
            // matters to the loader even without the exec bit, and a host
            // that cannot read permission bits at all would otherwise see
            // every candidate as non-executable.
            use std::os::unix::fs::PermissionsExt;
            let (_dir, bin, metadata) = content_tree();
            let binary = bin.join("bazel");
            glibc_binary(&bin, "bazel");
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o644)).expect("drop the exec bit");

            let error = check_declared_libc(bin.parent().expect("tree root"), &metadata, &platform("linux/amd64"))
                .await
                .expect_err("a non-executable ELF on the interface PATH must still be read");
            assert!(
                matches!(error, LibcLintError::UndeclaredLibc { .. }),
                "expected UndeclaredLibc, got {error:?}"
            );
        }

        #[tokio::test]
        async fn ignores_files_outside_the_interface_path() {
            // Scope statement, locked in: a glibc binary that the package
            // does not put on a consumer's PATH is not a subject.
            let (dir, bin, metadata) = content_tree();
            // `bin/` must hold something, or this would pass on the empty-scan
            // refusal instead of on the scope rule it means to pin.
            static_binary(&bin, "tool");
            let private = dir.path().join("libexec");
            std::fs::create_dir_all(&private).expect("create libexec/");
            glibc_binary(&private, "helper");

            check_declared_libc(dir.path(), &metadata, &platform("linux/amd64"))
                .await
                .expect("only interface PATH directories are scanned");
        }
    }
}
