// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Publishing a record into the operator's sink directory.
//!
//! The sink is a **directory**, never a file, and never inside `$OCX_HOME`.
//! Records are output the operator collects, not ocx runtime state, and
//! `$OCX_HOME` is per-user while the audit trail is fleet-wide. A directory sink
//! is also the only shape that is correct on every filesystem: no portable,
//! spec-backed guarantee exists for concurrent append anywhere — POSIX promises
//! non-interleaving only for pipes, NFS has no append opcode at all, and the
//! Windows CRT's emulation does not inherit the native atomic-append guarantee.
//!
//! **Publication must not clobber.** A plain tempfile-then-rename publish is
//! atomic but *replacing*: a record can appear atomically on top of another
//! record, which is a worse failure than an interleaved append — it is silent
//! and total. Two containers on one host sharing an NFS sink each have their own
//! PID namespace, so both can be PID 1 in the same millisecond, and a template
//! carrying only `{time}` collides across every host on a shared mount.
//!
//! So the publish is no-clobber and a collision retries under a freshly drawn
//! random component. On the hardlink fallback path — which is exactly the NFS
//! path — a lost reply can produce a spurious collision report even though the
//! link landed, so the failure mode is a byte-identical duplicate, never a loss.
//! Duplicates join on the same digest; loss would not be recoverable.
//!
//! One consequence worth stating for whoever writes the collector: a crash
//! between link and unlink strands a temp file in the sink permanently. A
//! collector must ignore `.tmp*`.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use tempfile::NamedTempFile;

use super::error::RecordsError;
use super::execution_record::ExecutionRecord;
use super::name_template::{NameContext, NameTemplate, random_component};
use super::policy::RecordingPolicy;
use crate::utility::fs::{PersistOutcome, persist_temp_file_noclobber};

/// How many names one record may try before giving up.
///
/// Every retry past the first draws 32 fresh bits, so exhausting this means
/// losing the same coin toss eight times running. The bound exists only so a
/// sink that is somehow permanently occupied fails loudly instead of spinning.
const MAX_PUBLISH_ATTEMPTS: usize = 8;

/// Write `record` into the policy's sink, returning the published path.
///
/// `Ok(None)` when recording is configured off — the common case, and not a
/// failure.
///
/// Async because publication is blocking filesystem work on a path that is
/// already inside an async task; the blocking half belongs on a blocking pool,
/// not on the runtime's worker thread.
///
/// # Errors
///
/// Returns [`RecordsError`] when the record cannot be serialized or published.
/// Whether the caller aborts on that is [`RecordingPolicy::required`]'s call,
/// not this function's.
pub async fn emit(record: &ExecutionRecord, policy: &RecordingPolicy) -> Result<Option<PathBuf>, RecordsError> {
    let Some(dir) = policy.dir() else {
        return Ok(None);
    };

    // Checked here, on every platform, and not only in the pre-spawn probe: that
    // probe is Windows-only on the `exec` path — Unix writes the record before
    // `execvp`, so the write is its own gate — which left the refusal inert on
    // Linux and macOS, where a sink swapped for a symlink would silently
    // relocate the whole audit trail.
    refuse_substituted_sink(dir).await?;

    let json = record.to_json()?;
    // Drawn from the record itself so the filename and the payload can never
    // disagree about when it was written or which process ran the tool.
    let context = NameContext {
        recorded_at: record.recorded_at,
        pid: record.process.pid,
        host: record.host.name.clone(),
    };

    let dir = dir.to_path_buf();
    let sink = dir.clone();
    let template = policy.name().clone();

    tokio::task::spawn_blocking(move || {
        publish(
            &dir,
            |attempt| candidate_name(&template, &context, attempt),
            json.as_bytes(),
        )
    })
    .await
    .map_err(|join_err| RecordsError::Io {
        path: sink,
        source: std::io::Error::other(join_err),
    })?
    .map(Some)
}

/// Check that `dir` exists and is writable **before** the child starts.
///
/// This is what keeps the fail-closed posture honest on the platform where the
/// record can only be written after the spawn: the probe refuses an unwritable
/// sink while nothing has run yet, and it costs one create/unlink rather than
/// reaching for platform-specific process APIs to learn a pid early.
///
/// # Errors
///
/// - [`RecordsError::SinkSymlink`] — `dir` no longer resolves to the directory
///   it was designated as, which would let whoever swapped it redirect an audit
///   trail somewhere the operator never designated.
/// - [`RecordsError::Io`] — `dir` is absent, or not writable.
pub async fn probe_writable(dir: &Path) -> Result<(), RecordsError> {
    // The same refusal [`emit`] applies, run here too so the platform whose gate
    // is *only* pre-spawn keeps both halves of it: on Windows the record is
    // written after the child exists, and a substituted sink caught only at emit
    // would mean the tool had already started.
    refuse_substituted_sink(dir).await?;

    let dir = dir.to_path_buf();
    let sink = dir.clone();
    tokio::task::spawn_blocking(move || {
        // Create and immediately unlink: the one probe that answers "can this
        // process put a file here" without trusting permission bits, which are
        // advisory for root and meaningless on a read-only mount.
        NamedTempFile::new_in(&dir)
            .map(drop)
            .map_err(|source| RecordsError::Io { path: dir, source })
    })
    .await
    .map_err(|join_err| RecordsError::Io {
        path: sink,
        source: std::io::Error::other(join_err),
    })?
}

/// Refuse a sink that is no longer the directory it was designated as.
///
/// [`super::policy::resolve_records`] resolves the operator's configured sink to
/// its real location once and pins the result, so a pinned path re-resolves to
/// itself for as long as nobody moves the directory. A disagreement here means a
/// symlink has been inserted into the pinned path since designation — which would
/// otherwise redirect an audit trail somewhere the operator never designated,
/// with no error and no missing record at the source, so the collector simply
/// sees an empty sink.
///
/// **What this does not catch**, because it compares a canonical *path* rather
/// than holding the directory's identity: replacing the sink with a *different
/// real directory* at the same path re-resolves to itself and is accepted; and a
/// symlink inserted between this check and [`NamedTempFile::new_in`] is followed,
/// because the file is created by path, not through a handle pinned here. Closing
/// either needs an `openat`-style directory handle opened at designation and every
/// write made relative to it. This is an integrity control against a script or a
/// rotator redirecting the trail by accident, not a defence against an adversary
/// racing it — consistent with the rest of `[records]`, where anyone able to plant
/// that symlink can equally just not run `ocx`.
///
/// Deliberately *not* a symlink-in-ancestor refusal: every path picks up OS
/// aliases (macOS reaches `/var/log` through `/var` → `/private/var`), so that
/// guard refused ordinary hosts — permanently, under `required = true`.
///
/// # Errors
///
/// - [`RecordsError::SinkSymlink`] — `dir` resolves somewhere other than itself.
/// - [`RecordsError::Io`] — `dir` could not be resolved at all.
async fn refuse_substituted_sink(dir: &Path) -> Result<(), RecordsError> {
    let pinned = dir.to_path_buf();
    let resolved = tokio::task::spawn_blocking({
        let pinned = pinned.clone();
        move || dunce::canonicalize(&pinned).map_err(|source| RecordsError::Io { path: pinned, source })
    })
    .await
    .map_err(|join_err| RecordsError::Io {
        path: pinned.clone(),
        source: std::io::Error::other(join_err),
    })??;

    if resolved == pinned {
        Ok(())
    } else {
        Err(RecordsError::SinkSymlink { path: pinned })
    }
}

/// Write `bytes` into `dir` under the first name `name_for` offers that is free.
///
/// Blocking. The retry is the whole point: a taken name never overwrites, it
/// draws another. `name_for` receives the attempt index so it can vary a
/// template that carries no randomness of its own.
fn publish(dir: &Path, mut name_for: impl FnMut(usize) -> String, bytes: &[u8]) -> Result<PathBuf, RecordsError> {
    let io_error = |path: &Path, source: std::io::Error| RecordsError::Io {
        path: path.to_path_buf(),
        source,
    };

    let mut tmp = NamedTempFile::new_in(dir).map_err(|e| io_error(dir, e))?;
    tmp.write_all(bytes).map_err(|e| io_error(dir, e))?;
    // Deliberately no `sync_data`. What this path promises is the ADR's own
    // guarantee — the whole record appears under its name atomically — and that
    // comes from the no-clobber publish, not from a flush. An `fsync` would add
    // only durability across host power loss, the one failure that also destroys
    // the job the record describes; measured on ext4 it cost 71 ms median and
    // 518 ms p99 against 43 µs / 290 µs without, on the critical path of every
    // tool invocation in a build. Should a deployment ever need a record to
    // outlive a power cut, `tmp.as_file().sync_data()` here is the whole change.

    for attempt in 0..MAX_PUBLISH_ATTEMPTS {
        let target = resolve_in_sink(dir, &name_for(attempt))?;
        match persist_temp_file_noclobber(tmp, &target).map_err(|e| io_error(&target, e))? {
            PersistOutcome::Published => return Ok(target),
            PersistOutcome::Occupied(returned) => {
                crate::log::debug!(
                    "execution record name '{}' is taken; retrying under a fresh name",
                    target.display()
                );
                tmp = returned;
            }
        }
    }

    Err(RecordsError::Io {
        path: dir.to_path_buf(),
        source: std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("every one of {MAX_PUBLISH_ATTEMPTS} candidate record names was taken"),
        ),
    })
}

/// Place one rendered name inside the sink, refusing anything that is not a
/// plain filename.
///
/// Every candidate name passes through here, which is what makes this the
/// backstop to [`NameTemplate`]'s own `{host}` sanitizer: a separator or a `..`
/// — from a template literal, from a hostname, or from some future placeholder
/// drawn from the environment — would put the record outside the directory the
/// operator designated. `Component::Normal` is checked against the name verbatim
/// so nothing is silently normalized into looking well-formed.
///
/// # Errors
///
/// Returns [`RecordsError::NameNotAFilename`] when `name` is not exactly one
/// normal path component.
fn resolve_in_sink(dir: &Path, name: &str) -> Result<PathBuf, RecordsError> {
    let mut components = Path::new(name).components();
    match (components.next(), components.next()) {
        (Some(std::path::Component::Normal(single)), None) if single == std::ffi::OsStr::new(name) => {
            Ok(dir.join(single))
        }
        _ => Err(RecordsError::NameNotAFilename { name: name.to_string() }),
    }
}

/// The filename for one publish attempt.
///
/// Attempt 0 is the operator's template verbatim. A retry must not re-offer the
/// name that just collided, so a template carrying `{rand}` is simply re-rendered
/// — it draws afresh every call — while one without gets a random component
/// appended.
fn candidate_name(template: &NameTemplate, context: &NameContext, attempt: usize) -> String {
    let name = template.render(context);
    if attempt == 0 || template.has_random() {
        name
    } else {
        with_random_component(&name)
    }
}

/// Insert a random component before the extension — `run.json` →
/// `run-3f7a1c08.json` — so a retried record still matches whatever glob the
/// collector was configured with.
fn with_random_component(name: &str) -> String {
    let component = random_component();
    match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => format!("{stem}-{component}.{extension}"),
        _ => format!("{name}-{component}"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{DateTime, Utc};

    use super::*;
    use crate::env::OcxConfigView;
    use crate::file_structure::PackageDir;
    use crate::oci::{Digest, Identifier, PinnedIdentifier};
    use crate::package::install_info::InstallInfo;
    use crate::package::resolved_package::ResolvedPackage;
    use crate::package_manager::tasks::resolve::AdmittedClaims;
    use crate::record::execution_record::{RecordInputs, Scope};
    use crate::record::{RecordsOptions, resolve_records};

    const HEX: &str = "3f7a2b9c5d1e8f04a6b3c7d2e9f1a5b8c4d6e0f2a3b7c9d1e5f8a0b2c4d6e8f0";

    /// The names a fixed template produces. Kept in the test rather than reaching
    /// for `NameTemplate::parse` so this file's own retry logic is what is under
    /// test — a real template's name carries milliseconds and a pid, so no test
    /// could predict, let alone pre-create, the path a record is about to take.
    fn pinned(fixed: &str) -> impl FnMut(usize) -> String + use<'_> {
        move |attempt| {
            if attempt == 0 {
                fixed.to_string()
            } else {
                with_random_component(fixed)
            }
        }
    }

    fn record_files(dir: &Path) -> Vec<PathBuf> {
        let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                // A collector ignores `.tmp*`; so does this. See the module doc.
                !path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(".tmp"))
            })
            .collect();
        found.sort();
        found
    }

    /// The defect D11 names: a record must never land on top of another one.
    /// Cross-container pid reuse makes this reachable in production — two
    /// containers on one host each have their own pid namespace, so both can be
    /// pid 1 in the same millisecond on a shared sink. Forcing the name is the
    /// only way to reproduce it, since a real name carries milliseconds and a pid
    /// that does not exist before the spawn.
    #[test]
    fn a_taken_name_never_replaces_the_record_already_there() {
        let dir = tempfile::tempdir().unwrap();
        let occupant = dir.path().join("pinned.json");
        std::fs::write(&occupant, b"{\"first\":true}").unwrap();

        let published = publish(dir.path(), pinned("pinned.json"), b"{\"second\":true}").unwrap();

        assert_ne!(published, occupant, "the retry must pick a different name");
        assert_eq!(
            std::fs::read(&occupant).unwrap(),
            b"{\"first\":true}",
            "the record already in the sink must survive byte-for-byte"
        );
        assert_eq!(
            std::fs::read(&published).unwrap(),
            b"{\"second\":true}",
            "the new record must survive too — a collision costs a name, never a record"
        );
        assert_eq!(
            record_files(dir.path()).len(),
            2,
            "both records are in the sink: {:?}",
            record_files(dir.path())
        );
    }

    /// A free name is used verbatim — the retry path must not fire when nothing
    /// collided, or every record would carry a spurious suffix.
    #[test]
    fn a_free_name_is_used_verbatim() {
        let dir = tempfile::tempdir().unwrap();

        let published = publish(dir.path(), pinned("pinned.json"), b"{}").unwrap();

        assert_eq!(published, dir.path().join("pinned.json"));
        assert_eq!(std::fs::read(&published).unwrap(), b"{}");
    }

    /// Successive collisions keep drawing fresh names rather than re-offering the
    /// one that just failed, so a sink can absorb a burst without losing a record.
    #[test]
    fn successive_collisions_each_draw_a_fresh_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pinned.json"), b"occupant").unwrap();

        for index in 0..4 {
            let published = publish(dir.path(), pinned("pinned.json"), format!("{index}").as_bytes()).unwrap();
            assert_eq!(std::fs::read(&published).unwrap(), format!("{index}").as_bytes());
        }

        assert_eq!(
            record_files(dir.path()).len(),
            5,
            "the occupant plus four records: {:?}",
            record_files(dir.path())
        );
    }

    /// The retry component lands before the extension, so a collector globbing
    /// `*.json` still sees a retried record.
    #[test]
    fn a_retry_keeps_the_extension() {
        let retried = with_random_component("20260726T140311482Z-48123.json");

        assert!(retried.ends_with(".json"), "retried name lost its extension: {retried}");
        assert!(retried.starts_with("20260726T140311482Z-48123-"));
        assert_ne!(retried, with_random_component("20260726T140311482Z-48123.json"));
    }

    /// An extensionless template still gets a distinguishing component appended,
    /// rather than silently re-offering the colliding name.
    #[test]
    fn a_retry_of_an_extensionless_name_still_varies() {
        let retried = with_random_component("record");

        assert!(retried.starts_with("record-"), "unexpected retry name: {retried}");
        assert_ne!(retried, "record");
    }

    /// Two draws in one process must differ, or the retry loop would re-offer the
    /// name that just collided and burn every attempt.
    #[test]
    fn successive_random_components_differ() {
        let draws: std::collections::HashSet<String> = (0..64).map(|_| random_component()).collect();

        assert!(
            draws.len() > 60,
            "random components repeated: {} unique of 64",
            draws.len()
        );
        assert!(
            draws
                .iter()
                .all(|draw| draw.len() == 8 && draw.chars().all(|c| c.is_ascii_hexdigit()))
        );
    }

    /// The bound exists so a permanently occupied sink fails loudly instead of
    /// spinning. Unlike [`pinned`], this name never varies, so every attempt
    /// collides and the loop can only end at the bound.
    #[test]
    fn a_name_that_never_varies_exhausts_the_bound_and_fails() {
        let dir = tempfile::tempdir().unwrap();
        let occupant = dir.path().join("constant.json");
        std::fs::write(&occupant, b"{\"first\":true}").unwrap();

        let error = publish(dir.path(), |_| "constant.json".to_string(), b"{\"second\":true}")
            .expect_err("a name that can never be free must fail rather than spin");

        match &error {
            RecordsError::Io { path, source } => {
                assert_eq!(path, dir.path(), "the error names the sink");
                assert_eq!(source.kind(), std::io::ErrorKind::AlreadyExists, "got: {source:?}");
            }
            other => panic!("expected an I/O error naming the sink, got {other:?}"),
        }
        assert_eq!(
            std::fs::read(&occupant).unwrap(),
            b"{\"first\":true}",
            "the record already in the sink must survive a failed publish byte-for-byte"
        );
        assert_eq!(
            record_files(dir.path()),
            vec![occupant],
            "a failed publish must leave nothing behind"
        );
    }

    /// A rendered name that is not a plain filename never reaches the
    /// filesystem: `..` would relocate the record out of the operator's sink,
    /// and a separator would send it to a parent directory that does not exist.
    #[test]
    fn a_name_that_is_not_a_plain_filename_is_refused() {
        let dir = tempfile::tempdir().unwrap();

        for name in ["../escaped.json", "sub/record.json", "..", ".", ""] {
            let error =
                publish(dir.path(), pinned(name), b"{}").expect_err(&format!("{name:?} must never be published under"));
            assert!(
                matches!(&error, RecordsError::NameNotAFilename { name: refused } if refused == name),
                "{name:?} must be refused by name, got: {error:?}"
            );
        }

        assert_eq!(
            record_files(dir.path()),
            Vec::<PathBuf>::new(),
            "a refused name must leave nothing behind"
        );
    }

    /// The discriminator for the guard above: an ordinary filename still
    /// publishes, so the check refuses structure rather than everything.
    #[test]
    fn a_plain_filename_still_publishes() {
        let dir = tempfile::tempdir().unwrap();

        let published = publish(dir.path(), pinned("20260726T140311482Z-4711.json"), b"{}").unwrap();

        assert_eq!(published, dir.path().join("20260726T140311482Z-4711.json"));
    }

    /// An absent sink is an error, not a panic and not a silent no-op — the
    /// caller's `required` posture decides what that costs.
    #[tokio::test]
    async fn probing_an_absent_sink_reports_io() {
        let dir = tempfile::tempdir().unwrap();
        let absent = dir.path().join("never-created");

        match probe_writable(&absent).await {
            Err(RecordsError::Io { path, .. }) => assert_eq!(path, absent),
            other => panic!("expected an I/O error for an absent sink, got {other:?}"),
        }
    }

    /// A writable sink probes clean, so the check above discriminates.
    #[tokio::test]
    async fn probing_a_writable_sink_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        // macOS puts tempdirs under `/var`, itself a symlink, which the sink's
        // ancestor walk would (correctly) refuse. `dunce`, not `std`: on Windows
        // `std::fs::canonicalize` returns a `\\?\`-prefixed path the sink's own
        // `dunce::canonicalize` never produces, so the two would differ and the
        // probe would read that as a substituted sink.
        let sink = dunce::canonicalize(dir.path()).unwrap();

        probe_writable(&sink).await.unwrap();
    }

    /// A sink substituted after it was designated is refused rather than
    /// followed: whoever swapped it would otherwise redirect the audit trail
    /// somewhere the operator never chose, and the collector would just see an
    /// empty sink.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_sink_substituted_after_designation_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let sink = root.join("records");
        std::fs::create_dir(&sink).unwrap();

        let policy = policy(&sink, None);
        let pinned = policy.dir().expect("a configured sink").to_path_buf();
        assert_eq!(pinned, sink, "the designated sink is the real directory");

        // The swap: same path, now pointing somewhere the operator never chose.
        std::fs::remove_dir(&sink).unwrap();
        let elsewhere = root.join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &sink).unwrap();

        match probe_writable(&pinned).await {
            Err(RecordsError::SinkSymlink { path }) => assert_eq!(path, pinned),
            other => panic!("expected a substitution refusal, got {other:?}"),
        }
        assert!(
            std::fs::read_dir(&elsewhere).unwrap().next().is_none(),
            "nothing may be written through the substituted path"
        );
    }

    /// The discriminator for the refusal above, and the defect it replaced: an
    /// OS alias in the sink path is ordinary, not an attack. macOS reaches
    /// `/var/log/ocx/records` through `/var` → `/private/var`, so refusing a
    /// symlinked ancestor made every macOS host a dead host under
    /// `required = true`.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_sink_reached_through_a_symlinked_ancestor_records_normally() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let real = root.join("private");
        std::fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, root.join("alias")).unwrap();
        std::fs::create_dir(real.join("records")).unwrap();

        let policy = policy(&root.join("alias").join("records"), None);
        let sink = policy.dir().expect("a configured sink");

        probe_writable(sink).await.expect("an aliased sink must be usable");
        emit(&record(recorded_at(), 4711), &policy)
            .await
            .expect("an aliased sink must record");
    }

    // ── emit ─────────────────────────────────────────────────────────────────

    /// The record's filename is drawn from the record itself, so the two can
    /// never disagree about when it was written or which process ran the tool.
    #[tokio::test]
    async fn emit_publishes_under_the_rendered_name_the_payload_agrees_with() {
        let dir = tempfile::tempdir().unwrap();
        let sink = dunce::canonicalize(dir.path()).unwrap();
        let policy = policy(&sink, Some("{time}-{pid}.json"));
        let record = record(recorded_at(), 4711);

        let published = emit(&record, &policy)
            .await
            .expect("a writable sink publishes")
            .expect("a configured sink returns the path it published to");

        assert_eq!(
            published,
            sink.join("20260726T140311482Z-4711.json"),
            "the filename must render from the record's own recordedAt and pid"
        );
        assert_eq!(
            std::fs::read_to_string(&published).unwrap(),
            record.to_json().unwrap(),
            "the published bytes must be the record the filename names"
        );
        assert_eq!(
            record.process.pid, 4711,
            "…and the payload's pid is the one in the name"
        );
    }

    /// Recording configured off is the common case and not a failure — not even
    /// under a fail-closed posture, where there is nothing to fail.
    ///
    /// The posture is minted through the SYSTEM lock rather than by writing
    /// `required = true` beside no `dir`: that spelling is a configuration
    /// error now ([`RecordsError::RequiredWithoutSink`]), because it is how an
    /// operator asks for recording and would otherwise get none. A locked block
    /// with no `dir` is the shape that still resolves — an operator locking
    /// recording *off* for the host, fail-closed by default with nothing to
    /// fail.
    #[tokio::test]
    async fn emit_without_a_sink_publishes_nothing_even_when_required() {
        let mut config = RecordsOptions::default();
        config.lock_as_system();
        let policy = resolve_records(config, RecordsOptions::default(), RecordsOptions::default())
            .expect("no sink resolves clean");
        assert!(policy.required(), "the fail-closed posture is in force");

        assert!(
            emit(&record(recorded_at(), 4711), &policy).await.unwrap().is_none(),
            "no sink means no record and no failure"
        );
    }

    // ── fixtures ─────────────────────────────────────────────────────────────

    /// `2026-07-26T14:03:11.482Z`, whose `{time}` expansion is the literal
    /// `20260726T140311482Z`.
    fn recorded_at() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-07-26T14:03:11.482Z")
            .expect("fixture timestamp is valid RFC 3339")
            .with_timezone(&Utc)
    }

    fn policy(dir: &Path, name: Option<&str>) -> RecordingPolicy {
        resolve_records(
            RecordsOptions {
                dir: Some(dir.to_path_buf()),
                name: name.map(str::to_string),
                ..RecordsOptions::default()
            },
            RecordsOptions::default(),
            RecordsOptions::default(),
        )
        .expect("the fixture template parses")
    }

    /// A minimal launcher-frame record — this file tests publication, not the
    /// payload, so the frame carries one package and nothing else.
    fn record(recorded_at: DateTime<Utc>, pid: u32) -> ExecutionRecord {
        let identifier = PinnedIdentifier::try_from(
            Identifier::new_registry("ocx/cmake", "index.ocx.sh").clone_with_digest(Digest::Sha256(HEX.to_string())),
        )
        .expect("digest present");
        let packages = vec![Arc::new(InstallInfo::new(
            identifier,
            serde_json::from_str(r#"{"type":"bundle","version":1,"env":[]}"#).expect("bundle metadata"),
            ResolvedPackage {
                dependencies: Vec::new(),
            },
            PackageDir::with_root(PathBuf::from("/store/cmake")),
        ))];
        let admitted = AdmittedClaims::default();
        let executable = PathBuf::from("/store/cmake/content/bin/cmake");
        let store_root = PathBuf::from("/store");
        let shim_root = PathBuf::from("/shims");
        let argv = vec!["cmake".to_string(), "--version".to_string()];
        let config = OcxConfigView::new("/opt/ocx/bin/ocx");

        ExecutionRecord::build(
            &RecordInputs {
                packages: &packages,
                admitted: &admitted,
                patch_companions: &[],
                executable: &executable,
                store_root: &store_root,
                shim_root: &shim_root,
                argv: &argv,
                config: &config,
                insecure_registries: &[],
                managed_config_digest: None,
                patch_snapshot_digest: None,
                platform: None,
                clean_env: false,
                auto_installed: &[],
                scope: Scope::Launcher,
            },
            recorded_at,
            pid,
        )
    }
}
