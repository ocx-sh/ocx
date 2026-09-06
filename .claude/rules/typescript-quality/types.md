---
title: Types and Escape Hatches
summary: Where TypeScript's checking gets switched off — assertions, `any`, ambient declarations, enums — and the checks that notice
---

# Types and Escape Hatches

Every place a TypeScript diff can silently stop being checked: an assertion, a
suppression comment, a hand-written predicate, an ambient declaration, a union
that grew an arm nothing handles. Compiler flags and `tsconfig` shape are not
here; neither is anything promise-typed.

Contents: [The Lint Gate](#the-lint-gate-pinned) · [Escape Hatches](#escape-hatches) ·
[`satisfies`, `as const`, and Annotations](#satisfies-as-const-and-annotations) ·
[Closed Sets and Exhaustiveness](#closed-sets-and-exhaustiveness) ·
[Ambient Declarations](#ambient-declarations) ·
[What Agents Get Wrong](#what-agents-get-wrong-here)

Severity maps onto the house tiers: MUST = Block, SHOULD = Warn.

## The Lint Gate (pinned)

Every rule below except TS-TYP-09 and TS-TYP-11 is enforced by one invocation:

```bash
npx eslint --max-warnings 0 .
```

against a flat config that carries typescript-eslint's `strictTypeChecked`
preset **and** `languageOptions.parserOptions.projectService: true`. The second
half is the load-bearing half: roughly fifty rules — the whole `no-unsafe-*`
family, `no-unnecessary-type-assertion`, `no-unnecessary-condition`,
`switch-exhaustiveness-check` — need a type checker and report *nothing*
without it. A config missing it produces a clean run that checked almost none
of what you think it checked.

Three rules this file leans on are not reliably in any preset and must be
selected by name: `@typescript-eslint/switch-exhaustiveness-check`,
`@typescript-eslint/method-signature-style` (`"property"` — method shorthand is
checked *bivariantly* by design, so `onEvent(e: T): void` accepts a handler the
property form correctly rejects), and `@typescript-eslint/ban-ts-comment` with
a `descriptionFormat`. Preset membership changes between majors: confirm with
`npx eslint --print-config src/index.ts` rather than assuming.

**Pin `typescript` to a version your linter supports.** TypeScript 7.0 (stable
2026-07-08) is a Go rewrite that shipped with no stable programmatic API until
7.1 — typescript-eslint, `ts-morph`, and anything else importing `typescript`
as a library can break on upgrade, and a linter that fails to build a program
tends to report zero findings rather than an error.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-TYP-01 | Wire typed linting before adopting any rule below, and prove it fires. | Without `projectService`, the rules that catch every escape hatch here are inert, and inert is indistinguishable from clean. | `rg -n --glob 'eslint.config.*' -e projectService -e 'parserOptions.project' .` — **no hit is the finding**, the one inverted row in this file. Go-red: paste `const s: unknown = 1; s.length;` into a source file and confirm `no-unsafe-member-access` errors. Silence means nothing below is enforced. | MUST |

## Escape Hatches

An assertion is not a check — it is you telling the compiler to stop checking.
Each row names a spelling that does that, and what to write instead.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-TYP-02 | Use `any` at exactly two sites: a genuinely-unbounded generic constraint (`(...args: any[]) => any`) and an `as any` inside a generic function body that a unit test covers. Every other occurrence is `unknown` plus a narrowing check. **Pinned default — an adopter may extend the list, but each site names which exception it claims.** | `any` is not one hole; every operation on an `any` returns `any`, so one untyped parameter erases checking across the whole call graph downstream of it. | `rg -n --glob '*.ts' --glob '*.tsx' -e ': any\b' -e '\bas any\b' -e '<any[>]' src` — a deliberate union over the three spellings. A hit with no adjacent `eslint-disable-next-line @typescript-eslint/no-explicit-any` naming its exception is the finding; empty output is the pass. | SHOULD |
| TS-TYP-03 | Never write `@ts-ignore` or `@ts-nocheck`. `@ts-expect-error` is allowed in test trees only, and always with a description on the same line. | `@ts-ignore` suppresses an error that may already have been fixed and keeps suppressing forever; `@ts-expect-error` at least fails loudly once the error is gone. | `rg -n -e '@ts-ignore' -e '@ts-nocheck' .` — any hit is the violation, empty output is the pass. Then `rg -n '@ts-expect-error\s*$' .` — a bare directive with no description is the finding. | MUST |
| TS-TYP-04 | Never assert the shape of data you did not construct — parse and validate it at the boundary instead. | `JSON.parse(x) as Config` compiles, produces no runtime check, and moves the failure to whatever reads a missing field three frames later. | `rg -n -e 'JSON\.parse\(.*\) as ' -e '\.json\(\) as ' src` — a deliberate union over the two spellings; every hit is the finding. Go-red: delete a required key from a fixture payload and confirm the boundary throws. A consumer failing downstream instead is the violation. | MUST |
| TS-TYP-05 | Treat a hand-written type predicate (`v is T`) as an assertion: delete it where the compiler now infers one, or cover its body with a test. | The body can lie — `typeof v === "object"` returning `v is Config` is an unchecked cast with a reassuring signature. TypeScript 5.5 (2024) infers predicates from a boolean return, so many hand-written ones are also redundant. | `rg -n --glob '*.ts' --glob '*.tsx' '\): \w+ is ' src` — every hit is a candidate; one whose body does not check each property the asserted type declares is the finding. | SHOULD |
| TS-TYP-06 | Never write `as unknown as T` at a call site. Where a third-party type has no test-double package, put the double-cast inside one named `fake<T>()` helper per faked interface, in the test tree. | The double-cast defeats every assignability check at once; inlined at each call site it becomes the most common escape hatch in a codebase and nothing counts it. | `rg -n 'as unknown as' .` — every hit outside the one helper module is the finding. A count above zero in a `src` tree (as opposed to a test tree) is a separate, worse finding. | SHOULD |
| TS-TYP-07 | Never widen `Object.keys` / `Object.entries` with an assertion to `keyof`. | Their return type is `string[]` deliberately: a value's static type does not rule out extra runtime keys, so `as (keyof T)[]` reintroduces exactly the unsoundness the language avoided, on your authority. Iterate the known key list, or keep `string` and narrow. | `rg -n -e 'Object\.keys\(.*\) as ' -e 'Object\.entries\(.*\) as ' src` — a deliberate union; every hit is the finding. | SHOULD |

## `satisfies`, `as const`, and Annotations

Three notations, three different inference outcomes, routinely conflated. An
**annotation** checks the literal and then discards its narrower inferred type.
`as const` freezes the literal types but checks nothing. `satisfies` (TS 4.9,
2022) checks against the type *and* keeps the inferred one — and, because the
value stays a fresh literal, keeps excess-property checking alive on a `const`
that a plain annotation-free assignment would have silently exempted.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-TYP-08 | Validate a configuration- or lookup-shaped literal with `satisfies`, not with a widening annotation. | The annotation throws away exactly the information the literal existed to carry: keys stop being a closed set, values widen to `string`, and every read back out needs an assertion to recover what was already known. | `rg -n -e 'const \w+: Record<' -e 'const \w+: \w+\[\] = \[' src` — a deliberate union; each hit whose keys or values are read back out is a candidate. Confirm by swapping the annotation for `satisfies` and re-running `tsc --noEmit`: still clean means the annotation was buying widening and nothing else. | SHOULD |

```ts
// wrong — routes.home.path is `string`; `keyof typeof routes` is `string`
const routes: Record<string, Route> = { home: { path: "/" } };
```

```ts
// right — validated against Route, and both literal types survive
const routes = { home: { path: "/" } } satisfies Record<string, Route>;
```

## Closed Sets and Exhaustiveness

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-TYP-09 | Model a closed set of values as a union of string literals, or an `as const` object plus `(typeof X)[keyof typeof X]`. Never `enum`, and never `const enum`. | `enum` is not erasable syntax: it emits a runtime object, so it hard-errors under Node's native type stripping and under `erasableSyntaxOnly`. A numeric `enum` additionally accepts any bare `number`. A `const enum` needs whole-program knowledge to inline and is rejected — or silently stripped into a `ReferenceError` — by every per-file transpiler (esbuild, swc, Vite, Bun). And JSON data satisfies a string union directly, where an `enum` demands an assertion. | `rg -n --glob '*.ts' --glob '*.tsx' '\benum ' .` — every hit is the finding; a match in prose or a string is not. On TypeScript ≥5.8 (2025), the authoritative check is `tsc --noEmit` with `"erasableSyntaxOnly": true`, which errors on `enum`, runtime `namespace`, and constructor parameter properties in one pass. | MUST |
| TS-TYP-10 | End every mapping from a closed union to a value with a `never`-typed fallthrough. Own one `assertNever` helper per workspace and call it from each. **Pinned — the helper's name is the adopter's to pick; that there is exactly one is not.** | Adding an arm to a union is the moment the check must fire. Without the `never` branch the compiler stays green, the new arm falls through to a default, and the wrong value ships. `switch-exhaustiveness-check` covers `switch` only — an `if`/`else` chain or a lookup object has no such rule. | The lint covers `switch` (needs TS-TYP-01). For everything else, go-red: add a member to the union and run `tsc --noEmit`. A build that stays green is the violation. Then `rg -n 'assertNever' src` — a codebase with discriminated unions and zero hits has no fallthrough anywhere. | MUST |

```ts
// wrong — a fourth status ships as `undefined`, silently, no compile error
const label = { queued: "…", running: "…", done: "…" }[status];
```

```ts
// right — the union widens, the switch stops compiling
switch (status) {
  case "queued": return "…";
  default: return assertNever(status); // status: never
}
```

## Ambient Declarations

An ambient declaration changes type-checking for code that never imports it.
That invisibility is the whole hazard: nothing at the use site says a global
was invented, and in a workspace sharing one `tsconfig` root, a declaration in
one package silently reaches its siblings.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-TYP-11 | Keep `declare global` and `declare module` in `.d.ts` files the `tsconfig` explicitly includes — never inside an implementation module. | Inside a `.ts` file the augmentation is load-bearing and invisible: it changes checking for the whole compilation unit, and no import anywhere records that it exists. | `rg -n --glob '*.ts' --glob '*.tsx' --glob '!*.d.ts' -e '^\s*declare global' -e '^\s*declare module' src` — a deliberate union; every hit is the finding, empty output is the pass. Asset-import shims (`declare module "*.css"`) are the same rule, not an exception — they belong in the `.d.ts` too. | SHOULD |
| TS-TYP-12 | Never write a bodyless `declare module "pkg";` to quiet an untyped dependency. | The shorthand form types every export of that package as `any`, which propagates through every call — a wider hole than the `any` TS-TYP-02 forbids, opened by a line that looks like configuration. Write the members you actually call, or install the `@types` package. | `rg -n -e "declare module '[^']+';" -e 'declare module "[^"]+";' .` — a deliberate union over the two quote styles; every hit is the finding. A `declare module "x" { … }` with a body does not match. | SHOULD |

## What Agents Get Wrong Here

1. Reaching for `as` the moment the compiler objects. The assertion is the
   model asking the checker to believe it, at the one moment the checker had
   found something. Fix the mismatch; assert only where you know a runtime
   invariant the type system cannot express, and say which one.
2. Reaching for `enum` for a closed set, because it is the construct other
   languages made habitual. It is the one TypeScript construct that does not
   erase, and the runtime rejects it.
3. Writing `const x: SomeType = { … }` for a config object. It reads as the
   careful spelling and is the one that throws information away.
4. Treating a green `tsc --noEmit` as coverage. Node, Bun and esbuild all
   strip types without checking them — "it ran" carries no type guarantee at
   all, and typed lint rules catch a class `tsc` never looks at.
5. Adding a union member and stopping when the build is green. Green means the
   exhaustiveness check is missing, not that every branch was updated.
6. Silencing a hand-written type predicate's failure by making the predicate
   broader, rather than narrowing at the boundary that produced the value.
7. Adding `as unknown as T` in a test because "it's only a test". It is a hole
   in exactly the code asserting the rest of the system is correct — and the
   spelling propagates: a codebase with one has dozens within a year.
