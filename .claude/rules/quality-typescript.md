---
paths:
  - "**/*.ts"
  - "**/*.tsx"
  - "**/*.mts"
  - "**/*.cts"
  - "**/tsconfig*.json"
---

# TypeScript Code Quality

TypeScript-specific quality guide (TS 5.x, 2026). Universal design principles
(SOLID, DRY, YAGNI, severity tiers, review checklist) live in `quality-core.md` —
this file covers **TypeScript-specific applications** plus modern strict-mode
baseline, module system, and tooling guidance. For Vite build-tool specifics,
see `quality-vite.md`.

Project-independent and shareable.

---

## tsconfig.json Strictness Baseline (2026)

`strict: true` is mandatory and non-negotiable. The 2026 community consensus
(Total TypeScript, WhatIsLove.dev) adds these flags on top of `strict` as the
de facto standard for new projects:

```jsonc
{
  "compilerOptions": {
    "strict": true,
    "noUncheckedIndexedAccess": true,          // arr[0] → T | undefined
    "exactOptionalPropertyTypes": true,        // ? means absent, not undefined
    "noPropertyAccessFromIndexSignature": true,
    "verbatimModuleSyntax": true,              // type imports stay type-only in emit
    "moduleResolution": "Bundler",             // for Vite/Bun/esbuild projects
    "module": "ESNext",
    "isolatedModules": true                    // safe for single-file transforms
  }
}
```

**`noUncheckedIndexedAccess` is NOT yet part of `strict`** (TS issue #49169, open since 2022). It's the single highest-value flag missing from strict mode — always enable it explicitly.

---

## `any` vs `unknown`

- **`unknown`** is the correct type for values whose shape you don't control (API responses, `JSON.parse`, error catches). Narrow before use.
- **`any`** is acceptable only at deliberate escape hatches in adapter/interop code and test fixtures — never in library surface or domain logic.
- **`catch (e)`** defaults to `unknown` since TS 4.4 (`useUnknownInCatchVariables`, included in `strict`). Never `catch (e: any)`.
- **Block-tier**: `any` in function signatures that cross module boundaries dissolves the entire type graph downstream.

---

## Anti-Patterns (TypeScript-Specific)

### Block (must fix before merge)

- **`any` in exported function signatures** — dissolves the entire type graph downstream.
- **`as SomeType` to silence a type error** — assertion without narrowing. Use a type guard (`is` predicate) or discriminated union instead.
- **Non-null assertion (`!`)** without justification — hides potential runtime errors. Use optional chaining (`?.`) or explicit checks.
- **`catch (e: any)`** — use default `unknown` and narrow with `instanceof` or type predicates.
- **`@ts-ignore` without a comment** — comment explaining why is mandatory. Prefer `@ts-expect-error` so the suppression is removed when the underlying issue is fixed.
- **TypeScript `enum`** — numeric enums are erased at runtime and cause subtle reverse-mapping bugs. Use `const` union types: `type Direction = "north" | "south"`.
- **`Object` / `{}` as a type** — use `Record<string, unknown>` or a named interface.
- **Implicit `any` from missing type annotations** — caught by `noImplicitAny` (part of `strict`).
- **Index signature access without `undefined` check** — caught by `noUncheckedIndexedAccess`.
- **Optional property typed as `T | undefined`** instead of `?: T` — caught by `exactOptionalPropertyTypes`.
- **`eval()` / `Function()` constructor** — injection risk. Always find a typed alternative.

### Warn (should fix)

- **Overusing generics** where `unknown` + narrowing suffices
- **Type predicates (`is`) without airtight runtime checks** — false type guards are silent bugs
- **Intersecting incompatible types with `&`** to "merge" them — use `Omit` + spread instead
- **`Function` as a type** — use explicit signature `(...args: unknown[]) => unknown`
- **Deeply nested conditional types** — split into named aliases
- **Barrel files (`index.ts`)** in library code that impede tree-shaking
- **Index signatures (`[key: string]: T`)** where a `Record<K, T>` or explicit interface would prevent typos

---

## Type Narrowing Patterns

- **Discriminated unions**: tag every union with a `kind`/`type` literal field. TypeScript narrows exhaustively in switch statements. Far safer than structural unions.
- **`satisfies` operator** (TS 4.9+): validates a value conforms to a type without widening its inferred type. Pattern: `const config = { … } satisfies Config` instead of `const config: Config = { … }` when you need autocomplete on literal values.
- **`as const`**: freezes literal types. Combine: `const STATUSES = ["open", "closed"] as const satisfies readonly Status[]`.
- **`never` exhaustion check**: in the default branch of a discriminated-union switch, assign to `never` to get compile errors on missing cases.

```ts
function handle(msg: Message): Result {
  switch (msg.kind) {
    case "text": return handleText(msg);
    case "image": return handleImage(msg);
    default: {
      const _exhaustive: never = msg;
      throw new Error(`Unhandled: ${_exhaustive}`);
    }
  }
}
```

---

## Module System (ESM-only in 2026)

- `"type": "module"` in `package.json`
- **`verbatimModuleSyntax: true`** — forces `import type` for type-only imports; what you write is what gets emitted. Load-bearing for single-file transpilers (esbuild, SWC, Bun) that don't do type-aware elision.
- **`moduleResolution: "Bundler"`** for Vite, Bun, esbuild — do NOT use `"node16"` unless targeting Node without a bundler.
- `.mts` / `.cts` extensions: only when mixing ESM and CJS in the same package. Unnecessary in bundler-only contexts.

---

## 2026 Features Worth Knowing

- **`using` / `await using`** (TS 5.2): Explicit Resource Management. Objects implementing `Symbol.dispose` are automatically disposed at scope exit. Relevant for file handles, DB connections, test teardown.
  ```ts
  async function processFile(path: string) {
    await using file = await openFile(path);  // auto-closed at scope exit
    return file.read();
  }
  ```
- **Import attributes** (TS 5.3): `import data from "./data.json" with { type: "json" }`. Replaces the old `assert` syntax. Needed for JSON module imports in native ESM.
- **Standard decorators** (TS 5.0): Standard ECMAScript decorator proposal, NOT the legacy experimental decorators. Do not set `experimentalDecorators: true` in new code.

---

## Tooling (2026 State)

| Tool | Status | Use when |
|------|--------|----------|
| **Biome** | Recommended default | New projects, single binary, 10-25x faster, ~85% ESLint rule coverage |
| **ESLint + `@typescript-eslint`** | Still required | Type-aware rules, framework-specific plugins (react, vue, svelte), custom rules |
| **Oxlint** | Emerging | Experimental speed option; keep alongside ESLint for rule coverage |
| **tsc** | Type checking only | Never the build tool in Vite/Bun projects — use `tsc --noEmit` in CI |
| **Bun** | Type-stripping runtime | Pair with `tsc --noEmit` in CI for actual type checking |
| **SWC** | Transpilation | Mature alternative to esbuild; no type checking |

2026 recommendation: **Biome** for formatting + basic linting, **`tsc --noEmit`** in CI for full type checking. Keep ESLint only when you need its plugin ecosystem (e.g., `eslint-plugin-react`).

---

## Code Review Checklist (TypeScript-Specific)

See `quality-core.md` for the universal review checklist. TypeScript-specific additions:

- [ ] `strict: true` + the 2026 strict-baseline flags in tsconfig
- [ ] `noUncheckedIndexedAccess: true` explicitly enabled
- [ ] No `any` in exported signatures
- [ ] No `as X` assertions bypassing narrowing
- [ ] No non-null `!` without justification comment
- [ ] `catch (e)` narrows from `unknown`, not `any`
- [ ] Unions are discriminated; switch has `never` exhaustion check
- [ ] `satisfies` used for config objects
- [ ] `import type` syntax used for type-only imports (enforced by `verbatimModuleSyntax`)
- [ ] No TypeScript `enum` — use `const` union types instead
- [ ] `tsc --noEmit` passes; Biome/ESLint passes

---

## Sources

Authoritative references used in this rule:

- [TypeScript TSConfig Reference](https://www.typescriptlang.org/tsconfig/)
- [noUncheckedIndexedAccess GitHub issue #49169](https://github.com/microsoft/TypeScript/issues/49169)
- [Total TypeScript: Configuring TypeScript](https://www.totaltypescript.com/books/total-typescript-essentials/configuring-typescript)
- [2ality: satisfies operator](https://2ality.com/2025/02/satisfies-operator.html)
- [2ality: TypeScript enum patterns](https://2ality.com/2025/01/typescript-enum-patterns.html)
- [TypeScript 5.2 release notes: using declarations](https://www.typescriptlang.org/docs/handbook/release-notes/typescript-5-2.html)
- [`verbatimModuleSyntax` TSConfig option](https://www.typescriptlang.org/tsconfig/verbatimModuleSyntax.html)
