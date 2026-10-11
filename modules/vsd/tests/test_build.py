"""Unit tests for modules/vsd/extension/build.py.

The module's HERE/SRC/DIST globals are redirected to a temporary fake layout,
so the real extension/dist directory is never touched.
"""

import contextlib
import importlib.util
import io
import json
import shutil
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

_BUILD_PY = Path(__file__).resolve().parent.parent / "extension" / "build.py"

# Loaded under a unique name: a plain `import build` could collide with the
# PyPA `build` package if it is installed.
_spec = importlib.util.spec_from_file_location("vsd_extension_build", _BUILD_PY)
build = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(build)


class BuildTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="vsd_build_tests_"))
        self.addCleanup(shutil.rmtree, self.root, ignore_errors=True)
        self.src = self.root / "src"
        self.dist = self.root / "dist"

        (self.src / "icons").mkdir(parents=True)
        (self.src / "background.js").write_text("// bg", encoding="utf-8")
        (self.src / "popup.html").write_text("<html></html>", encoding="utf-8")
        (self.src / "icons" / "icon48.png").write_bytes(b"\x89PNG")
        (self.src / "manifest.json").write_text('{"from": "src"}', encoding="utf-8")
        (self.src / "empty_dir").mkdir()

        for browser in ("chrome", "firefox"):
            (self.root / browser).mkdir()
            (self.root / browser / "manifest.json").write_text(
                json.dumps({"browser": browser}), encoding="utf-8"
            )

        for name, value in (("HERE", self.root), ("SRC", self.src), ("DIST", self.dist)):
            p = mock.patch.object(build, name, value)
            p.start()
            self.addCleanup(p.stop)

        self.real_dist = _BUILD_PY.parent / "dist"
        self.real_dist_snapshot = self._snapshot(self.real_dist)

    def tearDown(self):
        self.assertEqual(self._snapshot(self.real_dist), self.real_dist_snapshot,
                         "real extension/dist must not be modified")

    @staticmethod
    def _snapshot(path):
        if not path.exists():
            return None
        return sorted((str(p.relative_to(path)), p.stat().st_mtime_ns)
                      for p in path.rglob("*"))

    def _build(self, browser):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            result = build.build(browser)
        return result, out.getvalue()

    def test_builds_chrome(self):
        ok, output = self._build("chrome")
        self.assertTrue(ok)
        out = self.dist / "chrome"
        self.assertIn(f"Built: {out}", output)
        self.assertEqual((out / "background.js").read_text(encoding="utf-8"), "// bg")
        self.assertEqual((out / "popup.html").read_text(encoding="utf-8"), "<html></html>")
        self.assertEqual((out / "icons" / "icon48.png").read_bytes(), b"\x89PNG")

    def test_browser_manifest_overrides_src_manifest(self):
        self._build("firefox")
        manifest = json.loads((self.dist / "firefox" / "manifest.json").read_text(encoding="utf-8"))
        self.assertEqual(manifest, {"browser": "firefox"})
        # src/ itself is left untouched.
        self.assertEqual((self.src / "manifest.json").read_text(encoding="utf-8"), '{"from": "src"}')

    def test_empty_source_dirs_not_copied(self):
        self._build("chrome")
        self.assertFalse((self.dist / "chrome" / "empty_dir").exists())

    def test_output_file_set_matches_src(self):
        self._build("chrome")
        out = self.dist / "chrome"
        built = sorted(str(p.relative_to(out)) for p in out.rglob("*") if p.is_file())
        expected = sorted(str(p.relative_to(self.src)) for p in self.src.rglob("*") if p.is_file())
        self.assertEqual(built, expected)

    def test_rebuild_removes_stale_files(self):
        stale = self.dist / "chrome" / "old.js"
        stale.parent.mkdir(parents=True)
        stale.write_text("stale")
        ok, _ = self._build("chrome")
        self.assertTrue(ok)
        self.assertFalse(stale.exists())
        self.assertTrue((self.dist / "chrome" / "background.js").exists())

    def test_rebuild_does_not_touch_other_browser(self):
        self._build("firefox")
        marker = self.dist / "firefox" / "marker.txt"
        marker.write_text("keep")
        self._build("chrome")
        self.assertTrue(marker.exists())

    def test_unknown_browser_returns_false(self):
        ok, output = self._build("safari")
        self.assertFalse(ok)
        self.assertIn("not found", output)
        self.assertFalse(self.dist.exists())

    def test_main_builds_both_by_default(self):
        out = io.StringIO()
        with mock.patch.object(sys, "argv", ["build.py"]), contextlib.redirect_stdout(out):
            build.main()
        self.assertTrue((self.dist / "chrome" / "manifest.json").exists())
        self.assertTrue((self.dist / "firefox" / "manifest.json").exists())
        self.assertIn("Done!", out.getvalue())

    def test_main_single_target(self):
        with mock.patch.object(sys, "argv", ["build.py", "firefox"]), \
                contextlib.redirect_stdout(io.StringIO()):
            build.main()
        self.assertTrue((self.dist / "firefox").exists())
        self.assertFalse((self.dist / "chrome").exists())

    def test_main_exits_1_on_unknown_target(self):
        with mock.patch.object(sys, "argv", ["build.py", "chrome", "opera"]), \
                contextlib.redirect_stdout(io.StringIO()):
            with self.assertRaises(SystemExit) as ctx:
                build.main()
        self.assertEqual(ctx.exception.code, 1)
        self.assertTrue((self.dist / "chrome").exists())


if __name__ == "__main__":
    unittest.main()
