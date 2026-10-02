import {
  Ahrara,
  createInbox,
  type Email,
  type InboxOptions,
  type EmailPage,
  type AttachmentData,
} from "../index.js";

async function usage(options: InboxOptions): Promise<Email> {
  const inbox = await Ahrara.createInbox(options);
  try {
    const address: string = inbox.address;
    void address;
    return await inbox.waitForEmail({
      timeoutMs: 1000,
      signal: new AbortController().signal,
    });
  } finally {
    await inbox.close();
  }
}

void usage;
void createInbox;

async function completeApi(options: InboxOptions): Promise<void> {
  await using inbox = await createInbox(options);
  const credentials = await inbox.exportCredentials();
  const restoreOptions: InboxOptions = { ...options, credentials };
  void restoreOptions;
  const page: EmailPage = await inbox.listEmails({ limit: 1 });

  for (const metadata of page.emails) {
    const email: Email = await inbox.getEmail(metadata.id);
    const raw: Buffer = await inbox.getRawEmail(email.id);
    const html: string = (await inbox.getHtml(email.id)).html;
    void raw;
    void html;

    for (const attachment of email.attachments) {
      const data: AttachmentData = await inbox.getAttachment(
        email.id,
        attachment.attachment_id,
      );
      const path: string = await inbox.saveAttachment(
        email.id,
        attachment.attachment_id,
        "/tmp/downloads",
      );
      void data;
      void path;
    }
  }

  for await (const email of inbox.waitForEmails({ timeoutMs: 100 })) {
    const subject: string = email.subject;
    void subject;
    break;
  }

  await inbox.receiveEmails(
    async (email) => {
      void email.subject;
    },
    { timeoutMs: 100 },
  );
}

void completeApi;

async function searchApi(options: InboxOptions): Promise<void> {
  await using inbox = await createInbox({
    ...options,
    filter: { sender: "sender", headers: { "X-Test": "flow" } },
  });
  const match = await inbox.waitForRegexMatch("(?<code>[0-9]+)", {
    filter: { hasAttachments: false },
    source: "text",
    ignoreCase: true,
  });
  const value: string | null | undefined = match.groups[1].value;
  void value;

  for await (const match of inbox.waitForRegexMatches("[0-9]+", {
    timeoutMs: 100,
  }))
    void match.email_id;

  const xpath: string = await inbox.waitForXPath("//a/@href");
  void xpath;

  for await (const value of inbox.waitForXPathValues("//a/@href", {
    timeoutMs: 100,
  }))
    void value;
}

void searchApi;
