"""Build or verify both native Linux packages without installing anything on the builder."""

import argparse
import configparser
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import xml.etree.ElementTree as ET

from release import ARCH, DEB, HERE, digest, linux_manifest, notices, project_version, render
from smoke_init import verify as verify_init


APP_ID = "org.localboard.Board"


def run(*cmd, **options):
    return subprocess.run(cmd, check=True, **options)


def install(src, dest, mode=0o644):
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(src, dest)
    dest.chmod(mode)


def build(tag, binaries, out):
    version = project_version(tag)
    out.mkdir(parents=True, exist_ok=True)
    glibc = []
    with tempfile.TemporaryDirectory(prefix="board-build-") as tmp:
        build = Path(tmp)
        root = build / "root"
        for name in ("board", "board-ui"):
            src = binaries / name
            elf = run("readelf", "-WhV", src, capture_output=True, text=True, env={**os.environ, "LC_ALL": "C"}).stdout
            if not re.search(r"Machine:\s*Advanced Micro Devices X86-64", elf):
                raise ValueError(f"not a native amd64 ELF: {src}")
            glibc.extend(tuple(map(int, v.split("."))) for v in re.findall(r"GLIBC_(\d+(?:\.\d+)+)", elf))
            install(src, root / "usr/bin" / name, 0o755)
        minimum = ".".join(map(str, max(glibc)))
        print(f"Package runtime requires glibc >= {minimum}", flush=True)
        install(HERE / f"linux/{APP_ID}.desktop", root / f"usr/share/applications/{APP_ID}.desktop")
        install(HERE / "icons/board.svg", root / f"usr/share/icons/hicolor/scalable/apps/{APP_ID}.svg")
        notices("x86_64-unknown-linux-gnu", root / "usr/share/doc/board-local/copyright")
        recipe = build / "arch"
        recipe.mkdir()
        install(HERE / "linux/PKGBUILD", recipe / "PKGBUILD")
        run("makepkg", "--nodeps", "--noconfirm", cwd=recipe, env={**os.environ,
            "BOARD_PACKAGE_ROOT": str(root), "BOARD_PACKAGE_VERSION": version, "BOARD_MIN_GLIBC": minimum,
            "PKGDEST": str(recipe), "PKGEXT": ".pkg.tar.zst", "PACKAGER": "Board maintainers <tomharmon@users.noreply.github.com>",
        })
        arch, = recipe.glob("*.pkg.tar.zst")
        shutil.copyfile(arch, out / ARCH)
        control = root / "DEBIAN/control"
        control.parent.mkdir()
        control.write_text(render(HERE / "linux/control.in", VERSION=version, GLIBC=minimum), encoding="utf-8")
        run("dpkg-deb", "--root-owner-group", "-Zxz", "-z3", "--build", root, out / DEB)
    linux_manifest(tag, out)


def smoke(root, tmp, version):
    prefix = root / "usr"
    desktop = prefix / f"share/applications/{APP_ID}.desktop"
    run("desktop-file-validate", desktop)
    ini = configparser.ConfigParser(interpolation=None)
    ini.read(desktop)
    entry = ini["Desktop Entry"]
    assert entry["Exec"] == entry["TryExec"] == "/usr/bin/board-ui"
    assert entry["Type"] == "Application" and entry["Terminal"] == "false" and entry["Icon"] == APP_ID
    icon = prefix / f"share/icons/hicolor/scalable/apps/{entry['Icon']}.svg"
    assert ET.parse(icon).getroot().tag.endswith("}svg")
    docs = (prefix / "share/doc/board-local/copyright").read_text()
    assert all(t in docs for t in ("Rune-MIT.txt", "Inter-OFL.txt", "Lucide.txt", "rusqlite"))
    home = tmp / "home"
    home.mkdir()
    env = {k: v for k, v in os.environ.items() if not k.startswith("BOARD_")}
    env.update(HOME=str(home), XDG_DATA_HOME=str(home / "data"), XDG_CONFIG_HOME=str(home / "config"),
               XDG_CACHE_HOME=str(home / "cache"), XDG_STATE_HOME=str(home / "state"), BOARD_DB=str(home / "board.sqlite3"))
    assert version in run(prefix / "bin/board", "--version", env=env, capture_output=True, text=True).stdout
    verify_init(prefix / "bin/board")
    ui = root / entry["Exec"].lstrip("/")
    assert "Rune" in run(ui, "--ui-notices", env=env, capture_output=True, text=True).stdout
    assert not list(home.iterdir()), "version and notices must not perform a user init"
    run(ui, "--at", "0.25", "--size", "640x480", "--scale", "1", "--tree", tmp / "tree.json", "--snap", tmp / "board.png", env=env, timeout=120)
    assert json.loads((tmp / "tree.json").read_text())["elements"]
    assert (tmp / "board.png").read_bytes()[:8] == b"\x89PNG\r\n\x1a\n"


def verify(tag, out, binaries):
    version = project_version(tag)
    for name in (DEB, ARCH):
        with tempfile.TemporaryDirectory(prefix="board-test-") as path:
            tmp = Path(path)
            root = tmp / "installed"
            root.mkdir()
            pkg = out / name
            if name == DEB:
                assert run("dpkg-deb", "-f", pkg, "Package", capture_output=True, text=True).stdout.strip() == "board-local"
                assert run("dpkg-deb", "-f", pkg, "Version", capture_output=True, text=True).stdout.strip() == version
                run("dpkg-deb", "-e", pkg, tmp / "control")
                assert {f.name for f in (tmp / "control").iterdir()} == {"control"}, "unexpected maintainer script"
                run("dpkg-deb", "-x", pkg, root)
            else:
                run("bsdtar", "-xf", pkg, "-C", root)
                assert not (root / ".INSTALL").exists()
                info = (root / ".PKGINFO").read_text().splitlines()
                assert "pkgname = board-local" in info and f"pkgver = {version}-1" in info and "arch = x86_64" in info
            files = list(root.rglob("*"))
            assert all(not p.is_symlink() and (p.is_dir() or p.is_file()) for p in files), "unexpected package file type"
            actual = {str(p.relative_to(root)) for p in files if p.is_file()}
            expected = {"usr/bin/board", "usr/bin/board-ui", "usr/share/doc/board-local/copyright", f"usr/share/applications/{APP_ID}.desktop", f"usr/share/icons/hicolor/scalable/apps/{APP_ID}.svg"}
            if name == ARCH:
                expected.update({".BUILDINFO", ".MTREE", ".PKGINFO"})
            assert actual == expected, actual ^ expected
            for binary in ("board", "board-ui"):
                assert digest(root / "usr/bin" / binary) == digest(binaries / binary), f"packaged {binary} differs from the build"
            smoke(root, tmp, version)
        print(f"Verified installed CLI, init dry-run, Rune frame, desktop entry, icon and notices: {pkg}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("build", "verify"))
    parser.add_argument("tag")
    parser.add_argument("--bin-dir", type=Path, default=Path("target/release"))
    parser.add_argument("--out", type=Path, default=Path("target/packages"))
    args = parser.parse_args()
    if args.command == "build":
        build(args.tag, args.bin_dir.resolve(), args.out.resolve())
    else:
        verify(args.tag, args.out.resolve(), args.bin_dir.resolve())
