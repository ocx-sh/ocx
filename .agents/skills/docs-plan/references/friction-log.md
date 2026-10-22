# The friction log

Read this when running step 4. It holds the three-section template, the persona
contract, and what disqualifies a log.

A friction log is the one discovery method an agent can run alone. It needs no
voter pool and it produces a falsifiable observation, which is whether the task
completed and what actually happened.

Contents: [The three sections](#the-three-sections-and-the-fourth-that-must-not-exist) ·
[The persona contract](#the-persona-contract) · [Verbatim output](#verbatim-output-is-the-proof) ·
[Template](#template) · [What disqualifies a log](#what-disqualifies-a-log) ·
[Where logs live](#where-logs-live)

## The three sections, and the fourth that must not exist

Exactly three level-2 headings, in this order.

1. **Context.** Who is attempting the task, what they are trying to accomplish,
   what they had before starting. One named first-time persona.
2. **Pros and cons.** A bulleted list of what went well and what went badly.
3. **Detailed stream of consciousness.** First-person notes written during the
   attempt, with verbatim output pasted in.

There is no proposed-fix section. Naming a fix while the task is still being
discovered decides the solution before the task is understood. That is the same
failure as writing a user need that names a page, moved one stage earlier.

Solutions belong to whatever turns the coverage table into written pages. Not
here.

## The persona contract

The persona is concrete and explicitly inexperienced. "A Python developer who
has never run this CLI, following only the README" is a persona. "The developer"
and "a user" are not.

Never write "familiar with". An agent grading its own work as an unnamed expert
already knows the answer it is supposed to be discovering.

An agent with the repository in context cannot forget it. Where the harness
allows it, delegate the log to a subagent that has no repository access and only
the published entry point. Note in Context which of the two produced the log.

## Verbatim output is the proof

The stream-of-consciousness section must contain at least one shell prompt line
or one fenced block holding real output. Paste what the terminal printed,
including the error text, the exit code and the stack trace.

Without this, the log is a narration of what a user would probably feel. That is
the most common way this step gets faked, and it is invisible to every other
check.

Redact secrets by hand. Keep the shape of the output intact so a reader can see
where the run broke.

## Template

```markdown
# Friction log: T07 install the CLI

## Context

A Python developer who has never installed this tool, working on a clean
container with no toolchain. They were sent the README link and nothing else.
Written by a subagent with no repository access.

## Pros and cons

- Good: the install command was the first code block on the page.
- Good: the version check printed a version, so I knew it worked.
- Bad: the install failed on a missing system library, and the error did not
  name the package to install.
- Bad: I could not tell which of the three install paths was the supported one.

## Detailed stream of consciousness

Opened the README. Ran the first block.

$ curl -LsSf https://example.invalid/install.sh | sh
error: could not find `libssl`
exit code 1

Searched the README for "libssl". No match. Searched the docs site. No match.
Guessed the package name and installed it. Retried.

$ tool --version
tool 0.4.1

That worked, roughly six minutes after starting. The blocking step was the
undocumented system dependency.
```

## What disqualifies a log

- A section named "Solution", "Proposed fix" or "Recommendation".
- No verbatim output anywhere in the stream of consciousness.
- A persona sentence containing "familiar with", or no persona at all.
- Timings or step counts stated but never measured.
- More or fewer than three level-2 headings.

## Where logs live

One file per attempted task, named by the task id, under
`docs/discovery/friction-logs/`. The task row points at it by path. A log with no row, and a row with no log, are
both findable by a diff.
