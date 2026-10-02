package dev.ahrara;

/** Invoked with the built jar (not target/classes) to verify native extraction. */
public final class PackagingSmoke {
  public static void main(String[] args) throws Exception {
    if (System.getenv("AHRARA_NATIVE_LIBRARY") != null)
      throw new IllegalStateException(
          "Unset AHRARA_NATIVE_LIBRARY to verify packaged native loading.");

    try (var inbox = Ahrara.createInbox(Relay.options())) {
      Relay.send(inbox.address(), "Packaged Java SDK", "embedded core works");
      if (!inbox.waitForEmail().subject().equals("Packaged Java SDK"))
        throw new AssertionError("The packaged SDK did not receive its email.");
    }
    System.out.println("Packaged jar native loading and SMTP delivery passed.");
  }
}
