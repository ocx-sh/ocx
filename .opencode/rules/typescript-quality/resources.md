---
title: Resources, Processes and Timers
summary: Owns the TS-RES family — child-process bounds and descendant kill, timer teardown, file handles, and `using`/`await using` disposal.
---

# Resources, Processes and Timers

Owns everything acquired that the runtime will not reclaim on its own: a child
process, a timer handle, a file handle, a disposable. Not here — `fetch`/HTTP
deadlines (TS-ASYNC), argv construction and shell injection (TS-SEC), and
extension-host activation and subscription discipline (TS-HOST).

Contents: [Every Child-Process Call Site](#every-child-process-call-site) ·
[Terminating a Child That Can Fork](#terminating-a-child-that-can-fork) ·
[Timers](#timers) · [Before the First `using`](#before-the-first-using) ·
[Using `using`](#using-using) ·
[What Agents Get Wrong Here](#what-agents-get-wrong-here) · [Sources](#sources)

Severity maps onto the house tiers: MUST = Block, SHOULD = Warn,
CONSIDER = Suggest.

## Every Child-Process Call Site

Enumerate once, then read each options object. All four rules below bind every
hit:

```bash
rg -n --glob '*.ts' --glob '*.tsx' -e 'execFile' -e 'execSync' -e '\bexec\(' -e '\bspawn' src
```

Node's defaults are permissive, not safe: `timeout` is `0` (disabled) and
`maxBuffer` is 1 MiB, which truncates the output and kills the child rather
than raising anything diagnostic. A call site passing no options is not using a
safe default; it is opting into "runs until the OS kills it."

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-RES-01 | Set an explicit `timeout` **and** `maxBuffer` on every call that runs a real external command; size `maxBuffer` to the payload, or state in a comment why the output is provably small. | A missing `timeout` hangs the caller with no recovery; a breached 1 MiB `maxBuffer` silently truncates and kills. The bound reaches the direct child PID only — TS-RES-15 covers the rest. | For each hit of the enumeration above, the options object contains both keys. A call passing no options object at all is the finding. | MUST |
| TS-RES-02 | Branch on `(error as NodeJS.ErrnoException).code === 'ENOENT'` before reporting a generic failure from a child-process call. | A spawn failure and a non-zero exit are structurally different events — one never ran. Collapsing them turns "the tool is not installed" into "the tool errored," and the user is told to debug the wrong thing. | For each file from the enumeration, `rg --files-without-match --glob '*.ts' -e 'ENOENT' -e 'isNotFound' <file>` — a printed path is the finding, silence is the pass. | MUST |
| TS-RES-03 | Attach `child.stdin.on('error', () => {})` **before** the first write to a child's stdin. | A child that exits before draining stdin turns the write into an uncaught `EPIPE`. [nodejs/node#40085](https://github.com/nodejs/node/issues/40085) is closed as *not planned*, so the race is permanent, not a bug awaiting a patch. | `rg -n --glob '*.ts' -e '\.stdin' src` — every file with a `.stdin.end(` or `.stdin.write(` needs the listener attached on the same object above it. A call site that never writes to stdin is out of scope. | MUST |
| TS-RES-17 | Never pass `killDescendants`, `forceKillAfterDelay`, `cancelSignal`, or `gracefulCancel` to a bare `child_process` `exec`/`execFile`/`spawn`. | These are `execa`-only option names. TypeScript strips the excess property and Node ignores the unknown key — no error, no warning, and the bound the author believes they added does not exist. The code reads as hardened and is not. | `rg -n --glob '*.ts' -e 'killDescendants' -e 'forceKillAfterDelay' -e 'cancelSignal' -e 'gracefulCancel' src` — a hit in a file that does not import from `execa` is the finding. Empty output is the pass. | MUST |

## Terminating a Child That Can Fork

`subprocess.kill()` reaches the direct child PID only — Node's own docs say
verbatim that "child processes of child processes will not be terminated." The
`timeout` option calls `child.kill()` internally, so it inherits that limit
exactly, and so does `AbortSignal`-driven termination. Node core has no
`killTree()` and nobody building one: [nodejs/node#64406](https://github.com/nodejs/node/issues/64406)
is open (filed 2026-07-10, no maintainer response); its 2021 predecessor was
bot-closed on staleness. Everything below is **best-effort by construction** —
a descendant that calls `setsid()` escapes a process-group signal, and Windows'
`taskkill /t` depends on parent-PID bookkeeping staying intact. Do not write
that Node will ship a fix.

| The call spawns… | Unattended? | What it needs |
|---|---|---|
| a single static binary that does not fork (`tar`, a compiled CLI) | either | `timeout` — TS-RES-01 |
| `git`, a package manager, or anything under `shell: true` | no | `timeout` — TS-RES-01 |
| `git`, a package manager, or anything under `shell: true` | **yes** | `timeout` **and** a group kill — TS-RES-15 |
| through an API that never returns the process handle | either | neither is reachable — TS-RES-10 |

**Unattended** means reachable from a runtime path an agent, a server, or a
scheduled job invokes. A build script, a prepublish smoke test, or an
interactive scaffold's `npm install` is supervised by a human or a CI job that
already owns tree cleanup; hardening those is scope creep.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-RES-15 | On an unattended call that spawns `git`, a package manager (`npm`/`pnpm`/`yarn`/`bun`), or anything under `shell: true`, pair the `timeout` with a group kill — `detached` at spawn plus your own timer calling `process.kill(-pid, …)` or `taskkill` — never the built-in `timeout` option alone. | The built-in kill reaches one PID. `git` over `ssh://` spawns `ssh`, which spawns `GIT_ASKPASS`; `npm` spawns lifecycle scripts. The hang stops and the descendant keeps the port, the lock and the credential prompt. `Bun.spawn` has the identical single-PID limit — not an escape hatch. | From the enumeration above, classify each site by the table. For each qualifying site: the options object sets `detached`, and the timeout handler calls a platform-branching kill helper, not `child.kill()`. `rg -n --glob '*.ts' -e 'shell:\s*true' src` — every hit is in scope unconditionally. | MUST |
| TS-RES-16 | Write `detached: process.platform !== "win32"`, never a bare `detached: true`; make the Windows branch `taskkill /pid <pid> /t /f`; and never add `.unref()` to a child the caller still awaits. | On Windows `detached` means "own console window, outlives the parent" — an unrelated behaviour that pops a visible window on every run, with no process group Node can address by PID sign. `.unref()` is orthogonal to `detached` and silently changes whether the parent waits at all. | `rg -n --glob '*.ts' -e 'detached' src` — every hit is a platform expression, not `true`. `rg -n --glob '*.ts' -e 'process\.kill\(-' src` — every hit sits inside a non-`win32` branch. `rg -n --glob '*.ts' -e 'taskkill' src` — every hit carries both `/t` and `/f`. | MUST |
| TS-RES-10 | Never wrap a call in `Promise.race`/`Promise.any` to fake a timeout when the API never hands back the child process; move that one call to `execFile` and leave the rest. | `@actions/exec`'s `exec`/`getExecOutput` return `Promise<number>` and never expose a `ChildProcess` — its `ToolRunner` calls `.kill()` on no path, and its internal `delay` is a post-exit stdio drain. Racing stops the caller waiting while the process runs on orphaned. | `rg -n --glob '*.ts' -e 'Promise\.race' -e 'Promise\.any' src` — a hit racing a call that returns no process handle is the finding, unconditionally, not a judgment call. Empty output is the pass. | MUST |
| TS-RES-18 | Do not add `execa` or `tree-kill` for fewer than three call sites that actually meet TS-RES-15's bar; before adding either, diff its floor against your declared `engines.node` **and** against the runtime that is not `npm install`-selectable. | `execa@10.0.1` is ESM-only, `engines.node: ">=22"`, ~11 transitive deps. `tree-kill` is the only tool that walks the real process tree — and its last publish is 2019-12-11. A PR that bumps `engines.node` to satisfy a dependency added for one call site has moved the package's published contract for one function. | Count qualifying sites before any `package.json` edit. `npm view execa engines.node` against your own `engines` field — and against an editor extension host's bundled Node or a CI action's declared runtime, neither of which `engines` can move. **Adopter default:** three is a pinned project threshold, not a derived fact; set your own and write it down. | MUST |

```ts
// Wrong. `timeout` calls child.kill() — reaches `git`, never the `ssh` it spawned.
execFile("git", args, { timeout: 30_000, maxBuffer: 4 << 20 }, cb);

// Right. Own the timer; signal the group.
const child = execFile("git", args, { detached: process.platform !== "win32", maxBuffer: 4 << 20 }, cb);
setTimeout(() => { if (child.pid) killTree(child.pid, "SIGTERM"); }, 30_000).unref();
```

## Timers

```bash
rg -n --glob '*.ts' --glob '*.tsx' --glob '*.astro' -e 'setTimeout' -e 'setInterval' src
```

Three shapes, one leak. **Awaited-as-sleep** (`await new Promise(r => setTimeout(r, ms))`)
owns no handle and is exempt from both rules below. **Debounce/rearm** is
TS-RES-08. **Fire-and-forget** is TS-RES-09. A raw count of `setTimeout` against
`clearTimeout` is evidence of nothing — the shape is what binds, and a
file-extension-typed glob that misses `.astro`, `.vue` or `.svelte` will report
a clean zero.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-RES-08 | Clear the previous handle before reassigning a debounce/rearm timer to the same variable. | The stale callback stays scheduled and fires against state a newer trigger already replaced — a fade that stops mid-flight, a save that lands after its successor. | From the enumeration: for every `let`/`var` timer handle reassigned more than once in a file, a `clear*` on that variable precedes every reassignment after the first. | MUST |
| TS-RES-09 | Give every `setInterval` and every fire-and-forget `setTimeout` either a `clear*` reachable from its owner's teardown path, or a comment naming an *independently checkable* realm-teardown guarantee. | An uncleared interval is correct only where the whole script realm is destroyed atomically — a webview or iframe document replaced wholesale. The identical reasoning is false in the host process, which outlives every panel it opened, and the comment travels by copy-paste while the guarantee does not. | For each hit: a reachable `clear*`, **or** the file is imported only from the realm-destroyed entry point and never from the host's activation path, **and** nothing in that panel's registration retains its context across a hide. Awaited sleeps are exempt. | MUST |

## Before the First `using`

Two preconditions. Both fail at runtime, not at `tsc --noEmit`, so a
type-check-only gate reports green on either.

Native, unpolyfilled support, from MDN browser-compat-data as of 2026-08-29:
**Node ≥24.0.0, Chrome/Edge ≥134, Firefox ≥141, Bun ≥1.0.23, Deno ≥2.2.10.
Safari has never shipped it.** Node 22 ships V8 12.4 — below the line for that
whole LTS, so "modern Node" is not the same claim as "native `using`."

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-RES-04 | Never emit `using`/`await using` at `target: "esnext"` (tsc) or an `esnext` bundler target for code that reaches a runtime below the floor above. | At `esnext` both tsc and esbuild emit the syntax completely unlowered. A pre-native engine cannot *parse* it — a load-time `SyntaxError`, not a missing-API error, and invisible to a type check. `esnext` means "no lowering," not "modern and safe." | `rg -n --glob '*.ts' -e '\busing ' src` — if it returns anything, `rg -in -e 'target' tsconfig.json` and the bundler config; `esnext` in either is the finding. On a Vite 8 / Rolldown / Oxc toolchain, additionally run the repro in [oxc#25155](https://github.com/oxc-project/oxc/issues/25155) (open as of 2026-08-29) against the project's real browserslist — Oxc treats an engine absent from a feature's compat table as *supporting* it, so `using` ships unlowered to Safari with no build warning. | MUST |
| TS-RES-05 | Assign `Symbol.dispose ??= Symbol('Symbol.dispose'); Symbol.asyncDispose ??= Symbol('Symbol.asyncDispose');` before any module defining a `[Symbol.dispose]` member evaluates — under ESM that means a dedicated polyfill module listed as the entry point's **first** import, not a statement in the entry file's body. | Computed class keys resolve at class-definition time, and ESM hoists imports above your body statements — so a transitively imported class lands its method under the literal string key `"undefined"`. tsc then throws `TypeError: Symbol.dispose is not defined.` and esbuild throws `Object not disposable` at the first `using`. Both are runtime failures a `--noEmit` gate cannot see. | Read each entry point: the polyfill import is first, above every other import. Then one smoke test that actually **executes** a `using` block, run on the version named in `engines.node` — not the newer one the developer has locally. | MUST |

## Using `using`

The rule is the narrow one: *never hand-roll `try/finally` where the resource
already implements `Symbol.dispose`/`Symbol.asyncDispose`* — not *prefer `using`
for anything disposable-shaped*. The broad form drags in every
component-lifetime and session-lifetime resource that TS-RES-06 excludes.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-RES-06 | Bind with `using`/`await using` only where the resource's lifetime **is** the enclosing block — never a value that is returned, stored on `this` or a ref, or pushed onto a subscription list. | Disposal fires before a `return` completes, so a returned binding hands the caller an already-disposed object. Session- and component-lifetime resources are the wrong shape entirely, and a mechanical `try/finally` conversion produces this every time. | `rg -n --glob '*.ts' -e '\busing ' src`; for each binding, does that identifier later appear in a `return`, an assignment outside the block, or a `.push(`? Any yes is the finding. | MUST |
| TS-RES-07 | Bridge a foreign disposal protocol through exactly one `toDisposable` adapter per package, and never cast a value into a `using` binding. | An editor's `Disposable` protocol carries zero `Symbol.dispose` members, so the direct bind fails the type checker — and the habitual fix, `as unknown as T`, papers over the check and produces a resource that silently never disposes. One adapter keeps the block-scoped/session-scoped boundary auditable in one file. | `rg -n --glob '*.ts' -e 'using .*as unknown as' -e 'using .*as any' src` — any hit is the finding. `rg -n --glob '*.ts' -e '\[Symbol\.dispose\]:' src` — a hit outside the adapter's own file is the finding. | MUST |
| TS-RES-11 | Replace a `try/finally` whose `finally` only calls `.close()`/`.dispose()` with `await using` when the resource already implements the protocol — `fs/promises` `FileHandle` first. | `FileHandle` implements `Symbol.asyncDispose` natively and Node's own docs call GC-based auto-close unreliable. This is the one mechanical conversion; everything else is a rewrite wearing a refactor's clothes. | `rg -n -A2 --glob '*.ts' -e 'finally \{' src` — a body that only closes or disposes is a candidate. `rg -n --glob '*.ts' -e 'fs.*\.open\(' src` — a handle not bound with `await using` is a candidate. TS-RES-04 and TS-RES-05 must already hold. | SHOULD |
| TS-RES-12 | Register a `DisposableStack`/`AsyncDisposableStack` member in the same statement as its acquisition — `disposer.use(acquire())`, never on a following line. | Anything inserted between acquisition and `.use()` leaks the resource if it throws. | `rg -n --glob '*.ts' -e 'DisposableStack' src` names the files; in each, every `.use(` argument is itself a call or `new` expression on the same line. | SHOULD |

```ts
// Wrong. Disposal fires before the return completes; the caller gets a dead handle.
async function read(p: string) { await using fh = await open(p); return fh; }

// Right. The resource's lifetime is the block — return the value, not the resource.
async function read(p: string) { await using fh = await open(p); return fh.readFile(); }
```

## What Agents Get Wrong Here

1. `Promise.race` around a handle-less exec API to "add a timeout." It compiles,
   type-checks and appears to work in a manual test, and leaves the process
   running orphaned. The highest-value unconditional check in this file.
2. Adding a `timeout` to a `git`/`npm`/`shell: true` call and reporting the hang
   fixed. Correct as far as the option's own doc paragraph goes — Node buries
   the descendant caveat in the `kill()` section instead.
3. One `catch` for both a spawn failure and a non-zero exit. Looks complete;
   destroys the only distinction that tells a user which of the two to fix.
4. Assuming `using` "just works" once `lib` is set. The lib entry satisfies the
   type checker; the missing polyfill throws at the first execution, on a branch
   the tests may never reach.
5. Copying a "never cleared, the realm dies with it" comment into host-process
   code. The precedent is real and realm-specific; the citation looks authoritative
   while the guarantee is gone.
6. Casting past the `using` type error with `as unknown as`. The compiler caught
   the real bug; the cast converts it into a resource that never disposes.
7. Hallucinating `execa` option names onto bare `child_process` — most likely
   right after reading execa's docs in the same session.
8. Pasting `process.kill(-pid, sig)` onto a cross-platform path. The negative-PID
   idiom reads as portable; on Windows it throws or resolves to nothing.

## Sources

- [`child_process`](https://nodejs.org/api/child_process.html) and [`subprocess.kill()`](https://nodejs.org/api/child_process.html#subprocesskillsignal) — `timeout`/`maxBuffer`/`killSignal` defaults, ENOENT vs. exit-code semantics, and the verbatim "child processes of child processes will not be terminated"
- [`options.detached`](https://nodejs.org/api/child_process.html#optionsdetached) — the two unrelated meanings of one flag; [`FileHandle`](https://nodejs.org/api/fs.html#class-filehandle) — native `Symbol.asyncDispose`, GC-based close documented unreliable
- [nodejs/node#40085](https://github.com/nodejs/node/issues/40085) (stdin EPIPE, *not planned*), [#64406](https://github.com/nodejs/node/issues/64406) (open `killTree` request), [#40438](https://github.com/nodejs/node/issues/40438) (its predecessor, and why a generic tree-kill is hard)
- [TypeScript 5.2 release notes](https://www.typescriptlang.org/docs/handbook/release-notes/typescript-5-2.html) — `using`/`await using`/`DisposableStack` semantics and the `lib`/`target` requirement; [MDN: DisposableStack](https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/DisposableStack) — the register-at-acquisition leak window
- [esbuild v0.18.7 changelog](https://github.com/evanw/esbuild/blob/main/CHANGELOG-2023.md) — lowering for "all targets other than esnext," and its explicit refusal to polyfill `Symbol.dispose`; [oxc#25155](https://github.com/oxc-project/oxc/issues/25155) — `using` shipped unlowered to Safari
- [`@actions/exec` interfaces.ts](https://github.com/actions/toolkit/blob/main/packages/exec/src/interfaces.ts) and [toolrunner.ts](https://github.com/actions/toolkit/blob/main/packages/exec/src/toolrunner.ts) — proof there is no `timeout`/`signal` and no kill path
- [execa termination guide](https://github.com/sindresorhus/execa/blob/main/docs/termination.md) — the exact `killDescendants`/`forceKillAfterDelay`/`cancelSignal` contracts, and execa's own best-effort caveat; [taskkill](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/taskkill) — `/t` and `/f`
