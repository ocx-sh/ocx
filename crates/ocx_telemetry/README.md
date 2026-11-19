# ocx_telemetry

Usage export. The crate owns the arming gate, the destination selection, the resource and the command and
package records, the installation id, and one bounded OTLP/HTTP logs request per command, sent through ocx's own
HTTP client with no retry and no redirect.

**Tier:** internal

**May depend on:** `ocx_config`, `ocx_env`, `ocx_oci`, `ocx_util`

**Named dependency exceptions:** none

The gate reads only the environment and the merged `[telemetry]` section. Past it, nothing is built, read or
written unless the CLI armed the run with a destination: an unarmed run, and an armed run with no destination,
construct no client, touch no file and open no connection. The exec family and the per-prompt commands never call the crate at all.
