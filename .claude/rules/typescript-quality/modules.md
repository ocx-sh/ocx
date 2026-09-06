---
title: Modules and Resolution
summary: How TypeScript resolves a specifier, which extension the format demands, when a type import is elided, and what the import graph is allowed to look like
---

# Modules and Resolution

`module`/`moduleResolution` selection, the `.js`-extension rule and what actually
triggers it, `verbatimModuleSyntax` and `import type`, ESM/CJS interop, cycles,
barrels and workspace boundaries. Loads on any diff touching a `tsconfig*.json`,
an import specifier, or a `package.json` `"type"` field.

Contents: [Choosing the Mode](#choosing-the-mode-pinned) ·
[The Resolved Config](#the-resolved-config) ·
[Module Format and Extensions](#module-format-and-extensions) ·
[Type-Only Imports](#type-only-imports) · [The Import Graph](#the-import-graph) ·
[Barrels and Package Boundaries](#barrels-and-package-boundaries) ·
[What a Doc May Claim](#what-a-doc-may-claim) ·
[What Agents Get Wrong](#what-agents-get-wrong-here)

**Every trap below surfaces under `tsc --showConfig` or `tsc --noEmit`, and every
one of them is invisible to a JSON-schema check on the tsconfig.** That is the
parent failure this file exists to prevent: editing module configuration without
running the compiler against it.

The `package.json` distribution surface — `exports` condition order, `bin`, pack
verification, `engines` — is `TS-PKG`, not this file. The strictness flags that
sit beside these in the same `compilerOptions` block are `TS-CFG`.

Severity maps onto the house tiers: MUST = Block, SHOULD = Warn.

## Choosing the Mode (pinned)

One question decides it: **what program reads your import specifiers last, before
anything executes?** Not "target runtime", not "consumer toolchain" — TypeScript's
own handbook defines *host* this way and it subsumes both. If a bundler rewrites
the specifiers, the bundler is the host. If Node loads the emitted files by path,
Node is.

| What reads the specifiers last | `module` / `moduleResolution` | `.js` on relative imports |
|---|---|---|
| Node loads the emitted files by path | `nodenext` / `nodenext` | required once the package is `"type": "module"` |
| Anything outside this repository `import`s the package | `nodenext` / `nodenext` | same |
| One specific bundler always intervenes, and nothing external imports it | `esnext` or `preserve` / `bundler` | never required |
| A subtree the main config cannot resolve at all (single-file components, non-JS specifiers) | its own `bundler` config, with the forcing constraint in an inline comment | never required |

The burden of proof sits on `bundler`, not on `nodenext`: `bundler` mode is
documented as *infectious* — it accepts specifiers that work in a bundler and
crash in Node, with no compiler error.

**Plain `nodenext`, not the frozen `"module": "node18"`/`"node20"` pin — an
adopter default, not a law.** The handbook recommends the frozen pairing for
libraries; a TypeScript-team blog post recommends plain `nodenext`. The frozen pin
buys hypothetical stability for a certain recurring cost: a manual bump per Node
major. Take `nodenext` and revisit on an actual movement-induced break, or pin if
your release cadence cannot absorb one.

## The Resolved Config

One check catches this whole block: **`tsc --showConfig -p <tsconfig>`**, which
prints implied values as well as literal ones. Nothing else surfaces an invalid
value and an unexpected implied pairing in a single command.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-MOD-03 | Run `tsc --showConfig` after every edit to `module` or `moduleResolution`, and read the resolved pair before considering the change done. | A JSON-schema check validates shape, not semantics: `module: node18` silently implies `moduleResolution: node16`, and an invalid value is inert until something runs the compiler. | `tsc --showConfig -p <tsconfig>`; a non-zero exit is an invalid value, and the printed pair is the answer to every other rule here. | MUST |
| TS-MOD-01 | Set `moduleResolution` from the host question, never from the product category: `nodenext` when Node loads the emitted files or when code outside this repository imports the package; `bundler` only when one bundler always intervenes and no external importer exists. | `bundler` accepts specifiers that resolve under a bundler and crash under Node, with no compiler error — the error surfaces in someone else's install. | `tsc --showConfig -p <tsconfig>` for the effective value, then `jq '.exports' package.json` — a non-null `exports` with a consumer outside this repository forbids `bundler`. | MUST |
| TS-MOD-02 | Never write `"moduleResolution": "node"`, `"node10"`, `"node18"` or `"node20"`. The only legal values are `node16`, `nodenext`, `bundler`. `node18`/`node20` are legal `module` values, and they imply `moduleResolution: node16` — the frozen algorithm, never `nodenext`. | `node10`/`node` is deprecated in TS 6.0 (`TS5107`) and hard-removed in 7.0 with no `ignoreDeprecations` escape (`TS5108`); `node18`/`node20` in this position are `TS6046`. Verified against tsc 6.0.3 and 7.0.2, 2026-08-29. | `rg -n '"moduleResolution"' --glob 'tsconfig*.json' .` — every hit must read `node16`, `nodenext` or `bundler`. Empty output is **not** a pass: it means no file sets it literally, so read the resolved value with TS-MOD-03 instead. | MUST |
| TS-MOD-06 | Before editing import extensions in bulk, resolve the *effective* tsconfig for the specific file — respecting `include`/`exclude` and any sibling `tsconfig.*.json` — not the repository's top-level one. | A subtree with its own `bundler` config holds deliberately extensionless specifiers, some of which take no `.js` under any mode. A uniform "always suffix `.js`" edit corrupts every one of them and the top-level config never mentions them. | `tsc -p <the config whose include covers the file> --listFilesOnly > program.txt`, then confirm the file appears in `program.txt` before editing it. A file in no program is edited against a config that does not govern it. | MUST |
| TS-MOD-22 | In an `extends`-chain monorepo, verify each base-level flag survives **every** package's own `extends` line by reading the resolved config — not the base config, and not the package file's text. | A package extending a third-party framework preset instead of the repository base silently drops the base's flags. The gap is invisible from either file alone. | `tsc -p <each package tsconfig> --showConfig > resolved.json` and read the flag out of `resolved.json`, per package. | MUST |

## Module Format and Extensions

The `.js` requirement is triggered by a file's **module format**, not by the
`moduleResolution` name. Format comes from the `.mts`/`.cts` extension or, for a
plain `.ts`, from the nearest `package.json`'s `"type"` field. A `.ts` file in a
package with no `"type": "module"` is CommonJS-format, and CJS-format files under
`node16`/`nodenext` do not need the extension at all. **This is why grep is not
the verifier here and `tsc --noEmit` is.**

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-MOD-04 | In a package with `"type": "module"` under `node16`/`nodenext`, every relative import carries an explicit `.js`/`.mjs`/`.cjs` extension. | `TS2835` otherwise; Node's ESM loader performs no extension search, so an extensionless specifier that type-checks under a bundler dies at the first real `import`. | `tsc --noEmit -p <tsconfig>` — authoritative and free. Do **not** verify by grep: an extensionless relative import is correct in a CJS-format file and correct under `bundler`, so a pattern match cannot tell a violation from a legal one. | MUST |
| TS-MOD-05 | A package resolving to `node16`/`nodenext` also declares `"type": "module"`, or its tsconfig carries an inline comment naming why it stays CommonJS-format. | Without it the extension check is a silent no-op — the setting stops doing the work its name implies, and one unrelated `package.json` edit turns every relative import red at once. It also forecloses `verbatimModuleSyntax` outright (TS-MOD-17). | For each tsconfig whose resolved `moduleResolution` is `node16`/`nodenext`, `jq -r '.type' <that package>/package.json` must print `module`, or the comment must be there. `null` with no comment is the finding. | SHOULD |

## Type-Only Imports

`isolatedModules` and `verbatimModuleSyntax` are **disjoint guarantees, not a
weak/strong pair** — any prose ranking them is wrong. Measured against tsc 6.0.3:
`isolatedModules` alone forces `export type` on a re-export (`TS1205`) and
compiles an unmarked type-only *import* clean; only `verbatimModuleSyntax` raises
`TS1484` there.

The flag's gating condition is the **module-format pair**, not the product
category: it is free on `bundler`, and free on `node16`/`nodenext` *with*
`"type": "module"`. It is structurally impossible on `node16`/`nodenext` *without*
it, and two packages on the same target platform land on opposite sides of that
line purely from a `moduleResolution` choice.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-MOD-16 | Set `verbatimModuleSyntax: true` wherever the resolved `moduleResolution` is `bundler`, or is `node16`/`nodenext` **and** the nearest `package.json` declares `"type": "module"`. Prove it with the CLI override before editing any file. | It closes the import-side gap `isolatedModules` leaves open. On qualifying shapes the measured cost is zero errors — this is a one-line config change, not a migration. | `tsc -p <tsconfig> --noEmit --verbatimModuleSyntax > tc.log 2>&1`, then `rg -n -e TS1484 -e TS1205 -e TS1287 -e TS1295 -e TS1202 tc.log` — a deliberate union of the five codes this flag owns. Empty output means commit the flag; every other line in `tc.log` is pre-existing and unrelated. | MUST |
| TS-MOD-17 | Never set `verbatimModuleSyntax` on a package whose resolved `moduleResolution` is `node16`/`nodenext` while its nearest `package.json` has no `"type": "module"`. Fix the module format first — add `"type"`, or move to `bundler`. | ES `import`/`export` syntax in a file the compiler classifies as CommonJS-format is a hard `TS1295`/`TS1287`: structural, not lint-fixable, and no number of `type` keywords touches it. Every import and export in the package errors at once. | `jq -r '.type' package.json` before flipping the flag. `null` plus a `node16`/`nodenext` resolved value means blocked — not "try it and see". | MUST |
| TS-MOD-21 | Never write `importsNotUsedAsValues` or `preserveValueImports` in any tsconfig. | Deprecated in TS 5.0, no-op from 5.5, and a hard `TS5102` build failure from 6.0 — reproduced on 6.0.3, exit 2. On a 6.x floor this is a broken build, not a warning. | `rg -n --glob 'tsconfig*.json' -e importsNotUsedAsValues -e preserveValueImports .` — a deliberate union; any hit is the violation, empty output is the pass. | MUST |
| TS-MOD-18 | Never leave an import whose specifiers are **all** inline-`type`. Enable `@typescript-eslint/no-import-type-side-effects` wherever `verbatimModuleSyntax` is on; Biome's `useImportType` in `auto` style already cannot produce the shape. | Such an import is not elided — it emits `import {} from "mod"`, a bare side-effect import, with **zero** compiler diagnostics. The bug exists only in emitted JS the author never reads. | `rg -n --pcre2 --glob '*.ts*' 'import \{\s*type [^}]*\} from' src` and confirm every hit carries at least one non-`type` specifier. Then the rule name in `eslint.config.*`, or `useImportType` in `biome.json`. | MUST |
| TS-MOD-19 | Name `consistent-type-imports` and `no-import-type-side-effects` explicitly in the ESLint config. Never assume a `typescript-eslint` preset supplies either. | Verified against the installed plugin (8.61.0, `dist/configs/flat/*.js`): both appear only in `all` — not in `recommended`, `recommended-type-checked`, `strict`, `strict-type-checked`, `stylistic`, or `stylistic-type-checked`. A config pointing at a strict preset "to get the rule" never contained it. | `rg -l -e consistent-type-imports -e no-import-type-side-effects node_modules/@typescript-eslint/eslint-plugin/dist/configs/flat/` returns only `all.js`. Then `rg -n --glob 'eslint.config.*' -e consistent-type-imports -e no-import-type-side-effects .` — here **empty output is the finding**. | SHOULD |
| TS-MOD-20 | In a package blocked from the compiler flag by TS-MOD-17, wire `@typescript-eslint/consistent-type-imports: "error"` as the substitute — and do not record that package as satisfying TS-MOD-16. | Autofixable, needs no module-format migration, catches the same import-side pattern. It does not give the export-side elision determinism; naming that shortfall is the point. | The rule name in `eslint.config.*`, then `eslint --fix .` and a clean re-run. | SHOULD |

```ts
// wrong — every specifier inline-type; emits `import {} from "./api"`, a side-effect import
import { type Client } from "./api";
```

```ts
// right — the whole statement is erased
import type { Client } from "./api";
```

## The Import Graph

No import-graph rule is on by default in either linter, so this block is a set of
pinned defaults an adopter may override — the value is that they are agreed and
mechanical, replacing an invariant otherwise enforced by a comment. Cycle
detection is affordable on save at a few dozen files per package; re-time it
before assuming that holds past roughly 500.

```js
// eslint.config.js
rules: { 'import-x/no-cycle': ['error', { ignoreExternal: true }] }
settings: { 'import-x/resolver-next': [createTypeScriptImportResolver({ project: './tsconfig.json' })] }
```

```json
// biome.json — the rule name alone is a no-op without the domain
{ "linter": { "domains": { "project": "recommended" },
  "rules": { "suspicious": { "noImportCycles": "error" } } } }
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-MOD-12 | When enabling **any** Biome `project`-domain rule — `noImportCycles`, `noUndeclaredDependencies`, `noUnresolvedImports`, `noPrivateImports` — set `linter.domains.project` in the same change. | Biome builds the module graph only when the domain is on. The rule name under `linter.rules` alone ships a config that looks enforced and is very plausibly a no-op, with no error to say so. | `rg -n -e noImportCycles -e noUndeclaredDependencies -e noUnresolvedImports -e noPrivateImports biome.json` — a deliberate union; any hit demands that `rg -n -A2 '"domains"' biome.json` show `"project"`. | MUST |
| TS-MOD-09 | Enable `import-x/no-cycle` with `ignoreExternal: true` and no `maxDepth` cap, or Biome `noImportCycles`. | A cycle that is safe today is safe by hand-enforced discipline; the rule makes it mechanical before an edit reintroduces an eagerly-read binding across the edge. | `npx madge --circular --extensions ts,tsx <src>` for the baseline — the list it prints is what the enabled rule will report on day one; the enabled rule thereafter. | SHOULD |
| TS-MOD-10 | When breaking a reported cycle, decide by **when the binding is read**, not by `const`-vs-`function`. A `const` read only inside a function body is exactly as safe as a hoisted function; a function invoked at module top level across the edge is not. | The TDZ hazard is eager top-level evaluation, not the declaration keyword. Flipping the keyword moves code without moving the hazard. | For each binding crossing a reported cycle edge, read the importing file and locate every use: a use outside a function or class body is the violation. Move that binding to a third module rather than changing its keyword. | SHOULD |
| TS-MOD-11 | Install `eslint-plugin-import-x`, not `eslint-plugin-import`, and wire the resolver through `import-x/resolver-next` + `createTypeScriptImportResolver(...)` — never the legacy `settings['import/resolver'].typescript` object. | `import-x` is the fork that ships the TypeScript-era fixes; upstream declined the `exports`-field requests that produced it. The legacy settings shape is what training-era examples show and it is inert against `import-x >= 4.5.0`. The resolver handles the `./foo.js`→`foo.ts` rewrite correctly (fixture-verified 2026-08-29), so a NodeNext codebase needs no exception. | `rg -n '"eslint-plugin-import"' package.json` returns nothing — the closing quote excludes the `-x` fork. Then `rg -n "'import/resolver'" --glob 'eslint.config.*' .` returns nothing wherever `import-x` is installed. | SHOULD |
| TS-MOD-13 | Enable `import-x/no-extraneous-dependencies` (`packageDir` per package in a monorepo) or Biome `noUndeclaredDependencies`, ignoring the host-injected virtual modules your platform supplies — an extension host's `vscode`, a framework's `astro:content`, a runtime's `bun:test`. Never treat a green run in a checkout without `node_modules` as meaningful. | It catches the dependency that lives in `devDependencies` and breaks on a consumer's clean install. Without a resolvable tree the rule silently no-ops, and a no-op is indistinguishable from a clean run. | `test -d node_modules` before trusting any run of this rule. Then the rule's ignore list must name each virtual module the platform injects; `import-x` has no built-in alias exemption, so a `@/`-style path alias needs the TypeScript resolver from TS-MOD-11 or it reads as a scoped npm package. | SHOULD |

## Barrels and Package Boundaries

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-MOD-14 | Never flag a file as a barrel because it is named `index.ts`. Target the re-export fan-out pattern instead. | `index.ts` is also the conventional name for a CLI entry point, a subsystem's main file, and framework-mandated router config. A filename rule produces several false positives per real hit, and each one costs a pointless split. | Per file: `rg -c '^export\s.*\bfrom\b' <file>` against `wc -l <file>`. Mostly re-exports is a barrel; substantial own logic is a main file. No output from that count means zero re-exports — not a barrel. | SHOULD |
| TS-MOD-15 | Never reach across a workspace package boundary with a relative path, not even from a test. Fix it by relocating the fixture, not by rewriting the specifier to the package's bare name. | `no-relative-packages`' autofix rewrites the specifier; if the target is not in the other package's `exports` map the "fix" just moves the failure to `no-unresolved`. Biome has no rule covering this shape at all, so in a Biome workspace the guard is structural or absent. | `rg -n --glob '*.ts*' '\.\./\.\./' <workspace packages dir>` — read each hit and resolve it: one landing inside a sibling package's source tree is the violation. Before accepting any autofix, confirm the target path appears in that package's `exports`. | SHOULD |

## What a Doc May Claim

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-MOD-23 | A rule file, README or docblock may assert a compiler-enforced guarantee only where the resolved config confirms the flag. Otherwise state it as a convention and say it is unenforced. | A doc asserting `verbatimModuleSyntax: true` "forces `import type`" in a package that never sets it leaves an agent worse off than no claim at all — it reads as verified and licenses skipping the check. Copied rule files propagate the false claim verbatim. | `tsc -p <tsconfig> --showConfig > resolved.json`, then read each flag your own docs name out of `resolved.json`. A doc naming a flag absent from `resolved.json` is the violation — fix the flag or fix the prose, never leave both standing. | MUST |

## What Agents Get Wrong Here

1. Editing a tsconfig and never running the compiler against it. This is the
   parent of items 2, 3 and 6, and every one of them is a one-command catch.
2. `"moduleResolution": "node"` from pretraining — the only Node-flavored value
   before 2022. It still parses in TS 6.x behind a deprecation warning easy to
   lose in noisy output, and hard-fails in 7.0.
3. Inventing `"moduleResolution": "node18"`/`"node20"` by extrapolating from the
   real, similarly named `module` values.
4. Answering `TS2307` by deleting the extension. That is the wrong fix for two of
   its three causes: it trades `TS2307` for `TS2835`, or ships code that fails
   only under real Node. Dropping an extension is legitimate under `bundler` only.
5. Applying an extension fix uniformly across a repository, rewriting a subtree
   whose separate `bundler` config made those specifiers correct.
6. Enabling a Biome `project`-domain rule by name alone, shipping a config that
   looks enforced and is very plausibly a no-op.
7. Treating `isolatedModules` as covering imports because the two flags are
   always named in the same sentence. After adding `verbatimModuleSyntax` the new
   `TS1484`s *are* the imports that were silently relying on default elision —
   they are the work, not noise.
8. Answering "unused import" with `import { type X }` instead of
   `import type { X }`. Both are valid TypeScript; only one of them can collapse
   to a bare side-effect import, and it does so in JS the agent never reads.
9. Declaring `verbatimModuleSyntax` unsafe from a raw error count, without
   separating this flag's five codes from pre-existing missing-types noise.
10. Emitting `importsNotUsedAsValues`/`preserveValueImports` into a fresh tsconfig
    from pre-5.0 training data, especially when asked to "make type imports
    explicit". On a 6.x floor that is a hard build failure.
11. Pointing at a `strict`/`strictTypeChecked` preset as evidence that
    `consistent-type-imports` is on. No preset but `all` contains it.
12. Breaking a cycle by flipping `const` ↔ `function` without checking whether the
    value is read at module top level.
13. Autofixing a cross-package relative import to the bare specifier when the
    target is not in that package's `exports` — the failure just moves.
14. Flagging every `index.ts` as barrel debt on filename alone.
