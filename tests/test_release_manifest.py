"""Exercise the publisher with ephemeral signing keys and six synthetic artifacts."""
import base64
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
from types import SimpleNamespace
import unittest

spec = importlib.util.spec_from_file_location("release_manifest", Path(__file__).parent.parent / "scripts" / "release-manifest.py")
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class ReleaseManifestTest(unittest.TestCase):
    def test_signing_channel_publication_refresh_and_immutable_artifacts(self):
        with tempfile.TemporaryDirectory() as work:
            work = Path(work)
            key = work / "signing.pem"
            subprocess.run(["openssl", "genpkey", "-algorithm", "ED25519", "-out", str(key)], check=True, capture_output=True)
            public = release.openssl("pkey", "-in", key, "-pubout", "-outform", "DER")[-32:].hex()
            assets = work / "assets"
            assets.mkdir()
            for target in release.TARGETS:
                (assets / (f"ahrara-{target}" + (".exe" if target.startswith("windows-") else ""))).write_bytes(target.encode())
            version = release.tomllib.loads((Path(__file__).parent.parent / "src/cli/Cargo.toml").read_text())["package"]["version"]
            args = SimpleNamespace(version=version, key=key, public_key=public, valid_days=30,
                                   previous=None, assets=assets, available=[], output=work / "stable.json",
                                   homebrew_output=work / "Formula" / "ahrara.rb",
                                   plugin_key_output=work / "plugin" / "release-key.json")
            release.generate(args)
            self.assertEqual(json.loads(args.plugin_key_output.read_text()), {"ed25519": public})
            formula = args.homebrew_output.read_text()
            self.assertIn(f'  version "{version}"', formula)
            self.assertNotIn("disable!", formula)
            self.assertNotIn("windows", formula)
            self.assertEqual(formula.count('using: :nounzip'), 4)
            self.assertEqual(formula.count('sha256 "'), 4)
            for target in (target for target in release.TARGETS if not target.startswith("windows-")):
                self.assertIn(f'/v{version}/ahrara-{target}"', formula)
                self.assertIn(hashlib.sha256(target.encode()).hexdigest(), formula)
            # Generated metadata must preserve the installer and its ownership test.
            source = Path(__file__).parent.parent / "Formula" / "ahrara.rb"
            self.assertEqual(formula.split("  # END RELEASE METADATA\n")[1],
                             source.read_text().split("  # END RELEASE METADATA\n")[1])
            envelope = json.loads(args.output.read_text())
            payload = base64.b64decode(envelope["payload"])
            manifest = json.loads(payload)
            self.assertEqual(list(manifest["channels"]), ["direct"])
            self.assertEqual(len(manifest["channels"]["direct"]["artifacts"]), 6)
            # A refresh verifies the previous signature before preserving channels.
            args.previous = args.output
            args.available = ["brew=darwin-aarch64"]
            release.generate(args)
            self.assertEqual(args.homebrew_output.read_text(), formula)
            manifest = json.loads(base64.b64decode(json.loads(args.output.read_text())["payload"]))
            self.assertEqual(list(manifest["channels"]["brew"]["artifacts"]), ["darwin-aarch64"])
            args.available = ["pip=invalid-platform"]
            with self.assertRaises(ValueError):
                release.generate(args)
            args.available = []
            args.public_key = "00" * 32
            with self.assertRaises(ValueError):
                release.generate(args)
            args.public_key = public
            (assets / "ahrara-linux-x86_64").write_bytes(b"changed")
            with self.assertRaises(ValueError):
                release.generate(args)
            (assets / "ahrara-linux-x86_64").write_bytes(b"linux-x86_64")
            envelope["payload"] = base64.b64encode(payload + b" ").decode()
            args.output.write_text(json.dumps(envelope))
            with self.assertRaises(subprocess.CalledProcessError):
                release.generate(args)


if __name__ == "__main__":
    unittest.main()
