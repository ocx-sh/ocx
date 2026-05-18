"""Entrypoint for ``task test:doc-scripts:list``.

Emits the JSON discovery export consumed by the website publish task (PT6 seam).
Output is ``json.dumps`` of ``doc_scripts_export(root)`` with absolute ``path``
values, printed to stdout.  No logging, no progress — quiet by design.

The root directory is taken from the ``OCX_DOC_SCRIPTS_ROOT`` environment
variable when set, otherwise defaults to ``<project_root>/test/doc_scripts``.
"""
from __future__ import annotations

import json
import os
import sys
from pathlib import Path

# Resolve the project root relative to this script's location:
# test/scripts/doc_scripts_list.py → test/ → project_root
_HERE = Path(__file__).resolve()
_TEST_DIR = _HERE.parent.parent
_PROJECT_ROOT = _TEST_DIR.parent

# Add test/ to sys.path so ``src.doc_scripts`` is importable.
if str(_TEST_DIR) not in sys.path:
    sys.path.insert(0, str(_TEST_DIR))

from src.doc_scripts import doc_scripts_export  # noqa: E402


def main() -> None:
    root_override = os.environ.get("OCX_DOC_SCRIPTS_ROOT")
    if root_override:
        root = Path(root_override)
    else:
        root = _PROJECT_ROOT / "test" / "doc_scripts"

    export = doc_scripts_export(root)
    print(json.dumps(export))


if __name__ == "__main__":
    main()
