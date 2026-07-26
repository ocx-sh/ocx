// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Failure modes of the execution-record path.
//!
//! Every variant is reachable from a real operator misconfiguration, so each
//! carries the exit code a wrapper script should branch on. The split between
//! 74 and 78 is deliberate: an unwritable sink is an I/O fault, a malformed
//! name template is a configuration fault the operator must edit a file to fix.

use std::path::PathBuf;

use crate::cli::{ClassifyExitCode, ExitCode};

/// An execution record could not be built or published.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RecordsError {
    /// The sink directory or the record file could not be written.
    #[error("failed to write execution record to '{path}'", path = .path.display())]
    Io {
        /// The sink directory or record path that could not be written.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },

    /// A tier declared `required = true` while no tier declared a sink.
    ///
    /// The plainest way an operator writes "recording is mandatory" is a
    /// `[records]` block carrying nothing but `required = true`. Resolving that
    /// to a policy with nothing to write to would make every child run
    /// unrecorded, exit 0, no warning — the exact opposite of what was asked
    /// for. Refused where the configuration is read, before any work.
    #[error("[records] required = true but no sink is configured; set [records] dir, OCX_RECORDS_DIR or --records-dir")]
    RequiredWithoutSink,

    /// The configured name template contains a placeholder OCX does not know.
    #[error(
        "record name template contains unknown placeholder '{placeholder}'; the known set is {{time}}, {{pid}}, {{rand}}, {{host}}"
    )]
    TemplateUnknownPlaceholder {
        /// The offending placeholder, without its braces.
        placeholder: String,
    },

    /// The configured name template cannot produce a distinct name per record.
    ///
    /// A template with no varying component silently overwrites: every frame in
    /// the same sink resolves to one path.
    #[error("record name template has no varying component; include {{time}}, {{pid}} or {{rand}}")]
    TemplateNotUnique,

    /// The name template, or the filename it rendered, is not a single plain
    /// filename.
    ///
    /// A name carrying a path separator, or reducing to `.`/`..`, would put the
    /// record somewhere other than the sink the operator designated — silently,
    /// and on every invocation. Refused as the configuration fault it is.
    #[error("record name '{name}' is not a plain filename; use a single filename with no path separator")]
    NameNotAFilename {
        /// The template or rendered name that is not a single path component.
        name: String,
    },

    /// The sink no longer resolves to the directory it was designated as.
    ///
    /// Refused rather than followed: whoever swapped the sink for a symlink
    /// would otherwise redirect an audit trail somewhere the operator never
    /// designated.
    #[error(
        "execution record sink '{path}' resolves through a symlink to a different directory; point [records] dir at a real directory",
        path = .path.display()
    )]
    SinkSymlink {
        /// The pinned sink path that now resolves elsewhere.
        path: PathBuf,
    },

    /// The record could not be serialized to JSON.
    #[error("failed to serialize execution record")]
    Serialize(#[source] serde_json::Error),
}

impl ClassifyExitCode for RecordsError {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::Io { .. } | Self::Serialize(_) => ExitCode::IoError,
            // A bad template is a config parse error, not an I/O fault — the
            // operator fixes it by editing `[records] name`, and 74 would send
            // them looking at disk permissions instead.
            Self::TemplateUnknownPlaceholder { .. }
            | Self::TemplateNotUnique
            | Self::NameNotAFilename { .. }
            // Same reasoning: a posture with nothing to write to is a fault in
            // the config chain, fixed by adding a `dir` or dropping `required`.
            | Self::RequiredWithoutSink => ExitCode::ConfigError,
            // Not `SymlinkWalkError::Ancestor`'s 64: that precedent refuses a
            // path the user typed as an *argument*, where usage is the fault.
            // This sink arrives from `[records] dir` / `OCX_RECORDS_DIR` /
            // `--records-dir` — configuration, which the operator fixes by
            // editing a file, the same as a bad template.
            Self::SinkSymlink { .. } => ExitCode::ConfigError,
        })
    }
}
