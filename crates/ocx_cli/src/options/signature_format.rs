// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::cli::UsageError;
use ocx_lib::oci::sign::SignatureFormat;

/// Which cosign wire shape a command writes, or which shape a verify accepts.
///
/// Flatten into a command with `#[clap(flatten)]` to add `--signature-format`.
/// The value grammar is the same on both sides of the read/write split, but the
/// two sides admit different subsets: writing accepts `both`, pinning does not,
/// because "either of two signatures satisfied me" is not a statement a
/// verification result can carry. That asymmetry lives in the two resolvers
/// below rather than in a second enum or a stringly-typed value parser, so
/// there is exactly one vocabulary. Resolve with [`SignatureFormatOpt::write_format`]
/// or [`SignatureFormatOpt::pin`] and never read the field directly.
///
/// Arg id: `signature_format`.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct SignatureFormatOpt {
    /// Signature wire format: bundle (default), simplesigning, or both.
    ///
    /// `bundle` writes an OCI 1.1 referrer carrying a Sigstore bundle.
    /// `simplesigning` writes the cosign `sha256-<hex>.sig` sidecar tag
    /// instead. `both` writes each of them. When verifying, this pins the one
    /// shape to accept, and `both` is not a pin.
    #[clap(long = "signature-format", value_enum, value_name = "FORMAT")]
    signature_format: Option<SignatureFormat>,
}

impl SignatureFormatOpt {
    /// The shape to write. Defaults to [`SignatureFormat::Bundle`].
    pub fn write_format(&self) -> SignatureFormat {
        self.signature_format.unwrap_or_default()
    }

    /// The shape to accept when reading.
    ///
    /// `Ok(None)` means the caller was given no pin and should prefer a bundle,
    /// falling back to a simplesigning sidecar.
    ///
    /// # Errors
    /// [`SignatureFormatPinError`] when `both` is named. `both` selects what to
    /// write; a verify pins a single shape.
    pub fn pin(&self) -> Result<Option<SignatureFormat>, SignatureFormatPinError> {
        match self.signature_format {
            Some(SignatureFormat::Both) => Err(SignatureFormatPinError),
            other => Ok(other),
        }
    }
}

/// `--signature-format both` reached a read-side resolver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("--signature-format both selects what to write; verify pins a single format (bundle or simplesigning)")]
pub struct SignatureFormatPinError;

impl From<SignatureFormatPinError> for UsageError {
    /// The refusal is a bad invocation, so it must reach exit 64.
    ///
    /// `UsageError` is the type `classify_error` downcasts for that code; a
    /// bare [`SignatureFormatPinError`] propagated through `anyhow` would
    /// classify as a generic failure instead. A call site therefore spells the
    /// refusal `opt.pin().map_err(UsageError::from)?`.
    fn from(error: SignatureFormatPinError) -> Self {
        Self::new(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::*;

    #[derive(clap::Parser)]
    struct Harness {
        #[clap(flatten)]
        signature_format: SignatureFormatOpt,
    }

    fn parse(args: &[&str]) -> SignatureFormatOpt {
        let mut argv = vec!["harness"];
        argv.extend_from_slice(args);
        Harness::try_parse_from(argv).expect("parse").signature_format
    }

    /// T-21, write side. Unset writes a bundle; each named value round-trips
    /// through the clap channel to the enum the library froze.
    #[test]
    fn the_write_side_defaults_to_bundle_and_round_trips_every_value() {
        assert_eq!(parse(&[]).write_format(), SignatureFormat::Bundle);
        for (value, expected) in [
            ("bundle", SignatureFormat::Bundle),
            ("simplesigning", SignatureFormat::Simplesigning),
            ("both", SignatureFormat::Both),
        ] {
            assert_eq!(
                parse(&["--signature-format", value]).write_format(),
                expected,
                "`{value}` must reach the write side verbatim"
            );
        }
    }

    /// T-21, read side. `both` is the one value the write side accepts and the
    /// read side refuses, and the refusal names why rather than saying "invalid
    /// value" -- which is what a `PossibleValuesParser` narrowed to two values
    /// would have produced.
    #[test]
    fn the_read_side_pins_one_shape_and_refuses_both() {
        assert_eq!(parse(&[]).pin(), Ok(None), "no pin means prefer bundle, then sidecar");
        assert_eq!(
            parse(&["--signature-format", "bundle"]).pin(),
            Ok(Some(SignatureFormat::Bundle))
        );
        assert_eq!(
            parse(&["--signature-format", "simplesigning"]).pin(),
            Ok(Some(SignatureFormat::Simplesigning))
        );
        assert_eq!(
            parse(&["--signature-format", "both"]).pin(),
            Err(SignatureFormatPinError)
        );

        let message = SignatureFormatPinError.to_string();
        assert!(
            message.contains("both") && message.contains("single format"),
            "the refusal must say what `both` is for and what a verify needs: {message}"
        );
    }

    /// An unknown value is clap's error, not a silent fallback to the default.
    #[test]
    fn an_unknown_format_is_a_parse_error() {
        assert!(Harness::try_parse_from(["harness", "--signature-format", "dsse"]).is_err());
    }

    /// The refusal carries exit-64 classification through the house type,
    /// rather than leaving the code to whatever the call site remembers.
    #[test]
    fn the_pin_refusal_converts_into_a_usage_error() {
        let usage = UsageError::from(SignatureFormatPinError);
        assert_eq!(usage.to_string(), SignatureFormatPinError.to_string());
    }
}
