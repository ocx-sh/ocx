// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::error::{MetadataResolutionError, UsageError};
use ocx_oci::layer_ref::LayerRef;
use ocx_package::{cascade::apply::WriteOutcome, metadata::env::entry::Entry};
use ocx_package_manager::composer::lazy_mode_for_package;
use ocx_project::lazy::LazyMode;
use ocx_shell::{ci::CiFlavor, shell::Shell};

/// Derives a `<stem>-<suffix>.json` sidecar path beside an archive file.
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

/// Infers the metadata sidecar beside an archive (`package.tar.gz` -> `package-metadata.json`).
pub fn infer_metadata_file(content: &std::path::Path) -> Result<std::path::PathBuf, MetadataResolutionError> {
    sidecar_path(content, "metadata")
}

/// Infers the build-receipt sidecar beside an archive (`package.tar.gz` -> `package-receipt.json`).
pub fn infer_receipt_file(content: &std::path::Path) -> Result<std::path::PathBuf, MetadataResolutionError> {
    sidecar_path(content, "receipt")
}

/// Resolves the metadata path for `ocx package push` and `ocx package test`:
/// `explicit`, else the file layers' one sidecar.
///
/// Zero file layers is [`MetadataResolutionError::Required`],
/// several distinct candidates [`MetadataResolutionError::Ambiguous`].
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

/// Resolves the build-receipt path from the file layers alone; `--metadata` never redirects it.
///
/// `None` for zero or several candidates: no receipt is supported and only makes the flags required.
pub fn resolve_receipt_path(layers: &[LayerRef]) -> Option<std::path::PathBuf> {
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    for layer in layers {
        // An underivable path fails closed: no receipt makes `--platform` required.
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

/// Reads the compiled metadata sidecar `ocx package push` and `ocx package test` consume; a parse failure exits 65.
pub async fn read_published_metadata(path: &std::path::Path) -> anyhow::Result<ocx_package::metadata::Metadata> {
    use anyhow::Context as _;
    use ocx_util::prelude::*;

    ocx_package::metadata::Metadata::read_json(path).await.with_context(|| {
        format!(
            "reading package metadata from {}; `ocx package create -m <FILE> -p <PLATFORM>` \
                 compiles an authoring sidecar into this form",
            path.display()
        )
    })
}

/// Resolves `--platform`, defaulting to the host platform for every resolution command.
pub fn platform_or_default(platform: Option<ocx_oci::Platform>) -> ocx_oci::Platform {
    platform.unwrap_or_else(|| ocx_oci::Platform::current().unwrap_or_else(ocx_oci::Platform::any))
}

/// Resolves the OCI-tier `lazy-mode` ladder; under `--self` it is always [`LazyMode::Never`].
///
/// An `always` inherited from `OCX_LAZY_MODE` downgrades silently under `--self`.
///
/// # Errors
///
/// [`UsageError`] when `self_view` is set and the CLI tier explicitly typed [`LazyMode::Always`].
pub fn resolved_lazy_mode(cli: Option<LazyMode>, self_view: bool) -> Result<LazyMode, UsageError> {
    if self_view && cli == Some(LazyMode::Always) {
        return Err(UsageError::new(
            "--self and --lazy-mode always ask for contradictory things: a shim is a consumer-facing launcher, and --self selects the private view that bypasses launchers",
        ));
    }
    let resolved = lazy_mode_for_package(cli);
    if self_view && resolved == LazyMode::Always {
        log::debug!(
            "Composing eagerly: --self selects a package's private view, which bypasses the launchers a shim is made of."
        );
        return Ok(LazyMode::Never);
    }
    Ok(resolved)
}

/// Emit shell-sourceable export lines for env entries, noting on stderr each one the shell cannot express.
///
/// The reserved-`OCX_*` consent gate belongs at `PackageManager::resolve_env_with_attribution`, never here:
/// a copy here masks a regression there and leaves `ocx exec` exposed.
/// `is_valid_env_key` checks grammar only and is no consent gate.
pub fn emit_lines(shell: Shell, entries: &[Entry]) {
    for entry in entries {
        match emit_line(shell, entry) {
            Ok(line) => println!("{line}"),
            Err(note) => eprintln!("# ocx: {note}"),
        }
    }
}

/// The per-entry half of [`emit_lines`]: the statement to print on stdout, or the reason to note on stderr.
fn emit_line(shell: Shell, entry: &Entry) -> Result<String, String> {
    use ocx_package::metadata::env::list::DEFAULT_SEPARATOR;
    use ocx_package::metadata::env::modifier::ModifierKind;

    /// The `--shell=` spelling the user typed, read from clap so the two cannot drift.
    fn shell_argument_name(shell: Shell) -> String {
        use clap::ValueEnum as _;
        shell
            .to_possible_value()
            .map_or_else(|| shell.to_string(), |value| value.get_name().to_string())
    }

    // The reconciler's admission rule: an entry it cannot revert makes ksh, dash and pwsh
    // prepend a copy on every re-source.
    ocx_shell::shell::is_emittable(entry).map_err(|reason| format!("skipping env var {:?} — {reason}", entry.key))?;

    let line = match entry.kind {
        ModifierKind::Path => shell.export_path(&entry.key, &entry.value),
        ModifierKind::Constant => shell.export_constant(&entry.key, &entry.value),
        // A `None` surviving compose-time reconciliation means nothing established a separator for this key.
        ModifierKind::List => shell.export_list(
            &entry.key,
            &entry.value,
            entry.separator.as_deref().unwrap_or(DEFAULT_SEPARATOR),
        ),
    };
    line.ok_or_else(|| {
        format!(
            "skipping list env var {:?} — {} has no case-sensitive unique append",
            entry.key,
            shell_argument_name(shell)
        )
    })
}

/// Resolve a `--shell` argument: absent is `None`, bare autodetects, with a [`UsageError`] when undetectable.
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

/// Resolve a `--ci` argument: absent is `None`, bare autodetects, with a [`UsageError`] when no provider is detected.
pub fn resolve_ci_arg(ci: Option<Option<CiFlavor>>) -> anyhow::Result<Option<CiFlavor>> {
    resolve_ci_flavor(ci, "--ci")
}

/// [`resolve_ci_arg`] for `--ci-annotations`; its usage error must name the flag `ocx package push` actually has.
pub fn resolve_ci_annotations_arg(ci: Option<Option<CiFlavor>>) -> anyhow::Result<Option<CiFlavor>> {
    resolve_ci_flavor(ci, "--ci-annotations")
}

/// The shared body of the `--ci`-shaped resolvers; `flag` is named in the usage error.
fn resolve_ci_flavor(ci: Option<Option<CiFlavor>>, flag: &str) -> anyhow::Result<Option<CiFlavor>> {
    resolve_ci_flavor_with(ci, flag, CiFlavor::detect())
}

/// [`resolve_ci_flavor`] with the provider injected, so tests do not read the ambient CI the gate may run in.
fn resolve_ci_flavor_with(
    ci: Option<Option<CiFlavor>>,
    flag: &str,
    detected: Option<CiFlavor>,
) -> anyhow::Result<Option<CiFlavor>> {
    match ci {
        None => Ok(None),
        Some(Some(provider)) => Ok(Some(provider)),
        Some(None) => {
            let provider = detected.ok_or_else(|| undetectable_ci_provider(flag))?;
            Ok(Some(provider))
        }
    }
}

/// The usage error a bare `--ci`-shaped flag raises when no provider is detected.
///
/// The `=` clause is for a user who typed `--ci-annotations github`: the space form left the flag bare.
fn undetectable_ci_provider(flag: &str) -> UsageError {
    UsageError::new(format!(
        "could not autodetect CI provider; {}",
        value_needs_equals::<CiFlavor>(flag)
    ))
}

/// The one `=`-requirement clause every `require_equals` refusal states, listing `V`'s canonical spellings.
fn value_needs_equals<V: clap::ValueEnum>(flag: &str) -> String {
    let spellings: Vec<String> = V::value_variants()
        .iter()
        .filter_map(clap::ValueEnum::to_possible_value)
        .map(|value| format!("{flag}={}", value.get_name()))
        .collect();
    format!("pass {}; the value must be attached with `=`", spellings.join(" or "))
}

/// Refuses a `require_equals` flag whose value was written with a space, which parses as a bare flag plus a positional.
///
/// `flag_could_have_lost_its_value` is per grammar: `Some(None)` for `Option<Option<V>>`, the default
/// variant for `Option<V>` with `default_missing_value`.
pub fn refuse_spaced_enum_value<V: clap::ValueEnum>(
    flag: &str,
    flag_could_have_lost_its_value: bool,
    positionals: impl IntoIterator<Item = String>,
) -> Result<(), UsageError> {
    // ponytail: called only where positionals collide with the flag's vocabulary; `--shell` has no call
    // because `bash`, `zsh` and `fish` are real package names CI passes positionally.
    if !flag_could_have_lost_its_value {
        return Ok(());
    }
    // ponytail: case-insensitive on purpose (escape: `./gitlab`); matched case-sensitively, `--ci GitLab`
    // silently becomes a layer path on a real runner.
    let Some(token) = positionals.into_iter().find(|token| V::from_str(token, true).is_ok()) else {
        return Ok(());
    };
    Err(UsageError::new(format!(
        "{flag} was given `{token}` as a separate argument, so `{token}` parsed as a positional \
         argument and not as the flag's value; {}",
        value_needs_equals::<V>(flag)
    )))
}

/// Splits tag-list bytes on commas and newlines into trimmed, non-empty tag names, preserving order.
///
/// The `--tags-file` wire format, which the third-party `indexbot` also reads via `--tags-from-file`.
pub fn parse_tags_file(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .split(['\n', '\r', ','])
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .map(str::to_string)
        .collect()
}

/// Appends `tags` onto `existing`, deduping (first occurrence wins) while
/// preserving order, and returns the content to write back: one tag per line,
/// each newline-terminated, and empty when there are no tags.
pub fn merge_tags_file(existing: &[String], tags: &[String]) -> String {
    let mut merged = existing.to_vec();
    for tag in tags {
        if !merged.contains(tag) {
            merged.push(tag.clone());
        }
    }
    merged.iter().map(|tag| format!("{tag}\n")).collect()
}

/// Appends `tags` onto the tags-file at `path` (created if absent), deduping against what is
/// already there. Written even when there is nothing to add, so an unconditional
/// `announce --tags-file` finds a file.
///
/// The file is replaced by rename, so a reader never sees a half-written list.
pub async fn append_tags_file(path: &std::path::Path, tags: &[String]) -> anyhow::Result<()> {
    use anyhow::Context as _;

    // Bounded, not a bare `fs::read`, or `--tags-file /dev/zero` reads until memory runs out.
    let existing = crate::options::tags::read_tags_file_if_present(path).await?;
    let merged = merge_tags_file(&existing, tags);
    let target = path.to_path_buf();
    tokio::task::spawn_blocking(move || ocx_util::fs::write_bytes_atomic(&target, merged.as_bytes()))
        .await
        .context("tags file writer panicked")?
        .map_err(|error| ocx_util::error::FileError::new(path, error))
        .with_context(|| format!("writing tags file {}", path.display()))
}

/// Export resolved env entries into a CI system's persistence channel; `--export-file` is refused for GitHub.
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

/// Project composed env entries into the inspect report's wire shape, unmerged and in application order.
pub fn env_entries(entries: &[Entry]) -> Vec<crate::api::data::env::EnvEntry> {
    entries
        .iter()
        .map(|entry| crate::api::data::env::EnvEntry {
            key: entry.key.clone(),
            value: entry.value.clone(),
            kind: entry.kind.clone(),
            separator: entry.separator.clone(),
            source: None,
        })
        .collect()
}

/// Exit code for an inspect run: 65 on a closure conflict, where `ocx exec` over the same set exits too.
pub fn inspect_exit_code(report: &crate::api::data::package_inspect::InspectReport) -> std::process::ExitCode {
    if report.has_conflicts() {
        ocx_exit::ExitCode::DataError.into()
    } else {
        std::process::ExitCode::SUCCESS
    }
}

/// Exit code for `ocx package cascade check`: 65 when any package reported a finding, index staleness included.
///
/// Typed, not [`std::process::ExitCode`], which compares against nothing and so could not be asserted.
pub fn cascade_check_exit_code(
    report: &crate::api::data::package_cascade_check::PackageCascadeCheck,
) -> ocx_exit::ExitCode {
    if report.reports.iter().any(|report| report.has_findings()) {
        ocx_exit::ExitCode::DataError
    } else {
        ocx_exit::ExitCode::Success
    }
}

/// Exit code for `ocx package cascade repair`:
/// 65 when work this run could have done remains, or `--dry-run` planned any.
///
/// Index staleness does not count, or every healthy repair looks broken: `ocx package announce` closes it.
pub fn cascade_repair_exit_code(
    report: &crate::api::data::package_cascade_repair::PackageCascadeRepair,
) -> ocx_exit::ExitCode {
    let remains = report.entries.iter().any(|entry| {
        !entry.report.unrepairable.is_empty()
            || entry
                .outcomes
                .iter()
                .any(|outcome| !matches!(outcome.outcome, WriteOutcome::Written { .. }))
            || (report.dry_run && !entry.planned.is_empty())
    });
    if remains {
        ocx_exit::ExitCode::DataError
    } else {
        ocx_exit::ExitCode::Success
    }
}

/// Return the manager with the auto-verify opt-out refined by `--verify`/`--no-verify`, which outranks `OCX_NO_VERIFY`.
pub fn manager_with_verify_flag(
    context: &crate::app::Context,
    verify: &crate::options::SignatureVerify,
) -> ocx_package_manager::PackageManager {
    let manager = context.manager().clone();
    let Some(auto_verify) = manager.auto_verify().cloned() else {
        return manager;
    };
    let opted_out = !verify.resolve(context.config_view().no_verify);
    manager.with_auto_verify(Some(auto_verify.with_user_opted_out(opted_out)))
}

/// The "not eval-safe" advisory for a non-JSON report on a non-terminal stdout, where `eval "$(ocx env)"` breaks.
///
/// Never probe via `ColorModeConfig::stdout`: `CLICOLOR_FORCE` and `NO_COLOR` would invert the answer.
/// The tty probe is a parameter because a test's stdout is never a terminal.
#[must_use]
pub const fn not_eval_safe_advisory(is_json: bool, stdout_is_terminal: bool) -> Option<&'static str> {
    if is_json || stdout_is_terminal {
        return None;
    }
    Some("default output is not eval-safe; use --shell=bash to activate")
}

#[cfg(test)]
mod tests {
    use super::{
        append_tags_file, emit_line, export_ci, infer_metadata_file, infer_receipt_file, merge_tags_file,
        not_eval_safe_advisory, parse_tags_file, resolve_ci_arg, resolve_receipt_path, resolve_shell_arg,
        resolved_lazy_mode,
    };
    use crate::error::UsageError;
    use ocx_oci::layer_ref::LayerRef;
    use ocx_package::metadata::env::{entry::Entry, modifier::ModifierKind};
    use ocx_project::lazy::LazyMode;
    use ocx_shell::ci::CiFlavor;
    use ocx_shell::shell::Shell;

    // ── The "not eval-safe" advisory fires only where the footgun is ─────────

    /// The whole point of the change: a human reading the table on a terminal
    /// is not about to `eval` it, and warning them fires on the single most
    /// common benign invocation of the command.
    #[test]
    fn a_terminal_read_is_silent() {
        assert_eq!(
            not_eval_safe_advisory(false, true),
            None,
            "plain output to a terminal is a human reading it, and must not warn"
        );
    }

    /// And the case the warning exists for: stdout that is piped, captured or
    /// command-substituted is the one about to be `eval`ed.
    #[test]
    fn a_captured_read_still_warns() {
        assert_eq!(
            not_eval_safe_advisory(false, false),
            Some("default output is not eval-safe; use --shell=bash to activate"),
            "a non-terminal stdout is the `eval \"$(ocx env)\"` case and must still warn"
        );
    }

    /// JSON is already a machine channel that says so structurally — silent in
    /// both directions, exactly as before this predicate existed.
    #[test]
    fn json_is_silent_on_either_stream() {
        assert_eq!(
            not_eval_safe_advisory(true, true),
            None,
            "JSON to a terminal stays silent"
        );
        assert_eq!(not_eval_safe_advisory(true, false), None, "JSON to a pipe stays silent");
    }

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
    /// **One row, on every host.** This used to be a host-gated pair, because
    /// `LazyModeLadder::resolve_for_host` forced `LazyMode::Never` on Windows
    /// while nothing there could write a deferred tool's shim slot. C-026 ships
    /// that producer and C-027 removed the floor, so `always` is `always`
    /// everywhere and a Windows half asserting `Never` would only re-state a
    /// removed rule.
    #[test]
    fn lazy_mode_always_survives_when_the_self_view_is_not_selected() {
        assert_eq!(
            resolved_lazy_mode(Some(LazyMode::Always), false).expect("no --self, no contradiction"),
            LazyMode::Always
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

    /// A bare `--ci`/`--ci-annotations` with no detectable provider must reach
    /// the real resolver's autodetect-failure arm and surface the `=` requirement
    /// naming the caller's own flag. Driven through `resolve_ci_flavor_with` with
    /// `detected = None` injected, so the arm is reached deterministically without
    /// the ambient CI env `CiFlavor::detect` would otherwise read (which would pass
    /// vacuously inside CI). Both callers are covered — the message must name
    /// whichever flag the user actually typed, not a hardcoded one.
    #[test]
    fn a_bare_ci_flag_with_no_detected_provider_names_the_equals_requirement() {
        for flag in ["--ci", "--ci-annotations"] {
            let error = super::resolve_ci_flavor_with(Some(None), flag, None)
                .expect_err("an undetectable provider must be a usage error");
            assert!(
                error.downcast_ref::<UsageError>().is_some(),
                "the failure must be a UsageError (exit 64), got: {error:#}"
            );
            let message = error.to_string();
            assert!(
                message.contains("attached with `=`"),
                "the message must name the `=` requirement: {message}"
            );
            assert!(
                message.contains(&format!("{flag}=github")),
                "the message must show the correct spelling for the caller's flag: {message}"
            );
        }
    }

    /// A provider supplied explicitly (`--ci=github`) never consults autodetect,
    /// so it resolves regardless of the injected `detected` value.
    #[test]
    fn an_explicit_provider_resolves_without_autodetect() {
        let resolved = super::resolve_ci_flavor_with(Some(Some(CiFlavor::GitHubActions)), "--ci", None)
            .expect("an explicit provider must resolve");
        assert_eq!(resolved, Some(CiFlavor::GitHubActions));
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

    #[tokio::test]
    async fn append_tags_file_creates_merges_and_dedupes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("announce.txt");
        let tags = |names: &[&str]| names.iter().map(ToString::to_string).collect::<Vec<_>>();

        append_tags_file(&path, &tags(&["3.28.1", "3.28", "3"]))
            .await
            .expect("first append succeeds");
        append_tags_file(&path, &tags(&["3.28.2", "3.28"]))
            .await
            .expect("second append succeeds");

        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "3.28.1\n3.28\n3\n3.28.2\n"
        );
    }

    #[tokio::test]
    async fn append_tags_file_writes_an_empty_file_and_leaves_no_temp_file_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("announce.txt");

        append_tags_file(&path, &[]).await.expect("an empty write succeeds");

        assert_eq!(std::fs::read_to_string(&path).expect("read"), "");
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read dir")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(names, [std::ffi::OsString::from("announce.txt")]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn append_tags_file_writes_owner_only_mode() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("announce.txt");

        append_tags_file(&path, &["3.28".to_string()])
            .await
            .expect("append succeeds");

        let mode = std::fs::metadata(&path).expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

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
        assert_eq!(merged, "3.28.1\n3.28\n3\n");
    }

    #[test]
    fn merge_tags_file_dedupes_overlapping_appends_preserving_order() {
        let existing = parse_tags_file(b"3.28.1,3.28,3,latest");
        let merged = merge_tags_file(&existing, &["3.28.2".to_string(), "latest".to_string()]);
        assert_eq!(merged, "3.28.1\n3.28\n3\nlatest\n3.28.2\n");
    }

    #[test]
    fn merge_tags_file_of_nothing_is_an_empty_file() {
        assert_eq!(merge_tags_file(&[], &[]), "");
    }

    #[test]
    fn merge_tags_file_reads_an_old_comma_file_and_rewrites_it_as_lines() {
        let existing = parse_tags_file(b"3.28.1,3.28,3");
        assert_eq!(existing, ["3.28.1", "3.28", "3"]);
        let merged = merge_tags_file(&existing, &[]);
        assert_eq!(merged, "3.28.1\n3.28\n3\n");
        assert_eq!(parse_tags_file(merged.as_bytes()), existing);
    }

    // ── cascade exit codes ──────────────────────────────────────────────────
    //
    // The two commands answer different questions of the same graph, and the
    // pair that matters most is index staleness: `check` fails on it because
    // its contract is whole-graph consistency, `repair` does not because
    // closing it is `announce`'s hop. Both directions are asserted below so
    // neither code can quietly become the other's.

    mod cascade {
        use ocx_exit::ExitCode;

        use ocx_package::cascade::apply::{RepairOutcome, WriteOutcome};
        use ocx_package::cascade::graph::{
            AliasState, AliasTag, CascadeReport, IndexFinding, PlannedWrite, SlotRow, SlotStatus, Unrepairable,
        };
        use ocx_package::version::Version;

        use crate::api::data::package_cascade_check::PackageCascadeCheck;
        use crate::api::data::package_cascade_repair::{PackageCascadeRepair, RepairEntry};
        use crate::conventions::{cascade_check_exit_code, cascade_repair_exit_code};

        fn version(text: &str) -> Version {
            Version::parse(text).expect("fixture version parses")
        }

        fn tag(text: &str) -> AliasTag {
            AliasTag::Version(version(text))
        }

        fn digest() -> ocx_oci::Digest {
            ocx_oci::Digest::try_from("sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc")
                .expect("fixture digest parses")
        }

        /// A second digest, so a staleness fixture's committed and live sides
        /// actually differ - one value on both would describe an index that
        /// agrees, which is the opposite of the case under test.
        fn other_digest() -> ocx_oci::Digest {
            ocx_oci::Digest::try_from("sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd")
                .expect("fixture digest parses")
        }

        fn report() -> CascadeReport {
            CascadeReport {
                identifier: ocx_oci::OciIdentifier::parse_target("registry.test/acme/cmake", ocx_oci::DEFAULT_REGISTRY)
                    .expect("fixture parses"),
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
                platform: ocx_oci::native::Platform {
                    os: ocx_oci::native::Os::Linux,
                    architecture: ocx_oci::native::Arch::Amd64,
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
                index: ocx_oci::ImageIndex {
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
                tags_file: None,
                index_layer_skipped: Vec::new(),
            }
        }

        fn entry(report: CascadeReport) -> RepairEntry {
            RepairEntry {
                report,
                planned: Vec::new(),
                outcomes: Vec::new(),
                tags: Vec::new(),
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
        let separator = ocx_util::env::PATH_SEPARATOR;
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
