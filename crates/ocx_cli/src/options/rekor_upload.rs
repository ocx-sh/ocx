// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::oci::sign::SignErrorKind;

/// Whether a signature is recorded in the Rekor transparency log.
///
/// Flatten into a command with `#[clap(flatten)]` to add the paired
/// `--rekor-upload` / `--no-rekor-upload` flags, POSIX last-wins
/// (`overrides_with`) like every other flag pair here. Resolve with
/// [`RekorUploadOpt::enabled`] and never read the two booleans directly.
///
/// The two key models are deliberately asymmetric, and the resolver is where
/// that asymmetry lives:
///
/// * **Keyless always uploads.** A Fulcio certificate is valid for about ten
///   minutes, and the log entry's timestamp is the only lasting proof the
///   signature was made while it was. `--no-rekor-upload` is refused there, and
///   configuration is ignored without a warning.
/// * **A key pair does not upload unless asked.** The flag decides, then
///   `[trust.sigstore] rekor_upload`, then off.
///
/// The keyless refusal is **not** clap `requires = "key"`. Clap would render
/// "the following required arguments were not provided: --key", which inverts
/// the real reason: the problem is not that a key is missing, it is that a
/// keyless signature without a log entry cannot be verified once the
/// certificate expires. [`Self::enabled`] returns that reason instead.
///
/// Arg ids: `rekor_upload`, `no_rekor_upload`.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct RekorUploadOpt {
    /// Record the signature in the Rekor transparency log.
    ///
    /// Keyless signatures are always recorded, so this only has an effect
    /// alongside `--key`, where uploading is off by default.
    #[clap(long = "rekor-upload", overrides_with = "no_rekor_upload")]
    rekor_upload: bool,

    /// Skip the Rekor entry.
    ///
    /// Only valid alongside `--key`. A keyless signature must be recorded: its
    /// Fulcio certificate is valid for about ten minutes, and the log entry's
    /// timestamp is the only lasting proof the signature was made while it was.
    #[clap(long = "no-rekor-upload", overrides_with = "rekor_upload")]
    no_rekor_upload: bool,
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
impl RekorUploadOpt {
    /// Resolve whether a transparency record is created.
    ///
    /// `key_mode` is the caller's key model (`KeyOpt::is_key_mode`).
    /// `configured` is `[trust.sigstore] rekor_upload`, which applies to key
    /// mode **only**: under keyless it is ignored, and deliberately without a
    /// warning. Erroring, or even warning, on every keyless signature because a
    /// fleet-wide key-mode setting says `false` would let an unrelated
    /// configuration key break the default signing path.
    ///
    /// # Errors
    /// [`SignErrorKind::RekorUploadRequiredForKeyless`] when
    /// `--no-rekor-upload` is given without a key. That variant carries the
    /// reason, and exits 64.
    pub fn enabled(&self, key_mode: bool, configured: Option<bool>) -> Result<bool, SignErrorKind> {
        if !key_mode {
            if self.no_rekor_upload {
                return Err(SignErrorKind::RekorUploadRequiredForKeyless);
            }
            return Ok(true);
        }
        if self.rekor_upload {
            return Ok(true);
        }
        if self.no_rekor_upload {
            return Ok(false);
        }
        Ok(configured.unwrap_or(false))
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;
    use ocx_lib::cli::ClassifyErrorKind as _;

    use super::*;

    #[derive(clap::Parser)]
    struct Harness {
        #[clap(flatten)]
        rekor_upload: RekorUploadOpt,
    }

    fn parse(args: &[&str]) -> RekorUploadOpt {
        let mut argv = vec!["harness"];
        argv.extend_from_slice(args);
        Harness::try_parse_from(argv).expect("parse").rekor_upload
    }

    /// T-22. The whole resolution table, both key models in one place so the
    /// asymmetry is visible as a table rather than argued in prose.
    ///
    /// Rows 2 and 3 are the load-bearing pair: they differ only in `key_mode`
    /// and disagree in their answer, so a resolver that dropped the `key_mode`
    /// guard cannot satisfy both.
    #[test]
    fn enabled_resolves_the_key_model_before_the_configuration() {
        /// One row of the resolution table, named so the five values cannot
        /// be silently swapped -- three of them are booleans.
        struct Row {
            key_mode: bool,
            args: &'static [&'static str],
            configured: Option<bool>,
            expected: bool,
            why: &'static str,
        }
        let row = |key_mode, args, configured, expected, why| Row {
            key_mode,
            args,
            configured,
            expected,
            why,
        };

        let rows = [
            row(false, &[], None, true, "keyless uploads by default"),
            row(
                false,
                &[],
                Some(false),
                true,
                "a key-mode configuration must not disable the keyless upload",
            ),
            row(true, &[], None, false, "key mode is off unless asked"),
            row(true, &[], Some(true), true, "key mode honours the configuration"),
            row(
                true,
                &["--rekor-upload"],
                Some(false),
                true,
                "the flag outranks the configuration",
            ),
            row(
                true,
                &["--no-rekor-upload"],
                Some(true),
                false,
                "the flag outranks the configuration in both directions",
            ),
        ];
        for Row {
            key_mode,
            args,
            configured,
            expected,
            why,
        } in rows
        {
            // `SignErrorKind` is not `PartialEq` (it carries boxed sources), so
            // the row compares the resolved boolean and fails loudly on an
            // unexpected refusal rather than silently on a shape mismatch.
            let resolved = parse(args)
                .enabled(key_mode, configured)
                .unwrap_or_else(|error| panic!("row must resolve, got {error}: {why}"));
            assert_eq!(
                resolved, expected,
                "{why} (key_mode={key_mode}, args={args:?}, configured={configured:?})"
            );
        }
    }

    /// The refusal a clap `requires = "key"` would have rendered backwards.
    #[test]
    fn no_rekor_upload_without_a_key_is_refused_with_its_reason() {
        let error = parse(&["--no-rekor-upload"])
            .enabled(false, None)
            .expect_err("keyless must refuse to skip the log");
        assert_eq!(error.kind_detail(), "rekor_upload_required_for_keyless");

        let message = error.to_string();
        for fragment in ["--no-rekor-upload", "--key", "ten minutes"] {
            assert!(
                message.contains(fragment),
                "the refusal must carry `{fragment}` so the user learns why, not just that: {message}"
            );
        }
    }

    /// The refusal keys on the flag, not on the absence of `--rekor-upload`:
    /// an affirmative `--rekor-upload` under keyless is redundant, not wrong.
    #[test]
    fn an_explicit_rekor_upload_under_keyless_is_accepted() {
        assert_eq!(parse(&["--rekor-upload"]).enabled(false, Some(false)).ok(), Some(true));
    }

    /// POSIX last-wins, proved through the public resolver in both directions.
    #[test]
    fn last_wins() {
        assert_eq!(
            parse(&["--rekor-upload", "--no-rekor-upload"]).enabled(true, None).ok(),
            Some(false),
            "--no-rekor-upload wins when last"
        );
        assert_eq!(
            parse(&["--no-rekor-upload", "--rekor-upload"]).enabled(true, None).ok(),
            Some(true),
            "--rekor-upload wins when last"
        );
    }

    /// T-24. `enabled` reads `no_rekor_upload` first under keyless, so a stale
    /// losing occurrence would be invisible through the public path in exactly
    /// the case that matters: `--no-rekor-upload --rekor-upload` under keyless
    /// must be accepted, not refused. Assert the fields directly.
    #[test]
    fn the_losing_occurrence_is_dropped() {
        let loser_upload = parse(&["--rekor-upload", "--no-rekor-upload"]);
        assert!(
            !loser_upload.rekor_upload,
            "an overridden --rekor-upload must not stay set"
        );
        assert!(loser_upload.no_rekor_upload);

        let loser_no_upload = parse(&["--no-rekor-upload", "--rekor-upload"]);
        assert!(
            !loser_no_upload.no_rekor_upload,
            "an overridden --no-rekor-upload must not stay set"
        );
        assert!(loser_no_upload.rekor_upload);
        assert_eq!(
            loser_no_upload.enabled(false, None).ok(),
            Some(true),
            "an overridden --no-rekor-upload must not still refuse a keyless upload"
        );
    }
}
