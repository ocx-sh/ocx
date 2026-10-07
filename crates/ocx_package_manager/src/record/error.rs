// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Failure modes of the execution-record path.

use std::path::PathBuf;

/// An execution record could not be built or published.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
pub enum RecordsError {
    /// The sink directory or the record file could not be written.
    #[error("failed to write execution record to '{path}'", path = .path.display())]
    #[exit(
        IoError,
        slug = "record_write_failed",
        summary = "Writing an execution record failed"
    )]
    Io {
        /// The sink directory or record path that could not be written.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },

    /// A tier declared `required = true` while no tier declared a sink.
    ///
    /// Refused at config load, or every child runs unrecorded with exit 0.
    #[error("[records] required = true but no sink is configured; set [records] dir, OCX_RECORDS_DIR or --records-dir")]
    #[exit(
        ConfigError,
        slug = "records_required_without_sink",
        summary = "Execution records are required but no sink is configured"
    )]
    RequiredWithoutSink,

    /// The configured name template contains a placeholder OCX does not know.
    #[error(
        "record name template contains unknown placeholder '{placeholder}'; the known set is {{time}}, {{pid}}, {{rand}}, {{host}}"
    )]
    #[exit(
        ConfigError,
        slug = "record_template_unknown_placeholder",
        summary = "The record name template names an unknown placeholder"
    )]
    TemplateUnknownPlaceholder {
        /// The offending placeholder, without its braces.
        placeholder: String,
    },

    /// The configured name template cannot produce a distinct name per record.
    #[error("record name template has no varying component; include {{time}}, {{pid}} or {{rand}}")]
    #[exit(
        ConfigError,
        slug = "record_template_not_unique",
        summary = "The record name template has no varying component"
    )]
    TemplateNotUnique,

    /// The name template, or the filename it rendered, is not a single plain
    /// filename, so the record would land outside the designated sink.
    #[error("record name '{name}' is not a plain filename; use a single filename with no path separator")]
    #[exit(
        ConfigError,
        slug = "record_name_not_a_filename",
        summary = "The record name is not a plain filename"
    )]
    NameNotAFilename {
        /// The template or rendered name that is not a single path component.
        name: String,
    },

    /// The sink no longer resolves to the directory it was designated as.
    ///
    /// Refused rather than followed, or a swapped-in symlink redirects the audit trail.
    #[error(
        "execution record sink '{path}' resolves through a symlink to a different directory; point [records] dir at a real directory",
        path = .path.display()
    )]
    #[exit(
        ConfigError,
        slug = "record_sink_symlink",
        summary = "The execution-record sink is a symlink"
    )]
    SinkSymlink {
        /// The pinned sink path that now resolves elsewhere.
        path: PathBuf,
    },

    /// The record could not be serialized to JSON.
    #[error("failed to serialize execution record")]
    #[exit(
        IoError,
        slug = "record_serialize_failed",
        summary = "Serializing an execution record failed"
    )]
    Serialize(#[source] serde_json::Error),
}
