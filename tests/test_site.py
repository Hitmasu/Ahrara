import base64
import hashlib
import re
import unittest
from pathlib import Path

SITE = Path(__file__).resolve().parents[1] / "src/site"


class SiteHeadersTest(unittest.TestCase):
    def test_csp_allows_every_inline_script(self):
        csp = re.search(r"Content-Security-Policy: (.+)", (SITE / "_headers").read_text()).group(1)
        for page in SITE.rglob("*.html"):
            # Data blocks such as <script type="application/json"> never run, so CSP ignores them.
            for script in re.findall(r"<script>(.*?)</script>", page.read_text(), re.S):
                digest = base64.b64encode(hashlib.sha256(script.encode()).digest()).decode()
                self.assertIn(f"'sha256-{digest}'", csp, f"inline script in {page.relative_to(SITE)}")


if __name__ == "__main__":
    unittest.main()
