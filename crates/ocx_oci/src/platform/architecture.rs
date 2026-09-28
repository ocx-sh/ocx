// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! CPU architecture component of an OCI platform specification.
//!
//! A new variant also needs `FromStr`, [`VARIANTS`](Architecture::VARIANTS) and
//! [`SUPPORTED_PAIRS`](super::SUPPORTED_PAIRS) entries (no match forces them), and
//! goes last because [`Ord`] follows declaration order (`variants_is_sorted`).

use serde::{Deserialize, Serialize};

use super::error::PlatformErrorKind;
use crate::native;

/// Supported CPU architectures for OCX packages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Architecture {
    Amd64,
    Arm64,
    /// 32-bit WebAssembly: paired only with a `wasip*` OS, never reported by [`current`](Self::current).
    Wasm,
    // Unsupported upstream oci_spec::image::Arch values (Go GOARCH), listed at
    // adr_platform_model_unification.md § Rationale from code: ocx_oci
}

impl Architecture {
    /// All supported variants, in error-message order.
    pub const VARIANTS: &[Self] = &[Self::Amd64, Self::Arm64, Self::Wasm];

    /// The host's CPU architecture, or `None` when it is not supported.
    pub fn current() -> Option<Self> {
        match std::env::consts::ARCH {
            "x86_64" => Some(Self::Amd64),
            "aarch64" => Some(Self::Arm64),
            _ => None,
        }
    }
}

impl std::fmt::Display for Architecture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Amd64 => write!(f, "amd64"),
            Self::Arm64 => write!(f, "arm64"),
            Self::Wasm => write!(f, "wasm"),
        }
    }
}

impl std::str::FromStr for Architecture {
    type Err = PlatformErrorKind;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "amd64" => Ok(Self::Amd64),
            "arm64" => Ok(Self::Arm64),
            "wasm" => Ok(Self::Wasm),
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

impl From<Architecture> for native::Arch {
    fn from(arch: Architecture) -> Self {
        match arch {
            Architecture::Amd64 => native::Arch::Amd64,
            Architecture::Arm64 => native::Arch::ARM64,
            Architecture::Wasm => native::Arch::Wasm,
        }
    }
}

impl TryFrom<native::Arch> for Architecture {
    type Error = PlatformErrorKind;

    fn try_from(arch: native::Arch) -> Result<Self, Self::Error> {
        match arch {
            native::Arch::Amd64 => Ok(Self::Amd64),
            native::Arch::ARM64 => Ok(Self::Arm64),
            native::Arch::Wasm => Ok(Self::Wasm),
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
        assert_eq!(Architecture::Wasm.to_string(), "wasm");
    }

    #[test]
    fn fromstr_valid() {
        assert_eq!("amd64".parse::<Architecture>().unwrap(), Architecture::Amd64);
        assert_eq!("arm64".parse::<Architecture>().unwrap(), Architecture::Arm64);
        assert_eq!("wasm".parse::<Architecture>().unwrap(), Architecture::Wasm);
    }

    #[test]
    fn current_never_reports_wasm() {
        // No host executes wasm natively; `Wasm` exists purely as a
        // distribution label, so host detection must never produce it.
        assert_ne!(Architecture::current(), Some(Architecture::Wasm));
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
