// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::sync::Arc;

const PROGRESS_CHARS: &str = "=> ";

/// Indent marker for a bar nested under a parent (rendered via `{prefix}`).
const NEST_PREFIX: &str = "  ↳ ";

tokio::task_local! {
    /// The current task's parent bar, set by [`Spinner::scope`]; bars created under it render indented.
    static PARENT_BAR: indicatif::ProgressBar;
}

// ── Span-free progress ──────────────────────────────────────────────
// Never span-attached `tracing-indicatif`: its span registry panics in `sharded::clone_span` under concurrent
// span close (adr_progress_architecture.md).

use std::borrow::Cow;

/// Owns the `indicatif::MultiProgress` and hands out RAII bar guards; cheap to clone.
#[derive(Clone)]
pub struct ProgressManager {
    /// `None` = disabled; a live `MultiProgress`'s draw target decides terminal vs. hidden.
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

    /// A live manager whose `MultiProgress` never draws, for exercising the real bar lifecycle without a TTY.
    pub fn hidden() -> Self {
        Self {
            multi: Some(Arc::new(indicatif::MultiProgress::with_draw_target(
                indicatif::ProgressDrawTarget::hidden(),
            ))),
        }
    }

    /// A manager that renders bars on the process's **controlling terminal**, degrading to
    /// [`disabled`](Self::disabled) when none is reachable (`setsid`, Docker builds, CI).
    ///
    /// Not [`stderr`](Self::stderr): bars there would corrupt the output of a wrapper capturing stderr.
    pub async fn controlling_terminal() -> Self {
        // `open(2)` on a terminal can block (serial carrier detect), so it runs off the runtime.
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

    /// The current task's parent bar, if a [`Spinner::scope`] is active.
    fn parent() -> Option<indicatif::ProgressBar> {
        PARENT_BAR.try_with(|p| p.clone()).ok()
    }

    /// Places `bar` in the `MultiProgress` and reports whether it is nested; a disabled manager detaches it.
    ///
    /// Always `add`, never `insert_after`: that unwraps `parent.index()` and aborts once the parent finished or
    /// was detached, a race no pre-check closes.
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

    /// A spinner for work of unknown duration, ticking on its own timer so it animates while the task is
    /// `.await`-blocked; indented under a [`Spinner::scope`].
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

    /// A byte-transfer bar with `label` before it; indented under a [`Spinner::scope`].
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

    /// A writer that emits log lines inside [`indicatif::MultiProgress::suspend`], so they never tear active bars.
    pub fn writer(&self) -> LogWriter {
        LogWriter {
            multi: self.multi.clone(),
        }
    }
}

/// Opens the controlling terminal as a draw target, or `None` when the process has none; blocking.
///
/// The `is_term` check keeps a `/dev/tty` that is not a terminal from receiving escape sequences.
#[cfg(unix)]
fn open_controlling_terminal() -> Option<console::Term> {
    let write = match std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty") {
        Ok(file) => file,
        Err(error) => {
            log::debug!("Progress disabled: no controlling terminal ({error})");
            return None;
        }
    };
    let read = match write.try_clone() {
        Ok(file) => file,
        Err(error) => {
            log::debug!("Progress disabled: cannot duplicate the controlling terminal ({error})");
            return None;
        }
    };
    // `TERM=dumb` gets no escape-sequence bar, the gate `ProgressDrawTarget::stderr` applies too.
    let term = console::Term::read_write_pair(read, write);
    (term.is_term() && !console::is_dumb()).then_some(term)
}

/// Windows has no controlling-terminal draw target, so progress degrades to silence there.
///
/// `console::Term` has no constructor over a console handle, and a hand-rolled VT `TermLike` would own
/// terminal rendering.
#[cfg(windows)]
fn open_controlling_terminal() -> Option<console::Term> {
    log::debug!("Progress disabled: this phase opens no controlling terminal on Windows");
    None
}

/// RAII body for a bar guard: cleared on drop unless [`abandon`](Self::abandon) froze it with a final message.
struct Guard {
    pb: indicatif::ProgressBar,
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
    /// Runs `fut` with this spinner as the task-local parent, so bars created in it nest beneath it; task-locals
    /// do not cross `tokio::spawn`.
    pub async fn scope<F: std::future::Future>(&self, fut: F) -> F::Output {
        PARENT_BAR.scope(self.0.pb.clone(), fut).await
    }

    /// Replaces the spinner's trailing message.
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
    /// A progress callback for transport methods, holding a plain `indicatif` handle, never a `tracing::Span`.
    pub fn callback(&self) -> Arc<dyn Fn(u64) + Send + Sync> {
        let pb = self.0.pb.clone();
        Arc::new(move |bytes: u64| pb.set_position(bytes))
    }

    /// Freezes the bar with a failure message instead of clearing it.
    pub fn abandon(&mut self, msg: impl Into<Cow<'static, str>>) {
        self.0.abandon(msg);
    }
}

/// Writer factory routing formatted log events through [`indicatif::MultiProgress::suspend`], so they never
/// interleave with bar redraws.
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
