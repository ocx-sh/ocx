# Claim the pilot namespace (owner action, one human click)

The Track D live sequence is blocked on exactly one thing:
`p/michael-herwig/ocx-e2e-hello.json` does not exist on `ocx-sh/index`. This
file is everything needed to open that pull request, ready to paste.

Nothing here has been executed. The root has not been created and no pull
request has been opened.

## 1. The entry

Path in the index repo: `p/michael-herwig/ocx-e2e-hello.json`

```json
{
  "name": "ocx.sh/michael-herwig/ocx-e2e-hello",
  "repository": "oci://ghcr.io/michael-herwig/ocx-e2e-hello",
  "owners": [
    {
      "github": "michael-herwig",
      "github_id": 3511590
    }
  ],
  "status": "active",
  "deprecated_message": null,
  "created": "2026-07-25",
  "desc": null,
  "tags": {}
}
```

Where each value comes from — authority, not memory:

| Field | Value | Source |
|---|---|---|
| `name` | `ocx.sh/michael-herwig/ocx-e2e-hello` | `schema/root.schema.json` requires it to equal the path-derived logical name (G-02) |
| `repository` | `oci://ghcr.io/michael-herwig/ocx-e2e-hello` | the publisher's own push target: `identifier="ghcr.io/michael-herwig/ocx-e2e-hello:${TAG}"`, `.github/workflows/e2e-publish.yml:140` in `ocx-e2e-publisher` @ `76e24e2`. `ghcr.io` is the only host in `REPOSITORY_HOST_ALLOWLIST` (`bot/src/indexbot/core/validate_entry.py:59`), so G-03 passes |
| `owners[].github_id` | `3511590` | `gh api users/michael-herwig --jq .id`. The numeric id is the ownership key — a login is renameable and recyclable |
| `status` | `active` | schema enum; `active` is what a new claim uses |
| `deprecated_message` | `null` | required by schema even when unset |
| `created` | `2026-07-25` | today. Set once, never updated |
| `desc` | `null` | nothing observed yet; the bot fills this from `__ocx.desc` on the first announce |
| `tags` | `{}` | claiming ahead of the first announce. **Do not hand-write tag entries** — `content`/`observed` are bot-regenerated fields |
| `upstream` | *omitted* | governance requires it only where the namespace names a real third-party vendor. This is the owner's own test package, not a vendor mirror — same shape as `schema/fixtures/valid/root-valid-first-party-no-upstream-null-desc.json` |

Field order matches the bot's own serializer, so the first announce produces a
clean diff (only `tags` changes) rather than a reformat.

### Validation already run against the real schema

From `.agents/worktrees/index-integration` (branch `announce/b`, what merged as
index main `98f9aa32`), against a scratch copy — nothing was written into `p/`:

```
check-jsonschema --schemafile schema/root.schema.json <scratch>   ok -- validation done
G-02 name matches path          : ok
reserved-segment check          : ok
G-03 repository host allowlisted: ok
byte-identical to bot serializer: True
```

Negative control, to prove the validator was actually checking: dropping
`github_id` from the owner fails with
`$.owners[0]: 'github_id' is a required property`.

## 2. Opening the pull request

```sh
# From a checkout of your ocx-sh/index fork.
git checkout main && git pull
git checkout -b claim/michael-herwig-ocx-e2e-hello

mkdir -p p/michael-herwig
cat > p/michael-herwig/ocx-e2e-hello.json <<'JSON'
{
  "name": "ocx.sh/michael-herwig/ocx-e2e-hello",
  "repository": "oci://ghcr.io/michael-herwig/ocx-e2e-hello",
  "owners": [
    {
      "github": "michael-herwig",
      "github_id": 3511590
    }
  ],
  "status": "active",
  "deprecated_message": null,
  "created": "2026-07-25",
  "desc": null,
  "tags": {}
}
JSON

git add p/michael-herwig/ocx-e2e-hello.json
git commit -m "feat(index): claim michael-herwig/ocx-e2e-hello"
git push -u origin claim/michael-herwig-ocx-e2e-hello

gh pr create \
  --repo ocx-sh/index \
  --base main \
  --title "Claim michael-herwig/ocx-e2e-hello" \
  --body "First claim for the Track D announce E2E pilot package.

Physical mirror: ghcr.io/michael-herwig/ocx-e2e-hello (pushed by
michael-herwig/ocx-e2e-publisher). Claiming ahead of the first announce, so
tags is {} — the bot populates it.

Validated locally against schema/root.schema.json plus the bot's own
check_name_matches_path / check_namespace_not_reserved /
check_repository_allowlisted."
```

Expect `schema-validate` green and `governance-gate` to apply the
`new-package` label with a red `governance/review-required` status. That red
status is correct and does not auto-resolve — approve and merge by hand.

## 3. Why a human has to click this

Ratified ruling **R3**: announce refuses an unclaimed namespace and never
creates roots itself, so the first claim is a human-lane (G-04) prerequisite
that no driver in this directory can satisfy. Automating it would defeat the
one governance control the whole exercise exists to prove — a reviewer
judging whether the claimed namespace plausibly belongs to the entity it
names, which is not automatable by construction.

## 4. What unblocks the moment it merges

```sh
export GH_REPO_PUBLISHER=michael-herwig/ocx-e2e-publisher
export GH_REPO_INDEX=ocx-sh/index
export INDEX_FORK=michael-herwig/index
export E2E_NAMESPACE=michael-herwig
export E2E_PACKAGE=ocx-e2e-hello
export E2E_INDEX_PREFIX=ocx.sh
export BOT_ACTOR_IDS=41898282

./scripts/run_sequence.sh <tag>
```

`BOT_ACTOR_IDS=41898282` is `github-actions[bot]` (`gh api
users/github-actions[bot] --jq .id`). G-19 auto-merge is armed with the default
`GITHUB_TOKEN` (`.github/workflows/validate.yml:208`,
`gh pr merge --auto --squash`), so that is the identity a machine-lane merge is
recorded under. **Verify it against the first real merged PR before trusting
the machine-lane proof** — if the index later moves to a PAT-backed machine
account, this id changes and `classify_lane` would report `human` for a
genuine bot merge.

G-19 also requires the announcing identity to own every touched root. The
announce runs under `OCX_ANNOUNCE_TOKEN`, the owner's PAT, and `owners[]`
above carries `3511590` — so that precondition is satisfied by this entry as
written.

## 5. Still blocked after the merge

- **The clean-install leg needs an ocx carrying index-kind config.** Released
  ocx 0.4.3 rejects the config the clean-machine image writes:
  `unknown field 'index', expected 'url'` — proven in-container. Point
  `OCX_BINARY` at a dev-channel-equivalent build before running
  `clean_install_check.sh`; the default `target/release/ocx` works only if it
  is such a build.
- **The publisher's announce step passes no `--package`.**
  `ocx-e2e-publisher` `.github/workflows/e2e-publish.yml:228` invokes
  `ocx package announce --tags-file … --fork … --index-repo …`, but the
  shipped CLI declares `--package` as `required = true`
  (`crates/ocx_cli/src/command/package_announce.rs:38`). That is a clap usage
  error, not a soft skip. Track C's repo owns the fix; the sequenced and
  machine-lane drivers both depend on that step producing a pull request.
- **Everything downstream of "the HTTPS request leaves the container"** is
  unproven — that `index.ocx.sh` serves the rendered root, that a real root
  resolves through to a registry pull, and that
  `clean_install_check.sh`'s `grep -q "$E2E_PACKAGE"` matches a real success
  payload. That last one is the most likely false negative on the first run;
  it now greps the redacted log stream, not the raw one.
