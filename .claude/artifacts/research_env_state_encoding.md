# Research: encoding a versioned internal state in one environment variable

## Metadata

**Date:** 2026-11-06
**Domain:** cli
**Triggered by:** `/hex-architect` on `.agents/discussions/exec-env-model.md` (data model & compatibility axis)
**Expires:** 2027-05-06

## Direct Answer

Encode the state as JSON, deflate it (zlib), and base64url it without padding, behind a plaintext `<version>.` prefix (option C plus a visible version tag). Raw JSON (A) is not shell-safe; base64 alone (B) overruns cmd.exe's 8,191-character line at about 40 entries; a state-file pointer (D) is the only option with no size ceiling but it owns the cleanup and concurrency problems; multi-variable (E) multiplies the cmd.exe and `ps eww` surface for no gain. No size limit is hit on Linux or macOS at 200 entries. cmd.exe is the binding constraint, and even option C does not fit 200 entries there.

| Axis | A. plain JSON | B. JSON + base64url | C. zlib + base64url | D. pointer to file | E. multi-var |
|---|---|---|---|---|---|
| Size @50 / @200 entries (measured) | 9,511 / 37,133 B | 12,682 / 49,511 chars | 3,442 / 12,463 chars (gzip); 3,146 / 10,786 (zstd-19) | ~40-80 chars regardless | same total plus per-variable names; 0x1f-joined = raw size |
| Linux per-string 131,072 B incl NUL; total 1/4 of stack rlimit | fits | fits | fits | fits | fits (but total counts every name) |
| macOS ARG_MAX 1 MiB total | fits | fits | fits | fits | fits |
| Windows 32,767 chars per var | @200 exceeds (37 K) | @200 exceeds (49 K) | fits | fits | fits per var |
| cmd.exe 8,191-char `set` line | @50 exceeds | @50 exceeds | @50 fits; @200 exceeds | fits | fits only if each var is small |
| Shell-safe | no (`"`, `{`, `&`, `%`, `!`, `^`) | yes with URL alphabet, no padding | yes, same | yes (path may need quoting) | depends on values |
| Version mismatch | in-band field; needs a parser | in-band, must decode first | in-band, or prefix tag | prefix tag or file header | count variable plus a version variable |
| Debuggable | best | needs `base64 -d` | needs decode command | needs `cat` of file | best |
| Secret exposure (`/proc/<pid>/environ`) | plaintext | trivially decodable | trivially decodable | pointer only; file mode 0600 | plaintext |
| Prompt cost (python measure, 50 entries) | ~10 us parse | ~100 us | ~200 us | file open+read, ~tens of us warm [unverified] | ~10 us parse, n getenv calls |
| Failure modes | quoting bugs | size at cmd.exe | size at cmd.exe @200 | stale, orphaned, concurrent, tmux-inherited | partial set, count skew |

## Key Findings

1. Linux caps each environment string at `MAX_ARG_STRLEN = PAGE_SIZE * 32` (131,072 B on 4 KiB pages) and the whole argv plus envp at 1/4 of the soft `RLIMIT_STACK`, with a 32-page floor; `E2BIG` is returned when either limit is hit. Sources: [execve(2)](https://man7.org/linux/man-pages/man2/execve.2.html), [binfmts.h](https://github.com/torvalds/linux/blob/master/include/uapi/linux/binfmts.h). Measured on this host: `getconf ARG_MAX` is 2,097,152 with an 8,192 KiB stack, and `X=` plus 131,069 bytes runs while one more byte fails with `E2BIG` (script below). So one variable gets 131,071 bytes including the name.
2. Environment strings share one budget with argv, so a big variable taxes every `exec` of every descendant (including `PATH` already carrying the same paths). A 50 KB state is harmless against 2 MiB, but every exec copies it.
3. macOS: `ARG_MAX` is 1 MiB (`1024 * 1024`) in the xnu header, 256 KiB in the alternative branch, shared argv plus envp. Source: [xnu syslimits.h](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/sys/syslimits.h). No separate per-variable limit was found [unverified].
4. Windows: one variable is limited to 32,767 characters; since Vista there is no technical limit on the whole block. "A batch file cannot set a variable that is longer than the maximum command line length." Source: [Environment Variables](https://learn.microsoft.com/en-us/windows/win32/procthread/environment-variables).
5. cmd.exe: the string limit is 8,191 characters for a command line, a single inherited variable, and every expansion. The same page says cmd.exe ignores inherited variables longer than 8,191 even though the Win32 limit is 32,767. Source: [Command prompt line string limitation](https://learn.microsoft.com/en-us/troubleshoot/windows-client/shell-experience/command-line-string-limitation). Consequence: a `set OCX_STATE=...` hook line, and the variable inherited back into a cmd.exe child, both stop at 8,191. This is the page's claim; I could not run Windows here.
6. `setx` truncates to 1,024 characters (a well-known limit, [setx docs](https://learn.microsoft.com/windows-server/administration/windows-commands/setx)); a hook must use `set`, never `setx`. PowerShell `$env:X = ...` goes through .NET `SetEnvironmentVariable` and the 32,767 limit; no PowerShell-specific limit documented [unverified]. fish, nushell and elvish document no per-variable limit [unverified]; they sit on the OS limits above.
7. Compressibility: entries are dominated by a 64-hex digest in the store path (about 32 B of entropy each), so compression yields about 3.7x on realistic paths, not 10x. See Measurements. zstd-19 beats gzip-9 by about 13 percent at 200 entries; zlib and gzip are within 0.5 percent of each other (zlib has less header).
8. direnv: `DIRENV_DIFF` and `DIRENV_WATCHES` use "json+gzip+base64" in package `gzenv`; the code is actually `json.Marshal`, `zlib` writer, `base64.URLEncoding` (padded, so `=` appears). Source: [gzenv.go](https://github.com/direnv/direnv/blob/master/gzenv/gzenv.go). `EnvDiff` has `p` and `n` map fields and no version field: [env_diff.go](https://github.com/direnv/direnv/blob/master/internal/cmd/env_diff.go). A failed decode makes `Config.Revert` return an error and `exportCommand` fails with `Revert() failed` (fail closed): [config.go](https://github.com/direnv/direnv/blob/master/internal/cmd/config.go), [cmd_export.go](https://github.com/direnv/direnv/blob/master/internal/cmd/cmd_export.go).
9. mise: `__MISE_SESSION` and `__MISE_DIFF` use MessagePack (`rmp_serde::to_vec_named`) then zlib `Compression::fast()` then `BASE64_STANDARD_NO_PAD` (alphabet includes `+` and `/`): [hook_env.rs](https://github.com/jdx/mise/blob/main/src/hook_env.rs). Both carry a `v` field with `#[serde(default = "legacy_env_state_version")]`, so a blob without it is version 1; the current version is 2 and decides how values are compared: [env_diff.rs in mise-util](https://github.com/jdx/mise/blob/main/crates/mise-util/src/env_diff.rs). A session that fails to deserialize logs `warn!("error deserializing __MISE_SESSION")` and falls back to `Default` (treated as absent, so a full re-run). A shell that outlives an upgrade still holds a v1 blob and the new binary must still read it, because version decides compare semantics, not rejection.
10. mise stopped storing env values in the blobs: version 2 stores a `blake3:<hex>` digest per value because both variables are inherited by every child and "used to carry the full text of every env value mise set (including secrets)": [PR 13950](https://github.com/jdx/mise/pull/13950). Evidence that the env-visible state is a secrecy surface, and that a record used only for comparison need not hold the value.
11. tmux and stale pointers: direnv's variables survive into a tmux server, so direnv sees `DIRENV_DIFF`, `DIRENV_MTIME` and `DIRENV_DIR` unchanged and skips loading while PATH was reset; the maintainer declined to add logic and the issue is open: [direnv#106](https://github.com/direnv/direnv/issues/106). Applies to any inherited state variable, option D worst of all (pointer survives, file may not).
12. Quoting failures with raw values: [direnv#150](https://github.com/direnv/direnv/issues/150) (ampersand in a value breaks fish). Lesson: any unencoded text in a hook line is a bug in some shell.
13. Real E2BIG reports tied to a state variable: I found none for direnv or mise (searches of both trackers returned nothing relevant). General E2BIG-from-oversized-variable reports exist (bash-bug thread, Stack Exchange 128 KiB limit): [bug-bash 2020-09](https://lists.libreplanet.org/archive/html/bug-bash/2020-09/msg00095.html). Treat the failure mode as theoretical for Unix and real for cmd.exe.
14. Multi-variable prior art: git `GIT_CONFIG_COUNT` with `GIT_CONFIG_KEY_<n>` and `GIT_CONFIG_VALUE_<n>` ([git-config](https://github.com/git/git/blob/master/Documentation/git-config.adoc)) and cargo `CARGO_ENCODED_RUSTFLAGS` separated by 0x1f ([Cargo reference](https://doc.rust-lang.org/cargo/reference/environment-variables.html)). Both exist to avoid whitespace splitting of small lists, not to carry a large versioned record; neither carries a version tag.
15. Decode and dump: direnv has `direnv show_dump` / `direnv status` and mise has `mise doctor` as diagnostic surfaces [unverified, recall]; no prior art gives the encoded variable as the primary UX. A first-party decode command is the norm for opaque encodings.
16. Secrecy: any env var is readable by the same user through `/proc/<pid>/environ` and `ps eww`, and inherited by every child (finding 10). Compression and base64 are not secrecy. Never put credentials, tokens or secret values in it.
17. Overflow handling in prior art: mise reduced the payload (digests instead of values); direnv does nothing (no cap) [unverified, no code found]; neither spills to a file or refuses. A bounded design has to choose.

## Measurements

Synthetic state: 12 policy flags, 3 paths, n entries (1 in 5 a constant with a previous value; others PATH prepends of `/home/mherwig/.ocx/packages/ocx.sh/sha256/<2hex>/<62hex>/content/<sub>`), each tagged with an owner. Random hex digests make this a conservative (poorly compressible) sample. Compact JSON, then compress at max level, then base64url without padding.

| n | raw JSON B | b64url(raw) | gzip-9 B | b64url(gzip) | zlib-9 B | b64url(zlib) | zstd-19 B | b64url(zstd) |
|---|---|---|---|---|---|---|---|---|
| 5 | 1,209 | 1,612 | 520 | 694 | 508 | 678 | 518 | 691 |
| 50 | 9,511 | 12,682 | 2,581 | 3,442 | 2,569 | 3,426 | 2,359 | 3,146 |
| 200 | 37,133 | 49,511 | 9,347 | 12,463 | 9,335 | 12,447 | 8,089 | 10,786 |

Against limits: cmd.exe 8,191 line minus `set OCX_STATE=` (14 chars) leaves 8,177 characters. Option C fits up to roughly 120 realistic entries (extrapolating 62 B per entry); the 200-entry case needs 10.8-12.5 K and does not fit. Raw JSON fits cmd.exe only up to about 40 entries; B up to about 30. Python round trip (base64 + zlib-6 + json parse, 1,000 loops) was 77 us, 206 us and 786 us for 5, 50 and 200 entries; a Rust implementation will be faster, but one process spawn per prompt (milliseconds) dominates either way.

The E2BIG probe (Python, `/bin/true`, `X` of length n-2 'a'): n=131071 ok, 131072 and 131073 raise `Argument list too long`.

Script (saved at `/tmp/claude-1000/-home-mherwig--herdr-worktrees-ocx-uno/a79538a4-9579-477d-8611-afbe18e67ee8/scratchpad/measure.py`; zstd sizes came from `zstd -19 -c` on the same bytes):

```python
import json, random, gzip, zlib, base64, time
random.seed(7)
owners=["ocx.toml:default","ocx.toml:lint","ocx.toml:test","pkg:cmake","pkg:node"]
segs=["home","mherwig",".ocx","packages","ocx.sh","sha256","ab","cd","ef0123456789abcdef0123456789abcdef0123456789abcdef0123456789ab","content","bin","lib","share","include","python3.14","site-packages","node","cmake","current"]
def hexd(n): return ''.join(random.choice('0123456789abcdef') for _ in range(n))
def mkpath(L):
    base=f"/home/mherwig/.ocx/packages/ocx.sh/sha256/{hexd(2)}/{hexd(62)}/content"
    return base+random.choice(["/bin","/lib","/share/man","/include","/lib/pkgconfig"]) if L=="mid" else base+"/bin"
def state(n):
    ents=[]
    for i in range(n):
        if i%5==4: ents.append({"k":f"VAR_{i}","kind":"const","v":mkpath("mid"),"prev":"/usr/old/"+hexd(8),"o":owners[i%5]})
        else: ents.append({"k":"PATH","kind":"prepend","v":mkpath("mid"),"o":owners[i%5]})
    return {"v":1,"policy":{f"flag{i}":bool(i%2) for i in range(12)}|{"home":"/home/mherwig/.ocx","cfg":"/home/mherwig/.config/ocx","proj":"/home/mherwig/dev/x"},"applied":ents}
def zstd_size(b):
    try:
        import zstandard as z; return len(z.ZstdCompressor(level=19).compress(b))
    except Exception:
        import subprocess
        try: return len(subprocess.run(["zstd","-19","-c"],input=b,capture_output=True,check=True).stdout)
        except Exception: return None
def b64(b): return len(base64.urlsafe_b64encode(b).rstrip(b"="))
for n in (5,50,200):
    s=state(n); raw=json.dumps(s,separators=(",",":")).encode()
    g=gzip.compress(raw,9); d=zlib.compress(raw,9); z=zstd_size(raw)
    # cheaper variant: shorten store prefix via dictionary-less raw
    t=time.perf_counter()
    for _ in range(1000):
        x=base64.urlsafe_b64encode(zlib.compress(raw,6)); zlib.decompress(base64.urlsafe_b64decode(x)); json.loads(raw)
    dt=(time.perf_counter()-t)/1000*1e6
    print(f"n={n} raw={len(raw)} b64(raw)={b64(raw)} gzip9={len(g)} b64gz={b64(g)} zlib9={len(d)} b64zlib={b64(d)} zstd19={z} b64zstd={b64(b'x'*z) if z else None} roundtrip_us(py)={dt:.0f}")
```

## negative

- No Windows host: the cmd.exe 8,191 limit and its "ignore long inherited variables" behaviour are quoted from Microsoft's page, not reproduced. PowerShell, nushell, elvish and fish line or variable limits are undocumented as far as I found.
- No documented bug where `DIRENV_DIFF`, `__MISE_DIFF` or `__MISE_SESSION` caused `E2BIG`; the searches returned only generic reports. conda and nix-direnv state-variable behaviour (conda uses several small variables such as `CONDA_SHLVL` and `CONDA_PREFIX_<n>`; nix-direnv caches in a file under `.direnv/`) is recall only [unverified].
- Per-prompt cost for D is an estimate, not measured; I did not benchmark file reads against the hook path. Rust-side encode/decode times were not measured.
- macOS per-variable limit and fish/nushell/elvish handling of 10 K-char values were not tested (no such shells installed here).
- Compression ratios depend on the real path shape; a store path with a shared prefix and 64-hex digest was assumed. Real data may compress better with a path-prefix table.

## Recommendation

Use option C: `serde_json` compact, then zlib deflate, then base64url without padding, emitted as `<ver>.<payload>` (for example `1.eNqr...`). The version tag sits in plaintext before the payload so a reader on any version can classify the variable without decoding it: equal version decodes; different version, bad base64, bad inflate or bad JSON all mean "state absent" with one `warn`, never a hard failure and never reuse of a foreign layout. Keep an inner `v` field too only if the prefix cannot be trusted across shells; prefer the prefix as the single source. Treat this as the same-version-only contract the question requires; do not copy mise's cross-version reversal, which exists because mise shells outlive upgrades (finding 9).

Why not the others: A needs a quoter per shell and breaks on `%`, `!`, `^`, `&` in cmd.exe (finding 12); B costs 3.7x the size of C for nothing; E multiplies the number of names on the `ps eww` surface and gives no version atomicity; D is the right answer only if the state must exceed cmd.exe's limit, and it adds a stale-file lifecycle (tmux panes inheriting the pointer, crashed shells, two shells racing) that the env-only design avoids.

Overflow policy to accept: set a hard budget of 8,177 encoded characters on Windows (cmd.exe), 100 K elsewhere. When the record would exceed it, do not truncate; degrade in this order: store a digest per applied value instead of the value (mise's v2 trick, finding 10), which shrinks entries to about 20-30 B but loses the ability to re-derive the removal list; if it still overflows, refuse to write the variable and report that the shell-state is not tracked. Do not build the file-pointer spill in the first version; add it only if real users report overflow, as one extra tier behind the same `<ver>.` tag (`1f.<id>`).

Trade-off accepted: the variable is opaque, so ship a first-party decode command (print the JSON), and the state is still readable by every same-user process, so credentials never go in it (store only paths, constants and owner tags; apply the secret-value filter before recording a constant). It also remains inherited into every child, including exec-less stale panes, so the reader must validate the policy block against current config rather than trust it.

## Sources

| Source | Type | Date | Relevance |
|---|---|---|---|
| https://man7.org/linux/man-pages/man2/execve.2.html | Docs | accessed 2026-11-06 | Linux ARG_MAX, MAX_ARG_STRLEN, E2BIG |
| https://github.com/torvalds/linux/blob/master/include/uapi/linux/binfmts.h | Repo | accessed 2026-11-06 | `MAX_ARG_STRLEN = PAGE_SIZE * 32` |
| https://github.com/apple-oss-distributions/xnu/blob/main/bsd/sys/syslimits.h | Repo | accessed 2026-11-06 | macOS `ARG_MAX` 1 MiB |
| https://learn.microsoft.com/en-us/windows/win32/procthread/environment-variables | Docs | 2025-07-14 | 32,767 per variable; no block limit since Vista |
| https://learn.microsoft.com/en-us/troubleshoot/windows-client/shell-experience/command-line-string-limitation | Docs | 2026-02-12 | cmd.exe 8,191 limit incl. inherited variables |
| https://learn.microsoft.com/windows-server/administration/windows-commands/setx | Docs | accessed 2026-11-06 | setx 1,024 truncation |
| https://github.com/direnv/direnv/blob/master/gzenv/gzenv.go | Repo | accessed 2026-11-06 | direnv encoding: JSON, zlib, base64 URL padded |
| https://github.com/direnv/direnv/blob/master/internal/cmd/env_diff.go | Repo | accessed 2026-11-06 | `EnvDiff` fields, no version |
| https://github.com/direnv/direnv/blob/master/internal/cmd/config.go | Repo | accessed 2026-11-06 | `Revert` fails on decode error |
| https://github.com/direnv/direnv/issues/106 | Issue | accessed 2026-11-06 | tmux inherits stale `DIRENV_DIFF` |
| https://github.com/direnv/direnv/issues/150 | Issue | accessed 2026-11-06 | unencoded value breaks fish |
| https://github.com/jdx/mise/blob/main/src/hook_env.rs | Repo | accessed 2026-11-06 | mise msgpack + zlib + base64 std no-pad; failure falls back to default |
| https://github.com/jdx/mise/blob/main/crates/mise-util/src/env_diff.rs | Repo | accessed 2026-11-06 | `v` field, `ENV_STATE_VERSION` = 2, legacy default 1 |
| https://github.com/jdx/mise/pull/13950 | PR | accessed 2026-11-06 | digests instead of values, secrecy of inherited state |
| https://github.com/git/git/blob/master/Documentation/git-config.adoc | Docs | accessed 2026-11-06 | `GIT_CONFIG_COUNT` pattern |
| https://doc.rust-lang.org/cargo/reference/environment-variables.html | Docs | accessed 2026-11-06 | `CARGO_ENCODED_RUSTFLAGS` 0x1f |
| https://lists.libreplanet.org/archive/html/bug-bash/2020-09/msg00095.html | Mailing list | 2020-09 | oversized variable causes E2BIG |
