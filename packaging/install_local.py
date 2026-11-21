"""Build and install Board as a desktop app for the current Linux or macOS user."""

import argparse
import configparser
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import tempfile

from release import project_version


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


def validate_binaries(binaries):
    for name in ("board", "board-ui"):
        source = binaries / name
        if not source.is_file() or not os.access(source, os.X_OK):
            raise ValueError(f"missing executable: {source}; build first or omit --no-build")


def install(binaries, home, data_home):
    bin_dir = home / ".local/bin"
    ui = bin_dir / "board-ui"
    if any(ord(char) < 32 or ord(char) == 127 or char in "=%" for char in str(ui)):
        raise ValueError("the installation path cannot contain control characters, '=' or '%'")
    validate_binaries(binaries)

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


def install_macos(binaries, home):
    validate_binaries(binaries)
    app = home / "Applications/Board.app"
    bin_dir = home / ".local/bin"
    if app.is_symlink():
        raise ValueError(f"refusing to replace a symlink: {app}")
    if app.exists():
        with (app / "Contents/Info.plist").open("rb") as file:
            if plistlib.load(file).get("CFBundleIdentifier") != APP_ID:
                raise ValueError(f"refusing to replace an unrelated app: {app}")
    for name in ("board", "board-ui"):
        if (bin_dir / name).is_dir():
            raise ValueError(f"cannot replace a directory: {bin_dir / name}")
    version = project_version()
    app.parent.mkdir(parents=True, exist_ok=True)
    # Build and sign off to the side; a failed build must leave the old app intact.
    with tempfile.TemporaryDirectory(prefix=".board-install-", dir=app.parent) as tmp:
        staging = Path(tmp)
        staged_app = staging / "Board.app"
        contents = staged_app / "Contents"
        resources = contents / "Resources"
        resources.mkdir(parents=True)
        with (ROOT / "packaging/macos/Info.plist.in").open("rb") as file:
            info = plistlib.load(file)
        info.update(CFBundleVersion=version, CFBundleShortVersionString=version)
        with (contents / "Info.plist").open("wb") as file:
            plistlib.dump(info, file)
        for name in ("board", "board-ui"):
            install_file(binaries / name, contents / "MacOS" / name, 0o755)
        icons = staging / "Board.iconset"
        icons.mkdir()
        for size in (16, 32, 128, 256, 512):
            for scale in (1, 2):
                pixels = str(size * scale)
                suffix = "@2x" if scale == 2 else ""
                subprocess.run(["sips", "-z", pixels, pixels, str(ROOT / "packaging/icons/board.png"),
                                "--out", str(icons / f"icon_{size}x{size}{suffix}.png")],
                               check=True, stdout=subprocess.DEVNULL)
        subprocess.run(["iconutil", "-c", "icns", str(icons), "-o", str(resources / "Board.icns")], check=True)
        # Ad hoc signatures are for local use, not notarized release distribution.
        for name in ("board", "board-ui"):
            subprocess.run(["codesign", "--force", "--sign", "-", "--identifier", f"{APP_ID}.{name}",
                            str(contents / "MacOS" / name)], check=True)
        subprocess.run(["codesign", "--force", "--sign", "-", str(staged_app)], check=True)
        subprocess.run(["codesign", "--verify", "--deep", "--strict", str(staged_app)], check=True)
        previous = staging / "previous.app"
        if app.exists():
            app.rename(previous)
        try:
            staged_app.rename(app)
        except OSError:
            if previous.exists():
                previous.rename(app)
            raise
    bin_dir.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".board-links-", dir=bin_dir) as tmp:
        for name in ("board", "board-ui"):
            link = Path(tmp) / name
            link.symlink_to(app / "Contents/MacOS" / name)
            link.replace(bin_dir / name)
    return app


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--no-build", action="store_true", help="install existing release binaries")
    parser.add_argument("--target-dir", type=Path,
                        default=Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")),
                        help="Cargo target directory (default: CARGO_TARGET_DIR or target/)")
    args = parser.parse_args()
    if sys.platform not in ("linux", "darwin"):
        parser.error("this local installer supports Linux and macOS; see docs/INSTALL.md for other platforms")
    home = Path.home().resolve()
    data_home = Path(os.environ.get("XDG_DATA_HOME") or home / ".local/share")
    if not data_home.is_absolute():
        data_home = home / ".local/share"
    target = args.target_dir.resolve()
    try:
        if sys.platform == "darwin":
            for tool in ("sips", "iconutil", "codesign"):
                if not shutil.which(tool):
                    raise ValueError(f"missing macOS tool: {tool}; install Xcode Command Line Tools")
        if not args.no_build:
            env = dict(os.environ)
            if sys.platform == "darwin":
                env["MACOSX_DEPLOYMENT_TARGET"] = "14.0"
            subprocess.run(["cargo", "build", "--locked", "--release", "-p", "board-cli",
                            "-p", "board-ui", "--bins", "--target-dir", str(target)],
                           cwd=ROOT, env=env, check=True)
        installed = (install_macos(target / "release", home) if sys.platform == "darwin"
                     else install(target / "release", home, data_home))
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"Board installation failed: {error}\n")
    if sys.platform == "linux" and shutil.which("update-desktop-database"):
        result = subprocess.run(["update-desktop-database", str(installed.parent)], check=False)
        if result.returncode:
            print("Warning: launcher cache refresh failed; Board's launcher was installed.", file=sys.stderr)
    print(f"Installed Board: {installed}")
    if sys.platform == "darwin":
        print(f'Open with: open "{installed}"')
        print("Then choose Options > Keep in Dock on Board's Dock icon.")
    else:
        print("Open Board from your application launcher, then pin it to your dock.")
    if str(home / ".local/bin") not in os.environ.get("PATH", "").split(os.pathsep):
        print(f"For CLI commands, add {home / '.local/bin'} to PATH.")


if __name__ == "__main__":
    main()
