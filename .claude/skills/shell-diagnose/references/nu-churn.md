# Nushell errors by id

You are here because a nu script fails with a `nu::` error id or a warning. Nushell has no 1.0 and removes or renames commands about every release. The texts below are identical on nu 0.105.1 and 0.116.0 except where a row says otherwise. Captured 2026-10-11 with `nu --no-config-file --commands` on each one-liner, and the last column from `nu-check --debug FILE` on the same line written to a file.

Contents: [Table](#table) · [What nu-check sees](#what-nu-check-sees) · [Reading the id](#reading-the-id)

## Table

| Snippet | Exit | Error id and first line | Meaning and replacement | `nu-check --debug` |
|---|---|---|---|---|
| `let-env FOO = bar` | 1 | `nu::parser::assignment_requires_variable`, `Assignment operations require a variable.` | `let-env` was removed. The text does not say so (a bare `let-env` says `Removed command: let-env`). Write `$env.FOO = "bar"` | exit 1 |
| `date format "%Y"` | 1 | `nu::shell::removed_command`, `Removed command: date format` | Use `format date`, as the help text says | exit 0, `true` |
| `str collect ""` | 1 | `nu::parser::extra_positional`, `Extra positional argument.` | Renamed to `str join` | exit 1 |
| `random integer` | 1 | `nu::parser::extra_positional`, `Extra positional argument.` | Renamed to `random int` | exit 1 |
| `each -n {\|x\| $x}` | 1 | `nu::parser::unknown_flag`, ``The `each` command doesn't have flag `-n`.`` | Use `enumerate` | exit 1 |
| `str substring 0 2` | 1 | `nu::shell::cant_convert`, `Can't convert to range.` | The argument is a range, `0..2` | exit 0, `true` |
| `mut n = 0` captured by `each` | 1 | `nu::parser::expected_keyword`, `Capture of mutable variable.` | Use `reduce` | exit 1 |
| `true && echo yes` | 1 | `nu::parser::shell_andand`, ``The '&&' operator is not supported in Nushell`` | Use `;` or `and` | exit 1 |
| `false \|\| echo no` | 1 | `nu::parser::shell_oror`, ``The '\|\|' operator is not supported in Nushell`` | Use `try` or `or` | exit 1 |
| `echo $(date)` | 1 | `nu::parser::parse_mismatch_with_did_you_mean`, `Parse mismatch during operation.` on 0.105.1 and `Parse mismatch: expected valid variable name.` on 0.116.0 | Bash command substitution. Write `(date now)` | exit 1 |
| `"a" \| str upcase` | 0 | 0.116.0 only: `nu::parser::deprecated` warning, `str upcase was deprecated in 0.114.0 and will be removed in a future release.` 0.105.1 is silent | Use `str uppercase` (0.114.0 and later) | exit 0, `true` |
| `1 \| into-int` | 1 | `nu::shell::external_command`, `External command failed` | The hyphenated name is old. A removed or misspelled name falls through to an external binary. Write `into int` | exit 0, `true` |

## What nu-check sees

`nu-check --debug` catches parse errors only. It returned `true` with exit 0 for `date format`, `str substring 0 2`, `str upcase` and `into-int`, so an exit 0 from it is not a clean file. A removed command, a deprecation and a fall-through to an external are found at run time or by the SH-NU-02 gate with `--strict`, never by `nu-check` alone.

On 0.116.0 `nu-check --debug` prints the `nu::parser::` id first. On 0.105.1 it prints only `Failed to parse content` with the parser message after `Found :`, so run the one-liner with `nu --no-config-file --commands` to read the id there.

## Reading the id

- `nu::parser::` ids are parse-time, so nothing in the file ran.
- `nu::shell::` ids are run-time, so the lines before the failing one ran.
- A bareword that is neither a builtin nor a custom command runs whatever binary of that name is on PATH, with exit 0 and a wrong answer (SH-NU-03).
- The do-not-write list and the born-after-the-floor list are SH-NU-04 and SH-NU-05.
