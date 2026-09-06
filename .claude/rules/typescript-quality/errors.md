---
title: Errors and Boundaries
summary: The TS-ERR family — what a throw carries, what a catch does, one classifier, and where `unknown` stops being unknown
---

# Errors and Boundaries

Owns the `TS-ERR` family: `Error.cause`, custom `Error` classes, catch
discipline, one classifier function, and the single legal crossing from
`unknown` into a typed value. It does not own exit-code values, lint-tier
wiring, async cancellation, or the browser error boundary (`TS-WEB-01`).

Contents: [Pinned Decisions](#pinned-decisions) · [Cause and Construction](#cause-and-construction) ·
[Terminal Catches](#terminal-catches) · [Identity and Classification](#identity-and-classification) ·
[The Untrusted-to-Typed Crossing](#the-untrusted-to-typed-crossing) ·
[Version-Pinned Facts](#version-pinned-facts) · [What Agents Get Wrong](#what-agents-get-wrong-here)

Every command below takes an explicit path operand and assumes ripgrep, which
respects `.gitignore`, so build output is excluded without a flag. Replace
`src` with your source root. Severity maps onto the house tiers: MUST = Block,
SHOULD = Warn, CONSIDER = Suggest.

## Pinned Decisions

Agreed positions, not derivable ones. Each is a **default an adopter may
override** — override in writing, in the repo, not by drift.

- **A typed `Error` subclass is earned, not default.** A shape with exactly one
  outward value per run — a CLI's exit code, a CI action's failure message —
  earns a typed taxonomy. A command handler, a message handler, and a
  fetch-error toast have no such value; porting a taxonomy there builds
  vocabulary with nowhere to plug in. Those get bare `Error` plus `cause`,
  funnelled through one shared boundary function.
- **Enforcement here is grep and reading, never a type-aware lint rule.**
  `@typescript-eslint/only-throw-error` is the obvious tool and it needs type
  information; a rule set whose enforcement is gated on an unwired lint tier
  enforces nothing. Turn type-aware linting on if you have it — it replaces no
  row below.
- **`Error.isError()` is not adopted** (as of 2026-08-29): MDN marks it
  "Limited availability" and Node's own errors page does not document it.
  `instanceof Error` stays. Revisit when it reaches Baseline.
- **Adopting `cause` is a zero-config change.** TypeScript has typed the
  `cause` option since **4.6**, gated on `--target es2022` or `--lib es2022`.
  If your `target` is already ES2022 or later, nothing else is needed.

## Cause and Construction

Caught by reading every `catch` block and every `extends Error` constructor.
The whole group is one pass over two greps.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-ERR-01 | When a `catch` block throws a new error, pass the caught binding as the `cause` option: `new XError(msg, { cause: err })`. Options object, never a positional second argument. | The caught value is the only place the original diagnostic exists; once the block returns it is gone, and no amount of downstream logging recovers it. | `rg -n -A8 --glob '*.ts' --glob '*.tsx' 'catch\s*\(' src` — read each block containing a `throw new` and confirm `cause:` on the constructing call. Then, for the silently-dropped positional form, `rg -n --glob '*.ts' 'new [A-Za-z]*Error\([^)]*,\s*[^\s{]' src`: a hit on bare `Error`/`TypeError` is the violation; a hit on a class whose signature genuinely takes that positional parameter is not. | MUST |
| TS-ERR-02 | Every custom `Error` subclass accepts `options?: ErrorOptions` and forwards it: `super(message, options)`. | A constructor that swallows the second argument permanently blocks every caller from attaching a cause — TS-ERR-01 becomes unimplementable downstream, and the block is invisible at the call site. | `rg -nU --pcre2 --glob '*.ts' 'extends Error[\s\S]{0,300}?super\([^,)]*\)' src` — every hit is a `super()` taking fewer than two arguments within a class-sized window of `extends Error`. Empty output is the pass. | MUST |
| TS-ERR-03 | Do not write `Object.setPrototypeOf(this, X.prototype)` in an `Error` subclass constructor. | A TypeScript 2.1-era workaround for `--target es5`'s broken `new.target`. At ES2015 and later it is dead code an agent adds from training habit. | `rg -n --glob '*.ts' 'setPrototypeOf\(this' src` — must be empty. The one exception is a `tsconfig.json` whose `target` is below `ES2015`; confirm that before deleting a hit. | MUST |

```ts
// wrong — the second argument is treated as nothing; the cause is dropped silently
throw new ConfigError(`invalid JSON in ${path}`, err);
```

```ts
// right — options object, so the SyntaxError's position survives the rethrow
throw new ConfigError(`invalid JSON in ${path}`, { cause: err });
```

A cause is mandatory **only** where a `catch` block is in scope. A `throw` from
a direct validation check has nothing to attach and must not invent one. A
`catch` that swallows and returns throws nothing, so TS-ERR-01 does not bind
it — the crossing rules below still do.

## Terminal Catches

A terminal catch is one that logs and does not rethrow. Caught by grepping the
two spellings an agent reaches for.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-ERR-04 | Never `JSON.stringify(err)`, or stringify an object containing one, for a log line. Pass `Object.getOwnPropertyNames(err)` as the replacer, or flatten by hand. | `message`, `stack`, `name` and `cause` are all non-enumerable own properties, so a bare stringify yields `{}` — no error, no warning, no log. The property-name replacer is re-applied at every nesting level, so it recurses the whole cause chain. | Union of two patterns, both intended: `rg -n --glob '*.ts' -e 'JSON\.stringify\([^)]*\berr' -e 'JSON\.stringify\([^)]*\berror' src`. Each hit must pass a replacer or provably not be an `Error`. | MUST |
| TS-ERR-05 | A terminal catch logs `.stack` or the serialised cause chain, never `.message` or `String(err)` alone. Where the destination is end-user text, log the full chain to a separate diagnostic channel as well. | `.message` alone discards the only trace a bug report could carry, at zero cost to fix — and the line reads as handled, so nothing else flags it. | `rg -n -A6 --glob '*.ts' --glob '*.tsx' -e 'instanceof Error \? ' -e 'String\(err' src` — union intended. For each hit, confirm `.stack` appears somewhere in the same block. `console.error(err)` and `util.inspect(err)` already print the entire cause chain, nested, with no depth cap, so passing the error itself satisfies this. | MUST |

## Identity and Classification

Caught by one grep for `instanceof` over error classes, read for duplication,
plus a grep for the two forbidden identity spellings.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-ERR-06 | Map errors to an outward value through **one shared function**, called from every site that needs it. Never re-inline the same `instanceof` ladder in a second file. One call site and N call sites are both compliant; N copies of the ladder is not. | Independent copies drift the moment one gains a fourth branch and the others do not, and nothing reports the divergence. | `rg -n --glob '*.ts' 'instanceof [A-Z][A-Za-z]*Error' src` — group the hits by class name. The same class mapped to the same kind of outward value in more than one non-test file is the violation. | MUST |
| TS-ERR-07 | Match error identity with `instanceof` when the class is statically imported in the same module graph. Drop to `err.name === "Literal"` only at a deliberate dynamic-`import()` boundary, with a comment saying so. Never match `err.constructor.name`. | A minifier renames `class ConfigError` to `class r`, so `constructor.name` becomes `"r"` in production while a `this.name = "ConfigError"` string literal survives untouched. The two are indistinguishable in an unminified dev run. | `rg -n --glob '*.ts' '\.constructor\.name' src` — must be empty in any error-matching context. Then `rg -n -B2 --glob '*.ts' '\.name === "' src`: every name-match site needs an adjacent comment naming the lazy-load reason. | MUST |
| TS-ERR-08 | An `Error` crossing a worker `postMessage`, a webview `postMessage`, or any structured-clone or JSON channel is flattened on the sending side to `{ name, message, stack, cause? }`, with the cause recursed by hand. Never `instanceof`-check or read a custom field on the receiving side. | The structured-clone algorithm recognises exactly seven error names (`Error`, `EvalError`, `RangeError`, `ReferenceError`, `SyntaxError`, `TypeError`, `URIError`) and coerces every other name — `AggregateError` included — to plain `"Error"`; custom fields and `.errors` are dropped. The received value still *prints* correctly, because the original class name is baked into the cloned `.stack` string, so the bug never appears in a log. A JSON-only channel degrades it further, to `{}` (TS-ERR-04). | `rg -n --glob '*.ts' 'postMessage\(' src` — for each payload that can carry an `Error`, confirm a flatten call precedes it. On the receiving side, any hit from TS-ERR-06's grep whose operand arrived over such a channel is the violation. | MUST |
| TS-ERR-09 | Do not introduce a new `*Error` subclass unless at least two call sites branch on its identity, or it carries a field a caller reads. Otherwise throw `Error` with a `cause`. | Copying a taxonomy into a shape with no outward value to map to is unrequested vocabulary that every future branch has to keep consistent. | `rg -n --glob '*.ts' 'class [A-Z][A-Za-z]*Error extends' src` for the inventory, then TS-ERR-06's `instanceof` grep for the consumers. A class with fewer than two non-test consumers and no field read anywhere is not earned. | SHOULD |

## The Untrusted-to-Typed Crossing

`unknown` becomes a typed value at exactly one kind of statement: **a call that
runs code**. Three constructs qualify — a schema library's `.safeParse()`, a
compiled JSON-Schema `validate()`, and a hand-written `x is T` predicate.
Everything else that looks like a crossing is erased by the compiler before
anything runs. Caught by grepping every parse and every `.json()`.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-ERR-10 | A value crossing from `unknown` or `any` into a typed binding passes through `.safeParse()`, a compiled `validate()`, or a hand-written `x is T` predicate. Never an `as T` cast, never a typed `const` on an untyped right-hand side, never a typed callback parameter. | The compiler emits no code for any of the three forbidden forms, so a parseable-but-wrong payload arrives fully typed and flows into state unchallenged. | Explicit flavour: `rg -n --glob '*.ts' --glob '*.tsx' --glob '*.vue' 'JSON\.parse\([^;]*\) as [A-Z]' src` and `rg -n --glob '*.ts' --glob '*.tsx' --glob '*.vue' '\.json\(\) as ' src`. Invisible flavour: `rg -n --glob '*.ts' --glob '*.tsx' --glob '*.vue' ': [A-Z][A-Za-z]* = JSON\.parse' src` and `rg -n --glob '*.ts' --glob '*.tsx' --glob '*.vue' ': [A-Z][A-Za-z]* = await .*\.json\(\)' src`. Any hit with no guard call within three lines is the violation. | MUST |
| TS-ERR-11 | Bind `Response.json()` to `unknown` and hand it to a guard on the next line. Never annotate the binding with an interface, never cast it. | `.json()` returns `Promise<any>` by TypeScript's own lib types, so `const data: T = await resp.json()` has exactly the runtime strength of `as T` while reading as already-validated. This is the single highest-frequency form of the defect. | `rg -n -A3 --glob '*.ts' --glob '*.tsx' --glob '*.vue' '\.json\(\)' src` — a candidate list, not a finding list. The assigning line must be `unknown` or unannotated, with a guard call in the printed context. | MUST |
| TS-ERR-12 | A hand-written `x is T` predicate checks every field of `T` it claims to guarantee, including the element types inside a `Record` or array — not only the container. | A predicate that checks a discriminant and trusts the rest is only as strong as its checked subset, while reading as exhaustive validation to every later caller. | `rg -n --glob '*.ts' '\): [A-Za-z_]+ is [A-Z]' src` lists every predicate. For each, diff the fields checked against the type's member list; any member with no `typeof`, `instanceof`, or nested-guard check fails. | MUST |
| TS-ERR-13 | A parse function returns the schema-derived type (`z.infer` / `z.output<typeof schema>`), never a re-cast of `result.data` to a separately hand-maintained interface. | Two independently authored shapes for the same data diverge silently; structural typing will not catch a renamed field across an `as`. | `rg -n --glob '*.ts' '\.data as ' src` — every hit is the violation. Then `rg -n --glob '*.ts' 'safeParse\(' src` and confirm each enclosing function's return type names the schema's own derived type. | SHOULD |
| TS-ERR-14 | A message handler (`onDidReceiveMessage`, `addEventListener('message')`) narrows its payload with a runtime discriminant check before switching on it. A parameter type annotation is not validation. | Both ends of a message channel accept whatever the other side sends; the annotation compiles clean and is textually indistinguishable from a real guard, so both ends can be wrong at once. | `rg -n -A6 --glob '*.ts' -e 'onDidReceiveMessage\(' -e 'addEventListener\(.message.' src` — union intended. The handler's first statements must contain a `typeof` or predicate check on the discriminant field. | MUST |

```ts
// wrong — no `as` keyword appears anywhere, and zero code runs
const data: CatalogData = await resp.json();
```

```ts
// right — the cast, if any, happens after something has rejected bad data
const raw: unknown = await resp.json();
if (!isCatalogData(raw)) throw new Error("catalog response failed validation");
```

## Version-Pinned Facts

Two rules whose whole content is a fact that produces dead or broken code when
an agent guesses it. Caught by one grep each.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-ERR-16 | Do not write a branch expecting an `AggregateError` from `Promise.all`. To report every failure, use `Promise.allSettled` and construct the `AggregateError` explicitly. | `Promise.all` rejects with only the first reason and drops the siblings. `AggregateError` comes from `Promise.any`, and only when *every* input rejects — the opposite condition. The naming association is plausible and the resulting branch is unreachable, so no test catches it. | `rg -n -B10 --glob '*.ts' 'instanceof AggregateError' src` — any hit preceded by a `Promise.all` is dead code. | SHOULD |
| TS-ERR-17 | Before bumping a `zod` major, grep for `.errors` on a `ZodError` or `safeParse` result. Zod 4 removed it in favour of `.issues` with no compatibility alias (checked 2026-08-29, binds Zod 4.x). | A Zod-3-trained model writes `.errors` into a Zod-4 repo and a Zod-4-trained one writes `.issues` into a Zod-3 repo; one line blocks an otherwise clean bump in either direction. | `rg -n --glob '*.ts' '\.error\.errors' src`, cross-checked against the `zod` major in `package.json`. | CONSIDER |

`TS-ERR-15` is retired and is not reused: its content — "confirm a validator has
a non-test import before trusting the data" — produces the same diff as
TS-ERR-10, which guards the crossing regardless of what `package.json` claims.

## What Agents Get Wrong Here

Ranked by how often it bites.

1. `const x: T = await resp.json()` and `JSON.parse(raw) as T`. The shortest
   form that satisfies `tsc`, which is exactly what an agent optimises for, and
   nothing in the syntax separates it from a cast of already-checked data.
2. A try/catch that logs `.message` and continues. It compiles, runs, looks
   handled, and is undebuggable.
3. Annotating a message-handler parameter and considering the boundary done.
4. `new Error(msg, err)` instead of `new Error(msg, { cause: err })` — a
   plausible-looking two-arg constructor; the cause is dropped with no warning.
5. `Object.setPrototypeOf(this, X.prototype)` in every custom subclass. Pure
   ES5-era residue, added to every class an agent writes.
6. `JSON.stringify(err)` for a "structured log". Silently yields `{}`.
7. `err.constructor.name` for error matching. Correct in an unminified dev run,
   a live production bug wherever the build minifies.
8. `instanceof CustomError` on a value that arrived over `postMessage`. It
   prints correctly and is `false`.
9. `static getDerivedStateFromError` written onto a function component. Every
   other React lifecycle concept has a hook equivalent; this one does not.
