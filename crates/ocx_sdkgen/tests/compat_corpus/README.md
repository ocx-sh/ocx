# Compat mutation corpus

Synthetic inputs for the compat gate's differ (`compat::diff(base, current, kind)`). Each directory is one
mutation: `base.json` is a small document of the named kind, `current.json` is the same document with one change,
and `expect.json` is what the differ must report.

```json
{
  "kind": "reports | errors | cli",
  "verdict": "break | non_break",
  "change": "One sentence naming the mutation.",
  "findings": [{ "code": "B02", "pointer": "/$defs/PushReportRoot/properties/message", "subjects": ["PushReport"] }]
}
```

- `findings` is the exact set of coded findings, codes from the differ table: `B`, `D01`, `U01` and `R` for
  `reports`/`errors`, `G` for `cli`. Additive changes report no coded finding, so their list is empty.
- `pointer` names the changed node (property, `$def`, `x-ocx-enum` entry, union arm, command, arg, choice,
  output mode or env entry), never a keyword inside it. `items` and `additionalProperties` are nodes too: a change
  to an array's element or a map's value points at that schema. It points into `base.json` for a removal or change
  and into `current.json` for an addition. `cli.json` pointers are array-index pointers.
- A payload `$def` another root reaches is reported once, at the `$def`, with every reaching root (its own
  wrapper's root included) as subjects; the wrapper's inline mirror reports nothing. An unreachable payload's
  change sits on the wrapper.
- Reordering `x-ocx-enum` entries, union arms, named args or env entries is not a change; moving a positional is.
- `subjects` are the roots (`reports`) or command paths (`cli`) that reach the pointer. Only versioned leaf
  commands count, so a flag that stops being global names every command below it. `errors` and `cli`'s `env` and
  `retired` sections are document-wide: `["*"]`.
- `verdict` is the semantic-break call. `break` needs a ledger entry and a version bump of every subject. A
  `non_break` case carries no finding or only `D01`, which is then a reworded description acknowledged by a `doc`
  entry. A `D01` that changes what the value means is a `break` acknowledged by a `semantic` entry.
- In the exit-code registry (entries carrying `category`) a removed value is `R01`; in the slug registry (entries
  carrying `exit_code`) it is `R03`; elsewhere `B05`.
- Every document other than the two `u01_*` cases' `current.json` is inside the representation subset.

`test/lint/test_compat_corpus.py` holds the corpus to this shape, to every differ code and to the required change
classes.
