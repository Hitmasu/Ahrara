import ahrara from "k6/x/ahrara";
import http from "k6/http";
import { check, fail, sleep } from "k6";
import { Counter } from "k6/metrics";

const completedFlows = new Counter("completed_mail_flows");

export const options = {
  scenarios: {
    mail: {
      executor: "per-vu-iterations",
      vus: 2,
      iterations: 1,
      maxDuration: "30s",
    },
  },
  thresholds: { checks: ["rate==1"], completed_mail_flows: ["count==2"] },
};

export default function () {
  completedFlows.add(0);
  const settings = {
    serverUrl: __ENV.AHRARA_TEST_SERVER_URL,
    serverPublicKey: __ENV.AHRARA_TEST_SERVER_PUBLIC_KEY,
    tlsCaPath: __ENV.AHRARA_TEST_TLS_CA,
    connectTimeoutMs: 10000,
    profilePath: `/tmp/ahrara-k6-profile-${__VU}`,
  };
  const inbox = ahrara.createInbox(settings);
  let credentials;

  function send(
    subject,
    body = "Verification code 654321",
    target = inbox,
    html = "",
  ) {
    const response = http.post(
      `${__ENV.AHRARA_TEST_FIXTURE_URL || "http://127.0.0.1:18787"}/send`,
      JSON.stringify({ to: target.address, subject, body, html }),
      { headers: { "Content-Type": "application/json" } },
    );
    if (
      !check(response, {
        "application sent mail through SMTP": (value) => value.status === 202,
      })
    ) {
      fail("synthetic application could not deliver the test message");
    }
  }

  try {
    if (
      !check(inbox.address, {
        "registered inbox has an address": (value) => value.includes("@"),
      })
    ) {
      fail("inbox did not expose an address");
    }

    const subject = `k6 isolated VU ${__VU}`;
    send(subject);
    const email = inbox.waitForEmail({ filter: { subject } });
    check(email, {
      "received this VU message through Rust": (value) =>
        value !== null && value.subject === subject,
      "message body preserved": (value) =>
        value !== null && value.textBody.includes("654321"),
    });
    const empty = inbox.forEachEmail(() => fail("consumed email replayed"), {
      durationMs: 150,
    });
    check(empty, {
      "consumed message is not returned again": (value) => value === 0,
    });
    const page = inbox.listEmails({ limit: 1 });
    check(page, {
      "listing retains consumed messages": (value) =>
        value.emails.length === 1 && value.emails[0].id === email.id,
    });
    check(inbox.getEmail(email.id), {
      "get reads stored message": (value) => value.subject === subject,
    });
    const attachment = inbox.getAttachment(email.id, email.attachments[0].id);
    check(new Uint8Array(attachment.content), {
      "attachment bytes preserved": (value) =>
        value.length === 4 && value[0] === 1 && value[3] === 4,
    });
    const saved = inbox.saveAttachment(
      email.id,
      email.attachments[0].id,
      `/tmp/ahrara-k6-${__VU}.bin`,
    );
    check(saved, {
      "attachment saved to requested path": (value) =>
        value.endsWith(`ahrara-k6-${__VU}.bin`),
    });
    let noOverwrite = false;
    try {
      inbox.saveAttachment(email.id, email.attachments[0].id, saved);
    } catch (_) {
      noOverwrite = true;
    }

    check(noOverwrite, {
      "attachment save refuses overwrite": (value) => value,
    });
    check(inbox.getRawEmail(email.id), {
      "raw MIME bytes available": (value) => value.byteLength > 4,
    });
    check(inbox.getHtml(email.id, false), {
      "HTML resources sanitized": (value) =>
        value.html.includes("Fixture HTML") &&
        value.remoteResources === 1 &&
        !value.html.includes(' src="https://remote.example.test/image.png"'),
    });
    send("Sequential first");
    send("Sequential second");
    const subjects = [];
    const count = inbox.forEachEmail((message) => {
      subjects.push(message.subject);
      return subjects.length < 2;
    }, {});
    check(count, {
      "callback processes emails sequentially": (value) =>
        value === 2 &&
        subjects.join("|") === "Sequential first|Sequential second",
    });
    send("Early break first");
    send("Callback error next");
    inbox.forEachEmail(() => false, {});
    let callbackFailed = false;
    try {
      inbox.forEachEmail((message) => {
        if (message.subject !== "Callback error next")
          fail("early break consumed the next email");
        throw new Error("callback sentinel");
      }, {});
    } catch (error) {
      callbackFailed = String(error).includes("callback sentinel");
    }

    check(callbackFailed, {
      "callback failure propagates without prefetch": (value) => value,
    });
    check(inbox.deleteEmail(email.id), {
      "single message deleted": (value) => value === 1,
    });
    check(inbox.clearEmails(), {
      "remaining mailbox cleared": (value) => value === 4,
    });
    check(inbox.listEmails({}), {
      "cleared mailbox is empty": (value) => value.emails.length === 0,
    });
    const queried = ahrara.createInbox({
      ...settings,
      profilePath: "",
      filter: {
        subject: "Query",
        sender: "SENDER@EXAMPLE.TEST",
        hasAttachments: true,
      },
    });
    try {
      send("Discard", "999999 is your verification code", queried);
      send("Query retained", "888888 is your verification code", queried);
      send(
        "Query code first",
        "123456 is your verification code\n234567 is your verification code",
        queried,
      );
      send("Query code second", "345678 is your verification code", queried);
      const codes = [];
      const extracted = queried.forEachRegexMatch(
        "(?P<code>[0-9]{6}) is your verification code",
        (match) => {
          codes.push(match.groups.find((group) => group.name === "code").value);
          if (match.groups[0].value !== match.value || match.emailId <= 0)
            fail("missing regex metadata");
        },
        {
          durationMs: 800,
          filter: { subject: "code", headers: { "x-flow": "SYNTHETIC" } },
        },
      );
      check(extracted, {
        "regex callback visits all emails and occurrences during total period":
          (value) => value === 3 && codes.join(",") === "123456,234567,345678",
      });
      check(queried.listEmails({}), {
        "inbox filter discards rejected mail before storage": (value) =>
          value.emails.length === 3,
      });
      check(
        queried.waitForEmail({
          filter: { subject: "retained", text: "888888" },
        }),
        {
          "query filter leaves unrelated email available": (value) =>
            value.subject === "Query retained",
        },
      );
      send("Query literal (1)", "777777 is your verification code", queried);
      let anchored = false;
      try {
        queried.waitForEmail({
          durationMs: 300,
          filter: { regex: true, envelopeSender: "sender@example" },
        });
      } catch (error) {
        anchored = String(error).includes("deadline exceeded");
      }

      check(anchored, {
        "regex envelope sender must match the whole value": (value) => value,
      });
      check(
        queried.waitForEmail({
          durationMs: 2000,
          filter: {
            regex: true,
            subject: "literal \\(1\\)",
            envelopeSender: "sender@example\\.test",
            text: "^7{6} is",
          },
        }),
        {
          "regex filter matches escaped literals": (value) =>
            value.subject === "Query literal (1)",
        },
      );
      send(
        "Query HTML first",
        "Links",
        queried,
        "<p><a href='/one'>One<br><a href='/two'>Two",
      );
      send("Query HTML second", "Links", queried, "<a href='/three'>Three</a>");
      const links = [];
      const extractedLinks = queried.forEachXPathValue(
        "//a/@href",
        (value) => {
          links.push(value);
        },
        { durationMs: 600, filter: { subject: "HTML" } },
      );
      check(extractedLinks, {
        "XPath callback visits all HTML emails in document order": (value) =>
          value === 3 && links.join(",") === "/one,/two,/three",
      });
      send("Query break first", "111111", queried);
      send("Query break next", "222222", queried);
      check(
        queried.forEachRegexMatch("[0-9]{6}", () => false, {}),
        { "regex callback false stops immediately": (value) => value === 1 },
      );
      check(queried.waitForRegexMatch("[0-9]{6}", {}), {
        "early query exit preserves next email": (value) =>
          value.value === "222222",
      });
      send("Query callback error", "333333", queried);
      let queryCallbackFailed = false;
      try {
        queried.forEachRegexMatch(
          "[0-9]{6}",
          () => {
            throw new Error("query callback sentinel");
          },
          {},
        );
      } catch (error) {
        queryCallbackFailed = String(error).includes("query callback sentinel");
      }

      check(queryCallbackFailed, {
        "query callback errors propagate": (value) => value,
      });
      send(
        "Query single XPath",
        "One link",
        queried,
        "<a href='/single'>Single</a>",
      );
      check(queried.waitForXPath("//a/@href", {}), {
        "single XPath value is returned": (value) => value === "/single",
      });
      let invalidQuery = false;
      try {
        queried.waitForRegexMatch("[", {});
      } catch (error) {
        invalidQuery = String(error).includes("invalid_argument");
      }

      check(invalidQuery, {
        "invalid regex fails before waiting": (value) => value,
      });
    } finally {
      queried.close();
    }

    credentials = inbox.exportCredentials();
    check(credentials, {
      "credential string display redacted": (value) =>
        !String(value).includes(value.recoveryKey),
    });
    check(credentials, {
      "credential JSON display redacted": (value) =>
        !JSON.stringify(value).includes(value.recoveryKey),
    });
    send("Persistent consumed");
    inbox.waitForEmail({});
    send("Persistent available");

    while (inbox.listEmails({}).emails.length < 2) sleep(0.01);
  } finally {
    inbox.close();
    inbox.close();
  }

  let closed = false;
  try {
    inbox.waitForEmail({});
  } catch (_) {
    closed = true;
  }

  check(closed, { "closed inbox rejects waits": (value) => value });
  const reopened = ahrara.createInbox(settings);
  try {
    check(reopened.address, {
      "profile preserves address": (value) => value === inbox.address,
    });
    check(reopened.listEmails({}), {
      "profile preserves stored history": (value) => value.emails.length === 2,
    });
    check(reopened.waitForEmail({}), {
      "profile preserves consumed state": (value) =>
        value.subject === "Persistent available",
    });
  } finally {
    reopened.close();
  }

  const imported = ahrara.createInbox({
    ...settings,
    profilePath: "",
    credentials,
  });
  try {
    check(imported.address, {
      "credentials restore inbox address": (value) => value === inbox.address,
    });
    check(imported.listEmails({}), {
      "credentials start fresh local history": (value) =>
        value.emails.length === 0,
    });
    send("Restored inbox delivery");
    check(imported.waitForEmail({}), {
      "restored inbox receives mail": (value) =>
        value.subject === "Restored inbox delivery",
    });
  } finally {
    imported.close();
  }

  completedFlows.add(1);
}
