// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Regression: the shipped Linux binary must not dynamically link a host
//! compression library.
//!
//! `liblzma-sys` pkg-config-probes for a host liblzma on non-MSVC targets and
//! links it dynamically when the `-dev` package (`liblzma.pc`) is present — e.g.
//! on a CI runner with `liblzma-dev` installed. That bakes a `liblzma.so.5`
//! NEEDED entry into the ELF, so `ocx` refuses to start on a host without it.
//! The `static` feature (root `Cargo.toml`) forces the vendored xz to be
//! static-linked instead; this test guards that it stays that way. It mirrors
//! `macos_self_contained.rs`, which catches the same class of regression on
//! macOS via homebrew's `liblzma.5.dylib`.
//!
//! ponytail: guards the compression backends (the actual regression class), not
//! a full system-lib allowlist — glibc / libgcc / libm dynamic linking is normal
//! and expected on a glibc Linux build. Widen to an allowlist only if a
//! non-compression host dylib ever leaks in.
#![cfg(target_os = "linux")]

use std::process::Command;

#[test]
fn ocx_does_not_dynamically_link_compression_libs() {
    let bin = env!("CARGO_BIN_EXE_ocx");
    let Ok(out) = Command::new("ldd").arg(bin).output() else {
        // `ldd` absent (e.g. a minimal musl image) — nothing to assert against.
        eprintln!("skipping: ldd not available");
        return;
    };

    // A fully static binary makes `ldd` print "not a dynamic executable" on
    // stderr and exit non-zero; a glibc binary lists its NEEDED dylibs on
    // stdout. Inspect both so either shape is covered.
    let listing = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // Compression backends that MUST be vendored/static: xz (liblzma), zstd,
    // bzip2, zlib. A NEEDED entry for any of these means a host dylib leaked in.
    // `libz.so` (not bare `libz`) so the match does not also fire on `libzstd`.
    const BANNED: &[&str] = &["liblzma", "libzstd", "libbz2", "libz.so"];
    let leaked: Vec<&str> = BANNED.iter().copied().filter(|lib| listing.contains(lib)).collect();

    assert!(
        leaked.is_empty(),
        "ocx dynamically links compression libs (must be static/vendored): {leaked:?}\nldd output:\n{listing}"
    );
}
