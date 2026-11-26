// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_trust::SigstoreAuthority;

/// The Sigstore instance the identity given on the command line signed under.
// Arg id: `sigstore`. The flag it needs, `certificate_identity`, is declared by the command.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct SigstoreInstanceOpt {
    /// Sigstore instance the signer used, named by the host of its Fulcio (`host[:port]`).
    ///
    /// Defaults to the public instance, `fulcio.sigstore.dev`. The instance's
    /// `[sigstore."<host>"]` table supplies the trust root and the Rekor endpoint. Requires
    /// `--certificate-identity`.
    ///
    /// Example: `fulcio.corp.example`, `fulcio.corp.example:5555`.
    #[clap(
        long = "sigstore",
        value_name = "AUTHORITY",
        requires = "certificate_identity",
        value_parser = parse_authority
    )]
    sigstore: Option<SigstoreAuthority>,
}

impl SigstoreInstanceOpt {
    /// The named instance, or the public one.
    pub fn instance(&self) -> SigstoreAuthority {
        self.sigstore.clone().unwrap_or_else(SigstoreAuthority::public)
    }
}

fn parse_authority(text: &str) -> Result<SigstoreAuthority, String> {
    SigstoreAuthority::parse(text).map_err(|error| format!("{}: {}", error.input, error.reason))
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::*;

    #[derive(clap::Parser)]
    struct Probe {
        #[clap(long = "certificate-identity")]
        certificate_identity: Option<String>,
        #[clap(flatten)]
        sigstore: SigstoreInstanceOpt,
    }

    fn parse(args: &[&str]) -> Result<Probe, clap::Error> {
        Probe::try_parse_from(std::iter::once("probe").chain(args.iter().copied()))
    }

    #[test]
    fn the_instance_defaults_to_public() {
        let probe = parse(&["--certificate-identity", "me"]).expect("parses");
        assert_eq!(probe.sigstore.instance(), SigstoreAuthority::public());
    }

    #[test]
    fn the_flag_names_the_instance_in_its_canonical_spelling() {
        let probe = parse(&["--certificate-identity", "me", "--sigstore", "Fulcio.Corp.Example:443"]).expect("parses");
        assert_eq!(probe.sigstore.instance().as_str(), "fulcio.corp.example");
    }

    #[test]
    fn the_flag_without_an_identity_is_refused() {
        let Err(error) = parse(&["--sigstore", "fulcio.corp.example"]) else {
            panic!("--sigstore alone must not parse");
        };
        assert_eq!(error.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn a_url_is_not_an_authority() {
        let Err(error) = parse(&[
            "--certificate-identity",
            "me",
            "--sigstore",
            "https://fulcio.corp.example",
        ]) else {
            panic!("a scheme must be refused");
        };
        assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
    }
}
