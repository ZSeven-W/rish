#ifndef RISH_H
#define RISH_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/**
 * Plans a guest command.
 *
 * Input and output are NUL-terminated UTF-8 JSON strings. The returned value
 * belongs to Rust and must be released with rish_string_free.
 */
char *rish_plan_json(const char *input);

/** Releases a string returned by rish_plan_json. */
void rish_string_free(char *value);

/** Returns the host protocol version implemented by this library. */
uint32_t rish_protocol_version(void);

#ifdef __cplusplus
}
#endif

#endif
