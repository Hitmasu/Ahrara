"""Robot Framework keywords: ``Library    ahrara.robot.AhraraLibrary``."""

from collections.abc import Iterator
from pathlib import Path

from . import (
    AttachmentData,
    Email,
    EmailFilter,
    EmailPage,
    HtmlEmail,
    Inbox,
    InboxCredentials,
    InboxOptions,
    RegexMatch,
    create_inbox,
)


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

    def wait_for_email(
        self,
        timeout: float | None = None,
        subject: str | None = None,
        from_address: str | None = None,
        filter: EmailFilter | None = None,
    ) -> Email:
        """Return the next email; omit timeout to wait indefinitely (seconds)."""
        return self._require_inbox().wait_for_email(
            timeout, subject=subject, from_address=from_address, filter=filter
        )

    def wait_for_emails(
        self,
        timeout: float | None = None,
        subject: str | None = None,
        from_address: str | None = None,
        filter: EmailFilter | None = None,
    ) -> Iterator[Email]:
        """Return a sequential iterator; timeout is its total duration in seconds."""
        return self._require_inbox().wait_for_emails(
            timeout, subject=subject, from_address=from_address, filter=filter
        )

    def wait_for_regex_match(
        self,
        pattern: str,
        timeout: float | None = None,
        filter: EmailFilter | None = None,
    ) -> RegexMatch:
        """Return first regex match and captures; omitted timeout waits indefinitely."""
        return self._require_inbox().wait_for_regex_match(
            pattern, timeout, filter=filter
        )

    def wait_for_regex_matches(
        self,
        pattern: str,
        timeout: float | None = None,
        filter: EmailFilter | None = None,
    ) -> Iterator[RegexMatch]:
        """Iterate every regex match across emails until the total timeout expires."""
        return self._require_inbox().wait_for_regex_matches(
            pattern, timeout, filter=filter
        )

    def wait_for_xpath(
        self,
        expression: str,
        timeout: float | None = None,
        filter: EmailFilter | None = None,
    ) -> str:
        """Return the first XPath value from HTML; timeout is optional seconds."""
        return self._require_inbox().wait_for_xpath(expression, timeout, filter=filter)

    def wait_for_xpath_values(
        self,
        expression: str,
        timeout: float | None = None,
        filter: EmailFilter | None = None,
    ) -> Iterator[str]:
        """Iterate XPath values across HTML emails using one total deadline."""
        return self._require_inbox().wait_for_xpath_values(
            expression, timeout, filter=filter
        )

    def export_credentials(self) -> InboxCredentials:
        """Return restoration credentials with a redacted default representation."""
        return self._require_inbox().export_credentials()

    def list_emails(self, after_id: int = 0, limit: int = 20) -> EmailPage:
        """List stored metadata without consuming messages; page size is 1..100."""
        return self._require_inbox().list_emails(after_id=after_id, limit=limit)

    def get_email(self, email_id: int) -> Email:
        """Read a stored email belonging to this inbox."""
        return self._require_inbox().get_email(email_id)

    def delete_email(self, email_id: int) -> int:
        """Delete one inbox-owned email and return the number removed."""
        return self._require_inbox().delete_email(email_id)

    def clear_emails(self) -> int:
        """Clear only this inbox's stored messages; return the number removed."""
        return self._require_inbox().clear_emails()

    def get_attachment(self, email_id: int, attachment_id: str) -> AttachmentData:
        """Return attachment metadata and bytes without writing a file."""
        return self._require_inbox().get_attachment(email_id, attachment_id)

    def save_attachment(
        self,
        email_id: int,
        attachment_id: str,
        destination: str,
    ) -> Path:
        """Save to an absolute file or directory without overwriting files."""
        return self._require_inbox().save_attachment(
            email_id, attachment_id, destination
        )

    def get_raw_email(self, email_id: int) -> bytes:
        """Return original RFC 822 bytes."""
        return self._require_inbox().get_raw_email(email_id)

    def get_html(self, email_id: int, allow_remote: bool = False) -> HtmlEmail:
        """Return sanitized HTML; remote resources are blocked by default."""
        return self._require_inbox().get_html(email_id, allow_remote=allow_remote)

    def close_inbox(self) -> None:
        """Release the inbox after success or failure; persistent profiles remain."""
        if self._inbox is not None:
            self._inbox.close()
            self._inbox = None
