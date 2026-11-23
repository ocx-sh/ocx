# Research: module-extension facts on a fresh machine (Bazel 9.2.0)

## Metadata

**Date:** 2026-11-18
**Lane:** local experiment (`/hex-discuss`, `.agents/discussions/bazel-hermetic-package-bundles.md`)
**Basis:** Bazel 9.2.0, `bazel mod deps` only, in a scratch workspace; files under the session scratchpad `facts-spike/`
**Expires:** 2027-05-18
**Caveat:** findings only, no recommendation.

### Setup
- Bazel: `/home/mherwig/.herdr/worktrees/ocx/uno/.ocx/toolchain/links/default/bazel/content/bazel`, `bazel --version` → `bazel 9.2.0`.
- Files (scratch root `/tmp/claude-1000/-home-mherwig--herdr-worktrees-ocx-uno/884fced7-d8ab-48af-8366-591f3a7bc90b/scratchpad/facts-spike`):
  - `ws/MODULE.bazel`
  - `ws/ext.bzl`
  - `ws/BUILD.bazel` (empty)
  - `run.sh`
  - `registry-calls.log`, one `call` line per simulated registry call.
  - Lock snapshots: `lock.A`, `lock.E`, `lock.bak`.
- Every run was `bazel --ignore_all_rc_files --output_user_root=<scratch>/<name> mod deps [args]`. Run A and B used root `our`; C, D, E, F used fresh roots `c1`/`c2`, `d1`, `e1`, `f1`.
- The extension looks the digest up in `ctx.facts` and runs the `sh -c "echo call >> LOG; echo meta-for-<digest>"` call only on a miss. It returns `extension_metadata(reproducible=…, facts=…)` and creates repo `demo`.
- I ran `mod deps` only. I never ran `query` or `build` on `@demo`.

### Observations (reproducible = True)
| Run | Impl re-ran? | Facts received? | Calls added | Lock note |
|---|---|---|---|---|
| A cold | yes | no | +1 (total 1) | Top-level `"facts": {"//:ext.bzl%ext": {"sha256-aaa": "meta-for-sha256-aaa"}}` written. No `moduleExtensions` entry for `//:ext.bzl%ext`, so no generated-repo specs. |
| B same root, again | not recorded (see Deviations) | not recorded | 0 | Unchanged |
| C fresh root, lock kept | yes | yes (aaa=True) | 0 | Lock byte-identical to A |
| D control, fresh root, lock deleted | yes | no | +1 (total 2) | Facts re-written; detector works |
| E add `sha256-bbb`, fresh root, lock from A | yes | aaa=True, bbb=False | +1 (total 3) | Facts now hold both aaa and bbb |
| F fresh root, E's lock, `--lockfile_mode=error` | yes | aaa=True, bbb=True | 0 | Passes, lock byte-identical |

### Optional, reproducible = False
- I deleted the lock and the log, then re-ran in a fresh root. The impl ran, with both digests missing from facts, so 2 calls were made.
- The lock now has a `moduleExtensions` entry for `//:ext.bzl%ext` with `generatedRepoSpecs.demo` (`body` = the metadata strings). `recordedInputs` is `[]`. The top-level `facts` entry is also still written.
- On a second fresh root with that lock kept, the impl did not re-run: no `ext impl ran` line and 0 new calls. The lock supplied the generated repos.

### Deviations and errors
- `print(ctx.facts)` shows only `Facts(<opaque, inspect with print()>)`. I used `key in ctx.facts` and `ctx.facts[key]` instead. Both worked.
- Values were plain strings and keys plain strings. I hit no type, iteration or `facts_version` error. `factsVersions` stayed `{}` because I never set `facts_version`.
- Run B: I added the `ext impl ran` print only after B, so I did not see whether the impl re-ran. The call count did not grow.
- Run C was executed twice: `c1` with the opaque-facts print, `c2` with the `in` print. Both added 0 calls.
- I ran the reproducible=False check before verifying the digest set. `ws/MODULE.bazel` still carried both digests from run E, so the A2 count of 2 is aaa plus bbb.
- Output bases `our`, `c1`, `c2`, `d1`, `e1`, `f1`, `r1`, `r2` all got `bazel shutdown`. Nothing was written inside any git repository.

Not checked: Bazel behavior after a digest is removed from `MODULE.bazel` (whether stale facts are pruned from the lock), and any `build`/`query` path.
