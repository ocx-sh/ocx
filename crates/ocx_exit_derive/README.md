# ocx_exit_derive

`#[derive(Classify)]`: an error type declares its exit code and `error.detail` slug next to each variant, and the
derive writes `ClassifyExitCode`, `ClassifyErrorKind::kind_detail`, and the `DETAILS` rows
from those attributes. The grammar is in the crate docs; `ocx_exit` re-exports the derive, so a
consumer names `ocx_exit::Classify` and never this crate.

**Tier:** internal

**May depend on:** none (`proc-macro2`, `quote` and `syn` are its only dependencies; it names no workspace crate)

**Named dependency exceptions:** none
