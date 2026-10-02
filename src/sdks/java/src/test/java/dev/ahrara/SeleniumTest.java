package dev.ahrara;

import static org.junit.jupiter.api.Assertions.*;

import com.sun.net.httpserver.HttpServer;
import java.net.InetSocketAddress;
import java.net.URI;
import java.nio.charset.StandardCharsets;
import java.time.Duration;
import java.util.concurrent.Executors;
import org.junit.jupiter.api.Test;
import org.openqa.selenium.By;
import org.openqa.selenium.chrome.ChromeOptions;
import org.openqa.selenium.remote.RemoteWebDriver;
import org.openqa.selenium.support.ui.WebDriverWait;

class SeleniumTest {
  @Test
  void signsUpAndVerifiesTheCodeReceivedByTheNativeSdk() throws Exception {
    // Browser and Java test containers share the host network for this fixture.
    var server = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
    var executor = Executors.newCachedThreadPool();
    server.setExecutor(executor);
    server.createContext(
        "/",
        exchange -> {
          byte[] html =
              """
              <!doctype html><meta charset="utf-8"><title>SDK verification fixture</title>
              <form id="signup"><input id="email" type="email" required><button>Send code</button></form>
              <p id="sent"></p><input id="code"><button id="verify">Verify</button><p id="result"></p>
              <script>
              document.querySelector('#signup').onsubmit = async event => {
                event.preventDefault();
                const response = await fetch('/send', {method:'POST', body:document.querySelector('#email').value});
                document.querySelector('#sent').textContent = response.ok ? 'Code sent' : 'Send failed';
              };
              document.querySelector('#verify').onclick = async () => {
                const response = await fetch('/verify', {method:'POST', body:document.querySelector('#code').value});
                document.querySelector('#result').textContent = await response.text();
              };
              </script>
              """
                  .getBytes(StandardCharsets.UTF_8);
          exchange.getResponseHeaders().set("Content-Type", "text/html; charset=utf-8");
          exchange.sendResponseHeaders(200, html.length);
          try (var body = exchange.getResponseBody()) {
            body.write(html);
          }
        });
    server.createContext(
        "/send",
        exchange -> {
          try {
            String address =
                new String(exchange.getRequestBody().readAllBytes(), StandardCharsets.UTF_8);
            Relay.send(address, "Verify your signup", "Your verification code is 748291");
            exchange.sendResponseHeaders(204, -1);
          } catch (Exception error) {
            exchange.sendResponseHeaders(500, -1);
          } finally {
            exchange.close();
          }
        });
    server.createContext(
        "/verify",
        exchange -> {
          String code =
              new String(exchange.getRequestBody().readAllBytes(), StandardCharsets.UTF_8);
          byte[] response =
              (code.equals("748291") ? "Verified" : "Invalid code")
                  .getBytes(StandardCharsets.UTF_8);
          exchange.sendResponseHeaders(200, response.length);
          try (var body = exchange.getResponseBody()) {
            body.write(response);
          }
        });
    server.start();
    RemoteWebDriver browser = null;
    try (var inbox = Ahrara.createInbox(Relay.options())) {
      var options = new ChromeOptions();
      options.addArguments("--headless=new", "--no-sandbox", "--disable-dev-shm-usage");
      browser =
          new RemoteWebDriver(
              URI.create(Relay.required("AHRARA_TEST_SELENIUM_URL")).toURL(), options);
      browser.get("http://127.0.0.1:" + server.getAddress().getPort());
      browser.findElement(By.id("email")).sendKeys(inbox.address());
      browser.findElement(By.cssSelector("#signup button")).click();
      new WebDriverWait(browser, Duration.ofSeconds(10))
          .until(driver -> driver.findElement(By.id("sent")).getText().equals("Code sent"));
      var email = inbox.waitForEmail(Duration.ofSeconds(10), "Verify your signup", null);
      var code = java.util.regex.Pattern.compile("\\b\\d{6}\\b").matcher(email.textBody());

      assertTrue(code.find());
      browser.findElement(By.id("code")).sendKeys(code.group());
      browser.findElement(By.id("verify")).click();
      new WebDriverWait(browser, Duration.ofSeconds(10))
          .until(driver -> driver.findElement(By.id("result")).getText().equals("Verified"));
    } finally {
      try {
        if (browser != null) browser.quit();
      } finally {
        server.stop(0);
        executor.shutdownNow();
      }
    }
  }
}
