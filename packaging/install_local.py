"""Build and install Board in the current user's Linux application launcher."""

import argparse
import configparser
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile


ROOT = Path(__file__).resolve().parent.parent
APP_ID = "org.localboard.Board"


def desktop_string(value):
    return str(value).replace("\\", "\\\\")


def desktop_exec(path):
    # Desktop entries unescape strings before unquoting Exec arguments.
    value = str(path)
    for char in ('\\', '"', '`', '$'):
        value = value.replace(char, "\\" + char)
    return desktop_string('"' + value + '"')


def install_file(source, destination, mode):
    destination.parent.mkdir(parents=True, exist_ok=True)
    # Replace rather than truncate, so upgrades work while Board is running.
    with tempfile.NamedTemporaryFile(dir=destination.parent, delete=False) as file:
        temporary = Path(file.name)
    try:
        shutil.copyfile(source, temporary)
        temporary.chmod(mode)
        temporary.replace(destination)
    finally:
        temporary.unlink(missing_ok=True)


def install(binaries, home, data_home):
    bin_dir = home / ".local/bin"
    ui = bin_dir / "board-ui"
    if any(ord(char) < 32 or ord(char) == 127 or char in "=%" for char in str(ui)):
        raise ValueError("the installation path cannot contain control characters, '=' or '%'")
    for name in ("board", "board-ui"):
        source = binaries / name
        if not source.is_file() or not os.access(source, os.X_OK):
            raise ValueError(f"missing executable: {source}; build first or omit --no-build")

    entry = configparser.ConfigParser(interpolation=None)
    entry.optionxform = str
    entry.read(ROOT / f"packaging/linux/{APP_ID}.desktop", encoding="utf-8")
    entry["Desktop Entry"]["Exec"] = desktop_exec(ui)
    entry["Desktop Entry"]["TryExec"] = desktop_string(ui)
    desktop = data_home / f"applications/{APP_ID}.desktop"
    for name in ("board", "board-ui"):
        install_file(binaries / name, bin_dir / name, 0o755)
    install_file(ROOT / "packaging/icons/board.svg",
                 data_home / f"icons/hicolor/scalable/apps/{APP_ID}.svg", 0o644)
    with tempfile.TemporaryDirectory(prefix="board-launcher-") as tmp:
        source = Path(tmp) / desktop.name
        with source.open("w", encoding="utf-8") as file:
            entry.write(file, space_around_delimiters=False)
        install_file(source, desktop, 0o644)
    return desktop


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--no-build", action="store_true", help="install existing release binaries")
    parser.add_argument("--target-dir", type=Path,
                        default=Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")),
                        help="Cargo target directory (default: CARGO_TARGET_DIR or target/)")
    args = parser.parse_args()
    if sys.platform != "linux":
        parser.error("this local installer supports Linux; see docs/INSTALL.md for other platforms")
    home = Path.home().resolve()
    data_home = Path(os.environ.get("XDG_DATA_HOME") or home / ".local/share")
    if not data_home.is_absolute():
        data_home = home / ".local/share"
    target = args.target_dir.resolve()
    try:
        if not args.no_build:
            subprocess.run(["cargo", "build", "--locked", "--release", "-p", "board-cli",
                            "-p", "board-ui", "--bins", "--target-dir", str(target)],
                           cwd=ROOT, check=True)
        desktop = install(target / "release", home, data_home)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"Board installation failed: {error}\n")
    if shutil.which("update-desktop-database"):
        result = subprocess.run(["update-desktop-database", str(desktop.parent)], check=False)
        if result.returncode:
            print("Warning: launcher cache refresh failed; Board's launcher was installed.", file=sys.stderr)
    print(f"Installed Board: {desktop}")
    print("Open Board from your application launcher, then pin it to your dock.")
    if str(home / ".local/bin") not in os.environ.get("PATH", "").split(os.pathsep):
        print(f"For CLI commands, add {home / '.local/bin'} to PATH.")


if __name__ == "__main__":
    main()
