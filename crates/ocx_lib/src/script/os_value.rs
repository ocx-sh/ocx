// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Typed Starlark wrapper for [`OperatingSystem`].
//!
//! Projects the internal Rust enum [`OperatingSystem`] into the
//! `ocx.os.{Linux,Darwin,Windows}` Starlark namespace. The wrapper is the only
//! shape a `.star` script ever sees; the Rust enum stays internal.
//!
//! `Display` returns the OCI lowercase string (`"linux"`, `"darwin"`,
//! `"windows"`) — `str(ocx.os.Linux) == "linux"`. `equals` compares the inner
//! discriminant; cross-type equality (e.g. against an `ArchValue` or a plain
//! string) returns `false` without panicking.

use std::fmt;

use allocative::{Allocative, Visitor};
use starlark::any::ProvidesStaticType;
use starlark::values::{
    AllocFrozenValue, AllocValue, FrozenHeap, FrozenValue, Heap, NoSerialize, StarlarkValue, Value, ValueLike,
    starlark_value,
};

use crate::oci::platform::OperatingSystem;

/// Starlark-facing wrapper around an [`OperatingSystem`] variant.
///
/// `Allocative` is implemented manually (not derived) so the inner
/// [`OperatingSystem`] does not gain a transitive `Allocative` dependency —
/// the wrapper sits inside the Starlark firewall, the enum does not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ProvidesStaticType, NoSerialize)]
pub(super) struct OsValue(pub(super) OperatingSystem);

impl Allocative for OsValue {
    fn visit<'a, 'b: 'a>(&self, visitor: &'a mut Visitor<'b>) {
        visitor.enter_self_sized::<Self>().exit();
    }
}

impl OsValue {
    /// Starlark type tag (the result of `type()` in a script).
    ///
    /// Lowercase short form (`"os"`) matches the namespace name (`ocx.os`)
    /// and the attribute name on `PlatformValue` (`p.os`). Follows the
    /// starlark-rust convention for primitive/enum-like types — `"int"`,
    /// `"bool"`, `"string"`, `"namespace"` are all lowercase common nouns.
    pub(super) const TYPE: &'static str = "os";

    /// PascalCase variant name used as the attribute in the `ocx.os` namespace
    /// (`Linux`, `Darwin`, `Windows`). Mirrors the Rust enum variant exactly.
    pub(super) fn starlark_name(self) -> &'static str {
        match self.0 {
            OperatingSystem::Linux => "Linux",
            OperatingSystem::Darwin => "Darwin",
            OperatingSystem::Windows => "Windows",
        }
    }
}

impl fmt::Display for OsValue {
    /// Lowercase OCI string (`"linux"`, `"darwin"`, `"windows"`) — same as the
    /// inner [`OperatingSystem`]'s `Display`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

#[starlark_value(type = OsValue::TYPE)]
impl<'v> StarlarkValue<'v> for OsValue {
    fn equals(&self, other: Value<'v>) -> starlark::Result<bool> {
        Ok(other.downcast_ref::<OsValue>().map(|o| o.0 == self.0).unwrap_or(false))
    }
}

impl AllocFrozenValue for OsValue {
    fn alloc_frozen_value(self, heap: &FrozenHeap) -> FrozenValue {
        heap.alloc_simple(self)
    }
}

impl<'v> AllocValue<'v> for OsValue {
    fn alloc_value(self, heap: &'v Heap) -> Value<'v> {
        heap.alloc_simple(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::platform::Architecture;
    use crate::script::arch_value::ArchValue;

    #[test]
    fn display_matches_inner_enum() {
        for variant in OperatingSystem::VARIANTS {
            assert_eq!(OsValue(*variant).to_string(), variant.to_string());
        }
    }

    #[test]
    fn starlark_name_is_pascal_case() {
        assert_eq!(OsValue(OperatingSystem::Linux).starlark_name(), "Linux");
        assert_eq!(OsValue(OperatingSystem::Darwin).starlark_name(), "Darwin");
        assert_eq!(OsValue(OperatingSystem::Windows).starlark_name(), "Windows");
    }

    #[test]
    fn variants_parity_with_rust() {
        // Adding a Rust variant without extending starlark_name() must fail
        // here: the match in starlark_name() is total over `OperatingSystem`,
        // so a missing arm is a compile error — but the assertion that every
        // VARIANTS entry yields a non-empty name is the runtime gate.
        for variant in OperatingSystem::VARIANTS {
            let name = OsValue(*variant).starlark_name();
            assert!(!name.is_empty(), "missing starlark_name for {variant:?}");
            // PascalCase: first byte ASCII upper.
            assert!(
                name.as_bytes()[0].is_ascii_uppercase(),
                "starlark_name must be PascalCase, got '{name}'"
            );
        }
    }

    #[test]
    fn equals_same_variant_is_true() {
        let module = starlark::environment::Module::new();
        let heap = module.heap();
        let a = heap.alloc(OsValue(OperatingSystem::Linux));
        let b = heap.alloc(OsValue(OperatingSystem::Linux));
        assert!(a.equals(b).unwrap());
    }

    #[test]
    fn equals_different_variant_is_false() {
        let module = starlark::environment::Module::new();
        let heap = module.heap();
        let a = heap.alloc(OsValue(OperatingSystem::Linux));
        let b = heap.alloc(OsValue(OperatingSystem::Darwin));
        assert!(!a.equals(b).unwrap());
    }

    #[test]
    fn equals_cross_type_with_arch_is_false() {
        // The cross-type wall: `ocx.os.Linux == ocx.arch.Amd64` is `false`,
        // not an error. Same predicate applies vs strings (covered by the
        // acceptance suite).
        let module = starlark::environment::Module::new();
        let heap = module.heap();
        let os = heap.alloc(OsValue(OperatingSystem::Linux));
        let arch = heap.alloc(ArchValue(Architecture::Amd64));
        assert!(!os.equals(arch).unwrap());
    }
}
