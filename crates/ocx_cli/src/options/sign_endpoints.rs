// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The flags every signing command shares: where the Sigstore services are and how the OIDC identity token arrives.
//!
//! No `--identity-token <VALUE>` flag: a raw token on the command line leaks into shell history.

/// Fulcio and Rekor endpoints, the identity-token sources and the interactive-login switch.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct SignEndpointsOpt {
    // Both URLs are `Option`, not clap-defaulted, or a configured Sigstore instance can never apply.
    /// Fulcio CA endpoint (the keyless certificate issuer)
    ///
    /// Defaults to public Fulcio. The host picks the `[sigstore."<host>"]` config table whose trust
    /// root and Rekor endpoint apply, so a private stack is named by its Fulcio alone.
    ///
    /// Keyless-only: an error alongside `--key`, never silently ignored.
    #[clap(long = "fulcio-url", value_name = "URL", conflicts_with = "key")]
    fulcio_url: Option<String>,

    /// Rekor transparency-log endpoint
    ///
    /// Defaults to the selected `[sigstore."<host>"]` table's `rekor_url`, else public Rekor. With
    /// `--key`, the table whose `rekor_url` has this host is selected.
    #[clap(long = "rekor-url", value_name = "URL")]
    rekor_url: Option<String>,

    /// Read the OIDC identity token from this file (highest precedence).
    ///
    /// Use this when the CI system writes the token to a file instead of the
    /// environment (GitHub Actions `$ACTIONS_ID_TOKEN_REQUEST_TOKEN` flow is
    /// env-based; other systems write the token out).
    ///
    /// Security: `--identity-token-file` does not follow symlinks. On Unix the
    /// file is opened with `O_NOFOLLOW` and rejected if not owned by the
    /// effective user or if group/other permission bits are set.
    #[clap(
        long = "identity-token-file",
        value_name = "PATH",
        conflicts_with = "identity_token_stdin",
        conflicts_with = "key"
    )]
    identity_token_file: Option<std::path::PathBuf>,

    /// Read the OIDC identity token from stdin (second precedence).
    ///
    /// Mutually exclusive with `--identity-token-file`. Accepts a newline-terminated
    /// token on stdin; trailing whitespace is trimmed.
    #[clap(
        long = "identity-token-stdin",
        conflicts_with = "identity_token_file",
        conflicts_with = "key"
    )]
    identity_token_stdin: bool,

    /// Suppress the interactive browser OAuth fallback (CI / headless).
    ///
    /// When set, ambient detection must succeed or the override flags must
    /// supply a token; there is no interactive recovery path.
    #[clap(long = "no-tty", conflicts_with = "key")]
    no_tty: bool,
}

impl SignEndpointsOpt {
    pub fn fulcio_url(&self) -> Option<&str> {
        self.fulcio_url.as_deref()
    }

    pub fn rekor_url(&self) -> Option<&str> {
        self.rekor_url.as_deref()
    }

    pub fn identity_token_file(&self) -> Option<&std::path::Path> {
        self.identity_token_file.as_deref()
    }

    pub fn identity_token_stdin(&self) -> bool {
        self.identity_token_stdin
    }

    pub fn no_tty(&self) -> bool {
        self.no_tty
    }
}
