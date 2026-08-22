# GitLab announce E2E

One driver that proves the GitLab announce lane works end to end against a
**real** GitLab instance. Nothing here is pytest-collected; it is run by hand,
and it creates and then deletes a throwaway project on the account whose token
you give it.

The unattended coverage lives elsewhere and is not a substitute for this: the
GitLab client is exercised against a fake forge that serves the real REST shapes
(`test/tests/test_announce_gitlab.py` over `test/tests/fake_gitlab.py`), and both
forges are held to one object graph so their committed roots must match
byte-for-byte. What a fake cannot prove is that GitLab's *actual* status codes
and error bodies are the ones the client classifies on — above all the stale
`last_commit_id` rejection, which the client recognises from a message substring
because GitLab publishes no machine-readable code for it.

## Status

**Not yet executed.** As of 2026-08-22 the credential in
`~/.config/glab-cli/config.yml` is expired (`glab auth status` reports
`invalid_grant`), so no live run has happened. Re-authenticate with
`glab auth login --hostname <host>` and run the driver; until then the GitLab
client's live behaviour is unproven, and the ADR records that gap.

## Prerequisites

| Requirement | Why |
|---|---|
| `glab` authenticated (`glab auth status` clean) | creates, seeds and deletes the throwaway index project |
| An `ocx` binary with `package announce` | the thing under test — set `OCX_BIN`, else `test/bin/ocx` |
| Docker, with the acceptance registry up (`task test:registry` or `docker compose -f test/docker-compose.yml up -d`) | announce observes tags on a **real** OCI registry; the local one keeps the run self-contained |
| `jq` | reads the JSON report |

The announce target is a **local** registry (`localhost:5000`), which announce
would normally refuse as a private address. The driver writes the loopback host
into `[registries."localhost:5000"] trusted_hosts` in an isolated `OCX_HOME` —
the one sanctioned SSRF escape hatch, scoped to a throwaway home so nothing
touches your real configuration.

## Environment

| Variable | Default | Meaning |
|---|---|---|
| `GITLAB_HOST` | `gitlab.com` | the instance; set it to a self-managed host to prove that leg |
| `GITLAB_NAMESPACE` | the token's own username | where the throwaway index project is created |
| `OCX_BIN` | `test/bin/ocx` | the binary under test |
| `KEEP_PROJECT` | unset | set to `1` to skip the cleanup delete, to inspect the result |

`OCX_ANNOUNCE_TOKEN` is read by the driver from `glab`'s stored token, so the
credential is never typed on a command line.

## What it proves

The driver runs the **fork-free** lane — the announce branch is pushed to the
index project itself and a merge request is opened from it — because that is the
lane a single account can exercise without a second identity. It asserts, in
order:

1. A first announce commits the rebuilt root and opens a merge request. The MR
   URL is GitLab-shaped (`/-/merge_requests/<iid>`).
2. The committed root, read back over the API, carries the announced tag.
3. A second, identical announce reports `unchanged`, opens **no** second merge
   request, and does not advance the branch (C6).
4. A third announce adding a tag reuses the same merge request and leaves both
   tags in the branch (C4 accumulation).
5. The branch's first commit's parent is the index project's default-branch head
   — the base is the upstream, not a stale copy.

What it does **not** cover, and why: the fork lane needs a second namespace the
token can fork into, and the stale-`last_commit_id` rejection needs two
announces racing inside one commit window. Both are covered against the fake;
both are worth adding here once a second test identity exists.

## Running it

```sh
cd test/manual/announce-gitlab-e2e
./scripts/run_gitlab_e2e.sh
```

Every step prints its own assertion. A failure stops the run and leaves the
project in place so it can be inspected; re-run with `KEEP_PROJECT=1` to keep it
after a success too.

## Self-hosted

Set `GITLAB_HOST` to the instance and make sure `glab` is authenticated against
it. The driver passes `--forge gitlab` on every invocation, which is required
for a self-hosted host anyway, so the same script covers both legs — the only
difference is which host the coordinate names.
