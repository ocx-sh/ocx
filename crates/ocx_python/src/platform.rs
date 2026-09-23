// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Platform & axis model: L1 wheel-tag→facts and marker-environment
//! derivation.
//!
//! A Python target is 5-axis — `(os, arch, libc{family,floor}, python, abi)` —
//! but an OCX platform carries only os/arch (+ an optional `libc.*`
//! `os.features` entry, owned by the consumer's platform key):
//!
//! - [`PlatformFacts`] carries the os/arch axes [`marker_environment`]
//!   evaluates markers against; wheel-tag admissibility itself is prefix
//!   filtering over [`VariantConstraints`] in `select`.
//! - The published platform is the consumer's (the mirror's) declared key —
//!   the spec `wheels:` map, including any `+libc.glibc`/`+libc.musl`
//!   `os.features` suffix. This crate never computes it from wheel contents.
//!
//! [`marker_environment`] derives the PEP 508 evaluation environment from the
//! L1 facts and the interpreter pin; `select` feeds it to `uv-pep508`.

/// The operating-system axis of a Python target.
///
/// Mirrors [`ocx_oci::OperatingSystem`]'s supported set; kept as an
/// `ocx_python`-owned enum so the L1 fact table does not depend on OCX's
/// serialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TargetOperatingSystem {
    /// Linux (`manylinux` / `musllinux` wheel tags).
    Linux,
    /// macOS (`macosx` wheel tags).
    Darwin,
    /// Windows (`win_*` wheel tags).
    Windows,
}

/// The CPU-architecture axis of a Python target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TargetArchitecture {
    /// x86-64 (`x86_64` / `amd64` / `AMD64` wheel-tag spellings).
    Amd64,
    /// AArch64 (`aarch64` / `arm64` wheel-tag spellings).
    Arm64,
}

/// A dynamic-link libc family with a versioned floor (Linux only).
///
/// Both families are dynamic-link with per-family floors: PEP 600 glibc ≥ X.Y
/// (`manylinux`) and PEP 656 musl ≥ X.Y (`musllinux` — NOT static musl).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LibcFamily {
    /// glibc (`manylinux` tags).
    Gnu,
    /// musl (`musllinux` tags).
    Musl,
}

/// The Python implementation axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Implementation {
    /// CPython (`cp` ABI tags, `implementation_name == "cpython"`).
    CPython,
}

/// The os/arch facts a marker environment is derived from
/// ([`marker_environment`] reads nothing else).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformFacts {
    /// The operating-system axis.
    pub operating_system: TargetOperatingSystem,
    /// The CPU-architecture axis.
    pub architecture: TargetArchitecture,
}

/// The variant-constraint vocabulary, bounded to L1 fact fields (design spec,
/// "Variant constraint vocabulary").
///
/// A variant is a bounded set of L1-fact constraints, never a free-form tag
/// regex: `default = {libc: gnu, min_manylinux: "2_28"}`,
/// `musl = {libc: musl, min_musllinux: "1_2"}`, `cp313t = {abi: "cp313t"}`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VariantConstraints {
    /// The required libc family (Linux). `None` leaves it unconstrained.
    pub libc: Option<LibcFamily>,
    /// The minimum `manylinux` floor (e.g. `"2_28"`), when `libc` is glibc.
    pub min_manylinux: Option<String>,
    /// The minimum `musllinux` floor (e.g. `"1_2"`), when `libc` is musl.
    pub min_musllinux: Option<String>,
    /// A required ABI override (e.g. `"cp313t"` for free-threaded CPython).
    /// `None` means the interpreter pin's primary ABI.
    pub abi: Option<String>,
    /// Ordered wheel platform-tag-prefix list (e.g. `["any"]`,
    /// `["manylinux", "any"]`). A NON-empty list is an **admissibility
    /// filter plus ranking**: `select`'s `pick_wheel` excludes any
    /// tag-compatible wheel
    /// whose platform tags match no listed prefix, and ranks survivors by the
    /// position of their highest-priority matching prefix (first-listed =
    /// most preferred) before falling back to `uv-platform-tags`' own
    /// `TagPriority`. The filter never re-admits a wheel excluded by the
    /// libc/floor constraints above — it only narrows and reorders wheels
    /// that already passed tag-compatibility. `None`/empty keeps today's
    /// TagPriority-only ordering, unchanged (backcompat; the mirror always
    /// passes a non-empty filter derived from its `wheels:` platform key).
    pub wheel_priority: Option<Vec<String>>,
}

/// The interpreter pin: the `python`/`abi` axes of the target.
///
/// Sourced from the interpreter package in the lock / spec `python:` block.
/// Drives both marker-environment derivation and the ABI-consistency check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterpreterPin {
    /// `python_version` marker value (major.minor, e.g. `"3.13"`).
    pub python_version: String,
    /// `python_full_version` marker value (major.minor.patch, e.g. `"3.13.1"`).
    pub python_full_version: String,
    /// The primary ABI tag (e.g. `"cp313"`, or `"cp313t"` when free-threaded).
    pub abi: String,
    /// The Python implementation.
    pub implementation: Implementation,
}

/// The os/arch "platform key" of a target — what an L3 platform key selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TargetPlatform {
    /// The operating-system axis.
    pub operating_system: TargetOperatingSystem,
    /// The CPU-architecture axis.
    pub architecture: TargetArchitecture,
}

/// A fully specified selection target: one `(variant, platform key)` pair plus
/// the interpreter pin. One `PythonTarget` = one env composition = one
/// selection run.
///
/// Defined here (the platform/axis module) because its fields are all
/// platform-domain types; `select` and `compose` both consume it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonTarget {
    /// The os/arch key selecting the OCX platform.
    pub platform: TargetPlatform,
    /// The variant's L1-fact constraints (libc family, floors, ABI override).
    pub variant: VariantConstraints,
    /// The interpreter pin (python/abi axes).
    pub interpreter: InterpreterPin,
}

impl PythonTarget {
    /// The effective ABI tag for this target: the variant override, else the
    /// interpreter pin's primary ABI.
    ///
    /// Single source of truth for both `select` (wheel ranking/ABI-consistency
    /// check) and `compose` (per-wheel ABI check) — a target whose variant
    /// overrides the ABI (e.g. free-threaded `cp313t`) must be judged by that
    /// override everywhere, not just where the interpreter pin happens to match.
    pub fn effective_abi(&self) -> &str {
        self.variant.abi.as_deref().unwrap_or(self.interpreter.abi.as_str())
    }
}

/// The derived PEP 508 marker environment for evaluating package markers.
///
/// An `ocx_python`-owned struct (not `uv-pep508`'s type) so the derivation
/// table is the stable, versioned contract; `select` converts it into the
/// `uv-pep508` runtime type internally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkerEnvironment {
    /// `python_version` (major.minor).
    pub python_version: String,
    /// `python_full_version` (major.minor.patch).
    pub python_full_version: String,
    /// `sys_platform` (`"linux"` / `"darwin"` / `"win32"`).
    pub sys_platform: String,
    /// `platform_machine` (`"x86_64"` / `"aarch64"` / `"arm64"` / `"AMD64"`).
    pub platform_machine: String,
    /// `platform_system` (`"Linux"` / `"Darwin"` / `"Windows"`).
    pub platform_system: String,
    /// `os_name` (`"posix"` / `"nt"`).
    pub os_name: String,
    /// `implementation_name` (`"cpython"`).
    pub implementation_name: String,
    /// `platform_python_implementation` (`"CPython"`).
    pub platform_python_implementation: String,
}

/// Derives the PEP 508 [`MarkerEnvironment`] for a target from its L1 facts and
/// interpreter pin.
///
/// Pure mapping over the versioned derivation table (design spec, wheel
/// selection algorithm step 1) — infallible.
pub fn marker_environment(facts: &PlatformFacts, interpreter: &InterpreterPin) -> MarkerEnvironment {
    let os = facts.operating_system;
    // `sys_platform` / `platform_system` / `os_name` are pure OS-axis mappings;
    // `os_name` is `posix` for every Unix-like OS (Linux + Darwin), `nt` for
    // Windows.
    let (sys_platform, platform_system, os_name) = match os {
        TargetOperatingSystem::Linux => ("linux", "Linux", "posix"),
        TargetOperatingSystem::Darwin => ("darwin", "Darwin", "posix"),
        TargetOperatingSystem::Windows => ("win32", "Windows", "nt"),
    };
    let (implementation_name, platform_python_implementation) = match interpreter.implementation {
        Implementation::CPython => ("cpython", "CPython"),
    };
    MarkerEnvironment {
        python_version: interpreter.python_version.clone(),
        python_full_version: interpreter.python_full_version.clone(),
        sys_platform: sys_platform.to_string(),
        platform_machine: platform_machine(os, facts.architecture).to_string(),
        platform_system: platform_system.to_string(),
        os_name: os_name.to_string(),
        implementation_name: implementation_name.to_string(),
        platform_python_implementation: platform_python_implementation.to_string(),
    }
}

// ── L1 helpers (frozen table) ───────────────────────────────────────────────

/// The `platform_machine` marker value — OS-dependent: Linux reports
/// `x86_64`/`aarch64`, macOS `x86_64`/`arm64`, Windows `AMD64`/`ARM64`.
fn platform_machine(os: TargetOperatingSystem, arch: TargetArchitecture) -> &'static str {
    match (os, arch) {
        (TargetOperatingSystem::Windows, TargetArchitecture::Amd64) => "AMD64",
        (TargetOperatingSystem::Windows, TargetArchitecture::Arm64) => "ARM64",
        (TargetOperatingSystem::Linux, TargetArchitecture::Amd64)
        | (TargetOperatingSystem::Darwin, TargetArchitecture::Amd64) => "x86_64",
        (TargetOperatingSystem::Linux, TargetArchitecture::Arm64) => "aarch64",
        (TargetOperatingSystem::Darwin, TargetArchitecture::Arm64) => "arm64",
    }
}

/// Errors from platform-tag parsing.
///
/// Internal source type: never surfaced to the consumer directly — always
/// wrapped inside [`SelectError`](crate::select::SelectError) or
/// [`ComposeError`](crate::compose::ComposeError).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PlatformError {
    /// The tag's OS/architecture is outside OCX's supported set.
    #[error("unsupported wheel platform tag '{tag}'")]
    UnsupportedTag {
        /// The offending tag.
        tag: String,
    },
    /// The tag does not parse as a PEP 425/600/656 platform tag.
    #[error("malformed wheel platform tag '{tag}'")]
    MalformedTag {
        /// The offending tag.
        tag: String,
    },
    /// The tag is the platform-agnostic wildcard (`any`) or a Python/ABI-axis
    /// token (`py2.py3`, `abi3`) rather than a platform tag: it carries no
    /// concrete os/arch/libc facts. `select` resolves these by
    /// tag-compatibility semantics (an `any` wheel matches every target,
    /// `abi3` spans CPython minors), never by L1 fact-equality — so L1 fact
    /// parsing reports them here instead of inventing a platform.
    #[error("wheel platform tag '{tag}' carries no concrete platform facts")]
    AgnosticTag {
        /// The agnostic or non-platform-axis tag.
        tag: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Marker environment ──────────────────────────────────────────────────

    fn cpython(version: &str, full: &str, abi: &str) -> InterpreterPin {
        InterpreterPin {
            python_version: version.to_string(),
            python_full_version: full.to_string(),
            abi: abi.to_string(),
            implementation: Implementation::CPython,
        }
    }

    #[test]
    fn marker_env_cpython_312_linux_x86_64() {
        let facts = PlatformFacts {
            operating_system: TargetOperatingSystem::Linux,
            architecture: TargetArchitecture::Amd64,
        };
        let env = marker_environment(&facts, &cpython("3.12", "3.12.1", "cp312"));
        assert_eq!(env.python_version, "3.12");
        assert_eq!(env.python_full_version, "3.12.1");
        assert_eq!(env.sys_platform, "linux");
        assert_eq!(env.platform_machine, "x86_64");
        assert_eq!(env.platform_system, "Linux");
        assert_eq!(env.os_name, "posix");
        assert_eq!(env.implementation_name, "cpython");
        assert_eq!(env.platform_python_implementation, "CPython");
    }

    #[test]
    fn marker_env_platform_machine_is_os_dependent() {
        let cases = [
            (TargetOperatingSystem::Linux, TargetArchitecture::Amd64, "x86_64"),
            (TargetOperatingSystem::Linux, TargetArchitecture::Arm64, "aarch64"),
            (TargetOperatingSystem::Darwin, TargetArchitecture::Amd64, "x86_64"),
            (TargetOperatingSystem::Darwin, TargetArchitecture::Arm64, "arm64"),
            (TargetOperatingSystem::Windows, TargetArchitecture::Amd64, "AMD64"),
            (TargetOperatingSystem::Windows, TargetArchitecture::Arm64, "ARM64"),
        ];
        for (os, arch, expected) in cases {
            let facts = PlatformFacts {
                operating_system: os,
                architecture: arch,
            };
            let env = marker_environment(&facts, &cpython("3.13", "3.13.0", "cp313"));
            assert_eq!(env.platform_machine, expected, "platform_machine for {os:?}/{arch:?}");
        }
    }

    #[test]
    fn marker_env_windows_and_darwin_os_axis() {
        let win = PlatformFacts {
            operating_system: TargetOperatingSystem::Windows,
            architecture: TargetArchitecture::Amd64,
        };
        let env = marker_environment(&win, &cpython("3.12", "3.12.1", "cp312"));
        assert_eq!(
            (
                env.sys_platform.as_str(),
                env.platform_system.as_str(),
                env.os_name.as_str()
            ),
            ("win32", "Windows", "nt")
        );

        let mac = PlatformFacts {
            operating_system: TargetOperatingSystem::Darwin,
            architecture: TargetArchitecture::Arm64,
        };
        let env = marker_environment(&mac, &cpython("3.12", "3.12.1", "cp312"));
        assert_eq!(
            (
                env.sys_platform.as_str(),
                env.platform_system.as_str(),
                env.os_name.as_str()
            ),
            ("darwin", "Darwin", "posix")
        );
    }
}
