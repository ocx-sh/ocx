// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::{Path, PathBuf};

use ocx_package_manager::composer::LinkSource;

use crate::error::UsageError;

/// Selects how the content path for a package is resolved.
///
/// Used by `package which`, `package env` and `package exec`. Without any flag the
/// content-addressed object store path is used (default). `--candidate`, `--current` and
/// `--link` are mutually exclusive and yield a stable link path instead - see the
/// [path resolution](https://ocx.sh/docs/reference/command-line#path-resolution)
/// documentation for details.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct ContentPath {
    /// Resolve the content path via the installed candidate symlink
    /// (`~/.ocx/symlinks/<registry>/<repo>/candidates/<tag>`).
    ///
    /// The package must be installed before this flag can be used.
    /// Digest identifiers are rejected - use a tag-only identifier instead.
    /// No auto-install is performed.
    #[clap(long = "candidate", conflicts_with_all = ["current", "link"])]
    candidate: bool,

    /// Resolve the content path via the current-selected symlink
    /// (`~/.ocx/symlinks/<registry>/<repo>/current`).
    ///
    /// A version of the package must be selected before this flag can be used.
    /// Digest identifiers are rejected. The tag portion of the identifier is
    /// not validated against the selected version - only the registry and
    /// repository are used to locate the symlink.
    /// No auto-install is performed.
    #[clap(long = "current", conflicts_with_all = ["candidate", "link"])]
    current: bool,

    /// Resolve the content path via a link written by `ocx package install --link <PATH>`.
    ///
    /// Takes exactly one package, whose identifier names it in the output.
    /// Digest identifiers are rejected. No auto-install is performed.
    #[clap(long = "link", value_name = "PATH", conflicts_with_all = ["candidate", "current"])]
    link: Option<PathBuf>,
}

impl ContentPath {
    /// The [`LinkSource`] the flags select; `None` means the object-store path.
    ///
    /// `--link` names one path, so it is a usage error with more than one package.
    pub fn link_source(&self, package_count: usize) -> Result<Option<LinkSource>, UsageError> {
        if self.candidate {
            Ok(Some(LinkSource::Candidate))
        } else if self.current {
            Ok(Some(LinkSource::Current))
        } else if let Some(link) = &self.link {
            if package_count != 1 {
                return Err(UsageError::new("--link takes exactly one package"));
            }
            Ok(Some(LinkSource::Path(absolute_link(link)?)))
        } else {
            Ok(None)
        }
    }
}

/// `path` made absolute against the working directory, without resolving symlinks.
///
/// A link's back-reference is named after this spelling, so the write and every later read must agree on it.
pub fn absolute_link(path: &Path) -> Result<PathBuf, UsageError> {
    std::path::absolute(path)
        .map_err(|error| UsageError::with_source(format!("--link path '{}' cannot be resolved", path.display()), error))
}
