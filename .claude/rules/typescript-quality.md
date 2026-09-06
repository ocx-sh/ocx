---
paths:
  - "**/*.ts"
  - "**/*.tsx"
  - "**/*.mts"
  - "**/*.cts"
summary: The TypeScript quality index — the gate, the non-negotiables, and where the depth lives
keywords: typescript,quality,standards,review,types,async,errors,exit-codes,security,modules,testing,eslint,biome,tsconfig,node,browser,vscode
license: Apache-2.0
repository: https://github.com/ocx-sh/grimoire-lore
---

# TypeScript Quality

Traps, not maps. Everything here names a mistake that gets made without it;
the architecture of any particular codebase is discoverable by reading the
code, so it is not in this file.

Contents: [The Gate](#the-gate) · [Non-Negotiables](#non-negotiables) ·
[Rules This File Owns](#rules-this-file-owns) ·
[Where the Depth Is](#where-the-depth-is) · [Severity](#severity) ·
[Siblings](#siblings)

**Before trusting any rule below, confirm the lint is type-aware — read
[typescript-quality/gate.md](typescript-quality/gate.md) first.** Roughly
fifty rules, including the whole `no-unsafe-*` family,
`no-floating-promises`, `no-misused-promises` and
`switch-exhaustiveness-check`, need a type checker and are **off by
default**. A config without `parserOptions.projectService` (or, in Biome, a
rule key written outside its group) does not warn that those rules are
inert — it just never fires them, and inert is indistinguishable from clean.
Most of this rule set is unenforceable until that is wired.

## The Gate

Run it after every change, narrowest scope first — each stage costs more
than the last, so the common case never reaches the slow ones.

```bash
npx tsc --noEmit                    # or vue-tsc --noEmit
npx eslint --max-warnings 0 .       # whole repo, never a src-only glob
npx vitest run                      # or the runner the nearest package.json declares
npx attw --pack . && npx publint    # packages that publish
```

A Biome repo replaces the second line with `npx biome check .` and keeps
every other line unchanged. Typed lint does **not** replace the typecheck:
measured 2026-08-29 on two trees (8.3k and 4.5k source LOC), it cost
2.0–2.2× a bare `tsc --noEmit` on the same tree, because it builds its own
program rather than reusing one — and it reports only what a rule probes
for, not TypeScript's diagnostic set. Budget double the typecheck; there is
no version of "too slow to run".

Chain these into one named target (`npm run check`, `task check`) and have
CI invoke that target, never a hand-copied step list. Run them through the
project's pin, never through `$PATH`: a globally installed tool shadowing
the pinned one is the most common way two people get different answers from
the same command.

A task is done when a command, its exit code, and the tree it ran against
are all named. Narration is not evidence.

## Non-Negotiables

Every line below blocks a merge. IDs resolve to the depth files in
[Where the Depth Is](#where-the-depth-is), where each rule carries its
rationale and verification.

| # | Rule | ID |
|---|---|---|
| 1 | Type-aware linting is wired and proven to fire before any rule below is claimed to hold. Every out-of-tsconfig file is listed in `allowDefaultProject`, never fixed with `ignores`. | TS-TYP-01, TS-GATE-01 |
| 2 | No floating promise, and no `async` function in a `void`-returning callback position — `tsc` accepts both at every strictness level. | TS-ASYNC-01, TS-ASYNC-03 |
| 3 | Every outbound call — `fetch`, RPC transport, child process — carries an explicit deadline, and never one faked with `Promise.race`. | TS-ASYNC-04, TS-ASYNC-07 |
| 4 | A value the process did not construct is `unknown` until a runtime check narrows it. Never `JSON.parse(x) as T`, never an annotated `Response.json()` binding. | TS-TYP-04, TS-ERR-10, TS-ERR-11, TS-SEC-01 |
| 5 | Never `@ts-ignore` or `@ts-nocheck`. `@ts-expect-error` in test trees only, always with a description on the same line. | TS-TYP-03 |
| 6 | No `enum` and no `const enum`, and every mapping off a closed union ends in a `never`-typed fallthrough. | TS-TYP-09, TS-TYP-10 |
| 7 | A rethrow passes `{ cause: err }`; a terminal catch logs the error value or its `.stack`, never `.message` and never `JSON.stringify(err)`. | TS-ERR-01, TS-ERR-04, TS-ERR-05 |
| 8 | Never call `process.exit()`. Every status comes from one named code object and is assigned to `process.exitCode` at exactly one entrypoint. | TS-CLI-01, TS-CLI-02, TS-CLI-03 |
| 9 | stdout carries the result; every message *about* the run goes to stderr. Under a machine-output flag, stdout is exactly one parseable document. | TS-CLI-06, TS-CLI-07 |
| 10 | Children spawn as `execFile(file, argsArray)` with an explicit `timeout` and `maxBuffer` — never a built command string, never `shell: true` on external data. | TS-SEC-03, TS-RES-01 |
| 11 | A path from outside is resolved against its root and containment-checked before use; a record keyed from outside is a `Map` or a null-prototype object. | TS-SEC-05, TS-SEC-02 |
| 12 | No credential reaches a log line, an error message, `argv`, or a child process's inherited environment. | TS-SEC-07 |
| 13 | `moduleResolution` is `node16`, `nodenext` or `bundler` — never `node`, `node10`, `node18` or `node20` — and under `node16`/`nodenext` every relative import carries its `.js` extension. | TS-MOD-02, TS-MOD-04 |
| 14 | A lint config holding any `warn` runs under `--max-warnings 0`, and no file that ships or executes appears in `ignores`. | TS-GATE-07, TS-GATE-06 |
| 15 | A rule file, README or docblock asserts a compiler-enforced guarantee only where the resolved config confirms the flag. | TS-MOD-23 |
| 16 | Never reach green by weakening the check, and never ship a verification nobody has watched go red. | TS-CORE-01, TS-CORE-02 |

## Rules This File Owns

Three cross-cutting rules that belong to no single depth file. Everything
else is defined in a depth file and only cited here.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-CORE-01 | Never reach green by weakening the check: no new `eslint-disable` or `biome-ignore`, no widened `ignores`, no lowered coverage threshold, no `.skip`, no edit to the gate's own config as part of a functional change. | The gate's whole value is that it can go red. A change that edits both the code and the check that judges it reports nothing and looks identical to a passing change. | `git diff --stat -- 'eslint.config.*' 'biome.json' 'tsconfig*.json' 'package.json'` — any hit in a change that is not itself a gate change is the violation. Then `rg -n -e 'eslint-disable' -e 'biome-ignore' -e '\.skip\(' -e '\.only\(' <changed-file>` — a union over the four spellings — for each source file in the diff; a line added by this change is the finding. Empty output is the pass. | MUST |
| TS-CORE-02 | A verification enters a rule table, a CI job, or a review only after it has been watched go red against a deliberately planted violation. | A check that cannot fail launders an unchecked change as a checked one, and reads exactly like a passing one forever. TypeScript's failure modes here are specific: a rule inert for want of type information, a Biome key outside its group, a coverage block whose keys the runner does not read. | Copy the subject, break the thing the rule forbids, run the verification. A pass on the broken copy is the violation. | MUST |
| TS-CORE-03 | State whether empty output means a pass or means the finding, in every verification that is not self-evidently one or the other. | Half the checks in this rule set are inverted — a missing `projectService`, an absent `typecheck` script, four rules absent after a Biome migration are each *the finding*, not the pass. | Read each verification cell: one whose empty output is ambiguous is the violation. | SHOULD |

## Where the Depth Is

Read the file for the work you are about to do, not for the topic it is
filed under. One level deep; these files do not point at each other.

| Doing… | Read |
|---|---|
| Turning on a lint rule, wiring CI, or making a check able to go red at all | [typescript-quality/gate.md](typescript-quality/gate.md) |
| Writing `as`, `any`, a type predicate, an `enum`, a union arm, or a `declare` block | [typescript-quality/types.md](typescript-quality/types.md) |
| Adding an outbound call, a timer, an `await`, an async callback, or a fan-out over an array | [typescript-quality/async.md](typescript-quality/async.md) |
| Throwing, catching, rethrowing, or turning an untrusted payload into a typed value | [typescript-quality/errors.md](typescript-quality/errors.md) |
| Ending a process, choosing an exit status, parsing argv, or writing to stdout | [typescript-quality/cli-contract.md](typescript-quality/cli-contract.md) |
| Spawning a child process, setting an interval, opening a handle, or writing `using` | [typescript-quality/resources.md](typescript-quality/resources.md) |
| Editing an import specifier, a `module`/`moduleResolution` field, or a package's `"type"` | [typescript-quality/modules.md](typescript-quality/modules.md) |
| Passing outside data to a shell, a path, an object key, a regex, or an HTML sink; touching a credential or an install script | [typescript-quality/security.md](typescript-quality/security.md) |
| Writing a test, a coverage threshold, a type-level assertion, or a fake | [typescript-quality/testing.md](typescript-quality/testing.md) |
| Adding a log line, a catch that does not rethrow, a `.sort()`, or a formatted date | [typescript-quality/observability.md](typescript-quality/observability.md) |
| Rendering HTML, binding a URL, editing a CSP or a generated client, growing the bundle | [typescript-quality/browser.md](typescript-quality/browser.md) |
| Writing code that runs inside an editor or Electron extension host — `activate()`, a webview, the host bundle, a host-API double | [typescript-quality/extension-host.md](typescript-quality/extension-host.md) |
| Editing `package.json`, any `tsconfig*.json`, or a lint config file itself | `typescript-packaging` (sibling set — see below) |

## Severity

MUST = Block: fix before it lands. SHOULD = Warn: fix, or state why not in
the commit body. CONSIDER = Suggest: never blocks, never re-raised after a
decline.

Rules marked **pinned** in a depth file — the exit-code table, the `any`
exception list, the bundle budget — encode an agreed decision rather than a
derivable fact. They are defaults an adopter may override, once, in their
own config or code module, never per call site. Overriding one is a
decision; ignoring one is a violation.

Keep the Block list short enough that a blocked change is unusual. A rule
set where everything blocks teaches the reader to negotiate with all of it.

## Siblings

- **`typescript-packaging`** — what a repository claims about itself in
  files no compiler checks: the tsconfig strictness floor per shape,
  `extends` topology, `engines`, `exports`, `bin`, dependency placement,
  and publish verification. Loads on `**/package.json`,
  `**/tsconfig*.json`, `**/eslint.config.*` and `**/biome.json*` — globs
  this set deliberately does not cover, so the two never load together.
