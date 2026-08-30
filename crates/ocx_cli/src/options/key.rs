// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::oci::sign::{KeyRef, KeyRefError};

/// Sign or verify with a key pair instead of keyless Sigstore.
///
/// Flatten into a command with `#[clap(flatten)]` to add `--key`. Resolve with
/// [`KeyOpt::reference`] and never read the field directly: the raw string is
/// an unparsed reference, and the difference between an unimplemented backend
/// (exit 85) and a malformed reference (exit 64) is decided by the parser.
///
/// **Arg id: `key`** -- the field name, and the frozen half of this contract.
/// A command that carries both this group and a keyless-only flag declares
/// `conflicts_with = "key"` on that flag, in its own command file. Renaming the
/// field silently unhooks every one of those declarations, so
/// `the_arg_id_stays_key` pins it here.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct KeyOpt {
    /// Sign or verify with a key pair instead of keyless Sigstore.
    ///
    /// Takes a key reference, `[scheme://]<rest>`. A bare path, or a `file://`
    /// one, names a file. The `awskms`, `gcpkms`, `azurekms`, `hashivault`
    /// and `k8s` schemes are recognised and rejected by name. Leave it unset to
    /// sign or verify keyless. The password for an encrypted private key is
    /// read from `OCX_KEY_PASSWORD`.
    #[clap(long = "key", value_name = "REF")]
    key: Option<String>,
}

/// # This block no longer carries `expect(dead_code)`
///
/// It did while nothing attached this group: `[workspace.lints.rust] warnings =
/// "deny"` makes an uncalled inherent method a build failure, because the
/// `clap::Args` derive keeps the *type* live through its foreign-trait impls
/// but not its methods. `expect` rather than `allow` was the point -- an
/// unfulfilled expectation is itself a build failure, so the suppression could
/// not outlive its reason. Loop C attached the last resolver, the expectation
/// went unfulfilled, and deleting the attribute became the only way to compile:
/// exactly the self-cleaning the placement was chosen for.
///
/// The attribute sits on the **block**, never on the individual methods, and
/// that placement is part of the frozen contract. A block-level `expect` stays
/// fulfilled while any one item under it is still unattached, so a command that
/// attaches only some of these resolvers compiles without editing this file;
/// only the command attaching the last one sees the unfulfilled-expectation
/// error, and at that point deleting the attribute is both correct and
/// unavoidable. Per-method attributes would make *every* attaching command edit
/// this file instead -- several authors writing to one frozen file, which is
/// the collision the freeze exists to prevent.
///
/// `cfg_attr(not(test), ...)` because the tests below are callers, so the lint
/// never fires in a test build and an unconditional `expect` would be
/// unfulfilled there instead.
impl KeyOpt {
    /// Parse the reference. `Ok(None)` means keyless.
    ///
    /// # Errors
    /// [`KeyRefError`] verbatim. The caller maps it into its own taxonomy with
    /// `SignErrorKind::from` or `VerifyErrorKind::from`, which is what routes
    /// an unimplemented backend to exit 85 and everything else to exit 64.
    pub fn reference(&self) -> Result<Option<KeyRef>, KeyRefError> {
        self.key.as_deref().map(KeyRef::parse).transpose()
    }

    /// Whether key mode was selected, without parsing the reference.
    ///
    /// For the callers that only branch on the key model -- the Rekor upload
    /// rule is the one that matters -- so that a malformed reference is
    /// reported once, by [`Self::reference`], rather than twice in two
    /// different vocabularies.
    pub fn is_key_mode(&self) -> bool {
        self.key.is_some()
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;
    use ocx_lib::oci::sign::Scheme;

    use super::*;

    #[derive(clap::Parser)]
    struct Harness {
        #[clap(flatten)]
        key: KeyOpt,
    }

    fn parse(args: &[&str]) -> KeyOpt {
        let mut argv = vec!["harness"];
        argv.extend_from_slice(args);
        Harness::try_parse_from(argv).expect("parse").key
    }

    /// Unset is keyless, and keyless is not an error.
    #[test]
    fn no_key_is_keyless() {
        let opt = parse(&[]);
        assert!(!opt.is_key_mode());
        assert_eq!(opt.reference(), Ok(None));
    }

    /// A file reference parses through the library grammar, not a second one.
    #[test]
    fn a_file_reference_parses_through_the_library_grammar() {
        let opt = parse(&["--key", "cosign.pub"]);
        assert!(opt.is_key_mode());
        let key = opt.reference().expect("parse").expect("some");
        assert_eq!(key.scheme(), Scheme::File);
        assert_eq!(key.rest(), "cosign.pub");
    }

    /// A recognised-but-unimplemented backend surfaces as its own error, so the
    /// caller can route it to exit 85 instead of reporting a missing file.
    #[test]
    fn an_unimplemented_backend_surfaces_as_its_own_error() {
        assert_eq!(
            parse(&["--key", "awskms://alias/release"]).reference(),
            Err(KeyRefError::UnsupportedBackend { scheme: Scheme::AwsKms })
        );
        assert_eq!(
            parse(&["--key", "vault://secret/key"]).reference(),
            Err(KeyRefError::UnknownScheme {
                scheme: "vault".to_string()
            })
        );
        // `is_key_mode` answers without parsing, so a bad reference still reads
        // as key mode -- the parse error is reported once, by `reference`.
        assert!(parse(&["--key", "awskms://alias/release"]).is_key_mode());
    }

    /// The frozen arg id, proved the way a consumer will actually depend on it.
    ///
    /// A sibling flag declaring `conflicts_with = "key"` is exactly what a
    /// command file will write, and clap panics while building a command whose
    /// `conflicts_with` names an unknown id. Constructing this harness at all
    /// therefore proves the id resolves; the assertions then prove the conflict
    /// fires in both orders, so the declaration is not merely accepted.
    #[test]
    fn the_arg_id_stays_key() {
        #[derive(clap::Parser, Debug)]
        struct ConflictHarness {
            #[clap(flatten)]
            key: KeyOpt,
            #[clap(long = "fulcio-url", conflicts_with = "key")]
            fulcio_url: Option<String>,
        }

        let solo = ConflictHarness::try_parse_from(["harness", "--key", "cosign.pub"]).expect("key alone parses");
        assert!(solo.key.is_key_mode());
        assert!(
            ConflictHarness::try_parse_from(["harness", "--fulcio-url", "https://fulcio.test"]).is_ok(),
            "a keyless-only flag alone must still parse"
        );
        for argv in [
            ["harness", "--key", "cosign.pub", "--fulcio-url", "https://fulcio.test"],
            ["harness", "--fulcio-url", "https://fulcio.test", "--key", "cosign.pub"],
        ] {
            let error = ConflictHarness::try_parse_from(argv).expect_err("the two must conflict");
            let rendered = error.to_string();
            assert!(
                rendered.contains("--key") && rendered.contains("--fulcio-url"),
                "clap must name both flags so the message explains itself: {rendered}"
            );
        }
    }
}
