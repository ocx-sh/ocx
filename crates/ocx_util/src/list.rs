// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Move-to-back deduplication for separator-joined option-list values.

/// Unique append for a separator-joined list value: moves `value` to the back.
///
/// Must match the emitted shell snippets (`adr_env_modifier_types.md § Decision`), or the in-process env and the shell env diverge.
///
/// **Precondition:** `value` is not flanked by `separator` and `separator` is non-empty, or `value` is cut out of other elements.
///
/// # Examples
///
/// ```
/// use ocx_util::list::append_unique;
///
/// assert_eq!(append_unique("", "-ea", " "), "-ea");
/// assert_eq!(append_unique("-Xmx1g", "-ea", " "), "-Xmx1g -ea");
/// // Already present: removed from its old slot, re-appended at the back.
/// assert_eq!(append_unique("-ea -Xmx1g", "-ea", " "), "-Xmx1g -ea");
/// assert_eq!(append_unique("-ea", "-ea", " "), "-ea");
/// ```
pub fn append_unique(existing: &str, value: &str, separator: &str) -> String {
    if value.is_empty() {
        return existing.to_string();
    }

    let mut wrapped = String::with_capacity(existing.len() + 2 * separator.len());
    wrapped.push_str(separator);
    wrapped.push_str(existing);
    wrapped.push_str(separator);

    let occurrence = format!("{separator}{value}{separator}");
    // Loop to a fixpoint: one `replace` pass misses adjacent duplicates, which breaks idempotence.
    while wrapped.contains(&occurrence) {
        wrapped = wrapped.replace(&occurrence, separator);
    }

    // A fully collapsed list leaves one bare separator, which fails the strip and means empty.
    let survivors = wrapped
        .strip_prefix(separator)
        .and_then(|inner| inner.strip_suffix(separator))
        .unwrap_or("");

    if survivors.is_empty() {
        value.to_string()
    } else {
        format!("{survivors}{separator}{value}")
    }
}

#[cfg(test)]
mod tests {
    use super::append_unique;

    #[test]
    fn empty_existing_yields_the_bare_value() {
        assert_eq!(append_unique("", "-ea", " "), "-ea");
    }

    #[test]
    fn appends_to_the_back() {
        assert_eq!(append_unique("-Xmx1g", "-ea", " "), "-Xmx1g -ea");
    }

    #[test]
    fn moves_an_existing_contribution_to_the_back() {
        // Last applier wins for a consumer that resolves duplicates last-wins,
        // which is the whole point of re-applying rather than skipping.
        assert_eq!(append_unique("-ea -Xmx1g", "-ea", " "), "-Xmx1g -ea");
    }

    #[test]
    fn already_at_the_back_is_unchanged() {
        assert_eq!(append_unique("-Xmx1g -ea", "-ea", " "), "-Xmx1g -ea");
    }

    #[test]
    fn idempotent_when_reapplied() {
        let once = append_unique("-ea -Xmx1g -server", "-ea", " ");
        assert_eq!(append_unique(&once, "-ea", " "), once);
    }

    /// Mirrors `utility::path`'s `removes_every_repeated_occurrence`: an
    /// ambient value that already carried duplicates must collapse to one, or
    /// re-application is not idempotent.
    #[test]
    fn removes_every_repeated_occurrence() {
        assert_eq!(append_unique("a,b,a,c", "a", ","), "b,c,a");
    }

    /// Adjacent duplicates share the separator the first match consumes, so a
    /// single `replace` pass leaves one behind — the fixpoint loop is what
    /// makes the removal total.
    #[test]
    fn removes_adjacent_duplicates() {
        assert_eq!(append_unique("a,a,b", "a", ","), "b,a");
        assert_eq!(append_unique("a,a,a", "a", ","), "a");
    }

    /// The flank rule matches whole elements only: `-ea` must not be found
    /// inside `-eabc`, or a longer option would be silently deleted.
    #[test]
    fn a_longer_element_is_not_a_flank_match() {
        assert_eq!(append_unique("-eabc", "-ea", " "), "-eabc -ea");
        assert_eq!(append_unique("-ea", "-eabc", " "), "-ea -eabc");
    }

    /// A contribution carrying the separator is still one contribution — ocx
    /// never tokenizes elements — so it round-trips as a unit.
    #[test]
    fn value_containing_the_separator_is_one_contribution() {
        let once = append_unique("x", "a,b", ",");
        assert_eq!(once, "x,a,b");
        assert_eq!(append_unique(&once, "a,b", ","), "x,a,b");
    }

    #[test]
    fn multi_character_separator() {
        let once = append_unique("-Wall", "-Wextra", "; ");
        assert_eq!(once, "-Wall; -Wextra");
        assert_eq!(append_unique(&once, "-Wall", "; "), "-Wextra; -Wall");
    }

    #[test]
    fn empty_value_is_a_no_op() {
        assert_eq!(append_unique("a,b", "", ","), "a,b");
        assert_eq!(append_unique("", "", ","), "");
    }

    /// Named boundary, accepted by ADR D1: a contribution spanning two adjacent
    /// prior elements is flanked by separators just like a single element, so
    /// the span is removed and re-appended as one. Asserted rather than fixed —
    /// the result is stable and idempotent, and detecting the case would mean
    /// parsing element grammar ocx deliberately does not own.
    #[test]
    fn a_contribution_spanning_two_elements_is_removed_as_one_span() {
        assert_eq!(append_unique("a,b,c", "b,c", ","), "a,b,c");
        let once = append_unique("a,b,c", "b,c", ",");
        assert_eq!(append_unique(&once, "b,c", ","), once, "still idempotent");
    }

    /// A separator-edged value is refused before the fold ever sees it; this
    /// pins what the primitive does anyway, so a future parse-gate regression
    /// surfaces as a changed assertion rather than silent corruption.
    #[test]
    fn a_separator_edged_value_still_folds_deterministically() {
        assert_eq!(append_unique("a", ",b", ","), "a,,b");
    }
}
