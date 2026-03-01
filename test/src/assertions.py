from pathlib import Path


def assert_path_exists(path: Path, msg: str = ""):
    assert path.exists(), f"Expected path to exist: {path}. {msg}"


def assert_dir_exists(path: Path, msg: str = ""):
    assert path.is_dir(), f"Expected directory to exist: {path}. {msg}"


def assert_symlink_exists(path: Path, msg: str = ""):
    assert path.is_symlink(), f"Expected symlink: {path}. {msg}"


def assert_not_exists(path: Path, msg: str = ""):
    assert not path.exists() and not path.is_symlink(), (
        f"Expected path not to exist: {path}. {msg}"
    )
