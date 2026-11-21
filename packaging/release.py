"""Shared release metadata, notices and download manifests. Standard library only."""

import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess


ROOT = Path(__file__).resolve().parents[1]
HERE = ROOT / "packaging"
MAC = "Board-macOS-universal.zip"
WIN = "Board-Windows-x64.msi"
DEB = "board-linux-amd64.deb"
ARCH = "board-linux-x86_64.pkg.tar.zst"
ASSETS = {MAC, WIN, DEB, ARCH, "board.rb", "install-macos.sh", "install-linux.sh"}


def version(tag):
    match = re.fullmatch(r"v((?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*))", tag)
    if not match or any(n > limit for n, limit in zip(map(int, match[1].split(".")), (255, 255, 65535))):
        raise ValueError("expected vMAJOR.MINOR.PATCH within Windows Installer version limits")
    return match[1]


def metadata(*args):
    result = subprocess.check_output(["cargo", "metadata", "--locked", "--format-version", "1", *args], cwd=ROOT)
    return json.loads(result)["packages"]


def project_version(tag=None):
    versions = {p["version"] for p in metadata("--no-deps") if p["name"] in {"board-cli", "board-ui"}}
    if len(versions) != 1:
        raise ValueError("board-cli and board-ui must have the same version")
    current = version("v" + versions.pop())
    if tag and version(tag) != current:
        raise ValueError(f"tag {tag!r} does not match Cargo version {current}")
    return current


def digest(path):
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def render(template, **values):
    text = template.read_text(encoding="utf-8")
    return re.sub(r"@([A-Z][A-Z_0-9]*)@", lambda m: str(values[m[1]]), text)


def notices(target, dest):
    # Include the applicable shipped texts, not a snapshot of the checkout or Cargo cache.
    packages = metadata("--filter-platform", target)
    dest.parent.mkdir(parents=True, exist_ok=True)
    with dest.open("w", encoding="utf-8", newline="\n") as out:
        def include(path, label):
            out.write(f"\n--- {label} ---\n{path.read_text(encoding='utf-8', errors='replace')}\n")

        for path in [ROOT / "LICENSE", ROOT / "LICENSE-MIT", ROOT / "THIRD_PARTY.md", *sorted((ROOT / "ui/licenses").glob("*.txt"))]:
            include(path, path.relative_to(ROOT))
        for p in sorted((p for p in packages if p["source"]), key=lambda p: p["id"]):
            label = f"{p['name']} {p['version']}"
            out.write(f"\n=== {label} ===\nSource: {p['source']}\nLicense: {p['license'] or p['license_file']}\n")
            base = Path(p["manifest_path"]).parent
            def is_notice(path):
                return re.match(r"^(licen[cs]e|copying|copyright|notice|third.party.notices)(?:s|$|[._-])", path.name, re.I)

            candidates = {f for f in base.iterdir() if f.is_file() and is_notice(f)}
            for directory in (base / "licenses", base / "LICENSES"):
                if directory.is_dir():
                    candidates.update(f for f in directory.rglob("*") if f.is_file())
            if p["license_file"]:
                candidates.add((base / p["license_file"]).resolve())
            if p["source"].startswith("git+"):
                checkout = next((d for d in (base, *base.parents) if (d / ".git").exists()), None)
                if checkout:
                    candidates.update(f for f in checkout.iterdir() if f.is_file() and is_notice(f))
            for f in sorted(candidates):
                include(f, f"{label}: {f.name}")


def mac_manifest(tag, out):
    cask = out / "board.rb"
    cask.write_text(render(HERE / "macos/board.rb.in", VERSION=version(tag), ZIP_SHA256=digest(out / MAC)), encoding="utf-8")
    (out / "install-macos.sh").write_text(render(HERE / "macos/install.sh.in", TAG=tag, CASK_SHA256=digest(cask)), encoding="utf-8")


def linux_manifest(tag, out):
    version(tag)
    (out / "install-linux.sh").write_text(render(HERE / "linux/install.sh.in", TAG=tag, DEB_SHA256=digest(out / DEB), ARCH_SHA256=digest(out / ARCH)), encoding="utf-8")


def checksums(out):
    found = {f.name for f in out.iterdir() if f.name != "SHA256SUMS"}
    if found != ASSETS:
        raise ValueError(f"unexpected release assets: missing {ASSETS - found}; extra {found - ASSETS}")
    if any(not (out / f).is_file() or (out / f).is_symlink() or not (out / f).stat().st_size for f in found):
        raise ValueError("empty or non-regular release asset")
    (out / "SHA256SUMS").write_text("".join(f"{digest(out / f)}  {f}\n" for f in sorted(found)), encoding="ascii")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("version").add_argument("tag", nargs="?")
    p = commands.add_parser("notices")
    p.add_argument("target")
    p.add_argument("dest", type=Path)
    for cmd in ("mac-manifest", "linux-manifest"):
        p = commands.add_parser(cmd)
        p.add_argument("tag")
        p.add_argument("out", type=Path)
    commands.add_parser("checksums").add_argument("out", type=Path)
    args = parser.parse_args()
    if args.command == "version":
        print(project_version(args.tag))
    elif args.command == "notices":
        notices(args.target, args.dest)
    elif args.command == "mac-manifest":
        mac_manifest(args.tag, args.out)
    elif args.command == "linux-manifest":
        linux_manifest(args.tag, args.out)
    else:
        checksums(args.out)
