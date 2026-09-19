// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The two failures every `utility` file raises that no narrower local type
//! already covers (plan C-042; spec D-042, DEC-11).
//!
//! `utility` is the bottom tier: it becomes `ocx_util`, which nothing above it
//! may be named from. Until this module existed, its files reached back up to
//! the crate-wide `Error` for exactly two shapes — a file
//! operation that failed, and a JSON round-trip that failed — and those two
//! reaches were the whole reason `ocx_util → ocx_lib` still existed on paper.
//!
//! Both types are **the same user-visible text** as the wide variants they
//! replace, because the acceptance suite reads those strings: `error.rs`
//! converts each into its existing variant on the way up (`From` impls there,
//! pinned by a round-trip test), so nothing a user or a script can observe
//! moved with the construction site.

use std::path::{Path, PathBuf};

/// A file operation failed, named by the path it failed on.
///
/// **No `source()`, deliberately** (D-042) — the io cause is interpolated into
/// the message and exposed *only* there. That is the one exception to this
/// repository's "every wrapping error carries `#[source]`" rule, and it is
/// exactly one of the two, never both: with both, a `{err:#}` chain walk
/// printed the io text twice (ocx#286); with `#[source]` alone, every
/// `to_string()` producer — a warn line, a machine-readable `reason` — named
/// the path and nothing else (ocx#433). The field is named `cause` rather than
/// `source` for the same reason: `thiserror` promotes a field *named* `source`
/// to `Error::source` on its own, so the name is load-bearing.
#[derive(Debug, thiserror::Error)]
#[error("internal file error for '{path}': {cause}", path = .path.display(), cause = .cause)]
pub struct FileError {
    /// The path the operation was attempted on.
    pub path: PathBuf,
    /// The io failure, rendered into the message and never into `source()`.
    pub cause: std::io::Error,
}

impl FileError {
    /// Build a [`FileError`] for `path` from the io failure that produced it.
    pub fn new(path: impl AsRef<Path>, cause: std::io::Error) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            cause,
        }
    }
}

/// A JSON serialization or deserialization failed.
///
/// Carries the serializer's own error as `#[source]` (via `#[from]`), which is
/// what the wide variant did and what exit-code classification descends into.
#[derive(Debug, thiserror::Error)]
#[error("JSON serialization error")]
pub struct SerializationError(#[from] pub serde_json::Error);

/// What a `utility` function raises when it can fail in **both** of the tier's
/// own ways: a JSON round-trip that touches the disk, where either the file or
/// the codec can be the half that failed (DEC-11).
///
/// Both arms are `#[error(transparent)]`, so this type renders as whichever
/// concrete error it carries and adds no prefix of its own; `error.rs` unwraps
/// it arm by arm into the wide variant that arm already rendered as.
///
/// A function that can only fail one way keeps the concrete type instead —
/// `LockedFile`'s reads and writes, every `symlink` operation, `move_dir` and
/// the directory walker all return [`FileError`] itself, and `validate_target`
/// returns [`crate::archive::Error`]. Narrowing costs nothing and keeps the
/// union to the one shape that genuinely needs it, so no caller matches an arm
/// that cannot occur.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A file operation failed.
    #[error(transparent)]
    File(#[from] FileError),
    /// A JSON round-trip failed.
    #[error(transparent)]
    Serialization(#[from] SerializationError),
}

/// [`Result`](std::result::Result) over this module's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// Flatten an error and its `source()` chain into one line, `": {source}"` per
/// link.
///
/// `Display` renders the outermost message only, which for a wrapper variant
/// that adds no `{0}` interpolation (`#[error("failed to fetch managed
/// config")] Fetch(#[from] …)`) names nothing at all. The `{err:#}` chain walk
/// that makes such an error readable happens at the CLI boundary, so anywhere
/// an error is rendered into a value that does NOT reach `main` — a warn line,
/// a machine-readable `reason` field — the chain has to be walked here instead.
///
/// A link whose text the output already ends with is skipped: leaf subsystem
/// errors still interpolate their own source, and this keeps the walk
/// duplicate-free either way.
pub fn render_chain(error: &dyn std::error::Error) -> String {
    let mut out = error.to_string();
    append_chain(&mut out, error.source());
    out
}

/// Append a cause chain to an already-rendered message, `": {source}"` per
/// link — the shared tail of [`render_chain`] and the batch-entry renderer in
/// `package_manager::error`, which starts the walk at a different link.
///
/// A link whose text the output already ends with is skipped: leaf subsystem
/// errors still interpolate their own source, and this keeps the walk
/// duplicate-free either way.
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
