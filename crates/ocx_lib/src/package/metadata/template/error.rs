// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The refusal taxonomy for interpolation-token resolution, and how each
//! refusal reads.
//!
//! Every variant is malformed publisher input (exit 65) except
//! [`TemplateError::DependencyNotInstalled`], which is a missing resource (79).
//! Publisher-controlled text reaching a message is escaped at the site that
//! captures it — `scanner::for_message` — never here.

use super::scanner;
use crate::cli::{ClassifyExitCode, ExitCode};
use crate::oci;
use crate::package::metadata::dependency::DependencyName;

/// Which guidance a [`TemplateError::UnknownToken`] message carries (D13).
///
/// Three states, because the advice a blocked publisher needs differs by *how*
/// the token is unknown — and one of the three is defined by what it must
/// **not** say. Under the claim-all rule the natural message is "escape it as
/// `$${…}`", which for a typo is precisely wrong advice: it tells the publisher
/// to fix the error by shipping the typo as literal text into a digest-pinned
/// artifact. [`Self::SuggestedRoot`] is what keeps that out, and
/// [`Self::SupportedBodies`] is why the escape hint is not offered on a token
/// whose root OCX recognises.
///
/// An enum rather than an `Option<String>` because the three branches are not
/// "a suggestion or nothing": absence of a suggestion is itself two distinct
/// messages, and a two-state field cannot tell them apart without re-reading
/// the token text at render time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnknownTokenHint {
    /// Branch 1 — the token's root run is within the length-scaled edit
    /// distance of this recognised root, so the likeliest fault is a typo in a
    /// token the publisher meant to write.
    SuggestedRoot(String),
    /// Branch 2 — an unrecognised root with no near miss: most likely another
    /// tool's token, whose only correct spelling under D3 is the escape.
    Escape,
    /// Branch 3 — a recognised root carrying a body outside the closed set.
    /// The publisher was writing an OCX token, so the message enumerates the
    /// bodies that exist and offers **no** escape hint.
    SupportedBodies,
}

impl UnknownTokenHint {
    /// The clause appended after the token in the message.
    ///
    /// Takes `token` because the escape branch shows the escaped spelling of
    /// this very token — `$${workspaceFolder}` — rather than a generic `$${…}`
    /// the publisher then has to translate.
    fn advice_for(&self, token: &str) -> String {
        match self {
            // No escape hint: it would advise "fixing" a typo by shipping it as
            // literal text into a digest-pinned artifact.
            Self::SuggestedRoot(root) => format!(": did you mean root '{root}'"),
            // A token the echo cut short cannot be spelled back: the escaped
            // form of a prefix is a literal the publisher never wrote. The rule
            // is stated instead, and it is the same rule.
            Self::Escape if token.ends_with(scanner::TRUNCATION_MARKER) => {
                ": ocx expands every ${…}; write '$${' for every '${' to emit a literal".to_owned()
            }
            Self::Escape => format!(
                ": ocx expands every ${{…}}; write '{}' to emit a literal",
                scanner::escape(token)
            ),
            // The placeholders are spelled out because the publisher this
            // branch exists for wrote `${deps.Python.installPath}`: printing
            // `${deps.NAME.installPath}` at them without saying what `NAME`
            // stands for reads as confirmation that what they wrote was right.
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

/// Renders [`TemplateError::UndefinedSelfEnvRef`]'s declared-key list.
///
/// Quoting happens here rather than in the field, which stays clean data for a
/// programmatic reader. Each entry is already `scanner::for_message`-escaped, and
/// `str::escape_debug` escapes both quote characters — a key containing `'`
/// arrives as `\'` — so the delimiter cannot appear unescaped inside an entry and
/// no key can close its own quoting. It also settles what `A, PATH` means: one
/// declared key, not two.
///
/// A trailing elision marker is ours, not the publisher's, and stays bare —
/// quoting it would read as a declared key literally named `…`.
fn render_declared_before(declared_before: &[String]) -> String {
    // ponytail: a publisher whose LAST declared key is literally `…`, in a list
    // short enough not to be elided, gets that one key rendered bare. Cosmetic,
    // and the price of carrying the marker inline; splitting elision into its
    // own field is the upgrade if it ever matters.
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

/// Errors produced during template string resolution.
///
/// These are template-level failures only — they carry no `var_key`. The wrapping error
/// variant [`crate::package::error::Error::EnvVarInterpolation`] adds the `var_key` context
/// for the env-variable layer.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TemplateError {
    /// A `${deps.NAME.*}` token names a dependency that is not declared.
    #[error(
        "references unknown dependency '{ref_name}'; declared: [{declared}]",
        declared = declared.iter().map(|n| n.as_str()).collect::<Vec<_>>().join(", ")
    )]
    UnknownDependencyRef {
        ref_name: DependencyName,
        declared: Vec<DependencyName>,
    },

    /// Two direct dependencies share the same interpolation name (name field or basename) and
    /// the template references that name — the publisher must set `name` to disambiguate.
    ///
    /// Constructed only by the publish gate in `validation`. `TemplateResolver::resolve`
    /// never constructs this variant — it receives a pre-disambiguated map.
    #[error(
        "references ambiguous dependency name '{ref_name}': \
         matches both {first} and {second}"
    )]
    AmbiguousDependencyRef {
        ref_name: DependencyName,
        /// Boxed, with `second` and `DependencyNotInstalled`'s identifier: a
        /// bare `PinnedIdentifier` is ~100 bytes, and three of them across two
        /// variants put every `Result<_, TemplateError>` in this subsystem over
        /// clippy's `result_large_err` threshold — a cost the `Ok` path pays on
        /// every call to silence a lint on a path taken once, at the end.
        first: Box<oci::PinnedIdentifier>,
        second: Box<oci::PinnedIdentifier>,
    },

    /// A `${deps.NAME.*}` token names a known dependency that is not installed on disk.
    #[error("references dependency '{ref_name}' ({dep_identifier}) which is not installed")]
    DependencyNotInstalled {
        ref_name: DependencyName,
        dep_identifier: Box<oci::PinnedIdentifier>,
    },

    /// A `${…}` OCX does not recognise: a body that fails the anchored grammar,
    /// a recognised root whose body is outside the closed set, or an
    /// unrecognised root. The catch-all of the grammar — everything the more
    /// specific variants below cannot locate.
    ///
    /// `hint` selects which of D13's three message branches renders. It is
    /// computed at construction, from the root run the scanner already has
    /// (R2.2), and never re-derived from `token` at render time: a second
    /// reading of the token text here would be a second recogniser, free to
    /// disagree with the one that decided the token was unknown.
    #[error("unknown token '{token}'{advice}", advice = hint.advice_for(token))]
    UnknownToken { token: String, hint: UnknownTokenHint },

    /// A recognised namespace shape with exactly one unknown leaf —
    /// `${self.foo}` (`namespace` = `self`) or `${deps.cmake.version}`
    /// (`namespace` = `deps.cmake`). Everything else is
    /// [`TemplateError::UnknownToken`].
    #[error(
        "unknown field '{field}' under '{namespace}'; supported: [{supported}]",
        supported = supported.join(", ")
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
    UnknownModifier { modifier: String, supported: Vec<String> },

    /// A render modifier on a token whose value OCX cannot know is a path —
    /// today exactly `${self.env.KEY}` (D5).
    ///
    /// The modifier flips slash direction across the *whole* resolved value, so
    /// on a var holding a regex, a compiler flag, or a `list` it rewrites
    /// backslashes the publisher meant to keep. A `self.env` value composed from
    /// a path still renders — the modifier belongs on the token inside the
    /// declaring var, which is what the advice says.
    #[error(
        "render modifier '{modifier}' does not apply to '{token}'; \
         modifiers apply to install-path tokens only — set it where the var is declared"
    )]
    ModifierNotApplicable { modifier: String, token: String },

    /// `${self.env.KEY}` where `KEY` is not declared strictly earlier in the
    /// same package's `env` array — covers forward references and a var
    /// referencing itself, which are the same fault seen twice (D6.3).
    #[error(
        "references undefined env var '{key}'; declared before it: [{declared_before}]",
        declared_before = render_declared_before(declared_before)
    )]
    UndefinedSelfEnvRef { key: String, declared_before: Vec<String> },

    /// `${self.env.KEY}` where `KEY` is declared two or more times earlier.
    /// Both candidates are legally visible and neither is privileged, so the
    /// reference is refused rather than resolved to an arbitrary one (D7).
    #[error("references ambiguous env var '{key}': declared more than once before it")]
    AmbiguousSelfEnvRef { key: String },

    /// A recognized token class is present but not permitted by the current
    /// [`AllowedTokens`] capability set.
    ///
    /// [`AllowedTokens`]: super::AllowedTokens
    #[error("token '{token}' is not permitted here; '${{deps.*}}' and '${{self.env.*}}' are only valid in env values")]
    DisallowedToken { token: String },

    /// The resolved value grew past [`MAX_RESOLVED_VALUE_BYTES`].
    ///
    /// Reached by a `${self.env.KEY}` chain that doubles per var — the
    /// substituted value is the referenced var's *resolved* one, so a document
    /// small enough to publish resolves to an arbitrarily large one on every
    /// consumer.
    ///
    /// [`MAX_RESOLVED_VALUE_BYTES`]: super::MAX_RESOLVED_VALUE_BYTES
    #[error("resolved value exceeds the {limit}-byte budget")]
    ResolvedValueTooLarge { limit: usize },
}

impl ClassifyExitCode for TemplateError {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::UnknownDependencyRef { .. }
            | Self::AmbiguousDependencyRef { .. }
            | Self::UnknownToken { .. }
            | Self::UnknownField { .. }
            | Self::UnknownModifier { .. }
            | Self::ModifierNotApplicable { .. }
            | Self::UndefinedSelfEnvRef { .. }
            | Self::AmbiguousSelfEnvRef { .. }
            | Self::DisallowedToken { .. }
            | Self::ResolvedValueTooLarge { .. } => ExitCode::DataError,
            Self::DependencyNotInstalled { .. } => ExitCode::NotFound,
        })
    }
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
