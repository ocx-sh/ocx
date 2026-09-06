# Tiers, product shape and the first-steps contract

Read this when running step 1 or step 9, or when scoping a first-steps page.
It holds the tier model, the product shape that selects its thresholds, and the
exit condition a first-steps page must reach.

Contents: [Two axes](#two-axes-never-one) · [The three tiers](#the-three-tiers) ·
[Product shape](#product-shape-selects-the-thresholds) ·
[The exit condition](#the-exit-condition-one-observable-value) ·
[Budgets per shape](#budgets-per-shape) ·
[When a tutorial is required](#when-a-tutorial-is-required) ·
[The nav break](#the-nav-break)

## Two axes, never one

Tier and content type are independent. A page has both, and neither is computed
from the other. Collapsing them into one value such as `getting-started` is the
easiest mistake to make and the hardest to unpick later.

Tier is a reading-order decision across many pages. Type is what a single page
is for. Reference pages sit outside the tier progression entirely, because they
are addressable from every tier and gate no tier's entry or exit.

## The three tiers

`first-steps`, `everyday`, `integration`. Exactly three values. There is no
fourth tier for reference or for advanced material.

- **first-steps.** The reader has nothing yet. The tier ends when they have one
  verified result. Membership is decided by dependency order, not by rank, so a
  first-steps task has an empty `depends_on` list.
- **everyday.** Tasks a working user repeats. Organised by concept, not as more
  quickstarts.
- **integration.** Wiring the tool into something else: CI, another service, a
  hardened or multi-tenant environment. This is where threat models and
  environment levers live.

Ranking by friction severity puts the most painful task first. That is not the
same as the entry task, and the entry task is what tier one is for.

## Product shape selects the thresholds

Declare one shape per repository, from `cli`, `library`, `hosted-service` or
`framework`. Every first-steps threshold below branches on it. The thresholds
were calibrated on CLI and hosted-service examples, and they misfire on a
library. Read the shape first for that reason.

A library splits further into three sub-shapes, and the sub-shape decides
whether a precondition sentence is required.

| Sub-shape | What must exist first | Precondition sentence |
|---|---|---|
| Pure library | the install, nothing else | not needed |
| Network library | network access to a public unauthenticated endpoint | name the endpoint |
| Wrapper over a CLI, service or runtime | the wrapped binary, an account key, or a running process | required, above the fence |

A wrapper SDK cannot reach a zero-setup result. Writing its quickstart as if it
can produces a page that fails for every real reader. State the precondition
before the first call.

## The exit condition: one observable value

A first-steps page ends when the reader can see one concrete result. A printed
value, a returned object, an asserted value, a new file on disk, or a rendered
page. Not a status message. Not silence.

Every one of nine library quickstarts measured across Python, Rust and
TypeScript ends this way, with no exceptions. A CLI's working command and a
hosted service's returned object with an id field are two instances of the same
contract, not different contracts.

Every step of the page owes the reader a visible result too. A step whose only
effect is that no error appeared reads as progress to the author and as a dead
end to the reader. A `>>>` transcript line, a `#>` output comment or an inline
`// prints` comment satisfies this with no extra prose.

## Budgets per shape

These are smell thresholds. Exceeding one is not automatically wrong, but the
page owes a reason.

| Shape | What to count | Threshold |
|---|---|---|
| `cli`, `hosted-service` | ordered-list items plus command fences, from the H1 to the first verified result | above 9 actions, name the external systems being wired (9 from the longest hosted-service quickstart measured) |
| `library`, `framework` | fenced code blocks, from the heading that introduces the runnable snippet to the first block whose output is shown | above 4 blocks (the measured ceiling is 2 across 9 library quickstarts) |
| all | words before the first command, counted from the heading that introduces it | about 100 words (unsourced, and owned by the docs-quality page-type rules) |

Two counting traps.

- Counting ordered-list items on a library page returns zero, because 0 of 9
  measured library quickstarts use a numbered list. That is a silent false
  negative, not a pass. Count fences instead.
- Counting words from the H1 flags well-regarded pages that are landing page and
  quickstart at once. One measured page runs 320 words of pitch before its first
  block and is not in violation. Count from the example-introducing heading.
- Treat `<<<`, `--8<--` and `{{#include` as command blocks alongside a literal
  fence. A page that includes its command from a tested file still has a command.

## When a tutorial is required

A tutorial is required only when the reader must assemble two or more
interacting concepts before the tool is useful. Otherwise the page is a
quickstart or a how-to, and typing it `tutorial` imports obligations it does
not need.

A quickstart shows the primary feature as fast as possible. A tutorial teaches
through an example project. They are two templates with two jobs, not two points
on a spectrum. Zero tutorials across a 248-page corpus was a correct outcome for
CLI-shaped tools, not a gap.

A page typed `tutorial` takes the full contract. One non-branching path, no
unexplained action, a visible result from every step, and a walk-through by a
reader who did not write it. Package-manager
alternatives written as prose ("or, with pip:") are a branch and break the
contract just as a tabs component would.

There is a technique that chains two or three concepts without becoming a
tutorial. Annotate the example inline with numbered comments tied to a legend
directly below the block, one entry per call. The explanation sits beside the
code instead of walking the reader through separate typed steps.

## The nav break

Put the first-steps entry point and the everyday hub in different top-level nav
groups. The structural break is what stops tier one absorbing everyday and
integration content over time. A flat sidebar has no break and drifts within a
release or two.
