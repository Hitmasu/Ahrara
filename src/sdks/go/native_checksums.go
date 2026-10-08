package ahrara

// The release workflow replaces these values in the commit tagged
// src/sdks/go/vX.Y.Z with that GitHub Release's version and the SHA-256 of each
// native library it attaches. Source checkouts set AHRARA_NATIVE_LIBRARY.
var (
	nativeVersion = ""
	nativeSHA256  = map[string]string{}
)
