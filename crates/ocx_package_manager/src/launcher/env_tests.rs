// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The trampoline-identity case whose fixtures are **generated launchers**.
//!
//! `is_ocx_trampoline` lives in `ocx_config::env`, and every other case for it is a hand-written body beside that
//! predicate. This one renders the real [`generate`](super::generate) output and a real deferred launcher, so the
//! predicate is measured against the bytes this module actually emits rather than an approximation of them.

#![cfg(test)]

// Every item in this file is `#[cfg(unix)]` — the trampoline fixture is a POSIX body. Gating the imports too keeps
// the Windows build warning-clean.
#[cfg(unix)]
use std::path::PathBuf;

#[cfg(unix)]
use ocx_config::env::{TRAMPOLINE_MARKER, is_ocx_trampoline};

/// An executable POSIX body carrying [`TRAMPOLINE_MARKER`] on its second line,
/// exactly where the generated trampoline puts it.
#[cfg(unix)]
fn write_trampoline(dir: &std::path::Path, name: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\n{TRAMPOLINE_MARKER}\nunset OCX_GLOBAL OCX_PROJECT\n\
             __ocx_binary='/home/ocx/bin/ocx'\n\
             exec \"${{OCX_BINARY_PIN:-$__ocx_binary}}\" --project '/p' exec -- \"${{0##*/}}\" \"$@\"\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// The marker is **discriminating by construction**.
///
/// Three arms in one test so it cannot pass vacuously: the real package launcher and the real deferred launcher,
/// both written by the shipped writers, are not refused, and only a body carrying [`TRAMPOLINE_MARKER`] is.
#[cfg(unix)]
#[tokio::test]
async fn the_marker_admits_the_shipped_launchers_and_refuses_only_a_trampoline() {
    use std::collections::BTreeMap;

    use ocx_package::metadata::entrypoint::{Entrypoint, EntrypointName, Entrypoints};

    let tmp = tempfile::tempdir().unwrap();

    // 1. The real package launcher, written by the shipped generator.
    let pkg_root = tmp.path().join("pkg");
    let dest = pkg_root.join("launchers");
    let entries = Entrypoints::new(BTreeMap::from([(
        EntrypointName::try_from("cmake").unwrap(),
        Entrypoint::default(),
    )]));
    crate::launcher::generate(&pkg_root, &entries, &dest)
        .await
        .expect("the shipped generator writes a launcher");
    let launcher = dest.join("cmake");
    assert!(
        launcher.is_file(),
        "precondition: the shipped generator wrote the launcher"
    );
    assert!(
        !is_ocx_trampoline(&launcher),
        "a package launcher is not a trampoline and must keep resolving"
    );

    // 2. The real deferred-tool launcher, from the shipped writer.
    let identifier = ocx_oci::PinnedPackageRef::try_from(
        ocx_oci::PackageRef::new_registry("cmake", "example.com")
            .clone_with_digest(ocx_oci::Digest::Sha256("a".repeat(64))),
    )
    .expect("a digest-bearing identifier is pinned");
    let record = crate::launcher::deferred_record(&identifier).expect("the record renders");
    let blob = crate::launcher::host_launcher_blob(tmp.path()).expect("host blob");
    crate::launcher::write_launcher(tmp.path(), "shim-cmake", blob, &record, false)
        .await
        .expect("the shipped writer writes a launcher");
    assert!(
        !is_ocx_trampoline(&tmp.path().join("shim-cmake")),
        "a deferred launcher is not a trampoline and must keep resolving"
    );

    // 3. The trampoline itself.
    let trampoline = write_trampoline(tmp.path(), "trampoline-cmake");
    assert!(
        is_ocx_trampoline(&trampoline),
        "a body carrying TRAMPOLINE_MARKER is the one file that is refused"
    );
}
