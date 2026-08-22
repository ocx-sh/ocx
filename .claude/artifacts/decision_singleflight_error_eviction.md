<!-- SPDX-License-Identifier: Apache-2.0 -->

# Decision: singleflight error eviction

Scope: `crates/ocx_lib/src/utility/singleflight.rs`, its four consumers, and
ADR `adr_index_sync_performance.md` D-005 (`Group` on `OcxIndex` / `OciIndex`
for process-lifetime memoization).

## Question

Should `utility::singleflight::Group` evict a key **before** broadcasting a
failure — so only a successful value is retained — or should the ADR's new
process-lifetime use be confined to a narrower scope instead?

## Evidence

### The four stated facts

| # | Claim | Verdict | Lines |
|---|---|---|---|
| 1 | `try_acquire` returns `Err(e)` to every later caller after a leader broadcast an error | **confirmed** | `singleflight.rs:210-214` |
| 2 | Doc says resolved entries are retained for the group's lifetime | **confirmed** | `singleflight.rs:155-158` |
| 3 | No eviction API; `new` and `try_acquire` are the only methods | **confirmed** | `singleflight.rs:188-238`; zero `remove` in the file |
| 4 | `Handle::drop` broadcasts `Error::Abandoned`, retained the same way — a cancelled leader poisons the key | **confirmed** | `singleflight.rs:139-145` + `210-214` |

Detail:

- **(1)** `singleflight.rs:210-214` — the map hit path reads the current watch
  value under the `entries` lock and dispatches on it:
  ```rust
  if let Some(rx) = entries.get(&key) {
      let current = rx.borrow().clone();
      match current {
          Some(Ok(value)) => return Ok(Acquisition::Resolved(value)),
          Some(Err(e)) => return Err(e),
          None => rx.clone(),
      }
  ```
  `Some(Err(e)) => return Err(e)` is unconditional and has no expiry: a
  resolved-to-error entry answers every future `try_acquire` for that key.
- **(2)** `singleflight.rs:155-158`: "Resolved entries are retained for the
  group's lifetime … Scope the group to a single logical operation so entries
  are freed when the group is dropped." The doc states the mitigation as
  *scope the group*, not *evict*.
- **(3)** The only `impl` block carrying methods on `Group<K, V>` is
  `singleflight.rs:188`, containing exactly `new` (`:194`) and `try_acquire`
  (`:207`); `impl Clone` (`:178`) adds none. `grep -n "remove"` over the file
  returns **0 matches** — no code path ever takes a key out of `entries`.
- **(4)** `singleflight.rs:139-145`:
  ```rust
  impl<V> Drop for Handle<V> {
      fn drop(&mut self) {
          if let Some(sender) = self.sender.take() {
              let _ = sender.send(Some(Err(Error::Abandoned)));
          }
      }
  }
  ```
  The entry stays in the map holding `Some(Err(Abandoned))`, so `:214` serves
  `Err(Abandoned)` to every later caller. A leader that is merely *cancelled*
  — a `select!` loser, an aborted `JoinSet`, a `?` on an unrelated error
  higher in the same task — permanently poisons a key that was never even
  attempted to completion.

### `max_entries` interaction (asked, and it is real)

`singleflight.rs:218` admits a new key only while `entries.len() < max_entries`:

```rust
if entries.len() >= self.max_entries {
    return Err(Error::CapacityExceeded { max: self.max_entries });
}
```

`entries.len()` counts poisoned entries exactly like successful ones, and
nothing removes either. So a failed or abandoned key consumes one capacity
slot for the group's lifetime. On a long-lived group this compounds the
primary defect: after `max_entries` distinct failures the group stops
admitting **any** new key and every unrelated call returns
`CapacityExceeded` → `ExitCode::TempFail` (75) (`singleflight.rs:253`) — a
process-lifetime denial of service whose exit code advertises "retry, this is
transient" while retrying inside the same process can never clear it.

### Consumer inventory

Found by three independent single-term greps (`singleflight`, `try_acquire`,
`Acquisition`) and by opening each file. Note `TempStore::try_acquire`
(`file_structure/temp_store.rs:150`) is an unrelated file-lock API with a
colliding name — not a `Group` consumer.

| # | Consumer | Type | Group lifetime | Can observe a retained error? | Eviction changes behaviour a test asserts? |
|---|---|---|---|---|---|
| 1 | `ChainedIndex::singleflight` (`chained_index.rs:200`) | `Group<String, Option<(Digest, Manifest)>>` | **Process** — long-lived field; built once at `context.rs:292`; `box_clone` (`:1481`) shares the group across every cloned chain | **Yes** | No |
| 2 | `PullCoordinator::write_group` (`pull_local.rs:27`) | `Group<Digest, ()>` | Per `pull_local` call (`pull_local.rs:135`) | Only within one pull | No |
| 3 | `SetupGroups::packages` (`pull.rs:54,73`) | `Group<PinnedIdentifier, InstallInfo>` | Per `pull` / `pull_all` (`pull.rs:122`, `:157`) and per `pull_local` (`pull_local.rs:199`) | Only within one pull | No |
| 4 | `SetupGroups::layers` (`pull.rs:60,74`) | `Group<(String, Digest), ()>` | Same as #3 | Only within one pull | No |
| — | (proposed) `OcxIndex::{check_format_version, resolve_root}`, `OciIndex::Cache` tag maps — ADR D-005 | — | **Process** | **Yes** | n/a |

Consumers #2–#4 are exactly the shape the doc prescribes: "Scope the group to a
single logical operation" (`singleflight.rs:157-158`). **Consumer #1 is not** —
it already violates the doc's own mitigation, so the poisoning defect is
present in the tree today; D-005 does not introduce it, it widens it.

**The abandonment mechanism is live, confirmed by code, on consumer #1.**
`project::resolve::resolve_work` (`project/resolve.rs:114-144`) spawns one
resolver task per tool over a single shared `Arc<Index>` and calls
`set.abort_all()` (`:138`) on the first error. An aborted task that held the
singleflight leadership for its identifier has its `Handle` dropped mid-flight
→ `Error::Abandoned` broadcast → that key is poisoned in the shared group for
the rest of the process. No user-visible failure follows *today* only because
`resolve_work` returns `Err` immediately and the one-shot CLI exits. The moment
a group outlives one failure — a long-running `ocx index sync`, a plugin, or
`ocx-mirror` linking `ocx_lib` — the same mechanism is a permanent poison.

### Test run — actual output

Test module names resolved first (`cargo test -p ocx_lib --lib singleflight -- --list`):
the primitive's own tests live in `utility::singleflight::tests`, the coordinator's
in `package_manager::tasks::pull_local::tests`.

```
$ cargo test -p ocx_lib --lib singleflight
    Finished `test` profile [unoptimized + debuginfo] target(s) in 54.68s
     Running unittests src/lib.rs (target/debug/deps/ocx_lib-5d4d80fc5ff1bd37)
cargo test: 20 passed, 4621 filtered out (1 suite, 0.08s)

$ cargo test -p ocx_lib --lib pull_coordinator
     Running unittests src/lib.rs (target/debug/deps/ocx_lib-5d4d80fc5ff1bd37)
cargo test: 2 passed, 4639 filtered out (1 suite, 0.04s)
```

The 20 comprise 13 in `utility::singleflight::tests`, 3 in
`chained_index::chain_refs_tests`, 2 in `cli::classify::tests`, 1 in
`oci::index::error::tests`, 1 in `pull_local::tests`.

### Which tests depend on error retention — measured, not predicted

The brief forbids editing `crates/`, so the mutation was run against a
standalone copy of the primitive
(`<scratchpad>/sfprobe/src/lib.rs`, lines 1-239 **verified byte-identical** to
`singleflight.rs:1-239` by `diff` after restore; only the
`ClassifyExitCode` impl, which needs the `ocx_lib` crate, was omitted).

Baseline, unmutated: **13 passed, 0 failed.**

With eviction applied:

```
test tests::failed_error_is_durable_across_multiple_acquires ... FAILED
test tests::subsequent_acquire_after_failure_returns_error ... FAILED
...
thread 'tests::failed_error_is_durable_across_multiple_acquires' panicked at src/lib.rs:440:57:
called `Result::unwrap_err()` on an `Ok` value: Leader("...")
thread 'tests::subsequent_acquire_after_failure_returns_error' panicked at src/lib.rs:408:53:
called `Result::unwrap_err()` on an `Ok` value: Leader("...")
test result: FAILED. 11 passed; 2 failed
```

**Exactly two tests depend on error retention:**

| Test | Line | What it pins |
|---|---|---|
| `subsequent_acquire_after_failure_returns_error` | `singleflight.rs:406-419` (assert at `:414`) | a later acquire re-receives the leader's `Failed` |
| `failed_error_is_durable_across_multiple_acquires` | `singleflight.rs:437-452` (assert at `:446`) | it does so three times running |

Both are *statements of the current decision*, not coverage of a defect. Under
the change they are rewritten, not deleted — see "Red-reachable test".

**Everything else stayed green, including the two that matter most:**

- `failed_leader_propagates_error_to_waiters` (`:385-404`) — **passed**. This
  is the proof that eviction does not touch in-flight broadcast: a concurrent
  waiter already holds `rx.clone()` from the wait path (`:228-233`) and never
  re-consults the map, so it still receives the leader's error.
- `abandoned_handle_signals_error` (`:341-357`) — **passed**, same reason.
- `capacity_exceeded` (`:375-383`) and
  `complete_between_borrow_and_wait_is_caught` (`:454-473`) — **passed**.

The two consumer-side error tests were read and are unaffected by eviction:
`pull_coordinator_returns_singleflight_error_on_leader_failure`
(`pull_local.rs:1291-1313`) asserts only the *leader's own* return value, which
is unchanged; `singleflight_broadcasts_source_error_to_waiters`
(`chained_index.rs:2304-2358`) asserts `error_count > 0` over three concurrent
tasks against an always-erroring source — green whether a late arrival waits or
re-leads, since the source errors either way.

## Options considered

**Option 1 — eviction-on-failure in the shared primitive.** A resolved-to-error
entry is dropped and the asking caller is handed fresh leadership. Two sub-shapes:

- **1a, evict on read** — `try_acquire` drops the entry when it finds
  `Some(Err(_))`. ~10 lines, inside the lock it already holds. No back-reference,
  no `Drop` change, no signature change.
- **1b, evict on write** — `Handle` gains `Arc<Mutex<HashMap<..>>>` + `K` and
  removes the key in `fail()` and in `Drop`. Buys one thing over 1a: a failed key
  frees its capacity slot *immediately* rather than when that key is next asked.
  Costs: `Drop` cannot `.await`, so the map's `tokio::sync::Mutex` must become a
  `std::sync::Mutex` (safe — it is never held across an await, `singleflight.rs:227`)
  or the removal degrades to a best-effort `try_lock`; plus a `K` field on every
  `Handle`.

**Option 2 — confine the new use to a narrower scope.** Leave the primitive
alone; give D-005 a per-operation group (per `refresh_tags` call, per
`index sync` package). Rejected on three counts: (i) it does not fix consumer #1,
which is already process-lifetime and already carries the defect; (ii) it defeats
D-005's purpose — C-007 asks for "at most once **per process**, per source",
which a per-operation group cannot deliver; (iii) it leaves a shared primitive
whose failure mode is "poisons a key forever" available to the next caller who
reaches for it, which is exactly how consumer #1 got here.

**Option 3 — a TTL / negative-cache expiry on error entries.** Rejected: YAGNI.
It needs a clock, a policy value, and a test that manipulates time, to answer a
question ("how long is this failure true for?") the codebase asks nowhere else.
The correct lifetime of a failure is zero.

**Option 4 — leave it, document it, make every consumer defensive.** Rejected:
it pushes onto four call sites (soon six) an invariant that one arm in the
primitive holds for all of them.

## Decision

**Eviction-on-failure in the shared primitive, shape 1a (evict on read).**

1. **A failure is not a result.** The primitive's job is to make sure work happens
   once, not to remember that it once did not. Every other cache in this subsystem
   already draws that line explicitly: `resolve_root` memoizes a **confirmed 404**
   and nothing else (`ocx_index.rs:800-806`); `check_format_version` memoizes only
   a *served* document, never an assumed-v1 and never an unsupported version
   (`ocx_index.rs:760-767`, `:783-789`). `singleflight` is the one place that
   memoizes failure, and it is the one place that never decided to — the doc at
   `:155-158` says "resolved entries", and treats scoping, not eviction, as the
   mitigation.

2. **Retention makes D-005 unimplementable as written.** The ADR's own contracts
   forbid it: D-004 — "Any other status is `IndexHttpFailed` and **caches
   nothing**"; C-006 edge case (b) — "a **non-404** failure — nothing is memoized,
   and a repeat ask re-requests". `check_format_version` is worse still: its doc
   promises a transport failure "propagates as a hard error on **every call**"
   (`ocx_index.rs:766-768`), which a retained `Err` converts into *propagates the
   first error forever* — so the message the user sees stops being the current
   truth about the index.

3. **Fail-closed jurisdiction turns a transient error into a permanent namespace
   outage.** Per `subsystem-oci.md`, a root fetch that errors keeps the source
   `Authoritative`, and "nothing is cached on failure, so the immediately-following
   `resolve_root` re-fetches and raises the real error". Jurisdiction is re-entered
   from **every** `candidate_sources` call. Under retention, one 503 blip on one
   name pins that source `Authoritative` for that name — and `Authoritative` is a
   terminal stop, so no other source may answer it — for the rest of the process,
   with no way to clear it. That is the process-lifetime denial of service, and its
   cause is the primitive, not the ADR.

4. **`Abandoned`-on-drop is a bug for the existing consumers too, independent of
   the ADR.** A cancelled leader has learned nothing. It is the one error the
   primitive manufactures itself, and it is retained on the same terms as a real
   failure. `project/resolve.rs:138`'s `abort_all()` fires it today.

**Is retrying after an error ever wrong for these consumers?** No — each was
checked. #2/#3/#4 are per-operation groups where a second ask for the same key
after a failure is rare, and where retrying a blob write / package setup / layer
extraction is what a caller wants: all three are idempotent and content-addressed.
#1 and D-005 are network reads whose failures are transient by nature. Nothing in
the tree treats a singleflight failure as a decision that must not be revisited.

**Does eviction let a hot failure loop re-issue work without bound?** Not
meaningfully, and it is bounded elsewhere. Eviction adds no *new* call — it
restores the arity the caller would have had with no group at all. Concurrent
callers still coalesce onto one leader; only a *later, serial* caller re-issues,
so the worst case is one request per serial ask, which is what every non-memoized
path already does. The ADR's per-run retry-ratio budget on the index transport
sits underneath this, and `max_entries` still caps in-flight keys.

**Why 1a over 1b.** 1b's only gain is that a failed key costs zero capacity slots
instead of one. But a *successful* key costs one slot too — so under 1a a failure
is no more expensive than a success (parity, not a penalty), and `max_entries` has
to be sized for the successes regardless. A `Drop` body, a back-reference on every
`Handle`, and a mutex-type change are not worth buying that. 1b stays available if
a capacity problem is ever measured.

### Two things eviction does *not* fix — D-005 still needs both

1. **C-007's assumed-v1 case.** `check_format_version` deliberately does not
   memoize the 404 → `assumed_v1()` result (`ocx_index.rs:783-789`; ADR C-007: "A
   coalescing group that memoized the assumed-v1 result would break that contract
   silently"). That value is `Ok`, so eviction-on-failure leaves it memoized. The
   `Group` must cover only the served-document case, or that arm must bypass the
   group.
2. **`max_entries` sizing.** Copying `SINGLEFLIGHT_MAX_KEYS = 1024` from
   `chained_index.rs` into a process-lifetime `OcxIndex` group means
   `ocx index sync` against a registry holding more than 1024 packages hits
   `CapacityExceeded` → `TempFail(75)` on **successes** alone. D-005 says
   "Precedent to mirror: … `SINGLEFLIGHT_MAX_KEYS = 1024`" — that number was
   chosen for per-identifier refresh, not for a whole-registry sync. Size it for
   the run.

## Change sketch

One arm in `try_acquire`. No `Drop` change, no back-reference on `Handle`, no
signature change, no new bound. `try_acquire` still returns
`Result<Acquisition<V>, Error>` — the `Err` side keeps `Timeout`,
`CapacityExceeded`, and the wait-path `Failed`/`Abandoned` a *concurrent* waiter
receives; only the map-hit `Some(Err(_))` arm changes.

`crates/ocx_lib/src/utility/singleflight.rs:207-225`, replacing lines 208-225:

```rust
let mut rx = {
    let mut entries = self.entries.lock().await;
    // Read the state out first so the map borrow ends before the
    // `Some(Err(_))` arm re-inserts under the same key.
    let current = entries.get(&key).map(|rx| (rx.borrow().clone(), rx.clone()));
    if let Some((current, rx)) = current {
        match current {
            Some(Ok(value)) => return Ok(Acquisition::Resolved(value)),
            // A resolved-to-error entry is not an answer, it is the absence
            // of one: drop it and hand this caller fresh leadership so the
            // work is retried. Only a success is memoized. Replacing in
            // place leaves `entries.len()` unchanged, so a failed key never
            // holds a capacity slot hostage either. Waiters already blocked
            // on the OLD channel still receive the leader's broadcast — the
            // in-flight cohort's exit-code parity is untouched.
            Some(Err(_)) => {
                let (tx, rx) = watch::channel(None);
                entries.insert(key, rx);
                return Ok(Acquisition::Leader(Handle { sender: Some(tx) }));
            }
            None => rx,
        }
    } else {
        if entries.len() >= self.max_entries {
            return Err(Error::CapacityExceeded { max: self.max_entries });
        }
        let (tx, rx) = watch::channel(None);
        entries.insert(key, rx);
        return Ok(Acquisition::Leader(Handle { sender: Some(tx) }));
    }
};
```

**Where the removal sits relative to the broadcast.** *After* it, not before —
deliberately, and this is the load-bearing detail. Removing before the broadcast
(shape 1b) would be correct too, but removing on the **read** side means the
broadcast path is untouched: a waiter that entered `wait_for` (`:228-233`) holds
its own `watch::Receiver` clone and never re-consults the map, so it still
receives the leader's `Failed`/`Abandoned` verbatim. That preserves the property
D-005 explicitly chose `Group` for over `tokio::sync::OnceCell` — "all callers of
one logical operation get one outcome" — while dropping the property nobody chose:
that *callers of a later, different logical operation* inherit it.

**`Handle` and `Drop` are unchanged.** `Drop` keeps broadcasting `Abandoned`
(`:139-145`) — the in-flight cohort must still learn the leader vanished. What
changes is that the abandoned entry no longer answers for anyone who arrives
afterwards.

**Doc comment** (`:155-158`) must change with it:

```
/// A **successful** entry is retained for the group's lifetime so that later
/// callers (e.g. diamond dependencies discovered deeper in the tree) get an
/// instant cache hit instead of re-doing work. A failed or abandoned entry is
/// NOT an answer: the next caller to ask for that key drops it and is handed
/// fresh leadership, so the work is retried. Concurrent waiters already
/// blocked on the leader still receive its error — one in-flight operation,
/// one outcome.
```

### Test changes in the same commit

| Test | Line | Action |
|---|---|---|
| `subsequent_acquire_after_failure_returns_error` | `:406-419` | rewrite → `subsequent_acquire_after_failure_returns_a_fresh_leader` |
| `failed_error_is_durable_across_multiple_acquires` | `:437-452` | delete — superseded by the new retry test |
| `failed_leader_propagates_error_to_waiters` | `:385-404` | **keep unchanged** — it is the guard that eviction did not break in-flight broadcast |
| `abandoned_handle_signals_error` | `:341-357` | **keep unchanged** — same, for the `Drop` path |

## Red-reachable test

Two tests, both **measured** against a byte-identical standalone copy of the
primitive: green with the change, red without.

```rust
#[tokio::test]
async fn failed_key_is_retried_by_a_later_acquire() {
    let g = group(10);
    let Acquisition::Leader(handle) = g.try_acquire(key("key-a")).await.unwrap() else {
        panic!("expected Leader");
    };
    handle.fail(TestError("transient outage"));

    // A later, non-concurrent caller must be handed leadership so the work is
    // retried. A failure is the absence of an answer, not a cached one.
    let Acquisition::Leader(retry) = g.try_acquire(key("key-a")).await.unwrap() else {
        panic!("a failed key must be retryable, not poisoned for the group's lifetime");
    };
    retry.complete("recovered".to_owned());

    let Acquisition::Resolved(value) = g.try_acquire(key("key-a")).await.unwrap() else {
        panic!("expected Resolved");
    };
    assert_eq!(value, "recovered", "the retry's value must be the memoized one");
}

#[tokio::test]
async fn abandoned_key_is_retried_by_a_later_acquire() {
    let g = group(10);
    let Acquisition::Leader(handle) = g.try_acquire(key("key-a")).await.unwrap() else {
        panic!("expected Leader");
    };
    // A cancelled leader (a `select!` loser, an aborted `JoinSet` —
    // `project/resolve.rs:138` — or a `?` higher in the same task) must not
    // poison the key for every later caller.
    drop(handle);

    let Acquisition::Leader(retry) = g.try_acquire(key("key-a")).await.unwrap() else {
        panic!("an abandoned key must be reclaimable by the next caller");
    };
    retry.complete("recovered".to_owned());
}
```

**Both outcomes demonstrated.** The mutation is the change itself; the discriminating
run is the *restore*.

With the change applied (13 tests + the 2 new = 15; the 2 failures are the old
retention tests the change replaces):

```
test tests::abandoned_key_is_retried_by_a_later_acquire ... ok
test tests::failed_key_is_retried_by_a_later_acquire ... ok
test tests::failed_leader_propagates_error_to_waiters ... ok
test result: FAILED. 13 passed; 2 failed
```

After reverting the primitive to the shipped text — and **proving the restore
landed**, `diff` of the probe's lines 1-239 against `singleflight.rs:1-239`
reporting *identical*:

```
test tests::abandoned_key_is_retried_by_a_later_acquire ... FAILED
test tests::failed_key_is_retried_by_a_later_acquire ... FAILED
test tests::failed_error_is_durable_across_multiple_acquires ... ok
test tests::subsequent_acquire_after_failure_returns_error ... ok
test tests::failed_leader_propagates_error_to_waiters ... ok
test result: FAILED. 13 passed; 2 failed
```

Both new tests fail on the shipped primitive at
`called `Result::unwrap_err()`…`-equivalent panics on the `else` arms
(`a failed key must be retryable…`, `an abandoned key must be reclaimable…`).
They are checks, not habits.

**The mutation that proves they discriminate the right thing.** Deleting only the
`Some(Err(_))` eviction arm — restoring `Some(Err(e)) => return Err(e)` — reds
both, and reds nothing else. That is the narrow discrimination wanted: neither
test can be satisfied by the `Ok` path, by the wait path, or by capacity
handling. Note the converse also held and is the reason
`failed_leader_propagates_error_to_waiters` must stay in the file: it is green in
**both** states, so it alone proves nothing about eviction — it is the guard
against over-correcting into a primitive that drops the in-flight broadcast, and
a change that broke that would red it while leaving the two new tests green.

## Not verified

- No repro was constructed for a *user-visible* failure on consumer #1 today.
  The mechanism is confirmed from code (`project/resolve.rs:138` +
  `chained_index.rs:200,1481` + `context.rs:292`), but the one-shot CLI exits
  immediately after `resolve_work` returns `Err`, so the poisoned key is never
  asked again in that process. The harm is prospective (long-running commands,
  plugins, `ocx-mirror`) and immediate for D-005.
- The change was measured against a standalone copy, not against `ocx_lib`
  itself — the brief forbade editing `crates/`. The copy was proved
  byte-identical over the primitive's lines 1-239; the omitted lines 240-256 are
  the `ClassifyExitCode` impl, which `try_acquire` does not touch.
