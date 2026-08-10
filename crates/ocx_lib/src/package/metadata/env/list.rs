// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::{Deserialize, Serialize};

/// The separator assumed where one may be omitted.
///
/// Only the human-facing surfaces (`ocx.toml`, `ocx run --env`) may omit it —
/// and only when no other contributor to the same key established one. Package
/// metadata is the wire, where no human is present to be told, so it must spell
/// the separator out.
pub const DEFAULT_SEPARATOR: &str = " ";

/// A list-type environment variable.
///
/// List variables are appended to any existing value of the environment
/// variable, with every earlier occurrence of the same contribution removed
/// first, so re-applying moves the contribution to the back rather than
/// duplicating it. The consumer resolving duplicates last-wins is what the
/// direction serves. Interpolation tokens in `value` are replaced at
/// resolution time.
///
/// The contribution is opaque: ocx never splits it into elements, so a value
/// carrying the separator is still one contribution.
#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct List {
    /// The string joining this contribution to the variable's existing value —
    /// a single space for `JDK_JAVA_OPTIONS`, a comma for `GODEBUG`.
    ///
    /// Required in package metadata, and refused there by `ValidMetadata`
    /// rather than by serde, so the message names the variable instead of a
    /// field offset. Typed as optional because `ocx.toml` and `ocx run --env`
    /// may omit it.
    ///
    /// Deliberately no `skip_serializing_if`, unlike the tree's other optional
    /// wire fields: schemars drops a skipped field from `required`, and the
    /// published schema is the write contract where "a list needs a separator"
    /// has to be enforceable. The only value that would be written as `null` is
    /// one `ValidMetadata` refuses to publish.
    #[schemars(required)]
    pub separator: Option<String>,

    /// The value template. `${installPath}` — or its alias `${self.installPath}` — is this package's
    /// content directory, `${deps.NAME.installPath}` a declared dependency's, and `${self.env.KEY}` the
    /// resolved value of a variable declared earlier in this same list. Append `:native` or `:posix` to
    /// pick the path style. Every other `${...}` is rejected; write `$${` for a literal `${`.
    pub value: String,
}

/// A separator ocx can fold with: non-empty, and free of `=`, newline and
/// carriage return.
///
/// An empty separator degrades the flank match in
/// [`append_unique`](crate::utility::list::append_unique) to a bare substring
/// scan, which would delete text from the middle of unrelated elements. `=`
/// is excluded because `ocx run --env KEY:list:SEP=VALUE` splits on the first
/// `=`, and one grammar that accepts what another cannot express is a trap.
/// `\n` and `\r` are excluded because every export surface downstream is
/// line-oriented — a CI env file, a shell snippet, a JSON-lines record — and a
/// separator that ends a line is an injection primitive, not a delimiter any
/// real consumer asks for.
///
/// Carries no message of its own: each surface folds the verdict into its own
/// typed error (65 from metadata, 78 from `ocx.toml`, 64 from `--env`).
pub fn separator_is_valid(separator: &str) -> bool {
    !separator.is_empty() && !separator.contains(['=', '\n', '\r'])
}

/// `true` when `value` starts or ends with its own `separator`.
///
/// Such a value makes the fold's flank match ambiguous — its leading or
/// trailing separator fuses with the one the algorithm wraps around it — so
/// every parse boundary refuses it, and [`EnvResolver`] refuses it again after
/// template resolution: parse-time sees the authored bytes, and
/// `${installPath}` with separator `/` resolves to a `/`-edged value no parse
/// gate could have seen.
///
/// [`EnvResolver`]: super::resolver::EnvResolver
pub fn is_separator_edged(value: &str, separator: &str) -> bool {
    value.starts_with(separator) || value.ends_with(separator)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_separator_is_rejected() {
        assert!(!separator_is_valid(""));
    }

    #[test]
    fn a_separator_carrying_the_flag_grammars_delimiter_is_rejected() {
        assert!(!separator_is_valid("="));
        assert!(!separator_is_valid(" = "));
    }

    /// Every export surface is line-oriented, so a separator that ends a line
    /// is an injection primitive rather than a delimiter.
    #[test]
    fn a_line_breaking_separator_is_rejected() {
        for separator in ["\n", "\r", "\r\n", ",\n", "\n,"] {
            assert!(!separator_is_valid(separator), "{separator:?} must be refused");
        }
    }

    #[test]
    fn ordinary_separators_are_accepted() {
        for separator in [" ", ",", ";", ": ", "\t", "→"] {
            assert!(separator_is_valid(separator), "{separator:?} must be usable");
        }
    }

    #[test]
    fn edged_values_are_detected_on_both_flanks() {
        assert!(is_separator_edged(",a", ","));
        assert!(is_separator_edged("a,", ","));
        assert!(is_separator_edged(",", ","), "a bare separator is edged on both sides");
        assert!(!is_separator_edged("a,b", ","));
        assert!(!is_separator_edged("", ","), "an empty value carries no flank");
    }
}
