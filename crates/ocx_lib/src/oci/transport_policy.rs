// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Retry and timeout policy for HTTP transports
//! (`adr_index_sync_performance.md` D-010 / D-011).
//!
//! Three value objects and one driver, all transport-agnostic:
//!
//! - [`RetryPolicy`] — how many attempts, how long to back off, how far a
//!   server-stated `Retry-After` may be trusted.
//! - [`RetryBudget`] — how much *total* retry traffic a run may add. Shared
//!   state, not a value object: the counters live behind an [`Arc`] so every
//!   clone of a transport meters against one budget (D-010 rule 2).
//! - [`TransportHardening`] — the three timeout bounds a client is built with.
//!   Injectable so a fixture can assert the same semantics in milliseconds
//!   instead of the shipped minutes (D-011).
//! - [`run`] — the ladder itself, driving an attempt closure. It takes a
//!   closure rather than a request so it can be exercised under
//!   [`tokio::time::pause`] with no socket: a wall-clock assertion under a
//!   paused clock cannot tell "slept 2 s" from "returned immediately", and a
//!   real socket under a paused clock trips its own timeouts on auto-advance.
//!
//! # Why this is hand-rolled and not a crate
//!
//! `reqwest-middleware` + `reqwest-retry` implement most of this. The ADR's
//! Golden Path metadata commits this work to **no new dependency**, and two
//! behaviours are OCX's own anyway: the run-global retry *ratio* budget, and
//! ACR's `Retry-After` counting down across polls (so the first value seen is
//! never cached). Neither is a wire format, so this is Warn-tier, not Block.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Statuses worth a second attempt (D-010, oras-go's default set plus GCS's
/// documented one). Everything else — `401`, `403`, `404`, every other 4xx,
/// and every `3xx` — is terminal.
///
/// `404` is deliberately absent: on the index path a confirmed absence is
/// load-bearing (it is what `OcxIndex::jurisdiction` settles an `Outside`
/// verdict off), so re-asking cannot change the answer. `3xx` is absent
/// because `redirect::Policy::none()` surfaces it as a non-success status and
/// re-fetching an unfollowed redirect returns the same redirect (D-011c).
pub fn is_retryable_status(status: u16) -> bool {
    matches!(status, 408 | 429 | 500 | 502 | 503 | 504)
}

/// Whether a status may carry a `Retry-After` the ladder honours (RFC 9110
/// §10.2.3 defines it for `429`, `503` and `3xx`; `3xx` is never retried here).
pub fn honours_retry_after(status: u16) -> bool {
    matches!(status, 429 | 503)
}

/// Whether a `reqwest` transport failure is worth another attempt (D-010).
///
/// **Everything except a builder error is.**
///
/// The narrow rule this replaces — `is_connect() || is_timeout()`, plus a walk
/// of the source chain for a transient [`std::io::ErrorKind`] — is inert
/// against the transport this path actually negotiates. `Cargo.toml` enables
/// `reqwest`'s `http2` feature and nothing here calls `http1_only()`, so ALPN
/// settles on h2 against a CDN-fronted index; and an h2 failure does not reach
/// an `io::Error` at all. `hyper::Error::new_h2` routes only an *io-backed* h2
/// failure to `new_io` (hyper 1.11 `src/error.rs`) — everything else becomes
/// `Kind::Http2` carrying the `h2::Error`, and `impl error::Error for Error {}`
/// (h2 0.4 `src/error.rs`) overrides no `source()`, so the chain terminates
/// there with nothing for the walk to find. A `RST_STREAM` — which RFC 9113
/// §8.7 names as the code a peer sends precisely to say "retry this elsewhere",
/// and which is what a CDN emits when it sheds a stream — was therefore
/// classified **terminal**, and one of them inside a 512-wide burst failed the
/// whole run. `h2_wire_tests` is the guard, and it reds on exactly that rule.
///
/// A `GOAWAY` escaped the old rule only by accident: h2 fails an in-flight
/// stream with a *synthesized* `BrokenPipe`, which the walk did catch. Nothing
/// in the classification depended on that, and nothing does now.
///
/// # Why retrying this broadly is safe *here*
///
/// Not because retrying is generally safe — because of what this one caller
/// is. The request is a bodyless idempotent `GET` (D-010c). Redirects are
/// refused rather than followed (`redirect::Policy::none()`), so a `3xx` is a
/// *status* — which [`is_retryable_status`] excludes — and never an error to
/// classify. The size cap and the parse both happen outside the ladder, so no
/// attempt can commit a partial result. Volume stays bounded by
/// [`RetryPolicy::attempts`] and the run-global [`RetryBudget`], which is what
/// makes "retry unless a second attempt provably cannot change the answer"
/// affordable rather than an amplification risk.
///
/// A builder error is that one class: raised from the request the caller
/// constructed, before a byte leaves the process.
pub fn is_retryable_transport_error(error: &reqwest::Error) -> bool {
    !error.is_builder()
}

/// Parses a `Retry-After` header value into the interval to wait, relative to
/// `now` (RFC 9110 §10.2.3 — **both** wire forms).
///
/// Returns `None` when the value is unparseable, which the caller reads as
/// "the header was not usable" and falls back to the normal jittered backoff.
/// A **past-dated** HTTP-date returns `Some(ZERO)` — never a negative or a
/// wrapped duration — so a stale date retries immediately rather than
/// underflowing into a multi-century sleep.
///
/// The date form is parsed as RFC 2822, which accepts the IMF-fixdate
/// (`Sun, 06 Nov 1994 08:49:37 GMT`) RFC 9110 requires senders to use. The two
/// obsolete forms (RFC 850, asctime) fall through to `None` and therefore to
/// jittered backoff, which is a safe degradation rather than a wrong sleep.
pub fn parse_retry_after(value: &str, now: std::time::SystemTime) -> Option<Duration> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let target = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    let target = std::time::UNIX_EPOCH.checked_add(Duration::from_secs(u64::try_from(target.timestamp()).ok()?))?;
    // `saturating_duration_since` is what makes a past date zero rather than a
    // wrapped `Duration` — this is attacker-influenced input.
    Some(target.duration_since(now).unwrap_or(Duration::ZERO))
}

/// What the ladder should do before the next attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDelay {
    /// Sleep this long, then retry.
    After(Duration),
    /// The server asked for longer than the policy will wait. Give up now and
    /// surface the failure — do **not** sleep for the stated interval.
    StopRetrying,
}

/// How the ladder retries (D-010's table).
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// Total attempts, initial included. `3` ships (initial + 2 retries).
    ///
    /// `0` means **one** attempt, not none: [`run`] has to call the closure
    /// before it has a `T` to return at all, so the cap is only ever consulted
    /// after the first attempt has already run. Documented rather than
    /// clamped — a clamp would spend a branch making a value that cannot mean
    /// "never dispatch" merely *look* like it was rejected.
    pub attempts: u32,
    /// Backoff base; the ceiling for retry `n` is `base * 2^n`.
    pub base: Duration,
    /// Upper bound on the backoff ceiling however many retries have run.
    pub cap: Duration,
    /// Upper bound on a server-stated `Retry-After`. Above it the ladder stops
    /// (see [`RetryDelay::StopRetrying`]).
    ///
    /// **A security control, not a tuning knob (CWE-400).** `Retry-After` is
    /// attacker- or misconfiguration-controlled header input; `Retry-After:
    /// 86400` would otherwise freeze an unattended CI sync for a day on one
    /// header from one CDN edge.
    pub retry_after_clamp: Duration,
}

impl Default for RetryPolicy {
    /// oras-go's field-tested defaults (250 ms base, 3 s cap) at the Google SRE
    /// Book's per-request attempt floor, with D-010's 30 s `Retry-After` clamp.
    fn default() -> Self {
        Self {
            attempts: 3,
            base: Duration::from_millis(250),
            cap: Duration::from_secs(3),
            retry_after_clamp: Duration::from_secs(30),
        }
    }
}

impl RetryPolicy {
    /// The full-jitter ceiling for retry `retry_index` (0-based):
    /// `min(cap, base * 2^retry_index)`.
    pub fn backoff_ceiling(&self, retry_index: u32) -> Duration {
        self.base
            .saturating_mul(1u32.checked_shl(retry_index).unwrap_or(u32::MAX))
            .min(self.cap)
    }

    /// The delay before retry `retry_index`, honouring a parsed `Retry-After`
    /// when the server sent a usable one.
    ///
    /// Without a header this is **full jitter** — `random(0, ceiling)` — which
    /// the AWS Architecture Blog measured to do less total work than equal
    /// jitter under contention. Lockstep backoff across a 512-wide fan-out
    /// against one rate limiter reproduces the spike the ladder exists to
    /// absorb, so the jitter is not optional.
    pub fn delay_for(&self, retry_index: u32, retry_after: Option<Duration>) -> RetryDelay {
        match retry_after {
            Some(stated) if stated > self.retry_after_clamp => RetryDelay::StopRetrying,
            Some(stated) => RetryDelay::After(stated),
            None => RetryDelay::After(full_jitter(self.backoff_ceiling(retry_index))),
        }
    }
}

/// Uniform draw from `[0, ceiling)`.
///
/// SplitMix64 over a process-global counter, seeded once from the wall clock —
/// which keeps the retry path free of a `rand` dependency, the point of the
/// house pattern in `utility/fs.rs::jittered_backoff`, without inheriting its
/// one weakness.
///
/// **The mix is load-bearing, and this is not hypothetical.** Scaling
/// `SystemTime::now().subsec_nanos()` straight into the ceiling — the house
/// pattern verbatim — makes a draw a *linear* function of the wall clock, so
/// draws taken close together come out close together: 64 draws in a tight
/// loop span about 25 microseconds of a 3-second ceiling instead of spanning
/// the ceiling. That cluster is the lockstep the jitter exists to break, and a
/// 512-wide fan-out retrying off one shared rate limiter is exactly the case
/// that produces it. Nor is it theoretical: it made this module's own jitter
/// test fail on the first run, because tokio's millisecond-granular timer
/// rounded every one of 20 supposedly-jittered delays to the same value.
/// `full_jitter_draws_are_spread_across_the_ceiling_not_clustered_by_the_clock`
/// is the guard, and it reds on exactly that source.
///
/// The counter carries the smaller half: the finalizer avalanches its input, so
/// it needs the input to *move*, and a clock that has not ticked between two
/// calls would otherwise hand it the same one twice. The clock still seeds the
/// sequence, so two processes starting together do not draw the same one.
fn full_jitter(ceiling: Duration) -> Duration {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    static SEED: std::sync::OnceLock<u64> = std::sync::OnceLock::new();

    let ceiling_nanos = ceiling.as_nanos();
    if ceiling_nanos == 0 {
        return Duration::ZERO;
    }
    let seed = *SEED.get_or_init(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64
    });
    // SplitMix64 (Steele, Lea & Flood 2014): one finalizer round over a
    // golden-ratio-strided counter. Fixed cost, no state beyond the counter.
    let strided = COUNTER
        .fetch_add(1, Ordering::Relaxed)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let mut mixed = seed.wrapping_add(strided);
    mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    mixed ^= mixed >> 31;

    // Top 53 bits scaled into the ceiling: uniform over `[0, ceiling)`.
    let scaled = ceiling_nanos * u128::from(mixed >> 11) / (1u128 << 53);
    Duration::from_nanos(u64::try_from(scaled).unwrap_or(u64::MAX))
}

/// Retries admitted before the ratio applies at all.
///
/// Without it the ratio makes the very first request unretryable (`1/1 > 0.1`)
/// and the ladder is deleted for small runs. Google's SRE client budget
/// carries the same floor.
const RETRY_FLOOR: u64 = 10;

/// Retry traffic admitted as a fraction of total traffic, as a reciprocal:
/// `retries / total <= 1/10`.
const RETRY_RATIO_DENOMINATOR: u64 = 10;

/// Run-global cap on retry traffic (D-010 rule 2).
///
/// A ratio, not a count: a constant sized for 200 packages starves a
/// 5,000-package sync and is far too loose for a 20-package one. The Google
/// SRE Book records the amplification arithmetic this prevents (3 layers × 4
/// attempts = 64×) — and for a single process fanning out 512-wide against one
/// bottleneck the amplification is *within* one process.
///
/// # Why the counters are behind an `Arc`
///
/// `ReqwestIndexTransport` is `Clone` and `IndexTransport` requires
/// `box_clone`; `index_common.rs` builds one index per package **inside** the
/// fan-out, so it clones the transport per package. Counters held as plain
/// fields would therefore be per-package — which is the uncapped per-request
/// policy this budget exists to replace, and which every single-package test
/// would pass regardless. `Relaxed` is deliberate: at this width exactness is
/// not required, monotonicity is.
///
/// Scope is one transport, i.e. one source per process. A process-global
/// counter would couple two sources' failures.
#[derive(Debug, Clone, Default)]
pub struct RetryBudget {
    total: Arc<AtomicU64>,
    retries: Arc<AtomicU64>,
}

impl RetryBudget {
    pub fn new() -> Self {
        Self::default()
    }

    /// Counts one attempt — initial or retry — against the run's total.
    pub fn record_attempt(&self) {
        self.total.fetch_add(1, Ordering::Relaxed);
    }

    /// Admits a retry while `retries / total <= 1/10`, or while the floor is
    /// unspent. Returns `false` once the budget is exhausted, and the caller
    /// must then fail **immediately** without sleeping: a budget that still
    /// sleeps is the amplification the budget exists to prevent.
    pub fn try_admit_retry(&self) -> bool {
        let total = self.total.load(Ordering::Relaxed);
        self.retries
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |retries| {
                let admitted = retries + 1;
                (admitted <= RETRY_FLOOR || admitted.saturating_mul(RETRY_RATIO_DENOMINATOR) <= total)
                    .then_some(admitted)
            })
            .is_ok()
    }
}

#[cfg(test)]
impl RetryBudget {
    /// `(total, retries)` so far.
    ///
    /// Test-only: the budget's whole effect on production is the `bool`
    /// [`RetryBudget::try_admit_retry`] returns, and nothing outside the
    /// assertions below has ever read the counters. Kept out of the shipped
    /// surface rather than left `pub` under a "diagnostics" doc line that no
    /// diagnostic call site backed.
    pub fn counts(&self) -> (u64, u64) {
        (self.total.load(Ordering::Relaxed), self.retries.load(Ordering::Relaxed))
    }
}

/// The three timeout bounds an index HTTP client is built with (D-011).
///
/// All three apply **simultaneously** — `reqwest`'s `timeout`, `read_timeout`
/// and `connect_timeout` are independent composable builder calls — which is
/// curl's own recommended composition: a fast-fail connect, a stall detector in
/// the middle, and a generous outer cap.
///
/// They are fields rather than constants only so a fixture can assert the same
/// semantics at a few hundred milliseconds; the shipped values are the only
/// ones production uses.
#[derive(Debug, Clone, Copy)]
pub struct TransportHardening {
    /// Bound on the connect phase. A dead or black-holing endpoint must not
    /// stall a resolve indefinitely (CWE-400).
    pub connect_timeout: Duration,
    /// Per-frame idle bound: a body that goes quiet for this long is dead, but
    /// an honest slow body that keeps delivering frames is untouched however
    /// long it runs. This is the slowloris control the old hard total deadline
    /// used to provide, without punishing a throttled link.
    pub idle_bound: Duration,
    /// Per-attempt outer cap. The bound that a dribbling peer — one byte every
    /// `idle_bound − ε`, never tripping the idle bound and never reaching the
    /// byte cap — cannot outlast. Wraps **one attempt**, not the ladder.
    pub outer_cap: Duration,
}

/// One attempt's outcome, as the ladder sees it.
pub enum Attempt<T> {
    /// Terminal — success or a failure another attempt cannot change. Returned
    /// to the caller as-is.
    Done(T),
    /// Retryable. `retry_after` is the server's parsed hint (`None` when it
    /// sent none or an unusable one); `terminal` is what the caller gets if no
    /// further attempt is admitted.
    Retry { retry_after: Option<Duration>, terminal: T },
}

/// Drives `attempt` under `policy` and `budget` until it is done, out of
/// attempts, past the `Retry-After` clamp, or out of budget.
///
/// `attempt` receives the 0-based attempt index, which is what lets the caller
/// emit its own (redacted) retry diagnostic without this module knowing
/// anything about URLs.
///
/// Deliberately **not** wrapped in a `tokio::time::timeout`: total retry
/// volume is already bounded by the budget, and a wall-clock cap on the ladder
/// would abort a run making honest progress across several slow-but-not-stalled
/// attempts — the exact case D-011 exists to stop aborting.
pub async fn run<T, F, Fut>(policy: &RetryPolicy, budget: &RetryBudget, mut attempt: F) -> T
where
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = Attempt<T>>,
{
    let mut index = 0u32;
    loop {
        budget.record_attempt();
        match attempt(index).await {
            Attempt::Done(value) => return value,
            Attempt::Retry { retry_after, terminal } => {
                if index + 1 >= policy.attempts {
                    return terminal;
                }
                match policy.delay_for(index, retry_after) {
                    RetryDelay::StopRetrying => return terminal,
                    RetryDelay::After(delay) if budget.try_admit_retry() => {
                        tokio::time::sleep(delay).await;
                        index += 1;
                    }
                    // Out of budget: fail with the attempt's own terminal
                    // value, without sleeping.
                    RetryDelay::After(_) => return terminal,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;
    use tokio::time::Instant;

    // ── C-016 · classification ───────────────────────────────────────────────

    #[test]
    fn the_retryable_set_is_exactly_d010s() {
        for status in [408, 429, 500, 502, 503, 504] {
            assert!(is_retryable_status(status), "{status} is in D-010's retryable set");
        }
        for status in [
            200, 301, 302, 304, 307, 308, 400, 401, 403, 404, 405, 409, 410, 418, 422, 451, 501, 505,
        ] {
            assert!(
                !is_retryable_status(status),
                "{status} must be terminal: a 404 is a load-bearing confirmed absence, a 401/403 will not \
                 change on re-ask, and an unfollowed 3xx re-fetches the same redirect"
            );
        }
    }

    #[test]
    fn retry_after_is_read_only_where_rfc_9110_defines_it_for_a_retry() {
        assert!(honours_retry_after(429));
        assert!(honours_retry_after(503));
        for status in [408, 500, 502, 504, 301, 200] {
            assert!(
                !honours_retry_after(status),
                "{status} must fall to jittered backoff, not to a header the ladder never retries on"
            );
        }
    }

    /// The one exclusion in [`is_retryable_transport_error`] — and the only
    /// case in the module that must classify **terminal**, which is what keeps
    /// "retry everything else" a rule rather than an unconditional `true`.
    #[tokio::test]
    async fn a_builder_error_is_terminal_where_every_wire_failure_is_not() {
        let error = reqwest::Client::new()
            .get("http://[not-a-url")
            .send()
            .await
            .expect_err("an unparseable URL fails before the request is dispatched");
        assert!(
            error.is_builder(),
            "precondition: this is the builder class, not a wire failure that happened to fail fast"
        );
        assert!(
            !is_retryable_transport_error(&error),
            "a request the caller could not even construct is identical on every attempt; retrying \
             it spends the run's budget on an outcome that cannot change"
        );
    }

    // ── C-017 · `Retry-After`, both wire forms plus the three edge cases ─────

    #[test]
    fn delay_seconds_form_parses() {
        let now = UNIX_EPOCH + Duration::from_secs(1_000_000);
        assert_eq!(parse_retry_after("2", now), Some(Duration::from_secs(2)));
        assert_eq!(parse_retry_after("  7  ", now), Some(Duration::from_secs(7)));
        assert_eq!(parse_retry_after("0", now), Some(Duration::ZERO));
    }

    /// Also the only guard on the **injected** clock: a parser reading
    /// `SystemTime::now()` instead of `now` reds here, and nowhere else. The
    /// delta-seconds form cannot carry that property — it returns before `now`
    /// is read, so a test passing it a clock asserts nothing about the clock.
    #[test]
    fn http_date_form_parses_to_the_interval_from_now() {
        // 2026-08-22T12:00:00Z, and a `now` three seconds earlier.
        let target = chrono::DateTime::parse_from_rfc2822("Sat, 22 Aug 2026 12:00:00 GMT").expect("fixture parses");
        let now = UNIX_EPOCH + Duration::from_secs(u64::try_from(target.timestamp()).unwrap() - 3);
        assert_eq!(
            parse_retry_after("Sat, 22 Aug 2026 12:00:00 GMT", now),
            Some(Duration::from_secs(3)),
            "the HTTP-date form must resolve to an interval, not be ignored"
        );
    }

    #[test]
    fn a_past_dated_http_date_is_zero_and_never_a_wrapped_duration() {
        let target = chrono::DateTime::parse_from_rfc2822("Sat, 22 Aug 2026 12:00:00 GMT").expect("fixture parses");
        // `now` a full day *after* the stated date.
        let now = UNIX_EPOCH + Duration::from_secs(u64::try_from(target.timestamp()).unwrap() + 86_400);
        assert_eq!(
            parse_retry_after("Sat, 22 Aug 2026 12:00:00 GMT", now),
            Some(Duration::ZERO),
            "a stale date must resolve to zero — a subtraction the wrong way round wraps into centuries"
        );
    }

    #[test]
    fn an_unparseable_retry_after_falls_back_to_jitter_rather_than_to_a_wrong_sleep() {
        let now = UNIX_EPOCH + Duration::from_secs(1_000_000);
        for value in [
            "",
            "   ",
            "soon",
            "-5",
            "2.5",
            "Tomorrow",
            "Sat, 99 Xxx 2026 12:00:00 GMT",
        ] {
            assert_eq!(
                parse_retry_after(value, now),
                None,
                "`{value}` is not a usable Retry-After; None is what routes it to normal backoff"
            );
        }
    }

    #[test]
    fn a_retry_after_above_the_clamp_stops_the_ladder_instead_of_sleeping_it_out() {
        let policy = RetryPolicy::default();
        assert_eq!(
            policy.delay_for(0, Some(Duration::from_secs(86_400))),
            RetryDelay::StopRetrying,
            "CWE-400: one header must not freeze an unattended sync for a day"
        );
        assert_eq!(
            policy.delay_for(0, Some(Duration::from_secs(30))),
            RetryDelay::After(Duration::from_secs(30)),
            "exactly at the clamp is still honoured"
        );
        assert_eq!(
            policy.delay_for(0, Some(Duration::from_secs(31))),
            RetryDelay::StopRetrying
        );
    }

    /// Virtual time, per the ADR: a wall-clock assertion under a paused clock
    /// cannot tell "slept 2 s" from "returned immediately".
    #[tokio::test(start_paused = true)]
    async fn a_stated_retry_after_is_slept_in_full() {
        let policy = RetryPolicy::default();
        let budget = RetryBudget::new();
        let start = Instant::now();
        let outcome: &str = run(&policy, &budget, |index| async move {
            if index == 0 {
                Attempt::Retry {
                    retry_after: Some(Duration::from_secs(2)),
                    terminal: "gave up",
                }
            } else {
                Attempt::Done("recovered")
            }
        })
        .await;
        assert_eq!(outcome, "recovered");
        assert!(
            start.elapsed() >= Duration::from_secs(2),
            "the server-stated interval must be waited out, got {:?}",
            start.elapsed()
        );
    }

    /// ACR counts `Retry-After` *down* across polls, so the second wait must
    /// use the second header, never the first value seen.
    #[tokio::test(start_paused = true)]
    async fn each_retry_reads_its_own_retry_after() {
        let policy = RetryPolicy {
            attempts: 3,
            ..RetryPolicy::default()
        };
        let budget = RetryBudget::new();
        let marks = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = std::sync::Arc::clone(&marks);
        let start = Instant::now();
        let outcome: &str = run(&policy, &budget, move |index| {
            let recorder = std::sync::Arc::clone(&recorder);
            async move {
                recorder.lock().unwrap().push(start.elapsed());
                match index {
                    0 => Attempt::Retry {
                        retry_after: Some(Duration::from_secs(5)),
                        terminal: "gave up",
                    },
                    1 => Attempt::Retry {
                        retry_after: Some(Duration::from_secs(1)),
                        terminal: "gave up",
                    },
                    _ => Attempt::Done("recovered"),
                }
            }
        })
        .await;
        assert_eq!(outcome, "recovered");
        let marks = marks.lock().unwrap().clone();
        assert_eq!(marks.len(), 3, "three attempts, two waits");
        assert_eq!(
            marks[1] - marks[0],
            Duration::from_secs(5),
            "first wait uses the first header"
        );
        assert_eq!(
            marks[2] - marks[1],
            Duration::from_secs(1),
            "second wait must use the *new*, smaller header — caching the first value seen is the ACR bug"
        );
    }

    // ── C-018 · jitter ───────────────────────────────────────────────────────

    #[tokio::test(start_paused = true)]
    async fn the_first_retry_delay_is_jittered_across_the_full_ceiling() {
        let policy = RetryPolicy::default();
        let mut observed = std::collections::BTreeSet::new();
        for _ in 0..20 {
            let budget = RetryBudget::new();
            let start = Instant::now();
            let _: &str = run(&policy, &budget, |index| async move {
                if index == 0 {
                    Attempt::Retry {
                        retry_after: None,
                        terminal: "gave up",
                    }
                } else {
                    Attempt::Done("recovered")
                }
            })
            .await;
            let delay = start.elapsed();
            assert!(
                delay <= Duration::from_millis(250),
                "full jitter draws from [0, base] on the first retry, got {delay:?}"
            );
            observed.insert(delay);
        }
        assert!(
            observed.len() >= 2,
            "deterministic backoff produces 20 identical values and reproduces the lockstep spike; saw {observed:?}"
        );
    }

    #[test]
    fn the_backoff_ceiling_doubles_then_saturates_at_the_cap() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.backoff_ceiling(0), Duration::from_millis(250));
        assert_eq!(policy.backoff_ceiling(1), Duration::from_millis(500));
        assert_eq!(policy.backoff_ceiling(2), Duration::from_secs(1));
        assert_eq!(policy.backoff_ceiling(4), Duration::from_secs(3), "capped");
        assert_eq!(policy.backoff_ceiling(64), Duration::from_secs(3), "no shift overflow");
    }

    // ── C-019 · the budget ───────────────────────────────────────────────────

    #[test]
    fn the_floor_admits_the_first_ten_retries_then_the_ratio_binds() {
        let budget = RetryBudget::new();
        // One attempt in: without the floor, `1/1 > 0.1` would refuse.
        budget.record_attempt();
        for n in 1..=RETRY_FLOOR {
            assert!(budget.try_admit_retry(), "retry {n} is inside the floor");
        }
        assert!(
            !budget.try_admit_retry(),
            "past the floor with total=1 the ratio must bind immediately"
        );
        // Grow the run: at total=110 the ratio admits an 11th retry.
        for _ in 0..109 {
            budget.record_attempt();
        }
        assert!(budget.try_admit_retry(), "110 total admits 11 retries at 1/10");
        assert!(!budget.try_admit_retry(), "but not a 12th");
        assert_eq!(budget.counts(), (110, 11));
    }

    #[test]
    fn a_cloned_budget_meters_against_one_pool() {
        let budget = RetryBudget::new();
        let clone = budget.clone();
        budget.record_attempt();
        clone.record_attempt();
        for _ in 0..RETRY_FLOOR {
            assert!(clone.try_admit_retry());
        }
        assert!(
            !budget.try_admit_retry(),
            "the clone's spending must be visible through the original — a plain field would make this per-clone"
        );
        assert_eq!(budget.counts(), (2, 10));
    }

    /// C-019's edge case, asserted on **virtual** elapsed time: once the budget
    /// is gone the ladder must fail without sleeping.
    #[tokio::test(start_paused = true)]
    async fn an_exhausted_budget_fails_immediately_instead_of_sleeping() {
        let policy = RetryPolicy::default();
        let budget = RetryBudget::new();
        // Spend the floor and pin the ratio closed (total stays tiny).
        for _ in 0..RETRY_FLOOR {
            assert!(budget.try_admit_retry());
        }
        assert!(!budget.try_admit_retry(), "precondition: the budget is spent");

        let attempts = std::sync::Arc::new(AtomicU64::new(0));
        let counter = std::sync::Arc::clone(&attempts);
        let start = Instant::now();
        let outcome: &str = run(&policy, &budget, move |_| {
            let counter = std::sync::Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::Relaxed);
                Attempt::Retry {
                    retry_after: None,
                    terminal: "gave up",
                }
            }
        })
        .await;
        assert_eq!(outcome, "gave up");
        assert_eq!(attempts.load(Ordering::Relaxed), 1, "no retry was issued");
        assert_eq!(
            start.elapsed(),
            Duration::ZERO,
            "a budget that still sleeps is the amplification the budget exists to prevent"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_attempt_cap_bounds_the_ladder() {
        let policy = RetryPolicy::default();
        let budget = RetryBudget::new();
        let attempts = std::sync::Arc::new(AtomicU64::new(0));
        let counter = std::sync::Arc::clone(&attempts);
        let outcome: &str = run(&policy, &budget, move |_| {
            let counter = std::sync::Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::Relaxed);
                Attempt::Retry {
                    retry_after: None,
                    terminal: "gave up",
                }
            }
        })
        .await;
        assert_eq!(outcome, "gave up");
        assert_eq!(
            attempts.load(Ordering::Relaxed),
            3,
            "3 attempts ships: initial plus two retries"
        );
        assert_eq!(budget.counts(), (3, 2));
    }

    #[tokio::test(start_paused = true)]
    async fn a_terminal_outcome_is_never_retried() {
        let policy = RetryPolicy::default();
        let budget = RetryBudget::new();
        let attempts = std::sync::Arc::new(AtomicU64::new(0));
        let counter = std::sync::Arc::clone(&attempts);
        let outcome: &str = run(&policy, &budget, move |_| {
            let counter = std::sync::Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::Relaxed);
                Attempt::Done("absent")
            }
        })
        .await;
        assert_eq!(outcome, "absent");
        assert_eq!(attempts.load(Ordering::Relaxed), 1);
        assert_eq!(budget.counts(), (1, 0), "a terminal attempt spends no retry budget");
    }

    // ── jitter source ────────────────────────────────────────────────────────

    #[test]
    fn full_jitter_stays_inside_its_ceiling() {
        for _ in 0..100 {
            let drawn = full_jitter(Duration::from_millis(250));
            assert!(drawn < Duration::from_millis(250), "{drawn:?} escaped the ceiling");
        }
        assert_eq!(full_jitter(Duration::ZERO), Duration::ZERO);
    }

    /// Draws taken close together must not come out close together.
    ///
    /// **Spread, not distinctness** — and the difference is the whole test.
    /// Distinctness does not discriminate: a `SystemTime`-per-draw source still
    /// returns 64 *different* nanosecond values, they just all sit within a
    /// few microseconds of each other, because the draw is a function of a
    /// clock that barely moved. That cluster *is* the lockstep, and it only
    /// became visible in C-018's ladder test because tokio's millisecond timer
    /// rounded the cluster to one value. Asserting the sample spans at least
    /// half its ceiling reds on the clock-bound source directly, without
    /// depending on any timer's rounding or on the host's clock granularity.
    #[test]
    fn full_jitter_draws_are_spread_across_the_ceiling_not_clustered_by_the_clock() {
        let ceiling = Duration::from_secs(3);
        let drawn: Vec<Duration> = (0..64).map(|_| full_jitter(ceiling)).collect();
        let low = *drawn.iter().min().expect("64 draws");
        let high = *drawn.iter().max().expect("64 draws");
        assert!(
            high - low > ceiling / 2,
            "64 draws spanned only {:?} of a {ceiling:?} ceiling — a sample this tight is a clock read, \
             not a jitter draw, and retriers sharing a clock retry in lockstep",
            high - low
        );
    }
}

/// [`is_retryable_transport_error`]'s h2 branch, against a real HTTP/2 peer.
///
/// The branch that matters in production and the one no fixture in this tree
/// could reach: `RST_STREAM` and `GOAWAY` are h2 frames, and every other wire
/// fixture here — and the acceptance-suite index server — speaks HTTP/1.x, so
/// the h2 classification had never been exercised in either direction.
///
/// The peer speaks h2c under prior knowledge rather than h2-over-TLS. The
/// classifier walks the error `reqwest`/`hyper`/`h2` build above the socket,
/// and h2 builds that error identically whichever transport carries the
/// frames — a TLS wrapper would add a certificate fixture and no
/// discriminating power.
#[cfg(test)]
mod h2_wire_tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    const BODY: &[u8] = br#"{"formatVersion":1}"#;

    /// How the peer fails a request while it is still failing.
    #[derive(Clone, Copy)]
    enum Failure {
        /// `RST_STREAM(REFUSED_STREAM)` before any response head. RFC 9113 §8.7
        /// defines it as "not processed, retry elsewhere" — the frame a CDN
        /// sheds a stream with.
        RefuseStream,
        /// A response head, part of a body, then `RST_STREAM(INTERNAL_ERROR)` —
        /// the same class arriving after the client has committed to reading.
        ResetMidResponse,
    }

    /// An h2 endpoint that fails every request with `failure` until
    /// [`H2Endpoint::recover`] is called, and serves [`BODY`] after.
    ///
    /// A *flag*, not a scripted sequence indexed by request number: `hyper`
    /// re-issues a `REFUSED_STREAM` request on its own before surfacing it, so
    /// one attempt of the ladder above can be several requests down here. The
    /// flag makes the fixture independent of how many.
    struct H2Endpoint {
        address: String,
        failing: Arc<AtomicBool>,
        served: Arc<AtomicUsize>,
    }

    impl H2Endpoint {
        async fn start(failure: Failure) -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap().to_string();
            let failing = Arc::new(AtomicBool::new(true));
            let served = Arc::new(AtomicUsize::new(0));

            let state = (Arc::clone(&failing), Arc::clone(&served));
            tokio::spawn(async move {
                while let Ok((socket, _)) = listener.accept().await {
                    let (failing, served) = (Arc::clone(&state.0), Arc::clone(&state.1));
                    tokio::spawn(async move { serve(socket, failure, failing, served).await });
                }
            });

            Self {
                address,
                failing,
                served,
            }
        }

        fn url(&self) -> String {
            format!("http://{}/p/ocx.sh/tool.json", self.address)
        }

        fn recover(&self) {
            self.failing.store(false, Ordering::SeqCst);
        }

        fn served(&self) -> usize {
            self.served.load(Ordering::SeqCst)
        }
    }

    async fn serve(
        socket: tokio::net::TcpStream,
        failure: Failure,
        failing: Arc<AtomicBool>,
        served: Arc<AtomicUsize>,
    ) {
        let Ok(mut connection) = h2::server::handshake(socket).await else {
            return;
        };
        while let Some(accepted) = connection.accept().await {
            let Ok((_request, mut respond)) = accepted else {
                return;
            };
            served.fetch_add(1, Ordering::SeqCst);
            if !failing.load(Ordering::SeqCst) {
                let response = http::Response::builder().status(200).body(()).unwrap();
                if let Ok(mut stream) = respond.send_response(response, false) {
                    let _ = stream.send_data(bytes::Bytes::from_static(BODY), true);
                }
                continue;
            }
            match failure {
                Failure::RefuseStream => respond.send_reset(h2::Reason::REFUSED_STREAM),
                Failure::ResetMidResponse => {
                    let response = http::Response::builder().status(200).body(()).unwrap();
                    if let Ok(mut stream) = respond.send_response(response, false) {
                        let _ = stream.send_data(bytes::Bytes::from_static(b"{\"formatVer"), false);
                        stream.send_reset(h2::Reason::INTERNAL_ERROR);
                    }
                }
            }
        }
    }

    /// The production hardening at fixture speed. `http2_prior_knowledge` is
    /// what stands in for the ALPN negotiation a real TLS index does.
    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .http2_prior_knowledge()
            .connect_timeout(Duration::from_secs(2))
            .read_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("a client with only timeout and redirect settings always builds")
    }

    /// Reads the whole body the way the index transport does, so a failure
    /// arriving mid-response is classified through the same call the production
    /// body loop makes.
    async fn fetch(client: &reqwest::Client, url: &str) -> reqwest::Result<Vec<u8>> {
        let mut response = client.get(url).send().await?;
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    #[tokio::test]
    async fn an_h2_stream_reset_is_retryable() {
        for failure in [Failure::RefuseStream, Failure::ResetMidResponse] {
            let endpoint = H2Endpoint::start(failure).await;
            let error = fetch(&client(), &endpoint.url())
                .await
                .expect_err("the peer resets every stream");
            assert!(
                !error.is_connect() && !error.is_timeout(),
                "precondition: an h2 reset is neither a connect failure nor a timeout, so the two \
                 predicates the old rule led with cannot see it"
            );
            let mut source: Option<&(dyn std::error::Error + 'static)> = Some(&error);
            let mut chain = Vec::new();
            while let Some(current) = source {
                assert!(
                    !current.is::<std::io::Error>(),
                    "precondition: the chain reaches no io::Error — h2::Error overrides no source(), \
                     so walking for one classifies every h2 reset as terminal; chain was {chain:?}"
                );
                chain.push(current.to_string());
                source = current.source();
            }
            assert!(
                is_retryable_transport_error(&error),
                "RFC 9113 §8.7 names RST_STREAM as the frame a peer sends to say 'retry this \
                 elsewhere'; refusing to retry it fails a whole index sync on one shed stream. \
                 Error chain: {chain:?}"
            );
        }
    }

    /// End to end: the ladder must actually recover, not merely classify.
    #[tokio::test]
    async fn the_ladder_recovers_from_an_h2_stream_reset() {
        let endpoint = H2Endpoint::start(Failure::RefuseStream).await;
        let client = client();
        let url = endpoint.url();
        let policy = RetryPolicy::default();
        let budget = RetryBudget::new();

        let outcome = run(&policy, &budget, |attempt| {
            let (client, url) = (client.clone(), url.clone());
            if attempt > 0 {
                endpoint.recover();
            }
            async move {
                match fetch(&client, &url).await {
                    Ok(body) => Attempt::Done(Ok(body)),
                    Err(error) if is_retryable_transport_error(&error) => Attempt::Retry {
                        retry_after: None,
                        terminal: Err(error),
                    },
                    Err(error) => Attempt::Done(Err(error)),
                }
            }
        })
        .await;

        let body = outcome.unwrap_or_else(|error| {
            panic!("the ladder must survive one shed stream, but gave up with: {error}");
        });
        assert_eq!(body, BODY, "the recovered attempt's body is what the caller gets");
        assert!(
            endpoint.served() >= 2,
            "the peer must have seen a second request — a pass without one would mean the fixture \
             never failed at all, got {}",
            endpoint.served()
        );
        assert_eq!(
            budget.counts(),
            (2, 1),
            "two attempts, exactly one retry charged against the run-global budget"
        );
    }
}
