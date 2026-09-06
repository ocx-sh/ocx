---
paths:
  - "**/package.json"
  - "**/tsconfig*.json"
  - "**/eslint.config.*"
  - "**/biome.json"
  - "**/biome.jsonc"
summary: The declared contract — the tsconfig strictness floor per shape, the manifest, and verifying a package that ships a bin
keywords: typescript,tsconfig,package.json,strict,engines,exports,bin,publint,attw,npm,lockfile,dependencies,scripts,nodenext
license: Apache-2.0
repository: https://github.com/ocx-sh/grimoire-lore
---

# TypeScript Packaging and Configuration

What a repository claims about itself in files no compiler checks. Owns
`TS-CFG` (compiler options and `extends` topology) and `TS-PKG` (manifest,
engines, exports, bin, scripts, dependency placement, publish verification).
Code rules live in the `typescript-quality` sibling, which never loads while
you are editing any of the files above.

Contents: [The Strictness Floor](#the-strictness-floor) ·
[Version-Gated Flags](#version-gated-flags) · [The Manifest](#the-manifest) ·
[Publishing a `bin` Package](#publishing-a-bin-package) ·
[Editing a Lint Config](#editing-a-lint-config) · [Severity](#severity)

**The tsconfig glob is `**/tsconfig*.json`, never `**/tsconfig.json`.** The
narrow form was measured to miss 40% of real tsconfigs — and the ones it
missed carried the most load-bearing decisions: the mixed-resolution split
file, and the monorepo base that was the only place a strictness posture was
stated at all. Do not narrow it back.

## The Strictness Floor

`strict: true` is assumed, not a rule — it is the `tsc --init` default since
TS 5.0 and every config in scope already has it. What it does **not** imply is
the whole content of this section. `strict` covers exactly `alwaysStrict`,
`noImplicitAny`, `noImplicitThis`, `strictBindCallApply`,
`strictBuiltinIteratorReturn` (5.6+), `strictFunctionTypes`,
`strictNullChecks`, `strictPropertyInitialization`, and
`useUnknownInCatchVariables` — and nothing below.

**Pinned default; an adopter may override per repo, but the override states
its reason in a comment in the config that carries it.**

| Shape | Which program reads the specifiers last | `module` / `moduleResolution` | Floor beyond `strict` |
|---|---|---|---|
| **Node-loaded package** — has a `bin`, or anything outside the repo imports it | Node | `nodenext` / `nodenext` | The universal set, plus `exactOptionalPropertyTypes` (a public surface is where "absent" and "present but `undefined`" actually differ) and `declaration` wherever types ship |
| **Bundled app or editor extension** — one bundler always intervenes and no external importer exists | the bundler | `esnext` / `bundler` | The universal set, plus `noEmit: true` |
| **Type-stripped runtime** — Bun, or `node --experimental-strip-types` | the runtime's stripper | `esnext` / `bundler` | The universal set, plus `erasableSyntaxOnly` and `noEmit: true`. These runtimes strip types and never check them, so a standalone `tsc --noEmit` step is the only type gate |
| **Monorepo** | per package | per package | The floor lives in one base file; each member's *resolved* config is what counts, not the base |

Universal set, all outside `strict`: `noUncheckedIndexedAccess`,
`noImplicitOverride`, `noFallthroughCasesInSwitch`, `noImplicitReturns`,
`isolatedModules`, `skipLibCheck`, `allowUnreachableCode: false`,
`allowUnusedLabels: false`.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| **TS-CFG-01** | Every tsconfig resolves to its shape's row above. Judge by the resolved config, never by the file's own text. | A member extending a third-party preset instead of the repo base silently drops every flag the base set — invisible from either file alone. And the universal set is the checkable-but-not-default gap: every community strictness base independently re-adds a subset of exactly these flags. | `tsc --showConfig -p <tsconfig>` per config file, then read the printed values. Any flag from the row missing or `false` is the violation. `--showConfig` also prints *implied* values, which is the only way an unexpected `module`/`moduleResolution` pairing surfaces. | MUST |
| **TS-CFG-02** | Unused bindings have exactly one owner: the linter. Leave `noUnusedLocals` and `noUnusedParameters` off and configure the lint rule with an underscore ignore pattern instead. | The compiler flags have no ignore pattern, so an interface-implementing method or a callback with a deliberately unused parameter has no legal spelling — and the fix that follows is switching the check off entirely. | `rg -n --glob '*tsconfig*.json' -e 'noUnusedLocals' -e 'noUnusedParameters' .` — a union; any hit is the violation. Then confirm the lint config names an unused-vars rule with an `argsIgnorePattern`. | SHOULD |
| **TS-CFG-03** | Set `allowUnreachableCode` and `allowUnusedLabels` explicitly to `false`. | Left unset they are neither on nor off: the compiler emits an editor-only suggestion that CI never sees, so the code reads as checked and is not. | `tsc --showConfig -p <tsconfig>` — the two keys must be present and `false`. Absent is the violation, not a pass. | SHOULD |
| **TS-CFG-04** | Set `erasableSyntaxOnly` in any config whose code is executed by a runtime that strips types rather than compiling them. | `enum`, `namespace` and parameter properties have runtime semantics a stripper cannot produce. Without the flag they type-check clean and misbehave only when executed — the one failure mode a type checker is supposed to make impossible. | `tsc --showConfig -p <tsconfig>` shows `erasableSyntaxOnly: true`. Needs TS 5.8+; see the version table. | MUST |
| **TS-CFG-05** | When one repo holds both Node-loaded code and bundler-only code, give each its own `tsconfig.<name>.json` split on `moduleResolution` — never widen one config to admit both. | A single config that permits both accepts extensionless relative specifiers in the half that Node loads by path, and the error surfaces at run time in a consumer's install, not at build time. This is also why the glob must match `tsconfig*.json`. | For each source tree, `tsc --showConfig -p <the config whose include actually covers those files>` and confirm the file is matched before editing anything in it. Two trees resolving to one config with different hosts is the violation. | SHOULD |
| **TS-CFG-06** | A rule file, README, CLAUDE.md or docblock may assert a compiler-enforced guarantee only where the resolved config confirms that flag. Otherwise state it as an unenforced convention and say so. | Documentation claiming a flag the config does not set was measured in identical wording across two repos — copied once, checked by neither. An agent that trusts the claim is worse off than one given no claim at all. | For every compiler flag named in prose: `tsc --showConfig -p <tsconfig>` and grep the printed value. A miss means fix the flag or fix the prose — never leave both standing. | MUST |

## Version-Gated Flags

Compiler versions as of 2026-08-29: TS 7.0 is the current stable release,
6.0 is a common working floor, and two-majors-behind repos are normal. Any
rule resting on a flag below binds only where the pin clears its release.
An unknown `compilerOptions` key is a hard `tsc` error, not a silent ignore —
so an over-new flag fails loudly, which is the good case.

| Key | Needs | On an older compiler |
|---|---|---|
| `verbatimModuleSyntax` | 5.0 | Unknown-option error; build fails |
| `isolatedDeclarations` | 5.5 | Unknown-option error; build fails |
| `noUncheckedSideEffectImports` | 5.6 | Unknown-option error; build fails |
| `strictBuiltinIteratorReturn` (inside `strict`) | 5.6 | `strict` implies one fewer flag — a rule citing it does not hold below 5.6 |
| `erasableSyntaxOnly` | 5.8 | Unknown-option error; build fails |
| `importsNotUsedAsValues`, `preserveValueImports` | removed in 6.0 | Hard `TS5102` and exit 2 — never write either; `verbatimModuleSyntax` replaced both |
| `"moduleResolution": "node"` / `"node10"` | deprecated 6.0, removed 7.0.2 | `TS5107` then `TS5108`, with no `ignoreDeprecations` escape. Legal values are `node16`, `nodenext`, `bundler` |

## The Manifest

Every rule here is a claim nothing in a normal build verifies. They are all
checkable from the repo root with `jq` and `ls`.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| **TS-PKG-10** | Never declare an `engines.node` floor on a Node line that has reached end of life, and back the floor with a CI leg pinned to the **literal** declared version — not "latest of that major". | `engines.node` is advisory-only without `engine-strict`, and the manifest linters only check the field's *presence*, never its currency. Nothing in a normal pipeline verifies the number is true. Node 20 reached EOL on 2026-03-24. | `jq -r '.engines.node' package.json`, compared against [nodejs.org/en/about/previous-releases](https://nodejs.org/en/about/previous-releases); then confirm the CI matrix contains that exact version string, not just its major. | MUST |
| **TS-PKG-14** | Every script in `scripts` must run against a config that exists. Delete a script whose tool has no config rather than leaving it declared. | A `lint` script naming a linter with no config file anywhere is a gate that reads as present, fails if anyone runs it, and is therefore never run. This is the silent-pass class, in manifest form. | `jq -r '.scripts.lint // empty' package.json` returning a command, with `ls eslint.config.* biome.json biome.jsonc 2>/dev/null` printing nothing, is the violation. Repeat for every script naming a config-driven tool. | MUST |
| **TS-PKG-12** | Test, build and type-only packages go in `devDependencies`. Never `dependencies`. | Everything in `dependencies` is installed into every consumer of the package, and a test-only DOM shim there is both a several-megabyte tax and an unaudited dependency in a production tree. | `jq -r '.dependencies // {} \| keys[]' package.json \| rg -e '^@types/' -e '^@testing-library/' -e 'jsdom' -e 'vitest' -e 'jest' -e 'playwright' -e 'eslint' -e 'prettier' -e '^typescript$'` — a union of literal names; every line printed is the violation and empty output is the pass. The list is a starting heuristic, not exhaustive: read the whole key list once. | SHOULD |
| **TS-PKG-13** | Exactly one lockfile kind per workspace, and it is the one the CI install command produces. | Two committed lockfiles for one tree means one of them is stale and nobody knows which — and a contributor installing with the unused manager gets a resolution CI has never seen. | `ls package-lock.json pnpm-lock.yaml yarn.lock bun.lock bun.lockb 2>/dev/null` — more than one line is the violation. Then `jq -r '.packageManager // empty' package.json` must name the manager that produces the surviving one. | SHOULD |

## Publishing a `bin` Package

Sized for a package whose product is an executable, with at most an
incidental `exports` map — not for a typed library. `publint` and
`@arethetypeswrong/cli` verify that the manifest *declares* correctly; only
installing the tarball and running the installed binary proves the artifact
works. A package once shipped without its `bin` past a check that stopped at
the declaration.

```jsonc
// wrong — conditions alphabetized; the resolver matches "default" and never reaches "types"
"exports": { ".": { "default": "./dist/cli.js", "types": "./dist/cli.d.ts" } }
```

```jsonc
// right — key order is match priority: types first, default last
"exports": { ".": { "types": "./dist/cli.d.ts", "default": "./dist/cli.js" } }
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| **TS-PKG-01** | CI packs the tarball, installs it into a scripts-disabled sandbox outside the repo, and **executes the installed bin**, asserting on its output. | Manifest linting proves the declaration, not the artifact. A missing build step, an unbuilt entry file or a dependency in the wrong block all pass every declaration check and fail on first install. | The verification script contains `npm install --ignore-scripts <tarball>` inside a `mktemp -d` directory, followed by executing `node_modules/.bin/<name>` and asserting on its output. | MUST |
| **TS-PKG-02** | Run that pack-smoke on every pull request, not only at tag or release time. | A release-only gate finds the defect after it is already on the default branch, when the fix is a new version rather than a rejected diff. | `rg -l -e 'pack-smoke' -e 'publint' -e 'attw' .github/workflows/` — a union; the PR workflow must be among the files listed, not only the release workflow. | MUST |
| **TS-PKG-03** | Also run `npm publish --dry-run` and fail on the string `auto-corrected`; treat only the exact "cannot publish over the previously published versions" rejection as non-fatal. | Only `publish` normalizes the manifest — `pack` goes through a different code path and never does, so a pack-only check *structurally cannot* catch a silently auto-corrected field. Between releases the version is already live, so an unfiltered dry-run fails on every PR for an unrelated reason. | Two distinct invocations in the script — `npm pack` **and** `npm publish --dry-run` — and an error branch matching that rejection text, never a bare non-zero exit. | MUST |
| **TS-PKG-04** | Order `exports` conditions with `types` first and `default` last. | The resolver treats key order as match priority, not decoration. Alphabetizing the block breaks type resolution with no error anywhere. | `publint run <tarball>` inside the pack-smoke: `EXPORTS_TYPES_SHOULD_BE_FIRST` and `EXPORTS_DEFAULT_SHOULD_BE_LAST` are the violations. | MUST |
| **TS-PKG-05** | Every `bin` target file starts with `#!/usr/bin/env node`, and the build chmods it executable. Never ship a `postinstall` that chmods for the consumer. | npm self-heals the POSIX mode on a real install but never inserts a missing shebang — the shebang is the half npm cannot fix, and the mode bit still matters to anything that consumes the tarball outside npm. | `publint`'s `BIN_FILE_NOT_EXECUTABLE` gates the PR; `ls -l <built bin>` after a clean build shows the executable bit. | SHOULD |
| **TS-PKG-06** | When `attw` reports `cjs-resolves-to-esm` on a pure-ESM package with no CJS entry point, ignore exactly that one rule — never disable `attw` wholesale or widen the ignore list. | It is the expected shape of an ESM-only package. A broad disable also hides `NoResolution` and `InternalResolutionError`, which are real defects. | The `attw` invocation's `--ignore-rules` contains exactly `cjs-resolves-to-esm` and nothing else. | SHOULD |
| **TS-PKG-08** | For a package deliberately without a `"."` export, suppress the missing-root-entrypoint finding per package with a comment naming the CLI-only shape — never add a root export to silence it. | A root export added to satisfy a linter creates a real, unintended, permanently supported public API surface out of whatever file was nearest. | The `publint` ignore list carries that one code plus the comment. A new `"."` key in `exports` appearing in the same diff as a linter complaint is the violation. | SHOULD |
| **TS-PKG-09** | The pack-smoke's dependency walk extracts dynamic `import()` and `require()` specifiers, not only static `import … from`. | A dependency reachable only through a lazy `import()` is invisible to a static scan and can sit in `devDependencies` until a consumer's install breaks — a shipped incident, not a hypothesis. | The extraction handles both call forms, and the walk resolves bare specifiers against a sandbox `node_modules` populated only from `dependencies` and `peerDependencies`. | SHOULD |
| **TS-PKG-15** | Read the published file list once per release — via a `files` allowlist, or the pack listing itself. | The default publish set is "everything not ignored". A fixture directory, a source map, or a stray dotfile ships silently and is unpublishable-back once it is on a registry. | `npm pack --dry-run` and read the printed listing. Any path you did not intend to ship is the violation; `publint`'s `USE_FILES` flags the missing allowlist. | MUST |
| **TS-PKG-11** | Do not add a dual CJS/ESM build to support `require()` consumers once `engines.node` is at or above 20.19.0 / 22.12.0. | `require(esm)` is unflagged and warning-free from exactly those versions. A dual build adds real build, test and resolution surface for a problem that no longer exists at the package's own declared floor. The one surviving constraint is top-level `await`, which still raises `ERR_REQUIRE_ASYNC_MODULE`. | Compare `jq -r '.engines.node' package.json` against those thresholds, then `rg -n --glob '*.ts' -e '^await ' -e '^const .* = await ' src` for top-level await on a `require()`-reachable path. | SHOULD |

## Editing a Lint Config

These files are in this rule's globs because the defects they carry are
*config* defects, and the code rules never load here. The rules themselves
belong to the `typescript-quality` sibling — this is the pointer, not a
second copy.

- **Type-aware lint rules do not exist without a project service.** The
  floating-promise, misused-promise and unsafe-`any` families need
  `parserOptions.projectService` (or `project`) wired; the plain
  `recommended` preset cannot see any of them, and its clean run is not
  evidence. Check: `rg -n -e 'projectService' -e 'parserOptions' eslint.config.*`
  — empty output means every type-aware rule in the file is inert. Owned by
  `TS-GATE`.
- **A preset does not supply `consistent-type-imports` or
  `no-import-type-side-effects`** — both must be named explicitly in the
  config. Owned by `TS-MOD-19`.
- **A Biome project-domain rule name alone is a no-op.** Setting
  `linter.domains.project` in the same change is what builds the module
  graph. Owned by `TS-MOD-12`.

## Severity

MUST = Block: fix before it lands. SHOULD = Warn: fix, or state why not in
the commit body. CONSIDER = Suggest: never blocks, never re-raised after a
decline.

Every verification above states what empty output means. Before adding one,
watch it go red against a deliberately broken copy — a check that cannot
fail launders an unchecked change as a checked one.
