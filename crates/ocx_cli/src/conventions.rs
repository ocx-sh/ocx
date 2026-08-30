// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::{
    ci::CiFlavor,
    cli::{MetadataResolutionError, UsageError},
    lazy::LazyMode,
    oci,
    package::cascade::apply::WriteOutcome,
    package::metadata::env::entry::Entry,
    package_manager::composer::lazy_mode_for_package,
    publisher::LayerRef,
    shell::Shell,
};

/// Derives a `<stem>-<suffix>.json` sidecar path beside an archive file.
///
/// For example `/path/to/package.tar.gz` with suffix `metadata` yields
/// `/path/to/package-metadata.json`. Shared by [`infer_metadata_file`] and
/// [`infer_receipt_file`] so the two sidecars a build produces are always
/// derived the same way and land next to each other.
fn sidecar_path(content: &std::path::Path, suffix: &str) -> Result<std::path::PathBuf, MetadataResolutionError> {
    let content_parent = content
        .parent()
        .ok_or_else(|| MetadataResolutionError::InvalidLayerPath {
            layer: content.to_path_buf(),
            reason: "no parent directory".into(),
        })?;
    let mut content_name = content
        .file_stem()
        .ok_or_else(|| MetadataResolutionError::InvalidLayerPath {
            layer: content.to_path_buf(),
            reason: "no file stem".into(),
        })?
        .to_string_lossy()
        .to_string();
    let known_archive_extensions = [".tar", ".tar.gz", ".tgz", ".zip"];
    for extension in known_archive_extensions {
        if content_name.ends_with(extension) {
            content_name.truncate(content_name.len() - extension.len());
            break;
        }
    }
    Ok(content_parent.join(format!("{content_name}-{suffix}.json")))
}

/// Infers a metadata file path based on the archive file path.
/// For example, if the content path is `/path/to/package.tar.gz`, this function will return `/path/to/package-metadata.json`.
pub fn infer_metadata_file(content: &std::path::Path) -> Result<std::path::PathBuf, MetadataResolutionError> {
    sidecar_path(content, "metadata")
}

/// Infers the build-receipt path beside the archive file — the metadata path's
/// twin (`/path/to/package.tar.gz` -> `/path/to/package-receipt.json`).
///
/// Written by `ocx package create --metadata`, read by `ocx package push` and
/// `ocx package test`.
pub fn infer_receipt_file(content: &std::path::Path) -> Result<std::path::PathBuf, MetadataResolutionError> {
    sidecar_path(content, "receipt")
}

/// Resolves the metadata path used by `ocx package push` and `ocx package
/// test`.
///
/// When `explicit` is `Some`, it wins. Otherwise the helper walks the file
/// layers, infers a candidate metadata path for each, and dedups: zero file
/// layers → [`MetadataResolutionError::Required`], multiple distinct
/// candidates → [`MetadataResolutionError::Ambiguous`].
pub fn resolve_metadata_path(
    layers: &[LayerRef],
    explicit: Option<&std::path::Path>,
) -> Result<std::path::PathBuf, MetadataResolutionError> {
    if let Some(p) = explicit {
        return Ok(p.to_path_buf());
    }
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    for layer in layers {
        if let LayerRef::File { path: file, .. } = layer {
            let candidate = infer_metadata_file(file)?;
            if !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        }
    }
    match candidates.len() {
        0 => Err(MetadataResolutionError::Required),
        1 => Ok(candidates.into_iter().next().unwrap()),
        _ => Err(MetadataResolutionError::Ambiguous { candidates }),
    }
}

/// Resolves the build-receipt path `ocx package push` and `ocx package test`
/// fall back to for whatever their flags did not supply.
///
/// Mirrors [`resolve_metadata_path`]'s layer walk, but the receipt anchors to
/// the **bundle** alone — there is no `--receipt` flag and `--metadata` never
/// redirects it, because the receipt describes how the layer was built rather
/// than what the package declares. Zero file layers (a config-only push) or
/// several disagreeing candidates yield `None` rather than an error: no
/// receipt is a supported state that simply makes the flags required.
pub fn resolve_receipt_path(layers: &[LayerRef]) -> Option<std::path::PathBuf> {
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    for layer in layers {
        // Fail-closed on an underivable path: no receipt means `--platform`
        // becomes required, which is the safe answer. The metadata sibling
        // propagates instead because there the file is mandatory input.
        if let LayerRef::File { path: file, .. } = layer
            && let Ok(candidate) = infer_receipt_file(file)
            && !candidates.contains(&candidate)
        {
            candidates.push(candidate);
        }
    }
    match candidates.as_slice() {
        [only] => Some(only.clone()),
        _ => None,
    }
}

/// Reads the published metadata sidecar `ocx package push` and `ocx package
/// test` consume.
///
/// The file is the wire form `ocx package create --metadata` compiled — every
/// dependency pinned to a manifest digest — so a parse failure most often
/// means the sidecar was hand-authored or edited rather than compiled. The
/// context line says so; the underlying serialization failure classifies to
/// `DataError` (65).
pub async fn read_published_metadata(path: &std::path::Path) -> anyhow::Result<ocx_lib::package::metadata::Metadata> {
    use anyhow::Context as _;
    use ocx_lib::prelude::*;

    ocx_lib::package::metadata::Metadata::read_json(path)
        .await
        .with_context(|| {
            format!(
                "reading package metadata from {}; `ocx package create -m <FILE> -p <PLATFORM>` \
                 compiles an authoring sidecar into this form",
                path.display()
            )
        })
}

/// Resolves an explicit `--platform` value, falling back to the current host
/// platform when the flag was omitted.
///
/// The single source of truth for "which platform does this command resolve
/// against" — every resolution command (`ocx package install/pull/exec`,
/// `ocx exec`, `ocx env`, ...) applies the same default.
pub fn platform_or_default(platform: Option<oci::Platform>) -> oci::Platform {
    platform.unwrap_or_else(|| oci::Platform::current().unwrap_or_else(oci::Platform::any))
}

/// Resolves the OCI-tier `lazy-mode` ladder for one invocation, applying
/// `--self`'s eager override.
///
/// **Laziness has no meaning under `--self`.** A generated shim exists only to
/// serve consumers — it is an INTERFACE launcher — so under the private view it
/// is gated out along with `entrypoints/`, and a deferred package would land on
/// `PATH` as nothing at all. The private view therefore resolves to
/// [`LazyMode::Never`], logged at debug so the override is visible without
/// being noise.
///
/// The refusal is on the **value**, not on the flags co-occurring.
/// `--self --lazy-mode always` is two contradictory requests and is a usage
/// error (exit 64). `--self --lazy-mode never` asks for eager twice and is
/// accepted, as is `--self` with an `always` inherited from `OCX_LAZY_MODE` —
/// a less-specific tier outranked by the user's own more-specific flag, which
/// is the ladder working rather than a contradiction.
///
/// # Errors
///
/// [`UsageError`] when `self_view` is set and the CLI tier explicitly typed
/// [`LazyMode::Always`].
///
/// Shared by the two OCI-tier composing commands (`ocx package env`,
/// `ocx package exec`); the project tier resolves through
/// `ocx_lib::project::lazy_mode_for_tool` instead, which reads the `ocx.toml`
/// tiers this one has none of.
pub fn resolved_lazy_mode(cli: Option<LazyMode>, self_view: bool) -> Result<LazyMode, UsageError> {
    if self_view && cli == Some(LazyMode::Always) {
        return Err(UsageError::new(
            "--self and --lazy-mode always ask for contradictory things: a shim is a consumer-facing launcher, and --self selects the private view that bypasses launchers",
        ));
    }
    let resolved = lazy_mode_for_package(cli);
    if self_view && resolved == LazyMode::Always {
        ocx_lib::log::debug!(
            "Composing eagerly: --self selects a package's private view, which bypasses the launchers a shim is made of."
        );
        return Ok(LazyMode::Never);
    }
    Ok(resolved)
}

/// Emit shell-sourceable export lines for a slice of env entries.
///
/// This is the single shared emit helper consumed by:
/// - `ocx env` (toolchain-tier, new Phase 2 command)
/// - `ocx package env` (OCI-tier, delegates here for `--shell` output)
/// - `ocx direnv export` (delegates here instead of inlining the loop)
///
/// Wraps [`Shell::export_path`] / [`Shell::export_constant`] /
/// [`Shell::export_list`] and skips entries the shell cannot express — a key
/// that fails POSIX validation, or a `list` entry under `cmd.exe`, which has no
/// case-sensitive string replacement. Either way a `# ocx:` note goes to stderr
/// naming the actual reason, so the caller is informed without aborting the
/// full output.
///
/// **The reserved-`OCX_*` gate is deliberately not here.** This function's
/// output is `eval`d, so a package-declared `OCX_CONSENT_NAMESPACES` reaching it
/// is a consent bypass — but the refusal belongs at
/// `PackageManager::resolve_env_with_attribution`, the one seam this function
/// and `Env::apply_entries` both read from. A second copy of the check here
/// would silently absorb a regression in that one, and would leave `ocx exec` —
/// which never comes through here — exposed anyway. `is_valid_env_key` below is
/// the *grammar* gate and is not a substitute for it.
///
/// `Shell::Bash` is the fixed shell for `direnv export` (direnv always evals
/// `.envrc` in a bash sub-shell — no `--shell` flag on that command). For
/// `ocx env` / `ocx package env` the caller passes the user-selected shell.
///
/// # Panics
///
/// This function is infallible — `None` from `export_path` / `export_constant`
/// is handled by a stderr note.
pub fn emit_lines(shell: Shell, entries: &[Entry]) {
    for entry in entries {
        match emit_line(shell, entry) {
            Ok(line) => println!("{line}"),
            Err(note) => eprintln!("# ocx: {note}"),
        }
    }
}

/// The per-entry half of [`emit_lines`]: the statement to print on stdout, or
/// the reason to note on stderr.
///
/// Split out so the admission rules are testable — `emit_lines` itself only
/// decides which of the two streams the result goes to.
fn emit_line(shell: Shell, entry: &Entry) -> Result<String, String> {
    use ocx_lib::package::metadata::env::list::DEFAULT_SEPARATOR;
    use ocx_lib::package::metadata::env::modifier::ModifierKind;

    /// The `--shell=` value name (`cmd`-style), not the Rust variant name —
    /// read from clap's own possible values so the two cannot drift.
    fn shell_argument_name(shell: Shell) -> String {
        use clap::ValueEnum as _;
        shell
            .to_possible_value()
            .map_or_else(|| shell.to_string(), |value| value.get_name().to_string())
    }

    // One admission rule, shared with the reconciler's planner: an entry no arm
    // can emit *or* revert must not be emitted here either. Without it a
    // `type = "path"` value embedding the platform separator reached ksh, dash
    // and pwsh, whose split-based folds see it as two segments, match neither,
    // and prepend another copy on every re-source.
    ocx_lib::shell::is_emittable(entry).map_err(|reason| format!("skipping env var {:?} — {reason}", entry.key))?;

    let line = match entry.kind {
        ModifierKind::Path => shell.export_path(&entry.key, &entry.value),
        ModifierKind::Constant => shell.export_constant(&entry.key, &entry.value),
        // A surviving `None` separator has already been through compose-time
        // reconciliation, so nothing established one for this key.
        ModifierKind::List => shell.export_list(
            &entry.key,
            &entry.value,
            entry.separator.as_deref().unwrap_or(DEFAULT_SEPARATOR),
        ),
    };
    // The `--shell=` spelling, not the Rust variant name: this is the word the
    // reader typed and would type again.
    line.ok_or_else(|| {
        format!(
            "skipping list env var {:?} — {} has no case-sensitive unique append",
            entry.key,
            shell_argument_name(shell)
        )
    })
}

/// Resolve a `--shell` clap argument to an explicit [`Shell`], or `None`
/// when the default-format (JSON / `--format plain`) path should be taken.
///
/// `--shell` is declared as `Option<Option<Shell>>` with
/// `num_args=0..=1, require_equals=true` (clap 4.x produces `Some(None)` for
/// a bare `--shell`, `Some(Some(s))` for `--shell=NAME`, `None` when absent —
/// `require_equals` keeps a following positional from being swallowed):
///
/// - `None` (flag absent) → `Ok(None)`: caller uses the default-format path.
/// - `Some(None)` (bare `--shell`) → autodetect from `$SHELL`/parent; a
///   [`UsageError`] (exit 64) when undetectable.
/// - `Some(Some(s))` (explicit `--shell=NAME`) → `Ok(Some(s))`.
///
/// Shared by `ocx env` and `ocx package env` so the bare-shell autodetect and
/// its identical undetectable-shell `UsageError` exist exactly once.
pub fn resolve_shell_arg(shell: Option<Option<Shell>>) -> anyhow::Result<Option<Shell>> {
    match shell {
        None => Ok(None),
        Some(Some(s)) => Ok(Some(s)),
        Some(None) => {
            let s = Shell::detect().ok_or_else(|| {
                UsageError::new(
                    "could not autodetect shell from $SHELL or parent process; \
                     pass --shell=NAME explicitly. \
                     Legal values: bash, zsh, fish, ash, dash, ksh, sh, \
                     pwsh, elvish, nushell, batch (sh == dash POSIX alias)",
                )
            })?;
            Ok(Some(s))
        }
    }
}

/// Resolve a `--ci` clap argument to an explicit [`CiFlavor`], or `None` when
/// the flag is absent and the caller should take the non-CI path.
///
/// `--ci` is declared as `Option<Option<CiFlavor>>` with
/// `num_args=0..=1, require_equals=true` (mirroring `--shell`):
///
/// - `None` (flag absent) → `Ok(None)`: caller uses the structured-report /
///   `--shell` path.
/// - `Some(None)` (bare `--ci`) → autodetect from CI env vars
///   (`$GITHUB_ACTIONS`, `$GITLAB_CI`); a [`UsageError`] (exit 64) when no
///   provider is detected.
/// - `Some(Some(provider))` (explicit `--ci=NAME`) → `Ok(Some(provider))`.
///
/// Shared by `ocx env` and `ocx package env` so the bare-`--ci` autodetect and
/// its identical undetectable-provider `UsageError` exist exactly once.
pub fn resolve_ci_arg(ci: Option<Option<CiFlavor>>) -> anyhow::Result<Option<CiFlavor>> {
    match ci {
        None => Ok(None),
        Some(Some(provider)) => Ok(Some(provider)),
        Some(None) => {
            let provider = CiFlavor::detect()
                .ok_or_else(|| UsageError::new("could not autodetect CI provider; pass --ci=github or --ci=gitlab"))?;
            Ok(Some(provider))
        }
    }
}

/// Splits tag-list bytes on commas and newlines into trimmed, non-empty tag
/// names, preserving order.
///
/// Shared wire format for `ocx package announce --tags-from-file` (read) and
/// `ocx package push --announce-file` (write) — parses whatever either side
/// writes, and is byte-compatible with `indexbot --tags-from-file`.
pub fn parse_tags_file(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .split(['\n', '\r', ','])
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .map(str::to_string)
        .collect()
}

/// Appends `tags` onto `existing`, deduping (first occurrence wins) while
/// preserving order, and returns the comma-joined content to write back.
pub fn merge_tags_file(existing: &[String], tags: &[String]) -> String {
    let mut merged = existing.to_vec();
    for tag in tags {
        if !merged.contains(tag) {
            merged.push(tag.clone());
        }
    }
    merged.join(",")
}

/// Export resolved env entries into a CI system's persistence channel.
///
/// Shared by `ocx env` and `ocx package env`. Rejects `--export-file` for
/// GitHub Actions (which infers its two-file sink from `$GITHUB_ENV` /
/// `$GITHUB_PATH`); GitLab uses `export_file` as its output path, falling back
/// to stdout when `None`.
pub fn export_ci(provider: CiFlavor, export_file: Option<std::path::PathBuf>, entries: &[Entry]) -> anyhow::Result<()> {
    if provider == CiFlavor::GitHubActions && export_file.is_some() {
        return Err(UsageError::new(
            "--export-file is not supported with --ci=github; GitHub infers $GITHUB_ENV/$GITHUB_PATH",
        )
        .into());
    }
    provider.export(entries, export_file)?;
    Ok(())
}

/// Project composed env entries into the inspect report's wire shape.
///
/// Shared by `ocx package inspect` (where the entries are the `--env`
/// overrides alone) and `ocx inspect` (where they are `[env]`, the selected
/// groups' `[group.<name>.env]`, then `--env`, already in application order).
/// The report keeps entries in that order rather than merging them, so a
/// consumer sees every contributing declaration instead of a collapsed result;
/// `ocx env` is what answers "what is the final value".
pub fn env_entries(entries: &[Entry]) -> Vec<crate::api::data::env::EnvEntry> {
    entries
        .iter()
        .map(|entry| crate::api::data::env::EnvEntry {
            key: entry.key.clone(),
            value: entry.value.clone(),
            kind: entry.kind.clone(),
            separator: entry.separator.clone(),
            // Patch provenance is an `ocx env` concern — the inspect report
            // carries no package-composed entries to attribute.
            source: None,
        })
        .collect()
}

/// Exit code for an inspect run: 65 when the closure walk found a conflict.
///
/// Shared by both inspect commands. `DataError` is the code compose already
/// returns for the identical condition (`DependencyError::Conflict`,
/// `PackageErrorKind::EntrypointCollision`), so `ocx inspect --closure` exits
/// exactly where `ocx exec` over the same set would. The conflict detail stays
/// in the payload — the exit code is the machine-readable half, not a
/// replacement for it.
pub fn inspect_exit_code(report: &crate::api::data::package_inspect::InspectReport) -> std::process::ExitCode {
    if report.has_conflicts() {
        ocx_lib::cli::ExitCode::DataError.into()
    } else {
        std::process::ExitCode::SUCCESS
    }
}

/// Exit code for `ocx package cascade check`: 65 when any package reported a
/// finding.
///
/// `DataError` is the same code every other "the run succeeded, and what it
/// found is not clean" verdict uses ([`inspect_exit_code`]), so a job that
/// branches on 65 need not know which ocx command produced it. Index staleness
/// counts: check's contract is whole-graph consistency, and the report carries
/// the finding class a script needs to tell it from registry drift.
///
/// Returns the typed code rather than [`std::process::ExitCode`] (which
/// [`inspect_exit_code`] above does): the process type is opaque and compares
/// against nothing, so the verdict would only be assertable by running the
/// binary. The caller converts.
pub fn cascade_check_exit_code(
    report: &crate::api::data::package_cascade_check::PackageCascadeCheck,
) -> ocx_lib::cli::ExitCode {
    if report.reports.iter().any(|report| report.has_findings()) {
        ocx_lib::cli::ExitCode::DataError
    } else {
        ocx_lib::cli::ExitCode::Success
    }
}

/// Exit code for `ocx package cascade repair`: 65 when a finding survived the
/// run.
///
/// Deliberately not [`cascade_check_exit_code`]'s question. Index staleness is
/// the expected residue of a repair — closing it is `ocx package announce`'s
/// hop, and failing on it would make every healthy logical repair look broken.
/// What counts is work this run could have done and did not: a write the
/// registry rejected, an alias refused before it was attempted, and one the
/// fold cannot rebuild without new content being published. A preview counts
/// its whole plan, because a preview writes nothing — a `--dry-run` exiting 0
/// while naming repairs would be useless as a gate.
pub fn cascade_repair_exit_code(
    report: &crate::api::data::package_cascade_repair::PackageCascadeRepair,
) -> ocx_lib::cli::ExitCode {
    let remains = report.entries.iter().any(|entry| {
        !entry.report.unrepairable.is_empty()
            || entry
                .outcomes
                .iter()
                .any(|outcome| !matches!(outcome.outcome, WriteOutcome::Written { .. }))
            || (report.dry_run && !entry.planned.is_empty())
    });
    if remains {
        ocx_lib::cli::ExitCode::DataError
    } else {
        ocx_lib::cli::ExitCode::Success
    }
}

/// Return the manager for an install/pull command, refining the shared
/// auto-verify config's opt-out from this command's `--verify`/`--no-verify`
/// flag (the flag wins over `OCX_NO_VERIFY`).
///
/// Auto-verify itself is attached once on the shared manager in
/// [`Context::try_init`](crate::app::Context) so every install surface inherits
/// it; this only overrides the opt-out for the two commands that carry the
/// flag. A plain clone when no policy is configured (`auto_verify` is `None`).
pub fn manager_with_verify_flag(
    context: &crate::app::Context,
    verify: &crate::options::SignatureVerify,
) -> ocx_lib::package_manager::PackageManager {
    let manager = context.manager().clone();
    let Some(auto_verify) = manager.auto_verify().cloned() else {
        return manager;
    };
    // `OCX_NO_VERIFY` is resolved once in `Context::try_init` and read back from
    // the config view here — the `--verify`/`--no-verify` flag wins over it.
    let opted_out = !verify.resolve(context.config_view().no_verify);
    manager.with_auto_verify(Some(auto_verify.with_user_opted_out(opted_out)))
}

#[cfg(test)]
mod tests {
    use super::{
        emit_line, export_ci, infer_metadata_file, infer_receipt_file, merge_tags_file, parse_tags_file,
        resolve_ci_arg, resolve_receipt_path, resolve_shell_arg, resolved_lazy_mode,
    };
    use ocx_lib::ci::CiFlavor;
    use ocx_lib::cli::UsageError;
    use ocx_lib::lazy::LazyMode;
    use ocx_lib::package::metadata::env::{entry::Entry, modifier::ModifierKind};
    use ocx_lib::publisher::LayerRef;
    use ocx_lib::shell::Shell;

    // ── `--self` refuses a contradiction, not a co-occurrence ──────────────
    //
    // Both rows are independent of `OCX_LAZY_MODE`: the refusal returns before
    // the ladder is built, and an explicit CLI tier outranks the environment
    // one anyway. So neither asserts against ambient process state.

    /// F-8: the one combination that is genuinely contradictory.
    #[test]
    fn self_view_refuses_an_explicitly_typed_lazy_mode_always() {
        let error = resolved_lazy_mode(Some(LazyMode::Always), true)
            .expect_err("--self with an explicit --lazy-mode always is a usage error");
        let message = error.to_string();
        assert!(
            message.contains("contradictory"),
            "the message must say the two REQUESTS contradict, not that the flags cannot co-occur: {message}"
        );
    }

    /// The over-refusal a clap `conflicts_with` produced: `--self` composes
    /// eagerly and `--lazy-mode never` asks for eager, so they agree. Rejecting
    /// this is a false statement about the grammar.
    #[test]
    fn self_view_accepts_an_explicitly_typed_lazy_mode_never() {
        assert_eq!(
            resolved_lazy_mode(Some(LazyMode::Never), true).expect("--self and --lazy-mode never agree"),
            LazyMode::Never
        );
    }

    /// Without `--self`, an explicit `always` is exactly what the flag is for.
    ///
    /// Host-gated, with its Windows half below rather than a `cfg!(windows)`
    /// expectation inside one row: an assertion that restates the production
    /// `cfg!` agrees with the code on every host, including one where the code
    /// is wrong (the convention `ocx_lib::lazy` establishes for the floor).
    #[cfg(not(windows))]
    #[test]
    fn lazy_mode_always_survives_when_the_self_view_is_not_selected() {
        assert_eq!(
            resolved_lazy_mode(Some(LazyMode::Always), false).expect("no --self, no contradiction"),
            LazyMode::Always
        );
    }

    /// The Windows half of the row above. Nothing composes lazily there this
    /// phase (S-010: the `.shimref` reader ships, the producer does not), and
    /// that floor is applied by the ladder before `--self` is even consulted —
    /// so an explicit `always` composes eagerly whether or not `--self` was
    /// passed, and the sibling's `Always` is simply not a reachable answer.
    #[cfg(windows)]
    #[test]
    fn lazy_mode_always_composes_eagerly_on_windows_without_the_self_view() {
        assert_eq!(
            resolved_lazy_mode(Some(LazyMode::Always), false).expect("no --self, no contradiction"),
            LazyMode::Never
        );
    }

    // ── sidecar derivation: the receipt is the metadata path's twin ────────

    fn layer(path: &str) -> LayerRef {
        path.parse().expect("layer ref parses")
    }

    #[test]
    fn the_receipt_path_is_the_metadata_paths_twin() {
        let bundle = std::path::Path::new("/build/pkg.tar.xz");
        assert_eq!(
            infer_metadata_file(bundle).expect("metadata path"),
            std::path::Path::new("/build/pkg-metadata.json")
        );
        assert_eq!(
            infer_receipt_file(bundle).expect("receipt path"),
            std::path::Path::new("/build/pkg-receipt.json"),
            "both sidecars must derive from the same stem so they land side by side"
        );
    }

    #[test]
    fn a_single_file_layer_resolves_its_receipt() {
        let layers = [layer("./out/cmake.tar.gz")];
        assert_eq!(
            resolve_receipt_path(&layers),
            Some(std::path::PathBuf::from("./out/cmake-receipt.json"))
        );
    }

    #[test]
    fn zero_file_layers_resolve_no_receipt() {
        // A config-only push carries no bundle, so there is nothing for a
        // receipt to sit beside — `--platform` becomes required instead.
        assert_eq!(resolve_receipt_path(&[]), None);
    }

    #[test]
    fn ambiguous_file_layers_resolve_no_receipt() {
        // Never an error: two bundles disagree about which receipt describes
        // the build, so the caller falls through to the explicit-platform row.
        let layers = [layer("./out/base.tar.gz"), layer("./out/tool.tar.gz")];
        assert_eq!(resolve_receipt_path(&layers), None);
    }

    #[test]
    fn repeated_layers_of_one_bundle_still_resolve_one_receipt() {
        let layers = [layer("./out/cmake.tar.gz"), layer("./out/cmake.tar.gz")];
        assert_eq!(
            resolve_receipt_path(&layers),
            Some(std::path::PathBuf::from("./out/cmake-receipt.json")),
            "deduping matches resolve_metadata_path — one bundle, one receipt"
        );
    }

    #[test]
    fn shell_arg_absent_is_default_format() {
        assert!(resolve_shell_arg(None).expect("absent is ok").is_none());
    }

    #[test]
    fn shell_arg_explicit_is_passed_through() {
        let resolved = resolve_shell_arg(Some(Some(Shell::Bash))).expect("explicit is ok");
        assert!(matches!(resolved, Some(Shell::Bash)));
    }

    #[test]
    fn ci_arg_absent_is_none() {
        assert!(resolve_ci_arg(None).expect("absent is ok").is_none());
    }

    #[test]
    fn ci_arg_explicit_is_passed_through() {
        // Both providers pass through deterministically (no env reads). The
        // bare-`--ci` autodetect branch reads real CI env vars and is exercised
        // by the acceptance suite, not here (cf. `resolve_shell_arg`).
        assert_eq!(
            resolve_ci_arg(Some(Some(CiFlavor::GitHubActions))).expect("explicit is ok"),
            Some(CiFlavor::GitHubActions)
        );
        assert_eq!(
            resolve_ci_arg(Some(Some(CiFlavor::GitLab))).expect("explicit is ok"),
            Some(CiFlavor::GitLab)
        );
    }

    #[test]
    fn export_ci_github_rejects_export_file() {
        let result = export_ci(
            CiFlavor::GitHubActions,
            Some(std::path::PathBuf::from("/tmp/whatever")),
            &[],
        );
        let error = result.expect_err("github + --export-file must be rejected");
        assert!(
            error.downcast_ref::<UsageError>().is_some(),
            "rejection must be a UsageError (exit 64), got: {error:#}"
        );
    }

    #[test]
    fn export_ci_gitlab_writes_json_lines_to_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let export = tmp.path().join("export.env");
        let entries = vec![Entry {
            key: "JAVA_HOME".to_string(),
            value: "/pkg/java".to_string(),
            kind: ModifierKind::Constant,
            separator: None,
        }];

        export_ci(CiFlavor::GitLab, Some(export.clone()), &entries).expect("gitlab export ok");

        let content = std::fs::read_to_string(&export).expect("read export");
        assert_eq!(content, "{\"name\":\"JAVA_HOME\",\"value\":\"/pkg/java\"}\n");
    }

    // ── announce tag-file wire format (design register C2) ──────────────────

    #[test]
    fn parse_tags_file_splits_on_commas_and_newlines_and_trims() {
        let tags = parse_tags_file(b"3.28.1,3.28,3\nlatest\r\n , 1.0.0 ,");
        assert_eq!(
            tags,
            vec![
                "3.28.1".to_string(),
                "3.28".to_string(),
                "3".to_string(),
                "latest".to_string(),
                "1.0.0".to_string(),
            ]
        );
    }

    #[test]
    fn parse_tags_file_is_empty_for_empty_input() {
        assert!(parse_tags_file(b"").is_empty());
    }

    #[test]
    fn merge_tags_file_pushes_the_pushed_tag_and_cascade() {
        let merged = merge_tags_file(&[], &["3.28.1".to_string(), "3.28".to_string(), "3".to_string()]);
        assert_eq!(merged, "3.28.1,3.28,3");
    }

    #[test]
    fn merge_tags_file_dedupes_overlapping_appends_preserving_order() {
        let existing = parse_tags_file(b"3.28.1,3.28,3,latest");
        let merged = merge_tags_file(&existing, &["3.28.2".to_string(), "latest".to_string()]);
        assert_eq!(merged, "3.28.1,3.28,3,latest,3.28.2");
    }

    // ── cascade exit codes ──────────────────────────────────────────────────
    //
    // The two commands answer different questions of the same graph, and the
    // pair that matters most is index staleness: `check` fails on it because
    // its contract is whole-graph consistency, `repair` does not because
    // closing it is `announce`'s hop. Both directions are asserted below so
    // neither code can quietly become the other's.

    mod cascade {
        use ocx_lib::cli::ExitCode;
        use ocx_lib::oci;
        use ocx_lib::package::cascade::apply::{RepairOutcome, WriteOutcome};
        use ocx_lib::package::cascade::graph::{
            AliasState, AliasTag, CascadeReport, IndexFinding, PlannedWrite, SlotRow, SlotStatus, Unrepairable,
        };
        use ocx_lib::package::version::Version;

        use crate::api::data::package_cascade_check::PackageCascadeCheck;
        use crate::api::data::package_cascade_repair::{PackageCascadeRepair, RepairEntry};
        use crate::conventions::{cascade_check_exit_code, cascade_repair_exit_code};

        fn version(text: &str) -> Version {
            Version::parse(text).expect("fixture version parses")
        }

        fn tag(text: &str) -> AliasTag {
            AliasTag::Version(version(text))
        }

        fn digest() -> oci::Digest {
            oci::Digest::try_from("sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc")
                .expect("fixture digest parses")
        }

        /// A second digest, so a staleness fixture's committed and live sides
        /// actually differ - one value on both would describe an index that
        /// agrees, which is the opposite of the case under test.
        fn other_digest() -> oci::Digest {
            oci::Digest::try_from("sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd")
                .expect("fixture digest parses")
        }

        fn report() -> CascadeReport {
            CascadeReport {
                identifier: oci::Identifier::parse("registry.test/acme/cmake").expect("fixture parses"),
                logical: None,
                aliases: [(tag("3.28"), AliasState::Present)].into_iter().collect(),
                rows: Vec::new(),
                index_findings: Vec::new(),
                ignored_tags: Vec::new(),
                unrepairable: Vec::new(),
            }
        }

        fn stale_row() -> SlotRow {
            SlotRow {
                tag: tag("3.28"),
                platform: oci::native::Platform {
                    os: oci::native::Os::Linux,
                    architecture: oci::native::Arch::Amd64,
                    variant: None,
                    features: None,
                    os_version: None,
                    os_features: None,
                },
                status: SlotStatus::Stale,
                observed: None,
                expected: None,
                source: None,
                observed_source: None,
            }
        }

        fn planned_write() -> PlannedWrite {
            PlannedWrite {
                tag: tag("3.28"),
                index: oci::ImageIndex {
                    schema_version: 2,
                    media_type: None,
                    manifests: Vec::new(),
                    artifact_type: None,
                    annotations: None,
                },
                observed_digest: None,
                referenced_digests: Vec::new(),
                reasons: Vec::new(),
            }
        }

        fn repair(entry: RepairEntry, dry_run: bool) -> PackageCascadeRepair {
            PackageCascadeRepair {
                entries: vec![entry],
                dry_run,
                announce_tags_path: None,
                index_layer_skipped: Vec::new(),
            }
        }

        fn entry(report: CascadeReport) -> RepairEntry {
            RepairEntry {
                report,
                planned: Vec::new(),
                outcomes: Vec::new(),
                announce_tags: Vec::new(),
            }
        }

        // ── check ───────────────────────────────────────────────────────────

        #[test]
        fn check_on_a_clean_graph_succeeds() {
            let check = PackageCascadeCheck::new(vec![report()]);
            assert_eq!(cascade_check_exit_code(&check), ExitCode::Success);
        }

        #[test]
        fn check_on_a_registry_finding_is_a_data_error() {
            let mut clean = report();
            clean.rows.push(stale_row());
            let check = PackageCascadeCheck::new(vec![clean]);

            assert_eq!(cascade_check_exit_code(&check), ExitCode::DataError);
        }

        #[test]
        fn check_on_index_staleness_alone_is_a_data_error() {
            let mut clean = report();
            clean.index_findings.push(IndexFinding::Stale {
                tag: tag("3.28"),
                committed: digest(),
                live: other_digest(),
            });
            let check = PackageCascadeCheck::new(vec![clean]);

            assert_eq!(
                cascade_check_exit_code(&check),
                ExitCode::DataError,
                "check's contract is the whole graph, index copy included"
            );
        }

        #[test]
        fn check_reports_a_finding_from_any_package_in_the_batch() {
            let mut broken = report();
            broken.rows.push(stale_row());
            let check = PackageCascadeCheck::new(vec![report(), broken]);

            assert_eq!(cascade_check_exit_code(&check), ExitCode::DataError);
        }

        // ── repair ──────────────────────────────────────────────────────────

        #[test]
        fn repair_that_wrote_everything_succeeds() {
            let mut written = entry(report());
            written.planned = vec![planned_write()];
            written.outcomes = vec![RepairOutcome {
                tag: tag("3.28"),
                outcome: WriteOutcome::Written {
                    digest: digest(),
                    verified: true,
                    dropped: Vec::new(),
                },
            }];

            assert_eq!(cascade_repair_exit_code(&repair(written, false)), ExitCode::Success);
        }

        #[test]
        fn repair_leaves_index_staleness_to_announce() {
            let mut stale_index = report();
            stale_index.index_findings.push(IndexFinding::Stale {
                tag: tag("3.28"),
                committed: digest(),
                live: other_digest(),
            });

            assert_eq!(
                cascade_repair_exit_code(&repair(entry(stale_index), false)),
                ExitCode::Success,
                "the index hop is announce's job, not a repair failure"
            );
        }

        #[test]
        fn repair_fails_on_a_rejected_write() {
            let mut failed = entry(report());
            failed.planned = vec![planned_write()];
            failed.outcomes = vec![RepairOutcome {
                tag: tag("3.28"),
                outcome: WriteOutcome::Failed {
                    message: "registry said no".to_string(),
                },
            }];

            assert_eq!(cascade_repair_exit_code(&repair(failed, false)), ExitCode::DataError);
        }

        #[test]
        fn repair_fails_on_a_refused_alias() {
            let mut refused = entry(report());
            refused.planned = vec![planned_write()];
            refused.outcomes = vec![RepairOutcome {
                tag: tag("3.28"),
                outcome: WriteOutcome::Refused(Unrepairable::ChildManifestMissing {
                    tag: tag("3.28"),
                    digest: digest().to_string(),
                }),
            }];

            assert_eq!(cascade_repair_exit_code(&repair(refused, false)), ExitCode::DataError);
        }

        #[test]
        fn repair_fails_on_something_no_write_can_fix() {
            let mut unrepairable = report();
            unrepairable
                .unrepairable
                .push(Unrepairable::WouldEmptyIndex { tag: tag("3.28") });

            assert_eq!(
                cascade_repair_exit_code(&repair(entry(unrepairable), false)),
                ExitCode::DataError,
                "an alias needing new content published is a finding that remains"
            );
        }

        #[test]
        fn a_preview_with_repairs_to_make_is_a_data_error() {
            let mut preview = entry(report());
            preview.planned = vec![planned_write()];

            assert_eq!(
                cascade_repair_exit_code(&repair(preview, true)),
                ExitCode::DataError,
                "a preview writes nothing, so everything it planned still needs doing"
            );
        }

        #[test]
        fn a_preview_with_nothing_to_do_succeeds() {
            assert_eq!(
                cascade_repair_exit_code(&repair(entry(report()), true)),
                ExitCode::Success
            );
        }
    }

    // ── one admission rule across both emit sites ─────────────────────────
    //
    // `emit_lines` is the shared path for `ocx env --shell`, `ocx package env
    // --shell` and `ocx direnv export`. It used to refuse only an invalid key
    // and a `list` under cmd, while the reconciler's planner additionally
    // refused three shapes no arm can emit *or* revert — so the same entry was
    // dropped on the prompt path and emitted on the export path.

    fn path_entry(key: &str, value: &str) -> Entry {
        Entry {
            key: key.to_string(),
            value: value.to_string(),
            kind: ModifierKind::Path,
            separator: None,
        }
    }

    #[test]
    fn a_path_value_embedding_the_separator_is_refused() {
        // Executed on ksh, dash and pwsh: their split-based folds see `/n/a:b`
        // as two segments, match neither against the whole operand, and prepend
        // another copy on every re-source — PATH grows without bound.
        let separator = ocx_lib::env::PATH_SEPARATOR;
        let entry = path_entry("OCXP", &format!("/n/a{separator}b"));
        let note = emit_line(Shell::Bash, &entry).expect_err("must be refused, not emitted");
        assert!(note.contains("path separator"), "{note}");
    }

    #[test]
    fn an_empty_or_line_broken_element_is_refused() {
        for value in ["", "/n/a\nb", "/n/a\rb"] {
            let entry = path_entry("OCXP", value);
            assert!(
                emit_line(Shell::Bash, &entry).is_err(),
                "value {value:?} can be emitted but never removed, so it must not be emitted"
            );
        }
    }

    #[test]
    fn an_ordinary_entry_still_emits() {
        // The negative rows above only mean something against a positive one:
        // without this, a helper that refused everything would pass them all.
        let line = emit_line(Shell::Bash, &path_entry("OCXP", "/opt/bin")).expect("a plain directory emits");
        assert!(line.contains("/opt/bin"), "{line}");
        let invalid = emit_line(Shell::Bash, &path_entry("2FOO", "/opt/bin"));
        assert!(invalid.is_err(), "an invalid key is still refused");
    }
}
