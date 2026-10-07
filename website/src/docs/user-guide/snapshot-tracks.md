---
outline: deep
---
<!-- doc_type: how-to -->
<!-- doc_tier: integration -->

# Snapshot tracks {#snapshot-tracks}

This guide shows how to publish snapshot builds of a package, such as a nightly canary or one build per open change request. It also shows how to delete them again from the registry and from the index.

A snapshot build is a tag that should not outlive its purpose. A pipeline that pushes one per commit fills the registry with tags nobody installs. Deleting them by hand is error-prone, and a deleted release cannot be undone.

Two commands split the work. [`ocx package prune`][cmd-package-prune] deletes tags from the registry, and only tags the index marks as ephemeral. [`ocx package announce`][cmd-package-announce] is the only command that writes the index, and it removes the rows of tags the registry no longer has.

A **track** is one pre-release line, such as `0.5.0-canary` or `0.5.0-mr42`. Its builds are `0.5.0-canary_<build>` tags, and the rolling tag `0.5.0-canary` points at the newest one.

## Push snapshots {#snapshot-tracks-push}

Push each snapshot with a fixed-width build id and without a keep tag.

```sh
ocx package push --platform linux/amd64 --cascade --no-keep-tag --build-timestamp=datetime \
  --tags-file tags.txt -i "$REG:0.5.0-canary" dist/tool.tar.xz
```

`--build-timestamp=datetime` appends `_YYYYMMDDhhmmss` to the tag. `--cascade` moves the rolling tag `0.5.0-canary` to the new build. A pre-release build cascades only into its own pre-release, so `0.5`, `0` and `latest` never move. `--tags-file` appends the pushed tag and the rolling tag to `tags.txt`, one per line.

### Skip the keep tag {#snapshot-tracks-keep-tag}

Push ephemeral builds with `--no-keep-tag`, or prune frees no storage.

A push writes a `__ocx.keep.<algorithm>-<hex>` tag for each platform manifest by default. The keep tag exists so that a stray tag delete cannot orphan a manifest something else still uses. That protection is what defeats a prune.

Prune deletes only the tag it names. If a keep tag is left on a platform manifest, the manifest and its layers stay reachable, and the registry's garbage collection never reclaims them. Neither prune nor announce checks for keep tags. The failure is silent: every command succeeds and the registry keeps growing.

### Use fixed-width build ids {#snapshot-tracks-build-ids}

`--keep-builds` decides which builds are newest by version order. That order compares the build segment as text, so the ids must all have the same width. `--build-timestamp=datetime` does.

`--build-timestamp=date` does not. Two builds on one day share a tag, and the second push overwrites the first.

## Canary track {#snapshot-tracks-canary}

A canary is rebuilt from the default branch on every commit, and only the newest few builds are worth keeping. The pipeline pushes, prunes the old builds, repairs the rolling tags, and announces the change in one index update.

```sh
rm -f tags.txt
ocx package push --platform linux/amd64 --cascade --no-keep-tag --build-timestamp=datetime \
  --tags-file tags.txt -i "$REG:0.5.0-canary" dist/tool.tar.xz
ocx package prune --prerelease 0.5.0-canary --keep-builds 1 --tags-file tags.txt "$PKG"
ocx package cascade repair --tags-file tags.txt "$REG"
ocx package announce $FORGE --tags-file tags.txt --ephemeral --output index-out "$PKG"
```

The recipes use three shell variables.

- `$PKG` is the package as the index names it, such as `ocx.acme.example/acme/tool`.
- `$REG` is the registry repository the tags live in, such as `registry.acme.example/acme/tool`.
- `$FORGE` names the index repository, such as `--index-repo gitlab.example.com/platform/ocx-index --forge gitlab`.

Announce reads the committed entry from that index repository even when `--output` writes the result to a directory. All four commands share one tags file, and each appends to it.

- **push** creates the new build and moves `0.5.0-canary` onto it.
- **prune** deletes every older build of `0.5.0-canary` and keeps the newest one and the rolling tag. It appends each deleted tag that the index lists to the file.
- **cascade repair** moves nothing here, because the kept rolling tag already points at the newest build. It is part of the recipe for a prune that does delete a rolling tag's source.
- **announce** adds the new build as an ephemeral row, refreshes the rolling row, and removes the rows of the deleted builds.

`--output index-out` writes the rebuilt index entry to a local directory instead of opening a request. In a pipeline, drop `--output` so announce opens a request against the index repository, as the [CI examples](#snapshot-tracks-ci) do.

The rolling row keeps the marker it already has. `--ephemeral` marks only the rows a run adds and never changes an existing row.

Prune reads the served index before it deletes anything. It deletes a tag only when the index lists that tag as ephemeral, so the first run of a new track has nothing to delete. See [what prune refuses](#snapshot-tracks-refusals).

## Tear down a change request {#snapshot-tracks-teardown}

A change request gets its own track, such as `0.5.0-mr42`, and the track should disappear when the request closes. Prune selects every `0.5.0-mr42_*` build and then `0.5.0-mr42` itself.

```sh
rm -f tags.txt
ocx package prune --prerelease 0.5.0-mr42 --tags-file tags.txt "$PKG"
ocx package announce $FORGE --tags-file tags.txt --output index-out "$PKG"
```

Every build and the rolling tag are deleted from the registry, and announce removes their rows. `latest`, `0.5` and `0` are untouched.

Announce needs no `--ephemeral` here, because it adds nothing.

### Rerun a teardown {#snapshot-tracks-rerun}

A teardown can stop halfway, or run twice. Rerun it. A prune that finds the registry already empty selects nothing, exits 0 and writes an empty tags file.

```sh
rm -f tags.txt
ocx package prune --prerelease 0.5.0-mr42 --tags-file tags.txt "$PKG"
ocx package announce $FORGE --tags-file tags.txt --output index-out "$PKG"
ocx package announce $FORGE --refresh --output index-out "$PKG"
```

The first announce receives the empty file. It exits 0 and changes nothing, before any forge or registry work. The `--refresh` announce re-observes every row in the index and removes each ephemeral row whose tag is gone. That is how rows a lost run left behind are cleaned up.

## Explicit lists and lazy sync {#snapshot-tracks-lazy}

Two variants need no structural selection.

**Explicit list.** Your own policy computes the tags, and prune deletes exactly those. Any tag is allowed, including a tag that does not belong to a snapshot track.

```sh
./compute-stale-tags.sh | xargs ocx package prune --tags-file tags.txt "$PKG"
ocx package announce $FORGE --tags-file tags.txt --output index-out "$PKG"
```

**Lazy sync.** A teardown job only prunes, and a scheduled job converges the index.

```sh
ocx package prune --prerelease 0.5.0-mr42 "$PKG"
ocx package announce $FORGE --refresh --output index-out "$PKG"
```

Schedule `--refresh` to converge the index. Never schedule `--tags-from-registry` for that. It adds every tag the registry holds as a durable row, so a snapshot that was pushed but not yet announced with `--ephemeral` becomes durable. Prune then refuses it with exit 81.

## What prune refuses {#snapshot-tracks-refusals}

Prune checks every selected tag against the served index root before the first delete.

| Selected tag | Result |
|---|---|
| Listed with the ephemeral marker | Deleted |
| Listed without the marker | Exit 81. The run stops and nothing is deleted |
| Not listed, but in the registry | Exit 75. The run stops and nothing is deleted |
| Not listed and not in the registry | Reported as `absent` |

Exit 81 protects releases. `--force` deletes such a tag anyway, and the index row stays until a reviewed announce removes it. Without an index to ask, prune also exits 81 until you pass `--force`.

Exit 75 usually means the build's announce has not merged yet. Retry once it has. `--dry-run` runs the same check, exits the same way, and deletes nothing.

### Recover a build that was never announced {#snapshot-tracks-recover}

Suppose the push succeeded but the announce failed, and a whole-job retry pushed a fresh build. The first build is in the registry and not in the index. Every prune that selects it exits 75.

Announce it as an ephemeral row. Put its tag in the file, and prune can then delete it on the next run.

```sh
printf '%s\n' 0.5.0-canary_20260930101500 >> tags.txt
ocx package announce $FORGE --tags-file tags.txt --ephemeral --output index-out "$PKG"
```

Prune never announces on its own, because announce is the only command that writes the index. Splitting the push job from the announce job, as the CI examples do, keeps a retry from pushing twice.

## Serialize each track {#snapshot-tracks-serialize}

A delete is not conditional. Prune without `--keep-builds` deletes the rolling tag, and a push to the same track may cascade into it at the same moment. Build tags are unique, so builds cannot race. The rolling tag can.

Run the jobs of one track one at a time. [GitLab CI][gitlab-ci] has [`resource_group`][gitlab-resource-group], and [GitHub Actions][github-actions] has [`concurrency`][github-concurrency].

## In CI {#snapshot-tracks-ci}

Both examples split the push from the announce. A retried announce job reuses the pushed `tags.txt`, so it never pushes a second build. Registry credentials need push and delete rights on the repository. Set them with [`OCX_AUTH_<REGISTRY>_TYPE`][env-auth-type], `_USER` and `_TOKEN`. The index credential is [`OCX_ANNOUNCE_TOKEN`][env-announce-token].

Retry exit 75. Never retry exit 69 or 82.

### GitLab CI {#snapshot-tracks-ci-gitlab}

```yaml
variables:
  PKG: ocx.acme.example/acme/tool
  REG: registry.gitlab.example.com/acme/tool
  FORGE: --index-repo gitlab.example.com/platform/ocx-index --forge gitlab
  OCX_AUTH_registry_gitlab_example_com_TYPE: basic

.retry: &retry
  retry: { max: 2, exit_codes: [75] }

canary-push:
  rules: [{ if: $CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH }]
  resource_group: snapshot-canary
  script:
    - ocx package push --platform linux/amd64 --cascade --no-keep-tag --build-timestamp=datetime --tags-file tags.txt -i "$REG:0.5.0-canary" dist/tool.tar.xz
  artifacts: { paths: [tags.txt] }

canary-publish:
  <<: *retry
  rules: [{ if: $CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH }]
  needs: [canary-push]
  resource_group: snapshot-canary
  script:
    - ocx package prune --prerelease 0.5.0-canary --keep-builds 10 --tags-file tags.txt "$PKG"
    - ocx package cascade repair --tags-file tags.txt "$REG"
    - ocx package announce $FORGE --tags-file tags.txt --ephemeral "$PKG"

mr-push:
  rules: [{ if: $CI_PIPELINE_SOURCE == "merge_request_event" }]
  resource_group: snapshot-mr-$CI_MERGE_REQUEST_IID
  environment: { name: snapshot/mr-$CI_MERGE_REQUEST_IID, on_stop: mr-teardown, auto_stop_in: 3 days }
  script:
    - ocx package push --platform linux/amd64 --cascade --no-keep-tag --build-timestamp=datetime --tags-file tags.txt -i "$REG:0.5.0-mr$CI_MERGE_REQUEST_IID" dist/tool.tar.xz
  artifacts: { paths: [tags.txt] }

mr-announce:
  <<: *retry
  rules: [{ if: $CI_PIPELINE_SOURCE == "merge_request_event" }]
  needs: [mr-push]
  resource_group: snapshot-mr-$CI_MERGE_REQUEST_IID
  script:
    - ocx package announce $FORGE --tags-file tags.txt --ephemeral "$PKG"

mr-teardown:
  <<: *retry
  rules: [{ if: $CI_PIPELINE_SOURCE == "merge_request_event", when: manual }]
  resource_group: snapshot-mr-$CI_MERGE_REQUEST_IID
  environment: { name: snapshot/mr-$CI_MERGE_REQUEST_IID, action: stop }
  variables: { GIT_STRATEGY: none }
  script:
    - rm -f tags.txt
    - ocx package prune --prerelease "0.5.0-mr$CI_MERGE_REQUEST_IID" --tags-file tags.txt "$PKG"
    - ocx package announce $FORGE --tags-file tags.txt "$PKG"

index-refresh:
  rules: [{ if: $CI_PIPELINE_SOURCE == "schedule" }]
  script:
    - ocx package announce $FORGE --refresh "$PKG"
```

The `environment` block ties the teardown to [GitLab's `on_stop`][gitlab-environments], so closing the merge request runs it. `auto_stop_in` runs it for you after three days.

### GitHub Actions {#snapshot-tracks-ci-github}

```yaml
on:
  pull_request: { types: [opened, synchronize, reopened, closed] }
  schedule: [{ cron: "17 3 * * *" }]

concurrency:
  group: snapshot-${{ github.event.number || 'schedule' }}
  cancel-in-progress: false

env:
  PKG: ocx.acme.example/acme/tool
  REG: registry.acme.example/acme/tool
  FORGE: --index-repo github.com/acme/ocx-index --forge github

jobs:
  push:
    if: github.event_name == 'pull_request' && github.event.action != 'closed'
    runs-on: ubuntu-latest
    outputs: { tags: "${{ steps.push.outputs.tags }}" }
    steps:
      - uses: ocx-sh/setup-ocx@v1
      - id: push
        run: |
          ocx package push --platform linux/amd64 --cascade --no-keep-tag --build-timestamp=datetime \
            --tags-file tags.txt -i "$REG:0.5.0-pr${{ github.event.number }}" dist/tool.tar.xz
          echo "tags=$(paste -sd, tags.txt)" >> "$GITHUB_OUTPUT"

  announce:
    needs: push
    runs-on: ubuntu-latest
    steps:
      - uses: ocx-sh/setup-ocx@v1
      - run: |
          printf '%s\n' "$TAGS" > tags.txt
          ocx package announce $FORGE --tags-file tags.txt --ephemeral "$PKG"
        env:
          TAGS: ${{ needs.push.outputs.tags }}
          OCX_ANNOUNCE_TOKEN: ${{ secrets.OCX_INDEX_TOKEN }}

  teardown:
    if: github.event_name == 'pull_request' && github.event.action == 'closed'
    runs-on: ubuntu-latest
    steps:
      - uses: ocx-sh/setup-ocx@v1
      - run: |
          ocx package prune --prerelease "0.5.0-pr${{ github.event.number }}" --tags-file tags.txt "$PKG"
          ocx package announce $FORGE --tags-file tags.txt "$PKG"
        env:
          OCX_ANNOUNCE_TOKEN: ${{ secrets.OCX_INDEX_TOKEN }}

  index-refresh:
    if: github.event_name == 'schedule'
    runs-on: ubuntu-latest
    steps:
      - uses: ocx-sh/setup-ocx@v1
      - run: ocx package announce $FORGE --refresh "$PKG"
        env:
          OCX_ANNOUNCE_TOKEN: ${{ secrets.OCX_INDEX_TOKEN }}
```

The workflow-level `concurrency` group serializes each pull request's runs. The registry login for the push and the delete is omitted. Use the credential variables above.

## Registry support {#snapshot-tracks-registries}

Prune needs a registry that deletes tags. Tag deletion is optional in the [OCI distribution spec][oci-delete-tags]. A registry that refuses it fails the first delete with exit 82, `error.kind` `unsupported` and `error.detail` `registry_delete_unsupported`. Nothing was deleted.

Measured against running registries:

| Registry | Tag delete | Prune |
|---|---|---|
| [zot][zot] v2.1.18 | Answers 202 and removes the tag | Works |
| [`registry:2`][distribution] with delete disabled | Answers 405 `UNSUPPORTED` | Exit 82 |
| [`registry:2`][distribution] with delete enabled | Answers 400 `DIGEST_INVALID` and the tag stays | Exit 82 |

[GHCR][ghcr], [Docker Hub][docker-hub] and [Amazon ECR][ecr] do not offer tag deletion through the registry API, so prune exits 82 against them. This comes from their documentation, and no run against them is recorded here.

Exit 82 is never transient, so do not retry it. Use a registry that deletes tags, or delete through the registry's own tooling and skip prune.

## Limits {#snapshot-tracks-limits}

- **An emptied repository cannot be cleaned up by announce.** Some registries drop a repository after its last tag. Every lookup then answers `NAME_UNKNOWN`, and announce does not read that as "gone". It exits 79 and removes nothing. Remove those rows with a reviewed change to the index repository. zot and `registry:2` keep the repository after its last tag.
- **Prune deletes tags, never content.** It leaves digests, platform manifests, referrers and keep tags alone. The registry's garbage collection reclaims storage.
- **Prune never writes the index.** A removal reaches the index only through an announce.

## In depth {#snapshot-tracks-in-depth}

- [`ocx package prune`][cmd-package-prune]: every flag, the safeguard and the exit codes.
- [`ocx package announce`][cmd-package-announce]: the removal rules and the `removed` and `durable_missing` report keys.
- [Announcing a package][authoring-announcing-snapshots]: how a governed index reviews removals.
- [Versioning and cascades][ug-versioning]: why a pre-release build cascades only into its own track.

<!-- commands -->
[cmd-package-prune]: ../reference/command-line.md#package-prune
[cmd-package-announce]: ../reference/command-line.md#package-announce

<!-- environment -->
[env-auth-type]: ../reference/environment.md#ocx-auth-registry-type
[env-announce-token]: ../reference/environment.md#ocx-announce-token

<!-- in-depth -->
[authoring-announcing-snapshots]: ../authoring/announcing.md#announcing-snapshots
[ug-versioning]: ../in-depth/versioning.md#cascades

<!-- external -->
[oci-delete-tags]: https://github.com/opencontainers/distribution-spec/blob/main/spec.md#deleting-tags
[zot]: https://zotregistry.dev/
[distribution]: https://distribution.github.io/distribution/
[ghcr]: https://docs.github.com/en/packages/working-with-a-github-packages-registry/working-with-the-container-registry
[docker-hub]: https://hub.docker.com/
[ecr]: https://aws.amazon.com/ecr/
[gitlab-ci]: https://docs.gitlab.com/ci/
[gitlab-resource-group]: https://docs.gitlab.com/ci/resource_groups/
[gitlab-environments]: https://docs.gitlab.com/ci/environments/
[github-actions]: https://github.com/features/actions
[github-concurrency]: https://docs.github.com/en/actions/writing-workflows/choosing-what-your-workflow-does/control-the-concurrency-of-workflows-and-jobs
