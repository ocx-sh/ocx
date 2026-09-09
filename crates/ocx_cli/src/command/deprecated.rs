// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Deprecated command spellings, kept alive for exactly one release pair.
//!
//! Every name renamed in 0.6 dispatches through this module, so one grep finds
//! the whole set and 0.7 removes it by deleting this file together with the
//! hidden `Command` / `Package` variants that call it. Nothing else may depend
//! on it.
//!
//! An old spelling is a *hidden command*, never a clap alias: `ArgMatches`
//! reports the canonical name, so an alias is invisible to the code and could
//! not warn. The hidden variant also keeps its own
//! [`crate::app::canonical_command_name`] arm reporting the **old** string, so
//! the frozen v1 error envelope still distinguishes a deprecated invocation
//! from a current one and nothing already emitted by a released binary changes
//! meaning.
//!
//! # External sites the 0.7 removal must delete with this file
//!
//! One spelling in this window is a renamed **flag**, not a renamed command
//! (`ocx package announce --package` became a positional, C-062). clap cannot
//! tell the two forms apart at parse time, so the deprecated half is two
//! declarations that must live on the announce args struct rather than here.
//! Both carry a `// 0.7 removal:` comment naming this module, so one grep over
//! `0.7 removal:` finds the whole set:
//!
//! | Site | What it is |
//! |---|---|
//! | the `package_flag` `Arg` on [`crate::command::package_announce::PackageAnnounce`] | the hidden `--package` long |
//! | the `package_selector` `ArgGroup` on the same struct, and the `override_usage` beside it | what makes exactly one of the two spellings required, and what keeps the hidden one out of the rendered usage line (DX-65) |
//!
//! Deleting this file therefore is not the whole removal: the two clap
//! declarations go with it, and the positional loses its `Option`.

use crate::app::Context;

/// The release that deletes this module and every spelling in it.
const REMOVAL_RELEASE: &str = "0.7";

/// Every command spelling this window renames, as `(old, new)`.
///
/// The authority the sweep reads. Before this existed, each rename was three
/// hand-written literals — the hidden `Command` / `Package` variant, its
/// [`warn_renamed`] call, and its [`crate::app::canonical_command_name`] arm —
/// with nothing naming the set, so a repo-wide check for stale spellings had no
/// input it could be driven from. `test/tests/test_deprecated_spellings.py`
/// parses this list out of this file and sweeps every old spelling in it.
///
/// The renamed **flag** (`ocx package announce --package`, C-062) is not here
/// and cannot be: it renames an argument, not a command, so it has no `(old,
/// new)` command pair. The sweep carries it as its own rendering.
pub const RENAMED: &[(&str, &str)] = &[
    ("run", "exec"),
    ("package describe", "package description push"),
    ("package info", "package description pull"),
];

/// Warn on stderr that `old` has been renamed to `new`.
///
/// Fires once per invocation by construction — one process dispatches one
/// command. Routed through [`Context::ui`], so the notice never reaches stdout
/// and degrades to `log::warn!` when quiet or non-interactive.
pub fn warn_renamed(context: &Context, old: &str, new: &str) {
    context.ui().warn(format!(
        "`ocx {old}` is renamed to `ocx {new}` and is removed in {REMOVAL_RELEASE}"
    ));
}

/// The notice `ocx package announce --package` prints, naming the positional
/// form and [`REMOVAL_RELEASE`] (C-062).
///
/// Returns the sentence instead of warning with it, and that split is the
/// point: [`ocx_lib::cli::Printer`] writes the real streams and no seam in this
/// workspace captures one, so a `warn_renamed`-shaped helper would leave the
/// sentence — and the release it names — with nothing able to assert it. The
/// caller routes the value through [`Context::ui`], which is what keeps it on
/// stderr and off stdout; once-ness is the caller's too, and is why the two arg
/// ids are merged in `execute` rather than in an accessor read more than once.
///
/// [`warn_renamed`] cannot serve: its sentence renames one *command* to
/// another, and a flag becoming a positional is a different statement about a
/// different surface.
#[must_use]
pub fn package_flag_notice() -> String {
    format!(
        "`ocx package announce --package <PACKAGE>` takes the package as a positional argument now; \
         the flag is removed in {REMOVAL_RELEASE}"
    )
}

#[cfg(test)]
mod tests {
    use super::{RENAMED, package_flag_notice};

    /// C-062 / S-034: the notice names the deprecated spelling, the form that
    /// replaces it, and the release that removes it.
    ///
    /// This is the **reachable half** of the inventory's
    /// `announce_warning_never_reaches_stdout` (DX-69). No Rust seam in this
    /// workspace observes stdout — [`ocx_lib::cli::Printer`] writes the real
    /// streams and [`ocx_lib::cli::Cell`] exposes no text — so the stdout-purity
    /// and once-ness halves are acceptance assertions instead, in
    /// `test/tests/test_announce.py::test_deprecated_package_flag_warns_once_on_stderr_only`,
    /// whose mechanism is already proved in both directions at
    /// `test/tests/test_tag_reserved.py:131,137` (a `ui().warn` on stderr while
    /// stdout stays parseable JSON in the same run).
    ///
    /// `"0.7"` is quoted as a **literal**, never read off [`REMOVAL_RELEASE`]:
    /// reading the constant would make the named mutation invisible, because the
    /// expectation would move with it.
    ///
    /// Red at the stub: `package_flag_notice` is `unimplemented!()`, so this
    /// panics.
    /// Mutation once implemented: set [`REMOVAL_RELEASE`] to `"0.8"`; or drop
    /// the word `positional` from the sentence, which is the needle the
    /// acceptance half counts.
    ///
    /// [`REMOVAL_RELEASE`]: super::REMOVAL_RELEASE
    #[test]
    fn announce_package_flag_notice_names_the_removal_release() {
        let notice = package_flag_notice();
        assert!(
            notice.contains("--package"),
            "the notice must name the spelling the operator typed, got: {notice}"
        );
        assert!(
            notice.contains("positional"),
            "the notice must name the form that replaces it, got: {notice}"
        );
        assert!(
            notice.contains("0.7"),
            "the notice must name the release that removes the flag, got: {notice}"
        );
    }

    /// C-062 / E-22: every site the 0.7 removal must delete is findable from one
    /// `0.7 removal:` grep.
    ///
    /// Scanned over **`command/package_announce.rs`**, a different file, and
    /// that is the whole design: a scan run over this module's own source would
    /// match the `# External sites…` doc block above in every state — a detector
    /// measuring its own invocation (`quality-core.md` § Unchecked Green).
    ///
    /// Asserted as a **count**, never as presence: the doc block names two
    /// sites (the hidden `--package` `Arg`, and the `package_selector` `ArgGroup`
    /// with the `override_usage` beside it), so a marker deleted from one of them
    /// must red rather than being covered by its survivor. A bare
    /// `contains(...)` would also stay green if the file were renamed and the
    /// `include_str!` retargeted at something that happened to quote the phrase.
    ///
    /// **Green on arrival** — the stub carries both markers.
    /// Mutation: delete the `// 0.7 removal:` comment above the
    /// `package_selector` `ArgGroup`; the count drops to one and this reds.
    #[test]
    fn the_announce_deprecation_sites_are_marked_for_the_removal_release() {
        let announce_source = include_str!("package_announce.rs");
        assert!(
            announce_source.matches("0.7 removal:").count() >= 2,
            "both announce-side deprecation sites must carry the `0.7 removal:` marker this module's \
             doc block enumerates; found {}",
            announce_source.matches("0.7 removal:").count()
        );
    }

    /// The two quoted arguments of the first `warn_renamed(&context, …)` call in
    /// `tail`.
    fn first_pair(tail: &str) -> Option<(&str, &str)> {
        let (_, after_open) = tail.split_once('"')?;
        let (old, rest) = after_open.split_once('"')?;
        let (_, after_second_open) = rest.split_once('"')?;
        let (new, _) = after_second_open.split_once('"')?;
        Some((old, new))
    }

    /// [`RENAMED`] is the whole set: every dispatch site that warns about a
    /// renamed command names a pair this list carries, and carries no other.
    ///
    /// Scanned over `command.rs` and `command/package.rs`, never over this
    /// module's own source — the needle `warn_renamed(&context, ` is a literal
    /// in the scanner directly above, so a scan that included this file would
    /// match its own invocation in every state (`quality-core.md` § Unchecked
    /// Green), exactly as the `0.7 removal:` count below is scanned over a
    /// different file for the same reason.
    ///
    /// Asserted as a **count first**, then membership. Membership alone stays
    /// green when an entry is deleted from [`RENAMED`] *and* from its dispatch
    /// site together, which is the shape a half-finished 0.7 removal has; the
    /// count is what makes the sweep's input provably complete.
    ///
    /// Mutation: delete any one entry from [`RENAMED`] — the count reds.
    #[test]
    fn every_warn_renamed_dispatch_site_is_listed_in_renamed() {
        const NEEDLE: &str = "warn_renamed(&context, ";
        let sources = [include_str!("../command.rs"), include_str!("package.rs")];

        let sites: Vec<(&str, &str)> = sources
            .iter()
            .flat_map(|source| source.split(NEEDLE).skip(1))
            .map(|tail| first_pair(tail).expect("a warn_renamed call site quotes two literals"))
            .collect();

        assert_eq!(
            sites.len(),
            RENAMED.len(),
            "RENAMED must list exactly the dispatch sites that warn; found {sites:?} against {RENAMED:?}"
        );
        for site in &sites {
            assert!(
                RENAMED.contains(site),
                "`{}` -> `{}` warns at a dispatch site but is missing from RENAMED, so the \
                 repo-wide sweep in test/tests/test_deprecated_spellings.py never looks for it",
                site.0,
                site.1
            );
        }
    }
}
