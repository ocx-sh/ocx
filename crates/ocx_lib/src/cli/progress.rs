// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::sync::Arc;

/// Number of entries between periodic debug log messages during archiving.
pub const LOG_INTERVAL: u64 = 100;

const PROGRESS_CHARS: &str = "=> ";

/// Indent marker for a bar nested under a parent (rendered via `{prefix}`).
const NEST_PREFIX: &str = "  ↳ ";

tokio::task_local! {
    /// The active parent bar for the current task, set by
    /// [`Spinner::scope`]. Child bars created while this is set
    /// (`bytes`/`spinner`) insert after it and render indented. Plain
    /// `indicatif` `Arc` handle — never a `tracing::Span` — so the
    /// task-local carries no span-registry state.
    static PARENT_BAR: indicatif::ProgressBar;
}

// ── Span-free progress (ADR adr_progress_architecture) ──────────────
//
// Progress is driven through `indicatif` directly rather than through
// `tracing-indicatif`'s span-attached `IndicatifLayer`.
// `indicatif::ProgressBar` is `Arc`-backed `Send + Sync + Clone` with no
// global span registry, so concurrent updates from many tokio tasks
// cannot hit the `tracing_subscriber::registry::sharded::clone_span`
// ref-count assertion that the span-coupled model triggered under
// concurrent span close.

use std::borrow::Cow;

/// Owns the `indicatif::MultiProgress` and hands out RAII bar guards.
///
/// Cheap to clone (shares one `MultiProgress` via `Arc`) so it can be
/// threaded through facade structs whose fields must all be cheap to
/// clone. A [`disabled`](Self::disabled) manager renders nothing — every
/// guard method is a no-op — so library and test consumers pay no cost.
#[derive(Clone)]
pub struct ProgressManager {
    /// `None` = disabled (no rendering). `Some` = a live `MultiProgress`
    /// (its draw target decides terminal vs. hidden).
    multi: Option<Arc<indicatif::MultiProgress>>,
}

impl ProgressManager {
    /// A manager that renders bars to stderr.
    pub fn stderr() -> Self {
        Self {
            multi: Some(Arc::new(indicatif::MultiProgress::with_draw_target(
                indicatif::ProgressDrawTarget::stderr(),
            ))),
        }
    }

    /// A live manager whose `MultiProgress` never draws.
    ///
    /// Behaves like [`stderr`](Self::stderr) for ownership/lifetime
    /// purposes (bars are added and cloned) but produces no terminal
    /// output. Used by concurrency regression tests to exercise the
    /// real bar lifecycle without a TTY.
    pub fn hidden() -> Self {
        Self {
            multi: Some(Arc::new(indicatif::MultiProgress::with_draw_target(
                indicatif::ProgressDrawTarget::hidden(),
            ))),
        }
    }

    /// A no-op manager. Every guard method does nothing.
    pub fn disabled() -> Self {
        Self { multi: None }
    }

    /// The parent bar of the current task, if a [`Spinner::scope`] is
    /// active. `None` outside any scope (top-level bar).
    fn parent() -> Option<indicatif::ProgressBar> {
        PARENT_BAR.try_with(|p| p.clone()).ok()
    }

    /// Places `bar` in the `MultiProgress`. When a parent scope is
    /// active the bar is inserted directly after its parent so it renders
    /// as a child; otherwise it is appended top-level. Returns whether
    /// the bar is nested (for indent styling). A disabled manager
    /// detaches the bar so updates are cheap no-ops.
    fn attach(&self, bar: indicatif::ProgressBar) -> (indicatif::ProgressBar, bool) {
        match &self.multi {
            Some(multi) => match Self::parent() {
                Some(parent) => (multi.insert_after(&parent, bar), true),
                None => (multi.add(bar), false),
            },
            None => {
                bar.set_draw_target(indicatif::ProgressDrawTarget::hidden());
                (bar, false)
            }
        }
    }

    /// A spinner for work of unknown or instant duration.
    ///
    /// `label` renders after the spinner glyph (e.g.
    /// `⠋ Resolving 'cmake:3.28'`). The spinner ticks on its own timer
    /// so it animates even while the task is `.await`-blocked. Under a
    /// [`Spinner::scope`] it renders indented beneath its parent.
    pub fn spinner(&self, label: impl Into<Cow<'static, str>>) -> Spinner {
        let (pb, nested) = self.attach(indicatif::ProgressBar::new_spinner());
        let template = if nested {
            "{spinner} {prefix}{msg}"
        } else {
            "{spinner} {msg}"
        };
        pb.set_style(indicatif::ProgressStyle::with_template(template).expect("valid spinner template"));
        if nested {
            pb.set_prefix(NEST_PREFIX);
        }
        pb.set_message(label.into());
        pb.enable_steady_tick(std::time::Duration::from_millis(100));
        Spinner(Guard::new(pb))
    }

    /// A byte-transfer bar. `label` renders before the bar. Under a
    /// [`Spinner::scope`] it renders indented beneath its parent.
    pub fn bytes(&self, label: impl Into<Cow<'static, str>>, total: u64) -> BytesBar {
        let (pb, nested) = self.attach(indicatif::ProgressBar::new(total));
        let template = if nested {
            "{prefix}{msg} [{bar:30}] {bytes}/{total_bytes}"
        } else {
            "{msg} [{bar:30}] {bytes}/{total_bytes}"
        };
        pb.set_style(
            indicatif::ProgressStyle::with_template(template)
                .expect("valid bytes template")
                .progress_chars(PROGRESS_CHARS),
        );
        if nested {
            pb.set_prefix(NEST_PREFIX);
        }
        pb.set_message(label.into());
        BytesBar(Guard::new(pb))
    }

    /// A `MakeWriter` that emits log lines without tearing active bars.
    ///
    /// Wire into the `tracing_subscriber` fmt layer so formatted events
    /// flush inside [`MultiProgress::suspend`], which hides the bars for
    /// the duration of the write. When the manager is disabled it writes
    /// straight to stderr with no suspend overhead.
    pub fn writer(&self) -> LogWriter {
        LogWriter {
            multi: self.multi.clone(),
        }
    }
}

/// RAII body for a bar guard.
///
/// On drop the bar is cleared from the `MultiProgress`. [`abandon`](Self::abandon)
/// freezes the bar with a final message and suppresses the clear so a
/// failure stays visible.
struct Guard {
    pb: indicatif::ProgressBar,
    /// `true` once [`abandon`](Self::abandon) ran — the `Drop` clear is
    /// then skipped so the failure message survives.
    abandoned: bool,
}

impl Guard {
    fn new(pb: indicatif::ProgressBar) -> Self {
        Self { pb, abandoned: false }
    }

    fn abandon(&mut self, msg: impl Into<Cow<'static, str>>) {
        self.pb.abandon_with_message(msg.into());
        self.abandoned = true;
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if !self.abandoned {
            self.pb.finish_and_clear();
        }
    }
}

/// Carries the active parent bar across a `tokio::spawn` boundary.
///
/// Task-locals do not propagate into spawned tasks, so a layer download
/// dispatched on its own task would lose the package spinner as parent
/// and render flat. Wrap the spawned future: the parent is captured
/// **eagerly on the calling task** (this fn body runs before the
/// returned future), then re-established for the child task.
///
/// ```ignore
/// tasks.spawn(progress::inherit_scope(async move { work().await }));
/// ```
pub fn inherit_scope<F: std::future::Future>(fut: F) -> impl std::future::Future<Output = F::Output> {
    let parent = ProgressManager::parent();
    async move {
        match parent {
            Some(parent) => PARENT_BAR.scope(parent, fut).await,
            None => fut.await,
        }
    }
}

/// Spinner guard. Clears the spinner when it goes out of scope.
pub struct Spinner(Guard);

impl Spinner {
    /// Runs `fut` with this spinner registered as the task-local parent.
    ///
    /// Any `bytes`/`spinner` guard created while `fut` is in flight (on
    /// the same task — task-locals do not cross `tokio::spawn`) nests
    /// beneath this spinner and renders indented. Drop the spinner after
    /// the scope returns to clear it.
    pub async fn scope<F: std::future::Future>(&self, fut: F) -> F::Output {
        PARENT_BAR.scope(self.0.pb.clone(), fut).await
    }

    /// Replaces the spinner's trailing message (e.g. to show the active
    /// stage of a multi-step task).
    pub fn set_message(&self, msg: impl Into<Cow<'static, str>>) {
        self.0.pb.set_message(msg.into());
    }

    /// Freezes the spinner with a failure message instead of clearing it.
    pub fn abandon(&mut self, msg: impl Into<Cow<'static, str>>) {
        self.0.abandon(msg);
    }
}

/// Byte-transfer bar guard.
pub struct BytesBar(Guard);

impl BytesBar {
    /// A progress callback for transport methods.
    ///
    /// Captures a clone of the underlying `indicatif::ProgressBar` — a
    /// plain `Arc` handle, **not** a `tracing::Span`. Invoking it from
    /// any thread only touches indicatif's internal lock; it never
    /// reaches the `tracing` span registry, so the concurrent
    /// clone-after-close panic is impossible by construction.
    pub fn callback(&self) -> Arc<dyn Fn(u64) + Send + Sync> {
        let pb = self.0.pb.clone();
        Arc::new(move |bytes: u64| pb.set_position(bytes))
    }

    /// Freezes the bar with a failure message instead of clearing it.
    pub fn abandon(&mut self, msg: impl Into<Cow<'static, str>>) {
        self.0.abandon(msg);
    }
}

/// `MakeWriter` that routes formatted log events through
/// [`MultiProgress::suspend`] so they never interleave with bar redraws.
///
/// One handle is created per event by the fmt layer; it buffers the
/// formatted line and flushes on drop. Disabled managers write straight
/// to stderr with no suspend.
#[derive(Clone)]
pub struct LogWriter {
    multi: Option<Arc<indicatif::MultiProgress>>,
}

/// Per-event buffer; flushes on drop.
pub struct LogWriterHandle {
    multi: Option<Arc<indicatif::MultiProgress>>,
    buf: Vec<u8>,
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogWriter {
    type Writer = LogWriterHandle;

    fn make_writer(&'a self) -> Self::Writer {
        LogWriterHandle {
            multi: self.multi.clone(),
            buf: Vec::new(),
        }
    }
}

impl std::io::Write for LogWriterHandle {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let buf = std::mem::take(&mut self.buf);
        let emit = || std::io::Write::write_all(&mut std::io::stderr(), &buf);
        match &self.multi {
            Some(multi) => multi.suspend(emit),
            None => emit(),
        }
    }
}

impl Drop for LogWriterHandle {
    fn drop(&mut self) {
        let _ = std::io::Write::flush(self);
    }
}

#[cfg(test)]
mod span_free_tests {
    //! Regression spec for ADR adr_progress_architecture: the span-free
    //! byte path must survive concurrent bar creation/use/drop on a
    //! multi-thread runtime with no tracing subscriber installed. Under
    //! the old span-coupled model this exercised
    //! `tracing_subscriber::registry::sharded::clone_span` and could
    //! panic with "tried to clone a span that already closed".

    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_byte_bars_do_not_panic() {
        let manager = ProgressManager::hidden();
        let mut tasks = tokio::task::JoinSet::new();

        for i in 0..200 {
            let manager = manager.clone();
            tasks.spawn(async move {
                let bar = manager.bytes(format!(" 'pkg-{i}'"), 244);
                let on_progress = bar.callback();
                // Drive the callback concurrently with the guard drop —
                // exactly the resolve/download interleaving that tripped
                // the span registry assertion.
                for b in (0..=244).step_by(16) {
                    on_progress(b);
                    tokio::task::yield_now().await;
                }
                drop(bar);
                // Callback intentionally outlives the guard: under the
                // span model this was a cloned span outliving its close.
                on_progress(244);
            });
        }

        while let Some(joined) = tasks.join_next().await {
            joined.expect("byte-bar task must not panic (ADR adr_progress_architecture)");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_spinners_do_not_panic() {
        // Mirrors the package-manager JoinSet fan-out and the mirror
        // pipeline: many short-lived spinners created, message-updated
        // (set_stage), and dropped concurrently on different worker
        // threads — the exact interleaving the old spinner_span model
        // tripped in the sharded span registry.
        let manager = ProgressManager::hidden();
        let mut tasks = tokio::task::JoinSet::new();

        for i in 0..200 {
            let manager = manager.clone();
            tasks.spawn(async move {
                let spinner = manager.spinner(format!("Resolving 'pkg-{i}'"));
                for stage in ["Downloading", "Verifying", "Bundling"] {
                    spinner.set_message(format!("pkg-{i} — {stage}"));
                    tokio::task::yield_now().await;
                }
                drop(spinner);
            });
        }

        while let Some(joined) = tasks.join_next().await {
            joined.expect("spinner task must not panic (ADR adr_progress_architecture)");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_nested_bars_do_not_panic() {
        // Parent spinner scopes a child byte bar (the package-spinner →
        // download-bar nesting). Many tasks insert_after concurrently;
        // indicatif handles the reorder safely (no span registry).
        let manager = ProgressManager::hidden();
        let mut tasks = tokio::task::JoinSet::new();

        for i in 0..200 {
            let manager = manager.clone();
            tasks.spawn(async move {
                let spin = manager.spinner(format!("Pulling 'pkg-{i}'"));
                spin.scope(async {
                    let bar = manager.bytes(format!("Downloading 'pkg-{i}'"), 244);
                    let on_progress = bar.callback();
                    for b in (0..=244).step_by(32) {
                        on_progress(b);
                        tokio::task::yield_now().await;
                    }
                })
                .await;
                drop(spin);
            });
        }

        while let Some(joined) = tasks.join_next().await {
            joined.expect("nested-bar task must not panic (ADR adr_progress_architecture)");
        }
    }

    #[tokio::test]
    async fn child_nests_only_within_scope() {
        let manager = ProgressManager::disabled();
        // Outside any scope: no parent.
        assert!(ProgressManager::parent().is_none());
        let spin = manager.spinner("parent");
        spin.scope(async {
            // Inside scope: parent is visible to child constructors.
            assert!(ProgressManager::parent().is_some());
            let _child = manager.bytes("child", 1);
        })
        .await;
        assert!(
            ProgressManager::parent().is_none(),
            "scope must not leak past the future"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn inherit_scope_carries_parent_across_spawn() {
        // The layer download runs on a spawned task (extract_layers
        // JoinSet); task-locals do not cross tokio::spawn, so without
        // inherit_scope the download bar would lose its package parent
        // and render flat. inherit_scope captures the parent eagerly on
        // the spawning task and re-establishes it on the child.
        let manager = ProgressManager::hidden();
        let spin = manager.spinner("Pulling 'pkg'");
        spin.scope(async {
            // Plain spawn: parent NOT visible (control).
            let bare = tokio::spawn(async { ProgressManager::parent().is_some() })
                .await
                .unwrap();
            assert!(!bare, "plain spawn must not inherit the task-local parent");

            // inherit_scope: parent IS visible on the child task.
            let inherited = tokio::spawn(super::inherit_scope(async { ProgressManager::parent().is_some() }))
                .await
                .unwrap();
            assert!(inherited, "inherit_scope must carry the parent across spawn");
        })
        .await;
        drop(spin);
    }

    #[test]
    fn callback_holds_no_tracing_span() {
        // No subscriber, no entered span: a span-coupled callback would
        // be inert or panic; the indicatif handle just works.
        let bar = ProgressManager::disabled().bytes(" 'x'", 10);
        let cb = bar.callback();
        cb(5);
        cb(10);
    }

    #[test]
    fn abandon_suppresses_clear() {
        let mut bar = ProgressManager::hidden().bytes(" 'fail'", 100);
        bar.abandon("download failed");
        // Drop must not panic and must not clear the abandoned bar.
        drop(bar);
    }
}
