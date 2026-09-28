// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Pure translation library: PEP 751 `pylock.toml` in, OCX package compositions out.
//!
//! Every writer of the shared registry namespace must use this crate so their artifacts stay byte-compatible; see the
//! [design spec](https://github.com/ocx-sh/ocx-mirror/blob/main/.claude/artifacts/design_spec_ocx_python.md).
//! No registry I/O: output is repo-relative identifiers plus metadata, and the consumer builds the
//! [`ocx_oci::PackageRef`].
//!
//! ```text
//! parse_pylock ─▶ select_wheels ─▶ repack_wheel ─▶ check_collisions ─▶ compose_env
//!    (lock)         (select)          (repack)         (collide)          (compose)
//! ```

pub mod collide;
pub mod compose;
pub mod error;
pub mod lock;
pub mod naming;
pub mod platform;
pub mod repack;
pub mod select;

// Re-exported so `ocx-mirror` never pins its own astral-sh/uv git rev.
pub use uv_pep440;

// Re-exported so `ocx-mirror` parses PyPI filenames with the same pinned grammar `select` uses.
pub use uv_distribution_filename;

pub use collide::{CollisionError, check_collisions};
pub use compose::{ComposeError, EntrypointSelection, EnvComposition, EnvSpec, WheelLayer, compose_env};
pub use lock::{LockError, LockedPackage, LockedWheel, Pylock, parse_pylock};
pub use naming::{WheelReference, WheelScope, normalize_package_name, wheel_reference};
pub use platform::{
    Implementation, InterpreterPin, LibcFamily, MarkerEnvironment, PlatformError, PlatformFacts, PythonTarget,
    TargetArchitecture, TargetOperatingSystem, TargetPlatform, VariantConstraints, marker_environment,
};
pub use repack::{
    ConsoleScript, REPACK_VERSION, RepackError, RepackedWheel, WheelDescription, read_wheel_description, repack_wheel,
};
pub use select::{SelectError, WheelRef, select_wheels};
