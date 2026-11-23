# P-HARNESS — SP-8: deny a home all registry traffic and count its attempts

Base: `895ba20e`. Code: `test/src/registry_guard.py`, self-tests `test/tests/test_registry_guard.py`.

## SP-8

- **Claim (F18):** a home can be denied all registry traffic, and every request it attempts is counted.
- **Method:** the fallback shape, with a counting refusal listener in place of the forwarding `forward_proxy`. A loopback TCP listener records the request line of each connection and answers 503. A guarded runner gets `HTTP_PROXY`/`HTTPS_PROXY` (both cases) pointing at it and an empty `NO_PROXY`. Self-test commands: `ocx package pull` against the suite's zot `registry`.
- **Result: holds; fallback taken.** The `[mirrors]` primary was not chosen: a mirror reroutes only the registries named in config and needs a config file per home, while the proxy env needs no config and covers every host a command might dial, including one the test did not predict. The listener answers every request itself, so the guard needs no upstream to forward to (the existing `forward_proxy` forwards and would let traffic through).
- **Evidence (both outcomes):**
  ```
  unguarded home: ocx package pull -> exit 0, guard.count == 0   (test_unguarded_home_pulls_and_guard_counts_nothing)
  guarded home:   ocx package pull -> exit 75, guard.count == 2  (test_guarded_home_fails_and_the_guard_counts_the_attempt)
    GET http://localhost:<port>/v2/ HTTP/1.1
    GET http://localhost:<port>/v2/<repo>/manifests/sha256:<digest> HTTP/1.1
    stderr: transient registry failure: Server error: ... code: 503
  guard lifted:   same home pulls again, count unchanged         (test_guard_lifted_...)
  two guards:     the unguarded home's guard stays at 0          (test_each_guard_counts_only_...)
  ```
- **Limits.** The count is requests that reached the proxy, one per connection head; a client that retries counts each try (the guarded pull above made two distinct requests). A TLS registry would arrive as `CONNECT host:port`. A command that never reads `HTTP_PROXY` (none known in ocx) would bypass the guard and the count would read 0, so a test asserting "zero requests" must also assert the command succeeded offline, never rely on the count alone.
- **Fixture shape:** `registry_guard` (function scope, one listener; `.deny(runner)`, `.allow(runner)`, `.count`, `.requests`, `.reset()`); `registry_guard.running()` for a second guard when two homes are counted separately.
