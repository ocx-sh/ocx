// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx package announce` — publish an owner-curated tag set into the index.
//!
//! The write-target refusals live on [`options::ForgeWriteOptions`], shared with `ocx package claim`:
//! re-declaring `--out` ⟂ `--fork` or a `validate` refusal here would let the two commands diverge.

use std::path::PathBuf;
use std::process::ExitCode;

use crate::error::UsageError;
use clap::Parser;
use ocx_announce::announce::{self, AnnounceRequest, AnnounceTarget, TagSelection};
use ocx_announce::forge::ForgeCredentials;

use crate::{api::data::announce::AnnounceReport, command::deprecated, options};

/// The tag-selection flags other than `--tags`, named once: a mode added to one attribute but not
/// the other would be silently accepted beside `--tags`.
const TAG_SELECTION_SIBLINGS_OF_TAGS: [&str; 3] = ["tags_file", "tags_from_registry", "refresh"];

/// Observe an owner-curated set of registry tags and publish the rebuilt entry into the index.
///
/// Reads the committed index entry, re-observes the given tags, and writes the rebuilt entry
/// to a local directory (`--out`) or opens a pull or merge request against the index
/// repository: from a fork with `--fork`, else from a branch on the index repository itself,
/// which needs push access there. A run that changes nothing reports as unchanged and commits
/// nothing; it opens a request only to recover one an earlier run left unopened.
///
/// A request needs a forge credential: `OCX_ANNOUNCE_TOKEN`, or the job token under
/// `--transport git` inside a GitLab job. Writing to `--out` works without one.
//
// `override_usage`: a required `ArgGroup` renders every member regardless of `Arg::hide`, so without
// it `--help` advertises the deprecated `--package`.
#[derive(Parser)]
#[command(override_usage = "ocx package announce [OPTIONS] <PACKAGE>")]
// 0.7 removal: goes with `command/deprecated.rs`. `.required(true)` alone is exactly-one: both spellings
// is `ArgumentConflict`, neither `MissingRequiredArgument`, both exit 64.
#[clap(group(
    clap::ArgGroup::new("package_selector")
        .args(["package", "package_flag"])
        .required(true),
))]
pub struct PackageAnnounce {
    /// Package to announce, as `<namespace>/<package>` (e.g. `acme/widget`).
    ///
    /// The positional is the canonical form; flags come before it.
    //
    // `Option` only while `--package` may supply it; the `package_selector` group makes one required.
    #[clap(value_name = "PACKAGE")]
    package: Option<options::Identifier>,

    /// Package to announce, as `<namespace>/<package>` (e.g. `acme/widget`).
    //
    // 0.7 removal: a hidden `Arg`, never a clap alias: an alias is invisible to `ArgMatches`, so it could not warn.
    #[clap(long = "package", value_name = "PACKAGE", hide = true)]
    package_flag: Option<options::Identifier>,

    /// Replace the curated tag set with this comma-separated list. A
    /// currently-committed tag that is not named here is dropped. So is a
    /// reserved tag named here: an `__ocx` tag (the keep tag included) or a
    /// legacy `sha256.<hex>` one is not a version, so the run still succeeds
    /// and reports the drops.
    #[clap(
        long = "tags",
        value_name = "TAGS",
        value_delimiter = ',',
        conflicts_with_all = TAG_SELECTION_SIBLINGS_OF_TAGS,
        required_unless_present_any = TAG_SELECTION_SIBLINGS_OF_TAGS,
    )]
    tags: Vec<String>,

    /// Add, update or remove the tags listed in this file: a listed tag the
    /// registry no longer has is removed from the index. Rows the file does not
    /// list are left as they are.
    ///
    /// The file holds comma- or newline-separated tag names. An empty file
    /// changes nothing.
    #[clap(long = "tags-file", value_name = "PATH", conflicts_with_all = ["refresh", "tags_from_registry"])]
    tags_file: Option<PathBuf>,

    /// Add every tag the registry holds; remove ephemeral rows whose tag is
    /// gone, and report durable ones.
    ///
    /// Use it to announce versions that were published before the package was
    /// in the index, or that an earlier announce missed. A yanked tag stays
    /// yanked.
    #[clap(long = "tags-from-registry", conflicts_with = "refresh")]
    tags_from_registry: bool,

    /// Re-observe every committed tag; remove ephemeral rows whose tag is gone,
    /// and report durable ones.
    ///
    /// Also picks up a digest that moved (e.g. `latest`).
    #[clap(long = "refresh")]
    refresh: bool,

    /// Mark the tags this run adds as removable without review.
    ///
    /// A tag already in the index keeps the marker it has. Push ephemeral
    /// builds with `--no-keep-tag`, or prune frees no storage. A keep tag pins
    /// the manifest past the tag's deletion.
    #[clap(long = "ephemeral", conflicts_with = "refresh")]
    ephemeral: bool,

    /// Where the request is written, and how it gets there.
    #[command(flatten)]
    forge: options::ForgeWriteOptions,

    /// Mark a tag as yanked. Repeat for multiple tags. Requires
    /// `--yank-reason`; only applies to a tag already in the curated set.
    #[clap(long = "yank", value_name = "TAG", requires = "yank_reason")]
    yank: Vec<String>,

    /// Clear the yanked marker from a tag. Repeat for multiple tags.
    #[clap(long = "unyank", value_name = "TAG")]
    unyank: Vec<String>,

    /// Reason recorded on every tag named by `--yank` in this run.
    #[clap(long = "yank-reason", value_name = "TEXT")]
    yank_reason: Option<String>,
}

impl PackageAnnounce {
    /// The package from whichever spelling the `package_selector` group admitted.
    ///
    /// Call once: the deprecated spelling warns here, so a second call warns twice.
    ///
    /// # Errors
    ///
    /// A usage error (exit 64) if neither spelling reached the merge.
    fn selected_package(&self, context: &crate::app::Context) -> anyhow::Result<&options::Identifier> {
        match (&self.package, &self.package_flag) {
            (Some(positional), None) => Ok(positional),
            (None, Some(flag)) => {
                // `ui().warn` keeps the notice off stdout, so a `--format json` run stays one document.
                context.ui().warn(deprecated::package_flag_notice());
                Ok(flag)
            }
            // Unreachable through clap; a usage error rather than a panic, in case clap's semantics move.
            _ => Err(UsageError::new("name the package once, as a positional argument").into()),
        }
    }

    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // Argv faults before the credential check, so a malformed command reports exit 64, not a
        // missing token found only after the operator fixes the command.
        let kind = self.forge.validate()?;

        let package = self
            .selected_package(&context)?
            .with_domain(context.default_registry())?;

        let curated = if self.refresh {
            TagSelection::Refresh
        } else if self.tags_from_registry {
            TagSelection::FromRegistry
        } else if let Some(path) = &self.tags_file {
            // The shared bounded reader, not a bare `fs::read`: this path is
            // operator-typed, so `/dev/zero` here read until memory ran out.
            TagSelection::UnionFile(crate::options::tags::read_tags_file(path).await?)
        } else {
            TagSelection::Replace(self.tags.clone())
        };

        let target = self.target();

        // Only the namespace's `[registries."<ns>"]` entry, as on the index read path: no CLI flag may widen
        // the SSRF escape hatch.
        let trusted_hosts = context.trusted_hosts_for(package.registry());

        let request = AnnounceRequest {
            package: package.clone(),
            curated,
            target,
            index_repo: self.forge.index_repo.clone(),
            yank: self.yank.clone(),
            unyank: self.unyank.clone(),
            yank_reason: self.yank_reason.clone().unwrap_or_default(),
            trusted_hosts,
            // The client's own plain-HTTP allowance, so the pre-flight picks the dial scheme, and hence the
            // proxy variable, the client will actually use.
            insecure_hosts: context.insecure_hosts().to_vec(),
            ephemeral: self.ephemeral,
            run_url: run_url(|key| ocx_util::env::var(key)),
        };

        // Before the forge is built, so a missing or too-old git exits 69 with zero forge calls; gated on
        // the transport, since an unconditional probe would fail an `api` announce on a host without git.
        let git = if self.forge.needs_git() {
            Some(ocx_announce::forge::probe_git_binary().await?)
        } else {
            None
        };

        // Through `resolve` with the selected transport: a direct `OCX_ANNOUNCE_TOKEN` read misses the
        // GitLab job-token rung.
        let credentials = ForgeCredentials::resolve(self.forge.transport);
        self.forge.require_credential(&credentials)?;

        // On the shared flatten, so `ocx package claim` warns identically for the same state.
        self.forge.warn_push_identity(context.ui(), &credentials);

        // The merged extra-CA view, so a forge dial trusts what every other client does. Without `git`,
        // `ForgeKind::client` returns `GitUnavailable` (69) and `--transport git` could never succeed.
        let forge = kind.client(
            self.forge.transport,
            credentials.clone(),
            &self.forge.index_repo,
            git,
            context.extra_roots_merged(),
        )?;

        // Pinned through `ocx_oci::ssrf::GuardedResolver`: the registry a curated tag resolves against is
        // remote-controlled data, so the connect-time pin belongs here, not only in the pre-flight.
        let publisher = context.guarded_publisher(package.registry());

        let outcome = announce::announce(&publisher, Some(forge.as_ref()), request).await?;

        // Dropped, not refused: the run succeeded, so this is a stderr diagnostic beside the report.
        if !outcome.reserved_tags_dropped.is_empty() {
            context.ui().warn(format!(
                "not a version, dropped from the curated set: {}",
                outcome.reserved_tags_dropped.join(", ")
            ));
        }
        // The plain table has no column for these, so stderr is the only place a plain run reports them.
        if !outcome.removed.is_empty() {
            context.ui().warn(format!(
                "gone from the registry, removed from the index: {}",
                outcome.removed.join(", ")
            ));
        }
        if !outcome.durable_missing.is_empty() {
            context.ui().warn(format!(
                "gone from the registry, kept in the index; name them with --tags or --tags-file to remove them: {}",
                outcome.durable_missing.join(", ")
            ));
        }

        context.api().report(&AnnounceReport::from_outcome(
            outcome,
            kind,
            self.forge.transport,
            &credentials,
        ))?;

        Ok(ExitCode::SUCCESS)
    }

    /// The write target the flag pair selects; neither flag pushes to `--index-repo` itself.
    fn target(&self) -> AnnounceTarget {
        match (&self.forge.out, &self.forge.fork) {
            (Some(directory), _) => AnnounceTarget::Out(directory.clone()),
            (None, Some(coordinate)) => AnnounceTarget::Fork(coordinate.clone()),
            (None, None) => AnnounceTarget::Direct,
        }
    }
}

/// The URL of the CI run making this announce: GitLab's job URL, else the GitHub Actions run URL
/// assembled from its three parts, else none. A blank variable counts as unset.
fn run_url(var: impl Fn(&str) -> Option<String>) -> Option<String> {
    let read = |key: &str| {
        var(key)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    if let Some(job) = read("CI_JOB_URL") {
        return Some(job);
    }
    let server = read("GITHUB_SERVER_URL")?;
    let repository = read("GITHUB_REPOSITORY")?;
    let run_id = read("GITHUB_RUN_ID")?;
    Some(format!(
        "{}/{repository}/actions/runs/{run_id}",
        server.trim_end_matches('/')
    ))
}

#[cfg(test)]
mod tests {
    use clap::error::ErrorKind;
    use clap::{Args as _, CommandFactory as _, Parser as _};

    use ocx_announce::announce::AnnounceTarget;

    use super::PackageAnnounce;
    use crate::command::package_claim::PackageClaim;
    use crate::options::ForgeWriteOptions;

    // ── the `--package` deprecation window ─────────────────────────────

    /// The positional is the canonical spelling, and the value binds to the
    /// **positional** arg id.
    ///
    /// "Accepts" is satisfied by any grammar that parses, which is why the
    /// binding is asserted rather than `is_ok()`: a declaration that
    /// routed the bare argument into the deprecated `package_flag` id would
    /// parse exactly as happily and then warn every caller of the canonical form
    /// to migrate to the form they already use.
    ///
    /// **Green on arrival** — the stub declares the whole grammar.
    /// Mutation: swap the two `#[clap]` attribute blocks between `package` and
    /// `package_flag`; the parse still succeeds and both assertions red.
    #[test]
    fn announce_accepts_positional() {
        let args = PackageAnnounce::try_parse_from(["announce", "--tags", "1.0.0", "--out", "d", "acme/widget"])
            .expect("the positional is the canonical spelling");
        assert!(
            args.package_flag.is_none(),
            "a bare argument must not bind to the deprecated `--package` id"
        );
        assert_eq!(
            args.package.expect("the positional binds the value").raw(),
            "acme/widget"
        );
    }

    /// The deprecated spelling still parses, into its **own** arg id, and
    /// stays hidden from `--help`.
    ///
    /// Renamed from the inventory's `announce_hidden_package_flag_warns_once`.
    /// Once-ness is **not observable at unit scope**: one
    /// process dispatches one command, so "once" holds by construction here
    /// whatever the code does. The property that can actually break — a merge
    /// moved into an accessor `execute` reads more than once — is only visible
    /// against a real process, so the once-ness half is
    /// `test/tests/test_announce.py::test_deprecated_package_flag_warns_once_on_stderr_only`,
    /// which counts the needle in one run's stderr.
    ///
    /// What this half holds is the two facts that make the notice possible at
    /// all: a distinct arg id (a clap *alias* would be invisible to `ArgMatches`
    /// and could not be warned about) and `hide`.
    ///
    /// **Green on arrival** — the stub declares both.
    /// Mutation: replace the hidden `Arg` with `#[clap(alias = "package")]` on
    /// the positional (the id assertion reds); or delete `hide = true` (the
    /// `--help` assertion in `announce_usage_does_not_advertise_the_deprecated_flag`
    /// reds).
    #[test]
    fn announce_hidden_package_flag_binds_to_its_own_arg_id() {
        let args =
            PackageAnnounce::try_parse_from(["announce", "--tags", "1.0.0", "--out", "d", "--package", "acme/widget"])
                .expect("the deprecated spelling still executes for one release pair");
        assert!(
            args.package.is_none(),
            "the deprecated flag must not bind the positional id, or nothing could tell the two apart"
        );
        assert_eq!(
            args.package_flag.expect("the deprecated flag binds its own id").raw(),
            "acme/widget"
        );

        let declared = PackageAnnounce::command();
        let flag = declared
            .get_arguments()
            .find(|arg| arg.get_long() == Some("package"))
            .expect("the deprecated long is still declared, so it can be warned about");
        assert!(flag.is_hide_set(), "the deprecated spelling is hidden from --help");
    }

    /// Both spellings together is a **conflict**, exit 64.
    ///
    /// The `ErrorKind` is asserted, never `is_err()`: a mis-declared
    /// group refuses this argv too, with `MissingRequiredArgument`, so
    /// `is_err()` cannot tell a working exactly-one rule from a broken one.
    /// `clap_parse::parse` maps every non-help clap error to
    /// `ExitCode::UsageError`, which is where the 64 comes from.
    ///
    /// **Green on arrival** — the stub declares the `ArgGroup`.
    /// Mutation: delete the `package_selector` group; both spellings then parse
    /// together and this reds. (Dropping only `.required(true)` leaves this
    /// green and reds its sibling below — two guards over one rule, which is the
    /// case `quality-core.md` § Unchecked Green describes, not a weak check.)
    #[test]
    fn announce_both_forms_is_usage_error() {
        // `expect_err` would need `PackageAnnounce: Debug`, which is a
        // production derive added for a test's convenience; the match reads the
        // same and stays inside the test.
        let Err(error) = PackageAnnounce::try_parse_from([
            "announce",
            "--tags",
            "1.0.0",
            "--out",
            "d",
            "--package",
            "acme/widget",
            "acme/widget",
        ]) else {
            panic!("naming the package in both spellings must be refused");
        };
        assert_eq!(
            error.kind(),
            ErrorKind::ArgumentConflict,
            "both spellings together is a conflict, not a missing argument: {error}"
        );
    }

    /// Neither spelling is a **missing required argument**, exit 64.
    ///
    /// The sibling of the conflict above, and the half that
    /// `.required(true)` alone defends. The existing
    /// `package_is_required` asserts only `is_err()` and therefore cannot
    /// distinguish this from the conflict; it stays as the cheaper net.
    ///
    /// **Green on arrival** — the stub declares `.required(true)`.
    /// Mutation: drop `.required(true)` from the group; the argv parses `Ok`
    /// with both fields `None` and this reds.
    #[test]
    fn announce_neither_form_is_usage_error() {
        let Err(error) = PackageAnnounce::try_parse_from(["announce", "--tags", "1.0.0", "--out", "d"]) else {
            panic!("naming no package at all must be refused");
        };
        assert_eq!(
            error.kind(),
            ErrorKind::MissingRequiredArgument,
            "an absent package is a missing required argument: {error}"
        );
    }

    /// The deprecated spelling appears in **no** rendered help.
    ///
    /// A required `ArgGroup` renders every member in the usage line regardless
    /// of `Arg::hide`, and clap 4.6's `ArgGroup` carries no `hide` of its own —
    /// measured on this workspace's clap, both directions. Without the
    /// `override_usage` beside the group, the first line of
    /// `ocx package announce --help` reads
    /// `Usage: announce [OPTIONS] <PACKAGE|--package <PACKAGE>>` and advertises
    /// the spelling the window exists to retire, which `quality-cli-help.md`
    /// § Forbidden makes a Block-tier incorrect statement of behaviour once 0.7
    /// deletes the flag.
    ///
    /// The positive halves (`<PACKAGE>` in the usage line, `--tags` in the long
    /// help) are what keep the two negatives from passing over a string that
    /// stopped being rendered at all.
    ///
    /// **Green on arrival** — the stub carries `override_usage` and `hide`.
    /// Mutation: delete `override_usage` (the usage assertion reds); delete
    /// `hide = true` (the long-help assertion reds). Two attributes, two
    /// independent reds.
    #[test]
    fn announce_usage_does_not_advertise_the_deprecated_flag() {
        let usage = PackageAnnounce::command().render_usage().to_string();
        assert!(
            usage.contains("<PACKAGE>"),
            "the usage line still names the canonical positional: {usage}"
        );
        assert!(
            !usage.contains("--package"),
            "a required ArgGroup renders every member; override_usage is what keeps the deprecated \
             spelling out of the first line of --help: {usage}"
        );

        let help = PackageAnnounce::command().render_long_help().to_string();
        assert!(
            help.contains("--tags"),
            "the long help still renders the option block: {help}"
        );
        assert!(
            !help.contains("--package"),
            "a hidden Arg is absent from the option block too: {help}"
        );
    }

    /// Both write commands expose the whole shared grammar, and neither
    /// re-declares a flag the flatten already contributes.
    ///
    /// Membership, never set equality: `get_arguments()` yields announce's
    /// hidden `--package` too, so an exact-equality assertion over announce's
    /// longs would either fail spuriously or be "fixed" by filtering hidden args
    /// — which would equally hide a real regression. The claim-side sibling
    /// (`options/forge_write.rs::shared_forge_write_options_derived_set_matches_claim`)
    /// is membership-based for the same reason.
    ///
    /// Counted `== 1` rather than `contains`: announce declared
    /// `--index-repo`, `--forge`, `--fork` and `--out` itself before this
    /// package, and duplicating them beside the flatten rather than deleting
    /// them is the likely accident. A duplicate arg id is a
    /// `Command::build` panic, so it fails at runtime and not at `cargo check` —
    /// building both commands here is what makes it visible.
    ///
    /// The expected set is **derived** from `ForgeWriteOptions`, so a flag added
    /// to the flatten is required of both commands without anyone editing a
    /// list, and is asserted non-empty so an emptied struct reds rather than
    /// passing vacuously.
    ///
    /// **Green on arrival** — the stub already flattens.
    /// Mutation: rename `--transport` to `--write-transport` on the flatten
    /// alone; the membership loop reds for announce **and** for claim, from one
    /// place. Or restore announce's own `#[clap(long = "index-repo", …)]` beside
    /// the flatten; the count assertion reds at two.
    #[test]
    fn shared_forge_write_options_parity_across_both_commands() {
        let shared: Vec<String> = ForgeWriteOptions::augment_args(clap::Command::new("probe"))
            .get_arguments()
            .filter_map(clap::Arg::get_long)
            .map(str::to_string)
            .collect();
        assert!(
            !shared.is_empty(),
            "the shared write grammar must not be empty — an emptied struct would make this vacuous"
        );

        for (name, command) in [
            ("ocx package announce", PackageAnnounce::command()),
            ("ocx package claim", PackageClaim::command()),
        ] {
            let longs: Vec<&str> = command.get_arguments().filter_map(clap::Arg::get_long).collect();
            for flag in &shared {
                assert_eq!(
                    longs.iter().filter(|long| *long == &flag.as_str()).count(),
                    1,
                    "{name} must expose --{flag} exactly once: zero means the command dropped the shared \
                     grammar, two means it re-declared a flattened flag instead of inheriting it"
                );
            }
        }
    }

    /// The long help clap **renders** for a command, whitespace-normalized.
    ///
    /// Walks from the real root (`crate::app::Cli::command()`) rather than from
    /// the args struct's own `CommandFactory`, and that distinction is the whole
    /// point of this helper. `PackageAnnounce::command()` renders the struct's
    /// doc comment; `ocx package announce --help` renders whatever survived the
    /// `Package` enum's `augment_subcommands`, which is a **different string**
    /// whenever the variant carries a doc of its own. Every other help assertion
    /// in this file uses the former, so none of them can observe this.
    ///
    /// Normalized because `render_long_help` hard-wraps to the terminal width,
    /// so a needle spanning a wrap point never matches the raw text.
    fn rendered_long_help(path: [&str; 2]) -> String {
        use clap::CommandFactory as _;

        let mut command = crate::app::Cli::command();
        for name in path {
            let child = command
                .find_subcommand(name)
                .unwrap_or_else(|| panic!("`{name}` is a subcommand of `{}`", command.get_name()))
                .clone();
            command = child;
        }
        command
            .render_long_help()
            .to_string()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The credential guidance reaches the help clap actually renders, on
    /// **both** write commands.
    ///
    /// The defect this exists for was not wrong text — it was correct text
    /// nobody rendered. clap takes a subcommand's `about`/`long_about` from the
    /// enum **variant**'s doc comment when it has one, and falls back to the args
    /// struct's only when it does not, so the paragraph written on
    /// `PackageAnnounce`/`PackageClaim` was rustdoc-only while `--help` showed a
    /// one-line summary that named no credential at all. `quality-cli-help.md`
    /// § Render-source gotcha names exactly this trap; `package_cascade.rs`'s
    /// `CascadeGroup` is the in-repo precedent for the fix (variants carry no
    /// doc, the text lives with the flags it describes).
    ///
    /// The variable is read from [`ocx_config::env::keys`] rather than spelled here,
    /// the same discipline `options::forge_write` applies to its exit-80 refusal:
    /// help naming a variable the ladder no longer reads is worse than no help.
    ///
    /// The `--transport` row is the positive control. Without it a `contains`
    /// pair could pass over help that stopped rendering the option block
    /// entirely, which is the failure mode one command was already in.
    ///
    /// Mutation: reword any clause of the credential sentence in either
    /// command's doc comment (that command's row reds); or restore a `///` doc
    /// on the `Package::Announce` / `Package::Claim` variant, which is the
    /// original defect verbatim (that command's row reds).
    #[test]
    fn both_write_commands_render_the_credential_guidance() {
        let sentence = format!(
            "needs a forge credential: `{}`, or the job token under `--transport git` inside a \
             GitLab job. Writing to `--out` works without one.",
            ocx_config::env::keys::OCX_ANNOUNCE_TOKEN
        );

        for command in ["announce", "claim"] {
            let help = rendered_long_help(["package", command]);
            assert!(
                help.contains("--transport"),
                "ocx package {command} --help must render its option block at all, else the \
                 credential assertion below could pass over an empty page: {help}"
            );
            assert!(
                help.contains(&sentence),
                "ocx package {command} --help must state how the request is authenticated. \
                 Expected to find {sentence:?} in the RENDERED long help (clap prefers the \
                 `Package` enum variant's doc over the args struct's), got: {help}"
            );
        }
    }

    // ── clap surface ──────────────────────────────────────────────────────

    #[test]
    fn package_is_required() {
        assert!(
            PackageAnnounce::try_parse_from(["announce", "--tags", "1.0.0", "--out", "d"]).is_err(),
            "naming no package at all must be a clap usage error"
        );
    }

    #[test]
    fn package_parses_namespace_and_name() {
        let args = PackageAnnounce::try_parse_from(["announce", "--tags", "1.0.0", "--out", "d", "acme/widget"])
            .expect("valid invocation parses");
        assert_eq!(
            args.package.expect("the positional is the canonical spelling").raw(),
            "acme/widget"
        );
    }

    /// The four tag-selection modes, in the argv form each is given.
    const TAG_SELECTION_ARGV: [&[&str]; 4] = [
        &["--tags", "1.0.0"],
        &["--tags-file", "tags.txt"],
        &["--tags-from-registry"],
        &["--refresh"],
    ];

    #[test]
    fn tags_selection_is_required() {
        assert!(
            PackageAnnounce::try_parse_from(["announce", "--out", "d", "acme/widget"]).is_err(),
            "a tag selection is required"
        );
    }

    /// Exactly-one, proven over **every** pair rather than one sampled pair: the
    /// rule is spread across four `conflicts_with*` attributes, so a mode left
    /// out of one of them still compiles and is only observable as a pair that
    /// clap wrongly accepts.
    #[test]
    fn tags_selection_is_mutually_exclusive_over_every_pair() {
        for (index, first) in TAG_SELECTION_ARGV.iter().enumerate() {
            for second in &TAG_SELECTION_ARGV[index + 1..] {
                let mut argv = vec!["announce", "--out", "d"];
                argv.extend_from_slice(first);
                argv.extend_from_slice(second);
                argv.push("acme/widget");
                assert!(
                    PackageAnnounce::try_parse_from(&argv).is_err(),
                    "{first:?} and {second:?} together must be a clap usage error"
                );
            }
        }
    }

    /// The positive half: each mode on its own parses. Without it the pair test
    /// above would still pass if a flag were misspelled out of existence — every
    /// pair containing it would error for the wrong reason.
    #[test]
    fn every_tags_selection_parses_on_its_own() {
        for selection in &TAG_SELECTION_ARGV {
            let mut argv = vec!["announce", "--out", "d"];
            argv.extend_from_slice(selection);
            argv.push("acme/widget");
            assert!(
                PackageAnnounce::try_parse_from(&argv).is_ok(),
                "{selection:?} alone must be a valid selection"
            );
        }
    }

    #[test]
    fn ephemeral_with_refresh_is_a_usage_error() {
        let error =
            PackageAnnounce::try_parse_from(["announce", "--refresh", "--ephemeral", "--out", "d", "acme/widget"])
                .err()
                .expect("--ephemeral beside --refresh must not parse");
        assert_eq!(error.kind(), ErrorKind::ArgumentConflict);
        let args = PackageAnnounce::try_parse_from([
            "announce",
            "--tags-file",
            "t",
            "--ephemeral",
            "--out",
            "d",
            "acme/widget",
        ])
        .expect("--ephemeral beside --tags-file parses");
        assert!(args.ephemeral);
    }

    /// `--ephemeral` describes the rows a run adds, so it rides beside every
    /// selection that adds rows and is refused only beside `--refresh`, which
    /// adds none. Looping over all four keeps a selection left out of the
    /// conflict declaration from going unseen.
    #[test]
    fn ephemeral_composes_with_every_selection_except_refresh() {
        for selection in &TAG_SELECTION_ARGV {
            let mut argv = vec!["announce", "--ephemeral", "--out", "d"];
            argv.extend_from_slice(selection);
            argv.push("acme/widget");
            let parsed = PackageAnnounce::try_parse_from(&argv);
            if selection.contains(&"--refresh") {
                assert_eq!(
                    parsed
                        .err()
                        .expect("--ephemeral beside --refresh must not parse")
                        .kind(),
                    ErrorKind::ArgumentConflict,
                    "{selection:?} with --ephemeral must be a conflict"
                );
            } else {
                assert!(
                    parsed.expect("--ephemeral beside an adding selection parses").ephemeral,
                    "{selection:?} with --ephemeral must set the flag"
                );
            }
        }
    }

    /// Every one of the four help texts is rendered by `ocx package announce
    /// --help`, worded as the user contract. Each needle is a whole sentence, so
    /// a reworded clause reds its own row.
    #[test]
    fn announce_help_states_how_each_tag_flag_treats_a_vanished_tag() {
        let help = rendered_long_help(["package", "announce"]);
        for sentence in [
            "Add, update or remove the tags listed in this file: a listed tag the registry no \
             longer has is removed from the index. Rows the file does not list are left as \
             they are.",
            "Add every tag the registry holds; remove ephemeral rows whose tag is gone, and \
             report durable ones.",
            "Re-observe every committed tag; remove ephemeral rows whose tag is gone, and \
             report durable ones.",
            "Mark the tags this run adds as removable without review.",
        ] {
            assert!(
                help.contains(sentence),
                "ocx package announce --help must state {sentence:?}; rendered help: {help}"
            );
        }
    }

    /// The `--ephemeral` long help says when to disable the keep tag: an
    /// ephemeral build that keeps its keep tag leaves the registry holding the
    /// manifest, so pruning frees nothing.
    #[test]
    fn ephemeral_help_says_to_push_with_no_keep_tag() {
        let help = rendered_long_help(["package", "announce"]);
        assert!(
            help.contains("Push ephemeral builds with `--no-keep-tag`, or prune frees no storage."),
            "the --ephemeral long help must carry the keep-tag guidance; rendered help: {help}"
        );
    }

    /// An empty `--tags-file` is documented as a no-op, so the promise is in the
    /// same rendered text a user reads.
    #[test]
    fn tags_file_help_says_an_empty_file_changes_nothing() {
        let help = rendered_long_help(["package", "announce"]);
        assert!(
            help.contains("An empty file changes nothing."),
            "the --tags-file long help must state the empty-file behaviour; rendered help: {help}"
        );
    }

    #[test]
    fn run_url_prefers_gitlab_then_assembles_github_then_none() {
        let from = |pairs: &'static [(&'static str, &'static str)]| {
            super::run_url(move |key| {
                pairs
                    .iter()
                    .find(|(name, _)| *name == key)
                    .map(|(_, value)| (*value).to_string())
            })
        };
        const GITHUB: &[(&str, &str)] = &[
            ("GITHUB_SERVER_URL", "https://github.com/"),
            ("GITHUB_REPOSITORY", "acme/widget"),
            ("GITHUB_RUN_ID", "42"),
        ];
        assert_eq!(
            from(&[
                ("CI_JOB_URL", "https://gitlab.com/acme/widget/-/jobs/7"),
                ("GITHUB_RUN_ID", "42")
            ])
            .as_deref(),
            Some("https://gitlab.com/acme/widget/-/jobs/7")
        );
        assert_eq!(from(&[("CI_JOB_URL", "  ")]), None, "a blank job URL counts as unset");
        assert_eq!(
            from(GITHUB).as_deref(),
            Some("https://github.com/acme/widget/actions/runs/42")
        );
        assert_eq!(from(&GITHUB[..2]), None, "a GitHub run URL needs all three parts");
        assert_eq!(from(&[]), None);
    }

    #[test]
    fn tags_from_registry_sets_the_flag() {
        let args = PackageAnnounce::try_parse_from(["announce", "--tags-from-registry", "--out", "d", "acme/widget"])
            .expect("valid invocation parses");
        assert!(args.tags_from_registry);
    }

    #[test]
    fn tags_splits_on_commas() {
        let args = PackageAnnounce::try_parse_from(["announce", "--tags", "1.0.0,2.0.0", "--out", "d", "acme/widget"])
            .expect("valid invocation parses");
        assert_eq!(args.tags, vec!["1.0.0".to_string(), "2.0.0".to_string()]);
    }

    /// The whole point of the fork-free path: with neither `--out` nor
    /// `--fork`, the announce branch goes to the index repository — NOT to a
    /// fork, and not to a local directory. Asserting on the resolved
    /// `AnnounceTarget` rather than on "clap accepted it" is deliberate: clap
    /// would accept the invocation just as happily if the mapping still built
    /// a `Fork` out of thin air.
    #[test]
    fn omitting_out_and_fork_targets_the_index_repository_itself() {
        let args = PackageAnnounce::try_parse_from(["announce", "--tags", "1.0.0", "acme/widget"])
            .expect("a target-less invocation is the direct path, not a usage error");
        assert!(
            matches!(args.target(), AnnounceTarget::Direct),
            "no --out and no --fork must resolve to the direct (fork-free) target"
        );
    }

    /// The other two mappings, so the direct case above cannot pass by a
    /// `match` that collapsed everything onto one arm.
    #[test]
    fn out_and_fork_each_resolve_to_their_own_target() {
        let out = PackageAnnounce::try_parse_from(["announce", "--tags", "1.0.0", "--out", "somewhere", "acme/widget"])
            .expect("valid invocation parses");
        assert!(
            matches!(out.target(), AnnounceTarget::Out(directory) if directory == std::path::Path::new("somewhere"))
        );

        let fork = PackageAnnounce::try_parse_from([
            "announce",
            "--tags",
            "1.0.0",
            "--fork",
            "ocx-contrib/index",
            "acme/widget",
        ])
        .expect("valid invocation parses");
        assert!(
            matches!(fork.target(), AnnounceTarget::Fork(coordinate) if coordinate.full_path() == "ocx-contrib/index")
        );
    }

    #[test]
    fn target_selection_is_mutually_exclusive() {
        assert!(
            PackageAnnounce::try_parse_from([
                "announce",
                "--tags",
                "1.0.0",
                "--out",
                "d",
                "--fork",
                "o/r",
                "acme/widget",
            ])
            .is_err(),
            "--out and --fork together must be a clap usage error"
        );
    }

    #[test]
    fn fork_parses_a_namespace_and_project() {
        let args = PackageAnnounce::try_parse_from([
            "announce",
            "--tags",
            "1.0.0",
            "--fork",
            "ocx-contrib/index",
            "acme/widget",
        ])
        .expect("valid invocation parses");
        let fork = args.forge.fork.expect("--fork given");
        assert_eq!(fork.namespace, "ocx-contrib");
        assert_eq!(fork.project, "index");
    }

    #[test]
    fn index_repo_defaults_to_ocx_sh_index() {
        let args = PackageAnnounce::try_parse_from(["announce", "--tags", "1.0.0", "--out", "d", "acme/widget"])
            .expect("valid invocation parses");
        assert_eq!(args.forge.index_repo.host, None);
        assert_eq!(args.forge.index_repo.namespace, "ocx-sh");
        assert_eq!(args.forge.index_repo.project, "index");
    }

    #[test]
    fn yank_requires_yank_reason() {
        assert!(
            PackageAnnounce::try_parse_from([
                "announce",
                "--tags",
                "1.0.0",
                "--out",
                "d",
                "--yank",
                "0.9.0",
                "acme/widget",
            ])
            .is_err(),
            "--yank without --yank-reason must be a clap usage error"
        );
        let args = PackageAnnounce::try_parse_from([
            "announce",
            "--tags",
            "1.0.0",
            "--out",
            "d",
            "--yank",
            "0.9.0",
            "--yank-reason",
            "security",
            "acme/widget",
        ])
        .expect("--yank with --yank-reason parses");
        assert_eq!(args.yank, vec!["0.9.0".to_string()]);
        assert_eq!(args.yank_reason.as_deref(), Some("security"));
    }

    /// Negative test: the SSRF exemption is config-only (`[registries."<ns>"].trusted_hosts`)
    /// — there must be no CLI flag that could widen a locked source's trust
    /// set. Walks the built `clap::Command` looking for a `trusted-host`
    /// long flag by name, rather than trusting a hand exhaustive `--help`
    /// read.
    #[test]
    fn no_trusted_host_flag_is_registered() {
        let command = PackageAnnounce::command();
        let has_trusted_host_flag = command
            .get_arguments()
            .any(|arg| arg.get_long() == Some("trusted-host"));
        assert!(
            !has_trusted_host_flag,
            "ocx package announce must not expose a --trusted-host flag (design register X2)"
        );
    }
}
