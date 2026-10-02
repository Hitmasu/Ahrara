package dev.ahrara;

import java.util.Map;
import java.util.Objects;

/**
 * Reconnects an identity and address. Keep the recovery key secret; history lives in the profile.
 *
 * @param address address belonging to the identity
 * @param recoveryKey secret identity recovery key
 */
public record InboxCredentials(String address, String recoveryKey) {
  /**
   * Rejects missing credential fields.
   *
   * @throws IllegalArgumentException if a field is blank
   * @throws NullPointerException if a field is null
   */
  public InboxCredentials {
    Objects.requireNonNull(address, "address");
    Objects.requireNonNull(recoveryKey, "recoveryKey");
    if (address.isBlank() || recoveryKey.isBlank()) {
      throw new IllegalArgumentException("Both address and recovery key are required.");
    }
  }

  Map<String, String> toNative() {
    return Map.of("address", address, "recovery_key", recoveryKey);
  }

  /**
   * @return a representation containing no address or recovery key
   */
  @Override
  public String toString() {
    return "InboxCredentials[REDACTED]";
  }
}
