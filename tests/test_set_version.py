"""Set a version on a copy of the repository's manifests and set it back."""
import importlib.util
from pathlib import Path
import shutil
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("set_version", ROOT / "scripts" / "set-version.py")
versions = importlib.util.module_from_spec(spec)
spec.loader.exec_module(versions)


class SetVersionTest(unittest.TestCase):
    def test_every_place_changes_and_the_change_reverts_exactly(self):
        with tempfile.TemporaryDirectory() as work:
            work = Path(work)
            for path in versions.FILES:
                (work / path).parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(ROOT / path, work / path)
            original = {path: (work / path).read_text() for path in versions.FILES}

            old = versions.set_version(work, "98.76.54")
            self.assertEqual(tomllib.loads((work / "src/sdks/python/pyproject.toml").read_text())["project"]["version"], "98.76.54")
            for path in versions.FILES:
                text = (work / path).read_text()
                self.assertNotEqual(text, original[path], path)
                self.assertIn("98.76.54", text, path)

            versions.set_version(work, old)
            for path in versions.FILES:
                self.assertEqual((work / path).read_text(), original[path], path)
            for bad in ("v1.2.3", "1.2", "1.2.3-rc.1", "01.2.3"):
                with self.assertRaises(ValueError):
                    versions.set_version(work, bad)


if __name__ == "__main__":
    unittest.main()
