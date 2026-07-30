#ifndef RISH_H
#define RISH_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/**
 * Plans a guest command.
 *
 * Input is a bounded UTF-8 JSON byte slice. Output is NUL-terminated, belongs
 * to Rust, and must be released with rish_string_free.
 */
char *rish_plan_json(const char *input, size_t input_len);

/**
 * Executes a bounded portable applet in an app-owned sandbox root.
 *
 * The request is a versioned UTF-8 JSON envelope. Command stdin/stdout/stderr
 * are JSON byte arrays, so binary data is preserved.
 */
char *rish_execute_applet_json(const char *input, size_t input_len);

/** Releases a string returned by either JSON operation. */
void rish_string_free(char *value);

/** Returns the host protocol version implemented by this library. */
uint32_t rish_protocol_version(void);

#ifdef __cplusplus
}
#endif

#endif
