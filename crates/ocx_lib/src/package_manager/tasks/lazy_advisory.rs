// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Lazy-mode advisory detector.
//!
//! A pure classifier over a single package's already-loaded metadata,
//! surfacing the three ways a package's own declarations make lazy
//! composition observably different from eager composition. See plan
//! contract C-015 (`plan_lazy_package_loading.md`).
//!
//! **Warning only, never a decision.** [`classify_lazy_advisories`] has no
//! way to fail composition or steer resolution — every finding is an
//! informational [`LazyAdvisory`], emitted at lock/compose time for a
//! **deferred** tool only. Nothing downstream may treat a `LazyAdvisory` as
//! anything but advisory.

use crate::oci;
use crate::package::metadata::Metadata;
use crate::package::metadata::env::modifier::Modifier;
use crate::package::metadata::template::classify_install_path_rooted_dir;

/// The package-rooted interpolation token.
///
/// Spelled here rather than imported: `template.rs` owns only the
/// `"${installPath}/"` *directory prefix* (inside
/// [`classify_install_path_rooted_dir`]), and `package::libc_lint` — the other
/// classifier that reads declared `path` values segment by segment — spells the
/// bare token locally for the same reason.
const INSTALL_PATH_TOKEN: &str = "${installPath}";

/// A non-fatal observation about a deferred tool's declared metadata.
///
/// Each variant names one way a package's own declarations make lazy
/// composition observably different from eager composition — see the
/// module doc comment for the "warning only" contract. Emitted at
/// lock/compose time for a **deferred** tool only (never for a tool that
/// materializes eagerly), and serialized verbatim under `--format json`.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LazyAdvisory {
    /// An env var whose value interpolates `${installPath}` but is declared
    /// with a non-`path` modifier (`constant` or `list`).
    ///
    /// A `path`-kind entry pointing into an unmaterialized package is
    /// harmless — nothing resolves it until the shim fires on first
    /// invocation. A `constant`/`list` entry hands that same unmaterialized
    /// path to a tool that may `stat` it immediately, which is not.
    InstallPathRootedNonPathVar {
        /// The deferred tool whose metadata declared the var.
        package: oci::PinnedIdentifier,
        /// The declared env-var name.
        key: String,
    },
    /// The package's `binaries` claim is absent (`None`, not `Some([])`),
    /// so the interface name set the shim generator needs is not
    /// enumerable.
    UndeclaredBinaries {
        /// The deferred tool with no `binaries` claim.
        package: oci::PinnedIdentifier,
    },
    /// A `path`-modifier value concatenates a package-rooted
    /// `${installPath}` segment with something else — a literal
    /// prefix/suffix or a second token (`${deps.*}`) — so the shim slot
    /// cannot be substituted cleanly. Only the exact `${installPath}/<rel>`
    /// shape substitutes cleanly.
    CombinedPathValue {
        /// The deferred tool whose metadata declared the var.
        package: oci::PinnedIdentifier,
        /// The declared env-var name.
        key: String,
    },
}

impl std::fmt::Display for LazyAdvisory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InstallPathRootedNonPathVar { package, key } => write!(
                f,
                "{package}: '{key}' interpolates ${{installPath}} in a non-path variable; \
                 its value will not resolve until the package materializes"
            ),
            Self::UndeclaredBinaries { package } => write!(
                f,
                "{package}: no binaries claim declared; the interface name set is not enumerable"
            ),
            Self::CombinedPathValue { package, key } => write!(
                f,
                "{package}: '{key}' combines ${{installPath}} with another value; \
                 the shim slot cannot be substituted cleanly"
            ),
        }
    }
}

/// Classifies `metadata`'s declarations for `package` into lazy-mode
/// advisories.
///
/// Pure and warning-only — see the module doc comment. Reads only the
/// already-loaded `metadata`; performs no I/O and touches neither the
/// filesystem nor the network. Iterates every declared env var regardless
/// of surface visibility — the caller (lock/compose) decides which surface,
/// and which deferred tools, to run this over.
///
/// Free function, not a [`crate::package_manager::PackageManager`] method:
/// per `subsystem-package-manager.md`, only facade operations that need
/// `&self` state (file structure, index, client) hang off `impl
/// PackageManager`. This classifier needs none of that — its only inputs
/// are a package identifier and its already-loaded metadata — so it stays a
/// plain free function taking explicit params, following the
/// `tasks/common.rs` shared-helper convention.
pub fn classify_lazy_advisories(package: &oci::PinnedIdentifier, metadata: &Metadata) -> Vec<LazyAdvisory> {
    let mut advisories = Vec::new();

    // `None` is "the publisher declared nothing"; `Some([])` is "the publisher
    // declared zero". Only the former leaves the shim generator without an
    // enumerable name set.
    if metadata.binaries().is_none() {
        advisories.push(LazyAdvisory::UndeclaredBinaries {
            package: package.clone(),
        });
    }

    for var in metadata.env().into_iter().flatten() {
        // `None` here is `Modifier::Unknown` — a `type` tag a newer ocx defines,
        // whose value fields this binary cannot interpret. The value is then
        // neither provably package-rooted nor provably not, and a warning-only
        // classifier stays silent rather than guess (C-015 (b)).
        let Some(value) = var.value() else { continue };
        if !value.contains(INSTALL_PATH_TOKEN) {
            continue;
        }

        let advisory = match &var.modifier {
            Modifier::Path(_) if path_value_substitutes_cleanly(value) => continue,
            Modifier::Path(_) => LazyAdvisory::CombinedPathValue {
                package: package.clone(),
                key: var.key.clone(),
            },
            Modifier::Constant(_) | Modifier::List(_) => LazyAdvisory::InstallPathRootedNonPathVar {
                package: package.clone(),
                key: var.key.clone(),
            },
            // Unreachable — `Var::value()` returned `None` for this variant
            // above. Matched explicitly rather than through a wildcard so a
            // future modifier type has to be classified deliberately.
            Modifier::Unknown { .. } => continue,
        };
        advisories.push(advisory);
    }

    advisories
}

/// Whether a `path`-modifier value is package-rooted in the one shape the shim
/// slot can substitute: the whole value is a single `${installPath}`-rooted
/// directory.
///
/// Two clean shapes, and everything else is a concatenation:
///
/// - a bare `${installPath}` — the content root itself, the most trivially
///   substitutable shape there is (C-015 (a); [`classify_install_path_rooted_dir`]
///   returns `None` for it because it strips the literal `"${installPath}/"`
///   prefix, so the bare form is checked beside that helper, never by a second
///   `${installPath}` parser);
/// - `${installPath}/<rel>`, which is exactly what that helper classifies —
///   including its `<rel>` contains-`${` exclusion, which covers the
///   second-token form `${installPath}/bin:${deps.other.installPath}/bin`.
///
/// A `PATH` value is a separator-joined list, so more than one segment means the
/// package-rooted part is concatenated with something else regardless of what
/// that something is. Split on `:` rather than [`std::env::split_paths`] for the
/// reason `package::libc_lint::resolve_scan_scope` gives: the value is authored
/// for the *target*, so the build host's separator is the wrong one. Residual: a
/// Windows-targeted `;`-joined value whose segments carry neither `:` nor a
/// second token reads as one segment and is not flagged — a missed warning, and
/// warnings are all this function feeds.
fn path_value_substitutes_cleanly(value: &str) -> bool {
    let mut segments = value.split(':');
    let Some(only) = segments.next() else {
        return false;
    };
    if segments.next().is_some() {
        return false;
    }
    only == INSTALL_PATH_TOKEN || classify_install_path_rooted_dir(only).is_some()
}

#[cfg(test)]
mod tests {
    //! Specification tests for C-015, written from the plan's component
    //! contract before the classifier body exists.
    //!
    //! **No fixture here touches the filesystem or the network.** Every input
    //! is an in-memory [`Metadata`] and an in-memory [`oci::PinnedIdentifier`];
    //! no `TempDir`, no `tokio`, no transport. That is the executable half of
    //! C-015's purity claim — the other half is
    //! [`classify_lazy_advisories_takes_only_an_identifier_and_metadata`],
    //! which pins the signature so no I/O handle can be threaded in later.

    use std::collections::BTreeSet;

    use super::*;
    use crate::oci::{Digest, Identifier};
    use crate::package::metadata::bundle::{Bundle, Version};
    use crate::package::metadata::dependency::Dependencies;
    use crate::package::metadata::env::list::List;
    use crate::package::metadata::env::var::{Modifier, Var};
    use crate::package::metadata::env::{Env, EnvBuilder};
    use crate::package::metadata::visibility::Visibility;
    use crate::package::metadata::{Binaries, BinaryName, Entrypoints};

    const REGISTRY: &str = "example.com";

    // ── Fixtures ─────────────────────────────────────────────────────────────

    fn pinned(repository: &str) -> oci::PinnedIdentifier {
        let identifier =
            Identifier::new_registry(repository, REGISTRY).clone_with_digest(Digest::Sha256("a".repeat(64)));
        oci::PinnedIdentifier::try_from(identifier).expect("fixture identifier carries a digest")
    }

    fn path_var(key: &str, value: &str) -> Var {
        Var::new_path(key, value, false)
    }

    fn constant_var(key: &str, value: &str) -> Var {
        Var::new_constant(key, value)
    }

    fn list_var(key: &str, value: &str) -> Var {
        Var {
            key: key.to_string(),
            modifier: Modifier::List(List {
                separator: Some(":".to_string()),
                value: value.to_string(),
            }),
            visibility: Visibility::PRIVATE,
        }
    }

    fn env_of(vars: Vec<Var>) -> Env {
        let mut builder = EnvBuilder::new();
        for var in vars {
            builder.add_var(var);
        }
        builder.build()
    }

    fn declared_binaries(names: &[&str]) -> Binaries {
        let set: BTreeSet<BinaryName> = names
            .iter()
            .map(|name| BinaryName::try_from(*name).expect("fixture binary name is valid"))
            .collect();
        Binaries::try_from(set).expect("fixture binary names do not case-fold collide")
    }

    /// Metadata carrying `vars` and the given `binaries` claim.
    ///
    /// Negative tests pass `Some(Binaries::default())` — an *explicit* empty
    /// claim — so the only advisory their fixture could produce is the one
    /// under test; the assertion is then that the whole result is empty, not
    /// merely that one variant is absent.
    fn metadata_with(vars: Vec<Var>, binaries: Option<Binaries>) -> Metadata {
        Metadata::Bundle(Bundle {
            version: Version::V1,
            strip_components: None,
            env: env_of(vars),
            dependencies: Dependencies::default(),
            entrypoints: Entrypoints::default(),
            binaries,
        })
    }

    /// Projects advisories onto comparable `"<kind>:<key>"` strings, sorted.
    ///
    /// C-015 fixes no emission order, so the multi-finding assertion compares
    /// sorted sets rather than positions — see the Specify report's ordering
    /// note. `LazyAdvisory` derives no `PartialEq`/`Ord` of its own, which is
    /// why this projects instead of comparing values.
    fn summarize(advisories: &[LazyAdvisory]) -> Vec<String> {
        let mut summary: Vec<String> = advisories
            .iter()
            .map(|advisory| match advisory {
                LazyAdvisory::InstallPathRootedNonPathVar { key, .. } => {
                    format!("install_path_rooted_non_path_var:{key}")
                }
                LazyAdvisory::UndeclaredBinaries { .. } => "undeclared_binaries:".to_string(),
                LazyAdvisory::CombinedPathValue { key, .. } => format!("combined_path_value:{key}"),
            })
            .collect();
        summary.sort();
        summary
    }

    /// The first whitespace-delimited token made purely of ASCII letters.
    ///
    /// Skips the interpolated identifier prefix and a quoted env-var key —
    /// neither is message prose, and a key like `LD_LIBRARY_PATH` is the
    /// publisher's casing, not ours.
    fn first_prose_word(message: &str) -> &str {
        message
            .split_whitespace()
            .find(|word| !word.is_empty() && word.chars().all(|character| character.is_ascii_alphabetic()))
            .expect("every advisory message carries at least one plain word")
    }

    // ── C-015: InstallPathRootedNonPathVar ───────────────────────────────────

    #[test]
    fn install_path_rooted_constant_var_is_flagged() {
        let package = pinned("cmake");
        let metadata = metadata_with(
            vec![constant_var("CMAKE_ROOT", "${installPath}/share/cmake")],
            Some(Binaries::default()),
        );

        let advisories = classify_lazy_advisories(&package, &metadata);

        assert_eq!(
            summarize(&advisories),
            vec!["install_path_rooted_non_path_var:CMAKE_ROOT"],
            "a constant var interpolating ${{installPath}} hands an unmaterialized path to a tool that may stat it"
        );
    }

    #[test]
    fn install_path_rooted_list_var_is_flagged() {
        let package = pinned("cmake");
        let metadata = metadata_with(
            vec![list_var("CMAKE_PREFIX_PATH", "${installPath}/lib/cmake")],
            Some(Binaries::default()),
        );

        let advisories = classify_lazy_advisories(&package, &metadata);

        assert_eq!(
            summarize(&advisories),
            vec!["install_path_rooted_non_path_var:CMAKE_PREFIX_PATH"],
            "`list` is a non-path modifier and carries the same hazard as `constant`"
        );
    }

    #[test]
    fn install_path_rooted_path_var_is_not_flagged() {
        let package = pinned("cmake");
        let metadata = metadata_with(vec![path_var("PATH", "${installPath}/bin")], Some(Binaries::default()));

        let advisories = classify_lazy_advisories(&package, &metadata);

        assert!(
            advisories.is_empty(),
            "a `path` entry into an unmaterialized package is harmless — nothing resolves it \
             until the shim fires; got {:?}",
            summarize(&advisories)
        );
    }

    #[test]
    fn constant_var_without_install_path_is_not_flagged() {
        let package = pinned("cmake");
        let metadata = metadata_with(
            vec![constant_var("CMAKE_GENERATOR", "Ninja")],
            Some(Binaries::default()),
        );

        let advisories = classify_lazy_advisories(&package, &metadata);

        assert!(
            advisories.is_empty(),
            "the variant fires on ${{installPath}} interpolation, not on the modifier alone; got {:?}",
            summarize(&advisories)
        );
    }

    // ── C-015: UndeclaredBinaries ────────────────────────────────────────────

    #[test]
    fn absent_binaries_claim_is_flagged() {
        let package = pinned("cmake");
        let metadata = metadata_with(vec![], None);

        let advisories = classify_lazy_advisories(&package, &metadata);

        assert_eq!(
            summarize(&advisories),
            vec!["undeclared_binaries:"],
            "`None` means the interface name set the shim generator needs is not enumerable"
        );
    }

    #[test]
    fn empty_binaries_claim_is_not_flagged() {
        let package = pinned("cmake");
        let metadata = metadata_with(vec![], Some(Binaries::default()));

        let advisories = classify_lazy_advisories(&package, &metadata);

        assert!(
            advisories.is_empty(),
            "`Some([])` is an explicit publisher claim of zero binaries — absent and empty are \
             distinct wire states and must stay distinct here; got {:?}",
            summarize(&advisories)
        );
    }

    #[test]
    fn non_empty_binaries_claim_is_not_flagged() {
        let package = pinned("cmake");
        let metadata = metadata_with(vec![], Some(declared_binaries(&["cmake", "ctest"])));

        let advisories = classify_lazy_advisories(&package, &metadata);

        assert!(
            advisories.is_empty(),
            "a populated claim is the enumerable case; got {:?}",
            summarize(&advisories)
        );
    }

    // ── C-015: CombinedPathValue ─────────────────────────────────────────────

    #[test]
    fn path_value_combining_install_path_with_a_dep_token_is_flagged() {
        let package = pinned("cmake");
        let metadata = metadata_with(
            vec![path_var("PATH", "${installPath}/bin:${deps.ninja.installPath}/bin")],
            Some(Binaries::default()),
        );

        let advisories = classify_lazy_advisories(&package, &metadata);

        assert_eq!(
            summarize(&advisories),
            vec!["combined_path_value:PATH"],
            "a second token means the shim slot cannot be substituted cleanly"
        );
    }

    #[test]
    fn path_value_with_a_literal_prefix_is_flagged() {
        let package = pinned("cmake");
        let metadata = metadata_with(
            vec![path_var("PATH", "/opt/wrapper/bin:${installPath}/bin")],
            Some(Binaries::default()),
        );

        let advisories = classify_lazy_advisories(&package, &metadata);

        assert_eq!(
            summarize(&advisories),
            vec!["combined_path_value:PATH"],
            "a literal segment before the token is a concatenation just as a second token is"
        );
    }

    #[test]
    fn path_value_with_a_literal_suffix_is_flagged() {
        let package = pinned("cmake");
        let metadata = metadata_with(
            vec![path_var("PATH", "${installPath}/bin:/usr/local/bin")],
            Some(Binaries::default()),
        );

        let advisories = classify_lazy_advisories(&package, &metadata);

        assert_eq!(
            summarize(&advisories),
            vec!["combined_path_value:PATH"],
            "a literal segment after the token is a concatenation too"
        );
    }

    #[test]
    fn clean_install_path_rooted_path_value_is_not_flagged() {
        let package = pinned("cmake");
        let metadata = metadata_with(vec![path_var("PATH", "${installPath}/bin")], Some(Binaries::default()));

        let advisories = classify_lazy_advisories(&package, &metadata);

        assert!(
            advisories.is_empty(),
            "only the exact ${{installPath}}/<rel> shape substitutes cleanly, and it is the shape \
             the shim slot is built for; got {:?}",
            summarize(&advisories)
        );
    }

    #[test]
    fn path_value_without_install_path_is_not_flagged() {
        let package = pinned("cmake");
        let metadata = metadata_with(vec![path_var("PATH", "/usr/local/bin")], Some(Binaries::default()));

        let advisories = classify_lazy_advisories(&package, &metadata);

        assert!(
            advisories.is_empty(),
            "with no package-rooted segment there is nothing for the shim slot to substitute; got {:?}",
            summarize(&advisories)
        );
    }

    /// C-015 (a), closed 2026-08-10 and previously unguarded: a value that is
    /// *exactly* `${installPath}` fires nothing.
    ///
    /// The clean-shape predicate cannot be
    /// `metadata::template::classify_install_path_rooted_dir` alone — that helper
    /// strips the literal `"${installPath}/"` prefix and so returns `None` here,
    /// which would classify the most trivially substitutable shape there is as a
    /// *concatenation*. Deleting the bare-token arm beside it reds only this test.
    #[test]
    fn bare_install_path_under_a_path_modifier_is_not_flagged() {
        let package = pinned("cmake");
        let metadata = metadata_with(vec![path_var("PATH", "${installPath}")], Some(Binaries::default()));

        let advisories = classify_lazy_advisories(&package, &metadata);

        assert!(
            advisories.is_empty(),
            "a bare ${{installPath}} is exactly package-rooted, not a concatenation; got {:?}",
            summarize(&advisories)
        );
    }

    // ── C-015 (b): an unreadable modifier ────────────────────────────────────

    /// C-015 (b), closed 2026-08-10 and previously unguarded: a `type` tag this
    /// binary does not know emits nothing.
    ///
    /// `Var::value()` returns `None` for [`Modifier::Unknown`], so the value is
    /// neither provably `${installPath}`-rooted nor provably not — and an
    /// advisory is a warning, so silence beats a finding derived from a value
    /// that was never read. The fixture pairs it with an explicit empty
    /// `binaries` claim so the whole result must be empty.
    #[test]
    fn unknown_modifier_var_is_not_flagged() {
        let package = pinned("cmake");
        let metadata = metadata_with(
            vec![Var {
                key: "GODEBUG".to_string(),
                modifier: Modifier::Unknown {
                    type_name: "frobnicate".to_string(),
                },
                visibility: Visibility::PRIVATE,
            }],
            Some(Binaries::default()),
        );

        let advisories = classify_lazy_advisories(&package, &metadata);

        assert!(
            advisories.is_empty(),
            "a modifier this binary cannot read yields no advisory; got {:?}",
            summarize(&advisories)
        );
    }

    // ── C-015: several findings in one metadata ──────────────────────────────

    #[test]
    fn metadata_with_several_offenders_yields_one_advisory_per_finding() {
        let package = pinned("cmake");
        let metadata = metadata_with(
            vec![
                constant_var("CMAKE_ROOT", "${installPath}/share/cmake"),
                list_var("CMAKE_PREFIX_PATH", "${installPath}/lib/cmake"),
                path_var("PATH", "${installPath}/bin:${deps.ninja.installPath}/bin"),
                path_var("MANPATH", "${installPath}/share/man"),
            ],
            None,
        );

        let advisories = classify_lazy_advisories(&package, &metadata);

        assert_eq!(
            summarize(&advisories),
            vec![
                "combined_path_value:PATH",
                "install_path_rooted_non_path_var:CMAKE_PREFIX_PATH",
                "install_path_rooted_non_path_var:CMAKE_ROOT",
                "undeclared_binaries:",
            ],
            "each finding is its own advisory; the clean MANPATH entry contributes none"
        );
    }

    #[test]
    fn every_advisory_names_the_package_it_was_classified_for() {
        let package = pinned("cmake");
        let metadata = metadata_with(
            vec![
                constant_var("CMAKE_ROOT", "${installPath}/share/cmake"),
                path_var("PATH", "${installPath}/bin:/usr/local/bin"),
            ],
            None,
        );

        let advisories = classify_lazy_advisories(&package, &metadata);

        assert_eq!(advisories.len(), 3, "fixture declares three findings");
        for advisory in &advisories {
            let named = match advisory {
                LazyAdvisory::InstallPathRootedNonPathVar { package, .. }
                | LazyAdvisory::UndeclaredBinaries { package }
                | LazyAdvisory::CombinedPathValue { package, .. } => package,
            };
            assert_eq!(
                named, &package,
                "every advisory carries the package it was classified for"
            );
        }
    }

    // ── C-015: purity ────────────────────────────────────────────────────────

    /// The classifier takes an identifier and metadata, and nothing else.
    ///
    /// C-015 calls it pure; this is the part a test can hold. Coercing the
    /// function item to this exact `fn` pointer type fails to compile if a
    /// parameter is added (a `&FileStructure`, an `&oci::Client`), if it
    /// becomes `async`, or if the return type moves — so no I/O capability can
    /// be threaded in without this reddening the build.
    #[test]
    fn classify_lazy_advisories_takes_only_an_identifier_and_metadata() {
        let signature: fn(&oci::PinnedIdentifier, &Metadata) -> Vec<LazyAdvisory> = classify_lazy_advisories;
        let _ = signature;
    }

    // ── C-015: serialized shape (`--format json`) ────────────────────────────

    fn expected_package_string() -> String {
        format!("{REGISTRY}/cmake@sha256:{}", "a".repeat(64))
    }

    #[test]
    fn install_path_rooted_non_path_var_serializes_with_its_kind_tag() {
        let advisory = LazyAdvisory::InstallPathRootedNonPathVar {
            package: pinned("cmake"),
            key: "CMAKE_ROOT".to_string(),
        };

        assert_eq!(
            serde_json::to_value(&advisory).expect("advisory serializes"),
            serde_json::json!({
                "kind": "install_path_rooted_non_path_var",
                "package": expected_package_string(),
                "key": "CMAKE_ROOT",
            })
        );
    }

    #[test]
    fn undeclared_binaries_serializes_with_its_kind_tag() {
        let advisory = LazyAdvisory::UndeclaredBinaries {
            package: pinned("cmake"),
        };

        assert_eq!(
            serde_json::to_value(&advisory).expect("advisory serializes"),
            serde_json::json!({
                "kind": "undeclared_binaries",
                "package": expected_package_string(),
            })
        );
    }

    #[test]
    fn combined_path_value_serializes_with_its_kind_tag() {
        let advisory = LazyAdvisory::CombinedPathValue {
            package: pinned("cmake"),
            key: "PATH".to_string(),
        };

        assert_eq!(
            serde_json::to_value(&advisory).expect("advisory serializes"),
            serde_json::json!({
                "kind": "combined_path_value",
                "package": expected_package_string(),
                "key": "PATH",
            })
        );
    }

    // ── Display style (Rust API Guidelines C-GOOD-ERR) ───────────────────────

    fn every_variant() -> Vec<LazyAdvisory> {
        vec![
            LazyAdvisory::InstallPathRootedNonPathVar {
                package: pinned("cmake"),
                key: "LD_LIBRARY_PATH".to_string(),
            },
            LazyAdvisory::UndeclaredBinaries {
                package: pinned("cmake"),
            },
            LazyAdvisory::CombinedPathValue {
                package: pinned("cmake"),
                key: "PATH".to_string(),
            },
        ]
    }

    #[test]
    fn advisory_messages_carry_no_trailing_punctuation() {
        for advisory in every_variant() {
            let message = advisory.to_string();
            assert!(
                !message.ends_with('.') && !message.ends_with('!'),
                "advisory messages are concise sentences without trailing punctuation; got: {message}"
            );
        }
    }

    #[test]
    fn advisory_messages_open_their_prose_lowercase() {
        for advisory in every_variant() {
            let message = advisory.to_string();
            let word = first_prose_word(&message);
            assert!(
                word.chars().all(|character| character.is_ascii_lowercase()),
                "advisory prose is lowercase; '{word}' is not, in: {message}"
            );
        }
    }
}
