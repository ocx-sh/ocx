// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use ocx_lib::prelude::VecExt as _;
use ocx_lib::utility::fs::{BoundedReadError, read_bounded};

/// The largest a `--tags-file` may be.
///
/// An OCI tag is at most 128 characters, so this is room for roughly a thousand
/// of them with a separator each — orders of magnitude past any real cascade
/// sweep, and it exists only to bound the read of a path an operator names.
const MAX_TAGS_FILE_BYTES: u64 = 128 * 1024;

/// Which tags a command sweeps over.
///
/// Flatten into a command with `#[clap(flatten)]` to add `--tags` and
/// `--tags-file`. The two are a union, not alternatives: a caller can name a
/// few tags inline and read the rest from the file `ocx package push
/// --tags-file` wrote. Resolve with [`TagsOpt::resolve`] and never read either
/// field directly.
///
/// `--tags-file` keeps the spelling `push` and `announce` already use, and
/// reads it with the same parser, so one file format has one reader.
///
/// Arg ids: `tags`, `tags_file`. A command that also takes `--platform`
/// declares the sweep exclusivity against those ids in its own command file:
/// a sweep is over indices by definition, and narrowing into one platform
/// contradicts it.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct TagsOpt {
    /// Tags to sweep. Repeatable, and accepts a comma-separated list.
    #[clap(long = "tags", value_name = "TAG", value_delimiter = ',')]
    tags: Vec<String>,

    /// Read tags from a file, one per line or comma-separated.
    ///
    /// The same file `ocx package push --tags-file` writes, so a publish step
    /// can hand its tag list to a later step verbatim.
    #[clap(long = "tags-file", value_name = "PATH")]
    tags_file: Option<PathBuf>,
}

/// Read a `--tags-file`, bounded at [`MAX_TAGS_FILE_BYTES`] and refusing
/// anything that is not a regular file. Unparsed, and with the refusal still
/// typed: one caller treats an absent file as an empty set and needs to see
/// which refusal it got.
///
/// Blocking, so it goes to the pool rather than an async twin of the guard:
/// one bounded reader, not two.
async fn read_tags_bytes(path: &std::path::Path) -> Result<Vec<u8>, BoundedReadError> {
    let target = path.to_path_buf();
    match tokio::task::spawn_blocking(move || read_bounded(&target, MAX_TAGS_FILE_BYTES)).await {
        Ok(result) => result,
        // `ErrorKind::Other`, never `NotFound`, so a panicking pool task cannot
        // be mistaken for an absent file by the fall-through below.
        Err(join) => Err(BoundedReadError::Io {
            path: path.to_path_buf(),
            source: std::io::Error::other(format!("tags-file read task panicked: {join}")),
        }),
    }
}

/// One door for every way this file can be unusable — missing, a directory, or
/// past the cap — because the frozen exit-code table already sends
/// `--tags-file` failures to 74 and an enormous file is not a different
/// question for the caller.
fn tags_file_error(path: &std::path::Path, error: BoundedReadError) -> anyhow::Error {
    let io = match error {
        BoundedReadError::Io { source, .. } => source,
        refusal => std::io::Error::other(refusal),
    };
    anyhow::Error::new(ocx_lib::error::file_error(path, io)).context(format!("reading tags file {}", path.display()))
}

/// Read and parse a `--tags-file`, where the file must be there.
///
/// The one reader for this file format's *input* side, shared by [`TagsOpt`]
/// (`sign`, `attest`) and by `package announce`, which takes its own
/// `--tags-file` rather than flattening `TagsOpt`. Two copies of a bounded read
/// on an operator-typed path is how one of them ends up without the bound —
/// `read_bounded`'s own module doc records that history.
///
/// # Errors
/// When the path cannot be read: missing, not a regular file, or past the cap.
pub(crate) async fn read_tags_file(path: &std::path::Path) -> anyhow::Result<Vec<String>> {
    let bytes = read_tags_bytes(path)
        .await
        .map_err(|error| tags_file_error(path, error))?;
    // The one shared parser for this file format, already used by
    // `package announce` and `package cascade repair`.
    Ok(crate::conventions::parse_tags_file(&bytes))
}

/// The same read, for the one caller whose file may legitimately not exist yet:
/// `package push --tags-file` appends to a file it creates, so absence is an
/// empty set rather than a failure.
///
/// **Absence only.** `TooLarge` and `NotRegularFile` refuse a file that IS
/// there, and treating either as "no tags yet" would let `push` overwrite an
/// operator's tag list with just this run's tags — the same shape as the
/// trust-root ladder's rung-4 arm, and the reason `BoundedReadError` carries no
/// wildcard-friendly variant.
///
/// # Errors
/// When the path exists and cannot be read: not a regular file, or past the cap.
pub(crate) async fn read_tags_file_if_present(path: &std::path::Path) -> anyhow::Result<Vec<String>> {
    match read_tags_bytes(path).await {
        Ok(bytes) => Ok(crate::conventions::parse_tags_file(&bytes)),
        Err(BoundedReadError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(tags_file_error(path, error)),
    }
}

impl TagsOpt {
    /// Whether any sweep input was given at all.
    ///
    /// Answers without reading the file, because the exclusivity question ("is
    /// this a sweep?") must not depend on whether the file happens to be empty
    /// or readable: an unreadable `--tags-file` is still a sweep, and reporting
    /// it as a `--platform` conflict instead would name the wrong flag.
    pub fn is_sweep(&self) -> bool {
        !self.tags.is_empty() || self.tags_file.is_some()
    }

    /// The union of `--tags` and the file's entries, deduped with the first
    /// occurrence winning, `--tags` first. `Ok(vec![])` when neither was given.
    ///
    /// # Errors
    /// When `--tags-file` names a path that cannot be read. An empty or
    /// tag-less file is not an error: it contributes nothing.
    pub async fn resolve(&self) -> anyhow::Result<Vec<String>> {
        let mut resolved = self.tags.clone();
        if let Some(path) = &self.tags_file {
            resolved.extend(read_tags_file(path).await?);
        }
        // Keep-first dedup preserving order, so a tag named by both inputs
        // stays at the position `--tags` gave it.
        resolved.unique();
        Ok(resolved)
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::*;

    #[derive(clap::Parser)]
    struct Harness {
        #[clap(flatten)]
        tags: TagsOpt,
    }

    fn parse(args: &[&str]) -> TagsOpt {
        let mut argv = vec!["harness"];
        argv.extend_from_slice(args);
        Harness::try_parse_from(argv).expect("parse").tags
    }

    async fn resolve(args: &[&str]) -> Vec<String> {
        parse(args).resolve().await.expect("resolve")
    }

    #[test]
    fn nothing_given_is_not_a_sweep() {
        let opt = parse(&[]);
        assert!(!opt.is_sweep());
    }

    /// `--tags` is repeatable AND comma-delimited, and the two spellings are
    /// interchangeable rather than mutually exclusive.
    #[tokio::test]
    async fn tags_are_repeatable_and_comma_delimited() {
        assert_eq!(resolve(&["--tags", "3.28.1,3.28"]).await, ["3.28.1", "3.28"]);
        assert_eq!(
            resolve(&["--tags", "3.28.1", "--tags", "3.28"]).await,
            ["3.28.1", "3.28"]
        );
        assert_eq!(
            resolve(&["--tags", "3.28.1,3.28", "--tags", "latest"]).await,
            ["3.28.1", "3.28", "latest"]
        );
        assert!(parse(&["--tags", "3.28.1"]).is_sweep());
    }

    /// T-23. Union, dedup and order across both inputs: `--tags` leads, the
    /// file appends only what it adds, and a tag named by both keeps the
    /// position `--tags` gave it.
    ///
    /// The duplicated tag is deliberately **not** adjacent across the join:
    /// `--tags` names it first and something else second, so keep-first and
    /// keep-last produce different orders. With the obvious fixture (the
    /// duplicate landing next to itself) the two are indistinguishable, and
    /// "deduped" would be all this could pin.
    #[tokio::test]
    async fn the_file_and_the_flag_union_dedup_and_keep_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("tags.txt");
        std::fs::write(&path, "3.28,latest\n3\n").expect("write");
        let path = path.to_str().expect("utf-8 path");

        assert_eq!(
            resolve(&["--tags", "3.28,3.28.1", "--tags-file", path]).await,
            ["3.28", "3.28.1", "latest", "3"],
            "flag entries lead in their own order, `3.28` keeps its first position, \
             and the file contributes only `latest` and `3`"
        );
        // Each input alone, so the union above cannot be satisfied by one side.
        assert_eq!(resolve(&["--tags-file", path]).await, ["3.28", "latest", "3"]);
        assert_eq!(resolve(&["--tags", "3.28,3.28.1"]).await, ["3.28", "3.28.1"]);
    }

    /// A file is a sweep input even when it contributes nothing, and an empty
    /// file is not an error.
    #[tokio::test]
    async fn an_empty_file_is_a_sweep_that_contributes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("empty.txt");
        std::fs::write(&path, "").expect("write");
        let path = path.to_str().expect("utf-8 path");

        assert!(parse(&["--tags-file", path]).is_sweep());
        assert!(resolve(&["--tags-file", path]).await.is_empty());
    }

    /// An unreadable file is an error from `resolve`, and still a sweep for
    /// `is_sweep` -- the two questions are answered independently on purpose.
    #[tokio::test]
    async fn a_missing_file_is_an_error_but_still_a_sweep() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("absent.txt");
        let path = path.to_str().expect("utf-8 path");

        let opt = parse(&["--tags-file", path]);
        assert!(opt.is_sweep());
        let error = opt.resolve().await.expect_err("a missing file must fail");
        assert!(
            format!("{error:#}").contains("absent.txt"),
            "the error must name the path: {error:#}"
        );
    }

    /// The read is bounded, so a `--tags-file` pointed at something enormous
    /// is refused rather than pulled into memory (CWE-400).
    ///
    /// This asserts the *routing* — that `resolve` goes through
    /// `utility::fs::read_bounded` at all. The bound itself is proved where it
    /// lives, by `bounded_read`'s `Take::limit` test: no path-level assertion
    /// can see it, because the length check refuses the same file with the same
    /// error after reading all of it.
    #[tokio::test]
    async fn a_tags_file_past_the_cap_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("enormous.txt");
        std::fs::write(&path, vec![b'a'; MAX_TAGS_FILE_BYTES as usize + 1]).expect("write");
        let path = path.to_str().expect("utf-8 path");

        let error = parse(&["--tags-file", path])
            .resolve()
            .await
            .expect_err("a tags file past the cap must be refused");
        assert!(
            format!("{error:#}").contains(&format!("larger than {MAX_TAGS_FILE_BYTES} bytes")),
            "the refusal must name the ceiling it enforced: {error:#}"
        );

        // The accepting half: one byte under the cap still reads, so the bound
        // refuses the file rather than the feature.
        let ok = dir.path().join("big.txt");
        std::fs::write(&ok, vec![b'a'; MAX_TAGS_FILE_BYTES as usize]).expect("write");
        resolve(&["--tags-file", ok.to_str().expect("utf-8 path")]).await;
    }

    /// A directory is refused by name, not read as an empty tag list.
    #[tokio::test]
    async fn a_directory_tags_file_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let error = parse(&["--tags-file", dir.path().to_str().expect("utf-8 path")])
            .resolve()
            .await
            .expect_err("a directory must be refused");
        assert!(
            format!("{error:#}").contains("not a regular file"),
            "the refusal must name why the path was unusable: {error:#}"
        );
    }

    /// The frozen arg ids, proved the way a command file will depend on them:
    /// clap panics while building a command whose `conflicts_with` names an
    /// unknown id, and the conflict is then shown to actually fire.
    #[test]
    fn the_arg_ids_stay_tags_and_tags_file() {
        #[derive(clap::Parser, Debug)]
        struct ConflictHarness {
            #[clap(flatten)]
            tags: TagsOpt,
            #[clap(long = "platform", conflicts_with_all = ["tags", "tags_file"])]
            platform: Option<String>,
        }

        assert!(ConflictHarness::try_parse_from(["harness", "--platform", "linux/amd64"]).is_ok());
        assert!(ConflictHarness::try_parse_from(["harness", "--tags", "3.28"]).is_ok());
        for (argv, other) in [
            (vec!["harness", "--platform", "linux/amd64", "--tags", "3.28"], "--tags"),
            (
                vec!["harness", "--platform", "linux/amd64", "--tags-file", "tags.txt"],
                "--tags-file",
            ),
        ] {
            let rendered = ConflictHarness::try_parse_from(argv)
                .expect_err("a sweep and a platform must conflict")
                .to_string();
            assert!(
                rendered.contains("--platform") && rendered.contains(other),
                "clap must name both flags: {rendered}"
            );
        }
    }
}
