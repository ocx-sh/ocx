---
title: "Announce a package"
description: "Claim a package name in the index once, then announce each release so installs of <ns>/<pkg> resolve."
---
<!-- doc_type: how-to -->
<!-- doc_tier: everyday -->

This page gets a pushed package resolvable as `<ns>/<pkg>`. You claim the name once, then announce each release. You need
the package [pushed to a registry][authoring-building-pushing] and a credential with push access to the index repository. With `--fork`, push access to your fork is enough.
To run the announce step from a CI job, continue with [Announce from CI][announce-from-ci].

## Why the index needs an entry {#announcing-overview}

A registry holds anything: container images, Helm charts, whatever else your organization stores there.
[`ocx package push`][cmd-package-push] puts bytes in it. The registry cannot tell which repositories are packages, which you meant
to publish, or which tags are still fit to install.

Announcing writes that into an [index][in-depth-indices], the software catalog over the registry. The entry records that
`acme/widget` exists, which registry repository holds it and which tags it carries. It also records which tags are
[yanked or deprecated][in-depth-indices-status]. With the entry, [`ocx package install`][cmd-install] `acme/widget` resolves
for someone who was never told the host.

:::note[Same shape as winget]
[winget-pkgs][winget-pkgs] is the equivalent catalog for Windows. A publisher opens a review-gated pull request against a central
manifest repository. Only after it merges does the package appear in what `winget` resolves from. OCX takes the same shape,
minus the manifest-authoring step.
:::

Two commands write the entry, and the split is deliberate. Once the mapping exists, every later update from `acme/*` is a routine
refresh nobody reviews. So a human looks at the mapping itself, because someone has to judge that whoever claims `acme` plausibly
*is* ACME.

| Command | When | Reviewed by |
|---|---|---|
| [`ocx package claim`][cmd-package-claim] | Once per package, before its first announce | A person, always |
| [`ocx package announce`][cmd-package-announce] | Every release afterwards | The index's auto-merge rule, once the owners match |

`announce` never creates an entry. A package with no committed entry exits 79 and points you at `claim`. Neither command commits
to the index's default branch. Both arrive as a pull request (GitHub) or a merge request (GitLab).

## What the index entry records {#announcing-entry}

A claim writes one JSON entry into the index repository, at `p/<namespace>/<package>.json`. Four things go in it, and only two
come from you directly:

| Field | Where it comes from | Why it matters |
|---|---|---|
| The logical name | `<namespace>/<package>`, prefixed with the default registry | What a consumer types: `ocx add acme/widget`, `ocx package install acme/widget` |
| The physical repository | `--repository oci://HOST/PATH` | The registry repository every later announce resolves tags against |
| The owners | `--owner`, else the CI environment, else the credential's identity | The governance key, see [owners][announcing-owners] |
| The upstream block | `--upstream-org` and its two optional siblings | Only for a package that repackages someone else's software |

The `--repository` pointer names your **registry**, not the index. They are different systems: the registry holds the bytes, and
the index holds the mapping. A claim that points at the index repository would resolve to nothing.

Every later announce rewrites the entry's tag set in place. The owners and the repository pointer stay as the claim wrote them.
Changing either is a governance-sensitive edit that goes back to a person.

## Claiming the package {#announcing-claim}

```sh
export OCX_ANNOUNCE_TOKEN=<forge access token>
ocx package claim --repository oci://ghcr.io/acme/widget acme/widget
```

That is the whole minimum: a logical name and the registry repository behind it. Flags come before the positional. The full grammar is in the [`claim` reference][cmd-package-claim].

The claim opens a pull request, so it needs a forge credential in [`OCX_ANNOUNCE_TOKEN`][env-ocx-announce-token]. Without one, a bare
claim exits 80. Add `--output` to write the entry without opening anything. [Choosing a posture][announce-from-ci-postures] compares
the options, including `--fork`.

`--owner` names the accounts recorded as owners. On a fresh claim it **replaces** the detected list. On a re-claim of an
already-claimed package it adds them to the committed list instead. `--upstream-org`, with its optional
`--upstream-repository-url` and `--upstream-disclaimer`, marks a package that repackages someone else's software.

Every refusal that is decidable from the command line alone is decided before a credential is resolved and before anything is
dialled. That covers mutually exclusive write flags, a malformed `--repository` and an `--upstream-*` flag with no anchor. A bad
invocation costs you no round trip through a token you did not need.

The claimed unit is the **package**, not the namespace prefix. The entry lives at `p/<namespace>/<package>.json`, so `acme/gadget`
needs its own claim run even after `acme/widget` merged. A shared namespace prefix grants nothing on the ocx side.

Claiming `acme/widget` a second time, once its own entry has merged, is not an error. It is how a new maintainer adds themself to
`owners[]`, and it costs the release pipeline nothing to run unconditionally. See [who reviews it][announcing-review].

## Announcing tags {#announcing-tags}

An announce run re-observes a set of registry tags and rebuilds the entry from what it finds. Which tags make up that set is the
one required choice. The four modes differ in whether they *replace* the committed set or act only on the tags they are given:

| Mode | Effect on the committed set |
|---|---|
| `--tags 1.2.0,latest` | **Replaces** it. A committed tag not named here is dropped |
| `--tags-file <path>` | **Adds, updates or removes** the file's tags (comma- or newline-separated). A listed tag the registry no longer has is removed. A row the file does not list is left as it is |
| `--tags-from-registry` | **Adds** every tag the registry repository currently holds. Removes an ephemeral row whose tag is gone, and reports a durable one |
| `--refresh` | Adds no tag. Re-observes them all, picking up a digest that moved under a rolling tag like `latest`. Removes an ephemeral row whose tag is gone, and reports a durable one |

`--tags-file` is the mode a release pipeline wants, because [`ocx package push --tags-file <path>`][cmd-package-push] writes that
file as it pushes. The push records which tags it produced, and the announce step consumes the record instead of restating it.

```sh
ocx package push --tags-file tags.txt …
ocx package announce --tags-file tags.txt --fork acme-bot/index acme/widget
```

`--fork` opens the request from your own fork of the index. This is the same trust model as any open-source contribution, and no
index-side credential is ever required. Omit it and the announce branch is pushed to the index repository itself, which needs push
access there. When the publishing repository and the index share an owner, omit it. A repository cannot be forked into the
namespace that already owns it.

A run that changes nothing makes no commit and reports `unchanged`. A second announce for the same package updates the open
request in place rather than opening a second one. Announcing also picks up whatever
[`ocx package description push`][cmd-package-describe] last published (title, summary, keywords, README and logo), so a
description refresh needs no separate step.

Every announce also cleans up after itself, in the same commit. A moved tag's old dispatch object, a replaced README and a
replaced logo are deleted the moment the rebuilt entry stops referencing them. No flag is required, and the removals show up only
in the diff. Only an object the *previously committed* entry named is ever a candidate. An object orphaned by an announce that
ran before this behaviour shipped is not swept retroactively.

Yanking runs through the same command. `--yank <tag>` with a `--yank-reason` marks a tag as one that should no longer be
installed, and `--unyank <tag>` clears the marker. It is a publisher signal, not a delete. Resolving a yanked tag is refused by
default, and [`OCX_ALLOW_YANKED`][env-ocx-allow-yanked] opts back in. A resolve already pinned to the digest never needs that
opt-in, because immutable content cannot itself be yanked.

### Snapshot tags {#announcing-snapshots}

A snapshot build is a tag meant to disappear: a canary, or the build of an open change request. The index should list it while it
exists and stop listing it once the registry drops it. A durable row must never vanish unseen, so the index has to tell the two
kinds apart.

`--ephemeral` marks each row the run *adds* with `"ephemeral": true`. The marker never changes on a row already in the index, and
it has no effect on removals. Push ephemeral builds with `--no-keep-tag`, or [prune][cmd-package-prune] frees no storage. The keep
tag pins the manifest after the tag itself is gone.

Removal follows the registry. Announce reads the canonical registry, never a mirror or a cache. A given tag it reports as
`MANIFEST_UNKNOWN` is gone.

| Row in the committed entry | Reached by | Result |
|---|---|---|
| Ephemeral | any mode | Removed |
| Durable | `--tags`, `--tags-file` | Removed, because you named it. [Review][announcing-review] still applies on a governed index |
| Durable | `--refresh`, `--tags-from-registry` | Kept and listed in `durable_missing`, exit 0 |
| None | any mode | Exit 79, so a typo stays an error |

Schedule `--refresh` to converge the index, never `--tags-from-registry`. A snapshot that was pushed but not yet announced with
`--ephemeral` becomes a durable row under `--tags-from-registry`, and every later prune of it is refused. The whole loop, with CI
examples, is in [Snapshot tracks][ug-snapshot-tracks].

## Who the entry says owns it {#announcing-owners}

An owner is a `login:id` pair, and the **id** is the load-bearing half. Logins get renamed and recycled, but a numeric account id
does not. That is why the entry records both and why governance keys on the number.

Without `--owner`, ocx works down a short ladder. First come the CI environment's user variables (`GITLAB_USER_LOGIN` +
`GITLAB_USER_ID`, or `GITHUB_ACTOR` + `GITHUB_ACTOR_ID`, both halves required). Then comes the identity behind the credential.

Passing `--owner` **replaces** that result rather than adding to it. The invoking identity is not appended, so a run that names
owners names all of them. A bot account is refused at exit 64, because a package owned by a service account has nobody to ask
when something goes wrong.

The report's `owner_identity_source` says which rule produced the list, and it is **not uniform across forges**:

| Value | What it means | Where it happens |
|---|---|---|
| `resolved` | The forge's users API answered. Its canonical spelling, id and bot flag were taken from it | GitHub and GitLab |
| `ci_environment` | The CI variables named the list and the users API was out of reach, so nothing confirmed it | **GitLab only**, and only under a job token |
| `asserted` | The users API was out of reach and a `LOGIN:ID` pair was taken on your word | **GitLab only**, and only under a job token |

`asserted` and `ci_environment` are unreachable on GitHub. Both need the users API to be closed, and the only credential that
closes it is a GitLab job token. `asserted` applies when a `LOGIN:ID` was supplied, and `ci_environment` when the CI variables
named the list.

That is why the [job-token recipe][announcing-gitlab-job-token] passes `--owner` explicitly. Without an id, a bare login with no
reachable users API is a usage error naming the `LOGIN:ID` form. Guessing an id would write a stranger into a governance field.

The report also carries `author`. That is who opened the request, not who owns the package.

**Do not read `author` as an attestation.** Only the first rung is the forge speaking. That rung is the forge's answer about the
credential. The second rung is an ordinary read of `GITLAB_USER_LOGIN` or `GITHUB_ACTOR`, which an earlier pipeline step can set to
anything. The key does not say which rung answered.

## Who reviews it, and what happens after {#announcing-review}

On the public [`ocx-sh/index`][index-repo], a claim request is labelled `new-package` and carries a red `governance/review-required`
status that does not auto-resolve. A human approves and merges it. That review is the whole point of the human lane. Judging
whether whoever claims a name under `acme` plausibly *is* ACME is not automatable, and the claim command deliberately does not
try.

ocx verifies no ownership of its own at any point, and every privileged check runs in the index's CI. How much scrutiny a second
package under an already-reviewed prefix gets is that index's policy, not something ocx decides.

What the owner list buys you is everything *after* that merge. The index's auto-merge rule requires the announcing identity to own
every root a request touches. An [`ocx package announce`][cmd-package-announce] run whose credential resolves to an account id in
`owners[]` merges without a human. One that does not waits for a reviewer.

A change touching a governance-sensitive field always goes back to a person, whichever command made it. The
[governance contracts reference][index-governance-contracts] documents which fields route to which lane. Getting the owner list
right at claim time is what makes your release pipeline unattended later. A self-hosted index sets its own rules, so check with
whoever runs it.

Removing a row has its own lane. On a bot-governed index, the bot merges the removal of an [ephemeral row][announcing-snapshots]
once it confirms `MANIFEST_UNKNOWN` from the canonical registry. If the tag is still present, or the bot cannot check, a person
reviews it.

The removal of a durable row always goes to a person, whichever identity announced it. Adding or changing the `ephemeral` marker on
an existing row is reviewed too. With `--output`, or on an index no bot governs, nothing reviews the change. A durable tag you name
is then removed on your word, and a durable row you did not name is never touched.

Re-running a claim is always safe, before or after its request merges. Nothing new to say reports `status: unchanged` and opens no
request. A genuine change, such as a new `--owner` or a moved description, adds itself to the committed entry and reports `updated`.

Neither path ever fails just because the package is already claimed. Two refusals still exist: a name mismatch and a repository
mismatch on a re-claim. Both name what disagreed rather than pointing at `announce`.

## What your consumers need {#announcing-consumers}

Nothing. The [compiled-in defaults][config-registries-index] name `https://index.ocx.sh` as the index for the `ocx.sh`
namespace. So [`ocx package install`][cmd-install] `<ns>/<pkg>` resolves through the [public index][in-depth-indices-public] on a machine
with no OCX config at all. A consumer opts *out*, not in: `index = ""`, or a [`[mirrors]`][config-mirrors] entry pinning `ocx.sh`
at a registry endpoint.

That authority is exclusive, and it cuts both ways. The index is the sole resolver for `ocx.sh/…`, which is what lets a yank
reach everyone who installs the package. So an `index.ocx.sh` outage fails those installs outright rather than falling through to
whatever registry happens to serve a repository under the same name. An announced package inherits the index's availability along
with its guarantees.

## Check that the package resolves {#announcing-verify}

Once the request has merged, install the package by its logical name:

```sh
ocx package install acme/widget
```

A successful install resolves `acme/widget` through the index to the registry repository you claimed, and exits 0. A name the index does not know yet fails instead, so a failure before the merge is expected.

## Troubleshooting {#announcing-troubleshooting}

Every failure below is an [exit code][exit-codes] you can branch on, not a string to grep. Codes marked *claim* or *announce* are
reachable from that command only. The rest are shared.

| Exit | What ocx says | What to do |
|---|---|---|
| 64 | `--output` and `--fork` cannot both be given, or `--transport git` was given with `--fork`, with `--output`, or against a GitHub forge | Drop one. `--output` opens no request, so a fork has nothing to open from. The `git` transport pushes to the index itself and is GitLab-only |
| 64 | `--forge` is required for a self-hosted host | A hostname says nothing about which forge runs behind it, and a wrong guess would send your credential to the wrong API. Say `--forge gitlab` or `--forge github` |
| 64 | *claim*: `malformed --repository`, expected `oci://host/path` | The pointer names the registry repository, scheme included: `oci://ghcr.io/acme/widget`, not `ghcr.io/acme/widget` and not the index |
| 64 | *claim*: `--owner` is neither a `LOGIN` nor a `LOGIN:ID` pair | The id is a non-negative whole number, split on the first colon. `alice:7`, not `alice:7:8` and not `:7` |
| 64 | *claim*: `--upstream-repository-url` must be an http or https URL without embedded credentials | The refusal never echoes the value, because the most likely one is a forwarded `CI_REPOSITORY_URL` whose userinfo is a live job token. Pass the public URL |
| 64 | *claim*: `no acting identity`, `the forge users API is not reachable with this credential; give the owner as LOGIN:ID`, or `owner … is a bot account` | Pass `--owner LOGIN:ID` naming a human. On a job token the users API is closed, so the pair is required, as the [job-token recipe][announcing-gitlab-job-token] shows |
| 64 | *announce*: a `--tags` or `--tags-file` curated set resolved to nothing but reserved tags | Every tag named was an OCX-internal `__ocx` or legacy keep tag. Name a real version. With `--refresh` or `--tags-from-registry`, an empty or wholly reserved set is not a refusal. The run proceeds as a description-only pass and reports the drop in `reserved_tags_dropped` |
| 65 | *claim*: the committed entry names another package | Check the identifier. `<namespace>/<package>` decides the expected name, and a wrong registry or a renamed package both surface here |
| 65 | *claim*: `--repository` disagrees with the committed pointer | The pointer decides where a package's bytes come from and is never repointed as a side effect of adding an owner. Pass the current value, which the error names. If the repository genuinely changed, reach an existing owner to move the pointer deliberately |
| 65 | *announce*: the committed root's `name` disagrees with the identifier this run announces | Either the registry you typed differs from the one the root already committed, or the identifier itself changed. A publisher whose configured default registry differs from the index domain must type the full identifier. No `[registries."<domain>"]` entry is required to announce |
| 65 | *announce*: the recorded description no longer exists on the registry, or an unchanged run's open request can no longer merge | Republish the description with [`ocx package description push`][cmd-package-describe]. For the second case, close the request or delete the branch and announce again |
| 69 | The forge returned a 5xx other than 502, 503 or 504, dropped the connection after accepting it, or presented a TLS certificate the verifier refused. Or no `git` was found, or it is older than 2.31.0 | Check the forge's status. For the git case, install a newer git. The floor is checked before the forge is even constructed |
| 74 | Writing under `--output` failed, or `--tags-file` could not be read | Check the path's permissions. Parent directories are created for you, but a parent that is a regular file is not |
| 75 | The forge's API could not be connected to, timed out, or answered 408, 429, 502, 503 or 504 (a `git` clone or push that cannot reach it exits 1). Or the registry could not be resolved or connected to, or timed out. Or a concurrent run kept winning the branch | Retry with backoff |
| 77 | The push was refused by the forge's own policy | A protected branch or a push rule on the index project. Its administrator has to relax it, or use `--transport api` with a token that may push |
| 77 | `forge_publisher_not_allowlisted`: neither of the index project's job-token allowlists admits your project or one of its groups | Only an administrator of the index project can add it. `--transport api` with a stored token is the way around it |
| 78 | *announce*: a curated tag's physical host resolves to a private, loopback, link-local or metadata address | Add it to that namespace's [`trusted_hosts`][config-registries-trusted-hosts]. The entry is keyed on the package name's domain, not the registry host: `[registries."ocx.corp.example"] trusted_hosts = ["gitlab.corp.example"]`. The error names the exact key |
| 79 | *announce*: a curated tag does not resolve on the registry, or the package is unclaimed | Check the tag for a typo. For the second case, run [`ocx package claim`][cmd-package-claim] first |
| 79 | *claim*: `unknown owner …: the forge has no such account` | Check the spelling, or pass `LOGIN:ID` to skip the lookup |
| 80 | No credential, a rejected one, or one that cannot push to `--index-repo` | Set [`OCX_ANNOUNCE_TOKEN`][env-ocx-announce-token]. Without `--fork` the credential also needs push access, which ocx checks up front and names |
| 82 | A capability the transport needs is absent | Job-token pushes are disabled on the index project. Only an administrator of the index project can enable them. `--transport api` with a stored token is the way around it |

`--output` is the way to see what a run would commit without opening anything. It needs no credential and writes the whole entry
every time, including on a run that changes nothing.

:::tip[Learn more]
- [Announce from CI][announce-from-ci]: the credential postures and runner hygiene for announcing on every release.
- [Building and pushing packages][authoring-building-pushing]: the push workflow that produces the `--tags-file` an announce consumes.
- [Indices in depth, writing to an index][in-depth-indices-writing]: the transport and credential model behind both commands.
- [index.ocx.sh][in-depth-indices-public]: how the public index resolves an announced package back into an install.
:::

<!-- external -->
[index-repo]: https://github.com/ocx-sh/index
[index-governance-contracts]: https://index.ocx.sh/docs/reference/governance-contracts
[winget-pkgs]: https://github.com/microsoft/winget-pkgs

<!-- commands -->
[cmd-package-prune]: ../reference/command-line/package-consuming.md#package-prune
[cmd-package-claim]: ../reference/command-line/package-publishing.md#package-claim
[cmd-package-announce]: ../reference/command-line/package-publishing.md#package-announce
[cmd-package-push]: ../reference/command-line/package-publishing.md#package-push
[cmd-package-describe]: ../reference/command-line/package-publishing.md#package-description-push
[cmd-install]: ../reference/command-line/package-consuming.md#package-install
[exit-codes]: ../reference/exit-codes.md

<!-- environment -->
[env-ocx-announce-token]: ../reference/environment.md#ocx-announce-token
[env-ocx-allow-yanked]: ../reference/environment.md#ocx-allow-yanked

<!-- configuration -->
[config-registries-index]: ../reference/configuration.md#keys-registries-index
[config-registries-trusted-hosts]: ../reference/configuration.md#keys-registries-trusted-hosts
[config-mirrors]: ../reference/configuration.md#keys-mirrors

<!-- internal -->
[announce-from-ci]: ./announce-from-ci.md
[announce-from-ci-postures]: ./announce-from-ci.md#announcing-postures
[announcing-gitlab-job-token]: ./announce-from-ci.md#announcing-gitlab-job-token
[announcing-owners]: #announcing-owners
[announcing-review]: #announcing-review
[announcing-snapshots]: #announcing-snapshots
[ug-snapshot-tracks]: ./snapshot-tracks.md
[in-depth-indices]: ../explanation/indices.md
[in-depth-indices-public]: ../explanation/public-index.md#public-index
[in-depth-indices-status]: ../explanation/public-index.md#public-index-status
[in-depth-indices-writing]: ../explanation/indices.md#writing
[authoring-building-pushing]: ./build-and-push.mdx#first-push
