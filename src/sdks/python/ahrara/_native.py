"""Owned-string ctypes boundary; all mail/protocol behavior stays in Rust."""

import ctypes
import json
import os
from pathlib import Path
import sys
import threading
from typing import Any, cast


class AhraraError(RuntimeError):
    """Native failure identified by its stable ``code``."""

    def __init__(self, code: str, message: str) -> None:
        super().__init__(message)
        self.code = code


_native: ctypes.CDLL | None = None
_load_lock = threading.Lock()


def call(request: dict[str, Any]) -> dict[str, Any]:
    """Call ABI v1 and free its response with the same native allocator."""
    global _native
    with _load_lock:
        if _native is None:
            names = {"win32": "ahrara_native.dll", "darwin": "libahrara_native.dylib"}
            name = names.get(sys.platform, "libahrara_native.so")
            path = Path(
                os.environ.get("AHRARA_NATIVE_LIBRARY")
                or Path(__file__).parent / "native" / name
            )
            if not path.is_absolute() or not path.is_file():
                raise AhraraError(
                    "native_library_missing",
                    "Install a wheel containing the native library, or set "
                    "AHRARA_NATIVE_LIBRARY to its absolute path.",
                )

            native = ctypes.CDLL(str(path))
            native.ahrara_abi_version.argtypes = []
            native.ahrara_abi_version.restype = ctypes.c_uint32
            if native.ahrara_abi_version() != 1:
                raise AhraraError(
                    "abi_mismatch",
                    "The native library requires a different SDK version.",
                )

            native.ahrara_call.argtypes = [ctypes.c_char_p]
            native.ahrara_call.restype = ctypes.c_void_p
            native.ahrara_string_free.argtypes = [ctypes.c_void_p]
            native.ahrara_string_free.restype = None
            _native = native
    pointer = _native.ahrara_call(json.dumps(request).encode("utf-8"))
    if not pointer:
        raise AhraraError("native_error", "The native library returned no response.")
    try:
        response = json.loads(ctypes.string_at(pointer))
    finally:
        _native.ahrara_string_free(pointer)
    if not response["ok"]:
        raise AhraraError(response["error"]["code"], response["error"]["message"])

    return cast(dict[str, Any], response["result"])
