// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Exit-code classification for the config, env, TLS-trust-material and managed-config error family — the `ocx_config` rung of the
//! ladder, here rather than in that crate because classification is `ocx_cli`'s alone.

use ocx_exit::ExitCode;

use ocx_config::ToolchainRootError;
use ocx_config::edit::EditError;
use ocx_config::env::CommandResolutionError;
use ocx_config::error::Error as ConfigError;
use ocx_config::managed::ManagedConfigError;
use ocx_config::managed_config::ManagedConfigFetchError;
use ocx_config::managed_config::ManagedConfigPersistError;
use ocx_config::managed_config::ManagedConfigUpdateError;
use ocx_config::mirror::MirrorConfigError;
use ocx_config::patch::PatchConfigError;
use ocx_config::tls::TlsError;
use ocx_package::metadata::env::apply::ForwardedEnvError;
use ocx_package::metadata::env::apply::ListSeparatorError;
use ocx_package_manager::managed_config::ManagedConfigPublishError;

use super::{ClassifyExitCode, downcast_arm};

impl ClassifyExitCode for ToolchainRootError {
    /// Exhaustive on purpose, with no wildcard: a refusal added later cannot
    /// reach a user as an unclassified exit code without a decision here. Every
    /// variant is 78 today — the operator's remedy is always to edit a
    /// `toolchain_dir` value — but the match, not a blanket `Some`, is what
    /// makes that a statement rather than an accident (D-V15(e)).
    ///
    /// Reachable from the CLI only through
    /// [`config::error::Error::Toolchain`](ocx_config::error::Error), which
    /// `crate::exit::classify` already downcasts. Deleting that variant leaves this impl
    /// intact and flips the process exit from 78 to 1 — the mutation that
    /// separates "classified" from "reachable".
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::Unexpandable { .. }
            | Self::Relative { .. }
            | Self::ParentDirComponent { .. }
            | Self::NoContainmentAnchor { .. }
            | Self::IsContainmentAnchor { .. }
            | Self::OutsideHome { .. }
            | Self::SystemPrefix { .. }
            | Self::InsideGlobalToolchainHome { .. }
            | Self::Inaccessible { .. }
            | Self::NotADirectory { .. }
            | Self::NotOwnerOwned { .. }
            | Self::GroupOrWorldWritable { .. } => ExitCode::ConfigError,
        })
    }
}

impl ClassifyExitCode for ListSeparatorError {
    fn classify(&self) -> Option<ExitCode> {
        // Declarations that cannot be honoured as written — malformed input,
        // the same class as every other env-declaration refusal.
        Some(ExitCode::DataError)
    }
}

impl ClassifyExitCode for CommandResolutionError {
    /// An **exhaustive match with no wildcard arm** (D-V15), copying the shape
    /// of `ocx_lib::Error::classify`.
    ///
    /// It was a blanket `Some(DataError)`. Under a blanket, a variant added
    /// later is silently 65 and **no test can catch it** — the wrong answer and
    /// the right one are the same bytes. Under an exhaustive match, adding a
    /// variant is a compile error until somebody classifies it. Every arm
    /// happening to yield the same code today is not a reason to collapse them:
    /// the match is a gate on the *decision*, not a dispatch table.
    ///
    /// This type is already registered in `crate::exit::classify`; the arms
    /// below are the whole classification and nothing needs re-registering.
    fn classify(&self) -> Option<ExitCode> {
        match self {
            // Parity with `BinScanError::DeclaredNotExecutable`: the package's
            // own content contradicts what it claims to ship — malformed input
            // data, not a usage error and not a config fault.
            Self::NotExecutable { .. } => Some(ExitCode::DataError),
            // C-057/S-010: a name the composition does not provide is bad input
            // data, the same class as a package claiming a binary it omits.
            // Not `Failure` (1) — the caller can tell a missing tool from a
            // crashed one — and not `Usage` (64), because the argv was
            // well-formed; it is the *environment* that lacks the name.
            Self::NotFound { .. } => Some(ExitCode::DataError),
            // C-069. Same code as its siblings on purpose: the exit code
            // classifies the *class* of failure, and the guard identity lives
            // in the message and the variant, which is where the item-22 test
            // asserts which guard fired.
            Self::TrampolineRefused { .. } => Some(ExitCode::DataError),
        }
    }
}

impl ClassifyExitCode for ForwardedEnvError {
    fn classify(&self) -> Option<ExitCode> {
        // Every variant means the forwarded payload was malformed, truncated or
        // forged — input data that failed validation, not a config-file fault
        // and not a usage error (no user typed it).
        Some(ExitCode::DataError)
    }
}

impl ClassifyExitCode for ManagedConfigFetchError {
    fn classify(&self) -> Option<ExitCode> {
        match self {
            // Network/auth failures delegate to the inner OCI client error's
            // classification (Unavailable 69 / AuthError 80 / etc.).
            Self::FetchFailed { source } => source.classify(),
            // Shape mismatches are malformed registry data.
            Self::UnexpectedManifest { .. }
            | Self::NoAnyPlatformEntry
            | Self::NoGzipLayer
            | Self::MissingConfigToml
            | Self::LayerSizeExceeded { .. }
            | Self::LayerDigestMismatch { .. }
            | Self::ConfigEntryTooLarge { .. }
            | Self::InvalidArchive { .. } => Some(ExitCode::DataError),
        }
    }
}

impl ClassifyExitCode for ManagedConfigPersistError {
    fn classify(&self) -> Option<ExitCode> {
        match self {
            Self::InvalidToml { .. } => Some(ExitCode::DataError),
            Self::SnapshotWriteFailed { .. } => Some(ExitCode::IoError),
            Self::ExtraCaCertsInvalid { source } => source.classify(),
        }
    }
}

impl ClassifyExitCode for ManagedConfigUpdateError {
    fn classify(&self) -> Option<ExitCode> {
        match self {
            Self::Fetch(source) => source.classify(),
            Self::Persist(source) => source.classify(),
            Self::SourceNotFound { .. } => Some(ExitCode::NotFound),
            Self::PinDigestMismatch { .. } => Some(ExitCode::DataError),
        }
    }
}

impl ClassifyExitCode for ManagedConfigPublishError {
    fn classify(&self) -> Option<ExitCode> {
        match self {
            // Payload rejections are operator config mistakes.
            Self::PayloadTooLarge { .. }
            | Self::InvalidToml { .. }
            | Self::ContainsManagedSection
            | Self::AmbiguousTrustRoot
            | Self::AmbiguousExtraCaCerts
            | Self::ManagedConfigKeyByPath
            | Self::TrustedRootInvalid { .. }
            | Self::ExtraCaCertsPemInvalid { .. } => Some(ExitCode::ConfigError),
            // The third door onto one refusal: a payload naming a recognised
            // but unimplemented backend is "upgrade ocx", not "your config is
            // malformed", and `--key` plus the local config tiers both already
            // answer 85 for the identical value.
            Self::InvalidTrustPolicy { source } if source.names_unsupported_backend() => {
                Some(ExitCode::UnsupportedKeyBackend)
            }
            Self::InvalidTrustPolicy { .. } => Some(ExitCode::ConfigError),
            // Content that fails C-004 (wrong PEM tag, empty, malformed) is a
            // data error, not a config error — see the variant doc comment
            // for why this deliberately diverges from `TrustedRootInvalid`.
            Self::ExtraCaCertsInvalid { .. } | Self::ExtraCaCertsNotUtf8 { .. } => Some(ExitCode::DataError),
            Self::ReadFailed { source, .. }
            | Self::TrustedRootReadFailed { source, .. }
            | Self::ExtraCaCertsReadFailed { source, .. } => Some(match source.kind() {
                std::io::ErrorKind::NotFound => ExitCode::NotFound,
                std::io::ErrorKind::PermissionDenied => ExitCode::PermissionDenied,
                _ => ExitCode::IoError,
            }),
            Self::StageFailed { .. } => Some(ExitCode::IoError),
            // Registry/bundling failures delegate to the inner cause's own
            // classification (Unavailable 69 / AuthError 80 / …). Explicit
            // delegation, not `None`: the boxed source's `TypeId` is
            // `Box<ocx_lib::Error>`, which the chain walker's downcast ladder
            // would never match.
            Self::BundleFailed { source } | Self::ListTagsFailed { source, .. } | Self::PushFailed { source } => {
                source.classify()
            }
        }
    }
}

impl ClassifyExitCode for EditError {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            // The loader refuses the same over-size file as a config error,
            // and an edit that would produce one is the same refusal earlier.
            Self::TooLarge { .. } => ExitCode::ConfigError,
            // The ADR's lock-timeout row: the holder is another ocx, and it
            // will be gone on the retry (`adr_file_lock_unification.md`).
            Self::Locked { .. } => ExitCode::TempFail,
            // One code for "the edit did not happen" (C-051): the read, the
            // parse and the write it sits between are all the file's fault.
            Self::Io { .. } | Self::Parse { .. } | Self::Malformed { .. } => ExitCode::IoError,
        })
    }
}

impl ClassifyExitCode for ConfigError {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::FileNotFound { .. } => ExitCode::NotFound,
            // `SystemConfig` is 78, not the 74 its sibling `Io` takes: the
            // operator fixes it by editing (or un-symlinking) a policy file,
            // the same remediation a malformed one needs.
            Self::FileTooLarge { .. }
            | Self::Parse { .. }
            | Self::SystemConfig { .. }
            | Self::AmbiguousExtraCaCerts { .. } => ExitCode::ConfigError,
            Self::Io { .. } => ExitCode::IoError,
            // Delegated, not restated: every `toolchain_dir` refusal is 78
            // today, but duplicating that mapping here is how the two would
            // silently disagree once one of them changes.
            Self::Toolchain(refusal) => return refusal.classify(),
        })
    }
}

impl ClassifyExitCode for ManagedConfigError {
    fn classify(&self) -> Option<ExitCode> {
        Some(ExitCode::ConfigError)
    }
}

/// Every variant is a malformed `[mirrors]` tier — a config fault whether it
/// arrived from a config file or the forwarded `OCX_MIRRORS` env value. The
/// plain-HTTP refusal included: the entry is well-formed but the operator has
/// not allowed the transport it asks for, which is theirs to fix in config.
impl ClassifyExitCode for MirrorConfigError {
    fn classify(&self) -> Option<ExitCode> {
        Some(ExitCode::ConfigError)
    }
}

/// Every variant is a malformed `[patches]` tier — a config fault whether it
/// arrived from a config file or the forwarded `OCX_PATCHES` env value.
impl ClassifyExitCode for PatchConfigError {
    fn classify(&self) -> Option<ExitCode> {
        Some(ExitCode::ConfigError)
    }
}

impl ClassifyExitCode for TlsError {
    /// C-010: a file the operator named that this process could not use as
    /// given is 74 (`Unreadable`, and `TooLarge` from a file — parity with
    /// `[trust.sigstore]`'s `trust_resolve.rs`); content refused from a file
    /// is the file's data being wrong, 65; the same refusal from inline text
    /// (the env value, `extra_ca_certs_pem`) is the configuration itself
    /// being wrong, 78.
    fn classify(&self) -> Option<ExitCode> {
        let code = match self {
            Self::Unreadable { .. } => ExitCode::IoError,
            Self::TooLarge { origin, .. } if origin.is_file() => ExitCode::IoError,
            Self::TooLarge { .. } => ExitCode::ConfigError,
            Self::NotACertificate { origin, .. }
            | Self::Empty { origin }
            | Self::Malformed { origin, .. }
            | Self::Truncated { origin, .. } => {
                if origin.is_file() {
                    ExitCode::DataError
                } else {
                    ExitCode::ConfigError
                }
            }
        };
        Some(code)
    }
}

pub(super) fn try_downcast(cause: &(dyn std::error::Error + 'static)) -> Option<ExitCode> {
    downcast_arm!(cause, ConfigError);
    downcast_arm!(cause, EditError);
    downcast_arm!(cause, ForwardedEnvError);
    downcast_arm!(cause, ListSeparatorError);
    downcast_arm!(cause, CommandResolutionError);
    downcast_arm!(cause, ManagedConfigError);
    downcast_arm!(cause, MirrorConfigError);
    downcast_arm!(cause, PatchConfigError);
    downcast_arm!(cause, ManagedConfigFetchError);
    downcast_arm!(cause, ManagedConfigPersistError);
    downcast_arm!(cause, ManagedConfigUpdateError);
    downcast_arm!(cause, ManagedConfigPublishError);
    downcast_arm!(cause, TlsError);
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocx_config::ConfigTier;
    use ocx_config::ToolchainRootTier;
    use ocx_config::tls::ExtraRootsSource;
    use ocx_util::tls::MAX_EXTRA_CA_CERTS_BYTES;

    use std::path::PathBuf;

    // ── moved from ocx_cli::exit::ocx_util with the impl (WP-19) ──

    /// C-010: content refused from a **file** the operator named is data the
    /// file holds — 65.
    #[test]
    fn extra_ca_content_errors_from_a_file_classify_as_data_error() {
        for origin in file_origins() {
            let errors = [
                TlsError::Malformed {
                    origin: origin.clone(),
                    index: Some(0),
                },
                TlsError::Malformed {
                    origin: origin.clone(),
                    index: None,
                },
                TlsError::Truncated {
                    origin: origin.clone(),
                    index: 1,
                },
                TlsError::NotACertificate {
                    origin: origin.clone(),
                    tag: "PRIVATE KEY".to_owned(),
                },
                TlsError::Empty { origin: origin.clone() },
            ];
            for error in errors {
                assert_eq!(error.classify(), Some(ExitCode::DataError), "{error:?}");
            }
        }
    }

    /// C-010: the same content refused from **inline** text — the env value
    /// or `extra_ca_certs_pem` — is the configuration itself being wrong — 78.
    #[test]
    fn extra_ca_content_errors_from_inline_text_classify_as_config_error() {
        for origin in inline_origins() {
            let errors = [
                TlsError::Malformed {
                    origin: origin.clone(),
                    index: Some(0),
                },
                TlsError::Malformed {
                    origin: origin.clone(),
                    index: None,
                },
                TlsError::Truncated {
                    origin: origin.clone(),
                    index: 1,
                },
                TlsError::NotACertificate {
                    origin: origin.clone(),
                    tag: "PRIVATE KEY".to_owned(),
                },
                TlsError::Empty { origin: origin.clone() },
            ];
            for error in errors {
                assert_eq!(error.classify(), Some(ExitCode::ConfigError), "{error:?}");
            }
        }
    }

    /// C-004 / C-010: the chain walker reaches a `TlsError` behind a wrapper —
    /// the `try_downcast!` registration in `cli/classify.rs`, without which 78
    /// is unreachable from `Context::try_init`.
    #[test]
    fn extra_ca_tls_error_is_classified_through_the_error_chain_walker() {
        #[derive(Debug, thiserror::Error)]
        #[error("initialising the context")]
        struct Wrapped(#[source] TlsError);

        let wrapped = Wrapped(TlsError::Empty {
            origin: ExtraRootsSource::ConfigInline(ConfigTier::System),
        });
        assert_eq!(crate::exit::classify_library_error(&wrapped), ExitCode::ConfigError);

        let wrapped = Wrapped(TlsError::Malformed {
            origin: ExtraRootsSource::ConfigPath {
                path: PathBuf::from("/etc/ssl/corp.pem"),
                tier: Some(ConfigTier::Home),
            },
            index: Some(2),
        });
        assert_eq!(crate::exit::classify_library_error(&wrapped), ExitCode::DataError);
    }

    /// C-010: `Unreadable` is 74 whatever the read raised — a path the
    /// operator pointed at that this process could not use as given —
    /// including the reader's own `TooLarge` and `NotRegularFile`, which
    /// `read_path` maps to `InvalidInput`, and the OS's own `NotFound` /
    /// `PermissionDenied`, which `config push` splits but the runtime does not.
    #[test]
    fn extra_ca_unreadable_classifies_as_io_error_for_every_bounded_read_error() {
        for origin in file_origins() {
            let ios = [
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a regular file"),
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("over the {MAX_EXTRA_CA_CERTS_BYTES}-byte cap"),
                ),
                std::io::Error::from(std::io::ErrorKind::NotFound),
                std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            ];
            for io in ios {
                let error = TlsError::Unreadable {
                    origin: origin.clone(),
                    io,
                };
                assert_eq!(error.classify(), Some(ExitCode::IoError), "{error:?}");
            }
        }
    }

    /// C-010 / DX-9: `TooLarge` splits on the source too — inline text is the
    /// configuration itself being wrong (78); a file origin is the file being
    /// unusable (74), for parity with `Unreadable`'s file row.
    ///
    /// WP-10 moved this impl out of `ocx_util::tls` and left both `classify()`
    /// assertions behind in the library test, which kept only a message check;
    /// no test asserted either value between that merge and this one. The two
    /// expected codes are read from the pre-split baseline
    /// (`classify_baseline_7adaea62.json`, `TlsError` / `ClassifyExitCode`),
    /// never off the arms below — a test written by looking at the code it
    /// guards asserts only that the code equals itself, and would have been
    /// just as green had the split been inverted.
    #[test]
    fn extra_ca_too_large_classifies_by_inline_versus_file_origin() {
        for origin in inline_origins() {
            let error = TlsError::TooLarge {
                origin,
                bytes: MAX_EXTRA_CA_CERTS_BYTES + 1,
            };
            assert_eq!(error.classify(), Some(ExitCode::ConfigError), "{error:?}");
        }
        for origin in file_origins() {
            let error = TlsError::TooLarge {
                origin,
                bytes: MAX_EXTRA_CA_CERTS_BYTES + 1,
            };
            assert_eq!(error.classify(), Some(ExitCode::IoError), "{error:?}");
        }
    }

    // fixture from ocx_lib (config::tls)
    fn file_origins() -> [ExtraRootsSource; 2] {
        [
            ExtraRootsSource::EnvPath(PathBuf::from("/etc/ssl/corp.pem")),
            ExtraRootsSource::ConfigPath {
                path: PathBuf::from("/etc/ssl/corp.pem"),
                tier: Some(ConfigTier::User),
            },
        ]
    }

    // fixture from ocx_lib (config::tls)
    fn inline_origins() -> [ExtraRootsSource; 2] {
        [ExtraRootsSource::Env, ExtraRootsSource::ConfigInline(ConfigTier::User)]
    }

    // ── moved from ocx_config with the impl ──

    // ── moved from ocx_config::edit with the impl ──

    /// A `config.toml` held past the edit lock's timeout is **75**, not 74.
    ///
    /// Recovered from the pin row `crates/ocx_lib/src/config/edit.rs:120`
    /// (`Self::Locked { .. } => ExitCode::TempFail`). WP-10 took the assertion
    /// out of `edit.rs::an_editor_held_past_the_timeout_is_locked_and_exit_75`,
    /// whose name still promises the number.
    ///
    /// 75 is the whole point of the variant: the holder is another `ocx` and it
    /// will be gone on the retry, so a wrapper that retries on 75 must not see
    /// the 74 its `Io` sibling takes.
    #[test]
    fn an_editor_held_past_the_timeout_is_locked_and_exit_75() {
        let locked = EditError::Locked {
            path: std::path::PathBuf::from("/home/o/.ocx/config.toml"),
        };
        assert_eq!(locked.classify(), Some(ExitCode::TempFail));
        assert_eq!(
            crate::exit::classify_library_error(&locked),
            ExitCode::TempFail,
            "a retryable lock timeout must not arrive as the 74 its I/O sibling takes"
        );

        let unwritable = EditError::Io {
            path: std::path::PathBuf::from("/home/o/.ocx/config.toml"),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        };
        assert_eq!(
            unwritable.classify(),
            Some(ExitCode::IoError),
            "the contrast is what makes the 75 above a statement rather than a constant"
        );
    }

    // ── moved from ocx_config::loader with the impl ──

    // ── moved from ocx_config::env with the impl ──

    /// Every decode failure exits 65 (`DataError`) — the payload is input data
    /// that failed validation, not a config-file fault and not a usage error.
    ///
    /// Per variant rather than on one sample. The impl is a blanket answer with
    /// no match, so a later variant routed differently must red here; asserting
    /// one variant would pass whatever the rest did. Restores the coverage the
    /// classification relocation dropped — the body this test carried in
    /// `ocx_config::env` was emptied to `{}` when the impl left that crate.
    #[test]
    fn forwarded_env_error_classifies_as_data_error() {
        let malformed = serde_json::from_str::<serde_json::Value>("not json {{{").expect_err("invalid JSON");
        let variants = [
            ForwardedEnvError::MalformedJson { source: malformed },
            ForwardedEnvError::MissingEntries,
            ForwardedEnvError::InvalidEntry { index: 0 },
            ForwardedEnvError::UnknownKind {
                key: "CI".to_owned(),
                found: "append".to_owned(),
            },
            ForwardedEnvError::InvalidKey { key: "A\nB".to_owned() },
            ForwardedEnvError::ReservedKey {
                key: "OCX_OFFLINE".to_owned(),
            },
            ForwardedEnvError::MissingSeparator {
                key: "GODEBUG".to_owned(),
            },
            ForwardedEnvError::InvalidSeparator {
                key: "GODEBUG".to_owned(),
                separator: "=".to_owned(),
            },
            ForwardedEnvError::SeparatorEdgedValue {
                key: "GODEBUG".to_owned(),
                separator: ",".to_owned(),
                value: ",gctrace=1".to_owned(),
            },
        ];
        for error in &variants {
            assert_eq!(
                error.classify(),
                Some(ExitCode::DataError),
                "every OCX_ENV decode failure is 65; {error} was not"
            );
        }
    }

    /// A separator disagreement between two contributors to one key is exit
    /// **65** — a declaration that cannot be honoured as written, the same
    /// class as every other env-declaration refusal.
    ///
    /// Both variants, for the same reason as the row above. Restores the
    /// assertion the classification relocation dropped: the body left behind in
    /// `ocx_config::env` still produced the error but no longer classified it.
    #[test]
    fn reconcile_conflict_classifies_as_data_error() {
        let variants = [
            ListSeparatorError::Conflict {
                key: "GODEBUG".to_owned(),
                first: ",".to_owned(),
                second: ";".to_owned(),
            },
            ListSeparatorError::EdgedValue {
                key: "GODEBUG".to_owned(),
                separator: ",".to_owned(),
                value: ",gctrace=1".to_owned(),
            },
        ];
        for error in &variants {
            assert_eq!(
                error.classify(),
                Some(ExitCode::DataError),
                "every list-separator refusal is 65; {error} was not"
            );
        }
    }

    /// C-057 / S-010: a name the composition does not provide is exit **65**.
    ///
    /// Asserted per variant rather than on one sample: the classification is an
    /// exhaustive match with no wildcard (D-V15(e)), and a blanket answer would
    /// make a later variant silently 65 with no test able to catch it.
    #[test]
    fn command_resolution_not_found_classifies_as_data_error() {
        let error = CommandResolutionError::NotFound {
            command: "cmake".into(),
            searched: vec![PathBuf::from("/p/.ocx/toolchain/bin")],
        };
        assert_eq!(
            ClassifyExitCode::classify(&error),
            Some(ExitCode::DataError),
            "S-010: a trampoline invoked under a name the composition lacks exits 65"
        );
        assert_eq!(
            crate::exit::classify_library_error(&error),
            ExitCode::DataError,
            "the CLI's own classifier must reach the same answer through the error chain"
        );
    }

    /// C-057 / C-069: the trampoline refusal is exit **65** too.
    ///
    /// Same code as its siblings on purpose — the exit code classifies the
    /// *class* of failure, and the guard identity lives in the variant and the
    /// message, which is where item 22 asserts which guard fired.
    #[test]
    fn command_resolution_trampoline_refusal_classifies_as_data_error() {
        let error = CommandResolutionError::TrampolineRefused {
            command: "cmake".into(),
            path: PathBuf::from("/p/.ocx/toolchain/bin/cmake"),
        };
        assert_eq!(ClassifyExitCode::classify(&error), Some(ExitCode::DataError));
        assert_eq!(
            crate::exit::classify_library_error(&error),
            ExitCode::DataError,
            "the refusal reaches the CLI as 65, not as the generic Failure"
        );
    }

    // ── moved from ocx_config::managed_config::persistence with the impl ──

    // ── moved from ocx_package_manager::managed_config::publish with the impl ──

    /// Every payload-rejection arm of `ManagedConfigPublishError::classify`,
    /// per variant.
    ///
    /// Recovered from `7adaea62:crates/ocx_lib/src/managed_config/publish.rs`
    /// and from the committed pin, never read off the arms below them — the
    /// `ExtraCaCertsInvalid | ExtraCaCertsNotUtf8` arm was block-form at the base
    /// revision, so the pin's extractor discarded it, and WP-10's strip took the
    /// eleven `classify` assertions the module's own tests carried. Between the
    /// two, this impl survived a mutation with the whole `-p ocx --lib` run
    /// byte-identical.
    ///
    /// Recovery source per group:
    /// - the 78 alternation — pin row `publish.rs:277`;
    /// - the 65 pair — base source `publish.rs:289` (block-form, unpinned);
    /// - the `source.kind()` ladder — pin rows `publish.rs:295-297`, reached
    ///   through the base's block-form arm at `publish.rs:294`.
    #[test]
    fn every_managed_config_publish_rejection_classifies_as_it_did_before_the_split() {
        // The payload's own content is the operator's config mistake: 78.
        let config_faults: Vec<ManagedConfigPublishError> = vec![
            ManagedConfigPublishError::PayloadTooLarge {
                actual: 1_048_577,
                maximum: 1_048_576,
            },
            ManagedConfigPublishError::InvalidToml {
                source: toml::from_str::<toml::Value>("= 1").expect_err("a bare `=` is not valid TOML"),
            },
            ManagedConfigPublishError::AmbiguousTrustRoot,
            ManagedConfigPublishError::AmbiguousExtraCaCerts,
            ManagedConfigPublishError::ManagedConfigKeyByPath,
            ManagedConfigPublishError::ExtraCaCertsPemInvalid {
                source: TlsError::Empty {
                    origin: ExtraRootsSource::Env,
                },
            },
        ];
        for error in &config_faults {
            let rendered = error.to_string();
            assert_eq!(
                error.classify(),
                Some(ExitCode::ConfigError),
                "a payload rejection is an operator config mistake (78): {rendered}"
            );
            assert_eq!(
                crate::exit::classify_library_error(error),
                ExitCode::ConfigError,
                "and it must still reach main as 78 through the chain walker: {rendered}"
            );
        }

        // C-004: content that fails the certificate check is a *data* error,
        // deliberately diverging from `TrustedRootInvalid` above.
        let content_faults: Vec<ManagedConfigPublishError> = vec![
            ManagedConfigPublishError::ExtraCaCertsInvalid {
                source: TlsError::Empty {
                    origin: ExtraRootsSource::EnvPath(PathBuf::from("/etc/ocx/roots.pem")),
                },
            },
            ManagedConfigPublishError::ExtraCaCertsNotUtf8 {
                origin: ExtraRootsSource::EnvPath(PathBuf::from("/etc/ocx/roots.pem")),
                // Built from a runtime value: a literal would trip
                // `invalid_from_utf8`, which is deny-by-default here.
                source: std::str::from_utf8(&[0x80u8 | 0x7f]).expect_err("0xff is not UTF-8"),
            },
        ];
        for error in &content_faults {
            let rendered = error.to_string();
            assert_eq!(
                error.classify(),
                Some(ExitCode::DataError),
                "bundle content that fails C-004 is 65, not 78: {rendered}"
            );
            assert_eq!(
                crate::exit::classify_library_error(error),
                ExitCode::DataError,
                "and it must still reach main as 65 through the chain walker: {rendered}"
            );
        }

        // The three read failures answer by `source.kind()`, not by a fixed
        // code — asserted on every reader so a later one cannot be wired to a
        // constant without a red.
        type Reader = fn(std::io::Error) -> ManagedConfigPublishError;
        let readers: Vec<(&str, Reader)> = vec![
            ("ReadFailed", |source| ManagedConfigPublishError::ReadFailed {
                path: PathBuf::from("/etc/ocx/config.toml"),
                source,
            }),
            ("TrustedRootReadFailed", |source| {
                ManagedConfigPublishError::TrustedRootReadFailed {
                    path: PathBuf::from("/etc/ocx/trusted_root.json"),
                    source,
                }
            }),
            ("ExtraCaCertsReadFailed", |source| {
                ManagedConfigPublishError::ExtraCaCertsReadFailed {
                    origin: ExtraRootsSource::EnvPath(PathBuf::from("/etc/ocx/roots.pem")),
                    source,
                }
            }),
        ];
        let ladder = [
            (std::io::ErrorKind::NotFound, ExitCode::NotFound),
            (std::io::ErrorKind::PermissionDenied, ExitCode::PermissionDenied),
            (std::io::ErrorKind::InvalidData, ExitCode::IoError),
        ];
        for (name, build) in &readers {
            for (kind, expected) in ladder {
                let error = build(std::io::Error::from(kind));
                assert_eq!(
                    error.classify(),
                    Some(expected),
                    "{name} must answer by the io kind: {kind:?} is {expected:?}"
                );
                assert_eq!(
                    crate::exit::classify_library_error(&error),
                    expected,
                    "{name} must reach main as {expected:?} for {kind:?}"
                );
            }
        }
    }

    /// Registry-side failures delegate classification to the inner cause
    /// (here `OfflineMode` → PolicyBlocked 81), both directly and through the
    /// `classify_error` chain walker.
    #[test]
    fn push_failures_defer_to_inner_classification() {
        let err = ManagedConfigPublishError::PushFailed {
            source: Box::new(ocx_package_manager::Error::OfflineMode),
        };
        assert_eq!(err.classify(), Some(ExitCode::PolicyBlocked));
        assert_eq!(
            crate::exit::classify_library_error(&err as &(dyn std::error::Error + 'static)),
            ExitCode::PolicyBlocked
        );
    }

    // ── moved from ocx_config::loader with the impl ──

    // ── moved from ocx_config with the impl ──

    /// RUL-7 — **the refusal is reachable as exit 78 from a binary.** The route
    /// is `ToolchainRootError` → [`ocx_config::error::Error`] →
    /// `crate::exit::classify_error`, which is what `main` runs; the row above stays
    /// green when that route is broken, so this one is what separates
    /// "classified" from "reachable".
    #[test]
    fn every_refusal_reaches_the_binary_exit_code_as_78() {
        for refusal in every_refusal_variant() {
            let rendered = refusal.to_string();
            let routed = ocx_config::error::Error::from(refusal);
            assert_eq!(
                crate::exit::classify_library_error(&routed),
                ExitCode::ConfigError,
                "a refusal reaching main must exit 78, not 1: {rendered}"
            );
        }
    }

    /// Every refusal classifies as 78. On its own this proves **nothing** about
    /// what a user sees — the impl it exercises is reached only through
    /// [`ocx_config::error::Error`] — so it is the control for the row
    /// below, which follows the route a binary actually takes.
    #[test]
    fn every_refusal_variant_classifies_as_a_config_error() {
        for refusal in every_refusal_variant() {
            assert_eq!(
                refusal.classify(),
                Some(ExitCode::ConfigError),
                "every toolchain_dir refusal is a config error (78); {refusal} was not"
            );
        }
    }

    // fixture from ocx_lib (config)
    /// One value of each [`ToolchainRootError`] variant.
    ///
    /// Listed by hand rather than derived: the enum is `#[non_exhaustive]` and
    /// has no iterator, so a variant added without a row here is invisible to
    /// both exit-code rows. The compile-time guard against that is
    /// `classify`'s own wildcard-free `match`.
    fn every_refusal_variant() -> Vec<ToolchainRootError> {
        let tier = ToolchainRootTier::ConfigFile;
        let path = || PathBuf::from("/tmp/ocx-tc");
        vec![
            ToolchainRootError::Unexpandable {
                tier,
                declared: path(),
                defect: ocx_config::shell::EntryDefect::UnsupportedTildeUser,
            },
            ToolchainRootError::Relative { tier, declared: path() },
            ToolchainRootError::ParentDirComponent { tier, declared: path() },
            ToolchainRootError::NoContainmentAnchor { tier, declared: path() },
            ToolchainRootError::IsContainmentAnchor {
                tier,
                resolved: path(),
                anchor: path(),
            },
            ToolchainRootError::OutsideHome { tier, resolved: path() },
            ToolchainRootError::SystemPrefix { tier, resolved: path() },
            ToolchainRootError::InsideGlobalToolchainHome {
                tier,
                resolved: path(),
                global_home: path(),
            },
            ToolchainRootError::Inaccessible {
                tier,
                resolved: path(),
                checked: path(),
                source: std::io::Error::from_raw_os_error(13),
            },
            ToolchainRootError::NotADirectory {
                tier,
                resolved: path(),
                checked: path(),
            },
            ToolchainRootError::NotOwnerOwned {
                tier,
                resolved: path(),
                checked: path(),
                owner: "0".to_owned(),
                effective_user: "1000".to_owned(),
            },
            ToolchainRootError::GroupOrWorldWritable {
                tier,
                resolved: path(),
                checked: path(),
                mode: 0o777,
            },
        ]
    }
}
