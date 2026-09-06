---
title: Extension Host
summary: What a VS Code or Electron extension host does differently from server Node — activation order, disposal, the webview boundary, workspace trust, the esbuild-to-CJS bundle, and typed test doubles
---

# Extension Host

Owns the `TS-HOST` family: everything that is true because your TypeScript runs
inside a shared host process you do not control, and false in a server. It does
not own which bundler you use, your lint or CI wiring, or general module
hygiene — those belong to sibling families and are not restated here.

Contents: [Read This First](#read-this-first) · [Trust](#trust) ·
[Activation and the Host Process](#activation-and-the-host-process) ·
[The Webview Boundary](#the-webview-boundary) ·
[Bundle Semantics](#bundle-semantics) ·
[Faking Host APIs in Tests](#faking-host-apis-in-tests) ·
[What Agents Get Wrong Here](#what-agents-get-wrong-here) · [Sources](#sources)

Severity maps onto the house tiers: MUST = Block, SHOULD = Warn,
CONSIDER = Suggest.

## Read This First

**The extension host does not die from your JavaScript.** It installs
`process.on('unhandledRejection')` and `process.on('uncaughtException')` before
any extension code loads and routes both to its own error reporter; it
intercepts `process.exit()` and `process.crash()` and downgrades them to a
`console.warn`. Every server-Node crash reflex is inert here. A floating
rejection is not a crash — it is a context-poor report roughly a second later
with the real call site already gone, which is worse to debug, not better.

**"Extension host crashed" with no `terminated unexpectedly` notification is
not a crash.** It is the RPC layer's unresponsiveness signal, which fires after
3000 ms of a blocked event loop and kills nothing. A blocking synchronous frame
produces it; a thrown error does not. Wrapping the suspect call in `try`/`catch`
fixes nothing, because nothing threw.

**`untrustedWorkspaces.supported: "limited"` grants nothing.** `false` means the
host refuses to activate. `"limited"` means full activation with *zero*
automatic gating beyond substituting user-level values for the exact setting
keys named in `restrictedConfigurations`. Every other trust-sensitive path is
yours. This is the highest-severity misreading in the family.

Two rules below pin a project decision rather than derive one, and an adopter
may override either as long as they override it deliberately: **TS-HOST-18**
(`Node16` module resolution, chosen because it is the only setting that makes
the bundler's silent failures into compile errors) and **TS-HOST-25** (the
mock-library choice for host interfaces). Both name the reasoning, so a
different answer is a decision, not a drift.

## Trust

The check is one reading pass, not a grep: `rg -n 'registerCommand\(' src`
enumerates the handlers, and each one is traced forward to any process spawn,
module resolution, or filesystem read.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-HOST-01 | Gate every command handler that spawns a process, resolves a module, or reads a file using workspace-derived data — a resolved manifest path, a `cwd` inside the workspace, file content — on `workspace.isTrusted`, or hide the command behind an `isWorkspaceTrusted` `when` clause. Applies even when the executable's own path is listed in `restrictedConfigurations`. | `restrictedConfigurations` protects the *setting keys it names* and nothing else; protecting the binary's path does not protect what the workspace tells that binary to do. A command reachable from the Command Palette in a restricted window executes attacker-authored manifest content. | Trace each `registerCommand` handler to its spawn/resolve/read. Then `rg -n 'isTrusted' src` and `rg -n 'isWorkspaceTrusted' package.json` — a traced handler appearing in neither output is the finding. A gate on a *sibling* automatic path does not count. | MUST |

## Activation and the Host Process

`TS-HOST-04` is enforced by `@typescript-eslint/no-floating-promises`, which
needs type-aware linting (`parserOptions.projectService`) — wire that once and
the rest of this block is reading and grep.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-HOST-02 | Order `activate()` so every step that can throw — `JSON.parse`, config validation, path resolution — runs before the first `context.subscriptions.push(...)`; otherwise catch, dispose what was already pushed, and rethrow. | The host wires up disposal of `context.subscriptions` only on `activate()`'s success path. A mid-body throw leaves every earlier registration live and unreachable for the life of the window. | Read `activate()` top to bottom, find the first `context.subscriptions.push`, and confirm everything after it is either a registration call that cannot throw or is itself wrapped. | MUST |
| TS-HOST-03 | Return from `activate()` without awaiting slow initialization — a spawn, a fetch, a workspace scan. Start that work as a call whose own chain already ends in `.catch(...)`. | The host awaits the returned promise before considering the extension activated; a stalled chain pins the extension at "Activating…" with no timeout you control. | Read the literal `return` in `activate()`; any `await` on a spawn, fetch, or filesystem call on the path to it is the finding. | SHOULD |
| TS-HOST-04 | Every promise started in `activate()` or in a registered callback that is neither returned nor awaited terminates its own chain in `.catch(...)`. `void f()` documents intent and attaches nothing. | It will not crash the host — it surfaces about a second later through the host's own reporter as a generic error with the originating frame gone. | `@typescript-eslint/no-floating-promises` where type-aware linting is wired. Otherwise `rg -n 'void [a-zA-Z_$][\w$.]*\(' src` and read whether each named callee's body ends in a `.catch` or a `try`/`catch`. Empty output is a pass only under the lint. | MUST |
| TS-HOST-05 | Never call `process.exit()` or `process.crash()` from extension code, and never register `process.on('uncaughtException', …)` or `process.on('unhandledRejection', …)` in it. | Both exits are intercepted and downgraded to a warning outside a test harness, so the intended abort silently does nothing; the host installed its own handlers before your code loaded, so an added one is redundant and hides which reporter actually fires. | `rg -n -e 'process\.exit\(' -e 'process\.crash\(' -e 'uncaughtException' -e 'unhandledRejection' src` — every hit outside a test harness is the violation. Empty output is the pass. | MUST |
| TS-HOST-06 | Never compile extension-host code and webview code under one `lib`. Host code gets `["ES2022"]`; webview code gets its own tsconfig carrying `DOM` and `DOM.Iterable`. | A shared `lib` hands `document`, `window`, and DOM-adjacent types to code running in Node with no DOM. The compiler accepts a call that throws at runtime. | `npx tsc -p <tsconfig> --showConfig` per config (tsconfig is JSONC — `jq` on the raw file is unreliable) and read `compilerOptions.lib` against `files`. A config whose file list covers both host sources and webview sources while listing `DOM` is the finding. | MUST |

## The Webview Boundary

The seam between the host and a webview is a trust boundary in both directions.
The three MUSTs here are each one grep over the files that assign
`webview.html` or construct a panel.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-HOST-09 | Runtime-check every field read from `onDidReceiveMessage` or a `message` event listener before it reaches a privileged sink — a filesystem path, an argv slot, `env.openExternal`, `Uri.parse`, a DOM-write sink. Union-typed enum fields included: a field declared as the union of `'project'` and `'global'` is exactly as unchecked at runtime as `string`. | Type annotations are erased before the program runs. The declared union constrains what the sender's *compiled* code may construct, never what a compromised, stale, or version-skewed sender actually sends. The platform's own tutorial validates nothing and is the shape a model reproduces. | For each `case` in the message switch, confirm a `typeof`, `in`, regex test, literal-membership check, or schema call precedes the first use of the field. A shared `.ts` union between the two sides is not a check. | MUST |
| TS-HOST-07 | Set `localResourceRoots` explicitly on every `createWebviewPanel` and `resolveWebviewView`, to the narrowest directory the webview actually loads from. | The default is not "nothing". It is the extension install directory **plus the user's entire open workspace** — `.env` and credential files included. | `rg -n -e 'createWebviewPanel\(' -e 'resolveWebviewView' src` — an options object with no `localResourceRoots` key is the finding. | MUST |
| TS-HOST-08 | Every webview's HTML carries a `<meta http-equiv="Content-Security-Policy">` starting from `default-src 'none'` with `script-src 'nonce-<per-render random>'`, emitted by one shared helper per repository. | `script-src ${webview.cspSource}` — the documentation's baseline — permits any script under the webview's asset root. A nonce permits only the tag stamped for that render. One helper stops sibling panels drifting apart. | `rg -n 'webview\.html' src` lists the assigners; `rg -n 'cspSource' src` — a `cspSource` inside a `script-src` is the finding. Confirm one shared function produces all of them. | MUST |
| TS-HOST-11 | Every argument to a "render trusted HTML" directive (`unsafeHTML` and equivalents) traces to a string literal, to a markdown renderer constructed with HTML output disabled, or to an explicit sanitizer call — never directly to fetched, registry-supplied, or user-supplied text. | These directives are documented as *not* sanitizers and use `innerHTML` internally. Where safety rests on a renderer flag, no lint verifies it and it breaks silently when someone flips the flag for an unrelated feature. | `rg -n 'unsafeHTML\(' src` and trace each argument to a literal, an HTML-disabled renderer factory, or a sanitize call. Supplement with `rg -n -e 'innerHTML' -e 'insertAdjacentHTML' -e 'DOMParser' -e 'srcdoc' -e 'createContextualFragment' src`: the sink linters miss `DOMParser#parseFromString` and iframe `srcdoc` entirely. | MUST |
| TS-HOST-10 | Keep host-to-webview and webview-to-host payload types JSON-shape-only. No class instance, `Map`, `Set`, `Date`, or function type in any union member. | The channel is structured clone and *could* carry them, but the documented contract is JSON-serializable data. A `Date` field silently breaks a JSON-based mock, a snapshot test, and any non-Electron webview host. | Resolve every type in the message unions recursively; `rg -n -e 'Map<' -e 'Set<' -e ': Date' <protocol-file>` catches the shallow cases. | SHOULD |

The wrong shape is the one the platform documentation teaches, so it arrives
with a model's full confidence:

```ts
// Wrong. `message.scope` is a compile-time union and a runtime anything.
case 'setValue': await run(['config', 'set', message.scope, message.key]);

// Right. The union is re-established at the boundary it was erased at.
case 'setValue':
  if (message.scope !== 'project' && message.scope !== 'global') return;
```

Do not substitute an `event.origin` check for this. It addresses a threat model
that does not apply to the host seam and validates nothing about the payload.

## Bundle Semantics

esbuild ignores tsconfig's `module` and `moduleResolution` entirely — the
bundler's own `format` decides the output. So `module: "Node16"` buys exactly
one thing, and it is the thing that matters: it turns this section's two silent
failures into compile errors before the build runs. Measured 2026-08-29 against
TypeScript 6.0.3 and esbuild 0.28.1, with no `type` field in `package.json`.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-HOST-18 | Set `"module": "Node16"` and `"moduleResolution": "Node16"` on every tsconfig in an extension that bundles to CJS — and on *any* tsconfig driving real emission (a `tsc -p … --outDir` step), never `Bundler`. Pinned default: override it only with a stated reason, never to "align" one project with a sibling's `ESNext`/`Bundler`. | `Node16` raises **TS1470** on `import.meta` and **TS1309** on top-level `await`; `ESNext`/`Bundler` raises neither, which is how both constructs reach a bundler that mishandles them. On a `tsc`-emitting path, `Bundler` emits ESM syntax into extensionless CJS-loaded files: `SyntaxError` on first test run. Costs no `.js` import extensions unless `"type": "module"` is set. | `npx tsc -p <tsconfig> --showConfig` prints `Node16` for both keys. Keep the `tsc --noEmit` step ahead of the build in the `check`/`pretest` scripts — dropping it to "speed up CI" removes the only static gate these two constructs have. | MUST |
| TS-HOST-19 | Never write `import.meta` — `import.meta.url` included — or a top-level `await` in any file reachable from an esbuild entry point built as `cjs` or `iife`. Use `__dirname` (subject to TS-HOST-22) or a path joined onto the extension's own install path. | `import.meta` is this family's one genuinely silent build divergence: esbuild emits `var import_meta = {}` under a *warning*, and a config that silences the bundler swallows that warning end to end, leaving only an `undefined` several frames downstream. Top-level `await` is loud but still a wasted round trip. | `rg -n 'import\.meta' src` returns nothing. With TS-HOST-18 in place both constructs are compile errors at the moment they are typed. | MUST |
| TS-HOST-20 | Keep the bundle's `external` list to modules the host injects — the `vscode` module, plus the test runner in a test bundle. Never set `packages: 'external'`. Never add a third name without changing the packaging ignore file and the package script in the same commit. | `vscode` lives in the host's `require` cache and never exists on disk, so it must stay external. Everything else must stay *in* the bundle: package manifests that exclude `node_modules` and package with dependencies disabled mean an externalized dependency is simply absent from the shipped archive — `Cannot find module` at activation, which lint, build, and test all pass because they run against the dev tree. The most silent failure in this file. | `rg -n -e 'external:' -e 'packages:' <build-script>` — a name beyond the injected ones, or any `packages` key, is the finding. The only real gate is installing the packaged artifact and smoke-activating it. | MUST |
| TS-HOST-21 | Load any dependency you intend to keep out of the bundle with `await import('pkg')`, never a static top-level `import`. Treat `require()` of an ESM-only package as unsafe against the *declared* minimum host version, not against the host you are running. | esbuild leaves an external dynamic `import()` as a real `import()`, safe on any Node and any package shape; a static import of the same package becomes a literal `require("pkg")`. `require(esm)` only became flag-free in Node v20.19.0 / v22.12.0, and a declared floor two years old can pin an Electron below that line. A module whose graph uses top-level `await` throws `ERR_REQUIRE_ASYNC_MODULE` on *every* Node version. | `rg -n -e "^import .* from '" -e "^import '" src` lists every static import; cross-check each specifier against the build script's `external` list. To resolve the floor: read the host repository's `release/<declared-version>` manifest for its pinned Electron, then that Electron release's `node` field. | MUST |
| TS-HOST-22 | Read every `__dirname` and `__filename` in bundled source as *the output file's* directory, never the `.ts` file's. A dependency that reads a file relative to its own `__dirname` cannot be bundled — and by TS-HOST-20 it cannot be externalized either, so that combination is a packaging decision, not a flag. | Verified: two source files three directories apart, folded into one bundle, report an identical `__dirname` equal to the output directory. `path.join(__dirname, '../assets/x')` is then silently wrong, or accidentally right, depending on how the two trees line up. | `rg -n '__dirname' src` — check each hit against the bundle's output directory. Prefer the extension context's own path where one is in hand. | SHOULD |
| TS-HOST-23 | A bundler config that sets `logLevel: 'silent'` must have an `onEnd` handler that prints `result.warnings` as well as `result.errors`. | A silenced bundler plus an errors-only problem matcher means every warning — TS-HOST-19's included — reaches no terminal, no matcher, and no CI log. TS-HOST-18 closes today's case statically; a warning channel muted by construction will swallow the next one too. | `rg -n 'logLevel' <build-script>` — if `'silent'`, then `rg -n 'result\.warnings' <build-script>` must be non-empty. Empty is the finding. | SHOULD |

## Faking Host APIs in Tests

Every rule here is caught by one grep, `rg -n 'as unknown as' src`, read
per site. The categories are not interchangeable, and treating all casts as one
problem is the characteristic failure:

| The cast targets… | What you actually wanted | Rule |
|---|---|---|
| A class with a public constructor (`Uri`, `EventEmitter`, `Disposable`, `Position`, `Range`, `Selection`) | The real constructor. No fake, no cast. | TS-HOST-13 |
| A live host singleton (`window`, `commands`, `authentication`) so one method can be reassigned | `sandbox.stub(target, 'method')` — the live object, two methods swapped and put back | TS-HOST-24 |
| A constructor-less interface (`WebviewPanel`, `WebviewView`, `OutputChannel`, an environment collection) | A deep-partial mock factory, or one named fake factory where no library is present | TS-HOST-25, TS-HOST-14 |

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-HOST-16 | Never introduce a generic `fake<T>(partial: Partial<T>): T` helper. This does not ban a deep-partial mock library, which is a different mechanism. | Verified by compiler run: `fake<Foo>({ a: 'x' })` against a three-member `Foo` compiles clean with two members absent, because the helper's internal `as T` is checked against the unconstrained type parameter, not against what the caller passed. It reads as DRY and disciplined and protects nothing. A library whose parameter type is a real `DeepPartial` does not have this hole. | `rg -n -e 'function fake<T' -e 'function mock<T' .` — any hit is the wrong shape. Empty output is the pass. | MUST |
| TS-HOST-13 | Never fake a `vscode` type the type declarations export as a class with a usable public constructor. Construct the real one. | These are pure-logic classes with no host dependency. A fake costs a cast, can drift from real behaviour, and buys nothing. The adjacent error is the opposite one — `new vscode.WebviewView(...)`, a constructor that has never existed. | `rg -n -e 'as unknown as vscode\.Uri' -e 'as unknown as vscode\.EventEmitter' -e 'as unknown as vscode\.Disposable' -e 'as unknown as vscode\.Position' -e 'as unknown as vscode\.Range' -e 'as unknown as vscode\.Selection' src` — every hit is a violation with no legitimate reading. | MUST |
| TS-HOST-24 | Never cast to monkeypatch a live host singleton. Replace the method with `sandbox.stub(vscode.window, 'showErrorMessage')` from one file-level `sinon.createSandbox()`, restored in a single `afterEach(() => sandbox.restore())`. | The `stub<T, K extends keyof T>(obj, method)` overload imposes no readonly constraint and type-checks against the live singleton with **zero** assertion — the cast existed only to narrow enough to assign. A substitute object silently loses every other member the same test run reads, and it replaces the hand-rolled `try`/`finally` restore blocks with one hook. The tell: if the test re-reads other members of that object later, it wanted a stub, not a fake. | From the `as unknown as` sweep, any cast whose target is an inline object type standing in for a host singleton is the finding. Then `rg -c 'sandbox\.restore\(\)' <test-file>` — exactly one per file that stubs. | MUST |
| TS-HOST-25 | For a constructor-less host interface, or an app class faked to avoid its real side effects, build the double with a deep-partial mock factory typed as `DeepPartial<T>` in and `T` out — no call-site assertion and no repository-local helper. Pinned default: `sinon` plus `@golevelup/ts-sinon`'s `createMock<T>()`, whose peer range is `sinon@^21` (checked 2026-08-29). | A return type that intersects `T` is already a complete `T`, so no cast is needed anywhere, and a typo'd nested member fails as TS2561. Writing your own is TS-HOST-16. Overriding the library choice is fine; overriding the *shape* — deep-partial in, full type out — reintroduces the hole. | `npm ls sinon @golevelup/ts-sinon` reports no peer warning. `rg -n -e 'as unknown as vscode\.WebviewPanel' -e 'as unknown as vscode\.WebviewView' -e 'as unknown as vscode\.OutputChannel' src` returns nothing outside a migration window. | SHOULD |
| TS-HOST-26 | In every deep-partial mock call, explicitly provide each **plain-data** member the code under test reads — not only the function members. Omit only members the subject never touches. | The Proxy `get` trap auto-vivifies *any* unprovided member, function or not: an unfaked `panel.title` comes back as a callable stub wearing a string's name, so `panel.title.toUpperCase()` and template interpolation misbehave instead of failing loudly, and `JSON.stringify` hides it by dropping functions. Read from the published implementation, not documented upstream. | Diff the subject's reads against the partial's keys, per call. A clean `tsc --strict` is not evidence here — the intersection type says the member exists. | MUST |
| TS-HOST-14 | Where no mock library is present, a fake of a constructor-less interface is one named, colocated `function fakeX(...)` or `class FakeX`, declared once and reused. No bare `const x = { … } as unknown as T` inside a test body. | The double cast is TypeScript's own sanctioned escape hatch for a genuinely partial fake — `as T` alone is rejected as TS2352 — so the rule bounds where it may appear rather than banning it. Unbounded, the same literal is retyped inline at a dozen sites and drifts at each. | From the `as unknown as` sweep, a hit inside a test body rather than inside a `function fake*` / `class Fake*` declaration is the finding. Before writing a new fake, `rg -n -e '^function fake' -e '^function stub' <test-file>` — an existing helper of the same shape is the answer. | MUST |

## What Agents Get Wrong Here

Ranked by how often it bites.

1. Reaching for `fake<T>(partial: Partial<T>): T` as "the clean solution."
   Treat a model proposing it as having produced the weakest option available,
   not the best. (TS-HOST-16.)
2. Reproducing the platform tutorial's unvalidated message switch —
   `case 'alert': showErrorMessage(message.text)`, untyped and unguarded, the
   shape the docs and every blog copying them teach. (TS-HOST-09.)
3. Server-Node crash reflexes: `process.on('unhandledRejection', () =>
   process.exit(1))`. Both halves are inert here, so the safety net does
   nothing while looking like diligence. (TS-HOST-05.)
4. Reading `"limited"` as "the platform handles it" and skipping the gate on a
   new risky command. (TS-HOST-01.)
5. Writing `import.meta.url` + `fileURLToPath` for "my own directory" because
   the tsconfig says `ESNext`. The most-trained ESM idiom, type-checking clean,
   building with a warning nobody prints. (TS-HOST-19, TS-HOST-18.)
6. Marking a new dependency `external` to keep the bundle small — correct in a
   general Node context, and here it ships an archive that throws
   `Cannot find module` on activation, past every local script. (TS-HOST-20.)
7. Returning an awaited bootstrap promise from `activate()` as "the robust
   shape." Generic async-bootstrap idiom, exactly backwards here. (TS-HOST-03.)
8. Loosening an existing nonce CSP to `script-src ${webview.cspSource}` because
   that is the documentation's first example. (TS-HOST-08.)
9. Treating a "render trusted HTML" directive as the sanitized way to render
   HTML — `dangerouslySetInnerHTML` muscle memory transplanted into a library
   that documents the opposite. (TS-HOST-11.)
10. Assuming `require()` of an ESM-only package is fine because Node 22 supports
    it. True of the host the agent runs on, false of the floor the manifest
    declares — and that floor's Node is not visible without the Electron
    lookup. (TS-HOST-21.)
11. Reimplementing `restrictedConfigurations` by hand with `.inspect(key)` and
    manual value picking, for keys the platform already substitutes correctly.
12. Diagnosing the 3000 ms unresponsiveness signal as a crash and adding a
    `try`/`catch` around something that never threw.

## Sources

- [Extension host](https://code.visualstudio.com/api/advanced-topics/extension-host) — the shared-process intent
- [`extensionHostProcess.ts`](https://github.com/microsoft/vscode/blob/main/src/vs/workbench/api/node/extensionHostProcess.ts) — the rejection and exception handler overrides, and the `process.exit`/`crash` interception
- [`extHostExtensionService.ts`](https://github.com/microsoft/vscode/blob/main/src/vs/workbench/api/common/extHostExtensionService.ts) — `_callActivate`, and the subscriptions leak on partial activation failure
- [`rpcProtocol.ts`](https://github.com/microsoft/vscode/blob/main/src/vs/workbench/services/extensions/common/rpcProtocol.ts) — the 3000 ms unresponsiveness threshold, the hang-vs-crash distinction
- [Workspace trust](https://code.visualstudio.com/api/extension-guides/workspace-trust) — `supported`, `restrictedConfigurations`, `isTrusted`, `isWorkspaceTrusted`
- [Webviews](https://code.visualstudio.com/api/extension-guides/webview) — the CSP baseline, the `localResourceRoots` default scope, and the unvalidated message tutorial
- [`webview-sample`](https://github.com/microsoft/vscode-extension-samples/tree/main/webview-sample) — the nonce-CSP helper worth copying
- [esbuild API](https://esbuild.github.io/api/) and [content types](https://esbuild.github.io/content-types/) — `format`, `packages`, what esbuild reads from tsconfig (not `module`), and the top-level-await restriction
- [Bundling an extension](https://code.visualstudio.com/api/working-with-extensions/bundling-extension) — why the `vscode` module must stay external
- [`require()`ing ES modules](https://nodejs.org/api/modules.html) — the v20.19.0 / v22.12.0 timeline and the unconditional `ERR_REQUIRE_ASYNC_MODULE` caveat
- [Electron releases feed](https://releases.electronjs.org/releases.json) — which Node a given host version actually runs
- [Everyday types](https://www.typescriptlang.org/docs/handbook/2/everyday-types.html) — TS2352 and the sanctioned two-step cast; annotations are erased and never affect runtime behaviour
