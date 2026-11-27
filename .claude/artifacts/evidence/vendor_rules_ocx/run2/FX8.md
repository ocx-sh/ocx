# FX8: flaky macOS `forge::github_packages` tests

## Root cause
`crates/ocx_announce/src/forge/github_packages.rs` (test module, `#[tokio::test(start_paused = true)]` on
`a_404_on_a_listed_root_whose_tag_is_still_served_is_delete_denied` and
`a_404_on_a_root_whose_tag_is_gone_is_reported_deleted`) combined with the real-socket `Fake` loopback peer.
Under a paused tokio clock, whenever the runtime has nothing ready and waits on I/O, the clock auto-advances to the next
timer. That timer is the deleter client's 5 s request timeout (`deleter()`, `build_forge_http_client(5 s)`), so a request
whose reply is not yet readable at that poll times out instantly. The error text hides the cause: `forge_failure` prints
only the top-level `error sending request for url (...)`, which is what reqwest prints for a timeout too.
These are the only two `start_paused` tests in the file, and exactly the two that failed in CI; every other test uses real time.
Linux and Windows win the race (reply readable at first poll); macOS kqueue loopback delivery on a loaded runner loses it.

## Ruled out
- (a) RST on unclean close: GET has no body, the fake reads to `\r\n\r\n`, so nothing is left unread. Also would hit unpaused tests.
- (b) pooled client reuse: client is built per `deleter()` call, no process-wide client.
- (c) serial accept blocked: the failing call is the first request of the test, one client per test.
- (d)/(e) port exhaustion, short timeout: 5 s timeout never really elapses; ports are fine for the 30 unpaused tests that pass.

## Repro
- Stress, unfixed, Linux, `forge::github_packages`: threads=1 0/40, threads=2 0/40, threads=8 0/40 failures. Linux does not fail naturally.
- Forced mechanism (temporary test, deleted): paused-clock test with reqwest (5 s timeout) against a `std::thread` listener
  that replies after a real 50 ms delay: fails with `error sending request for url (http://127.0.0.1:PORT/x)`,
  `is_timeout() == true`. The same request under an unpaused `#[tokio::test]` passes. Red and green shown.

## Fix
Commit "test: ..." removes `start_paused` from both tests (real time; the confirm loop costs 2 x 250 ms in the first test).
No product change.

## Commands
- `cargo test -p ocx_announce`: 556 passed, 0 failed (lib), doc-tests 0.
- `cargo fmt --check -p ocx_announce`: clean.
- macOS: not run, CI only. The mechanism is platform-independent tokio semantics, shown deterministically here;
  fix removes the paused clock, so no virtual-time jump can occur.
