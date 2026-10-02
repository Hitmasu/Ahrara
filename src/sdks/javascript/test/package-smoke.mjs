import assert from "node:assert/strict";
import { createInbox } from "@ahrara/sdk";

// Copied into an isolated consumer directory by Makefile, so the import above
// resolves the installed tarball instead of this repository's source package.
assert.equal(process.env.AHRARA_NATIVE_LIBRARY, undefined);
const { inboxOptions, sendEmail } = await import(process.argv[2]);
const inbox = await createInbox(inboxOptions());
try {
  await sendEmail(
    inbox.address,
    "Packaged JavaScript SDK",
    "Bundled native core received this email.",
  );
  const email = await inbox.waitForEmail({ timeoutMs: 10_000 });

  assert.equal(email.subject, "Packaged JavaScript SDK");
  assert.ok(email.text_body.includes("Bundled native core"));
  await sendEmail(inbox.address, "Packaged query", "code=123");

  assert.equal(
    (await inbox.waitForRegexMatch("(?<code>[0-9]+)", { timeoutMs: 1000 }))
      .groups[1].value,
    "123",
  );
  console.log(
    "Installed npm package received email with its bundled native core.",
  );
} finally {
  await inbox.close();
}
