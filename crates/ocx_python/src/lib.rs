// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Pure translation library: PEP 751 `pylock.toml` in → OCX package
//! compositions out.
//!
//! `ocx_python` encodes the cross-repo Python-on-OCX conventions (wheel
//! naming, repack determinism, layer layout, entrypoint synthesis,
//! platform/axis encoding) exactly once, so that every writer of the shared
//! registry namespace — `ocx-mirror` today, `ocx-dist` later — produces
//! byte-compatible artifacts. See the
//! [design spec](https://github.com/ocx-sh/ocx-mirror/blob/main/.claude/artifacts/design_spec_ocx_python.md).
//!
//! # Boundary
//!
//! This crate performs **no registry I/O**: no `Publisher`, no HTTP download,
//! no registry existence checks. Filesystem I/O (reading wheels, writing
//! repacked layers) is in scope; everything network-facing is the consumer's
//! responsibility (e.g. the mirror's `pipeline/download.rs`). It also stays
//! **target-agnostic**: it emits repo-relative identifiers and OCX metadata
//! but never knows a concrete registry host — the consumer supplies the
//! registry and assembles the final [`ocx_oci::Identifier`].
//!
//! # Pipeline
//!
//! ```text
//! parse_pylock ─▶ select_wheels ─▶ repack_wheel ─▶ check_collisions ─▶ compose_env
//!    (lock)         (select)          (repack)         (collide)          (compose)
//! ```
//!
//! `wheel_reference` (naming) renders the conventional repo path for each
//! selected wheel; `platform` holds the L1 wheel-tag→facts model that
//! `select` and `compose` share.

pub mod collide;
pub mod compose;
pub mod error;
pub mod lock;
pub mod naming;
pub mod platform;
pub mod repack;
pub mod select;

// ── Public entry points (re-exported at the crate root for ergonomics) ──────

// PEP 440 version parsing, re-exported so `ocx-mirror` never declares its own
// copy of the astral-sh/uv git pin — one manifest (ocx's workspace root) owns
// the rev for all four uv-* crates.
pub use uv_pep440;

// PEP 503/691 file listings name a version only inside a wheel or sdist
// filename; `ocx-mirror`'s pypi discovery parses them with the same crate
// `select` already uses for wheel tags, so one pin owns filename grammar
// for the whole repo.
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
