// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Lazy package loading: `LazyMode` and `LazyReport`, the two closed enums
//! backing the `lazy-mode` / `lazy-report` config ladder, plus the ladder's
//! resolution entry point.
//!
//! See `plan_lazy_package_loading.md` contracts C-005 / C-006 for the full
//! design: a tool declared with `lazy-mode = "always"` composes onto `PATH`
//! as a generated shim instead of eager content; `lazy-report` controls
//! whether the shim's first-invocation materialization renders progress.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Whether a declared tool composes onto `PATH` eagerly or as a shim that
/// defers content materialization to first invocation.
///
/// Closed, internal enum — no `#[non_exhaustive]` (`arch-principles.md`
/// "Internal enum exhaustiveness"): `ocx_lib` ships no external API, so
/// every match over `LazyMode` stays total across the workspace.
///
/// `Deserialize` rejects any wire value outside `"never"` / `"always"` —
/// the derived enum tag match is exhaustive by construction, so an unknown
/// value surfaces as a parse error rather than silently defaulting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum LazyMode {
    /// Compose eagerly: content materializes before the tool reaches
    /// `PATH`. The floor [`LazyModeLadder::resolve`] applies when every
    /// tier of the resolution ladder is absent.
    Never,
    /// Compose a shim: content materializes on the first invocation of one
    /// of the tool's declared names.
    Always,
}

impl fmt::Display for LazyMode {
    /// Formats as the lowercase wire value (e.g. `"never"`, `"always"`).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Never => write!(f, "never"),
            Self::Always => write!(f, "always"),
        }
    }
}

impl FromStr for LazyMode {
    type Err = InvalidLazyModeError;

    /// Parses from the lowercase wire value. Case-sensitive, and so is
    /// `--lazy-mode`: nothing sets `Arg::ignore_case`, so `--lazy-mode Always`
    /// is rejected as an invalid value rather than folded. [`LazyMode::from_env`]
    /// is the only reader that folds case.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "never" => Ok(Self::Never),
            "always" => Ok(Self::Always),
            other => Err(InvalidLazyModeError(other.to_string())),
        }
    }
}

impl LazyMode {
    /// Reads the [`crate::env::keys::OCX_LAZY_MODE`] tier of the resolution
    /// ladder (`plan_lazy_package_loading.md` C-006).
    ///
    /// Parses case-insensitively and **warns and falls back** on an
    /// unrecognized value rather than erroring — the
    /// [`crate::cli::ColorMode::from_args`] idiom, mirroring
    /// [`crate::env::flag`]'s treatment of an invalid boolean.
    ///
    /// `None` means "this tier is absent": the variable is unset, empty, or
    /// carried a value outside `never` / `always`. Absence lets the ladder
    /// continue to its floor — it never short-circuits resolution.
    pub fn from_env() -> Option<Self> {
        let key = crate::env::keys::OCX_LAZY_MODE;
        let value = crate::env::var(key)?;
        if value.is_empty() {
            return None;
        }
        match value.to_ascii_lowercase().parse::<Self>() {
            Ok(mode) => Some(mode),
            Err(error) => {
                crate::log::warn!("Environment variable '{key}' ignored: {error}");
                None
            }
        }
    }
}

/// Invalid [`LazyMode`] wire value, returned by [`FromStr::from_str`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid lazy mode '{0}' (expected 'never' or 'always')")]
pub struct InvalidLazyModeError(String);

impl clap_builder::ValueEnum for LazyMode {
    fn value_variants<'a>() -> &'a [Self] {
        &[Self::Never, Self::Always]
    }

    fn to_possible_value(&self) -> Option<clap_builder::builder::PossibleValue> {
        use clap_builder::builder::PossibleValue;

        Some(match self {
            Self::Never => PossibleValue::new("never"),
            Self::Always => PossibleValue::new("always"),
        })
    }
}

/// Whether a shim's first-invocation materialization renders progress.
///
/// Closed, internal enum — no `#[non_exhaustive]`, same rationale as
/// [`LazyMode`]. Under [`LazyReport::Progress`], opening the controlling
/// terminal (`/dev/tty`, `CONOUT$`) degrades to [`LazyReport::Silent`] on
/// failure, never to an error — `ENXIO` is the documented, common case in
/// Docker builds, CI runners, and anything under `setsid`. Errors still go
/// to stderr regardless of this setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum LazyReport {
    /// No progress channel is opened for a first-invocation materialization.
    Silent,
    /// Render progress for a first-invocation materialization, best-effort.
    Progress,
}

impl fmt::Display for LazyReport {
    /// Formats as the lowercase wire value (e.g. `"silent"`, `"progress"`).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Silent => write!(f, "silent"),
            Self::Progress => write!(f, "progress"),
        }
    }
}

impl FromStr for LazyReport {
    type Err = InvalidLazyReportError;

    /// Parses from the lowercase wire value. Case-sensitive, and so is
    /// `--lazy-report`: nothing sets `Arg::ignore_case`, so `--lazy-report
    /// Progress` is rejected as an invalid value rather than folded.
    /// [`LazyReport::from_env`] is the only reader that folds case.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "silent" => Ok(Self::Silent),
            "progress" => Ok(Self::Progress),
            other => Err(InvalidLazyReportError(other.to_string())),
        }
    }
}

impl LazyReport {
    /// Reads the [`crate::env::keys::OCX_LAZY_REPORT`] tier of the resolution
    /// ladder (`plan_lazy_package_loading.md` C-006).
    ///
    /// Same contract as [`LazyMode::from_env`]: case-insensitive, warn and
    /// fall back on an unrecognized value, `None` meaning "this tier is
    /// absent" so the ladder continues to its floor.
    pub fn from_env() -> Option<Self> {
        let key = crate::env::keys::OCX_LAZY_REPORT;
        let value = crate::env::var(key)?;
        if value.is_empty() {
            return None;
        }
        match value.to_ascii_lowercase().parse::<Self>() {
            Ok(report) => Some(report),
            Err(error) => {
                crate::log::warn!("Environment variable '{key}' ignored: {error}");
                None
            }
        }
    }
}

/// Invalid [`LazyReport`] wire value, returned by [`FromStr::from_str`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid lazy report '{0}' (expected 'silent' or 'progress')")]
pub struct InvalidLazyReportError(String);

impl clap_builder::ValueEnum for LazyReport {
    fn value_variants<'a>() -> &'a [Self] {
        &[Self::Silent, Self::Progress]
    }

    fn to_possible_value(&self) -> Option<clap_builder::builder::PossibleValue> {
        use clap_builder::builder::PossibleValue;

        Some(match self {
            Self::Silent => PossibleValue::new("silent"),
            Self::Progress => PossibleValue::new("progress"),
        })
    }
}

/// One tier's contribution to the `lazy-mode` resolution ladder
/// (`plan_lazy_package_loading.md` C-006), most-specific tier first:
/// CLI flag ▸ `[package."<id>"]` ▸ `[group.<g>]` ▸ toolchain ▸ `OCX_LAZY_MODE`.
///
/// Every field is independently optional, and each `None` means "inherit
/// from the next-less-specific tier" — never "resolves to
/// [`LazyMode::Never`]". Only [`Self::resolve`]'s floor applies that
/// default, and only once every tier is `None`. A five-field struct (rather
/// than five positional `Option<LazyMode>` parameters) so a caller cannot
/// transpose two same-typed tiers by accident.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LazyModeLadder {
    /// `--lazy-mode` on the invoked command.
    pub cli: Option<LazyMode>,
    /// `[package."<id>"].lazy-mode` for the identifier being resolved.
    pub package: Option<LazyMode>,
    /// `[group.<g>].lazy-mode` for the active group.
    pub group: Option<LazyMode>,
    /// Toolchain-tier `lazy-mode` (`ocx.toml`'s top-level scalar).
    pub toolchain: Option<LazyMode>,
    /// `OCX_LAZY_MODE`, already read by the caller via [`LazyMode::from_env`]
    /// — the case-insensitive reader. [`FromStr::from_str`] is case-sensitive
    /// and would drop `OCX_LAZY_MODE=Always`.
    pub environment: Option<LazyMode>,
}

impl LazyModeLadder {
    /// Resolves the ladder most-specific-first per C-006's precedence
    /// order — `--lazy-mode ▸ [package."<id>"] ▸ [group.<g>] ▸ toolchain ▸
    /// OCX_LAZY_MODE ▸ never` — defaulting to the literal `LazyMode::Never`
    /// only once every tier above the floor is absent.
    pub fn resolve(self) -> LazyMode {
        self.cli
            .or(self.package)
            .or(self.group)
            .or(self.toolchain)
            .or(self.environment)
            // The floor is this literal, never `LazyMode::default()` — C-006
            // (ii) dropped the derive so moving the floor cannot go unnoticed.
            .unwrap_or(LazyMode::Never)
    }

    /// Resolves the ladder, then applies the host's shim-support floor —
    /// the form every production caller uses.
    ///
    /// Identical to [`Self::resolve`] except on Windows, where the resolved
    /// mode is forced to [`LazyMode::Never`]: scenario S-010 of
    /// `plan_lazy_package_loading.md` has Windows composing **eagerly** in
    /// this phase. A user who set `lazy-mode = "always"` there gets a working
    /// eager environment and a debug line — never a warning, never an error.
    ///
    /// [`Self::resolve`] stays the pure precedence function so the C-006
    /// ladder tests assert precedence on every host.
    pub fn resolve_for_host(self) -> LazyMode {
        let resolved = self.resolve();
        // Deleted in the same change that adds the Windows shim PRODUCER.
        // Only half the Windows path exists today: `ocx launcher shim` reads
        // a `.shimref` sidecar (WP-11), but nothing writes one —
        // `prepare_lazy::write_shim_launchers` emits an extensionless
        // `#!/bin/sh` body on every platform, so a lazily composed tool would
        // put a directory of non-executable shell scripts on a Windows `PATH`.
        // `cfg!(windows)` and not a probe of the shim tree: the tree may not
        // exist yet at resolution time, and the platform rule has to be
        // greppable rather than inferred from a runtime directory listing.
        if cfg!(windows) && resolved == LazyMode::Always {
            crate::log::debug!(
                "Composing eagerly: lazy-mode resolved to always, but this phase has no Windows shim producer"
            );
            return LazyMode::Never;
        }
        resolved
    }
}

/// One tier's contribution to the `lazy-report` resolution ladder
/// (`plan_lazy_package_loading.md` C-006), most-specific tier first:
/// CLI flag ▸ `[package."<id>"]` ▸ toolchain ▸ `OCX_LAZY_REPORT`.
///
/// **Four tiers, not [`LazyModeLadder`]'s five: there is no `[group.<g>]`
/// tier.** `lazy-mode` is resolved while composing, where the selected group
/// is known; `lazy-report` is resolved inside `ocx launcher shim`, a separate
/// process that receives only a pinned identifier and a basename and cannot
/// learn which group composed the tool. The group tier was therefore settable
/// and unreadable — C-006 (i)'s own defect one tier down — and is removed
/// rather than left to be silently ignored.
///
/// Every field is independently optional, and each `None` means "inherit
/// from the next-less-specific tier" — never "resolves to
/// [`LazyReport::Silent`]". Only [`Self::resolve`]'s floor applies that
/// default, and only once every tier is `None`. A four-field struct (rather
/// than four positional `Option<LazyReport>` parameters) so a caller cannot
/// transpose two same-typed tiers by accident.
///
// Deliberately a second concrete struct, not `Ladder<T>`: `LazyMode` and
// `LazyReport` are two unrelated vocabularies, so sharing a generic here
// would be incidental similarity, not shared logic. A generic ladder would
// also need its own floor mechanism — `T: Default` re-creates exactly the
// hazard the derive removal above eliminates.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LazyReportLadder {
    /// `--lazy-report` on the invoked command.
    pub cli: Option<LazyReport>,
    /// `[package."<id>"].lazy-report` for the identifier being resolved.
    pub package: Option<LazyReport>,
    /// Toolchain-tier `lazy-report` (`ocx.toml`'s top-level scalar).
    pub toolchain: Option<LazyReport>,
    /// `OCX_LAZY_REPORT`, already read by the caller via
    /// [`LazyReport::from_env`] — the case-insensitive reader.
    /// [`FromStr::from_str`] is case-sensitive and would drop
    /// `OCX_LAZY_REPORT=Progress`.
    pub environment: Option<LazyReport>,
}

impl LazyReportLadder {
    /// Resolves the ladder most-specific-first per C-006's precedence
    /// order — `--lazy-report ▸ [package."<id>"] ▸ toolchain ▸
    /// OCX_LAZY_REPORT ▸ silent` — defaulting to the literal
    /// `LazyReport::Silent` only once every tier above the floor is absent.
    pub fn resolve(self) -> LazyReport {
        self.cli
            .or(self.package)
            .or(self.toolchain)
            .or(self.environment)
            // The floor is this literal — same C-006 (ii) rationale as
            // [`LazyModeLadder::resolve`].
            .unwrap_or(LazyReport::Silent)
    }
}

#[cfg(test)]
mod tests {
    //! Contract-first tests for C-005 (the two closed enums) and C-006 (the
    //! two resolution ladders) of `plan_lazy_package_loading.md`.
    //!
    //! Every expectation is written from the contract, never from an
    //! implementation: the ladder tests below fail with `unimplemented` until
    //! `resolve()` is filled in, which is the Specify phase's proof that they
    //! test the contract rather than restate the code.

    use super::*;
    use crate::env::keys;

    // ── C-006: `lazy-mode` ladder precedence ────────────────────────────────
    //
    // One test per tier. Each sets its own tier to `Always` and EVERY
    // less-specific tier to `Never`, so a resolver that consults the tiers in
    // the wrong order returns `Never` and reds. Taken together the six tests
    // pin the full order CLI ▸ package ▸ group ▸ toolchain ▸ environment ▸
    // floor.

    #[test]
    fn lazy_mode_cli_beats_every_less_specific_tier() {
        let ladder = LazyModeLadder {
            cli: Some(LazyMode::Always),
            package: Some(LazyMode::Never),
            group: Some(LazyMode::Never),
            toolchain: Some(LazyMode::Never),
            environment: Some(LazyMode::Never),
        };
        assert_eq!(ladder.resolve(), LazyMode::Always, "--lazy-mode is the top tier");
    }

    #[test]
    fn lazy_mode_package_beats_group_toolchain_and_environment() {
        let ladder = LazyModeLadder {
            cli: None,
            package: Some(LazyMode::Always),
            group: Some(LazyMode::Never),
            toolchain: Some(LazyMode::Never),
            environment: Some(LazyMode::Never),
        };
        assert_eq!(
            ladder.resolve(),
            LazyMode::Always,
            "[package.\"<id>\"] outranks [group.<g>], toolchain and OCX_LAZY_MODE"
        );
    }

    #[test]
    fn lazy_mode_group_beats_toolchain_and_environment() {
        let ladder = LazyModeLadder {
            cli: None,
            package: None,
            group: Some(LazyMode::Always),
            toolchain: Some(LazyMode::Never),
            environment: Some(LazyMode::Never),
        };
        assert_eq!(
            ladder.resolve(),
            LazyMode::Always,
            "[group.<g>] outranks the toolchain tier and OCX_LAZY_MODE"
        );
    }

    #[test]
    fn lazy_mode_toolchain_beats_environment() {
        let ladder = LazyModeLadder {
            cli: None,
            package: None,
            group: None,
            toolchain: Some(LazyMode::Always),
            environment: Some(LazyMode::Never),
        };
        assert_eq!(
            ladder.resolve(),
            LazyMode::Always,
            "the toolchain tier outranks OCX_LAZY_MODE"
        );
    }

    #[test]
    fn lazy_mode_environment_applies_when_no_config_tier_declares() {
        let ladder = LazyModeLadder {
            cli: None,
            package: None,
            group: None,
            toolchain: None,
            environment: Some(LazyMode::Always),
        };
        assert_eq!(
            ladder.resolve(),
            LazyMode::Always,
            "an absent config tier means inherit, so OCX_LAZY_MODE must be reached"
        );
    }

    /// The floor, as a **literal** — never `LazyMode::default()`.
    ///
    /// C-006 (ii) dropped `Default` from the enum precisely so the floor
    /// cannot be read off a derive: a `.unwrap_or_default()` implementation
    /// would answer the derive rather than the contract, and a future move of
    /// the floor would then red nothing. Writing `default()` here would
    /// re-create that hazard inside the test that exists to prevent it.
    #[test]
    fn lazy_mode_all_tiers_absent_resolves_to_the_never_floor() {
        assert_eq!(
            LazyModeLadder::default().resolve(),
            LazyMode::Never,
            "an all-absent ladder resolves to the contract's floor, `never`"
        );
    }

    /// `Some(Never)` is a decision, `None` is an absence — an implementation
    /// that collapses the two (`unwrap_or(Never)` per tier, or treating the
    /// floor value as "unset") passes every test above and fails this one.
    #[test]
    fn lazy_mode_explicit_never_at_a_more_specific_tier_beats_always_below() {
        let ladder = LazyModeLadder {
            cli: None,
            package: Some(LazyMode::Never),
            group: Some(LazyMode::Always),
            toolchain: Some(LazyMode::Always),
            environment: Some(LazyMode::Always),
        };
        assert_eq!(
            ladder.resolve(),
            LazyMode::Never,
            "an explicit `never` at the package tier is a declaration, not an absence"
        );
    }

    // ── S-010: the host's shim-support floor ────────────────────────────────
    //
    // Two host-gated halves rather than one `cfg!(windows)` expectation: an
    // assertion that restates the production `cfg!` is tautological — it agrees
    // with the code on every host, including a host where the code is wrong.
    // Each arm below asserts a literal, and each runs on the host it describes.

    /// `--lazy-mode always` is the MOST specific tier, so forcing `Never` here
    /// proves the host floor overrides every tier, not just the weak ones.
    #[cfg(windows)]
    #[test]
    fn windows_composes_eagerly_whatever_tier_asked_for_always() {
        let ladder = LazyModeLadder {
            cli: Some(LazyMode::Always),
            package: Some(LazyMode::Always),
            group: Some(LazyMode::Always),
            toolchain: Some(LazyMode::Always),
            environment: Some(LazyMode::Always),
        };
        assert_eq!(
            ladder.resolve(),
            LazyMode::Always,
            "the pure ladder still answers the configured value"
        );
        assert_eq!(
            ladder.resolve_for_host(),
            LazyMode::Never,
            "S-010: Windows composes eagerly until the shim producer lands"
        );
    }

    /// The inverse, and the one that reds if the gate is written to fire
    /// everywhere: off Windows, `resolve_for_host` must be `resolve`.
    #[cfg(not(windows))]
    #[test]
    fn a_non_windows_host_composes_lazily_when_a_tier_asked_for_always() {
        let ladder = LazyModeLadder {
            toolchain: Some(LazyMode::Always),
            ..LazyModeLadder::default()
        };
        assert_eq!(
            ladder.resolve_for_host(),
            LazyMode::Always,
            "the host floor is Windows-only; elsewhere it must not touch the resolved mode"
        );
    }

    // ── C-006: `lazy-report` ladder precedence ──────────────────────────────
    //
    // The mirror of the set above, one tier shorter: `lazy-report` has no
    // `[group.<g>]` tier, because the shim process that resolves it cannot
    // learn the composing group (see [`LazyReportLadder`]). It is deliberately
    // a second concrete struct, so its order needs its own proof rather than
    // inheriting `LazyMode`'s.

    #[test]
    fn lazy_report_cli_beats_every_less_specific_tier() {
        let ladder = LazyReportLadder {
            cli: Some(LazyReport::Progress),
            package: Some(LazyReport::Silent),
            toolchain: Some(LazyReport::Silent),
            environment: Some(LazyReport::Silent),
        };
        assert_eq!(ladder.resolve(), LazyReport::Progress, "--lazy-report is the top tier");
    }

    #[test]
    fn lazy_report_package_beats_toolchain_and_environment() {
        let ladder = LazyReportLadder {
            cli: None,
            package: Some(LazyReport::Progress),
            toolchain: Some(LazyReport::Silent),
            environment: Some(LazyReport::Silent),
        };
        assert_eq!(
            ladder.resolve(),
            LazyReport::Progress,
            "[package.\"<id>\"] outranks the toolchain tier and OCX_LAZY_REPORT"
        );
    }

    #[test]
    fn lazy_report_toolchain_beats_environment() {
        let ladder = LazyReportLadder {
            cli: None,
            package: None,
            toolchain: Some(LazyReport::Progress),
            environment: Some(LazyReport::Silent),
        };
        assert_eq!(
            ladder.resolve(),
            LazyReport::Progress,
            "the toolchain tier outranks OCX_LAZY_REPORT"
        );
    }

    #[test]
    fn lazy_report_environment_applies_when_no_config_tier_declares() {
        let ladder = LazyReportLadder {
            cli: None,
            package: None,
            toolchain: None,
            environment: Some(LazyReport::Progress),
        };
        assert_eq!(
            ladder.resolve(),
            LazyReport::Progress,
            "an absent config tier means inherit, so OCX_LAZY_REPORT must be reached"
        );
    }

    /// The floor as a literal — same C-006 (ii) rationale as
    /// [`lazy_mode_all_tiers_absent_resolves_to_the_never_floor`].
    #[test]
    fn lazy_report_all_tiers_absent_resolves_to_the_silent_floor() {
        assert_eq!(
            LazyReportLadder::default().resolve(),
            LazyReport::Silent,
            "an all-absent ladder resolves to the contract's floor, `silent`"
        );
    }

    #[test]
    fn lazy_report_explicit_silent_at_a_more_specific_tier_beats_progress_below() {
        let ladder = LazyReportLadder {
            cli: None,
            package: Some(LazyReport::Silent),
            toolchain: Some(LazyReport::Progress),
            environment: Some(LazyReport::Progress),
        };
        assert_eq!(
            ladder.resolve(),
            LazyReport::Silent,
            "an explicit `silent` at the package tier is a declaration, not an absence"
        );
    }

    // ── C-005: wire vocabulary ──────────────────────────────────────────────

    /// Every variant's `Display` output parses back to that same variant, and
    /// the spellings are the documented lowercase wire values.
    #[test]
    fn lazy_mode_display_and_from_str_round_trip_every_variant() {
        for variant in [LazyMode::Never, LazyMode::Always] {
            let rendered = variant.to_string();
            assert_eq!(
                rendered.parse::<LazyMode>().expect("Display output must parse back"),
                variant,
                "`{rendered}` must round-trip"
            );
        }
        assert_eq!(LazyMode::Never.to_string(), "never");
        assert_eq!(LazyMode::Always.to_string(), "always");
    }

    #[test]
    fn lazy_report_display_and_from_str_round_trip_every_variant() {
        for variant in [LazyReport::Silent, LazyReport::Progress] {
            let rendered = variant.to_string();
            assert_eq!(
                rendered.parse::<LazyReport>().expect("Display output must parse back"),
                variant,
                "`{rendered}` must round-trip"
            );
        }
        assert_eq!(LazyReport::Silent.to_string(), "silent");
        assert_eq!(LazyReport::Progress.to_string(), "progress");
    }

    #[test]
    fn lazy_mode_from_str_rejects_an_unknown_value() {
        let err = "sometimes"
            .parse::<LazyMode>()
            .expect_err("an unknown wire value must not parse");
        assert!(
            err.to_string().contains("sometimes"),
            "the diagnostic must quote the offending value; got {err}"
        );
    }

    #[test]
    fn lazy_report_from_str_rejects_an_unknown_value() {
        let err = "loud"
            .parse::<LazyReport>()
            .expect_err("an unknown wire value must not parse");
        assert!(
            err.to_string().contains("loud"),
            "the diagnostic must quote the offending value; got {err}"
        );
    }

    /// C-005: `Deserialize` **rejects** an unknown value rather than silently
    /// defaulting. Exercised through TOML because `ocx.toml` is the wire the
    /// contract speaks about.
    #[test]
    fn lazy_mode_deserialize_rejects_an_unknown_value() {
        let accepted: LazyMode = toml::Value::String("always".to_string())
            .try_into()
            .expect("a documented value must deserialize");
        assert_eq!(accepted, LazyMode::Always);

        let rejected: Result<LazyMode, _> = toml::Value::String("sometimes".to_string()).try_into();
        assert!(
            rejected.is_err(),
            "`sometimes` must be a deserialize error, never a silent default"
        );
    }

    #[test]
    fn lazy_report_deserialize_rejects_an_unknown_value() {
        let accepted: LazyReport = toml::Value::String("progress".to_string())
            .try_into()
            .expect("a documented value must deserialize");
        assert_eq!(accepted, LazyReport::Progress);

        let rejected: Result<LazyReport, _> = toml::Value::String("loud".to_string()).try_into();
        assert!(
            rejected.is_err(),
            "`loud` must be a deserialize error, never a silent default"
        );
    }

    /// The `ValueEnum` spellings and the `Display` spellings are the same
    /// strings. Drift here would let `--lazy-mode always` and
    /// `lazy-mode = "always"` name different things — the CLI and the config
    /// file are two producers of one vocabulary with no compiler link.
    #[test]
    fn lazy_mode_value_enum_spellings_match_display() {
        let variants = <LazyMode as clap_builder::ValueEnum>::value_variants();
        assert_eq!(variants.len(), 2, "every variant must be offered by the CLI");
        for variant in variants {
            let possible =
                clap_builder::ValueEnum::to_possible_value(variant).expect("no variant is hidden from `--lazy-mode`");
            assert_eq!(
                possible.get_name(),
                variant.to_string(),
                "ValueEnum spelling must equal the Display/serde wire value"
            );
        }
    }

    #[test]
    fn lazy_report_value_enum_spellings_match_display() {
        let variants = <LazyReport as clap_builder::ValueEnum>::value_variants();
        assert_eq!(variants.len(), 2, "every variant must be offered by the CLI");
        for variant in variants {
            let possible =
                clap_builder::ValueEnum::to_possible_value(variant).expect("no variant is hidden from `--lazy-report`");
            assert_eq!(
                possible.get_name(),
                variant.to_string(),
                "ValueEnum spelling must equal the Display/serde wire value"
            );
        }
    }

    // ── C-006: `OCX_LAZY_MODE` / `OCX_LAZY_REPORT` ──────────────────────────

    #[test]
    fn lazy_mode_from_env_parses_case_insensitively() {
        let env = crate::test::env::lock();
        for spelling in ["always", "ALWAYS", "Always"] {
            env.set(keys::OCX_LAZY_MODE, spelling);
            assert_eq!(
                LazyMode::from_env(),
                Some(LazyMode::Always),
                "`OCX_LAZY_MODE={spelling}` must parse case-insensitively"
            );
        }
        env.set(keys::OCX_LAZY_MODE, "NEVER");
        assert_eq!(LazyMode::from_env(), Some(LazyMode::Never));
    }

    #[test]
    fn lazy_mode_from_env_is_absent_when_unset() {
        let env = crate::test::env::lock();
        env.remove(keys::OCX_LAZY_MODE);
        assert_eq!(
            LazyMode::from_env(),
            None,
            "an unset variable is an absent tier, not the floor"
        );
    }

    /// Garbage warns and falls back — it is never an error, and never a
    /// short-circuit to the floor: the tier is simply absent, so the rest of
    /// the ladder still applies.
    #[test]
    fn lazy_mode_from_env_falls_back_on_an_unrecognized_value() {
        let env = crate::test::env::lock();
        env.set(keys::OCX_LAZY_MODE, "sometimes");
        assert_eq!(
            LazyMode::from_env(),
            None,
            "an unrecognized value must warn and fall back, never error"
        );
    }

    /// An exported-but-empty variable (`OCX_LAZY_MODE=` in a CI job's env
    /// block) is the same absence as unset. C-006 does not name this case;
    /// treating it as "absent" is the only reading consistent with
    /// [`crate::env::string`], which already maps empty to the default.
    #[test]
    fn lazy_mode_from_env_treats_an_empty_value_as_absent() {
        let env = crate::test::env::lock();
        env.set(keys::OCX_LAZY_MODE, "");
        assert_eq!(LazyMode::from_env(), None, "an empty value is an absent tier");
    }

    #[test]
    fn lazy_report_from_env_parses_case_insensitively() {
        let env = crate::test::env::lock();
        for spelling in ["progress", "PROGRESS", "Progress"] {
            env.set(keys::OCX_LAZY_REPORT, spelling);
            assert_eq!(
                LazyReport::from_env(),
                Some(LazyReport::Progress),
                "`OCX_LAZY_REPORT={spelling}` must parse case-insensitively"
            );
        }
        env.set(keys::OCX_LAZY_REPORT, "SILENT");
        assert_eq!(LazyReport::from_env(), Some(LazyReport::Silent));
    }

    #[test]
    fn lazy_report_from_env_is_absent_when_unset() {
        let env = crate::test::env::lock();
        env.remove(keys::OCX_LAZY_REPORT);
        assert_eq!(
            LazyReport::from_env(),
            None,
            "an unset variable is an absent tier, not the floor"
        );
    }

    #[test]
    fn lazy_report_from_env_falls_back_on_an_unrecognized_value() {
        let env = crate::test::env::lock();
        env.set(keys::OCX_LAZY_REPORT, "loud");
        assert_eq!(
            LazyReport::from_env(),
            None,
            "an unrecognized value must warn and fall back, never error"
        );
    }

    /// The `lazy-report` mirror of
    /// [`lazy_mode_from_env_treats_an_empty_value_as_absent`].
    #[test]
    fn lazy_report_from_env_treats_an_empty_value_as_absent() {
        let env = crate::test::env::lock();
        env.set(keys::OCX_LAZY_REPORT, "");
        assert_eq!(LazyReport::from_env(), None, "an empty value is an absent tier");
    }
}
