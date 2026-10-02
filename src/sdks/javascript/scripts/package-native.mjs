import { copyFileSync, existsSync, mkdirSync } from "node:fs";
import { isAbsolute } from "node:path";

const name =
  process.platform === "win32"
    ? "ahrara_native.dll"
    : process.platform === "darwin"
      ? "libahrara_native.dylib"
      : "libahrara_native.so";
const directory = new URL(
  `../native/${process.platform}-${process.arch}/`,
  import.meta.url,
);
const destination = new URL(name, directory);
const source = process.env.AHRARA_NATIVE_LIBRARY;
if (source) {
  if (!isAbsolute(source))
    throw new Error("AHRARA_NATIVE_LIBRARY must be an absolute path.");

  mkdirSync(directory, { recursive: true });
  copyFileSync(source, destination);
}
if (!existsSync(destination))
  throw new Error(
    "Build the native library and set AHRARA_NATIVE_LIBRARY before packaging.",
  );
