// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::file_structure::SymlinkKind;

/// Selects how the content path for a package is resolved.
///
/// Used by `find`, `env`, and `shell env`. Without any flag the content-addressed
/// object store path is used (default). `--candidate` and `--current` are mutually
/// exclusive and yield a stable symlink path instead — see the
/// [path resolution](https://ocx.sh/docs/reference/command-line#path-resolution)
/// documentation for details.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct ContentPath {
    /// Resolve the content path via the installed candidate symlink
    /// (`~/.ocx/symlinks/<registry>/<repo>/candidates/<tag>`).
    ///
    /// The package must be installed before this flag can be used.
    /// Digest identifiers are rejected — use a tag-only identifier instead.
    /// No auto-install is performed.
    #[clap(long = "candidate", conflicts_with = "current")]
    candidate: bool,

    /// Resolve the content path via the current-selected symlink
    /// (`~/.ocx/symlinks/<registry>/<repo>/current`).
    ///
    /// A version of the package must be selected before this flag can be used.
    /// Digest identifiers are rejected. The tag portion of the identifier is
    /// not validated against the selected version — only the registry and
    /// repository are used to locate the symlink.
    /// No auto-install is performed.
    #[clap(long = "current", conflicts_with = "candidate")]
    current: bool,
}

impl ContentPath {
    /// Returns the [`SymlinkKind`] implied by the flags, or `None` if neither
    /// flag was set (object store path should be used).
    pub fn symlink_kind(&self) -> Option<SymlinkKind> {
        if self.candidate {
            Some(SymlinkKind::Candidate)
        } else if self.current {
            Some(SymlinkKind::Current)
        } else {
            None
        }
    }
}
