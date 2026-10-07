# ocx_env

The environment registry: every variable OCX reads, declared once with its doc, value kind, visibility, secrecy and
child propagation, and the one seam that reads them.

**Tier:** ecosystem

**May depend on:** `ocx_exit` (`tempfile` under `__testing`)

**Named dependency exceptions:** none

## What it holds

| Item | Contract |
|---|---|
| `env_vars!` | Declares variables: one `static` per entry (`EnvVar`, or `SecretVar` for a `secret` entry) plus one `DECLARED` list per invocation. `Testing` entries exist only under `cfg(any(test, feature = "__testing"))` of the invoking crate, so an ungated read fails a release build. Satellites declare their own variables with it and need a `__testing` feature of their own. |
| `EnvVar` | A declaration and its readers. `get`/`get_os` treat an empty value as unset; `get_raw` returns the value verbatim; `bool_or` honours `on_invalid` and names the key, never the value. |
| `SecretVar`, `Sensitive` | A secret reads only as `Sensitive`, which has no `Display` and debug-prints `<redacted>`. |
| `dynamic` | Reads a name that comes from data (`${self.env.KEY}`, a forge passthrough, a computed auth name). |
| `RETIRED` | Renamed and removed spellings: a `Window` entry is honoured with a warning until its removal release, a `Removed` one is refused. |
| `overrides` | `__testing` only: the process-wide table tests set variables through instead of `std::env::set_var`. |

The `__testing` feature is enabled from `[dev-dependencies]` only — resolver v3 keeps a dev-dependency feature out of
the normal build.
