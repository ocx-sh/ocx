---
title: Promises, Cancellation and Concurrency
summary: The TS-ASYNC family — floating promises, async functions in void positions, deadlines and signals, per-runtime rejection semantics, and concurrency bounds
---

# Promises, Cancellation and Concurrency

Owns whether a promise is observed, whether an operation can be stopped, and how
many run at once. Does not own what a rejected promise *contains* (error shape),
nor disposing what an operation opened — child processes and handles are `TS-RES`.

Contents: [Scope](#scope) · [The Lint Gate](#the-lint-gate) ·
[Async in a Void Position](#async-in-a-void-position) ·
[Deadlines](#deadlines) · [Rejections Nobody Observes](#rejections-nobody-observes) ·
[Concurrency Bounds](#concurrency-bounds) · [Numbers You Pin](#numbers-you-pin) ·
[What Agents Get Wrong Here](#what-agents-get-wrong-here)

## Scope

- **`tsc --strict` catches none of this.** A `void`-returning callback type means
  "I will not look at your return value", and `Promise<void>` is a legal
  substitution for `void` — TypeScript's own FAQ says so, and it is a design
  decision, not a bug that will be closed. `forEach(async …)`, `onClick={async …}`,
  `addEventListener('click', async …)` and `setTimeout(async …)` all compile clean
  at every strictness level. The exception is `useEffect(async …)`, which `tsc`
  rejects unconditionally (`TS2345`) — do not write a rule for that one.
- **Type-aware linting is the only mechanical check in this family, and it is off
  by default.** `no-floating-promises`, `no-misused-promises` and
  `strict-void-return` all require type information; a config without it does not
  warn that they are inert, it just never fires them. Every rule below that is a
  *reading* rule is a reading rule because the lint that would replace it is
  probably not running in your repo. TS-ASYNC-01 is the line that turns it on;
  the wiring of type-aware linting itself belongs to `TS-TOOL-03`.
- **No lint will ever catch an unbounded-but-observed fan-out.**
  `no-floating-promises`' own *correct* example is
  `await Promise.all(arr.map(async …))`. The rule stops at "is the promise
  observed" and evaluates no resource bound. TS-ASYNC-15/16 are reading
  heuristics by necessity.

## The Lint Gate

One invocation covers both rows: `npx eslint --print-config <any-source-file> | jq
'.rules["@typescript-eslint/no-floating-promises"], .rules["@typescript-eslint/no-misused-promises"], .rules["@typescript-eslint/strict-void-return"]'`
— a `null` for any of the three is the violation.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-ASYNC-01 | Wire type-aware linting, keep `@typescript-eslint/no-floating-promises` and `no-misused-promises` (`checksVoidReturn: true`, all six sub-options on) at `error`, and add `@typescript-eslint/strict-void-return: "error"` **by name**. | Adopting `strictTypeChecked` does *not* enable `strict-void-return` — it ships only in the `all` config (verified in `@typescript-eslint/eslint-plugin` 8.68.0, released 2026-08-24), and it is the only one of the three carrying an autofix for the handler bug. Disabling a `checksVoidReturn` sub-option "for convenience" reopens exactly the hole TS-ASYNC-03 exists to close. | The `--print-config` command above: all three must be `2` or `["error", …]`. | MUST |
| TS-ASYNC-02 | Write `void expr` only when `expr` provably cannot reject: either the callee's body wraps **every** `await` and throwing statement in try/catch, or a `.catch()` is chained onto that exact call before the `void`. This covers the `void (async () => {…})()` IIFE too — the IIFE body needs the try/catch. | `void` marks intent and attaches nothing. `no-floating-promises` defaults to `ignoreVoid: true`, so even a running lint accepts every `void` unconditionally — a safe `void f()` and an unsafe one are visually identical and the difference lives entirely inside the callee. | `rg -n '^\s*void [a-zA-Z_$]' src/` and `rg -n 'void \(async' src/`, then read each callee or IIFE body. A `try` that does not cover a later `await` in the same function is not self-catching. No lint substitutes. | MUST |

```js
// eslint.config.js — type info is the precondition; without it all three rules are silently absent
languageOptions: { parserOptions: { projectService: true, tsconfigRootDir: import.meta.dirname } },
rules: {
  "@typescript-eslint/no-floating-promises": "error",
  "@typescript-eslint/no-misused-promises": "error",  // checksVoidReturn defaults true — leave all six sub-options on
  "@typescript-eslint/strict-void-return": "error",   // NOT in strictTypeChecked; name it or it is off
}
```

## Async in a Void Position

Where TS-ASYNC-01 holds, the lint catches these. Where it does not, the check is
the two-step **Handler Identifier Trace** below — and step 1 alone is not enough.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-ASYNC-03 | Never pass an `async` function where a `void`-returning callback is expected — `forEach(async …)`, `onClick={async …}`, `addEventListener('…', async …)`, `setTimeout(async …)`. `.map(async …)` is permitted only inside `Promise.all(…)`/`Promise.allSettled(…)` in the same expression. **One exception:** a Vue *template*-bound handler (`@click="method"`) may be a plain `async` method — Vue's compiler routes it through `callWithAsyncErrorHandling`, which attaches a real `.catch()` — but only where TS-ASYNC-10's `app.config.errorHandler` is wired. A raw `addEventListener` inside a `.vue` file gets no such net. | The rejection has no reachable handler by construction, and it compiles clean. The Vue carve-out is read from `vue@3.5` runtime source, not inferred from docs. | Step 1: `rg -n -e 'forEach\(async' -e '\.map\(async' -e 'addEventListener\([^)]*,\s*async' -e 'on[A-Z]\w*=\{\s*async' -e 'setTimeout\(\s*async' -e 'setInterval\(\s*async' src/` must be empty, every `.map(async` hit sitting inside `Promise.all(`/`Promise.allSettled(`. **An empty step-1 grep is not compliance** — it misses a bare identifier and an arrow that forwards to an async callee. Step 2 is mandatory. | MUST |
| TS-ASYNC-14 | The only permitted shape in a void-typed callback position is a **non-`async` outer function** containing a `void`-discarded, **self-catching** `async` IIFE. It follows that a bare reference to an `async`-declared function (`onClick={onSave}`) is equally forbidden there, **even when that function self-catches internally** — nothing at the call site distinguishes a safe callee from an unsafe one. An arrow whose expression body forwards to an `async` callee (`onChange={(e) => onImport(e.target.files?.[0])}`) is the same violation one indirection deeper. Do not introduce a shared `safeHandler(fn)` wrapper for this until you have crossed roughly a dozen sites with genuinely shared failure behaviour. | This exact shape is the autofix `strict-void-return` emits (`suggestWrapInAsyncIIFE`). The no-exceptions stance is typescript-eslint's own: they declined a per-callee "trust me, it self-catches" escape hatch in [typescript-eslint#9930](https://github.com/typescript-eslint/typescript-eslint/issues/9930). | **Handler Identifier Trace, step 2** (reading; no mechanical substitute without type-aware lint): `rg -n 'on[A-Z]\w*=\{[A-Za-z_$][\w$]*\}' -g '*.tsx' src/` and `rg -n '@[a-z]+="[A-Za-z_$][\w$]*"' -g '*.vue' src/` list every bare-identifier binding. Then, for each identifier those two commands actually printed, grep its declaration and check for `async` — that last step is reading, not a command, and there is no mechanical substitute for it without type-aware lint. Any `async` declaration is a violation. | MUST |

```tsx
// wrong — and exactly as wrong after the "cleanup" refactor to `const onDelete = async () => …`
<button onClick={async () => { await repo.remove(id); }} />
```

```tsx
// right — non-async outer, void-discarded self-catching async IIFE
<button onClick={() => { void (async () => {
  try { await repo.remove(id); } catch (e) { reportToUser(e); }
})(); }} />
```

## Deadlines

Grep per call shape; TS-ASYNC-08 is the one that has to be read.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-ASYNC-04 | Give every outbound call an explicit deadline: `fetch` gets `signal:`; an RPC transport factory gets its own default timeout option (`createConnectTransport` → `defaultTimeoutMs`). An outbound call with no signal is the common default, not an unusual oversight. | A signal-less `fetch` on Node does not hang forever and does not stop promptly either — it inherits undici's `connectTimeout` 10 s plus `headersTimeout` 300 s plus `bodyTimeout` 300 s, none of it documented at the call site and none of it chosen. Connect-ES's precedence resolves an unset `defaultTimeoutMs` to `undefined`, not to a default. | `rg -n 'fetch\(' src/` — every hit's init object carries `signal:`. `rg -n 'createConnectTransport\(' src/` — every hit carries `defaultTimeoutMs`. | MUST |
| TS-ASYNC-05 | Where a subprocess needs an in-code bound, call `node:child_process`'s `execFile`/`spawn` with `timeout:` (and `signal:`). A wrapper library that exposes no such field is not a bound, and neither is a CI-level `timeout-minutes`. | `@actions/exec@1.x`'s `ExecOptions` has no `timeout`, `signal` or `killSignal` field at all — passing one as an object literal is `TS2353`, and passing one through a spread options object is dropped **silently**. A job- or step-level timeout is minute-granularity, kills the whole step, and lives in the consumer's YAML. | `rg -n -e 'execFile\(' -e 'spawn\(' src/` — every hit's options carry `timeout:`. `rg -n 'exec\.exec\(' src/` — each hit is migrated or carries a comment naming the external bound it relies on. | MUST |
| TS-ASYNC-06 | Compose a caller's signal with an internal deadline as `AbortSignal.any([callerSignal, AbortSignal.timeout(ms)])`. Never hand-roll an `addEventListener('abort', …)` fan-in. A helper that makes an outbound call takes `signal?: AbortSignal` and composes it internally, unless it is provably an outermost leaf. | One stdlib line, and `.reason` propagates from whichever signal fired, so a `TimeoutError` vs an `AbortError` discriminates "we timed out" from "the caller cancelled" with no bookkeeping. Mature libraries that hand-roll this predate the primitive (`AbortSignal.any` needs Node ≥ 20.3 / 18.17); that excuse does not transfer to code written today. | `rg -n "addEventListener\(['\"]abort" src/` — any hit outside vendored code is the violation. | SHOULD |
| TS-ASYNC-07 | Never express a timeout as `Promise.race([op, delay])`. Any new `Promise.race` is a design review, not a routine merge. | Racing stops *awaiting*; it does not cancel. The socket, subprocess or RPC keeps running, and the loser's later rejection is itself unobserved. `p-timeout`'s own maintainer recommends `AbortSignal.timeout()` instead. | `rg -n 'Promise\.race\(' src/` — any hit needs a written rationale in the diff. | MUST |
| TS-ASYNC-08 | A retry loop gives each attempt its own timeout, strictly shorter than the overall budget, and re-checks the caller's signal **before** backing off — a cancellation stops the loop, it never consumes a retry. | Without a per-attempt bound the backoff machinery is unreachable: one silent server holds the whole budget inside undici's 5-minute headers window and attempt 2 never runs. Without the signal re-check, a caller-initiated cancellation looks like any other thrown error and gets retried three more times before surfacing. | Reading check, no lint exists. `rg -n -i -e 'retry' -e 'backoff' src/` locates the loops (empty output means there are none, which is a pass); for each one confirm (a) the awaited I/O call receives a signal distinct from the loop counter, and (b) the `catch` that decides to sleep-and-retry tests `signal?.aborted` first. | MUST |
| TS-ASYNC-09 | Narrow `signal.reason` at the point of observation — `reason instanceof Error ? reason : new Error(String(reason))` — before throwing, logging, or passing it on. | `AbortSignal.reason` is typed `any` in `lib.dom.d.ts`. `signal.reason.message` therefore type-checks when the reason is a string or `undefined`, and only fails at runtime; and the `any` propagates into everything downstream that touches it. | `rg -n '\.reason\.' src/` must be empty. `rg -n '\.reason\b' src/` — every hit is narrowed before use. | SHOULD |

```ts
// TS-ASYNC-08: both halves, or neither works
const attempt = caller ? AbortSignal.any([caller, AbortSignal.timeout(perAttemptMs)])
                       : AbortSignal.timeout(perAttemptMs);
try { return await fetch(url, { ...init, signal: attempt }); }
catch (err) {
  if (caller?.aborted) throw err;              // cancelled — stop, do not spend a retry
  if (n < MAX_ATTEMPTS - 1) await sleep(backoffMs(n));
}
```

## Rejections Nobody Observes

What an unobserved rejection does depends entirely on the runtime, and the
browser is the one with nothing. Pick the guard your entry point's row names.

| Runtime | Default on an unobserved rejection | Guard you must add |
|---|---|---|
| Node ≥ 15, `bun run` | Terminates the process, exit 1, with a raw V8 stack | TS-ASYNC-12: one awaited call in try/catch, mapped to your failure channel |
| Browser | Fires a cancelable `unhandledrejection`; the UA *may* log to console. Nothing else. | TS-ASYNC-10: a global listener that reports |
| Browser, Vue template handler | Vue attaches its own `.catch()`, so `unhandledrejection` **never fires**; production falls through to a bare `console.error` | TS-ASYNC-10: `app.config.errorHandler` *as well as* the global listener |
| VS Code extension host | Host catches it, attributes it to your extension, writes one line to an Output channel, continues | TS-ASYNC-02 — a silent rejection here is a debuggability failure, not a crash |
| `bun test` | Fails **whichever test was executing** when the rejection's microtask fired, not the one that created it | Nothing in production code; know it when a test failure looks misattributed |

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-ASYNC-10 | Every browser entry point registers `window.addEventListener('unhandledrejection', …)` that reports the rejection. A React error boundary does **not** substitute. A Vue app must **additionally** set `app.config.errorHandler` (or `onErrorCaptured`). | React's own docs exclude async code and promise rejections from an error boundary's catch surface. Vue fails the opposite way: it catches template-bound handler rejections itself, so the global listener can never see them and they land in a production `console.error`. Check both; assume neither. | `rg -n 'unhandledrejection' src/` — ≥ 1 per browser entry point. In a Vue app, `rg -n -e 'app\.config\.errorHandler' -e 'onErrorCaptured' src/` — ≥ 1. A hit for `ErrorBoundary`/`componentDidCatch` is not evidence for either. | MUST |
| TS-ASYNC-11 | A global rejection handler reports enough to locate the call site — reason, stack, and an id. Never add one, and never add a bare `catch {}`, to silence a lint finding or a red squiggle. | That converts a compile-time-catchable bug into a runtime-silent one. A handler that logs a bare reason string is a swallow with a log line. | `rg -n -i 'unhandledrejection' src/` — read each handler body; empty, or a bare `console.error(reason)`, fails. | MUST |
| TS-ASYNC-12 | A Node/Bun entry point's entire async execution is a single awaited call inside a try/catch that maps the error onto that runtime's failure channel — a named exit code for a CLI, the platform's failure API for a CI action. Do not rely on the runtime's default terminate. | Node has terminated by default since v15 (DEP0018 EOL), so the default *works* — it just reports a raw V8 stack and a generic code instead of your CLI's own exit vocabulary. CI action toolkits generally wire no rejection→failure bridge at all, so the step dies with a stack trace and is never marked failed through its own contract. | The bin/entry file contains exactly one top-level `await` (or one `void <selfCatchingRun>()`) inside a try/catch, and every catch branch ends in `process.exitCode = <named code>` or the platform's failure call. Complete only while TS-ASYNC-02 holds. | MUST |
| TS-ASYNC-13 | If you choose `Promise.allSettled`, handle every `rejected` result explicitly — log it, or fold it into the return. | An `allSettled` whose rejected branch does nothing is a `Promise.all` with extra steps and a silent swallow. It is the reflexive "make this robust" edit, and it removes the only thing that was reporting failures. | `rg -n 'Promise\.allSettled\(' src/` — each hit's result loop has a `status === 'rejected'` branch that does something observable. | SHOULD |

## Concurrency Bounds

No lint reaches any of this. TS-ASYNC-15/16 are read; TS-ASYNC-17 has a grep.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-ASYNC-15 | An array decoded from a network response (`await res.json()`, `JSON.parse` of fetched bytes) that drives per-element I/O must hit a concurrency bound **before that element's first `fetch`/`fs.write*`/`spawn`**. The bound belongs on the I/O, not on the `Promise.all` line. This does **not** extend to arrays of code-controlled length, or to local-only loops with a hard cap — do not add a limiter there. | 50,000 pending promises queued on a semaphore are cheap; 50,000 sockets and file descriptors are not. Node's global `fetch` has no ceiling of its own — undici's `Pool` defaults `connections: null`, explicitly unlimited. A bound placed on the `Promise.all` line while the real I/O happens three frames deeper is not a bound. | Reading heuristic. For each `Promise.all(`/`Promise.allSettled(` over a variable-length array, trace the array to its source; if it came off the wire, grep the mapped callback **and everything it calls one or two levels down** for `Semaphore\|acquire(\|pLimit\|new PQueue`, and confirm the *last* unguarded I/O call inside it is still inside the gate. A bound acquired, released, then followed by a fetch is not a bound. | MUST |
| TS-ASYNC-16 | The same wire-decoded array gets an explicit `length` cap, checked before the fan-out, **in addition to** TS-ASYNC-15's concurrency bound. | Two independent layers: the concurrency bound limits how many run at once, the length cap limits how much total work one hostile or oversized response can demand at all. Neither substitutes for the other. | For every site TS-ASYNC-15 flags, confirm a `length >` check (or equivalent) near where the array is decoded, distinct from the concurrency gate. | MUST |
| TS-ASYNC-17 | When a bound is needed: import the repo's existing limiter if one is reachable, otherwise add `p-limit`. Not `p-queue`, not a fresh hand-rolled class. Where `p-map` is used, pass `concurrency` **always**. | `p-map`'s `concurrency` defaults to `Infinity`, so `pMap(arr, fn)` looks bounded and is not. `p-queue`'s differentiators — `intervalCap`, priority, pause/resume — solve a problem you do not have unless a server-side rate limit is in play; a `p-queue` that only ever calls `.add(fn)` is `p-limit` with extra weight. | `rg -n 'pMap\(' src/` — every hit's options include `concurrency:`. A new concurrency class in a repo that could import one or use `p-limit` is a design review. | SHOULD |

## Numbers You Pin

These are **defaults, not contracts** — pin one number per call shape in your own
repo and this table is superseded. What is not negotiable: a number exists, and
"one attempt" and "the whole operation" never share it.

| Decision | Default | Change it when |
|---|---|---|
| Concurrency cap for a wire-sized fan-out | `16` | a measurement says otherwise — and the new number carries a comment saying which |
| Interactive / browser fetch deadline | ≤ 10 s | never longer without a written reason; this is a UI backing a person |
| CLI or CI validation fetch deadline | ≤ 30 s | a documented server-side operation genuinely takes longer |
| One attempt inside a retry loop | meaningfully shorter than the loop's total budget (e.g. 10 s against a ~1-minute 4-attempt budget) | always shorter — a per-attempt bound equal to the budget makes retries unreachable |
| Wire-decoded array length | pick one, throw before the fan-out | it is not driving I/O at all |

## What Agents Get Wrong Here

1. **Adding `void` to make a floating-promise warning go away without reading the
   callee.** The highest-frequency failure in this family, because it is the
   *documented* fix and because a safe `void` and an unsafe one are
   indistinguishable at the call site.
2. **`onClick={async () => {…}}` as the natural completion for "wire up a delete
   button."** It compiles, and in most repos it draws no lint either.
3. **"Cleaning up" that handler into `const handleX = async () => {…}` and
   referencing it by name.** Looks like an improvement, is volunteered unprompted,
   makes the violation invisible to every grep, and is exactly as wrong. Only
   TS-ASYNC-14's step 2 catches it.
4. **`Promise.race([op, delay(ms)])` as "the" timeout.** The dominant
   pre-`AbortSignal` idiom in training data; compiles, reads correctly in review,
   and leaves the socket running until undici reaps it minutes later.
5. **Reaching for a React `ErrorBoundary` when asked to "add error handling" to
   async UI code** — boundaries never catch it — or assuming a Vue app already has
   an `errorHandler` somewhere. Wrong twice, for two different reasons.
6. **`Promise.all(arr.map(async …))` as the reflexive "make it parallel" move**
   without asking what `arr`'s length depends on. Correct for a fixed list of two
   or three operations; TS-ASYNC-15's whole subject for a response-sized array.
7. **Bounding the `Promise.all` line and declaring it done** while the real I/O
   happens three frames deeper.
8. **Hallucinating a `timeout` option where none exists** — `fetch(url, {timeout:
   5000})`, `exec.exec(bin, args, {timeout: 30_000})`. TypeScript rejects both as
   object literals; the reflex fix is a cast, and a spread options object with an
   extra `timeout` key is dropped with no error at all.
9. **Adding a per-attempt timeout to a retry loop and stopping there** — not
   gating the backoff branch on the caller's signal. Passes a "does it time out"
   smoke test and retries a cancellation three more times.
10. **Treating a global `unhandledRejection` handler as a complete safety net**,
    then writing `void` freely under its cover. It prevents a crash, nothing more.
11. **Volunteering a shared `safeHandler(fn)` utility** when asked to handle
    rejections in event handlers. At a handful of sites it is the inline IIFE plus
    an import. Smallest tell: a new cross-cutting file whose only callers are two
    handlers.
12. **Trusting `pMap(arr, fn)` to be bounded** because the function is named after
    mapping, or reaching for `p-queue` because its README reads as the
    professional choice.
