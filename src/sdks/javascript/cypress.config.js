import { defineConfig } from "cypress";
import { createInbox } from "./index.js";
import { inboxOptions, startApp } from "./test/support.mjs";

export default defineConfig({
  video: false,
  e2e: {
    supportFile: false,
    async setupNodeEvents(on, config) {
      const app = await startApp();
      const inboxes = new Map();
      config.baseUrl = app.url;
      on("task", {
        async createInbox() {
          const inbox = await createInbox(inboxOptions());
          inboxes.set(inbox.address, inbox);
          return inbox.address;
        },
        waitForRegexMatch(address) {
          return inboxes.get(address).waitForRegexMatch("(?<code>[0-9]{6})", {
            filter: { subject: "Confirm signup" },
            timeoutMs: 10_000,
          });
        },
        async closeInbox(address) {
          await inboxes.get(address)?.close();
          inboxes.delete(address);
          return null;
        },
      });
      on("after:run", async () => {
        await Promise.all([...inboxes.values()].map((inbox) => inbox.close()));
        await app.close();
      });

      return config;
    },
  },
});
