import hashlib
from pathlib import Path
import plistlib
import shutil
import struct
import subprocess
import tempfile
import unittest
import xml.etree.ElementTree as ET

import release as r


class ReleaseTests(unittest.TestCase):
    def test_versions_are_safe_for_all_package_formats(self):
        self.assertEqual(r.version("v0.10.21"), "0.10.21")
        for tag in ("latest", "v1.2", "v01.0.0", "v1.2.3-rc1", "v256.0.0", "v1.0.65536", "v1.0.0\nurl 'bad'"):
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                r.version(tag)

    def test_cask_bootstraps_and_manifest_identify_final_bytes(self):
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp)
            for name in (r.MAC, r.ARCH, r.DEB, r.WIN):
                (out / name).write_bytes(name.encode())
            r.mac_manifest("v1.2.3", out)
            r.linux_manifest("v1.2.3", out)
            rb = (out / "board.rb").read_text()
            self.assertIn('version "1.2.3"', rb)
            self.assertIn(f'sha256 "{hashlib.sha256(r.MAC.encode()).hexdigest()}"', rb)
            self.assertIn('binary "#{appdir}/Board.app/Contents/MacOS/board"', rb)
            bootstrap = (out / "install-macos.sh").read_text()
            self.assertIn(r.digest(out / "board.rb"), bootstrap)
            self.assertIn('/download/v1.2.3/board.rb', bootstrap)
            linux = (out / "install-linux.sh").read_text()
            for f in (r.DEB, r.ARCH):
                self.assertIn(r.digest(out / f), linux)
            for path in (out / "install-linux.sh", out / "install-macos.sh"):
                subprocess.run(["sh", "-n", path], check=True)
            if shutil.which("ruby"):
                subprocess.run(["ruby", "-c", out / "board.rb"], check=True)
            r.checksums(out)
            sums = (out / "SHA256SUMS").read_text().splitlines()
            self.assertEqual(sums, [f"{r.digest(out / f)}  {f}" for f in sorted(r.ASSETS)])
            (out / "unexpected.p12").touch()
            with self.assertRaises(ValueError):
                r.checksums(out)

    def test_icons_and_launcher_metadata(self):
        png = (r.HERE / "icons/board.png").read_bytes()
        self.assertEqual(png[:8], b"\x89PNG\r\n\x1a\n")
        self.assertEqual(struct.unpack(">II", png[16:24]), (1024, 1024))
        ico = (r.HERE / "icons/board.ico").read_bytes()
        reserved, kind, count = struct.unpack_from("<HHH", ico)
        self.assertEqual((reserved, kind), (0, 1))
        sizes = set()
        for n in range(count):
            w, h, _, _, _, _, length, offset = struct.unpack_from("<BBBBHHII", ico, 6 + 16 * n)
            self.assertEqual(w, h)
            self.assertLessEqual(offset + length, len(ico))
            sizes.add(w or 256)
        self.assertTrue({16, 32, 48, 256} <= sizes)
        plist = plistlib.loads(r.render(r.HERE / "macos/Info.plist.in", VERSION="1.2.3").encode())
        self.assertEqual(plist["CFBundleExecutable"], "board-ui")
        self.assertEqual(plist["CFBundleIdentifier"], "org.localboard.Board")
        self.assertEqual(plist["CFBundleIconFile"], "Board.icns")
        ns = {"w": "http://schemas.microsoft.com/wix/2006/wi"}
        wix = ET.parse(r.HERE / "windows/Board.wxs")
        self.assertEqual(wix.find(".//w:Package", ns).get("InstallScope"), "perUser")
        self.assertEqual(wix.find(".//w:Environment", ns).get("System"), "no")
        self.assertEqual(wix.find(".//w:Shortcut", ns).get("Target"), "[#BoardUI]")
        self.assertEqual(wix.find(".//w:File[@Id='BoardUI']", ns).get("Source"), r"$(var.BinDir)\board-ui.exe")
        self.assertFalse(wix.findall(".//w:CustomAction", ns))


if __name__ == "__main__":
    unittest.main()
