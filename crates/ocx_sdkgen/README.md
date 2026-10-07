# ocx_sdkgen

The published-document contract: the JSON Schema subset `reports`, `errors` and `cli.json` may use, and the lint that
holds them to it.

**Tier:** internal

**May depend on:** none

**Named dependency exceptions:** none

## What it holds

| Item | Contract |
|---|---|
| `subset` | The keyword allowlist, the `x-ocx-*` markers, the vocabulary `$def` names and the shape predicates (unknown arm, tagged arm, opaque leaf) every reader of the documents shares. |
| `lint::run` | Rules `L01`–`L18` over a schema document and `C01`–`C09` over the command grammar, plus the counts of what it read. |
| `lint::cross` | `C09`'s cross-document half: every root a command names is published in `reports`. |
| `lint::reconcile` | Subtracts `crates/ocx_schema/contract/exemptions.toml` (permanent, each citing a decision record) and `contract/waivers/<area>.toml` (a ratchet that only shrinks) from the findings; an unmatched finding, a stale entry or an exemption without `adr` reds. |

It reads the documents as JSON and links no `ocx_*` crate, so a waiver or golden edit never rebuilds the generator.
