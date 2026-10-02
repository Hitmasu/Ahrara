import { test as base, expect } from "@playwright/test";
import { createInbox } from "../index.js";
import { inboxOptions, startApp } from "./support.mjs";

const test = base.extend({
  inbox: async ({}, use) => {
    const inbox = await createInbox(inboxOptions());
    try {
      await use(inbox);
    } finally {
      await inbox.close();
    }
  },
});

test("signup and verification use a directly connected inbox", async ({
  page,
  inbox,
}) => {
  const app = await startApp();
  try {
    await page.goto(app.url);
    await page.getByLabel("Email").fill(inbox.address);
    await page.getByRole("button", { name: "Sign up" }).click();
    const match = await inbox.waitForRegexMatch("(?<code>[0-9]{6})", {
      filter: { subject: "Confirm signup" },
      timeoutMs: 10_000,
    });
    await expect(page.locator("#status")).toHaveText("Email sent");
    await page
      .getByLabel("Verification code")
      .fill(match.groups.find((group) => group.name === "code").value);
    await page.getByRole("button", { name: "Verify", exact: true }).click();
    await expect(page.locator("#status")).toHaveText("Verified");
  } finally {
    await app.close();
  }
});
