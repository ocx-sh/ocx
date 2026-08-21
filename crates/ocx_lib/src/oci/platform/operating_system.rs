// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Operating system component of an OCI platform specification.
//!
//! This enum is a closed subset of the operating systems defined by the
//! [OCI Image Index specification](https://github.com/opencontainers/image-spec/blob/main/image-index.md),
//! which in turn mirrors the values from Go's `GOOS`.
//!
//! The full list of valid values is maintained upstream in the `oci-spec` crate
//! (`oci_spec::image::Os`). OCX intentionally restricts this to the operating
//! systems we actively support and test. Unsupported values are rejected at
//! parse time rather than silently accepted via an `Other(String)` fallback.
//!
//! To add a new operating system: add a variant here, update [`Display`],
//! [`FromStr`], [`VARIANTS`](OperatingSystem::VARIANTS), and both `From`/`TryFrom`
//! impls for `native::Os`. Append the variant **last** — [`Ord`] derives from
//! declaration order and `variants_is_sorted` pins it.
//!
//! A value upstream's `native::Os` does not name travels as `Os::Other(..)`;
//! its `TryFrom` arm must precede the generic `Other` reject, and the pairing
//! it is legal in belongs in [`SUPPORTED_PAIRS`](super::SUPPORTED_PAIRS).

use serde::{Deserialize, Serialize};

use super::error::PlatformErrorKind;
use crate::oci::native;

// The upstream `oci_spec::image::Os` enum has no WASI variants, so both
// spellings travel the wire as `Os::Other`. Naming them once keeps the
// `Display`, `FromStr` and both native conversions reading from one place —
// a typo in any single arm would otherwise break the round-trip silently.
const WASIP1_STR: &str = "wasip1";
const WASIP2_STR: &str = "wasip2";

/// Supported operating systems for OCX packages.
///
/// Translates bidirectionally to [`native::Os`] (`oci_spec::image::Os`) at the
/// OCI transport boundary. Only the subset that OCX supports is represented;
/// unsupported values from the OCI spec are listed as commented-out variants
/// for reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum OperatingSystem {
    // --- Supported ---
    Darwin,
    Linux,
    Windows,
    /// WASI Preview 1 — a WebAssembly module targeting the `wasip1` ABI.
    Wasip1,
    /// WASI Preview 2 — a WebAssembly component targeting the `wasip2` ABI.
    Wasip2,
    // --- Unsupported (upstream oci_spec::image::Os values from Go GOOS) ---
    // AIX,
    // Android,
    // DragonFlyBSD,
    // FreeBSD,
    // Hurd,
    // Illumos,
    // iOS,
    // Js,
    // Nacl,
    // NetBSD,
    // OpenBSD,
    // Plan9,
    // Solaris,
    // zOS,
}

impl OperatingSystem {
    /// All supported variants, in the order used for error messages.
    pub const VARIANTS: &[Self] = &[Self::Darwin, Self::Linux, Self::Windows, Self::Wasip1, Self::Wasip2];

    /// Detects the operating system of the current host.
    ///
    /// Maps Rust's [`std::env::consts::OS`] to the corresponding OCI value.
    /// Returns `None` if the host OS is not in [`VARIANTS`](Self::VARIANTS).
    pub fn current() -> Option<Self> {
        match std::env::consts::OS {
            "linux" => Some(Self::Linux),
            "macos" => Some(Self::Darwin),
            "windows" => Some(Self::Windows),
            _ => None,
        }
    }
}

impl std::fmt::Display for OperatingSystem {
    /// Formats as the lowercase OCI string value (e.g. `"linux"`, `"darwin"`).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Linux => write!(f, "linux"),
            Self::Darwin => write!(f, "darwin"),
            Self::Windows => write!(f, "windows"),
            Self::Wasip1 => write!(f, "{}", WASIP1_STR),
            Self::Wasip2 => write!(f, "{}", WASIP2_STR),
        }
    }
}

impl std::str::FromStr for OperatingSystem {
    type Err = PlatformErrorKind;

    /// Parses from the lowercase OCI string value. Case-sensitive.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "linux" => Ok(Self::Linux),
            "darwin" => Ok(Self::Darwin),
            "windows" => Ok(Self::Windows),
            WASIP1_STR => Ok(Self::Wasip1),
            WASIP2_STR => Ok(Self::Wasip2),
            _ => Err(PlatformErrorKind::UnsupportedOs { os: s.to_string() }),
        }
    }
}

impl Serialize for OperatingSystem {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for OperatingSystem {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse()
            .map_err(|kind: PlatformErrorKind| serde::de::Error::custom(kind))
    }
}

/// Converts to the upstream `native::Os` for OCI transport operations.
impl From<OperatingSystem> for native::Os {
    fn from(os: OperatingSystem) -> Self {
        match os {
            OperatingSystem::Linux => native::Os::Linux,
            OperatingSystem::Darwin => native::Os::Darwin,
            OperatingSystem::Windows => native::Os::Windows,
            OperatingSystem::Wasip1 => native::Os::Other(WASIP1_STR.to_string()),
            OperatingSystem::Wasip2 => native::Os::Other(WASIP2_STR.to_string()),
        }
    }
}

/// Converts from the upstream `native::Os`, rejecting unsupported values.
impl TryFrom<native::Os> for OperatingSystem {
    type Error = PlatformErrorKind;

    fn try_from(os: native::Os) -> Result<Self, Self::Error> {
        match os {
            native::Os::Linux => Ok(Self::Linux),
            native::Os::Darwin => Ok(Self::Darwin),
            native::Os::Windows => Ok(Self::Windows),
            // Must precede the generic `Other` reject below, or the two WASI
            // spellings would round-trip out as unsupported.
            native::Os::Other(ref os) if os == WASIP1_STR => Ok(Self::Wasip1),
            native::Os::Other(ref os) if os == WASIP2_STR => Ok(Self::Wasip2),
            other => Err(PlatformErrorKind::UnsupportedOs { os: other.to_string() }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_fromstr_roundtrip() {
        for variant in OperatingSystem::VARIANTS {
            let s = variant.to_string();
            let parsed: OperatingSystem = s.parse().unwrap();
            assert_eq!(*variant, parsed);
        }
    }

    #[test]
    fn display_values() {
        assert_eq!(OperatingSystem::Linux.to_string(), "linux");
        assert_eq!(OperatingSystem::Darwin.to_string(), "darwin");
        assert_eq!(OperatingSystem::Windows.to_string(), "windows");
        assert_eq!(OperatingSystem::Wasip1.to_string(), "wasip1");
        assert_eq!(OperatingSystem::Wasip2.to_string(), "wasip2");
    }

    #[test]
    fn fromstr_valid() {
        assert_eq!("linux".parse::<OperatingSystem>().unwrap(), OperatingSystem::Linux);
        assert_eq!("darwin".parse::<OperatingSystem>().unwrap(), OperatingSystem::Darwin);
        assert_eq!("windows".parse::<OperatingSystem>().unwrap(), OperatingSystem::Windows);
        assert_eq!("wasip1".parse::<OperatingSystem>().unwrap(), OperatingSystem::Wasip1);
        assert_eq!("wasip2".parse::<OperatingSystem>().unwrap(), OperatingSystem::Wasip2);
    }

    #[test]
    fn current_never_reports_wasi() {
        // A WASI target is a distribution label, never a host OCX runs on;
        // host detection must never produce one.
        let current = OperatingSystem::current();
        assert_ne!(current, Some(OperatingSystem::Wasip1));
        assert_ne!(current, Some(OperatingSystem::Wasip2));
    }

    #[test]
    fn native_wasi_roundtrips_through_other() {
        // Upstream has no WASI variant, so both spellings travel as
        // `Os::Other`. Assert the exact payload, not just the roundtrip: the
        // serialized manifest field is that bare string.
        for (os, expected) in [(OperatingSystem::Wasip1, "wasip1"), (OperatingSystem::Wasip2, "wasip2")] {
            let native_os: native::Os = os.into();
            assert_eq!(native_os, native::Os::Other(expected.to_string()));
            assert_eq!(OperatingSystem::try_from(native_os).unwrap(), os);
        }
    }

    #[test]
    fn native_other_still_rejected_for_unknown_values() {
        // The two WASI arms sit before the generic `Other` reject; every
        // other `Other` payload must still fail.
        assert!(OperatingSystem::try_from(native::Os::Other("wasip3".to_string())).is_err());
        assert!(OperatingSystem::try_from(native::Os::Other("wasi".to_string())).is_err());
        assert!(OperatingSystem::try_from(native::Os::Other("WASIP1".to_string())).is_err());
    }

    #[test]
    fn serde_wasi_serializes_as_bare_string() {
        // The OS field on the wire is the bare spelling — no `Other` wrapper
        // leaks into the manifest JSON.
        assert_eq!(serde_json::to_string(&OperatingSystem::Wasip1).unwrap(), "\"wasip1\"");
        assert_eq!(serde_json::to_string(&OperatingSystem::Wasip2).unwrap(), "\"wasip2\"");
    }

    #[test]
    fn fromstr_rejects_typos() {
        assert!("linus".parse::<OperatingSystem>().is_err());
        assert!("osx".parse::<OperatingSystem>().is_err());
        assert!("win".parse::<OperatingSystem>().is_err());
    }

    #[test]
    fn fromstr_is_case_sensitive() {
        assert!("Linux".parse::<OperatingSystem>().is_err());
        assert!("LINUX".parse::<OperatingSystem>().is_err());
        assert!("Darwin".parse::<OperatingSystem>().is_err());
        assert!("WINDOWS".parse::<OperatingSystem>().is_err());
    }

    #[test]
    fn fromstr_rejects_unsupported_oci_values() {
        assert!("freebsd".parse::<OperatingSystem>().is_err());
        assert!("android".parse::<OperatingSystem>().is_err());
        assert!("aix".parse::<OperatingSystem>().is_err());
    }

    #[test]
    fn fromstr_error_lists_valid_values() {
        let err = "freebsd".parse::<OperatingSystem>().unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("linux"), "error should list valid values: {msg}");
        assert!(msg.contains("darwin"), "error should list valid values: {msg}");
        assert!(msg.contains("windows"), "error should list valid values: {msg}");
    }

    #[test]
    fn native_roundtrip() {
        for variant in OperatingSystem::VARIANTS {
            let native_os: native::Os = (*variant).into();
            let back = OperatingSystem::try_from(native_os).unwrap();
            assert_eq!(*variant, back);
        }
    }

    #[test]
    fn native_rejects_unsupported() {
        let unsupported = [
            native::Os::FreeBSD,
            native::Os::Android,
            native::Os::AIX,
            native::Os::NetBSD,
            native::Os::OpenBSD,
            native::Os::Solaris,
        ];
        for os in unsupported {
            assert!(OperatingSystem::try_from(os).is_err());
        }
    }

    #[test]
    fn native_rejects_other() {
        let os = native::Os::Other("custom".to_string());
        assert!(OperatingSystem::try_from(os).is_err());
    }

    #[test]
    fn serde_roundtrip() {
        for variant in OperatingSystem::VARIANTS {
            let json = serde_json::to_string(variant).unwrap();
            let parsed: OperatingSystem = serde_json::from_str(&json).unwrap();
            assert_eq!(*variant, parsed);
        }
    }

    #[test]
    fn serde_serializes_as_lowercase_string() {
        assert_eq!(serde_json::to_string(&OperatingSystem::Linux).unwrap(), "\"linux\"");
        assert_eq!(serde_json::to_string(&OperatingSystem::Darwin).unwrap(), "\"darwin\"");
        assert_eq!(serde_json::to_string(&OperatingSystem::Windows).unwrap(), "\"windows\"");
    }

    #[test]
    fn current_returns_supported_os() {
        let current = OperatingSystem::current();
        assert!(
            current.is_some(),
            "current() should detect a supported OS on dev machines"
        );
        assert!(OperatingSystem::VARIANTS.contains(&current.unwrap()));
    }

    #[test]
    fn variants_is_sorted() {
        let variants = OperatingSystem::VARIANTS;
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
