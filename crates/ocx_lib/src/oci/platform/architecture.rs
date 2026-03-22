// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! CPU architecture component of an OCI platform specification.
//!
//! This enum is a closed subset of the architectures defined by the
//! [OCI Image Index specification](https://github.com/opencontainers/image-spec/blob/main/image-index.md),
//! which in turn mirrors the values from Go's `GOARCH`.
//!
//! The full list of valid values is maintained upstream in the `oci-spec` crate
//! (`oci_spec::image::Arch`). OCX intentionally restricts this to the
//! architectures we actively support and test. Unsupported values are rejected
//! at parse time rather than silently accepted via an `Other(String)` fallback.
//!
//! To add a new architecture: add a variant here, update [`Display`],
//! [`FromStr`], [`VARIANTS`](Architecture::VARIANTS), and both `From`/`TryFrom`
//! impls for `native::Arch`.

use serde::{Deserialize, Serialize};

use super::error::PlatformErrorKind;
use crate::oci::native;

/// Supported CPU architectures for OCX packages.
///
/// Translates bidirectionally to [`native::Arch`] (`oci_spec::image::Arch`) at
/// the OCI transport boundary. Only the subset that OCX supports is
/// represented; unsupported values from the OCI spec are listed as
/// commented-out variants for reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Architecture {
    // --- Supported ---
    Amd64,
    Arm64,
    // --- Unsupported (upstream oci_spec::image::Arch values from Go GOARCH) ---
    // i386,          // 32 bit x86, little-endian
    // Amd64p32,      // 64 bit x86 with 32 bit pointers, little-endian
    // ARM,           // 32 bit ARM, little-endian
    // ARMbe,         // 32 bit ARM, big-endian
    // ARM64be,       // 64 bit ARM, big-endian
    // LoongArch64,   // 64 bit Loongson RISC CPU, little-endian
    // Mips,          // 32 bit Mips, big-endian
    // Mipsle,        // 32 bit Mips, little-endian
    // Mips64,        // 64 bit Mips, big-endian
    // Mips64le,      // 64 bit Mips, little-endian
    // Mips64p32,     // 64 bit Mips with 32 bit pointers, big-endian
    // Mips64p32le,   // 64 bit Mips with 32 bit pointers, little-endian
    // PowerPC,       // 32 bit PowerPC, big endian
    // PowerPC64,     // 64 bit PowerPC, big-endian
    // PowerPC64le,   // 64 bit PowerPC, little-endian
    // RISCV,         // 32 bit RISC-V, little-endian
    // RISCV64,       // 64 bit RISC-V, little-endian
    // s390,          // 32 bit IBM System/390, big-endian
    // s390x,         // 64 bit IBM System/390, big-endian
    // SPARC,         // 32 bit SPARC, big-endian
    // SPARC64,       // 64 bit SPARC, bi-endian
    // Wasm,          // 32 bit Web Assembly
}

impl Architecture {
    /// All supported variants, in the order used for error messages.
    pub const VARIANTS: &[Self] = &[Self::Amd64, Self::Arm64];

    /// Detects the CPU architecture of the current host.
    ///
    /// Maps Rust's [`std::env::consts::ARCH`] to the corresponding OCI value.
    /// Returns `None` if the host architecture is not in [`VARIANTS`](Self::VARIANTS).
    pub fn current() -> Option<Self> {
        match std::env::consts::ARCH {
            "x86_64" => Some(Self::Amd64),
            "aarch64" => Some(Self::Arm64),
            _ => None,
        }
    }
}

impl std::fmt::Display for Architecture {
    /// Formats as the lowercase OCI string value (e.g. `"amd64"`, `"arm64"`).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Amd64 => write!(f, "amd64"),
            Self::Arm64 => write!(f, "arm64"),
        }
    }
}

impl std::str::FromStr for Architecture {
    type Err = PlatformErrorKind;

    /// Parses from the lowercase OCI string value. Case-sensitive.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "amd64" => Ok(Self::Amd64),
            "arm64" => Ok(Self::Arm64),
            _ => Err(PlatformErrorKind::UnsupportedArch { arch: s.to_string() }),
        }
    }
}

impl Serialize for Architecture {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Architecture {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse()
            .map_err(|kind: PlatformErrorKind| serde::de::Error::custom(kind))
    }
}

/// Converts to the upstream `native::Arch` for OCI transport operations.
impl From<Architecture> for native::Arch {
    fn from(arch: Architecture) -> Self {
        match arch {
            Architecture::Amd64 => native::Arch::Amd64,
            Architecture::Arm64 => native::Arch::ARM64,
        }
    }
}

/// Converts from the upstream `native::Arch`, rejecting unsupported values.
impl TryFrom<native::Arch> for Architecture {
    type Error = PlatformErrorKind;

    fn try_from(arch: native::Arch) -> Result<Self, Self::Error> {
        match arch {
            native::Arch::Amd64 => Ok(Self::Amd64),
            native::Arch::ARM64 => Ok(Self::Arm64),
            other => Err(PlatformErrorKind::UnsupportedArch {
                arch: other.to_string(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_fromstr_roundtrip() {
        for variant in Architecture::VARIANTS {
            let s = variant.to_string();
            let parsed: Architecture = s.parse().unwrap();
            assert_eq!(*variant, parsed);
        }
    }

    #[test]
    fn display_values() {
        assert_eq!(Architecture::Amd64.to_string(), "amd64");
        assert_eq!(Architecture::Arm64.to_string(), "arm64");
    }

    #[test]
    fn fromstr_valid() {
        assert_eq!("amd64".parse::<Architecture>().unwrap(), Architecture::Amd64);
        assert_eq!("arm64".parse::<Architecture>().unwrap(), Architecture::Arm64);
    }

    #[test]
    fn fromstr_rejects_rust_arch_names() {
        // Rust uses different names than OCI/Go
        assert!("x86_64".parse::<Architecture>().is_err());
        assert!("aarch64".parse::<Architecture>().is_err());
    }

    #[test]
    fn fromstr_is_case_sensitive() {
        assert!("AMD64".parse::<Architecture>().is_err());
        assert!("Amd64".parse::<Architecture>().is_err());
        assert!("ARM64".parse::<Architecture>().is_err());
        assert!("Arm64".parse::<Architecture>().is_err());
    }

    #[test]
    fn fromstr_rejects_unsupported_oci_values() {
        assert!("386".parse::<Architecture>().is_err());
        assert!("arm".parse::<Architecture>().is_err());
        assert!("ppc64le".parse::<Architecture>().is_err());
        assert!("s390x".parse::<Architecture>().is_err());
        assert!("riscv64".parse::<Architecture>().is_err());
        assert!("wasm".parse::<Architecture>().is_err());
    }

    #[test]
    fn fromstr_error_lists_valid_values() {
        let err = "arm".parse::<Architecture>().unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("amd64"), "error should list valid values: {msg}");
        assert!(msg.contains("arm64"), "error should list valid values: {msg}");
    }

    #[test]
    fn native_roundtrip() {
        for variant in Architecture::VARIANTS {
            let native_arch: native::Arch = (*variant).into();
            let back = Architecture::try_from(native_arch).unwrap();
            assert_eq!(*variant, back);
        }
    }

    #[test]
    fn native_rejects_unsupported() {
        let unsupported = [
            native::Arch::ARM,
            native::Arch::i386,
            native::Arch::PowerPC64,
            native::Arch::s390x,
            native::Arch::RISCV64,
            native::Arch::Wasm,
        ];
        for arch in unsupported {
            assert!(Architecture::try_from(arch).is_err());
        }
    }

    #[test]
    fn native_rejects_other() {
        let arch = native::Arch::Other("custom".to_string());
        assert!(Architecture::try_from(arch).is_err());
    }

    #[test]
    fn serde_roundtrip() {
        for variant in Architecture::VARIANTS {
            let json = serde_json::to_string(variant).unwrap();
            let parsed: Architecture = serde_json::from_str(&json).unwrap();
            assert_eq!(*variant, parsed);
        }
    }

    #[test]
    fn serde_serializes_as_lowercase_string() {
        assert_eq!(serde_json::to_string(&Architecture::Amd64).unwrap(), "\"amd64\"");
        assert_eq!(serde_json::to_string(&Architecture::Arm64).unwrap(), "\"arm64\"");
    }

    #[test]
    fn current_returns_supported_arch() {
        let current = Architecture::current();
        assert!(
            current.is_some(),
            "current() should detect a supported arch on dev machines"
        );
        assert!(Architecture::VARIANTS.contains(&current.unwrap()));
    }

    #[test]
    fn variants_is_sorted() {
        let variants = Architecture::VARIANTS;
        for window in variants.windows(2) {
            assert!(
                window[0] <= window[1],
                "VARIANTS should be sorted: {:?} > {:?}",
                window[0],
                window[1]
            );
        }
    }
}
