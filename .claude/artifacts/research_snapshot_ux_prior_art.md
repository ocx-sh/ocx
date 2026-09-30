# Research: snapshot channel UX — prior art

## Metadata

**Date:** 2026-09-29
**Domain:** product-ux
**Triggered by:** adr snapshot lifecycle
**Expires:** 2027-03-29

Builds on `.agents/research/snapshot-lifecycle-prior-art.md` (registry-native retention mechanics,
index removal semantics). Goes deeper on **user-facing shape**: CLI flag naming, channel-pin UX,
per-PR teardown triggers, declarative-vs-imperative. Skips prior GitLab/Harbor/Nexus mechanics
coverage except where the *shape*, not the mechanism, is new.

## Direct Answer

No tool surveyed unifies "rolling channel, keep-last-N" and "per-MR channel, torn down on close"
under one command shape — different problems, different owners everywhere observed. Rolling
retention is **declared once** (policy object, evaluated by a scheduler); per-PR teardown is
**triggered by an event** (MR/PR close), never by a retention count. Treating both as "prune with
a flag" is the shape most tools reject. Recommendation at the end.

## 1. CLI shapes for prune/retention

**Low-level OCI tools carry no retention policy at all** — `regctl`, `crane`, `oras` delete one
tag or manifest per invocation, nothing else:

```sh
regctl tag rm registry.example.org/repo:v42          # single tag, --ignore-missing only flag
crane delete registry.example.org/repo:v42            # same shape; digest delete removes every
                                                        # tag sharing it [github.com/google/go-containerregistry#1578]
```
[regclient.org/cli/regctl/tag/delete](https://regclient.org/cli/regctl/tag/delete/)

Retention ("keep N", "older than X") is layered on top by a **separate** tool/script that lists
tags, filters, and calls the primitive in a loop — never a flag on the delete verb itself. This
is the load-bearing finding: **the primitive and the policy are different products** in every
OCI-native tool surveyed.

**Policy-bearing tools** all converge on the same four concepts, named differently:

| Tool | Keep-N | Age cutoff | Keep-pattern (never delete) | Dry-run |
|---|---|---|---|---|
| GitLab cleanup policy | `keep_n` | `older_than` | `name_regex_keep` | none — policy runs async, no preview |
| Harbor retention rule | "retain most recently pushed" count | N/A (count-based) | tag pattern `matching`/`excluding`, `**` glob | explicit **Dry Run** button before Save |
| `snok/container-retention-policy` | `keep-n-most-recent` | `cut-off` (relative duration `"2h 5m"`, not a date) | `image-tags` prefix match | `dry-run: true` input |
| `actions/delete-package-versions` | `min-versions-to-keep` | N/A | `ignore-versions` (regex) | not supported upstream; third-party forks add it |

Sources: [GitLab cleanup policies dev doc](https://docs.gitlab.com/development/packages/cleanup_policies/),
[GitLab reduce container registry storage](https://docs.gitlab.com/user/packages/container_registry/reduce_container_registry_storage/),
[Harbor tag retention rules](https://goharbor.io/docs/1.10/working-with-projects/working-with-images/create-tag-retention-rules/),
[snok/container-retention-policy README](https://github.com/snok/container-retention-policy/blob/main/README.md),
[actions/delete-package-versions](https://github.com/actions/delete-package-versions).

Naming pattern: every policy-bearing tool spells count as **keep**, never **retain**/**preserve**
in the flag itself (Harbor's UI says "retain", its API field is `retention_rule`); age is
**cut-off**/**older-than**, never "TTL" — a container-runtime term these tools avoid. GitLab is
the only tool that states precedence when keep-N and older-than disagree: exclude-keep-pattern →
order by created_date → exclude keep_n most recent → exclude anything newer than older_than.
Worth lifting that order as-is.

**Registry package managers with unpublish, not delete** (npm `dist-tag rm`, cargo `yank`, PyPI
`yank`): these are index-visibility toggles, not storage prune — orthogonal axis, already
covered in the first scan. Worth restating once: npm allows true unpublish only ≤72h
[npm unpublish policy](https://docs.npmjs.com/policies/unpublish/); cargo `yank` never deletes
bytes, only blocks new dependents [cargo yank docs](https://doc.rust-lang.org/cargo/commands/cargo-yank.html);
PyPI restricts deletion further under PEP 763. None of these apply to a rolling binary channel —
they assume one build per version string, not N builds racing under one tag.

## 2. Channel/rolling-tag UX in binary distribution

**Two orthogonal axes every tool separates**: (a) a **channel pointer** that always resolves to
"whatever is newest" (`nightly`, `canary`, `:edge`, `HEAD`), and (b) an **immutable, addressable
build** a user can pin instead. The UX question is how (b) is spelled and whether (a) is ever
itself deleted.

- **rustup**: `rustup default nightly` follows the channel; `nightly-2026-09-29` (or
  `rust-toolchain.toml` → `channel = "nightly-2026-09-29"`) pins one dated snapshot, which
  rustup resolves through its own manifest server — the *pin* is a date string, not a build ID
  or digest [rustup channels](https://rust-lang.github.io/rustup/concepts/channels.html),
  [rustup overrides](https://rust-lang.github.io/rustup/overrides.html). Old dated nightlies are
  never deleted server-side (they're small compiler artifacts, cost is accepted); rustup instead
  prunes *locally installed* toolchains a user opts into via `rustup toolchain uninstall`.
- **Deno**: `deno upgrade --canary` follows latest canary; `deno upgrade --canary --version
  <7-char-commit-hash>` pins one. The pin unit is a **git commit hash**, surfaced directly in
  `deno --version` output, so a user can always identify and re-request the exact snapshot they
  are on [Deno upgrade docs](https://docs.deno.com/runtime/reference/cli/upgrade/).
- **Bun**: no per-commit artifact exists to pin to at all — `bun upgrade --canary` always means
  "latest canary"; the workaround (`bunx bun-pr <hash>`) only works if CI happened to build that
  commit, under a hash-suffixed binary name rather than replacing `bun`
  [oven-sh/bun#17485](https://github.com/oven-sh/bun/discussions/17485). Negative case worth
  citing: a rolling channel with **no pinnable identity** forces vendoring the binary or giving
  up on reproducing a past canary run.
- **Homebrew `--HEAD`** / **Docker `:edge`**: neither offers a companion pinnable identity.
  `brew install --HEAD foo` resolves whatever the formula's git ref is at install time,
  discoverable only after the fact via `brew info --HEAD`; `:edge` pinning requires the consumer
  to resolve the tag to a digest themselves (`docker inspect --format '{{.RepoDigests}}'`) at the
  moment they care, with no "list past resolutions" API.

**Removal communication**: none of these tools signal deletion of an old snapshot proactively.
rustup and Deno's dated/hashed builds are *retained indefinitely upstream* — never deleting a
compiler/runtime binary is the accepted cost of reproducibility. The only "communicated removal"
found anywhere in this survey is GitLab's — storage policy, not a UX message to a pinned consumer.

## 3. Per-PR preview/snapshot artifacts

- **pkg.pr.new**: every commit/PR gets an npm-compatible package at a stable URL
  (`https://pkg.pr.new/<org>/<repo>@<commit-or-pr>`); nothing is "published" to the real
  registry, so there's no retention flag at all — lifetime is whatever pkg.pr.new's own backend
  retains, decoupled entirely from PR open/close state
  [pkg.pr.new announcement](https://blog.stackblitz.com/posts/pkg-pr-new/). Cleanest precedent
  for **"never touches the real index, so there is nothing to prune"** — the opposite of
  registry-native retention.
- **GitLab review apps**: teardown is **event-triggered**, not count- or age-based. A
  `stop_review` job wired via `on_stop:` fires when the MR merges *or* its source branch is
  deleted; a 2-day **auto-stop** backstop covers the case neither happens — age is the fallback
  for a broken trigger, not the primary mechanism
  [GitLab review apps testing guide](https://docs.gitlab.com/development/testing_guide/review_apps/).
- **Vercel**: preview deployments are retained independent of PR state — clock starts at deploy
  creation, defaults to 30 days (Hobby) / 180 days (Pro/Enterprise, configurable to 3 years);
  closing the PR does **not** trigger deletion, deletion is a scheduled sweep with ~48h lag and a
  30-day soft-delete recovery window [Vercel deployment retention](https://vercel.com/docs/deployment-retention).
  Confirms: hosted-preview platforms treat "PR closed" and "artifact expired" as unrelated events
  by design — a closed PR's preview stays live until its own clock runs out, because someone may
  still need to look at it after merge.

Net shape: event-triggered teardown and count/age-triggered retention answer different questions
— "is this MR still open" vs. "have we kept too many of these." A per-MR channel wants the
first; a rolling nightly wants the second. No surveyed tool conflates them.

## 4. Declarative vs imperative

| Shape | Examples | Why |
|---|---|---|
| **Declarative — policy lives in config/index, evaluated by a scheduler/server** | GitLab cleanup policy (project settings + API), Harbor retention rule (project-level, cron-evaluated), Vercel retention window (account/project setting), GitLab review-app auto-stop (`environment.auto_stop_in` in `.gitlab-ci.yml`) | Retention is a property of *the channel*, not of any one CI run — every push must obey the same rule, so it belongs where the channel is defined, not on the command that happens to publish this build. |
| **Imperative — flags passed per invocation, usually from a CI job** | `snok/container-retention-policy` (GitHub Action inputs per workflow run), `actions/delete-package-versions` (same), raw `regctl`/`crane` scripted loops | GitHub Packages/GHCR has no first-party retention API at all — third-party Actions fill the gap, and an Action's only interface *is* per-invocation inputs. Imperative here is a platform-capability gap, not a preference. |

Dividing line: **who owns the storage**. Platforms that own their registry server ship a policy
object (GitLab, Harbor, Vercel); platforms exposing only a dumb API (GHCR, raw OCI registries)
push the decision to whoever schedules the CI job — imperative by necessity, not preference.
OCX's registry is the second kind (proxy over arbitrary OCI backends, no server-side policy
engine): declarative *config* (`ocx.toml`/index-level) is still possible, but **execution** must
be imperative (an `ocx` CLI invocation on a schedule), same as the GHCR-Action pattern.

## Recommendation

OCX has no registry delete today and a `--keep-tag` anti-deletion net on every push — any
snapshot CLI ships prune as new surface, not a toggle on existing verbs. Recommended split,
mirroring finding 4:

- **Declare retention once, per channel, in `ocx.toml`** — not a flag repeated on every publish.
  Mirror GitLab's field names rather than inventing new ones:
  `[package.channel.nightly]` with `keep_n = 14`, `older_than = "30d"`.
- **Per-MR channels are event-triggered, never count-based** — a channel keyed by
  `pr-<number>` that a CI step tears down on MR close, the GitLab review-app shape, not a
  retention rule. Do not let `keep_n`/`older_than` apply to PR channels at all; give them their
  own `ocx package channel close <name>` verb fired from the MR-closed CI hook, with an
  auto-stop age fallback (Vercel/GitLab's belt-and-suspenders) for the case the hook never runs.
- **`ocx package prune <channel>` is the imperative executor** of the declared policy — run on a
  schedule (cron job, not a webhook), `--dry-run` on by default given OCX has no registry delete
  today and Harbor's Dry-Run-before-Save UX is the safest precedent found; require `--yes` (or
  `--dry-run=false`) to actually delete. It must resolve and strip `__ocx.keep.*` anti-deletion
  tags first, or the existing push-time guard silently defeats every prune.
  ```sh
  ocx package prune nightly --dry-run          # default; lists what --keep_n/--older_than would remove
  ocx package prune nightly --yes               # executes
  ocx package channel close pr-482 --yes        # MR-close hook, no policy involved
  ```
- **Pin unit = digest, not date** — OCX already resolves by digest (`ocx.lock`); do not invent a
  rustup-style dated-string pin. `ocx add pkg@nightly` follows the channel; `ocx add
  pkg@sha256:<digest>` (or a resolved `ocx.lock` entry) pins one build, closer to Deno's
  commit-hash model than rustup's date model, and reuses a mechanism that already exists.
- **Communicate removal via yank, never silent delete**, consistent with the existing
  D-yank-not-delete decision: a prune sets `yanked` on the dropped tag's index entry before (or
  instead of) removing registry bytes, so a consumer resolving an old digest gets a stated reason
  rather than a bare 404 — the gap Bun's canary shows is real when skipped.

## Sources

| Source | Relevance |
|---|---|
| [regclient.org regctl tag delete](https://regclient.org/cli/regctl/tag/delete/) | primitive delete has no retention flags |
| [google/go-containerregistry#1578](https://github.com/google/go-containerregistry/issues/1578) | crane digest-delete removes every co-tagged tag |
| [GitLab cleanup policies dev doc](https://docs.gitlab.com/development/packages/cleanup_policies/) | keep_n/older_than/name_regex_keep + evaluation order |
| [GitLab reduce container registry storage](https://docs.gitlab.com/user/packages/container_registry/reduce_container_registry_storage/) | user-facing cleanup policy UI/API |
| [Harbor tag retention rules](https://goharbor.io/docs/1.10/working-with-projects/working-with-images/create-tag-retention-rules/) | retain-N, glob patterns, Dry Run button |
| [snok/container-retention-policy README](https://github.com/snok/container-retention-policy/blob/main/README.md) | keep-n-most-recent, cut-off, dry-run input |
| [actions/delete-package-versions](https://github.com/actions/delete-package-versions) | min-versions-to-keep, ignore-versions, no upstream dry-run |
| [npm unpublish policy](https://docs.npmjs.com/policies/unpublish/) | 72h unpublish window |
| [cargo yank docs](https://doc.rust-lang.org/cargo/commands/cargo-yank.html) | yank never deletes bytes |
| [rustup channels](https://rust-lang.github.io/rustup/concepts/channels.html) | dated nightly pin syntax |
| [rustup overrides](https://rust-lang.github.io/rustup/overrides.html) | `rust-toolchain.toml` pin mechanism |
| [Deno upgrade docs](https://docs.deno.com/runtime/reference/cli/upgrade/) | `--canary --version <hash>` pin |
| [oven-sh/bun#17485](https://github.com/oven-sh/bun/discussions/17485) | Bun canary has no pinnable per-commit artifact |
| [pkg.pr.new announcement](https://blog.stackblitz.com/posts/pkg-pr-new/) | per-PR package never touches real registry |
| [GitLab review apps testing guide](https://docs.gitlab.com/development/testing_guide/review_apps/) | on_stop event trigger + 2-day auto-stop fallback |
| [Vercel deployment retention](https://vercel.com/docs/deployment-retention) | retention independent of PR state, 30/180-day defaults |
| `.agents/research/snapshot-lifecycle-{prior-art,codebase-recon}.md` | internal: registry mechanics; OCX has no delete, `--keep-tag` net, yank-not-delete |
