"""Benchmark harness for ocx install performance measurement.

Standalone entry point: python -m bench.harness (or uv run python bench/harness.py)

The harness requires:
- Docker bench profile running: task bench:setup
- ocx binary built: task build (via test/taskfile.yml)
- hyperfine on PATH or in test/bin/: task .bench:acquire-hyperfine

Do NOT import this module from pytest tests directly — use bench/conftest.py
fixtures for pytest-based smoke validation only.
"""
