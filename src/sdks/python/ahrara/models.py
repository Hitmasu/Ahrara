"""Public email shapes and explicitly exported identity credentials."""

from dataclasses import dataclass
from typing import Literal, NotRequired, TypedDict


class EmailFilter(TypedDict, total=False):
    """ANDed fields; strings match case-insensitive substrings unless documented.

    recipient, envelope_sender and message_id match the whole value, ignoring
    case. Header names are case-insensitive; each requested value is a substring.
    Set regex=True to interpret string values as case-insensitive Rust regex:
    recipient, envelope_sender and message_id patterns are anchored to the whole
    value; other patterns match anywhere. Header names remain literal; inline
    flags such as (?-i) override case sensitivity.
    """

    regex: bool
    sender: str
    subject: str
    to: str
    cc: str
    recipient: str
    envelope_sender: str
    message_id: str
    text: str
    has_attachments: bool
    headers: dict[str, str]


class RegexOptions(TypedDict, total=False):
    """Shared Rust regex flags and per-wait email filtering; no local matching."""

    source: Literal["text", "html", "subject", "raw"]
    # Defaults to False: query regex is case-sensitive, unlike filter regex.
    ignore_case: bool
    multi_line: bool
    dot_matches_new_line: bool
    filter: EmailFilter | None


class RegexGroup(TypedDict):
    """Capture zero is the entire match; unmatched optional values are None."""

    index: int
    name: NotRequired[str | None]
    value: NotRequired[str | None]


class RegexMatch(TypedDict):
    """One substring with captures and its inbox-owned email identifier."""

    email_id: int
    value: str
    groups: list[RegexGroup]


@dataclass(frozen=True, repr=False)
class InboxCredentials:
    """Identity restoration material. Never log or disclose ``recovery_key``."""

    address: str
    recovery_key: str

    def __repr__(self) -> str:
        return "InboxCredentials(<redacted>)"


EmailMetadata = TypedDict(
    "EmailMetadata",
    {
        "id": int,
        "transfer_id": str,
        "identity_fingerprint": str,
        "recipient": str,
        "envelope_sender": str,
        "received_at": str,
        "from": str,
        "to": str,
        "subject": str,
        "message_id": str | None,
        "size_bytes": int,
    },
)


class Attachment(TypedDict):
    """MIME attachment metadata; content is fetched separately."""

    attachment_id: str
    filename: str
    content_type: str
    size: int
    content_disposition: str


class Email(EmailMetadata):
    """Parsed email. Text is capped at 256 KiB by the shared core."""

    cc: str
    date: str
    forwarded_to: str | None
    text_body: str
    text_body_truncated: bool
    html_available: bool
    attachments: list[Attachment]
    parse_error: bool


class EmailPage(TypedDict):
    """Metadata page and exclusive ID cursor for the next page, if present."""

    emails: list[EmailMetadata]
    next_cursor: int | None


class AttachmentData(TypedDict):
    """Attachment metadata and decoded content, without filesystem writes."""

    attachment: Attachment
    content: bytes


class HtmlEmail(TypedDict):
    """Sanitized HTML and the number of remote resources detected."""

    html: str
    remote_resources: int
