// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The family registry: one list of classifiable error types, one macro that derives everything from it.

/// Declares the classification ladder and the detail registry from one list of types.
///
/// Invoke once, in the binary, as `families!(path::TypeA, TypeB: rows_only, ...)`. It generates, as `pub` items:
/// `try_classify` (an exact `downcast_ref` per type; the first whose `classify` answers wins, so order is not
/// precedence) and `detail_registry` (every type's `DETAILS` in list order, so the first family's description wins
/// for a shared slug).
///
/// `: rows_only` lists a type for its rows alone: for a kind only its wrapper's chain reaches, or a type the
/// binary classifies in a pass of its own. A listed type missing either trait does not compile:
/// ```compile_fail
/// #[derive(Debug)]
/// struct Unarmed;
/// impl std::fmt::Display for Unarmed {
///     fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
///         f.write_str("unarmed")
///     }
/// }
/// impl std::error::Error for Unarmed {}
///
/// ocx_exit::families!(Unarmed);
/// ```
/// ```compile_fail
/// #[derive(Debug)]
/// struct Unarmed;
/// impl std::fmt::Display for Unarmed {
///     fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
///         f.write_str("unarmed")
///     }
/// }
/// impl std::error::Error for Unarmed {}
///
/// ocx_exit::families!(Unarmed: rows_only);
/// ```
#[macro_export]
macro_rules! families {
    ($($family:ty $(: $role:ident)?),* $(,)?) => {
        /// The verdict of the first listed type `cause` downcasts to and classifies; `None` otherwise.
        pub fn try_classify<'a>(cause: &'a (dyn ::std::error::Error + 'static)) -> ::std::option::Option<$crate::Decision<'a>> {
            $($crate::__family_rung!(cause, $family $(: $role)?);)*
            ::std::option::Option::None
        }

        /// Every listed family's `error.detail` rows, in list order.
        pub fn detail_registry() -> ::std::vec::Vec<&'static $crate::DetailEntry> {
            let mut rows = ::std::vec::Vec::new();
            $(rows.extend(<$family as $crate::ClassifyErrorKind>::DETAILS.iter());)*
            rows
        }
    };
}

/// One rung of the `families!` ladder; `: rows_only` is no rung but still demands both traits.
#[doc(hidden)]
#[macro_export]
macro_rules! __family_rung {
    ($cause:ident, $family:ty : rows_only) => {
        const _: fn() = || {
            fn armed<T: $crate::ClassifyExitCode + $crate::ClassifyErrorKind>() {}
            armed::<$family>();
        };
    };
    ($cause:ident, $family:ty) => {
        if let ::std::option::Option::Some(error) = $cause.downcast_ref::<$family>()
            && let ::std::option::Option::Some(code) = $crate::ClassifyExitCode::classify(error)
        {
            return ::std::option::Option::Some($crate::Decision {
                code,
                detail: $crate::ClassifyErrorKind::kind_detail(error),
            });
        }
    };
}

#[cfg(test)]
mod tests {
    use std::fmt;

    use crate::{ClassifyErrorKind, ClassifyExitCode, Detail, DetailEntry, ExitCode};

    const ALPHA_ROW: DetailEntry = DetailEntry {
        slug: "alpha_bad",
        exit_code: ExitCode::DataError,
        family: "Alpha",
        summary: "Alpha is bad",
    };
    const ALPHA_FALLBACK: DetailEntry = DetailEntry {
        slug: "alpha_chain",
        exit_code: ExitCode::Failure,
        family: "Alpha",
        summary: "Alpha defers to its cause",
    };
    const BETA_ROW: DetailEntry = DetailEntry {
        slug: "beta_gone",
        exit_code: ExitCode::NotFound,
        family: "Beta",
        summary: "Beta is gone",
    };
    /// A slug `Alpha` and `Beta` both declare, so the registry's order is observable.
    const SHARED_ALPHA: DetailEntry = DetailEntry {
        slug: "shared",
        exit_code: ExitCode::IoError,
        family: "Alpha",
        summary: "Described by Alpha",
    };
    const SHARED_BETA: DetailEntry = DetailEntry {
        slug: "shared",
        exit_code: ExitCode::IoError,
        family: "Beta",
        summary: "Described by Beta",
    };

    #[derive(Debug)]
    enum Alpha {
        Bad,
        Deferred,
    }

    impl fmt::Display for Alpha {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("alpha")
        }
    }
    impl std::error::Error for Alpha {}

    impl ClassifyExitCode for Alpha {
        fn classify(&self) -> Option<ExitCode> {
            match self {
                Self::Bad => Some(ExitCode::DataError),
                Self::Deferred => None,
            }
        }
    }

    impl ClassifyErrorKind for Alpha {
        const DETAILS: &'static [DetailEntry] = &[ALPHA_ROW, ALPHA_FALLBACK, SHARED_ALPHA];

        fn kind_detail(&self) -> Detail<'_> {
            match self {
                Self::Bad => Detail::Fixed(&ALPHA_ROW),
                Self::Deferred => Detail::Chain {
                    fallback: &ALPHA_FALLBACK,
                    from: self,
                },
            }
        }
    }

    #[derive(Debug)]
    struct Beta;

    impl fmt::Display for Beta {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("beta")
        }
    }
    impl std::error::Error for Beta {}

    impl ClassifyExitCode for Beta {
        fn classify(&self) -> Option<ExitCode> {
            Some(ExitCode::NotFound)
        }
    }

    impl ClassifyErrorKind for Beta {
        const DETAILS: &'static [DetailEntry] = &[BETA_ROW, SHARED_BETA];

        fn kind_detail(&self) -> Detail<'_> {
            Detail::Fixed(&BETA_ROW)
        }
    }

    const GAMMA_ROW: DetailEntry = DetailEntry {
        slug: "gamma_gone",
        exit_code: ExitCode::NotFound,
        family: "Gamma",
        summary: "Gamma is gone",
    };

    /// Listed `: rows_only`: classifies like `Beta`, yet is no rung.
    #[derive(Debug)]
    struct Gamma;

    impl fmt::Display for Gamma {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("gamma")
        }
    }
    impl std::error::Error for Gamma {}

    impl ClassifyExitCode for Gamma {
        fn classify(&self) -> Option<ExitCode> {
            Some(ExitCode::NotFound)
        }
    }

    impl ClassifyErrorKind for Gamma {
        const DETAILS: &'static [DetailEntry] = &[GAMMA_ROW];

        fn kind_detail(&self) -> Detail<'_> {
            Detail::Fixed(&GAMMA_ROW)
        }
    }

    /// A type outside the list, to prove the ladder downcasts exactly.
    #[derive(Debug)]
    struct Unlisted;

    impl fmt::Display for Unlisted {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("unlisted")
        }
    }
    impl std::error::Error for Unlisted {}

    #[expect(
        unreachable_pub,
        reason = "the macro emits `pub` items for a reachable module; this private test module reaches nobody"
    )]
    mod ladder {
        crate::families!(super::Alpha, super::Beta, super::Gamma: rows_only);
    }
    use ladder::{detail_registry, try_classify};

    fn decide(cause: &(dyn std::error::Error + 'static)) -> Option<(ExitCode, &'static str)> {
        try_classify(cause).map(|decision| (decision.code, decision.detail.fallback_entry().slug))
    }

    /// Reds on: a listed type no longer reached, or a type's code paired with another type's slug.
    #[test]
    fn ladder_answers_each_listed_type_with_its_own_code_and_slug() {
        assert_eq!(decide(&Alpha::Bad), Some((ExitCode::DataError, "alpha_bad")));
        assert_eq!(decide(&Beta), Some((ExitCode::NotFound, "beta_gone")));
    }

    /// Reds on: a type that defers (`classify` is `None`) being reported as classified.
    #[test]
    fn ladder_passes_over_a_type_that_defers_and_over_an_unlisted_one() {
        assert_eq!(decide(&Alpha::Deferred), None);
        assert_eq!(decide(&Unlisted), None);
        assert_eq!(decide(&Gamma), None, "a rows-only type is no rung");
    }

    /// Reds on: `Chain` collapsing to a `Fixed` slug before the CLI can resolve it from the chain.
    #[test]
    fn chain_detail_reaches_the_resolver_with_its_fallback() {
        let deferred = Alpha::Deferred;
        let chain = deferred.kind_detail();
        let Detail::Chain { fallback, from } = chain else {
            panic!("a deferred arm names a chain, not a fixed slug");
        };
        assert_eq!(fallback.slug, "alpha_chain");
        assert!(
            std::ptr::addr_eq(from, &deferred),
            "the chain starts at the error itself"
        );
        assert_eq!(chain.fallback_entry().slug, "alpha_chain");
        assert_eq!(Detail::Fixed(&BETA_ROW).fallback_entry().slug, "beta_gone");
    }

    /// Reds on: families listed out of order, or one dropped; the shared slug's first description must win.
    #[test]
    fn registry_lists_rows_in_list_order() {
        let rows: Vec<(&str, &str)> = detail_registry().iter().map(|row| (row.slug, row.family)).collect();
        assert_eq!(
            rows,
            [
                ("alpha_bad", "Alpha"),
                ("alpha_chain", "Alpha"),
                ("shared", "Alpha"),
                ("beta_gone", "Beta"),
                ("shared", "Beta"),
                ("gamma_gone", "Gamma"),
            ]
        );
    }
}
