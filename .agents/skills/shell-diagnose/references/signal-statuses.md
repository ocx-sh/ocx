# Signal statuses above 128

You are here because a script or CI step ended with 141, 143, 269, 271, 397 or 399, or because a fix tests a signal death against a literal status. A signal death is 128 plus the signal number on most shells, 256 plus on ksh93u+m, and 384 plus on yash. A test written against `141` misses on three of eleven shells. All rows were captured on 2026-10-11 by killing a child with `kill -PIPE` or `kill -TERM` and reading the status through `wait`, and by running a real `yes` into `head -n 1` under `pipefail`.

Contents: [Table](#table) · [Decode](#decode) · [Notes](#notes)

## Table

| Shell and version | SIGPIPE child via `wait` | SIGPIPE through a pipefail pipeline | SIGTERM via `wait` | `kill -l N` decodes |
|---|---|---|---|---|
| dash 0.5.12 (Debian 12) | 141 | pipefail unsupported, status 0 | 143 | yes |
| dash 0.5.12-12 (Debian 13), 0.5.12-12ubuntu3 (Ubuntu 26.04) | 141 | 141 | 143 | yes |
| busybox ash 1.35 and 1.37 | 141 | 141 | 143 | yes |
| mksh R59c | 141 | 141 | 143 for one job, 0 for the second job of a two-signal loop (unexplained, unverified) | yes |
| ksh93u+m 2023 (Debian 12) | 269 | 13 | 271 | yes |
| ksh93u+m 1.0.10 (Debian 13) | 269 | 1, with `yes: standard output: Connection reset by peer` | 271 | yes |
| yash 2.52 | 397 | 397 | 399 | yes |
| posh 0.14.1 | 141 | 0, pipefail rejected | 143 | no, `kill: unknown signal name 143` |
| bash 3.2, 4.0, 4.2, 4.4, 5.0 to 5.3 | 141 | 141 | 143 (3.2 to 4.4 also print a `Terminated` job line) | yes |
| zsh 5.0.8, 5.3.1, 5.8, 5.9 | 141 | 141 | 143 | yes |

## Decode

Test `-gt 128` and decode with `kill -l`. A literal `-eq 141` hit on eight shells and missed on ksh93u+m 2023, ksh93u+m 1.0.10 and yash 2.52. The `-gt 128` form hit on all eleven.

```sh
rc=141   # the status the parent saw
if [ "$rc" -gt 128 ]; then kill -l "$rc"; fi
```

The block prints `PIPE` for 141, 269 and 397 and `TERM` for 143, 271 and 399 on every shell above except posh.

## Notes

- 141 under `pipefail` is the producer losing its reader: `producer | grep -q` and `producer | head -n 1` end the reader early (SH-ERR-09). The pipeline status is the producer's 141, not a failure of the command. Capture the output, then test it.
- ksh93u+m documents 256 plus the signal number (sh.1) and yash documents the signal number plus 384 (exec chapter), so 13 and 256 give 269 and 13 and 384 give 397. POSIX XCU 2.8.2 allows a status above 255 for a signal death.
- The ksh93u+m 1.0.10 result `1` with `Connection reset by peer` is an ECONNRESET on the writer, not a SIGPIPE. The mechanism was not verified.
- Which status a script exits with itself is SH-ERR-16 and SH-ERR-18 (never 123, 124, 125, 128 or 200 to 255).
