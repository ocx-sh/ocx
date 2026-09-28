---
# Read on demand through the index. This glob matches no file, so a client never auto-loads the file.
paths:
  - "**/.code-docs-depth-on-demand"
title: Code-Rendered Surfaces
summary: The SRF family, owning doc text that a generator copies to readers outside the repo (CLI help, JSON Schema, MCP tool lists, published API docs), the tokens it never carries, where its rationale moves, the output-side test per generator, the summary paragraph and the module front page
---

# Code-Rendered Surfaces

A generator copies doc text out of the source verbatim, and nothing filters it.
This file owns that text: what counts as interface text, the tokens it never
carries, where its rationale moves, the test that proves the rendered output
clean, the summary paragraph every listing shows, and where a module states
its reason. Length caps on interface blocks are LEN-08. Bare IDs in ordinary
comments are LNK-01.

Contents: [Dates and Defaults](#dates-and-defaults) ·
[What Interface Text Is](#what-interface-text-is) · [The Token Set](#the-token-set) ·
[Interface Text Rules](#interface-text-rules) · [The Output Gate](#the-output-gate) ·
[Summary and Front Page](#summary-and-front-page) ·
[Hover Truncates Nothing](#hover-truncates-nothing) ·
[What Agents Get Wrong Here](#what-agents-get-wrong-here)

## Dates and Defaults

Generator behaviour read from source in September 2026: clap_derive sends the
first paragraph to `-h` and the whole body to `--help`, and strips a trailing
period. schemars copies the whole body into `description`. rmcp's `#[tool]`
uses `description =` verbatim, else the function's doc comment. argparse,
click and commander print what they are given.

- **Defaults an adopter may override:** the 200-character summary cap (clippy's
  number, unvalidated for Python and TS) and the allow-list. A repo's own ID
  families add to the token set; the built-in ten never come off.
- **Fixed:** the negative fixtures in [The Token Set](#the-token-set). A token
  set that flags them fails forever on clean text.
- **Commands** name each script by its path in this rule's `checks/`
  directory. A test that runs in CI needs its own copy: vendor
  `checks/interface_leak.py` together with `checks/comment_census.py`, which
  it imports, for example under `tools/`. The examples below use that path.

## What Interface Text Is

The audience decides, not the generator. Interface text is any doc comment or
description string that a generator copies to a reader outside the repo, who
cannot open the source or resolve an ID. Every row but the last copies the
text raw; the API-docs renderers (rustdoc, mkdocstrings, TypeDoc) resolve
cross-references into links.

| Generator | What it copies | What a doc-comment scan misses |
|---|---|---|
| clap derive (`Parser`, `Args`, `Subcommand`, `ValueEnum`) | `///` on the item and every field | `about`/`help` attribute strings, builder `.help("…")` calls |
| schemars `JsonSchema` | the whole doc body into `description`, with no short/long split | a `cfg_attr(…, derive(JsonSchema))` derive; `"description"` literals in a hand-written schema |
| TS-to-JSON-Schema generator (ts-json-schema-generator, typescript-json-schema, a custom script) | the JSDoc on each exported type and property into `description` | all of it until `.code-docs.json` declares the source files: nothing in a `.ts` file says a generator reads it |
| MCP server | every tool description and its argument schema | an rmcp `#[tool]` with no `description =` publishes the function's `///` |
| argparse, click, typer | `help=`, `description=`, `epilog=`; click and typer command docstrings | strings built at runtime |
| commander | `.description()`, `.summary()`, option and argument descriptions | strings passed through a helper |
| GitHub Action | `action.yml`: the action's `description` and every input and output `description` | all of it: the manifest is not source, so a doc-comment scan never reads it |
| VS Code extension | `package.json` `contributes`: command titles, setting descriptions (`markdownDescription`, `enumDescriptions`), and every JSON Schema it contributes, which renders on hover | all of it, including a schema vendored from another repo |
| a published package's API docs | every public doc comment, Python attribute docstrings included | attribute docstrings, which `inspect.getdoc()` cannot see |

Not interface text: private items, `//` and `#` comments, and a module doc
that never renders outside the repo. Record pointers belong there.

Declare the source files a generator the checks do not recognise reads, such
as a TS-to-JSON-Schema generator's types, in `.code-docs.json` under
`interface_files`: globs whose doc comments the census counts as interface
lines (LEN-08) and `cleanup_check.py` freezes (CLN-03). The gate stays the
output scan of the generated schema (SRF-03).

## The Token Set

One scanner holds the list: `SRF01_PATTERNS` in `checks/interface_leak.py`,
20 families in 21 patterns. Summary:

| Family | Matches | Must not match |
|---|---|---|
| Process IDs, ten prefixed families | `C-`, `S-`, `WP-`, `DEC-`, `DX-`, `RUL-`, `A-`, `D-`, `D-V`, `ADR-` plus digits | a Windows SID `S-1-5-18` |
| Clause labels | `C-S1-1` | |
| Record filenames | every LNK-02 record form: a record stem (`adr_`, `plan_`, `rulings_`, `subsystem-`, `decision-`, `design_spec_`, `handover_`) or a `.md` under `adr/`, `adrs/` or `decisions/` | |
| Source paths | any source file named with a directory (`src/cli/app.py`) | a bare script name, such as `mycli.py` in argparse's usage line |
| ISO dates | `2026-09-27` | a date inside a URL, such as a spec version |
| Rust code syntax, dropped by `--lang python` and `--lang ts` | a source filename (`lib.rs`), an intra-doc link, `Self::`, `crate::` | a URL host such as `example.rs/` |

- Short labels (`D4`, `H1`) stay out: a quarter of their hits are false
  (`V1`, `C0`, `K8s`), so review catches them.
- When `linkage_check.py ids --discover` leads you to add a family to the
  repo's config, add it to `SRF01_PATTERNS` too. The scanner also covers
  LNK-04's record names, so one output gate serves both rules.
- `--allow FILE` takes one reviewed exception per line: `TOKEN`, or
  `GLOB TOKEN` scoped to a path or `path#/json/pointer`.
- `--lang python` and `--lang ts` drop the Rust code-syntax families, because
  mkdocstrings and TypeDoc cross-references share the bracket syntax.
- Maintainer rationale has no token. Read each interface sentence and ask:
  would a user of this flag, field or tool need it to use it?

## Interface Text Rules

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SRF-01 | State the user contract only in interface text: no plan or process ID, record or source filename or ISO date. Where the generator copies raw (help, schema and tool text), also no code path (`Self::`, `crate::`, a private function name) or intra-doc link: name flags, keys and fields in plain backticks, even where a language rule asks for intra-doc links. Published API docs keep their renderer's cross-references. | No generator filters what it copies, so every token ships to a user who can resolve none of them, and on a raw-copy surface each link prints as brackets and rewrites a byte-pinned schema golden whenever its target moves. | The output gate below exits 0; the reading question above finds rationale | MUST |
| SRF-02 | When interface text holds maintainer rationale (a guard, why the code has its shape, what later code relies on), move it verbatim in the same edit to a plain comment on the line it protects or directly above the item. When it runs past the plain cap, split it one sentence per breaking edit, each at its line (GRD-03). Never delete a clause. | The leak cleanup is where guards die: a guard rendered into `--help` is often the only statement of a panic or a flag-id collision, and deleting it satisfies SRF-01. | The diff command below: every removed doc sentence that names a breaking edit or a consequence has a matching added plain-comment line | MUST |

```bash
# SRF-02: removed doc lines beside added plain comments, for reading
git diff -U0 "$(git merge-base origin/main HEAD)" -- '*.rs' '*.py' '*.ts' '*.js' '*.go' '*.java' '*.kt' | rg -n -e '^\+\+\+ ' -e '^-\s*///' -e '^-\s*\*' -e '^\+\s*//[^/!]' -e '^\+\s*#[^!\[]'
```

Python docstring lines carry no marker, so read every removed line inside the
docstring. GRD-04 picks the register for any doc comment; on an interface
item the answer is fixed, and rationale never stays.

```rust
// wrong: the guard renders into --help, and deleting it passes SRF-01
/// Registry host. Named `host`, not `registry`: that id collides with the global `--registry` flag.
host: String,

// right: the contract renders; the guard stays at the field, verbatim
/// Registry host to log in to.
// Named `host`, not `registry`: that id collides with the global `--registry` flag.
host: String,
```

Ship an interface cleanup as its own change. `cleanup_check.py` freezes
interface lines (CLN-03) and fails a comment cleanup that edits them. A golden
file that pins rendered text regenerates in the same change; treat description
prose as regenerable unless the project declares it contract.

rust-quality's DOC-16 (intra-doc links, which print raw in clap and schemars
output) stops at raw-copy items, and DOC-01 (third-person summary; clap
summaries are imperative or noun phrases) at clap items; both point here. DOC-11 keeps its clap
form limits: a short line of about 70 characters, ASCII only.

## The Output Gate

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SRF-03 | Give every generated surface the repo ships a test that walks the generator's own model, feeds all rendered text to the shared scanner and fails on any SRF-01 token. Run the source scan before the build as a cheaper complement, never as the gate. | A doc-comment scan misses hand-written descriptions, `cfg_attr` derives, explicit MCP descriptions, runtime strings and rmcp's `///` fallback. A CLI with no help test shipped a plan ID in `--help`. | Plant `(C-999)` in one rendered doc comment: the test fails. Remove it: the test passes | MUST |

| Surface | Walk | Scanner input |
|---|---|---|
| clap | `render_long_help()` of every `Command` in the `Cli::command()` tree | stdin, `--lang rust` (the default) |
| commander | `helpInformation()` of the program and every `.commands` entry | stdin, `--lang ts` |
| argparse | `format_help()` of the parser and every parser in each subparsers action's `choices` | stdin, `--lang python` |
| JSON Schema | every emitted schema: the committed goldens, the binary's own schema export, or a TS generator's output | the directory; every `description` and `title` value is scanned and reported with its JSON pointer |
| MCP | the tool list serialized in-process (rmcp: the router's `list_all()`), which covers `description =`, the `///` fallback and the argument schemas | the JSON on stdin |
| Python API reference | griffe, mkdocstrings' own loader, over the public API under the docs build's `filters`, every docstring included; the scanner's module docstring carries the walk | stdin, `--lang python`; never `inspect.getdoc()`, never a grep of built HTML (it counts `show_source` listings) |
| GitHub Action | `action.yml` itself; the scanner reads only its description keys, block scalars included | the file, `--lang ts` |
| VS Code extension | `package.json` and each contributed schema file; the JSON walk reads every `description`, `title` and `*Description(s)` key | the files, `--lang ts` |
| a built CLI in CI | `--help-walk "BIN"` recurses through clap and commander `Commands:` sections; for another help layout, capture each screen and pipe it | none: the scanner runs the binary |

Never feed a bundle (`dist/index.js`) to the scanner. It holds dependency
comments and version strings, not interface text, and one Action bundle
produced 3,003 hits, none real. Scan the manifest and the source it is built
from.

```rust
#[test]
fn help_has_no_internal_references() {
    fn walk(c: &mut clap::Command, out: &mut String) {
        out.push_str(&c.render_long_help().to_string());
        c.get_subcommands_mut().for_each(|s| walk(s, out));
    }
    let mut help = String::new();
    walk(&mut <Cli as clap::CommandFactory>::command(), &mut help);
    let mut scan = std::process::Command::new("python3")
        .args([concat!(env!("CARGO_MANIFEST_DIR"), "/../../tools/interface_leak.py"), "-"]) // crate two levels below the root
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(&mut scan.stdin.take().unwrap(), help.as_bytes()).unwrap();
    assert!(scan.wait().unwrap().success(), "internal reference in --help");
}
```

```python
import argparse, subprocess, sys
from mycli.cli import build_parser  # the function that returns the root parser


def screens(p):
    yield p.format_help()
    for a in p._actions:
        if isinstance(a, argparse._SubParsersAction):
            for sub in a.choices.values():
                yield from screens(sub)


def test_help_has_no_internal_references():
    subprocess.run(
        [sys.executable, "tools/interface_leak.py", "--lang", "python", "-"],
        input="\n".join(screens(build_parser())),
        text=True,
        check=True,
    )
```

```ts
import { execFileSync } from "node:child_process";
import type { Command } from "commander";
import { test } from "vitest";
import { buildProgram } from "../src/cli/program"; // export the program builder first

test("help has no internal references", () => {
  const screens: string[] = [];
  const walk = (c: Command): void => { screens.push(c.helpInformation()); c.commands.forEach(walk); };
  walk(buildProgram());
  execFileSync("python3", ["tools/interface_leak.py", "--lang", "ts", "-"], { input: screens.join("\n") });
});
```

```bash
python3 checks/interface_leak.py schemas/                     # emitted JSON Schemas; exit 1 on any token
python3 checks/interface_leak.py --help-walk "target/release/mycli"
python3 checks/interface_leak.py --source --root . --strict   # pre-build; add --published for a library
```

The source scan cannot see a description built at runtime or passed through a
helper; the output gate can. Exit codes: 0 clean, 1 findings, 2 usage.

## Summary and Front Page

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SRF-04 | Open a doc comment that says more than one sentence with a one-sentence summary of at most 200 rendered characters, then a blank doc line. On a clap-rendered item, rust-quality's DOC-11 short line of about 70 characters applies instead. | rustdoc listings, clap `-h`, TypeDoc and Click split at the first blank line and truncate nothing, so a run-on paragraph becomes the whole listing entry. In one agent-written TS repo, 93 of 196 long blocks had no break at all. | Rust: `rg -n --glob 'Cargo.toml' -e 'too_long_first_doc_paragraph' .` hits under `[workspace.lints.clippy]` (a nursery lint, so opt in), with existing debt ratcheted per crate so it never rises. Any language: the census command below, whose line count must not rise | SHOULD |
| SRF-05 | State why a module exists once, in its module doc. Item docs in the same file describe only their local mechanism and never repeat the module's pointer or ID. | Without it the reason is restated at nearly every item: one agent-written file repeats one module-level ID 57 times, and 340 of its doc lines carry an ID. | The triage below: a record name or process ID in its output is a finding; a standard name (`UTF-8`, `SHA-256`) is not | SHOULD |

```bash
# SRF-04: every summary paragraph over 200 characters, as file:line:length
python3 checks/comment_census.py --root . --scope prod --list-blocks --kind doc --min-block 1 --format json | python3 -c 'import json, re, sys
for b in json.load(sys.stdin):
    t = re.sub(r"(?m)^[ \t]*(?:/{2,3}!?|/\*\*|\*(?!/)|\x22{3}|\x27{3})[ \t]?|\s*(?:\*/|\x22{3}|\x27{3})\s*$", "", b["text"])
    s = " ".join(re.split(r"\n\s*\n|\n\s*@", t.strip())[0].split())
    if len(s) > 200: print(b["file"], b["line"], len(s), sep=":")'
```

```bash
# SRF-05: every record name or ID token repeated within one file, with its count
git ls-files -z -- '*.rs' '*.py' '*.ts' '*.go' '*.java' '*.kt' | xargs -r -0 rg -o -N --with-filename -e '\badr_[a-z0-9_]+\.md\b' -e '\bplan_[a-z0-9_]+\.md\b' -e '\b[A-Z]{1,5}-[0-9]{1,4}\b' | sort | uniq -c | awk '$1 > 1'
```

Module-doc presence is not SRF-05's concern; the language rule owns it (for
Rust, rust-quality's DOC-10).

## Hover Truncates Nothing

rust-analyzer, typescript-go and pyright render the whole doc block on hover,
and the LSP 3.17 hover result has no length field (read September 2026). Never
justify a summary, a cut or a split by hover truncation. The split matters for
listings (SRF-04), and the whole body matters because clap long help and
schemars copy all of it.

## What Agents Get Wrong Here

1. **Writes for the rustdoc reader on a rendered item.** Intra-doc links and
   `Self::` paths on a clap or schemars item print raw. SRF-01.
2. **Carries plan IDs and record names into help, schemas and SDK references.**
   SRF-01, caught by SRF-03.
3. **Writes the rationale as one unbroken paragraph**, so the listing entry is
   the whole essay. SRF-04.
4. **Restates the module's reason at every item.** SRF-05.
5. **Deletes the guard while cleaning the leak.** It is rarer than 1 to 4 and
   costs the most. SRF-02.
6. **Changes what renders without re-reading the doc:** adds `JsonSchema` or
   `Parser` to an existing item, or drops `description =` from a `#[tool]`. A
   hunk-scoped check misses both; the SRF-03 walk catches them.
