# User needs

Read this when running step 6. It holds the form, a worked good and bad pair,
the banned verbs, and the check that rejects a solution-shaped need.

## The form

One sentence per shortlisted task, in three clauses.

> As a **X**, I need to **Y**, so that **Z**.

- **X** is who the reader is at the moment they hit this task. Not a job title
  in general, but the state they are in.
- **Y** is what they want to do, stated as an outcome.
- **Z** is why, stated as the thing that becomes possible.

Stored as three fields, never as one string. A stored sentence cannot be
checked clause by clause, and the check needs the clauses apart.

## The rejection rule

Reject a need whose **need** or **outcome** clause names a page, a command, a
flag or a product feature. Such a need creates a justification for content that
already exists. It also decides the solution before anyone asks what the reader
is trying to do.

The canonical pair, from the guidance this form comes from.

| Verdict | Need |
|---|---|
| Bad | As a carer, I need to use a benefits calculator, so that I can find out if I can get Carer's Allowance. |
| Good | As a carer, I need to get financial help, so that I can carry on looking after the person I care for. |

The bad one names the tool. The good one names the outcome and leaves the tool
open. The coverage step is then free to answer with a different page, or with no
page at all.

The same pair in this program's own domain.

| Verdict | Need |
|---|---|
| Bad | As a user, I need to run `tool lock --frozen`, so that CI passes. |
| Good | As someone wiring this into CI, I need the build to use the exact versions I tested, so that a green build stays green tomorrow. |

## Banned verbs

Reject "understand", "know" and "be aware of" in the need clause unless a
concrete action follows. Those verbs describe a mental state, which is not a
task, so nothing in the coverage table can ever satisfy them.

"I need to understand the lockfile format" is not a need. "I need to tell which
dependency changed between two lockfiles" is.

## The mechanical check

The check is advisory. Treat a hit as a prompt to re-read the sentence, not as
a verdict.

1. Build a token file from every heading in the docs tree, plus every
   subcommand and flag name the tool exposes.
2. Drop every token shorter than 4 characters. A stripped single-letter flag
   such as `-i` otherwise matches the pronoun "I".
3. Keep only phrases of two words or more. A single shared word is not evidence.
4. Match the need clauses against the token file.

```sh
grep -oiFf tokens.txt needs.txt
```

Any hit is a candidate rejection. The first version of this construction
measured a 100% false-positive rate on 5 legitimate needs during calibration
run A. That is why the rule sits at SHOULD and why the two filters above exist.
The rewritten construction has not been re-measured. Read every hit yourself.

## Keep the template out of the docs

The literal strings "As a" and "so that" belong in the discovery artifact only.
A rendered documentation page that opens "As a developer, I need to..." is the
template leaking into published content.

```sh
grep -rn "so that I can" docs/ --include='*.md' | grep -v docs/discovery/
```

That must print nothing.
