// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Wheel selection: marker evaluation + tag-compatibility ranking per target.
//!
//! For a single `(variant, platform key)` [`PythonTarget`], selects exactly one
//! wheel per applicable package (design spec, "Wheel selection algorithm"):
//! filter packages by their PEP 508 marker against the derived marker
//! environment, then rank each package's candidate wheels by tag priority from
//! `uv-platform-tags`, tiebreaking by build tag then filename. Zero candidates
//! for an applicable package is an actionable [`SelectError::NoCompatibleWheel`].

use std::collections::BTreeSet;
use std::str::FromStr;

use uv_distribution_filename::{BuildTag, WheelFilename};
use uv_pep508::{MarkerEnvironment as UvMarkerEnvironment, MarkerEnvironmentBuilder, MarkerTree};
use uv_platform_tags::{AbiTag, Arch, Os, Platform as UvPlatform, TagCompatibility, TagPriority, Tags};

use crate::lock::{LockedPackage, LockedWheel, Pylock};
use crate::platform::{
    Implementation, LibcFamily, PlatformFacts, PythonTarget, TargetArchitecture, TargetOperatingSystem,
    VariantConstraints, marker_environment,
};

/// Default `manylinux` (glibc) floor when a linux target leaves it unset —
/// matches the design spec's `default` variant (`min_manylinux: "2_28"`).
const DEFAULT_MANYLINUX_FLOOR: &str = "2_28";
/// Default `musllinux` floor when a musl target leaves it unset — matches the
/// design spec's `musl` variant (`min_musllinux: "1_2"`).
const DEFAULT_MUSLLINUX_FLOOR: &str = "1_2";

/// A resolved wheel chosen for a package under a target.
#[derive(Debug, Clone)]
pub struct WheelRef {
    /// The distribution name (e.g. `"numpy"`).
    pub name: String,
    /// The pinned version (e.g. `"2.1.3"`).
    pub version: String,
    /// The wheel filename.
    pub filename: String,
    /// The wheel download URL, when the lock provides one.
    pub url: Option<String>,
    /// The wheel `sha256` hash (hex, no prefix).
    pub sha256: String,
}

/// Selects one wheel per applicable package for `target`.
///
/// Applicability is decided by each package's PEP 508 marker evaluated against
/// the target's derived marker environment; non-applicable packages (OS forks,
/// implementation forks) are dropped, not failed. Wheels are matched by
/// `uv-platform-tags` compatibility (never string equality), so `any`,
/// `py2.py3-none-any`, and `abi3` wheels are honored through compat semantics.
///
/// # Errors
///
/// Returns [`SelectError::NoCompatibleWheel`] when an applicable package has no
/// wheel intersecting the target tag set — naming the package, triple, variant,
/// and the platform tags that WERE available (so a no-wheel-anywhere package is
/// distinguishable from a no-wheel-for-this-triple one). Returns
/// [`SelectError::AbiMismatch`] when a selected binary wheel's ABI is
/// inconsistent with the interpreter pin (`cp313` vs `cp313t`, fails closed),
/// [`SelectError::MissingUrl`] when the chosen wheel carries no download URL and
/// so cannot be mirrored, [`SelectError::MarkerSyntax`] when a package's marker
/// fails to parse, and [`SelectError::TargetModel`] when the target's own axes
/// cannot be turned into a `uv` tag model or marker environment.
pub fn select_wheels(lock: &Pylock, target: &PythonTarget) -> Result<Vec<WheelRef>, SelectError> {
    // Step 1: derive the marker environment for package filtering.
    let marker_env = build_marker_environment(target)?;
    // Step 3: the ordered priority tag set — built once, reused across packages.
    let tags = build_target_tags(target)?;

    let target_label = target_label(target);
    let variant_label = variant_label(&target.variant);
    let interpreter_abi = target.effective_abi().to_string();
    let free_threaded = is_free_threaded(target);
    // `wheel_priority` semantics: a NON-empty list is an admissibility filter
    // + ranking — a tag-compatible wheel whose platform tags match no listed
    // prefix is EXCLUDED, and survivors rank by first-listed-prefix-wins.
    // Absent/empty keeps today's TagPriority-only ordering, unchanged
    // (lib backcompat; the mirror always passes a non-empty derived filter).
    let wheel_priority = target.variant.wheel_priority.as_deref().unwrap_or(&[]);

    let mut selected = Vec::new();
    for package in &lock.packages {
        // Step 2: drop packages excluded by their PEP 508 marker.
        if !package_applies(package, &marker_env)? {
            continue;
        }

        // Step 4: pick the highest-priority compatible wheel.
        let wheel = pick_wheel(package, &tags, &target_label, &variant_label, wheel_priority)?;

        // A wheel with no URL is not mirrorable — reject it so the downstream
        // naming convention's URL assumption holds.
        let Some(url) = wheel.url.clone() else {
            return Err(SelectError::MissingUrl {
                package: package.name.clone(),
                filename: wheel.filename.clone(),
            });
        };

        selected.push(WheelRef {
            name: package.name.clone(),
            version: package.version.clone(),
            filename: wheel.filename.clone(),
            url: Some(url),
            sha256: wheel.sha256.clone(),
        });
    }

    // Step 5: fail closed if any selected binary wheel's ABI contradicts the pin.
    validate_abi_consistency(&selected, &interpreter_abi, free_threaded)?;

    Ok(selected)
}

// ── Selection steps ─────────────────────────────────────────────────────────

/// Step 1: converts the target's L1 facts + interpreter pin into a `uv-pep508`
/// [`UvMarkerEnvironment`] from the target's os/arch [`PlatformFacts`].
fn build_marker_environment(target: &PythonTarget) -> Result<UvMarkerEnvironment, SelectError> {
    let facts = PlatformFacts {
        operating_system: target.platform.operating_system,
        architecture: target.platform.architecture,
    };
    let env = marker_environment(&facts, &target.interpreter);
    UvMarkerEnvironment::try_from(MarkerEnvironmentBuilder {
        implementation_name: &env.implementation_name,
        // CPython reports its interpreter version as the implementation version.
        implementation_version: &env.python_full_version,
        os_name: &env.os_name,
        platform_machine: &env.platform_machine,
        platform_python_implementation: &env.platform_python_implementation,
        platform_release: "",
        platform_system: &env.platform_system,
        platform_version: "",
        python_full_version: &env.python_full_version,
        python_version: &env.python_version,
        sys_platform: &env.sys_platform,
    })
    .map_err(|error| model_error("marker environment", error))
}

/// Step 2: `true` when the package's marker matches the environment (or it has
/// no marker). A malformed marker string is [`SelectError::MarkerSyntax`].
fn package_applies(package: &LockedPackage, env: &UvMarkerEnvironment) -> Result<bool, SelectError> {
    let Some(marker) = package.marker.as_deref() else {
        return Ok(true);
    };
    let tree = MarkerTree::from_str(marker).map_err(|source| SelectError::MarkerSyntax {
        package: package.name.clone(),
        source: Box::new(source),
    })?;
    // No extras drive base-package platform filtering (colorama, watchdog, …).
    Ok(tree.evaluate(env, &[]))
}

/// Step 3: builds the ordered priority tag set for the target. `uv`'s
/// [`Tags::from_env`] already spans `abi3` across CPython minors, unions the
/// `py3`/`none`/`any` tags, and orders exact matches highest — so tag
/// compatibility (never equality) falls out of it.
fn build_target_tags(target: &PythonTarget) -> Result<Tags, SelectError> {
    let platform = build_uv_platform(target)?;
    let version = parse_python_version(&target.interpreter.python_version)?;
    let implementation_name = match target.interpreter.implementation {
        Implementation::CPython => "cpython",
    };
    Tags::from_env(
        &platform,
        version,
        implementation_name,
        // CPython's implementation version tracks its Python version.
        version,
        // Only consulted for a `manylinux` OS; harmless for musl/macOS/Windows.
        true,
        is_free_threaded(target),
    )
    .map_err(|error| model_error("platform tag set", error))
}

/// A ranked wheel candidate for a package: the `wheel_priority` class rank
/// (decision B), then tag priority, then the tiebreak axes (PEP 427 build
/// tag, then filename), highest wins.
struct Candidate<'a> {
    class_rank: usize,
    priority: TagPriority,
    build_tag: Option<BuildTag>,
    wheel: &'a LockedWheel,
}

impl Candidate<'_> {
    /// The descending sort key: higher class rank first, then higher tag
    /// priority, then higher build tag, then greater filename (deterministic
    /// final tiebreak).
    fn key(&self) -> (usize, TagPriority, &Option<BuildTag>, &str) {
        (
            self.class_rank,
            self.priority,
            &self.build_tag,
            self.wheel.filename.as_str(),
        )
    }
}

/// The `wheel_priority` class rank of a wheel: the position of its
/// highest-priority matching prefix among `priority`, inverted so the
/// first-listed prefix ranks highest (`priority.len()`); an unmatched tag
/// contributes nothing. Rank `0` means no prefix matched — with a non-empty
/// `priority` the caller EXCLUDES such wheels (admissibility filter); with an
/// empty `priority` every wheel ranks `0` and today's TagPriority-only
/// ordering applies unchanged (backcompat). Matching is a prefix match
/// against each of the wheel's platform tags (a wheel may carry a compressed
/// multi-tag set), never re-admitting a wheel that tag-compatibility already
/// excluded.
fn class_rank<'a>(platform_tags: impl Iterator<Item = &'a str>, priority: &[String]) -> usize {
    platform_tags
        .filter_map(|tag| priority.iter().position(|prefix| tag.starts_with(prefix.as_str())))
        .map(|position| priority.len() - position)
        .max()
        .unwrap_or(0)
}

/// Step 4: filters a package's tag-compatible wheels through the non-empty
/// `wheel_priority` admissibility list (a wheel matching no listed prefix is
/// excluded), ranks survivors by class rank then tag priority (build tag then
/// filename as deterministic tiebreakers), and returns the best. Zero
/// admissible wheels is [`SelectError::NoCompatibleWheel`], whose
/// `available_tags` names the platform tags that WERE present on the
/// (excluded or incompatible) candidates.
fn pick_wheel<'a>(
    package: &'a LockedPackage,
    tags: &Tags,
    target_label: &str,
    variant_label: &str,
    wheel_priority: &[String],
) -> Result<&'a LockedWheel, SelectError> {
    let mut best: Option<Candidate<'a>> = None;
    let mut available: BTreeSet<String> = BTreeSet::new();

    for wheel in &package.wheels {
        let Ok(parsed) = WheelFilename::from_str(&wheel.filename) else {
            // A non-wheel filename can't be a candidate and contributes no
            // platform tag to the diagnostic set.
            continue;
        };
        let wheel_tags: Vec<String> = parsed.platform_tags().iter().map(ToString::to_string).collect();
        available.extend(wheel_tags.iter().cloned());
        if let TagCompatibility::Compatible(priority) = parsed.compatibility(tags) {
            let class_rank = class_rank(wheel_tags.iter().map(String::as_str), wheel_priority);
            // Admissibility: a non-empty priority list excludes wheels whose
            // platform tags match none of its prefixes (rank 0). Its tags stay
            // in `available` so the NoCompatibleWheel diagnostic names them.
            if !wheel_priority.is_empty() && class_rank == 0 {
                continue;
            }
            let candidate = Candidate {
                class_rank,
                priority,
                build_tag: parsed.build_tag().cloned(),
                wheel,
            };
            if best.as_ref().is_none_or(|best| candidate.key() > best.key()) {
                best = Some(candidate);
            }
        }
    }

    match best {
        Some(Candidate { wheel, .. }) => Ok(wheel),
        None => Err(SelectError::NoCompatibleWheel {
            package: package.name.clone(),
            target: target_label.to_string(),
            variant: variant_label.to_string(),
            available_tags: available.into_iter().collect(),
        }),
    }
}

/// Step 5: rejects any selected binary wheel whose CPython ABI's free-threaded
/// flag contradicts the target's (`cp313` vs `cp313t`). `abi3`/`none` wheels are
/// ABI-agnostic and always consistent.
fn validate_abi_consistency(
    selected: &[WheelRef],
    interpreter_abi: &str,
    free_threaded: bool,
) -> Result<(), SelectError> {
    for wheel in selected {
        let Ok(parsed) = WheelFilename::from_str(&wheel.filename) else {
            continue;
        };
        for abi in parsed.abi_tags() {
            if let AbiTag::CPython { gil_disabled, .. } = abi
                && *gil_disabled != free_threaded
            {
                return Err(SelectError::AbiMismatch {
                    filename: wheel.filename.clone(),
                    wheel_abi: abi.to_string(),
                    interpreter_abi: interpreter_abi.to_string(),
                });
            }
        }
    }
    Ok(())
}

// ── Target → uv model helpers ───────────────────────────────────────────────

/// Maps the target's os/arch + variant libc into a `uv` [`UvPlatform`]. The
/// variant's libc floor becomes the `manylinux`/`musllinux` OS version, which is
/// exactly what constrains [`Tags::from_env`]'s compatible platform tags.
fn build_uv_platform(target: &PythonTarget) -> Result<UvPlatform, SelectError> {
    let arch = match target.platform.architecture {
        TargetArchitecture::Amd64 => Arch::X86_64,
        TargetArchitecture::Arm64 => Arch::Aarch64,
    };
    let os = match target.platform.operating_system {
        TargetOperatingSystem::Linux => linux_os(&target.variant)?,
        // ponytail: v1 ships linux/amd64 only; macOS uses a permissive recent
        // deployment floor (accepts wheels for that OS version or older).
        // Refine the floor when the darwin leg lands.
        TargetOperatingSystem::Darwin => Os::Macos { major: 15, minor: 0 },
        TargetOperatingSystem::Windows => Os::Windows,
    };
    Ok(UvPlatform::new(os, arch))
}

/// Derives the linux [`Os`] (with libc floor) from the variant constraints.
fn linux_os(variant: &VariantConstraints) -> Result<Os, SelectError> {
    if variant.libc == Some(LibcFamily::Musl) {
        let floor = variant.min_musllinux.as_deref().unwrap_or(DEFAULT_MUSLLINUX_FLOOR);
        let (major, minor) = parse_libc_floor(floor)?;
        Ok(Os::Musllinux { major, minor })
    } else {
        let floor = variant.min_manylinux.as_deref().unwrap_or(DEFAULT_MANYLINUX_FLOOR);
        let (major, minor) = parse_libc_floor(floor)?;
        Ok(Os::Manylinux { major, minor })
    }
}

/// Parses a `python_version` marker value (`"3.13"`) into `(major, minor)`.
fn parse_python_version(version: &str) -> Result<(u8, u8), SelectError> {
    let mut parts = version.split('.');
    let major = parts
        .next()
        .unwrap_or_default()
        .parse::<u8>()
        .map_err(|error| model_error("python major version", error))?;
    let minor = parts
        .next()
        .unwrap_or_default()
        .parse::<u8>()
        .map_err(|error| model_error("python minor version", error))?;
    Ok((major, minor))
}

/// Parses a libc floor in wheel-tag spelling (`"2_28"`) into `(major, minor)`.
fn parse_libc_floor(floor: &str) -> Result<(u16, u16), SelectError> {
    let mut parts = floor.split('_');
    let major = parts
        .next()
        .unwrap_or_default()
        .parse::<u16>()
        .map_err(|error| model_error("libc floor major", error))?;
    let minor = parts
        .next()
        .unwrap_or_default()
        .parse::<u16>()
        .map_err(|error| model_error("libc floor minor", error))?;
    Ok((major, minor))
}

/// `true` when the target is free-threaded CPython (`cp313t`-style ABI).
fn is_free_threaded(target: &PythonTarget) -> bool {
    target.effective_abi().ends_with('t')
}

/// A short `os/arch` triple label for error messages.
fn target_label(target: &PythonTarget) -> String {
    let os = match target.platform.operating_system {
        TargetOperatingSystem::Linux => "linux",
        TargetOperatingSystem::Darwin => "darwin",
        TargetOperatingSystem::Windows => "windows",
    };
    let arch = match target.platform.architecture {
        TargetArchitecture::Amd64 => "amd64",
        TargetArchitecture::Arm64 => "arm64",
    };
    format!("{os}/{arch}")
}

/// A short variant label for error messages (`"default"`, `"musl"`, `"cp313t"`,
/// `"musl-cp313t"`), mirroring the L2 variant-prefix composition.
fn variant_label(variant: &VariantConstraints) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if variant.libc == Some(LibcFamily::Musl) {
        parts.push("musl");
    }
    if let Some(abi) = &variant.abi {
        parts.push(abi);
    }
    if parts.is_empty() {
        "default".to_string()
    } else {
        parts.join("-")
    }
}

/// Wraps a `uv`/parse failure encountered while turning the target's own axes
/// into a tag model as a [`SelectError::TargetModel`], carrying the source.
fn model_error(context: &str, source: impl std::error::Error + Send + Sync + 'static) -> SelectError {
    SelectError::TargetModel {
        context: context.to_string(),
        source: Box::new(source),
    }
}

/// Errors from wheel selection.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SelectError {
    /// No wheel for an applicable package intersects the target tag set.
    ///
    /// Names the package, the target triple, the variant, and the tags that
    /// were available on the package's wheels — distinguishing
    /// no-wheel-for-triple (e.g. `psycopg2`) from no-wheel-anywhere
    /// (e.g. `uwsgi`).
    #[error(
        "no compatible wheel for package '{package}' on target '{target}' (variant '{variant}'); available tags: {available_tags:?}"
    )]
    NoCompatibleWheel {
        /// The package with no compatible wheel.
        package: String,
        /// The target triple (os/arch/libc).
        target: String,
        /// The variant name/constraints.
        variant: String,
        /// The platform tags present on the package's candidate wheels.
        available_tags: Vec<String>,
    },
    /// A selected binary wheel's ABI is inconsistent with the interpreter pin
    /// (e.g. `cp313` wheel against a `cp313t` free-threaded interpreter).
    #[error("wheel '{filename}' ABI '{wheel_abi}' is incompatible with interpreter ABI '{interpreter_abi}'")]
    AbiMismatch {
        /// The offending wheel filename.
        filename: String,
        /// The wheel's ABI tag.
        wheel_abi: String,
        /// The interpreter's ABI tag.
        interpreter_abi: String,
    },
    /// The wheel selected for a package carries no download URL and so cannot be
    /// mirrored (a path-based lock entry reached selection).
    #[error("selected wheel '{filename}' for package '{package}' has no download URL")]
    MissingUrl {
        /// The package whose selected wheel has no URL.
        package: String,
        /// The URL-less wheel filename.
        filename: String,
    },
    /// A package's PEP 508 environment marker failed to parse.
    #[error("invalid PEP 508 marker for package '{package}'")]
    MarkerSyntax {
        /// The package whose marker failed to parse.
        package: String,
        /// The underlying `uv-pep508` parse error.
        #[source]
        source: Box<uv_pep508::Pep508Error>,
    },
    /// The target's own axes could not be turned into a `uv` tag model or marker
    /// environment (malformed interpreter version, libc floor, or unsupported
    /// implementation).
    #[error("cannot build the selection tag model ({context})")]
    TargetModel {
        /// Which part of the model construction failed.
        context: String,
        /// The underlying `uv`/parse error.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{InterpreterPin, TargetPlatform};

    // ── Inline fixtures (self-contained; no on-disk fixtures) ───────────────

    fn linux_amd64() -> PythonTarget {
        PythonTarget {
            platform: TargetPlatform {
                operating_system: TargetOperatingSystem::Linux,
                architecture: TargetArchitecture::Amd64,
            },
            variant: VariantConstraints {
                libc: Some(LibcFamily::Gnu),
                min_manylinux: Some("2_28".to_string()),
                min_musllinux: None,
                wheel_priority: None,
                abi: None,
            },
            interpreter: InterpreterPin {
                python_version: "3.13".to_string(),
                python_full_version: "3.13.1".to_string(),
                abi: "cp313".to_string(),
                implementation: Implementation::CPython,
            },
        }
    }

    fn linux_amd64_free_threaded() -> PythonTarget {
        let mut target = linux_amd64();
        target.variant.abi = Some("cp313t".to_string());
        target.interpreter.abi = "cp313t".to_string();
        target
    }

    fn wheel(filename: &str, sha256: &str) -> LockedWheel {
        LockedWheel {
            filename: filename.to_string(),
            url: Some(format!("https://example.test/{filename}")),
            sha256: sha256.to_string(),
        }
    }

    fn package(name: &str, version: &str, marker: Option<&str>, wheels: Vec<LockedWheel>) -> LockedPackage {
        LockedPackage {
            name: name.to_string(),
            version: version.to_string(),
            marker: marker.map(str::to_string),
            wheels,
        }
    }

    fn lock_of(packages: Vec<LockedPackage>) -> Pylock {
        Pylock {
            lock_version: "1.0".to_string(),
            requires_python: None,
            extras: Vec::new(),
            packages,
        }
    }

    // ── Step 4: ranking ─────────────────────────────────────────────────────

    #[test]
    fn selects_highest_priority_wheel_from_multi_wheel_package() {
        // A pure `any` wheel and an exact `cp313` manylinux wheel are both
        // compatible; the exact platform match ranks strictly higher.
        let lock = lock_of(vec![package(
            "numpy",
            "2.1.3",
            None,
            vec![
                wheel("numpy-2.1.3-py3-none-any.whl", "aaaa"),
                wheel("numpy-2.1.3-cp313-cp313-manylinux_2_28_x86_64.whl", "bbbb"),
            ],
        )]);

        let selected = select_wheels(&lock, &linux_amd64()).expect("selection succeeds");
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].name, "numpy");
        assert_eq!(
            selected[0].filename,
            "numpy-2.1.3-cp313-cp313-manylinux_2_28_x86_64.whl"
        );
        assert_eq!(selected[0].sha256, "bbbb");
        assert_eq!(
            selected[0].url.as_deref(),
            Some("https://example.test/numpy-2.1.3-cp313-cp313-manylinux_2_28_x86_64.whl")
        );
    }

    // ── Step 2: marker filtering ────────────────────────────────────────────

    #[test]
    fn drops_package_excluded_by_platform_marker() {
        // colorama is Windows-only; on a linux target it must be dropped, not
        // failed — while an unmarked sibling is kept.
        let lock = lock_of(vec![
            package(
                "colorama",
                "0.4.6",
                Some("sys_platform == \"win32\""),
                vec![wheel("colorama-0.4.6-py3-none-any.whl", "cccc")],
            ),
            package(
                "click",
                "8.1.7",
                None,
                vec![wheel("click-8.1.7-py3-none-any.whl", "dddd")],
            ),
        ]);

        let selected = select_wheels(&lock, &linux_amd64()).expect("selection succeeds");
        let names: Vec<&str> = selected.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, vec!["click"], "colorama must be filtered out on linux");
    }

    // ── Step 3/4: abi3 spanning + universal wheels via compat ───────────────

    #[test]
    fn selects_abi3_wheel_spanning_minor_versions() {
        // A cp39-abi3 wheel is compatible with a cp313 interpreter (abi3 spans
        // CPython minors) — resolved by compat, not tag equality.
        let lock = lock_of(vec![package(
            "cryptography",
            "43.0.0",
            None,
            vec![wheel("cryptography-43.0.0-cp39-abi3-manylinux_2_28_x86_64.whl", "eeee")],
        )]);

        let selected = select_wheels(&lock, &linux_amd64()).expect("selection succeeds");
        assert_eq!(selected.len(), 1);
        assert_eq!(
            selected[0].filename,
            "cryptography-43.0.0-cp39-abi3-manylinux_2_28_x86_64.whl"
        );
    }

    #[test]
    fn selects_universal_wheels_via_compat_not_equality() {
        // py2.py3-none-any and pure py3-none-any both match a cp313 linux target
        // through tag-compat semantics (an `any` wheel matches every target).
        let lock = lock_of(vec![
            package(
                "six",
                "1.16.0",
                None,
                vec![wheel("six-1.16.0-py2.py3-none-any.whl", "ffff")],
            ),
            package("idna", "3.10", None, vec![wheel("idna-3.10-py3-none-any.whl", "gggg")]),
        ]);

        let selected = select_wheels(&lock, &linux_amd64()).expect("selection succeeds");
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].filename, "six-1.16.0-py2.py3-none-any.whl");
        assert_eq!(selected[1].filename, "idna-3.10-py3-none-any.whl");
    }

    // ── Step 4: distinguishable no-wheel failures ───────────────────────────

    #[test]
    fn no_wheel_anywhere_reports_empty_available_tags() {
        // uwsgi-shaped: no platform wheel at all → NoCompatibleWheel with an
        // empty available-tags set ("no wheel anywhere").
        let lock = lock_of(vec![package("uwsgi", "2.0.24", None, Vec::new())]);

        let error = select_wheels(&lock, &linux_amd64()).expect_err("no wheels means no selection");
        match error {
            SelectError::NoCompatibleWheel {
                package,
                available_tags,
                ..
            } => {
                assert_eq!(package, "uwsgi");
                assert!(
                    available_tags.is_empty(),
                    "no-wheel-anywhere must carry no available tags, got {available_tags:?}"
                );
            }
            other => panic!("expected NoCompatibleWheel, got {other:?}"),
        }
    }

    #[test]
    fn no_wheel_for_triple_reports_available_non_matching_tags() {
        // psycopg2-shaped: wheels exist for other triples (macOS, Windows) but
        // not linux → NoCompatibleWheel whose available_tags shows those tags,
        // making it distinguishable from the no-wheel-anywhere case.
        let lock = lock_of(vec![package(
            "psycopg2",
            "2.9.9",
            None,
            vec![
                wheel("psycopg2-2.9.9-cp313-cp313-macosx_11_0_arm64.whl", "hhhh"),
                wheel("psycopg2-2.9.9-cp313-cp313-win_amd64.whl", "iiii"),
            ],
        )]);

        let error = select_wheels(&lock, &linux_amd64()).expect_err("no linux wheel means no selection");
        match error {
            SelectError::NoCompatibleWheel {
                package,
                target,
                available_tags,
                ..
            } => {
                assert_eq!(package, "psycopg2");
                assert_eq!(target, "linux/amd64");
                assert!(
                    available_tags.contains(&"macosx_11_0_arm64".to_string()),
                    "available tags must surface the macOS wheel, got {available_tags:?}"
                );
                assert!(
                    available_tags.contains(&"win_amd64".to_string()),
                    "available tags must surface the Windows wheel, got {available_tags:?}"
                );
            }
            other => panic!("expected NoCompatibleWheel, got {other:?}"),
        }
    }

    // ── URL-less rejection ──────────────────────────────────────────────────

    #[test]
    fn rejects_selected_wheel_without_url() {
        let lock = lock_of(vec![package(
            "numpy",
            "2.1.3",
            None,
            vec![LockedWheel {
                filename: "numpy-2.1.3-cp313-cp313-manylinux_2_28_x86_64.whl".to_string(),
                url: None,
                sha256: "bbbb".to_string(),
            }],
        )]);

        let error = select_wheels(&lock, &linux_amd64()).expect_err("a URL-less wheel is not mirrorable");
        match error {
            SelectError::MissingUrl { package, filename } => {
                assert_eq!(package, "numpy");
                assert_eq!(filename, "numpy-2.1.3-cp313-cp313-manylinux_2_28_x86_64.whl");
            }
            other => panic!("expected MissingUrl, got {other:?}"),
        }
    }

    // ── Free-threaded ABI axis ──────────────────────────────────────────────

    #[test]
    fn free_threaded_target_selects_free_threaded_wheel() {
        // A cp313t target must pick the cp313t wheel; the non-free-threaded
        // cp313 wheel is not even tag-compatible with it.
        let lock = lock_of(vec![package(
            "numpy",
            "2.1.3",
            None,
            vec![
                wheel("numpy-2.1.3-cp313-cp313-manylinux_2_28_x86_64.whl", "bbbb"),
                wheel("numpy-2.1.3-cp313-cp313t-manylinux_2_28_x86_64.whl", "tttt"),
            ],
        )]);

        let selected = select_wheels(&lock, &linux_amd64_free_threaded()).expect("selection succeeds");
        assert_eq!(selected.len(), 1);
        assert_eq!(
            selected[0].filename,
            "numpy-2.1.3-cp313-cp313t-manylinux_2_28_x86_64.whl"
        );
        assert_eq!(selected[0].sha256, "tttt");
    }

    // ── Decision B: wheel_priority ranking ───────────────────────────────────

    #[test]
    fn wheel_priority_flips_the_default_tag_priority_order() {
        // Same fixture as `selects_highest_priority_wheel_from_multi_wheel_package`,
        // where the exact manylinux wheel normally outranks the pure `any`
        // wheel — `wheel_priority: ["any"]` must flip that (mandatory for a
        // fully-static interpreter, which cannot dlopen the compiled wheel).
        let lock = lock_of(vec![package(
            "numpy",
            "2.1.3",
            None,
            vec![
                wheel("numpy-2.1.3-py3-none-any.whl", "aaaa"),
                wheel("numpy-2.1.3-cp313-cp313-manylinux_2_28_x86_64.whl", "bbbb"),
            ],
        )]);
        let mut target = linux_amd64();
        target.variant.wheel_priority = Some(vec!["any".to_string()]);

        let selected = select_wheels(&lock, &target).expect("selection succeeds");
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].filename, "numpy-2.1.3-py3-none-any.whl");
        assert_eq!(selected[0].sha256, "aaaa");
    }

    #[test]
    fn wheel_priority_absent_matches_default_tag_priority_order() {
        // Backcompat lock: no `wheel_priority` set is the same fixture as above,
        // asserting the class rank is uniform (0) for every candidate, so the
        // exact manylinux wheel still wins on tag priority alone — unchanged
        // from `selects_highest_priority_wheel_from_multi_wheel_package`.
        let lock = lock_of(vec![package(
            "numpy",
            "2.1.3",
            None,
            vec![
                wheel("numpy-2.1.3-py3-none-any.whl", "aaaa"),
                wheel("numpy-2.1.3-cp313-cp313-manylinux_2_28_x86_64.whl", "bbbb"),
            ],
        )]);
        assert!(linux_amd64().variant.wheel_priority.is_none());

        let selected = select_wheels(&lock, &linux_amd64()).expect("selection succeeds");
        assert_eq!(
            selected[0].filename,
            "numpy-2.1.3-cp313-cp313-manylinux_2_28_x86_64.whl"
        );
    }

    #[test]
    fn wheel_priority_excludes_wheels_matching_no_listed_prefix() {
        // Admissibility: `["any"]` on a package shipping ONLY a compiled
        // manylinux wheel must EXCLUDE it (fail closed: the maintainer's
        // filter admits no binary wheel), not silently fall back to it.
        let lock = lock_of(vec![package(
            "numpy",
            "2.1.3",
            None,
            vec![wheel("numpy-2.1.3-cp313-cp313-manylinux_2_28_x86_64.whl", "bbbb")],
        )]);
        let mut target = linux_amd64();
        target.variant.wheel_priority = Some(vec!["any".to_string()]);

        let error = select_wheels(&lock, &target).expect_err("filter admits no wheel of this package");
        match error {
            SelectError::NoCompatibleWheel {
                package,
                available_tags,
                ..
            } => {
                assert_eq!(package, "numpy");
                assert!(
                    available_tags.contains(&"manylinux_2_28_x86_64".to_string()),
                    "the excluded wheel's tags must stay in the diagnostic set, got {available_tags:?}"
                );
            }
            other => panic!("expected NoCompatibleWheel, got {other:?}"),
        }
    }

    #[test]
    fn wheel_priority_ranks_within_admitted_set() {
        // `["manylinux", "any"]` admits both wheels; the first-listed prefix
        // (manylinux) outranks the pure wheel even though both are admitted.
        let lock = lock_of(vec![package(
            "numpy",
            "2.1.3",
            None,
            vec![
                wheel("numpy-2.1.3-py3-none-any.whl", "aaaa"),
                wheel("numpy-2.1.3-cp313-cp313-manylinux_2_28_x86_64.whl", "bbbb"),
            ],
        )]);
        let mut target = linux_amd64();
        target.variant.wheel_priority = Some(vec!["manylinux".to_string(), "any".to_string()]);

        let selected = select_wheels(&lock, &target).expect("selection succeeds");
        assert_eq!(
            selected[0].filename,
            "numpy-2.1.3-cp313-cp313-manylinux_2_28_x86_64.whl"
        );
    }

    #[test]
    fn wheel_priority_cannot_readmit_a_floor_excluded_wheel() {
        // A gnu/manylinux target ranking `musllinux` first still can't select
        // a musllinux-only package — ranking only reorders wheels that already
        // passed tag-compatibility, it never re-admits a floor-excluded one.
        let lock = lock_of(vec![package(
            "cryptography",
            "43.0.0",
            None,
            vec![wheel(
                "cryptography-43.0.0-cp313-cp313-musllinux_1_2_x86_64.whl",
                "eeee",
            )],
        )]);
        let mut target = linux_amd64();
        target.variant.wheel_priority = Some(vec!["musllinux".to_string()]);

        let error = select_wheels(&lock, &target).expect_err("musllinux wheel is not gnu-compatible");
        assert!(
            matches!(error, SelectError::NoCompatibleWheel { .. }),
            "expected NoCompatibleWheel, got {error:?}"
        );
    }
}
