// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Domain-free primitives: fs, locking, extension traits, async singleflight, TLS roots, archive extraction, path-context error helpers.
//!
//! The bottom tier of the crate map: `ocx_util` depends on no `ocx_*` crate,
//! which is what lets every other one depend on it. `ocx_lib::utility::…` is
//! `ocx_util::…` — the module root that was `utility.rs` is this file, so the
//! tier's own name no longer appears in any path (DEC-3: a log target follows
//! the module path, so the targets moved with it).
//!
//! The tier is **closed** (DEC-40). Sixteen bare `pub mod`s made every internal
//! type workspace-public by default, which is not the same thing as a designed
//! seam: `error` and `archive::error` were public because their module was,
//! minted as the *mechanism* of DEC-27 — a crate names its own root error
//! before it is extracted — rather than as an interface anyone shaped. That
//! matters twice over. `ocx_cli` already names this crate in production
//! (`ocx_util::tls::ExtraRoots`), and the CLI's Data and ErrorCode surface has
//! to stay reusable by a future SDK that shells out to `ocx`; an SDK consumer
//! pins whatever is *reachable*, never whatever was intended.
//!
//! So nothing here is exported because a caller inside this crate finds it
//! convenient. `every_public_item_of_ocx_util_has_a_consumer` in
//! `ocx_test_support`'s workspace guards holds that open: a `pub` item no crate
//! outside this one names is `pub(crate)`, and a new one reds until it is
//! either used or narrowed.
//!
//! What is deliberately **not** here:
//!
//! - Any domain knowledge. These are primitives — a path, a lock, an archive —
//!   and knowing what a package or a registry is belongs to whoever owns that
//!   value. It is what lets this crate depend on no `ocx_*` crate at all.
//! - The process-outcome vocabulary. `ExitCode` and `ErrorCategory` are the
//!   seam an SDK links without linking anything else, and live in `ocx_exit`.
//! - A crate-wide `Error`. [`error::Error`] is the two-arm union `serde_ext`
//!   and `fs` raise, not a root that absorbs its callers': `ocx_lib::prelude`
//!   carried one and E1 deletes it, with no successor here.

pub mod archive;
pub mod boolean_string;
pub mod child_process;
pub mod compression;
pub mod env;
pub mod error;
pub mod fs;
pub mod list;
pub mod path;
pub mod result_ext;
pub mod schema;
pub mod serde_ext;
pub mod singleflight;
pub mod string_ext;
pub mod tls;
pub mod vec_ext;

/// The extension traits every crate pulls in wholesale.
///
/// `ocx_lib::prelude` carried `Error` and `Result` beside these four; those are
/// the crate-wide error E1 deletes, and they have no successor here — a
/// consumer that needs its own root error names it directly.
pub mod prelude {
    pub use crate::result_ext::ResultExt;
    pub use crate::serde_ext::SerdeExt;
    pub use crate::string_ext::StringExt;
    pub use crate::vec_ext::VecExt;
}
