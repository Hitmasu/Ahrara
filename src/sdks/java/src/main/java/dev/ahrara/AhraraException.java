package dev.ahrara;

/** An error reported by the shared Rust core. */
public final class AhraraException extends RuntimeException {
  private final String code;

  /**
   * Creates a native error.
   *
   * @param code stable machine-readable native code
   * @param message diagnostic message
   */
  public AhraraException(String code, String message) {
    super(message);
    this.code = code;
  }

  /**
   * @return the stable native error code
   */
  public String code() {
    return code;
  }
}
