#ifndef AHRARA_H
#define AHRARA_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

uint32_t ahrara_abi_version(void);

/* Thread-safe blocking call; call from a worker for asynchronous host APIs.
 * Input: borrowed NUL-terminated UTF-8 JSON, valid until the call returns.
 * Output: owned NUL-terminated UTF-8 JSON. Free exactly once using the function
 * below. No exceptions or Rust panics cross this boundary. See docs/sdks.md.
 */
char *ahrara_call(const char *request_json);

void ahrara_string_free(char *response_json);

#ifdef __cplusplus
}
#endif

#endif
