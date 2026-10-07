// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The two failures `ocx_util` raises that no narrower local type covers: a file operation and a JSON round-trip.

use std::path::{Path, PathBuf};

/// A file operation failed, named by the path it failed on.
///
/// No `source()`: the io cause lives in the message only, or a `{err:#}` walk prints it twice.
/// The field is `cause` because `thiserror` promotes a field named `source` to `Error::source`.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
#[exit(IoError, slug = "file_io", summary = "Reading or writing an internal file failed")]
#[error("internal file error for '{path}': {cause}", path = .path.display(), cause = .cause)]
pub struct FileError {
    pub path: PathBuf,
    pub cause: std::io::Error,
}

impl FileError {
    pub fn new(path: impl AsRef<Path>, cause: std::io::Error) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            cause,
        }
    }
}

/// A JSON serialization or deserialization failed.
///
/// The serializer error stays `source()`, which exit-code classification descends into.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
#[exit(
    DataError,
    slug = "json_serialization",
    summary = "JSON could not be serialized or deserialized"
)]
#[error("JSON serialization error")]
pub struct SerializationError(#[from] pub serde_json::Error);

/// Either failure, for a JSON round-trip that touches the disk; a function that fails one way returns the concrete type.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
#[exit(family = "UtilError")]
pub enum Error {
    #[error(transparent)]
    #[exit(delegate)]
    File(#[from] FileError),
    #[error(transparent)]
    #[exit(delegate)]
    Serialization(#[from] SerializationError),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Flatten an error and its `source()` chain into one line, `": {source}"` per link.
///
/// Use it for any error rendered short of `main`, whose `{err:#}` walk is the only other one.
pub fn render_chain(error: &dyn std::error::Error) -> String {
    let mut out = error.to_string();
    append_chain(&mut out, error.source());
    out
}

/// Append a cause chain to an already-rendered message, skipping a link whose text the output already ends with.
pub fn append_chain(out: &mut String, first_cause: Option<&(dyn std::error::Error + 'static)>) {
    use std::fmt::Write as _;

    let mut cause = first_cause;
    while let Some(source) = cause {
        let text = source.to_string();
        if !out.ends_with(&text) {
            let _ = write!(out, ": {text}");
        }
        cause = source.source();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// D-042 / D-049: the text is a byte-for-byte copy of the literal
    /// `Error::InternalFile` carried, because `ocx`'s acceptance suite and
    /// every `reason` field read it.
    ///
    /// Written as a literal, not `format!`-derived from the fields: a test
    /// that rebuilds the message from the same pieces the `#[error]`
    /// attribute uses asserts only that the formatter equals itself.
    #[test]
    fn file_error_display_is_the_moved_literal() {
        let error = FileError::new(
            "/tmp/example/data",
            std::io::Error::new(std::io::ErrorKind::NotFound, "no such file or directory (os error 2)"),
        );
        assert_eq!(
            error.to_string(),
            "internal file error for '/tmp/example/data': no such file or directory (os error 2)"
        );
    }

    /// D-042: the cause is interpolated and **not** a `source()` — the
    /// ocx#286 doubling and the ocx#433 silence are the two ways this goes
    /// wrong, and they are opposite, so the absence has to be asserted.
    #[test]
    fn file_error_has_no_source() {
        let error = FileError::new("/tmp/example/data", std::io::Error::other("boom"));
        assert!(
            std::error::Error::source(&error).is_none(),
            "FileError must expose no source(); the cause belongs in the message alone"
        );
    }

    /// D-049: the union is invisible — each arm renders and chains exactly as
    /// the concrete error it carries, which is what makes it safe to insert
    /// between a `utility` function and the wide `Error` it converts into.
    ///
    /// `#[error(transparent)]` forwards **both** `Display` and `source()`, so
    /// wrapping adds no message and no chain link: the serializer error stays
    /// one level below the union, exactly where `Error::SerializationFailure`
    /// put it before, and `FileError`'s deliberate source-lessness (D-042)
    /// survives the wrap rather than being replaced by a link to itself.
    #[test]
    fn the_union_is_invisible_in_both_render_and_chain() {
        let inner = serde_json::from_str::<serde_json::Value>("{").expect_err("`{` is not valid JSON");
        let inner_text = inner.to_string();
        let wrapped: Error = SerializationError::from(inner).into();
        assert_eq!(
            wrapped.to_string(),
            "JSON serialization error",
            "the union adds no prefix"
        );
        let source = std::error::Error::source(&wrapped).expect("the serializer error survives the wrap");
        assert_eq!(source.to_string(), inner_text, "the union must not truncate the chain");

        let file: Error = FileError::new("/tmp/example/data", std::io::Error::other("boom")).into();
        assert_eq!(file.to_string(), "internal file error for '/tmp/example/data': boom");
        assert!(
            std::error::Error::source(&file).is_none(),
            "wrapping must not invent a source FileError deliberately does not have (D-042, ocx#286)"
        );
    }

    /// C-068: the chain walk reaches **every** link, not just the first.
    ///
    /// The three chain tests that existed before this one (in the then-undissolved
    /// `ocx_lib`'s `error.rs`, and the private copy in
    /// `config/error.rs`) are each one level deep, so `append_chain`'s loop
    /// body ran exactly once in every one of them: replacing
    /// `cause = source.source()` with `cause = None` — truncating the
    /// workspace's only cause-chain renderer to a single link — left 257 of 257
    /// tests green (B5-8). Three levels is the shortest chain that can tell a
    /// loop from an `if`, and this is the crate the helper now lives in.
    ///
    /// Distinct, non-overlapping texts on purpose: the walk's duplicate-skip
    /// (asserted below) is exactly the rule that would absorb a dropped link if
    /// two levels rendered alike.
    #[test]
    fn display_chain_is_transparent() {
        #[derive(Debug, thiserror::Error)]
        #[error("innermost")]
        struct Innermost;

        #[derive(Debug, thiserror::Error)]
        #[error("middle")]
        struct Middle(#[from] Innermost);

        #[derive(Debug, thiserror::Error)]
        #[error("outermost")]
        struct Outermost(#[from] Middle);

        assert_eq!(
            render_chain(&Outermost(Middle(Innermost))),
            "outermost: middle: innermost",
            "every link of the chain is rendered, not only the first cause"
        );

        // The other half of the same walk: a leaf that already interpolated its
        // own source must not have that text appended a second time (ocx#286).
        #[derive(Debug, thiserror::Error)]
        #[error("wrapper: {0}")]
        struct Interpolating(#[from] Innermost);

        assert_eq!(
            render_chain(&Interpolating(Innermost)),
            "wrapper: innermost",
            "a link the output already ends with is skipped, never doubled"
        );
    }

    /// DEC-11 / D-049: the text is the moved literal, and the serializer's
    /// error *is* reachable through `source()` — the half that differs from
    /// [`FileError`], and the half exit-code classification descends.
    #[test]
    fn serialization_error_display_is_the_moved_literal_and_keeps_its_source() {
        let inner = serde_json::from_str::<serde_json::Value>("{").expect_err("`{` is not valid JSON");
        let inner_text = inner.to_string();
        let error = SerializationError::from(inner);
        assert_eq!(error.to_string(), "JSON serialization error");
        let source = std::error::Error::source(&error).expect("the serializer error is the source");
        assert_eq!(source.to_string(), inner_text);
    }
}
