# ocx_env_apply

How one environment record is applied to an existing value (move-to-front for `PATH`-style lists, move-to-back for separator lists, replace for constants), and the `.launch` sidecar wire types a vendored launcher reads. Shared by `ocx` and the direct launcher, which carries a size budget.

**Tier:** ecosystem

**May depend on:** none (`serde` and `serde_json` are its only dependencies)

**Named dependency exceptions:** none
