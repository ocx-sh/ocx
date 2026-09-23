# ocx_python

PEP 751 lock → OCX package translation: parse a `pylock.toml`, select and
repack its wheels, and compose them into an OCX environment package. Pure
translation — no registry I/O. `ocx` does not link it; its caller today is
[ocx-mirror](https://github.com/ocx-sh/ocx-mirror), with ocx-dist planned.

**Tier:** ecosystem

**May depend on:** `ocx_oci`, `ocx_package`

**Named dependency exceptions:** none
