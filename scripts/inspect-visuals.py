"""Render the comparison board with the real native inspector. No server or network."""
import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile


def compare_geometry(name, tree, width, height, thread):
    """Bounds observed in the reference browser; allow two native layout points."""
    rects = {el["key"]: el["rect"] for el in tree["elements"] if el.get("key")}
    wide = width > 900
    left = 224 if wide else 0
    middle = width - left - (440 if wide and thread else 0)
    expected = {}
    if wide:
        expected["channels"] = (0, 0, 224, height)
        expected["sidebar-header"] = (0, 0, 224, 46)
    if not thread or wide:
        expected["feed-header"] = (left, 0, middle, 46)
        box_height = 137 if wide and thread else 136
        expected["inactive-main-composer" if thread else "main-composer"] = (
            left + 20, height - 20 - box_height, middle - 40, box_height)
    if thread:
        x = width - 440 if wide else 0
        expected["thread-header"] = (x, 0, width - x, 46)
        expected["reply-composer"] = (x + 20, height - 120, width - x - 40, 100)
    for key, target in expected.items():
        assert all(abs(a - b) <= 2 for a, b in zip(rects[key], target)), (name, key, rects[key], target)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("out", type=Path, help="new directory for PNGs and element trees")
    parser.add_argument("--bin-dir", type=Path, default=Path(__file__).resolve().parents[1] / "target/debug")
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(mode=0o700, parents=True, exist_ok=False)
    with tempfile.TemporaryDirectory(prefix="board-inspector-") as tmp:
        db = Path(tmp) / "board.sqlite3"
        env = {k: v for k, v in os.environ.items() if not k.startswith("BOARD_")}
        env.update(BOARD_DB=str(db), BOARD_USER="client", RUNE_DIRS_ROOT=str(Path(tmp) / "rune"))
        swift = Path("/usr/lib/chromium/vk_swiftshader_icd.json")
        if not Path("/dev/dri").exists() and swift.is_file() and "VK_ICD_FILENAMES" not in env:
            env.setdefault("VK_DRIVER_FILES", str(swift))

        def cli(author, *argv):
            data = subprocess.check_output([args.bin_dir / "board", "--json", *argv],
                                           env={**env, "BOARD_USER": author}, timeout=30)
            return json.loads(data)

        times = {}

        def post(author, dest, text, day, *options):
            mid = cli(author, "post", str(dest), text, *options)["message"]["id"]
            times[mid] = day
            return mid

        post("mira", "general", "Welcome to the local board. Post handoffs and questions here.", "2026-11-18T17:05:00Z")
        root = post("rowan", "engineering", "The build cache is ready for review.", "2026-11-19T18:20:00Z", "--label", "phase=review")
        cli("mira", "react", str(root), "eyes")
        post("lin", "engineering/build", "Linux and macOS checks passed on the new settings.", "2026-11-20T20:10:00Z")
        reply = post("mira", root, "I checked the cold run. The cache reduced build time to 42 seconds.", "2026-11-21T16:40:00Z")
        post("rowan", "general", "Please leave any final notes before the release.", "2026-11-21T17:15:00Z")
        # Replay the five messages at their measured ages, so a later run still
        # exercises today/yesterday groupings. Only this throwaway DB is backdated.
        clock_delta = datetime.now(timezone.utc) - datetime.fromisoformat("2026-11-21T19:30:00+00:00")
        with sqlite3.connect(db) as conn:
            for mid, timestamp in times.items():
                timestamp = (datetime.fromisoformat(timestamp.replace("Z", "+00:00")) + clock_delta).isoformat(timespec="seconds")
                conn.execute("UPDATE messages SET created_at=? WHERE id=?", (timestamp, mid))
                conn.execute("""UPDATE events SET created_at=?, message=CASE WHEN message IS NULL THEN NULL
                                ELSE json_set(message,'$.created_at',?) END WHERE message_id=?""",
                             (timestamp, timestamp, mid))

        for theme, width, height, thread in [
            ("light", 1200, 800, False), ("light", 1200, 800, True), ("dark", 1200, 800, True),
            ("light", 390, 844, False), ("light", 390, 844, True),
        ]:
            name = f"{'thread' if thread else 'activity'}-{theme}-{width}"
            inputs = Path(tmp) / "input.json"
            inputs.write_text(json.dumps([
                {"t": 0.1, "type": "tap", "key": f"thread-{root}"},
                {"t": 0.3, "type": "tap", "key": "message-body"},
                {"t": 0.45, "type": "move", "x": width - 100, "y": 80},
            ] if thread else []))
            tree = out / f"{name}.json"
            subprocess.run([args.bin_dir / "board-ui", "--script", inputs, "--at", "0.6", "--fps", "12",
                            "--size", f"{width}x{height}", "--scale", "1", "--tree", tree,
                            "--snap", out / f"{name}.png"], env={**env, "BOARD_UI_THEME": theme},
                           check=True, capture_output=True, timeout=60)
            shot = json.loads(tree.read_text())
            assert not shot["warnings"], shot["warnings"]
            keys = {el.get("key") for el in shot["elements"]}
            assert ("thread-pane" in keys) == thread and ("channels" in keys) == (width > 900)
            assert ("feed-pane" in keys) == (not thread or width > 900)
            assert (f"message-{reply}" if thread else f"activity-reply-{reply}") in keys
            compare_geometry(name, shot, width, height, thread)
            print(name)


if __name__ == "__main__":
    main()
