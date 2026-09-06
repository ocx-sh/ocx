# HTTP Clients

httpx rules for a long-running bot that talks to OCI registries and the GitHub
API: client lifetime, deadlines, server-supplied URLs, retries, bodies whose
size the server chose, and what reaches the operator's terminal. Loads when
editing any file that imports `httpx` or handles an HTTP response.

Contents: [Scope](#scope-pinned) · [The Client](#the-client) ·
[Server-Supplied URLs and Retries](#server-supplied-urls-and-retries) ·
[Responses](#responses) · [Messages and Tests](#messages-and-tests) ·
[What Agents Get Wrong](#what-agents-get-wrong-here)

## Scope (pinned)

- **`index/bot` is the only httpx codebase in the fleet**, and it is pure httpx
  — one runtime dependency, no subprocess anywhere. `ocx-sdk-python` has zero
  runtime dependencies and therefore cannot use httpx at all; where a rule
  below names an httpx API, its `urllib.request` position is in the same cell.
- **httpx's implicit default timeout is 5 seconds** on each of connect, read,
  write and pool — not "none". An agent carrying `requests` habits assumes
  there is no default and that omitting `timeout=` is merely untidy; here it is
  a real deadline nobody wrote down. `timeout=None` disables all four axes.
- **Two of these are open defects, not preventive rules.** PY-HTTP-02:
  `github_api.py::_paginate` follows a server-supplied URL on the authenticated
  client with no host check, where the sibling adapter has exactly that guard
  and a test for it. PY-HTTP-05: `get_blob`/`get_manifest` buffer an uncapped
  body, where the zero-dependency SDK streams under a 256 MiB cap.
- **Do not add a retry library.** `tenacity`/`stamina` would be a lateral move:
  the bot's own delay generator already separates the pure backoff maths from
  the loop that sleeps, which is the split those libraries do not give you.

## The Client

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-HTTP-01 | Every client construction states `timeout=` explicitly, even to restate the library default, and never `verify=False`. Writing the number down makes the deadline a reviewable decision instead of a fact you have to know the library to see; `verify=False` to quiet a certificate error is a self-inflicted MITM hole that never ships. `timeout=None` disables every axis and leaves a stalled connection bounded only by TCP keepalive, often hours. A client with no client-level deadline is acceptable only where every call site in that file passes its own. On `urllib.request` there is no client object to carry a default — the deadline is `urlopen(..., timeout=)` at every call. | `rg -nU --pcre2 --type py 'httpx\.(Async)?Client\((?!(?:.*\n){0,3}?.*timeout=)' src/` — every construction printed lacks a client-level deadline; zero on `index/bot`. `rg -n --type py 'default_factory=httpx\.(Async)?Client' src/` — the dataclass spelling the pattern above cannot see; each hit must pass `timeout=` at every use. `rg -n --type py -e 'verify\s*=\s*False' -e 'timeout\s*=\s*None' src/` — any line printed is the violation | MUST |
| PY-HTTP-03 | One client per adapter instance, constructed at the boundary — a dataclass field or an injected parameter — and held for the process lifetime. Never construct one inside the method that uses it: a client-per-call pays a fresh TCP and TLS handshake to a host the pool would have kept warm, and the keep-alive pool it would have filled dies with the `with` block. Never the bare top-level `httpx.get()`/`post()`/… API, which opens and closes a connection per call and states no deadline at the call site. Never `import requests` in an httpx codebase — a second pool, no shared mocking story, and no default timeout at all. | `rg -n --type py -e 'return httpx\.(Async)?Client\(' -e 'with httpx\.(Async)?Client\(' src/` — any line printed is a per-call client; one hit on `index/bot`, `github_api.py:271`, and `registry_v2.py`'s `field(default_factory=httpx.Client)` is the shape to copy. `rg -n --type py 'httpx\.[a-z]+\(' src/` — a lowercase attribute called on the module is the top-level API, since every client class is capitalised. `rg -n --type py -e '^import requests' -e '^from requests' src/`. Both empty today | MUST |

## Server-Supplied URLs and Retries

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-HTTP-02 | Every server-supplied URL — a 3xx `Location` or a `Link: rel="next"` — has its host compared against the current base host before the same authenticated client requests it. This is CVE-2018-20060's shape moved up to the application layer: a client that follows a URL it did not choose hands its `Authorization` header to whoever controls that host. httpx strips that header across a cross-origin *redirect*, which is exactly the protection a next-link the code re-requests itself does not get. Transfers to `urllib` unchanged. | `rg -n --type py -e '\.links\.get\(' -e '\.links\[' -e 'follow_redirects' -e 'headers\.get\("Location"' src/` — every line printed consumes a URL the server chose, and each needs a host comparison before the request goes out. One hit on `index/bot`: `github_api.py:309`, `_paginate`, which has none. A hand-rolled `Link:` parser will not match the pattern — `registry_v2.py::_parse_next_link` is that shape, and is the reference to copy, guard and cross-host test together | MUST |
| PY-HTTP-06 | Only idempotent methods are retried automatically — GET, HEAD, PUT, DELETE. A replayed POST or PATCH can double-apply, opening two pull requests or filing two issues, which makes an automatic in-process resend a correctness bug wearing a performance-tuning costume; a mutating call that fails goes back to the caller, who can decide. Attempts are bounded, and a `Retry-After` is clamped by a ceiling before it reaches a sleep — honoured outright, a single header parks the process for as long as the server likes. | `rg -nU --pcre2 --type py -e '^\s*for (?:.*\n){0,6}?.*\.post\(' -e '^\s*while (?:.*\n){0,6}?.*\.post\(' src/` — a loop header within six lines above a POST is the violation; empty on `index/bot`, and worth a comment plus a test so it stays a decision rather than an accident. `rg -n --type py -e 'headers\.get\("Retry-After"' -e 'headers\["Retry-After"\]' src/` — trace every line printed to the sleep it feeds and require a `min(...)` ceiling; two hits today, and both reach a sleep unclamped | MUST |

The host check PY-HTTP-02 asks for, which is also PY-HTTP-08 in the same six
lines — the rejected URL is server-controlled, so it goes out `!r`-quoted:

```python
def _next_link(response: httpx.Response, base_url: str) -> str | None:
    url = response.links.get("next", {}).get("url")
    if url is None:
        return None
    if urlsplit(url).netloc != urlsplit(base_url).netloc:
        raise AnomalyError(f"pagination link left the base host: {url!r}")
    return url
```

## Responses

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-HTTP-04 | The status is checked before the body is touched: `raise_for_status()`, or an explicit status branch, precedes every `.json()`/`.content` read — otherwise a 404 carrying valid JSON parses cleanly and flows on as data. A bare 3xx is handled by name as well. `raise_for_status()` fires only at status ≥ 400, so under httpx's `follow_redirects=False` default a redirect response falls through every check into the body parser and surfaces as a `JSONDecodeError` with no relation to the real cause. | Both checks below, pointed at the tree holding the HTTP adapters. **Empty output is the pass** — no file parses a body without a status check, none calls `raise_for_status()` without a redirect branch. Any path printed lacks that guard anywhere in the file: on `index/bot` the first prints nothing and the second prints both adapters. Neither proves the guard sits next to each read, but the candidate set is one or two files, not a tree | MUST |
| PY-HTTP-05 | A body from a registry is read under an explicit byte cap, streamed in chunks. Never bare `.content`/`.text`/`.json()` on one: each buffers the whole body into memory with no ceiling, and does it *after* transparent decompression, so a small compressed reply expands without bound (CWE-409). GHCR will serve a layer up to 10 GB and nothing client-side stops it; a manifest documented as "a few KiB" has no enforced upper bound either. The SDK, with no httpx to lean on, was forced into the same shape by hand: streamed in 1 MiB chunks under a 256 MiB cap — the mechanism does not transfer to `urllib`, the ceiling does. | `rg -n --type py -e 'response\.content' -e 'response\.text' src/` — every line printed buffers a whole body, and each must be under a byte ceiling or replaced by a streamed read. Two hits on `index/bot`, both `registry_v2.py` (`get_manifest:278`, `get_blob:309`), both uncapped. `.json()` sits on the same buffer, so a `.json()` on a registry response is this finding under another name | MUST |
| PY-HTTP-07 | `response.json()` is validated once, at a single boundary function returning a real type; call sites never subscript the raw result and never `cast()` a fresh ad hoc shape out of it. `.json()` is `Any`, so each downstream `cast` is an unchecked assertion that the wire matched what the author imagined, re-derived independently at every site. Hand-written `isinstance` checks against a `TypedDict` or dataclass, not a validation dependency — these shapes are small, spec-fixed, and the fleet's precedent for exactly this is stdlib. | `rg -n --type py -e '\.json\(\)\[' -e 'cast\([^)]*\.json\(\)' src/` — any line printed is a raw subscript off an untyped body; ten today across both adapters | SHOULD |

The two file-scoped checks PY-HTTP-04 refers to. Both are `--type py` and
scoped to the adapter tree; point `src/` at whichever directory holds the HTTP
client in the project at hand:

```sh
rg -l --type py '\.json\(\)' src/ \
  | xargs -r rg --files-without-match -e 'raise_for_status\(' -e 'status_code'
rg -l --type py 'raise_for_status\(' src/ \
  | xargs -r rg --files-without-match -e 'is_redirect' -e 'is_success'
```

The taxonomy new adapter code catches on: `httpx.HTTPError` splits into
`RequestError` — everything that happened before a response existed
(`TimeoutException`, `NetworkError`, `ProtocolError`, `ProxyError`) — and
`HTTPStatusError`, which only `raise_for_status()` raises and which therefore
means a *successful* exchange that came back ≥ 400. Where the code does not
discriminate a `ReadTimeout` from a `ConnectError`, `except httpx.TransportError`
is the single net for the first group. `except Exception` around a request never
is: it catches the `TypeError` in the header-building helper too, and reports a
bug as "transient, retry later".

The capped read PY-HTTP-05 asks for. The cap is enforced *while* reading, which
is also what closes the decompression-bomb case — httpx inflates incrementally,
so a ceiling on the buffered total is a ceiling on the inflated size:

```python
_MAX_BODY_BYTES = 256 * 1024 * 1024
_CHUNK_BYTES = 1024 * 1024


def read_capped(client: httpx.Client, url: str) -> bytes:
    with client.stream("GET", url, timeout=30.0) as response:
        response.raise_for_status()
        body = bytearray()
        for chunk in response.iter_bytes(_CHUNK_BYTES):
            body += chunk
            if len(body) > _MAX_BODY_BYTES:
                raise AnomalyError(f"body exceeded {_MAX_BODY_BYTES} bytes: {url!r}")
        return bytes(body)
```

## Messages and Tests

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-HTTP-08 | A server-controlled string reaches an exception message, a log line, or a terminal only through `!r`. A registry or API header carrying `\x1b[2J` prints raw from any handler that interpolates it plainly, and CWE-150 covers what that buys someone on the operator's terminal — cursor control, a cleared screen, a fake prompt. The rule covers the value bound to a variable three lines earlier, not just the inline interpolation, which is the form the codebase actually gets wrong. | `rg -n --type py -e '\{[^}!]*\.headers[^}]*\}' -e '=\s*\w+\.headers\.get\(' src/` — read each line printed through to its use site; two hits today, `registry_v2.py:283` compliant via `!r` and `github_api.py:292` not | SHOULD |
| PY-HTTP-09 | No test opens a socket to a host outside loopback, and the runner enforces it rather than convention. Transport-level mocks and loopback `ThreadingHTTPServer` fakes make this true today by construction — the point of the guard is the test added next year without the decorator, which would otherwise pass quietly against the live internet instead of failing loudly. | `pytest --disable-socket --allow-hosts=127.0.0.1 tests/` — a `SocketConnectBlockedError` in the output names the test that reached the network. Needs `pytest-socket` as a dev dependency and the loopback carve-out for the integration tier; neither exists yet | SHOULD |

## What Agents Get Wrong Here

1. **Reaching for `requests`.** It is what the training corpus uses by
   default, it looks like a harmless import, and it brings a second connection
   pool with no default timeout whatsoever.
2. **Assuming no `timeout=` means no deadline.** httpx has one — 5 seconds per
   axis — and it is invisible at the call site that inherits it.
3. **`verify=False` to make a certificate error go away**, usually while
   debugging something unrelated, and never removed afterwards.
4. **Constructing the client inside the method that uses it**, because
   `with httpx.Client() as client:` is the shape every quickstart shows.
5. **Treating `response.json()` as typed** — subscripted directly at each call
   site, with a `cast` added to quiet the type checker, which converts a
   missing-key crash into a lie the checker now believes.
6. **`.content` on a body whose size the server chose.** The convenient
   attribute is also the unbounded one, and nothing in the API hints at that.
7. **Wrapping a POST in a retry loop**, because retry is filed mentally under
   "robustness" rather than under "idempotency".
8. **Following a `Link: rel="next"` URL because it came from the same API.**
   The guard reads as paranoia right up until it is a CVE — and this codebase
   already has the guard, one file away from the call site that lacks it.
9. **`except Exception:` around a request**, which swallows the `TypeError` in
   the header-building helper alongside the network failure and reports both as
   "transient, retry later".
10. **Interpolating a header value straight into an error message** that a CLI
    then prints to a terminal.
