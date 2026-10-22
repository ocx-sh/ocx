#!/usr/bin/env python3
"""Round-trip the 36-case hostile corpus through one target interpreter, byte for byte.

    python3 -I roundtrip.py QUOTER -- INTERP [ARGS...]
    python3 -I roundtrip.py --family FAMILY --shim CMD [--env] [--var NAME] -- INTERP [ARGS...]
    python3 -I roundtrip.py ... --corpus FILE -- INTERP [ARGS...]
    python3 -I roundtrip.py --list

QUOTER names a reference quoter below. --shim runs your own emitter instead: CMD is
split like a shell line and is called as `CMD NAME HEX` once per case, where HEX is
the value in hex. It must print one statement that sets NAME and exit 0, or exit 65
with empty stdout to refuse (the NUL case must be refused). FAMILY is sh, fish, nu,
elvish or pwsh. --env reads NAME as an environment variable (nu, elvish, pwsh).
--corpus FILE swaps in another name<TAB>hex file (completions/help-payloads.txt holds the hostile help
texts). The interpreter runs in an empty directory and the run fails when a file named `pwned` appears,
which means a payload executed. The 36-case corpus payloads only print PWNED, so there the proof is the hex mismatch; the help payloads create the file. Run it once per interpreter version. Exit 0 only when every round-trip case matches
and the NUL case is refused. A yash run needs LC_ALL=C.UTF-8 set by the caller.
"""

import base64
import json
import re
import shlex
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).absolute().parent
SQ = "\u0027\u2018\u2019\u201a\u201b"  # PowerShell single-quote class
DQ = "\u0022\u201c\u201d\u201e"  # PowerShell double-quote class


def nonul(v):
    if "\x00" in v:
        raise ValueError("NUL")
    return v


def sh(v):
    return "'" + nonul(v).replace("'", "'\\''") + "'"


def sh_dollarq(v):  # plant: $'..' is literal text on dash, yash and posh
    o = nonul(v).replace("\\", "\\\\").replace("'", "\\'").replace("\n", "\\n")
    return "$'" + o.replace("\r", "\\r").replace("\t", "\\t") + "'"


def sh_dq(v):  # plant: double quotes without escaping $ or backtick, the payload executes
    return '"' + nonul(v).replace("\\", "\\\\").replace('"', '\\"') + '"'


def fish(v):
    return "'" + nonul(v).replace("\\", "\\\\").replace("'", "\\'") + "'"


def nu_raw(v):
    nonul(v)
    n = 1 + max([len(m) for m in re.findall(r"(?<=')#*", v)] + [0])
    return "r" + "#" * n + "'" + v + "'" + "#" * n


def nu_dq(v):
    return '"' + nonul(v).replace("\\", "\\\\").replace('"', '\\"') + '"'


def elvish(v):
    return "'" + nonul(v).replace("'", "''") + "'"


def pwsh_apos(v):  # plant: doubles U+0027 only
    return "'" + nonul(v).replace("'", "''") + "'"


def pwsh_sq(v):
    out = nonul(v)
    for c in SQ:
        out = out.replace(c, c + c)
    return "'" + out + "'"


def pwsh_dq_naive(v):  # plant: leaves the curly double quotes alone
    return '"' + nonul(v).replace("`", "``").replace("$", "`$").replace('"', '""') + '"'


def pwsh_dq(v):
    out = nonul(v).replace("`", "``").replace("$", "`$")
    for c in DQ:
        out = out.replace(c, "`" + c)
    return '"' + out + '"'


def pwsh_b64(v):
    b = base64.b64encode(nonul(v).encode()).decode()
    return "[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('" + b + "'))"


def ascii_json(v):
    d = json.dumps({"v": nonul(v)}, ensure_ascii=True)
    return d.replace("'", "\\u0027")


QUOTERS = {  # name -> (family, quoter, expected on the oldest and newest interpreter)
    "sh": ("sh", sh, "green"),
    "sh_dollarq": ("sh", sh_dollarq, "red on dash, yash, posh"),
    "sh_dq": ("sh", sh_dq, "red on bt, cmdsub, nested, dollar_brace payloads"),
    "fish": ("fish", fish, "green"),
    "fish_posix": ("fish", sh, "red"),
    "nu_raw": ("nu", nu_raw, "green"),
    "nu_dq": ("nu", nu_dq, "green"),
    "nu_posix": ("nu", sh, "red"),
    "nu_json": ("nu", lambda v: "('" + ascii_json(v) + "' | from json | get v)", "green"),
    "elvish": ("elvish", elvish, "green"),
    "elvish_posix": ("elvish", sh, "red"),
    "elvish_json": ("elvish", lambda v: "(echo '" + ascii_json(v) + "' | from-json)[v]", "green"),
    "pwsh_sq": ("pwsh", pwsh_sq, "green"),
    "pwsh_apos": ("pwsh", pwsh_apos, "red"),
    "pwsh_dq": ("pwsh", pwsh_dq, "green"),
    "pwsh_dq_naive": ("pwsh", pwsh_dq_naive, "red"),
    "pwsh_b64": ("pwsh", pwsh_b64, "green"),
    "pwsh_json": ("pwsh", lambda v: "(ConvertFrom-Json '" + ascii_json(v) + "').v", "red on 7.x"),
}
SET = {  # family -> (statement builder for a quoted literal, script suffix)
    "sh": (lambda q: "v=" + q, "sh"),
    "fish": (lambda q: "set -gx v " + q, "fish"),
    "nu": (lambda q: "let v = " + q, "nu"),
    "elvish": (lambda q: "var v = " + q, "elv"),
    "pwsh": (lambda q: "$v = " + q, "ps1"),
}

DRIVER = {  # family -> evaluator that prints NAME:HEX for each case; {R} is the read expression
    "sh": """run() {
  code=$(printf '%s' "$2" | base64 -d)
  out=$( ( eval "$code" || exit 1; [ "${NAME+set}" = set ] || exit 1; printf '%s' "$NAME" | od -An -v -tx1 | tr -d ' \\n' ) 2>/dev/null ) || out=ERR
  printf '%s:%s\\n' "$1" "$out"
}
""",
    "fish": """function run --argument-names name b64
    set -e NAME
    set -l code (printf '%s' $b64 | base64 -d | string collect)
    eval $code 2>/dev/null
    or begin; echo "$name:ERR"; return; end
    if not set -q NAME; echo "$name:ERR"; return; end
    if test (count $NAME) -ne 1; echo "$name:ERR"; return; end
    set -l h (printf '%s' $NAME | od -An -v -tx1 | tr -d ' \\n' | string collect)
    echo "$name:$h"
end
""",
    "nu": """def chk [name: string, b64: string] {
  let code = ($b64 | decode base64 | decode utf-8)
  let full = ($code ++ "; {R} | encode utf8 | encode hex --lower")
  let r = (do -i { ^$nu.current-exe --no-config-file -c $full } | complete)
  if $r.exit_code != 0 { print $"($name):ERR" } else { print $"($name):($r.stdout | str trim)" }
}
""",
    "elvish": """use str
fn run {|name b64|
  try {
    var code = (printf '%s' $b64 | base64 -d | slurp)
    var r = (eval (str:join '' [$code '; put {R}']))
    var h = (printf '%s' $r | od -An -v -tx1 | tr -d ' \\n' | slurp)
    echo $name':'$h
  } catch e {
    echo $name':ERR'
  }
}
""",
    "pwsh": """function Run($name, $b64) {
  $code = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($b64))
  try {
    $r = Invoke-Expression ($code + '; {R}')
    if ($null -eq $r) { "$($name):ERR-null"; return }
    $h = ([Text.Encoding]::UTF8.GetBytes([string]$r) | ForEach-Object { $_.ToString('x2') }) -join ''
    "$($name):$h"
  } catch { "$($name):ERR" }
}
""",
}
CALL = {"sh": "run", "fish": "run", "nu": "chk", "elvish": "run", "pwsh": "Run"}


def reads(family, name, env):
    if env:
        return {"nu": "$env." + name, "elvish": "$E:" + name, "pwsh": "$env:" + name}.get(
            family, "$" + name
        )
    return "$" + name


def load_cases(path=None):
    with Path(path or HERE / "corpus.txt").open() as f:
        return [line.rstrip("\n").split("\t") for line in f if line.strip()]


def main(argv):
    if "--list" in argv:
        for k, (fam, _, want) in QUOTERS.items():
            print(f"{k:<14} {fam:<7} {want}")
        return 0
    if "--" not in argv:
        sys.exit(__doc__)
    cut = argv.index("--")
    opts, interp = argv[:cut], argv[cut + 1 :]
    shim, family, name, env, quoter, corpus = None, None, "v", False, None, None
    i = 0
    while i < len(opts):
        o = opts[i]
        if o == "--shim":
            shim = shlex.split(opts[i + 1])
            i += 1
        elif o == "--family":
            family = opts[i + 1]
            i += 1
        elif o == "--var":
            name = opts[i + 1]
            i += 1
        elif o == "--corpus":
            corpus = opts[i + 1]
            i += 1
        elif o == "--env":
            env = True
        else:
            quoter = o
        i += 1
    if quoter:
        family, fn, _ = QUOTERS[quoter]
    if family not in SET or not interp or not (quoter or shim) or (quoter and name != "v"):
        sys.exit(__doc__)
    cases = load_cases(corpus)
    rows, expect = [], {}
    for case, hexv in cases:
        raw = bytes.fromhex(hexv)
        if quoter:
            try:
                stmt = SET[family][0](fn(raw.decode()))
            except ValueError:
                expect[case] = "REJECTED"
                continue
        else:
            r = subprocess.run(  # noqa: S603  the shim is the emitter under test, called once per case
                [*shim, name, hexv], capture_output=True, check=False
            )
            if r.returncode == 65 and not r.stdout:
                expect[case] = "REJECTED"
                continue
            if r.returncode != 0:
                expect[case] = "SHIM-FAILED"
                continue
            stmt = r.stdout.decode()
        if b"\x00" in raw:
            expect[case] = "NOT-REJECTED"
            continue
        rows.append((case, stmt))
        expect[case] = hexv
    drv = DRIVER[family].replace("NAME", name).replace("{R}", reads(family, name, env))

    def b64(s):
        return base64.b64encode(s.encode()).decode()

    body = drv + "".join(f"{CALL[family]} {c} {b64(s)}\n" for c, s in rows)
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / f"round.{SET[family][1]}"
        path.write_text(body)
        work = Path(tmp) / "cwd"
        work.mkdir()
        r = subprocess.run(  # noqa: S603  the interpreter under test is the operand
            [*interp, str(path)], capture_output=True, timeout=600, cwd=work, check=False
        )
        canary = (work / "pwned").exists()
    got = {}
    for line in r.stdout.decode("utf-8", "replace").splitlines():
        m = re.match(r"^([A-Za-z0-9_]+):(.*)$", line.strip())
        if m:
            got[m.group(1)] = m.group(2).strip()
    fails = [
        c for c, e in expect.items() if e not in ("REJECTED", "NOT-REJECTED") and got.get(c) != e
    ]
    n = sum(1 for e in expect.values() if e not in ("REJECTED", "NOT-REJECTED"))
    label = quoter or "shim"
    verdict = " FAIL " + ",".join(fails) if fails else ""
    print(f"{label} on {' '.join(interp)}: {n - len(fails)}/{n}{verdict}")
    nul = [c for c, e in expect.items() if e in ("REJECTED", "NOT-REJECTED")]
    for c in nul:
        print(f"{c}: {expect[c]}")
    bad = (
        bool(fails)
        or any(expect[c] == "NOT-REJECTED" for c in nul)
        or "SHIM-FAILED" in expect.values()
    )
    if canary:
        print("CANARY: a file named pwned appeared, a payload executed")
        bad = True
    if not got:
        err = r.stderr.decode("utf-8", "replace")[:200]
        print(f"no output from the interpreter: exit {r.returncode}, stderr {err!r}")
        bad = True
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
