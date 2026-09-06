# Tested examples by language

Read this before choosing an example-test mechanism, and before writing any
harness of your own.

Contents: [Decision order](#decision-order) · [Per language](#per-language) ·
[The binding key](#the-binding-key) · [Marking a fence](#marking-a-fence) ·
[The generic harness](#the-generic-harness) ·
[Failure output](#failure-output) · [The recording layer](#the-recording-layer) ·
[Dead tools](#dead-tools)

## Decision order

1. Does the language ship a doctest runner that covers this example? Use it.
2. Does the example need a real external system, such as a registry, a network
   service or a filesystem the runner cannot give it? Then the doctest runner
   cannot help, and you need a subprocess harness.
3. Only then write the harness, as one file, in the shape below.

The failure this order prevents is measured and common. An agent asked to test
TypeScript examples reaches for a test framework and writes a Markdown parser,
not knowing that two native mechanisms already exist.

Two contracts are not interchangeable. A rewrite-in-place runner regenerates the
page's own text from a live command. A bind-and-assert runner leaves the page
alone and fails a separate test. A rewrite tool that is also the only check will
accept whatever a broken command printed as the new expected output. Never let
one tool both rewrite the page and serve as the only correctness check
(DOC-EX-23).

## Per language

| Language and surface | Mechanism | What it actually does |
|---|---|---|
| Shell, a documented command | Bind-and-assert: an acceptance-tested script per command, in the required gate | Runs the real command. This is the pattern the rule set is built on |
| Python, a fenced or docstring example | `doctest`, or Sybil for fenced Markdown blocks | Collects and executes. Confirm the collection glob reaches a page two directories deep before trusting it (DOC-EX-09) |
| Rust, a doc comment on a public item | `cargo test --doc` | Compiles and runs every doctest in the crate |
| Rust, a fenced block in an mdBook page | `mdbook test` | Compiles and runs Rust fences in the book |
| TypeScript, a type-level claim in a rendered sample | Twoslash, with the VitePress integration where the site is VitePress | Type-checks the real compiler over the sample at doc-build time and throws on an undeclared compiler error. It does not execute and does not capture output |
| TypeScript or JavaScript, a sample that must run | `deno test --doc` to execute, `deno check --doc-only` to type-check only | Runs fenced `js`, `mjs`, `cjs`, `jsx`, `ts`, `mts`, `cts` and `tsx` blocks straight from a Markdown file. A fence marked ignore is skipped. It runs under Deno, which is a real gap for Node-specific code |
| TypeScript, no doctest tool wanted | Subprocess per example, `node <file>.ts` for type stripping with no install, or a TypeScript runner when full type support is needed | Asserts the exit code only. No type checking without an added compile step |
| Go, an API-usage sample beside its source | `go test` Example functions | An Example with no `// Output:` comment is compiled and not executed. With one, it executes and diffs stdout |
| Go, a fenced sample inside a docs page | Transclude the tested Example's real source | The page never holds its own untested copy. Correctness is inherited from the Go test |
| Anything else | The generic harness below | Globs the example tree, runs each file as a subprocess, asserts the exit code |

Two mechanisms that look like answers and are not. In-source testing in a
JavaScript test framework tests source files, not fenced Markdown. A runtime
that ships a Markdown parser but no doctest feature gives you nothing here.

## The binding key

Bind a page to its out-of-page test with a declared key in the test header,
never with a mirrored file path. A mirrored path breaks every page the first
time the test tree moves.

```sh
#!/usr/bin/env sh
# doc: install-the-cli
# cast: true
```

Schema:

| Field | Required | Value |
|---|---|---|
| `# doc:` | yes | one slug, unique across the test tree, cited by exactly one page |
| `# cast:` | no | `true` opts this script into the optional recording layer. Absent means no recording |

The gate is a set diff in `checks/doc_examples.py`. It lists the fences whose
info string is on the runnable tier list, and the fences carrying a binding
key. A non-empty difference fails (DOC-EX-01). Both directions must come
back empty. An orphan key in the test tree fails too (DOC-EX-02).

## Marking a fence

Tag every fence with a language from the project's tier list (DOC-EX-05). Write a
tier suffix as one hyphen-joined token, never with a space.

````text
```python-no-run
```
````

Wrap a snippet that must not run in a paired marker that states why, and close it
before the next open marker (DOC-EX-06). A bare skip flag reads later as an
oversight rather than a decision.

A harness often substitutes a value, such as a temporary path or a parallel-safe
port. Do not then claim the shown example is identical to what ran (DOC-EX-08).
Name the canonicalization step instead.

## The generic harness

One file. It globs the example tree, dispatches each file to its language's
interpreter, and asserts the exit code. The measured floor is about 55 lines, and
that is the whole thing. Do not size it like a large worked example you read.

Its two required fixtures are one passing example file and one deliberately
broken one. Exit codes must be 0 and 1. `checks/doc_examples.py` carries this
mode and ships those fixtures.

Scan for documented commands that have no backing test as a lead list, never as a
merge gate (DOC-EX-10). One measured run flagged 20 mentions on a single page and
11 of them were legitimate annotated history.

Do not add a declarative terminal recorder such as a tape-file tool beside an
existing page-bound script tree (DOC-EX-18). Two script formats mean two
discovery paths and two classes of sanitization.

## Failure output

A failing example test must name the doc page, not only the test file and line
(DOC-EX-07). Without the page name, every failure costs a manual reverse map back
to the reader's view. Print the binding key and the page's human title.

## The recording layer

Optional, and it stays out of the gate. The correctness check is the test. The
recording is a view on a test that already passed.

- Disable the recording step and re-run the required gate. The result must not
  change (DOC-EX-11).
- Every recording comes from a real command run. Delete any non-executing mockup
  mode rather than leave it available (DOC-EX-12).
- Commit a cast only when no build step regenerates it. Tracked and regenerated
  at once is the contradiction to catch (DOC-EX-13).
- State the cast version you write and the player version you pin, and confirm
  the player has a parser branch for that version (DOC-EX-14).
- Default the player to no autoplay whenever a recording source is set
  (DOC-EX-15). Leave the player's own accessible controls enabled (DOC-EX-16).
- Check `prefers-reduced-motion` before starting playback (DOC-EX-17).

The public worked example of this whole shape is the `ocx` project. Every
documented command is an acceptance-tested script bound to its page by a
`# doc:` key. Cast generation is opt-in per script through a second header key. The casts are
produced only in the site build, so the merge gate never depends on the
recorder.

## Dead tools

Five JavaScript and TypeScript doctest packages are four or more years stale and
must not be recommended, however high they rank in a search. Check a package's
latest release date before proposing it. A gap of more than 12 months since
the last release means read the issue tracker first, and flag an existing
dependency for a maintenance review.
