---
title: TypeScript CLI Contract
summary: The pinned exit-code table, how a status is chosen and set in Node, and the stdout/stderr split a caller parses against
---

# TypeScript CLI Contract

This file owns `TS-CLI`: the exit-code table, how a status travels from the
code that decides it to the process that reports it, and which stream every
byte lands on. It does **not** own error taxonomy — what an error class
carries and how errors map to a value is `TS-ERR`; promise-rejection
semantics are `TS-ASYNC`. Read this before touching anything that can end a
process or write to stdout.

Contents: [The Exit-Code Table](#the-exit-code-table-pinned) ·
[Choosing and Setting a Status](#choosing-and-setting-a-status) ·
[Streams and Machine Output](#streams-and-machine-output) ·
[What Agents Get Wrong Here](#what-agents-get-wrong-here) ·
[Sources](#sources)

Two layers, and the difference decides what you may change:

- **The mechanism** — one named code object, one classifier, one
  `process.exitCode` assignment at the entrypoint, stdout for the result and
  stderr for everything else — is general Node CLI practice and is not
  negotiable.
- **The table** — the specific numbers — is a **pinned decision**, and an
  adopter's default rather than a law. Its value is that it is *agreed*: two
  tools in one pipeline classify a failure the same way. Override it once, in
  your own code module, and never per-command. A number invented at a call
  site is the failure this file exists to stop.

Severity maps onto the house tiers: MUST = Block, SHOULD = Warn,
CONSIDER = Suggest.

## The Exit-Code Table (pinned)

| Code | Name | Meaning |
|---|---|---|
| 0 | `ok` | Success. **A gate command overloads this as an authorization — see below** |
| 1 | `failure` | General failure: something went wrong that no named category covers |
| 64 | `usage` | Bad invocation — a bad flag, a missing subcommand, mutually exclusive options |
| 65 | `data` | Input was reachable and readable but malformed or invalid. A human decides |
| 69 | `unavailable` | A dependency the tool needs is not there: an unreachable registry, a bound port, a missing build output |
| 126, 127 | *(not yours)* | Shell-level: found-but-not-executable, and command-not-found |
| 128+N | *(not yours)* | Killed by signal N — `130` is SIGINT, `141` is SIGPIPE, `143` is SIGTERM |

Names are `sysexits.h`-derived, and deliberately only the subset a program
actually controls. Nothing in `2`–`63`, `66`–`68`, `70`–`125` is claimed; a
new category updates this table first, in the same change. Never assign a
status at or above `126`: the shell reserves `126`/`127`, `128+N` is the
signal range, and Node truncates to the low 8 bits — `process.exitCode = 256`
exits **0** (measured, Node 24.14.0, 2026-08-29).

**The one deliberate overload.** For a command whose status a CI gate reads
as permission to proceed — auto-merge, deploy, promote — `0` no longer means
"nothing to report", it means "I judged this and it passes". That makes every
path which can end the process *before your judgement runs* a security
boundary, not a convenience: see TS-CLI-05.

## Choosing and Setting a Status

Every rule below is caught by one command over the CLI source tree; run it
once and read all four results:

```bash
rg -n --glob '*.ts' -e 'process\.exit\(' -e 'process\.exitCode' \
  -e 'exitOverride' -e 'CommanderError' src/
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-CLI-01 | Every status comes from one named, exported code object in a single module (`export const EXIT = { ok: 0, failure: 1, usage: 64, data: 65, unavailable: 69 } as const`). A numeric literal never appears at an exit site. **Pinned decision — an adopter may renumber this table, but only here, and only once.** | An integer chosen at a call site is a private convention that a caller already scripting against the shared set reads as "some kind of failure", silently and forever. | From the sweep above, any `process.exitCode = <digit>` or `process.exit(<digit>)` is the finding. Then `rg -n -A8 --glob '*.ts' 'EXIT = \{' src/` and check the members against the table. A test must assert each constant's **numeric value** (`expect(EXIT.data).toBe(65)`), not just its presence — otherwise a rename renumbers the wire contract with a green suite. | MUST |
| TS-CLI-02 | Never call `process.exit()`. Assign `process.exitCode` and let Node exit when the event loop drains. | `process.exit()` terminates before the stdout pipe buffer flushes. Measured on Node 24.14.0 (2026-08-29): 200,000 written lines, piped to a reader, arrive as **12,742** — 94% of the output silently lost, and only when piped, so it never reproduces in a terminal. | The sweep must return zero `process.exit(` hits. **One exception, and only one:** the EPIPE guard of TS-CLI-08, where the destination is already gone and there is nothing left to flush. | MUST |
| TS-CLI-03 | `process.exitCode` is assigned in exactly one place — the bin entrypoint, from the value its `run(argv)` returned. Every other layer *returns* a code or throws a typed error; none of them touch `process`. | Assigning `process.exitCode` does not stop execution. A branch that sets `usage` and forgets to `return` runs on into the success path, which overwrites the status with `0` — after the error message has already printed, which is what carries it through review. | The sweep must show exactly one `process.exitCode` hit, in the entrypoint. Two or more is the finding. Then `rg -n --glob '*.ts' 'process\.' src/commands/ src/lib/` (your non-entrypoint dirs) — a status touched below the entrypoint is the finding. | MUST |
| TS-CLI-04 | Stop the argument parser from terminating the process itself, then map its error onto your own table. With `commander`, that is `.exitOverride()` plus a `CommanderError` branch; with a hand-rolled parser, return a code instead of exiting. | Measured, commander 15.0.0 (2026-08-29): without `.exitOverride()`, `parse()` on `--version` **never returns** — the line after it does not run, and the process exits `0` through a path your code cannot see. With it, a `CommanderError` is thrown and control comes back. Its own `exitCode` is `0` for `--help`/`--version` and `1` for a parse error — never `64`, so an unremapped parse failure lands on your general-failure code and is indistinguishable from a real one. | The sweep must show `exitOverride` present whenever `commander` is a dependency, and a `CommanderError` branch that remaps `err.exitCode === 0 ? EXIT.ok : EXIT.usage`. Missing either is the finding. | MUST |
| TS-CLI-05 | For any command whose exit status is read as an authorization, force a non-zero code on every path that can end the process before your own judgement runs — `-h`, `--help`, `-V`, `--version`, and parse errors — and keep `0` reachable from exactly one explicit success line. | This is a live exploit class, not a hypothetical: where CI appends filenames to argv, a contributor adding a file named `-h` makes the gate print help and exit `0`, which the workflow reads as "approved". The parser short-circuits before any of your code runs, so no amount of care inside the command prevents it. | Runnable, and it fails loudly: `for f in -h --help -V --version; do node dist/cli/index.js <gate-cmd> "$f" >/dev/null 2>&1; echo "$f -> $?"; done` — every line must print a non-zero status. Any `-> 0` is the finding. Ship this as a test, not a one-off. | MUST |

The shape that satisfies 01–04 at once:

```ts
// bin entrypoint — the only file that knows `process` exists
process.exitCode = await run(process.argv);

// run(): one parse, one catch, one classifier
export async function run(argv: string[]): Promise<ExitCode> {
  const program = buildProgram().exitOverride(); // never exits for us
  try {
    return await program.parseAsync(argv);
  } catch (err) {
    return classify(err, { gate });  // the only place a code is chosen
  }
}
```

```ts
// WRONG — prints the error, then reports success
if (!isValid(cfg)) { process.exitCode = EXIT.data; console.error("bad config"); }
return EXIT.ok;

// RIGHT
if (!isValid(cfg)) { process.stderr.write("bad config\n"); return EXIT.data; }
```

## Streams and Machine Output

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| TS-CLI-06 | stdout carries the tool's result — the thing a caller parses, greps, or redirects to a file. Every message *about* the run — progress, status, warnings, prompts, errors — goes to stderr, unconditionally. | `console.log`→stdout and `console.error`→stderr is Node's default routing, not a decision. A CLI that reaches for `console.*` inherits a split that happens to be right for errors and is wrong for every progress line, and nothing reports the difference. | `rg -n --glob '*.ts' -e 'console\.log\(' -e 'process\.stdout\.write\(' src/` — read every hit and answer one question per line: *is this line the tool's result?* Anything else is the finding. Empty output from a CLI that emits a payload at all is also the finding: it means the payload is going somewhere else. | MUST |
| TS-CLI-07 | Where a machine-output flag exists (`--json` and friends), stdout under it is exactly one parseable document: no banner, no progress, no trailing "done", no partial write on the error path. Diagnostics still go to stderr, and the exit code still carries the verdict. | Output that is correct to read and unparseable to a caller is the whole failure mode — the caller's `JSON.parse` throws on text that looked fine in the terminal. | `node dist/cli/index.js <cmd> --json \| node -e 'JSON.parse(require("fs").readFileSync(0,"utf8"))'` — a non-zero status from the second process is the finding. Run it for the success path **and** a failure path, and with any verbosity flag also set. | MUST |
| TS-CLI-08 | A tool whose stdout can exceed one pipe buffer attaches an EPIPE guard to `process.stdout` before its first write. | Measured, Node 24.14.0 (2026-08-29): piping an unguarded writer into `head -1` raises an unhandled `'error'` event — a full stack trace on stderr and a writer status of `1`, which a caller reads as a real failure. It never reproduces unpiped, so it survives every manual test. | In bash: `node dist/cli/index.js <cmd producing >64 KiB> 2>err.log \| head -1 >/dev/null; echo "${PIPESTATUS[0]}"; cat err.log` — any `EPIPE` or `Unhandled 'error' event` text is the finding. **Empty `err.log` is the pass**, and the writer's status is then `0`. | MUST |
| TS-CLI-09 | Add a machine-output mode to any command whose output a pipeline consumes — most of all a gate command, whose reasons a CI job wants to render. Do not add one to a command only humans run. | A gate that reports "manual review required" as prose forces the workflow to scrape English; the next wording change breaks it with no failing test anywhere. | `rg -n --glob '*.ts' -e "'--json'" -e '"--json"' src/` — zero hits in a CLI with a CI-consumed subcommand is the finding. This one is a design decision the adopter owns; record the answer either way. | CONSIDER |

## What Agents Get Wrong Here

1. `process.exit(0)` at the end of `main()`, "to be explicit". It truncates
   piped stdout and reproduces only under a pipe.
2. Letting the argument parser exit for you. `--version` never returns, and
   your gate reports success through a path you never wrote.
3. `process.exitCode = EXIT.data` without a `return` — the error prints, the
   run continues, and the status is overwritten with `0`.
4. Treating commander's `exitCode` as if it were your table's. It is `0` or
   `1`; a parse error remapped to nothing lands on general failure.
5. A progress line, a banner or a spinner on stdout — output that reads
   correctly and breaks the caller's parser.
6. Inventing a code for a new failure class instead of using `65` or `69`,
   or extending the table.
7. Adding a `--quiet` flag that silences stderr. Quiet suppresses progress;
   errors are never optional.
8. No EPIPE guard, because the tool works in every invocation that is not
   piped into `head`.

## Sources

- [Node `process.exit()`](https://nodejs.org/api/process.html#processexitcode) — why it truncates, and the `process.exitCode` alternative
- [Node `process.exitCode`](https://nodejs.org/api/process.html#processexitcode_1) — assignment does not terminate; the value is truncated to 8 bits
- [Node `process.stdout` — "a note on process I/O"](https://nodejs.org/api/process.html#a-note-on-process-io) — pipes are asynchronous, TTYs and files are not
- [commander `.exitOverride()`](https://github.com/tj/commander.js#override-exit-and-output-handling) — `CommanderError`, and its `exitCode` of 0 or 1
- [FreeBSD `sysexits.h`](https://man.freebsd.org/cgi/man.cgi?sysexits) — `EX_USAGE` 64, `EX_DATAERR` 65, `EX_UNAVAILABLE` 69
- [clig.dev](https://clig.dev/) — streams, machine output, and the stdout/stderr split
- [POSIX shell exit status](https://pubs.opengroup.org/onlinepubs/9699919799/utilities/V3_chap02.html) — 126, 127, and 128+N
