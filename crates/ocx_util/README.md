# ocx_util

Domain-free primitives: fs, locking, extension traits, async singleflight, TLS roots, archive extraction, path-context error helpers.

**Tier:** ecosystem

**May depend on:** none

**Named dependency exceptions:** none

`ocx_lib::utility::X` is `ocx_util::X` — the module root that was `utility.rs`
is `src/lib.rs`, so the tier's own name appears in no path. `archive`,
`compression` and `tls` were top-level modules of `ocx_lib` and are top-level
modules here; `utility::tls` (the bundled Mozilla seed) is `tls::embedded_roots`,
under the module it always documented itself against.

The `__testing` feature gates `env::overrides`, the process-environment
override table tests write through. Enable it from `[dev-dependencies]` only —
resolver v3 keeps a dev-dependency feature out of the normal build, so a
release binary physically lacks the table.
