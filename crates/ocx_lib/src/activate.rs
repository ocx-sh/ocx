// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Toolchain activation mode and the two environment tiers of its resolution
//! ladders (`plan_toolchain_activation.md` C-006 / C-007).
//!
//! [`ActivateMode`] decides how a project's rendered toolchain reaches a
//! shell: `env` composes the environment per prompt (today's behaviour),
//! `bin` puts `<home>/toolchain/bin` on `PATH` instead, `none` does neither.
//! [`pinned_from_env`] reads the sibling `pinned` setting, which decides
//! whether composed paths follow the `<group>/<entry>` links or pin to
//! digests.
//!
//! Both readers are the **weakest** tier of their ladder ([`crate::ladder`]),
//! below `ocx.toml`. Neither ever overrides a project that states a value.
//!
//! The two floors are named here as [`ACTIVATE_FLOOR`] and [`PINNED_FLOOR`],
//! and a later package resolving either ladder is expected to pass *those*
//! constants to [`crate::ladder::Ladder::resolve`] rather than a re-spelled
//! literal — a convention checked at review, not by the type.
//!
//! # Not [`crate::activation`]
//!
//! Four letters apart, unrelated subjects. This module owns the `activate`
//! setting's vocabulary and its environment tier; [`crate::activation`]
//! sequences the per-prompt reconciliation of a shell's environment, which is
//! what one of this setting's three values selects.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// How a rendered toolchain home reaches a shell's environment.
///
/// # C-006
///
/// Closed, internal enum — no `#[non_exhaustive]` (`arch-principles.md`
/// "Internal enum exhaustiveness"): `ocx_lib` ships no external API, so every
/// match over `ActivateMode` stays total across the workspace.
///
/// `Deserialize` rejects any wire value outside `"env"` / `"bin"` / `"none"`
/// — the derived enum tag match is exhaustive by construction, so an unknown
/// `ocx.toml` value surfaces as a parse error (C-012, exit 78) rather than
/// silently defaulting.
///
/// Deliberately **no `Default` impl**. The floor is [`ACTIVATE_FLOOR`] and it
/// is applied by [`crate::ladder::Ladder::resolve`]'s `floor` parameter at the
/// one site that resolves the ladder; a `Default` here would be a second floor
/// that no reader of the ladder can see (C-005).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ActivateMode {
    /// Compose the toolchain environment on every prompt — the shipped
    /// behaviour, and the ladder's floor.
    Env,
    /// Put `<home>/toolchain/bin` on `PATH` and compose nothing else, so a
    /// tool is resolved by its launcher trampoline at invocation time.
    Bin,
    /// Neither. The reconciler withdraws whatever it owns and adds nothing.
    None,
}

impl fmt::Display for ActivateMode {
    /// Formats as the lowercase wire value (e.g. `"env"`, `"bin"`, `"none"`).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Env => write!(f, "env"),
            Self::Bin => write!(f, "bin"),
            Self::None => write!(f, "none"),
        }
    }
}

impl FromStr for ActivateMode {
    type Err = InvalidActivateModeError;

    /// Parses from the lowercase wire value. Case-sensitive, exactly as
    /// [`crate::lazy::LazyMode`]'s is: nothing sets `Arg::ignore_case`, so a
    /// flag or an `ocx.toml` value spelled `Bin` is rejected rather than
    /// folded. [`ActivateMode::from_env`] is the only reader that folds case.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "env" => Ok(Self::Env),
            "bin" => Ok(Self::Bin),
            "none" => Ok(Self::None),
            other => Err(InvalidActivateModeError(other.to_string())),
        }
    }
}

impl ActivateMode {
    /// Reads the [`crate::env::keys::OCX_TOOLCHAIN_ACTIVATE`] tier of the
    /// `activate` ladder (C-006).
    ///
    /// The shipped [`crate::lazy::LazyMode::from_env`] idiom, verbatim:
    ///
    /// - Read through [`crate::env::var`], never `std::env::var` — that
    ///   function carries the `#[cfg(test)]` override seam, and reading the
    ///   process environment directly would make every unit test of this
    ///   ladder order-dependent inside nextest's shared process.
    /// - An **empty** value returns `None` before parsing and without a
    ///   warning: `OCX_TOOLCHAIN_ACTIVATE=` is "unset", not "invalid".
    /// - An unrecognised value **warns and returns `None`**, exit 0. `None`
    ///   means *this tier is absent*, so the ladder continues to the next tier
    ///   — it never short-circuits to the floor.
    /// - Whitespace is **not** trimmed, because the shipped reader does not
    ///   trim: ` bin` is an unrecognised value and says so.
    ///
    /// Case is folded with [`str::to_ascii_lowercase`]. See
    /// [`pinned_from_env`] for why the sibling reader folds differently.
    pub fn from_env() -> Option<Self> {
        let key = crate::env::keys::OCX_TOOLCHAIN_ACTIVATE;
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

/// The floor of the `activate` ladder (C-007): compose the environment per
/// prompt, today's behaviour.
///
/// Named so the floor of this setting is written once. `ActivateMode` has no
/// `Default` on purpose (C-005), so without this constant the floor would be
/// whatever each caller happens to spell.
///
/// Passing it is a **convention** — [`crate::ladder::Ladder`] takes the floor as
/// a plain parameter and accepts any value, deliberately — and the convention
/// has one enforcement point per setting: the **shared resolver**.
/// [`crate::activation::activate_mode`] owns this floor;
/// [`crate::package_manager::pinned_for_project`] owns [`PINNED_FLOOR`]. Every
/// other site calls one of those, so a re-spelled floor is a *new* `Ladder`
/// construction and shows up as one — see
/// `the_ladder_is_constructed_only_inside_the_two_shared_resolvers`, which
/// asserts it rather than asking a reader to.
pub const ACTIVATE_FLOOR: ActivateMode = ActivateMode::Env;

/// The floor of the `pinned` ladder (C-007): follow the rendered
/// `<group>/<entry>` links rather than pinning composed paths to digest roots.
///
/// Named for the reason [`ACTIVATE_FLOOR`] is, and the reason
/// [`pinned_from_env`] yields `Option<bool>` rather than `bool`: "unset" and
/// "explicitly false" must stay distinguishable all the way down the ladder,
/// and only this value is allowed to answer for a tier that never spoke.
pub const PINNED_FLOOR: bool = false;

/// Invalid [`ActivateMode`] wire value, returned by [`FromStr::from_str`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid activate mode '{0}' (expected 'env', 'bin' or 'none')")]
pub struct InvalidActivateModeError(String);

impl clap_builder::ValueEnum for ActivateMode {
    fn value_variants<'a>() -> &'a [Self] {
        &[Self::Env, Self::Bin, Self::None]
    }

    fn to_possible_value(&self) -> Option<clap_builder::builder::PossibleValue> {
        use clap_builder::builder::PossibleValue;

        Some(match self {
            Self::Env => PossibleValue::new("env"),
            Self::Bin => PossibleValue::new("bin"),
            Self::None => PossibleValue::new("none"),
        })
    }
}

/// Reads the [`crate::env::keys::OCX_TOOLCHAIN_PINNED`] tier of the `pinned`
/// ladder (C-007).
///
/// Same contract as [`ActivateMode::from_env`]: read through
/// [`crate::env::var`], empty is absent, an unparseable value warns and
/// returns `None` so the ladder continues to the next tier.
///
/// Parsing goes through the shipped [`crate::utility::boolean_string::BooleanString`],
/// which already defines ocx's boolean vocabulary (`1|y|yes|on|true` /
/// `0|n|no|off|false`) — inventing a second one here would mean two spellings
/// of "truthy" in one product.
///
/// **Not [`crate::env::flag`]**, which collapses absent and false into one
/// `bool`. A ladder tier needs `Option<bool>`: "unset" must fall through to
/// the floor, while an explicit `OCX_TOOLCHAIN_PINNED=false` must *win* over
/// the floor when no more specific tier speaks. `flag` cannot express that
/// difference.
///
/// # A deliberate, unobservable folding difference
///
/// `BooleanString` folds with full Unicode [`str::to_lowercase`];
/// [`ActivateMode::from_env`] folds with ASCII-only
/// [`str::to_ascii_lowercase`]. Both vocabularies are pure ASCII, so no input
/// can distinguish the two — recorded here only so that nobody "fixes" one of
/// them into agreement with the other and calls it a bug fix.
pub fn pinned_from_env() -> Option<bool> {
    let key = crate::env::keys::OCX_TOOLCHAIN_PINNED;
    let value = crate::env::var(key)?;
    if value.is_empty() {
        return None;
    }
    match crate::utility::boolean_string::BooleanString::try_from(value.as_str()) {
        Ok(boolean) => Some(boolean.into()),
        Err(error) => {
            crate::log::warn!("Environment variable '{key}' ignored: {error}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::env::keys;
    use crate::ladder::Ladder;

    // ── C-006: one wire vocabulary, four producers ──────────────────────────

    /// C-006: `Display`, `FromStr`, serde and `ValueEnum` spell every variant
    /// the same way. Modelled on `lazy.rs`'s pair of spelling tests.
    ///
    /// Four independent producers of one vocabulary with **no compiler link**
    /// between them: `--activate bin`, `activate = "bin"` in `ocx.toml`, the
    /// `OCX_TOOLCHAIN_ACTIVATE=bin` reader and a rendered value all have to be
    /// the same three strings, or two of them name different things. The
    /// `#[serde(rename_all = "lowercase")]` attribute in particular is a
    /// separate spelling from the hand-written `Display` arms and can drift
    /// from them silently.
    #[test]
    fn activate_mode_spells_every_variant_the_same_way_in_all_four_producers() {
        for (variant, wire) in [
            (ActivateMode::Env, "env"),
            (ActivateMode::Bin, "bin"),
            (ActivateMode::None, "none"),
        ] {
            assert_eq!(variant.to_string(), wire, "Display must render the wire value");
            assert_eq!(
                wire.parse::<ActivateMode>().expect("the wire value must parse back"),
                variant,
                "`{wire}` must round-trip through FromStr"
            );
            assert_eq!(
                serde_json::to_value(variant).expect("serialize"),
                serde_json::Value::String(wire.to_string()),
                "serde must agree with Display on `{wire}`"
            );
            assert_eq!(
                toml::Value::String(wire.to_string())
                    .try_into::<ActivateMode>()
                    .expect("a documented value must deserialize"),
                variant,
                "`ocx.toml` is the wire this vocabulary is spoken on"
            );
            assert_eq!(
                clap_builder::ValueEnum::to_possible_value(&variant)
                    .expect("no variant is hidden from the CLI")
                    .get_name(),
                wire,
                "the ValueEnum spelling must equal the Display/serde wire value"
            );
        }

        let variants = <ActivateMode as clap_builder::ValueEnum>::value_variants();
        assert_eq!(variants.len(), 3, "every variant must be offered by the CLI");
    }

    /// C-006 / C-012: an unknown value is a **parse error**, never a silent
    /// default — asserted on both wires that can carry one.
    #[test]
    fn activate_mode_rejects_an_unknown_value_on_every_wire() {
        let error = "sometimes"
            .parse::<ActivateMode>()
            .expect_err("an unknown wire value must not parse");
        assert!(
            error.to_string().contains("sometimes"),
            "the diagnostic must quote the offending value; got {error}"
        );

        let rejected: Result<ActivateMode, _> = toml::Value::String("sometimes".to_string()).try_into();
        assert!(
            rejected.is_err(),
            "`activate = \"sometimes\"` must be a deserialize error, never a silent default"
        );
    }

    // ── C-006: `OCX_TOOLCHAIN_ACTIVATE` ─────────────────────────────────────
    //
    // Every reader test goes through `crate::test::env::lock()`, the crate's
    // `#[cfg(test)]` override seam for `crate::env::var` — never
    // `std::env::set_var`, which is process-global and racy across a test
    // binary's threads.
    //
    // **One property has no reachable red at this layer and is not claimed
    // here:** "warns" versus "does not warn". `crate::log` is the process-wide
    // `log` facade, and the crate's only capture (`oci/index/local_index.rs`)
    // installs the single global logger with an `expect` that nothing else
    // does — a second installer would make these tests order-dependent. Each
    // test below therefore asserts the returned `Option`, which is the half a
    // caller can observe.

    /// C-006: an unset variable is an absent tier, not the floor.
    #[test]
    fn activate_from_env_is_absent_when_the_variable_is_unset() {
        let env = crate::test::env::lock();
        env.remove(keys::OCX_TOOLCHAIN_ACTIVATE);
        assert_eq!(
            ActivateMode::from_env(),
            None,
            "an unset variable is an absent tier, so the ladder continues"
        );
    }

    /// C-006: `OCX_TOOLCHAIN_ACTIVATE=` — an exported-but-empty variable, the
    /// shape a CI job's env block produces — is the same absence as unset, and
    /// is decided **before** parsing.
    #[test]
    fn activate_from_env_treats_an_empty_value_as_absent() {
        let env = crate::test::env::lock();
        env.set(keys::OCX_TOOLCHAIN_ACTIVATE, "");
        assert_eq!(ActivateMode::from_env(), None, "an empty value is an absent tier");
    }

    /// C-006: the value is **not** trimmed. ` bin` is an unrecognised value,
    /// not `bin` — the discriminating input, because a whitespace-only value
    /// answers `None` either way.
    #[test]
    fn activate_from_env_does_not_trim_a_padded_value() {
        let env = crate::test::env::lock();
        for padded in [" bin", "bin ", "\tbin", "   "] {
            env.set(keys::OCX_TOOLCHAIN_ACTIVATE, padded);
            assert_eq!(
                ActivateMode::from_env(),
                None,
                "`OCX_TOOLCHAIN_ACTIVATE={padded:?}` must not be trimmed into a recognised value"
            );
        }
    }

    /// C-006 / C-015: ASCII case is folded, so `Bin` is `bin`.
    #[test]
    fn activate_from_env_folds_ascii_case() {
        let env = crate::test::env::lock();
        for spelling in ["bin", "Bin", "BIN", "bIn"] {
            env.set(keys::OCX_TOOLCHAIN_ACTIVATE, spelling);
            assert_eq!(
                ActivateMode::from_env(),
                Some(ActivateMode::Bin),
                "`OCX_TOOLCHAIN_ACTIVATE={spelling}` must fold to `bin`"
            );
        }
        env.set(keys::OCX_TOOLCHAIN_ACTIVATE, "ENV");
        assert_eq!(ActivateMode::from_env(), Some(ActivateMode::Env));
        env.set(keys::OCX_TOOLCHAIN_ACTIVATE, "None");
        assert_eq!(ActivateMode::from_env(), Some(ActivateMode::None));
    }

    /// C-006: an unrecognised value warns and falls back — never an error,
    /// never a short-circuit to the floor.
    ///
    /// `None` rather than `Some(ACTIVATE_FLOOR)` is the whole assertion. The
    /// two readings are indistinguishable through
    /// [`crate::ladder::Ladder::resolve`] — the environment tier is the
    /// weakest, so nothing sits below it for a stated floor value to outrank —
    /// which is exactly why the discrimination has to be made here, on the
    /// reader's own return value.
    #[test]
    fn activate_from_env_falls_back_to_absent_on_an_unrecognised_value() {
        let env = crate::test::env::lock();
        for garbage in ["sometimes", "Bin;rm -rf /", "binn", "0"] {
            env.set(keys::OCX_TOOLCHAIN_ACTIVATE, garbage);
            assert_eq!(
                ActivateMode::from_env(),
                None,
                "`OCX_TOOLCHAIN_ACTIVATE={garbage:?}` must be absent, never the floor and never an error"
            );
        }
    }

    /// C-006 / C-007: the two keys have separate vocabularies. A value that is
    /// valid for `OCX_TOOLCHAIN_PINNED` is refused here — one shared parser
    /// would admit it.
    #[test]
    fn activate_from_env_refuses_a_value_that_belongs_to_the_pinned_key() {
        let env = crate::test::env::lock();
        for foreign in ["true", "false", "1", "0", "yes", "off"] {
            env.set(keys::OCX_TOOLCHAIN_ACTIVATE, foreign);
            assert_eq!(
                ActivateMode::from_env(),
                None,
                "`{foreign}` belongs to OCX_TOOLCHAIN_PINNED and must not parse as an activate mode"
            );
        }
    }

    /// C-006 / C-015: a non-ASCII lookalike is refused.
    ///
    /// **Honest about what this pins.** It asserts the *rejection*, not the
    /// folding rule: `env` / `bin` / `none` are pure ASCII, so full-Unicode
    /// [`str::to_lowercase`] and ASCII-only [`str::to_ascii_lowercase`] agree
    /// on every one of these inputs — fullwidth `ＢＩＮ` lowercases to `ｂｉｎ`
    /// and the dotless `ı` in `bın` is already lowercase. No input can
    /// distinguish the two folds for this vocabulary, so no test here can.
    #[test]
    fn activate_from_env_refuses_a_non_ascii_lookalike() {
        let env = crate::test::env::lock();
        for lookalike in ["ＢＩＮ", "bın", "ｂｉｎ", "ｅｎｖ"] {
            env.set(keys::OCX_TOOLCHAIN_ACTIVATE, lookalike);
            assert_eq!(
                ActivateMode::from_env(),
                None,
                "`OCX_TOOLCHAIN_ACTIVATE={lookalike:?}` is not one of the three wire values"
            );
        }
    }

    // ── C-007: `OCX_TOOLCHAIN_PINNED` ───────────────────────────────────────

    /// C-007: an unset variable is an absent tier.
    #[test]
    fn pinned_from_env_is_absent_when_the_variable_is_unset() {
        let env = crate::test::env::lock();
        env.remove(keys::OCX_TOOLCHAIN_PINNED);
        assert_eq!(
            pinned_from_env(),
            None,
            "an unset variable is an absent tier, so the ladder continues"
        );
    }

    /// C-007: an exported-but-empty value is absent, before parsing.
    #[test]
    fn pinned_from_env_treats_an_empty_value_as_absent() {
        let env = crate::test::env::lock();
        env.set(keys::OCX_TOOLCHAIN_PINNED, "");
        assert_eq!(pinned_from_env(), None, "an empty value is an absent tier");
    }

    /// C-007: the value is not trimmed — ` true` is an unrecognised value.
    #[test]
    fn pinned_from_env_does_not_trim_a_padded_value() {
        let env = crate::test::env::lock();
        for padded in [" true", "true ", "\tfalse", "   "] {
            env.set(keys::OCX_TOOLCHAIN_PINNED, padded);
            assert_eq!(
                pinned_from_env(),
                None,
                "`OCX_TOOLCHAIN_PINNED={padded:?}` must not be trimmed into a recognised value"
            );
        }
    }

    /// C-007: the shipped boolean vocabulary, truthy half.
    #[test]
    fn pinned_from_env_accepts_the_shipped_truthy_vocabulary() {
        let env = crate::test::env::lock();
        for truthy in ["1", "y", "yes", "on", "true"] {
            env.set(keys::OCX_TOOLCHAIN_PINNED, truthy);
            assert_eq!(
                pinned_from_env(),
                Some(true),
                "`OCX_TOOLCHAIN_PINNED={truthy}` is the shipped truthy vocabulary"
            );
        }
    }

    /// C-007: an **explicit false is `Some(false)`, not absent** — the case
    /// that makes the ladder's "the file tier's explicit false beats the
    /// environment tier's true" observable at all.
    ///
    /// This is why the reader yields `Option<bool>` rather than going through
    /// [`crate::env::flag`], which collapses absent and false into one `bool`.
    #[test]
    fn pinned_from_env_reports_an_explicit_false_as_some_false_not_absent() {
        let env = crate::test::env::lock();
        for falsy in ["0", "n", "no", "off", "false"] {
            env.set(keys::OCX_TOOLCHAIN_PINNED, falsy);
            assert_eq!(
                pinned_from_env(),
                Some(false),
                "`OCX_TOOLCHAIN_PINNED={falsy}` is an explicit false, never an absent tier"
            );
        }
    }

    /// C-007 / C-015: case is folded.
    #[test]
    fn pinned_from_env_folds_case() {
        let env = crate::test::env::lock();
        for spelling in ["TRUE", "True", "tRuE", "YES", "On"] {
            env.set(keys::OCX_TOOLCHAIN_PINNED, spelling);
            assert_eq!(
                pinned_from_env(),
                Some(true),
                "`OCX_TOOLCHAIN_PINNED={spelling}` must fold to a truthy value"
            );
        }
        for spelling in ["FALSE", "False", "OFF", "No"] {
            env.set(keys::OCX_TOOLCHAIN_PINNED, spelling);
            assert_eq!(
                pinned_from_env(),
                Some(false),
                "`OCX_TOOLCHAIN_PINNED={spelling}` must fold to a falsy value"
            );
        }
    }

    /// C-007: a value that belongs to `OCX_TOOLCHAIN_ACTIVATE` is refused here.
    #[test]
    fn pinned_from_env_refuses_a_value_that_belongs_to_the_activate_key() {
        let env = crate::test::env::lock();
        for foreign in ["bin", "env", "none"] {
            env.set(keys::OCX_TOOLCHAIN_PINNED, foreign);
            assert_eq!(
                pinned_from_env(),
                None,
                "`{foreign}` belongs to OCX_TOOLCHAIN_ACTIVATE and must not parse as a boolean"
            );
        }
    }

    /// C-007 / C-015: a non-ASCII lookalike is refused. Same honesty as
    /// [`activate_from_env_refuses_a_non_ascii_lookalike`]: the boolean
    /// vocabulary is pure ASCII too, so this pins the rejection and not the
    /// choice of fold.
    #[test]
    fn pinned_from_env_refuses_a_non_ascii_lookalike() {
        let env = crate::test::env::lock();
        for lookalike in ["ＴＲＵＥ", "ｔｒｕｅ", "yｅs", "оn"] {
            env.set(keys::OCX_TOOLCHAIN_PINNED, lookalike);
            assert_eq!(
                pinned_from_env(),
                None,
                "`OCX_TOOLCHAIN_PINNED={lookalike:?}` is not in the boolean vocabulary"
            );
        }
    }

    // ── C-007: the weakest-tier rule, composed ──────────────────────────────
    //
    // The sentence users get wrong. Every fixture below reads its environment
    // tier through the real reader, so a ladder assembled from a reader that
    // stopped folding case or stopped refusing a foreign value reds here too.

    /// C-007: `ocx.toml activate = "bin"` + `OCX_TOOLCHAIN_ACTIVATE=none`
    /// resolves to **`Bin`**. The environment variable is the weakest tier.
    #[test]
    fn an_ocx_toml_activate_beats_the_environment_tier() {
        let env = crate::test::env::lock();
        env.set(keys::OCX_TOOLCHAIN_ACTIVATE, "none");
        let ladder = Ladder {
            cli: None,
            file: Some(ActivateMode::Bin),
            environment: ActivateMode::from_env(),
        };
        assert_eq!(
            ladder.environment,
            Some(ActivateMode::None),
            "precondition: the environment tier really did read `none`"
        );
        assert_eq!(
            ladder.resolve(ACTIVATE_FLOOR),
            ActivateMode::Bin,
            "`ocx.toml` states a value, so OCX_TOOLCHAIN_ACTIVATE must lose"
        );
    }

    /// C-007: `ocx.toml pinned = false` + `OCX_TOOLCHAIN_PINNED=true`
    /// resolves to **`false`**. The file's explicit false beats the
    /// environment's true — the case that only works because
    /// [`pinned_from_env`] can say `Some(false)`.
    #[test]
    fn an_ocx_toml_pinned_false_beats_an_environment_pinned_true() {
        let env = crate::test::env::lock();
        env.set(keys::OCX_TOOLCHAIN_PINNED, "true");
        let ladder = Ladder {
            cli: None,
            file: Some(false),
            environment: pinned_from_env(),
        };
        assert_eq!(
            ladder.environment,
            Some(true),
            "precondition: the environment tier really did read `true`"
        );
        assert!(
            !ladder.resolve(PINNED_FLOOR),
            "`ocx.toml pinned = false` outranks OCX_TOOLCHAIN_PINNED=true"
        );
    }

    /// C-007: `--pinned` + `ocx.toml pinned = false` + `OCX_TOOLCHAIN_PINNED=false`
    /// resolves to **`true`**. The flag is the most specific tier.
    #[test]
    fn the_pinned_flag_beats_both_the_file_and_the_environment_tier() {
        let env = crate::test::env::lock();
        env.set(keys::OCX_TOOLCHAIN_PINNED, "false");
        let ladder = Ladder {
            cli: Some(true),
            file: Some(false),
            environment: pinned_from_env(),
        };
        assert_eq!(
            ladder.environment,
            Some(false),
            "precondition: the environment tier really did read `false`"
        );
        assert!(
            ladder.resolve(PINNED_FLOOR),
            "`--pinned` outranks both `ocx.toml` and OCX_TOOLCHAIN_PINNED"
        );
    }

    /// C-006 / C-007: `ocx.toml activate = "bin"` + an unrecognised
    /// `OCX_TOOLCHAIN_ACTIVATE` still resolves to **`Bin`**.
    ///
    /// The `assert_eq!` on `ladder.environment` is what discriminates. Through
    /// `resolve` alone, "unrecognised = absent" and "unrecognised = the floor"
    /// give the same answer for **every** input, because the environment tier
    /// is the weakest and nothing sits below it but the floor. So the composed
    /// assertion states the user-visible outcome and the field assertion states
    /// the property that has a reachable red.
    #[test]
    fn an_unrecognised_environment_activate_leaves_the_ocx_toml_value_standing() {
        let env = crate::test::env::lock();
        env.set(keys::OCX_TOOLCHAIN_ACTIVATE, "garbage");
        let ladder = Ladder {
            cli: None,
            file: Some(ActivateMode::Bin),
            environment: ActivateMode::from_env(),
        };
        assert_eq!(
            ladder.environment, None,
            "an unrecognised value makes the tier absent — it must not become the floor"
        );
        assert_eq!(
            ladder.resolve(ACTIVATE_FLOOR),
            ActivateMode::Bin,
            "a project that states `activate = \"bin\"` keeps it whatever the environment says"
        );
    }

    /// C-007: the two floors, pinned as constants so a later package cannot
    /// install one of its own by writing a literal at its resolution site.
    #[test]
    fn the_two_named_floors_are_env_and_false() {
        assert_eq!(
            ACTIVATE_FLOOR,
            ActivateMode::Env,
            "the `activate` floor is `env` — compose the environment per prompt"
        );
        const { assert!(!PINNED_FLOOR, "the `pinned` floor is `false` — follow the links") };
        assert_eq!(
            Ladder::<ActivateMode>::default().resolve(ACTIVATE_FLOOR),
            ActivateMode::Env,
            "an all-absent `activate` ladder resolves to `env`"
        );
        assert!(
            !Ladder::<bool>::default().resolve(PINNED_FLOOR),
            "an all-absent `pinned` ladder resolves to `false`"
        );
    }

    /// C-007 — outside this module's own file and the ladder's, a
    /// [`Ladder`] is constructed **only** inside the two shared resolvers.
    ///
    /// [`ACTIVATE_FLOOR`]'s doc used to say "no production site resolves this
    /// ladder yet"; six did by the time the branch landed, and two of them
    /// cited that sentence as their authority. A count in a comment is a test
    /// with no assertion — so the count is gone and this is the assertion.
    ///
    /// What it defends is the drift this module's own doc names: a call site
    /// that re-spells `Ladder { cli: None, .. }.resolve(FLOOR)` installs a
    /// second floor no reader of the named constant can see, and answers the
    /// tier order for itself. `shell_state.rs`, `pull.rs` and `mutate.rs` each
    /// carried exactly that shape.
    ///
    /// RED: put any `Ladder { .. }` back at a call site — the test fails
    /// naming the file and line. Proven by re-spelling `mutate.rs`'s.
    #[test]
    fn the_ladder_is_constructed_only_inside_the_two_shared_resolvers() {
        /// The type's own file and its floors' file — every `Ladder` in either
        /// is the definition or a unit test that varies the floor on purpose —
        /// then the two shared resolvers, and nothing else.
        const ALLOWED: &[&str] = &[
            "ocx_lib/src/ladder.rs",
            "ocx_lib/src/activate.rs",
            "ocx_lib/src/activation.rs",
            "ocx_lib/src/package_manager/composer.rs",
        ];

        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates/ocx_lib has a parent")
            .to_path_buf();
        let mut offenders: Vec<String> = Vec::new();
        // Both trees asserted present rather than skipped: a scan that finds
        // nothing because it looked nowhere is the green that never ran.
        for source in ["ocx_lib/src", "ocx_cli/src"] {
            let root = crates.join(source);
            assert!(root.is_dir(), "{} is not a directory", root.display());
            scan_for_ladder_constructions(&root, &crates, ALLOWED, &mut offenders);
        }

        assert!(
            offenders.is_empty(),
            "a `Ladder` is constructed outside the two shared resolvers — call \
             `activate_mode` or `pinned_for_project` instead of re-spelling the \
             ladder and its floor:\n{}",
            offenders.join("\n")
        );
    }

    /// Walk `dir` and record every non-comment line constructing a `Ladder`
    /// in a file outside `allowed`.
    ///
    /// The needle is the identifier `Ladder` immediately followed by `{`, `<`
    /// or `:` — a struct literal, a turbofish or a path — with a preceding
    /// character that is not part of an identifier, so `LazyModeLadder` and
    /// `LazyReportLadder` (a different type, with its own floor) do not match,
    /// and `use crate::ladder::Ladder;` does not either.
    fn scan_for_ladder_constructions(
        dir: &std::path::Path,
        crates: &std::path::Path,
        allowed: &[&str],
        offenders: &mut Vec<String>,
    ) {
        for entry in std::fs::read_dir(dir).expect("the source tree is readable") {
            let path = entry.expect("a readable directory entry").path();
            if path.is_dir() {
                scan_for_ladder_constructions(&path, crates, allowed, offenders);
                continue;
            }
            if path.extension().is_none_or(|extension| extension != "rs") {
                continue;
            }
            let relative = path
                .strip_prefix(crates)
                .expect("every scanned file is under crates/")
                .to_string_lossy()
                .replace('\\', "/");
            if allowed.contains(&relative.as_str()) {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("a readable source file");
            for (number, line) in text.lines().enumerate() {
                // Doc comments quote the forbidden shape on purpose — this
                // module's own does, one screen up.
                if line.trim_start().starts_with("//") {
                    continue;
                }
                if constructs_a_ladder(line) {
                    offenders.push(format!("  {relative}:{}: {}", number + 1, line.trim()));
                }
            }
        }
    }

    /// Whether `line` names the bare `Ladder` type in constructing position.
    fn constructs_a_ladder(line: &str) -> bool {
        line.match_indices("Ladder").any(|(at, _)| {
            let preceded = line[..at]
                .chars()
                .next_back()
                .is_some_and(|character| character.is_alphanumeric() || character == '_');
            let follows = line[at + "Ladder".len()..].trim_start();
            !preceded && (follows.starts_with('{') || follows.starts_with('<') || follows.starts_with("::"))
        })
    }
}
