---
title: TypeScript Observability
summary: Which channel carries a diagnostic in each runtime, what a catch must keep, and what makes output depend on the machine that produced it
---

# Observability

What a run tells the person and the script reading it: the channel each
runtime actually delivers to, what survives a `catch` that logs, and what
silently varies between a laptop and a CI runner. Read this before adding a
log call, a `catch` that does not rethrow, a comparator, or a formatted date.

It does not own the exit code or the stdout-versus-stderr payload contract
(`TS-CLI`), error identity across a worker or webview boundary (`TS-ERR`),
secret masking (`TS-SEC`), or the lint wiring that runs any of these
commands in CI (`TS-GATE`).

Contents: [The Channel Per Runtime](#the-channel-per-runtime) ·
[What a Catch Logs](#what-a-catch-logs) ·
[Locale, Collation, Ordering](#locale-collation-ordering) ·
[Clocks and Date Construction](#clocks-and-date-construction) ·
[What Agents Get Wrong](#what-agents-get-wrong-here) · [Sources](#sources)

Two layers, and the difference matters when adopting this elsewhere:

- **The mechanism** — the channel table, passing the error value instead of a
  string built from it, an explicit locale, `performance.now()` for a
  duration — is general TypeScript practice and binds everywhere.
- **The pinned default** is `TS-OBS-02`: no logging dependency. It is a
  decision, not a derivation, and an adopter running a long-lived service
  that ships to an aggregator should override it and say so.

Severity maps onto the house tiers: MUST = Block, SHOULD = Warn,
CONSIDER = Suggest.

## The Channel Per Runtime

`console.*` is not a channel — it is a channel in exactly one shape and a
dead end in the rest. Pick the row before writing the call.

| Runtime | The channel | What `console.*` does there instead |
|---|---|---|
| CLI or script | `console.error` for every diagnostic; `console.log` reserved for the payload | Correct — this is the one shape where `console` *is* the channel |
| Editor extension (VS Code and its forks) | an output channel the extension creates and can tell the user to open | Goes to the extension-host log, which no bug reporter is ever asked to open |
| CI action (GitHub Actions toolkit) | `core.info` / `core.warning` / `core.error` / `core.setFailed` | Lands in the raw step log with no annotation, no grouping, and no step failure |
| Browser app | one app-level error surface — framework error boundary or a global handler — plus `console.error` behind it | Invisible to everyone but a developer who already had devtools open on the failing session |
| Worker or webview | the parent's channel, reached by an explicit message | A separate console in a separate context that nothing aggregates |

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-OBS-01 | Every diagnostic goes through the channel its runtime's row names; extension, action, worker and webview source contains no `console.*` at all. | A log line written to a channel nobody reads is indistinguishable from no logging, and the absence only shows up when a user reports a bug nobody can reproduce. | `rg -n -t ts 'console\.' <non-cli-src>` — every hit is a violation, zero output is expected and clean. Enforce it standing with ESLint `no-console` scoped to those directories, or Biome `suspicious/noConsole`. | MUST |
| TS-OBS-02 | Do not add a logging library to a CLI, an editor extension, a CI action, or a browser app. The runtime channel plus the error value is the whole requirement; a long-running service shipping to an aggregator is the named exception, declared in the commit body. | A dependency added for "structured logging" in a process with one output channel adds a configuration surface, a bundle, and a second place log level is decided, and removes nothing. | `rg -n -e pino -e winston -e bunyan -e 'consola' <package.json>` — each hit needs the service exception stated; zero output is expected and clean. | SHOULD |

## What a Catch Logs

A `catch` that rethrows preserves everything by construction. A **terminal**
catch — one that logs and lets execution continue — is the only place a stack
can be destroyed, and it is destroyed at the log call, not at the throw.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-OBS-03 | A terminal catch passes the error **value** to the channel, never a string built from `.message` or `String(err)`. Where the channel only accepts a string, pass `inspect(err)` (Node) or `err.stack`. The named exception is user-facing text — a toast, a modal, a CLI validation line — which is deliberately message-only, and which then sends the full value to the diagnostic channel in the same block. | `console.error(err)` and `util.inspect(err)` print the entire nested `cause` chain with no depth cap; `err.message` prints one sentence. The bug report pasted out of that log has no trace and no wrapped cause, and nothing downstream recovers them. | `rg -n -t ts -e 'instanceof Error \? [^:]*\.message' -e 'console\.\w+\([^)]*\.message\)' -e '\(String\(e' <src>` — each hit is a log site to read; a hit whose expression does not also name `.stack` is the violation. Watched red on the wrong form below and silent on a `(err.stack ?? err.message)` rewrite of it. | MUST |
| TS-OBS-04 | Never `JSON.stringify` an error, or an object holding one, for a log line — pass `Object.getOwnPropertyNames(err)` as the replacer, or flatten to `{ name, message, stack, cause }` by hand. | `message`, `stack`, `name` and `cause` are all non-enumerable own properties, so `JSON.stringify(err)` is `"{}"` — the structured log line ships an empty object, with no error and no warning. The replacer array is re-applied at every nesting level, so it recurses the whole cause chain. | `rg -n -t ts 'JSON\.stringify\([a-zA-Z{ ]*[eE]rr\w*\s*\}?\s*\)' <src>` — each hit is a violation, zero output is expected and clean. Watched red on `JSON.stringify(err)` and on `JSON.stringify({ error })`, and silent on the replacer form. | MUST |

```ts
// Wrong — the stack and every wrapped cause are gone before the log call runs.
catch (err) { out.appendLine(`refresh failed: ${err instanceof Error ? err.message : String(err)}`); }

// Right — the channel takes a string, so render the value, chain included.
catch (err) { out.appendLine(`refresh failed: ${inspect(err)}`); }

// Right — the channel takes a value, so hand it over untouched.
catch (err) { console.error("refresh failed", err); }
```

## Locale, Collation, Ordering

One command finds the whole section's locale half:
`rg -n -t ts -e '\.toLocale\w*\(\)' -e '\.localeCompare\([^,)]*\)' <src>`.
Both patterns match only the argument-less call, so every hit is a real
finding and zero output is expected and clean.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-OBS-05 | Pass an explicit BCP-47 tag to every `toLocaleString`, `toLocaleDateString`, `toLocaleTimeString` and `localeCompare` call. | With no argument both fall back to a locale derived from OS region settings and how ICU was built into the runtime, so the same code formats and orders differently on a laptop, a CI runner, and a slim container — surfacing as a flaky snapshot test rather than as an error. | The section command above; each hit is the violation. | SHOULD |
| TS-OBS-06 | A comparator whose output is a contract — generated files, a published ordering, a version or identifier sort another implementation must agree with — compares by code unit: `a < b ? -1 : a > b ? 1 : 0`. Never `localeCompare` there, tag or no tag. | Locale collation is not code-unit order. A comparator documented as mirroring another language's ordering agrees with it on plain ASCII and diverges on punctuation-heavy prerelease tags, so the wrong version is published as latest with no crash and no diff. | `rg -n -t ts 'localeCompare' <src>` and read each hit: one inside a comparator that feeds generated output, a version ordering, or any sort a second implementation must reproduce is the violation. Display ordering is not. | MUST |
| TS-OBS-07 | Every `.sort()` on anything but a `string[]` passes a comparator. | The default comparator coerces to string and compares UTF-16 code units, so `[80, 9].sort()` returns `[80, 9]` — deterministic, silent, and the wrong order. | `@typescript-eslint/require-array-sort-compare`, which is type-aware and needs `projectService` wired before it sees anything. Without it: `rg -n -t ts '\.sort\(\)' <src>` and read each hit; a receiver that is not a `string[]` is the violation. | MUST |

## Clocks and Date Construction

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-OBS-08 | Measure a duration with `performance.now()`. `Date.now()` is for a calendar delta a human reads, and for nothing else. | `Date.now()` follows the wall clock, so an NTP step or a DST change during the measured window makes the logged duration negative or an hour long for work that took a millisecond. | `rg -n -t ts 'Date\.now\(\) -' <src>` — each hit is a duration computed from the wall clock and is the violation unless it is an explicit calendar delta; zero output is expected and clean. Then read any `Date.now()` stored in a variable and subtracted later. | SHOULD |
| TS-OBS-09 | Never construct a `Date` from a bare `YYYY-MM-DD` string when the caller means a local day — append `T00:00:00` for local, or `Z` for UTC, and say which. | ECMA-262 parses a date-only string as **UTC** midnight and the same string carrying a time but no offset as **local** midnight. One character flips the instant by up to a day, so a date rendered back to the user is off by one everywhere west of UTC. | `rg -n -t ts 'new Date\("' <src>` — a string-literal argument matching `YYYY-MM-DD` with no offset is the violation. Then read the variable-fed `new Date(` sites for the shape of the string they receive. | SHOULD |

## What Agents Get Wrong Here

1. Writes `console.log` in an editor extension or a CI action, because it is
   the reflex log call in every language and it produces no error — it just
   delivers to a place nobody looks (TS-OBS-01).
2. Reaches for a logging library the moment "structured logging" is asked
   for, in a process with exactly one output channel (TS-OBS-02).
3. Writes `err instanceof Error ? err.message : String(err)` in a terminal
   catch. It is the idiomatic way to satisfy `unknown` catch bindings, and it
   throws away the stack and the whole cause chain on the way (TS-OBS-03).
4. Assumes `JSON.stringify(err)` serializes the error. It returns `"{}"`,
   silently, and the log line looks structured and correct (TS-OBS-04).
5. Calls `toLocaleDateString()` with no locale to render a timestamp,
   producing output that differs between the machine that wrote the test
   snapshot and the machine that runs it (TS-OBS-05).
6. Uses `localeCompare` as the default string comparator everywhere, having
   learned it is the "correct" one — including inside a comparator whose
   ordering another implementation has to reproduce byte for byte
   (TS-OBS-06).
7. Writes `const start = Date.now()` to time an operation, because it is the
   shorter call and it works on every run that does not cross a clock
   adjustment (TS-OBS-08).
8. Writes `new Date("2026-01-01")` for a local calendar day and renders it
   back a day early for every reader west of UTC (TS-OBS-09).

## Sources

- [MDN — `Error.cause`](https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/Error/cause) — the options-object constructor shape, and that `cause` is a non-enumerable own property
- [Node.js — Errors](https://nodejs.org/api/errors.html) and [`util.inspect`](https://nodejs.org/api/util.html#utilinspectobject-options) — what a terminal log call prints when handed the error value itself
- [MDN — `JSON.stringify`](https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/JSON/stringify) — enumerable-own-property serialization, and the replacer-array form
- [MDN — `Array.prototype.sort`](https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/Array/sort) — the default string-coercing comparator, spelled out
- [MDN — `Date`](https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/Date) — the date-only-is-UTC, date-and-time-is-local parsing split
- [ECMA-402 / Mozilla bug 769871](https://bugzilla.mozilla.org/show_bug.cgi?id=769871) — direct evidence that the default locale is environment-derived
- [typescript-eslint — `require-array-sort-compare`](https://typescript-eslint.io/rules/require-array-sort-compare/) — the type-aware check for TS-OBS-07, and why it needs project service
- [VS Code — output channels](https://code.visualstudio.com/api/references/vscode-api#OutputChannel) and [GitHub Actions toolkit — `core`](https://github.com/actions/toolkit/tree/main/packages/core) — the two non-`console` channels named in the table
