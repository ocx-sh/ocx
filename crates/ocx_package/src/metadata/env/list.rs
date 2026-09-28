// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::{Deserialize, Serialize};

/// The separator assumed where `ocx.toml` or `ocx exec --env` omit one and no
/// other contributor to the key set one; package metadata must spell it out.
pub const DEFAULT_SEPARATOR: &str = " ";

// The consumer resolving duplicates last-wins is what the append direction serves.
/// A list-type environment variable.
///
/// List variables are appended to any existing value of the environment
/// variable, with every earlier occurrence of the same contribution removed
/// first, so re-applying moves the contribution to the back rather than
/// duplicating it. Interpolation tokens in `value` are replaced at
/// resolution time.
///
/// The contribution is opaque: ocx never splits it into elements, so a value
/// carrying the separator is still one contribution.
#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct List {
    // Missing in package metadata is refused by `ValidMetadata` rather than by
    // serde, so the message names the variable instead of a field offset.
    /// The string joining this contribution to the variable's existing value —
    /// a single space for `JDK_JAVA_OPTIONS`, a comma for `GODEBUG`.
    ///
    /// Required in package metadata; `ocx.toml` and `ocx exec --env` may omit it.
    // Deliberately no `skip_serializing_if`, unlike the tree's other optional
    // wire fields: schemars drops a skipped field from `required`, and the
    // published schema is the write contract where "a list needs a separator"
    // has to be enforceable. The only value that would be written as `null` is
    // one `ValidMetadata` refuses to publish.
    #[schemars(required)]
    pub separator: Option<String>,

    /// The value template. `${installPath}` — or its alias `${self.installPath}` — is this package's
    /// content directory, `${deps.NAME.installPath}` a declared dependency's, and `${self.env.KEY}` the
    /// resolved value of a variable declared earlier in this same list. Append `:native` or `:posix` to
    /// pick the path style. Every other `${...}` is rejected; write `$${` for a literal `${`.
    pub value: String,
}

/// A separator ocx can fold with: non-empty, and free of `=`, `\n` and `\r`.
///
/// Empty turns [`append_unique`](ocx_util::list::append_unique) into a substring
/// scan that deletes text from unrelated elements; `ocx exec --env
/// KEY:list:SEP=VALUE` splits on the first `=`; a line break injects lines into
/// every line-oriented export.
pub fn separator_is_valid(separator: &str) -> bool {
    !separator.is_empty() && !separator.contains(['=', '\n', '\r'])
}

/// `true` when `value` starts or ends with its own `separator`, which makes the
/// fold's flank match ambiguous; [`EnvResolver`] rechecks after template
/// resolution, since `${installPath}` can produce an edge no parse gate saw.
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
