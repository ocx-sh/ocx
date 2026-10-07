// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The classification vocabulary an error type declares: its exit code and its `error.detail` slug.

use crate::ExitCode;

/// One `error.detail` slug a family can emit, with the exit code it ships under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetailEntry {
    /// The frozen snake_case value of `error.detail`.
    pub slug: &'static str,
    /// The process exit code an error carrying this slug exits with.
    pub exit_code: ExitCode,
    /// Name of the error type whose `kind_detail` produces the slug.
    pub family: &'static str,
    /// One-line user-facing meaning, as the published contract states it.
    pub summary: &'static str,
}

/// Classify an error into an [`ExitCode`].
pub trait ClassifyExitCode {
    /// Return an exit code for this error, or `None` to defer to the next link in the source chain.
    ///
    /// A `chain = field` arm answers its fallback code here. The authoritative verdict is the binary's chain
    /// resolution, which prefers the first classified cause of `field`, so the exit status can differ.
    fn classify(&self) -> Option<ExitCode>;
}

/// Where an error's `error.detail` slug comes from.
///
/// Borrows from the error that produced it, so a chain can name the error it starts at.
#[derive(Debug, Clone, Copy)]
pub enum Detail<'a> {
    /// The error itself names the slug.
    Fixed(&'static DetailEntry),
    /// The slug is the first classified cause's in the source chain of `from`; `fallback` when none classifies.
    ///
    /// Only the binary can walk that chain, since foreign error types are classified there. Two arms use it,
    /// told apart by [`ClassifyExitCode::classify`]: `None` means the walker reaches the same causes by itself
    /// and `from` is the error itself; `Some(code)` means the arm decides the exit code too, so the resolver
    /// answers with the first classified cause of `from` (its code and slug) and falls back to `code` and
    /// `fallback`. `from` is a field the arm's own `source()` skips, such as an `#[error(transparent)]` payload.
    Chain {
        /// The entry used when no cause in the chain classifies.
        fallback: &'static DetailEntry,
        /// The error the walk starts at.
        from: &'a (dyn std::error::Error + 'static),
    },
}

impl Detail<'_> {
    /// The entry this error names itself: the fixed one, or the chain's fallback.
    pub const fn fallback_entry(self) -> &'static DetailEntry {
        match self {
            Self::Fixed(entry) | Self::Chain { fallback: entry, .. } => entry,
        }
    }
}

/// A row an arm declared in `rows(...)`: the only value a `with` function can answer with.
///
/// Only the derive builds one, so a function cannot name a slug its arm did not declare.
#[derive(Debug, Clone, Copy)]
pub struct Row {
    entry: &'static DetailEntry,
    defers: bool,
}

impl Row {
    /// Derive output only: wraps a row the arm declared; `defers` is true for a `defer(Code)` row.
    #[doc(hidden)]
    pub const fn __declared(entry: &'static DetailEntry, defers: bool) -> Self {
        Self { entry, defers }
    }
}

/// The verdict a `with` function returns for one arm: an exit code answer and a slug source.
#[derive(Debug, Clone, Copy)]
pub struct Pick<'a> {
    code: Option<ExitCode>,
    detail: Detail<'a>,
}

impl<'a> Pick<'a> {
    /// A declared row: its code, or `None` when the row is `defer(Code)`; the slug is the row's.
    pub const fn row(row: Row) -> Self {
        Self {
            code: if row.defers { None } else { Some(row.entry.exit_code) },
            detail: Detail::Fixed(row.entry),
        }
    }

    /// Hand the whole verdict to `inner`, which must classify itself; statically checked, no row involved.
    pub fn delegate<T: ClassifyExitCode + ClassifyErrorKind>(inner: &'a T) -> Self {
        Self {
            code: inner.classify(),
            detail: inner.kind_detail(),
        }
    }

    /// A [`Detail::Chain`] over `from` with `row` as the fallback; the code follows the row as in [`Pick::row`].
    pub const fn chain(from: &'a (dyn std::error::Error + 'static), row: Row) -> Self {
        Self {
            code: if row.defers { None } else { Some(row.entry.exit_code) },
            detail: Detail::Chain {
                fallback: row.entry,
                from,
            },
        }
    }

    /// The answer for [`ClassifyExitCode::classify`].
    pub const fn code(self) -> Option<ExitCode> {
        self.code
    }

    /// The answer for [`ClassifyErrorKind::kind_detail`].
    pub const fn detail(self) -> Detail<'a> {
        self.detail
    }
}

/// Infallible companion of [`ClassifyExitCode`]: the non-`Option` return forces an exhaustive impl.
pub trait ClassifyErrorKind: std::error::Error + 'static {
    /// Every row this type declares, once per slug: fixed and `with` rows, chain fallbacks, then `reserve` rows.
    ///
    /// A delegating arm lists none, since its field type lists its own; a delegating struct's is its field type's.
    /// A chain can answer with a cause's slug that only the cause's own type lists.
    const DETAILS: &'static [DetailEntry];

    /// Stable snake_case discriminant for `error.detail`, frozen across releases: consumers dispatch on it.
    fn kind_detail(&self) -> Detail<'_>;
}

/// One classified cause's verdict: the exit code and where the `error.detail` slug of the same error comes from.
#[derive(Debug, Clone, Copy)]
pub struct Decision<'a> {
    /// The exit code the cause decided.
    pub code: ExitCode,
    /// The cause's slug source.
    pub detail: Detail<'a>,
}
