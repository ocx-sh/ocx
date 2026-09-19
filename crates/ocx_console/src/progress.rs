// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::sync::Arc;

const PROGRESS_CHARS: &str = "=> ";

/// Indent marker for a bar nested under a parent (rendered via `{prefix}`).
const NEST_PREFIX: &str = "  ↳ ";

tokio::task_local! {
    /// The active parent bar for the current task, set by
    /// [`Spinner::scope`]. Child bars created while this is set
    /// (`bytes`/`spinner`) render indented beneath it. Plain
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

    /// A manager that renders bars on the process's **controlling terminal**,
    /// degrading to [`disabled`](Self::disabled) when none is reachable.
    ///
    /// Deliberately not [`stderr`](Self::stderr): a tool invoked by `make`, or
    /// by any wrapper that captures stderr, still has a controlling terminal,
    /// and that is the channel a long transfer has to render on — writing into
    /// the wrapped tool's own stderr stream would corrupt it, and skipping it
    /// because stderr is a pipe leaves the user staring at a silent hang.
    ///
    /// Opening it fails, and must degrade rather than error, in exactly the
    /// environments this is best-effort for: `ENXIO` under `setsid`, in Docker
    /// builds and on CI runners is the common case, not the exception.
    pub async fn controlling_terminal() -> Self {
        // `open(2)` on a terminal can block (carrier detect on a serial
        // console), so the probe runs off the runtime rather than on it.
        match tokio::task::spawn_blocking(open_controlling_terminal).await {
            Ok(Some(term)) => Self {
                multi: Some(Arc::new(indicatif::MultiProgress::with_draw_target(
                    indicatif::ProgressDrawTarget::term_like(Box::new(term)),
                ))),
            },
            Ok(None) => Self::disabled(),
            Err(join_error) => {
                log::debug!("Progress disabled: the controlling-terminal probe failed: {join_error}");
                Self::disabled()
            }
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

    /// Places `bar` in the `MultiProgress` and reports whether it is nested
    /// (for indent styling). A disabled manager detaches the bar so updates
    /// are cheap no-ops.
    ///
    /// Nesting is **styling only**: the bar is always appended, never
    /// positioned relative to its parent. `MultiProgress::insert_after`
    /// unwraps `parent.index()`, which is `None` whenever the parent is not a
    /// member of this `MultiProgress` — a parent whose [`Guard`] finished
    /// before the child attached (observed as an abort of `ocx package exec
    /// --lazy-mode always` on a real terminal, back when a task spinner was
    /// carried across the layer-download `tokio::spawn`), or a parent created
    /// by a [`disabled`](Self::disabled) manager (detached by construction)
    /// while an enabled manager attaches the child.
    ///
    /// A membership pre-check cannot close it: `index()` is private to
    /// `indicatif`, and the public `is_hidden`/`is_finished` pair still leaves
    /// the window where the parent finishes between the check and
    /// `insert_after` re-reading the index. So the positional API is dropped
    /// altogether — `add` cannot fail this way. Children keep their indent
    /// prefix; the only loss is adjacency, a child rendering at the bottom
    /// rather than directly beneath its parent.
    fn attach(&self, bar: indicatif::ProgressBar) -> (indicatif::ProgressBar, bool) {
        match &self.multi {
            Some(multi) => match Self::parent() {
                Some(_) => (multi.add(bar), true),
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

    /// A writer that emits log lines without tearing active bars.
    ///
    /// Formatted output flushes inside [`indicatif::MultiProgress::suspend`], which hides
    /// the bars for the duration of the write. When the manager is disabled it
    /// writes straight to stderr with no suspend overhead. The binary adapts
    /// this to whatever writer trait its log subscriber asks for; the console
    /// itself names no subscriber.
    pub fn writer(&self) -> LogWriter {
        LogWriter {
            multi: self.multi.clone(),
        }
    }
}

/// Opens the controlling terminal as an `indicatif` draw target, or `None`
/// when the process has none.
///
/// Blocking — call it from [`spawn_blocking`](tokio::task::spawn_blocking).
///
/// `console::Term` is the draw target rather than a hand-written one: it
/// already owns the cursor-movement, width-probing and line-clearing that
/// `indicatif::TermLike` asks for, and `read_write_pair` builds one over an
/// arbitrary file descriptor. The `is_term` check is what makes a `/dev/tty`
/// that opened but is not a terminal (a redirected slave, a captured pty)
/// degrade instead of writing escape sequences into a file.
#[cfg(unix)]
fn open_controlling_terminal() -> Option<console::Term> {
    let write = match std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty") {
        Ok(file) => file,
        Err(error) => {
            log::debug!("Progress disabled: no controlling terminal ({error})");
            return None;
        }
    };
    // `read_write_pair` wants both halves; the read side is never used for
    // drawing, but `Term`'s feature probes read the write half's descriptor.
    let read = match write.try_clone() {
        Ok(file) => file,
        Err(error) => {
            log::debug!("Progress disabled: cannot duplicate the controlling terminal ({error})");
            return None;
        }
    };
    // `is_dumb` is the same gate `ProgressDrawTarget::stderr` applies: an
    // escape-sequence bar on a `TERM=dumb` console is garbage, whichever fd
    // it reaches.
    let term = console::Term::read_write_pair(read, write);
    (term.is_term() && !console::is_dumb()).then_some(term)
}

/// Windows has no controlling-terminal draw target yet, so `progress` degrades
/// to silence there.
///
/// `console::Term` exposes no constructor over an arbitrary console handle
/// (`read_write_pair` is `#[cfg(unix)]`), and hand-rolling a VT `TermLike` over
/// `CONOUT$` would mean owning terminal rendering — which is exactly what
/// `quality-core.md` § *Don't Own Non-Domain Code* argues against.
///
/// **This is the end state, not a placeholder.** An earlier version of this
/// comment deferred the `CONOUT$` arm to "the change that lands the Windows
/// shim producer", on the premise that `LazyModeLadder::resolve_for_host`
/// forced `LazyMode::Never` on Windows and nothing could ever be deferred
/// there. That floor is gone (C-027), so the premise is false and the arm stays
/// closed on its own merits: returning `None` degrades progress to
/// `LazyReport::Silent`, which `LazyReport`'s own contract sanctions
/// ("degrades to Silent on failure, never to an error"). A Windows user loses a
/// progress bar during a deferred materialization, and nothing else.
#[cfg(windows)]
fn open_controlling_terminal() -> Option<console::Term> {
    log::debug!("Progress disabled: this phase opens no controlling terminal on Windows");
    None
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

/// Writer factory that routes formatted log events through
/// [`indicatif::MultiProgress::suspend`] so they never interleave with bar redraws.
///
/// One handle is created per event by the caller; it buffers the formatted
/// line and flushes on drop. Disabled managers write straight to stderr with
/// no suspend.
#[derive(Clone)]
pub struct LogWriter {
    multi: Option<Arc<indicatif::MultiProgress>>,
}

/// Per-event buffer; flushes on drop.
pub struct LogWriterHandle {
    multi: Option<Arc<indicatif::MultiProgress>>,
    buf: Vec<u8>,
}

impl LogWriter {
    /// Opens one per-event buffer. The caller flushes it by dropping it.
    pub fn handle(&self) -> LogWriterHandle {
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

    /// A parent in scope that is not a member of the attaching
    /// `MultiProgress` must not abort the process.
    ///
    /// Reported from `ocx package exec --lazy-mode always` on a real terminal:
    /// `insert_after` unwrapped `parent.index()` for a byte bar whose parent
    /// spinner, carried across a `tokio::spawn`, had already finished.
    /// Backtrace: `insert_after` ← `attach` ← `bytes` ← `extract_layer_inner`.
    ///
    /// The state is built from the other production — a detached parent from
    /// a disabled manager, an enabled manager attaching the child — because it
    /// needs no scheduling race to reproduce. `disabled()` sets a hidden draw
    /// target, so the parent is a non-member either way, which is the input
    /// `insert_after` could not survive.
    ///
    /// The sibling test above cannot catch this: it keeps every parent alive
    /// and uses one manager.
    #[tokio::test]
    async fn a_child_whose_parent_is_not_in_this_multi_does_not_panic() {
        let detached = ProgressManager::disabled();
        let rendering = ProgressManager::hidden();

        let parent = detached.spinner("parent outside any MultiProgress");
        assert!(
            parent.0.pb.is_hidden(),
            "a disabled manager's bar must be detached, or this test proves nothing"
        );

        parent
            .scope(async {
                let child = rendering.bytes("child attached by a different manager", 8);
                child.callback()(8);
            })
            .await;
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
