# Reader signals

Read this when standing up step 7. It holds the sink shapes, the cost bands, the
bias rules, and the schema of every file that step produces.

Contents: [The manifest](#the-manifest) · [Deferral form](#deferral-form) ·
[Zero-result sink](#zero-result-sink) · [Analytics vendors](#analytics-vendors) ·
[Feedback widget and bias](#feedback-widget-and-bias) ·
[Time to first working result](#time-to-first-working-result) ·
[Issue template](#issue-template) · [Trigger matrix](#trigger-matrix) ·
[Error identifiers](#error-identifiers) · [What to refuse](#what-to-refuse)

## The manifest

One file holds every signal's state, `docs/.meta/observability.md`. A signal that
exists but has no recorded review is indistinguishable from one nobody ever read
(DOC-OBS-10).

Every entry carries four fields. A signal with status `instrumented` and no
`last_reviewed` date fails the parser.

```yaml
- signal: zero-result-search
  status: instrumented        # instrumented | deferred | declined
  sink: cloudflare-worker     # the mechanism, not a vendor slogan
  review_trigger: 20 new entries or the next tagged release
  last_reviewed: 2026-09-05
  bias: none beyond ordinary search abandonment
```

The review trigger is a count or a release boundary, never a bare cadence word
such as regularly or periodically (DOC-NAV-15). A cadence word never fires on a
site with little traffic.

## Deferral form

Defer any signal the current stack cannot produce, and record the exact
precondition that would unblock it (DOC-OBS-12). A requirement no repo can
satisfy trains readers to ignore the whole rule set.

```yaml
- signal: agent-versus-human-traffic-share
  status: deferred
  precondition: a named, checkable consumer question in the pull request
  note: static hosting exposes no request log to the site owner
```

A precondition must be checkable. Write the thing that would have to become true,
not a wish. The reviewer then checks the precondition rather than the missing log.

## Zero-result sink

The zero-result query is already computed in the reader's browser. The only
question is where the event lands. It is required, not deferred, because every
sink shape is priced and cheap (DOC-NAV-10).

| Sink shape | Cost | Notes |
|---|---|---|
| A serverless function you own, writing to a key-value or SQL store | free tier covers 100,000 requests a day on the measured vendor, paid from about 5 a month | Cheapest and most portable. The same function can also receive the feedback vote, so a project that builds it once pays the fixed cost once |
| An analytics vendor's named custom event | free to about 20 a month | Only for a vendor that accepts a named event with properties. See the table below |
| A raw access-log line on the static host | not available | The host most repositories reach for logs visitor addresses and never exposes that log to the site owner. This shape collapses back into the serverless function with no advantage |
| A search-product migration | real engineering | Never require it. It is an upgrade for other reasons |

Classify each query seen twice as reword or gap (DOC-NAV-14). A `gap`
disposition attaches the repository-wide grep that found nothing, and files an
issue quoting the literal query. An unclassified log becomes noise, and then
every query reads as a missing page.

## Analytics vendors

Only needed when the repo wants page traffic. Confirm current pricing at the
vendor's own page before citing a number. Two rows below were read from
third-party trackers rather than the vendor.

| Vendor | Self-host | Hosted entry cost | Named custom events |
|---|---|---|---|
| Plausible | free, AGPL | about 9 a month at the smallest tier | yes, on every plan |
| Umami | free, MIT | a free hobby tier is reported, unconfirmed at the vendor's own page | yes, including the free tier |
| GoatCounter | free, source available | free for reasonable public usage | no, click tracking only |
| Fathom | not offered | about 45 a month | yes, on every plan |

A vendor with no named custom event cannot carry the zero-result signal or the
feedback vote. That rules out GoatCounter and the free tier of the large
edge-network analytics product, both of which answer page views only.

A repository with no site at all still has one signal at zero cost. The forge's
own repository traffic endpoint returns 14 days of views, unique visitors and
the most-visited paths for anyone with push access. It measures traffic to the
repository page, not to a docs domain, which makes it the right fit only where
no site exists.

## Feedback widget and bias

Defer the widget until a real denominator exists. A helpfulness percentage with
no traffic number under it is the unmeasured metric the rule set already forbids
(DOC-OBS-16). The precondition is a page-analytics signal reporting nonzero for
30 consecutive days.

The moment a widget ships, its bias disclosure is not deferrable (DOC-OBS-17).
Two biases stack:

1. Survivorship. The reader who almost succeeded stays to click. The reader who
   was completely lost leaves no trace.
2. The sink's own filter. A widget that posts through a forge comment app makes
   every voter authorize an account first. The reader new enough to be genuinely
   confused is exactly the reader least likely to clear that filter. A serverless
   function holding a repository-scoped token files the same vote with nothing
   asked of the reader.

Name the sink mechanism and its filter in the same manifest entry, beside any
percentage it produces.

## Time to first working result

Measure it by hand, once, and record the number with its measurement date
(DOC-OBS-07). An unrecorded onboarding time cannot regress visibly, so a broken
step hides.

`docs/.meta/tthw.md` holds an integer and an ISO date, nothing else:

```
minutes: 12
measured: 2026-09-05
```

CI fails when a page declaring `doc_type: landing` or `doc_type: tutorial` is in
the changed paths and this file is not. Never estimate the number. An estimate is
the fabricated metric the rule set exists to catch.

## Issue template

The cheapest signal in the whole set, and 22 of 22 measured repositories failed
it (DOC-OBS-11). Ship a template under the forge's issue-template directory whose
`labels:` list contains `docs`, and name the trigger on which that label is
triaged. Without a labelled landing place, a deferred docs fix silently becomes
no fix.

## Trigger matrix

`docs/.meta/trigger-matrix.md` maps each source glob to the doc file and section
a change to it invalidates (DOC-OBS-03). At least three rows.

| Source glob | Invalidates | Section |
|---|---|---|
| `src/cli/**` | `reference/command-line.md` | the flag tables |

Keep it portable. The template must carry no path from the repository you copied
it out of. That is the most common way a source path leaks into a shipped file.

## Error identifiers

Where the project emits stable, documented error identifiers, every one must
resolve to a docs anchor (DOC-OBS-18). Grep the error-defining source for its
identifier pattern, then diff that list against the anchor set the raw link pass
already produces. Both differences must be empty. Reuse that resolver rather than
building a second checker.

A dependency's error payload sometimes carries its own docs link. Surface that
link rather than folding it into a dump of the raw body (DOC-OBS-19).

## What to refuse

- Any published number without its denominator, its channel and its date
  (DOC-OBS-08).
- A page's last-updated date as a build gate. No validated review interval
  exists, so a clock gate fails on a number nobody can defend (DOC-OBS-13).
- A sentence-level duplication ban. Detect forked pages by hashing normalized
  paragraphs across files instead, and let a restated default value stand
  (DOC-OBS-14).
- Silence read as success. No open issues is not evidence the docs work.
