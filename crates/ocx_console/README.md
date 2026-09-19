# ocx_console

Presentation vocabulary shared by every OCX binary: tables and trees, styling
and themes, progress bars, and the per-stream colour decision.

**Tier:** ecosystem

**May depend on:** `ocx_exit`, `ocx_util`

**Named dependency exceptions:** none

The tier is closed around `DataInterface`: everything exported here either
builds one or is handed to one. It is shaped for a future `ocx_api` — which
drives the `ocx` binary rather than linking its commands — not for `ocx_cli`'s
convenience, so nothing is public because one caller found it handy.

Three things deliberately live elsewhere. `ExitCode` and `ErrorCategory` are
`ocx_exit`'s, so an SDK can link the process-outcome vocabulary without linking
a terminal. Argument dispatch and the input-validation errors a command raises
before it does any work are `ocx_cli`'s, because they belong to the process that
owns `main`. And no item here names `tracing-subscriber`: installing a global
subscriber is a `main`-owner's privilege, which is what lets this manifest omit
the dependency. `progress::LogWriter` is a plain per-event buffer the binary
adapts to whatever writer trait its subscriber asks for.
