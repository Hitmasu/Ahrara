package dev.ahrara;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.sun.jna.Library;
import com.sun.jna.Native;
import com.sun.jna.Pointer;
import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.util.Locale;
import java.util.Map;

final class NativeCore {
  private static final ObjectMapper JSON = new ObjectMapper();
  private static final Api API = load();

  interface Api extends Library {
    int ahrara_abi_version();

    Pointer ahrara_call(String request);

    void ahrara_string_free(Pointer response);
  }

  private static Api load() {
    String library = System.getenv("AHRARA_NATIVE_LIBRARY");
    if (library != null && !library.isBlank()) {
      if (!Path.of(library).isAbsolute())
        throw new IllegalArgumentException("AHRARA_NATIVE_LIBRARY must be an absolute path.");
    } else {
      library = extractPackagedLibrary();
    }

    Api api = Native.load(library, Api.class, Map.of(Library.OPTION_STRING_ENCODING, "UTF-8"));
    if (api.ahrara_abi_version() != 1)
      throw new IllegalStateException("This SDK requires Ahrara native ABI version 1.");

    return api;
  }

  private static String extractPackagedLibrary() {
    String osName = System.getProperty("os.name").toLowerCase(Locale.ROOT);
    String os =
        osName.contains("win")
            ? "windows"
            : osName.contains("mac")
                ? "darwin"
                : osName.contains("linux") ? "linux" : "unsupported";
    String archName = System.getProperty("os.arch");
    String arch =
        switch (archName) {
          case "amd64", "x86_64" -> "x86_64";
          case "aarch64", "arm64" -> "aarch64";
          default -> "unsupported";
        };
    String file = System.mapLibraryName("ahrara_native");
    String resource = "/native/" + os + "-" + arch + "/" + file;
    try (var source = NativeCore.class.getResourceAsStream(resource)) {
      if (source == null)
        throw new IllegalStateException(
            "No packaged Ahrara native library for "
                + os
                + "-"
                + arch
                + ". Use a package for this platform, or set AHRARA_NATIVE_LIBRARY for"
                + " development.");

      Path directory = Files.createTempDirectory("ahrara-native-");
      Path destination = directory.resolve(file);
      directory.toFile().deleteOnExit();
      destination.toFile().deleteOnExit();
      Files.copy(source, destination, StandardCopyOption.REPLACE_EXISTING);
      return destination.toAbsolutePath().toString();
    } catch (IOException error) {
      throw new IllegalStateException(
          "Unable to extract the packaged Ahrara native library.", error);
    }
  }

  static JsonNode call(Map<String, ?> request) {
    Pointer response = null;
    try {
      response = API.ahrara_call(JSON.writeValueAsString(request));
      if (response == null)
        throw new IllegalStateException("Ahrara returned an empty native response.");

      JsonNode root = JSON.readTree(response.getString(0, "UTF-8"));
      if (!root.path("ok").asBoolean()) {
        JsonNode error = root.path("error");
        throw new AhraraException(error.path("code").asText(), error.path("message").asText());
      }

      return root.path("result");
    } catch (IOException error) {
      throw new IllegalStateException("Invalid Ahrara native JSON response.", error);
    } finally {
      if (response != null) API.ahrara_string_free(response);
    }
  }
}
