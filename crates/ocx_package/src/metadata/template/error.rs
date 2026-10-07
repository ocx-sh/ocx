// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The refusal taxonomy for interpolation-token resolution: every variant is exit 65
//! except [`TemplateError::DependencyNotInstalled`] (79).
//!
//! Publisher text is escaped where it is captured (`scanner::for_message`), never here.

use super::scanner;
use crate::metadata::dependency::DependencyName;

/// Which guidance a [`TemplateError::UnknownToken`] message carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnknownTokenHint {
    /// The token's root is within edit distance of this recognised root: likely a typo.
    SuggestedRoot(String),
    /// An unrecognised root with no near miss: likely another tool's token, needing the escape.
    Escape,
    /// A recognised root with a body outside the closed set; lists the bodies, no escape hint.
    SupportedBodies,
}

impl UnknownTokenHint {
    /// The clause appended after the token; the escape branch spells this token escaped.
    fn advice_for(&self, token: &str) -> String {
        match self {
            // No escape hint: it would ship the typo as literal text into a digest-pinned artifact.
            Self::SuggestedRoot(root) => format!(": did you mean root '{root}'"),
            // A truncated token's escaped form is a literal the publisher never wrote; state the rule.
            Self::Escape if token.ends_with(scanner::TRUNCATION_MARKER) => {
                ": ocx expands every ${…}; write '$${' for every '${' to emit a literal".to_owned()
            }
            Self::Escape => format!(
                ": ocx expands every ${{…}}; write '{}' to emit a literal",
                scanner::escape(token)
            ),
            // Placeholders explained, or `${deps.NAME.installPath}` reads as confirming `${deps.Python…}`.
            Self::SupportedBodies => format!(
                ": supported bodies are {}, where NAME is a lowercase dependency name and KEY is an env-var key",
                scanner::RECOGNISED_BODIES
                    .iter()
                    .map(|body| format!("'${{{body}}}'"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

/// Renders [`TemplateError::UndefinedSelfEnvRef`]'s declared-key list, each key quoted and a
/// trailing elision marker bare.
// Keys arrive `for_message`-escaped, quotes included, so no key can close its own quoting.
fn render_declared_before(declared_before: &[String]) -> String {
    // ponytail: a non-elided list whose last key is literally `…` renders it bare; give
    // elision its own field if that ever matters.
    let (keys, elided) = match declared_before.split_last() {
        Some((last, rest)) if last == scanner::TRUNCATION_MARKER => (rest, true),
        _ => (declared_before, false),
    };

    let mut rendered: Vec<String> = keys.iter().map(|key| format!("'{key}'")).collect();
    if elided {
        rendered.push(scanner::TRUNCATION_MARKER.to_owned());
    }
    rendered.join(", ")
}

/// Template resolution errors; [`crate::error::Error::EnvVarInterpolation`] adds the var key.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
pub enum TemplateError {
    /// A `${deps.NAME.*}` token names a dependency that is not declared.
    #[error(
        "references unknown dependency '{ref_name}'; declared: [{declared}]",
        declared = declared.iter().map(|n| n.as_str()).collect::<Vec<_>>().join(", ")
    )]
    #[exit(
        DataError,
        slug = "template_unknown_dependency_ref",
        summary = "An interpolation token names an unknown dependency"
    )]
    UnknownDependencyRef {
        ref_name: DependencyName,
        declared: Vec<DependencyName>,
    },

    /// Two direct dependencies share the referenced interpolation name; the publisher must set
    /// `name`. Only the publish gate constructs it.
    #[error(
        "references ambiguous dependency name '{ref_name}': \
         matches both {first} and {second}"
    )]
    #[exit(
        DataError,
        slug = "template_ambiguous_dependency_ref",
        summary = "An interpolation token names an ambiguous dependency"
    )]
    AmbiguousDependencyRef {
        ref_name: DependencyName,
        // Boxed, like `second` and `dep_identifier`, or every `Result<_, TemplateError>` trips `result_large_err`.
        first: Box<ocx_oci::PinnedPackageRef>,
        second: Box<ocx_oci::PinnedPackageRef>,
    },

    /// A `${deps.NAME.*}` token names a known dependency that is not installed on disk.
    #[error("references dependency '{ref_name}' ({dep_identifier}) which is not installed")]
    #[exit(
        NotFound,
        slug = "template_dependency_not_installed",
        summary = "An interpolation token names a dependency that is not installed"
    )]
    DependencyNotInstalled {
        ref_name: DependencyName,
        dep_identifier: Box<ocx_oci::PinnedPackageRef>,
    },

    /// A `${…}` OCX does not recognise, where no more specific variant applies.
    // `hint` comes from the scanner at construction, never re-derived from `token`, or a
    // second recogniser could disagree with the first.
    #[error("unknown token '{token}'{advice}", advice = hint.advice_for(token))]
    #[exit(
        DataError,
        slug = "template_unknown_token",
        summary = "An interpolation token is not recognised"
    )]
    UnknownToken { token: String, hint: UnknownTokenHint },

    /// A recognised namespace with one unknown leaf (`${self.foo}`, `${deps.cmake.version}`).
    #[error(
        "unknown field '{field}' under '{namespace}'; supported: [{supported}]",
        supported = supported.join(", ")
    )]
    #[exit(
        DataError,
        slug = "template_unknown_field",
        summary = "An interpolation token names an unknown field"
    )]
    UnknownField {
        namespace: String,
        field: String,
        supported: Vec<String>,
    },

    /// A `:suffix` outside the closed render-modifier set.
    #[error(
        "unknown render modifier '{modifier}'; supported: [{supported}]",
        supported = supported.join(", ")
    )]
    #[exit(
        DataError,
        slug = "template_unknown_modifier",
        summary = "An interpolation token carries an unknown modifier"
    )]
    UnknownModifier { modifier: String, supported: Vec<String> },

    /// A render modifier on `${self.env.KEY}`, whose value may not be a path.
    // Refused because it would flip every slash in the value, corrupting a regex or compiler flag.
    #[error(
        "render modifier '{modifier}' does not apply to '{token}'; \
         modifiers apply to install-path tokens only — set it where the var is declared"
    )]
    #[exit(
        DataError,
        slug = "template_modifier_not_applicable",
        summary = "An interpolation modifier does not apply to its token"
    )]
    ModifierNotApplicable { modifier: String, token: String },

    /// `${self.env.KEY}` where `KEY` is not declared strictly earlier (forward or self reference).
    #[error(
        "references undefined env var '{key}'; declared before it: [{declared_before}]",
        declared_before = render_declared_before(declared_before)
    )]
    #[exit(
        DataError,
        slug = "template_undefined_self_env_ref",
        summary = "An interpolation token names an env var not declared before it"
    )]
    UndefinedSelfEnvRef { key: String, declared_before: Vec<String> },

    /// `${self.env.KEY}` where `KEY` is declared more than once earlier; neither is privileged.
    #[error("references ambiguous env var '{key}': declared more than once before it")]
    #[exit(
        DataError,
        slug = "template_ambiguous_self_env_ref",
        summary = "An interpolation token names an env var declared more than once"
    )]
    AmbiguousSelfEnvRef { key: String },

    /// A recognised token the active [`AllowedTokens`] forbids.
    ///
    /// [`AllowedTokens`]: super::AllowedTokens
    #[error("token '{token}' is not permitted here; '${{deps.*}}' and '${{self.env.*}}' are only valid in env values")]
    #[exit(
        DataError,
        slug = "template_disallowed_token",
        summary = "An interpolation token is not permitted in this field"
    )]
    DisallowedToken { token: String },

    /// The resolved value grew past [`MAX_RESOLVED_VALUE_BYTES`].
    ///
    /// [`MAX_RESOLVED_VALUE_BYTES`]: super::MAX_RESOLVED_VALUE_BYTES
    #[error("resolved value exceeds the {limit}-byte budget")]
    #[exit(
        DataError,
        slug = "template_value_too_large",
        summary = "An interpolated value exceeds its size budget"
    )]
    ResolvedValueTooLarge { limit: usize },
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Unknown-token message branches (D13) ──────────────────────────────────
    //
    // Which branch a given token routes to is the scanner's contract (C-033,
    // pinned in `scanner.rs`). What each branch then *says* is this module's,
    // and it is asserted on a directly-constructed error so the wording stands
    // on its own — a message is a fact about a hint, not about the one caller
    // that happened to build it.

    fn unknown_token_message(token: &str, hint: UnknownTokenHint) -> String {
        TemplateError::UnknownToken {
            token: token.to_string(),
            hint,
        }
        .to_string()
    }

    /// D13 branch 1 — the suggestion reaches the publisher, and the escape hint
    /// never does. Advising an escape here would tell them to "fix" a typo by
    /// shipping it as literal text into a digest-pinned artifact, which is the
    /// silent-wrong-value failure returning through the error message.
    ///
    /// `self` is not a substring of `${slef.env.HOME}`, so the only way the
    /// message can carry it is the suggestion itself.
    #[test]
    fn a_root_suggestion_names_the_root_and_never_offers_the_escape() {
        let message = unknown_token_message("${slef.env.HOME}", UnknownTokenHint::SuggestedRoot("self".to_string()));

        assert!(
            message.contains("${slef.env.HOME}"),
            "the token must be named verbatim: {message}"
        );
        assert!(
            message.contains("self"),
            "a suggestion the message never renders is a fact the publisher is never told: {message}"
        );
        assert!(
            !message.contains("$${"),
            "a typo must never be advised to escape itself: {message}"
        );
    }

    /// D13 branch 2 — the hint shows the escaped spelling of *this* token, not a
    /// generic `$${…}` the publisher then has to translate onto their own value.
    ///
    /// The expected text comes from `scanner::escape`, the authored inverse of
    /// the scanner's escape rule: spelling the escaping out here would assert
    /// that rule a second time, and a test can get it wrong the same way the
    /// code did.
    #[test]
    fn the_escape_hint_names_the_token_it_is_advising_about() {
        let token = "${workspaceFolder}";
        let message = unknown_token_message(token, UnknownTokenHint::Escape);

        assert!(
            message.contains(&scanner::escape(token)),
            "the hint must carry the escaped spelling of this very token: {message}"
        );
        assert!(
            !message.contains("$${…}") && !message.contains("$${...}"),
            "a generic placeholder leaves the translation to the publisher: {message}"
        );
    }

    /// D13 branch 3 — a recognised root means the publisher was writing an OCX
    /// token, so the message enumerates the closed set and offers **no** escape.
    ///
    /// The token under test contains none of the four bodies, so every
    /// `contains` below can only be satisfied by the list itself.
    ///
    /// `NAME` and `KEY` are placeholders, not literals, and the list must say
    /// so. The publisher this branch exists for is the one who wrote
    /// `${deps.Python.installPath}` — printing `${deps.NAME.installPath}` at
    /// them without spelling out that `NAME` is a lowercase dependency name
    /// reads as confirmation that what they wrote was already right.
    #[test]
    fn the_supported_bodies_message_lists_the_closed_set_and_explains_its_placeholders() {
        let message = unknown_token_message("${self.env.A B}", UnknownTokenHint::SupportedBodies);

        for body in scanner::RECOGNISED_BODIES {
            assert!(message.contains(body), "the message must list '{body}': {message}");
        }
        assert!(
            !message.contains("$${"),
            "a recognised root gets the body list, never the escape hint: {message}"
        );
        assert!(
            message.contains("NAME is") && message.contains("KEY is"),
            "both placeholders must be spelled out — 'NAME is …', 'KEY is …': {message}"
        );
        assert!(
            message.contains("lowercase"),
            "NAME's character class is what rejected '${{deps.Python.installPath}}'; say it: {message}"
        );
    }
}
