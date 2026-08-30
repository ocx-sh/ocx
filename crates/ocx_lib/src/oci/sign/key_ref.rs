// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `--key` reference grammar: `[scheme://]<rest>`, in cosign's spelling.
//!
//! One parser serves `ocx package sign`, `attest`, `verify` **and** the
//! `signers` entry in a trust policy, so nothing here may depend on CLI types.
//!
//! Two vocabularies live side by side on purpose ([`Scheme`] and
//! [`KeyBackendKind`], plan D-13): [`Scheme`] is what a *key reference* can
//! name, and it can never be `Keyless` — that would be a lie about what a key
//! reference is. [`KeyBackendKind`] is the reported `signatures[].key_backend`
//! value, which must be able to say `keyless`. `impl From<Scheme>` is the one
//! bridge, and it lives in this file so the two spellings cannot drift.
//!
//! # Grammar, in evaluation order
//!
//! 1. The value contains `://` — the text before the **first** `://` is the
//!    scheme token, and `rest` is the remainder verbatim.
//! 2. Otherwise the whole value is a bare file path — [`Scheme::File`]. This is
//!    cosign's only file spelling, and there is no second one. `env://VAR` is
//!    the exception that proves it: the variable holds the key PEM itself, not
//!    a path to one — cosign's spelling, and the one shape that keeps a key
//!    out of the filesystem on a runner with no writable disk.
//! 3. Except `file:` with a single colon — [`KeyRefError::FileColonPrefix`],
//!    whose message names the bare path as the fix. cosign resolves that string
//!    to a file *literally named* `file:…`, so honouring it as a prefix made one
//!    value name two different files depending on which tool read it. A file
//!    genuinely named that is still addressable, as `file://file:…`.
//! 4. A scheme token outside [`Scheme::SPELLINGS`] — [`KeyRefError::UnknownScheme`].
//! 5. A recognised but unimplemented scheme — [`KeyRefError::UnsupportedBackend`].
//! 6. An empty `rest` — [`KeyRefError::Empty`].
//!
//! Rule 1 keys on `://`, never on a bare `:`, so a Windows drive path
//! (`C:\keys\cosign.pub`) is a bare path and not a scheme. Rule 3 keys on the
//! one token that used to mean something else here; every other single-colon
//! value (`awskms:alias/x`) is a bare path, as it is to cosign.

use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// The largest a key PEM may be, on either side of the pair.
///
/// A cosign encrypted P-256 private key is under a kilobyte and an SPKI public
/// key is ~180 bytes, so the cap is orders of magnitude of headroom and exists
/// only to bound the read.
///
/// **One constant, not two.** The sign-side reader
/// ([`key_backend::read_key_pem`](super::key_backend)) and the verify-side one
/// (`crate::trust::read_key_file`) bound the same operator-typed file, and their
/// agreement is what makes sign and verify answer with the same exit code for
/// an over-cap key. The readers stay separate — they raise different error
/// types, and each names its own half in the message — but the bound they
/// enforce cannot be, so it lives here, in the module that owns the `--key`
/// grammar that named the file. A second definition anywhere is the drift.
pub const MAX_KEY_PEM_BYTES: u64 = 64 * 1024;

/// Why the environment variable an `env://` reference names yielded no key.
///
/// Two causes, because the two sides of the pair answer them with different
/// exit codes and must answer them with the *same* ones: nothing to read is an
/// I/O-class fault (74), the code a missing key **file** already gets, and
/// something present that no key can be is a data-class one (65). That is the
/// split [`read_bounded`](crate::utility::fs::read_bounded) already makes for a
/// file; this type is what lets an env-held key make it identically.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum KeyEnvError {
    /// The variable is unset or empty. A name an environment cannot hold (one
    /// containing `=` or NUL) also lands here, because that is what the
    /// platform reports for it.
    #[error("environment variable `{name}` is unset or empty")]
    Unset {
        /// The variable the reference named.
        name: String,
    },
    /// The value is larger than a key ever is.
    #[error("environment variable `{name}` holds more than {cap} bytes, which no key is")]
    TooLarge {
        /// The variable the reference named.
        name: String,
        /// The cap it exceeded — [`MAX_KEY_PEM_BYTES`].
        cap: u64,
    },
}

/// Read a key PEM out of the environment variable `name`, bounded by
/// [`MAX_KEY_PEM_BYTES`].
///
/// **One reader, both halves of the pair.** The sign side wants a private PEM
/// and the verify side a public one, but "unset is not a key", "empty is not a
/// key" and the cap are the same three rules for both, and a second copy of
/// them is how sign and verify come to answer one bad `env://` with two exit
/// codes — the drift [`MAX_KEY_PEM_BYTES`] is written the way it is to prevent.
/// Each caller keeps its own **wording**, mapping [`KeyEnvError`] into its own
/// error type; only the rules are shared.
///
/// Routed through [`crate::env::var`] rather than `std::env::var`, which is
/// what makes this testable without `unsafe` or a mutated process
/// environment. That wrapper reports a non-UTF-8 value as absent (logging a
/// warning that names the variable), and a PEM is ASCII, so the two collapse
/// onto one message here on purpose.
///
/// The value is not zeroized: it already lives in this process's environment
/// block, so wiping a copy of it protects nothing — the same reasoning
/// [`key_backend::key_password`](super::key_backend::key_password) records.
///
/// # Errors
///
/// [`KeyEnvError::Unset`] when the variable is absent or empty, and
/// [`KeyEnvError::TooLarge`] when its value exceeds [`MAX_KEY_PEM_BYTES`].
pub fn read_key_env(name: &str) -> Result<String, KeyEnvError> {
    let value = crate::env::var(name)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| KeyEnvError::Unset { name: name.to_owned() })?;
    if value.len() as u64 > MAX_KEY_PEM_BYTES {
        return Err(KeyEnvError::TooLarge {
            name: name.to_owned(),
            cap: MAX_KEY_PEM_BYTES,
        });
    }
    Ok(value)
}

/// A key-backend scheme in cosign's spelling.
///
/// The serde spelling is pinned to [`Scheme::as_str`] rather than left at the
/// derive default: a second spelling for one concept is the drift this file
/// exists to prevent, and `Kubernetes` renders `k8s` on both channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scheme {
    /// A key read from the filesystem.
    File,
    /// A key PEM held **in** an environment variable (`env://VAR`) — the
    /// variable carries the material, never a path to it.
    Env,
    /// AWS KMS (`awskms://`).
    AwsKms,
    /// Google Cloud KMS (`gcpkms://`).
    GcpKms,
    /// Azure Key Vault (`azurekms://`).
    AzureKms,
    /// HashiCorp Vault transit (`hashivault://`).
    HashiVault,
    /// A Kubernetes secret (`k8s://`).
    #[serde(rename = "k8s")]
    Kubernetes,
}

impl Scheme {
    /// Every recognised spelling, in table order — the vocabulary an error
    /// message names.
    pub const SPELLINGS: &'static [&'static str] =
        &["file", "env", "awskms", "gcpkms", "azurekms", "hashivault", "k8s"];

    /// The `<scheme>://` token, matching cosign exactly.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Env => "env",
            Self::AwsKms => "awskms",
            Self::GcpKms => "gcpkms",
            Self::AzureKms => "azurekms",
            Self::HashiVault => "hashivault",
            Self::Kubernetes => "k8s",
        }
    }

    /// Whether OCX implements this backend. [`Scheme::File`] and
    /// [`Scheme::Env`] — the two that read a PEM this process can already see.
    ///
    /// **Not exhaustive, and no compiler will say so.** A backend added to the
    /// enum but forgotten here is refused as
    /// [`KeyRefError::UnsupportedBackend`] (exit 85) with no build error
    /// anywhere, so `every_implemented_scheme_is_reachable_end_to_end` asserts
    /// the membership by hand rather than trusting the shape of the `match`.
    pub const fn is_implemented(self) -> bool {
        matches!(self, Self::File | Self::Env)
    }

    /// Parse a scheme token. `None` for an unrecognised one.
    ///
    /// The `_ => None` arm is the second non-exhaustive site in this file (see
    /// [`Scheme::is_implemented`]): a variant added to the enum and forgotten
    /// here falls through to [`KeyRefError::UnknownScheme`] while its own
    /// spelling sits in [`Scheme::SPELLINGS`]. `key_backend_kind_slug_matches_scheme_spelling`
    /// round-trips every spelling through this function for exactly that reason.
    pub fn parse(token: &str) -> Option<Self> {
        match token {
            "file" => Some(Self::File),
            "env" => Some(Self::Env),
            "awskms" => Some(Self::AwsKms),
            "gcpkms" => Some(Self::GcpKms),
            "azurekms" => Some(Self::AzureKms),
            "hashivault" => Some(Self::HashiVault),
            "k8s" => Some(Self::Kubernetes),
            _ => None,
        }
    }
}

impl fmt::Display for Scheme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A `--key` value: `[scheme://]<rest>`.
///
/// Constructed only by [`KeyRef::parse`], so the scheme is always one OCX
/// implements — a recognised-but-unimplemented backend is refused at the
/// parse boundary, naming itself, and never reaches a caller as a path that
/// then fails with "no such file or directory".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRef {
    scheme: Scheme,
    rest: String,
}

impl KeyRef {
    /// Parse `[scheme://]<rest>`.
    ///
    /// # Errors
    ///
    /// [`KeyRefError::UnsupportedBackend`] for a recognised scheme with no
    /// implementation (exit 85); [`KeyRefError::UnknownScheme`] for an
    /// unrecognised one; [`KeyRefError::FileColonPrefix`] for the single-colon
    /// `file:` spelling; [`KeyRefError::Empty`] when nothing follows the
    /// scheme.
    pub fn parse(value: &str) -> Result<Self, KeyRefError> {
        let (scheme, rest) = match value.split_once("://") {
            Some((token, rest)) => {
                let scheme = Scheme::parse(token).ok_or_else(|| KeyRefError::UnknownScheme {
                    scheme: token.to_owned(),
                })?;
                if !scheme.is_implemented() {
                    return Err(KeyRefError::UnsupportedBackend { scheme });
                }
                (scheme, rest)
            }
            // No `://`. The one value that is not a bare path is a `file:`
            // prefix — a near-miss for rule 1 that cosign reads as a literal
            // filename, so it names its fix rather than silently meaning a
            // different file here than it does there.
            None => match value.strip_prefix("file:") {
                Some("") => return Err(KeyRefError::Empty),
                Some(path) => {
                    return Err(KeyRefError::FileColonPrefix { path: path.to_owned() });
                }
                None => (Scheme::File, value),
            },
        };
        if rest.is_empty() {
            return Err(KeyRefError::Empty);
        }
        Ok(Self {
            scheme,
            rest: rest.to_owned(),
        })
    }

    /// The backend this reference names.
    pub fn scheme(&self) -> Scheme {
        self.scheme
    }

    /// Everything after the scheme, verbatim.
    ///
    /// **Production-dead on purpose — do not delete.** Every caller is a test,
    /// and the one in `trust.rs`
    /// (`a_key_signers_reference_is_the_same_grammar_the_key_flag_parses`) is
    /// what pins a `key` in a trust policy and a `--key` on the command line to
    /// **one grammar, one parser**. A dead-code sweep that removes this
    /// accessor takes that assertion with it, and two spellings of one grammar
    /// drifting apart is the defect this subsystem has already shipped three
    /// times: `keyid` with two producers, `SignErrorKind` classified at one
    /// call site and unwrapped at six, and the cosign sidecar tag formatted in
    /// two places. The metric is not worth the guard.
    pub fn rest(&self) -> &str {
        &self.rest
    }

    /// The filesystem path, for [`Scheme::File`] only.
    pub fn as_path(&self) -> Option<&Path> {
        (self.scheme == Scheme::File).then(|| Path::new(self.rest.as_str()))
    }

    /// The environment variable **name**, for [`Scheme::Env`] only.
    ///
    /// The name, never the value: reading it is the caller's job, through
    /// [`read_key_env`], so the bound and the unset/empty rule have one
    /// implementation on both sides of the sign/verify pair.
    pub fn as_env_var(&self) -> Option<&str> {
        (self.scheme == Scheme::Env).then_some(self.rest.as_str())
    }
}

impl fmt::Display for KeyRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.scheme == Scheme::File {
            f.write_str(&self.rest)
        } else {
            write!(f, "{}://{}", self.scheme.as_str(), self.rest)
        }
    }
}

/// Why a `--key` value could not be turned into a [`KeyRef`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum KeyRefError {
    /// A real backend OCX recognises but has not implemented. Exit 85.
    #[error("unsupported key backend `{scheme}`; only file-based keys are implemented")]
    UnsupportedBackend {
        /// The backend named by the reference.
        scheme: Scheme,
    },
    /// A scheme token outside [`Scheme::SPELLINGS`]. Exit 64.
    #[error("unknown key reference scheme `{scheme}`: expected a path or one of {known}",
            known = Scheme::SPELLINGS.join(", "))]
    UnknownScheme {
        /// The token found before the first `://`.
        scheme: String,
    },
    /// Nothing followed the scheme. Exit 64 — not "file not found".
    #[error("key reference is empty")]
    Empty,
    /// `file:<path>` — the removed single-colon prefix form. Exit 64.
    ///
    /// There is no deprecation window for it, so the message **is** the
    /// migration: it names the bare path the author meant.
    #[error("key reference `file:{path}` is not a supported spelling; write the path on its own as `{path}`")]
    FileColonPrefix {
        /// Everything after `file:` — the path the author meant.
        path: String,
    },
}

/// What produced or verified a signature — the frozen `signatures[].key_backend`
/// vocabulary (`design_spec_cosign_parity.md` §"--format json").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyBackendKind {
    /// A Fulcio-issued ephemeral certificate; no long-lived key.
    Keyless,
    /// A key read from the filesystem.
    File,
    /// A key PEM held in an environment variable.
    Env,
    /// AWS KMS.
    #[serde(rename = "awskms")]
    AwsKms,
    /// Google Cloud KMS.
    #[serde(rename = "gcpkms")]
    GcpKms,
    /// Azure Key Vault.
    #[serde(rename = "azurekms")]
    AzureKms,
    /// HashiCorp Vault transit.
    #[serde(rename = "hashivault")]
    HashiVault,
    /// A Kubernetes secret.
    #[serde(rename = "k8s")]
    Kubernetes,
}

impl KeyBackendKind {
    /// The frozen wire slug — the same word serde emits.
    ///
    /// Added by loop C, which reports a key-mode signature's backend in a
    /// plain-text table as well as in JSON. Derived from [`Scheme::as_str`]
    /// wherever a scheme exists, so the flag grammar, the config spelling and
    /// the reported word cannot drift into three answers;
    /// `the_display_slug_is_the_serde_slug` pins it against serde.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            // The one kind with no `Scheme`: keyless is the absence of a key
            // reference, not a backend a `--key` value can name.
            Self::Keyless => "keyless",
            Self::File => Scheme::File.as_str(),
            Self::Env => Scheme::Env.as_str(),
            Self::AwsKms => Scheme::AwsKms.as_str(),
            Self::GcpKms => Scheme::GcpKms.as_str(),
            Self::AzureKms => Scheme::AzureKms.as_str(),
            Self::HashiVault => Scheme::HashiVault.as_str(),
            Self::Kubernetes => Scheme::Kubernetes.as_str(),
        }
    }
}

impl std::fmt::Display for KeyBackendKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl From<Scheme> for KeyBackendKind {
    fn from(scheme: Scheme) -> Self {
        match scheme {
            Scheme::File => Self::File,
            Scheme::Env => Self::Env,
            Scheme::AwsKms => Self::AwsKms,
            Scheme::GcpKms => Self::GcpKms,
            Scheme::AzureKms => Self::AzureKms,
            Scheme::HashiVault => Self::HashiVault,
            Scheme::Kubernetes => Self::Kubernetes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `Scheme` variant. A new variant makes this array's length wrong
    /// and reds the parity test below, which is the point.
    const ALL_SCHEMES: [Scheme; 7] = [
        Scheme::File,
        Scheme::Env,
        Scheme::AwsKms,
        Scheme::GcpKms,
        Scheme::AzureKms,
        Scheme::HashiVault,
        Scheme::Kubernetes,
    ];

    /// The C-003 edge-case table (E-01…E-04), one row per documented outcome.
    ///
    /// The four outcomes are distinct on purpose: a bare path and `file://`
    /// both resolve to a file, a recognised-but-unimplemented scheme names
    /// itself, and a bogus scheme is a different error entirely.
    #[test]
    fn key_ref_parse_table() {
        // Ok rows: (input, expected rest).
        let ok: &[(&str, &str)] = &[
            ("cosign.pub", "cosign.pub"),
            // A single colon on any *unrecognised* token is a filename, here as
            // in cosign: only `file:` is claimed by the rejection below.
            ("awskms:us-east-1/abc", "awskms:us-east-1/abc"),
            ("file://./cosign.pub", "./cosign.pub"),
            // A file genuinely named `file:…` keeps one spelling that reaches
            // it, which is what makes the rejection below a grammar rule and
            // not a hole in the addressable filesystem.
            ("file://file:etc/acme-release.pub", "file:etc/acme-release.pub"),
            // E-01: the drive colon is not `://`, so rule 1 never fires.
            (r"C:\keys\cosign.pub", r"C:\keys\cosign.pub"),
        ];
        for (input, rest) in ok {
            let parsed = KeyRef::parse(input).unwrap_or_else(|e| panic!("{input}: {e}"));
            assert_eq!(parsed.scheme(), Scheme::File, "{input}");
            assert_eq!(parsed.rest(), *rest, "{input}");
            assert_eq!(parsed.as_path(), Some(Path::new(rest)), "{input}");
            assert_eq!(parsed.to_string(), *rest, "{input} renders bare");
        }

        // E-04: a recognised backend names itself and never becomes a path.
        let unsupported: &[(&str, Scheme)] = &[
            ("awskms://alias/release", Scheme::AwsKms),
            (
                "gcpkms://projects/p/locations/l/keyRings/r/cryptoKeys/k",
                Scheme::GcpKms,
            ),
            ("azurekms://vault.vault.azure.net/keys/k", Scheme::AzureKms),
            ("hashivault://transit/keys/k", Scheme::HashiVault),
            ("k8s://namespace/secret", Scheme::Kubernetes),
        ];
        for (input, scheme) in unsupported {
            assert_eq!(
                KeyRef::parse(input),
                Err(KeyRefError::UnsupportedBackend { scheme: *scheme }),
                "{input}"
            );
            let message = KeyRefError::UnsupportedBackend { scheme: *scheme }.to_string();
            assert!(
                message.contains(scheme.as_str()),
                "message must name the backend: {message}"
            );
        }

        // A bogus scheme is a *different* outcome from an unimplemented one.
        for (input, token) in [("vault://secret/key", "vault"), ("./weird://name", "./weird")] {
            assert_eq!(
                KeyRef::parse(input),
                Err(KeyRefError::UnknownScheme {
                    scheme: token.to_owned()
                }),
                "{input}"
            );
        }

        // E-02: nothing after the scheme is `Empty`, never "file not found".
        for input in ["", "file:", "file://"] {
            assert_eq!(KeyRef::parse(input), Err(KeyRefError::Empty), "{input}");
        }

        // The removed spelling. Not an `Ok` row and not a bare path either: it
        // is refused by name, because cosign resolves it to a file *literally*
        // named `file:etc/acme-release.pub` and OCX used to resolve it to
        // `etc/acme-release.pub` — one value, two files.
        let removed = KeyRef::parse("file:etc/acme-release.pub");
        assert_eq!(
            removed,
            Err(KeyRefError::FileColonPrefix {
                path: "etc/acme-release.pub".to_owned()
            })
        );
        // The message *is* the migration — there is no deprecation window, so
        // it has to carry the replacement text, not just report the fault.
        let message = removed.expect_err("refused").to_string();
        assert!(
            message.contains("etc/acme-release.pub"),
            "the refusal must name the bare path to write instead; got: {message}"
        );
    }

    /// The unknown-scheme message names the whole recognised vocabulary, so a
    /// user who typed `vault` learns `hashivault` exists.
    #[test]
    fn unknown_scheme_message_lists_every_spelling() {
        let message = KeyRefError::UnknownScheme {
            scheme: "vault".to_owned(),
        }
        .to_string();
        for spelling in Scheme::SPELLINGS {
            assert!(message.contains(spelling), "{message} omits {spelling}");
        }
    }

    /// T-05: the reported `key_backend` slug, the `<scheme>://` token and the
    /// `Scheme` serde spelling are one string per backend.
    #[test]
    fn key_backend_kind_slug_matches_scheme_spelling() {
        assert_eq!(Scheme::SPELLINGS.len(), ALL_SCHEMES.len());
        for (scheme, spelling) in ALL_SCHEMES.iter().zip(Scheme::SPELLINGS) {
            assert_eq!(scheme.as_str(), *spelling, "SPELLINGS is out of table order");
            assert_eq!(Scheme::parse(spelling), Some(*scheme), "{spelling} does not round-trip");
            let kind = KeyBackendKind::from(*scheme);
            assert_eq!(
                serde_json::to_value(kind).expect("serialize kind"),
                serde_json::Value::String((*spelling).to_owned()),
                "key_backend slug drifted from {spelling}"
            );
            assert_eq!(
                serde_json::to_value(scheme).expect("serialize scheme"),
                serde_json::Value::String((*spelling).to_owned()),
                "Scheme serde spelling drifted from {spelling}"
            );
        }
        // `Keyless` has no `Scheme` twin by construction (D-13): a key
        // reference can never name it.
        assert_eq!(
            serde_json::to_value(KeyBackendKind::Keyless).expect("serialize keyless"),
            serde_json::Value::String("keyless".to_owned())
        );
    }

    /// C-030: `file` and `env` are implemented; the rest exist so their
    /// refusal can name them.
    ///
    /// The membership is spelled out here rather than derived from
    /// [`Scheme::is_implemented`], because that `matches!` is the thing under
    /// test: asking it what it admits and then checking it admits that is the
    /// green that cannot go red.
    #[test]
    fn only_the_file_and_env_backends_are_implemented() {
        for scheme in ALL_SCHEMES {
            let expected = matches!(scheme, Scheme::File | Scheme::Env);
            assert_eq!(scheme.is_implemented(), expected, "{scheme}");
        }
    }

    /// C-030, site 1 — `Scheme::parse`'s `_ => None` wildcard.
    ///
    /// A variant whose spelling is in [`Scheme::SPELLINGS`] but missing from
    /// `parse` falls through to [`KeyRefError::UnknownScheme`] with no build
    /// error, so the token is asserted through the *public* grammar rather
    /// than through `Scheme::parse` alone: `env://` must not be an unknown
    /// scheme to a user typing it at `--key`.
    #[test]
    fn the_env_scheme_token_is_recognised_by_the_grammar() {
        assert_eq!(Scheme::parse("env"), Some(Scheme::Env));
        assert!(
            Scheme::SPELLINGS.contains(&"env"),
            "the vocabulary an error message names must include env"
        );
        let parsed = KeyRef::parse("env://OCX_SIGNING_KEY").expect("env:// is a recognised scheme");
        assert_eq!(parsed.scheme(), Scheme::Env);
    }

    /// C-030, site 2 — `Scheme::is_implemented`'s `matches!`.
    ///
    /// Recognised is not the same as implemented: forget the `Env` arm and
    /// `--key env://VAR` is refused as [`KeyRefError::UnsupportedBackend`]
    /// (exit 85) while every spelling test above still passes.
    #[test]
    fn an_env_reference_is_implemented_not_merely_recognised() {
        assert!(Scheme::Env.is_implemented(), "env:// must reach a backend");
        let parsed = KeyRef::parse("env://OCX_SIGNING_KEY").expect("env:// must not be refused as unsupported");
        assert_eq!(parsed.as_env_var(), Some("OCX_SIGNING_KEY"));
        // The name, never a path: `as_path` is the file accessor and must stay
        // silent here, or `build_signer` would open a file called
        // `OCX_SIGNING_KEY`.
        assert_eq!(parsed.as_path(), None);
        // Round-trips through Display as written, so a report or a log line
        // names the variable rather than inventing a second spelling.
        assert_eq!(parsed.to_string(), "env://OCX_SIGNING_KEY");
        // The reverse pairing, so neither accessor answers for both schemes.
        let file = KeyRef::parse("cosign.key").expect("a bare path parses");
        assert_eq!(file.as_env_var(), None);
    }

    /// C-032: the wire value an `env://` signature reports is `env`.
    #[test]
    fn an_env_scheme_reports_the_env_key_backend() {
        assert_eq!(KeyBackendKind::from(Scheme::Env), KeyBackendKind::Env);
        assert_eq!(
            serde_json::to_value(KeyBackendKind::Env).expect("serialize"),
            serde_json::Value::String("env".to_owned())
        );
    }

    /// C-033: an empty `env://` reference is a grammar error, not a lookup of
    /// the empty variable name.
    #[test]
    fn an_env_reference_with_no_variable_is_empty() {
        assert_eq!(KeyRef::parse("env://"), Err(KeyRefError::Empty));
    }

    /// C-033: unset and empty both refuse, and the message names the variable.
    #[test]
    fn read_key_env_refuses_an_unset_or_empty_variable() {
        let env = crate::test::env::lock();
        env.remove("OCX_TEST_MISSING_KEY");
        let unset = read_key_env("OCX_TEST_MISSING_KEY").expect_err("unset must refuse");
        assert_eq!(
            unset,
            KeyEnvError::Unset {
                name: "OCX_TEST_MISSING_KEY".to_owned()
            }
        );
        assert!(
            unset.to_string().contains("OCX_TEST_MISSING_KEY"),
            "the refusal must name the variable: {unset}"
        );

        env.set("OCX_TEST_EMPTY_KEY", "");
        assert_eq!(
            read_key_env("OCX_TEST_EMPTY_KEY"),
            Err(KeyEnvError::Unset {
                name: "OCX_TEST_EMPTY_KEY".to_owned()
            }),
            "an empty variable holds no key either"
        );
    }

    /// C-033: `MAX_KEY_PEM_BYTES` bounds the variable exactly as it bounds a
    /// file, so an oversized value is a data fault and not a key.
    #[test]
    fn read_key_env_bounds_the_value_at_the_shared_cap() {
        let env = crate::test::env::lock();
        let cap = usize::try_from(MAX_KEY_PEM_BYTES).expect("cap fits a usize");

        env.set("OCX_TEST_BIG_KEY", "k".repeat(cap));
        assert!(
            read_key_env("OCX_TEST_BIG_KEY").is_ok(),
            "a value exactly at the cap is still readable"
        );

        env.set("OCX_TEST_BIG_KEY", "k".repeat(cap + 1));
        let error = read_key_env("OCX_TEST_BIG_KEY").expect_err("one byte over the cap must refuse");
        assert_eq!(
            error,
            KeyEnvError::TooLarge {
                name: "OCX_TEST_BIG_KEY".to_owned(),
                cap: MAX_KEY_PEM_BYTES,
            }
        );
        assert!(
            error.to_string().contains("OCX_TEST_BIG_KEY"),
            "the refusal must name the variable: {error}"
        );
    }

    /// The plain-text slug and the JSON slug are the same word.
    ///
    /// Two channels for one vocabulary is how a reported `file` and a
    /// serialized `file` drift into `File` and `file`. serde is the wire
    /// authority here, so it is the oracle.
    #[test]
    fn the_display_slug_is_the_serde_slug() {
        let kinds = [
            KeyBackendKind::Keyless,
            KeyBackendKind::File,
            KeyBackendKind::Env,
            KeyBackendKind::AwsKms,
            KeyBackendKind::GcpKms,
            KeyBackendKind::AzureKms,
            KeyBackendKind::HashiVault,
            KeyBackendKind::Kubernetes,
        ];
        for kind in kinds {
            let json = serde_json::to_string(&kind).expect("a unit variant serializes");
            assert_eq!(format!("\"{kind}\""), json, "Display and serde must agree for {kind:?}",);
        }
    }
}
