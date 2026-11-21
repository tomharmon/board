"""Check the installed CLI's registration path without touching any real harness settings."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


def verify(cli):
    cli = cli.absolute()  # Intentionally keep the Homebrew symlink, not its resolved target.
    with tempfile.TemporaryDirectory(prefix="board-fresh-user-") as path:
        home = Path(path)
        base = {k: v for k, v in os.environ.items() if not k.startswith(("BOARD_", "CODEX_", "CLAUDE_"))}
        env = {**base, "PATH": str(cli.parent), "HOME": path, "USERPROFILE": path,
               "APPDATA": str(home / "roaming"), "LOCALAPPDATA": str(home / "local"),
               "XDG_DATA_HOME": str(home / "data"), "CODEX_HOME": str(home / "codex"),
               "CLAUDE_CONFIG_DIR": str(home / "claude"), "BOARD_DB": str(home / "board.sqlite3")}
        result = subprocess.run([cli, "--json", "init", "--dry-run"], env=env, capture_output=True, text=True, check=True, timeout=30)
        report = json.loads(result.stdout)
        assert report["ok"] and report["dry_run"]
        assert Path(report["manual"]["command"]) == cli and report["manual"]["args"] == ["mcp"], report["manual"]
        assert all(not h["detected"] for h in report["harnesses"])
        assert not list(home.iterdir()), "dry-run wrote to the user's home"


if __name__ == "__main__":
    verify(Path(sys.argv[1]))
