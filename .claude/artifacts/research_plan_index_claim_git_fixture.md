# Research: git-over-HTTP test fixture

**Date:** 2026-09-05
**Expires:** 2027-03-05
**Axis:** technology / tools
**Consumer:** plan_index_claim_command.md

## Summary

- Build the CGI bridge by hand (~60–90 lines): `http.server.CGIHTTPRequestHandler` is deprecated since Python 3.13 and scheduled for removal in 3.15 (this repo runs Python 3.14 locally), so it cannot be the transport — the bridge is `subprocess.Popen(["git", "http-backend"], env=..., ...)` plus a header/body splitter.
- `git http-backend` is CGI-classic: environment in (`GIT_PROJECT_ROOT`, `GIT_HTTP_EXPORT_ALL`, `PATH_INFO`, `REQUEST_METHOD`, `QUERY_STRING`, `CONTENT_TYPE`, `REMOTE_USER`, `REMOTE_ADDR`), stdin in (request body, exactly `Content-Length` bytes), stdout out. **Verified locally (git 2.54.0):** header lines are `\r\n`-terminated, the header block always ends `\r\n\r\n` (even with a zero-length body), a non-2xx response is signalled by a `Status: <code> <reason>` header line, and the process **exits 0 regardless of the emitted Status** — the caller must parse the header, never trust the exit code.
- Python's `http.server` does **not** decode `Transfer-Encoding: chunked` request bodies anywhere in `BaseHTTPRequestHandler` — confirmed by reading `Lib/http/server.py`. A hand-rolled chunk reader (~20–25 lines, RFC 9112 §7.1 grammar) is mandatory before handing bytes to `git http-backend`'s stdin.
- git does not always chunk: `http.postBuffer` (default 1 MiB) is the threshold — a push whose pack fits inside it goes out with a plain `Content-Length`, only a pack **larger** than the buffer switches to `HTTP/1.1` + `Transfer-Encoding: chunked`. A fixture that only ever pushes tiny test fixtures may never legitimately exercise the chunked path unless a test deliberately forces it (`-c http.postBuffer=1` or a padded blob).
- Push options are real and cheap to prove: **verified locally** — `GIT_PUSH_OPTION_COUNT` / `GIT_PUSH_OPTION_<n>` land in `pre-receive` and `post-receive`, and are **absent** (empty) in `update`. `receive.advertisePushOptions=true` must be set on the bare repo or the capability is never advertised — verified in the real `info/refs?service=git-receive-pack` CGI response. Nothing about this is HTTP-specific; it is negotiated as an ordinary pack-protocol capability, so the CGI bridge gets it for free.
- `http.extraHeader` is set test-side via `GIT_CONFIG_COUNT`/`GIT_CONFIG_KEY_<n>`/`GIT_CONFIG_VALUE_<n>` (no config file needed) and observed server-side as an ordinary `HTTP_<NAME>` env var inside the CGI child — no new plumbing needed beyond what the CGI bridge already builds.
- **Reject `dulwich` as the transport.** Its WSGI server (`dulwich/web.py`) *does* self-decode chunked bodies and *does* run real `pre-receive`/`update`/`post-receive` hooks (`self.repo.hooks.get(...)`) — but its `ReceivePackHandler` (`dulwich/server.py`) never negotiates the `push-options` capability at all: the constant exists in `protocol.py`'s capability enum but is absent from the server's advertised/consumed capability set. Since push options are a named requirement (Q4/Q8), dulwich cannot deliver the feature regardless of the "one new test dependency is allowed" allowance.
- **Reject `pygit2`/libgit2 too**: client-side push-options exist, but no maintained server-side smart-HTTP receive-pack implementation ships in the libgit2 ecosystem (an open ask, not a shipped feature).
- Recording-shim prior art is thin outside git's own test suite: `git/git`'s `t/helper/test-fake-ssh.c` is the closest real precedent — a compiled helper (not a shell script, so it has no shebang problem on Windows) installed via `GIT_SSH`, which records **argv only** (not env) and runs the trailing argument locally instead of delegating to a real `ssh`. No widely-known Python project does argv+env recording plus real delegation for `git` itself; that piece would be original code (~15–20 lines: dump `sys.argv`/`os.environ` as JSON, then `os.execv(real_git, sys.argv)`), with the caveat that Windows needs a `.cmd`/`.exe` shim, not a bare script — moot here since this suite's pytest leg is Linux-only in CI.
- Don't make a hook sleep. `pre-receive`/`post-receive` run synchronously inside `git-receive-pack`, so a sleeping hook holds the HTTP response open and wedges the push. Have the hook write a `ready_at = now + delay` timestamp record and return immediately (0 new lines of concurrency machinery); the fixture's poll route compares wall-clock time to that timestamp — the same shape as the `not_ready` remaining-counter knob `fake_forge.py` already uses for fork-readiness polling.
- Total new surface for the whole capability is roughly 150–220 lines of Python: CGI-env builder + stdin writer + chunked-body decoder + stdout header/body splitter + one `hooks/post-receive` shell script + a timestamp-based readiness gate. No new runtime dependency.

## Findings

### 1. `git http-backend` as CGI

Primary source: [git-http-backend(1)](https://git-scm.com/docs/git-http-backend).

> "A simple CGI program to serve the contents of a Git repository to Git clients accessing the repository over http:// and https:// protocols."

Required/consumed environment, per the doc's "DISCUSSION"/webserver-config examples section:

- **Core CGI variables** (set by the web server / the fixture, not by git itself): `PATH_INFO` (or `PATH_TRANSLATED` if `GIT_PROJECT_ROOT` unset), `REMOTE_USER`, `REMOTE_ADDR`, `CONTENT_TYPE`, `QUERY_STRING`, `REQUEST_METHOD`.
- **Git-specific**: `GIT_PROJECT_ROOT` — "must be set manually in the web server configuration" to anchor repo lookup; `GIT_HTTP_EXPORT_ALL` — "may be passed to git-http-backend to bypass the check for the `git-daemon-export-ok` file in each repository"; `GIT_PROTOCOL` for protocol v2 ("Most webservers will pass this header to the CGI via the `HTTP_GIT_PROTOCOL` variable, and git-http-backend will automatically copy that to `GIT_PROTOCOL`"); `GIT_HTTP_MAX_REQUEST_BUFFER` to bound ref-negotiation request size.
- Repo path resolution: "_git http-backend_ concatenates the environment variables `PATH_INFO` ... and `GIT_PROJECT_ROOT`". Since the fixture already exposes an `owner/repo` two-segment identity for the REST surface, the natural `PATH_INFO` shape is `/<owner>/<repo>.git/info/refs` etc., with `GIT_PROJECT_ROOT` pointed at a directory containing `<owner>/<repo>.git` bare repos.
- Auth breadcrumb the doc calls out explicitly: "sets `GIT_COMMITTER_NAME` to `$REMOTE_USER` and `GIT_COMMITTER_EMAIL` to `${REMOTE_USER}@http.${REMOTE_ADDR}`" for any reflog entries `receive-pack` creates — so `REMOTE_USER` should be set to the fake's `token_identity_login` for parity with the REST surface's identity.
- Service enable/disable is config, not env: `http.receivepack` must be `true` (or an equivalent `git config` call at repo-init time) for pushes to be accepted at all — anonymous `receive-pack` is refused by default.

**The doc does not spell out the header/body wire format** — that had to be established empirically (git 2.54.0, local, read-only):

```
$ GIT_HTTP_EXPORT_ALL=1 GIT_PROJECT_ROOT=<root> PATH_INFO=/repo.git/info/refs \
  REQUEST_METHOD=GET QUERY_STRING=service=git-upload-pack CONTENT_TYPE= \
  REMOTE_USER=tester REMOTE_ADDR=127.0.0.1 git http-backend > out.raw
```
Byte-exact output (success case, `od -c`):
```
Expires: Fri, 01 Jan 1980 00:00:00 GMT\r\n
Pragma: no-cache\r\n
Cache-Control: no-cache, max-age=0, must-revalidate\r\n
Content-Type: application/x-git-upload-pack-advertisement\r\n
\r\n
001e# service=git-upload-pack\n0000...
```
Error case (`PATH_INFO` pointed at a repo that doesn't exist):
```
Status: 404 Not Found\r\n
Expires: Fri, 01 Jan 1980 00:00:00 GMT\r\n
Pragma: no-cache\r\n
Cache-Control: no-cache, max-age=0, must-revalidate\r\n
\r\n
```
with **process exit code 0** in both cases (stderr carried `Not a git repository: '...'` but the CGI process itself never signals failure via exit status). So the bridge must:
1. Read stdout fully (or stream it) into a bounded prefix, split on the first `\r\n\r\n`.
2. Parse `key: value\r\n` lines out of the prefix; if one of them is `Status: NNN ...`, use `NNN` as the HTTP status, else default `200`.
3. Forward the remaining header lines verbatim as response headers, then stream the rest of stdout as the body.
4. **Never** treat subprocess exit code as a success/failure signal.

Smart vs dumb: "Smart HTTP" (what a fixture needs) auto-negotiates via the `service=` query string on `GET .../info/refs`; the doc lists `http.uploadpack` (default enabled) and `http.receivepack` (default disabled for anonymous, enabled for authenticated) as the gating config, plus a legacy `http.getanyfile` (dumb protocol, on by default — should be turned off in the fixture unless deliberately testing dumb fallback, to keep the surface matching real git-http-backend defaults without inviting an unintended second code path).

Negative finding: the doc does not state whether `git http-backend` ever writes `\n\n` instead of `\r\n\r\n` (some CGI implementations tolerate bare LF). Empirically, with this git build, it is `\r\n\r\n` throughout. A defensive splitter that accepts either is cheap and removes the ambiguity rather than hard-coding on an unverified assumption for a different git version.

### 2. Chunked request bodies

**Python's `http.server` does not decode chunked bodies.** Read directly from CPython's `Lib/http/server.py` (`raw.githubusercontent.com/python/cpython/main/Lib/http/server.py`): no occurrence of `"Transfer-Encoding"` or `"chunked"` anywhere in `BaseHTTPRequestHandler.parse_request`/`handle_one_request`; headers are parsed via `http.client.parse_headers()` with no subsequent body-decoding step. `self.rfile` is the raw, unbuffered-at-the-framing-level socket stream — whatever bytes arrive are whatever bytes arrive, chunk-framed or not. This confirms the prompt's premise precisely: a handler that does `self.rfile.read(int(self.headers["Content-Length"]))` (exactly what `fake_forge.py::_Handler._read_body` does today) will either read zero bytes (no `Content-Length` header present on a chunked request) or, worse, silently return whatever partial/garbage slice `int(None)`-style code produces.

Minimal correct decoder, per [RFC 9112 §7.1](https://www.rfc-editor.org/rfc/rfc9112.html#section-7.1) ("Chunked Transfer Coding"):
```
chunked-body = *chunk last-chunk trailer-section CRLF
chunk        = chunk-size [chunk-ext] CRLF chunk-data CRLF
chunk-size   = 1*HEXDIG
last-chunk   = 1*("0") [chunk-ext] CRLF
```
A loop against a `BaseHTTPRequestHandler.rfile` (line-buffered `rfile` from `socket.makefile`, so `readline()` is safe for the size line):
```python
def _read_chunked(rfile) -> bytes:
    body = bytearray()
    while True:
        size_line = rfile.readline()
        size = int(size_line.split(b";", 1)[0].strip(), 16)  # drop chunk-ext
        if size == 0:
            while rfile.readline() not in (b"\r\n", b""):  # consume trailer-section
                pass
            break
        body += rfile.read(size)
        rfile.read(2)  # trailing CRLF after chunk-data
    return bytes(body)
```
Dispatch: use this when `self.headers.get("Transfer-Encoding", "").lower() == "chunked"`, else fall back to the existing `Content-Length`-bounded read — `git` sends exactly one or the other, never both (RFC 9112 §6.1 forbids both being present; git itself picks one via the postBuffer threshold below).

**Does git always chunk?** No. [`http.postBuffer`](https://github.com/git/git/blob/master/Documentation/config/http.adoc) (fetched from git's own source tree, since the rendered `git-scm.com` page truncated before this entry when fetched via a summarizing tool):
> "Maximum size in bytes of the buffer used by smart HTTP transports when POSTing data to the remote system. For requests larger than this buffer size, HTTP/1.1 and Transfer-Encoding: chunked is used to avoid creating a massive pack file locally. Default is 1 MiB, which is sufficient for most requests.
>
> Note that raising this limit is only effective for disabling chunked transfer encoding and therefore should be used only where the remote server or a proxy only supports HTTP/1.0 or is noncompliant with the HTTP standard."

So: pack ≤ 1 MiB (essentially every fixture-scale push in this test suite) → plain `Content-Length` request, no chunking at all, `git-receive-pack` request bodies read exactly like the REST handlers already do. **Only a push engineered to exceed 1 MiB** (or a test that sets `-c http.postBuffer=1` to force the small-buffer path) will actually exercise the chunked branch — worth calling out explicitly in the plan so "prove chunked decoding works" gets a dedicated oversized-blob test rather than being assumed covered by an ordinary small push.

### 3. `CGIHTTPRequestHandler` status

Confirmed via [Python 3.15 "pending removal" page](https://docs.python.org/3/deprecations/pending-removal-in-3.15.html):
> "The obsolete and rarely used `CGIHTTPRequestHandler` has been deprecated since Python 3.13. No direct replacement exists. _Anything_ is better than CGI to interface a web server with a request handler."
> "The `--cgi` flag to the **python -m http.server** command-line interface has been deprecated since Python 3.13."

Both are "scheduled for removal in Python 3.15." [PEP 594](https://peps.python.org/pep-0594/) itself only covers the `cgi`/`cgitb` *library modules* (deprecated 3.11, removed 3.13) — `CGIHTTPRequestHandler` is a separate, later deprecation (3.13→3.15) documented outside PEP 594 proper, in the ordinary deprecations/whatsnew pages; the mechanism is the same "rarely used, high security/functionality bug potential" rationale.

Practical consequence for this repo: local Python is 3.14.5 (confirmed: `python3 --version`), inside the deprecated-but-present window — `CGIHTTPRequestHandler` would still import and technically run, but building new test infrastructure on a class already flagged for removal one version later is a guaranteed near-term breakage. It should not be used regardless of whether it "still works today."

### 4. Push options end to end

Docs, from git's own source tree (`Documentation/githooks.adoc`, `Documentation/config/receive.adoc`):

> `pre-receive`/`post-receive`: "The number of push options given on the command line of `git push --push-option=...` can be read from the environment variable `GIT_PUSH_OPTION_COUNT`, and the options themselves are found in `GIT_PUSH_OPTION_0`, `GIT_PUSH_OPTION_1`,… If it is negotiated to not use the push options phase, the environment variables will not be set. If the client selects to use push options, but doesn't transmit any, the count variable will be set to zero, `GIT_PUSH_OPTION_COUNT=0`."

> `receive.advertisePushOptions`: "When set to true, git-receive-pack will advertise the push options capability to its clients. False by default."

The `update` hook's section documents no such variables, and the pre-receive/post-receive sections are the *only* two that mention `GIT_PUSH_OPTION_*`.

**Verified locally (git 2.54.0)**, in three steps, no HTTP involved (the pack-protocol capability layer is transport-independent, so a local push exercises the identical negotiation the CGI bridge would carry over HTTP):
1. `receive.advertisePushOptions=true` set on a bare repo; `GET info/refs?service=git-receive-pack` through the real `git http-backend` CGI shows the capability line: `...report-status report-status-v2 delete-refs side-band-64k quiet atomic ofs-delta **push-options** object-format=sha1 agent=git/2.54.0-Linux` — so the CGI bridge advertises it automatically once the repo config is set; no extra bridge code needed.
2. `pre-receive`, `update`, and `post-receive` hooks were installed, each dumping `GIT_PUSH_OPTION_COUNT`/`GIT_PUSH_OPTION_<n>` to a log file. A local `git push --push-option=hello=world --push-option=second ...` produced:
   ```
   PRE-RECEIVE GIT_PUSH_OPTION_COUNT=2
   PRE-RECEIVE OPTION_0=hello=world
   PRE-RECEIVE OPTION_1=second
   UPDATE ref=refs/heads/main GIT_PUSH_OPTION_COUNT=        # <- empty, confirmed absent
   POST-RECEIVE GIT_PUSH_OPTION_COUNT=2
   POST-RECEIVE OPTION_0=hello=world
   POST-RECEIVE OPTION_1=second
   ```
   This is an exact, direct confirmation of the doc's silence on `update`: it is silent because `update` genuinely does not receive them.
3. A push without `--push-option` at all naturally never runs the "options phase," matching the doc's "environment variables will not be set" line (not `COUNT=0`, simply unset) — a fixture asserting on "no push options" should check for the env var's *absence*, not its equality to `"0"`.

**Survives over smart HTTP specifically**: yes, structurally — push options are part of the pkt-line capability negotiation embedded inside the `git-receive-pack` request body itself (the same bytes flow whether carried over `ssh://`, `file://`, or `http://`), and step 1 above shows the CGI backend advertising the capability identically to any other transport. Nothing in the docs or in the CGI backend singles out HTTP for different treatment. This was not re-verified with a full HTTP push cycle (would need a live listening server, not just an offline CGI invocation) — flagged as the one link in the chain not independently re-proven end-to-end here, though there is no documented or plausible mechanism by which the transport layer would strip a pack-protocol capability.

### 5. `http.extraHeader`

From git's own source (`Documentation/config/http.adoc`):
> "Pass an additional HTTP header when communicating with a server. If more than one such entry exists, all of them are added as extra headers."

URL-scoped form (`http.<url>.extraHeader`) uses git's general [URL-matching rules](https://git-scm.com/docs/git-config) for `http.*`: a `[http "https://weak.example.com"]` section applies only to requests to that URL/prefix, coexisting with a global `[http]` section — the documented example (`git config get --url=https://weak.example.com http.sslverify` picking the scoped `false` over the global default) is the same matching machinery `http.extraHeader` uses; git-scm.com's own worked example is scoped to `sslVerify` but the matching engine is shared across every `http.<url>.<key>`.

Environment injection, from the canonical git-config man page (`kernel.org` mirror, since the rendered `git-scm.com` page is too large to fetch its ENVIRONMENT section whole):
> "If `GIT_CONFIG_COUNT` is set to a positive number, all environment pairs `GIT_CONFIG_KEY_<n>` and `GIT_CONFIG_VALUE_<n>` up to that number will be added to the process's runtime configuration. The config pairs are zero-indexed. Any missing key or value is treated as an error. An empty `GIT_CONFIG_COUNT` is treated the same as `GIT_CONFIG_COUNT=0`... These environment variables will override values in configuration files, but will be overridden by any explicit options passed via `git -c`."

So a test can set, per-invocation, without touching any config file:
```
GIT_CONFIG_COUNT=1
GIT_CONFIG_KEY_0=http.extraHeader
GIT_CONFIG_VALUE_0=X-Test-Marker: abc123
```
and the CGI bridge observes it server-side as an ordinary CGI-mapped header env var (`HTTP_X_TEST_MARKER`) — no bridge-side change needed beyond the header-forwarding the bridge already needs for the plain smart-HTTP flow.

Gotchas:
- **Multiple headers**: doc confirms "all of them are added" when the key is set more than once (config keys of this type are git's ordinary multi-value keys, appended not overwritten) — but the doc text does not explicitly confirm that *two separate* `GIT_CONFIG_KEY_n=http.extraHeader` entries (same key, different index) combine into two headers rather than the later index overwriting the earlier one. **Unverified** — would need an empirical check (`GIT_CONFIG_COUNT=2` with both keys `http.extraHeader`) before a plan relies on it; plausible given how git's own multi-valued keys behave from a config *file*, but the env-var path was not independently confirmed here.
- **Redirects**: [`http.followRedirects`](https://github.com/git/git/blob/master/Documentation/config/http.adoc) — "If set to `true`, git will transparently follow any redirect... If set to `initial`, git will follow redirects only for the initial request to a remote, but not for subsequent follow-up HTTP requests... The default is `initial`." This means a redirect on the very first `info/refs` request is followed and the resolved URL becomes the new base for everything after — directly relevant to `fake_forge.py`'s existing `redirect_next_contents`/`test_announce_forge_redirect_is_not_followed` pattern on the REST side, and worth an analogous git-side test ("does the client re-send `extraHeader` / credentials against the redirected host, and should it"). **Negative finding**: no primary-source sentence was found stating explicitly whether `http.extraHeader` (as opposed to Basic-Auth credentials, which is a documented, actively-patched class of vulnerability in *other* git implementations — go-git CVE-2026-... GHSA-3xc5-wrhm-f963, gitoxide's curl-backend redirect issue — both concern credential/header leakage to a redirected host) is retained, dropped, or re-scoped when the base URL changes under `initial` redirect-following. This would need either a source read of `http.c`'s header-attachment logic or an empirical two-host redirect test; treat as open until one of those is done.

### 6. Recording-shim pattern

The closest real, load-bearing prior art is git's **own** test suite, not a third-party project:

[`t/helper/test-fake-ssh.c`](https://github.com/git/git/blob/master/t/helper/test-fake-ssh.c) (used by `t/t5601-clone.sh` and others) — a **compiled** C helper, installed on `PATH` via `GIT_SSH="$TRASH_DIRECTORY/ssh$X"` (the `$X` is git's own placeholder for `.exe` on Windows, sidestepping the "no shebang" problem entirely by not being a script). It:
- Records argv (prefixed `ssh:`) to a file at `"$TRASH_DIRECTORY/ssh-output"`.
- Does **not** record the environment.
- Does **not** delegate to a real `ssh` binary — it takes the trailing argument (the remote command git would have sent over the wire) and runs it locally via `run_command` with `use_shell = 1`, i.e. it *simulates* the remote session rather than proxying to a real one.
- Assertions read back the recorded file with `test_cmp ssh-expect ssh-output` (`expect_ssh` helper).

This is a **partial** match to what's asked for (Q6 wants argv **and** full env, recorded, with real delegation) — git's own test suite has never needed the env-recording or delegation half because `test-fake-ssh` intentionally short-circuits the network hop rather than proving a real one. No third-party Python project surfaced in search that does the "record argv+env, then `os.execve()` into the real binary" shim specifically for `git` — the closest structural analogues found were generic (Kubernetes' Go `exec/testing/fake_exec.go`, which validates and records calls but is Go, not Python, and not git-specific) and none were a direct hit.

**Cross-platform note** (bears on whether to build this piece at all): a Python-script shim on `PATH` named `git` works fine on POSIX (executable bit + shebang), but on Windows a `git` with no `.exe`/`.bat`/`.cmd` extension is not found by `CreateProcess`'s implicit-extension search unless `PATHEXT` is configured for extensionless scripts (it normally is not) — this exact class of problem is why `test-fake-ssh` is compiled, not scripted. Since [`subsystem-tests.md`](../rules/subsystem-tests.md) states the pytest acceptance leg runs **Linux-only in CI** ("The `acceptance-tests` job... matrices a single `ubuntu-latest` leg"), a Python-script shim is sufficient for this project's actual CI surface; a Windows-safe version would need a compiled or `.cmd`-wrapped variant, matching git's own precedent, only if this fixture is ever asked to run there.

Recommended shape if built: a small Python script (~15–20 lines) that on start does `json.dump({"argv": sys.argv, "env": dict(os.environ)}, open(record_path, "a"))` then `os.execv(real_git_path, sys.argv)` — `real_git_path` resolved via `shutil.which("git")` **before** the shim's directory is prepended to `PATH` (captured once at fixture setup, baked into the shim's own source or passed via a private env var the shim reads and then scrubs before exec, so it never itself becomes something a nested `git` invocation could recurse into).

### 7. A cheaper alternative, honestly evaluated

**`dulwich`** (pure-Python, PyPI `dulwich`, [readthedocs](https://dulwich.readthedocs.io/en/latest/)): actively maintained — "Healthy" maintenance signal (Snyk advisor), a security patch shipped as recently as the CVE-2026-47734 (thin-pack memory-allocation) fix, releases through Jan 2026, 2151 GitHub stars, supports CPython 3.10+.

What it gets right, read directly from source (`dulwich/web.py`, `dulwich/server.py`, `HEAD` branch):
- Its own WSGI layer **does** decode chunked bodies itself: `if req.environ.get("HTTP_TRANSFER_ENCODING") == "chunked": read = ChunkReader(req.environ["wsgi.input"]).read` — i.e. it has already solved problem #2 for anyone using its own dev server.
- Its `ReceivePackHandler` **does** invoke real hooks: `self.repo.hooks.get("pre-receive"/"update"/"post-receive", None)` then `hook.execute(...)` — genuine hook support, not a stub.
- Its own `main()` carries an explicit disclaimer: "This server is intended for debugging only. For production use, run the WSGI app... inside a proper WSGI server."

What sinks it for this fixture: **push options are not implemented** in the receive-pack path. `dulwich/protocol.py` defines `CAPABILITY_PUSH_OPTIONS = b"push-options"` and includes it in `KNOWN_RECEIVE_CAPABILITIES`, but `dulwich/server.py`'s `ReceivePackHandler` never advertises or consumes it — the capability constant is vestigial (present in the enum, absent from the actual negotiated capability list and from the hook-argument plumbing). Since Q4/Q8 (push options reaching a hook, on a delay) are named requirements of the plan this fixture serves, dulwich cannot deliver the feature at all, not just imperfectly — no amount of extra fixture code recovers a capability the library's server never negotiates.

**`pygit2`** (libgit2 binding): push options exist **client**-side (`Remote.push(..., push_options=[...])`, confirmed via the library's own docs/changelog and a resolved GitHub issue). But there is no maintained server-side smart-HTTP receive-pack implementation in the libgit2/pygit2 ecosystem — a sibling project's open issue ("Support for server side smart HTTP" on `libgit2sharp`) is itself evidence the capability is a standing feature request, not a shipped one, across libgit2 bindings generally.

**Verdict**: real `git http-backend`, shelled out to as CGI, is the only option that delivers every named requirement (push options reaching `pre-receive`/`post-receive`, real hook execution, real chunked-body semantics, real advertisement text) without gaps — confirmed empirically in this session, not just from docs. The cost is genuinely small (git is already a required toolchain binary for this project; the new code is ~150–220 lines, see Recommendation) and buys byte-for-byte real git semantics instead of a library's subset. Cost of *not* using dulwich: none avoided — this repo does not need a new Python dependency for a feature dulwich can't provide anyway. The "test-only Python dependency is a real option" allowance in the brief does not change the verdict, because the option that would have justified spending it (dulwich) does not clear the bar on the one requirement that matters most (push options).

### 8. Delaying a hook's side effect

Git's own docs describe hook execution as inline and synchronous with the RPC: "Both standard output and standard error output are forwarded to git send-pack on the other end" ([`githooks(5)`](https://git-scm.com/docs/githooks)) — the client only sees that output (and the RPC only completes) once the hook process has exited. This was independently confirmed by the local push test in Finding 4: `pre-receive`/`update`/`post-receive` all ran and finished, in order, before `git push` returned `ok` to the caller — there is no async dispatch of hooks anywhere in the mechanism.

**Consequence**: a `post-receive` hook that `sleep`s to simulate an async merge-request worker blocks the pushing client's connection for the sleep's full duration — exactly the wedge the brief warns against, and (worse) over the HTTP CGI transport specifically, it also holds the CGI child process and its stdout pipe open for that whole window, tying up one `ThreadingHTTPServer` worker thread per concurrent push.

**Recommended mechanism**: no background thread, no detached process, no real waiting at all. The hook computes `ready_at = time.time() + delay_seconds` (delay read from an env var or a fixed test knob) and writes a small JSON record (`{"ready_at": ..., "push_options": [...], "ref": ...}`) to a file the fixture's HTTP handler already has a path to, then exits immediately (`exit 0`). Whatever "poll for the merge-request" REST route the fixture serves computes readiness at *read* time — `is_ready = time.time() >= record["ready_at"]` — not at write time. This is the same idiom already proven in this exact file: `FakeForge.not_ready` is a per-repo remaining-attempts counter consulted lazily inside `handle_get_repo` on every poll, rather than anything that runs on a timer or a background thread. A timestamp comparison is a strict generalization of that counter (continuous delay instead of discrete attempt-count), reuses the identical "state lives in a dict, read lazily on each poll" shape, and needs no process lifecycle management, no teardown-time "did the background thread finish" bookkeeping, and no flakiness budget for scheduler jitter beyond whatever the test's own delay tolerance already allows.

Rejected alternative: spawning a detached worker (`subprocess.Popen([...], start_new_session=True)` from the hook, or a shell hook doing `nohup ... & disown`) that itself sleeps then writes the record. This reproduces the *real* production shape more faithfully (an actual out-of-band worker), but costs: process-group cleanup at test teardown (an orphaned sleeping child if the test fails before the delay elapses), platform divergence (`start_new_session`/`setsid` vs. Windows job objects — moot per the CI-is-Linux-only note above, but still an added surface), and zero additional coverage over the timestamp approach for what the plan's stated goal is ("a polling client can be tested both inside and beyond its bound") — both shapes produce the identical observable HTTP behavior from the polling client's point of view. Reach for the detached-process version only if a later requirement needs the *worker itself* to be independently observable (e.g., asserting the worker's own env or exit code), which is not stated here.

## Negative findings

- Could not obtain a primary-source sentence confirming whether `git http-backend`'s header/body separator is always `\r\n\r\n` across git versions, or whether some builds/platforms emit bare `\n\n` — the docs are silent; only local empirical testing (git 2.54.0) was available, and that showed `\r\n\r\n` consistently. Recommend the bridge's splitter accept either, and/or pin a minimum git version if the plan wants to assume CRLF unconditionally.
- Could not verify — from docs or by testing — whether two `GIT_CONFIG_KEY_<n>` entries carrying the same multi-valued key (e.g. two `http.extraHeader` entries at different indices) combine into two applied headers, versus the later index silently winning. Plausible by analogy to git's ordinary config-file multi-value semantics, but not independently confirmed for the environment-variable injection path specifically.
- Could not find a primary-source statement on whether `http.extraHeader` (as distinct from Basic-Auth credentials, which is a documented CVE class in *other* git implementations, not git itself) is retained, dropped, or re-scoped when `http.followRedirects=initial` follows a same-request redirect to a different host. Would need either a read of git's `http.c` header-attachment code or an empirical two-host redirect test.
- Could not independently re-verify push options surviving a **full** HTTP round trip (client → real listening `git http-backend` CGI bridge → hook) in this session — only the offline CGI-invocation half (capability advertisement) and the local non-HTTP push half (hook env vars) were verified separately; the two were not chained through an actual running server here (no server was started, per the task's "no docker/pytest" constraint and the general no-live-server posture of a research-only pass). The plan's first git-fixture test should be exactly this end-to-end chain, since it is the one link asserted only by inference (transport-independence of the pack-protocol capability layer) rather than direct observation.
- Searches for a maintained, well-known **Python** prior-art project that records both argv and the full environment for a `git` PATH shim, then delegates to the real binary, came up empty — the closest hits were git's own `test-fake-ssh.c` (argv-only, no delegation, compiled C) and a generic Go pattern from `kubernetes/utils`. If such a project exists, it was not found by search in this session.
- `git-http-mock-server` (isomorphic-git, Node.js) was confirmed as real prior art for "spawn `git http-backend`-equivalent and get hooks for free," but its actual internal CGI/env-var/header-splitting code could not be fully retrieved (the specific source file returned 404 or was too shallow to show the mechanics) — its README-level description ("uses the native git-http-backend process," "automatically supports Git hooks such as hooks/update and hooks/post-receive," "copy-on-write") was corroborated, but the line-level implementation was not.
