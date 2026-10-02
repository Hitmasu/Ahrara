"""Run from outside the source tree after installing the built wheel."""

import os
from pathlib import Path
import sys

import ahrara

assert "AHRARA_NATIVE_LIBRARY" not in os.environ
assert "site-packages" in Path(ahrara.__file__).parts, "Expected the installed wheel"
sys.path.insert(0, sys.argv[1])
from support import inbox_options, send_email

with ahrara.create_inbox(**inbox_options()) as inbox:
    send_email(
        inbox.address, "Packaged Python SDK", "Bundled native core received this email."
    )
    email = inbox.wait_for_email(10)
    assert email["subject"] == "Packaged Python SDK"
    assert "Bundled native core" in email["text_body"]
    send_email(inbox.address, "Packaged query", "code=123")
    assert (
        inbox.wait_for_regex_match(r"(?<code>[0-9]+)", 1)["groups"][1]["value"] == "123"
    )
print("Installed Python wheel received email with its bundled native core.")
