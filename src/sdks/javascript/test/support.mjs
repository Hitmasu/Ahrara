import { readFileSync } from "node:fs";
import { createServer } from "node:http";
import nodemailer from "nodemailer";

export function inboxOptions() {
  return {
    serverUrl: process.env.AHRARA_TEST_SERVER_URL ?? "wss://localhost:8443",
    serverPublicKey:
      process.env.AHRARA_TEST_SERVER_PUBLIC_KEY ??
      readFileSync(
        new URL("../../../../.dev/certs/transport-public.txt", import.meta.url),
        "utf8",
      ).trim(),
    tlsCaPath:
      process.env.AHRARA_TEST_TLS_CA ??
      new URL("../../../../.dev/certs/ca.pem", import.meta.url).pathname,
    connectTimeoutMs: 10_000,
  };
}

export async function sendEmail(
  address,
  subject,
  text = "Verification code: 123456",
  { html, attachments, headers, cc, messageId } = {},
) {
  const endpoint = new URL(
    `smtp://${process.env.AHRARA_TEST_SMTP_ADDRESS ?? "localhost:2525"}`,
  );
  const transport = nodemailer.createTransport({
    host: endpoint.hostname,
    port: Number(endpoint.port),
    requireTLS: true,
    tls: {
      ca: readFileSync(inboxOptions().tlsCaPath),
      servername: "localhost",
    },
  });
  try {
    await transport.sendMail({
      from: "sender@example.test",
      to: address,
      subject,
      text,
      html,
      attachments,
      headers,
      cc,
      messageId,
    });
  } finally {
    transport.close();
  }
}

export async function startApp() {
  const html = `<!doctype html><html><head><title>Signup fixture</title></head><body>
    <form id="signup"><label>Email <input name="email" type="email" required></label><button>Sign up</button></form>
    <p id="status"></p><label>Verification code <input name="code"></label><button id="verify">Verify</button>
    <script>
      document.querySelector('#signup').onsubmit = async event => {
        event.preventDefault();
        const response = await fetch('/signup', { method: 'POST', body: document.querySelector('[name=email]').value });
        document.querySelector('#status').textContent = response.ok ? 'Email sent' : 'Send failed';
      };
      document.querySelector('#verify').onclick = () => {
        document.querySelector('#status').textContent = document.querySelector('[name=code]').value === '123456' ? 'Verified' : 'Invalid code';
      };
    </script></body></html>`;
  const server = createServer(async (request, response) => {
    try {
      if (request.method === "POST" && request.url === "/signup") {
        let address = "";
        for await (const chunk of request) address += chunk;

        await sendEmail(address, "Confirm signup");
        response.end("sent");
      } else {
        response.setHeader("Content-Type", "text/html");
        response.end(html);
      }
    } catch {
      response.statusCode = 500;
      response.end("Send failed");
    }
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  return {
    url: `http://127.0.0.1:${server.address().port}`,
    close: () => new Promise((resolve) => server.close(resolve)),
  };
}
