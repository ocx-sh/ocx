// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The capability gate: which token *classes* a surface may carry.
//!
//! Orthogonal to the grammar. The scanner decides what a `${…}` **is**; this
//! decides whether the surface being interpolated is allowed to carry one of
//! that class. Both enforcement points — the resolver before it substitutes
//! anything, and the publish-time entrypoint-args check in `validation` — call
//! [`first_disallowed_token`] over the same scanner output, so the two cannot
//! disagree about what a template may carry (D9).

use super::scanner::{Segment, TokenShape};

/// Caller-facing intent: what surface is interpolation serving?
///
/// Maps into [`AllowedTokens`] via `From<Usage>`. Use at call sites so the engine
/// sees a capability set, not a consumer identity (SRP: engine policy ≠ consumer
/// identity).
#[derive(Debug, Clone, Copy)]
pub enum Usage {
    /// Interpolating an environment-variable value. Every recognised token is
    /// permitted.
    Environment,
    /// Interpolating an entrypoint `args` element. Only `${installPath}` and
    /// its alias are permitted; `${deps.*}` and `${self.env.*}` are rejected
    /// with [`TemplateError::DisallowedToken`].
    ///
    /// [`TemplateError::DisallowedToken`]: super::TemplateError::DisallowedToken
    EntryPointArgs,
}

/// Engine-facing capability set: which token classes the resolver may substitute.
///
/// Constructed from [`Usage`] via `From<Usage>` or built directly for tests. The
/// engine gates on this struct, never on consumer identity — callers set intent via
/// [`Usage`], the engine sees only what is allowed.
///
/// `installPath` and `self.installPath` are always permitted regardless of the
/// capability set: they are the same referent (D4), so a gate that told them
/// apart would make an alias observably not an alias. Render modifiers are not
/// gated here at all — a modifier is a rendering property of a token already
/// admitted, and which shapes may carry one is settled once in the scanner (D5).
///
/// Extending is one `bool` field plus one `From<Usage>` arm; there is
/// deliberately no trait registry.
#[derive(Debug, Clone, Copy)]
pub struct AllowedTokens {
    /// Whether `${deps.NAME.*}` tokens are permitted.
    pub deps: bool,
    /// Whether `${self.env.KEY}` tokens are permitted.
    pub self_env: bool,
}

impl From<Usage> for AllowedTokens {
    fn from(usage: Usage) -> Self {
        match usage {
            Usage::Environment => AllowedTokens {
                deps: true,
                self_env: true,
            },
            Usage::EntryPointArgs => AllowedTokens {
                deps: false,
                self_env: false,
            },
        }
    }
}

/// Returns the verbatim source text of the first token in `segments` whose
/// class `allowed` does not permit.
///
/// The one capability gate. Both enforcement points call it — the resolver
/// before substituting anything, and the publish-time entrypoint-args check in
/// `validation` — over the same scanner output, so the two cannot disagree
/// about what a template may carry.
#[must_use]
pub fn first_disallowed_token<'a>(segments: &[Segment<'a>], allowed: AllowedTokens) -> Option<&'a str> {
    segments.iter().find_map(|segment| {
        let Segment::Token(token) = segment else {
            return None;
        };
        let permitted = match &token.shape {
            // Always allowed, on every surface: the bare form and its alias are
            // the same referent, and a gate that told them apart would make an
            // alias observably not an alias (D4/D9).
            TokenShape::InstallPath => true,
            TokenShape::Dep { .. } => allowed.deps,
            TokenShape::SelfEnv { .. } => allowed.self_env,
        };
        (!permitted).then_some(token.source)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // C-030 (From<Usage> mapping): Usage::Environment permits every token class;
    // Usage::EntryPointArgs permits neither ${deps.*} nor ${self.env.*}.
    #[test]
    fn usage_maps_to_allowed_tokens() {
        let env_caps = AllowedTokens::from(Usage::Environment);
        assert!(
            env_caps.deps && env_caps.self_env,
            "Usage::Environment must map to AllowedTokens {{ deps: true, self_env: true }}"
        );

        let args_caps = AllowedTokens::from(Usage::EntryPointArgs);
        assert!(
            !args_caps.deps && !args_caps.self_env,
            "Usage::EntryPointArgs must map to AllowedTokens {{ deps: false, self_env: false }}"
        );
    }
}
