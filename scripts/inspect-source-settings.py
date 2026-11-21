"""Create a source in the installed native UI on an empty offline board; keep its screenshots."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("out", type=Path, help="new directory for native PNGs and element trees")
    parser.add_argument("--bin-dir", type=Path, default=Path(__file__).resolve().parents[1] / "target/debug")
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(mode=0o700, parents=True, exist_ok=False)
    binaries = args.bin_dir.resolve()
    with tempfile.TemporaryDirectory(prefix="board-source-ui-") as temp:
        scratch = Path(temp)
        env = {k: v for k, v in os.environ.items() if not k.startswith("BOARD_")}
        env.update(BOARD_DB=str(scratch / "board.sqlite3"), BOARD_USER="operator",
                   BOARD_UI_THEME="dark", RUNE_DIRS_ROOT=str(scratch / "rune"))
        software = Path("/usr/lib/chromium/vk_swiftshader_icd.json")
        if not Path("/dev/dri").exists() and software.is_file() and "VK_ICD_FILENAMES" not in env:
            env.setdefault("VK_DRIVER_FILES", str(software))

        def cli(*argv):
            return json.loads(subprocess.check_output([binaries / "board", "--json", *argv], env=env, timeout=30))

        def scene(name, inputs, required, size="1120x700"):
            events = []
            for i, item in enumerate(inputs, 1):
                event = {"type": "tap", "key": item} if isinstance(item, str) else {"type": "ime", "commit": item[0]}
                events.append({"t": i * 0.25, **event})
            script = scratch / "input.json"
            script.write_text(json.dumps(events))
            tree = out / (name + ".json")
            subprocess.run([binaries / "board-ui", "--script", script, "--size", size, "--scale", "1",
                            "--at", str(len(events) * 0.25 + 0.5), "--fps", "12", "--tree", tree,
                            "--snap", out / (name + ".png")], env=env, check=True, capture_output=True, timeout=60)
            state = json.loads(tree.read_text())
            assert not state["warnings"], state["warnings"]
            keys = {e.get("key") for e in state["elements"]}
            assert set(required) <= keys, (name, set(required) - keys)
            print(name)

        assert cli("sources", "list") == []
        settings = ["workspace", "work-sources"]
        scene("sync-empty", settings, ["source-add"])
        form = [*settings, "source-add", "source-provider-github", "source-name", ("project",),
                "source-remote", ("example/project",), "source-channel", ("engineering/issues",)]
        scene("sync-new-github", form, ["source-create", "source-cancel"])
        scene("sync-saved", [*form, "source-create"], ["source-configure"])
        stored = cli("sources", "show", "project")
        assert (stored["provider"], stored["remote"], stored["channel"], stored["outbound"]) == (
            "github", "example/project", "engineering/issues", False), stored
        assert not stored["settings"]["mirror_files"] and not stored["settings"]["mirror_roots"]
        # A new process opens the saved source and sees a setting written by a different interface.
        cli("sources", "configure", "project", "--github-items", "issues")
        scene("sync-configure", [*settings, "source-project", "source-configure"],
              ["source-settings-save", "source-gh-issues"])
        assert cli("sources", "show", "project")["settings"]["github_items"] == "issues"
        print("Saved from Rune; CLI read the mapping; reopened from SQLite. Publishing stayed off.")


if __name__ == "__main__":
    main()
