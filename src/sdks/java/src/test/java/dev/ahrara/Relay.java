package dev.ahrara;

import jakarta.mail.Session;
import jakarta.mail.Transport;
import jakarta.mail.internet.InternetAddress;
import jakarta.mail.internet.MimeMessage;
import java.io.InputStream;
import java.nio.file.Files;
import java.nio.file.Path;
import java.security.KeyStore;
import java.security.cert.CertificateFactory;
import java.time.Duration;
import java.util.Properties;
import javax.net.ssl.SSLContext;
import javax.net.ssl.TrustManagerFactory;

final class Relay {
  static String required(String name) {
    String value = System.getenv(name);
    if (value == null || value.isBlank())
      throw new IllegalStateException("Set " + name + " for the real relay tests.");

    return value;
  }

  static InboxOptions options() {
    return new InboxOptions(
        required("AHRARA_TEST_SERVER_URL"),
        required("AHRARA_TEST_SERVER_PUBLIC_KEY"),
        required("AHRARA_TEST_TLS_CA"),
        Duration.ofSeconds(10));
  }

  static void send(String address, String subject, String body) throws Exception {
    send(address, subject, body, null, null);
  }

  static void send(String address, String subject, String body, String html, byte[] attachment)
      throws Exception {
    send(address, subject, body, html, attachment, java.util.Map.of());
  }

  static void send(
      String address,
      String subject,
      String body,
      String html,
      byte[] attachment,
      java.util.Map<String, String> headers)
      throws Exception {
    var endpoint = java.net.URI.create("smtp://" + required("AHRARA_TEST_SMTP_ADDRESS"));
    var trust = KeyStore.getInstance(KeyStore.getDefaultType());
    trust.load(null, null);
    try (InputStream input = Files.newInputStream(Path.of(required("AHRARA_TEST_TLS_CA")))) {
      trust.setCertificateEntry(
          "relay-ca", CertificateFactory.getInstance("X.509").generateCertificate(input));
    }
    var managers = TrustManagerFactory.getInstance(TrustManagerFactory.getDefaultAlgorithm());
    managers.init(trust);
    var tls = SSLContext.getInstance("TLS");
    tls.init(null, managers.getTrustManagers(), null);
    var properties = new Properties();
    properties.setProperty("mail.smtp.host", endpoint.getHost());
    properties.setProperty("mail.smtp.port", String.valueOf(endpoint.getPort()));
    properties.setProperty("mail.smtp.starttls.enable", "true");
    properties.setProperty("mail.smtp.starttls.required", "true");
    properties.setProperty("mail.smtp.ssl.checkserveridentity", "true");
    properties.setProperty("mail.smtp.connectiontimeout", "10000");
    properties.setProperty("mail.smtp.timeout", "10000");
    properties.put("mail.smtp.ssl.socketFactory", tls.getSocketFactory());
    var message = new MimeMessage(Session.getInstance(properties));
    message.setFrom(new InternetAddress("sdk@example.test"));
    message.setRecipients(jakarta.mail.Message.RecipientType.TO, address);
    message.setSubject(subject, "UTF-8");
    var mixed = new jakarta.mail.internet.MimeMultipart("mixed");
    var text = new jakarta.mail.internet.MimeBodyPart();
    text.setText(body, "UTF-8");
    if (html == null) {
      mixed.addBodyPart(text);
    } else {
      var alternative = new jakarta.mail.internet.MimeMultipart("alternative");
      alternative.addBodyPart(text);
      var htmlPart = new jakarta.mail.internet.MimeBodyPart();
      htmlPart.setContent(html, "text/html; charset=UTF-8");
      alternative.addBodyPart(htmlPart);
      var textParts = new jakarta.mail.internet.MimeBodyPart();
      textParts.setContent(alternative);
      mixed.addBodyPart(textParts);
    }
    if (attachment != null) {
      var part = new jakarta.mail.internet.MimeBodyPart();
      part.setDataHandler(
          new jakarta.activation.DataHandler(
              new jakarta.mail.util.ByteArrayDataSource(attachment, "application/octet-stream")));
      part.setFileName("proof.bin");
      part.setDisposition(jakarta.mail.Part.ATTACHMENT);
      mixed.addBodyPart(part);
    }

    message.setContent(mixed);
    for (var header : headers.entrySet()) message.setHeader(header.getKey(), header.getValue());
    Transport.send(message);
  }
}
