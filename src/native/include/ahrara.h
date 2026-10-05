#ifndef AHRARA_H
#define AHRARA_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

uint32_t ahrara_abi_version(void);

/* Thread-safe blocking call; call from a worker for asynchronous host APIs.
 * Input: borrowed NUL-terminated UTF-8 JSON of at most 1 MiB, valid until the
 * call returns: {"op": "<operation>", ...}. Operations and their fields are the
 * `Request` enum in src/native/src/lib.rs (snake_case names).
 * Output: owned NUL-terminated UTF-8 JSON, either {"ok": true, "result": ...}
 * or {"ok": false, "error": {"code": "...", "message": "..."}}. Codes:
 * invalid_argument, inbox_closed, connection_failed, resource_limit,
 * internal_error, email_not_found, attachment_not_found, html_not_found,
 * invalid_message or request_failed. Free exactly once using the function
 * below, which clears the response first. No exceptions or Rust panics cross
 * this boundary.
 */
char *ahrara_call(const char *request_json);

void ahrara_string_free(char *response_json);

#ifdef __cplusplus
}
#endif

#endif
