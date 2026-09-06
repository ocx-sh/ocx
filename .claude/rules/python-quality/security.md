# Security

The threat model is content this fleet did not author: release archives pulled
from a registry, catalog entries a stranger opened a PR for, and registry
credentials held in process. Read this before unpacking an archive, before using
a path that came from outside the process, before touching a credential or the
environment of a child process, and before interpolating a value the process did
not produce into a message someone will read.

Contents: [Untrusted Archives](#untrusted-archives) · [Credentials](#credentials) ·
[Paths From Outside](#paths-from-outside) · [Untrusted Text in Messages](#untrusted-text-in-messages) ·
[The Security Gate](#the-security-gate) ·
[What Agents Get Wrong](#what-agents-get-wrong-here) · [Sources](#sources)

Two layers, and the difference matters when adopting this elsewhere:

- **The mechanism** — stream one named member, bound the stream rather than the
  header, resolve before comparing, funnel secrets through one redactor — is
  general Python practice for any tool that consumes third-party archives.
- **The pinned decisions** are this fleet's, already shipped and not
  re-litigated: `filter='data'` is written out at every call site because the
  `requires-python` floors are 3.12/3.13, below the version where the default
  changed; `ruff`'s `S` family is the gate and `bandit` is advisory; `pip-audit`
  is the advisory source and `uv audit` is not while it remains in preview; a
  credential leaves the process only on a child's stdin.

Extraction is currently clean fleet-wide — two real sites, both fed by
downloaded release archives, both byte-bounded and link-checked, zero
`extractall()` calls anywhere. The pytest harnesses never unpack anything: they
build fixtures and let the binary under test do the extracting. So most of what
follows is a do-not-regress contract rather than a backlog, and the shipped
single-member loop is the reference implementation to copy.

## Untrusted Archives

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-SEC-01 | Never call `.extractall()` or a batch `.extract()` on an archive the process downloaded — resolve one member, check it, stream it. Where a batch extract is genuinely unavoidable, `filter='data'` is written at the call site and never inherited from the interpreter: the default is `fully_trusted` below Python 3.14 and every package here floors below that, so an unfiltered `extractall()` on the stated minimum Python behaves exactly as it did before CVE-2007-4559 was fixed. `zipfile` has no `filter=` — it strips `..` and absolute prefixes itself but never inspects link members, so `stat.S_ISLNK(info.external_attr >> 16)` rejection is what replaces the filter there. | `rg -n --glob '**/*.py' 'extractall\(' <src>` — every hit needs a read; one without an explicit `filter=` is the violation. Then `ruff check --select S202 --no-cache --isolated <src>`, which is **not** a substitute: measured, S202 sees a `zipfile` `.extractall()` only when `tarfile` happens to be imported in the same file, so a pure-`zipfile` module gets zero coverage from it | MUST |
| PY-SEC-02 | Cap member count and cumulative decompressed bytes **inside** the read loop, against a named module constant, aborting before the next chunk is pulled from the decompressor. `ZipInfo.file_size` and `TarInfo.size` are attacker-supplied header metadata, not verified properties, and a check that runs after the read has already lost: measured on CPython 3.14, 64 MiB of zeros deflates to 65 KB (1027:1 in one layer) and `zf.read()` puts all 64 MiB resident before a post-hoc size test can run. | `rg -n --glob '**/*.py' -e 'file_size' -e '\.size >' <src>` — zero output is expected and clean; any hit used as the bound rather than reported is the violation. Then the fixture: unpack a member that deflates at ~1000:1 and assert the extractor raises at the constant, with peak resident bytes near the cap and not near the payload | MUST |

```python
# Wrong — the header is not a bound, and the join is steered by the archive.
with zipfile.ZipFile(archive) as zf:
    for info in zf.infolist():
        if info.file_size > MAX_BYTES:  # attacker-supplied metadata
            raise ValueError("member too large")
        (dest / info.filename).write_bytes(zf.read(info.filename))

# Right — one member, resolved by base name, link rejected, stream bounded.
with zipfile.ZipFile(archive) as zf:
    entry = _only_member(zf, wanted)  # base name only; never info.filename
    if stat.S_ISLNK(entry.external_attr >> 16):
        raise OcxError(f"{entry.filename!r} is a link member")
    total = 0
    with zf.open(entry) as src, out.open("wb") as sink:
        while chunk := src.read(_CHUNK):
            total += len(chunk)
            if total > MAX_BYTES:
                raise OcxError(f"{entry.filename!r} exceeds {MAX_BYTES} bytes")
            sink.write(chunk)
```

## Credentials

No live credential-value exposure path exists in this fleet today, and the two
rules below are the properties that keep it that way rather than fixes for a
known leak. Every credential-bearing surface — `repr()`, a log line, an
exception message, subprocess argv, a child's environment — already routes
through something that excludes or scrubs the value.

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-SEC-03 | A credential value reaches no observable surface: not a log line, not an exception message, not a `repr()`, not `argv`, and not the environment of a child process. Concretely — it travels to a child on stdin (`--password-stdin` plus `input=`), never as an argv element, because argv is world-readable through `ps` and `/proc/<pid>/cmdline`; a field holding one is declared `field(..., repr=False)`, so exclusion is a property of the type rather than a habit every future `logger.debug(self)` has to remember; and redaction happens at **one** chokepoint covering argv, captured stderr and the text carried on raised errors, not per call site. Captured stdout is deliberately exempt: substituting inside the JSON payload a caller is about to parse corrupts the document. PY-SEC-04 is the mechanism for the environment half. | `rg -n --glob '**/*.py' -e 'run\(\[[^\]]*token' -e 'run\(\[[^\]]*password' -e 'Popen\(\[[^\]]*token' -e 'Popen\(\[[^\]]*password' <src>` — each hit puts a credential in an argv literal, and zero output is expected and clean; the compliant `run(argv, input=token)` shape is correctly not matched. Then `rg -n --glob '**/*.py' -e 'token: ' -e 'password: ' -e 'secret: ' <src>` — the trailing `: ` anchors it to a declaration; each hit that declares a field and lacks `repr=False` is the violation | MUST |
| PY-SEC-04 | A child process that does not need the parent's environment is given an explicitly constructed `dict` naming what it gets. Never `os.environ.copy()`, never `{**os.environ, ...}`, and never an unset `env=` where the parent may hold a registry token — an allowlist bounds the blast radius to what is named, instead of handing every ambient credential in a developer's shell to every process the child spawns in turn. SHOULD rather than MUST because the fleet's only gap is four `subprocess.run` calls to `git` with fixed argv, where no credential-relevant behaviour turns on an inherited variable; the next hook that shells out to something less trusted inherits the gap by default unless it names its environment. | `rg -n --glob '**/*.py' -e 'os\.environ\.copy\(\)' -e '\{\*\*os\.environ' -e 'env=os\.environ' <src>` — each hit is a violation; zero output is expected and clean. This cannot see an *implicit* unset `env=`, so each `subprocess.run`/`Popen` site still needs the argument read | SHOULD |

## Paths From Outside

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-SEC-05 | Never build a destination by joining a raw `.filename`/`.name` from an archive, a wire document, or a PR-supplied field onto a directory. Resolve by base name, or resolve both sides and compare with `Path.is_relative_to()` / `os.path.commonpath()`. A `.startswith()` on a resolved path is not a containment check: `/dest-evil` is a string prefix of `/dest`, and appending a separator still admits a resolved symlink that shares the prefix while denoting a different directory. | Two checks, both printing only violations, both silent on the reference loop above. Raw-member joins, in either idiom: `rg -n --glob '**/*.py' -e 'join\([^)]*\.filename' -e 'join\([^)]*\.name\b' -e '/ ?[\w.]*\w+\.filename' -e '/ ?[\w.]*\w+\.name\b' <src>`. Naive containment: `rg -nU --glob '**/*.py' -e 'realpath\([\s\S]{0,160}?startswith\(' -e 'resolve\(\)[\s\S]{0,160}?startswith\(' <src>`. Both are unions — every hit is independently a violation, none of them is a condition on another | MUST |

## Untrusted Text in Messages

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-SEC-06 | Every value the process did not produce — a subprocess's stderr, an API response field, a filename, a schema-validated but PR-supplied identifier — is `!r`-wrapped when it is interpolated into an exception message or any string a caller may print. `repr()` renders control bytes as their `\x..` literal, so this is free and already idiomatic; raw ANSI reaching a terminal is CWE-150, and the sibling Rust binary in this catalog shipped exactly that defect. The failure mode is one bare interpolation sitting beside `!r`-wrapped siblings in the same message, which reads as correct at review speed. | `rg -n --glob '**/*.py' 'f"[^"]*\{[a-z_][\w.\[\]()]*\}' <module>` — each hit is an interpolation with no conversion. Scope it to the module under change: whole-tree this is legitimately noisy, and the real finding it caught was a single bare `{tag_name}` between two `!r`-wrapped values on one line | SHOULD |

## The Security Gate

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PY-SEC-07 | Every shipped package selects `ruff`'s `S` family. It is one line in a config `ruff` already runs, and it is what makes `pickle`/`marshal` (`S301`, `S302`), `yaml.load` (`S506`), `eval`/`exec` (`S307`), `tempfile.mktemp` (`S306`) and unsafe hashes (`S324`) gated rather than remembered. `S105`/`S106` fire on `*_TOKEN`/`*_TOKEN_ENV` constants that hold variable *names*: 3 of the 4 hits fleet-wide are that false positive, and the answer is a per-site `# noqa: S105` naming why, never dropping the code from the family. `bandit` stays runnable and advisory for the three checks `ruff` never ported — `B325`, `B614`, `B615` — and for nothing else. | `rg --files-without-match '"S"' --glob '**/pyproject.toml' <root>` — every listed manifest that configures `ruff` is a violation. Then `rg -n --glob '**/pyproject.toml' '"S105"' <root>` — a hit inside `[tool.ruff.lint] ignore` is the violation; inside `per-file-ignores` it is not | MUST |
| PY-SEC-08 | A security lint counts as enforced only after it has been watched go red, against a planted violation, at the exact path and config the CI job passes it. A rule selected where it cannot fire certifies every unchecked change as checked, and the failure is invisible because the output is identical to a clean tree. This is not hypothetical here: a shipped `S307`-based rule is selected at a scope that reaches no file, so `eval("1+1")` passes it today. | Write the violation the rule names into a file inside the tree CI actually scans, run the project's own gate command verbatim, and confirm a non-zero exit naming that file. A green run on a planted violation is the violation — then delete the plant | MUST |
| PY-SEC-09 | `pip-audit` runs in CI, and a finding is triaged as runtime-reachable or build-only before anyone dismisses it. The distinction is the whole value: a `gitpython` advisory reached only through the docs toolchain is not the same finding as an `idna` advisory reached transitively through `httpx` in a shipped package. `uv audit` and `UV_MALWARE_CHECK` are faster and OSV-backed but explicitly preview, so they do not gate anything yet. | `uv export --no-hashes --no-emit-project --output-file <req.txt>` then `pip-audit -r <req.txt>` — a non-zero exit lists the vulnerable pins. The export step is load-bearing, not ceremony: run directly against a `uv` venv, `pip-audit` fails on the project's own unpublished distribution and neither `--skip-editable` nor `--local` fixes it | SHOULD |

## What Agents Get Wrong Here

1. Writes `archive.extractall(dest)` and considers extraction handled. It is the
   one-line method every tutorial shows, `filter=` is opt-in on exactly the
   Python versions this fleet targets, and nothing about the call site hints
   that the right-hand side is attacker-controlled (PY-SEC-01).
2. Adds `filter='data'` and marks the archive work done. The stdlib's own docs
   say the filters prevent no denial-of-service at all — no cap on member count,
   total size, or filename length (PY-SEC-02).
3. Bounds the extraction with `info.file_size`, because it reads as the size of
   the thing about to be written. It is a number the archive supplied about
   itself (PY-SEC-02).
4. Writes `dest / member.name`, which is syntactically indistinguishable from
   every other path join in the file (PY-SEC-05).
5. Produces `resolved.startswith(dest)` as the containment check — the top
   answer to "prevent path traversal in Python" throughout training data
   (PY-SEC-05).
6. Passes a token as `["cmd", "--password", token]` because it is the shape the
   tool's `--help` documents, and `Popen` takes a list so it "isn't shell
   injection" (PY-SEC-03).
7. Builds a child environment as `{**os.environ, "SOME_VAR": value}` — the
   idiomatic dict-merge, which quietly forwards every ambient credential in the
   developer's shell (PY-SEC-04).
8. Reaches for `requests.get(url, verify=False)` because the TLS error message
   suggests it, or `subprocess.run(f"cmd {value}", shell=True)` because
   f-strings read as string-building rather than command construction
   (PY-SEC-07).
9. Wraps a security check in `except Exception: pass`. Around a function whose
   only job is to say no, swallowing the exception makes the default answer yes.
10. Turns a lint green by widening a suppression or narrowing the path it runs
    on, and reports the gate as passing (PY-SEC-08).

## Sources

- [PEP 706](https://peps.python.org/pep-0706/) — the extraction-filter design and the 3.12→3.14 default timeline
- [`tarfile` — extraction filters](https://docs.python.org/3/library/tarfile.html#extraction-filters) — what each filter does, and "Hints for further verification" on what none of them cover
- [`zipfile` — decompression pitfalls](https://docs.python.org/3/library/zipfile.html) — the built-in path sanitization, the missing link-member check, and the zip-bomb note
- [CVE-2007-4559](https://nvd.nist.gov/vuln/detail/CVE-2007-4559) — the traversal that sat unfixed at the language level for 17 years, re-scored 9.8 Critical
- [CWE-150](https://cwe.mitre.org/data/definitions/150.html) — improper neutralization of escape, meta, or control sequences
- [ruff — flake8-bandit (`S`)](https://docs.astral.sh/ruff/rules/#flake8-bandit-s) — the ported rule set; `S324` is the real ceiling
- [`bandit` plugin index](https://bandit.readthedocs.io/en/latest/plugins/index.html) — ground truth for `B325`, `B614`, `B615`, the three with no `S` equivalent
- [pip-audit](https://pypi.org/project/pip-audit/) — PyPA/OSV-backed advisory scanning and its SBOM and requirements-file modes
