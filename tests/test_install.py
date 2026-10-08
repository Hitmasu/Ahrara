import base64
import hashlib
import json
import os
import platform
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BINARY = os.environ.get("AHRARA_TEST_BINARY")
ARCH = {"x86_64": "x86_64", "amd64": "x86_64", "aarch64": "aarch64", "arm64": "aarch64"}.get(platform.machine().lower())
TARGET = f"linux-{ARCH}"

# Serves fixtures by URL basename and logs each URL; the installer only passes -o FILE URL.
FAKE_CURL = """#!/bin/sh
out= url=
while [ $# -gt 0 ]; do
  case $1 in -o) out=$2; shift 2 ;; --proto) shift 2 ;; -*) shift ;; *) url=$1; shift ;; esac
done
echo "$url" >> "$FIXTURES/requests"
cp "$FIXTURES/${url##*/}" "$out"
"""


def artifact(sha256, size, name):
    return {"sha256": sha256, "size": size, "url": f"https://example.test/v0.1.0/{name}"}


@unittest.skipUnless(BINARY and ARCH and platform.system() == "Linux", "needs AHRARA_TEST_BINARY on Linux")
class InstallShTest(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.root)
        self.fixtures = self.root / "fixtures"
        self.bin = self.root / "bin"
        self.fixtures.mkdir()
        self.bin.mkdir()
        curl = self.bin / "curl"
        curl.write_text(FAKE_CURL)
        curl.chmod(0o755)
        self.dest = self.root / "home" / ".ahrara" / "bin"
        shutil.copy(BINARY, self.fixtures / f"ahrara-{TARGET}")

    def publish(self, sha256=None):
        data = (self.fixtures / f"ahrara-{TARGET}").read_bytes()
        name = f"ahrara-{TARGET}"
        # Another channel lists the same target with different values first.
        channels = {
            "brew": {"artifacts": {TARGET: artifact("0" * 64, 1, "brew-binary")}, "version": "9.9.9"},
            "direct": {"artifacts": {TARGET: artifact(sha256 or hashlib.sha256(data).hexdigest(), len(data), name)}, "version": "0.1.0"},
            "pip": {"artifacts": {TARGET: artifact("1" * 64, 2, "pip-binary")}, "version": "9.9.9"},
        }
        payload = json.dumps({"channels": channels, "expires_at": 4102444800, "schema_version": 1},
                             separators=(",", ":"), sort_keys=True).encode()
        envelope = {"payload": base64.b64encode(payload).decode(), "signature": base64.b64encode(b"unchecked").decode()}
        (self.fixtures / "stable.json").write_text(json.dumps(envelope))

    def install(self):
        env = {
            "PATH": f"{self.bin}:/usr/bin:/bin",
            "HOME": str(self.root / "home"),
            "FIXTURES": str(self.fixtures),
            "AHRARA_MANIFEST_URL": "https://example.test/stable.json",
        }
        return subprocess.run(["sh", str(ROOT / "src/site/install.sh")], env=env, capture_output=True, text=True, timeout=60)

    def requests(self):
        log = self.fixtures / "requests"
        return log.read_text().split() if log.exists() else []

    def test_installs_the_direct_artifact_and_registers_it(self):
        self.publish()
        result = self.install()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.requests(), ["https://example.test/stable.json", f"https://example.test/v0.1.0/ahrara-{TARGET}"])
        self.assertTrue(os.access(self.dest / "ahrara", os.X_OK))
        self.assertEqual(json.loads((self.dest / "ahrara.install.json").read_text()), {"schema_version": 1, "method": "sh"})
        self.assertIn("ahrara 0.1.0", result.stdout)
        self.assertIn(f'export PATH="{self.dest}:$PATH"', result.stdout)

    def test_rejects_a_checksum_mismatch_without_installing(self):
        self.publish(sha256="f" * 64)
        result = self.install()

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("checksum does not match", result.stderr)
        self.assertEqual(list(self.dest.iterdir()), [])

    def test_leaves_installations_from_another_installer(self):
        self.publish()
        self.dest.mkdir(parents=True)
        (self.dest / "ahrara.install.json").write_text('{"schema_version":1,"method":"brew"}')
        result = self.install()

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("managed by brew", result.stderr)
        self.assertEqual(self.requests(), [])
        self.assertFalse((self.dest / "ahrara").exists())


if __name__ == "__main__":
    unittest.main()
