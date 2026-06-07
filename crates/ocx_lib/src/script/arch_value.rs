// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Typed Starlark wrapper for [`Architecture`].
//!
//! Parallel to [`super::os_value::OsValue`]. Projects the internal Rust enum
//! [`Architecture`] into the `ocx.arch.{Amd64,Arm64}` Starlark namespace. The
//! wrapper is the only shape a `.star` script ever sees; the Rust enum stays
//! internal.
//!
//! `Display` returns the OCI lowercase string (`"amd64"`, `"arm64"`). `equals`
//! compares the inner discriminant; cross-type equality returns `false` without
//! panicking.

use std::fmt;

use allocative::{Allocative, Visitor};
use starlark::any::ProvidesStaticType;
use starlark::values::{
    AllocFrozenValue, AllocValue, FrozenHeap, FrozenValue, Heap, NoSerialize, StarlarkValue, Value, ValueLike,
    starlark_value,
};

use crate::oci::platform::Architecture;

/// Starlark-facing wrapper around an [`Architecture`] variant.
///
/// See [`super::os_value::OsValue`] for the rationale on a manual `Allocative`
/// impl rather than derive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ProvidesStaticType, NoSerialize)]
pub(super) struct ArchValue(pub(super) Architecture);

impl Allocative for ArchValue {
    fn visit<'a, 'b: 'a>(&self, visitor: &'a mut Visitor<'b>) {
        visitor.enter_self_sized::<Self>().exit();
    }
}

impl ArchValue {
    /// Starlark type tag (the result of `type()` in a script).
    ///
    /// Lowercase short form (`"arch"`) — same convention as
    /// [`super::os_value::OsValue::TYPE`].
    pub(super) const TYPE: &'static str = "arch";

    /// PascalCase variant name used as the attribute in the `ocx.arch`
    /// namespace (`Amd64`, `Arm64`). Mirrors the Rust enum variant exactly.
    pub(super) fn starlark_name(self) -> &'static str {
        match self.0 {
            Architecture::Amd64 => "Amd64",
            Architecture::Arm64 => "Arm64",
        }
    }
}

impl fmt::Display for ArchValue {
    /// Lowercase OCI string (`"amd64"`, `"arm64"`) — same as the inner
    /// [`Architecture`]'s `Display`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

#[starlark_value(type = ArchValue::TYPE)]
impl<'v> StarlarkValue<'v> for ArchValue {
    fn equals(&self, other: Value<'v>) -> starlark::Result<bool> {
        Ok(other
            .downcast_ref::<ArchValue>()
            .map(|a| a.0 == self.0)
            .unwrap_or(false))
    }
}

impl AllocFrozenValue for ArchValue {
    fn alloc_frozen_value(self, heap: &FrozenHeap) -> FrozenValue {
        heap.alloc_simple(self)
    }
}

impl<'v> AllocValue<'v> for ArchValue {
    fn alloc_value(self, heap: &'v Heap) -> Value<'v> {
        heap.alloc_simple(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_matches_inner_enum() {
        for variant in Architecture::VARIANTS {
            assert_eq!(ArchValue(*variant).to_string(), variant.to_string());
        }
    }

    #[test]
    fn starlark_name_is_pascal_case() {
        assert_eq!(ArchValue(Architecture::Amd64).starlark_name(), "Amd64");
        assert_eq!(ArchValue(Architecture::Arm64).starlark_name(), "Arm64");
    }

    #[test]
    fn variants_parity_with_rust() {
        for variant in Architecture::VARIANTS {
            let name = ArchValue(*variant).starlark_name();
            assert!(!name.is_empty(), "missing starlark_name for {variant:?}");
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
        let a = heap.alloc(ArchValue(Architecture::Amd64));
        let b = heap.alloc(ArchValue(Architecture::Amd64));
        assert!(a.equals(b).unwrap());
    }

    #[test]
    fn equals_different_variant_is_false() {
        let module = starlark::environment::Module::new();
        let heap = module.heap();
        let a = heap.alloc(ArchValue(Architecture::Amd64));
        let b = heap.alloc(ArchValue(Architecture::Arm64));
        assert!(!a.equals(b).unwrap());
    }
}
