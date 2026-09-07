// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! [`Ladder<T>`] — the three-tier resolution ladder shared by the toolchain
//! `activate` and `pinned` settings (`plan_toolchain_activation.md` C-005).
//!
//! Deliberately *not* a generalisation of [`crate::lazy::LazyModeLadder`] /
//! [`crate::lazy::LazyReportLadder`]: those carry five and four tiers of two
//! unrelated vocabularies, and collapsing them here would be incidental
//! similarity rather than shared logic. This type exists because `activate`
//! and `pinned` genuinely share one shape — CLI ▸ file ▸ environment — and
//! `lazy.rs`'s objection to a generic ladder ("a generic ladder would also
//! need its own floor mechanism — `T: Default` re-creates exactly the hazard
//! the derive removal above eliminates") is answered by taking the floor as a
//! parameter of [`Ladder::resolve`], never from a `Default` bound.

/// One resolution ladder over three tiers, most specific first.
///
/// # C-005
///
/// `cli ▸ file ▸ environment ▸ floor`. Every field is independently optional
/// and each `None` means "inherit from the next-less-specific tier" — never
/// "resolves to the floor". Only [`Self::resolve`] applies the floor, and
/// only once every tier is `None`.
///
/// **The `environment` tier is the weakest, not an override.** `OCX_*` here
/// sits *below* the file tier, so an exported `OCX_TOOLCHAIN_ACTIVATE` loses
/// to an `ocx.toml` that states a value, and only decides the outcome for a
/// project that states none. This is the opposite of the usual
/// "environment beats config" reflex and is the entire reason the tier is
/// named `environment` rather than `env` or `override`.
///
/// # Why named fields
///
/// The three tiers are the same type, so three positional parameters — or a
/// tuple, or an array — would let a caller transpose two of them with no
/// error anywhere. A struct literal naming each field cannot. This is the
/// documented reason [`crate::lazy::LazyModeLadder`] has named fields, and it
/// applies with more force here because `T` is generic.
///
/// # Why not `Copy`
///
/// `Copy` would bind `T: Copy` at every use and quietly forbid a future
/// `Ladder<PathBuf>` or `Ladder<String>` — a resolution ladder over a path is
/// exactly the shape `toolchain-dir` will want. `Clone` costs a call and
/// forbids nothing.
///
/// # Tier population is a fact about callers, not a dead field
///
/// The `activate` ladder leaves `cli` `None`: there is no `--activate` flag,
/// by design (C-006/C-042 put the choice in `ocx.toml` and `ocx self setup`).
/// The `pinned` ladder uses all three — `--pinned` ▸ `ocx.toml` `pinned` ▸
/// `OCX_TOOLCHAIN_PINNED`. A tier no caller populates is not evidence the
/// field is unused; it is one of two callers stating it has no such input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ladder<T> {
    /// The flag on the invoked command, when the setting has one.
    pub cli: Option<T>,
    /// The `ocx.toml` key.
    pub file: Option<T>,
    /// The `OCX_*` variable — **the weakest tier**, below the file tier.
    /// Already read by the caller through the case-folding reader
    /// ([`crate::activate::ActivateMode::from_env`],
    /// [`crate::activate::pinned_from_env`]), never through `FromStr` alone.
    pub environment: Option<T>,
}

/// All tiers absent.
///
/// Hand-written rather than derived on purpose: `#[derive(Default)]` on a
/// generic struct emits a `T: Default` bound even though `Option<T>: Default`
/// needs none, and that bound is exactly the hazard C-005 forbids — it would
/// make `Ladder::<ActivateMode>::default()` demand an `ActivateMode: Default`
/// impl, i.e. a second, silent floor living on the value type where nobody
/// resolving the ladder would see it.
impl<T> Default for Ladder<T> {
    fn default() -> Self {
        Self {
            cli: None,
            file: None,
            environment: None,
        }
    }
}

impl<T> Ladder<T> {
    /// Resolves most-specific-first — `cli ▸ file ▸ environment ▸ floor`
    /// (C-005).
    ///
    /// The order is `cli` first, then `file`, then `environment`, then `floor`,
    /// and nothing else: transposing any two adjacent tiers changes the answer
    /// for the input where exactly those two disagree.
    ///
    /// `floor` is a **parameter**, never `T::default()`: the floor differs per
    /// setting and belongs at the resolution site where the reader can see it,
    /// not on the value type where it becomes invisible to every caller.
    ///
    /// The type enforces nothing about *which* value arrives here —
    /// `the_floor_is_a_parameter_not_a_property_of_the_value_type` passes
    /// arbitrary floors on purpose, and that freedom is the point of the
    /// parameter. The convention layered on top is a review rule: a production
    /// site resolving the `activate` or `pinned` ladder passes
    /// [`crate::activate::ACTIVATE_FLOOR`] or
    /// [`crate::activate::PINNED_FLOOR`], never a re-spelled literal, so the
    /// floor of a setting is defined once. There are no such sites yet; the
    /// first one to appear is where that rule is checked.
    pub fn resolve(self, floor: T) -> T {
        self.cli.or(self.file).or(self.environment).unwrap_or(floor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::activate::{ACTIVATE_FLOOR, ActivateMode, PINNED_FLOOR};

    // ── C-005: `Ladder<T>` resolution ───────────────────────────────────────
    //
    // The precedence pair below is written the way `lazy.rs`'s ladder tests
    // are: each test sets its own tier to one value and EVERY less-specific
    // tier to a *different* one, so a resolver that consults the tiers in the
    // wrong order returns the other value and reds. The floor test and the
    // single-tier test deliberately populate at most one tier, so transposing
    // two `.or()` operands cannot reach them — that separation is what lets a
    // transposition mutation red precedence and nothing else.

    /// C-005: every tier `None` means the floor, and only then.
    #[test]
    fn an_all_absent_ladder_resolves_to_the_floor_parameter() {
        assert_eq!(
            Ladder::<ActivateMode>::default().resolve(ACTIVATE_FLOOR),
            ActivateMode::Env,
            "an all-absent ladder resolves to the floor the caller passed"
        );
        assert!(
            !Ladder::<bool>::default().resolve(PINNED_FLOOR),
            "the `pinned` ladder's floor is `false`"
        );
    }

    /// C-005: `None` on the two more specific tiers means *inherit*, never
    /// "resolve to the floor" — the weakest tier still decides the outcome.
    #[test]
    fn only_the_weakest_tier_set_resolves_to_that_tier_not_the_floor() {
        let ladder = Ladder {
            cli: None,
            file: None,
            environment: Some(ActivateMode::Bin),
        };
        assert_eq!(
            ladder.resolve(ACTIVATE_FLOOR),
            ActivateMode::Bin,
            "an absent cli and file tier mean inherit, so `environment` must be reached"
        );
    }

    /// C-005: `cli ▸ file`. The two tiers disagree, so a transposed `.or()`
    /// chain answers `None` here.
    #[test]
    fn the_cli_tier_beats_the_file_tier_when_both_speak() {
        let ladder = Ladder {
            cli: Some(ActivateMode::Bin),
            file: Some(ActivateMode::None),
            environment: Some(ActivateMode::None),
        };
        assert_eq!(
            ladder.resolve(ACTIVATE_FLOOR),
            ActivateMode::Bin,
            "the flag on the invoked command is the most specific tier"
        );
    }

    /// C-005 / C-007: `file ▸ environment` — **the environment variable is the
    /// weakest tier, not an override.** The two tiers disagree, so a transposed
    /// `.or()` chain answers `None` here.
    #[test]
    fn the_file_tier_beats_the_environment_tier_when_both_speak() {
        let ladder = Ladder {
            cli: None,
            file: Some(ActivateMode::Bin),
            environment: Some(ActivateMode::None),
        };
        assert_eq!(
            ladder.resolve(ACTIVATE_FLOOR),
            ActivateMode::Bin,
            "`ocx.toml` outranks OCX_TOOLCHAIN_ACTIVATE; the environment is the weakest tier"
        );
    }

    /// C-005: a tier that explicitly *states* the floor value still answers for
    /// the ladder, so a weaker tier holding something else never wins.
    ///
    /// Through [`Ladder::resolve`] alone, "this tier holds the floor value" and
    /// "this tier is absent" are indistinguishable **when no weaker tier
    /// speaks** — which is correct, and is why the discriminating fixture below
    /// gives the weaker tier a different value. Provenance, when a caller wants
    /// it, is read off the field, not off the resolved answer; the second
    /// assertion states that explicitly.
    #[test]
    fn a_tier_holding_the_floor_value_still_answers_for_the_ladder() {
        let ladder = Ladder {
            cli: None,
            file: Some(ACTIVATE_FLOOR),
            environment: Some(ActivateMode::Bin),
        };
        assert_eq!(
            ladder.file,
            Some(ActivateMode::Env),
            "provenance lives on the field: the file tier stated the floor value"
        );
        assert_eq!(
            ladder.resolve(ACTIVATE_FLOOR),
            ActivateMode::Env,
            "a stated floor value is a stated value, so the environment tier must not be reached"
        );
    }

    /// C-005: the floor is a **parameter**, never `T::default()`.
    ///
    /// One ladder shape, two settings, two different floors — and the same
    /// all-absent `Ladder<bool>` resolves to whichever floor its caller names.
    /// An implementation that reached for a `Default` bound could not answer
    /// both.
    #[test]
    fn the_floor_is_a_parameter_not_a_property_of_the_value_type() {
        assert!(
            !Ladder::<bool>::default().resolve(PINNED_FLOOR),
            "the `pinned` floor is `false`"
        );
        assert!(
            Ladder::<bool>::default().resolve(true),
            "the same ladder shape resolves to whichever floor the caller passes"
        );
        assert_eq!(
            Ladder::<ActivateMode>::default().resolve(ActivateMode::Bin),
            ActivateMode::Bin,
            "no floor is baked into `Ladder`; `ACTIVATE_FLOOR` is one caller's choice"
        );
    }

    /// C-005: `Ladder::<ActivateMode>::default()` must compile even though
    /// [`ActivateMode`] deliberately has **no** `Default` impl.
    ///
    /// This is a compile-level assertion, not a runtime one: `#[derive(Default)]`
    /// on a generic struct emits a `T: Default` bound that `Option<T>: Default`
    /// does not need, and that bound is exactly the second, invisible floor
    /// C-005 forbids. Re-deriving `Default` in place of the hand-written impl
    /// makes this line fail to build.
    #[test]
    fn a_default_ladder_needs_no_default_impl_on_its_value_type() {
        let ladder: Ladder<ActivateMode> = Ladder::default();
        assert_eq!(
            ladder,
            Ladder {
                cli: None,
                file: None,
                environment: None,
            },
            "`Ladder::default()` is all tiers absent, derived from `Option`, not from `T`"
        );
    }
}
