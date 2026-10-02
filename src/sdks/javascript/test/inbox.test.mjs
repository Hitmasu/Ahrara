import assert from "node:assert/strict";
import test from "node:test";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { inspect } from "node:util";
import { setTimeout as delay } from "node:timers/promises";
import { createInbox } from "../index.js";
import { inboxOptions, sendEmail } from "./support.mjs";

test("buffers mail sent before waiting, keeps full UTF-8 body, and consumes once", async () => {
  const inbox = await createInbox(inboxOptions());
  try {
    const body = `Olá — Rust shared core\n${"long body ".repeat(500)}`;
    await sendEmail(inbox.address, "Buffered message", body);
    const email = await inbox.waitForEmail({ timeoutMs: 10_000 });

    assert.equal(email.subject, "Buffered message");
    assert.ok(
      email.text_body.includes(body.replaceAll("\n", "\r\n")) ||
        email.text_body.includes(body),
    );
    await assert.rejects(inbox.waitForEmail({ timeoutMs: 100 }), {
      code: "timeout",
    });
  } finally {
    await inbox.close();
  }
});

test("parallel inboxes are isolated and filters match", async () => {
  const inboxes = await Promise.all([
    createInbox(inboxOptions()),
    createInbox(inboxOptions()),
  ]);
  try {
    assert.notEqual(inboxes[0].address, inboxes[1].address);
    const waits = inboxes.map((inbox, i) =>
      inbox.waitForEmail({
        subject: `isolated ${i}`,
        from: "sender@example.test",
        timeoutMs: 10_000,
      }),
    );
    await Promise.all(
      inboxes.map((inbox, i) => sendEmail(inbox.address, `Isolated ${i}`)),
    );
    const emails = await Promise.all(waits);

    assert.deepEqual(
      emails.map((email) => email.subject),
      ["Isolated 0", "Isolated 1"],
    );
  } finally {
    await Promise.all(inboxes.map((inbox) => inbox.close()));
  }
});

test("AbortSignal stops a wait and the inbox remains usable", async () => {
  const inbox = await createInbox(inboxOptions());
  try {
    const controller = new AbortController();
    const wait = inbox.waitForEmail({ signal: controller.signal });
    const rejected = assert.rejects(wait, { name: "AbortError" });
    controller.abort();
    await rejected;
    await sendEmail(inbox.address, "After abort");

    assert.equal(
      (await inbox.waitForEmail({ timeoutMs: 10_000 })).subject,
      "After abort",
    );
  } finally {
    await inbox.close();
  }
});

test("closing cancels active waits and is idempotent", async () => {
  const inbox = await createInbox(inboxOptions());
  const rejected = assert.rejects(inbox.waitForEmail(), {
    code: "inbox_closed",
  });
  await inbox.close();
  await rejected;
  await inbox.close();
  await assert.rejects(inbox.waitForEmail(), { code: "inbox_closed" });
});

test("rejects invalid timeouts and simultaneous waits", async () => {
  const inbox = await createInbox(inboxOptions());
  try {
    await assert.rejects(
      inbox.waitForEmail({ timeoutMs: Infinity }),
      RangeError,
    );
    const first = assert.rejects(inbox.waitForEmail({ timeoutMs: 100 }), {
      code: "timeout",
    });
    await assert.rejects(inbox.waitForEmail(), { code: "wait_in_progress" });
    await first;
  } finally {
    await inbox.close();
  }
});

test("omitted timeout remains indefinite beyond the old thirty-second deadline", async (t) => {
  const inbox = await createInbox(inboxOptions());
  const originalNow = performance.now.bind(performance);
  let offset = 0;
  t.mock.method(performance, "now", () => originalNow() + offset);
  try {
    const result = inbox.waitForEmail().then(
      (email) => ({ email }),
      (error) => ({ error }),
    );
    offset = 31_000;
    await delay(150);
    await sendEmail(inbox.address, "Beyond old default");
    const received = await result;

    assert.equal(received.error, undefined);
    assert.equal(received.email.subject, "Beyond old default");
  } finally {
    await inbox.close();
  }
});

test("streams are sequential, count consumer time, and preserve inbox on early exit", async () => {
  const inbox = await createInbox(inboxOptions());
  try {
    await sendEmail(inbox.address, "Stream one");
    await sendEmail(inbox.address, "Stream two");
    const seen = [];

    for await (const email of inbox.waitForEmails({ timeoutMs: 80 })) {
      seen.push(email.subject);
      await delay(100);
    }

    assert.deepEqual(seen, ["Stream one"]);

    for await (const email of inbox.waitForEmails()) {
      assert.equal(email.subject, "Stream two");
      break;
    }

    await sendEmail(inbox.address, "Still open");

    assert.equal(
      (await inbox.waitForEmail({ timeoutMs: 1000 })).subject,
      "Still open",
    );
    const empty = [];

    for await (const email of inbox.waitForEmails({ timeoutMs: 30 }))
      empty.push(email);

    assert.deepEqual(empty, []);
  } finally {
    await inbox.close();
  }
});

test("callback receiver awaits callbacks and propagates their errors without closing", async () => {
  const inbox = await createInbox(inboxOptions());
  try {
    await sendEmail(inbox.address, "Callback one");
    await sendEmail(inbox.address, "Callback two");
    let active = false;
    const seen = [];
    await inbox.receiveEmails(
      async (email) => {
        assert.equal(active, false);
        active = true;
        await delay(10);
        seen.push(email.subject);
        active = false;
      },
      { timeoutMs: 100 },
    );

    assert.deepEqual(seen, ["Callback one", "Callback two"]);
    await sendEmail(inbox.address, "Callback failure");
    const failure = new Error("Consumer failed");
    await assert.rejects(
      inbox.receiveEmails(() => {
        throw failure;
      }),
      (error) => error === failure,
    );
    const controller = new AbortController();
    const pending = inbox.waitForEmails({ signal: controller.signal }).next();
    controller.abort();
    await assert.rejects(pending, { name: "AbortError" });
    await sendEmail(inbox.address, "After stream abort");

    assert.equal(
      (await inbox.waitForEmail({ timeoutMs: 1000 })).subject,
      "After stream abort",
    );
  } finally {
    await inbox.close();
  }
});

test("persistent identity/history, explicit redacted credentials, and scoped CRUD", async () => {
  const directory = await mkdtemp(join(tmpdir(), "ahrara-js-profile-"));
  const options = {
    ...inboxOptions(),
    profilePath: join(directory, "profile"),
  };
  let inbox = await createInbox(options);
  try {
    await sendEmail(inbox.address, "Stored one");
    const first = await inbox.waitForEmail({ timeoutMs: 1000 });
    await sendEmail(inbox.address, "Stored two");
    const credentials = await inbox.exportCredentials();
    const secretRedacted =
      !inspect(credentials).includes(credentials.recoveryKey) &&
      !JSON.stringify(credentials).includes(credentials.recoveryKey);

    assert.ok(secretRedacted);
    const address = inbox.address;
    await inbox.close();
    inbox = await createInbox(options);

    assert.equal(inbox.address, address);
    assert.equal((await inbox.getEmail(first.id)).subject, "Stored one");
    assert.equal(
      (await inbox.waitForEmail({ timeoutMs: 1000 })).subject,
      "Stored two",
    );
    const page = await inbox.listEmails({ limit: 1 });

    assert.equal(page.emails.length, 1);
    assert.notEqual(page.next_cursor, null);
    assert.equal(
      (await inbox.listEmails({ afterId: page.next_cursor })).emails.length,
      1,
    );
    assert.equal(await inbox.deleteEmail(first.id), 1);
    await assert.rejects(inbox.getEmail(first.id));

    assert.equal(await inbox.clearEmails(), 1);
    await inbox.close();
    inbox = await createInbox({ ...inboxOptions(), credentials });

    assert.equal(inbox.address, address);
    assert.equal((await inbox.listEmails()).emails.length, 0);
    await sendEmail(inbox.address, "Restored identity");

    assert.equal(
      (await inbox.waitForEmail({ timeoutMs: 1000 })).subject,
      "Restored identity",
    );
    const unrelated = await createInbox(inboxOptions());
    try {
      const wrong = await unrelated.exportCredentials();
      await assert.rejects(
        createInbox({ ...options, recoveryKey: wrong.recoveryKey }),
        (error) => Boolean(error.code),
      );
    } finally {
      await unrelated.close();
    }
  } finally {
    await inbox.close();
    await rm(directory, { recursive: true, force: true });
  }
});

test("attachment bytes, safe download, raw source, sanitized HTML and cross-inbox isolation", async () => {
  const directory = await mkdtemp(join(tmpdir(), "ahrara-js-attachment-"));
  const inbox = await createInbox(inboxOptions());
  const other = await createInbox(inboxOptions());
  try {
    const content = Buffer.from([0, 1, 2, 255]);
    await sendEmail(inbox.address, "Rich message", "Text fallback", {
      html: '<p>HTML body</p><script>alert(1)</script><img src="https://example.test/remote.png">',
      attachments: [{ filename: "sample.bin", content }],
    });
    const email = await inbox.waitForEmail({ timeoutMs: 1000 });
    const attachment = email.attachments[0];

    assert.deepEqual(
      (await inbox.getAttachment(email.id, attachment.attachment_id)).content,
      content,
    );
    const path = await inbox.saveAttachment(
      email.id,
      attachment.attachment_id,
      directory,
    );

    assert.deepEqual(await readFile(path), content);
    await assert.rejects(
      inbox.saveAttachment(email.id, attachment.attachment_id, path),
    );

    assert.ok(
      (await inbox.getRawEmail(email.id)).includes(Buffer.from("Rich message")),
    );
    const html = await inbox.getHtml(email.id);

    assert.ok(html.html.includes("HTML body"));
    assert.ok(!html.html.includes("<script"));
    assert.ok(!html.html.includes(' src="https://example.test/remote.png"'));
    assert.ok(html.remote_resources > 0);
    await assert.rejects(other.getEmail(email.id));
    await assert.rejects(
      other.getAttachment(email.id, attachment.attachment_id),
    );
  } finally {
    await inbox.close();
    await other.close();
    await rm(directory, { recursive: true, force: true });
  }
});

test("receipt filters drop rejected mail while method filters preserve it", async () => {
  const inbox = await createInbox({
    ...inboxOptions(),
    filter: {
      sender: "SENDER@EXAMPLE",
      subject: "Accepted",
      headers: { "X-Test-Flow": "SIGNUP" },
    },
  });
  try {
    await sendEmail(inbox.address, "Rejected", "Code 000", {
      headers: { "X-Test-Flow": "Signup" },
    });
    await sendEmail(inbox.address, "Accepted later", "Code 111", {
      headers: { "X-Test-Flow": "Signup" },
    });
    await sendEmail(inbox.address, "Accepted first", "Code 222", {
      headers: { "X-Test-Flow": "Signup" },
    });
    const first = await inbox.waitForEmail({
      timeoutMs: 1000,
      filter: {
        subject: "FIRST",
        text: "222",
        to: inbox.address,
        recipient: inbox.address.toUpperCase(),
        envelopeSender: "SENDER@EXAMPLE.TEST",
        hasAttachments: false,
      },
    });

    assert.equal(first.subject, "Accepted first");
    assert.equal(
      (await inbox.waitForEmail({ timeoutMs: 1000 })).subject,
      "Accepted later",
    );
    assert.equal((await inbox.listEmails()).emails.length, 2);
  } finally {
    await inbox.close();
  }
});

test("regex captures return every match across emails and keep nonmatches available", async () => {
  const inbox = await createInbox(inboxOptions());
  try {
    await sendEmail(inbox.address, "No match", "Keep this email");
    await sendEmail(inbox.address, "Codes", "code=123 code=456X");
    await sendEmail(inbox.address, "Codes", "CODE=789");
    const matches = [];

    for await (const match of inbox.waitForRegexMatches(
      "code=(?<code>[0-9]+)(X)?",
      { ignoreCase: true, timeoutMs: 300, filter: { subject: "Codes" } },
    ))
      matches.push(match);

    assert.deepEqual(
      matches.map((match) => match.value),
      ["code=123", "code=456X", "CODE=789"],
    );
    assert.equal(matches[0].groups[0].value, "code=123");
    assert.equal(
      matches[0].groups.find((group) => group.name === "code").value,
      "123",
    );
    assert.equal(matches[0].groups[2].value ?? null, null);
    assert.equal(matches[1].groups[2].value, "X");
    assert.equal((await inbox.getEmail(matches[0].email_id)).subject, "Codes");
    assert.equal(
      (await inbox.waitForEmail({ timeoutMs: 1000 })).subject,
      "No match",
    );
    await sendEmail(inbox.address, "Invalid query survives", "code=987");
    await assert.rejects(inbox.waitForRegexMatch("[", { timeoutMs: 1000 }));

    assert.equal(
      (await inbox.waitForRegexMatch("(?<code>[0-9]+)")).value,
      "987",
    );
  } finally {
    await inbox.close();
  }
});

test("XPath returns attributes, axes and scalar values from repaired HTML across emails", async () => {
  const inbox = await createInbox(inboxOptions());
  try {
    await sendEmail(inbox.address, "Links one", "fallback", {
      html: '<div><a href="/one">One<br></a><a href="/two">Two</a>',
    });
    await sendEmail(inbox.address, "Links two", "fallback", {
      html: '<section><a href="/three">Three</a></section>',
    });
    const values = [];

    for await (const value of inbox.waitForXPathValues("//a/@href", {
      timeoutMs: 300,
    }))
      values.push(value);

    assert.deepEqual(values, ["/one", "/two", "/three"]);
    await sendEmail(inbox.address, "Functions", "fallback", {
      html: "<ul><li>First</li><li>Second</li></ul>",
    });
    await assert.rejects(inbox.waitForXPath("//*[", { timeoutMs: 1000 }));

    assert.equal(
      await inbox.waitForXPath("string(//li[2]/preceding-sibling::li[1])", {
        timeoutMs: 1000,
      }),
      "First",
    );
  } finally {
    await inbox.close();
  }
});

test("search streams count consumer time and cancellation preserves the inbox", async () => {
  const inbox = await createInbox(inboxOptions());
  try {
    await sendEmail(inbox.address, "Many", "code=1 code=2");
    const matches = [];

    for await (const match of inbox.waitForRegexMatches("code=[0-9]", {
      timeoutMs: 80,
    })) {
      matches.push(match.value);
      await delay(100);
    }

    assert.deepEqual(matches, ["code=1"]);
    const controller = new AbortController();
    const pending = inbox.waitForXPath("//a/@href", {
      signal: controller.signal,
    });
    const rejected = assert.rejects(pending, { name: "AbortError" });
    controller.abort();
    await rejected;
    await sendEmail(inbox.address, "After search cancellation", "fallback", {
      html: '<a href="/alive">Alive</a>',
    });
    const stream = inbox.waitForXPathValues("//a/@href");

    assert.equal((await stream.next()).value, "/alive");
    await stream.return();
    await sendEmail(inbox.address, "Still open");

    assert.equal(
      (await inbox.waitForEmail({ timeoutMs: 1000 })).subject,
      "Still open",
    );
  } finally {
    await inbox.close();
  }
});
