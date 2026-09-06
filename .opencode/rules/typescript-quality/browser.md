---
title: Browser SPAs
summary: The TS-WEB family — what a React/Vue/Vite app must ship at its error surface, its DOM sinks, its CSP, its bundle budget, and its generated RPC boundary
---

# Browser SPAs

Owns `TS-WEB`: the failures that only exist once code runs in a browser tab —
no error boundary, an unescaped sink, an inert CSP, an unbudgeted bundle, a
hand-edited generated client. It does not own general TypeScript idiom,
validation of untrusted payloads (`TS-ERR`), request cancellation and RPC
timeouts (`TS-ASYNC`), or webviews inside an extension host (`TS-HOST`).

Contents: [Scope](#scope) · [The Error Surface](#the-error-surface) ·
[DOM Sinks](#dom-sinks) · [CSP in the Built Page](#csp-in-the-built-page) ·
[Bundle Cost](#bundle-cost) ·
[The Generated RPC Boundary](#the-generated-rpc-boundary) ·
[Version-Bound Framework Drift](#version-bound-framework-drift)

## Scope

- **"SPA" here means a client-rendered bundle served as static files**, built
  by Vite, with no server the team controls in front of it. That single fact
  drives the CSP rules: header delivery is not available, so the policy ships
  in a `<meta>` tag and loses directives.
- **Two rules pin a project decision** rather than deriving one: the byte
  budget (TS-WEB-09) and the a11y gate level (TS-WEB-06). Both are stated as
  defaults an adopter overrides with their own number, in their own config.
- **A validated payload is `TS-ERR`'s subject, not this file's.** The rules
  below assume a value has already crossed that boundary; where a browser
  shape makes the crossing different — protobuf — TS-WEB-11 says how.
- Run every check from the SPA package root, so `src/` resolves to that app.

## The Error Surface

Uncaught render errors, rejected promises with no handler, and errors thrown
inside event handlers reach three different places in a browser. A framework
error boundary catches only the first.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| **TS-WEB-01** | Ship one top-level error boundary — React: a class with `static getDerivedStateFromError` plus `componentDidCatch`, or `react-error-boundary`; Vue: `app.config.errorHandler` paired with a component or app state that renders a fallback — and log inside the handler. | Without a boundary an uncaught render error is a permanent white screen; without a log inside it there is no trace at all. React boundaries are class-only — there is no hook equivalent, and a model will confidently write one. A Vue `errorHandler` alone silences the crash without redrawing anything. | `rg -n -e getDerivedStateFromError -e componentDidCatch -e 'config.errorHandler' -e onErrorCaptured src/` — zero hits in an SPA is the violation. Each hit must contain a log or reporter call in its body. | MUST |
| **TS-WEB-02** | Register the browser's global handlers, `window.addEventListener('unhandledrejection', …)` and `'error'` — never `process.on('unhandledRejection', …)` — in bundled browser code. | The Node `process` surface does not exist in a tab; under Vite it is either undefined at module load or shimmed to an object with no `.on`, so the handler an agent writes from Node habit never fires and nothing says so. The WHATWG event name is all-lowercase; the Node one is camelCase, and neither runtime accepts the other's spelling. | `rg -n "process\.on\(" src/` — any hit in browser-bundled code is the violation. `rg -n "addEventListener\(['\"]unhandledrejection" src/` — zero hits is the violation. | MUST |

```ts
// wrong — no `process` in the tab; the handler is never installed
process.on("unhandledRejection", (e) => report(e));

// right
window.addEventListener("unhandledrejection", (e) => report(e.reason));
```

## DOM Sinks

One lint invocation covers the enable; the sanitizer placement is a read-back
the lint cannot do. Enable the sink rule explicitly — React's
`react/no-danger` is **opt-in** and absent from every recommended React
config, while Vue's `vue/no-v-html` is **on by default** in
`plugin:vue/recommended`, so the two stacks fail in opposite directions.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| **TS-WEB-03** | Turn on the framework's raw-HTML lint and never blanket-disable it: `react/no-danger` as `error` in the flat config; `vue/no-v-html` left at its recommended default. A single site that genuinely needs raw HTML carries a line-scoped disable naming why. | React ships no default-on rule for `dangerouslySetInnerHTML`, so an agent adding one gets a clean lint run and no signal; on the Vue side the agent's fix for a red build is to switch the rule off file-wide. | `rg -n 'react/no-danger' eslint.config.*` — no hit in a React app is the violation. `rg -n -e "'vue/no-v-html': *0" -e "'vue/no-v-html': *'off'" -e 'eslint-disable.*no-v-html' src/ eslint.config.*` — a file-scoped or config-level disable is the violation; a line-scoped one with a reason is not. | SHOULD |
| **TS-WEB-04** | Sanitize as the last operation before the value reaches `dangerouslySetInnerHTML` or `v-html` — never sanitize and then interpolate, template, or rewrite the result. | Post-sanitize modification can reintroduce exactly the markup the sanitizer removed, which is DOMPurify's own documented warning; the code still reads as sanitized. | `rg -n -e dangerouslySetInnerHTML -e 'v-html' src/` — for each hit, trace the bound expression back to its source and confirm the sanitizer call is the final transform. Empty output is a pass. | MUST |
| **TS-WEB-05** | Never bind a URL attribute (`href`, `:href`, `src`) to an unvalidated string, and never bind `:style`/`style` to a whole user-supplied object — allowlist the scheme, and set individual style properties. | A `javascript:` URL is script execution through an attribute no HTML sanitizer looks at; a wholesale style binding is a clickjacking surface. Both are escaped correctly by the framework and still exploitable. | `rg -n -e ':href=' -e ':src=' -e ':style=' -e 'href={' -e 'style={' src/` — every hit whose value did not originate in this codebase needs a scheme check or a per-property assignment. | SHOULD |
| **TS-WEB-06** | Run a static accessibility lint in the same invocation as the rest of the lint, at `error`: `eslint-plugin-jsx-a11y` for JSX, `eslint-plugin-vuejs-accessibility` for SFCs, or Biome's `a11y` group where Biome is the linter. *(Default — an adopter may set `warn`; do not run it as a separate optional job.)* | A11y defects an AST can see (missing label, `onClick` on a `div`, invalid ARIA) are the ones agents introduce most and the only ones that cost nothing to catch; a separate optional job is a job nobody runs. | `rg -n -e jsx-a11y -e vuejs-accessibility eslint.config.*`, or `rg -n '"a11y"' biome.json` — no hit is the violation. Then confirm the a11y plugin is inside the config the CI lint command loads, not a second unused config file. | SHOULD |

## CSP in the Built Page

A static SPA delivers its policy in `<meta http-equiv="Content-Security-Policy">`,
which is a strictly weaker channel than the HTTP header, and MDN is explicit
about which directives it drops.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| **TS-WEB-07** | A production policy contains neither `'unsafe-inline'` nor `'unsafe-eval'` in `script-src` or `default-src`. Prefer a nonce- or hash-based `script-src` with `'strict-dynamic'` over a host allowlist. | Either keyword defeats most of what CSP exists to stop, and both are what an agent adds to make a build-time warning go away. | `rg -n -e unsafe-inline -e unsafe-eval index.html public/ vite.config.*` — any hit in the shipped policy is the violation. Empty output is a pass. | MUST |
| **TS-WEB-08** | Do not write `frame-ancestors`, `report-uri`, or `report-to` into a `<meta>` CSP, and do not claim clickjacking or reporting coverage from one. Those directives are ignored in meta delivery; carry them on the HTTP header or state in the same file that the app has none. | The directive parses without error and reports nothing, so the policy reads as complete while the protection does not exist. `X-Frame-Options` has the same meta-tag gap. | `rg -n -e frame-ancestors -e report-uri -e report-to -e X-Frame-Options index.html public/` — any hit inside a `<meta>` element is the violation. | SHOULD |

## Bundle Cost

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| **TS-WEB-09** | Gate the entry chunk on a byte budget that exits non-zero — `size-limit`, via a `"size-limit"` key in `package.json` or a `.size-limit.*` file, run in CI. Start the entry-chunk limit at Vite's own 500 kB warning threshold. *(Default — the adopter picks the number and records how it was measured.)* Never `bundlesize`. | `build.chunkSizeWarningLimit` defaults to 500 kB, compares against the **uncompressed** chunk, and only prints — a bundle can double in a single dependency bump with a green build. `bundlesize` last published 2024-03-15 and is stale training data an agent reaches for by name. | `rg -n 'size-limit' package.json` — no hit is the violation; then confirm the CI workflow runs it as its own step. `rg -n 'bundlesize' package.json` must be empty. | SHOULD |

## The Generated RPC Boundary

Generated protobuf/RPC TypeScript is a build output that happens to be
committed. Two independent failures live here: editing it, and mistaking its
type safety for validation.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| **TS-WEB-10** | Never hand-edit a generated client or message file. Change the schema and regenerate; if the generated shape is wrong for a caller, wrap it in hand-written code beside the generated directory. | A hand edit survives exactly until the next codegen run, and the type error it silenced comes back in CI on someone else's branch with no record of the fix. | `rg --files-without-match '@generated' <generated-dir>` — a file there without the banner is either hand-written in the wrong place or an edited generated file. Then re-run codegen and `git diff --exit-code -- <generated-dir>`; a diff is the violation. | MUST |
| **TS-WEB-11** | Treat a decoded protobuf message as type-safe, not validated: apply domain checks (non-empty, in-range, allowed enum) after decoding, and never distinguish "absent" from "zero/empty" on a proto3 field that is not declared `optional`. | proto3 implicit presence decodes a missing field and an explicitly-zero field identically, so a falsiness test on a scalar silently conflates them — and generated code guarantees only the declared scalar type, never a value the app can use. | `rg -n 'syntax = "proto3"' <proto-dir>` — if it matches, every field the client tests for presence must carry `optional` in its declaration (`rg -n '^\s*optional ' <proto-dir>`), or the test must not distinguish absent from zero. | MUST |

Per-call deadlines on an RPC transport are `TS-ASYNC-04`, not this file.

## Version-Bound Framework Drift

Each row below binds a specific release and is stale advice before it. Check
the installed version first; the rule does not apply below the floor named.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| **TS-WEB-12** | On React ≥19 (stable 2024-12-05) do not write `forwardRef` — destructure `ref` from props like any other prop — and give every ref callback a block body returning either nothing or a cleanup function. | `forwardRef` is deprecated as of 19 and is the single most reflexive stale pattern in React training data; the ref-callback tightening turns an implicit return into a type error, which agents then "fix" by widening the callback's type. | `rg -n 'forwardRef\(' src/` against a `react` dependency at `^19` or later — any hit is the violation. The ref-callback half is caught by `tsc --noEmit`. | SHOULD |
| **TS-WEB-13** | Where `babel-plugin-react-compiler` is installed (stable 1.0.0, 2025-10-07), do not add new `useMemo`/`useCallback`/`React.memo`. If one is genuinely needed, comment the reason the compiler cannot see the call site. | The compiler already memoizes; a manual layer on top is review noise that reads as required, and "always memoize" is the dominant pre-2025 advice a model carries in. | `rg -n 'react-compiler' package.json` — if it matches, `rg -n -e 'useMemo\(' -e 'useCallback\(' -e 'React\.memo\(' src/` and require a justification comment on each hit added by the change under review. | CONSIDER |
| **TS-WEB-14** | On Vite ≥8 (2026-03-12, Rolldown) write `build.rolldownOptions`; `build.rollupOptions` still works through a compat layer scheduled for removal. | The rename is newer than most training data, so a config edit reintroduces the deprecated key on every touch. | `rg -n 'rollupOptions' vite.config.*` against a `vite` dependency at `^8` or later — any hit is the violation. | SHOULD |
