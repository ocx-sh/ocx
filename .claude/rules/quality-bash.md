---
paths:
  - "**/*.sh"
  - "**/*.bash"
---

# Bash Script Quality

Bash-specific quality guide (2026). Universal design principles live in
`quality-core.md` — this file covers **Bash-specific safety, quoting, and
tooling** conventions.

Project-independent and shareable.

---

## Required Script Header

Every Bash script in production MUST start with:

```bash
#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'
```

- `-e`: exit on any command returning non-zero
- `-u`: treat unset variables as errors (catches typos)
- `-o pipefail`: pipe fails if any stage fails, not just the last
- `IFS=$'\n\t'`: prevents word-splitting on spaces — critical for filenames with spaces

Add a cleanup trap for any script that creates temp files or needs teardown:

```bash
cleanup() { rm -rf "$tmpdir"; }
trap cleanup EXIT
```

`EXIT` fires on both normal exit and error — prefer over individual `ERR`/`INT`/`TERM` traps for general cleanup.

---

## Anti-Patterns (Bash-Specific)

### Block (must fix before merge)

1. **Missing `set -euo pipefail`** — silently continues after errors.
2. **Unquoted `$var`** in command position — word-splits and globs unexpectedly.
3. **`rm -rf $dir`** with unquoted or potentially-empty variable — catastrophic on empty string.
4. **`eval` on user input** — shell injection.
5. **Storing structured data as strings** and parsing with `grep`/`sed` — use arrays or JSON tools (`jq`).
6. **Reading filenames with `for f in $(ls)`** — breaks on spaces. Use `for f in ./*` or `find … -print0 | xargs -0`.
7. **Piping into `while read`** and assigning variables — subshell scope loss. Use process substitution: `while read; do …; done < <(cmd)`.
8. **`if [ $? -eq 0 ]`** — use `if cmd; then` directly.
9. **`[ ]` instead of `[[ ]]`** for string comparisons in Bash — `[ ]` has POSIX word-splitting risks.
10. **Ignoring shellcheck warnings** without an inline directive and an explanation.
11. **Missing `--` separator** before a user-supplied variable in `rm`, `grep`, etc. — variable starting with `-` is treated as a flag.
12. **Missing `local`** in function-scoped variables — globally mutable by default.
13. **`set -e` with `|| true` sprinkled everywhere** — defeats the purpose. If you're absorbing a specific failure, comment why.

### Warn (should fix)

- **Heredocs without `<<'EOF'`** when interpolation is unintended
- **Temporary files without `mktemp`** — collisions and symlink attacks
- **Scripts over 200 lines** without function decomposition
- **No `main` function** — all code at top level, no clear entry point
- **`pipefail` + `grep` gotcha** — `grep` exits 1 on no match, which `pipefail` propagates. Use `|| true` when that's intentional, and comment it
- **Associative arrays without Bash version check** (`bash >=4`)
- **Subshell variable scoping surprises** — variables set inside `(...)` don't persist outside

---

## Quoting Rules

- **ALWAYS double-quote variable expansions**: `"$var"`, `"${var}"`, `"${arr[@]}"`
- `"$var"` for brevity; `"${var}"` when needed to disambiguate: `"${var}_suffix"`
- `"$(cmd)"` over backticks — nestable and readable
- Glob patterns in conditions are NOT quoted: `[[ $file == *.txt ]]`
- **Arrays**: `"${arr[@]}"` preserves each element as a separate argument; `"${arr[*]}"` collapses to one string

Build command arguments as arrays, not concatenated strings:

```bash
local args=()
args+=("--flag" "$value")
args+=("--output" "$output")
cmd "${args[@]}"
```

---

## Shellcheck Discipline

- Run at `shellcheck -S warning` — treats `warning` and `error` as blocking; `info`/`style` are advisory
- **Never blanket-suppress.** Use inline directives with comments:
  ```bash
  # shellcheck disable=SC2034  # intentionally unused, sourced externally
  ```
- **Legitimate to suppress**: `SC1091` (can't follow sourced file), `SC2154` (variable defined in sourced file)
- **Never suppress**: `SC2086` (unquoted variable), `SC2068` (array expansion without quotes), `SC2046` (unquoted command substitution)
- `shfmt -i 4 -ci` for formatting (4-space indent, switch cases aligned)

---

## POSIX sh vs Bash

| Use POSIX sh (`#!/bin/sh`) when... | Use Bash when... |
|------------------------------------|------------------|
| Minimal containers (Alpine, distroless) | You need arrays, associative arrays, `[[ ]]`, `(( ))` |
| Cross-platform portability (BSD, macOS `/bin/sh` is dash) | You need process substitution, `mapfile`, `nameref` |
| Short glue scripts with no complex logic | Long-lived scripts benefiting from Bash-specific safety features |

For CI/CD (GitHub Actions), Bash is always available — use it. For Docker `ENTRYPOINT` in minimal images, use POSIX sh or install Bash explicitly.

---

## Testing

- **bats-core**: TAP-compliant, simple, widely used in CI. Good for integration tests ("run the script, check output").
- **ShellSpec**: BDD-style, pure POSIX, supports mocking/stubbing and parameterized tests. Better for unit-level isolation.

Recommendation: bats-core for smoke tests, ShellSpec when you need mocking.

---

## Code Review Checklist (Bash-Specific)

See `quality-core.md` for the universal review checklist. Bash-specific additions:

- [ ] Script starts with `#!/usr/bin/env bash`, `set -euo pipefail`, `IFS=$'\n\t'`
- [ ] All variable expansions quoted
- [ ] Functions use `local` for scoped variables, `readonly` for constants
- [ ] `trap … EXIT` for cleanup when temp files or resources acquired
- [ ] `shellcheck` and `shfmt` both pass
- [ ] No `eval` on unsanitized input
- [ ] Arrays used for command-arg construction, not string concatenation
- [ ] `--` separator used before user-supplied variables in destructive commands
- [ ] `[[ ]]` for string comparisons (not `[ ]`)
- [ ] Scripts > 50 lines have a `main` function

---

## Sources

Authoritative references used in this rule:

- [ShellCheck wiki](https://github.com/koalaman/shellcheck/wiki)
- [Shell Script Best Practices (sharats.me)](https://sharats.me/posts/shell-script-best-practices/)
- [ShellSpec documentation](https://shellspec.info/why.html)
- [Google Shell Style Guide](https://google.github.io/styleguide/shellguide.html)
