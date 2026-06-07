// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Typed Starlark wrapper for [`Platform`].
//!
//! Exposed to scripts as the `ocx.target_platform` attribute (no parens) with
//! three attributes of its own:
//!
//! - `p.is_any` — `bool`, `true` when the package is platform-agnostic
//!   (`Platform::Any`); `False` for `Platform::Specific`.
//! - `p.os` — [`OsValue`] for `Specific`, `None` for `Any`.
//! - `p.arch` — [`ArchValue`] for `Specific`, `None` for `Any`.
//!
//! Follows the `T | None` + companion `is_*` convention codified in
//! `subsystem-script.md` (no tagged-union types).

use std::fmt;

use allocative::{Allocative, Visitor};
use starlark::any::ProvidesStaticType;
use starlark::values::{
    AllocFrozenValue, AllocValue, FrozenHeap, FrozenValue, Heap, NoSerialize, StarlarkValue, Value, starlark_value,
};

use super::arch_value::ArchValue;
use super::os_value::OsValue;
use crate::oci::Platform;

/// Starlark-facing wrapper around an OCX [`Platform`].
///
/// `Allocative` is implemented manually (see [`super::os_value::OsValue`] for
/// the rationale).
#[derive(Clone, Debug, ProvidesStaticType, NoSerialize)]
pub(super) struct PlatformValue {
    pub(super) is_any: bool,
    pub(super) os: Option<OsValue>,
    pub(super) arch: Option<ArchValue>,
}

impl Allocative for PlatformValue {
    fn visit<'a, 'b: 'a>(&self, visitor: &'a mut Visitor<'b>) {
        visitor.enter_self_sized::<Self>().exit();
    }
}

impl PlatformValue {
    /// Starlark type tag (the result of `type()` in a script).
    pub(super) const TYPE: &'static str = "Platform";

    /// Projects an OCX [`Platform`] into the typed Starlark wrapper. The
    /// optional CPU `variant` / `os_version` / feature lists are not exposed
    /// in v1 — they exist on the OCX `Platform` for OCI manifest fidelity, but
    /// a test script that reads `p.os` / `p.arch` is the v1 contract.
    pub(super) fn from_platform(p: &Platform) -> Self {
        match p {
            Platform::Any => Self {
                is_any: true,
                os: None,
                arch: None,
            },
            Platform::Specific { os, arch, .. } => Self {
                is_any: false,
                os: Some(OsValue(*os)),
                arch: Some(ArchValue(*arch)),
            },
        }
    }
}

impl fmt::Display for PlatformValue {
    /// Mirrors [`Platform`]'s `Display`: `"any"` for the sentinel,
    /// `"os/arch"` for the populated form.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.is_any, self.os, self.arch) {
            (true, _, _) => write!(f, "any"),
            (false, Some(os), Some(arch)) => write!(f, "{os}/{arch}"),
            // Defensive: a non-any PlatformValue without os/arch is unreachable
            // (from_platform never builds it), but render something legible
            // rather than panic in a Display impl.
            (false, _, _) => write!(f, "any"),
        }
    }
}

#[starlark_value(type = PlatformValue::TYPE)]
impl<'v> StarlarkValue<'v> for PlatformValue {
    fn get_attr(&self, attribute: &str, heap: &'v Heap) -> Option<Value<'v>> {
        match attribute {
            "is_any" => Some(Value::new_bool(self.is_any)),
            "os" => Some(match self.os {
                Some(v) => heap.alloc(v),
                None => Value::new_none(),
            }),
            "arch" => Some(match self.arch {
                Some(v) => heap.alloc(v),
                None => Value::new_none(),
            }),
            _ => None,
        }
    }

    fn dir_attr(&self) -> Vec<String> {
        vec!["is_any".to_string(), "os".to_string(), "arch".to_string()]
    }
}

impl<'v> AllocValue<'v> for PlatformValue {
    /// Allocates via `alloc_simple`: the value holds no heap pointers
    /// (`Option<OsValue>` / `Option<ArchValue>` are plain Rust data with no
    /// borrowed `Value`s), so the simple path is correct — no `Trace` impl
    /// needed.
    fn alloc_value(self, heap: &'v Heap) -> Value<'v> {
        heap.alloc_simple(self)
    }
}

impl AllocFrozenValue for PlatformValue {
    /// Required so the per-run `ocx.target_platform` attribute can be set
    /// as a frozen value in the `ocx` namespace during globals build.
    fn alloc_frozen_value(self, heap: &FrozenHeap) -> FrozenValue {
        heap.alloc_simple(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::platform::{Architecture, OperatingSystem};
    use starlark::values::ValueLike;

    #[test]
    fn from_platform_any_has_no_os_or_arch() {
        let p = PlatformValue::from_platform(&Platform::Any);
        assert!(p.is_any);
        assert!(p.os.is_none());
        assert!(p.arch.is_none());
    }

    #[test]
    fn from_platform_specific_populates_os_and_arch() {
        let platform = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: None,
            os_version: None,
            os_features: None,
            features: None,
        };
        let p = PlatformValue::from_platform(&platform);
        assert!(!p.is_any);
        assert_eq!(p.os, Some(OsValue(OperatingSystem::Linux)));
        assert_eq!(p.arch, Some(ArchValue(Architecture::Amd64)));
    }

    #[test]
    fn get_attr_returns_typed_values_for_specific() {
        let module = starlark::environment::Module::new();
        let heap = module.heap();
        let platform = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: None,
            os_version: None,
            os_features: None,
            features: None,
        };
        let value = heap.alloc(PlatformValue::from_platform(&platform));
        let is_any = value.get_attr("is_any", heap).unwrap().unwrap();
        assert_eq!(is_any.unpack_bool(), Some(false));
        let os_attr = value.get_attr("os", heap).unwrap().unwrap();
        let os_inner = os_attr.downcast_ref::<OsValue>().expect("p.os is OsValue");
        assert_eq!(os_inner.0, OperatingSystem::Linux);
        let arch_attr = value.get_attr("arch", heap).unwrap().unwrap();
        let arch_inner = arch_attr.downcast_ref::<ArchValue>().expect("p.arch is ArchValue");
        assert_eq!(arch_inner.0, Architecture::Amd64);
    }

    #[test]
    fn get_attr_returns_none_for_any_variant() {
        let module = starlark::environment::Module::new();
        let heap = module.heap();
        let value = heap.alloc(PlatformValue::from_platform(&Platform::Any));
        let is_any = value.get_attr("is_any", heap).unwrap().unwrap();
        assert_eq!(is_any.unpack_bool(), Some(true));
        let os_attr = value.get_attr("os", heap).unwrap().unwrap();
        assert!(os_attr.is_none());
        let arch_attr = value.get_attr("arch", heap).unwrap().unwrap();
        assert!(arch_attr.is_none());
    }

    #[test]
    fn display_matches_platform() {
        let platform = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: None,
            os_version: None,
            os_features: None,
            features: None,
        };
        assert_eq!(PlatformValue::from_platform(&platform).to_string(), "linux/amd64");
        assert_eq!(PlatformValue::from_platform(&Platform::Any).to_string(), "any");
    }
}
