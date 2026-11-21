import configparser
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

import install_local as local


class LocalInstallTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="board-install-test-")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.home = self.root / "home with spaces"
        self.data = self.home / ".local/share"
        self.binaries = self.root / "target/release"
        self.binaries.mkdir(parents=True)
        for name in ("board", "board-ui"):
            path = self.binaries / name
            path.write_text('#!/bin/sh\nprintf launched > "$BOARD_LAUNCH_MARKER"\n')
            path.chmod(0o755)

    def test_installs_binaries_icon_and_matching_terminal_free_launcher(self):
        desktop = local.install(self.binaries, self.home, self.data)
        entry = configparser.ConfigParser(interpolation=None)
        entry.read(desktop)
        app = entry["Desktop Entry"]
        self.assertEqual(app["Exec"], f'"{self.home}/.local/bin/board-ui"')
        self.assertEqual(app["TryExec"], f"{self.home}/.local/bin/board-ui")
        self.assertEqual(app["Terminal"], "false")
        self.assertEqual(app["Icon"], local.APP_ID)
        self.assertEqual(app["StartupWMClass"], local.APP_ID)
        for name in ("board", "board-ui"):
            binary = self.home / ".local/bin" / name
            self.assertEqual(binary.read_bytes(), (self.binaries / name).read_bytes())
            self.assertEqual(binary.stat().st_mode & 0o777, 0o755)
        icon = self.data / f"icons/hicolor/scalable/apps/{local.APP_ID}.svg"
        self.assertEqual(icon.read_bytes(), (local.ROOT / "packaging/icons/board.svg").read_bytes())
        self.assertEqual(desktop.stat().st_mode & 0o777, 0o644)
        if shutil.which("desktop-file-validate"):
            subprocess.run(["desktop-file-validate", desktop], check=True)

    def test_upgrade_replaces_files_without_changing_board_data(self):
        local.install(self.binaries, self.home, self.data)
        database = self.data / "board/board.sqlite3"
        database.parent.mkdir()
        database.write_bytes(b"existing board data")
        old = self.home / ".local/bin/board-ui"
        with old.open("rb") as running:
            (self.binaries / "board-ui").write_text("#!/bin/sh\nexit 0\n")
            local.install(self.binaries, self.home, self.data)
            self.assertIn(b"launched", running.read())
        self.assertEqual(old.read_text(), "#!/bin/sh\nexit 0\n")
        self.assertEqual(database.read_bytes(), b"existing board data")

    def test_missing_binary_fails_before_installing_anything(self):
        (self.binaries / "board-ui").unlink()
        with self.assertRaisesRegex(ValueError, "missing executable"):
            local.install(self.binaries, self.home, self.data)
        self.assertFalse(self.home.exists())

    def test_invalid_launcher_paths_fail_before_installing_anything(self):
        for suffix in ("\nExec=bad", "=bad", "\tbad", "%f"):
            home = self.home / suffix
            with self.subTest(suffix=suffix), self.assertRaises(ValueError):
                local.install(self.binaries, home, self.data)
            self.assertFalse(self.home.exists())

    @unittest.skipUnless(shutil.which("gio"), "GIO is needed to exercise desktop launching")
    def test_launcher_executes_exact_path_with_reserved_characters(self):
        for name in ('spaces', 'quote"', 'backslash\\', 'dollar$', 'tick`'):
            with self.subTest(name=name):
                home = self.home / name
                desktop = local.install(self.binaries, home, self.data)
                marker = self.root / "launched"
                marker.unlink(missing_ok=True)
                result = subprocess.run(["gio", "launch", desktop], check=False,
                                        env={**os.environ, "BOARD_LAUNCH_MARKER": str(marker)},
                                        capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, 0, result.stderr)
                deadline = time.monotonic() + 5
                while not marker.exists() and time.monotonic() < deadline:
                    time.sleep(0.01)
                self.assertEqual(marker.read_text(), "launched")

    def test_cli_honors_xdg_and_runs_from_outside_checkout(self):
        for setting in (str(self.root / "xdg data"), "relative/data", ""):
            with self.subTest(setting=setting):
                result = subprocess.run(
                    [sys.executable, local.ROOT / "packaging/install_local.py", "--no-build",
                     "--target-dir", self.binaries.parent],
                    cwd=self.root, env={**os.environ, "HOME": str(self.home), "XDG_DATA_HOME": setting},
                    capture_output=True, text=True, check=True)
                data = Path(setting) if Path(setting).is_absolute() else self.data
                self.assertTrue((data / f"applications/{local.APP_ID}.desktop").is_file())
                self.assertIn("Open Board from your application launcher", result.stdout)
        self.assertFalse((self.root / "relative").exists())

    def test_cli_builds_both_apps_and_honors_cargo_target_dir(self):
        tools = self.root / "tools"
        tools.mkdir()
        cargo = tools / "cargo"
        cargo.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > "$BOARD_BUILD_LOG"\n')
        cargo.chmod(0o755)
        log = self.root / "build.log"
        env = {**os.environ, "HOME": str(self.home), "XDG_DATA_HOME": str(self.data),
               "PATH": str(tools) + os.pathsep + os.environ.get("PATH", ""),
               "CARGO_TARGET_DIR": str(self.binaries.parent), "BOARD_BUILD_LOG": str(log)}
        subprocess.run([sys.executable, local.ROOT / "packaging/install_local.py"],
                       cwd=self.root, env=env, check=True, capture_output=True, text=True)
        self.assertEqual(log.read_text().splitlines(),
                         ["build", "--locked", "--release", "-p", "board-cli", "-p", "board-ui",
                          "--bins", "--target-dir", str(self.binaries.parent)])
        self.assertTrue((self.home / ".local/bin/board-ui").is_file())

    def test_cli_stops_on_build_failure(self):
        tools = self.root / "tools"
        tools.mkdir()
        cargo = tools / "cargo"
        cargo.write_text("#!/bin/sh\nexit 42\n")
        cargo.chmod(0o755)
        result = subprocess.run(
            [sys.executable, local.ROOT / "packaging/install_local.py"], cwd=self.root,
            env={**os.environ, "HOME": str(self.home), "XDG_DATA_HOME": str(self.data),
                 "PATH": str(tools) + os.pathsep + os.environ.get("PATH", "")},
            capture_output=True, text=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn("Board installation failed", result.stderr)
        self.assertFalse(self.home.exists())


if __name__ == "__main__":
    unittest.main()
