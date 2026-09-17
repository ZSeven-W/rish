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

/**
 * Boots the in-repository pure-Rust x86_64 interpreter with an app-supplied
 * kernel and initramfs and runs one command inside the Linux guest — the full
 * docker surface.
 *
 * Call this from a worker thread: it boots a Linux guest and is slow. The
 * request is UTF-8 JSON with kernel_path, initrd_path, an optional
 * root_disk_path, an optional writable data_disk_path the guest sees as
 * /dev/vdb, memory_mib, a command argv array, an optional command_line,
 * and optional boot_budget_units / handshake_budget_units. The kernel and
 * initramfs are named by path (staged as app bundle resources) so the large
 * binaries never cross the ABI as data. The reply JSON carries ok, exit_code,
 * stdout, stderr, and boot_units, or ok=false with an error. The returned
 * string belongs to Rust and must be released with rish_string_free.
 */
char *rish_vm_run_docker_json(const char *input, size_t input_len);

/**
 * Boots an interactive guest session and returns an opaque handle (or NULL on
 * failure). Run many commands over the same booted guest with
 * rish_vm_session_exec_json, then release the handle with rish_vm_session_free.
 * Boots a Linux guest and blocks, so call it from a worker thread. The request
 * JSON matches rish_vm_run_docker_json without the command field.
 */
void *rish_vm_boot_session(const char *input, size_t input_len);

/** Creates a one-shot cancellation token for exactly one VM lifetime. */
void *rish_vm_cancel_new(void);

/** Requests cancellation from any thread without touching the session handle.
 * NULL is a no-op. Cancellation is sticky and cannot be reset. Boot, handshake
 * and execution observe it between bounded provider quanta; active worker
 * execution is also signalled. This call never waits for the guest or session.
 * Do not race token destruction with this call or any other token use.
 */
void rish_vm_cancel_request(void *cancel);

/** Releases token ownership exactly once, after all boot/request calls using
 * this pointer have returned. NULL is a no-op. A booted session owns an internal
 * reference, so it remains safe if the caller releases this handle first.
 * Never access this pointer after free. Usually keep it until the run ends.
 */
void rish_vm_cancel_free(void *cancel);

/** Same boot JSON and session handle as boot_session, with cancellation.
 * The non-NULL token must come from cancel_new and stay alive through this
 * call. Use a fresh token per boot (including failed boots). A cancelled boot
 * returns NULL; cancelled exec/stream returns ok=false with E_VM_CANCELLED in
 * error. Existing exec/stream calls automatically observe the session token.
 * Cancel, wait for boot/exec to return, free any returned session exactly once,
 * then free the token. Never cancel by concurrently freeing the session.
 */
void *rish_vm_boot_session_cancellable(const char *input, size_t input_len,
                                      void *cancel);

/**
 * Runs one command in a live session and returns an owned JSON reply
 * ({ok, exit_code, stdout, stderr, ...}). The request is
 * {"command":["argv0","argv1",...]}. Optional version 2 accepts
 * {"protocol_version":2,"command":[...],"cwd":"/workspace",
 *  "env":{"NAME":"value"},"timeout_ms":60000}. cwd is a guest path, never
 * a host mapping. timeout_ms is 1..86400000 on the HOST monotonic clock (the
 * budget is not forwarded to the guest clock); expiry cancels this VM lifetime
 * (E_VM_TIMEOUT), so free the session after the call returns. V1 behavior is
 * unchanged. Optional v2 max_output_bytes (1..67108864) bounds combined output;
 * exceeding it cancels the session with E_VM_OUTPUT_LIMIT. The exceeding chunk
 * is not delivered to the callback. One bounded control batch may be decoded
 * transiently before the cap is enforced. Default stream caps remain 64MiB.
 * The returned string belongs to Rust; use rish_string_free.
 * Execute on one worker at a time; do not race execution with session_free.
 */
char *rish_vm_session_exec_json(void *session, const char *input, size_t input_len);

/** Incremental output as a versioned JSON envelope:
 * {protocol_version:1,event:"output",sequence:0,channel:"stdout",data_base64:"..."}.
 * Channels are stdout, stderr, or console; sequence starts at zero per call.
 * UTF-8 JSON bytes are borrowed only during the synchronous callback and are
 * not NUL-terminated. Do not retain the
 * pointer or re-enter/free the session from this callback. No credential is
 * interpreted or stored by the bridge; the command owns its output contract.
 */
typedef void (*rish_vm_output_callback)(void *context, const char *event_json,
                                        size_t length);

/** Same owned JSON result as exec_json, with output delivered before exit.
 * Call on a worker thread. The callback/context must remain valid until this
 * call returns. The caller releases the returned string with rish_string_free.
 */
char *rish_vm_session_exec_stream_json(void *session, const char *input,
    size_t input_len, void *context, rish_vm_output_callback callback);

/** Writes stdin to the command an exec_stream call is already running.
 *
 * A stream execution hands the guest one stdin buffer before the command
 * starts, which cannot answer a prompt the command has not printed yet. Ask
 * for it with "interactive_stdin":true on the exec request, then queue bytes
 * here while the command runs; the execution writes them at its next frame
 * boundary. This never takes the session lock that execution holds, so it may
 * be called from another thread while the command is still running.
 *
 * Request: {"protocol_version":1,"action":"write_stdin","data_base64":"..."}
 * or {"protocol_version":1,"action":"close_stdin"}. The reply carries ok, or
 * ok=false with an error. Release the returned string with rish_string_free.
 */
char *rish_vm_session_control_json(void *session, const char *input,
                                   size_t input_len);

/** Releases a session from either boot ABI, exactly once after all its calls
 * and callbacks have returned. NULL is a no-op. */
void rish_vm_session_free(void *session);

/** Releases a string returned by any JSON operation. */
void rish_string_free(char *value);

/** Returns the host protocol version implemented by this library. */
uint32_t rish_protocol_version(void);

#ifdef __cplusplus
}
#endif

#endif
