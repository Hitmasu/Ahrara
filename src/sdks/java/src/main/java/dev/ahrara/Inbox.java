package dev.ahrara;

import com.fasterxml.jackson.databind.JsonNode;
import java.lang.ref.Cleaner;
import java.nio.file.Path;
import java.time.Duration;
import java.util.Base64;
import java.util.Collections;
import java.util.Iterator;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Objects;
import java.util.Spliterator;
import java.util.Spliterators;
import java.util.concurrent.CancellationException;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.TimeoutException;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.locks.ReentrantLock;
import java.util.function.BooleanSupplier;
import java.util.function.Consumer;
import java.util.function.Function;
import java.util.stream.Stream;
import java.util.stream.StreamSupport;

/**
 * A connected inbox with lazy reception and access to encrypted local history. Close with
 * try-with-resources. Closing an email stream leaves the inbox usable.
 */
public final class Inbox implements AutoCloseable {
  private static final Cleaner CLEANER = Cleaner.create();
  private final State state;
  private final Cleaner.Cleanable cleanable;
  private final String address;
  private final ReentrantLock waitLock = new ReentrantLock();

  private static final class State implements Runnable {
    final long handle;
    final AtomicBoolean closed = new AtomicBoolean();

    State(long handle) {
      this.handle = handle;
    }

    @Override
    public void run() {
      if (closed.compareAndSet(false, true)) {
        NativeCore.call(Map.of("op", "close", "handle", handle));
      }
    }
  }

  Inbox(long handle, String address) {
    this.state = new State(handle);
    this.address = address;
    this.cleanable = CLEANER.register(this, state);
  }

  /**
   * @return the connected inbox address
   */
  public String address() {
    return address;
  }

  /**
   * Exports the secret identity and address. Close this connection before reopening the identity.
   *
   * @return credentials to store securely; history is stored separately in the profile
   * @throws IllegalStateException if the inbox is closed
   * @throws AhraraException if export fails
   */
  public InboxCredentials exportCredentials() {
    return NativeCore.decode(call("export_credentials"), InboxCredentials.class);
  }

  /**
   * Waits indefinitely for the next email, including buffered mail.
   *
   * @return the next email, consumed exactly once by receiving operations
   * @throws InterruptedException if interrupted; the inbox remains usable
   * @throws AhraraException if reception fails
   * @throws IllegalStateException if the inbox is closed
   */
  public Email waitForEmail() throws InterruptedException {
    try {
      return waitForEmail(null);
    } catch (TimeoutException impossible) {
      throw new AssertionError("An indefinite wait cannot expire.", impossible);
    }
  }

  /**
   * Waits for one email with an optional timeout.
   *
   * @param timeout positive timeout, or null for no time limit
   * @return the next email
   * @throws InterruptedException if interrupted; the inbox remains usable
   * @throws TimeoutException if the supplied timeout expires
   * @throws IllegalArgumentException if the timeout is invalid
   * @throws AhraraException if reception fails
   * @throws IllegalStateException if the inbox is closed
   */
  public Email waitForEmail(Duration timeout) throws InterruptedException, TimeoutException {
    return waitForEmail(timeout, null);
  }

  /**
   * Waits for one matching email without discarding unmatched messages.
   *
   * @param timeout optional positive timeout; null waits indefinitely
   * @param filter optional AND conditions; nonmatching mail remains available
   * @return the next matching email
   * @throws InterruptedException if interrupted
   * @throws TimeoutException if the finite timeout expires
   * @throws AhraraException if receiving or filter validation fails
   */
  public Email waitForEmail(Duration timeout, EmailFilter filter)
      throws InterruptedException, TimeoutException {
    var result = awaitResult(timeout, request("wait", null, filter), () -> false);
    return NativeCore.decode(result.path("email"), Email.class);
  }

  private JsonNode awaitResult(
      Duration timeout, Map<String, Object> arguments, BooleanSupplier stopped)
      throws InterruptedException, TimeoutException {
    final long duration = durationNanos(timeout);
    final long started = System.nanoTime();
    requireOpen();

    while (true) {
      if (stopped.getAsBoolean()) {
        return null;
      }

      requireOpen();
      long remaining = remainingNanos(duration, started);
      if (remaining <= 0) {
        throw timeout();
      }
      if (waitLock.tryLock(
          Math.min(remaining, TimeUnit.MILLISECONDS.toNanos(100)), TimeUnit.NANOSECONDS)) {
        break;
      }
    }
    try {
      while (true) {
        if (Thread.interrupted()) {
          throw new InterruptedException("Ahrara email wait interrupted.");
        }
        if (stopped.getAsBoolean()) {
          return null;
        }

        requireOpen();
        long remaining = remainingNanos(duration, started);
        if (remaining <= 0) {
          throw timeout();
        }

        var request = new LinkedHashMap<String, Object>(arguments);
        request.put("handle", state.handle);
        request.put(
            "timeout_ms", Math.max(1, Math.min(100, TimeUnit.NANOSECONDS.toMillis(remaining))));
        var result = callNative(request);
        if (result.path("status").asText().equals("matched")) {
          return result;
        }
      }
    } finally {
      waitLock.unlock();
    }
  }

  /**
   * Creates a lazy stream without a duration limit, background thread, or prefetch queue.
   *
   * @return a sequential stream; use try-with-resources when consuming it
   * @throws IllegalStateException if the inbox is closed
   */
  public Stream<Email> emails() {
    return emails(null, null);
  }

  /**
   * Creates a lazy stream with a total duration and no filters.
   *
   * @param duration optional positive total duration, including consumer processing
   * @return a sequential stream; closing it leaves the inbox connected
   * @throws IllegalArgumentException if duration is invalid
   * @throws IllegalStateException if the inbox is closed
   */
  public Stream<Email> emails(Duration duration) {
    return emails(duration, null);
  }

  /**
   * Creates a lazy sequential stream. Duration starts on first advance and includes consumer time.
   *
   * @param duration optional positive total duration; null is unlimited
   * @param filter optional AND conditions retaining unmatched mail
   * @return a stream ending normally on duration expiry or close, without closing the inbox
   * @throws CancellationException during traversal if interrupted; the interrupt flag is restored
   * @throws IllegalArgumentException if duration is invalid
   * @throws IllegalStateException if the inbox is closed
   * @throws AhraraException during traversal if validation or reception fails
   */
  public Stream<Email> emails(Duration duration, EmailFilter filter) {
    return results(
        request("wait", null, filter),
        result -> List.of(result.path("email")).iterator(),
        email -> NativeCore.decode(email, Email.class),
        duration);
  }

  /**
   * Receives indefinitely and invokes a handler sequentially on the calling thread.
   *
   * @param handler callback; its failures propagate
   * @throws CancellationException if interrupted; the interrupt flag is restored
   * @throws NullPointerException if handler is null
   * @throws AhraraException if reception fails
   * @throws IllegalStateException if the inbox is closed
   */
  public void receiveEmails(Consumer<Email> handler) {
    receiveEmails(handler, null, null);
  }

  /**
   * Receives emails and invokes a handler sequentially on the calling thread.
   *
   * @param handler callback; its exceptions propagate and delivered mail remains consumed
   * @param duration optional positive total duration, including callback time; null is unlimited
   * @param filter optional AND conditions retaining nonmatching mail
   * @throws CancellationException if interrupted; the interrupt flag is restored
   * @throws IllegalArgumentException if duration is invalid
   * @throws NullPointerException if handler is null
   * @throws AhraraException if validation or reception fails
   * @throws IllegalStateException if the inbox is closed
   */
  public void receiveEmails(Consumer<Email> handler, Duration duration, EmailFilter filter) {
    Objects.requireNonNull(handler, "handler");
    try (var stream = emails(duration, filter)) {
      stream.forEachOrdered(handler);
    }
  }

  /**
   * Lists the first 20 stored emails without consuming them.
   *
   * @return the first metadata page
   * @throws AhraraException if listing fails
   * @throws IllegalStateException if the inbox is closed
   */
  public EmailPage listEmails() {
    return listEmails(0, 20, null, null);
  }

  /**
   * Lists stored metadata without affecting receiving operations.
   *
   * @param afterId exclusive ID cursor; zero starts the first page
   * @param limit page size, 1 to 100
   * @param subject optional subject substring
   * @param from optional sender substring
   * @return the metadata page and next cursor
   * @throws AhraraException if arguments are invalid or listing fails
   * @throws IllegalStateException if the inbox is closed
   */
  public EmailPage listEmails(long afterId, int limit, String subject, String from) {
    var arguments = new LinkedHashMap<String, Object>();
    arguments.put("after_id", afterId);
    arguments.put("limit", limit);
    if (subject != null) {
      arguments.put("subject", subject);
    }
    if (from != null) {
      arguments.put("from", from);
    }

    return NativeCore.decode(call("list", arguments), EmailPage.class);
  }

  /**
   * Reads a stored email without consuming it.
   *
   * @param id local email identifier
   * @return the parsed email and attachment metadata
   * @throws AhraraException if the email is missing or reading fails
   * @throws IllegalStateException if the inbox is closed
   */
  public Email getEmail(long id) {
    return NativeCore.decode(call("get", Map.of("id", id)), Email.class);
  }

  /**
   * Deletes one email belonging to this inbox.
   *
   * @param id local email identifier
   * @return number of deleted emails
   * @throws AhraraException if deletion fails
   * @throws IllegalStateException if the inbox is closed
   */
  public int deleteEmail(long id) {
    return call("delete", Map.of("id", id)).path("deleted").asInt();
  }

  /**
   * Deletes this inbox's stored history.
   *
   * @return number of deleted emails
   * @throws AhraraException if deletion fails
   * @throws IllegalStateException if the inbox is closed
   */
  public int clearEmails() {
    return call("clear").path("deleted").asInt();
  }

  /**
   * Returns decoded attachment bytes without writing a file.
   *
   * @param id email identifier
   * @param attachmentId identifier from the email's attachment metadata
   * @return attachment metadata and content
   * @throws AhraraException if the email or attachment is missing or reading fails
   * @throws IllegalStateException if the inbox is closed
   */
  public AttachmentDownload getAttachment(long id, String attachmentId) {
    var value = call("attachment", Map.of("id", id, "attachment_id", attachmentId));
    return new AttachmentDownload(
        NativeCore.decode(value.path("attachment"), Attachment.class),
        Base64.getDecoder().decode(value.path("content_base64").asText()));
  }

  /**
   * Saves an attachment with native safe filename and no-overwrite rules.
   *
   * @param id email identifier
   * @param attachmentId attachment identifier
   * @param destination absolute destination directory or filename
   * @return saved file path
   * @throws AhraraException if saving fails, including an existing destination
   * @throws IllegalStateException if the inbox is closed
   */
  public Path saveAttachment(long id, String attachmentId, Path destination) {
    return Path.of(
        call(
                "save_attachment",
                Map.of(
                    "id", id, "attachment_id", attachmentId, "destination", destination.toString()))
            .path("path")
            .asText());
  }

  /**
   * Reads the original RFC 5322 message.
   *
   * @param id email identifier
   * @return original message bytes
   * @throws AhraraException if the email is missing or reading fails
   * @throws IllegalStateException if the inbox is closed
   */
  public byte[] getRawEmail(long id) {
    return Base64.getDecoder()
        .decode(call("raw", Map.of("id", id)).path("content_base64").asText());
  }

  /**
   * Reads HTML sanitized by the Rust core.
   *
   * @param id email identifier
   * @param allowRemote whether sanitized HTML may reference remote resources
   * @return sanitized HTML and remote-resource count
   * @throws AhraraException if the email is missing or reading fails
   * @throws IllegalStateException if the inbox is closed
   */
  public EmailHtml getHtml(long id, boolean allowRemote) {
    return NativeCore.decode(
        call("html", Map.of("id", id, "allow_remote", allowRemote)), EmailHtml.class);
  }

  /**
   * Waits indefinitely for the first matching result.
   *
   * @param query the regex pattern, source, and flags
   * @return the first result, consuming its source email
   * @throws InterruptedException if interrupted; the inbox remains usable
   * @throws AhraraException if query validation or reception fails
   */
  public RegexMatch waitForRegexMatch(RegexQuery query) throws InterruptedException {
    try {
      return waitForRegexMatch(query, null, null);
    } catch (TimeoutException impossible) {
      throw new AssertionError("An indefinite search cannot expire.", impossible);
    }
  }

  /**
   * Waits for the first matching result using the shared Rust engine.
   *
   * @param query the regex pattern, source, and flags
   * @param timeout optional positive timeout; null is unlimited
   * @param filter additional AND conditions; nonmatching emails remain available
   * @return the first result, consuming its source email
   * @throws InterruptedException if interrupted; the inbox remains usable
   * @throws TimeoutException if the finite timeout expires
   * @throws AhraraException if query validation or reception fails
   */
  public RegexMatch waitForRegexMatch(RegexQuery query, Duration timeout, EmailFilter filter)
      throws InterruptedException, TimeoutException {
    Objects.requireNonNull(query, "query");
    var result =
        awaitResult(timeout, request("search", query.toNative(false), filter), () -> false);
    return NativeCore.decode(result.path("matches").get(0), RegexMatch.class);
  }

  /**
   * Creates an indefinite lazy stream of matches across all emails.
   *
   * @param query the regex pattern, source, and flags
   * @return sequential results; close the stream after consumption
   * @throws CancellationException on interruption; the interrupt flag remains set
   * @throws AhraraException if query validation or reception fails
   */
  public Stream<RegexMatch> waitForRegexMatches(RegexQuery query) {
    return waitForRegexMatches(query, null, null);
  }

  /**
   * Lazily yields all results across emails, including all matches in each email. Duration includes
   * consumer time. Expiry ends normally; closing leaves the inbox open. An email is consumed once;
   * an early stop does not replay its remaining matches.
   *
   * @param query the regex pattern, source, and flags
   * @param duration optional positive total duration; null is unlimited
   * @param filter additional AND conditions retaining nonmatching email
   * @return a sequential stream with at most one native email batch buffered
   * @throws CancellationException on interruption; the interrupt flag remains set
   * @throws AhraraException if query validation or reception fails
   */
  public Stream<RegexMatch> waitForRegexMatches(
      RegexQuery query, Duration duration, EmailFilter filter) {
    Objects.requireNonNull(query, "query");
    return results(
        request("search", query.toNative(true), filter),
        result -> result.path("matches").iterator(),
        match -> NativeCore.decode(match, RegexMatch.class),
        duration);
  }

  /**
   * Waits indefinitely for the first matching result.
   *
   * @param expression XPath expression over the email HTML
   * @return the first result, consuming its source email
   * @throws InterruptedException if interrupted; the inbox remains usable
   * @throws AhraraException if query validation or reception fails
   */
  public String waitForXPath(String expression) throws InterruptedException {
    try {
      return waitForXPath(expression, null, null);
    } catch (TimeoutException impossible) {
      throw new AssertionError("An indefinite search cannot expire.", impossible);
    }
  }

  /**
   * Waits for the first matching result using the shared Rust engine.
   *
   * @param expression XPath expression over the email HTML
   * @param timeout optional positive timeout; null is unlimited
   * @param filter additional AND conditions; nonmatching emails remain available
   * @return the first result, consuming its source email
   * @throws InterruptedException if interrupted; the inbox remains usable
   * @throws TimeoutException if the finite timeout expires
   * @throws AhraraException if query validation or reception fails
   */
  public String waitForXPath(String expression, Duration timeout, EmailFilter filter)
      throws InterruptedException, TimeoutException {
    Objects.requireNonNull(expression, "expression");
    var result =
        awaitResult(
            timeout,
            request(
                "search", Map.of("kind", "xpath", "expression", expression, "all", false), filter),
            () -> false);

    return result.path("values").get(0).asText();
  }

  /**
   * Creates an indefinite lazy stream of matches across all emails.
   *
   * @param expression XPath expression over the email HTML
   * @return sequential results; close the stream after consumption
   * @throws CancellationException on interruption; the interrupt flag remains set
   * @throws AhraraException if query validation or reception fails
   */
  public Stream<String> waitForXPathValues(String expression) {
    return waitForXPathValues(expression, null, null);
  }

  /**
   * Lazily yields all results across emails, including all matches in each email. Duration includes
   * consumer time. Expiry ends normally; closing leaves the inbox open. An email is consumed once;
   * an early stop does not replay its remaining matches.
   *
   * @param expression XPath expression over the email HTML
   * @param duration optional positive total duration; null is unlimited
   * @param filter additional AND conditions retaining nonmatching email
   * @return a sequential stream with at most one native email batch buffered
   * @throws CancellationException on interruption; the interrupt flag remains set
   * @throws AhraraException if query validation or reception fails
   */
  public Stream<String> waitForXPathValues(
      String expression, Duration duration, EmailFilter filter) {
    Objects.requireNonNull(expression, "expression");
    return results(
        request("search", Map.of("kind", "xpath", "expression", expression, "all", true), filter),
        result -> result.path("values").iterator(),
        JsonNode::asText,
        duration);
  }

  /**
   * Waits indefinitely for a regex using default plain-text, case-sensitive options.
   *
   * @param pattern Rust regex expression
   * @return the first match
   * @throws InterruptedException if interrupted
   * @throws AhraraException if query validation or reception fails
   */
  public RegexMatch waitForRegexMatch(String pattern) throws InterruptedException {
    return waitForRegexMatch(new RegexQuery(pattern));
  }

  /**
   * Waits for a regex using default plain-text, case-sensitive options.
   *
   * @param pattern Rust regex expression
   * @param timeout optional positive timeout; null is unlimited
   * @param filter additional email conditions
   * @return the first match
   * @throws InterruptedException if interrupted
   * @throws TimeoutException if the finite timeout expires
   * @throws AhraraException if query validation or reception fails
   */
  public RegexMatch waitForRegexMatch(String pattern, Duration timeout, EmailFilter filter)
      throws InterruptedException, TimeoutException {
    return waitForRegexMatch(new RegexQuery(pattern), timeout, filter);
  }

  /**
   * Creates an indefinite regex result stream with default plain-text options.
   *
   * @param pattern Rust regex expression
   * @return a lazy sequential stream
   * @throws CancellationException during traversal if interrupted
   * @throws AhraraException if query validation or reception fails
   */
  public Stream<RegexMatch> waitForRegexMatches(String pattern) {
    return waitForRegexMatches(new RegexQuery(pattern));
  }

  /**
   * Creates a regex result stream with default plain-text options.
   *
   * @param pattern Rust regex expression
   * @param duration total duration including consumer processing; null is unlimited
   * @param filter additional email conditions
   * @return a lazy sequential stream
   * @throws CancellationException during traversal if interrupted
   * @throws AhraraException if query validation or reception fails
   */
  public Stream<RegexMatch> waitForRegexMatches(
      String pattern, Duration duration, EmailFilter filter) {
    return waitForRegexMatches(new RegexQuery(pattern), duration, filter);
  }

  private static Map<String, Object> request(
      String operation, Map<String, Object> query, EmailFilter filter) {
    var request = new LinkedHashMap<String, Object>();
    request.put("op", operation);
    if (query != null) request.put("query", query);
    if (filter != null) request.put("filter", filter);

    return request;
  }

  // Lazily yields each item of every matched native result; at most one result is buffered.
  private <T> Stream<T> results(
      Map<String, Object> request,
      Function<JsonNode, Iterator<JsonNode>> items,
      Function<JsonNode, T> decode,
      Duration duration) {
    long limit = durationNanos(duration);
    requireOpen();
    var stopped = new AtomicBoolean();
    var iterator =
        new Spliterators.AbstractSpliterator<T>(
            Long.MAX_VALUE, Spliterator.ORDERED | Spliterator.NONNULL) {
          private Long started;
          private Iterator<JsonNode> pending = Collections.emptyIterator();

          @Override
          public Spliterator<T> trySplit() {
            return null;
          }

          @Override
          public boolean tryAdvance(Consumer<? super T> action) {
            Objects.requireNonNull(action, "action");
            if (stopped.get()) return false;
            if (Thread.currentThread().isInterrupted()) {
              throw new CancellationException("Ahrara stream interrupted.");
            }
            if (started == null) started = System.nanoTime();
            if (remainingNanos(limit, started) <= 0) return false;

            try {
              while (!pending.hasNext()) {
                long remaining = remainingNanos(limit, started);
                if (remaining <= 0) return false;

                JsonNode result =
                    awaitResult(
                        limit == Long.MAX_VALUE ? null : Duration.ofNanos(remaining),
                        request,
                        stopped::get);
                if (result == null) return false;

                pending = items.apply(result);
              }

              action.accept(decode.apply(pending.next()));
              return true;
            } catch (TimeoutException expired) {
              return false;
            } catch (InterruptedException interrupted) {
              Thread.currentThread().interrupt();
              throw new CancellationException("Ahrara stream interrupted.");
            }
          }
        };

    return StreamSupport.stream(iterator, false).onClose(() -> stopped.set(true));
  }

  private JsonNode call(String operation) {
    return call(operation, Map.of());
  }

  private JsonNode call(String operation, Map<String, ?> arguments) {
    requireOpen();
    var request = new LinkedHashMap<String, Object>(arguments);
    request.put("op", operation);
    request.put("handle", state.handle);
    return callNative(request);
  }

  private JsonNode callNative(Map<String, Object> request) {
    try {
      return NativeCore.call(request);
    } catch (AhraraException error) {
      // A call racing close reports the documented closed-inbox error.
      if (state.closed.get() && "inbox_closed".equals(error.code())) {
        throw new IllegalStateException("The Ahrara inbox is closed.", error);
      }

      throw error;
    }
  }

  private void requireOpen() {
    if (state.closed.get()) {
      throw new IllegalStateException("The Ahrara inbox is closed.");
    }
  }

  private static long durationNanos(Duration duration) {
    if (duration == null) {
      return Long.MAX_VALUE;
    }
    if (duration.isNegative() || duration.isZero()) {
      throw new IllegalArgumentException("Duration must be positive when supplied.");
    }
    try {
      return duration.toNanos();
    } catch (ArithmeticException overflow) {
      throw new IllegalArgumentException("Duration is too large.", overflow);
    }
  }

  private static long remainingNanos(long duration, long started) {
    return duration == Long.MAX_VALUE ? Long.MAX_VALUE : duration - (System.nanoTime() - started);
  }

  private static TimeoutException timeout() {
    return new TimeoutException("Timed out waiting for an Ahrara email.");
  }

  /**
   * Disconnects and releases resources. Persistent profiles remain on disk; repeated calls are
   * safe.
   */
  @Override
  public void close() {
    cleanable.clean();
  }
}
