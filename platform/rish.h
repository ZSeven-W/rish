#ifndef RISH_H
#define RISH_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/**
 * Trusted-host registry fetch callback used by rish_pull_image_json.
 *
 * request_json is a bounded, versioned JSON envelope and is borrowed only for
 * the duration of this call. body_fd is a borrowed writable file descriptor:
 * the callback must not close it or retain it after returning. Stream response
 * bytes into body_fd incrementally and stop at request.max_response_bytes;
 * never encode a manifest, config, or layer body into response_meta_json.
 *
 * response_meta_json receives a versioned JSON envelope:
 * {"protocol_version":1,"ok":true,"status":200,
 *  "headers":{"content-type":["application/octet-stream"]},
 *  "error":null,"retryable":false}
 *
 * Set response_meta_len to the exact number of bytes written, without a NUL
 * terminator. Return zero when the callback itself completed; network failures
 * are represented with ok=false. A non-zero return is treated as a host bridge
 * failure. The callback must enforce HTTPS/redirect/auth policy and must not
 * forward Authorization across origins.
 */
typedef int32_t (*rish_registry_fetch_callback)(
    void *context,
    const uint8_t *request_json,
    size_t request_len,
    int32_t body_fd,
    uint8_t *response_meta_json,
    size_t response_meta_capacity,
    size_t *response_meta_len
);

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

/**
 * Synchronously pulls, verifies, stores, and pins one OCI image.
 *
 * Call this from a worker thread. The request is a versioned UTF-8 JSON
 * envelope containing protocol_version, reference, store_root, an optional
 * exact platform token ("linux/arm64/v8" or "linux/amd64"), and optional
 * tightening limits. Omitting platform preserves the linux/arm64/v8 default.
 * fetch and context must remain valid until this call returns. The returned
 * JSON string belongs to Rust and must be released with rish_string_free.
 */
char *rish_pull_image_json(
    const char *input,
    size_t input_len,
    rish_registry_fetch_callback fetch,
    void *context
);

/** Releases a string returned by either JSON operation. */
void rish_string_free(char *value);

/** Returns the host protocol version implemented by this library. */
uint32_t rish_protocol_version(void);

#ifdef __cplusplus
}
#endif

#endif
