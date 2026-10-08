//go:build cgo && (linux || darwin || windows)

package ahrara

/*
#cgo linux LDFLAGS: -ldl
#include <stdint.h>
#include <stdlib.h>
#ifdef _WIN32
#include <windows.h>
static HMODULE library;
// Go passes UTF-8; the ANSI LoadLibraryExA would misread non-ASCII paths.
static HMODULE load_library(const char *path) {
    int length = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, path, -1, NULL, 0);
    wchar_t *wide = length ? malloc(length * sizeof(wchar_t)) : NULL;
    HMODULE result = NULL;
    if (wide && MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, path, -1, wide, length))
        result = LoadLibraryExW(wide, NULL, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS | LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR);
    free(wide);
    return result;
}
#define LOAD(path) load_library(path)
#define SYMBOL(name) GetProcAddress(library, name)
#else
#include <dlfcn.h>
static void *library;
#define LOAD(path) dlopen(path, RTLD_NOW | RTLD_LOCAL)
#define SYMBOL(name) dlsym(library, name)
#endif
static char *(*call_fn)(const char *);
static void (*free_fn)(char *);
static const char *load_ahrara(const char *path) {
    library = LOAD(path);
    if (!library) return "cannot load the Ahrara native library; set AHRARA_NATIVE_LIBRARY to its absolute path";
    uint32_t (*version_fn)(void) = (uint32_t (*)(void))SYMBOL("ahrara_abi_version");
    call_fn = (char *(*)(const char *))SYMBOL("ahrara_call");
    free_fn = (void (*)(char *))SYMBOL("ahrara_string_free");
    if (!version_fn || !call_fn || !free_fn) return "the Ahrara native library is missing ABI symbols";
    if (version_fn() != 1) return "unsupported Ahrara native ABI; version 1 is required";
    return NULL;
}
static char *call_ahrara(const char *request) { return call_fn(request); }
static void free_ahrara(char *response) { free_fn(response); }
*/
import "C"

import (
	"errors"
	"path/filepath"
	"strings"
	"sync"
	"unsafe"
)

var nativeOnce sync.Once
var nativeError error

func nativeCall(request []byte) ([]byte, error) {
	nativeOnce.Do(func() {
		path, err := nativeLibrary()
		if err != nil {
			nativeError = err
			return
		}

		if !filepath.IsAbs(path) || strings.ContainsRune(path, 0) {
			nativeError = errors.New("AHRARA_NATIVE_LIBRARY must be an absolute path without NUL bytes")
			return
		}

		cpath := C.CString(path)
		defer C.free(unsafe.Pointer(cpath))
		if message := C.load_ahrara(cpath); message != nil {
			nativeError = errors.New(C.GoString(message))
		}
	})
	if nativeError != nil {
		return nil, nativeError
	}

	input := C.CString(string(request))
	defer C.free(unsafe.Pointer(input))
	output := C.call_ahrara(input)
	if output == nil {
		return nil, errors.New("Ahrara native library returned a null response")
	}

	defer C.free_ahrara(output)
	return []byte(C.GoString(output)), nil
}
