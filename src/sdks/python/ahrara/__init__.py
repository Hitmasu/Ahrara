"""Typed, in-process Ahrara inboxes backed by the shared Rust core."""

import asyncio
import base64
from collections.abc import AsyncGenerator, Awaitable, Callable, Generator
import inspect
import math
import os
from pathlib import Path
import threading
import time
from types import TracebackType
from typing import Any, Self, TypeVar, TypedDict, Unpack, cast
import weakref

from ._native import AhraraError as AhraraError, call as _call
from .models import (
    Attachment as Attachment,
    AttachmentData as AttachmentData,
    Email as Email,
    EmailFilter as EmailFilter,
    EmailMetadata as EmailMetadata,
    EmailPage as EmailPage,
    HtmlEmail as HtmlEmail,
    InboxCredentials as InboxCredentials,
    RegexMatch as RegexMatch,
    RegexGroup as RegexGroup,
    RegexOptions as RegexOptions,
)

StrPath = str | os.PathLike[str]
T = TypeVar("T")


class InboxOptions(TypedDict, total=False):
    """Optional relay trust, profile, and identity-restoration settings."""

    server_url: str | None
    server_public_key: str | None
    tls_ca_path: StrPath | None
    connect_timeout_ms: int | None
    profile_path: StrPath | None
    credentials: InboxCredentials | None
    recovery_key: str | None
    address: str | None
    filter: EmailFilter | None


def _deadline(timeout: float | None) -> float:
    if timeout is None:
        return math.inf
    if not math.isfinite(timeout) or timeout <= 0:
        raise ValueError("timeout must be positive and finite, or None for no timeout.")

    return time.monotonic() + timeout


def _check_cancel(cancel: threading.Event | None) -> None:
    if cancel is not None and cancel.is_set():
        raise AhraraError("cancelled", "The wait was cancelled.")


def _regex_query(
    pattern: str, all_matches: bool, options: RegexOptions
) -> dict[str, Any]:
    return {
        "kind": "regex",
        "pattern": pattern,
        "all": all_matches,
        **{key: value for key, value in options.items() if key != "filter"},
    }


class Inbox:
    """A ready inbox; use a context manager or call close() to release it.

    Email IDs and operations are scoped to this inbox's address. Stored email
    remains until deleted or a temporary inbox is closed; consuming a receive
    cursor does not delete the stored message.
    """

    def __init__(self, handle: int, address: str) -> None:
        self._handle = handle
        self.address = address
        self._closed = False
        self._closing = threading.Event()
        self._close_lock = threading.Lock()
        self._wait_lock = threading.Lock()
        # Releases an inbox collected without close(); explicit close stays primary.
        self._finalizer = weakref.finalize(
            self, _call, {"op": "close", "handle": handle}
        )
        self._finalizer.atexit = False

    def _request(self, op: str, **values: Any) -> dict[str, Any]:
        if self._closing.is_set():
            raise AhraraError("inbox_closed", "The inbox is closed.")

        return _call({"op": op, "handle": self._handle, **values})

    def wait_for_email(
        self,
        timeout: float | None = None,
        *,
        subject: str | None = None,
        from_address: str | None = None,
        filter: EmailFilter | None = None,
        cancel: threading.Event | None = None,
    ) -> Email:
        """Consume one matching email; omitted timeout waits indefinitely.

        Args:
            timeout: Positive seconds, or None for no deadline.
            subject: ASCII case-insensitive subject substring.
            from_address: Sender substring using the same matching rules.
            filter: Additional ANDed criteria; rejected mail stays unconsumed.
            cancel: Optional event; setting it cancels only this wait.

        Returns:
            The next unconsumed matching message, including buffered mail.

        Raises:
            TimeoutError: An explicit finite deadline expired.
            AhraraError: Cancelled, closed, or native failure.
            ValueError: The timeout is invalid.
        """
        result = self._wait(
            "wait",
            timeout,
            cancel,
            subject=subject,
            filter=filter,
            **{"from": from_address},
        )

        return cast(Email, result["email"])

    def _wait(
        self,
        op: str,
        timeout: float | None,
        cancel: threading.Event | None,
        **values: Any,
    ) -> dict[str, Any]:
        deadline = _deadline(timeout)
        # Concurrent waits queue; a queued wait still observes cancel, close and deadline.
        while not self._wait_lock.acquire(
            timeout=min(0.1, max(0.0, deadline - time.monotonic()))
        ):
            _check_cancel(cancel)
            if self._closing.is_set():
                raise AhraraError("inbox_closed", "The inbox is closed.")
            if time.monotonic() >= deadline:
                raise TimeoutError("No matching email arrived before the deadline.")
        try:
            while True:
                _check_cancel(cancel)
                remaining = deadline - time.monotonic()
                duration_ms = max(1, math.ceil(min(0.1, remaining) * 1000))
                result = self._request(
                    op,
                    timeout_ms=duration_ms,
                    **values,
                )
                if result["status"] == "matched":
                    return result
                if time.monotonic() >= deadline:
                    raise TimeoutError("No matching email arrived before the deadline.")
        finally:
            self._wait_lock.release()

    def wait_for_emails(
        self,
        timeout: float | None = None,
        *,
        subject: str | None = None,
        from_address: str | None = None,
        filter: EmailFilter | None = None,
        cancel: threading.Event | None = None,
    ) -> Generator[Email, None, None]:
        """Yield sequentially with no prefetch; finite timeout is total seconds.

        Consumer work counts toward the deadline. Expiry ends iteration normally;
        cancellation and other errors propagate. Closing/breaking the generator
        leaves the inbox open. Omit timeout for an indefinite subscription.
        """
        deadline = _deadline(timeout)

        while True:
            _check_cancel(cancel)
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return
            try:
                email = self.wait_for_email(
                    None if math.isinf(remaining) else remaining,
                    subject=subject,
                    from_address=from_address,
                    filter=filter,
                    cancel=cancel,
                )
            except TimeoutError:
                return

            yield email

    def receive_emails(
        self,
        handler: Callable[[Email], None],
        timeout: float | None = None,
        *,
        subject: str | None = None,
        from_address: str | None = None,
        filter: EmailFilter | None = None,
        cancel: threading.Event | None = None,
    ) -> None:
        """Call handler sequentially; propagate its errors and keep the inbox open.

        Uses wait_for_emails' total deadline and cancellation semantics.
        """
        for email in self.wait_for_emails(
            timeout,
            subject=subject,
            from_address=from_address,
            filter=filter,
            cancel=cancel,
        ):
            handler(email)

    def export_credentials(self) -> InboxCredentials:
        """Return sensitive identity credentials with a redacted representation.

        Access recovery_key only to store securely or restore the identity.
        Raises AhraraError if the inbox is closed or the native export fails.
        """
        result = self._request("export_credentials")
        return InboxCredentials(result["address"], result["recovery_key"])

    def wait_for_regex_match(
        self,
        pattern: str,
        timeout: float | None = None,
        *,
        cancel: threading.Event | None = None,
        **options: Unpack[RegexOptions],
    ) -> RegexMatch:
        """Consume one matching email and return its first regex match and groups.

        pattern uses shared Rust regex syntax. options select full decoded text
        (default), HTML, subject or raw mail, flags, and an optional EmailFilter.
        timeout is seconds, omitted for indefinite. Group zero is the whole match;
        optional unmatched groups contain None. Invalid queries raise AhraraError
        before consuming mail; expiry raises TimeoutError. cancel preserves inbox.
        """
        result = self._wait(
            "search",
            timeout,
            cancel,
            query=_regex_query(pattern, False, options),
            filter=options.get("filter"),
        )

        return cast(RegexMatch, result["matches"][0])

    def wait_for_regex_matches(
        self,
        pattern: str,
        timeout: float | None = None,
        *,
        cancel: threading.Event | None = None,
        **options: Unpack[RegexOptions],
    ) -> Generator[RegexMatch, None, None]:
        """Yield every regex match across emails, without prefetching another email.

        The total deadline includes consumer work. Finite expiry ends normally;
        invalid queries, cancellation and native errors propagate. Closing this
        generator preserves the inbox. Each matched email is consumed once.
        """
        for value in self._search_values(
            _regex_query(pattern, True, options),
            "matches",
            timeout,
            options.get("filter"),
            cancel,
        ):
            yield cast(RegexMatch, value)

    def wait_for_xpath(
        self,
        expression: str,
        timeout: float | None = None,
        *,
        filter: EmailFilter | None = None,
        cancel: threading.Event | None = None,
    ) -> str:
        """Return the first XPath string value from one matching email's HTML.

        The shared core evaluates expression. timeout is seconds; None waits
        indefinitely. Invalid XPath raises AhraraError without consuming mail;
        finite expiry raises TimeoutError. filter and cancel affect only this wait.
        """
        result = self._wait(
            "search",
            timeout,
            cancel,
            query={"kind": "xpath", "expression": expression, "all": False},
            filter=filter,
        )

        return str(result["values"][0])

    def wait_for_xpath_values(
        self,
        expression: str,
        timeout: float | None = None,
        *,
        filter: EmailFilter | None = None,
        cancel: threading.Event | None = None,
    ) -> Generator[str, None, None]:
        """Yield all XPath values across emails; finite total expiry ends normally.

        Consumer work counts toward timeout. Omit it for indefinite reception;
        cancellation and errors propagate. Early exit keeps the inbox open.
        """
        for value in self._search_values(
            {"kind": "xpath", "expression": expression, "all": True},
            "values",
            timeout,
            filter,
            cancel,
        ):
            yield str(value)

    def _search_values(
        self,
        query: dict[str, Any],
        field: str,
        timeout: float | None,
        filter: EmailFilter | None,
        cancel: threading.Event | None,
    ) -> Generator[Any, None, None]:
        deadline = _deadline(timeout)
        while True:
            _check_cancel(cancel)
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return
            try:
                result = self._wait(
                    "search",
                    None if math.isinf(remaining) else remaining,
                    cancel,
                    query=query,
                    filter=filter,
                )
            except TimeoutError:
                return

            for value in result[field]:
                _check_cancel(cancel)
                if time.monotonic() >= deadline:
                    return

                yield value

    def list_emails(
        self,
        *,
        after_id: int = 0,
        limit: int = 20,
        subject: str | None = None,
        from_address: str | None = None,
    ) -> EmailPage:
        """List metadata without advancing the receive cursor.

        Args:
            after_id: Exclusive message-ID cursor, zero for the first page.
            limit: Page size from 1 to 100, default 20.
            subject: ASCII case-insensitive subject substring.
            from_address: Sender substring using the same matching rules.

        Returns:
            Metadata and an exclusive next_cursor, or None when exhausted.
            A full final page may be followed by an empty page.

        Raises:
            AhraraError: Invalid pagination, closed inbox, or storage failure.
        """
        return cast(
            EmailPage,
            self._request(
                "list",
                after_id=after_id,
                limit=limit,
                subject=subject,
                **{"from": from_address},
            ),
        )

    def get_email(self, email_id: int) -> Email:
        """Return full parsed mail and attachment metadata for an inbox-owned ID.

        Raises AhraraError for an unavailable/out-of-scope ID, closed inbox, or
        native storage failure. Reading never advances the receive cursor.
        """
        return cast(Email, self._request("get", id=email_id))

    def delete_email(self, email_id: int) -> int:
        """Delete an inbox-owned ID; return one, or zero if no owned ID matches.

        Raises AhraraError if the inbox is closed or native storage fails.
        """
        return int(self._request("delete", id=email_id)["deleted"])

    def clear_emails(self) -> int:
        """Delete this inbox's stored messages and return the number removed.

        Raises AhraraError if the inbox is closed or native storage fails.
        """
        return int(self._request("clear")["deleted"])

    def get_attachment(self, email_id: int, attachment_id: str) -> AttachmentData:
        """Return MIME metadata and decoded bytes without creating a file.

        email_id must belong to this inbox; attachment_id comes from the email's
        attachments list. Raises AhraraError for unavailable content, a closed
        inbox, or a native parsing/storage failure.
        """
        result = self._request("attachment", id=email_id, attachment_id=attachment_id)
        return {
            "attachment": cast(Attachment, result["attachment"]),
            "content": base64.b64decode(result["content_base64"], validate=True),
        }

    def save_attachment(
        self,
        email_id: int,
        attachment_id: str,
        destination: StrPath,
    ) -> Path:
        """Save attachment bytes and return the resulting absolute Path.

        Args:
            email_id: Inbox-owned email ID.
            attachment_id: Identifier from the email's attachments list.
            destination: Absolute new file path or existing directory. For a
                directory, the core sanitizes the attachment filename.

        Raises:
            AhraraError: Unavailable content, closed inbox, invalid path, an
                existing file (never overwritten), or filesystem failure.
        """
        result = self._request(
            "save_attachment",
            id=email_id,
            attachment_id=attachment_id,
            destination=os.fspath(destination),
        )

        return Path(result["path"])

    def get_raw_email(self, email_id: int) -> bytes:
        """Return original RFC 822 bytes for an inbox-owned email ID.

        Raises AhraraError for unavailable content, a closed inbox, or storage
        failure. The SDK does not create a local file.
        """
        return base64.b64decode(
            self._request("raw", id=email_id)["content_base64"], validate=True
        )

    def get_html(self, email_id: int, *, allow_remote: bool = False) -> HtmlEmail:
        """Return sanitized HTML and detected remote-resource count.

        email_id must belong to this inbox. allow_remote explicitly opts into
        external resource URLs; default False blocks them. Raises AhraraError
        for unavailable HTML, a closed inbox, or native parsing/storage failure.
        """
        return cast(
            HtmlEmail, self._request("html", id=email_id, allow_remote=allow_remote)
        )

    def close(self) -> None:
        """Disconnect once; pending waits fail and persistent profile files remain."""
        self._closing.set()
        with self._close_lock:
            if not self._closed:
                _call({"op": "close", "handle": self._handle})
                self._closed = True
                self._finalizer.detach()

    def __enter__(self) -> Self:
        return self

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        traceback: TracebackType | None,
    ) -> None:
        self.close()


async def _finish_native(pending: asyncio.Task[T]) -> T:
    """Retain native-operation ownership through repeated caller cancellation."""
    while not pending.done():
        try:
            await asyncio.shield(pending)
        except asyncio.CancelledError:
            pass

    return pending.result()


class AsyncInbox:
    """Async counterpart of Inbox; native calls run off the event loop."""

    def __init__(self, inbox: Inbox) -> None:
        self._inbox = inbox
        self.address = inbox.address
        self._wait_lock = asyncio.Lock()

    async def wait_for_email(
        self,
        timeout: float | None = None,
        *,
        subject: str | None = None,
        from_address: str | None = None,
        filter: EmailFilter | None = None,
    ) -> Email:
        """Wait indefinitely by default; task cancellation preserves the inbox.

        Explicit timeout is in seconds and raises TimeoutError. Native failures
        raise AhraraError. A concurrently matched message may win the cancellation
        race; cancelled waits do not start another native receive.
        """
        result = await self._wait(
            "wait",
            timeout,
            subject=subject,
            filter=filter,
            **{"from": from_address},
        )

        return cast(Email, result["email"])

    async def _wait(
        self, op: str, timeout: float | None, **values: Any
    ) -> dict[str, Any]:
        deadline = _deadline(timeout)
        try:
            # Concurrent waits queue in call order without holding executor threads.
            async with asyncio.timeout(
                None if math.isinf(deadline) else deadline - time.monotonic()
            ):
                await self._wait_lock.acquire()
        except TimeoutError:
            raise TimeoutError(
                "No matching email arrived before the deadline."
            ) from None
        try:
            while True:
                remaining = deadline - time.monotonic()
                # Each worker call runs one bounded native slice, then frees its thread.
                pending = asyncio.create_task(
                    asyncio.to_thread(
                        self._inbox._request,
                        op,
                        timeout_ms=max(1, math.ceil(min(0.1, remaining) * 1000)),
                        **values,
                    )
                )
                try:
                    result = await asyncio.shield(pending)
                except asyncio.CancelledError:
                    # A message matched by the in-flight slice wins the cancellation race.
                    try:
                        result = await _finish_native(pending)
                    except Exception:
                        result = {}
                    if result.get("status") == "matched":
                        return result

                    raise
                if result["status"] == "matched":
                    return result
                if time.monotonic() >= deadline:
                    raise TimeoutError("No matching email arrived before the deadline.")
        finally:
            self._wait_lock.release()

    async def wait_for_emails(
        self,
        timeout: float | None = None,
        *,
        subject: str | None = None,
        from_address: str | None = None,
        filter: EmailFilter | None = None,
    ) -> AsyncGenerator[Email, None]:
        """Yield sequentially; timeout is total seconds including consumer work.

        Omitted timeout is indefinite. Finite expiry ends normally. Cancellation
        propagates and leaves the inbox open, as do break and generator aclose().
        No message is prefetched while the consumer handles the previous one.
        """
        deadline = _deadline(timeout)

        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return

            task = asyncio.current_task()
            cancellations = task.cancelling() if task is not None else 0
            try:
                email = await self.wait_for_email(
                    None if math.isinf(remaining) else remaining,
                    subject=subject,
                    from_address=from_address,
                    filter=filter,
                )
            except TimeoutError:
                return

            yield email
            # A committed match can win cancellation once, never restart a stream.
            if task is not None and task.cancelling() > cancellations:
                raise asyncio.CancelledError

    async def receive_emails(
        self,
        handler: Callable[[Email], Awaitable[None] | None],
        timeout: float | None = None,
        *,
        subject: str | None = None,
        from_address: str | None = None,
        filter: EmailFilter | None = None,
    ) -> None:
        """Await each callback before receiving again; callback errors propagate.

        Accepts sync or async callbacks. Uses wait_for_emails' total deadline;
        cancellation and normal completion preserve the inbox.
        """
        async for email in self.wait_for_emails(
            timeout, subject=subject, from_address=from_address, filter=filter
        ):
            result = handler(email)
            if inspect.isawaitable(result):
                await result

    async def wait_for_regex_match(
        self,
        pattern: str,
        timeout: float | None = None,
        **options: Unpack[RegexOptions],
    ) -> RegexMatch:
        """Return the first regex match and groups; default timeout is indefinite.

        See Inbox.wait_for_regex_match for source/flag/filter semantics. Task
        cancellation keeps the inbox open; an already matched result may win.
        Invalid queries/native failures raise AhraraError; expiry raises TimeoutError.
        """
        result = await self._wait(
            "search",
            timeout,
            query=_regex_query(pattern, False, options),
            filter=options.get("filter"),
        )

        return cast(RegexMatch, result["matches"][0])

    async def wait_for_regex_matches(
        self,
        pattern: str,
        timeout: float | None = None,
        **options: Unpack[RegexOptions],
    ) -> AsyncGenerator[RegexMatch, None]:
        """Yield every regex match across emails; finite total expiry ends normally.

        Consumer work counts toward timeout; no background prefetch. Cancellation,
        invalid queries and native errors propagate while preserving the inbox.
        """
        async for value in self._search_values(
            _regex_query(pattern, True, options),
            "matches",
            timeout,
            options.get("filter"),
        ):
            yield cast(RegexMatch, value)

    async def wait_for_xpath(
        self,
        expression: str,
        timeout: float | None = None,
        *,
        filter: EmailFilter | None = None,
    ) -> str:
        """Return the first XPath value from HTML; None timeout waits indefinitely.

        Native errors/invalid expressions raise AhraraError; finite expiry raises
        TimeoutError. Task cancellation preserves the inbox and drains native work.
        """
        result = await self._wait(
            "search",
            timeout,
            query={"kind": "xpath", "expression": expression, "all": False},
            filter=filter,
        )

        return str(result["values"][0])

    async def wait_for_xpath_values(
        self,
        expression: str,
        timeout: float | None = None,
        *,
        filter: EmailFilter | None = None,
    ) -> AsyncGenerator[str, None]:
        """Yield XPath values across emails with the same total deadline as regex streams.

        Finite expiry ends normally; cancellation/errors propagate. Early exit
        preserves the inbox. No further email is received during consumer work.
        """
        async for value in self._search_values(
            {"kind": "xpath", "expression": expression, "all": True},
            "values",
            timeout,
            filter,
        ):
            yield str(value)

    async def _search_values(
        self,
        query: dict[str, Any],
        field: str,
        timeout: float | None,
        filter: EmailFilter | None,
    ) -> AsyncGenerator[Any, None]:
        deadline = _deadline(timeout)
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return

            task = asyncio.current_task()
            cancellations = task.cancelling() if task is not None else 0
            try:
                result = await self._wait(
                    "search",
                    None if math.isinf(remaining) else remaining,
                    query=query,
                    filter=filter,
                )
            except TimeoutError:
                return

            for value in result[field]:
                # A buffered value must still give pending task cancellation a turn.
                await asyncio.sleep(0)
                if time.monotonic() >= deadline:
                    return

                yield value
                if task is not None and task.cancelling() > cancellations:
                    raise asyncio.CancelledError

    async def export_credentials(self) -> InboxCredentials:
        """Explicitly export sensitive identity restoration material."""
        return await asyncio.to_thread(self._inbox.export_credentials)

    async def list_emails(
        self,
        *,
        after_id: int = 0,
        limit: int = 20,
        subject: str | None = None,
        from_address: str | None = None,
    ) -> EmailPage:
        """List inbox metadata without consuming; limit is 1..100."""
        return await asyncio.to_thread(
            self._inbox.list_emails,
            after_id=after_id,
            limit=limit,
            subject=subject,
            from_address=from_address,
        )

    async def get_email(self, email_id: int) -> Email:
        """Read one inbox-owned parsed email."""
        return await asyncio.to_thread(self._inbox.get_email, email_id)

    async def delete_email(self, email_id: int) -> int:
        """Delete one inbox-owned message; return the number removed."""
        return await asyncio.to_thread(self._inbox.delete_email, email_id)

    async def clear_emails(self) -> int:
        """Clear inbox-scoped stored messages; return the number removed."""
        return await asyncio.to_thread(self._inbox.clear_emails)

    async def get_attachment(self, email_id: int, attachment_id: str) -> AttachmentData:
        """Return attachment metadata and decoded content without a file."""
        return await asyncio.to_thread(
            self._inbox.get_attachment, email_id, attachment_id
        )

    async def save_attachment(
        self,
        email_id: int,
        attachment_id: str,
        destination: StrPath,
    ) -> Path:
        """Save to an absolute file/directory path without overwriting."""
        return await asyncio.to_thread(
            self._inbox.save_attachment, email_id, attachment_id, destination
        )

    async def get_raw_email(self, email_id: int) -> bytes:
        """Return original RFC 822 bytes."""
        return await asyncio.to_thread(self._inbox.get_raw_email, email_id)

    async def get_html(self, email_id: int, *, allow_remote: bool = False) -> HtmlEmail:
        """Return sanitized HTML, blocking remote resources by default."""
        return await asyncio.to_thread(
            self._inbox.get_html, email_id, allow_remote=allow_remote
        )

    async def close(self) -> None:
        """Disconnect safely even if this cleanup task is cancelled repeatedly."""
        self._inbox._closing.set()
        pending = asyncio.create_task(asyncio.to_thread(self._inbox.close))
        try:
            await asyncio.shield(pending)
        except asyncio.CancelledError:
            await _finish_native(pending)
            raise

    async def __aenter__(self) -> Self:
        return self

    async def __aexit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        traceback: TracebackType | None,
    ) -> None:
        await self.close()


def create_inbox(
    *,
    server_url: str | None = None,
    server_public_key: str | None = None,
    tls_ca_path: StrPath | None = None,
    connect_timeout_ms: int | None = None,
    profile_path: StrPath | None = None,
    credentials: InboxCredentials | None = None,
    recovery_key: str | None = None,
    address: str | None = None,
    filter: EmailFilter | None = None,
) -> Inbox:
    """Authenticate and subscribe before returning a ready, caller-owned inbox.

    Args:
        server_url: Relay WSS URL; omit for the release core's production endpoint.
        server_public_key: Independently verified public Noise key.
        tls_ca_path: Optional custom trusted CA PEM file, with TLS verification.
        connect_timeout_ms: Connection deadline, 1..300000 ms; core default 20000.
        profile_path: Absolute directory retaining encrypted history and identity.
            Omit for a temporary profile removed after close.
        credentials: Exported credentials; cannot be combined with recovery_key/address.
        recovery_key: Sensitive identity import, optionally paired with address.
        address: An owned address; alone requires an existing profile identity.
        filter: Receipt criteria applied before storage; rejected mail is dropped.

    Returns:
        An inbox ready to receive, including before the first wait.

    Raises:
        AhraraError: Invalid configuration/identity, profile conflict, or connection failure.
    """
    options = {
        key: value
        for key, value in {
            "server_url": server_url,
            "server_public_key": server_public_key,
            "tls_ca_path": os.fspath(tls_ca_path) if tls_ca_path is not None else None,
            "connect_timeout_ms": connect_timeout_ms,
            "profile_path": (
                os.fspath(profile_path) if profile_path is not None else None
            ),
            "credentials": (
                {
                    "address": credentials.address,
                    "recovery_key": credentials.recovery_key,
                }
                if credentials is not None
                else None
            ),
            "recovery_key": recovery_key,
            "address": address,
            "filter": filter,
        }.items()
        if value is not None
    }
    result = _call({"op": "create", "options": options})
    return Inbox(result["handle"], result["address"])


async def create_inbox_async(**options: Unpack[InboxOptions]) -> AsyncInbox:
    """Async create_inbox with identical options and safe cancellation cleanup.

    Cancellation waits for the bounded connection attempt and closes any result;
    repeated cancellation never abandons a successfully created native handle.
    """
    pending = asyncio.create_task(asyncio.to_thread(create_inbox, **options))
    try:
        return AsyncInbox(await asyncio.shield(pending))
    except asyncio.CancelledError:
        try:
            inbox = await _finish_native(pending)
        except Exception:
            pass
        else:
            await _finish_native(asyncio.create_task(asyncio.to_thread(inbox.close)))

        raise
