---
title: TypeScript Security
summary: The TS-SEC family — untrusted input, injection, path containment, prototype pollution, ReDoS, secrets, and install-time supply chain, in TypeScript and Node
---

# Security

This file owns what happens when data the process did not author reaches a
sink: a validator, a shell, a path, an object key, a regex engine, a DOM node,
a comparison against a secret, or an install script. It does not own transport
deadlines, retry policy, CI workflow permissions, or lockfile policy.

Contents: [The Trust Boundary](#the-trust-boundary) ·
[Executing What You Did Not Write](#executing-what-you-did-not-write) ·
[Paths From Outside](#paths-from-outside) ·
[Secrets, Randomness, Transport](#secrets-randomness-transport) ·
[Browser Sinks](#browser-sinks) · [The Security Lint Block](#the-security-lint-block) ·
[Install-Time Supply Chain](#install-time-supply-chain) ·
[Untrusted Workspaces](#untrusted-workspaces) ·
[What Agents Get Wrong](#what-agents-get-wrong-here) · [Sources](#sources)

Two layers, and the difference matters when you adopt this:

- **The mechanism** — validate at the boundary rather than assert, hand a
  child an argv array rather than a string, resolve then contain, key wire
  data into a container with no prototype, sanitize as the last step before
  insertion — is general Node and browser practice.
- **The pinned defaults**, which an adopter may override once, in writing:
  the ReDoS lint selection is *both* regex plugins (neither subsumes the
  other); a release cooldown is configured rather than left at the tool
  default; `Object.hasOwn` replaces `key in obj` on merged external data.

TypeScript is not a control here. `as` performs no runtime check, the
compiler erases at emit, and every rule below is about a value whose real
shape is decided by someone else at runtime.

## The Trust Boundary

Everything downstream assumes this held. `unknown` is the only honest type
for a value that arrived from outside the process.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-SEC-01 | A value from `JSON.parse`, a response body, `process.env`, `argv`, an RPC/IPC message, or a file the process did not write is typed `unknown` until a runtime validator narrows it. Never `as T`, never a bare annotation, never `as unknown as T`. Exception: a value the same process serialized moments earlier, in the same file, to a type it owns. | An `as` on external data is an assertion the wire matched what the author imagined; the checker then believes it, so the missing field surfaces hundreds of lines away as `undefined is not a function` instead of at the boundary. | `rg -n -g '*.ts' -g '*.tsx' -e 'JSON\.parse\([^;\n]*\bas\s+[A-Z]' -e '\.json\(\)[^;\n]*\bas\s+[A-Z]' -e 'process\.env[^;\n]*\bas\s+[A-Z]' -e '\bas\s+unknown\s+as\b' <src>` — each `-e` matches independently; every line printed is the violation, and empty output is the pass. It sees the single-line form only: a `parse` on one line and the cast three lines later needs the read. | MUST |
| TS-SEC-02 | A record whose keys come from outside is a `Map`, or an object built by `Object.create(null)` / `{ __proto__: null }` — never a plain `{}`. Where external data is merged or iterated into an existing object, gate every key with `Object.hasOwn(obj, key)`, not `key in obj` and not a bare `obj[key]`. Reach for `Object.freeze` on a prototype only after the container swap is impossible, and never on a built-in prototype a dependency may patch. | A `"__proto__"` or `"constructor"` key written into a plain object literal replaces the prototype instead of adding an entry, and `key in obj` then answers `true` for keys the object never had. `--disable-proto` does not close this: its own documentation states it leaves `constructor.prototype` pollution untouched. | Loud and clean: `rg -n --pcre2 -g '*.ts' -e 'function deep[Mm]erge' -e 'function merge\(' -e 'Object\.assign\(\s*\{\s*\}' <src>` — a hand-rolled recursive merge is the classic sink; every hit needs a `FORBIDDEN_KEYS` guard or a rewrite. Then the candidate set: `rg -l -g '*.ts' 'JSON\.parse\(' <src>` piped into `xargs -r rg --files-without-match -e 'Object\.create\(null\)' -e 'new Map\(' -e '__proto__'` — these files parse wire data and hold no prototype-safe container; read each for a record keyed by that data. Candidates, not violations. | MUST |

```ts
// Wrong — the cast is the only thing standing between the wire and the type.
const cfg = JSON.parse(await readFile(p, "utf8")) as Config;

// Right — unknown until a validator says otherwise; the throw names the boundary.
const raw: unknown = JSON.parse(await readFile(p, "utf8"));
const cfg = ConfigSchema.parse(raw); // any Standard Schema validator
```

A shared helper that accepts a schema should type its parameter as
`StandardSchemaV1` rather than one library's schema type: zod, valibot and
arktype all implement that ~60-line interface, so the helper stays usable
when the validator choice changes.

## Executing What You Did Not Write

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-SEC-03 | Spawn children with `execFile(file, argsArray)` or `spawn(file, argsArray)`. Never `exec`/`execSync` on a built string, and never `shell: true` on any spawner. No exception for "the argument is a constant today". | With no shell, a metacharacter in an argument is an argument; with a shell it is syntax. Node's own docs carry the identical *"never pass unsanitized user input to this function"* warning on `exec` and on `shell: true`, and the argv array removes the whole class rather than escaping around it. | Two stages, because a bare `\bexec\(` grep drowns in `RegExp.prototype.exec`: `rg -l -g '*.ts' -e 'node:child_process' -e '"child_process"' <src>` piped into `xargs -r rg -n -e '\bexec\(' -e '\bexecSync\(' -e 'shell:\s*true'`. Read each hit — `/re/.exec(s)` inside such a file still matches and is not a finding. Then `rg -n -g '*.ts' 'shell:\s*true' <src>` on its own, for spawners that never import `child_process`. | MUST |
| TS-SEC-04 | Never construct executable text from data: no `eval`, no `new Function`, no framework template compiled from a runtime string, and no `new RegExp(x)` where `x` is not a literal the file itself wrote. A pattern that must vary comes from a fixed lookup keyed by a validated enum. | `new RegExp(userInput)` hands an attacker the pattern, which is the precondition for ReDoS by construction — no amount of input-length capping helps once the attacker writes the quantifiers. `eval` and `new Function` are the same hole with a wider mouth. | `rg -n -g '*.ts' -g '*.tsx' -e '\beval\(' -e 'new Function\(' -e 'new RegExp\(' <src>` — every `eval`/`new Function` hit is the violation; a `new RegExp` hit whose first argument is not a string literal is the violation. Lint coverage is `no-eval`, `no-implied-eval`, `no-new-func` and `security/detect-non-literal-regexp`, all in [the block below](#the-security-lint-block). | MUST |

## Paths From Outside

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-SEC-05 | A path segment from a request, a CLI argument, a wire document, or an archive entry name is resolved against the root and then *contained*: compute `path.relative(root, resolved)` and reject it if it starts with `..` or is absolute. `path.join` is not containment, `path.isAbsolute` is not containment — Node's own docs state it is "not safe for mitigating path traversals" — and a bare `resolved.startsWith(root)` is not containment either, because `/data-evil` is a string prefix of `/data`. | Without the check, one `../` segment in an archive entry or a route parameter writes outside the tree the code believes it owns. Node's API reference documents no safe pattern at all: `path.resolve` gives no traversal guidance, so this composition is outside knowledge the reviewer must supply. Extracting an archive with a system `tar` and no `--strip-components` inherits the same gap, checksum or not. | `rg -n --pcre2 -g '*.ts' 'startsWith\((?![\x27"\x60])(?![^)]*\bsep\b)' <src>` — a prefix comparison against a variable, with no `path.sep` inside the call; every line printed is a naive containment check and empty output is the pass. The literal-argument and `root + path.sep` forms are deliberately excluded. Then read every `path.join`/`path.resolve` in the file that handles the untrusted segment. | MUST |

```ts
// Wrong — join does not remove `..`, and the prefix test admits `/data-evil`.
const p = path.join(root, entryName);
if (!p.startsWith(root)) throw new Error("escape");

// Right — one resolve, one relative, no separator arithmetic to get wrong.
const rel = path.relative(root, path.resolve(root, entryName));
if (rel.startsWith("..") || path.isAbsolute(rel)) throw new Error(`escapes root: ${JSON.stringify(entryName)}`);
```

## Secrets, Randomness, Transport

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-SEC-06 | Every security-relevant value — session id, token, nonce, salt, PKCE verifier, reset code, filename an attacker must not guess — comes from `crypto.randomUUID()` or `crypto.getRandomValues()`. `Math.random()` is permitted only for jitter, sampling, animation, and test fixtures; name which one at the call site. | `Math.random()` is a non-cryptographic PRNG whose stream is recoverable from a handful of outputs, and `Math.random().toString(36).slice(2)` is the id generator the training corpus reaches for by reflex. | `rg -n -g '*.ts' -g '*.tsx' 'Math\.random\(' <src>` — read each hit and classify it. A hit feeding an identifier, token, key, nonce or path is the violation; a hit feeding backoff jitter is not. | MUST |
| TS-SEC-07 | A credential value reaches no observable surface: not a log line, not an error message, not `argv`, not a child process's inherited environment. In a CI action, call the runner's secret-registration API (`core.setSecret(value)`) on every credential input immediately after reading it, before its first use. | The platform-supplied token is masked by the runner automatically; a caller-supplied token passed to the same overridable input is not, so the masking that appears to exist in testing vanishes for exactly the user who supplied their own credential. | `rg -n -g '*.ts' 'getInput\(' <src>` — every input whose name or use is credential-shaped must have a `setSecret` call on it before first use; `rg -n -g '*.ts' 'setSecret' <src>` returning fewer hits than that is the violation. Then the [logging grep below](#the-security-lint-block). | MUST |
| TS-SEC-08 | Compare a secret, token, signature or HMAC against attacker-supplied input with `crypto.timingSafeEqual` on equal-length buffers, never `===`/`==`. Because `timingSafeEqual` throws on a length mismatch, compare lengths first and route both outcomes through the same failure path. Exempt: comparing two values the process itself produced, and comparing a non-secret. | `===` on strings short-circuits at the first differing byte, so response time reports how much of the secret the caller guessed — enough to recover it byte by byte over a few thousand requests. | The two-direction grep in [the block below](#the-security-lint-block); every line printed compares a credential-named variable with `==`/`===` and is the violation. | SHOULD |
| TS-SEC-09 | Never disable certificate verification or the strict HTTP parser to make an error go away: no `rejectUnauthorized: false`, no `NODE_TLS_REJECT_UNAUTHORIZED=0`, no `insecureHTTPParser: true`. A self-signed certificate in development is added to the trust store, not switched off. | The first two turn every request into an opt-in MITM; the third accepts the malformed framing that request smuggling (CWE-444) depends on. All three are set while debugging something unrelated and never removed. | `rg -n -e 'rejectUnauthorized:\s*false' -e 'NODE_TLS_REJECT_UNAUTHORIZED' -e 'insecureHTTPParser:\s*true' <repo>` — any line printed is the violation, in source, scripts, Dockerfiles or CI alike; empty output is the pass. | MUST |

## Browser Sinks

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-SEC-10 | An HTML sink — `dangerouslySetInnerHTML`, `v-html`, `.innerHTML =`, `insertAdjacentHTML`, `document.write` — receives only a string returned by a sanitizer, called as the *last* step before insertion. Nothing edits, concatenates or re-templates the string after sanitization. Where the renderer can escape instead (markdown with raw HTML disabled), do that too and keep the sanitizer as the second layer. | Sanitizing early and mutating later voids the sanitization — DOMPurify's own documentation states this — and the mutation is usually added months later by someone who sees a sanitized variable and assumes it stays sanitized. | `rg -n -g '*.ts' -g '*.tsx' -g '*.vue' -e 'dangerouslySetInnerHTML' -e '\.innerHTML\s*=' -e 'insertAdjacentHTML\(' -e 'document\.write\(' -e 'v-html' <src>` — every hit needs the sanitizer traced from the sink back to the source. Lint is asymmetric and this is the part that gets assumed wrong: `vue/no-v-html` is **on by default** in `eslint-plugin-vue`'s recommended config; `react/no-danger` is **opt-in** and in no recommended config, so a React tree has zero lint coverage until it is added. | MUST |
| TS-SEC-11 | A URL bound to `href`/`src`/`window.open` from external data is checked against a scheme allowlist (`http:`, `https:`, and whatever else the feature genuinely needs) before binding. A `style` binding takes individually validated properties, never a whole externally-sourced object. | HTML escaping does not touch `javascript:` — the string is a valid, correctly-escaped attribute value that executes on click, so a framework's automatic attribute escaping gives no protection here at all. A whole-object style binding hands over `position`, `opacity` and `z-index`, which is UI redress. | `rg -n -g '*.ts' -g '*.tsx' -g '*.vue' -e ':href=' -e 'href=\{' -e '\.href\s*=' -e 'window\.open\(' -e ':style=' <src>` — every hit bound to data the process did not produce needs the scheme check or the per-property split; a hit bound to a route constant is not a finding. | SHOULD |

## The Security Lint Block

One config, stated once. Every rule above that a linter can reach is reached
from here; the rows above give the grep for what it cannot.

```js
// eslint.config.js
import security from "eslint-plugin-security";
import regexp from "eslint-plugin-regexp";

export default [
  security.configs.recommended,          // detect-non-literal-regexp, detect-unsafe-regex, detect-child-process
  regexp.configs["flat/recommended"],    // no-super-linear-backtracking, no-super-linear-move
  { rules: {
      "no-eval": "error",
      "no-implied-eval": "error",
      "no-new-func": "error",
      "react/no-danger": "error",        // React trees only — opt-in, in no recommended config
      // vue/no-v-html is already error in eslint-plugin-vue's recommended config
  } },
];
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-SEC-12 | Select **both** regex plugins, not one. `eslint-plugin-security` judges where a pattern came from (`detect-non-literal-regexp` on `new RegExp(variable)`, `detect-unsafe-regex` against a known-bad signature set); `eslint-plugin-regexp` judges whether the pattern itself backtracks super-linearly, regardless of source. Neither subsumes the other, and the case they disagree on — a catastrophic literal regex the team wrote by hand — is the common one. Where the repo's linter has no equivalent rule, the greps in the rows above are the whole check and must be run by hand. | Selecting only the source-aware plugin certifies a hand-written `/(a+)+b/` as clean; selecting only the structural one lets `new RegExp(req.query.q)` through. | `rg -n -e 'eslint-plugin-security' -e 'eslint-plugin-regexp' <repo>/package.json` — fewer than two hits is the violation. Then prove the rules actually reach source, which a manifest hit does not: `npx eslint --print-config <a real source file> > /tmp/eslint-effective.json` followed by `rg -n -e 'no-super-linear-backtracking' -e 'detect-non-literal-regexp' /tmp/eslint-effective.json`. Empty output there is the violation — the plugin is installed and reaching nothing. | MUST |

The two checks the table rows point at. Each `-e` is an independent match:

```sh
# TS-SEC-08 — a credential-named variable compared with == or ===, either direction
rg -ni -g '*.ts' -g '*.tsx' \
  -e '\w*(token|secret|signature|hmac|apikey|password)\w*\s*[=!]=' \
  -e '[=!]=\s*\w*(token|secret|signature|hmac|apikey|password)\w*' <src>

# TS-SEC-07 — a credential-named value reaching a log or error surface
rg -ni -g '*.ts' -g '*.tsx' \
  -e '(console\.\w+|appendLine|new Error)\([^)]*(token|secret|password|apikey)' <src>
```

## Install-Time Supply Chain

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-SEC-13 | Install-time script execution is off by default (`ignore-scripts=true` in `.npmrc`, the equivalent key in `bunfig.toml`, an explicit built-dependency allowlist under pnpm), with any package that genuinely needs a build script named in an allowlist rather than the flag being dropped. State the documented gap wherever the setting is claimed: it suppresses `preinstall`/`install`/`postinstall`, but `npm start`, `npm test` and `npm run <name>` still execute the named script — only their own pre/post hooks are skipped. | Arbitrary code execution at `install` time is the largest npm supply-chain vector, and it runs before any test, lint or review sees the package. A reviewer who greps for the flag and stops has verified something narrower than they believe. | `rg -n -e 'ignore-scripts' -e 'onlyBuiltDependencies' -e 'trustedDependencies' <repo>` — no hit is the violation; a hit set to `false` is the violation. A missing config file makes `rg` fail loudly rather than pass quietly, which is the point of the repo-root path operand. | MUST |
| TS-SEC-14 | A release cooldown is configured, so a version published minutes ago cannot be installed before anyone has looked at it. Every major package manager now has one as an install-time control, not just a PR gate: npm 11.10.0 (Feb 2026) `min-release-age`, pnpm 10.16 (Sep 2025) `minimumReleaseAge` (default-on from pnpm 11), with Yarn and Bun shipping their own flags in late 2025. Renovate's `minimumReleaseAge` gates the PR, which is a different layer — set both. **Pinned default the adopter may override**: the window is a project decision; the setting existing is not. | Malicious package versions rely on being consumed in the hours before takedown. The naming differs per tool and nothing propagates between them, so a repo that set it in one file is unprotected in the others. | `rg -n -e 'min-release-age' -e 'minimumReleaseAge' <repo>` — empty output means the control is configured nowhere and is the violation. Verify the setting sits in the file the tool actually reads: npm and pnpm read `.npmrc`, Bun reads `bunfig.toml`, Renovate reads its own config. | SHOULD |

## Untrusted Workspaces

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-SEC-15 | An editor extension declaring `capabilities.untrustedWorkspaces.supported: true` must genuinely execute nothing the workspace supplied before any trust check: no `require` of a workspace-resolved module, no child process spawned from a workspace-provided path or command, no acting on a workspace setting. Where it does any of these, the declaration is `false` or `"limited"` — and `"limited"` gates each such feature behind the runtime trusted flag and lists the risky settings in `restrictedConfigurations`. | The manifest is the only thing the editor consults before activating the extension in a restricted workspace; a `true` that the activation path contradicts hands code execution to whoever opened the folder. Nothing fails, warns or logs — the mismatch is silent by construction. | A cross-check, not a grep: read the manifest capability, then read the activation function and everything it reaches before the first trust check. `rg -n -g '*.ts' -e 'isTrusted' -e 'onDidGrantWorkspaceTrust' <src>` returning nothing while the manifest declares `"limited"` is a decisive violation; `true` plus a `spawn`/`require` on a workspace path in the activation path is the other. | SHOULD |

## What Agents Get Wrong Here

1. Writes `JSON.parse(x) as Config` and considers the input validated. The
   cast is erased at emit and checks nothing at runtime (TS-SEC-01).
2. Builds a lookup table as `{}` from wire-supplied keys, because an object
   literal is what a dictionary looks like in every tutorial (TS-SEC-02).
3. Reaches for `exec` with a template literal, because that is the shorter
   `child_process` example in the docs and it reads as string building
   rather than command construction (TS-SEC-03).
4. Guards traversal with `path.isAbsolute` or `resolved.startsWith(root)` —
   the two top answers to "prevent path traversal in Node" (TS-SEC-05).
5. Generates an id with `Math.random().toString(36).slice(2)` and uses it as
   a token because it looks random enough (TS-SEC-06).
6. Compares a signature with `===`, because timing attacks are filed
   mentally under cryptography rather than under string comparison
   (TS-SEC-08).
7. Sets `rejectUnauthorized: false` to clear a certificate error while
   debugging something else, and never removes it (TS-SEC-09).
8. Sanitizes HTML early, then appends a wrapper `<div>` to the result before
   inserting it, voiding the sanitization (TS-SEC-10).
9. Assumes `react/no-danger` is on because `vue/no-v-html` is. It is not, in
   any recommended config (TS-SEC-10).
10. Adds `eslint-plugin-security` and reports ReDoS as covered, leaving
    hand-written catastrophic literals entirely unchecked (TS-SEC-12).
11. Confirms `ignore-scripts=true` and reports install-time execution
    disabled, without the `npm run` caveat that setting does not carry
    (TS-SEC-13).

## Sources

- [Node.js Security Best Practices](https://nodejs.org/en/learn/getting-started/security-best-practices) — timing attacks, prototype pollution, `insecureHTTPParser`, `--permission`
- [Node.js `child_process`](https://nodejs.org/api/child_process.html) — the identical "never pass unsanitized user input" warning on `exec` and on `shell: true`
- [Node.js `path`](https://nodejs.org/api/path.html) — `isAbsolute` is "not safe for mitigating path traversals"; no traversal guidance on `resolve`
- [OWASP Prototype Pollution Prevention Cheat Sheet](https://cheatsheetseries.owasp.org/cheatsheets/Prototype_Pollution_Prevention_Cheat_Sheet.html) — the graduated mitigation order and the `--disable-proto` limitation
- [OWASP NPM Security Cheat Sheet](https://cheatsheetseries.owasp.org/cheatsheets/NPM_Security_Cheat_Sheet.html) — install-script policy, allowlisting, token scoping
- [npm config — `ignore-scripts`](https://docs.npmjs.com/cli/v10/using-npm/config#ignore-scripts) — the documented `npm run` gap
- [Renovate — minimum release age](https://docs.renovatebot.com/key-concepts/minimum-release-age/) — the PR-gating layer, distinct from the package manager's install-time cooldown
- [eslint-plugin-regexp rules](https://ota-meshi.github.io/eslint-plugin-regexp/rules/) — structural backtracking analysis
- [eslint-plugin-security](https://github.com/eslint-community/eslint-plugin-security) — source-aware regex and child-process detection
- [DOMPurify](https://github.com/cure53/DOMPurify) — sanitize last; post-sanitization edits void the result
- [Vue security guide](https://vuejs.org/guide/best-practices/security.html) — `v-html`, URL injection, style injection as three separate controls
- [Standard Schema](https://github.com/standard-schema/standard-schema) — the validator-agnostic interface zod, valibot and arktype implement
- [VS Code Workspace Trust Extension Guide](https://code.visualstudio.com/api/extension-guides/workspace-trust) — capability declarations and the runtime trust check
