// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Which forge a coordinate lives on, which write transport it is driven
//! through, and how a client for the pair is built.

use super::{ForgeCredentials, ForgeError, GitBinary, GitHubForge, GitLabForge, RepoCoordinate};

/// How a run writes to the index repository.
///
/// [`Api`](Self::Api) is the default, and the default is contracted rather than
/// incidental: it is what makes "nothing changes for existing announce users"
/// literally true. The value spellings are contracted for the same reason —
/// they are a CLI flag's accepted values, so `api` and `git` are a published
/// grammar.
///
/// [`Git`](Self::Git) exists for one credential shape the REST API refuses: a
/// GitLab CI job token can push over HTTP but cannot open a merge request, so
/// the request is created by push options carried on the push itself.
///
/// A closed internal enum with no `#[non_exhaustive]`, per the arch-principles
/// convention — every match over it stays total, which is what forces a third
/// transport to be classified everywhere the second one is.
///
/// The word `non_exhaustive` two lines up is deliberate prose stating the
/// attribute's *absence*, not the attribute itself — this enum and
/// [`ForgeKind`] must never carry it. `non_exhaustive_policy_holds` strips
/// `//`-prefixed lines before scanning `kind.rs`'s source text, so this
/// explanation does not trip the very denylist it documents; the same guard
/// also asserts the positive side — the attribute on [`ForgeError`]'s own
/// declaration, the one exempt enum in `forge` — not only a zero count here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WriteTransport {
    /// The forge's REST API.
    #[default]
    Api,
    /// A local `git` clone and one authenticated push.
    Git,
}

impl std::fmt::Display for WriteTransport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Api => "api",
            Self::Git => "git",
        })
    }
}

// Hand-written rather than `#[derive(ValueEnum)]`: this crate depends on
// `clap_builder`, which carries the trait but not the derive macro. The
// spellings below and `Display`'s above are the same two strings and must stay
// that way — the flag's accepted values and the report's rendered value are one
// vocabulary, not two.
impl clap_builder::ValueEnum for WriteTransport {
    fn value_variants<'a>() -> &'a [Self] {
        &[Self::Api, Self::Git]
    }

    fn to_possible_value(&self) -> Option<clap_builder::builder::PossibleValue> {
        Some(match self {
            Self::Api => clap_builder::builder::PossibleValue::new("api"),
            Self::Git => clap_builder::builder::PossibleValue::new("git"),
        })
    }
}

/// The forge implementations announce can talk to.
///
/// A closed internal enum with no `#[non_exhaustive]`, per the arch-principles
/// convention: the binary is the only consumer, and every match staying total is
/// what forces a third forge to be classified everywhere it matters — most
/// sharply in credential handling, where a wildcard would send a mutation
/// unauthenticated.
///
/// Same deliberate-prose note as [`WriteTransport`]'s doc comment: the word
/// above is not the attribute, and `non_exhaustive_policy_holds` strips
/// comment lines before it scans for either one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForgeKind {
    /// GitHub.com or a GitHub Enterprise Server instance.
    GitHub,
    /// GitLab.com or a self-managed GitLab instance.
    GitLab,
}

impl ForgeKind {
    /// The forge a canonical host belongs to, or `None` for anything else.
    ///
    /// Only the two hosts whose forge is a fact are recognised. A self-hosted
    /// instance is deliberately **not** guessed: no unauthenticated probe
    /// distinguishes the forges reliably, hostnames carry no convention
    /// (`git.example.com` is equally likely to be either), and guessing wrong
    /// sends the announce credential to the wrong API in the wrong header. Every
    /// surveyed tool that supports both forges makes the operator declare the
    /// kind for a self-hosted host, and so does this.
    #[must_use]
    pub fn from_host(host: Option<&str>) -> Option<Self> {
        match host {
            // No host at all means the default index, which is on GitHub.
            None => Some(Self::GitHub),
            Some(host) if host.eq_ignore_ascii_case("github.com") => Some(Self::GitHub),
            Some(host) if host.eq_ignore_ascii_case("gitlab.com") => Some(Self::GitLab),
            Some(_) => None,
        }
    }

    /// Resolve the forge for `coordinate`, honouring an explicit `declared` kind.
    ///
    /// # Errors
    ///
    /// Returns [`ForgeError::ForgeKindUnknown`] when the host is self-hosted and
    /// nothing was declared.
    pub fn resolve(declared: Option<Self>, coordinate: &RepoCoordinate) -> Result<Self, ForgeError> {
        if let Some(kind) = declared {
            return Ok(kind);
        }
        Self::from_host(coordinate.host.as_deref()).ok_or_else(|| ForgeError::ForgeKindUnknown {
            host: coordinate.host.clone().unwrap_or_default(),
        })
    }

    /// The host this forge lives on when a coordinate names none.
    ///
    /// A coordinate's `host` is `None` for the canonical host, so `None` and
    /// `Some("github.com")` are two spellings of one instance. Anything
    /// comparing two coordinates' hosts must resolve both through here first, or
    /// it decides that `ocx-sh/index` and `github.com/ocx-sh/index` are on
    /// different servers.
    #[must_use]
    pub fn canonical_host(self) -> &'static str {
        match self {
            Self::GitHub => "github.com",
            Self::GitLab => "gitlab.com",
        }
    }

    /// Whether two coordinates name the same instance of this forge.
    ///
    /// Case-folded, and `None` resolved to [`Self::canonical_host`] on both
    /// sides — the same two normalisations [`Self::from_host`] and the API
    /// base-URL builders already apply, so this cannot disagree with where the
    /// requests actually go.
    #[must_use]
    pub fn same_host(self, left: &RepoCoordinate, right: &RepoCoordinate) -> bool {
        let resolve = |coordinate: &RepoCoordinate| {
            coordinate
                .host
                .clone()
                .unwrap_or_else(|| self.canonical_host().to_string())
        };
        resolve(left).eq_ignore_ascii_case(&resolve(right))
    }

    /// Refuse a coordinate this forge cannot express, before a request is made.
    ///
    /// The per-operation clients already refuse a nested namespace where they
    /// use it — but only for the *fork*, because that is the coordinate whose
    /// namespace they interpolate. A nested `--index-repo` on GitHub reached the
    /// wire and came back as a bare 404 reading "no such repository", which is
    /// the misdiagnosis the flatness rule exists to prevent. This is the check
    /// applied to every coordinate a run names, at the point they are all known.
    ///
    /// # Errors
    ///
    /// Returns [`ForgeError::NestedNamespaceUnsupported`] when `coordinate` has a
    /// nested namespace and this forge does not nest.
    pub fn validate_coordinate(self, coordinate: &RepoCoordinate) -> Result<(), ForgeError> {
        match self {
            Self::GitHub => super::github::require_flat_namespace(coordinate).map(|_| ()),
            Self::GitLab => Ok(()),
        }
    }

    /// Refuse a write transport this forge cannot serve. Pure — no network call,
    /// no client built.
    ///
    /// Called from two places on purpose and implemented once: from the CLI's
    /// argv-fault block, so a bad combination is diagnosed before the credential
    /// check, and from [`Self::client`], so no future caller can reach a client
    /// by skipping the first.
    ///
    /// The match is wildcard-free so a third forge cannot inherit a transport
    /// nobody decided it supports — the same reason [`ForgeKind`] itself carries
    /// no `#[non_exhaustive]`. (Deliberate prose, per the note on
    /// [`WriteTransport`]'s doc comment — `non_exhaustive_policy_holds` strips
    /// comment lines before scanning.)
    ///
    /// # Errors
    ///
    /// Returns [`ForgeError::TransportUnsupported`] for GitHub with
    /// [`WriteTransport::Git`]: GitHub has no push-option merge-request
    /// creation, so there is nothing for the second transport to do there.
    pub fn validate_transport(self, transport: WriteTransport) -> Result<(), ForgeError> {
        match (self, transport) {
            (Self::GitHub, WriteTransport::Git) => Err(ForgeError::TransportUnsupported { forge: self, transport }),
            (Self::GitHub, WriteTransport::Api) | (Self::GitLab, WriteTransport::Api | WriteTransport::Git) => Ok(()),
        }
    }

    /// Build the client for this forge, transport and credential pair against
    /// `coordinate`'s host.
    ///
    /// The single place a concrete forge is named. It validates the transport
    /// itself rather than trusting a caller to have done it — and it validates
    /// by *calling* [`Self::validate_transport`], never by repeating the rule,
    /// so the argv-boundary refusal and this one cannot drift apart.
    ///
    /// `git` is the binary the argv-boundary gate already resolved and
    /// version-checked; it is **required** whenever `transport` is
    /// [`WriteTransport::Git`] and unused otherwise.
    ///
    /// # Errors
    ///
    /// Returns [`ForgeError::TransportUnsupported`] (via
    /// [`Self::validate_transport`]) for a transport this forge cannot serve,
    /// [`ForgeError::GitUnavailable`] when the git transport was selected with
    /// no resolved `git`, or [`ForgeError::ClientBuild`] when the hardened HTTP
    /// client cannot be constructed.
    pub fn client(
        self,
        transport: WriteTransport,
        credentials: ForgeCredentials,
        coordinate: &RepoCoordinate,
        git: Option<GitBinary>,
    ) -> Result<Box<dyn super::Forge>, ForgeError> {
        self.validate_transport(transport)?;
        if transport == WriteTransport::Git && git.is_none() {
            return Err(ForgeError::GitUnavailable {
                reason: "no git executable was resolved before the client was built".to_string(),
            });
        }
        let host = coordinate.host.as_deref();
        Ok(match self {
            // GitHub is unreachable with `WriteTransport::Git` — `validate_transport`
            // above refuses that pair — so its client carries neither the transport
            // nor the binary. A field it could never read would be dead by
            // construction and would have to be suppressed forever.
            Self::GitHub => Box::new(GitHubForge::new(credentials, host)?),
            Self::GitLab => Box::new(GitLabForge::new(credentials, transport, git, host)?),
        })
    }
}

impl std::fmt::Display for ForgeKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::GitHub => "github",
            Self::GitLab => "gitlab",
        })
    }
}

impl clap_builder::ValueEnum for ForgeKind {
    fn value_variants<'a>() -> &'a [Self] {
        &[Self::GitHub, Self::GitLab]
    }

    fn to_possible_value(&self) -> Option<clap_builder::builder::PossibleValue> {
        Some(match self {
            Self::GitHub => clap_builder::builder::PossibleValue::new("github"),
            Self::GitLab => clap_builder::builder::PossibleValue::new("gitlab"),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::forge::{ForgeToken, GitVersion};

    fn coordinate(value: &str) -> RepoCoordinate {
        value.parse().expect("valid coordinate")
    }

    fn credentials() -> ForgeCredentials {
        ForgeCredentials::new(ForgeToken::new("token".to_string()))
    }

    #[test]
    fn canonical_hosts_resolve_without_a_declaration() {
        assert_eq!(
            ForgeKind::resolve(None, &coordinate("ocx-sh/index")).expect("no host is the default index"),
            ForgeKind::GitHub
        );
        assert_eq!(
            ForgeKind::resolve(None, &coordinate("github.com/ocx-sh/index")).expect("canonical GitHub"),
            ForgeKind::GitHub
        );
        assert_eq!(
            ForgeKind::resolve(None, &coordinate("gitlab.com/acme/index")).expect("canonical GitLab"),
            ForgeKind::GitLab
        );
    }

    #[test]
    fn a_self_hosted_host_is_refused_rather_than_guessed() {
        let error = ForgeKind::resolve(None, &coordinate("git.example.com/acme/index"))
            .expect_err("a self-hosted host must not be guessed");
        assert!(matches!(error, ForgeError::ForgeKindUnknown { .. }), "got {error:?}");
        // ...and is accepted the moment the operator says which forge it is.
        assert_eq!(
            ForgeKind::resolve(Some(ForgeKind::GitLab), &coordinate("git.example.com/acme/team/index"))
                .expect("declared kind wins"),
            ForgeKind::GitLab
        );
    }

    #[test]
    fn a_declaration_overrides_even_a_canonical_host() {
        // Not a hypothetical: an instance can be reverse-proxied under a name
        // that looks canonical, and the operator's word beats the heuristic.
        assert_eq!(
            ForgeKind::resolve(Some(ForgeKind::GitLab), &coordinate("github.com/acme/index"))
                .expect("declaration wins"),
            ForgeKind::GitLab
        );
    }

    #[test]
    fn an_omitted_host_is_the_canonical_host_not_a_different_one() {
        // The regression this pins: `--index-repo ocx-sh/index --fork
        // github.com/me/index` names ONE instance twice. Comparing the two
        // `Option<String>` hosts directly makes them differ, and the run is
        // refused with a message that contradicts itself.
        assert!(
            ForgeKind::GitHub.same_host(&coordinate("ocx-sh/index"), &coordinate("github.com/me/index")),
            "an omitted host must resolve to the forge's canonical host"
        );
        assert!(
            ForgeKind::GitHub.same_host(&coordinate("github.com/me/index"), &coordinate("ocx-sh/index")),
            "and symmetrically"
        );
        assert!(
            ForgeKind::GitLab.same_host(&coordinate("acme/index"), &coordinate("gitlab.com/me/index")),
            "the canonical host is per forge, not a single constant"
        );
        assert!(
            ForgeKind::GitHub.same_host(&coordinate("GitHub.COM/a/b"), &coordinate("github.com/c/d")),
            "hosts are compared case-insensitively, as DNS and every URL builder here do"
        );
        // The falsifying half: genuinely different instances still differ, and a
        // canonical host is not the same as a self-hosted one.
        assert!(
            !ForgeKind::GitHub.same_host(&coordinate("github.example.com/a/b"), &coordinate("ocx-sh/index")),
            "a self-hosted host must not collapse onto the canonical one"
        );
        assert!(
            !ForgeKind::GitLab.same_host(&coordinate("a.example.com/x/y"), &coordinate("b.example.com/x/y")),
            "two self-hosted instances must stay distinct"
        );
    }

    /// C-013/C-016: the one forge-and-transport pair no implementation can
    /// serve, refused purely and up front — no network call, no client built.
    ///
    /// Both arms are asserted. A gate that refuses everything passes every
    /// refusal test, so the three legal pairs are named too — most sharply
    /// GitLab with `git`, which is the entire reason the second transport
    /// exists.
    ///
    /// Reds on: dropping the `(GitHub, Git)` arm, or widening the refusal to
    /// any other pair.
    #[test]
    fn validate_transport_refuses_github_git() {
        let error = ForgeKind::GitHub
            .validate_transport(WriteTransport::Git)
            .expect_err("GitHub has no push-option merge-request creation");
        assert!(
            matches!(
                error,
                ForgeError::TransportUnsupported {
                    forge: ForgeKind::GitHub,
                    transport: WriteTransport::Git
                }
            ),
            "got {error:?}"
        );

        for (kind, transport) in [
            (ForgeKind::GitHub, WriteTransport::Api),
            (ForgeKind::GitLab, WriteTransport::Api),
            (ForgeKind::GitLab, WriteTransport::Git),
        ] {
            assert!(
                kind.validate_transport(transport).is_ok(),
                "{kind} must serve the {transport} transport"
            );
        }
    }

    /// C-014: the default is contracted, not incidental. It is what makes
    /// "nothing changes by default for existing announce users" literally true
    /// rather than a claim about the CLI's current wiring.
    ///
    /// Reds on: moving `#[default]` to [`WriteTransport::Git`].
    #[test]
    fn write_transport_default_is_api() {
        assert_eq!(WriteTransport::default(), WriteTransport::Api);
    }

    /// C-014: `api` and `git` are a published flag grammar, and the two impls
    /// that carry them must not drift — the flag's accepted values and the
    /// value rendered into the JSON report are one vocabulary, not two.
    ///
    /// Paired against `value_variants()` so the arity is a guard: a third
    /// transport added there without a spelling reds on the length, and a
    /// rename in only one of the two impls reds on its own row.
    ///
    /// Reds on: renaming a spelling in `Display` or in `ValueEnum`, or in only
    /// one of them.
    #[test]
    fn write_transport_value_spellings() {
        use clap_builder::ValueEnum as _;

        let expected = [(WriteTransport::Api, "api"), (WriteTransport::Git, "git")];
        let variants = WriteTransport::value_variants();
        assert_eq!(
            variants.len(),
            expected.len(),
            "every transport owes a flag spelling; variants are {variants:?}"
        );
        for (index, (transport, spelling)) in expected.into_iter().enumerate() {
            assert_eq!(
                variants[index], transport,
                "the flag lists the variants in declaration order"
            );
            assert_eq!(transport.to_string(), spelling, "Display renders the flag's own value");
            let possible = transport
                .to_possible_value()
                .expect("every transport is a selectable flag value");
            assert_eq!(
                possible.get_name(),
                spelling,
                "the flag must accept exactly what Display renders"
            );
        }
    }

    /// C-017 and DX-16: [`ForgeKind::client`] requires the resolved
    /// [`GitBinary`] whenever the git transport is selected, and names that
    /// refusal [`ForgeError::GitUnavailable`] — the same variant C-065's
    /// argv-boundary gate raises for a missing `git`, rather than a twelfth
    /// variant minted for the internal case.
    ///
    /// The accept side carries as much weight as the refusal: a constructor
    /// that refuses every call passes every refusal test.
    /// [`GitVersion::new`] is a `const` constructor and exists so that side is
    /// constructible without resolving a real `git` off `PATH`.
    ///
    /// Reds on: dropping the `git.is_none()` guard (the refusal arm), and on
    /// requiring a binary unconditionally (the three accept arms).
    #[test]
    fn client_requires_git_binary_under_git() {
        let gitlab = coordinate("gitlab.com/acme/index");

        let error = ForgeKind::GitLab
            .client(WriteTransport::Git, credentials(), &gitlab, None)
            .err()
            .expect("the git transport cannot run without a resolved git");
        assert!(matches!(error, ForgeError::GitUnavailable { .. }), "got {error:?}");

        let git = GitBinary {
            path: PathBuf::from("git"),
            version: GitVersion::new(2, 31, 0),
        };
        assert!(
            ForgeKind::GitLab
                .client(WriteTransport::Git, credentials(), &gitlab, Some(git))
                .is_ok(),
            "the same pair must build once the argv gate has resolved a binary"
        );
        assert!(
            ForgeKind::GitLab
                .client(WriteTransport::Api, credentials(), &gitlab, None)
                .is_ok(),
            "a binary is required under `git` only — the API transport spawns nothing"
        );
        assert!(
            ForgeKind::GitHub
                .client(WriteTransport::Api, credentials(), &coordinate("ocx-sh/index"), None)
                .is_ok(),
            "and the requirement is not a blanket refusal on the other forge either"
        );
    }

    /// The arch-principles exhaustiveness rule for this module, held
    /// structurally because it has no runtime observation at all:
    /// `non_exhaustive` is inert inside its own crate, so no behavioural test
    /// can see whether an internal enum carries it.
    ///
    /// The rule: error enums are exempt and carry it — [`ForgeError`] does —
    /// while every other enum in `forge` must not, so each match over one stays
    /// total and a third forge or transport cannot inherit a classification
    /// nobody made.
    ///
    /// Two traps this guard is written around, both named in `quality-rust.md`:
    ///
    /// - **It would measure itself with a plain literal.** `kind.rs` is one of
    ///   the files scanned and this module lives inside it, so the needle is
    ///   assembled from two tokens and never appears whole in the scanned text.
    ///   Comment lines are stripped for the same reason: three doc comments in
    ///   this file quote the attribute while stating its absence.
    /// - **A zero is also what a needle that stopped matching returns.** The
    ///   positive half pins `error.rs`'s count at **exactly one** — the
    ///   attribute on [`ForgeError`]'s own declaration — and the walk asserts it
    ///   actually read the module. Exactly one rather than non-zero because the
    ///   exemption is [`ForgeError`], not the file: a second `#[non_exhaustive]`
    ///   enum declared in `error.rs` is exempted by *filename* here and by
    ///   nothing at all in the arch-principles rule, and a `> 0` assertion
    ///   cannot see it arrive.
    ///
    /// Two stated limits, both a single literal needle's, and both **tripwires
    /// for the likely accident** rather than the contract:
    ///
    /// - It matches the one spelling `rustfmt` emits, so the attribute reached
    ///   through a `cfg_attr` or written with unusual spacing slips past.
    /// - The comment strip is `//`-only, so a `/* … */` block comment quoting
    ///   the attribute counts as a real occurrence and reds the guard. Safe
    ///   direction — a false red, which an author sees — but it is why a future
    ///   reader documenting this rule should use line comments, as every
    ///   explanation in this file does.
    ///
    /// The contract itself is the arch-principles rule; this catches the way it
    /// actually gets broken.
    ///
    /// Reds on: adding the attribute to [`WriteTransport`] (negative half), on
    /// removing it from [`ForgeError`] (positive half), and on declaring a
    /// second `#[non_exhaustive]` enum in `error.rs` (positive half, arity).
    /// All three were run.
    #[test]
    fn non_exhaustive_policy_holds() {
        let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let forge_root = source_root.join("forge");
        let mut files = rust_sources(&forge_root);
        files.push(source_root.join("forge.rs"));
        files.sort();

        // Split so this line does not match the scan it defines.
        let attribute = concat!("#[non_", "exhaustive]");
        let count = |path: &Path| -> usize {
            std::fs::read_to_string(path)
                .expect("a forge source file is readable")
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .filter(|line| line.contains(attribute))
                .count()
        };

        // The positive half. Without it a zero everywhere else is equally the
        // answer a scanner that recognises nothing returns. Pinned at exactly
        // one, not merely non-zero: the exemption belongs to `ForgeError`, and
        // an arity of one is the only way this filename-scoped guard can notice
        // a second exempt-by-accident enum moving into the same file.
        let error_source = forge_root.join("error.rs");
        assert_eq!(
            count(&error_source),
            1,
            "`error.rs` holds exactly one exempt enum, `ForgeError`. Zero means it lost the attribute or the scanner \
             stopped recognising it, and the zero counts below then prove nothing; more than one means a non-error \
             enum is being exempted by filename"
        );

        // The negative half: every enum outside the exempt error module.
        let offenders: Vec<&Path> = files
            .iter()
            .filter(|path| path.as_path() != error_source.as_path())
            .filter(|path| count(path) > 0)
            .map(PathBuf::as_path)
            .collect();
        assert!(
            offenders.is_empty(),
            "internal forge enums stay closed so every match over one is total: {offenders:?}"
        );

        assert!(
            files.len() > 1 && files.contains(&error_source) && files.contains(&forge_root.join("kind.rs")),
            "the walk did not reach the forge module, so it scanned nothing: {files:?}"
        );
    }

    /// Every `.rs` file under `root`, recursing so a submodule directory added
    /// later cannot silently drop out of the scan.
    fn rust_sources(root: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(&directory)
                .expect("the forge source tree is readable")
                .flatten()
            {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.extension().is_some_and(|extension| extension == "rs") {
                    files.push(path);
                }
            }
        }
        files
    }
}
