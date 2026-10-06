"""Robot Framework keywords: ``Library    ahrara.robot.AhraraLibrary``."""

from collections.abc import Callable
import functools
import inspect
from typing import Any

from . import (
    EmailFilter,
    Inbox,
    InboxCredentials,
    InboxOptions,
    create_inbox,
)

# Keywords that call the same-named Inbox method, with their Robot documentation.
_FORWARDED = {
    "wait_for_email": "Return the next email; omit timeout to wait indefinitely (seconds).",
    "wait_for_emails": "Return a sequential iterator; timeout is its total duration in seconds.",
    "wait_for_regex_match": "Return first regex match and captures; omitted timeout waits indefinitely.",
    "wait_for_regex_matches": "Iterate every regex match across emails until the total timeout expires.",
    "wait_for_xpath": "Return the first XPath value from HTML; timeout is optional seconds.",
    "wait_for_xpath_values": "Iterate XPath values across HTML emails using one total deadline.",
    "export_credentials": "Return restoration credentials with a redacted default representation.",
    "list_emails": "List stored metadata without consuming messages; page size is 1..100.",
    "get_email": "Read a stored email belonging to this inbox.",
    "delete_email": "Delete one inbox-owned email and return the number removed.",
    "clear_emails": "Clear only this inbox's stored messages; return the number removed.",
    "get_attachment": "Return attachment metadata and bytes without writing a file.",
    "save_attachment": "Save to an absolute file or directory without overwriting files.",
    "get_raw_email": "Return original RFC 822 bytes.",
    "get_html": "Return sanitized HTML; remote resources are blocked by default.",
}


class AhraraLibrary:
    """One inbox per Robot test. Register Close Inbox as test teardown."""

    ROBOT_LIBRARY_SCOPE = "TEST"

    def __init__(
        self,
        server_url: str | None = None,
        server_public_key: str | None = None,
        tls_ca_path: str | None = None,
        profile_path: str | None = None,
    ) -> None:
        self._options: InboxOptions = {
            "server_url": server_url,
            "server_public_key": server_public_key,
            "tls_ca_path": tls_ca_path,
            "profile_path": profile_path,
        }
        self._inbox: Inbox | None = None

    def get_keyword_names(self) -> list[str]:
        return ["create_inbox", *_FORWARDED, "close_inbox"]

    def __getattr__(self, name: str) -> Callable[..., Any]:
        if name not in _FORWARDED:
            raise AttributeError(name)

        method = getattr(Inbox, name)

        # Robot reads the Inbox signature and annotations to convert arguments.
        @functools.wraps(method)
        def keyword(*args: Any, **kwargs: Any) -> Any:
            return getattr(self._require_inbox(), name)(*args, **kwargs)

        signature = inspect.signature(method)
        keyword.__doc__ = _FORWARDED[name]
        keyword.__signature__ = signature.replace(  # type: ignore[attr-defined]
            parameters=list(signature.parameters.values())[1:]
        )
        return keyword

    def _require_inbox(self) -> Inbox:
        if self._inbox is None:
            raise RuntimeError("Call Create Inbox before accessing email.")

        return self._inbox

    def create_inbox(
        self,
        credentials: InboxCredentials | None = None,
        filter: EmailFilter | None = None,
    ) -> str:
        """Create or restore the configured inbox and return its address."""
        self.close_inbox()
        options = self._options.copy()
        if credentials is not None:
            options["credentials"] = credentials

        options["filter"] = filter
        self._inbox = create_inbox(**options)
        return self._inbox.address

    def close_inbox(self) -> None:
        """Release the inbox after success or failure; persistent profiles remain."""
        if self._inbox is not None:
            self._inbox.close()
            self._inbox = None
