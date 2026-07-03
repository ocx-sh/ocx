// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Regression: the shipped macOS binary must be self-contained.
//!
//! A build that dynamically links liblzma against homebrew's xz bakes an
//! absolute `/opt/homebrew/.../liblzma.5.dylib` load command into the Mach-O,
//! so `ocx` refuses to start on any Mac without that exact dylib. OCX targets
//! CI / Bazel / scripted hosts that have no homebrew, so any non-system dylib
//! dependency is a hard runtime failure. This guards every non-system dylib,
//! not just lzma.
//!
//! ponytail: inspects link-time `LC_LOAD_DYLIB` only, not runtime `dlopen`
//! (ocx does none). Widen to a dlopen audit only if that ever changes.
#![cfg(target_os = "macos")]

use std::process::Command;

#[test]
fn ocx_links_only_system_dylibs() {
    let bin = env!("CARGO_BIN_EXE_ocx");
    let out = Command::new("otool").args(["-L", bin]).output().expect("run otool");
    assert!(
        out.status.success(),
        "otool failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let listing = String::from_utf8_lossy(&out.stdout);

    // `otool -L` prints the binary path on line 1, then `<dylib> (compat ...)`
    // per dependency. Only `/usr/lib` and `/System/Library` are guaranteed
    // present on stock, SIP-protected macOS. Anything else — `/opt/homebrew`,
    // `/usr/local` (Intel homebrew), MacPorts, `@rpath`/`@loader_path` — is not
    // portable. An origin-prefix allowlist stays green across macOS/toolchain
    // bumps that add or version system libraries.
    let bad: Vec<&str> = listing
        .lines()
        .skip(1)
        .filter_map(|line| line.split_whitespace().next())
        .filter(|path| !path.starts_with("/usr/lib/") && !path.starts_with("/System/Library/"))
        .collect();

    assert!(
        bad.is_empty(),
        "ocx links non-system dylibs (must be self-contained): {bad:?}"
    );
}
