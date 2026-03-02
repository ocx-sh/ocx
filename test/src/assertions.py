import os
import sys
from pathlib import Path


def _is_link(path: Path) -> bool:
    """Check if path is a symlink or (on Windows) an NTFS junction point.

    Python's Path.is_symlink() does not detect junction points on Windows.
    """
    if sys.platform == "win32":
        try:
            return bool(
                os.lstat(path).st_file_attributes
                & 0x400  # FILE_ATTRIBUTE_REPARSE_POINT
            )
        except OSError:
            return False
    return path.is_symlink()


def assert_path_exists(path: Path, msg: str = ""):
    assert path.exists(), f"Expected path to exist: {path}. {msg}"


def assert_dir_exists(path: Path, msg: str = ""):
    assert path.is_dir(), f"Expected directory to exist: {path}. {msg}"


def assert_symlink_exists(path: Path, msg: str = ""):
    assert _is_link(path), f"Expected symlink: {path}. {msg}"


def assert_not_exists(path: Path, msg: str = ""):
    assert not path.exists() and not _is_link(path), (
        f"Expected path not to exist: {path}. {msg}"
    )
