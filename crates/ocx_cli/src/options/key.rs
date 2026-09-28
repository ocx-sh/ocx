// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_trust::key_ref::{KeyRef, KeyRefError};

/// Sign or verify with a key pair instead of keyless Sigstore.
// Arg id `key` is frozen: keyless-only flags declare `conflicts_with = "key"`, so a rename silently
// unhooks them; `the_arg_id_stays_key` pins it.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct KeyOpt {
    /// Sign or verify with a key pair instead of keyless Sigstore.
    ///
    /// Takes a key reference, `[scheme://]<rest>`: a bare path or `file://` names
    /// a file; `env://VAR` reads the key PEM from the variable `VAR` itself (it
    /// holds the key, not a path), for a runner with no writable disk. Name it
    /// `OCX_SIGNING_KEY` unless you have a reason not to: ocx strips that one from
    /// every child process it spawns, while a name ocx does not know is inherited
    /// by plugins and generated launchers. The `awskms`, `gcpkms`, `azurekms`,
    /// `hashivault` and `k8s` schemes are recognised and rejected by name. Unset
    /// means keyless; an encrypted key's password comes from `OCX_KEY_PASSWORD`.
    #[clap(long = "key", value_name = "REF")]
    key: Option<String>,
}

// A future `#[expect(dead_code)]` goes on this block, never per method, or every attaching command edits this file.
impl KeyOpt {
    /// Parse the reference. `Ok(None)` means keyless.
    ///
    /// # Errors
    /// [`KeyRefError`] verbatim; the caller's `SignErrorKind::from` or
    /// `VerifyErrorKind::from` routes an unimplemented backend to 85, the rest to 64.
    pub fn reference(&self) -> Result<Option<KeyRef>, KeyRefError> {
        self.key.as_deref().map(KeyRef::parse).transpose()
    }

    /// Whether key mode was selected, without parsing, so a malformed reference
    /// is reported once, by [`Self::reference`].
    pub fn is_key_mode(&self) -> bool {
        self.key.is_some()
    }
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory as _, Parser as _};
    use ocx_trust::key_ref::Scheme;

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

    /// Every scheme is described the way it actually behaves -- an implemented
    /// one by the spelling that reaches it, an unimplemented one as a bare
    /// name in the rejected list.
    ///
    /// Nothing compiler-enforces this: `Scheme` gaining a variant, or one
    /// flipping to implemented, changes no string in this file. The loop
    /// derives from `Scheme::SPELLINGS` and `is_implemented`, so a scheme that
    /// changes status reds here instead of shipping help that contradicts the
    /// parser -- Block-tier per `quality-cli-help.md`.
    #[test]
    fn the_help_describes_every_scheme_the_way_the_parser_treats_it() {
        let rendered = Harness::command().render_long_help().to_string();
        assert!(
            rendered.contains("recognised and rejected by name"),
            "the help must still say which schemes are refused: {rendered}"
        );
        for spelling in Scheme::SPELLINGS {
            let scheme = Scheme::parse(spelling).expect("every spelling parses back");
            let bare = format!("`{spelling}`");
            if scheme.is_implemented() {
                assert!(
                    rendered.contains(&format!("{spelling}://")),
                    "`{spelling}` is implemented, so the help must show how to write it: {rendered}"
                );
                assert!(
                    !rendered.contains(&bare),
                    "`{spelling}` is implemented and must not sit in the rejected list: {rendered}"
                );
            } else {
                assert!(
                    rendered.contains(&bare),
                    "`{spelling}` is not implemented and must be named as rejected: {rendered}"
                );
            }
        }
    }

    /// `--key env://VAR` reaches the library grammar as an env reference, so
    /// the flag and the parser agree on what the spelling means.
    #[test]
    fn an_env_reference_parses_through_the_library_grammar() {
        let opt = parse(&["--key", "env://OCX_SIGNING_KEY"]);
        assert!(opt.is_key_mode());
        let key = opt.reference().expect("env:// must not be refused").expect("some");
        assert_eq!(key.scheme(), Scheme::Env);
        assert_eq!(key.as_env_var(), Some("OCX_SIGNING_KEY"));
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
