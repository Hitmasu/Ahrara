package dev.ahrara;

import java.time.Duration;
import java.util.LinkedHashMap;
import java.util.Map;

/**
 * Connection, identity, and storage settings. Null values use native defaults.
 *
 * @param serverUrl secure relay URL
 * @param serverPublicKey independently verified relay public key
 * @param tlsCaPath optional PEM CA file
 * @param connectTimeout optional connection timeout, 1 ms to 5 minutes
 * @param credentials optional exported identity and address; close the old connection first
 * @param profilePath optional absolute profile directory preserving encrypted history
 * @param recoveryKey optional secret identity to import, mutually exclusive with credentials
 * @param address optional owned address to restore, mutually exclusive with credentials
 * @param filter optional global conditions; rejected incoming mail is discarded before storage
 */
public record InboxOptions(
    String serverUrl,
    String serverPublicKey,
    String tlsCaPath,
    Duration connectTimeout,
    InboxCredentials credentials,
    String profilePath,
    String recoveryKey,
    String address,
    EmailFilter filter) {
  /**
   * Validates connection settings.
   *
   * @throws IllegalArgumentException if the timeout is outside the supported range
   */
  public InboxOptions {
    if (connectTimeout != null
        && (connectTimeout.isNegative()
            || connectTimeout.isZero()
            || connectTimeout.toMillis() < 1
            || connectTimeout.toMillis() > 300_000)) {
      throw new IllegalArgumentException("Connect timeout must be between 1 ms and 5 minutes.");
    }
  }

  /**
   * Configures a fresh temporary inbox.
   *
   * @param serverUrl secure relay URL
   * @param serverPublicKey verified relay public key
   * @param tlsCaPath optional CA file
   * @param connectTimeout optional connection timeout
   */
  public InboxOptions(
      String serverUrl, String serverPublicKey, String tlsCaPath, Duration connectTimeout) {
    this(serverUrl, serverPublicKey, tlsCaPath, connectTimeout, null, null, null, null, null);
  }

  /**
   * @return settings using all native defaults
   */
  public static InboxOptions defaults() {
    return new InboxOptions(null, null, null, null);
  }

  /**
   * Selects exported credentials for a later connection.
   *
   * @param value exported credentials
   * @return new settings preserving relay and profile settings
   */
  public InboxOptions withCredentials(InboxCredentials value) {
    return new InboxOptions(
        serverUrl,
        serverPublicKey,
        tlsCaPath,
        connectTimeout,
        value,
        profilePath,
        null,
        null,
        filter);
  }

  /**
   * Selects durable encrypted storage.
   *
   * @param value absolute directory to retain across executions
   * @return new settings preserving connection and identity settings
   */
  public InboxOptions withProfilePath(String value) {
    return new InboxOptions(
        serverUrl,
        serverPublicKey,
        tlsCaPath,
        connectTimeout,
        credentials,
        value,
        recoveryKey,
        address,
        filter);
  }

  /**
   * Imports an identity without requiring a previously exported credential object.
   *
   * @param key secret recovery key
   * @param ownedAddress optional existing address belonging to this identity
   * @return new settings using this identity instead of exported credentials
   */
  public InboxOptions withIdentity(String key, String ownedAddress) {
    return new InboxOptions(
        serverUrl,
        serverPublicKey,
        tlsCaPath,
        connectTimeout,
        null,
        profilePath,
        key,
        ownedAddress,
        filter);
  }

  /**
   * Sets global conditions applied before incoming email is stored.
   *
   * @param value filter, or null to accept all mail
   * @return new settings preserving connection, identity, and profile
   */
  public InboxOptions withFilter(EmailFilter value) {
    return new InboxOptions(
        serverUrl,
        serverPublicKey,
        tlsCaPath,
        connectTimeout,
        credentials,
        profilePath,
        recoveryKey,
        address,
        value);
  }

  Map<String, Object> toNative() {
    var options = new LinkedHashMap<String, Object>();
    if (serverUrl != null) options.put("server_url", serverUrl);
    if (serverPublicKey != null) options.put("server_public_key", serverPublicKey);
    if (tlsCaPath != null) options.put("tls_ca_path", tlsCaPath);
    if (connectTimeout != null) options.put("connect_timeout_ms", connectTimeout.toMillis());
    if (credentials != null) options.put("credentials", credentials.toNative());
    if (profilePath != null) options.put("profile_path", profilePath);
    if (recoveryKey != null) options.put("recovery_key", recoveryKey);
    if (address != null) options.put("address", address);
    if (filter != null) options.put("filter", filter.toNative());

    return options;
  }

  /**
   * @return a representation with all potentially sensitive values redacted
   */
  @Override
  public String toString() {
    return "InboxOptions[REDACTED]";
  }
}
