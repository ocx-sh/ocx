// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Platform and axis model: wheel-tag facts and marker-environment derivation.
//!
//! The published platform is the consumer's declared key, never computed from wheel contents.

/// The operating-system axis of a Python target.
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

/// The os/arch facts a marker environment is derived from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformFacts {
    /// The operating-system axis.
    pub operating_system: TargetOperatingSystem,
    /// The CPU-architecture axis.
    pub architecture: TargetArchitecture,
}

/// A variant: a bounded set of platform-fact constraints, never a free-form tag regex.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VariantConstraints {
    /// The required libc family (Linux). `None` leaves it unconstrained.
    pub libc: Option<LibcFamily>,
    /// The minimum `manylinux` floor (e.g. `"2_28"`), when `libc` is glibc.
    pub min_manylinux: Option<String>,
    /// The minimum `musllinux` floor (e.g. `"1_2"`), when `libc` is musl.
    pub min_musllinux: Option<String>,
    /// A required ABI override (e.g. `"cp313t"`); `None` means the interpreter pin's primary ABI.
    pub abi: Option<String>,
    /// Ordered platform-tag prefixes (e.g. `["manylinux", "any"]`): when non-empty, wheels matching no prefix are
    /// excluded and survivors rank first-listed-first; it never re-admits a wheel the libc/floor fields excluded.
    pub wheel_priority: Option<Vec<String>>,
}

/// The interpreter pin: the `python`/`abi` axes of the target.
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

/// The os/arch "platform key" of a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TargetPlatform {
    /// The operating-system axis.
    pub operating_system: TargetOperatingSystem,
    /// The CPU-architecture axis.
    pub architecture: TargetArchitecture,
}

/// A fully specified selection target: one `(variant, platform key)` pair plus the interpreter pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonTarget {
    /// The os/arch key selecting the OCX platform.
    pub platform: TargetPlatform,
    /// The variant's fact constraints (libc family, floors, ABI override).
    pub variant: VariantConstraints,
    /// The interpreter pin (python/abi axes).
    pub interpreter: InterpreterPin,
}

impl PythonTarget {
    /// The effective ABI tag: the variant override, else the interpreter pin's primary ABI.
    ///
    /// Every ABI check must use this, or a `cp313t` variant is judged by the pin's `cp313`.
    pub fn effective_abi(&self) -> &str {
        self.variant.abi.as_deref().unwrap_or(self.interpreter.abi.as_str())
    }
}

/// The derived PEP 508 marker environment for evaluating package markers.
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

/// Derives the PEP 508 [`MarkerEnvironment`] for a target from its facts and interpreter pin.
pub fn marker_environment(facts: &PlatformFacts, interpreter: &InterpreterPin) -> MarkerEnvironment {
    let os = facts.operating_system;
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

/// The OS-dependent `platform_machine` marker value.
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

/// Errors from platform-tag parsing, surfaced only wrapped in a `SelectError` or `ComposeError`.
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
    /// The tag is `any` or a Python/ABI-axis token (`py2.py3`, `abi3`) carrying no os/arch/libc facts.
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
