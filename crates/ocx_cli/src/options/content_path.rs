use ocx_lib::file_structure::SymlinkKind;

/// Selects how the content path for package environment resolution is obtained.
///
/// Without any flag the content-addressed object store path is used (default).
/// `--candidate` and `--current` are mutually exclusive and yield a stable symlink
/// path instead — see the [path resolution modes](https://ocx.sh/docs/reference/command-line#path-resolution)
/// documentation for details.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct ContentPath {
    /// Resolve env paths relative to the installed candidate symlink
    /// (`~/.ocx/installs/<registry>/<repo>/candidates/<tag>`).
    ///
    /// The package must be installed before this flag can be used.
    /// Digest identifiers are rejected — use a tag-only identifier instead.
    /// No auto-install is performed.
    #[clap(long = "candidate", conflicts_with = "current")]
    candidate: bool,

    /// Resolve env paths relative to the current-selected symlink
    /// (`~/.ocx/installs/<registry>/<repo>/current`).
    ///
    /// A version of the package must be selected before this flag can be used.
    /// Digest identifiers are rejected.
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
