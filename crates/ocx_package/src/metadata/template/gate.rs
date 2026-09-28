// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The capability gate: which token classes a surface may carry, orthogonal to the grammar.

use super::scanner::{Segment, TokenShape};

/// The surface interpolation serves; maps into [`AllowedTokens`].
#[derive(Debug, Clone, Copy)]
pub enum Usage {
    /// An env-var value: every token permitted.
    Environment,
    /// An entrypoint `args` element: `${deps.*}` and `${self.env.*}` refused.
    EntryPointArgs,
}

/// The token classes the resolver may substitute; `${installPath}` and its alias always pass.
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

/// Returns the source text of the first token in `segments` that `allowed` does not permit.
// The resolver and the publish gate both call this over one scan, so they cannot disagree.
#[must_use]
pub fn first_disallowed_token<'a>(segments: &[Segment<'a>], allowed: AllowedTokens) -> Option<&'a str> {
    segments.iter().find_map(|segment| {
        let Segment::Token(token) = segment else {
            return None;
        };
        let permitted = match &token.shape {
            // One referent with its alias; gating them apart would make the alias observable.
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
