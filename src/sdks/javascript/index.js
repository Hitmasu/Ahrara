import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { isAbsolute } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { inspect, promisify } from "node:util";
import koffi from "koffi";

/** Native failure with a stable `code`; messages never include Recovery Keys. */
export class AhraraError extends Error {
  constructor(code, message) {
    super(message);
    this.name = "AhraraError";
    this.code = code;
  }
}

/** Explicitly exported identity material. Inspection and JSON redact its key. */
export class InboxCredentials {
  #recoveryKey;

  constructor(address, recoveryKey) {
    this.address = address;
    this.#recoveryKey = recoveryKey;
  }

  /** Sensitive secret: access only to store securely or restore an identity. */
  get recoveryKey() {
    return this.#recoveryKey;
  }

  toJSON() {
    return { address: this.address, recoveryKey: "[redacted]" };
  }

  [inspect.custom]() {
    return "InboxCredentials { [redacted] }";
  }
}

function deadlineFor(timeoutMs) {
  if (timeoutMs === undefined) return Infinity;
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1) {
    throw new RangeError(
      "timeoutMs must be a positive safe integer, or omitted for no timeout.",
    );
  }

  return performance.now() + timeoutMs;
}

function nativeFilter(filter) {
  if (filter == null) return undefined;

  const { envelopeSender, messageId, hasAttachments, ...fields } = filter;
  return {
    ...fields,
    envelope_sender: envelopeSender,
    message_id: messageId,
    has_attachments: hasAttachments,
  };
}

let nativeCall;

function loadNative() {
  if (nativeCall) return nativeCall;

  const name =
    process.platform === "win32"
      ? "ahrara_native.dll"
      : process.platform === "darwin"
        ? "libahrara_native.dylib"
        : "libahrara_native.so";
  const path =
    process.env.AHRARA_NATIVE_LIBRARY ||
    fileURLToPath(
      new URL(
        `./native/${process.platform}-${process.arch}/${name}`,
        import.meta.url,
      ),
    );
  if (!isAbsolute(path) || !existsSync(path)) {
    throw new AhraraError(
      "native_library_missing",
      "Install a package containing the native library for this platform, or set AHRARA_NATIVE_LIBRARY to its absolute path.",
    );
  }

  const library = koffi.load(path);
  if (library.func("uint32_t ahrara_abi_version(void)")() !== 1) {
    throw new AhraraError(
      "abi_mismatch",
      "The native library requires a different SDK version.",
    );
  }

  const release = library.func("void ahrara_string_free(void *value)");
  const call = promisify(
    library.func("void *ahrara_call(const char *request)").async,
  );
  nativeCall = async (request) => {
    const pointer = await call(JSON.stringify(request));
    if (!pointer)
      throw new AhraraError(
        "native_error",
        "The native library returned no response.",
      );

    let response;
    try {
      response = JSON.parse(koffi.decode(pointer, "char", -1));
    } finally {
      release(pointer);
    }
    if (!response.ok)
      throw new AhraraError(response.error.code, response.error.message);

    return response.result;
  };

  return nativeCall;
}

// Releases inboxes collected without close(); explicit close remains the primary path.
const unclosed = new FinalizationRegistry(({ call, handle }) =>
  call({ op: "close", handle }).catch(() => {}),
);

/** Owns one relay subscription. Close explicitly; stopping a stream keeps it open. */
export class Inbox {
  #handle;
  #call;
  #closing;
  #queue = Promise.resolve();

  /** @internal Use createInbox(). */
  constructor(handle, address, call) {
    this.#handle = handle;
    this.address = address;
    this.#call = call;
    unclosed.register(this, { call, handle }, this);
  }

  /**
   * Consume one matching email, including messages buffered before this call.
   * @param {import('./index.js').WaitOptions} [options] Omit timeoutMs to wait indefinitely.
   * @returns {Promise<import('./index.js').Email>} The next unconsumed matching email.
   * @throws {AhraraError} `timeout`, `inbox_closed`, or a native error.
   * @throws {Error} AbortSignal.reason when cancelled; the inbox remains usable.
   */
  async waitForEmail({ subject, from, filter, ...options } = {}) {
    const result = await this.#wait(
      "wait",
      { subject, from, filter: nativeFilter(filter) },
      options,
    );

    return result.email;
  }

  async #wait(op, values, { timeoutMs, signal } = {}) {
    const deadline = deadlineFor(timeoutMs);
    // Waits take turns in call order; a queued wait still observes its
    // deadline, signal, and close.
    const previous = this.#queue;
    let release;
    const finished = new Promise((resolve) => (release = resolve));
    this.#queue = previous.then(() => finished);
    let queued = true;
    previous.then(() => (queued = false));
    try {
      while (queued && performance.now() < deadline) {
        signal?.throwIfAborted();
        const remaining = Math.ceil(deadline - performance.now());
        await Promise.race([
          previous,
          delay(Math.min(100, remaining), undefined, { ref: false }),
        ]);
      }
      if (!queued) {
        do {
          signal?.throwIfAborted();
          if (this.#closing)
            throw new AhraraError("inbox_closed", "The inbox is closed.");
          const remaining = Math.max(
            1,
            Math.ceil(deadline - performance.now()),
          );
          const result = await this.#call({
            op,
            handle: this.#handle,
            timeout_ms: Math.min(100, remaining),
            ...values,
          });
          if (result.status === "matched") return result;
        } while (performance.now() < deadline);
      }

      signal?.throwIfAborted();
      throw new AhraraError(
        "timeout",
        `No matching email arrived within ${timeoutMs} ms.`,
      );
    } finally {
      release();
    }
  }

  /**
   * Yield matching emails sequentially, with no prefetch queue.
   * Finite timeoutMs is the total subscription duration, including consumer work;
   * expiry ends normally. Omit it for an indefinite stream. Breaking the loop,
   * throwing, or aborting leaves the inbox open. Only the active wait owns a waiter.
   * @param {import('./index.js').WaitOptions} [options] Filters, total deadline, and abort signal.
   * @returns {AsyncGenerator<import('./index.js').Email, void, unknown>}
   */
  async *waitForEmails({ timeoutMs, subject, from, filter, signal } = {}) {
    yield* this.#stream({ timeoutMs, signal }, async (remaining) => [
      await this.waitForEmail({
        subject,
        from,
        filter,
        signal,
        timeoutMs: remaining,
      }),
    ]);
  }

  async *#stream({ timeoutMs, signal }, receive) {
    const deadline = deadlineFor(timeoutMs);
    while (true) {
      signal?.throwIfAborted();
      const remaining = deadline - performance.now();
      if (remaining <= 0) return;

      let values;
      try {
        values = await receive(
          Number.isFinite(remaining) ? Math.ceil(remaining) : undefined,
        );
      } catch (error) {
        if (error instanceof AhraraError && error.code === "timeout") return;

        throw error;
      }

      // A consumed email still yields its first value at the deadline.
      for (const [index, value] of values.entries()) {
        if (index) {
          signal?.throwIfAborted();
          if (performance.now() >= deadline) return;
        }

        yield value;
      }
    }
  }

  /**
   * Await handler(email) before receiving another message. Handler errors propagate.
   * Uses the same total deadline/cancellation/lifetime rules as waitForEmails().
   * @param {(email: import('./index.js').Email) => void | Promise<void>} handler
   * @param {import('./index.js').WaitOptions} [options]
   * @returns {Promise<void>} Resolves when a finite subscription expires.
   */
  async receiveEmails(handler, options = {}) {
    if (typeof handler !== "function")
      throw new TypeError("handler must be a function.");
    for await (const email of this.waitForEmails(options)) await handler(email);
  }

  /**
   * Consume one email with a regex match and return its first match and captures.
   * @param {string} pattern Shared Rust regex syntax, including named captures.
   * @param {import('./index.js').RegexOptions} [options] Source, flags, filter, deadline and cancellation.
   * @returns {Promise<import('./index.js').RegexMatch>} Group zero is the entire match; unmatched captures are null.
   * @throws {AhraraError} Invalid regex (without consuming mail), timeout, closed inbox or native failure.
   */
  async waitForRegexMatch(pattern, options = {}) {
    const result = await this.#search(
      this.#regexQuery(pattern, options, false),
      options,
    );

    return result.matches[0];
  }

  /**
   * Yield every regex match across matching emails; no background prefetch.
   * @param {string} pattern Shared Rust regex pattern.
   * @param {import('./index.js').RegexOptions} [options] Total deadline includes consumer work.
   * @returns {AsyncGenerator<import('./index.js').RegexMatch, void, unknown>}
   * @throws {AhraraError | Error} Invalid query, native failure, or AbortSignal.reason; expiry ends normally.
   */
  async *waitForRegexMatches(pattern, options = {}) {
    yield* this.#searchValues(
      this.#regexQuery(pattern, options, true),
      "matches",
      options,
    );
  }

  /**
   * Consume one email with an XPath result and return its first string value.
   * @param {string} expression XPath evaluated against the email's HTML by the shared core.
   * @param {import('./index.js').SearchOptions} [options] Filter, deadline and cancellation.
   * @returns {Promise<string>} Selected text, attribute, or scalar value.
   * @throws {AhraraError | Error} Invalid XPath, timeout, native failure, or AbortSignal.reason.
   */
  async waitForXPath(expression, options = {}) {
    const result = await this.#search(
      { kind: "xpath", expression, all: false },
      options,
    );

    return result.values[0];
  }

  /**
   * Yield every XPath value across matching emails. Finite total expiry ends normally.
   * @param {string} expression Shared core XPath expression.
   * @param {import('./index.js').SearchOptions} [options] No timeout means indefinite; cancellation keeps the inbox open.
   * @returns {AsyncGenerator<string, void, unknown>} Values from each matched email in query order.
   * @throws {AhraraError | Error} Invalid query, native failure, or AbortSignal.reason.
   */
  async *waitForXPathValues(expression, options = {}) {
    yield* this.#searchValues(
      { kind: "xpath", expression, all: true },
      "values",
      options,
    );
  }

  #regexQuery(
    pattern,
    {
      source = "text",
      ignoreCase = false,
      multiLine = false,
      dotMatchesNewLine = false,
    },
    all,
  ) {
    return {
      kind: "regex",
      pattern,
      source,
      ignore_case: ignoreCase,
      multi_line: multiLine,
      dot_matches_new_line: dotMatchesNewLine,
      all,
    };
  }

  #search(query, { filter, ...options }) {
    return this.#wait(
      "search",
      { query, filter: nativeFilter(filter) },
      options,
    );
  }

  #searchValues(query, field, { timeoutMs, signal, ...options }) {
    return this.#stream(
      { timeoutMs, signal },
      async (remaining) =>
        (
          await this.#search(query, {
            ...options,
            signal,
            timeoutMs: remaining,
          })
        )[field],
    );
  }

  /**
   * Export sensitive credentials explicitly. Keep the Recovery Key out of logs.
   * @returns {Promise<InboxCredentials>} Address and key with redacted inspection/JSON.
   * @throws {AhraraError} If the inbox is closed or native export fails.
   */
  async exportCredentials() {
    const result = await this.#request("export_credentials");
    return new InboxCredentials(result.address, result.recovery_key);
  }

  /**
   * List inbox-scoped metadata without consuming the receive cursor.
   * @param {import('./index.js').ListOptions} [options] Exclusive ID cursor, page size, and filters.
   * @returns {Promise<import('./index.js').EmailPage>} Metadata page and optional next cursor.
   * @throws {AhraraError} Invalid cursor/page size, closed inbox, or native storage failure.
   */
  listEmails({ afterId = 0, limit = 20, subject, from } = {}) {
    return this.#request("list", { after_id: afterId, limit, subject, from });
  }

  /**
   * Read a stored email belonging to this inbox.
   * @param {number} id Inbox-owned email ID.
   * @returns {Promise<import('./index.js').Email>} Full parsed message and attachment metadata.
   * @throws {AhraraError} If unavailable, outside this inbox, closed, or native storage fails.
   */
  getEmail(id) {
    return this.#request("get", { id });
  }

  /**
   * Delete one inbox-owned message from local history.
   * @param {number} id Inbox-owned email ID.
   * @returns {Promise<number>} Deleted count, or zero if no owned message matches.
   * @throws {AhraraError} If closed or the native operation fails.
   */
  async deleteEmail(id) {
    return (await this.#request("delete", { id })).deleted;
  }

  /**
   * Delete all stored messages belonging to this inbox.
   * @returns {Promise<number>} Number of deleted messages.
   * @throws {AhraraError} If closed or native storage fails.
   */
  async clearEmails() {
    return (await this.#request("clear")).deleted;
  }

  /**
   * Return attachment metadata and decoded bytes, without writing a file.
   * @param {number} id Inbox-owned email ID.
   * @param {string} attachmentId Identifier from Email.attachments.
   * @returns {Promise<import('./index.js').AttachmentData>} Metadata and binary content.
   * @throws {AhraraError} If unavailable, outside this inbox, closed, or native parsing fails.
   */
  async getAttachment(id, attachmentId) {
    const result = await this.#request("attachment", {
      id,
      attachment_id: attachmentId,
    });

    return {
      attachment: result.attachment,
      content: Buffer.from(result.content_base64, "base64"),
    };
  }

  /**
   * Save an attachment; existing files are never overwritten.
   * @param {number} id Inbox-owned email ID.
   * @param {string} attachmentId Identifier from Email.attachments.
   * @param {string} destination Absolute new file path or existing directory; filenames are sanitized.
   * @returns {Promise<string>} Absolute path of the saved file.
   * @throws {AhraraError} Missing attachment, existing destination, invalid path, closed inbox, or I/O failure.
   */
  async saveAttachment(id, attachmentId, destination) {
    return (
      await this.#request("save_attachment", {
        id,
        attachment_id: attachmentId,
        destination,
      })
    ).path;
  }

  /**
   * Read the original RFC 822 message bytes.
   * @param {number} id Inbox-owned email ID.
   * @returns {Promise<Buffer>} Original message content.
   * @throws {AhraraError} If unavailable, outside this inbox, closed, or native storage fails.
   */
  async getRawEmail(id) {
    return Buffer.from(
      (await this.#request("raw", { id })).content_base64,
      "base64",
    );
  }

  /**
   * Read sanitized HTML. Remote resources are blocked unless explicitly allowed.
   * @param {number} id Inbox-owned email ID.
   * @param {{allowRemote?: boolean}} [options] Explicit opt-in to remote resource URLs.
   * @returns {Promise<import('./index.js').HtmlEmail>} Sanitized HTML and remote resource count.
   * @throws {AhraraError} If HTML is unavailable, outside this inbox, closed, or native parsing fails.
   */
  getHtml(id, { allowRemote = false } = {}) {
    return this.#request("html", { id, allow_remote: allowRemote });
  }

  #request(op, values = {}) {
    if (this.#closing)
      return Promise.reject(
        new AhraraError("inbox_closed", "The inbox is closed."),
      );

    return this.#call({ op, handle: this.#handle, ...values });
  }

  /**
   * Disconnect and release resources. Idempotent; persistent profiles remain on disk.
   * @returns {Promise<void>} Resolves after the native subscription shuts down.
   * @throws {AhraraError} If native cleanup fails.
   */
  close() {
    unclosed.unregister(this);
    this.#closing ??= this.#call({ op: "close", handle: this.#handle });
    return this.#closing.then(() => undefined);
  }

  async [Symbol.asyncDispose]() {
    await this.close();
  }
}

/**
 * Authenticate and subscribe before returning an inbox. No CLI or daemon is started.
 * @param {import('./index.js').InboxOptions} [options] Relay trust, persistence, and identity options.
 * @returns {Promise<Inbox>} A ready inbox; messages arriving before the first wait are buffered.
 * @throws {AhraraError} Invalid identity/configuration, profile conflict, or connection failure.
 */
export async function createInbox(options = {}) {
  const call = loadNative();
  const result = await call({
    op: "create",
    options: {
      server_url: options.serverUrl,
      server_public_key: options.serverPublicKey,
      tls_ca_path: options.tlsCaPath,
      connect_timeout_ms: options.connectTimeoutMs,
      profile_path: options.profilePath,
      credentials: options.credentials && {
        address: options.credentials.address,
        recovery_key: options.credentials.recoveryKey,
      },
      recovery_key: options.recoveryKey,
      address: options.address,
      filter: nativeFilter(options.filter),
    },
  });

  return new Inbox(result.handle, result.address, call);
}

export const Ahrara = Object.freeze({ createInbox });
