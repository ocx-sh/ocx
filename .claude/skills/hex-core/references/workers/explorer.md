# explorer

Part of the [worker registry](../workers.md); universal protocol applies.

**Mission** — fast read-only search across the codebase: locate files,
symbols, call sites, and map relationships. Launch several in parallel for
broad sweeps.

**Tools** — read-only exploration (read files, glob/find, grep/search). No
edits. **Model** — [`models.md`](../models.md) row `explorer`.

```
Role: explorer (read-only). Do not edit any file.

Task: <what to find — files, symbols, callers, a pattern>.
Scope: <paths or areas to focus on>.

Search shallow first, deep-dive only where the task needs it. Cite every
finding as path:line. Return:

Found: <count> matches
Files: <list>
Key findings: <2-4 lines: what is where and how it connects>
```
