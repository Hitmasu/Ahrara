import asyncio
from concurrent.futures import ThreadPoolExecutor
import threading
import time

import pytest

from ahrara import AhraraError, create_inbox, create_inbox_async
import ahrara
from support import inbox_options, send_email


@pytest.fixture
def inbox():
    with create_inbox(**inbox_options()) as inbox:
        yield inbox


def test_buffered_full_message_and_consumption(inbox):
    body = "Olá — shared Rust core " * 300
    send_email(inbox.address, "Buffered", body)
    email = inbox.wait_for_email(10)
    assert email["subject"] == "Buffered"
    assert body in email["text_body"]
    with pytest.raises(TimeoutError):
        inbox.wait_for_email(0.1)


def test_parallel_isolation_and_filters(inbox):
    with create_inbox(**inbox_options()) as other:
        assert inbox.address != other.address
        with ThreadPoolExecutor() as executor:
            first = executor.submit(
                inbox.wait_for_email,
                10,
                subject="first",
                from_address="sender@example.test",
            )
            second = executor.submit(other.wait_for_email, 10, subject="second")
            send_email(other.address, "Second")
            send_email(inbox.address, "First")
            assert first.result()["subject"] == "First"
            assert second.result()["subject"] == "Second"


def test_cancel_and_close(inbox):
    cancelled = threading.Event()
    cancelled.set()
    with pytest.raises(AhraraError) as error:
        inbox.wait_for_email(cancel=cancelled)
    assert error.value.code == "cancelled"
    with ThreadPoolExecutor() as executor:
        pending = executor.submit(inbox.wait_for_email, 10)
        inbox.close()
        with pytest.raises(AhraraError) as error:
            pending.result()
        assert error.value.code == "inbox_closed"
    inbox.close()


def test_invalid_timeout(inbox):
    with pytest.raises(ValueError):
        inbox.wait_for_email(float("nan"))


def test_async_receive_and_cancellation():
    async def run():
        async with await create_inbox_async(**inbox_options()) as inbox:
            pending = asyncio.create_task(inbox.wait_for_email(10))
            await asyncio.to_thread(send_email, inbox.address, "Async")
            assert (await pending)["subject"] == "Async"
            pending = asyncio.create_task(inbox.wait_for_email())
            await asyncio.sleep(0.02)
            pending.cancel()
            with pytest.raises(asyncio.CancelledError):
                await pending
            await asyncio.to_thread(
                send_email, inbox.address, "After async cancellation"
            )
            assert (await inbox.wait_for_email(10))[
                "subject"
            ] == "After async cancellation"

    asyncio.run(run())


def test_repeated_create_cancellation_closes_native_result(monkeypatch):
    started, release, closed = threading.Event(), threading.Event(), threading.Event()
    native_inbox = create_inbox(**inbox_options())
    original_close = native_inbox.close

    def tracked_close():
        original_close()
        closed.set()

    def delayed_create(**_):
        started.set()
        assert release.wait(5), "Test did not release native creation"
        return native_inbox

    monkeypatch.setattr(native_inbox, "close", tracked_close)
    monkeypatch.setattr(ahrara, "create_inbox", delayed_create)

    async def run():
        pending = asyncio.create_task(create_inbox_async(**inbox_options()))
        assert await asyncio.to_thread(started.wait, 2)
        pending.cancel()
        await asyncio.sleep(0)
        pending.cancel()
        release.set()
        with pytest.raises(asyncio.CancelledError):
            await pending
        assert await asyncio.to_thread(
            closed.wait, 1
        ), "Cancelled creation leaked its native inbox"

    try:
        asyncio.run(run())
    finally:
        release.set()
        original_close()


def test_async_cancellation_releases_saturated_executor():
    async def run():
        asyncio.get_running_loop().set_default_executor(
            ThreadPoolExecutor(max_workers=1)
        )
        async with await create_inbox_async(**inbox_options()) as inbox:
            pending = asyncio.create_task(inbox.wait_for_email(1.2))
            await asyncio.sleep(0.05)
            started = time.monotonic()
            pending.cancel()
            with pytest.raises(asyncio.CancelledError):
                await pending
            assert (
                time.monotonic() - started < 0.5
            ), "Cleanup was queued behind the entire email wait"

    asyncio.run(run())


def test_cancelled_close_still_releases_queued_native_handle():
    started, release = threading.Event(), threading.Event()

    def block_executor():
        started.set()
        assert release.wait(5), "Test did not release executor"

    async def run():
        asyncio.get_running_loop().set_default_executor(
            ThreadPoolExecutor(max_workers=1)
        )
        inbox = await create_inbox_async(**inbox_options())
        blocker = asyncio.create_task(asyncio.to_thread(block_executor))
        try:
            while not started.is_set():
                await asyncio.sleep(0)

            closing = asyncio.create_task(inbox.close())
            await asyncio.sleep(0)
            closing.cancel()
            await asyncio.sleep(0)
            closing.cancel()
            release.set()
            with pytest.raises(asyncio.CancelledError):
                await closing
            await blocker
            assert (
                inbox._inbox._closed
            ), "Cancelling queued close leaked its native handle"
        finally:
            release.set()
            await inbox.close()

    asyncio.run(run())


def test_async_close_interrupts_indefinite_wait_with_saturated_executor():
    async def run():
        asyncio.get_running_loop().set_default_executor(
            ThreadPoolExecutor(max_workers=1)
        )
        inbox = await create_inbox_async(**inbox_options())
        waiting = asyncio.create_task(inbox.wait_for_email())
        await asyncio.sleep(0.05)
        closing = asyncio.create_task(inbox.close())
        try:
            completed, _ = await asyncio.wait({closing}, timeout=0.5)
            assert closing in completed, "Close queued behind an indefinite wait"
            await closing
            with pytest.raises(AhraraError) as error:
                await waiting
            assert error.value.code == "inbox_closed"
        finally:
            waiting.cancel()
            await asyncio.gather(waiting, return_exceptions=True)
            await closing

    asyncio.run(run())


def test_async_cancellation_preserves_a_message_already_consumed(monkeypatch):
    matched, release = threading.Event(), threading.Event()

    async def run():
        async with await create_inbox_async(**inbox_options()) as inbox:
            original_wait = inbox._inbox.wait_for_email

            def delayed_return(*args, **kwargs):
                email = original_wait(*args, **kwargs)
                matched.set()
                assert release.wait(5), "Test did not release a matched email"
                return email

            monkeypatch.setattr(inbox._inbox, "wait_for_email", delayed_return)
            waiting = asyncio.create_task(inbox.wait_for_email())
            try:
                await asyncio.to_thread(
                    send_email, inbox.address, "Matched before cancel"
                )
                assert await asyncio.to_thread(matched.wait, 2)
                waiting.cancel()
                await asyncio.sleep(0)
                release.set()
                email = await waiting
                assert email["subject"] == "Matched before cancel"
            finally:
                release.set()
                await asyncio.gather(waiting, return_exceptions=True)

    asyncio.run(run())


def test_sync_stream_total_deadline_early_exit_and_callback_error(inbox):
    send_email(inbox.address, "Stream one")
    send_email(inbox.address, "Stream two")
    seen = []

    for email in inbox.wait_for_emails(0.08):
        seen.append(email["subject"])
        time.sleep(0.1)

    assert seen == ["Stream one"]
    stream = inbox.wait_for_emails()
    assert next(stream)["subject"] == "Stream two"
    stream.close()
    assert list(inbox.wait_for_emails(0.03)) == []

    send_email(inbox.address, "Callback failure")

    def fail(_):
        raise RuntimeError("Consumer failed")

    with pytest.raises(RuntimeError, match="Consumer failed"):
        inbox.receive_emails(fail)
    send_email(inbox.address, "Still open")
    assert inbox.wait_for_email(1)["subject"] == "Still open"


def test_async_stream_callbacks_and_cancel_preserve_inbox():
    async def run():
        async with await create_inbox_async(**inbox_options()) as inbox:
            await asyncio.to_thread(send_email, inbox.address, "Async stream one")
            await asyncio.to_thread(send_email, inbox.address, "Async stream two")
            seen = []

            async for email in inbox.wait_for_emails(0.08):
                seen.append(email["subject"])
                await asyncio.sleep(0.1)

            assert seen == ["Async stream one"]
            stream = inbox.wait_for_emails()
            assert (await anext(stream))["subject"] == "Async stream two"
            await stream.aclose()
            assert [email async for email in inbox.wait_for_emails(0.03)] == []

            for subject in ("Callback one", "Callback two"):
                await asyncio.to_thread(send_email, inbox.address, subject)

            seen = []
            active = False

            async def handler(email):
                nonlocal active
                assert not active
                active = True
                await asyncio.sleep(0.01)
                seen.append(email["subject"])
                active = False

            await inbox.receive_emails(handler, 0.1)
            assert seen == ["Callback one", "Callback two"]
            await asyncio.to_thread(send_email, inbox.address, "Callback error")

            async def fail(_):
                raise RuntimeError("Consumer failed")

            with pytest.raises(RuntimeError, match="Consumer failed"):
                await inbox.receive_emails(fail)

            stream = inbox.wait_for_emails()
            pending = asyncio.create_task(anext(stream))
            await asyncio.sleep(0.02)
            pending.cancel()
            with pytest.raises(asyncio.CancelledError):
                await pending
            await stream.aclose()
            await asyncio.to_thread(
                send_email, inbox.address, "After stream cancellation"
            )
            assert (await inbox.wait_for_email(1))[
                "subject"
            ] == "After stream cancellation"

    asyncio.run(run())


def test_persistent_history_credentials_and_crud(tmp_path):
    options = {**inbox_options(), "profile_path": tmp_path / "profile"}
    with create_inbox(**options) as inbox:
        send_email(inbox.address, "Stored one")
        first = inbox.wait_for_email(1)
        send_email(inbox.address, "Stored two")
        credentials = inbox.export_credentials()
        redacted = credentials.recovery_key not in repr(credentials)
        assert redacted
        address = inbox.address
    with create_inbox(**options) as inbox:
        assert inbox.address == address
        assert inbox.get_email(first["id"])["subject"] == "Stored one"
        assert inbox.wait_for_email(1)["subject"] == "Stored two"
        page = inbox.list_emails(limit=1)
        assert len(page["emails"]) == 1
        assert page["next_cursor"] is not None
        assert len(inbox.list_emails(after_id=page["next_cursor"])["emails"]) == 1
        assert inbox.delete_email(first["id"]) == 1
        with pytest.raises(AhraraError):
            inbox.get_email(first["id"])
        assert inbox.clear_emails() == 1
    with create_inbox(**inbox_options(), credentials=credentials) as restored:
        assert restored.address == address
        assert restored.list_emails()["emails"] == []
        send_email(restored.address, "Restored identity")
        assert restored.wait_for_email(1)["subject"] == "Restored identity"
    with create_inbox(**inbox_options()) as unrelated:
        wrong = unrelated.export_credentials()
    with pytest.raises(AhraraError):
        create_inbox(**options, recovery_key=wrong.recovery_key)


def test_attachment_crud_and_html_are_scoped(inbox, tmp_path):
    content = b"\x00\x01\x02\xff"
    send_email(
        inbox.address,
        "Rich message",
        "Text fallback",
        attachment=content,
        html='<p>HTML body</p><script>alert(1)</script><img src="https://example.test/remote.png">',
    )
    email = inbox.wait_for_email(1)
    attachment_id = email["attachments"][0]["attachment_id"]
    assert inbox.get_attachment(email["id"], attachment_id)["content"] == content
    saved = inbox.save_attachment(email["id"], attachment_id, tmp_path)
    assert saved.parent == tmp_path
    assert saved.read_bytes() == content
    with pytest.raises(AhraraError):
        inbox.save_attachment(email["id"], attachment_id, saved)
    assert b"Rich message" in inbox.get_raw_email(email["id"])
    html = inbox.get_html(email["id"])
    assert "HTML body" in html["html"]
    assert "<script" not in html["html"]
    assert ' src="https://example.test/remote.png"' not in html["html"]
    assert html["remote_resources"] > 0
    with create_inbox(**inbox_options()) as other:
        with pytest.raises(AhraraError):
            other.get_email(email["id"])
        with pytest.raises(AhraraError):
            other.get_attachment(email["id"], attachment_id)


def test_receipt_filter_and_method_filter_have_separate_scope():
    with create_inbox(
        **inbox_options(),
        filter={
            "sender": "SENDER@EXAMPLE",
            "subject": "Accepted",
            "headers": {"X-Test-Flow": "SIGNUP"},
        }
    ) as inbox:
        send_email(
            inbox.address, "Rejected", "Code 000", headers={"X-Test-Flow": "Signup"}
        )
        send_email(
            inbox.address,
            "Accepted later",
            "Code 111",
            headers={"X-Test-Flow": "Signup"},
        )
        send_email(
            inbox.address,
            "Accepted first",
            "Code 222",
            headers={"X-Test-Flow": "Signup"},
        )
        first = inbox.wait_for_email(
            1,
            filter={
                "subject": "FIRST",
                "text": "222",
                "to": inbox.address,
                "recipient": inbox.address.upper(),
                "envelope_sender": "SENDER@EXAMPLE.TEST",
                "has_attachments": False,
            },
        )
        assert first["subject"] == "Accepted first"
        assert inbox.wait_for_email(1)["subject"] == "Accepted later"
        assert len(inbox.list_emails()["emails"]) == 2


def test_regex_groups_plural_across_emails_and_invalid_query(inbox):
    send_email(inbox.address, "Nonmatch", "Keep this email")
    send_email(inbox.address, "Codes", "code=123 code=456X")
    send_email(inbox.address, "Codes", "CODE=789")
    matches = list(
        inbox.wait_for_regex_matches(
            r"code=(?<code>[0-9]+)(X)?",
            0.3,
            ignore_case=True,
            filter={"subject": "Codes"},
        )
    )
    assert [match["value"] for match in matches] == [
        "code=123",
        "code=456X",
        "CODE=789",
    ]
    assert matches[0]["groups"][0]["value"] == "code=123"
    assert (
        next(group for group in matches[0]["groups"] if group.get("name") == "code")[
            "value"
        ]
        == "123"
    )
    assert matches[0]["groups"][2].get("value") is None
    assert matches[1]["groups"][2]["value"] == "X"
    assert inbox.get_email(matches[0]["email_id"])["subject"] == "Codes"
    assert inbox.wait_for_email(1)["subject"] == "Nonmatch"
    send_email(inbox.address, "Invalid query survives", "code=987")
    with pytest.raises(AhraraError):
        inbox.wait_for_regex_match("[", 1)
    assert inbox.wait_for_regex_match(r"(?<code>[0-9]+)")["value"] == "987"


def test_xpath_values_across_emails_and_total_deadline(inbox):
    send_email(
        inbox.address,
        "Links one",
        html='<div><a href="/one">One<br></a><a href="/two">Two</a>',
    )
    send_email(inbox.address, "Links two", html='<a href="/three">Three</a>')
    assert list(inbox.wait_for_xpath_values("//a/@href", 0.3)) == [
        "/one",
        "/two",
        "/three",
    ]
    send_email(
        inbox.address, "Functions", html="<ul><li>First</li><li>Second</li></ul>"
    )
    with pytest.raises(AhraraError):
        inbox.wait_for_xpath("//*[", 1)
    assert (
        inbox.wait_for_xpath("string(//li[2]/preceding-sibling::li[1])", 1) == "First"
    )
    send_email(inbox.address, "Many", "code=1 code=2")
    seen = []

    for match in inbox.wait_for_regex_matches("code=[0-9]", 0.08):
        seen.append(match["value"])
        time.sleep(0.1)

    assert seen == ["code=1"]
    cancelled = threading.Event()
    cancelled.set()
    with pytest.raises(AhraraError) as error:
        inbox.wait_for_xpath("//a/@href", cancel=cancelled)
    assert error.value.code == "cancelled"
    send_email(inbox.address, "Early exit", html='<a href="/alive">Alive</a>')
    stream = inbox.wait_for_xpath_values("//a/@href")
    assert next(stream) == "/alive"
    stream.close()
    send_email(inbox.address, "Still open")
    assert inbox.wait_for_email(1)["subject"] == "Still open"


def test_async_query_streams_and_cancellation():
    async def run():
        async with await create_inbox_async(**inbox_options()) as inbox:
            for body in ("code=1 code=2", "code=3"):
                await asyncio.to_thread(send_email, inbox.address, "Async codes", body)

            matches = [
                match
                async for match in inbox.wait_for_regex_matches(
                    "code=(?<code>[0-9])", 0.3
                )
            ]
            assert [match["value"] for match in matches] == [
                "code=1",
                "code=2",
                "code=3",
            ]
            pending = asyncio.create_task(inbox.wait_for_regex_match("pending"))
            await asyncio.sleep(0.02)
            pending.cancel()
            with pytest.raises(asyncio.CancelledError):
                await pending
            await asyncio.to_thread(
                send_email,
                inbox.address,
                "HTML",
                html='<a href="/one">One</a><a href="/two">Two</a>',
            )
            stream = inbox.wait_for_xpath_values("//a/@href")
            assert await anext(stream) == "/one"
            assert await anext(stream) == "/two"
            await stream.aclose()
            await asyncio.to_thread(
                send_email, inbox.address, "Scalar", html="<p>Complete</p>"
            )
            assert await inbox.wait_for_xpath("string(//p)", 1) == "Complete"
            assert [
                value async for value in inbox.wait_for_xpath_values("//a/@href", 0.03)
            ] == []

    asyncio.run(run())


@pytest.mark.parametrize("kind", ["email", "regex", "xpath"])
def test_stream_cancellation_after_match_does_not_start_another_wait(monkeypatch, kind):
    matched, release = threading.Event(), threading.Event()

    async def run():
        async with await create_inbox_async(**inbox_options()) as inbox:
            original_wait = inbox._inbox._wait
            calls = 0
            seen = []

            def delayed_return(*args, **kwargs):
                nonlocal calls
                calls += 1
                result = original_wait(*args, **kwargs)
                matched.set()
                assert release.wait(5), "Test did not release matched result"
                return result

            monkeypatch.setattr(inbox._inbox, "_wait", delayed_return)
            if kind == "email":
                stream = inbox.wait_for_emails()
            elif kind == "regex":
                stream = inbox.wait_for_regex_matches("[0-9]+")
            else:
                stream = inbox.wait_for_xpath_values("//a/@href")

            async def consume():
                async for value in stream:
                    seen.append(value)

            consuming = asyncio.create_task(consume())
            try:
                await asyncio.to_thread(
                    send_email,
                    inbox.address,
                    "Matched before stream cancellation",
                    "code=123",
                    html='<a href="/verify">Verify</a>',
                )
                assert await asyncio.to_thread(matched.wait, 2)
                consuming.cancel()
                await asyncio.sleep(0)
                release.set()
                completed, _ = await asyncio.wait({consuming}, timeout=0.5)
                assert (
                    consuming in completed
                ), "Stream swallowed cancellation and waited indefinitely again"
                with pytest.raises(asyncio.CancelledError):
                    await consuming
                assert len(seen) == 1, "The committed match should be delivered once"
                assert calls == 1, "Cancellation must not start another native wait"
            finally:
                release.set()
                consuming.cancel()
                await asyncio.gather(consuming, return_exceptions=True)
                await stream.aclose()

    asyncio.run(run())
